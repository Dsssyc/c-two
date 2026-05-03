//! Route control-plane authority.
//!
//! All mutating route operations flow through this module so owner checks,
//! peer ownership checks, and route-table updates stay in one state machine.

use std::sync::Arc;

use c2_ipc::IpcClient;
use c2_wire::control::MAX_CALL_ROUTE_NAME_BYTES;

use crate::relay::conn_pool::{CachedClient, OwnerToken};
use crate::relay::state::RelayState;
use crate::relay::types::{Locality, RouteEntry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControlError {
    InvalidName { reason: String },
    InvalidServerId { reason: String },
    AddressMismatch { existing_address: String },
    DuplicateRoute { existing_address: String },
    OwnerMismatch,
    NotFound,
}

#[derive(Clone)]
pub(crate) enum RegisterPreflight {
    Available {
        replacement: Option<OwnerReplacement>,
    },
    SameOwner,
}

pub(crate) enum RegisterPreparation {
    Available {
        replacement: Option<OwnerReplacement>,
    },
    SameOwner,
    DuplicateAlive {
        existing_address: String,
    },
}

#[derive(Clone)]
pub(crate) struct OwnerReplacement {
    pub existing_address: String,
    pub token: OwnerToken,
}

pub(crate) enum RouteCommand {
    RegisterLocal {
        name: String,
        server_id: String,
        address: String,
        crm_ns: String,
        crm_ver: String,
        client: Arc<IpcClient>,
        replacement: Option<OwnerReplacement>,
    },
    UnregisterLocal {
        name: String,
        server_id: String,
    },
    AnnouncePeer {
        sender_relay_id: String,
        entry: RouteEntry,
    },
    WithdrawPeer {
        sender_relay_id: String,
        name: String,
        relay_id: String,
        removed_at: f64,
    },
    RemovePeerRoutes {
        relay_id: String,
    },
}

pub(crate) enum RouteCommandResult {
    Registered {
        entry: RouteEntry,
    },
    SameOwner {
        entry: RouteEntry,
    },
    Unregistered {
        entry: RouteEntry,
        removed_at: f64,
        client: Option<Arc<IpcClient>>,
    },
    AlreadyUnregistered,
    PeerRouteChanged,
    PeerRoutesRemoved,
}

pub(crate) struct RouteAuthority<'a> {
    state: &'a RelayState,
}

impl<'a> RouteAuthority<'a> {
    pub(crate) fn new(state: &'a RelayState) -> Self {
        Self { state }
    }

    pub(crate) fn validate_route_name(&self, name: &str) -> Result<(), ControlError> {
        if name.trim().is_empty() {
            return Err(ControlError::InvalidName {
                reason: "name cannot be empty".to_string(),
            });
        }
        if name.as_bytes().len() > MAX_CALL_ROUTE_NAME_BYTES {
            return Err(ControlError::InvalidName {
                reason: "name exceeds wire route-name limit".to_string(),
            });
        }
        Ok(())
    }

    pub(crate) fn validate_relay_id(&self, relay_id: &str) -> Result<(), ControlError> {
        if relay_id.trim().is_empty() {
            return Err(ControlError::OwnerMismatch);
        }
        Ok(())
    }

    pub(crate) fn validate_server_id(&self, server_id: &str) -> Result<(), ControlError> {
        c2_config::validate_server_id(server_id)
            .map_err(|reason| ControlError::InvalidServerId { reason })
    }

    pub(crate) fn register_local_preflight(
        &self,
        name: &str,
        server_id: &str,
        address: &str,
    ) -> Result<RegisterPreflight, ControlError> {
        self.validate_route_name(name)?;
        self.validate_server_id(server_id)?;

        let existing = self.state.local_route(name);
        let Some(existing) = existing else {
            return Ok(RegisterPreflight::Available { replacement: None });
        };

        let existing_address = existing.ipc_address.clone().unwrap_or_default();
        let existing_server_id = existing.server_id.unwrap_or_default();
        if existing_server_id == server_id {
            if existing_address == address {
                return Ok(RegisterPreflight::SameOwner);
            }
            return Err(ControlError::AddressMismatch { existing_address });
        }

        match self.state.connection_lookup(name) {
            CachedClient::Ready {
                address: existing_address,
                ..
            } => Err(ControlError::DuplicateRoute { existing_address }),
            CachedClient::Evicted {
                address: existing_address,
            }
            | CachedClient::Disconnected {
                address: existing_address,
            } => {
                let Some(token) = self.state.owner_token(name) else {
                    return Err(ControlError::DuplicateRoute { existing_address });
                };
                Ok(RegisterPreflight::Available {
                    replacement: Some(OwnerReplacement {
                        existing_address,
                        token,
                    }),
                })
            }
            CachedClient::Missing => Err(ControlError::DuplicateRoute { existing_address }),
        }
    }

