//! The core. It owns the state and wires the layers together.
//!
//! The user interface never talks to the network or the audio engine directly.
//! It reads a snapshot and it sends commands. See `ARCHITECTURE.md` §9.3.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use iroh::{Endpoint, EndpointId, Watcher};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::audio::{AudioSink, Engine};
use crate::config;
use crate::error::Result;
use crate::net::control::{self, Control};
use crate::net::peer::Peer;
use crate::net::{PeerMap, driver};
use crate::session::{Session, SessionTx};
use crate::state::{AudioCounters, KnockView, MicState, PeerView, StateHandle, UiState};
use crate::store::identity::Identity;
use crate::store::knocks::Admission;
use crate::store::{Store, ticket::Ticket};

/// The running application.
pub struct App {
    /// The local keypair and name.
    pub identity: Identity,
    /// The local endpoint id. Cached because it is read on every connection.
    pub me: EndpointId,
    pub endpoint: Endpoint,
    pub peers: PeerMap,
    pub audio: Arc<dyn AudioSink>,
    /// The audio engine. `None` when the machine has no usable devices, which
    /// must not stop presence and the roster from working.
    pub engine: Option<Arc<Engine>>,
    /// Publishes the member connections to the audio sender thread.
    pub tx: Arc<SessionTx>,
    pub state: StateHandle,
    pub shutdown: CancellationToken,

    /// `rusqlite::Connection` is not `Sync`, so the store lives behind a lock.
    /// Every write is a small local statement, so holding it briefly is fine.
    store: Mutex<Store>,

    /// Round trip probes still in flight, keyed by peer and nonce.
    pub pending_pings: Mutex<HashMap<(EndpointId, u64), Instant>>,

    /// Datagrams refused by the parser. Exposed by `walkie doctor`.
    pub bad_datagrams: AtomicU64,

    /// The user refuses incoming sessions.
    dnd: Mutex<bool>,

    /// Contacts that already have a supervisor task.
    supervised: Mutex<std::collections::HashSet<EndpointId>>,

    /// The live conversation, if there is one.
    session: Mutex<Option<Session>>,

    /// The user forced the microphone off.
    muted: Mutex<bool>,

    /// The speaking state last sent to the session, so a change is sent once
    /// rather than every second.
    last_reported_voice: std::sync::atomic::AtomicBool,

    /// Requests for a new peer supervisor. See `supervise_one`.
    supervise_tx: tokio::sync::mpsc::UnboundedSender<EndpointId>,
}

impl App {
    /// Opens the store, binds the endpoint, and starts every background task.
    ///
    /// The audio engine is started too. A machine with no usable audio still
    /// gets a working roster, because seeing who is online is useful even when
    /// you cannot talk.
    pub async fn start() -> Result<Arc<Self>> {
        let store = Store::open()?;
        let identity = store.identity(&crate::store::identity::default_name())?;
        let me = identity.endpoint_id();

        info!(id = %me, name = %identity.name, "starting");

        let endpoint = crate::net::bind(identity.secret_key.clone()).await?;

        let tx = Arc::new(SessionTx::new());
        let engine = match Engine::start(tx.clone()) {
            Ok(engine) => Some(engine),
            Err(e) => {
                warn!("running without audio: {e}");
                None
            }
        };

        let audio: Arc<dyn AudioSink> = match &engine {
            Some(engine) => engine.clone(),
            None => Arc::new(crate::audio::NullSink),
        };

        let (supervise_tx, supervise_rx) = tokio::sync::mpsc::unbounded_channel();

        let app = Arc::new(App {
            identity,
            me,
            endpoint,
            peers: PeerMap::new(),
            audio,
            engine,
            tx,
            state: StateHandle::new(),
            shutdown: CancellationToken::new(),
            store: Mutex::new(store),
            pending_pings: Mutex::new(HashMap::new()),
            bad_datagrams: AtomicU64::new(0),
            dnd: Mutex::new(false),
            supervised: Mutex::new(std::collections::HashSet::new()),
            session: Mutex::new(None),
            muted: Mutex::new(false),
            last_reported_voice: std::sync::atomic::AtomicBool::new(false),
            supervise_tx,
        });

        app.refresh_ui().await;

        tokio::spawn(run_supervisors(app.clone(), supervise_rx));
        tokio::spawn(driver::accept_loop(app.clone()));
        tokio::spawn(watch_online(app.clone()));
        tokio::spawn(watch_session(app.clone()));

        // One supervisor per contact. They dial immediately and stay warm.
        app.supervise_contacts().await?;

        Ok(app)
    }

