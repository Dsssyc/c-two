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

use crate::relay::authority::{ControlError, RegisterPreparation, RouteAuthority};
use crate::relay::conn_pool::{AcquireError, UpstreamLease};
use crate::relay::gossip::{broadcast_route_announce, broadcast_route_withdraw};
use crate::relay::peer_handlers;
use crate::relay::state::{RegisterCommitResult, RelayState};
use c2_ipc::IpcClient;

const CONTROL_BODY_LIMIT_BYTES: usize = 64 * 1024;

enum RequestClient {
    Ready { lease: UpstreamLease },
    NotFound,
    Unreachable,
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
    let control_router = Router::new()
        .route("/_register", post(handle_register))
        .route("/_unregister", post(handle_unregister))
        .route("/_routes", get(handle_list_routes))
        .route("/_resolve/{*name}", get(handle_resolve))
        .route("/_probe/{*name}", get(handle_probe))
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
        .layer(DefaultBodyLimit::max(CONTROL_BODY_LIMIT_BYTES));

    let data_router = Router::new()
        .route("/_echo", post(echo_handler))
        .route("/{route_name}/{method_name}", post(call_handler))
        .layer(DefaultBodyLimit::disable());

    Router::new()
        .merge(control_router)
        .merge(data_router)
        .with_state(state)
}

// -- Control-plane handlers -----------------------------------------------

/// `POST /_register` — register a new upstream CRM.
///
/// Body: `{"name": "grid", "server_id": "...", "address": "ipc://...", "crm_ns": "...", "crm_ver": "..."}`
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
    let server_id = match body.get("server_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return (StatusCode::BAD_REQUEST, "Missing \"server_id\"").into_response(),
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

    let replacement = match RouteAuthority::new(&state)
        .prepare_register(&name, &server_id, &address)
        .await
    {
        Ok(RegisterPreparation::Available { replacement }) => {
            replacement.map(|r| crate::relay::state::OwnerReplacementToken {
                existing_address: r.existing_address,
                token: r.token,
            })
        }
        Ok(RegisterPreparation::SameOwner) => {
            return (
                StatusCode::OK,
                Json(serde_json::json!({"registered": name})),
            )
                .into_response();
        }
        Ok(RegisterPreparation::DuplicateAlive { existing_address })
        | Err(ControlError::AddressMismatch { existing_address })
        | Err(ControlError::DuplicateRoute { existing_address }) => {
            return duplicate_route_response(&name, &existing_address);
        }
        Err(ControlError::InvalidName { reason })
        | Err(ControlError::InvalidServerId { reason }) => {
            return (StatusCode::BAD_REQUEST, reason).into_response();
        }
        Err(ControlError::OwnerMismatch) | Err(ControlError::NotFound) => {
            return (StatusCode::CONFLICT, "Route owner is not replaceable").into_response();
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
        server_id,
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
        RegisterCommitResult::Duplicate { existing_address }
        | RegisterCommitResult::ConflictingOwner { existing_address } => {
            close_arc_client(client);
            return duplicate_route_response(&name, &existing_address);
        }
    };

    broadcast_route_announce(&state, &entry);

    (
        StatusCode::CREATED,
        Json(serde_json::json!({"registered": name})),
    )
        .into_response()
}

fn close_arc_client(arc_client: Arc<IpcClient>) {
    tokio::spawn(async move { arc_client.close_shared().await });
}

/// `POST /_unregister` — remove a CRM upstream.
///
/// Body: `{"name": "grid", "server_id": "..."}`
/// Returns: 200 on success, 403 on owner mismatch, 404 on missing.
async fn handle_unregister(
    State(state): State<Arc<RelayState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let name = match body.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return (StatusCode::BAD_REQUEST, "Missing \"name\"").into_response(),
    };
    let server_id = match body.get("server_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return (StatusCode::BAD_REQUEST, "Missing \"server_id\"").into_response(),
    };

    if let Err(ControlError::InvalidServerId { reason }) =
        RouteAuthority::new(&state).validate_server_id(&server_id)
    {
        return (StatusCode::BAD_REQUEST, reason).into_response();
    }

    match state.unregister_upstream(&name, &server_id) {
        crate::relay::state::UnregisterResult::Removed {
            entry,
            removed_at,
            client,
        } => {
            // Close old client asynchronously
            if let Some(arc_client) = client {
                close_arc_client(arc_client);
            }

            broadcast_route_withdraw(&state, &entry, removed_at);

            (
                StatusCode::OK,
                Json(serde_json::json!({"unregistered": name})),
            )
                .into_response()
        }
        crate::relay::state::UnregisterResult::AlreadyRemoved => (
            StatusCode::OK,
            Json(serde_json::json!({"unregistered": name})),
        )
            .into_response(),
        crate::relay::state::UnregisterResult::OwnerMismatch => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "OwnerMismatch",
                "name": name,
            })),
        )
            .into_response(),
        crate::relay::state::UnregisterResult::NotFound => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("Route name not registered: '{name}'")})),
        )
            .into_response(),
    }
}

