//! Capture and encode.
//!
//! The Opus encode runs **inside** the CoreAudio input callback. That is
//! unusual, so here is the reason.
//!
//! The alternative is a ring of raw samples and a separate encoder thread. That
//! costs one thread hand-off, and the hand-off has to be woken, which adds
//! anything from a few hundred microseconds to a whole scheduler quantum. An
//! Opus encode of a 10 ms frame at complexity 8 costs about 0.3 ms, against a
//! callback budget of 5.3 ms. libopus allocates its state once at creation and
//! never again, so the encode itself obeys the real-time rules in
//! `ARCHITECTURE.md` §2.1.
//!
//! Encoding in the callback is therefore both cheaper and more predictable.
//! What the callback must **not** do is send. Sending touches QUIC state and
//! allocates, so the encoded frame goes into a lock-free queue and a normal
//! thread does the network work.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use cpal::traits::DeviceTrait;
use cpal::{Stream, StreamConfig};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use crate::config::{
    FRAME_SAMPLES, MAX_PACKET_BYTES, OPUS_BITRATE, OPUS_COMPLEXITY, OPUS_EXPECTED_LOSS,
    PEER_QUEUE_PACKETS, SAMPLE_RATE, SPEAKING_THRESHOLD,
};
use crate::error::{Error, Result};
use crate::net::audio_wire::{AudioPacket, FLAG_TALKSPURT_START, HEADER_LEN};

use super::device::Chosen;
use super::resample::Resampler;

/// One encoded frame, ready for the network.
#[derive(Clone, Copy)]
pub struct OutgoingFrame {
    len: u16,
    bytes: [u8; HEADER_LEN + MAX_PACKET_BYTES],
}

impl OutgoingFrame {
    fn empty() -> Self {
        OutgoingFrame {
            len: 0,
            bytes: [0; HEADER_LEN + MAX_PACKET_BYTES],
        }
    }

