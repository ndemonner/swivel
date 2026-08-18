//! walkie — a peer-to-peer low-latency voice intercom.
//!
//! Read `LOOP.md` before you change this crate.

// The crate is under construction. Modules land before their callers do.
// T-126 removes this attribute once M7 closes.
#![allow(dead_code)]

mod config;
mod error;
mod logging;

use clap::{Parser, Subcommand};

use crate::error::Result;

#[derive(Parser)]
#[command(
    name = "walkie",
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
    /// Move a contact to a different slot.
    Slot { who: String, slot: u8 },
    /// Check audio devices, permission, and connectivity.
    Doctor {
        /// Measure real mouth-to-ear latency through the device stack.
        #[arg(long)]
        loopback: bool,
        /// Suggest settings for the measured link.
        #[arg(long)]
        tune: bool,
    },
    /// Run without the menu bar. Used for two-process tests.
    Tui,
}

fn main() {
    logging::init();

    if let Err(e) = run() {
        eprintln!("walkie: {e}");
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
        None => todo!("T-070: run the menu bar application"),
        Some(Command::Key { .. }) => todo!("T-100: print the ticket"),
        Some(Command::Add { .. }) => todo!("T-101: add a contact"),
        Some(Command::List) => todo!("T-101: list contacts"),
        Some(Command::Rm { .. }) => todo!("T-101: remove a contact"),
        Some(Command::Slot { .. }) => todo!("T-101: reassign a slot"),
        Some(Command::Doctor { .. }) => todo!("T-102: check the local machine"),
        Some(Command::Tui) => todo!("T-105: headless mode"),
    }
}
