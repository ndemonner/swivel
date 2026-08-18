//! The audio datagram format.
//!
//! Audio never uses a QUIC stream. A stream retransmits and it blocks on order.
//! Late audio is worse than missing audio, so audio goes in unreliable
//! datagrams. See `ARCHITECTURE.md` §4.5.
//!
//! ```text
//! byte 0   version (high 4 bits) | kind (low 4 bits)
//! byte 1   flags
//! byte 2-3 sequence, wrapping u16, little endian
//! byte 4-7 timestamp in samples at 48 kHz, wrapping u32, little endian
//! byte 8+  the Opus payload
//! ```

use crate::config::MAX_PACKET_BYTES;

/// The header length in bytes.
pub const HEADER_LEN: usize = 8;

/// The format version. It occupies the high 4 bits of byte 0.
const VERSION: u8 = 1;

/// Datagram kind. It occupies the low 4 bits of byte 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Audio = 1,
}

/// Bit 0 of the flags byte. It marks the first packet after a silence.
///
/// The receiver uses it to reset its jitter buffer without waiting for the
/// buffer to drain, and to avoid concealing a gap that was deliberate.
pub const FLAG_TALKSPURT_START: u8 = 0b0000_0001;

/// A parsed audio datagram. The payload borrows the receive buffer, so parsing
/// costs no allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPacket<'a> {
    pub seq: u16,
    pub timestamp: u32,
    pub flags: u8,
    pub payload: &'a [u8],
}

impl<'a> AudioPacket<'a> {
    /// True when this packet starts a talkspurt.
    pub fn is_talkspurt_start(&self) -> bool {
        self.flags & FLAG_TALKSPURT_START != 0
    }

    /// Writes the header and the payload into `out`.
    ///
    /// `out` must already hold room for `HEADER_LEN + payload.len()`. The
    /// encoder owns a buffer for the life of the process, so this never
    /// allocates.
    pub fn encode_into(
        seq: u16,
        timestamp: u32,
        flags: u8,
        payload: &[u8],
        out: &mut [u8],
    ) -> Result<usize, WireError> {
        let total = HEADER_LEN + payload.len();
        if out.len() < total {
            return Err(WireError::BufferTooSmall);
        }
        if payload.len() > MAX_PACKET_BYTES {
            return Err(WireError::PayloadTooLarge);
        }

        out[0] = (VERSION << 4) | (Kind::Audio as u8);
        out[1] = flags;
        out[2..4].copy_from_slice(&seq.to_le_bytes());
        out[4..8].copy_from_slice(&timestamp.to_le_bytes());
        out[HEADER_LEN..total].copy_from_slice(payload);
        Ok(total)
    }

    /// Parses a datagram.
    ///
    /// An unknown version or kind is an error, not a panic. Anything on the
    /// network is untrusted.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, WireError> {
        if bytes.len() < HEADER_LEN {
            return Err(WireError::TooShort);
        }

        let version = bytes[0] >> 4;
        if version != VERSION {
            return Err(WireError::UnknownVersion(version));
        }

        let kind = bytes[0] & 0x0f;
        if kind != Kind::Audio as u8 {
            return Err(WireError::UnknownKind(kind));
        }

        let payload = &bytes[HEADER_LEN..];
        if payload.len() > MAX_PACKET_BYTES {
            return Err(WireError::PayloadTooLarge);
        }
        if payload.is_empty() {
            return Err(WireError::TooShort);
        }

        Ok(AudioPacket {
            seq: u16::from_le_bytes([bytes[2], bytes[3]]),
            timestamp: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            flags: bytes[1],
            payload,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("the datagram is shorter than a header")]
    TooShort,
    #[error("the buffer is too small for the packet")]
    BufferTooSmall,
    #[error("the payload is larger than the codec can produce")]
    PayloadTooLarge,
    #[error("unknown format version {0}")]
    UnknownVersion(u8),
    #[error("unknown datagram kind {0}")]
    UnknownKind(u8),
}

/// Compares two wrapping sequence numbers.
///
/// Returns true when `a` is newer than `b`. This is the RFC 1982 rule. A plain
/// `>` breaks when the counter wraps, and at 100 packets per second a `u16`
/// wraps every 11 minutes. A session lasts longer than that.
pub fn seq_newer(a: u16, b: u16) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000
}

