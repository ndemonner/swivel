//! Echo cancellation, through Apple's Voice Processing unit.
//!
//! Without this, using `swivel` on speakers is unusable: the far end hears
//! themselves, delayed by the round trip, which is the single most distracting
//! artefact a voice link can produce. Headphones avoid it. Most people do not
//! wear headphones all day.
//!
//! macOS already has the answer. `kAudioUnitSubType_VoiceProcessingIO` is the
//! same acoustic echo canceller, noise suppressor, and gain control that
//! FaceTime uses. It is far better than anything reasonable to write here, and
//! it costs no dependency.
//!
//! # Why this replaces both streams
//!
//! An echo canceller needs the signal that went to the speaker in order to
//! subtract it from what the microphone hears. The Voice Processing unit is
//! therefore **one unit doing input and output together**. When it is running,
//! it owns both directions, and the plain CoreAudio streams in `capture.rs` and
//! `playback.rs` are not used.
//!
//! # Why it starts and stops with a conversation
//!
//! Whether the input element is enabled is fixed when the unit is initialised.
//! Leaving it enabled all the time would hold the microphone open, which is
//! exactly what the product promises not to do, so the unit is rebuilt at the
//! two moments a conversation starts and ends.
//!
//! # Element numbering
//!
//! CoreAudio numbers the buses from the hardware's point of view, which reads
//! backwards at first:
//!
//! - Element 1 is the microphone. Its **output** scope is what we read.
//! - Element 0 is the speaker. Its **input** scope is what we write.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use coreaudio::audio_unit::render_callback::{self, data};
use coreaudio::audio_unit::{AudioUnit, Element, IOType, SampleFormat, Scope, StreamFormat};
use objc2_audio_toolbox::{
    kAUVoiceIOProperty_BypassVoiceProcessing, kAUVoiceIOProperty_VoiceProcessingEnableAGC,
    kAudioOutputUnitProperty_CurrentDevice, kAudioOutputUnitProperty_EnableIO,
};
use tracing::{debug, info, warn};

use crate::config::{SAMPLE_RATE, SPEAKING_THRESHOLD};
use crate::error::{Error, Result};

use super::capture::{CaptureShared, Encoder, OutgoingFrame};
use super::chirp::ChirpPlayer;
use super::packet::{PacketConsumer, SlotTable};
use super::playback::Mixer;
use super::resample::Resampler;

/// A running Voice Processing unit. Dropping it stops both directions.
pub struct VoiceUnit {
    unit: AudioUnit,
    /// What the unit reported once it was configured. It does not always accept
    /// the rate it is asked for.
    pub input_rate: u32,
    pub output_rate: u32,
}

impl Drop for VoiceUnit {
    fn drop(&mut self) {
        // Stopping before the callbacks are freed avoids a callback firing into
        // state that is going away.
        let _ = self.unit.stop();
    }
}

/// Which devices the unit should use.
#[derive(Debug, Clone, Copy, Default)]
pub struct Devices {
    pub input: Option<u32>,
    pub output: Option<u32>,
}

/// Builds and starts the unit.
///
/// `mixer` feeds the speaker. `encoder` receives the echo-cancelled microphone.
pub fn start(
    devices: Devices,
    mut mixer: Mixer,
    mut encoder: Encoder,
    shared: Arc<CaptureShared>,
) -> Result<VoiceUnit> {
    let mut unit = AudioUnit::new_uninitialized(IOType::VoiceProcessingIO).map_err(Error::audio)?;

    enable_io(&mut unit)?;
    set_devices(&mut unit, devices);

    // Voice processing on, gain control off. The canceller and the noise
    // suppressor are the point. Automatic gain makes a familiar voice keep
    // changing level, which is worse than a quiet friend.
    set_flag(&mut unit, kAUVoiceIOProperty_BypassVoiceProcessing, false);
    set_flag(
        &mut unit,
        kAUVoiceIOProperty_VoiceProcessingEnableAGC,
        false,
    );

    let format = client_format();
    unit.set_property(
        objc2_audio_toolbox::kAudioUnitProperty_StreamFormat,
        Scope::Output,
        Element::Input,
        Some(&format.to_asbd()),
    )
    .map_err(Error::audio)?;

    unit.set_property(
        objc2_audio_toolbox::kAudioUnitProperty_StreamFormat,
        Scope::Input,
        Element::Output,
        Some(&format.to_asbd()),
    )
    .map_err(Error::audio)?;

    // Read back what was actually accepted. The unit may refuse 48 kHz and
    // choose the hardware rate instead, and assuming otherwise is the exact
    // mistake that made playback run slow before T-052.
    let input_rate = actual_rate(&unit, Scope::Output, Element::Input).unwrap_or(SAMPLE_RATE);
    let output_rate = actual_rate(&unit, Scope::Input, Element::Output).unwrap_or(SAMPLE_RATE);

    if input_rate != SAMPLE_RATE || output_rate != SAMPLE_RATE {
        info!(
            input_rate,
            output_rate, "the voice unit did not take 48 kHz, so its audio is converted"
        );
    }

    let mut to_codec = Resampler::new(input_rate, SAMPLE_RATE);
    let mut to_device = Resampler::new(SAMPLE_RATE, output_rate);

    // --- the microphone side, already echo cancelled ---
    let capture_shared = shared.clone();
    unit.set_input_callback(move |args: render_callback::Args<data::Interleaved<f32>>| {
        let data::Interleaved {
            buffer, channels, ..
        } = args.data;
        let channels = channels.max(1);

        if !capture_shared.transmitting.load(Ordering::Relaxed) {
            encoder.reset();
            to_codec.reset();
            capture_shared.peak_bits.store(0, Ordering::Relaxed);
            capture_shared.speaking.store(false, Ordering::Relaxed);
            return Ok(());
        }

        let mut frames = buffer.chunks(channels);
        let mut next_mono = || {
            let chunk = frames.next()?;
            let sum: f32 = chunk.iter().sum();
            Some(sum / chunk.len() as f32)
        };

        let mut peak = 0f32;
        while let Some(mono) = to_codec.next(&mut next_mono) {
            let magnitude = mono.abs();
            if magnitude > peak {
                peak = magnitude;
            }
            encoder.push(mono);
        }

        capture_shared
            .peak_bits
            .store(peak.to_bits(), Ordering::Relaxed);
        capture_shared
            .speaking
            .store(peak > SPEAKING_THRESHOLD, Ordering::Relaxed);
        Ok(())
    })
    .map_err(Error::audio)?;

    // --- the speaker side, which is also the canceller's reference ---
    unit.set_render_callback(move |args: render_callback::Args<data::Interleaved<f32>>| {
        let data::Interleaved {
            buffer, channels, ..
        } = args.data;
        let channels = channels.max(1);

        for frame in buffer.chunks_mut(channels) {
            let mut source = || Some(mixer.next_sample());
            let sample = to_device.next(&mut source).unwrap_or(0.0);
            for slot in frame.iter_mut() {
                *slot = sample;
            }
        }
        Ok(())
    })
    .map_err(Error::audio)?;

    unit.initialize().map_err(Error::audio)?;
    unit.start().map_err(Error::audio)?;

    info!("echo cancellation is on");

    Ok(VoiceUnit {
        unit,
        input_rate,
        output_rate,
    })
}

