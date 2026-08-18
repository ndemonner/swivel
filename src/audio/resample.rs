//! Sample rate conversion.
//!
//! Everything in `swivel` runs at 48 kHz. Plenty of devices do not. A Bluetooth
//! headset commonly runs at 44.1 kHz, and feeding it 48 kHz audio without
//! converting plays everything about 8 percent slow and drops one frame in
//! twelve. That is not a subtle fault, and it is the default experience for
//! anyone on wireless headphones.
//!
//! The converter is a Catmull-Rom cubic. It is not the best resampler that
//! exists, and `rubato` would be better, but it has three properties that
//! matter more here:
//!
//! 1. It allocates nothing, so it can run inside a CoreAudio callback.
//! 2. It costs a handful of multiplies per sample.
//! 3. It works one sample at a time, so it does not add a block of latency.
//!
//! A device that already runs at 48 kHz gets `Passthrough` and pays nothing.

/// Converts a stream from one rate to another, one sample at a time.
#[derive(Debug, Clone)]
pub struct Resampler {
    /// Input samples to advance per output sample.
    ///
    /// Below 1.0 means upsampling, above means downsampling.
    step: f64,
    /// Position between `history[1]` and `history[2]`, from 0.0 to 1.0.
    phase: f64,
    /// The four samples the cubic needs, oldest first.
    history: [f32; 4],
    /// How many samples have been fed. The first three prime the history.
    primed: usize,
    passthrough: bool,
}

impl Resampler {
    /// Builds a converter from `from` hertz to `to` hertz.
    pub fn new(from: u32, to: u32) -> Self {
        Resampler {
            step: from as f64 / to as f64,
            phase: 0.0,
            history: [0.0; 4],
            primed: 0,
            passthrough: from == to,
        }
    }

    /// True when the rates match and nothing is converted.
    pub fn is_passthrough(&self) -> bool {
        self.passthrough
    }

    /// Forgets the history. Use it when a stream restarts.
    pub fn reset(&mut self) {
        self.phase = 0.0;
        self.history = [0.0; 4];
        self.primed = 0;
    }

    /// Produces the next output sample, pulling input as it needs to.
    ///
    /// `src` returns the next input sample, or `None` when there is no more
    /// input right now. The converter keeps its position across calls, so a
    /// device buffer that ends part way through is resumed cleanly on the next
    /// callback.
    pub fn next(&mut self, src: &mut impl FnMut() -> Option<f32>) -> Option<f32> {
        if self.passthrough {
            return src();
        }

        // The cubic needs one sample behind and two ahead, so three arrive
        // before the first output can be produced.
        while self.primed < 3 {
            self.shift(src()?);
            self.primed += 1;
        }

        while self.phase >= 1.0 {
            self.shift(src()?);
            self.phase -= 1.0;
        }

        let out = catmull_rom(self.history, self.phase as f32);
        self.phase += self.step;
        Some(out)
    }

    fn shift(&mut self, sample: f32) {
        self.history[0] = self.history[1];
        self.history[1] = self.history[2];
        self.history[2] = self.history[3];
        self.history[3] = sample;
    }
}

