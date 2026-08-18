//! Knocks.
//!
//! A knock is a connection attempt from an endpoint you have not approved.
//! `swivel` records it, refuses the connection, and shows it in the roster.
//! See `DESIGN.md` §4.3.

use iroh::EndpointId;
use rusqlite::{OptionalExtension, params};

use super::{Store, now_secs};
use crate::error::{Error, Result};

/// A pending or blocked connection attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Knock {
    pub endpoint_id: EndpointId,
    /// The name the caller claims. Treat it as untrusted text.
    pub claimed: Option<String>,
    pub first_seen: i64,
    pub blocked: bool,
}

/// What the accept loop should do with an inbound connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// An approved contact. Register the connection.
    Contact,
    /// Not approved. Record a knock and refuse.
    Knock,
    /// Rejected before. Drop it with no record and no user interface noise.
    Blocked,
}

impl Store {
    /// Decides what to do with an inbound connection.
    pub fn admit(&self, endpoint_id: EndpointId) -> Result<Admission> {
        if self.is_contact(endpoint_id)? {
            return Ok(Admission::Contact);
        }
        match self.knock(endpoint_id)? {
            Some(k) if k.blocked => Ok(Admission::Blocked),
            _ => Ok(Admission::Knock),
        }
    }

    /// Records a knock, or refreshes the claimed name on a repeat.
    ///
    /// The claimed name is truncated. A caller must not be able to push a long
    /// string into the roster.
    pub fn record_knock(&self, endpoint_id: EndpointId, claimed: Option<&str>) -> Result<()> {
        let claimed = claimed.map(|s| {
            let clean: String = s.chars().filter(|c| !c.is_control()).take(40).collect();
            clean
        });

        self.conn().execute(
            "INSERT INTO knocks (endpoint_id, claimed, first_seen, blocked) \
             VALUES (?1, ?2, ?3, 0) \
             ON CONFLICT(endpoint_id) DO UPDATE SET claimed = COALESCE(?2, claimed)",
            params![endpoint_id.to_string(), claimed, now_secs()],
        )?;
        Ok(())
    }

    /// Reads one knock.
    pub fn knock(&self, endpoint_id: EndpointId) -> Result<Option<Knock>> {
        self.conn()
            .query_row(
                "SELECT endpoint_id, claimed, first_seen, blocked FROM knocks \
                 WHERE endpoint_id = ?1",
                [endpoint_id.to_string()],
                row_to_knock,
            )
            .optional()
            .map_err(Error::from)
            .map(|o| o.flatten())
    }

    /// Lists the knocks waiting for a decision, oldest first.
    pub fn pending_knocks(&self) -> Result<Vec<Knock>> {
        let mut stmt = self.conn().prepare(
            "SELECT endpoint_id, claimed, first_seen, blocked FROM knocks \
             WHERE blocked = 0 ORDER BY first_seen",
        )?;
        let rows = stmt.query_map([], row_to_knock)?;

        let mut out = Vec::new();
        for row in rows {
            if let Some(k) = row? {
                out.push(k);
            }
        }
        Ok(out)
    }

    /// Turns a knock into a contact and clears the knock.
    pub fn approve_knock(&self, endpoint_id: EndpointId, name: Option<&str>) -> Result<()> {
        let knock = self
            .knock(endpoint_id)?
            .ok_or_else(|| Error::NoSuchContact(endpoint_id.to_string()))?;

        let name = name
            .map(str::to_string)
            .or(knock.claimed)
            .unwrap_or_else(|| endpoint_id.fmt_short().to_string());

        self.add_contact(endpoint_id, &name)?;
        self.conn().execute(
            "DELETE FROM knocks WHERE endpoint_id = ?1",
            [endpoint_id.to_string()],
        )?;
        Ok(())
    }

    /// Blocks an endpoint. It cannot knock again.
    pub fn block(&self, endpoint_id: EndpointId) -> Result<()> {
        self.conn().execute(
            "INSERT INTO knocks (endpoint_id, claimed, first_seen, blocked) \
             VALUES (?1, NULL, ?2, 1) \
             ON CONFLICT(endpoint_id) DO UPDATE SET blocked = 1",
            params![endpoint_id.to_string(), now_secs()],
        )?;
        Ok(())
    }

    /// Lifts a block.
    pub fn unblock(&self, endpoint_id: EndpointId) -> Result<()> {
        self.conn().execute(
            "DELETE FROM knocks WHERE endpoint_id = ?1",
            [endpoint_id.to_string()],
        )?;
        Ok(())
    }
}

fn row_to_knock(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<Knock>> {
    let id_text: String = row.get(0)?;
    let Ok(endpoint_id) = id_text.parse::<EndpointId>() else {
        return Ok(None);
    };
    Ok(Some(Knock {
        endpoint_id,
        claimed: row.get(1)?,
        first_seen: row.get(2)?,
        blocked: row.get::<_, i64>(3)? != 0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn id(n: u8) -> EndpointId {
        SecretKey::from_bytes(&[n; 32]).public()
    }

    #[test]
    fn an_unknown_endpoint_knocks() {
        let store = Store::open_memory().unwrap();
        assert_eq!(store.admit(id(1)).unwrap(), Admission::Knock);
    }

    #[test]
    fn a_contact_is_admitted() {
        let store = Store::open_memory().unwrap();
        store.add_contact(id(1), "maggie").unwrap();
        assert_eq!(store.admit(id(1)).unwrap(), Admission::Contact);
    }

    #[test]
    fn approval_creates_a_contact_with_a_slot() {
        let store = Store::open_memory().unwrap();
        store.record_knock(id(1), Some("Will")).unwrap();
        assert_eq!(store.pending_knocks().unwrap().len(), 1);

        store.approve_knock(id(1), None).unwrap();

        let c = store.contact(id(1)).unwrap().unwrap();
        assert_eq!(c.name, "Will");
        assert_eq!(c.slot, Some(1));
        assert!(store.pending_knocks().unwrap().is_empty());
    }

    #[test]
    fn a_blocked_endpoint_stays_blocked() {
        let store = Store::open_memory().unwrap();
        store.record_knock(id(1), None).unwrap();
        store.block(id(1)).unwrap();

        assert_eq!(store.admit(id(1)).unwrap(), Admission::Blocked);
        assert!(store.pending_knocks().unwrap().is_empty());

        // A second attempt must not resurrect it as pending.
        store.record_knock(id(1), Some("please")).unwrap();
        assert_eq!(store.admit(id(1)).unwrap(), Admission::Blocked);
    }

    #[test]
    fn a_claimed_name_cannot_be_long_or_contain_control_characters() {
        let store = Store::open_memory().unwrap();
        let nasty = format!("bad\nname\u{0}{}", "x".repeat(200));
        store.record_knock(id(1), Some(&nasty)).unwrap();

        let claimed = store.knock(id(1)).unwrap().unwrap().claimed.unwrap();
        assert!(claimed.len() <= 40);
        assert!(!claimed.contains('\n'));
        assert!(!claimed.contains('\u{0}'));
    }
}