    pub(crate) async fn prepare_register(
        &self,
        name: &str,
        server_id: &str,
        address: &str,
    ) -> Result<RegisterPreparation, ControlError> {
        let mut last_stale_owner = None;
        for _ in 0..3 {
            match self.register_local_preflight(name, server_id, address)? {
                RegisterPreflight::Available { replacement: None } => {
                    return Ok(RegisterPreparation::Available { replacement: None });
                }
                RegisterPreflight::SameOwner => return Ok(RegisterPreparation::SameOwner),
                RegisterPreflight::Available {
                    replacement: Some(replacement),
                } => match self
                    .probe_owner(name, &replacement.existing_address, &replacement.token)
                    .await
                {
                    OwnerProbe::Alive => {
                        return Ok(RegisterPreparation::DuplicateAlive {
                            existing_address: replacement.existing_address,
                        });
                    }
                    OwnerProbe::RouteMissing | OwnerProbe::Dead => {
                        return Ok(RegisterPreparation::Available {
                            replacement: Some(replacement),
                        });
                    }
                    OwnerProbe::Stale => {
                        last_stale_owner = Some(replacement.existing_address);
                    }
                },
            }
        }
        Ok(RegisterPreparation::DuplicateAlive {
            existing_address: last_stale_owner.unwrap_or_else(|| "<unknown>".to_string()),
        })
    }

    pub(crate) fn execute(
        &self,
        command: RouteCommand,
    ) -> Result<RouteCommandResult, ControlError> {
        match command {
            RouteCommand::RegisterLocal {
                name,
                server_id,
                address,
                crm_ns,
                crm_ver,
                client,
                replacement,
            } => self.register_local(
                name,
                server_id,
                address,
                crm_ns,
                crm_ver,
                client,
                replacement,
            ),
            RouteCommand::UnregisterLocal { name, server_id } => {
                self.unregister_local(name, server_id)
            }
            RouteCommand::AnnouncePeer {
                sender_relay_id,
                entry,
            } => self.announce_peer(sender_relay_id, entry),
            RouteCommand::WithdrawPeer {
                sender_relay_id,
                name,
                relay_id,
                removed_at,
            } => self.withdraw_peer(sender_relay_id, name, relay_id, removed_at),
            RouteCommand::RemovePeerRoutes { relay_id } => {
                self.remove_peer_routes(&relay_id);
                Ok(RouteCommandResult::PeerRoutesRemoved)
            }
        }
    }

    fn register_local(
        &self,
        name: String,
        server_id: String,
        address: String,
        crm_ns: String,
        crm_ver: String,
        client: Arc<IpcClient>,
        replacement: Option<OwnerReplacement>,
    ) -> Result<RouteCommandResult, ControlError> {
        self.validate_route_name(&name)?;
        self.validate_server_id(&server_id)?;

        let mut route_table = self.state.route_table_write();
        if let Some(existing) = route_table.local_route(&name) {
            let existing_address = existing.ipc_address.clone().unwrap_or_default();
            let existing_server_id = existing.server_id.clone().unwrap_or_default();
            if existing_server_id == server_id {
                if existing_address == address {
                    return Ok(RouteCommandResult::SameOwner { entry: existing });
                }
                return Err(ControlError::AddressMismatch { existing_address });
            }
            match replacement {
                Some(token) if token.existing_address == existing_address => {
                    if !self.state.can_replace_owner_token(&name, &token.token) {
                        return Err(ControlError::DuplicateRoute { existing_address });
                    }
                }
                _ => return Err(ControlError::DuplicateRoute { existing_address }),
            }
        }

        let entry = RouteEntry {
            name: name.clone(),
            relay_id: self.state.relay_id().to_string(),
            relay_url: self.state.config().effective_advertise_url(),
            server_id: Some(server_id),
            ipc_address: Some(address.clone()),
            crm_ns,
            crm_ver,
            locality: Locality::Local,
            registered_at: route_table.next_local_timestamp(),
        };
        if !route_table.register_route(entry.clone()) {
            return Err(ControlError::OwnerMismatch);
        }
        self.state.insert_connection(name, address, client);
        drop(route_table);
        Ok(RouteCommandResult::Registered { entry })
    }