    /// Starts a supervisor for every contact that does not have one.
    ///
    /// This also runs on a timer, so a contact added by a second process, such
    /// as `walkie add` in another terminal, is picked up without a restart.
    pub async fn supervise_contacts(self: &Arc<Self>) -> Result<()> {
        let contacts = self.store.lock().await.contacts()?;
        for contact in contacts {
            self.supervise_one(contact.endpoint_id).await;
        }
        Ok(())
    }

    /// Asks for a supervisor, rather than starting one here.
    ///
    /// The request goes through a channel on purpose. A supervisor runs a
    /// connection, a connection can carry a `SessionOpen`, and that can bring a
    /// new member into the mesh who needs a supervisor. Calling directly makes
    /// the future's type refer to itself, which the compiler cannot resolve.
    /// A channel breaks the loop and keeps the layering honest: the control
    /// path asks, and one task decides.
    async fn supervise_one(self: &Arc<Self>, id: EndpointId) {
        if !self.supervised.lock().await.insert(id) {
            return;
        }
        let _ = self.supervise_tx.send(id);
    }

    /// Your shareable key.
    pub fn my_ticket(&self) -> String {
        Ticket::new(self.me, &self.identity.name).encode()
    }

    /// Stops every background task and closes the endpoint.
    pub async fn shutdown(&self) {
        self.end_session().await;
        self.shutdown.cancel();
        if let Some(engine) = &self.engine {
            engine.shutdown();
        }
        self.endpoint.close().await;
    }

    // -----------------------------------------------------------------------
    // Sessions
    // -----------------------------------------------------------------------

    /// Opens the audio devices without transmitting.
    ///
    /// Call this when the panel opens, before a contact is chosen. A CoreAudio
    /// device takes tens of milliseconds to start, and paying that while the
    /// user is still deciding is what makes the first word survive.
    pub fn arm(&self) {
        if let Some(engine) = &self.engine {
            engine.arm();
        }
    }

    /// Closes the microphone when no session is live.
    pub async fn disarm_if_idle(&self) {
        if self.session.lock().await.is_some() {
            return;
        }
        if let Some(engine) = &self.engine {
            engine.disarm();
        }
    }

    /// Adds or removes a contact by slot number. This is the digit press.
    ///
    /// Returns the resulting member count, or `None` when the slot is empty.
    pub async fn toggle_slot(self: &Arc<Self>, slot: u8) -> Option<usize> {
        let contact = self
            .store
            .lock()
            .await
            .contact_by_slot(slot)
            .ok()
            .flatten()?;
        Some(self.toggle_member(contact.endpoint_id).await)
    }

    /// Adds or removes one peer from the session.
    pub async fn toggle_member(self: &Arc<Self>, peer: EndpointId) -> usize {
        let live = {
            let session = self.session.lock().await;
            session.as_ref().is_some_and(|s| s.contains(peer))
        };

        if live {
            self.remove_member(peer).await
        } else {
            self.add_member(peer).await
        }
    }

    /// Puts a peer in the session and opens the microphone to them.
    pub async fn add_member(self: &Arc<Self>, peer: EndpointId) -> usize {
        // Open the devices first. Everything after this is instant.
        self.arm();

        let members = {
            let mut guard = self.session.lock().await;
            let session = guard.get_or_insert_with(|| Session::new(crate::session::new_id()));

            if !session.add(peer) {
                warn!("the session is full, so {} was not added", peer.fmt_short());
                return session.members.len();
            }
            session.members.len()
        };

        if let Some(engine) = &self.engine
            && engine.slots().activate(peer).is_none()
        {
            warn!("no audio slot was free for {}", peer.fmt_short());
        }

        self.publish_session().await;
        self.announce_session().await;
        self.refresh_ui().await;
        members
    }

