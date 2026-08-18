//! One contact, and the connection to them.
//!
//! A connection is opened once and kept warm for the life of the process. This
//! is the reason a talk session starts instantly: pressing a digit opens a
//! microphone, never a connection. See `DESIGN.md` §2.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use iroh::EndpointId;
use iroh::endpoint::Connection;
use tokio::sync::{Mutex, Notify, mpsc};
use tracing::debug;

use super::control::Control;
use crate::state::PathKind;

/// The live view of one contact.
pub struct Peer {
    pub id: EndpointId,

    /// The active connection, if any. `None` means offline.
    slot: Mutex<Option<Active>>,

    /// Fires when the active connection is replaced or lost.
    disconnected: Notify,

    /// The last measured application round trip time, in milliseconds.
    /// `u32::MAX` means not measured.
    rtt_ms: AtomicU32,

    /// The path kind, stored as the discriminant of `PathKind`.
    path: AtomicU32,

    /// What the peer told us about itself.
    pub peer_dnd: AtomicBool,
    pub peer_muted: AtomicBool,
    pub speaking: AtomicBool,

    /// The name the peer claims. The local contact name wins in the roster.
    claimed_name: Mutex<Option<String>>,
}

/// A connection and the channel that writes to its control stream.
struct Active {
    conn: Connection,
    /// True when this side dialled. It decides the tie-break in `offer`.
    dialed_by_me: bool,
    control: mpsc::UnboundedSender<Control>,
}

impl Peer {
    pub fn new(id: EndpointId) -> Arc<Self> {
        Arc::new(Peer {
            id,
            slot: Mutex::new(None),
            disconnected: Notify::new(),
            rtt_ms: AtomicU32::new(u32::MAX),
            path: AtomicU32::new(PathKind::Unknown as u32),
            peer_dnd: AtomicBool::new(false),
            peer_muted: AtomicBool::new(false),
            speaking: AtomicBool::new(false),
            claimed_name: Mutex::new(None),
        })
    }

    /// Offers a new connection.
    ///
    /// Both sides dial each other, so two connections can exist for one pair.
    /// Both sides must discard the same one or they end up talking on different
    /// connections. The rule is deterministic and needs no negotiation: keep
    /// the connection whose **dialer has the smaller endpoint id**.
    ///
    /// Returns true when the caller should run the connection. Returns false
    /// when the caller should close it.
    pub async fn offer(
        &self,
        conn: Connection,
        dialed_by_me: bool,
        me: EndpointId,
        control: mpsc::UnboundedSender<Control>,
    ) -> bool {
        let mut slot = self.slot.lock().await;

        if let Some(existing) = slot.as_ref() {
            // Nothing to decide when the existing connection is already dead.
            if existing.conn.close_reason().is_none() {
                let existing_dialer = dialer_of(existing.dialed_by_me, me, self.id);
                let offered_dialer = dialer_of(dialed_by_me, me, self.id);

                let keep_existing = existing_dialer <= offered_dialer;
                if keep_existing {
                    debug!(peer = %self.id.fmt_short(), "dropping a duplicate connection");
                    return false;
                }

                debug!(peer = %self.id.fmt_short(), "replacing a duplicate connection");
                existing.conn.close(1u32.into(), b"duplicate");
            }
        }

        *slot = Some(Active {
            conn,
            dialed_by_me,
            control,
        });
        drop(slot);

        // A replaced connection must wake anything waiting on the old one.
        self.disconnected.notify_waiters();
        true
    }

    /// Clears the active connection, but only if it is the one given.
    ///
    /// The check matters. A slow teardown of a losing connection must not
    /// remove the winning one that replaced it.
    pub async fn clear(&self, conn: &Connection) {
        let mut slot = self.slot.lock().await;
        if slot
            .as_ref()
            .is_some_and(|a| a.conn.stable_id() == conn.stable_id())
        {
            *slot = None;
            self.rtt_ms.store(u32::MAX, Ordering::Relaxed);
            self.path.store(PathKind::Unknown as u32, Ordering::Relaxed);
            self.peer_dnd.store(false, Ordering::Relaxed);
            self.peer_muted.store(false, Ordering::Relaxed);
            self.speaking.store(false, Ordering::Relaxed);
        }
        drop(slot);
        self.disconnected.notify_waiters();
    }

    /// True when a connection is registered and not closed.
    pub async fn is_connected(&self) -> bool {
        let slot = self.slot.lock().await;
        slot.as_ref()
            .is_some_and(|a| a.conn.close_reason().is_none())
    }

    /// Returns the active connection, for the audio send path.
    ///
    /// The encoder thread calls `Connection::send_datagram`, which is not
    /// async. Cloning the connection lets audio bypass the tokio runtime
    /// entirely, which removes a scheduling hop from the hot path.
    pub async fn connection(&self) -> Option<Connection> {
        let slot = self.slot.lock().await;
        slot.as_ref().map(|a| a.conn.clone())
    }

    /// Waits until the active connection goes away.
    pub async fn wait_disconnected(&self) {
        loop {
            let notified = self.disconnected.notified();
            if !self.is_connected().await {
                return;
            }
            notified.await;
        }
    }

    /// Queues a control message. Returns false when the peer is offline.
    pub async fn send_control(&self, msg: Control) -> bool {
        let slot = self.slot.lock().await;
        match slot.as_ref() {
            Some(active) => active.control.send(msg).is_ok(),
            None => false,
        }
    }

    pub fn rtt(&self) -> Option<u32> {
        match self.rtt_ms.load(Ordering::Relaxed) {
            u32::MAX => None,
            ms => Some(ms),
        }
    }

