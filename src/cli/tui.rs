//! Headless mode.
//!
//! `walkie tui` runs everything except the menu bar. Two instances on one
//! machine prove that connection, presence, sessions, and audio work without
//! the user interface in the way. See `LOOP.md` §5.
//!
//! The key bindings mirror the real ones. A digit toggles a contact in and out
//! of the session, exactly as it will from the panel.

use std::io::BufRead;
use std::sync::Arc;

use crate::app::App;
use crate::error::Result;
use crate::state::UiState;

use super::fmt::{Table, box_line};

/// Runs until the user types `q` or presses Ctrl-C.
pub fn run() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("walkie-net")
        .build()?;

    runtime.block_on(async move {
        let app = App::start().await?;

        println!();
        println!("{}", box_line::top("WALKIE", 68));
        println!("  name  {}", app.identity.name);
        println!("  key   {}", app.my_ticket());
        println!("{}", box_line::bottom(68));
        println!();
        println!("  1-9  talk to that contact, press again to drop them");
        println!("  m    mute      d  do not disturb      x  end the session");
        println!("  q    quit");
        println!();

        let printer = tokio::spawn(print_changes(app.clone()));
        let keys = read_keys(app.clone());

        tokio::select! {
            _ = keys => {}
            _ = tokio::signal::ctrl_c() => {}
        }

        println!("\n  stopping");
        printer.abort();
        app.shutdown().await;
        Ok(())
    })
}

/// Reads single-letter commands from standard input.
///
/// Standard input blocks, so it runs on a blocking thread rather than holding a
/// runtime worker.
async fn read_keys(app: Arc<App>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { return };
            if tx.send(line).is_err() {
                return;
            }
        }
    });

    while let Some(line) = rx.recv().await {
        for key in line.trim().chars() {
            match key {
                'q' => return,
                'm' => {
                    let muted = !app.muted().await;
                    app.set_muted(muted).await;
                    println!("  microphone {}", if muted { "muted" } else { "live" });
                }
                'd' => {
                    let dnd = !app.dnd().await;
                    app.set_dnd(dnd).await;
                    println!("  do not disturb {}", if dnd { "on" } else { "off" });
                }
                'x' => {
                    app.end_session().await;
                    println!("  session ended");
                }
                '1'..='9' => {
                    let slot = key as u8 - b'0';
                    match app.toggle_slot(slot).await {
                        Some(count) => println!("  session has {count} member(s)"),
                        None => println!("  slot {slot} is empty"),
                    }
                }
                _ => {}
            }
        }
    }
}

/// Prints the roster whenever it changes.
async fn print_changes(app: Arc<App>) {
    let mut last = String::new();

    loop {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;

        let state = app.state.load();
        let rendered = render(&state);
        if rendered != last {
            print!("{rendered}");
            last = rendered;
        }
    }
}

fn render(state: &UiState) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    let relay = if state.online { "online" } else { "offline" };
    let mic = match state.mic {
        crate::state::MicState::Closed => "closed",
        crate::state::MicState::Live => "LIVE",
        crate::state::MicState::Muted => "muted",
    };
    let _ = writeln!(out, "\n  endpoint {relay}   microphone {mic}");

    let a = &state.audio;
    if a.encoded > 0 || a.sent > 0 {
        let _ = writeln!(
            out,
            "  audio  encoded {}  sent {}  refused {}  |  played {}  concealed {}  late {}  overrun {}",
            a.encoded, a.sent, a.refused, a.played, a.concealed, a.late, a.overrun
        );
    }

    if state.peers.is_empty() {
        let _ = writeln!(
            out,
            "  no contacts. run `walkie add wt1…` in another shell."
        );
    } else {
        let mut table = Table::new(["", "SLOT", "NAME", "STATE", "RTT", "PATH"]);
        for p in &state.peers {
            table.row([
                if p.live { "»".into() } else { " ".into() },
                p.slot.map(|s| s.to_string()).unwrap_or_else(|| "-".into()),
                p.name.clone(),
                match (p.online, p.speaking, p.muted, p.dnd) {
                    (false, ..) => "offline".into(),
                    (true, true, ..) => "speaking".into(),
                    (true, _, true, _) => "muted".into(),
                    (true, _, _, true) => "dnd".into(),
                    _ => "online".into(),
                },
                p.rtt_ms
                    .map(|ms| format!("{ms}ms"))
                    .unwrap_or_else(|| "-".into()),
                p.path.short().to_string(),
            ]);
        }
        let _ = writeln!(out);
        out.push_str(&render_table(&table));
    }

    for knock in &state.knocks {
        let _ = writeln!(
            out,
            "  waiting: {} {}   approve with `walkie approve {}`",
            knock.claimed.as_deref().unwrap_or("(no name)"),
            knock.endpoint_id.fmt_short(),
            knock.endpoint_id.fmt_short(),
        );
    }

    out
}

fn render_table(table: &Table) -> String {
    let mut buf = Vec::new();
    table
        .write(&mut buf, "  ")
        .expect("writing to a Vec cannot fail");
    String::from_utf8(buf).unwrap_or_default()
}
