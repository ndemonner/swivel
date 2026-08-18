//! Playback, decode, and mix.
//!
//! Decode and mix run inside the CoreAudio output callback. Doing it here
//! rather than on a timer thread removes one buffer of latency and one thread
//! hand-off, and the callback is the only place that knows the exact playback
//! time. See `ARCHITECTURE.md` §5.3.
//!
//! Nothing in the callback allocates. Every decoder, buffer, and slot is
//! created before the stream starts.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use cpal::traits::DeviceTrait;
use cpal::{Stream, StreamConfig};
use ringbuf::traits::Consumer;

use crate::config::{
    CONCEAL_LIMIT, FRAME_SAMPLES, LIMITER_CEILING, LIMITER_THRESHOLD, MAX_PEERS, PEER_GAIN,
    SAMPLE_RATE,
};
use crate::error::{Error, Result};

use super::chirp::ChirpPlayer;
use super::jitter::{Buffer, Decision};
use super::packet::{PacketConsumer, SlotTable, drain};

/// Everything one peer needs on the callback side.
///
/// This is owned by the callback and never shared, so no field needs a lock.
struct PlaybackSlot {
    decoder: opus::Decoder,
    consumer: PacketConsumer,
    buffer: Buffer,
    /// The generation this slot last saw. A change means the slot changed
    /// owner, so the decoder and the buffer must be reset.
    generation: u32,
    /// Decoded samples for the current frame.
    pcm: Vec<f32>,
}

/// The running output stream. Dropping it stops playback.
pub struct Playback {
    /// `cpal::Stream` is not `Send` on macOS. It stays on its own thread.
    _stream: Stream,
    pub level: Arc<AtomicU32>,
}

impl Playback {
    /// The peak level of the last mixed frame, from 0.0 to 1.0.
    pub fn peak(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
}

/// Opens the speaker and starts mixing.
pub fn open(
    chosen: &super::device::Chosen,
    slots: Arc<SlotTable>,
    consumers: Vec<PacketConsumer>,
    chirps: Arc<ChirpPlayer>,
) -> Result<Playback> {
    let config: StreamConfig = chosen.config;
    let channels = config.channels.max(1) as usize;
    let level = Arc::new(AtomicU32::new(0));

    if consumers.len() != MAX_PEERS {
        return Err(Error::Audio(format!(
            "expected {MAX_PEERS} peer queues and got {}",
            consumers.len()
        )));
    }

    // Build every slot before the stream starts. This is the allocation that
    // the callback then never has to make.
    let mut playback_slots = Vec::with_capacity(MAX_PEERS);
    for consumer in consumers {
        playback_slots.push(PlaybackSlot {
            decoder: opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono).map_err(Error::audio)?,
            consumer,
            buffer: Buffer::new(),
            generation: 0,
            pcm: vec![0f32; FRAME_SAMPLES],
        });
    }

    let mut mix = vec![0f32; FRAME_SAMPLES];
    // How much of `mix` has already been written to the device. A device buffer
    // is rarely a whole number of 10 ms frames, so a frame is carried across
    // callbacks.
    let mut mix_position = FRAME_SAMPLES;

    let callback_level = level.clone();

    let stream = chosen
        .device
        .build_output_stream::<f32, _, _>(
            config,
            move |output: &mut [f32], _info| {
                let mut peak = 0f32;

                for out_frame in output.chunks_mut(channels) {
                    if mix_position >= FRAME_SAMPLES {
                        mix_frame(&mut playback_slots, &slots, &chirps, &mut mix);
                        mix_position = 0;
                    }

                    let sample = mix[mix_position];
                    mix_position += 1;

                    let magnitude = sample.abs();
                    if magnitude > peak {
                        peak = magnitude;
                    }

                    // Mono to every channel. A conversation has no useful
                    // stereo image, and duplicating costs nothing.
                    for slot in out_frame.iter_mut() {
                        *slot = sample;
                    }
                }

                callback_level.store(peak.to_bits(), Ordering::Relaxed);
            },
            |err| {
                tracing::warn!("the speaker stream reported an error: {err}");
            },
            None,
        )
        .map_err(Error::audio)?;

    cpal::traits::StreamTrait::play(&stream).map_err(Error::audio)?;

    Ok(Playback {
        _stream: stream,
        level,
    })
}

