//! Contacts and slot numbers.
//!
//! A slot is the number you press to talk to a person. It is the whole point of
//! the interaction model, so slot assignment must never surprise the user.

use iroh::EndpointId;
use rusqlite::{OptionalExtension, params};

use super::{Store, now_secs};
use crate::config::MAX_SLOT;
use crate::error::{Error, Result};

/// One person you can talk to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub endpoint_id: EndpointId,
    /// `None` means the contact is past slot 9 and has no single keypress.
    pub slot: Option<u8>,
    pub name: String,
    /// When false, this contact must knock before their audio opens.
    pub auto_open: bool,
    pub added_at: i64,
    pub last_seen: Option<i64>,
}

impl Store {
    /// Adds a contact and gives it the lowest free slot.
    ///
    /// Adding a contact that already exists updates the name and returns the
    /// existing record. This makes `walkie add` safe to run twice.
    pub fn add_contact(&self, endpoint_id: EndpointId, name: &str) -> Result<Contact> {
        if let Some(existing) = self.contact(endpoint_id)? {
            if existing.name != name {
                self.conn().execute(
                    "UPDATE contacts SET name = ?1 WHERE endpoint_id = ?2",
                    params![name, endpoint_id.to_string()],
                )?;
            }
            return self
                .contact(endpoint_id)?
                .ok_or_else(|| Error::NoSuchContact(endpoint_id.to_string()));
        }

        let slot = self.lowest_free_slot()?;

        self.conn().execute(
            "INSERT INTO contacts (endpoint_id, slot, name, auto_open, added_at) \
             VALUES (?1, ?2, ?3, 1, ?4)",
            params![endpoint_id.to_string(), slot, name, now_secs()],
        )?;

        self.contact(endpoint_id)?
            .ok_or_else(|| Error::NoSuchContact(endpoint_id.to_string()))
    }

    /// Reads one contact.
    pub fn contact(&self, endpoint_id: EndpointId) -> Result<Option<Contact>> {
        self.conn()
            .query_row(
                "SELECT endpoint_id, slot, name, auto_open, added_at, last_seen \
                 FROM contacts WHERE endpoint_id = ?1",
                [endpoint_id.to_string()],
                row_to_contact,
            )
            .optional()
            .map_err(Error::from)
            .map(|o| o.flatten())
    }

