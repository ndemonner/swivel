//! Driving connections.
//!
//! One supervisor task runs per contact for the life of the process. It dials,
//! runs the connection, and reconnects with backoff. Nothing here waits for a
//! user action, because a warm connection is what makes a talk session start
//! instantly.

use std::sync::Arc;
use std::time::Instant;

use iroh::EndpointId;
use iroh::endpoint::Connection;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{debug, info, warn};

use super::control::{self, Control};
use super::peer::{self, Peer};
use crate::app::App;
use crate::config;
use crate::net::audio_wire::AudioPacket;
use crate::store::knocks::Admission;

/// Accepts inbound connections for the life of the endpoint.
pub async fn accept_loop(app: Arc<App>) {
    while let Some(incoming) = app.endpoint.accept().await {
        let app = app.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_incoming(app, incoming).await {
                debug!("an inbound connection ended: {e}");
            }
        });
    }
    info!("the endpoint stopped accepting");
}

async fn handle_incoming(
    app: Arc<App>,
    incoming: iroh::endpoint::Incoming,
) -> crate::error::Result<()> {
    let accepting = incoming
        .accept()
        .map_err(|e| crate::error::Error::net(format!("cannot accept: {e}")))?;

    let conn = accepting
        .await
        .map_err(|e| crate::error::Error::net(format!("handshake failed: {e}")))?;

    let remote = conn.remote_id();

    // Authorisation happens before anything else. An unapproved endpoint never
    // reaches the audio path.
    match app.admit(remote).await? {
        Admission::Contact => {}
        Admission::Blocked => {
            debug!(peer = %remote.fmt_short(), "refused a blocked endpoint");
            super::close_connection(&conn, "blocked");
            return Ok(());
        }
        Admission::Knock => {
            // Read the caller's name before refusing. A user cannot decide
            // whether to approve a bare hex string. This accepts one message
            // from an unapproved endpoint, which is bounded by the control
            // stream's own size limit, and then closes.
            let claimed = read_claimed_name(&conn).await;

            let known = app.knock_is_known(remote).await;
            app.record_knock(remote, claimed.as_deref()).await?;
            super::close_connection(&conn, "not a contact");

            if known {
                // The caller retries on a backoff. Do not repeat the notice.
                debug!(peer = %remote.fmt_short(), "a waiting endpoint knocked again");
            } else {
                info!(
                    peer = %remote.fmt_short(),
                    name = claimed.as_deref().unwrap_or("(no name)"),
                    "an unknown endpoint knocked"
                );
            }

            app.refresh_ui().await;
            return Ok(());
        }
    }

    run_connection(app, remote, conn, false).await;
    Ok(())
}

/// Reads the `Hello` from an endpoint we are about to refuse.
///
/// The wait is short. A caller that does not introduce itself promptly is
/// refused without a name rather than holding a task open.
async fn read_claimed_name(conn: &Connection) -> Option<String> {
    let wait = std::time::Duration::from_secs(2);

    let (_send, mut recv) = timeout(wait, conn.accept_bi()).await.ok()?.ok()?;
    let mut buf = Vec::with_capacity(128);
    let msg = timeout(wait, control::read_message(&mut recv, &mut buf))
        .await
        .ok()?
        .ok()?;

    match msg {
        Control::Hello { name, .. } => Some(control::clean_name(&name)),
        _ => None,
    }
}

/// Keeps one contact connected for the life of the process.
pub async fn supervise(app: Arc<App>, id: EndpointId) {
    let mut attempt = 0usize;

    loop {
        if app.shutdown.is_cancelled() {
            return;
        }

        let peer = app.peers.get_or_create(id).await;

        // The other side may have dialled us first. Nothing to do but wait.
        if peer.is_connected().await {
            attempt = 0;
            peer.wait_disconnected().await;
            continue;
        }

        debug!(peer = %id.fmt_short(), attempt, "dialling");

        let dial = app.endpoint.connect(id, config::ALPN);
        let result = tokio::select! {
            r = dial => r,
            _ = app.shutdown.cancelled() => return,
        };

        match result {
            Ok(conn) => {
                attempt = 0;
                let end = run_connection(app.clone(), id, conn, true).await;

                // Both sides dial, so one connection of the pair is always
                // closed. Give the replacement a moment to register before
                // deciding whether to dial again. Without this the pair churns
                // through several connections before it settles.
                tokio::select! {
                    _ = tokio::time::sleep(config::RECONNECT_SETTLE) => {}
                    _ = app.shutdown.cancelled() => return,
                }

                if end == ConnectionEnd::Duplicate {
                    debug!(peer = %id.fmt_short(), "this dial lost the tie-break");
                }
            }
            Err(e) => {
                let wait = peer::backoff_for(attempt);
                debug!(peer = %id.fmt_short(), ?wait, "dial failed: {e}");
                attempt = attempt.saturating_add(1);

                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = app.shutdown.cancelled() => return,
                }
            }
        }
    }
}

/// Why a connection stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionEnd {
    /// It ran and then it closed.
    Closed,
    /// It never ran, because the other connection of the pair won.
    Duplicate,
}

