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

use crate::audio::AudioSink;
use crate::config;
use crate::error::Result;
use crate::net::control::{self, Control};
use crate::net::peer::Peer;
use crate::net::{PeerMap, driver};
use crate::state::{KnockView, MicState, PeerView, StateHandle, UiState};
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
}

impl App {
    /// Opens the store, binds the endpoint, and starts every background task.
    pub async fn start(audio: Arc<dyn AudioSink>) -> Result<Arc<Self>> {
        let store = Store::open()?;
        let identity = store.identity(&crate::store::identity::default_name())?;
        let me = identity.endpoint_id();

        info!(id = %me, name = %identity.name, "starting");

        let endpoint = crate::net::bind(identity.secret_key.clone()).await?;

        let app = Arc::new(App {
            identity,
            me,
            endpoint,
            peers: PeerMap::new(),
            audio,
            state: StateHandle::new(),
            shutdown: CancellationToken::new(),
            store: Mutex::new(store),
            pending_pings: Mutex::new(HashMap::new()),
            bad_datagrams: AtomicU64::new(0),
            dnd: Mutex::new(false),
            supervised: Mutex::new(std::collections::HashSet::new()),
        });

        app.refresh_ui().await;

        tokio::spawn(driver::accept_loop(app.clone()));
        tokio::spawn(watch_online(app.clone()));

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

    async fn supervise_one(self: &Arc<Self>, id: EndpointId) {
        if !self.supervised.lock().await.insert(id) {
            return;
        }
        tokio::spawn(driver::supervise(self.clone(), id));
    }

    /// Your shareable key.
    pub fn my_ticket(&self) -> String {
        Ticket::new(self.me, &self.identity.name).encode()
    }

    /// Stops every background task and closes the endpoint.
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        self.endpoint.close().await;
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

            // M4 handles session membership.
            Control::SessionOpen { .. } | Control::SessionClose { .. } => {
                debug!(peer = %peer.id.fmt_short(), "a session message arrived before M4");
            }
        }
    }

    /// Tells one peer how we are.
    pub async fn send_presence_to(&self, peer: &Arc<Peer>) {
        let dnd = *self.dnd.lock().await;
        peer.send_control(Control::Presence {
            available: true,
            dnd,
            muted: false,
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
                // M4 fills this in.
                live: false,
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
            mic: MicState::Closed,
            dnd,
            live_slots: Vec::new(),
            fault: None,
        });
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