    /// Reads every contact, ordered by slot. Contacts without a slot come last.
    pub fn contacts(&self) -> Result<Vec<Contact>> {
        let mut stmt = self.conn().prepare(
            "SELECT endpoint_id, slot, name, auto_open, added_at, last_seen \
             FROM contacts ORDER BY slot IS NULL, slot, name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], row_to_contact)?;

        let mut out = Vec::new();
        for row in rows {
            if let Some(c) = row? {
                out.push(c);
            }
        }
        Ok(out)
    }

    /// Finds a contact by slot number.
    pub fn contact_by_slot(&self, slot: u8) -> Result<Option<Contact>> {
        self.conn()
            .query_row(
                "SELECT endpoint_id, slot, name, auto_open, added_at, last_seen \
                 FROM contacts WHERE slot = ?1",
                [slot],
                row_to_contact,
            )
            .optional()
            .map_err(Error::from)
            .map(|o| o.flatten())
    }

    /// Finds a contact by slot number, by name, or by the start of a key.
    ///
    /// This backs `walkie rm 3` and `walkie rm maggie`.
    pub fn find_contact(&self, needle: &str) -> Result<Contact> {
        let needle = needle.trim();

        if let Ok(slot) = needle.parse::<u8>()
            && let Some(c) = self.contact_by_slot(slot)?
        {
            return Ok(c);
        }

        let all = self.contacts()?;
        let lower = needle.to_lowercase();

        let matches: Vec<&Contact> = all
            .iter()
            .filter(|c| {
                c.name.to_lowercase() == lower
                    || c.name.to_lowercase().starts_with(&lower)
                    || c.endpoint_id.to_string().starts_with(&lower)
            })
            .collect();

        match matches.len() {
            1 => Ok(matches[0].clone()),
            0 => Err(Error::NoSuchContact(needle.to_string())),
            _ => Err(Error::NoSuchContact(format!(
                "{needle}. It matches {} contacts: {}",
                matches.len(),
                matches
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    /// Removes a contact and frees its slot.
    pub fn remove_contact(&self, endpoint_id: EndpointId) -> Result<()> {
        self.conn().execute(
            "DELETE FROM contacts WHERE endpoint_id = ?1",
            [endpoint_id.to_string()],
        )?;
        Ok(())
    }

    /// Moves a contact to a slot.
    ///
    /// If another contact holds the slot, the two swap. A swap never fails, and
    /// it never silently leaves a contact without a number.
    pub fn set_slot(&self, endpoint_id: EndpointId, slot: u8) -> Result<()> {
        if slot == 0 || slot > MAX_SLOT {
            return Err(Error::BadSlot(slot));
        }

        let target = self
            .contact(endpoint_id)?
            .ok_or_else(|| Error::NoSuchContact(endpoint_id.to_string()))?;

        if target.slot == Some(slot) {
            return Ok(());
        }

        let occupant = self.contact_by_slot(slot)?;

        // A UNIQUE index guards the slot column, so the swap goes through a
        // vacant value. -1 can never collide with a real slot.
        let tx = self.conn().unchecked_transaction()?;
        tx.execute(
            "UPDATE contacts SET slot = -1 WHERE endpoint_id = ?1",
            [endpoint_id.to_string()],
        )?;
        if let Some(occupant) = &occupant {
            tx.execute(
                "UPDATE contacts SET slot = ?1 WHERE endpoint_id = ?2",
                params![target.slot, occupant.endpoint_id.to_string()],
            )?;
        }
        tx.execute(
            "UPDATE contacts SET slot = ?1 WHERE endpoint_id = ?2",
            params![slot, endpoint_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Sets whether a contact may open your microphone without a keypress.
    pub fn set_auto_open(&self, endpoint_id: EndpointId, auto_open: bool) -> Result<()> {
        self.conn().execute(
            "UPDATE contacts SET auto_open = ?1 WHERE endpoint_id = ?2",
            params![auto_open as i32, endpoint_id.to_string()],
        )?;
        Ok(())
    }

    /// Records that a contact was reachable just now.
    pub fn touch_contact(&self, endpoint_id: EndpointId) -> Result<()> {
        self.conn().execute(
            "UPDATE contacts SET last_seen = ?1 WHERE endpoint_id = ?2",
            params![now_secs(), endpoint_id.to_string()],
        )?;
        Ok(())
    }

    /// Returns true when the endpoint is an approved contact.
    pub fn is_contact(&self, endpoint_id: EndpointId) -> Result<bool> {
        Ok(self.contact(endpoint_id)?.is_some())
    }

    fn lowest_free_slot(&self) -> Result<Option<u8>> {
        let mut stmt = self
            .conn()
            .prepare("SELECT slot FROM contacts WHERE slot IS NOT NULL ORDER BY slot")?;
        let taken: Vec<u8> = stmt
            .query_map([], |row| row.get::<_, i64>(0))?
            .filter_map(|r| r.ok())
            .filter(|s| *s >= 1)
            .map(|s| s as u8)
            .collect();

        for slot in 1..=MAX_SLOT {
            if !taken.contains(&slot) {
                return Ok(Some(slot));
            }
        }
        Ok(None)
    }
}

/// Turns a row into a `Contact`.
///
/// A row with a key that no longer parses yields `None` rather than an error.
/// One damaged row must not stop the roster from loading.
fn row_to_contact(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<Contact>> {
    let id_text: String = row.get(0)?;
    let Ok(endpoint_id) = id_text.parse::<EndpointId>() else {
        return Ok(None);
    };

    let slot: Option<i64> = row.get(1)?;

    Ok(Some(Contact {
        endpoint_id,
        slot: slot.and_then(|s| u8::try_from(s).ok()).filter(|s| *s >= 1),
        name: row.get(2)?,
        auto_open: row.get::<_, i64>(3)? != 0,
        added_at: row.get(4)?,
        last_seen: row.get(5)?,
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
    fn the_first_contact_gets_slot_one() {
        let store = Store::open_memory().unwrap();
        let c = store.add_contact(id(1), "maggie").unwrap();
        assert_eq!(c.slot, Some(1));
    }

    #[test]
    fn slots_fill_the_lowest_gap() {
        let store = Store::open_memory().unwrap();
        store.add_contact(id(1), "a").unwrap();
        let b = store.add_contact(id(2), "b").unwrap();
        store.add_contact(id(3), "c").unwrap();
        assert_eq!(b.slot, Some(2));

        store.remove_contact(b.endpoint_id).unwrap();
        let d = store.add_contact(id(4), "d").unwrap();
        assert_eq!(d.slot, Some(2), "a freed slot is reused");
    }

    #[test]
    fn contacts_past_nine_have_no_slot() {
        let store = Store::open_memory().unwrap();
        for n in 1..=9u8 {
            assert!(
                store
                    .add_contact(id(n), &format!("p{n}"))
                    .unwrap()
                    .slot
                    .is_some()
            );
        }
        let tenth = store.add_contact(id(10), "p10").unwrap();
        assert_eq!(tenth.slot, None);
    }

    #[test]
    fn set_slot_swaps_rather_than_fails() {
        let store = Store::open_memory().unwrap();
        let a = store.add_contact(id(1), "a").unwrap();
        let b = store.add_contact(id(2), "b").unwrap();
        assert_eq!((a.slot, b.slot), (Some(1), Some(2)));

        store.set_slot(b.endpoint_id, 1).unwrap();

        assert_eq!(store.contact(b.endpoint_id).unwrap().unwrap().slot, Some(1));
        assert_eq!(store.contact(a.endpoint_id).unwrap().unwrap().slot, Some(2));
    }

    #[test]
    fn set_slot_to_an_empty_number_moves_without_a_partner() {
        let store = Store::open_memory().unwrap();
        let a = store.add_contact(id(1), "a").unwrap();
        store.set_slot(a.endpoint_id, 7).unwrap();
        assert_eq!(store.contact(a.endpoint_id).unwrap().unwrap().slot, Some(7));
        assert!(store.contact_by_slot(1).unwrap().is_none());
    }

    #[test]
    fn a_slot_outside_the_range_is_refused() {
        let store = Store::open_memory().unwrap();
        let a = store.add_contact(id(1), "a").unwrap();
        assert!(store.set_slot(a.endpoint_id, 0).is_err());
        assert!(store.set_slot(a.endpoint_id, 10).is_err());
    }

    #[test]
    fn adding_twice_updates_the_name() {
        let store = Store::open_memory().unwrap();
        let first = store.add_contact(id(1), "maggie").unwrap();
        let second = store.add_contact(id(1), "Maggie Henry").unwrap();
        assert_eq!(first.slot, second.slot);
        assert_eq!(second.name, "Maggie Henry");
        assert_eq!(store.contacts().unwrap().len(), 1);
    }

    #[test]
    fn find_by_slot_name_and_key() {
        let store = Store::open_memory().unwrap();
        let c = store.add_contact(id(1), "Maggie Henry").unwrap();

        assert_eq!(store.find_contact("1").unwrap(), c);
        assert_eq!(store.find_contact("maggie").unwrap(), c);
        assert_eq!(store.find_contact("MAGGIE HENRY").unwrap(), c);
        let prefix = &c.endpoint_id.to_string()[..8];
        assert_eq!(store.find_contact(prefix).unwrap(), c);
        assert!(store.find_contact("nobody").is_err());
    }

    #[test]
    fn an_ambiguous_name_lists_the_candidates() {
        let store = Store::open_memory().unwrap();
        store.add_contact(id(1), "Will Tachau").unwrap();
        store.add_contact(id(2), "Willow Smith").unwrap();
        let e = store.find_contact("wil").unwrap_err().to_string();
        assert!(
            e.contains("Will Tachau") && e.contains("Willow Smith"),
            "{e}"
        );
    }
}
