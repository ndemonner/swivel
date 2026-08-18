//! Rendering the interface to a file.
//!
//! `walkie snapshot` draws the panel into a PNG and exits. It exists for two
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
pub fn run(path: &Path, demo: bool, live: bool) -> Result<()> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("AppKit needs the main thread")))?;

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let panel = Panel::new(mtm, Rc::new(|_action| {}));

    let state = if demo {
        demo_state(live)
    } else {
        real_state()?
    };

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

    Ok(UiState {
        my_name: me.name,
        my_id: Some(my_id),
        online: false,
        peers,
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
