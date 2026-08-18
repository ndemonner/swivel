//! The adaptive jitter buffer.
//!
//! A jitter buffer trades latency for smoothness. Too shallow and every network
//! hiccup is a gap. Too deep and the conversation stops feeling immediate. The
//! rule is in `ARCHITECTURE.md` §5.4: grow at once, shrink slowly.
//!
//! The work is split by thread. The **estimator** runs on the network task,
//! where measuring arrival time is safe. It publishes a target depth through an
//! atomic. The **buffer** runs inside the output callback, where it must not
//! allocate and must not lock.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::config::{
    FRAME_MS, JITTER_MAX_FRAMES, JITTER_MIN_FRAMES, JITTER_SHRINK_AFTER, JITTER_SLACK_FRAMES,
    JITTER_START_FRAMES,
};
use crate::net::audio_wire::seq_delta;

use super::packet::Packet;

/// The reorder array size. It must exceed the largest target depth so a packet
/// that arrives early still has somewhere to sit.
pub const CAPACITY: usize = JITTER_MAX_FRAMES * 2 + 4;

// A wrapping sequence number indexes this array, so the capacity must divide
// the u16 range evenly or the index jumps at the wrap point.
const _: () = assert!(
    CAPACITY.is_power_of_two() || 65_536 % CAPACITY == 0,
    "the reorder array must tile the sequence space evenly"
);

/// Measures arrival jitter and publishes a target depth.
///
/// This lives on the network task. It never touches the callback's state.
pub struct Estimator {
    last_arrival: Option<Instant>,
    last_seq: Option<u16>,
    /// Smoothed absolute deviation from the expected arrival spacing, in
    /// milliseconds. This is the RFC 3550 interarrival jitter estimate.
    estimate_ms: f32,
    /// When the target last changed. Shrinking waits for this to age.
    settled_at: Instant,
    current: u32,
}

impl Estimator {
    pub fn new() -> Self {
        Estimator {
            last_arrival: None,
            last_seq: None,
            estimate_ms: 0.0,
            settled_at: Instant::now(),
            current: JITTER_START_FRAMES as u32,
        }
    }

    /// Forgets the history. Used when a slot changes owner.
    pub fn reset(&mut self, target: &AtomicU32) {
        *self = Estimator::new();
        target.store(self.current, Ordering::Relaxed);
    }

    /// Records one arrival and updates the published target.
    pub fn on_arrival(&mut self, seq: u16, now: Instant, target: &AtomicU32) {
        let (Some(last_arrival), Some(last_seq)) = (self.last_arrival, self.last_seq) else {
            self.last_arrival = Some(now);
            self.last_seq = Some(seq);
            return;
        };

        let gap = now.saturating_duration_since(last_arrival).as_secs_f32() * 1000.0;
        let frames = seq_delta(seq, last_seq);

        self.last_arrival = Some(now);
        self.last_seq = Some(seq);

        // A reordered packet says nothing about spacing. Skip it.
        if frames <= 0 {
            return;
        }

        let expected = frames as f32 * FRAME_MS as f32;
        let deviation = (gap - expected).abs();

        // RFC 3550 smoothing. The 1/16 factor reacts within a few packets
        // without chasing a single outlier.
        self.estimate_ms += (deviation - self.estimate_ms) / 16.0;

        self.apply(target);
    }

    fn apply(&mut self, target: &AtomicU32) {
        // Two standard deviations of headroom, plus one frame so the buffer is
        // never empty at the moment the callback asks for audio.
        let wanted = ((self.estimate_ms * 2.0) / FRAME_MS as f32).ceil() as i64 + 1;
        let wanted = wanted.clamp(JITTER_MIN_FRAMES as i64, JITTER_MAX_FRAMES as i64) as u32;

        if wanted > self.current {
            // Grow at once. A gap the user can hear is worse than latency.
            self.current = wanted;
            self.settled_at = Instant::now();
            target.store(self.current, Ordering::Relaxed);
            return;
        }

        if wanted < self.current && self.settled_at.elapsed() >= JITTER_SHRINK_AFTER {
            // Shrink by one frame at a time, and only after a quiet spell.
            // Collapsing straight to the new target would drop a whole run of
            // audio at once.
            self.current -= 1;
            self.settled_at = Instant::now();
            target.store(self.current, Ordering::Relaxed);
        }
    }