/// Runs one connection until it closes.
async fn run_connection(
    app: Arc<App>,
    id: EndpointId,
    conn: Connection,
    dialed_by_me: bool,
) -> ConnectionEnd {
    let peer = app.peers.get_or_create(id).await;
    let (control_tx, control_rx) = mpsc::unbounded_channel();

    // Two connections can exist for one pair, because both sides dial. Only one
    // survives, and both sides pick the same one.
    if !peer
        .offer(conn.clone(), dialed_by_me, app.me, control_tx)
        .await
    {
        super::close_connection(&conn, "duplicate");
        return ConnectionEnd::Duplicate;
    }

    info!(peer = %id.fmt_short(), dialed_by_me, "connected");
    peer.set_path(peer::path_of(&conn));
    app.touch_contact(id).await;
    app.refresh_ui().await;

    // The dialer opens the control stream. The acceptor takes it. Doing this in
    // one direction avoids a race for who owns the stream.
    let streams = if dialed_by_me {
        timeout(config::MAX_IDLE, conn.open_bi()).await
    } else {
        timeout(config::MAX_IDLE, conn.accept_bi()).await
    };

    let (send, recv) = match streams {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => {
            warn!(peer = %id.fmt_short(), "no control stream: {e}");
            finish(&app, &peer, &conn).await;
            return ConnectionEnd::Closed;
        }
        Err(_) => {
            warn!(peer = %id.fmt_short(), "the control stream did not open in time");
            finish(&app, &peer, &conn).await;
            return ConnectionEnd::Closed;
        }
    };

    let writer = tokio::spawn(control_writer(send, control_rx));
    let reader = tokio::spawn(control_reader(app.clone(), peer.clone(), recv));
    let datagrams = tokio::spawn(datagram_reader(app.clone(), id, conn.clone()));
    let pings = tokio::spawn(ping_loop(app.clone(), peer.clone()));
    let watcher = tokio::spawn(path_watcher(peer.clone(), conn.clone()));

    // Say who we are, then say how we are.
    peer.send_control(Control::Hello {
        name: app.identity.name.clone(),
        version: config::PROTOCOL_VERSION,
    })
    .await;
    app.send_presence_to(&peer).await;

    let reason = tokio::select! {
        e = conn.closed() => format!("{e}"),
        _ = app.shutdown.cancelled() => {
            super::close_connection(&conn, "shutting down");
            "shutdown".to_string()
        }
    };

    debug!(peer = %id.fmt_short(), "disconnected: {reason}");

    writer.abort();
    reader.abort();
    datagrams.abort();
    pings.abort();
    watcher.abort();

    finish(&app, &peer, &conn).await;
    ConnectionEnd::Closed
}

async fn finish(app: &Arc<App>, peer: &Arc<Peer>, conn: &Connection) {
    peer.clear(conn).await;
    app.on_peer_lost(peer.id).await;
    app.refresh_ui().await;
}

/// Writes queued control messages to the stream.
async fn control_writer(
    mut send: iroh::endpoint::SendStream,
    mut rx: mpsc::UnboundedReceiver<Control>,
) {
    while let Some(msg) = rx.recv().await {
        if let Err(e) = control::write_message(&mut send, &msg).await {
            debug!("the control stream stopped writing: {e}");
            return;
        }
    }
}

/// Reads control messages and applies them.
async fn control_reader(app: Arc<App>, peer: Arc<Peer>, mut recv: iroh::endpoint::RecvStream) {
    let mut buf = Vec::with_capacity(512);

    loop {
        match control::read_message(&mut recv, &mut buf).await {
            Ok(msg) => app.on_control(&peer, msg).await,
            Err(e) => {
                debug!(peer = %peer.id.fmt_short(), "the control stream stopped reading: {e}");
                return;
            }
        }
    }
}

/// Reads audio datagrams and hands them to the audio engine.
///
/// This task does no work beyond parsing and a copy. Decoding happens in the
/// output callback, which knows the exact playback time.
async fn datagram_reader(app: Arc<App>, id: EndpointId, conn: Connection) {
    loop {
        let bytes = match conn.read_datagram().await {
            Ok(b) => b,
            Err(e) => {
                debug!(peer = %id.fmt_short(), "datagrams stopped: {e}");
                return;
            }
        };

        match AudioPacket::decode(&bytes) {
            Ok(packet) => app.audio.deliver(id, &packet),
            Err(e) => {
                // A malformed datagram is not worth closing a call over. Count
                // it and move on.
                app.bad_datagrams
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                debug!(peer = %id.fmt_short(), "a datagram was refused: {e}");
            }
        }
    }
}

/// Measures the round trip time at the application level.
///
/// The QUIC estimate leaves out scheduling delay. This one includes it, and
/// scheduling delay is what a user hears.
async fn ping_loop(app: Arc<App>, peer: Arc<Peer>) {
    let mut nonce = 0u64;

    loop {
        tokio::time::sleep(config::PING_INTERVAL).await;

        nonce = nonce.wrapping_add(1);
        let sent = Instant::now();
        app.pending_pings
            .lock()
            .await
            .insert((peer.id, nonce), sent);

        if !peer.send_control(Control::Ping { nonce }).await {
            return;
        }

        // Drop a probe that never came back, so the map cannot grow.
        let stale = sent - config::PING_INTERVAL * 4;
        app.pending_pings.lock().await.retain(|_, t| *t > stale);
    }
}

/// Watches for a change between a direct path and a relay path.
async fn path_watcher(peer: Arc<Peer>, conn: Connection) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        if conn.close_reason().is_some() {
            return;
        }
        peer.set_path(peer::path_of(&conn));
    }
}
