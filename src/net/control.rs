//! The control protocol.
//!
//! One bidirectional QUIC stream per connection carries everything that is not
//! audio: presence, session membership, and the application ping. It is
//! reliable and ordered, which is right for state and wrong for audio.
//!
//! Framing is a `u16` length prefix followed by a `postcard` body.

use iroh::EndpointId;
use iroh::endpoint::{RecvStream, SendStream};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The largest control message. A `SessionOpen` with 8 members is about 300
/// bytes, so this is generous. A peer that sends more is refused.
const MAX_MESSAGE: usize = 4096;

/// A control message.
///
/// Add a variant at the end. `postcard` encodes the variant index, so an
/// insertion in the middle breaks every older build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Control {
    /// The first message on the stream. It carries the sender's display name.
    Hello { name: String, version: u16 },

    /// The sender's availability changed.
    Presence {
        available: bool,
        dnd: bool,
        muted: bool,
    },

    /// Open a session, or update its membership.
    ///
    /// Every member receives the full list. Each member then opens audio to
    /// every other member. This is what makes a three-way conversation a mesh
    /// rather than two private links.
    SessionOpen {
        session: u64,
        members: Vec<[u8; 32]>,
    },

    /// Leave a session.
    SessionClose { session: u64 },

    /// The sender started or stopped speaking. This drives the roster only.
    TalkState { session: u64, speaking: bool },

    /// Round trip probe.
    Ping { nonce: u64 },

    /// Round trip reply. `nonce` is copied from the `Ping`.
    Pong { nonce: u64 },
}

impl Control {
    /// Reads the member list as endpoint ids, skipping any that do not parse.
    ///
    /// A peer controls this list. A bad entry must not stop the good ones.
    pub fn members_as_ids(members: &[[u8; 32]]) -> Vec<EndpointId> {
        members
            .iter()
            .filter_map(|b| EndpointId::from_bytes(b).ok())
            .collect()
    }

    /// Turns endpoint ids into the wire form.
    pub fn ids_as_members(ids: impl IntoIterator<Item = EndpointId>) -> Vec<[u8; 32]> {
        ids.into_iter().map(|id| *id.as_bytes()).collect()
    }
}

/// Writes one message with a `u16` length prefix.
pub async fn write_message(stream: &mut SendStream, msg: &Control) -> Result<()> {
    let body = postcard::to_allocvec(msg)
        .map_err(|e| Error::net(format!("cannot encode a control message: {e}")))?;

    if body.len() > MAX_MESSAGE {
        return Err(Error::net("a control message is too large to send"));
    }

    let len = (body.len() as u16).to_le_bytes();
    stream.write_all(&len).await.map_err(Error::net)?;
    stream.write_all(&body).await.map_err(Error::net)?;
    Ok(())
}

/// Reads one message.
///
/// `buf` is reused across calls so a busy connection does not allocate per
/// message.
pub async fn read_message(stream: &mut RecvStream, buf: &mut Vec<u8>) -> Result<Control> {
    let mut len = [0u8; 2];
    stream.read_exact(&mut len).await.map_err(Error::net)?;
    let len = u16::from_le_bytes(len) as usize;

    if len == 0 {
        return Err(Error::net("a control message claims zero length"));
    }
    if len > MAX_MESSAGE {
        // Do not allocate what a peer asks for. Close instead.
        return Err(Error::net(format!(
            "a peer claims a {len} byte control message, and the limit is {MAX_MESSAGE}"
        )));
    }

    buf.clear();
    buf.resize(len, 0);
    stream.read_exact(buf).await.map_err(Error::net)?;

    postcard::from_bytes(buf)
        .map_err(|e| Error::net(format!("cannot decode a control message: {e}")))
}

/// Trims a name that came from the network.
///
/// A peer chooses this string and it lands in the roster. Strip control
/// characters and cap the length.
pub fn clean_name(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(40).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn id(n: u8) -> EndpointId {
        SecretKey::from_bytes(&[n; 32]).public()
    }

    fn round_trip(msg: &Control) -> Control {
        let bytes = postcard::to_allocvec(msg).unwrap();
        postcard::from_bytes(&bytes).unwrap()
    }

    #[test]
    fn every_variant_survives_a_round_trip() {
        let cases = [
            Control::Hello {
                name: "Maggie".into(),
                version: 1,
            },
            Control::Presence {
                available: true,
                dnd: false,
                muted: true,
            },
            Control::SessionOpen {
                session: 42,
                members: Control::ids_as_members([id(1), id(2)]),
            },
            Control::SessionClose { session: 42 },
            Control::TalkState {
                session: 42,
                speaking: true,
            },
            Control::Ping { nonce: 9 },
            Control::Pong { nonce: 9 },
        ];

        for case in cases {
            assert_eq!(round_trip(&case), case);
        }
    }

    #[test]
    fn members_convert_both_ways() {
        let ids = vec![id(1), id(2), id(3)];
        let wire = Control::ids_as_members(ids.clone());
        assert_eq!(Control::members_as_ids(&wire), ids);
    }

    #[test]
    fn a_bad_member_is_skipped_not_fatal() {
        let mut wire = Control::ids_as_members([id(1)]);
        // [2; 32] does not decompress to a point on the curve. It was found by
        // probing, because most byte patterns do decompress.
        assert!(EndpointId::from_bytes(&[2u8; 32]).is_err());
        wire.push([2u8; 32]);
        wire.push(*id(2).as_bytes());

        let parsed = Control::members_as_ids(&wire);
        assert_eq!(parsed, vec![id(1), id(2)]);
    }

    #[test]
    fn a_hostile_name_is_cleaned() {
        let cleaned = clean_name(&format!("a\nb\u{0}{}", "x".repeat(100)));
        assert!(cleaned.len() <= 40);
        assert!(!cleaned.contains('\n'));
    }

    #[test]
    fn a_session_open_with_eight_members_fits_the_limit() {
        let members = Control::ids_as_members((1..=8u8).map(id));
        let msg = Control::SessionOpen {
            session: u64::MAX,
            members,
        };
        assert!(postcard::to_allocvec(&msg).unwrap().len() < MAX_MESSAGE);
    }
}
