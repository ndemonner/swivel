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
    NSControlStateValue, NSControlStateValueOff, NSControlStateValueOn, NSEventMask, NSEventType,
    NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
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
    /// The menu's own "Open swivel" item.
    OpenPanel,
    ToggleMute,
    ToggleDnd,
    EndSession,
    CopyKey,
    Quit,
    /// Choose the input device at this position in `device::names(Input)`.
    ChooseInput(usize),
    /// Choose the output device at this position in `device::names(Output)`.
    ChooseOutput(usize),
    /// Follow the system defaults again.
    ResetDevices,
    /// Turn echo cancellation on or off.
    ToggleEchoCancellation,
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
    #[name = "SwivelMenuTarget"]
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

        /// The tag carries the device's position in the list the menu was
        /// built from.
        #[unsafe(method(chooseInput:))]
        fn choose_input(&self, sender: Option<&AnyObject>) {
            if let Some(index) = tag_of(sender) {
                (self.ivars().actions)(MenuAction::ChooseInput(index));
            }
        }

        #[unsafe(method(chooseOutput:))]
        fn choose_output(&self, sender: Option<&AnyObject>) {
            if let Some(index) = tag_of(sender) {
                (self.ivars().actions)(MenuAction::ChooseOutput(index));
            }
        }

        #[unsafe(method(resetDevices:))]
        fn reset_devices(&self, _sender: Option<&AnyObject>) {
            (self.ivars().actions)(MenuAction::ResetDevices);
        }

        #[unsafe(method(toggleEcho:))]
        fn toggle_echo(&self, _sender: Option<&AnyObject>) {
            (self.ivars().actions)(MenuAction::ToggleEchoCancellation);
        }
    }
);

/// Renders the whole menu as text, ticks included.
///
/// `swivel snapshot --menu` prints this. A menu is an `NSMenu`, not a view, so
/// the panel renderer cannot draw it and a screenshot needs a permission the
/// terminal usually does not have. Reading the real `NSMenuItem` states is the
/// way to check that exactly one microphone and one speaker carry a tick.
pub fn describe_menu(mtm: MainThreadMarker, state: &UiState) -> String {
    let target = MenuTarget::new(mtm, Rc::new(|_action| {}));
    let menu = StatusItem::build_menu(mtm, &target, state);
    let mut out = String::new();
    describe_into(&menu, 0, &mut out);
    out
}

fn describe_into(menu: &NSMenu, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);

    for index in 0..menu.numberOfItems() {
        let Some(item) = menu.itemAtIndex(index) else {
            continue;
        };

        if item.isSeparatorItem() {
            out.push_str(&format!("{indent}--\n"));
            continue;
        }

        let mark = if item.state() == NSControlStateValueOn {
            "[x]"
        } else {
            "[ ]"
        };
        out.push_str(&format!("{indent}{mark} {}\n", item.title()));

        if let Some(submenu) = item.submenu() {
            describe_into(&submenu, depth + 1, out);
        }
    }
}

/// The tick state for a menu item.
fn tick(on: bool) -> NSControlStateValue {
    if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    }
}

/// Reads the tag from a menu item that sent an action.
fn tag_of(sender: Option<&AnyObject>) -> Option<usize> {
    let item: &NSMenuItem = unsafe { std::mem::transmute(sender?) };
    usize::try_from(item.tag()).ok()
}

impl MenuTarget {
    fn new(mtm: MainThreadMarker, actions: Rc<dyn Fn(MenuAction)>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TargetIvars { actions });
        unsafe { msg_send![super(this), init] }
    }
}

/// The menu bar item and its menu.
pub struct StatusItem {
    item: Retained<NSStatusItem>,
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

