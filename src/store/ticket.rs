//! The shareable key, called a ticket.
//!
//! A ticket carries a public key and a display name. You send it to a friend
//! over any channel. It contains no secret. See `DESIGN.md` §4.1.

use data_encoding::BASE32_NOPAD;
use iroh::EndpointId;
use serde::{Deserialize, Serialize};

use crate::config::TICKET_PREFIX;
use crate::error::{Error, Result};

/// The wire form of a ticket. `postcard` encodes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TicketV1 {
    /// The format version. A future format changes this first byte, so an old
    /// build can say "you need a newer walkie" instead of failing obscurely.
    version: u8,
    /// The 32 byte Ed25519 public key.
    key: [u8; 32],
    /// The name the sender suggests. The receiver may override it.
    name: String,
}

const VERSION: u8 = 1;

/// A decoded ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub endpoint_id: EndpointId,
    pub name: String,
}

impl Ticket {
    /// Builds a ticket for the local identity.
    pub fn new(endpoint_id: EndpointId, name: impl Into<String>) -> Self {
        Ticket {
            endpoint_id,
            name: name.into(),
        }
    }

    /// Encodes the ticket as a `wt1…` string.
    pub fn encode(&self) -> String {
        let body = TicketV1 {
            version: VERSION,
            key: *self.endpoint_id.as_bytes(),
            name: self.name.clone(),
        };

        // postcard on a fixed struct cannot fail for these field types.
        let bytes = postcard::to_allocvec(&body).expect("postcard cannot fail here");
        format!(
            "{TICKET_PREFIX}{}",
            BASE32_NOPAD.encode(&bytes).to_lowercase()
        )
    }

    /// Decodes a `wt1…` string.
    ///
    /// The input is trimmed and case is ignored, because a ticket travels
    /// through chat applications that reformat text.
    pub fn decode(s: &str) -> Result<Self> {
        let s = s.trim();

        let body = s
            .strip_prefix(TICKET_PREFIX)
            .or_else(|| s.strip_prefix(&TICKET_PREFIX.to_uppercase()))
            .ok_or_else(|| Error::Ticket(format!("a key starts with `{TICKET_PREFIX}`")))?;

        let bytes = BASE32_NOPAD
            .decode(body.to_uppercase().as_bytes())
            .map_err(|_| Error::Ticket("the key contains a character that is not valid".into()))?;

        let decoded: TicketV1 = postcard::from_bytes(&bytes)
            .map_err(|_| Error::Ticket("the key is truncated or damaged".into()))?;

        if decoded.version != VERSION {
            return Err(Error::Ticket(format!(
                "the key is format version {}, and this build reads version {VERSION}. \
                 Use a newer walkie.",
                decoded.version
            )));
        }

        let endpoint_id = EndpointId::from_bytes(&decoded.key)
            .map_err(|_| Error::Ticket("the key does not contain a valid public key".into()))?;

        Ok(Ticket {
            endpoint_id,
            name: decoded.name,
        })
    }
}

impl std::fmt::Display for Ticket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.encode())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn some_id() -> EndpointId {
        SecretKey::from_bytes(&[7u8; 32]).public()
    }

    #[test]
    fn a_ticket_survives_a_round_trip() {
        let ticket = Ticket::new(some_id(), "Maggie Henry");
        let text = ticket.encode();
        assert!(text.starts_with("wt1"));
        assert_eq!(Ticket::decode(&text).unwrap(), ticket);
    }

    #[test]
    fn whitespace_and_case_do_not_matter() {
        let ticket = Ticket::new(some_id(), "will");
        let text = ticket.encode();
        let messy = format!("  {}  ", text.to_uppercase());
        assert_eq!(Ticket::decode(&messy).unwrap(), ticket);
    }

    #[test]
    fn a_name_with_unicode_survives() {
        let ticket = Ticket::new(some_id(), "Zoë 🎙");
        assert_eq!(Ticket::decode(&ticket.encode()).unwrap(), ticket);
    }

    #[test]
    fn a_missing_prefix_says_so() {
        let e = Ticket::decode("hello").unwrap_err();
        assert!(e.to_string().contains("starts with"), "{e}");
    }

    #[test]
    fn a_bad_character_says_so() {
        let e = Ticket::decode("wt1!!!!").unwrap_err();
        assert!(e.to_string().contains("not valid"), "{e}");
    }

    #[test]
    fn a_truncated_ticket_says_so() {
        let ticket = Ticket::new(some_id(), "will").encode();
        let cut = &ticket[..ticket.len() / 2];
        let e = Ticket::decode(cut).unwrap_err();
        assert!(
            e.to_string().contains("truncated") || e.to_string().contains("not valid"),
            "{e}"
        );
    }
}
