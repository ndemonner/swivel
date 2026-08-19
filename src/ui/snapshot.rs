//! Rendering the interface to a file.
//!
//! `swivel snapshot` draws the panel into a PNG and exits. It exists for two
//! reasons.
//!
//! A terminal without the screen recording permission captures the desktop with
//! every window missing, so a screenshot is not a reliable way to check the
//! interface. Rendering from inside the process always works.
//!
//! And `--demo` fills the roster with states that are awkward to produce on
//! demand: a live session, a peer speaking, a relayed path, someone knocking.
//! Those are exactly the states most likely to be drawn wrong.

use std::path::Path;
use std::rc::Rc;

use iroh::SecretKey;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::MainThreadMarker;

use crate::error::{Error, Result};
use crate::state::{KnockView, MicState, PathKind, PeerView, UiState};

use super::panel::Panel;

/// Renders the panel and writes it to `path`.
///
/// `menu` prints the menu bar menu as text instead. See
/// `statusitem::describe_menu`.
pub fn run(path: &Path, demo: bool, live: bool, menu: bool) -> Result<()> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("AppKit needs the main thread")))?;

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let state = if demo {
        demo_state(live)
    } else {
        real_state()?
    };

    if menu {
        print!("{}", super::statusitem::describe_menu(mtm, &state));
        return Ok(());
    }

    let panel = Panel::new(mtm, Rc::new(|_action| {}));

    panel.set_state(state);
    // Showing lays out the subviews and sizes the panel to its content. A panel
    // that was never shown would render at its starting size.
    panel.show(None);
    panel.write_png(path)?;
    panel.hide();

    println!("wrote {}", path.display());
    Ok(())
}

/// Reads the real roster, without starting the network.
fn real_state() -> Result<UiState> {
    use crate::store::{Store, identity};

    let store = Store::open()?;
    let me = store.identity(&identity::default_name())?;

    let peers = store
        .contacts()?
        .into_iter()
        .map(|c| PeerView {
            endpoint_id: c.endpoint_id,
            slot: c.slot,
            name: c.name,
            online: false,
            rtt_ms: None,
            path: PathKind::Unknown,
            dnd: false,
            muted: false,
            live: false,
            speaking: false,
        })
        .collect();

    let my_id = me.endpoint_id();

    // The settings the menu ticks are read from. Without them `--menu` would
    // report the defaults on every machine and prove nothing.
    let echo_cancelling = store
        .setting(crate::store::SETTING_ECHO_CANCEL)?
        .map(|v| v != "off")
        .unwrap_or(true);

    Ok(UiState {
        my_name: me.name.clone(),
        my_id: Some(my_id),
        my_key: crate::store::ticket::Ticket::new(my_id, &me.name).encode(),
        key_copied: false,
        online: false,
        peers,
        echo_cancelling,
        input_device: store.setting(crate::store::SETTING_INPUT_DEVICE)?,
        output_device: store.setting(crate::store::SETTING_OUTPUT_DEVICE)?,
        ..Default::default()
    })
}

/// A roster with every state worth looking at.
fn demo_state(live: bool) -> UiState {
    fn id(n: u8) -> iroh::EndpointId {
        SecretKey::from_bytes(&[n; 32]).public()
    }

    let peer = |n: u8, slot: u8, name: &str| PeerView {
        endpoint_id: id(n),
        slot: Some(slot),
        name: name.to_string(),
        online: false,
        rtt_ms: None,
        path: PathKind::Unknown,
        dnd: false,
        muted: false,
        live: false,
        speaking: false,
    };

    let peers = vec![
        PeerView {
            online: true,
            rtt_ms: Some(12),
            path: PathKind::Direct,
            live,
            speaking: live,
            ..peer(1, 1, "Maggie Henry")
        },
        PeerView {
            online: true,
            rtt_ms: Some(44),
            path: PathKind::Relay,
            live,
            ..peer(2, 3, "Will Tachau")
        },
        PeerView {
            online: true,
            rtt_ms: Some(8),
            path: PathKind::Direct,
            muted: true,
            ..peer(3, 4, "Ada Bell")
        },
        PeerView {
            online: true,
            rtt_ms: Some(120),
            path: PathKind::Relay,
            dnd: true,
            ..peer(4, 6, "Tomas Vidal")
        },
        peer(5, 5, "David Marcin"),
        peer(6, 7, "Priya Raman"),
    ];

    UiState {
        my_name: "nick".into(),
        my_id: Some(id(9)),
        my_key: crate::store::ticket::Ticket::new(id(9), "nick").encode(),
        key_copied: false,
        echo_cancelling: true,
        input_device: None,
        output_device: None,
        online: true,
        peers,
        knocks: vec![KnockView {
            endpoint_id: id(7),
            claimed: Some("Jun Park".into()),
        }],
        mic: if live {
            MicState::Live
        } else {
            MicState::Closed
        },
        dnd: false,
        live_slots: if live { vec![1, 3] } else { Vec::new() },
        fault: None,
        audio: Default::default(),
    }
}
