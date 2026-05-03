use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Locality of a route entry relative to this relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Locality {
    Local,
    Peer,
}

/// A single CRM route entry in the RouteTable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteEntry {
    pub name: String,
    pub relay_id: String,
    pub relay_url: String,
    pub server_id: Option<String>,
    pub ipc_address: Option<String>,
    pub crm_ns: String,
    pub crm_ver: String,
    pub locality: Locality,
    pub registered_at: f64,
}

fn observed_now() -> Instant {
    Instant::now()
}

/// Negative route state used to propagate deletions through gossip and
/// anti-entropy without letting old announcements resurrect stale routes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteTombstone {
    pub name: String,
    pub relay_id: String,
    pub removed_at: f64,
    #[serde(skip)]
    pub server_id: Option<String>,
    #[serde(skip, default = "observed_now")]
    pub observed_at: Instant,
}

/// Resolution result returned to clients via /_resolve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteInfo {
    pub name: String,
    pub relay_url: String,
    pub ipc_address: Option<String>,
    pub crm_ns: String,
    pub crm_ver: String,
}

impl RouteEntry {
    pub fn to_route_info(&self) -> RouteInfo {
        // `ipc_address` is private to the owning relay's filesystem; only
        // expose it for LOCAL routes. PEER routes go through `relay_url`.
        let ipc_address = match self.locality {
            Locality::Local => self.ipc_address.clone(),
            Locality::Peer => None,
        };
        RouteInfo {
            name: self.name.clone(),
            relay_url: self.relay_url.clone(),
            ipc_address,
            crm_ns: self.crm_ns.clone(),
            crm_ver: self.crm_ver.clone(),
        }
    }
}

/// Peer relay status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerStatus {
    Alive,
    Suspect,
    Dead,
}

/// Information about a peer relay.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub relay_id: String,
    pub url: String,
    pub route_count: u32,
    pub last_heartbeat: Instant,
    pub status: PeerStatus,
}

/// Full sync snapshot for join protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullSync {
    pub routes: Vec<RouteEntry>,
    #[serde(default)]
    pub tombstones: Vec<RouteTombstone>,
    pub peers: Vec<PeerSnapshot>,
}

/// Serializable peer info (Instant is not serializable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerSnapshot {
    pub relay_id: String,
    pub url: String,
    pub route_count: u32,
    pub status: PeerStatus,
}

/// Convert a DigestDiffEntry into a PEER RouteEntry.
impl TryFrom<crate::relay::peer::DigestDiffEntry> for RouteEntry {
    type Error = ();

    fn try_from(d: crate::relay::peer::DigestDiffEntry) -> Result<Self, Self::Error> {
        match d {
            crate::relay::peer::DigestDiffEntry::Active {
                name,
                relay_id,
                relay_url,
                crm_ns,
                crm_ver,
                registered_at,
            } => Ok(Self {
                name,
                relay_id,
                relay_url,
                server_id: None,
                ipc_address: None,
                crm_ns,
                crm_ver,
                locality: Locality::Peer,
                registered_at,
            }),
            crate::relay::peer::DigestDiffEntry::Deleted { .. } => Err(()),
        }
    }
}