        // The menu is built each time it is shown, because the device list
        // changes when headphones are plugged in, and because the ticks come
        // from the snapshot of the moment. It is attached only for the instant
        // it is shown: attaching it permanently would make a left click open
        // the menu instead of the roster.
        StatusItem {
            item,
            target,
            last_title: RefCell::new(String::new()),
            mtm,
        }
    }

    fn build_menu(mtm: MainThreadMarker, target: &MenuTarget, state: &UiState) -> Retained<NSMenu> {
        let menu = NSMenu::new(mtm);

        // `None` marks a separator.
        let entries: [Option<(&str, Sel, &str)>; 8] = [
            Some(("Open swivel   ⌃⌥⌘T", sel!(openPanel:), "")),
            None,
            Some(("Mute microphone   ⌃⌥⌘M", sel!(toggleMute:), "")),
            Some(("Do not disturb", sel!(toggleDnd:), "")),
            Some(("End session   ⌃⌥⌘⎋", sel!(endSession:), "")),
            None,
            Some(("Copy my key", sel!(copyKey:), "")),
            Some(("Quit swivel", sel!(quit:), "q")),
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

        // The device submenu goes above "Copy my key".
        let devices = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str("Audio devices"),
                None,
                &NSString::from_str(""),
            )
        };
        menu.setSubmenu_forItem(
            Some(&Self::build_devices_menu(mtm, target, state)),
            &devices,
        );
        menu.insertItem_atIndex(&devices, (menu.numberOfItems() - 2).max(0));

        menu
    }

    /// Shows the menu under the status item.
    ///
    /// The menu is attached, clicked, and detached again. `NSStatusItem` has no
    /// other way to show a menu on demand, and leaving it attached would make
    /// every left click open the menu instead of the roster.
    pub fn show_menu(&self, state: &UiState) {
        // Rebuild first. A device list from a minute ago is often wrong.
        let fresh = Self::build_menu(self.mtm, &self.target, state);
        self.item.setMenu(Some(&fresh));
        if let Some(button) = self.item.button(self.mtm) {
            unsafe { button.performClick(None) };
        }
        self.item.setMenu(None);
    }

    /// Builds the audio device submenu.
    ///
    /// The system default is offered first, so a user who changed a device by
    /// accident has an obvious way back.
    ///
    /// Exactly one microphone and one speaker carry a tick. The tick follows
    /// the device the audio path really opens, so a stored device that has
    /// gone away ticks the system default instead of nothing.
    fn build_devices_menu(
        mtm: MainThreadMarker,
        target: &MenuTarget,
        state: &UiState,
    ) -> Retained<NSMenu> {
        use crate::audio::device::{self, Direction};

        let menu = NSMenu::new(mtm);

        let echo = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str("Cancel echo"),
                Some(sel!(toggleEcho:)),
                &NSString::from_str(""),
            )
        };
        unsafe { echo.setTarget(Some(target)) };
        // A tick means it is on. Without cancellation, using a loudspeaker
        // makes the far end hear themselves.
        echo.setState(tick(state.echo_cancelling));
        menu.addItem(&echo);
        menu.addItem(&NSMenuItem::separatorItem(mtm));

        let reset = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str("Use the system defaults"),
                Some(sel!(resetDevices:)),
                &NSString::from_str(""),
            )
        };
        unsafe { reset.setTarget(Some(target)) };
        // The item returns both directions to the system, so one stored device
        // is enough to make the tick untrue.
        reset.setState(tick(
            state.input_device.is_none() && state.output_device.is_none(),
        ));
        menu.addItem(&reset);

        for (direction, action, label, preferred) in [
            (
                Direction::Input,
                sel!(chooseInput:),
                "Microphone",
                state.input_device.as_deref(),
            ),
            (
                Direction::Output,
                sel!(chooseOutput:),
                "Speaker",
                state.output_device.as_deref(),
            ),
        ] {
            menu.addItem(&NSMenuItem::separatorItem(mtm));

            let header = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(label),
                    None,
                    &NSString::from_str(""),
                )
            };
            header.setEnabled(false);
            menu.addItem(&header);

            let names = device::names(direction);
            let default = device::default_name(direction);
            let in_use = device::in_use(preferred, &names, default.as_deref());

            for (index, name) in names.iter().enumerate() {
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(mtm),
                        &NSString::from_str(&format!("   {name}")),
                        Some(action),
                        &NSString::from_str(""),
                    )
                };
                unsafe {
                    item.setTarget(Some(target));
                    item.setTag(index as isize);
                }
                item.setState(tick(in_use.as_deref() == Some(name.as_str())));
                menu.addItem(&item);
            }
        }

        menu
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
