//! Axum router for the multi-upstream relay server.

use std::sync::Arc;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};

use crate::relay::conn_pool::{AcquireError, UpstreamLease};
use crate::relay::peer::{PeerEnvelope, PeerMessage};
use crate::relay::peer_handlers;
use crate::relay::state::{
    LocalOwnerStatus, OwnerReplacementToken, RegisterCommitResult, RelayState,
};
use c2_ipc::IpcClient;

enum RegisterOwnerCheck {
    Available {
        replacement: Option<OwnerReplacementToken>,
    },
    SameOwner,
    DuplicateAlive {
        existing_address: String,
    },
}

enum RequestClient {
    Ready { lease: UpstreamLease },
    NotFound,
    Unreachable,
}

enum OwnerProbe {
    Alive,
    Dead,
    Stale,
}

fn duplicate_route_response(name: &str, existing_address: &str) -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "error": "DuplicateRoute",
            "name": name,
            "existing_address": existing_address,
        })),
    )
        .into_response()
}

/// Build the relay axum router with control-plane and data-plane endpoints.
pub fn build_router(state: Arc<RelayState>) -> Router {
    Router::new()
        .route("/_register", post(handle_register))
        .route("/_unregister", post(handle_unregister))
        .route("/_routes", get(handle_list_routes))
        .route("/_resolve/{name}", get(handle_resolve))
        .route("/_peers", get(handle_peers))
        .route("/_peer/announce", post(peer_handlers::handle_peer_announce))
        .route("/_peer/join", post(peer_handlers::handle_peer_join))
        .route("/_peer/sync", get(peer_handlers::handle_peer_sync))
        .route(
            "/_peer/heartbeat",
            post(peer_handlers::handle_peer_heartbeat),
        )
        .route("/_peer/leave", post(peer_handlers::handle_peer_leave))
        .route("/_peer/digest", post(peer_handlers::handle_peer_digest))
        .route("/health", get(handle_health))
        .route("/_echo", post(echo_handler))
        .route("/{route_name}/{method_name}", post(call_handler))
        .with_state(state)
        .layer(DefaultBodyLimit::disable())
}

// -- Control-plane handlers -----------------------------------------------

/// `POST /_register` — register a new upstream CRM.
///
/// Body: `{"name": "grid", "address": "ipc://...", "crm_ns": "...", "crm_ver": "..."}`
/// Returns: 201 on success, 409 on duplicate, 502 on connection failure.
async fn handle_register(
    State(state): State<Arc<RelayState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let name = match body.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return (StatusCode::BAD_REQUEST, "Missing \"name\"").into_response(),
    };
    let address = match body.get("address").and_then(|v| v.as_str()) {
        Some(a) => a.to_string(),
        None => return (StatusCode::BAD_REQUEST, "Missing \"address\"").into_response(),
    };
    let crm_ns = body
        .get("crm_ns")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let crm_ver = body
        .get("crm_ver")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let replacement = match check_local_owner_for_register(&state, &name, &address).await {
        RegisterOwnerCheck::Available { replacement } => replacement,
        RegisterOwnerCheck::SameOwner => None,
        RegisterOwnerCheck::DuplicateAlive { existing_address } => {
            return duplicate_route_response(&name, &existing_address);
        }
    };

    // Connect IPC client (or skip if configured for testing)
    let client = if state.config().skip_ipc_validation {
        Arc::new(IpcClient::new(&address))
    } else {
        let mut c = IpcClient::new(&address);
        if let Err(e) = c.connect().await {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": format!("Failed to connect upstream '{name}' at {address}: {e}")})),
            ).into_response();
        }
        Arc::new(c)
    };

    let commit_client = client.clone();
    let entry = match state.commit_register_upstream(
        name.clone(),
        address,
        crm_ns,
        crm_ver,
        commit_client,
        replacement,
    ) {
        RegisterCommitResult::Registered { entry } => entry,
        RegisterCommitResult::SameOwner { entry } => {
            close_arc_client(client);
            entry
        }
        RegisterCommitResult::Duplicate { existing_address } => {
            close_arc_client(client);
            return duplicate_route_response(&name, &existing_address);
        }
    };

    // Gossip announce to peers — strip ipc_address since it's a local UDS
    // path on THIS relay's filesystem and is meaningless to remote peers.
    let envelope = PeerEnvelope::new(
        state.relay_id(),
        PeerMessage::RouteAnnounce {
            name: entry.name.clone(),
            relay_id: entry.relay_id.clone(),
            relay_url: entry.relay_url.clone(),
            ipc_address: None,
            crm_ns: entry.crm_ns.clone(),
            crm_ver: entry.crm_ver.clone(),
            registered_at: entry.registered_at,
        },
    );
    let peers = state.list_peers();
    state.disseminator().broadcast(envelope, &peers);

    (
        StatusCode::CREATED,
        Json(serde_json::json!({"registered": name})),
    )
        .into_response()
}