    /// Takes a peer out of the session.
    pub async fn remove_member(self: &Arc<Self>, peer: EndpointId) -> usize {
        let (remaining, session_id) = {
            let mut guard = self.session.lock().await;
            let Some(session) = guard.as_mut() else {
                return 0;
            };
            session.remove(peer);
            (session.members.len(), session.id)
        };

        if let Some(engine) = &self.engine {
            engine.slots().deactivate(peer);
        }

        // Tell the peer it is out, so its own microphone closes.
        if let Some(handle) = self.peers.get(peer).await {
            handle
                .send_control(Control::SessionClose {
                    session: session_id,
                })
                .await;
        }

        if remaining == 0 {
            self.end_session().await;
            return 0;
        }

        self.publish_session().await;
        self.announce_session().await;
        self.refresh_ui().await;
        remaining
    }

    /// Ends the session and closes the microphone.
    pub async fn end_session(&self) {
        let closed = {
            let mut guard = self.session.lock().await;
            guard.take()
        };

        let Some(session) = closed else {
            return;
        };

        for peer in &session.members {
            if let Some(handle) = self.peers.get(*peer).await {
                handle
                    .send_control(Control::SessionClose {
                        session: session.id,
                    })
                    .await;
            }
        }

        if let Some(engine) = &self.engine {
            engine.set_transmitting(false);
            engine.slots().clear();
            engine.disarm();
        }
        self.tx.clear();

        info!(members = session.members.len(), "session ended");
        self.refresh_ui().await;
    }

    /// Rebuilds the connection list the audio sender thread uses, and decides
    /// whether the microphone should be open.
    async fn publish_session(&self) {
        let members = {
            let guard = self.session.lock().await;
            match guard.as_ref() {
                Some(session) => session.members.iter().copied().collect::<Vec<_>>(),
                None => Vec::new(),
            }
        };

        let mut connections = Vec::with_capacity(members.len());
        for peer in &members {
            if let Some(handle) = self.peers.get(*peer).await
                && let Some(conn) = handle.connection().await
            {
                connections.push(conn);
            }
        }

        let reachable = connections.len();
        self.tx.set_targets(connections);

        let muted = *self.muted.lock().await;
        let transmitting = reachable > 0 && !muted;

        if let Some(engine) = &self.engine {
            engine.set_transmitting(transmitting);
        }
    }

    /// Tells every member who else is in the session.
    ///
    /// This is what turns two links into one conversation. Every member gets
    /// the full list and opens its own microphone to the others.
    async fn announce_session(&self) {
        let (id, members) = {
            let guard = self.session.lock().await;
            match guard.as_ref() {
                Some(session) => (
                    session.id,
                    session.members.iter().copied().collect::<Vec<_>>(),
                ),
                None => return,
            }
        };

        // The list each member receives includes us, so they know who to open
        // audio to, and excludes themselves.
        for peer in &members {
            let others: Vec<EndpointId> = members
                .iter()
                .copied()
                .filter(|m| m != peer)
                .chain(std::iter::once(self.me))
                .collect();

            if let Some(handle) = self.peers.get(*peer).await {
                handle
                    .send_control(Control::SessionOpen {
                        session: id,
                        members: Control::ids_as_members(others),
                    })
                    .await;
            }
        }
    }

    /// Applies a `SessionOpen` from a peer.
    async fn on_session_open(
        self: &Arc<Self>,
        from: EndpointId,
        id: u64,
        members: Vec<EndpointId>,
    ) {
        if *self.dnd.lock().await {
            if let Some(handle) = self.peers.get(from).await {
                handle
                    .send_control(Control::SessionClose { session: id })
                    .await;
            }
            return;
        }

        // Only an approved contact may open your microphone, and only one you
        // have not set to knock.
        let contact = self.store.lock().await.contact(from).ok().flatten();
        let Some(contact) = contact else {
            debug!(peer = %from.fmt_short(), "ignoring a session from a stranger");
            return;
        };
        if !contact.auto_open {
            info!(peer = %contact.name, "wants to talk, and is set to knock");
            self.state.update(|s| {
                s.fault = Some(format!("{} wants to talk", contact.name));
            });
            return;
        }

        self.arm();

        {
            let mut guard = self.session.lock().await;
            let session = guard.get_or_insert_with(|| Session::new(id));

            // Everyone the sender named, plus the sender.
            for peer in members.iter().copied().chain(std::iter::once(from)) {
                if peer == self.me {
                    continue;
                }
                if !session.add(peer) {
                    warn!("the session is full, so {} was not added", peer.fmt_short());
                }
            }
        }

        // Dial anyone in the mesh we do not already hold a connection to.
        let members = {
            let guard = self.session.lock().await;
            guard
                .as_ref()
                .map(|s| s.members.clone())
                .unwrap_or_default()
        };

        for peer in &members {
            if let Some(engine) = &self.engine {
                engine.slots().activate(*peer);
            }
            self.supervise_one(*peer).await;
        }

        self.publish_session().await;
        self.refresh_ui().await;
        info!(members = members.len(), "session open");
    }

