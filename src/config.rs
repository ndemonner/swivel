//! Every tunable constant lives here.
//!
//! Do not put a magic number anywhere else in the code. A reviewer must be able
//! to read this one file and know the whole latency budget.

use std::time::Duration;

// ---------------------------------------------------------------------------
// Protocol
// ---------------------------------------------------------------------------

/// The QUIC application protocol name. Bump the number on a breaking change.
pub const ALPN: &[u8] = b"walkie/0";

/// The control protocol version sent in `Control::Hello`.
pub const PROTOCOL_VERSION: u16 = 1;

/// The human readable prefix of a shareable ticket.
pub const TICKET_PREFIX: &str = "wt1";

// ---------------------------------------------------------------------------
// Audio format
// ---------------------------------------------------------------------------

/// The one sample rate. Every stage runs at this rate. Resampling only happens
/// when a device refuses it, and that is reported as a fault.
pub const SAMPLE_RATE: u32 = 48_000;

/// The Opus frame length in milliseconds. Opus allows 2.5, 5, 10, 20, 40, 60.
/// 10 balances packet overhead against latency.
pub const FRAME_MS: u32 = 10;

/// Samples in one mono frame. 480 at 48 kHz and 10 ms.
pub const FRAME_SAMPLES: usize = (SAMPLE_RATE * FRAME_MS / 1000) as usize;

/// The device buffer size in frames. Lower means less latency and more risk of
/// a dropout. CoreAudio on the test machine accepts values down to 15.
pub const DEVICE_BUFFER_FRAMES: u32 = 256;

/// The target Opus bitrate in bits per second, for one mono stream.
pub const OPUS_BITRATE: i32 = 64_000;

/// Opus encoder complexity, 0 to 10. 8 costs about 0.3 ms per frame.
pub const OPUS_COMPLEXITY: i32 = 8;

/// The loss percentage the encoder assumes when it adds in-band FEC.
pub const OPUS_EXPECTED_LOSS: i32 = 10;

/// The largest Opus packet the decoder will accept. A 10 ms frame at 64 kbps is
/// about 72 bytes. 1000 leaves room for a bitrate change.
pub const MAX_PACKET_BYTES: usize = 1000;

/// Capture ring capacity in samples. Four device buffers of headroom.
pub const CAPTURE_RING_SAMPLES: usize = DEVICE_BUFFER_FRAMES as usize * 4;

/// Encoded packets held per peer before the network side drops the oldest.
pub const PEER_QUEUE_PACKETS: usize = 32;

// ---------------------------------------------------------------------------
// Jitter buffer
// ---------------------------------------------------------------------------

/// The smallest target depth in frames. 1 only works on a stable LAN.
pub const JITTER_MIN_FRAMES: usize = 1;

/// The starting target depth in frames. 2 frames is 20 ms.
pub const JITTER_START_FRAMES: usize = 2;

/// The largest target depth in frames. Above this, drop instead of buffering.
pub const JITTER_MAX_FRAMES: usize = 6;

/// How long the buffer must stay stable before it shrinks by one frame.
pub const JITTER_SHRINK_AFTER: Duration = Duration::from_secs(5);

/// Consecutive concealed frames before the output fades to silence.
pub const CONCEAL_LIMIT: u32 = 5;

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// The largest number of people in one conversation, including you.
/// A mesh sends `MAX_PEERS - 1` streams up.
pub const MAX_PEERS: usize = 8;

/// The highest slot number that a single keypress can reach.
pub const MAX_SLOT: u8 = 9;

/// A session with no voice for this long closes itself.
pub const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(600);

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

/// Application level ping period. This measures scheduling delay as well as
/// network delay, so it is more honest than the QUIC estimate.
pub const PING_INTERVAL: Duration = Duration::from_secs(2);

/// Reconnect backoff. The supervisor walks this list and holds the last value.
pub const BACKOFF_SECS: &[u64] = &[1, 2, 5, 15, 30];

/// The random fraction added to each backoff step, to avoid a thundering herd.
pub const BACKOFF_JITTER: f64 = 0.2;

/// How long to wait after a connection ends before dialling again.
///
/// Both sides dial, so a duplicate is normal and one of the two is closed. The
/// side whose connection was closed must not redial into the replacement that
/// is still arriving, or the pair churns through several connections before it
/// settles. This delay closes that race.
pub const RECONNECT_SETTLE: Duration = Duration::from_millis(300);

/// QUIC datagram send buffer. Deliberately small. When it fills, the encoder
/// drops a frame rather than queueing it. A queued frame costs unbounded
/// latency for the rest of the session.
pub const DATAGRAM_SEND_BUFFER: usize = 8 * 1024;

/// QUIC datagram receive buffer.
pub const DATAGRAM_RECV_BUFFER: usize = 64 * 1024;

/// The round trip time QUIC assumes before it measures one. The default of
/// 333 ms delays the first packets of a new connection.
pub const INITIAL_RTT: Duration = Duration::from_millis(30);

/// QUIC keep alive period. This holds the NAT mapping open.
pub const KEEP_ALIVE: Duration = Duration::from_secs(5);

/// Idle time before QUIC drops a connection.
pub const MAX_IDLE: Duration = Duration::from_secs(20);

/// The MTU assumed at connection start. Audio packets are far below it.
pub const INITIAL_MTU: u16 = 1200;

// ---------------------------------------------------------------------------
// User interface
// ---------------------------------------------------------------------------

/// How often the user interface reads the state snapshot. Immediate changes
/// also post a main-thread wake, so this is only a floor.
pub const UI_REDRAW_HZ: u64 = 10;

// ---------------------------------------------------------------------------
// Compile-time checks
// ---------------------------------------------------------------------------
// These hold the invariants that the rest of the code assumes. A bad edit to a
// constant above fails the build here, not at run time.

const _: () = assert!(FRAME_SAMPLES == 480, "48 kHz at 10 ms is 480 samples");
const _: () = assert!(JITTER_MIN_FRAMES <= JITTER_START_FRAMES);
const _: () = assert!(JITTER_START_FRAMES <= JITTER_MAX_FRAMES);
const _: () = assert!(MAX_SLOT <= 9, "a slot must fit in one keypress");
const _: () = assert!(MAX_PEERS >= 2);
const _: () = assert!(CAPTURE_RING_SAMPLES > FRAME_SAMPLES);
