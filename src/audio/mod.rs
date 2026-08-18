//! The audio engine.
//!
//! M3 fills this in. For now it defines the boundary between the network layer
//! and the audio layer, so the two can be built and tested apart.

use iroh::EndpointId;

use crate::net::audio_wire::AudioPacket;

/// Where received audio goes.
///
/// The network layer calls this from a tokio task. The implementation must not
/// block and must not allocate: it copies the payload into a lock-free queue
/// that the CoreAudio output callback drains. See `ARCHITECTURE.md` §5.3.
pub trait AudioSink: Send + Sync {
    /// Delivers one decoded datagram from a peer.
    fn deliver(&self, peer: EndpointId, packet: &AudioPacket<'_>);

    /// Marks a peer as a member of the live session, so its audio is mixed.
    ///
    /// Returns false when every peer slot is in use.
    fn activate(&self, peer: EndpointId) -> bool;

    /// Removes a peer from the live session.
    fn deactivate(&self, peer: EndpointId);
}

/// An audio sink that drops everything.
///
/// Used by `walkie tui` before M3 lands, and by tests that only exercise the
/// network layer.
#[derive(Debug, Default)]
pub struct NullSink;

impl AudioSink for NullSink {
    fn deliver(&self, _peer: EndpointId, _packet: &AudioPacket<'_>) {}
    fn activate(&self, _peer: EndpointId) -> bool {
        true
    }
    fn deactivate(&self, _peer: EndpointId) {}
}
