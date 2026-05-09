use std::collections::HashMap;

use c2_server::scheduler::{AccessLevel, ConcurrencyMode};

#[derive(Debug, Clone)]
pub struct RuntimeRouteSpec {
    pub name: String,
    pub crm_ns: String,
    pub crm_name: String,
    pub crm_ver: String,
    pub method_names: Vec<String>,
    pub access_map: HashMap<u16, AccessLevel>,
    pub concurrency_mode: ConcurrencyMode,
    pub max_pending: Option<usize>,
    pub max_workers: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterOutcome {
    pub route_name: String,
    pub server_id: String,
    pub server_instance_id: String,
    pub ipc_address: String,
    pub relay_registered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayCleanupError {
    pub route_name: String,
    pub status_code: Option<u16>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnregisterOutcome {
    pub route_name: String,
    pub local_removed: bool,
    pub relay_error: Option<RelayCleanupError>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShutdownOutcome {
    pub removed_routes: Vec<String>,
    pub relay_errors: Vec<RelayCleanupError>,
    pub server_was_started: bool,
    pub ipc_clients_drained: bool,
    pub http_clients_drained: bool,
}
