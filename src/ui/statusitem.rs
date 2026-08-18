//! The menu bar item.
//!
//! The whole application lives here. There is no Dock icon and no main window.
//! The icon carries the one piece of state the user must never miss: whether
//! the microphone is open. See `DESIGN.md` §6.1.

use std::cell::RefCell;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSEventMask, NSEventType, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSString};

use crate::state::{MicState, UiState};

use super::style;

/// What the menu bar asks the core to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// A left click. Show the roster, or hide it if it is already up.
    TogglePanel,
    /// A right click or a control click. Show the menu.
    ShowMenu,
    /// The menu's own "Open walkie" item.
    OpenPanel,
    ToggleMute,
    ToggleDnd,
    EndSession,
    CopyKey,
    Quit,
}

pub struct TargetIvars {
    actions: Rc<dyn Fn(MenuAction)>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - MenuTarget does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WalkieMenuTarget"]
    #[ivars = TargetIvars]
    pub struct MenuTarget;

    unsafe impl NSObjectProtocol for MenuTarget {}

    impl MenuTarget {
        /// The status item was clicked.
        ///
        /// AppKit gives one action for both buttons, so which one it was has to
        /// be read back from the current event.
        #[unsafe(method(statusClicked:))]
        fn status_clicked(&self, _sender: Option<&AnyObject>) {
            let Some(mtm) = MainThreadMarker::new() else {
                return;
            };

            let secondary = objc2_app_kit::NSApplication::sharedApplication(mtm)
                .currentEvent()
                .is_some_and(|event| {
                    let kind = event.r#type();
                    let control_held = event
                        .modifierFlags()
                        .contains(objc2_app_kit::NSEventModifierFlags::Control);

                    kind == NSEventType::RightMouseUp
                        || kind == NSEventType::RightMouseDown
                        || control_held
                });

            (self.ivars().actions)(if secondary {
                MenuAction::ShowMenu
            } else {
                MenuAction::TogglePanel
            });
        }

        #[unsafe(method(openPanel:))]
        fn open_panel(&self, _sender: Option<&AnyObject>) {
            (self.ivars().actions)(MenuAction::OpenPanel);
        }

        #[unsafe(method(toggleMute:))]
        fn toggle_mute(&self, _sender: Option<&AnyObject>) {
            (self.ivars().actions)(MenuAction::ToggleMute);
        }

        #[unsafe(method(toggleDnd:))]
        fn toggle_dnd(&self, _sender: Option<&AnyObject>) {
            (self.ivars().actions)(MenuAction::ToggleDnd);
        }

        #[unsafe(method(endSession:))]
        fn end_session(&self, _sender: Option<&AnyObject>) {
            (self.ivars().actions)(MenuAction::EndSession);
        }

        #[unsafe(method(copyKey:))]
        fn copy_key(&self, _sender: Option<&AnyObject>) {
            (self.ivars().actions)(MenuAction::CopyKey);
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            (self.ivars().actions)(MenuAction::Quit);
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker, actions: Rc<dyn Fn(MenuAction)>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TargetIvars { actions });
        unsafe { msg_send![super(this), init] }
    }
}

/// The menu bar item and its menu.
pub struct StatusItem {
    item: Retained<NSStatusItem>,
    menu: Retained<NSMenu>,
    target: Retained<MenuTarget>,
    /// The last title drawn, so a redraw at 10 Hz does not churn the menu bar.
    last_title: RefCell<String>,
    mtm: MainThreadMarker,
}

impl StatusItem {
    pub fn new(mtm: MainThreadMarker, actions: Rc<dyn Fn(MenuAction)>) -> Self {
        let bar = NSStatusBar::systemStatusBar();
        let item = bar.statusItemWithLength(NSVariableStatusItemLength);

        let target = MenuTarget::new(mtm, actions);

        if let Some(button) = item.button(mtm) {
            unsafe {
                button.setTarget(Some(&target));
                button.setAction(Some(sel!(statusClicked:)));
                // Without this the button reports only a left click, and a
                // right click would do nothing at all.
                button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);
            }
            button.setFont(Some(&style::mono(11.0)));
        }

        // The menu is built now but attached only for the instant it is shown.
        // Attaching it permanently would make a left click open the menu
        // instead of the roster.
        let menu = Self::build_menu(mtm, &target);