    /// Applies a `SessionClose` from a peer.
    async fn on_session_close(self: &Arc<Self>, from: EndpointId) {
        let remaining = {
            let mut guard = self.session.lock().await;
            let Some(session) = guard.as_mut() else {
                return;
            };
            session.remove(from);
            session.members.len()
        };

        if let Some(engine) = &self.engine {
            engine.slots().deactivate(from);
        }

        if remaining == 0 {
            self.end_session().await;
            return;
        }

        self.publish_session().await;
        self.refresh_ui().await;
    }

    /// Forces the microphone off without leaving the session.
    pub async fn set_muted(&self, muted: bool) {
        *self.muted.lock().await = muted;
        self.publish_session().await;
        self.broadcast_presence().await;
        self.refresh_ui().await;
    }

    pub async fn muted(&self) -> bool {
        *self.muted.lock().await
    }

    /// The slots in the live session, in slot order.
    pub async fn live_slots(&self) -> Vec<u8> {
        let members = {
            let guard = self.session.lock().await;
            match guard.as_ref() {
                Some(s) => s.members.clone(),
                None => return Vec::new(),
            }
        };

        let store = self.store.lock().await;
        let mut slots: Vec<u8> = members
            .iter()
            .filter_map(|id| store.contact(*id).ok().flatten().and_then(|c| c.slot))
            .collect();
        slots.sort_unstable();
        slots
    }

    /// Tells the session whether the local user is speaking.
    ///
    /// This drives the roster indicator and keeps the idle timer honest. It is
    /// a report, not a gate: audio is transmitted whether or not this is true.
    pub async fn report_local_voice(&self) {
        let Some(engine) = &self.engine else {
            return;
        };
        if !engine.transmitting() {
            return;
        }

        let speaking = engine.speaking();

        let (id, members) = {
            let mut guard = self.session.lock().await;
            let Some(session) = guard.as_mut() else {
                return;
            };
            if speaking {
                session.last_voice = std::time::Instant::now();
            }
            (
                session.id,
                session.members.iter().copied().collect::<Vec<_>>(),
            )
        };

        if speaking == self.last_reported_voice.swap(speaking, Ordering::Relaxed) {
            return;
        }

        for peer in members {
            if let Some(handle) = self.peers.get(peer).await {
                handle
                    .send_control(Control::TalkState {
                        session: id,
                        speaking,
                    })
                    .await;
            }
        }
    }

    /// Gathers the audio counters for the interface and for `doctor`.
    pub fn audio_counters(&self) -> AudioCounters {
        let stats = self.engine.as_ref().map(|e| e.stats()).unwrap_or_default();

        AudioCounters {
            sent: self.tx.sent.load(Ordering::Relaxed),
            refused: self.tx.refused.load(Ordering::Relaxed),
            encoded: stats.encoded,
            played: stats.played,
            concealed: stats.concealed,
            late: stats.late,
            overrun: stats.overrun,
        }
    }

    /// True when the given peer is in the live session.
    pub async fn is_live(&self, peer: EndpointId) -> bool {
        let guard = self.session.lock().await;
        guard.as_ref().is_some_and(|s| s.contains(peer))
    }

    // -----------------------------------------------------------------------
    // Store access
    // -----------------------------------------------------------------------

    pub async fn admit(&self, id: EndpointId) -> Result<Admission> {
        self.store.lock().await.admit(id)
    }

    pub async fn record_knock(&self, id: EndpointId, claimed: Option<&str>) -> Result<()> {
        self.store.lock().await.record_knock(id, claimed)
    }

    /// True when this endpoint has knocked before.
    ///
    /// A refused caller retries on a backoff, so the notice must be printed
    /// once, not on every attempt.
    pub async fn knock_is_known(&self, id: EndpointId) -> bool {
        self.store.lock().await.knock(id).ok().flatten().is_some()
    }

    pub async fn touch_contact(&self, id: EndpointId) {
        if let Err(e) = self.store.lock().await.touch_contact(id) {
            warn!("cannot record that a contact was seen: {e}");
        }
    }

