//! The packet queue between the network and the output callback.
//!
//! The output callback is a real-time thread. It must not allocate and it must
//! not take a lock that a slower thread can hold. Every buffer here is
//! allocated once at start up, and the queues are lock-free. See
//! `ARCHITECTURE.md` §2.1 and §5.3.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use iroh::EndpointId;
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use crate::config::{MAX_PACKET_BYTES, MAX_PEERS, PEER_QUEUE_PACKETS};
use crate::net::audio_wire::AudioPacket;

/// One encoded frame, sized so a queue slot never allocates.
#[derive(Clone, Copy)]
pub struct Packet {
    pub seq: u16,
    pub timestamp: u32,
    pub flags: u8,
    len: u16,
    data: [u8; MAX_PACKET_BYTES],
}

impl Packet {
    /// An empty packet. Used to fill the preallocated arrays.
    pub const fn empty() -> Self {
        Packet {
            seq: 0,
            timestamp: 0,
            flags: 0,
            len: 0,
            data: [0; MAX_PACKET_BYTES],
        }
    }

    /// Copies a parsed datagram into a queue slot.
    ///
    /// Returns `None` when the payload is larger than a slot. That cannot
    /// happen with our own encoder settings, and a peer does not get to make
    /// us allocate.
    pub fn from_wire(packet: &AudioPacket<'_>) -> Option<Self> {
        if packet.payload.len() > MAX_PACKET_BYTES {
            return None;
        }

        let mut out = Packet {
            seq: packet.seq,
            timestamp: packet.timestamp,
            flags: packet.flags,
            len: packet.payload.len() as u16,
            data: [0; MAX_PACKET_BYTES],
        };
        out.data[..packet.payload.len()].copy_from_slice(packet.payload);
        Some(out)
    }

    /// The Opus payload.
    pub fn payload(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl std::fmt::Debug for Packet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Packet")
            .field("seq", &self.seq)
            .field("timestamp", &self.timestamp)
            .field("bytes", &self.len)
            .finish()
    }
}

/// The write side of one peer's queue. The network task owns it.
pub type PacketProducer = HeapProd<Packet>;

/// The read side of one peer's queue. The output callback owns it.
pub type PacketConsumer = HeapCons<Packet>;

/// Shared state for one peer slot.
///
/// The output callback reads `active`, `generation`, and the target depth. It
/// never touches the mutex.
pub struct SlotShared {
    /// The callback mixes this slot only when it is set.
    pub active: AtomicBool,

    /// Bumped whenever the slot changes owner. The callback compares it against
    /// what it last saw, and resets its decoder and jitter state on a change.
    /// This is how a slot is reassigned without allocating in the callback.
    pub generation: AtomicU32,

    /// The jitter target in frames, published by the network side.
    pub target_frames: AtomicU32,

    /// Frames concealed because a packet never arrived.
    pub concealed: AtomicU64,

    /// Packets dropped because they arrived too late to play.
    pub late: AtomicU64,

    /// Packets dropped because the queue was full.
    pub overrun: AtomicU64,

    /// The write side. Only the network task locks this.
    producer: Mutex<Option<PacketProducer>>,
}

impl SlotShared {
    fn new(producer: PacketProducer) -> Self {
        SlotShared {
            active: AtomicBool::new(false),
            generation: AtomicU32::new(0),
            target_frames: AtomicU32::new(crate::config::JITTER_START_FRAMES as u32),
            concealed: AtomicU64::new(0),
            late: AtomicU64::new(0),
            overrun: AtomicU64::new(0),
            producer: Mutex::new(Some(producer)),
        }
    }

