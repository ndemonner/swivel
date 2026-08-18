//! Fault tones.
//!
//! There is deliberately **no tone when the microphone opens or closes**. The
//! product is an intercom, and an intercom that beeps every time somebody
//! speaks is a worse intercom. Opening a microphone must feel seamless. The
//! menu bar state and the mute control carry that job instead. See
//! `DESIGN.md` §7.
//!
//! What is left is the fault tone. It plays when the audio devices fail, which
//! is the one case where the interface cannot be trusted to be visible: a user
//! whose microphone never opened would otherwise talk into nothing.
//!
//! The tone is mixed into the local output only. It is never transmitted.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::config::SAMPLE_RATE;

/// Which tone to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chirp {
    /// The audio devices failed. Two notes, falling.
    Fault,
}

impl Chirp {
    fn tones(&self) -> [(f32, f32); 2] {
        // (frequency in hertz, length in seconds)
        match self {
            Chirp::Fault => [(400.0, 0.09), (300.0, 0.12)],
        }
    }
}

/// The peak level of a chirp. Loud enough to notice, quiet enough not to
/// startle someone wearing headphones.
const CHIRP_LEVEL: f32 = 0.22;

/// Generates and mixes confirmation tones.
///
/// The rendered samples are built on a normal thread and handed to the callback
/// through a lock that the callback only ever tries to take. If the lock is
/// held, the callback skips the chirp for that frame rather than waiting.
pub struct ChirpPlayer {
    /// The rendered tone, and how far through it playback has reached.
    state: Mutex<ChirpState>,
    /// Set by `play`, cleared by the callback. Lets the callback skip the lock
    /// entirely in the common case where no chirp is playing.
    pending: AtomicU8,
}

#[derive(Default)]
struct ChirpState {
    samples: Vec<f32>,
    position: usize,
}

const NONE: u8 = 0;
const PLAYING: u8 = 1;

impl ChirpPlayer {
    pub fn new() -> Self {
        ChirpPlayer {
            state: Mutex::new(ChirpState::default()),
            pending: AtomicU8::new(NONE),
        }
    }

    /// Queues a tone. Called from a normal thread.
    ///
    /// A tone that is already playing is replaced. Two tones in quick
    /// succession say less than one clear tone.
    pub fn play(&self, chirp: Chirp) {
        let samples = render(chirp);

        if let Ok(mut state) = self.state.lock() {
            state.samples = samples;
            state.position = 0;
            self.pending.store(PLAYING, Ordering::Release);
        }
    }

    /// Adds any playing tone to the mix. Called from the output callback.
    ///
    /// This is the one place in the real-time path that takes a lock, and it
    /// uses `try_lock`, so it can never wait on a slower thread. A missed chirp
    /// frame is inaudible. A blocked callback is a dropout.
    pub fn mix_into(&self, mix: &mut [f32]) {
        if self.pending.load(Ordering::Acquire) == NONE {
            return;
        }

        let Ok(mut state) = self.state.try_lock() else {
            return;
        };

        let remaining = state.samples.len().saturating_sub(state.position);
        if remaining == 0 {
            self.pending.store(NONE, Ordering::Release);
            return;
        }

        let count = remaining.min(mix.len());
        let start = state.position;

        for (out, tone) in mix.iter_mut().zip(&state.samples[start..start + count]) {
            *out += tone;
        }
        state.position += count;

        if state.position >= state.samples.len() {
            self.pending.store(NONE, Ordering::Release);
        }
    }

    /// True while a tone is playing.
    pub fn is_playing(&self) -> bool {
        self.pending.load(Ordering::Acquire) != NONE
    }
}

impl Default for ChirpPlayer {
    fn default() -> Self {
        Self::new()
    }
}

/// Renders a chirp to samples.
///
/// Each note gets a short raised-cosine envelope. Without it the note starts
/// and ends with a click, and a click is exactly the artefact that makes a tool
/// sound broken.
fn render(chirp: Chirp) -> Vec<f32> {
    let rate = SAMPLE_RATE as f32;
    let mut out = Vec::new();

    for (frequency, seconds) in chirp.tones() {
        let count = (seconds * rate) as usize;
        let ramp = (count / 8).max(1);

        for i in 0..count {
            let t = i as f32 / rate;
            let envelope = if i < ramp {
                i as f32 / ramp as f32
            } else if i >= count - ramp {
                (count - i) as f32 / ramp as f32
            } else {
                1.0
            };
            // A raised cosine, not a straight line, so the corners are smooth.
            let envelope = 0.5 - 0.5 * (std::f32::consts::PI * envelope).cos();
            let envelope = envelope.clamp(0.0, 1.0);

            out.push((std::f32::consts::TAU * frequency * t).sin() * envelope * CHIRP_LEVEL);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chirp_is_short_and_quiet() {
        let chirp = Chirp::Fault;
        let samples = render(chirp);
        let seconds = samples.len() as f32 / SAMPLE_RATE as f32;

        assert!(
            (0.05..0.35).contains(&seconds),
            "{chirp:?} lasts {seconds} seconds"
        );
        assert!(
            samples.iter().all(|s| s.abs() <= CHIRP_LEVEL + 1e-6),
            "{chirp:?} is louder than the limit"
        );
    }

    #[test]
    fn a_chirp_starts_and_ends_at_silence() {
        // A non-zero first or last sample is a click.
        let samples = render(Chirp::Fault);
        assert!(samples[0].abs() < 1e-3, "the tone starts with a click");
        assert!(
            samples[samples.len() - 1].abs() < 1e-3,
            "the tone ends with a click"
        );
    }

    #[test]
    fn a_chirp_plays_once_and_stops() {
        let player = ChirpPlayer::new();
        assert!(!player.is_playing());

        player.play(Chirp::Fault);
        assert!(player.is_playing());

        let mut heard = 0f32;
        let mut mix = vec![0f32; crate::config::FRAME_SAMPLES];
        for _ in 0..200 {
            mix.fill(0.0);
            player.mix_into(&mut mix);
            heard += mix.iter().map(|s| s.abs()).sum::<f32>();
            if !player.is_playing() {
                break;
            }
        }

        assert!(heard > 0.0, "the chirp produced no sound");
        assert!(!player.is_playing(), "the chirp did not finish");

        // Once finished it must stay silent.
        mix.fill(0.0);
        player.mix_into(&mut mix);
        assert!(mix.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn mixing_adds_rather_than_replaces() {
        let player = ChirpPlayer::new();
        player.play(Chirp::Fault);

        let mut mix = vec![0.5f32; crate::config::FRAME_SAMPLES];
        player.mix_into(&mut mix);

        // The existing audio must still be there underneath.
        assert!(mix.iter().all(|s| *s > 0.0));
    }

    #[test]
    fn a_silent_player_costs_nothing() {
        let player = ChirpPlayer::new();
        let mut mix = vec![0.25f32; 480];
        let before = mix.clone();
        player.mix_into(&mut mix);
        assert_eq!(mix, before);
    }
}