/// Produces one 10 ms frame of mixed audio.
///
/// This is the whole real-time path. Read `ARCHITECTURE.md` §2.1 before you add
/// anything to it.
fn mix_frame(
    playback: &mut [PlaybackSlot],
    slots: &SlotTable,
    chirps: &ChirpPlayer,
    mix: &mut [f32],
) {
    mix.fill(0.0);

    for (index, slot) in playback.iter_mut().enumerate() {
        let shared = slots.slot(index);

        // A generation change means the slot changed owner. Throw away the old
        // stream rather than decode one peer's audio with another's state.
        let generation = shared.generation.load(Ordering::Acquire);
        if generation != slot.generation {
            slot.generation = generation;
            slot.buffer.reset();
            drain(&mut slot.consumer);
            // The decoder keeps state from the previous stream. Resetting it
            // avoids a burst of noise on the first frame of the new one.
            let _ = slot.decoder.reset_state();
        }

        if !shared.active.load(Ordering::Acquire) {
            continue;
        }

        // Move everything the network delivered into the reorder buffer.
        while let Some(packet) = slot.consumer.try_pop() {
            if !slot.buffer.insert(packet) {
                shared.late.fetch_add(1, Ordering::Relaxed);
            }
        }

        let target = shared.target_frames.load(Ordering::Relaxed) as usize;
        let (decision, packet) = slot.buffer.take(target);

        let decoded = match decision {
            Decision::Prime => continue,

            Decision::Play => {
                let Some(packet) = packet else { continue };
                slot.decoder
                    .decode_float(packet.payload(), &mut slot.pcm, false)
            }

            Decision::Conceal => {
                shared.concealed.fetch_add(1, Ordering::Relaxed);

                if slot.buffer.conceal_run() > CONCEAL_LIMIT {
                    // A long silence is not a glitch. Stop inventing audio.
                    continue;
                }

                match slot.buffer.peek_next() {
                    // The next packet carries in-band forward error correction
                    // for the frame that went missing. Rebuild it from there.
                    // This recovers a real gap, where concealment only hides
                    // one.
                    Some(next) => {
                        let payload = next.payload();
                        slot.decoder.decode_float(payload, &mut slot.pcm, true)
                    }
                    // Nothing to rebuild from. Let Opus conceal.
                    None => slot.decoder.decode_float(&[], &mut slot.pcm, false),
                }
            }
        };

        let Ok(samples) = decoded else { continue };
        let samples = samples.min(FRAME_SAMPLES);

        for (out, decoded) in mix.iter_mut().zip(&slot.pcm[..samples]) {
            *out += decoded * PEER_GAIN;
        }
    }

    chirps.mix_into(mix);
    limit(mix);
}

/// A soft limiter.
///
/// Eight peers can sum past full scale. Hard clipping sounds like a fault, and
/// a user hearing a fault stops trusting the tool. This curve leaves everything
/// below the threshold untouched and bends the rest.
fn limit(mix: &mut [f32]) {
    for sample in mix.iter_mut() {
        let magnitude = sample.abs();
        if magnitude <= LIMITER_THRESHOLD {
            continue;
        }

        let over = magnitude - LIMITER_THRESHOLD;
        let headroom = LIMITER_CEILING - LIMITER_THRESHOLD;
        // tanh compresses everything above the threshold into the headroom, so
        // however loud the input is the result approaches the ceiling and never
        // passes it.
        let shaped = LIMITER_THRESHOLD + headroom * (over / headroom).tanh();
        *sample = shaped.copysign(*sample);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limiter_leaves_quiet_audio_alone() {
        let mut mix = vec![0.0, 0.25, -0.5, LIMITER_THRESHOLD];
        let before = mix.clone();
        limit(&mut mix);
        assert_eq!(mix, before);
    }

    #[test]
    fn the_limiter_never_reaches_full_scale() {
        let mut mix: Vec<f32> = vec![1.0, 2.0, 8.0, -1.0, -4.0, 100.0];
        limit(&mut mix);

        for sample in &mix {
            assert!(
                sample.abs() <= LIMITER_CEILING,
                "{sample} passed the ceiling, which clips"
            );
        }
    }

    #[test]
    fn the_limiter_keeps_the_sign() {
        let mut mix = vec![3.0, -3.0];
        limit(&mut mix);
        assert!(mix[0] > 0.0);
        assert!(mix[1] < 0.0);
        assert_eq!(mix[0], -mix[1]);
    }

    #[test]
    fn the_limiter_is_monotonic() {
        // A louder input must never come out quieter, or the sound pumps.
        let mut previous = 0f32;
        for step in 0..200 {
            let input = step as f32 * 0.05;
            let mut mix = vec![input];
            limit(&mut mix);
            assert!(
                mix[0] >= previous,
                "input {input} came out below the previous step"
            );
            previous = mix[0];
        }
    }

    #[test]
    fn eight_peers_at_full_level_stay_inside_full_scale() {
        // The worst realistic case: every peer at full scale, in phase.
        let mut mix = vec![MAX_PEERS as f32 * PEER_GAIN; FRAME_SAMPLES];
        limit(&mut mix);
        assert!(mix.iter().all(|s| s.abs() <= LIMITER_CEILING));
        assert!(mix.iter().all(|s| s.abs() < 1.0));
    }
}
