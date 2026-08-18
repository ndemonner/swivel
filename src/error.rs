//! The error type for the whole binary.
//!
//! Library code returns `Error`. The command line layer prints it. Nothing in
//! a real-time callback ever constructs one, because that would allocate.

use std::path::PathBuf;

/// The result type used across the crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("cannot find a home directory")]
    NoHome,

    #[error("cannot create {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("input or output error: {0}")]
    Io(#[from] std::io::Error),

    #[error("bad ticket: {0}")]
    Ticket(String),

    #[error("no contact matches {0}")]
    NoSuchContact(String),

    #[error("slot {0} is not between 1 and {max}", max = crate::config::MAX_SLOT)]
    BadSlot(u8),

    #[error("audio error: {0}")]
    Audio(String),

    #[error("network error: {0}")]
    Net(String),

    #[error("update error: {0}")]
    Update(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl Error {
    /// Wraps an audio backend failure. `cpal` errors are not `'static` clean
    /// across versions, so they become strings at the boundary.
    pub fn audio(e: impl std::fmt::Display) -> Self {
        Error::Audio(e.to_string())
    }

    /// Wraps a transport failure.
    pub fn net(e: impl std::fmt::Display) -> Self {
        Error::Net(e.to_string())
    }
}
