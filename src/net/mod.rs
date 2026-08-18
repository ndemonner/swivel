//! The transport layer.
//!
//! Connections are opened once and kept warm. A talk session never waits for a
//! connection. See `ARCHITECTURE.md` §4.

pub mod audio_wire;
pub mod control;
pub mod driver;
pub mod peer;

use std::collections::HashMap;
use std::sync::Arc;

use iroh::endpoint::{Connection, QuicTransportConfig};
use iroh::{Endpoint, EndpointId, SecretKey, endpoint::presets};
use tokio::sync::RwLock;

use crate::config;
use crate::error::{Error, Result};

pub use peer::Peer;

/// Builds the QUIC transport configuration.
///
/// Every value here is a latency decision. See `ARCHITECTURE.md` §4.2 before
/// you change one.
pub fn transport_config() -> Result<QuicTransportConfig> {
    let idle = config::MAX_IDLE
        .try_into()
        .map_err(|_| Error::net("the idle timeout does not fit a QUIC varint"))?;

    Ok(QuicTransportConfig::builder()
        // Small on purpose. When the link congests, `send_datagram` fails and
        // the encoder drops a frame. A dropped frame costs 10 ms once. A queued
        // frame costs latency for the rest of the session.
        .datagram_send_buffer_size(config::DATAGRAM_SEND_BUFFER)
        .datagram_receive_buffer_size(Some(config::DATAGRAM_RECV_BUFFER))
        // The default assumed round trip delays the first packets of a new
        // connection. Audio starts flowing the moment a session opens.
        .initial_rtt(config::INITIAL_RTT)
        .keep_alive_interval(config::KEEP_ALIVE)
        .max_idle_timeout(Some(idle))
        // Audio packets are about 80 bytes. Do not pay for MTU probing.
        .initial_mtu(config::INITIAL_MTU)
        .build())
}

/// Builds and binds the endpoint.
pub async fn bind(secret_key: SecretKey) -> Result<Endpoint> {
    Endpoint::builder(presets::N0)
        .secret_key(secret_key)
        .alpns(vec![config::ALPN.to_vec()])
        .transport_config(transport_config()?)
        .bind()
        .await
        .map_err(Error::net)
}

/// The set of contacts and their connections.
#[derive(Default)]
pub struct PeerMap {
    peers: RwLock<HashMap<EndpointId, Arc<Peer>>>,
}

impl PeerMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the peer, creating it if this is the first time we saw it.
    pub async fn get_or_create(&self, id: EndpointId) -> Arc<Peer> {
        if let Some(p) = self.peers.read().await.get(&id) {
            return p.clone();
        }

        let mut peers = self.peers.write().await;
        // Another task may have created it while we waited for the write lock.
        peers.entry(id).or_insert_with(|| Peer::new(id)).clone()
    }

    pub async fn get(&self, id: EndpointId) -> Option<Arc<Peer>> {
        self.peers.read().await.get(&id).cloned()
    }

    pub async fn remove(&self, id: EndpointId) -> Option<Arc<Peer>> {
        self.peers.write().await.remove(&id)
    }

    pub async fn all(&self) -> Vec<Arc<Peer>> {
        self.peers.read().await.values().cloned().collect()
    }

    pub async fn len(&self) -> usize {
        self.peers.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.peers.read().await.is_empty()
    }
}

/// Closes a connection that lost a duplicate tie-break, or that we refuse.
pub fn close_connection(conn: &Connection, reason: &'static str) {
    conn.close(1u32.into(), reason.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn id(n: u8) -> EndpointId {
        SecretKey::from_bytes(&[n; 32]).public()
    }

    #[test]
    fn the_transport_config_builds() {
        transport_config().expect("the tuned config must be valid");
    }

    #[tokio::test]
    async fn get_or_create_is_stable() {
        let map = PeerMap::new();
        let first = map.get_or_create(id(1)).await;
        let second = map.get_or_create(id(1)).await;
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(map.len().await, 1);

        map.get_or_create(id(2)).await;
        assert_eq!(map.len().await, 2);

        map.remove(id(1)).await;
        assert_eq!(map.len().await, 1);
        assert!(map.get(id(1)).await.is_none());
    }
}
