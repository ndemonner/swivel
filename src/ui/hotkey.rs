//! Global shortcuts.
//!
//! `global-hotkey` uses Carbon `RegisterEventHotKey` on macOS. That matters for
//! two reasons: it needs no Accessibility permission, and it consumes the
//! keystroke so the shortcut does not leak into whatever application is in
//! front.
//!
//! Digits are deliberately **not** global hotkeys. When the panel opens it
//! becomes the key window and digits arrive as ordinary key events. Registering
//! ten global digit hotkeys would steal them from every other application.
//! See `ARCHITECTURE.md` §9.4.

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tracing::warn;

/// What a global shortcut means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    /// Open the panel and arm the microphone.
    Talk,
    /// End the session and close the microphone.
    Drop,
    /// Force the microphone off, without leaving the session.
    Mute,
}

/// The registered shortcuts.
///
/// Dropping this releases them.
pub struct Hotkeys {
    _manager: GlobalHotKeyManager,
    talk: u32,
    drop: u32,
    mute: u32,
}

impl Hotkeys {
    /// Registers the three shortcuts.
    ///
    /// The chord is control, option, and command together. Nothing in macOS
    /// uses it, and it cannot be pressed by accident.
    pub fn register() -> crate::error::Result<Self> {
        let manager = GlobalHotKeyManager::new()
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("cannot use hotkeys: {e}")))?;

        let chord = Modifiers::CONTROL | Modifiers::ALT | Modifiers::META;

        let talk = HotKey::new(Some(chord), Code::KeyT);
        let drop = HotKey::new(Some(chord), Code::Escape);
        let mute = HotKey::new(Some(chord), Code::KeyM);

        for (hotkey, name) in [(talk, "talk"), (drop, "drop"), (mute, "mute")] {
            if let Err(e) = manager.register(hotkey) {
                // One shortcut taken by another application must not stop the
                // rest from working.
                warn!("the {name} shortcut is already taken by something else: {e}");
            }
        }

        Ok(Hotkeys {
            _manager: manager,
            talk: talk.id,
            drop: drop.id,
            mute: mute.id,
        })
    }

    /// Reads any shortcut that fired since the last call.
    ///
    /// Only the press is reported. Acting on both press and release would fire
    /// every action twice.
    pub fn poll(&self) -> Vec<Shortcut> {
        let mut out = Vec::new();

        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }
            if let Some(shortcut) = self.classify(event.id) {
                out.push(shortcut);
            }
        }

        out
    }

    fn classify(&self, id: u32) -> Option<Shortcut> {
        match id {
            x if x == self.talk => Some(Shortcut::Talk),
            x if x == self.drop => Some(Shortcut::Drop),
            x if x == self.mute => Some(Shortcut::Mute),
            _ => None,
        }
    }
}
