//! swivel — a peer-to-peer low-latency voice intercom.
//!
//! Read `LOOP.md` before you change this crate.

// The crate is under construction. Modules land before their callers do.
// T-126 removes this attribute once M7 closes.
#![allow(dead_code)]

mod app;
mod audio;
mod cli;
mod config;
mod error;
mod logging;
mod net;
mod session;
mod state;
mod store;
#[cfg(target_os = "macos")]
mod ui;

use clap::{Parser, Subcommand};

use crate::error::Result;

#[derive(Parser)]
#[command(
    name = "swivel",
    version,
    about = "Peer-to-peer push-to-talk. No accounts. No calls.",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print your shareable key.
    Key {
        /// Copy the key to the clipboard instead of printing it.
        #[arg(long)]
        copy: bool,
    },
    /// Add a contact from a `wt1…` key.
    Add {
        ticket: String,
        /// Override the name carried in the key.
        #[arg(long)]
        name: Option<String>,
    },
    /// List contacts, slots, and presence.
    List,
    /// Remove a contact by slot or by name.
    Rm { who: String },
    /// Approve a waiting contact.
    Approve {
        who: String,
        /// Override the name they claim.
        #[arg(long)]
        name: Option<String>,
    },
    /// Block an endpoint. It cannot connect or knock again.
    Block { who: String },
    /// Move a contact to a different slot.
    Slot { who: String, slot: u8 },
    /// List audio devices, or choose one.
    Devices {
        /// The input device, by number or by name.
        #[arg(long = "in")]
        input: Option<String>,
        /// The output device, by number or by name.
        #[arg(long = "out")]
        output: Option<String>,
        /// Follow the system default again.
        #[arg(long)]
        reset: bool,
    },
    /// Check audio devices, permission, and connectivity.
    Doctor {
        /// Measure real mouth-to-ear latency through the device stack.
        #[arg(long)]
        loopback: bool,
        /// Suggest settings for the measured link.
        #[arg(long)]
        tune: bool,
    },
    /// Update swivel to the latest release.
    Update {
        /// Report whether an update exists, without installing it.
        #[arg(long)]
        check: bool,
    },
    /// Create the release signing key. Maintainer only.
    #[command(hide = true)]
    ReleaseKeygen,
    /// Sign a release artifact with the release key. Maintainer only.
    #[command(hide = true)]
    ReleaseSign { file: std::path::PathBuf },
    /// Run without the menu bar. Used for two-process tests.
    Tui,
    /// Draw the panel to a PNG and exit.
    ///
    /// A terminal without the screen recording permission captures the desktop
    /// with every window missing, so this is the reliable way to look at the
    /// interface.
    #[cfg(target_os = "macos")]
    Snapshot {
        /// Where to write the image.
        #[arg(long, default_value = "swivel-panel.png")]
        out: std::path::PathBuf,
        /// Fill the roster with every state worth looking at.
        #[arg(long)]
        demo: bool,
        /// Draw the demo roster with a live session.
        #[arg(long)]
        live: bool,
        /// Print the menu bar menu as text, rather than drawing the panel.
        #[arg(long)]
        menu: bool,
        /// Fill the search field, to draw the roster that filter leaves.
        #[arg(long, default_value = "")]
        search: String,
    },
}

fn main() {
    logging::init();

    if let Err(e) = run() {
        eprintln!("swivel: {e}");
        let mut source = std::error::Error::source(&e);
        while let Some(s) = source {
            eprintln!("  caused by: {s}");
            source = std::error::Error::source(s);
        }
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => ui::app_runner::run(),
        Some(Command::Key { copy }) => cli::key(copy),
        Some(Command::Add { ticket, name }) => cli::add(&ticket, name.as_deref()),
        Some(Command::List) => cli::list(),
        Some(Command::Rm { who }) => cli::remove(&who),
        Some(Command::Slot { who, slot }) => cli::set_slot(&who, slot),
        Some(Command::Approve { who, name }) => cli::approve(&who, name.as_deref()),
        Some(Command::Block { who }) => cli::block(&who),
        Some(Command::Devices {
            input,
            output,
            reset,
        }) => cli::devices(input.as_deref(), output.as_deref(), reset),
        Some(Command::Doctor { loopback, tune }) => cli::doctor::run(loopback, tune),
        Some(Command::Update { check }) => cli::update::update(check),
        Some(Command::ReleaseKeygen) => cli::update::release_keygen(),
        Some(Command::ReleaseSign { file }) => cli::update::release_sign(&file),
        Some(Command::Tui) => cli::tui::run(),
        #[cfg(target_os = "macos")]
        Some(Command::Snapshot {
            out,
            demo,
            live,
            menu,
            search,
        }) => ui::snapshot::run(&out, demo, live, menu, &search),
    }
}
