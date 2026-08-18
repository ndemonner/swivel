//! The main thread.
//!
//! AppKit owns `main`. The tokio runtime lives on its own threads. They meet in
//! exactly two places: the interface reads an immutable snapshot, and it sends
//! commands down a channel. Neither side ever waits on the other.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSPasteboard, NSPasteboardTypeString,
};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString, NSTimer};
use tracing::{info, warn};

use crate::app::App;
use crate::config::UI_REDRAW_HZ;
use crate::error::Result;
use crate::state::StateHandle;

use super::hotkey::{Hotkeys, Shortcut};
use super::panel::Panel;
use super::roster_view::Action;
use super::statusitem::{MenuAction, StatusItem};

/// A request from the interface to the core.
///
/// The interface never blocks on the core, so every request is one-way.
#[derive(Debug, Clone)]
pub enum Command {
    ToggleSlot(u8),
    EndSession,
    ToggleMute,
    ToggleDnd,
    Arm,
    /// Close the microphone, but only if no session is using it.
    DisarmIfIdle,
    AddTicket(String),
    ApproveFirst,
    RejectFirst,
    Quit,
}

/// Everything the main thread owns.
struct Ui {
    app: Arc<App>,
    state: StateHandle,
    panel: Panel,
    status: StatusItem,
    hotkeys: Option<Hotkeys>,
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    mtm: MainThreadMarker,
    /// A development affordance. Opening the panel needs a global shortcut, and
    /// a shortcut cannot be pressed from a script, so screenshots and manual
    /// checks would otherwise be impossible.
    ///
    /// It fires on the first tick, not before `run`, because a window ordered
    /// front before the run loop starts is never composited.
    open_on_first_tick: Cell<bool>,
}

thread_local! {
    /// The interface, reachable from the timer callback.
    ///
    /// AppKit callbacks arrive with no context of ours, so the one instance is
    /// parked here. It is only ever touched on the main thread.
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - Ticker does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WalkieTicker"]
    #[ivars = ()]
    struct Ticker;

    unsafe impl NSObjectProtocol for Ticker {}

    impl Ticker {
        #[unsafe(method(tick:))]
        fn tick(&self, _sender: Option<&AnyObject>) {
            UI.with(|slot| {
                if let Some(ui) = slot.borrow().as_ref() {
                    ui.tick();
                }
            });
        }
    }
);

impl Ticker {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// Runs the menu bar application. This never returns until the user quits.
pub fn run() -> Result<()> {
    let mtm = MainThreadMarker::new().ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("AppKit needs the main thread"))
    })?;

    // The core runs on its own threads. The runtime is leaked on purpose: it
    // must outlive this function, and this function only returns when the
    // process is on its way out.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("walkie-net")
        .build()?;
    let runtime = Box::leak(Box::new(runtime));

    let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
    let app = runtime.block_on(App::start())?;
    let state = app.state.clone();

    runtime.spawn(handle_commands(app.clone(), command_rx));

    let ns_app = NSApplication::sharedApplication(mtm);
    // Accessory means a menu bar item and no Dock icon.
    ns_app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let panel_commands = commands.clone();
    let panel = Panel::new(
        mtm,
        Rc::new(move |action| {
            UI.with(|slot| {
                if let Some(ui) = slot.borrow().as_ref() {
                    ui.on_roster_action(action);
                }
            });
            let _ = &panel_commands;
        }),
    );

    let menu_commands = commands.clone();
    let status = StatusItem::new(
        mtm,
        Rc::new(move |action| {
            UI.with(|slot| {
                if let Some(ui) = slot.borrow().as_ref() {
                    ui.on_menu_action(action);
                }
            });
            let _ = &menu_commands;
        }),
    );

    let hotkeys = match Hotkeys::register() {
        Ok(h) => Some(h),
        Err(e) => {
            warn!("running without global shortcuts: {e}");
            None
        }
    };

    let ui = Rc::new(Ui {
        app,
        state,
        panel,
        status,
        hotkeys,
        commands,
        mtm,
        open_on_first_tick: Cell::new(std::env::var_os("WALKIE_PANEL_ON_START").is_some()),
    });

    UI.with(|slot| *slot.borrow_mut() = Some(ui.clone()));

    // One timer drives both the redraw and the hotkey poll. A separate thread
    // for either would have to hop back to the main thread anyway.
    let ticker = Ticker::new(mtm);
    let interval = 1.0 / UI_REDRAW_HZ as f64;
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
            interval,
            &ticker,
            sel!(tick:),
            None,
            true,
        );
    }

    ui.tick();
    info!(
        status_button = ui.status.has_button(),
        "walkie is in the menu bar"
    );

    ns_app.run();
    Ok(())
}

impl Ui {
    /// Runs at `UI_REDRAW_HZ`. It polls the shortcuts and refreshes the icon.
    fn tick(&self) {
        if self.open_on_first_tick.replace(false) {
            self.show_panel();
            info!(
                visible = self.panel.is_visible(),
                "opened the panel on start"
            );
        }

        if let Some(hotkeys) = &self.hotkeys {
            for shortcut in hotkeys.poll() {
                self.on_shortcut(shortcut);
            }
        }

        let state = self.state.load();
        self.status.set_state(&state);

        // Redrawing a hidden panel is wasted work, and at 10 Hz it would be
        // the only thing keeping the process busy while idle.
        if self.panel.is_visible() {
            self.panel.set_state((*state).clone());
        }
    }

    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    fn on_shortcut(&self, shortcut: Shortcut) {
        match shortcut {
            // Opening the panel arms the microphone. The devices start while
            // the user is still choosing a number, so the first word survives.
            Shortcut::Talk => self.show_panel(),
            Shortcut::Drop => {
                self.send(Command::EndSession);
                self.close_panel();
            }
            Shortcut::Mute => self.send(Command::ToggleMute),
        }
    }

