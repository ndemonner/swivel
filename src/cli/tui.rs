//! Headless mode.
//!
//! `walkie tui` runs everything except the menu bar. It exists so two
//! instances can run on one machine and prove that connection, presence, and
//! audio work without the user interface in the way. See `LOOP.md` §5.

use std::sync::Arc;

use crate::app::App;
use crate::audio::NullSink;
use crate::error::Result;
use crate::state::UiState;

use super::fmt::{Table, box_line};

/// Runs until the user presses Ctrl-C.
pub fn run() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("walkie-net")
        .build()?;

    runtime.block_on(async move {
        let app = App::start(Arc::new(NullSink)).await?;

        println!();
        println!("{}", box_line::top("WALKIE", 68));
        println!("  name  {}", app.identity.name);
        println!("  key   {}", app.my_ticket());
        println!("{}", box_line::bottom(68));
        println!();
        println!("  Ctrl-C to stop.");
        println!();

        let printer = tokio::spawn(print_changes(app.clone()));

        tokio::signal::ctrl_c().await?;
        println!("\n  stopping");

        printer.abort();
        app.shutdown().await;
        Ok(())
    })
}

/// Prints the roster whenever it changes.
///
/// Polling at 4 Hz and comparing is simpler than a change notification, and a
/// headless debug tool does not need to be efficient.
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
    let _ = writeln!(out, "\n  endpoint {relay}");

    if state.peers.is_empty() {
        let _ = writeln!(
            out,
            "  no contacts. run `walkie add wt1…` in another shell."
        );
    } else {
        let mut table = Table::new(["SLOT", "NAME", "STATE", "RTT", "PATH"]);
        for p in &state.peers {
            table.row([
                p.slot.map(|s| s.to_string()).unwrap_or_else(|| "-".into()),
                p.name.clone(),
                if p.online { "online" } else { "offline" }.into(),
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

/// Renders a table into a string rather than to stdout.
fn render_table(table: &Table) -> String {
    let mut buf = Vec::new();
    table
        .write(&mut buf, "  ")
        .expect("writing to a Vec cannot fail");
    String::from_utf8(buf).unwrap_or_default()
}