    /// Pushes a packet. Called from a tokio task, never from the callback.
    ///
    /// A full queue drops the packet. That sounds wrong, because the newest
    /// audio is usually the audio worth keeping, but it is the right choice
    /// here for two reasons.
    ///
    /// First, a split single-producer queue has no way to remove the oldest
    /// entry from the writing side.
    ///
    /// Second, this queue is not the jitter buffer. It is a hand-off, and the
    /// output callback empties it completely every few milliseconds. It holds
    /// 32 packets, which is 320 ms of audio, so it can only fill if the audio
    /// device has stopped calling back. At that point the stream is already
    /// broken and the choice of which packet to drop does not matter. The
    /// jitter buffer, not this queue, decides what is too late to play.
    pub fn push(&self, packet: Packet) {
        let Ok(mut guard) = self.producer.lock() else {
            return;
        };
        let Some(producer) = guard.as_mut() else {
            return;
        };

        if producer.try_push(packet).is_err() {
            self.overrun.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The fixed set of peer slots.
///
/// The size never changes, so the callback can index it without a lock and
/// without a bounds surprise. Adding a peer sets a flag. It never allocates.
pub struct SlotTable {
    shared: [SlotShared; MAX_PEERS],
    /// Who owns each slot. Read and written by non-real-time threads only.
    owners: Mutex<[Option<EndpointId>; MAX_PEERS]>,
}

impl SlotTable {
    /// Allocates every queue up front, and returns the read sides for the
    /// output callback.
    pub fn new() -> (Self, Vec<PacketConsumer>) {
        let mut shared = Vec::with_capacity(MAX_PEERS);
        let mut consumers = Vec::with_capacity(MAX_PEERS);

        for _ in 0..MAX_PEERS {
            let (producer, consumer) = HeapRb::<Packet>::new(PEER_QUEUE_PACKETS).split();
            shared.push(SlotShared::new(producer));
            consumers.push(consumer);
        }

        let shared: [SlotShared; MAX_PEERS] = shared
            .try_into()
            .unwrap_or_else(|_| unreachable!("the loop ran MAX_PEERS times"));

        (
            SlotTable {
                shared,
                owners: Mutex::new([None; MAX_PEERS]),
            },
            consumers,
        )
    }

    pub fn slot(&self, index: usize) -> &SlotShared {
        &self.shared[index]
    }

    /// Gives a peer a slot. Returns the index, or `None` when all are taken.
    ///
    /// Reactivating a peer that already holds a slot returns the same index and
    /// does not disturb its stream.
    pub fn activate(&self, peer: EndpointId) -> Option<usize> {
        let mut owners = self.owners.lock().ok()?;

        if let Some(index) = owners.iter().position(|o| *o == Some(peer)) {
            self.shared[index].active.store(true, Ordering::Release);
            return Some(index);
        }

        let index = owners.iter().position(|o| o.is_none())?;
        owners[index] = Some(peer);

        let slot = &self.shared[index];
        slot.concealed.store(0, Ordering::Relaxed);
        slot.late.store(0, Ordering::Relaxed);
        slot.overrun.store(0, Ordering::Relaxed);
        slot.target_frames
            .store(crate::config::JITTER_START_FRAMES as u32, Ordering::Relaxed);

        // Tell the callback to throw away whatever the previous owner left.
        slot.generation.fetch_add(1, Ordering::Release);
        slot.active.store(true, Ordering::Release);

        Some(index)
    }

    /// Takes a peer's slot away.
    pub fn deactivate(&self, peer: EndpointId) {
        let Ok(mut owners) = self.owners.lock() else {
            return;
        };
        let Some(index) = owners.iter().position(|o| *o == Some(peer)) else {
            return;
        };

        owners[index] = None;
        self.shared[index].active.store(false, Ordering::Release);
        // The callback drains the leftover packets when it sees this change.
        self.shared[index]
            .generation
            .fetch_add(1, Ordering::Release);
    }

    /// The slot a peer holds, if any.
    pub fn index_of(&self, peer: EndpointId) -> Option<usize> {
        let owners = self.owners.lock().ok()?;
        owners.iter().position(|o| *o == Some(peer))
    }

    /// Every peer that currently holds a slot.
    pub fn active_peers(&self) -> Vec<EndpointId> {
        let Ok(owners) = self.owners.lock() else {
            return Vec::new();
        };
        owners.iter().flatten().copied().collect()
    }

    /// Clears every slot.
    pub fn clear(&self) {
        let Ok(mut owners) = self.owners.lock() else {
            return;
        };
        for (index, owner) in owners.iter_mut().enumerate() {
            if owner.is_some() {
                *owner = None;
                self.shared[index].active.store(false, Ordering::Release);
                self.shared[index]
                    .generation
                    .fetch_add(1, Ordering::Release);
            }
        }
    }
}

/// Drains everything from a consumer without allocating.
pub fn drain(consumer: &mut PacketConsumer) {
    while consumer.try_pop().is_some() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::audio_wire::{AudioPacket, FLAG_TALKSPURT_START};
    use iroh::SecretKey;

    fn id(n: u8) -> EndpointId {
        SecretKey::from_bytes(&[n; 32]).public()
    }

    fn wire(seq: u16, payload: &[u8]) -> Packet {
        Packet::from_wire(&AudioPacket {
            seq,
            timestamp: seq as u32 * 480,
            flags: FLAG_TALKSPURT_START,
            payload,
        })
        .unwrap()
    }

    #[test]
    fn a_packet_keeps_its_payload() {
        let p = wire(3, &[9, 8, 7]);
        assert_eq!(p.seq, 3);
        assert_eq!(p.timestamp, 3 * 480);
        assert_eq!(p.payload(), &[9, 8, 7]);
        assert!(!p.is_empty());
    }

    #[test]
    fn an_oversized_payload_is_refused() {
        let big = vec![0u8; MAX_PACKET_BYTES + 1];
        assert!(
            Packet::from_wire(&AudioPacket {
                seq: 0,
                timestamp: 0,
                flags: 0,
                payload: &big,
            })
            .is_none()
        );
    }

    #[test]
    fn slots_are_handed_out_and_returned() {
        let (table, _cons) = SlotTable::new();

        let a = table.activate(id(1)).unwrap();
        let b = table.activate(id(2)).unwrap();
        assert_ne!(a, b);
        assert_eq!(table.index_of(id(1)), Some(a));
        assert!(table.slot(a).active.load(Ordering::Acquire));

        table.deactivate(id(1));
        assert_eq!(table.index_of(id(1)), None);
        assert!(!table.slot(a).active.load(Ordering::Acquire));

        // The freed slot is reused.
        assert_eq!(table.activate(id(3)), Some(a));
    }

    #[test]
    fn activating_twice_keeps_the_same_slot_and_generation() {
        let (table, _cons) = SlotTable::new();
        let first = table.activate(id(1)).unwrap();
        let generation = table.slot(first).generation.load(Ordering::Acquire);

        let second = table.activate(id(1)).unwrap();

        assert_eq!(first, second);
        assert_eq!(
            table.slot(first).generation.load(Ordering::Acquire),
            generation,
            "reactivating a peer must not reset its stream"
        );
    }

    #[test]
    fn reassigning_a_slot_bumps_the_generation() {
        let (table, _cons) = SlotTable::new();
        let index = table.activate(id(1)).unwrap();
        let before = table.slot(index).generation.load(Ordering::Acquire);

        table.deactivate(id(1));
        let same = table.activate(id(2)).unwrap();

        assert_eq!(same, index);
        assert!(table.slot(index).generation.load(Ordering::Acquire) > before);
    }

    #[test]
    fn the_table_runs_out_gracefully() {
        let (table, _cons) = SlotTable::new();
        for n in 1..=MAX_PEERS as u8 {
            assert!(table.activate(id(n)).is_some());
        }
        assert_eq!(table.activate(id(200)), None);

        table.clear();
        assert!(table.active_peers().is_empty());
        assert!(table.activate(id(200)).is_some());
    }

    #[test]
    fn an_overrun_is_counted_and_does_not_block() {
        let (table, mut consumers) = SlotTable::new();
        let index = table.activate(id(1)).unwrap();
        let slot = table.slot(index);

        let extra = 5u16;
        for seq in 0..(PEER_QUEUE_PACKETS as u16 + extra) {
            slot.push(wire(seq, &[seq as u8]));
        }

        assert_eq!(slot.overrun.load(Ordering::Relaxed), extra as u64);

        let consumer = &mut consumers[index];
        let mut count = 0;
        let mut previous = None;
        while let Some(p) = consumer.try_pop() {
            if let Some(prev) = previous {
                assert_eq!(p.seq, prev + 1, "the queue must not reorder");
            }
            previous = Some(p.seq);
            count += 1;
        }
        assert_eq!(count, PEER_QUEUE_PACKETS);
    }

    #[test]
    fn draining_empties_a_queue() {
        let (table, mut consumers) = SlotTable::new();
        let index = table.activate(id(1)).unwrap();
        for seq in 0..4u16 {
            table.slot(index).push(wire(seq, &[1]));
        }

        drain(&mut consumers[index]);
        assert!(consumers[index].try_pop().is_none());
    }
}
