//! Logging setup.
//!
//! Rule: no real-time thread logs. A CoreAudio callback that formats a string
//! allocates, and allocation in a real-time thread is a defect. Real-time code
//! reports through atomic counters instead. See `ARCHITECTURE.md` §2.1.

use tracing_subscriber::{EnvFilter, fmt};

/// Starts logging to stderr. `SWIVEL_LOG` sets the filter, for example
/// `SWIVEL_LOG=swivel=debug,iroh=info`.
pub fn init() {
    let filter = EnvFilter::try_from_env("SWIVEL_LOG")
        .unwrap_or_else(|_| EnvFilter::new("swivel=info,warn"));

    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .without_time()
        .init();
}