/// A Catmull-Rom cubic through `h[1]` and `h[2]`, at `t` between them.
fn catmull_rom(h: [f32; 4], t: f32) -> f32 {
    let (p0, p1, p2, p3) = (h[0], h[1], h[2], h[3]);
    let t2 = t * t;
    let t3 = t2 * t;

    0.5 * ((2.0 * p1)
        + (-p0 + p2) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a whole input buffer through, collecting every output sample.
    fn convert(input: &[f32], from: u32, to: u32) -> Vec<f32> {
        let mut resampler = Resampler::new(from, to);
        let mut iter = input.iter().copied();
        let mut src = move || iter.next();

        let mut out = Vec::new();
        while let Some(sample) = resampler.next(&mut src) {
            out.push(sample);
        }
        out
    }

    #[test]
    fn matching_rates_pass_straight_through() {
        let input: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let out = convert(&input, 48_000, 48_000);
        assert_eq!(out, input);
        assert!(Resampler::new(48_000, 48_000).is_passthrough());
    }

    #[test]
    fn downsampling_produces_the_right_count() {
        // 48 kHz to 44.1 kHz keeps about 91.9 percent of the samples. This is
        // the exact case that made playback run slow.
        let input: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.001).sin()).collect();
        let out = convert(&input, 48_000, 44_100);

        let expected = 44_100.0;
        let ratio = out.len() as f64 / expected;
        assert!(
            (0.99..1.01).contains(&ratio),
            "expected about {expected} samples and produced {}",
            out.len()
        );
    }

    #[test]
    fn upsampling_produces_the_right_count() {
        let input: Vec<f32> = (0..44_100).map(|i| (i as f32 * 0.001).sin()).collect();
        let out = convert(&input, 44_100, 48_000);

        let ratio = out.len() as f64 / 48_000.0;
        assert!(
            (0.99..1.01).contains(&ratio),
            "expected about 48000 samples and produced {}",
            out.len()
        );
    }

    #[test]
    fn a_constant_stays_constant() {
        // A cubic through four equal points must give that value back. If it
        // does not, the interpolation weights are wrong and every signal gains
        // a ripple.
        let input = vec![0.5f32; 500];
        let out = convert(&input, 48_000, 44_100);

        for sample in out.iter().skip(4) {
            assert!(
                (sample - 0.5).abs() < 1e-5,
                "a constant became {sample}, so the weights do not sum to one"
            );
        }
    }

    #[test]
    fn a_ramp_stays_a_ramp() {
        // Catmull-Rom is exact for a straight line. A ramp that comes out bent
        // means the phase is advancing wrongly.
        let input: Vec<f32> = (0..200).map(|i| i as f32).collect();
        let out = convert(&input, 48_000, 24_000);

        // Two input samples per output, so output n lands on input 2n. The
        // first output is skipped: the history is still filling and it carries
        // a startup transient.
        for (n, sample) in out.iter().enumerate().skip(2).take(80) {
            let expected = n as f32 * 2.0;
            assert!(
                (sample - expected).abs() < 1e-3,
                "output {n} is {sample} and the straight line says {expected}"
            );
        }
    }

    #[test]
    fn a_sine_keeps_its_shape() {
        // A converted sine must stay inside its original range. A resampler
        // that overshoots is one that rings.
        let rate = 48_000.0;
        let input: Vec<f32> = (0..4_800)
            .map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / rate).sin())
            .collect();

        let out = convert(&input, 48_000, 44_100);

        let peak = out.iter().fold(0f32, |a, b| a.max(b.abs()));
        assert!(
            (0.95..1.05).contains(&peak),
            "a unit sine came out with a peak of {peak}"
        );
    }

    #[test]
    fn it_resumes_across_buffers() {
        // The real callbacks hand over a few hundred samples at a time. The
        // count over many small buffers must match one big one, or the streams
        // drift apart over a call.
        let input: Vec<f32> = (0..12_000).map(|i| (i as f32 * 0.01).sin()).collect();

        let one_pass = convert(&input, 48_000, 44_100).len();

        let mut resampler = Resampler::new(48_000, 44_100);
        let mut chunked = 0;
        for chunk in input.chunks(256) {
            let mut iter = chunk.iter().copied();
            let mut src = move || iter.next();
            while resampler.next(&mut src).is_some() {
                chunked += 1;
            }
        }

        let difference = (one_pass as i64 - chunked as i64).abs();
        assert!(
            difference <= 2,
            "one pass gave {one_pass} and chunked gave {chunked}"
        );
    }

    #[test]
    fn a_reset_clears_the_history() {
        let mut resampler = Resampler::new(48_000, 44_100);
        let mut iter = [1.0f32; 10].into_iter();
        let mut src = move || iter.next();
        resampler.next(&mut src);

        resampler.reset();
        assert_eq!(resampler.phase, 0.0);
        assert_eq!(resampler.history, [0.0; 4]);
    }
}