fn close_arc_client(arc_client: Arc<IpcClient>) {
    tokio::spawn(async move { arc_client.close_shared().await });
}

async fn check_local_owner_for_register(
    state: &RelayState,
    name: &str,
    address: &str,
) -> RegisterOwnerCheck {
    let mut last_stale_owner = None;
    for _ in 0..3 {
        match state.check_local_owner(name, address) {
            LocalOwnerStatus::NoOwner => {
                return RegisterOwnerCheck::Available { replacement: None };
            }
            LocalOwnerStatus::SameAddress => return RegisterOwnerCheck::SameOwner,
            LocalOwnerStatus::DifferentAddressReady {
                existing_address, ..
            } => return RegisterOwnerCheck::DuplicateAlive { existing_address },
            LocalOwnerStatus::DifferentAddressNeedsProbe {
                existing_address,
                generation,
            } => match probe_owner(state, name, &existing_address, generation).await {
                OwnerProbe::Alive => {
                    return RegisterOwnerCheck::DuplicateAlive { existing_address };
                }
                OwnerProbe::Dead => {
                    return RegisterOwnerCheck::Available {
                        replacement: Some(OwnerReplacementToken {
                            existing_address,
                            generation,
                        }),
                    };
                }
                OwnerProbe::Stale => {
                    last_stale_owner = Some(existing_address);
                }
            },
        }
    }
    RegisterOwnerCheck::DuplicateAlive {
        existing_address: last_stale_owner.unwrap_or_else(|| "<unknown>".to_string()),
    }
}

async fn probe_owner(
    state: &RelayState,
    name: &str,
    existing_address: &str,
    generation: u64,
) -> OwnerProbe {
    let mut client = IpcClient::new(existing_address);
    match client.connect().await {
        Ok(()) => {
            if state
                .reconnect_candidate(name)
                .is_some_and(|(address, current_generation)| {
                    address == existing_address && current_generation == generation
                })
            {
                client.close().await;
                OwnerProbe::Alive
            } else {
                client.close().await;
                OwnerProbe::Stale
            }
        }
        Err(_) => OwnerProbe::Dead,
    }
}

/// `POST /_unregister` — remove a CRM upstream.
///
/// Body: `{"name": "grid"}`
/// Returns: 200 on success, 404 on missing.
async fn handle_unregister(
    State(state): State<Arc<RelayState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let name = match body.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return (StatusCode::BAD_REQUEST, "Missing \"name\"").into_response(),
    };

    match state.unregister_upstream(&name) {
        Some((entry, old_client)) => {
            // Close old client asynchronously
            if let Some(arc_client) = old_client {
                close_arc_client(arc_client);
            }

            // Gossip withdraw to peers
            let envelope = PeerEnvelope::new(
                state.relay_id(),
                PeerMessage::RouteWithdraw {
                    name: entry.name.clone(),
                    relay_id: entry.relay_id.clone(),
                },
            );
            let peers = state.list_peers();
            state.disseminator().broadcast(envelope, &peers);

            (
                StatusCode::OK,
                Json(serde_json::json!({"unregistered": name})),
            )
                .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("Route name not registered: '{name}'")})),
        )
            .into_response(),
    }
}

/// `GET /_routes` — list all registered routes.
///
/// Intentionally omits `ipc_address` even for LOCAL routes: this endpoint is
/// reachable by anyone who can hit the relay's HTTP port, and the local UDS
/// path is filesystem-private to the owning host.
async fn handle_list_routes(State(state): State<Arc<RelayState>>) -> impl IntoResponse {
    let routes: Vec<serde_json::Value> = state
        .list_routes()
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "name": r.name,
                "relay_id": r.relay_id,
                "relay_url": r.relay_url,
                "locality": match r.locality {
                    crate::relay::types::Locality::Local => "local",
                    crate::relay::types::Locality::Peer => "peer",
                },
                "crm_ns": r.crm_ns,
                "crm_ver": r.crm_ver,
            })
        })
        .collect();
    Json(serde_json::json!({"routes": routes}))
}

// -- Data-plane handlers --------------------------------------------------

/// `GET /health` — liveness check.
async fn handle_health(State(state): State<Arc<RelayState>>) -> impl IntoResponse {
    let route_names = state.route_names();
    Json(serde_json::json!({
        "status": "ok",
        "routes": route_names,
    }))
}