    pub fn set_rtt(&self, rtt: Duration) {
        let ms = rtt.as_millis().min(u32::MAX as u128 - 1) as u32;
        self.rtt_ms.store(ms, Ordering::Relaxed);
    }

    pub fn path(&self) -> PathKind {
        match self.path.load(Ordering::Relaxed) {
            0 => PathKind::Direct,
            1 => PathKind::Relay,
            _ => PathKind::Unknown,
        }
    }

    pub fn set_path(&self, path: PathKind) {
        self.path.store(path as u32, Ordering::Relaxed);
    }

    pub async fn claimed_name(&self) -> Option<String> {
        self.claimed_name.lock().await.clone()
    }

    pub async fn set_claimed_name(&self, name: String) {
        *self.claimed_name.lock().await = Some(name);
    }
}

/// Which endpoint dialled a connection.
fn dialer_of(dialed_by_me: bool, me: EndpointId, them: EndpointId) -> [u8; 32] {
    if dialed_by_me {
        *me.as_bytes()
    } else {
        *them.as_bytes()
    }
}

/// Reconnect backoff. The supervisor walks the table and holds the last value.
pub fn backoff_for(attempt: usize) -> Duration {
    let table = crate::config::BACKOFF_SECS;
    let base = table[attempt.min(table.len() - 1)];

    // Jitter keeps a group of peers from reconnecting in lockstep after a
    // network drop.
    let spread = base as f64 * crate::config::BACKOFF_JITTER;
    let extra = fastrand_fraction() * spread;
    Duration::from_secs_f64(base as f64 + extra)
}

/// A cheap fraction in `0.0..1.0`.
///
/// Backoff jitter does not need a strong generator, and pulling `rand` into
/// this path would need a thread-local generator for no benefit.
fn fastrand_fraction() -> f64 {
    use std::cell::Cell;
    thread_local! {
        static SEED: Cell<u64> = const { Cell::new(0x2545_F491_4F6C_DD1D) };
    }
    SEED.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        (x >> 11) as f64 / (1u64 << 53) as f64
    })
}

/// Reads the path kind from a live connection.
///
/// iroh prefers a direct hole-punched path and falls back to a relay. The
/// difference is the single biggest latency factor a user can see, so the
/// roster reports it.
///
/// Only the selected path matters. iroh may hold a relay path open as a standby
/// while it sends over a direct one, and reporting that as a relay would be a
/// lie.
pub fn path_of(conn: &Connection) -> PathKind {
    let paths = conn.paths();

    let mut fallback = PathKind::Unknown;
    for path in paths.iter() {
        let kind = if path.is_ip() {
            PathKind::Direct
        } else if path.is_relay() {
            PathKind::Relay
        } else {
            PathKind::Unknown
        };

        if path.is_selected() {
            return kind;
        }
        if fallback == PathKind::Unknown {
            fallback = kind;
        }
    }

    fallback
}

/// Tracks how long a peer has been silent, for the session idle timer.
pub struct VoiceClock {
    last: Mutex<Instant>,
}

impl VoiceClock {
    pub fn new() -> Self {
        VoiceClock {
            last: Mutex::new(Instant::now()),
        }
    }

    pub async fn mark(&self) {
        *self.last.lock().await = Instant::now();
    }

    pub async fn idle_for(&self) -> Duration {
        self.last.lock().await.elapsed()
    }
}

impl Default for VoiceClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BACKOFF_SECS;
    use iroh::SecretKey;

    fn id(n: u8) -> EndpointId {
        SecretKey::from_bytes(&[n; 32]).public()
    }

    #[test]
    fn the_tie_break_picks_the_same_connection_on_both_sides() {
        let a = id(1);
        let b = id(2);

        // Side A holds: one it dialled, one B dialled.
        let a_dialed = dialer_of(true, a, b);
        let a_accepted = dialer_of(false, a, b);
        let a_keeps_own = a_dialed <= a_accepted;

        // Side B holds the mirror image.
        let b_dialed = dialer_of(true, b, a);
        let b_accepted = dialer_of(false, b, a);
        let b_keeps_own = b_dialed <= b_accepted;

        // Exactly one side keeps the connection it dialled. If both kept their
        // own, the two would end up on different connections.
        assert!(
            a_keeps_own != b_keeps_own,
            "the rule must pick one dialer, not both"
        );
    }

    #[test]
    fn backoff_grows_and_then_holds() {
        let first = backoff_for(0).as_secs_f64();
        let last = backoff_for(BACKOFF_SECS.len() * 4).as_secs_f64();

        assert!(first >= BACKOFF_SECS[0] as f64);
        assert!(first < BACKOFF_SECS[1] as f64);

        let cap = *BACKOFF_SECS.last().unwrap() as f64;
        assert!(last >= cap);
        assert!(last <= cap * (1.0 + crate::config::BACKOFF_JITTER));
    }

    #[test]
    fn backoff_jitter_is_not_constant() {
        let samples: Vec<f64> = (0..8).map(|_| backoff_for(2).as_secs_f64()).collect();
        let all_same = samples.windows(2).all(|w| w[0] == w[1]);
        assert!(!all_same, "jitter must vary or peers reconnect in lockstep");
    }

    #[test]
    fn rtt_starts_unmeasured() {
        let peer = Peer::new(id(1));
        assert_eq!(peer.rtt(), None);
        peer.set_rtt(Duration::from_millis(17));
        assert_eq!(peer.rtt(), Some(17));
    }

    #[test]
    fn path_defaults_to_unknown() {
        let peer = Peer::new(id(1));
        assert_eq!(peer.path(), PathKind::Unknown);
        peer.set_path(PathKind::Relay);
        assert_eq!(peer.path(), PathKind::Relay);
    }
}
