//! The macOS interface.
//!
//! Every call here runs on the main thread. The tokio runtime lives on its own
//! threads, so the two never share a lock. The interface reads an immutable
//! snapshot and sends commands back through a channel. See
//! `ARCHITECTURE.md` §9.3.

#[cfg(target_os = "macos")]
pub mod app_runner;
#[cfg(target_os = "macos")]
pub mod hotkey;
#[cfg(target_os = "macos")]
pub mod panel;
#[cfg(target_os = "macos")]
pub mod roster_view;
#[cfg(target_os = "macos")]
pub mod snapshot;
#[cfg(target_os = "macos")]
pub mod statusitem;
#[cfg(target_os = "macos")]
pub mod style;
