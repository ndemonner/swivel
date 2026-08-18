//! Talk sessions.
//!
//! A session is a set of people whose microphones are open to each other. It is
//! not a call. Opening one does not open a network connection, because the
//! connections are already warm. It only opens a microphone and starts mixing
//! the members' audio. That is why pressing a digit is instant.
//!
//! See `DESIGN.md` §5 and `ARCHITECTURE.md` §6.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;

use arc_swap::ArcSwap;
use bytes::Bytes;
use iroh::EndpointId;
use iroh::endpoint::Connection;

use crate::audio::AudioTx;
use crate::config::MAX_PEERS;

/// The live conversation, if there is one.
#[derive(Debug, Clone)]
pub struct Session {
    /// Chosen by whoever opened it. Members echo it back so a stale message
    /// from a finished session cannot reopen one.
    pub id: u64,
    pub members: BTreeSet<EndpointId>,
    pub started: Instant,
    /// The last time anyone in the session was heard. Drives the idle timer.
    pub last_voice: Instant,
}

impl Session {
    pub fn new(id: u64) -> Self {
        let now = Instant::now();
        Session {
            id,
            members: BTreeSet::new(),
            started: now,
            last_voice: now,
        }
    }

    /// Adds a member. Returns false when the session is already full.
    pub fn add(&mut self, peer: EndpointId) -> bool {
        // The local user counts towards the limit, so a session holds
        // `MAX_PEERS - 1` other people.
        if !self.members.contains(&peer) && self.members.len() >= MAX_PEERS - 1 {
            return false;
        }
        self.members.insert(peer);
        true
    }

    pub fn remove(&mut self, peer: EndpointId) -> bool {
        self.members.remove(&peer)
    }

    pub fn contains(&self, peer: EndpointId) -> bool {
        self.members.contains(&peer)
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    pub fn idle_for(&self) -> std::time::Duration {
        self.last_voice.elapsed()
    }
}

/// Sends encoded audio to every member.
///
/// This is the one place where the audio thread meets the network. It must not
/// block and it must not take an async lock, because it is called from the
/// sender thread once every 10 ms.
///
/// The member connections are therefore published as an immutable snapshot.
/// Reading one is a pointer swap.
#[derive(Default)]
pub struct SessionTx {
    targets: ArcSwap<Vec<Connection>>,
    /// Datagrams the transport refused, almost always because its small send
    /// buffer was full. Reported by `swivel doctor`.
    pub refused: std::sync::atomic::AtomicU64,
    pub sent: std::sync::atomic::AtomicU64,
}

impl SessionTx {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the set of connections that receive audio.
    pub fn set_targets(&self, connections: Vec<Connection>) {
        self.targets.store(Arc::new(connections));
    }

    /// Stops sending to everyone.
    pub fn clear(&self) {
        self.targets.store(Arc::new(Vec::new()));
    }

    pub fn target_count(&self) -> usize {
        self.targets.load().len()
    }
}

impl AudioTx for SessionTx {
    fn send_frame(&self, wire: &[u8]) -> usize {
        let targets = self.targets.load();
        if targets.is_empty() {
            return 0;
        }

        // One allocation for the frame, then a reference count bump per peer.
        // This is the only allocation in the send path.
        let bytes = Bytes::copy_from_slice(wire);
        let mut reached = 0;

        for connection in targets.iter() {
            // `send_datagram` is not async and it never blocks. When the
            // transport's small send buffer is full it returns an error, and
            // dropping the frame is the right answer: a queued frame would make
            // every later frame late as well. See ARCHITECTURE.md §4.2.
            match connection.send_datagram(bytes.clone()) {
                Ok(()) => reached += 1,
                Err(_) => {
                    self.refused
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }

        self.sent
            .fetch_add(reached as u64, std::sync::atomic::Ordering::Relaxed);
        reached
    }
}

/// Picks an identifier for a new session.
pub fn new_id() -> u64 {
    use rand::Rng;
    rand::rng().random()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn id(n: u8) -> EndpointId {
        SecretKey::from_bytes(&[n; 32]).public()
    }

    #[test]
    fn a_session_holds_members() {
        let mut session = Session::new(1);
        assert!(session.is_empty());

        assert!(session.add(id(1)));
        assert!(session.contains(id(1)));
        assert!(!session.is_empty());

        assert!(session.remove(id(1)));
        assert!(session.is_empty());
        assert!(!session.remove(id(1)));
    }

    #[test]
    fn adding_the_same_member_twice_is_harmless() {
        let mut session = Session::new(1);
        assert!(session.add(id(1)));
        assert!(session.add(id(1)));
        assert_eq!(session.members.len(), 1);
    }

    #[test]
    fn a_session_stops_at_the_mesh_limit() {
        let mut session = Session::new(1);

        // The local user is one of the MAX_PEERS, so MAX_PEERS - 1 others fit.
        for n in 1..MAX_PEERS as u8 {
            assert!(session.add(id(n)), "member {n} should fit");
        }
        assert_eq!(session.members.len(), MAX_PEERS - 1);

        assert!(
            !session.add(id(200)),
            "a mesh past the limit would saturate a home connection"
        );

        // An existing member can still be re-added when it is full.
        assert!(session.add(id(1)));
    }

    #[test]
    fn session_ids_differ() {
        let a = new_id();
        let b = new_id();
        assert_ne!(
            a, b,
            "a repeated id would let a stale message reopen a session"
        );
    }

    #[test]
    fn a_transmitter_with_no_targets_sends_nothing() {
        let tx = SessionTx::new();
        assert_eq!(tx.target_count(), 0);
        assert_eq!(tx.send_frame(&[1, 2, 3]), 0);
    }
}