/// `GET /_routes` — list all registered routes.
///
/// Intentionally omits `server_id` and `ipc_address` even for LOCAL routes:
/// this endpoint is reachable by anyone who can hit the relay's HTTP port, and
/// owner identity plus local UDS path are private to this relay.
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

/// `GET /_probe/{name}` — verify that a route's local upstream is reachable.
async fn handle_probe(
    Path(route_name): Path<String>,
    State(state): State<Arc<RelayState>>,
) -> Response {
    match acquire_request_client(state, &route_name).await {
        RequestClient::Ready { lease } => {
            drop(lease);
            StatusCode::OK.into_response()
        }
        RequestClient::NotFound => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "ResourceNotFound",
                "route": route_name,
            })),
        )
            .into_response(),
        RequestClient::Unreachable => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "error": "UpstreamUnavailable",
                "route": route_name,
            })),
        )
            .into_response(),
    }
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
                    "error": "ResourceNotFound",
                    "route": route_name,
                })),
            )
                .into_response();
        }
        RequestClient::Unreachable => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": "UpstreamUnavailable",
                    "route": route_name,
                })),
            )
                .into_response();
        }
    };
    let client = lease.client();

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
        Err(c2_ipc::IpcError::RouteNotFound(route)) => {
            let address = lease.address();
            drop(lease);
            remove_unreachable_route(&state, &route, &address);
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": "ResourceNotFound",
                    "route": route,
                })),
            )
                .into_response()
        }
        Err(e) => {
            // Evict dead client so next request triggers reconnect.
            if let Some(old_client) = lease.evict_current_client() {
                close_arc_client(old_client);
            }
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
        Err(AcquireError::Unreachable { address, error }) => {
            eprintln!("[relay] Failed to acquire upstream '{route_name}': {error}");
            remove_unreachable_route(&state, route_name, &address);
            match error {
                c2_ipc::IpcError::RouteNotFound(_) => RequestClient::NotFound,
                _ => RequestClient::Unreachable,
            }
        }
    }
}