    /// Hides the panel, and closes the microphone if it is not being used.
    ///
    /// Opening the panel arms the microphone so the first word survives the
    /// device start. Closing it without talking to anyone must undo that, or
    /// the microphone stays open for the life of the process.
    fn close_panel(&self) {
        self.panel.hide();
        self.send(Command::DisarmIfIdle);
    }

    /// Opens the panel and arms the microphone.
    ///
    /// Arming belongs here rather than in the callers. Every way of opening the
    /// panel must arm, and every way of closing it must disarm, so pairing the
    /// two in one place is what keeps the microphone from being left open.
    fn show_panel(&self) {
        self.send(Command::Arm);

        let app = NSApplication::sharedApplication(self.mtm);
        // A borderless panel in an accessory application does not get key
        // focus unless the application is activated first.
        app.activate();
        self.panel.show(self.status.anchor());
        self.panel.set_state((*self.state.load()).clone());
    }

    fn on_roster_action(&self, action: Action) {
        match action {
            Action::ToggleSlot(slot) => self.send(Command::ToggleSlot(slot)),
            Action::EndSession => {
                self.send(Command::EndSession);
                self.close_panel();
            }
            Action::ApproveFirstKnock => self.send(Command::ApproveFirst),
            Action::RejectFirstKnock => self.send(Command::RejectFirst),
            Action::FocusField => self.panel.focus_field(),
            Action::Submit => self.submit_field(),
            Action::Close => {
                // Escape hides the panel. It never ends the session, because a
                // conversation must not stop because a window closed.
                let text = self.panel.field_text();
                if text.trim().is_empty() {
                    self.close_panel();
                } else {
                    self.panel.clear_field();
                    self.panel.focus_roster();
                }
            }
        }
    }

    /// Applies whatever is in the search and add field.
    ///
    /// One field does two jobs, so what happens depends on what is in it. A
    /// `wt1` key is added as a contact. Anything else is a search, which the
    /// roster already applies as it is typed.
    fn submit_field(&self) {
        let text = self.panel.field_text();
        let trimmed = text.trim();

        if trimmed.is_empty() {
            self.panel.focus_roster();
            return;
        }

        if trimmed
            .to_lowercase()
            .starts_with(crate::config::TICKET_PREFIX)
        {
            self.send(Command::AddTicket(trimmed.to_string()));
            self.panel.clear_field();
            self.panel.focus_roster();
        }
    }

    fn on_menu_action(&self, action: MenuAction) {
        match action {
            MenuAction::TogglePanel => {
                if self.panel.is_visible() {
                    self.close_panel();
                } else {
                    self.show_panel();
                }
            }
            MenuAction::ShowMenu => self.status.show_menu(),
            MenuAction::OpenPanel => self.show_panel(),
            MenuAction::ToggleMute => self.send(Command::ToggleMute),
            MenuAction::ToggleDnd => self.send(Command::ToggleDnd),
            MenuAction::EndSession => self.send(Command::EndSession),
            MenuAction::CopyKey => self.copy_key(),
            MenuAction::Quit => {
                self.send(Command::Quit);
                let app = NSApplication::sharedApplication(self.mtm);
                app.terminate(None);
            }
        }
    }

    fn copy_key(&self) {
        let ticket = self.app.my_ticket();
        let pasteboard = NSPasteboard::generalPasteboard();
        unsafe {
            pasteboard.clearContents();
            pasteboard.setString_forType(&NSString::from_str(&ticket), NSPasteboardTypeString);
        }
        info!("your key is on the clipboard");
    }
}

/// Applies interface commands on the runtime.
async fn handle_commands(app: Arc<App>, mut rx: tokio::sync::mpsc::UnboundedReceiver<Command>) {
    while let Some(command) = rx.recv().await {
        match command {
            Command::ToggleSlot(slot) => {
                app.toggle_slot(slot).await;
            }
            Command::EndSession => app.end_session().await,
            Command::ToggleMute => {
                let muted = !app.muted().await;
                app.set_muted(muted).await;
            }
            Command::ToggleDnd => {
                let dnd = !app.dnd().await;
                app.set_dnd(dnd).await;
            }
            Command::Arm => app.arm(),
            Command::DisarmIfIdle => app.disarm_if_idle().await,
            Command::AddTicket(ticket) => {
                if let Err(e) = app.add_contact(&ticket, None).await {
                    warn!("cannot add that key: {e}");
                    app.state.update(|s| s.fault = Some(e.to_string()));
                }
            }
            Command::ApproveFirst => {
                let first = app.state.load().knocks.first().map(|k| k.endpoint_id);
                if let Some(id) = first
                    && let Err(e) = app.approve(id, None).await
                {
                    warn!("cannot approve: {e}");
                }
            }
            Command::RejectFirst => {
                let first = app.state.load().knocks.first().map(|k| k.endpoint_id);
                if let Some(id) = first
                    && let Err(e) = app.block(id).await
                {
                    warn!("cannot block: {e}");
                }
            }
            Command::Quit => {
                app.shutdown().await;
                return;
            }
        }
    }
}