    /// The smoothed jitter estimate in milliseconds. Reported by `doctor`.
    pub fn estimate_ms(&self) -> f32 {
        self.estimate_ms
    }
}

impl Default for Estimator {
    fn default() -> Self {
        Self::new()
    }
}

/// What the callback should do for this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Play this packet.
    Play,
    /// The packet never arrived. Run concealment.
    Conceal,
    /// The stream has not filled to its target yet. Play silence and wait.
    Prime,
}

/// The reorder buffer. It lives inside the output callback.
///
/// Every field is fixed size. Nothing here allocates.
pub struct Buffer {
    ring: [Packet; CAPACITY],
    filled: [bool; CAPACITY],
    /// The sequence number the next `take` will play.
    next_seq: u16,
    started: bool,
    /// True until the buffer has filled to its target for the first time.
    /// Playback holds off while this is set, which is what buys the buffer its
    /// tolerance for the rest of the talkspurt.
    priming: bool,
    held: usize,
    conceal_run: u32,
}

impl Buffer {
    pub fn new() -> Self {
        Buffer {
            ring: [Packet::empty(); CAPACITY],
            filled: [false; CAPACITY],
            next_seq: 0,
            started: false,
            priming: true,
            held: 0,
            conceal_run: 0,
        }
    }

    /// Throws away every held packet. Used when a slot changes owner.
    pub fn reset(&mut self) {
        self.filled = [false; CAPACITY];
        self.started = false;
        self.priming = true;
        self.held = 0;
        self.conceal_run = 0;
    }

    pub fn held(&self) -> usize {
        self.held
    }

    pub fn conceal_run(&self) -> u32 {
        self.conceal_run
    }

    /// Files one packet.
    ///
    /// Returns false when the packet arrived too late to play, which the caller
    /// counts.
    pub fn insert(&mut self, packet: Packet) -> bool {
        // A talkspurt start after silence means the sender's numbering may have
        // moved on. Restart rather than conceal the whole quiet stretch.
        if packet.flags & crate::net::audio_wire::FLAG_TALKSPURT_START != 0
            && (!self.started || seq_delta(packet.seq, self.next_seq) > CAPACITY as i32)
        {
            self.reset();
        }

        if !self.started {
            self.next_seq = packet.seq;
            self.started = true;
        }

        let ahead = seq_delta(packet.seq, self.next_seq);

        if ahead < 0 {
            // It belongs to a frame already played.
            return false;
        }
        if ahead >= CAPACITY as i32 {
            // It is so far ahead that the sender must have jumped. Start again
            // from this packet rather than stall.
            self.reset();
            self.next_seq = packet.seq;
            self.started = true;
        }

        let index = packet.seq as usize % CAPACITY;
        if !self.filled[index] {
            self.held += 1;
        }
        self.ring[index] = packet;
        self.filled[index] = true;
        true
    }

    /// Decides what to play for this frame, and advances.
    ///
    /// `target` is the depth the estimator asked for.
    pub fn take(&mut self, target: usize) -> (Decision, Option<Packet>) {
        if !self.started {
            return (Decision::Prime, None);
        }

        // Wait until enough audio is held to survive the measured jitter. This
        // happens once per talkspurt. After that the buffer plays through gaps
        // with concealment rather than stopping again, because a second pause
        // would be more noticeable than a concealed frame.
        if self.priming {
            if self.held < target.max(1) {
                return (Decision::Prime, None);
            }
            self.priming = false;
        }

        let index = self.next_seq as usize % CAPACITY;

        if self.filled[index] {
            let packet = self.ring[index];
            self.filled[index] = false;
            self.held -= 1;
            self.next_seq = self.next_seq.wrapping_add(1);
            self.conceal_run = 0;

            self.trim(target);
            return (Decision::Play, Some(packet));
        }

        self.next_seq = self.next_seq.wrapping_add(1);
        self.conceal_run = self.conceal_run.saturating_add(1);
        (Decision::Conceal, None)
    }

    /// The packet that follows the one just concealed, for in-band FEC.
    ///
    /// Opus can rebuild a lost frame from the next packet when the sender
    /// enabled FEC. That recovers a gap without a retransmission, which no
    /// real-time link has time for.
    pub fn peek_next(&self) -> Option<&Packet> {
        let index = self.next_seq as usize % CAPACITY;
        self.filled[index].then(|| &self.ring[index])
    }

