//! Logging setup.
//!
//! Rule: no real-time thread logs. A CoreAudio callback that formats a string
//! allocates, and allocation in a real-time thread is a defect. Real-time code
//! reports through atomic counters instead. See `ARCHITECTURE.md` §2.1.

use tracing_subscriber::{EnvFilter, fmt};

/// Starts logging to stderr. `WALKIE_LOG` sets the filter, for example
/// `WALKIE_LOG=walkie=debug,iroh=info`.
pub fn init() {
    let filter = EnvFilter::try_from_env("WALKIE_LOG")
        .unwrap_or_else(|_| EnvFilter::new("walkie=info,warn"));

    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .without_time()
        .init();
}