    /// Adds a contact and starts its supervisor at once.
    pub async fn add_contact(self: &Arc<Self>, ticket: &str, name: Option<&str>) -> Result<()> {
        let ticket = Ticket::decode(ticket)?;
        if ticket.endpoint_id == self.me {
            return Err(crate::error::Error::Ticket("that is your own key".into()));
        }

        let name = name.unwrap_or(&ticket.name).to_string();
        self.store
            .lock()
            .await
            .add_contact(ticket.endpoint_id, &name)?;

        self.supervise_one(ticket.endpoint_id).await;
        self.refresh_ui().await;
        Ok(())
    }

    /// Approves a waiting endpoint and starts its supervisor.
    pub async fn approve(self: &Arc<Self>, id: EndpointId, name: Option<&str>) -> Result<()> {
        self.store.lock().await.approve_knock(id, name)?;
        self.supervise_one(id).await;
        self.refresh_ui().await;
        Ok(())
    }

    /// Blocks an endpoint and drops any connection to it.
    pub async fn block(&self, id: EndpointId) -> Result<()> {
        {
            let store = self.store.lock().await;
            store.remove_contact(id)?;
            store.block(id)?;
        }
        if let Some(peer) = self.peers.remove(id).await
            && let Some(conn) = peer.connection().await
        {
            crate::net::close_connection(&conn, "blocked");
        }
        self.refresh_ui().await;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Control messages
    // -----------------------------------------------------------------------

    /// Applies one control message from a peer.
    pub async fn on_control(self: &Arc<Self>, peer: &Arc<Peer>, msg: Control) {
        match msg {
            Control::Hello { name, version } => {
                if version != config::PROTOCOL_VERSION {
                    debug!(
                        peer = %peer.id.fmt_short(),
                        "the peer speaks protocol version {version}, and this build speaks {}",
                        config::PROTOCOL_VERSION
                    );
                }
                peer.set_claimed_name(control::clean_name(&name)).await;
                self.refresh_ui().await;
            }

            Control::Presence {
                available: _,
                dnd,
                muted,
            } => {
                peer.peer_dnd.store(dnd, Ordering::Relaxed);
                peer.peer_muted.store(muted, Ordering::Relaxed);
                self.refresh_ui().await;
            }

            Control::TalkState { speaking, .. } => {
                peer.speaking.store(speaking, Ordering::Relaxed);
                if speaking && let Some(session) = self.session.lock().await.as_mut() {
                    session.last_voice = std::time::Instant::now();
                }
                self.refresh_ui().await;
            }

            Control::Ping { nonce } => {
                peer.send_control(Control::Pong { nonce }).await;
            }

            Control::Pong { nonce } => {
                let sent = self.pending_pings.lock().await.remove(&(peer.id, nonce));
                if let Some(sent) = sent {
                    peer.set_rtt(sent.elapsed());
                    self.refresh_ui().await;
                }
            }

            Control::SessionOpen { session, members } => {
                self.on_session_open(peer.id, session, Control::members_as_ids(&members))
                    .await;
            }

            Control::SessionClose { .. } => {
                self.on_session_close(peer.id).await;
            }
        }
    }

    /// Tells one peer how we are.
    pub async fn send_presence_to(&self, peer: &Arc<Peer>) {
        let dnd = *self.dnd.lock().await;
        let muted = *self.muted.lock().await;
        peer.send_control(Control::Presence {
            available: true,
            dnd,
            muted,
        })
        .await;
    }

    /// Tells every connected peer how we are.
    pub async fn broadcast_presence(&self) {
        for peer in self.peers.all().await {
            self.send_presence_to(&peer).await;
        }
    }

    /// Sets do-not-disturb and tells everyone.
    pub async fn set_dnd(&self, on: bool) {
        *self.dnd.lock().await = on;
        self.broadcast_presence().await;
        self.refresh_ui().await;
    }

    pub async fn dnd(&self) -> bool {
        *self.dnd.lock().await
    }

    /// Cleans up after a peer goes offline.
    pub async fn on_peer_lost(&self, id: EndpointId) {
        self.pending_pings.lock().await.retain(|(p, _), _| *p != id);
    }

    // -----------------------------------------------------------------------
    // Interface state
    // -----------------------------------------------------------------------

    /// Rebuilds the snapshot the user interface reads.
    ///
    /// This runs on every state change. It reads the whole roster, which is at
    /// most a few dozen rows, so the cost does not matter.
    pub async fn refresh_ui(&self) {
        let (contacts, knocks) = {
            let store = self.store.lock().await;
            (
                store.contacts().unwrap_or_default(),
                store.pending_knocks().unwrap_or_default(),
            )
        };

        let live_members = {
            let guard = self.session.lock().await;
            guard
                .as_ref()
                .map(|s| s.members.clone())
                .unwrap_or_default()
        };

        let mut peers = Vec::with_capacity(contacts.len());
        for contact in contacts {
            let live = self.peers.get(contact.endpoint_id).await;

            let (online, rtt_ms, path, dnd, muted, speaking) = match &live {
                Some(p) => (
                    p.is_connected().await,
                    p.rtt(),
                    p.path(),
                    p.peer_dnd.load(Ordering::Relaxed),
                    p.peer_muted.load(Ordering::Relaxed),
                    p.speaking.load(Ordering::Relaxed),
                ),
                None => Default::default(),
            };

            peers.push(PeerView {
                endpoint_id: contact.endpoint_id,
                slot: contact.slot,
                name: contact.name,
                online,
                rtt_ms,
                path,
                dnd,
                muted,
                live: live_members.contains(&contact.endpoint_id),
                speaking: speaking && online,
            });
        }

        let knocks = knocks
            .into_iter()
            .map(|k| KnockView {
                endpoint_id: k.endpoint_id,
                claimed: k.claimed,
            })
            .collect();

        let dnd = *self.dnd.lock().await;
        let muted_locally = *self.muted.lock().await;

        let mic = if live_members.is_empty() {
            MicState::Closed
        } else if muted_locally {
            MicState::Muted
        } else {
            MicState::Live
        };

        let mut live_slots: Vec<u8> = peers
            .iter()
            .filter(|p| p.live)
            .filter_map(|p| p.slot)
            .collect();
        live_slots.sort_unstable();

        // Nothing can connect before the endpoint reaches a relay, so the
        // interface must show it.
        let online = {
            let mut status = self.endpoint.home_relay_status();
            status.get().iter().any(|relay| relay.is_connected())
        };

        self.state.store(UiState {
            my_name: self.identity.name.clone(),
            my_id: Some(self.me),
            online,
            peers,
            knocks,
            mic,
            dnd,
            live_slots,
            fault: None,
            audio: self.audio_counters(),
        });
    }
}

/// Starts a supervisor for every requested contact.
///
/// This is the only place that calls `driver::supervise`. See
/// `App::supervise_one` for why the request arrives on a channel.
async fn run_supervisors(app: Arc<App>, mut rx: tokio::sync::mpsc::UnboundedReceiver<EndpointId>) {
    loop {
        tokio::select! {
            next = rx.recv() => match next {
                Some(id) => {
                    tokio::spawn(driver::supervise(app.clone(), id));
                }
                None => return,
            },
            _ = app.shutdown.cancelled() => return,
        }
    }
}

/// Watches the live session.
///
/// It does two jobs. It republishes the member connections, so a member that
/// reconnects starts receiving audio again without a keypress. And it closes a
/// session that nobody has spoken in, so a forgotten open microphone does not
/// stay open all day.
async fn watch_session(app: Arc<App>) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
            _ = app.shutdown.cancelled() => return,
        }

        let idle = {
            let guard = app.session.lock().await;
            guard.as_ref().map(|session| session.idle_for())
        };

        let Some(idle) = idle else {
            continue;
        };

        if idle >= config::SESSION_IDLE_TIMEOUT {
            info!("the session was silent for too long, so it closed");
            app.end_session().await;
            continue;
        }

        // A member may have reconnected on a new connection. Refresh the
        // targets so their audio resumes without the user pressing anything.
        app.publish_session().await;
        app.report_local_voice().await;
    }
}

/// Publishes a state change when the endpoint reaches or loses a relay.
///
/// Nothing can connect before the endpoint is online, so the interface must
/// show it.
async fn watch_online(app: Arc<App>) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
            _ = app.shutdown.cancelled() => return,
        }

        // A second process may have added a contact. Pick it up.
        if let Err(e) = app.supervise_contacts().await {
            warn!("cannot read the contact list: {e}");
        }
        app.refresh_ui().await;
    }
}
