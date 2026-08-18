//! The snapshot the user interface reads.
//!
//! The interface never reads network or audio state directly. The core
//! publishes an immutable snapshot through `arc-swap`, and the interface swaps
//! a pointer to read it. This keeps AppKit off every lock in the program.
//! See `ARCHITECTURE.md` §9.3.

use std::sync::Arc;

use arc_swap::ArcSwap;
use iroh::EndpointId;

/// How a peer's packets reach us.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathKind {
    /// A hole-punched direct path. This is the low latency case.
    Direct,
    /// The traffic goes through a relay. This roughly doubles the delay, so the
    /// roster shows it.
    Relay,
    /// Not connected, or not yet known.
    #[default]
    Unknown,
}

impl PathKind {
    pub fn short(&self) -> &'static str {
        match self {
            PathKind::Direct => "DIR",
            PathKind::Relay => "RLY",
            PathKind::Unknown => "",
        }
    }
}

/// One row of the roster.
#[derive(Debug, Clone)]
pub struct PeerView {
    pub endpoint_id: EndpointId,
    pub slot: Option<u8>,
    pub name: String,
    pub online: bool,
    /// The application level round trip time in milliseconds.
    pub rtt_ms: Option<u32>,
    pub path: PathKind,
    /// The peer refuses incoming sessions.
    pub dnd: bool,
    /// The peer has closed its own microphone.
    pub muted: bool,
    /// The peer is in the local session.
    pub live: bool,
    /// The peer is speaking now.
    pub speaking: bool,
}

/// A connection attempt from an endpoint that is not a contact.
#[derive(Debug, Clone)]
pub struct KnockView {
    pub endpoint_id: EndpointId,
    pub claimed: Option<String>,
}

/// What the local microphone is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MicState {
    /// Closed. Nothing is transmitted.
    #[default]
    Closed,
    /// Open to at least one peer.
    Live,
    /// Forced off by the user, while a session is open.
    Muted,
}

/// The whole interface state, as one value.
#[derive(Debug, Clone, Default)]
pub struct UiState {
    pub my_name: String,
    pub my_id: Option<EndpointId>,
    /// False until the endpoint reaches a relay. Nothing can connect before it.
    pub online: bool,
    pub peers: Vec<PeerView>,
    pub knocks: Vec<KnockView>,
    pub mic: MicState,
    pub dnd: bool,
    /// Slots in the live session, in the order they were added.
    pub live_slots: Vec<u8>,
    /// Set when something went wrong that the user must see.
    pub fault: Option<String>,
    /// Audio counters. Shown by the headless mode and by `doctor`.
    pub audio: AudioCounters,
}

/// What the audio path is actually doing.
///
/// These are the numbers to look at when a call sounds wrong. Concealed frames
/// mean loss. Late frames mean the jitter buffer is too shallow. Refused
/// datagrams mean the link is congested.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioCounters {
    pub sent: u64,
    pub refused: u64,
    pub encoded: u64,
    /// Frames decoded and heard.
    pub played: u64,
    pub concealed: u64,
    pub late: u64,
    pub overrun: u64,
}

impl UiState {
    /// The peers in the live session.
    pub fn live_peers(&self) -> impl Iterator<Item = &PeerView> {
        self.peers.iter().filter(|p| p.live)
    }

    /// True when any peer is sending audio right now.
    pub fn receiving(&self) -> bool {
        self.peers.iter().any(|p| p.live && p.speaking)
    }
}

/// The published snapshot. Cloning this is cheap and it is safe to share.
#[derive(Clone, Default)]
pub struct StateHandle(Arc<ArcSwap<UiState>>);

impl StateHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads the current snapshot. This never blocks.
    pub fn load(&self) -> Arc<UiState> {
        self.0.load_full()
    }

    /// Replaces the snapshot.
    pub fn store(&self, state: UiState) {
        self.0.store(Arc::new(state));
    }

    /// Reads, edits a copy, and publishes it.
    ///
    /// Two writers can race here and one edit can be lost. Only the core
    /// publishes state, and it is single threaded, so that cannot happen.
    pub fn update(&self, f: impl FnOnce(&mut UiState)) {
        let mut next = (*self.load()).clone();
        f(&mut next);
        self.store(next);
    }
}

impl std::fmt::Debug for StateHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("StateHandle").field(&self.load()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_is_visible_to_the_next_load() {
        let handle = StateHandle::new();
        assert!(handle.load().peers.is_empty());

        handle.update(|s| s.my_name = "nick".into());
        assert_eq!(handle.load().my_name, "nick");
    }

    #[test]
    fn a_load_holds_its_value_across_a_later_store() {
        let handle = StateHandle::new();
        handle.update(|s| s.my_name = "first".into());
        let held = handle.load();

        handle.update(|s| s.my_name = "second".into());

        assert_eq!(held.my_name, "first", "a reader keeps a stable snapshot");
        assert_eq!(handle.load().my_name, "second");
    }
}