/// Turns on both directions.
///
/// This must happen before the unit is initialised. The Voice Processing unit
/// has output enabled and input disabled by default, exactly like the plain
/// output unit it is derived from.
fn enable_io(unit: &mut AudioUnit) -> Result<()> {
    let on: u32 = 1;

    unit.set_property(
        kAudioOutputUnitProperty_EnableIO,
        Scope::Input,
        Element::Input,
        Some(&on),
    )
    .map_err(|e| Error::Audio(format!("the voice unit refused the microphone: {e}")))?;

    unit.set_property(
        kAudioOutputUnitProperty_EnableIO,
        Scope::Output,
        Element::Output,
        Some(&on),
    )
    .map_err(|e| Error::Audio(format!("the voice unit refused the speaker: {e}")))?;

    Ok(())
}

/// Points the unit at the chosen devices.
///
/// A failure here is not fatal. The unit falls back to the system default,
/// which is still better than no echo cancellation.
fn set_devices(unit: &mut AudioUnit, devices: Devices) {
    if let Some(id) = devices.input
        && let Err(e) = unit.set_property(
            kAudioOutputUnitProperty_CurrentDevice,
            Scope::Global,
            Element::Input,
            Some(&id),
        )
    {
        debug!("the voice unit kept its own microphone: {e}");
    }

    if let Some(id) = devices.output
        && let Err(e) = unit.set_property(
            kAudioOutputUnitProperty_CurrentDevice,
            Scope::Global,
            Element::Output,
            Some(&id),
        )
    {
        debug!("the voice unit kept its own speaker: {e}");
    }
}

fn set_flag(unit: &mut AudioUnit, property: u32, on: bool) {
    let value: u32 = u32::from(on);
    if let Err(e) = unit.set_property(property, Scope::Global, Element::Output, Some(&value)) {
        warn!("the voice unit refused property {property}: {e}");
    }
}

/// The format we want on both sides: mono float at 48 kHz.
fn client_format() -> StreamFormat {
    StreamFormat {
        sample_rate: SAMPLE_RATE as f64,
        sample_format: SampleFormat::F32,
        flags: coreaudio::audio_unit::audio_format::LinearPcmFlags::IS_FLOAT
            | coreaudio::audio_unit::audio_format::LinearPcmFlags::IS_PACKED,
        channels: 1,
    }
}

/// Reads back the rate the unit actually settled on.
fn actual_rate(unit: &AudioUnit, scope: Scope, element: Element) -> Option<u32> {
    unit.stream_format(scope, element)
        .ok()
        .map(|format| format.sample_rate.round() as u32)
}

/// Looks up the CoreAudio device id for a device name.
pub fn device_id(name: &str, input: bool) -> Option<u32> {
    coreaudio::audio_unit::macos_helpers::get_device_id_from_name(name, input)
}

/// The default device id for a direction.
pub fn default_device_id(input: bool) -> Option<u32> {
    coreaudio::audio_unit::macos_helpers::get_default_device_id(input)
}

/// Resolves the chosen device names to ids.
pub fn resolve(preference: &super::DevicePreference) -> Devices {
    Devices {
        input: preference
            .input
            .as_deref()
            .and_then(|name| device_id(name, true)),
        output: preference
            .output
            .as_deref()
            .and_then(|name| device_id(name, false)),
    }
}

/// Consumers are needed to build a mixer, so this bundles the pieces the audio
/// thread has to hand over when it switches to the voice unit.
pub struct Parts {
    pub table: Arc<SlotTable>,
    pub consumers: Vec<PacketConsumer>,
    pub chirps: Arc<ChirpPlayer>,
    pub shared: Arc<CaptureShared>,
    pub producer: ringbuf::HeapProd<OutgoingFrame>,
}
