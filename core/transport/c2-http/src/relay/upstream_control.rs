use std::sync::Arc;
use std::time::Duration;

use c2_ipc::IpcClient;

use crate::relay::gossip::broadcast_route_withdraw;
use crate::relay::state::RelayState;
use crate::relay::types::{Locality, RouteEntry};

const CONTROL_RETRY_DELAY: Duration = Duration::from_secs(1);
const CONTROL_OBSERVE_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct UpstreamOwnerKey {
    server_id: String,
    server_instance_id: String,
    address: String,
}

impl UpstreamOwnerKey {
    pub(crate) fn address(&self) -> &str {
        &self.address
    }

    pub(crate) fn server_id(&self) -> &str {
        &self.server_id
    }

    pub(crate) fn server_instance_id(&self) -> &str {
        &self.server_instance_id
    }
}

pub(crate) struct UpstreamControlTask {
    token: Arc<()>,
    handle: tokio::task::JoinHandle<()>,
}

impl UpstreamControlTask {
    pub(crate) fn new(token: Arc<()>, handle: tokio::task::JoinHandle<()>) -> Self {
        Self { token, handle }
    }

    pub(crate) fn token_matches(&self, token: &Arc<()>) -> bool {
        Arc::ptr_eq(&self.token, token)
    }

    pub(crate) fn abort(self) {
        self.handle.abort();
    }
}

pub(crate) fn owner_key_for_route(entry: &RouteEntry) -> Option<UpstreamOwnerKey> {
    if entry.locality != Locality::Local {
        return None;
    }
    Some(UpstreamOwnerKey {
        server_id: entry.server_id.clone()?,
        server_instance_id: entry.server_instance_id.clone()?,
        address: entry.ipc_address.clone()?,
    })
}

pub(crate) fn spawn(state: Arc<RelayState>, key: UpstreamOwnerKey) -> UpstreamControlTask {
    let task_key = key.clone();
    let token = Arc::new(());
    let task_token = Arc::clone(&token);
    let handle = tokio::spawn(async move {
        run_control_watch(state, task_key, task_token).await;
    });
    UpstreamControlTask::new(token, handle)
}

async fn run_control_watch(state: Arc<RelayState>, key: UpstreamOwnerKey, token: Arc<()>) {
    loop {
        if state.local_routes_for_owner(&key).is_empty() {
            state.clear_upstream_control_if_matches(&key, &token);
            return;
        }

        let mut client = IpcClient::new(key.address());
        match client.connect().await {
            Ok(()) => {}
            Err(err) => {
                eprintln!(
                    "[relay] Upstream control watch connect failed: server_id={} server_instance_id={} address={} error={err}",
                    key.server_id(),
                    key.server_instance_id(),
                    key.address()
                );
                state.mark_upstream_control_watch_unavailable(
                    &key,
                    format!("control watch connect failed: {err}"),
                );
                tokio::time::sleep(CONTROL_RETRY_DELAY).await;
                continue;
            }
        }

        if client.server_id() != Some(key.server_id())
            || client.server_instance_id() != Some(key.server_instance_id())
        {
            eprintln!(
                "[relay] Upstream control watch identity mismatch: expected_server_id={} expected_server_instance_id={} address={} actual_server_id={} actual_server_instance_id={}",
                key.server_id(),
                key.server_instance_id(),
                key.address(),
                client.server_id().unwrap_or(""),
                client.server_instance_id().unwrap_or("")
            );
            remove_owner_routes(&state, &key, "identity_mismatch").await;
            client.close().await;
            state.clear_upstream_control_if_matches(&key, &token);
            return;
        }
        state.clear_upstream_control_watch_unavailable(&key);

        loop {
            if state.local_routes_for_owner(&key).is_empty() {
                client.close().await;
                state.clear_upstream_control_if_matches(&key, &token);
                return;
            }

            for route in state.local_routes_for_owner(&key) {
                if route_is_semantically_gone(&client, &route) {
                    remove_route(&state, &route, "route_catalog_update").await;
                }
            }

            if !client.is_connected() {
                eprintln!(
                    "[relay] Upstream control watch disconnected: server_id={} server_instance_id={} address={}",
                    key.server_id(),
                    key.server_instance_id(),
                    key.address()
                );
                state.mark_upstream_control_watch_unavailable(&key, "control watch disconnected");
                break;
            }
            tokio::time::sleep(CONTROL_OBSERVE_INTERVAL).await;
        }

        client.close().await;
        tokio::time::sleep(CONTROL_RETRY_DELAY).await;
    }
}

fn route_is_semantically_gone(client: &IpcClient, route: &RouteEntry) -> bool {
    let expected = expected_contract_for_route(route);
    matches!(
        client.validate_route_contract(&expected),
        Err(c2_ipc::IpcError::RouteNotFound(_))
            | Err(c2_ipc::IpcError::RouteRemoved { .. })
            | Err(c2_ipc::IpcError::RouteClosed { .. })
            | Err(c2_ipc::IpcError::ContractMismatch(_))
    )
}

async fn remove_owner_routes(state: &Arc<RelayState>, key: &UpstreamOwnerKey, reason: &str) {
    for route in state.local_routes_for_owner(key) {
        remove_route(state, &route, reason).await;
    }
}

async fn remove_route(state: &Arc<RelayState>, route: &RouteEntry, reason: &str) {
    let Some((entry, removed_at, removed_revision, client)) =
        state.remove_unreachable_local_upstream_if_matches(route)
    else {
        return;
    };
    if let Some(client) = client {
        client.close_shared().await;
    }
    eprintln!(
        "[relay] Upstream control watch removed route: name={} server_id={} server_instance_id={} address={} removed_at={} removed_revision={} reason={reason}",
        entry.name,
        entry.server_id.as_deref().unwrap_or(""),
        entry.server_instance_id.as_deref().unwrap_or(""),
        entry.ipc_address.as_deref().unwrap_or(""),
        removed_at,
        removed_revision
    );
    broadcast_route_withdraw(state, &entry, removed_at, removed_revision);
}

fn expected_contract_for_route(route: &RouteEntry) -> c2_contract::ExpectedRouteContract {
    c2_contract::ExpectedRouteContract {
        route_name: route.name.clone(),
        crm_ns: route.crm_ns.clone(),
        crm_name: route.crm_name.clone(),
        crm_ver: route.crm_ver.clone(),
        abi_hash: route.abi_hash.clone(),
        signature_hash: route.signature_hash.clone(),
    }
}