/// `GET /_resolve/{name}` — resolve a CRM name to available routes.
async fn handle_resolve(
    Path(name): Path<String>,
    State(state): State<Arc<RelayState>>,
) -> impl IntoResponse {
    let routes = state.resolve(&name);
    if routes.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "ResourceNotFound", "name": name,
            })),
        )
            .into_response();
    }
    Json(routes).into_response()
}

/// `GET /_peers` — list known peer relays.
async fn handle_peers(State(state): State<Arc<RelayState>>) -> impl IntoResponse {
    Json(state.list_peers()).into_response()
}

/// `POST /{route_name}/{method_name}` — relay CRM call to upstream.
///
/// If the upstream was evicted by the idle sweeper, attempts a lazy
/// reconnect before returning 502.
async fn call_handler(
    State(state): State<Arc<RelayState>>,
    Path((route_name, method_name)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    let lease = match acquire_request_client(state.clone(), &route_name).await {
        RequestClient::Ready { lease } => lease,
        RequestClient::NotFound => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": format!("No upstream registered for route: '{route_name}'")
                })),
            )
                .into_response();
        }
        RequestClient::Unreachable => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": format!("Upstream '{route_name}' is registered but unreachable")
                })),
            )
                .into_response();
        }
    };
    let client = lease.client();
    let request_generation = lease.generation();

    match client.call(&route_name, &method_name, &body).await {
        Ok(result) => {
            let bytes = result
                .into_bytes_with_pool(client.server_pool_arc(), &client.reassembly_pool_arc())
                .unwrap_or_default();
            (
                StatusCode::OK,
                [("content-type", "application/octet-stream")],
                bytes,
            )
                .into_response()
        }
        Err(c2_ipc::IpcError::CrmError(err_bytes)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [("content-type", "application/octet-stream")],
            err_bytes,
        )
            .into_response(),
        Err(e) => {
            // Evict dead client so next request triggers reconnect.
            state.evict_connection_generation(&route_name, request_generation);
            (
                StatusCode::BAD_GATEWAY,
                [("content-type", "text/plain")],
                format!("relay error: {e}"),
            )
                .into_response()
        }
    }
}

async fn acquire_request_client(state: Arc<RelayState>, route_name: &str) -> RequestClient {
    match state.acquire_upstream(route_name).await {
        Ok(lease) => RequestClient::Ready { lease },
        Err(AcquireError::NotFound) => RequestClient::NotFound,
        Err(AcquireError::Unreachable(e)) => {
            eprintln!("[relay] Failed to acquire upstream '{route_name}': {e}");
            RequestClient::Unreachable
        }
    }
}