    /// Drops a frame when the buffer has drifted deeper than the target.
    ///
    /// Latency creep is the failure this guards against. Two clocks are never
    /// exactly equal, so a buffer that only ever grows will slowly put the
    /// conversation behind, and nothing recovers it.
    ///
    /// The trim is deliberately reluctant. It allows `JITTER_SLACK_FRAMES` of
    /// headroom for a normal network burst, and then removes **one** frame per
    /// call rather than collapsing to the target, because a run of dropped
    /// frames is far more audible than a single one.
    fn trim(&mut self, target: usize) {
        // A hard limit first. The reorder array must never wrap onto itself.
        let hard = CAPACITY - 2;
        while self.held > hard {
            self.drop_head();
        }

        let limit = target.max(JITTER_MIN_FRAMES) + JITTER_SLACK_FRAMES;
        if self.held > limit {
            self.drop_head();
        }
    }

    /// Discards the frame at the play position and moves on.
    fn drop_head(&mut self) {
        let index = self.next_seq as usize % CAPACITY;
        if self.filled[index] {
            self.filled[index] = false;
            self.held -= 1;
        }
        self.next_seq = self.next_seq.wrapping_add(1);
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

/// The playback delay a target depth costs, for reporting.
pub fn depth_to_delay(frames: u32) -> Duration {
    Duration::from_millis(u64::from(frames) * u64::from(FRAME_MS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::audio_wire::FLAG_TALKSPURT_START;

    fn packet(seq: u16, flags: u8) -> Packet {
        let mut p = Packet::empty();
        p.seq = seq;
        p.timestamp = seq as u32 * 480;
        p.flags = flags;
        p
    }

    fn feed(buffer: &mut Buffer, seqs: &[u16]) {
        for (i, seq) in seqs.iter().enumerate() {
            let flags = if i == 0 { FLAG_TALKSPURT_START } else { 0 };
            buffer.insert(packet(*seq, flags));
        }
    }

    #[test]
    fn it_primes_before_it_plays() {
        let mut buffer = Buffer::new();
        feed(&mut buffer, &[0]);

        // One packet held, target of two. It waits.
        assert_eq!(buffer.take(2).0, Decision::Prime);

        buffer.insert(packet(1, 0));
        assert_eq!(buffer.take(2).0, Decision::Play);
    }

    #[test]
    fn it_plays_in_order() {
        let mut buffer = Buffer::new();
        feed(&mut buffer, &[0, 1, 2]);

        for expected in 0..3u16 {
            let (decision, packet) = buffer.take(1);
            assert_eq!(decision, Decision::Play);
            assert_eq!(packet.unwrap().seq, expected);
        }
    }

    #[test]
    fn it_reorders_a_late_arrival() {
        let mut buffer = Buffer::new();
        feed(&mut buffer, &[0]);
        buffer.insert(packet(2, 0));
        // Packet 1 arrives after packet 2.
        buffer.insert(packet(1, 0));

        for expected in 0..3u16 {
            let (_, p) = buffer.take(1);
            assert_eq!(p.unwrap().seq, expected, "order must be restored");
        }
    }

    #[test]
    fn a_missing_packet_becomes_concealment() {
        let mut buffer = Buffer::new();
        feed(&mut buffer, &[0]);
        buffer.insert(packet(2, 0));

        assert_eq!(buffer.take(1).0, Decision::Play); // 0
        assert_eq!(buffer.take(1).0, Decision::Conceal); // 1 never arrived
        assert_eq!(buffer.conceal_run(), 1);
        assert_eq!(buffer.take(1).0, Decision::Play); // 2
        assert_eq!(buffer.conceal_run(), 0);
    }

    #[test]
    fn a_packet_that_arrives_after_its_turn_is_refused() {
        let mut buffer = Buffer::new();
        feed(&mut buffer, &[0, 1]);
        buffer.take(1);
        buffer.take(1);

        assert!(!buffer.insert(packet(0, 0)), "a played frame cannot return");
    }

    #[test]
    fn the_buffer_does_not_creep_deeper_than_the_target() {
        let mut buffer = Buffer::new();
        feed(&mut buffer, &[0]);
        for seq in 1..20u16 {
            buffer.insert(packet(seq, 0));
        }

        // Play frames until the trim has brought the depth back down.
        let target = 2;
        for _ in 0..20 {
            buffer.take(target);
        }
        assert!(
            buffer.held() <= target + JITTER_SLACK_FRAMES,
            "held {} frames, which is latency creep",
            buffer.held()
        );
    }

    #[test]
    fn a_sequence_wrap_is_handled() {
        let mut buffer = Buffer::new();
        feed(&mut buffer, &[65_534]);
        buffer.insert(packet(65_535, 0));
        buffer.insert(packet(0, 0));
        buffer.insert(packet(1, 0));

        for expected in [65_534u16, 65_535, 0, 1] {
            let (decision, p) = buffer.take(1);
            assert_eq!(
                decision,
                Decision::Play,
                "wrap broke playback at {expected}"
            );
            assert_eq!(p.unwrap().seq, expected);
        }
    }

    #[test]
    fn a_new_talkspurt_restarts_the_buffer() {
        let mut buffer = Buffer::new();
        feed(&mut buffer, &[0, 1]);
        buffer.take(1);

        // The sender went quiet and came back far ahead.
        buffer.insert(packet(9000, FLAG_TALKSPURT_START));
        let (decision, p) = buffer.take(1);
        assert_eq!(decision, Decision::Play);
        assert_eq!(p.unwrap().seq, 9000, "a talkspurt must not conceal the gap");
    }

    #[test]
    fn fec_can_see_the_next_packet() {
        let mut buffer = Buffer::new();
        feed(&mut buffer, &[0]);
        buffer.insert(packet(2, 0));
        assert_eq!(buffer.take(1).0, Decision::Play); // 0

        // Frame 1 never arrived. `take` reports the gap and steps past it, so
        // the next packet is now at the play position and Opus can rebuild the
        // lost frame from its in-band FEC.
        assert_eq!(buffer.take(1).0, Decision::Conceal);
        assert_eq!(buffer.peek_next().map(|p| p.seq), Some(2));
    }

    #[test]
    fn the_estimator_grows_at_once_and_shrinks_slowly() {
        let target = AtomicU32::new(JITTER_START_FRAMES as u32);
        let mut estimator = Estimator::new();

        let start = Instant::now();
        // Steady arrivals, one frame apart.
        for i in 0..40u16 {
            let at = start + Duration::from_millis(i as u64 * FRAME_MS as u64);
            estimator.on_arrival(i, at, &target);
        }
        let steady = target.load(Ordering::Relaxed);

        // A burst of late arrivals.
        let mut clock = start + Duration::from_millis(40 * FRAME_MS as u64);
        for i in 40..70u16 {
            clock += Duration::from_millis(FRAME_MS as u64 + 45);
            estimator.on_arrival(i, clock, &target);
        }
        let after_jitter = target.load(Ordering::Relaxed);

        assert!(
            after_jitter > steady,
            "the target must grow when arrivals scatter: {steady} -> {after_jitter}"
        );

        // Calm returns, but the target must not collapse immediately.
        for i in 70..90u16 {
            clock += Duration::from_millis(FRAME_MS as u64);
            estimator.on_arrival(i, clock, &target);
        }
        assert_eq!(
            target.load(Ordering::Relaxed),
            after_jitter,
            "the target must not shrink before the settle time"
        );
    }

    #[test]
    fn the_estimator_stays_within_bounds() {
        let target = AtomicU32::new(JITTER_START_FRAMES as u32);
        let mut estimator = Estimator::new();

        let mut clock = Instant::now();
        for i in 0..200u16 {
            clock += Duration::from_millis(FRAME_MS as u64 + 500);
            estimator.on_arrival(i, clock, &target);
        }

        let value = target.load(Ordering::Relaxed) as usize;
        assert!(
            (JITTER_MIN_FRAMES..=JITTER_MAX_FRAMES).contains(&value),
            "target {value} is outside the configured bounds"
        );
    }

    #[test]
    fn a_reordered_arrival_does_not_inflate_the_estimate() {
        let target = AtomicU32::new(JITTER_START_FRAMES as u32);
        let mut estimator = Estimator::new();

        let mut clock = Instant::now();
        for i in 0..30u16 {
            clock += Duration::from_millis(FRAME_MS as u64);
            estimator.on_arrival(i, clock, &target);
        }
        let before = estimator.estimate_ms();

        // An old sequence number arrives out of order.
        clock += Duration::from_millis(FRAME_MS as u64);
        estimator.on_arrival(5, clock, &target);

        assert_eq!(
            estimator.estimate_ms(),
            before,
            "a reordered packet says nothing about spacing"
        );
    }
}