        StatusItem {
            item,
            menu,
            target,
            last_title: RefCell::new(String::new()),
            mtm,
        }
    }

    fn build_menu(mtm: MainThreadMarker, target: &MenuTarget) -> Retained<NSMenu> {
        let menu = NSMenu::new(mtm);

        // `None` marks a separator.
        let entries: [Option<(&str, Sel, &str)>; 8] = [
            Some(("Open walkie   ⌃⌥⌘T", sel!(openPanel:), "")),
            None,
            Some(("Mute microphone   ⌃⌥⌘M", sel!(toggleMute:), "")),
            Some(("Do not disturb", sel!(toggleDnd:), "")),
            Some(("End session   ⌃⌥⌘⎋", sel!(endSession:), "")),
            None,
            Some(("Copy my key", sel!(copyKey:), "")),
            Some(("Quit walkie", sel!(quit:), "q")),
        ];

        for entry in entries {
            match entry {
                None => menu.addItem(&NSMenuItem::separatorItem(mtm)),
                Some((title, action, key)) => {
                    let item = unsafe {
                        NSMenuItem::initWithTitle_action_keyEquivalent(
                            NSMenuItem::alloc(mtm),
                            &NSString::from_str(title),
                            Some(action),
                            &NSString::from_str(key),
                        )
                    };
                    unsafe { item.setTarget(Some(target)) };
                    menu.addItem(&item);
                }
            }
        }

        menu
    }

    /// Shows the menu under the status item.
    ///
    /// The menu is attached, clicked, and detached again. `NSStatusItem` has no
    /// other way to show a menu on demand, and leaving it attached would make
    /// every left click open the menu instead of the roster.
    pub fn show_menu(&self) {
        self.item.setMenu(Some(&self.menu));
        if let Some(button) = self.item.button(self.mtm) {
            unsafe { button.performClick(None) };
        }
        self.item.setMenu(None);
    }

    /// Updates the icon from a snapshot.
    pub fn set_state(&self, state: &UiState) {
        let title = Self::title_for(state);

        // The menu bar redraws on every change, so only set it when it moved.
        if *self.last_title.borrow() == title {
            return;
        }
        *self.last_title.borrow_mut() = title.clone();

        if let Some(button) = self.item.button(self.mtm) {
            button.setTitle(&NSString::from_str(&title));
        }
    }

    /// The icon text for a state.
    ///
    /// Text rather than an image, so the live slot numbers can be shown. A
    /// glyph cannot say "you are live to 3 and 5", and that is the single most
    /// important thing the icon has to communicate.
    fn title_for(state: &UiState) -> String {
        if !state.online {
            return "((~))".into();
        }
        if state.dnd {
            return "((x))".into();
        }

        match state.mic {
            MicState::Muted => "((/))".into(),
            MicState::Live => {
                let slots: Vec<String> = state.live_slots.iter().map(|s| s.to_string()).collect();
                if slots.is_empty() {
                    "((•))".into()
                } else {
                    format!("(({}))", slots.join(" "))
                }
            }
            // The input device is open but silent. Say so. A user is entitled
            // to know the microphone is on before they say anything.
            MicState::Armed => "((o))".into(),
            MicState::Closed => {
                if state.receiving() {
                    "((•))".into()
                } else {
                    "((·))".into()
                }
            }
        }
    }

    /// True when the menu bar gave us a button to draw into.
    pub fn has_button(&self) -> bool {
        self.item.button(self.mtm).is_some()
    }

    /// Where the panel should appear, just under the icon.
    pub fn anchor(&self) -> Option<NSPoint> {
        let button = self.item.button(self.mtm)?;
        let window = button.window()?;
        let frame = window.frame();
        Some(NSPoint::new(
            frame.origin.x + frame.size.width / 2.0,
            frame.origin.y,
        ))
    }

    /// Keeps the target alive for as long as the item exists.
    pub fn target(&self) -> &MenuTarget {
        &self.target
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::UiState;

    fn state(mic: MicState, slots: Vec<u8>, online: bool, dnd: bool) -> UiState {
        UiState {
            online,
            dnd,
            mic,
            live_slots: slots,
            ..Default::default()
        }
    }

    #[test]
    fn the_icon_shows_the_live_slots() {
        let title = StatusItem::title_for(&state(MicState::Live, vec![3, 5], true, false));
        assert_eq!(title, "((3 5))");
    }

    #[test]
    fn an_open_but_silent_microphone_is_shown() {
        // The panel arms the microphone before a contact is chosen. That state
        // must be visible, or the icon would claim the microphone is shut while
        // the device is open.
        let title = StatusItem::title_for(&state(MicState::Armed, vec![], true, false));
        assert_eq!(title, "((o))");
        assert_ne!(
            title,
            StatusItem::title_for(&state(MicState::Closed, vec![], true, false))
        );
    }

    #[test]
    fn the_icon_shows_every_other_state() {
        assert_eq!(
            StatusItem::title_for(&state(MicState::Closed, vec![], true, false)),
            "((·))"
        );
        assert_eq!(
            StatusItem::title_for(&state(MicState::Muted, vec![1], true, false)),
            "((/))"
        );
        assert_eq!(
            StatusItem::title_for(&state(MicState::Closed, vec![], true, true)),
            "((x))"
        );
        assert_eq!(
            StatusItem::title_for(&state(MicState::Closed, vec![], false, false)),
            "((~))"
        );
    }

    #[test]
    fn offline_beats_every_other_state() {
        // A user who is offline cannot be live, and showing "live" would be a
        // lie that costs trust.
        let title = StatusItem::title_for(&state(MicState::Live, vec![2], false, false));
        assert_eq!(title, "((~))");
    }
}