/// `POST /_echo` — echo endpoint for benchmarking the relay itself.
///
/// Returns the request body immediately with no IPC round-trip.
async fn echo_handler(body: Bytes) -> Response {
    (
        StatusCode::OK,
        [("content-type", "application/octet-stream")],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use c2_config::RelayConfig;
    use c2_ipc::IpcClient;
    use c2_server::{
        ConcurrencyMode, CrmCallback, CrmError, CrmRoute, RequestData, ResponseMeta, Scheduler,
        Server, ServerIpcConfig,
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use tower::ServiceExt;

    struct NoopDisseminator;

    impl crate::relay::disseminator::Disseminator for NoopDisseminator {
        fn broadcast(
            &self,
            _envelope: PeerEnvelope,
            _peers: &[crate::relay::types::PeerSnapshot],
        ) -> Option<tokio::task::JoinHandle<()>> {
            None
        }
    }

    struct Echo;

    impl CrmCallback for Echo {
        fn invoke(
            &self,
            _route_name: &str,
            _method_idx: u16,
            _request: RequestData,
            _response_pool: Arc<parking_lot::RwLock<c2_mem::MemPool>>,
        ) -> Result<ResponseMeta, CrmError> {
            Ok(ResponseMeta::Inline(b"echo".to_vec()))
        }
    }

    fn test_state() -> Arc<RelayState> {
        let config = RelayConfig {
            relay_id: "test-relay".into(),
            skip_ipc_validation: false,
            ..RelayConfig::default()
        };
        Arc::new(RelayState::new(
            Arc::new(config),
            Arc::new(NoopDisseminator),
        ))
    }

    fn register_body(name: &str, address: &str) -> Body {
        Body::from(serde_json::json!({ "name": name, "address": address }).to_string())
    }

    async fn post_register(state: Arc<RelayState>, name: &str, address: &str) -> StatusCode {
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_register")
                    .header("content-type", "application/json")
                    .body(register_body(name, address))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        status
    }

    async fn start_live_server(address: &str) -> Arc<Server> {
        let server = Arc::new(Server::new(address, ServerIpcConfig::default()).unwrap());
        server
            .register_route(CrmRoute {
                name: "grid".into(),
                scheduler: Arc::new(Scheduler::new(
                    ConcurrencyMode::ReadParallel,
                    HashMap::new(),
                )),
                callback: Arc::new(Echo),
                method_names: vec!["ping".into()],
            })
            .await;
        let run_server = server.clone();
        tokio::spawn(async move {
            let _ = run_server.run().await;
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let mut client = IpcClient::new(address);
                if client.connect().await.is_ok() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        server
    }

    #[tokio::test]
    async fn register_rejects_different_address_when_idle_evicted_owner_is_alive() {
        let state = test_state();
        let old_address = format!(
            "ipc://relay_old_owner_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let new_address = format!(
            "ipc://relay_new_owner_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let old_server = start_live_server(&old_address).await;
        let new_server = start_live_server(&new_address).await;

        assert_eq!(
            post_register(state.clone(), "grid", &old_address).await,
            StatusCode::CREATED
        );
        state.evict_connection("grid");

        assert_eq!(
            post_register(state.clone(), "grid", &new_address).await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            state.get_address("grid").as_deref(),
            Some(old_address.as_str())
        );

        old_server.shutdown();
        new_server.shutdown();
    }

    #[tokio::test]
    async fn register_allows_different_address_when_idle_evicted_owner_is_dead() {
        let state = test_state();
        let old_address = format!(
            "ipc://relay_dead_owner_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let new_address = format!(
            "ipc://relay_replacement_owner_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let old_server = start_live_server(&old_address).await;
        let new_server = start_live_server(&new_address).await;

        assert_eq!(
            post_register(state.clone(), "grid", &old_address).await,
            StatusCode::CREATED
        );
        state.evict_connection("grid");
        old_server.shutdown();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        assert_eq!(
            post_register(state.clone(), "grid", &new_address).await,
            StatusCode::CREATED
        );
        assert_eq!(
            state.get_address("grid").as_deref(),
            Some(new_address.as_str())
        );

        new_server.shutdown();
    }

    #[tokio::test]
    async fn concurrent_calls_after_idle_eviction_do_not_report_unreachable() {
        let state = test_state();
        let address = format!(
            "ipc://relay_concurrent_reconnect_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let server = start_live_server(&address).await;

        assert_eq!(
            post_register(state.clone(), "grid", &address).await,
            StatusCode::CREATED
        );
        state.evict_connection("grid");

        let app = build_router(state.clone());
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let app = app.clone();
            tasks.push(tokio::spawn(async move {
                let response = app
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri("/grid/ping")
                            .header("content-type", "application/octet-stream")
                            .body(Body::from(Vec::new()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                response.status()
            }));
        }

        let mut statuses = Vec::new();
        for task in tasks {
            statuses.push(task.await.unwrap());
        }

        assert!(
            statuses.iter().all(|status| *status == StatusCode::OK),
            "all concurrent requests should succeed after one task reconnects, got {statuses:?}"
        );

        server.shutdown();
    }

    #[tokio::test]
    async fn concurrent_register_different_addresses_keeps_single_owner() {
        let state = test_state();
        let first_address = format!(
            "ipc://relay_race_first_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let second_address = format!(
            "ipc://relay_race_second_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let first_server = start_live_server(&first_address).await;
        let second_server = start_live_server(&second_address).await;

        let first = {
            let state = state.clone();
            let first_address = first_address.clone();
            tokio::spawn(async move { post_register(state, "grid", &first_address).await })
        };
        let second = {
            let state = state.clone();
            let second_address = second_address.clone();
            tokio::spawn(async move { post_register(state, "grid", &second_address).await })
        };

        let statuses = vec![first.await.unwrap(), second.await.unwrap()];
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == StatusCode::CREATED)
                .count(),
            1,
            "exactly one registration should create the route, got {statuses:?}"
        );
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == StatusCode::CONFLICT)
                .count(),
            1,
            "exactly one registration should be rejected as duplicate, got {statuses:?}"
        );

        let final_address = state.get_address("grid").expect("route should exist");
        assert!(
            final_address == first_address || final_address == second_address,
            "final route owner should be one of the racing addresses, got {final_address}"
        );

        first_server.shutdown();
        second_server.shutdown();
    }

    fn unique_suffix() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
}