    fn unregister_local(
        &self,
        name: String,
        server_id: String,
    ) -> Result<RouteCommandResult, ControlError> {
        self.validate_route_name(&name)?;
        self.validate_server_id(&server_id)?;

        let (entry, removed_at, client) = {
            let mut route_table = self.state.route_table_write();
            let Some(existing) = route_table.local_route(&name) else {
                if route_table.local_tombstone_matches_server(&name, &server_id) {
                    return Ok(RouteCommandResult::AlreadyUnregistered);
                }
                return Err(ControlError::NotFound);
            };
            if existing.server_id.as_deref() != Some(server_id.as_str()) {
                return Err(ControlError::OwnerMismatch);
            }
            let (entry, removed_at) =
                route_table.unregister_local_route_with_tombstone(&name, &server_id);
            let client = self.state.remove_connection(&name);
            (entry, removed_at, client)
        };
        let Some(entry) = entry else {
            return Err(ControlError::NotFound);
        };
        Ok(RouteCommandResult::Unregistered {
            entry,
            removed_at,
            client,
        })
    }

    fn announce_peer(
        &self,
        sender_relay_id: String,
        mut entry: RouteEntry,
    ) -> Result<RouteCommandResult, ControlError> {
        self.validate_route_name(&entry.name)?;
        self.validate_relay_id(&sender_relay_id)?;
        self.validate_relay_id(&entry.relay_id)?;
        if !self.trusted_peer_owner(&sender_relay_id, &entry.relay_id) {
            return Err(ControlError::OwnerMismatch);
        }
        let mut route_table = self.state.route_table_write();
        if entry.relay_id == route_table.relay_id() {
            return Err(ControlError::OwnerMismatch);
        }
        let peer_url = route_table
            .get_peer(&entry.relay_id)
            .map(|peer| peer.url.clone())
            .ok_or(ControlError::OwnerMismatch)?;
        entry.relay_url = peer_url;
        entry.server_id = None;
        entry.ipc_address = None;
        entry.locality = Locality::Peer;
        route_table.register_route(entry);
        Ok(RouteCommandResult::PeerRouteChanged)
    }

    fn withdraw_peer(
        &self,
        sender_relay_id: String,
        name: String,
        relay_id: String,
        removed_at: f64,
    ) -> Result<RouteCommandResult, ControlError> {
        self.validate_route_name(&name)?;
        self.validate_relay_id(&sender_relay_id)?;
        self.validate_relay_id(&relay_id)?;
        if !self.trusted_peer_owner(&sender_relay_id, &relay_id) {
            return Err(ControlError::OwnerMismatch);
        }
        if relay_id == self.state.relay_id() {
            return Err(ControlError::OwnerMismatch);
        }
        self.state
            .route_table_write()
            .unregister_route_with_tombstone(&name, &relay_id, removed_at);
        Ok(RouteCommandResult::PeerRouteChanged)
    }

    fn remove_peer_routes(&self, relay_id: &str) {
        if self.validate_relay_id(relay_id).is_err() || relay_id == self.state.relay_id() {
            return;
        }
        self.state
            .route_table_write()
            .remove_routes_by_relay(relay_id);
    }

    fn trusted_peer_owner(&self, sender_relay_id: &str, relay_id: &str) -> bool {
        sender_relay_id == relay_id
            && sender_relay_id != self.state.relay_id()
            && self.state.peer_is_alive(sender_relay_id)
    }

    async fn probe_owner(
        &self,
        name: &str,
        existing_address: &str,
        token: &OwnerToken,
    ) -> OwnerProbe {
        let mut client = IpcClient::new(existing_address);
        match client.connect().await {
            Ok(()) => {
                let route_exists = client.route_table(name).is_some();
                client.close().await;

                if !route_exists {
                    OwnerProbe::RouteMissing
                } else if self.state.matches_owner_token(name, token) {
                    OwnerProbe::Alive
                } else {
                    OwnerProbe::Stale
                }
            }
            Err(_) => OwnerProbe::Dead,
        }
    }
}

enum OwnerProbe {
    Alive,
    RouteMissing,
    Dead,
    Stale,
}
