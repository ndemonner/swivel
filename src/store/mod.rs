//! Local state. There is no server and no account, so this file holds
//! everything the product remembers.

pub mod contacts;
pub mod identity;
pub mod knocks;
pub mod ticket;

use std::path::PathBuf;

use rusqlite::Connection;
use tracing::debug;

use crate::error::{Error, Result};

/// The schema version this build expects.
const SCHEMA_VERSION: i32 = 1;

/// A handle to the local database.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens the database at the default path and runs any missing migration.
    pub fn open() -> Result<Self> {
        Self::open_at(default_path()?)
    }

    /// Opens the database at an explicit path. Used by tests and by the
    /// `WALKIE_DB` override, which lets two instances run on one machine.
    pub fn open_at(path: PathBuf) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| Error::CreateDir {
                path: dir.to_path_buf(),
                source,
            })?;
        }

        let conn = Connection::open(&path)?;

        // The database holds a private key. Nobody else may read it.
        restrict_permissions(&path)?;

        // WAL keeps a reader from blocking the writer. The user interface reads
        // while the network layer writes.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let mut store = Store { conn };
        store.migrate()?;
        debug!(?path, "store open");
        Ok(store)
    }

    /// Opens a temporary in-memory database. Tests only.
    #[cfg(test)]
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let mut store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Exposes the connection to the sub-modules in this directory.
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    fn migrate(&mut self) -> Result<()> {
        let current: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;

        if current > SCHEMA_VERSION {
            return Err(Error::Other(anyhow::anyhow!(
                "the database is version {current} but this build only knows \
                 version {SCHEMA_VERSION}. Use a newer walkie."
            )));
        }

        // Each step runs in order. Add a new step, never edit an old one.
        if current < 1 {
            self.conn.execute_batch(MIGRATION_1)?;
        }

        self.conn
            .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }
}

/// The first schema. See `ARCHITECTURE.md` §8.
const MIGRATION_1: &str = r#"
CREATE TABLE identity (
  id          INTEGER PRIMARY KEY CHECK (id = 1),
  secret_key  BLOB    NOT NULL,
  name        TEXT    NOT NULL,
  created_at  INTEGER NOT NULL
);

CREATE TABLE contacts (
  endpoint_id TEXT    PRIMARY KEY,
  slot        INTEGER UNIQUE,
  name        TEXT    NOT NULL,
  auto_open   INTEGER NOT NULL DEFAULT 1,
  added_at    INTEGER NOT NULL,
  last_seen   INTEGER
);

CREATE TABLE knocks (
  endpoint_id TEXT    PRIMARY KEY,
  claimed     TEXT,
  first_seen  INTEGER NOT NULL,
  blocked     INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
"#;

/// Returns the database path, honouring the `WALKIE_DB` override.
pub fn default_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("WALKIE_DB") {
        return Ok(PathBuf::from(p));
    }

    let home = std::env::var_os("HOME").ok_or(Error::NoHome)?;
    let mut path = PathBuf::from(home);
    path.push("Library");
    path.push("Application Support");
    path.push("dev.motor.walkie");
    path.push("walkie.db");
    Ok(path)
}

/// Sets mode `0600` on the database file.
#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

/// Seconds since the Unix epoch. The database stores every time this way.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_is_idempotent() {
        let mut store = Store::open_memory().unwrap();
        store.migrate().unwrap();
        store.migrate().unwrap();
        let v: i32 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn env_override_wins() {
        // SAFETY: single-threaded test, and the variable is read immediately.
        unsafe { std::env::set_var("WALKIE_DB", "/tmp/walkie-test-override.db") };
        assert_eq!(
            default_path().unwrap(),
            PathBuf::from("/tmp/walkie-test-override.db")
        );
        unsafe { std::env::remove_var("WALKIE_DB") };
    }
}