fn remove_unreachable_route(state: &Arc<RelayState>, route_name: &str, address: &str) {
    if let Some((entry, removed_at, client)) =
        state.remove_unreachable_local_upstream(route_name, address)
    {
        if let Some(client) = client {
            close_arc_client(client);
        }
        broadcast_route_withdraw(state, &entry, removed_at);
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
pub(crate) mod tests {
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

    use crate::relay::peer::PeerEnvelope;

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

    pub(crate) fn test_state_for_client() -> Arc<RelayState> {
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

    fn test_state() -> Arc<RelayState> {
        test_state_for_client()
    }

    fn register_body(name: &str, server_id: &str, address: &str) -> Body {
        Body::from(
            serde_json::json!({
                "name": name,
                "server_id": server_id,
                "address": address,
            })
            .to_string(),
        )
    }

    async fn post_register(
        state: Arc<RelayState>,
        name: &str,
        server_id: &str,
        address: &str,
    ) -> StatusCode {
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_register")
                    .header("content-type", "application/json")
                    .body(register_body(name, server_id, address))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        status
    }

    async fn post_unregister(state: Arc<RelayState>, name: &str, server_id: &str) -> StatusCode {
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_unregister")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": name,
                            "server_id": server_id,
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        status
    }

    async fn post_call(state: Arc<RelayState>, name: &str, method: &str) -> StatusCode {
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/{name}/{method}"))
                    .body(Body::from(Vec::new()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        status
    }

    async fn get_probe(state: Arc<RelayState>, name: &str) -> StatusCode {
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/_probe/{name}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        status
    }

    async fn get_resolve(state: Arc<RelayState>, name: &str) -> StatusCode {
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/_resolve/{name}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        status
    }

    #[tokio::test]
    async fn register_rejects_invalid_server_id_at_control_boundary() {
        let state = test_state();
        assert_eq!(
            post_register(state.clone(), "grid", " ", "ipc://grid").await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            post_register(state, "grid", "bad/path", "ipc://grid").await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn register_rejects_route_name_that_cannot_fit_wire_control() {
        let state = test_state();
        let long_name = "x".repeat(c2_wire::control::MAX_CALL_ROUTE_NAME_BYTES + 1);

        assert_eq!(
            post_register(state, &long_name, "server-grid", "ipc://grid").await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn call_unreachable_upstream_removes_stale_local_route() {
        let state = test_state();

        assert_eq!(
            post_register(state.clone(), "grid", "server-grid", "ipc://missing-grid").await,
            StatusCode::BAD_GATEWAY
        );

        let stale_client = Arc::new(IpcClient::new("ipc://missing-grid"));
        stale_client.force_connected(false);
        state.commit_register_upstream(
            "grid".into(),
            "server-grid".into(),
            "ipc://missing-grid".into(),
            String::new(),
            String::new(),
            stale_client,
            None,
        );

        assert_eq!(
            post_call(state.clone(), "grid", "step").await,
            StatusCode::BAD_GATEWAY
        );
        assert!(state.resolve("grid").is_empty());
    }

    #[tokio::test]
    async fn unregister_rejects_invalid_server_id_at_control_boundary() {
        let state = test_state();
        assert_eq!(
            post_unregister(state.clone(), "grid", " ").await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            post_unregister(state, "grid", "bad\\path").await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn oversized_control_plane_body_is_rejected() {
        let state = test_state();
        let app = build_router(state);
        let oversized = serde_json::json!({
            "name": "grid",
            "server_id": "server-grid",
            "address": "ipc://grid",
            "padding": "x".repeat(2 * 1024 * 1024),
        })
        .to_string();

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_register")
                    .header("content-type", "application/json")
                    .body(Body::from(oversized))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn data_plane_echo_allows_large_payloads() {
        let state = test_state();
        let app = build_router(state);
        let payload = vec![b'x'; 2 * 1024 * 1024];

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_echo")
                    .header("content-type", "application/octet-stream")
                    .body(Body::from(payload.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.len(), payload.len());
    }

    async fn register_echo_route(server: &Arc<Server>, name: &str) {
        server
            .register_route(CrmRoute {
                name: name.into(),
                scheduler: Arc::new(Scheduler::new(
                    ConcurrencyMode::ReadParallel,
                    HashMap::new(),
                )),
                callback: Arc::new(Echo),
                method_names: vec!["ping".into()],
            })
            .await
            .unwrap();
    }

    async fn start_live_server(address: &str) -> Arc<Server> {
        let server = Arc::new(Server::new(address, ServerIpcConfig::default()).unwrap());
        register_echo_route(&server, "grid").await;
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
    async fn register_allows_replacement_when_idle_evicted_server_no_longer_serves_route() {
        let state = test_state();
        let old_address = format!(
            "ipc://relay_route_removed_owner_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let new_address = format!(
            "ipc://relay_route_replacement_owner_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let old_server = start_live_server(&old_address).await;
        register_echo_route(&old_server, "counter").await;
        let new_server = start_live_server(&new_address).await;

        assert_eq!(
            post_register(state.clone(), "grid", "server-old", &old_address).await,
            StatusCode::CREATED
        );
        state.evict_connection("grid");
        assert!(old_server.unregister_route("grid").await);

        assert_eq!(
            post_register(state.clone(), "grid", "server-new", &new_address).await,
            StatusCode::CREATED
        );
        assert_eq!(
            state.get_address("grid").as_deref(),
            Some(new_address.as_str())
        );

        old_server.shutdown();
        new_server.shutdown();
    }

    #[tokio::test]
    async fn probe_unreachable_upstream_removes_stale_local_route() {
        let state = test_state();
        let stale_address = format!(
            "ipc://relay_probe_stale_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let stale_server = start_live_server(&stale_address).await;

        assert_eq!(
            post_register(state.clone(), "grid", "server-grid", &stale_address).await,
            StatusCode::CREATED
        );
        state.evict_connection("grid");
        stale_server.shutdown();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        assert_eq!(
            get_probe(state.clone(), "grid").await,
            StatusCode::BAD_GATEWAY
        );
        assert!(state.local_route("grid").is_none());
        assert_eq!(
            get_probe(state.clone(), "grid").await,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn probe_missing_upstream_route_removes_stale_local_route() {
        let state = test_state();
        let stale_address = format!(
            "ipc://relay_probe_missing_route_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let stale_server = start_live_server(&stale_address).await;

        assert_eq!(
            post_register(state.clone(), "grid", "server-grid", &stale_address).await,
            StatusCode::CREATED
        );
        state.evict_connection("grid");
        assert!(stale_server.unregister_route("grid").await);

        assert_eq!(
            get_probe(state.clone(), "grid").await,
            StatusCode::NOT_FOUND
        );
        assert!(state.local_route("grid").is_none());
        assert_eq!(
            get_resolve(state.clone(), "grid").await,
            StatusCode::NOT_FOUND
        );

        stale_server.shutdown();
    }

    #[tokio::test]
    async fn call_missing_upstream_route_removes_stale_local_route() {
        let state = test_state();
        let stale_address = format!(
            "ipc://relay_call_missing_route_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let stale_server = start_live_server(&stale_address).await;

        assert_eq!(
            post_register(state.clone(), "grid", "server-grid", &stale_address).await,
            StatusCode::CREATED
        );
        assert!(stale_server.unregister_route("grid").await);

        assert_eq!(
            post_call(state.clone(), "grid", "ping").await,
            StatusCode::NOT_FOUND
        );
        assert!(state.local_route("grid").is_none());
        assert_eq!(
            get_resolve(state.clone(), "grid").await,
            StatusCode::NOT_FOUND
        );

        stale_server.shutdown();
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
            post_register(state.clone(), "grid", "server-old", &old_address).await,
            StatusCode::CREATED
        );
        state.evict_connection("grid");

        assert_eq!(
            post_register(state.clone(), "grid", "server-new", &new_address).await,
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
            post_register(state.clone(), "grid", "server-old", &old_address).await,
            StatusCode::CREATED
        );
        state.evict_connection("grid");
        old_server.shutdown();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        assert_eq!(
            post_register(state.clone(), "grid", "server-new", &new_address).await,
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
            post_register(state.clone(), "grid", "server-grid", &address).await,
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
            tokio::spawn(async move {
                post_register(state, "grid", "server-first", &first_address).await
            })
        };
        let second = {
            let state = state.clone();
            let second_address = second_address.clone();
            tokio::spawn(async move {
                post_register(state, "grid", "server-second", &second_address).await
            })
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

    #[tokio::test]
    async fn unregister_rejects_wrong_server_id_without_removing_route() {
        let state = test_state();
        let address = format!(
            "ipc://relay_unregister_owner_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let server = start_live_server(&address).await;

        assert_eq!(
            post_register(state.clone(), "grid", "server-grid", &address).await,
            StatusCode::CREATED
        );

        assert_eq!(
            post_unregister(state.clone(), "grid", "server-other").await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(state.get_address("grid").as_deref(), Some(address.as_str()));

        assert_eq!(
            post_unregister(state.clone(), "grid", "server-grid").await,
            StatusCode::OK
        );

        server.shutdown();
    }

    #[tokio::test]
    async fn unregister_is_idempotent_after_success_for_same_server_id() {
        let state = test_state();
        let address = format!(
            "ipc://relay_unregister_idempotent_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let server = start_live_server(&address).await;

        assert_eq!(
            post_register(state.clone(), "grid", "server-grid", &address).await,
            StatusCode::CREATED
        );
        assert_eq!(
            post_unregister(state.clone(), "grid", "server-grid").await,
            StatusCode::OK
        );
        assert_eq!(
            post_unregister(state.clone(), "grid", "server-grid").await,
            StatusCode::OK
        );

        server.shutdown();
    }

    #[tokio::test]
    async fn unregister_tombstone_does_not_authorize_different_server_id() {
        let state = test_state();
        let address = format!(
            "ipc://relay_unregister_wrong_idempotent_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let server = start_live_server(&address).await;

        assert_eq!(
            post_register(state.clone(), "grid", "server-grid", &address).await,
            StatusCode::CREATED
        );
        assert_eq!(
            post_unregister(state.clone(), "grid", "server-grid").await,
            StatusCode::OK
        );
        assert_eq!(
            post_unregister(state.clone(), "grid", "server-other").await,
            StatusCode::NOT_FOUND
        );

        server.shutdown();
    }

    fn unique_suffix() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
}
