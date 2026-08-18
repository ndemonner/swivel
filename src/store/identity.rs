//! The local keypair.
//!
//! There is no account. The keypair *is* the identity. It is created once and
//! it never leaves this machine.

use iroh::{EndpointId, SecretKey};
use rusqlite::OptionalExtension;
use tracing::info;

use super::{Store, now_secs};
use crate::error::Result;

/// The local identity.
#[derive(Clone)]
pub struct Identity {
    pub secret_key: SecretKey,
    pub name: String,
}

impl Identity {
    /// The public half. This is what you share.
    pub fn endpoint_id(&self) -> EndpointId {
        self.secret_key.public()
    }
}

impl std::fmt::Debug for Identity {
    /// Never print the secret half.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("endpoint_id", &self.endpoint_id())
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Store {
    /// Loads the identity, or creates one on the first run.
    ///
    /// `default_name` is used only when the identity does not exist yet.
    pub fn identity(&self, default_name: &str) -> Result<Identity> {
        if let Some(id) = self.load_identity()? {
            return Ok(id);
        }

        let secret_key = SecretKey::generate();
        let name = default_name.to_string();

        self.conn().execute(
            "INSERT INTO identity (id, secret_key, name, created_at) VALUES (1, ?1, ?2, ?3)",
            rusqlite::params![secret_key.to_bytes().as_slice(), &name, now_secs()],
        )?;

        let identity = Identity { secret_key, name };
        info!(endpoint_id = %identity.endpoint_id(), "created a new identity");
        Ok(identity)
    }

    fn load_identity(&self) -> Result<Option<Identity>> {
        let row = self
            .conn()
            .query_row(
                "SELECT secret_key, name FROM identity WHERE id = 1",
                [],
                |row| {
                    let bytes: Vec<u8> = row.get(0)?;
                    let name: String = row.get(1)?;
                    Ok((bytes, name))
                },
            )
            .optional()?;

        let Some((bytes, name)) = row else {
            return Ok(None);
        };

        let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
            crate::error::Error::Other(anyhow::anyhow!(
                "the stored secret key is not 32 bytes. The database is corrupt."
            ))
        })?;

        Ok(Some(Identity {
            secret_key: SecretKey::from_bytes(&bytes),
            name,
        }))
    }

    /// Changes the display name that contacts see.
    pub fn set_name(&self, name: &str) -> Result<()> {
        self.conn()
            .execute("UPDATE identity SET name = ?1 WHERE id = 1", [name])?;
        Ok(())
    }
}

/// A reasonable first name for a new install. The user can change it.
pub fn default_name() -> String {
    std::env::var("USER").unwrap_or_else(|_| "swivel".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_survives_a_reload() {
        let store = Store::open_memory().unwrap();
        let first = store.identity("nick").unwrap();
        let second = store.identity("someone else").unwrap();

        assert_eq!(first.endpoint_id(), second.endpoint_id());
        // The default name applies only on creation.
        assert_eq!(second.name, "nick");
    }

    #[test]
    fn debug_hides_the_secret() {
        let store = Store::open_memory().unwrap();
        let id = store.identity("nick").unwrap();
        let text = format!("{id:?}");
        let secret = format!("{:?}", id.secret_key.to_bytes());
        assert!(!text.contains(&secret));
    }
}