    /// The complete datagram, header and payload.
    pub fn wire(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

/// State the capture callback shares with the rest of the program.
///
/// Every field is atomic. The callback never takes a lock.
pub struct CaptureShared {
    /// The microphone is open. When false the callback discards its input and
    /// does no work at all.
    pub transmitting: AtomicBool,

    /// The peak level of the last frame, as `f32` bits. Drives the meter.
    peak_bits: AtomicU32,

    /// True while the local level is above the speaking threshold.
    pub speaking: AtomicBool,

    /// Frames dropped because the send queue was full.
    pub dropped: AtomicU64,

    /// Frames encoded since the stream opened.
    pub encoded: AtomicU64,

    /// Callbacks that could not encode. A non-zero value means a real fault.
    pub encode_errors: AtomicU64,
}

impl CaptureShared {
    /// Creates the shared state.
    ///
    /// It outlives any single device open, so a caller can hold it across an
    /// arm and disarm cycle and keep its counters.
    pub fn new_shared() -> Self {
        CaptureShared {
            transmitting: AtomicBool::new(false),
            peak_bits: AtomicU32::new(0),
            speaking: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            encoded: AtomicU64::new(0),
            encode_errors: AtomicU64::new(0),
        }
    }

    /// The peak level of the most recent frame, from 0.0 to 1.0.
    pub fn peak(&self) -> f32 {
        f32::from_bits(self.peak_bits.load(Ordering::Relaxed))
    }
}

/// The running capture stream. Dropping it stops the microphone.
pub struct Capture {
    /// `cpal::Stream` is not `Send` on macOS, so it stays on the thread that
    /// built it. The engine keeps that thread alive.
    _stream: Stream,
    pub shared: Arc<CaptureShared>,
}

/// Opens the microphone and starts encoding.
///
/// The returned consumer carries encoded frames to the sender thread.
pub fn open(
    chosen: &Chosen,
    shared: Arc<CaptureShared>,
) -> Result<(Capture, HeapCons<OutgoingFrame>)> {
    let (producer, consumer) = HeapRb::<OutgoingFrame>::new(PEER_QUEUE_PACKETS).split();

    let stream = build_stream(chosen, shared.clone(), producer)?;
    cpal::traits::StreamTrait::play(&stream).map_err(Error::audio)?;

    Ok((
        Capture {
            _stream: stream,
            shared,
        },
        consumer,
    ))
}

fn build_stream(
    chosen: &Chosen,
    shared: Arc<CaptureShared>,
    mut producer: HeapProd<OutgoingFrame>,
) -> Result<Stream> {
    let config: StreamConfig = chosen.config;
    let channels = config.channels.max(1) as usize;

    let mut encoder = opus::Encoder::new(
        SAMPLE_RATE,
        opus::Channels::Mono,
        // `Audio` rather than `Voip`. The goal is presence, not intelligibility
        // at a low bitrate. `Voip` applies speech shaping that makes a voice
        // sound processed.
        opus::Application::Audio,
    )
    .map_err(Error::audio)?;

    encoder
        .set_bitrate(opus::Bitrate::Bits(OPUS_BITRATE))
        .map_err(Error::audio)?;
    encoder
        .set_complexity(OPUS_COMPLEXITY)
        .map_err(Error::audio)?;
    // In-band forward error correction. A lost frame is rebuilt from the next
    // packet. A real-time link has no time to ask for a retransmission.
    encoder.set_inband_fec(true).map_err(Error::audio)?;
    encoder
        .set_packet_loss_perc(OPUS_EXPECTED_LOSS)
        .map_err(Error::audio)?;

    // The codec is always fed 48 kHz. A microphone that runs at another rate,
    // which most Bluetooth headsets do, is converted first. Without this the
    // encoder is handed the wrong number of samples per second and the far end
    // hears a pitch shift.
    let mut resampler = Resampler::new(config.sample_rate, SAMPLE_RATE);
    if !resampler.is_passthrough() {
        tracing::info!(
            device_rate = config.sample_rate,
            "the microphone does not run at 48 kHz, so the input is converted"
        );
    }

    // Everything the callback touches is allocated here, before the stream
    // starts. Nothing below this line allocates.
    let mut accumulator = vec![0f32; FRAME_SAMPLES];
    let mut filled = 0usize;
    let mut payload = vec![0u8; MAX_PACKET_BYTES];
    let mut frame = OutgoingFrame::empty();
    let mut seq: u16 = 0;
    let mut timestamp: u32 = 0;
    let mut was_transmitting = false;

    let callback_shared = shared.clone();

    let stream = chosen
        .device
        .build_input_stream::<f32, _, _>(
            config,
            move |input: &[f32], _info| {
                let transmitting = callback_shared.transmitting.load(Ordering::Relaxed);

                if !transmitting {
                    // Closed microphone. Forget any partial frame and the
                    // converter's history, so the next talkspurt starts clean
                    // rather than with samples from before the pause.
                    filled = 0;
                    resampler.reset();
                    was_transmitting = false;
                    callback_shared.peak_bits.store(0, Ordering::Relaxed);
                    callback_shared.speaking.store(false, Ordering::Relaxed);
                    return;
                }

                let mut peak = 0f32;

                // Downmix to mono as the samples are pulled. The wire format is
                // mono, and a stereo microphone carries nothing a conversation
                // needs.
                let mut frames = input.chunks(channels);
                let mut next_mono = || {
                    let chunk = frames.next()?;
                    let sum: f32 = chunk.iter().sum();
                    Some(sum / chunk.len() as f32)
                };

                while let Some(mono) = resampler.next(&mut next_mono) {
                    let magnitude = mono.abs();
                    if magnitude > peak {
                        peak = magnitude;
                    }

                    accumulator[filled] = mono;
                    filled += 1;

                    if filled < FRAME_SAMPLES {
                        continue;
                    }
                    filled = 0;

                    let encoded = match encoder.encode_float(&accumulator, &mut payload) {
                        Ok(n) => n,
                        Err(_) => {
                            callback_shared
                                .encode_errors
                                .fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    };

                    let flags = if was_transmitting {
                        0
                    } else {
                        // The first frame after silence. The receiver uses this
                        // to restart its jitter buffer instead of concealing
                        // the pause.
                        FLAG_TALKSPURT_START
                    };
                    was_transmitting = true;

                    match AudioPacket::encode_into(
                        seq,
                        timestamp,
                        flags,
                        &payload[..encoded],
                        &mut frame.bytes,
                    ) {
                        Ok(total) => frame.len = total as u16,
                        Err(_) => {
                            callback_shared
                                .encode_errors
                                .fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    }

                    seq = seq.wrapping_add(1);
                    timestamp = timestamp.wrapping_add(FRAME_SAMPLES as u32);

                    if producer.try_push(frame).is_err() {
                        // The sender thread is behind. Dropping the frame is
                        // correct: holding it would only make the audio later.
                        callback_shared.dropped.fetch_add(1, Ordering::Relaxed);
                    }

                    callback_shared.encoded.fetch_add(1, Ordering::Relaxed);
                }

                callback_shared
                    .peak_bits
                    .store(peak.to_bits(), Ordering::Relaxed);
                callback_shared
                    .speaking
                    .store(peak > SPEAKING_THRESHOLD, Ordering::Relaxed);
            },
            |err| {
                // The error callback is not real-time. Logging here is allowed.
                tracing::warn!("the microphone stream reported an error: {err}");
            },
            None,
        )
        .map_err(Error::audio)?;

    Ok(stream)
}

/// Drains encoded frames and hands them to the network.
///
/// This runs on a normal thread. It may allocate and it may block.
pub fn sender_loop(
    mut consumer: HeapCons<OutgoingFrame>,
    tx: Arc<dyn super::AudioTx>,
    running: Arc<AtomicBool>,
) {
    while running.load(Ordering::Relaxed) {
        let mut sent_any = false;

        while let Some(frame) = consumer.try_pop() {
            tx.send_frame(frame.wire());
            sent_any = true;
        }

        if !sent_any {
            // A short park rather than a spin. The capture callback produces a
            // frame every 10 ms, so this wakes at worst 1 ms late.
            std::thread::park_timeout(std::time::Duration::from_millis(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::audio_wire::AudioPacket;

    #[test]
    fn an_outgoing_frame_holds_a_full_packet() {
        let mut frame = OutgoingFrame::empty();
        let payload = vec![7u8; MAX_PACKET_BYTES];

        let n = AudioPacket::encode_into(1, 480, FLAG_TALKSPURT_START, &payload, &mut frame.bytes)
            .expect("the buffer must fit the largest packet the codec can make");
        frame.len = n as u16;

        let parsed = AudioPacket::decode(frame.wire()).unwrap();
        assert_eq!(parsed.seq, 1);
        assert_eq!(parsed.payload.len(), MAX_PACKET_BYTES);
        assert!(parsed.is_talkspurt_start());
    }

    #[test]
    fn the_encoder_settings_produce_a_small_packet() {
        // This is the measurement the whole bandwidth budget rests on. If a
        // change to the settings breaks it, the budget in ARCHITECTURE.md §4.5
        // is wrong.
        let mut encoder =
            opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Audio)
                .unwrap();
        encoder
            .set_bitrate(opus::Bitrate::Bits(OPUS_BITRATE))
            .unwrap();
        encoder.set_complexity(OPUS_COMPLEXITY).unwrap();
        encoder.set_inband_fec(true).unwrap();
        encoder.set_packet_loss_perc(OPUS_EXPECTED_LOSS).unwrap();

        let frame: Vec<f32> = (0..FRAME_SAMPLES)
            .map(|i| (i as f32 * 0.05).sin() * 0.4)
            .collect();

        let mut out = vec![0u8; MAX_PACKET_BYTES];
        let n = encoder.encode_float(&frame, &mut out).unwrap();

        assert!(n > 0);
        assert!(
            n <= 200,
            "a 10 ms frame encoded to {n} bytes, and the budget assumes about 72"
        );
    }
}