/// The signed distance from `b` to `a`, across a wrap.
pub fn seq_delta(a: u16, b: u16) -> i32 {
    let diff = a.wrapping_sub(b);
    if diff < 0x8000 {
        diff as i32
    } else {
        (diff as i32) - 0x1_0000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_packet_survives_a_round_trip() {
        let payload = [1u8, 2, 3, 4, 5];
        let mut buf = [0u8; 64];
        let n =
            AudioPacket::encode_into(7, 480 * 7, FLAG_TALKSPURT_START, &payload, &mut buf).unwrap();
        assert_eq!(n, HEADER_LEN + payload.len());

        let p = AudioPacket::decode(&buf[..n]).unwrap();
        assert_eq!(p.seq, 7);
        assert_eq!(p.timestamp, 480 * 7);
        assert_eq!(p.payload, &payload);
        assert!(p.is_talkspurt_start());
    }

    #[test]
    fn a_short_datagram_is_refused() {
        assert_eq!(AudioPacket::decode(&[]).unwrap_err(), WireError::TooShort);
        assert_eq!(
            AudioPacket::decode(&[0u8; 7]).unwrap_err(),
            WireError::TooShort
        );
        // A header with no payload carries no audio.
        let mut buf = [0u8; 8];
        buf[0] = (VERSION << 4) | 1;
        assert_eq!(AudioPacket::decode(&buf).unwrap_err(), WireError::TooShort);
    }

    #[test]
    fn a_future_version_is_refused_not_misread() {
        let mut buf = [0u8; 16];
        buf[0] = (9 << 4) | 1;
        assert_eq!(
            AudioPacket::decode(&buf).unwrap_err(),
            WireError::UnknownVersion(9)
        );
    }

    #[test]
    fn an_unknown_kind_is_refused() {
        let mut buf = [0u8; 16];
        buf[0] = (VERSION << 4) | 7;
        assert_eq!(
            AudioPacket::decode(&buf).unwrap_err(),
            WireError::UnknownKind(7)
        );
    }

    #[test]
    fn an_oversized_payload_is_refused_at_both_ends() {
        let payload = vec![0u8; MAX_PACKET_BYTES + 1];
        let mut buf = vec![0u8; MAX_PACKET_BYTES + 64];
        assert_eq!(
            AudioPacket::encode_into(0, 0, 0, &payload, &mut buf).unwrap_err(),
            WireError::PayloadTooLarge
        );

        let mut wire = vec![0u8; HEADER_LEN + MAX_PACKET_BYTES + 1];
        wire[0] = (VERSION << 4) | 1;
        assert_eq!(
            AudioPacket::decode(&wire).unwrap_err(),
            WireError::PayloadTooLarge
        );
    }

    #[test]
    fn a_small_buffer_is_refused() {
        let mut buf = [0u8; 4];
        assert_eq!(
            AudioPacket::encode_into(0, 0, 0, &[1, 2, 3], &mut buf).unwrap_err(),
            WireError::BufferTooSmall
        );
    }

    #[test]
    fn sequence_comparison_survives_a_wrap() {
        assert!(seq_newer(5, 4));
        assert!(!seq_newer(4, 5));
        assert!(!seq_newer(5, 5));

        // Across the wrap point 65535 -> 0.
        assert!(seq_newer(0, 65_535));
        assert!(seq_newer(3, 65_530));
        assert!(!seq_newer(65_535, 0));
    }

    #[test]
    fn sequence_distance_survives_a_wrap() {
        assert_eq!(seq_delta(10, 7), 3);
        assert_eq!(seq_delta(7, 10), -3);
        assert_eq!(seq_delta(2, 65_535), 3);
        assert_eq!(seq_delta(65_535, 2), -3);
        assert_eq!(seq_delta(0, 0), 0);
    }

    #[test]
    fn a_timestamp_wrap_is_representable() {
        // A u32 of samples at 48 kHz wraps after about 24.8 hours. Confirm the
        // encoder does not clamp it.
        let mut buf = [0u8; 16];
        let n = AudioPacket::encode_into(0, u32::MAX, 0, &[9], &mut buf).unwrap();
        assert_eq!(AudioPacket::decode(&buf[..n]).unwrap().timestamp, u32::MAX);
    }
}
