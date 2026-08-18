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
    /// `SWIVEL_DB` override, which lets two instances run on one machine.
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
                 version {SCHEMA_VERSION}. Use a newer swivel."
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

/// Returns the database path, honouring the `SWIVEL_DB` override.
pub fn default_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("SWIVEL_DB") {
        return Ok(PathBuf::from(p));
    }

    let home = std::env::var_os("HOME").ok_or(Error::NoHome)?;
    let mut path = PathBuf::from(home);
    path.push("Library");
    path.push("Application Support");
    path.push("dev.motor.swivel");
    path.push("swivel.db");

    // The product was called `walkie` until 2026-08-18. Carry an existing
    // identity across rather than silently handing the user a new key and
    // breaking any key they already shared.
    //
    // T-131 removes this once nobody is on the old name.
    if !path.exists() {
        adopt_former_name(&path);
    }

    Ok(path)
}

/// Moves a database left behind by the former name, if there is one.
///
/// Every failure here is ignored on purpose. The worst outcome is a new
/// identity, and refusing to start would be worse than that.
fn adopt_former_name(new_path: &std::path::Path) {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };

    let mut old = PathBuf::from(home);
    old.push("Library");
    old.push("Application Support");
    old.push("dev.motor.walkie");
    let old_db = old.join("walkie.db");

    if !old_db.exists() {
        return;
    }
    let Some(parent) = new_path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }

    // The write-ahead log and its index have to travel with the database, or
    // SQLite sees a torn state.
    for suffix in ["", "-wal", "-shm"] {
        let from = old.join(format!("walkie.db{suffix}"));
        let to = parent.join(format!("swivel.db{suffix}"));
        if from.exists() {
            let _ = std::fs::rename(&from, &to);
        }
    }

    let _ = std::fs::remove_dir(&old);
    tracing::info!("carried your identity over from the former name");
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

impl Store {
    /// Reads a setting.
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        self.conn()
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(Error::from)
    }

    /// Writes a setting. `None` removes it.
    pub fn set_setting(&self, key: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(value) => {
                self.conn().execute(
                    "INSERT INTO settings (key, value) VALUES (?1, ?2) \
                     ON CONFLICT(key) DO UPDATE SET value = ?2",
                    rusqlite::params![key, value],
                )?;
            }
            None => {
                self.conn()
                    .execute("DELETE FROM settings WHERE key = ?1", [key])?;
            }
        }
        Ok(())
    }
}

/// The setting that names the preferred input device.
pub const SETTING_INPUT_DEVICE: &str = "input_device";

/// The setting that names the preferred output device.
pub const SETTING_OUTPUT_DEVICE: &str = "output_device";

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
    fn a_setting_round_trips_and_clears() {
        let store = Store::open_memory().unwrap();
        assert_eq!(store.setting("x").unwrap(), None);

        store.set_setting("x", Some("one")).unwrap();
        assert_eq!(store.setting("x").unwrap().as_deref(), Some("one"));

        // Writing twice must update, not fail on the primary key.
        store.set_setting("x", Some("two")).unwrap();
        assert_eq!(store.setting("x").unwrap().as_deref(), Some("two"));

        store.set_setting("x", None).unwrap();
        assert_eq!(store.setting("x").unwrap(), None);
    }

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
        unsafe { std::env::set_var("SWIVEL_DB", "/tmp/swivel-test-override.db") };
        assert_eq!(
            default_path().unwrap(),
            PathBuf::from("/tmp/swivel-test-override.db")
        );
        unsafe { std::env::remove_var("SWIVEL_DB") };
    }
}
