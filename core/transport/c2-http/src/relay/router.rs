//! Axum router for the multi-upstream relay server.

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{ConnectInfo, DefaultBodyLimit, FromRequestParts, Path, Query, State},
    http::request::Parts,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};

use crate::relay::authority::{
    ControlError, RegisterPreparation, RouteAuthority, attest_ipc_route_contract,
    read_ipc_route_contract,
};
use crate::relay::conn_pool::UpstreamLease;
use crate::relay::gossip::{broadcast_route_announce, broadcast_route_withdraw};
use crate::relay::peer_handlers;
use crate::relay::route_table::valid_route_name;
use crate::relay::state::{RegisterCommitResult, RelayState, UpstreamAcquireError};
use crate::relay::types::RouteEntry;
use c2_ipc::IpcClient;

const CONTROL_BODY_LIMIT_BYTES: usize = 64 * 1024;
const EXPECTED_CRM_NS_HEADER: &str = "x-c2-expected-crm-ns";
const EXPECTED_CRM_NAME_HEADER: &str = "x-c2-expected-crm-name";
const EXPECTED_CRM_VER_HEADER: &str = "x-c2-expected-crm-ver";
const EXPECTED_ABI_HASH_HEADER: &str = "x-c2-expected-abi-hash";
const EXPECTED_SIGNATURE_HASH_HEADER: &str = "x-c2-expected-signature-hash";

enum RequestClient {
    Ready {
        lease: UpstreamLease,
        route: RouteEntry,
    },
    NotFound,
    Unreachable,
}

struct OptionalConnectInfo(Option<SocketAddr>);

impl<S> FromRequestParts<S> for OptionalConnectInfo
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let remote_addr = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(addr)| *addr);
        std::future::ready(Ok(Self(remote_addr)))
    }
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

#[derive(Debug, Default, serde::Deserialize)]
struct ResolveQuery {
    crm_ns: Option<String>,
    crm_name: Option<String>,
    crm_ver: Option<String>,
    abi_hash: Option<String>,
    signature_hash: Option<String>,
}

#[cfg(test)]
struct DataPlanePrecheckHook {
    route_name: String,
    action: Box<dyn FnOnce() + Send + 'static>,
}

#[cfg(test)]
static DATA_PLANE_AFTER_PRECHECK_HOOKS: std::sync::Mutex<Vec<DataPlanePrecheckHook>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn set_data_plane_after_precheck_hook(route_name: String, action: impl FnOnce() + Send + 'static) {
    DATA_PLANE_AFTER_PRECHECK_HOOKS
        .lock()
        .unwrap()
        .push(DataPlanePrecheckHook {
            route_name,
            action: Box::new(action),
        });
}

#[cfg(test)]
fn run_data_plane_after_precheck_hook(route_name: &str) {
    let hook = {
        let mut guard = DATA_PLANE_AFTER_PRECHECK_HOOKS.lock().unwrap();
        if let Some(index) = guard.iter().position(|hook| hook.route_name == route_name) {
            Some(guard.remove(index))
        } else {
            None
        }
    };
    if let Some(hook) = hook {
        (hook.action)();
    }
}

fn expected_crm_from_headers(
    route_name: &str,
    headers: &HeaderMap,
) -> Result<c2_contract::ExpectedRouteContract, Response> {
    let crm_ns = headers
        .get(EXPECTED_CRM_NS_HEADER)
        .map(|value| value.to_str().map(str::to_string));
    let crm_name = headers
        .get(EXPECTED_CRM_NAME_HEADER)
        .map(|value| value.to_str().map(str::to_string));
    let crm_ver = headers
        .get(EXPECTED_CRM_VER_HEADER)
        .map(|value| value.to_str().map(str::to_string));
    let abi_hash = headers
        .get(EXPECTED_ABI_HASH_HEADER)
        .map(|value| value.to_str().map(str::to_string));
    let signature_hash = headers
        .get(EXPECTED_SIGNATURE_HASH_HEADER)
        .map(|value| value.to_str().map(str::to_string));

    match (crm_ns, crm_name, crm_ver, abi_hash, signature_hash) {
        (None, None, None, None, None) => Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "InvalidCrmTag",
                "message": "expected CRM headers and hash headers are required",
            })),
        )
            .into_response()),
        (
            Some(Ok(crm_ns)),
            Some(Ok(crm_name)),
            Some(Ok(crm_ver)),
            Some(Ok(abi_hash)),
            Some(Ok(signature_hash)),
        ) => {
            let expected = c2_contract::ExpectedRouteContract {
                route_name: route_name.to_string(),
                crm_ns,
                crm_name,
                crm_ver,
                abi_hash,
                signature_hash,
            };
            if let Err(err) = c2_contract::validate_expected_route_contract(&expected) {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": "InvalidCrmTag",
                        "message": err.to_string(),
                    })),
                )
                    .into_response());
            }
            Ok(expected)
        }
        (Some(Err(_)), _, _, _, _)
        | (_, Some(Err(_)), _, _, _)
        | (_, _, Some(Err(_)), _, _)
        | (_, _, _, Some(Err(_)), _)
        | (_, _, _, _, Some(Err(_))) => Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "InvalidCrmTag",
                "message": "expected CRM headers must be valid UTF-8",
            })),
        )
            .into_response()),
        _ => Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "InvalidCrmTag",
                "message": "expected CRM headers and hash headers must be supplied together",
            })),
        )
            .into_response()),
    }
}

fn crm_contract_mismatch_response(route_name: &str) -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "error": "CRMContractMismatch",
            "route": route_name,
            "message": format!("CRM contract mismatch for route {route_name}"),
        })),
    )
        .into_response()
}

fn route_matches_expected_crm(
    route: &RouteEntry,
    expected: &c2_contract::ExpectedRouteContract,
) -> bool {
    route.name == expected.route_name
        && route.crm_ns == expected.crm_ns
        && route.crm_name == expected.crm_name
        && route.crm_ver == expected.crm_ver
        && route.abi_hash == expected.abi_hash
        && route.signature_hash == expected.signature_hash
}

fn register_contract_claim_from_body(
    route_name: &str,
    body: &serde_json::Value,
) -> Result<Option<c2_contract::ExpectedRouteContract>, Response> {
    let crm_ns = register_body_string_field(body, "crm_ns")?;
    let crm_name = register_body_string_field(body, "crm_name")?;
    let crm_ver = register_body_string_field(body, "crm_ver")?;
    let abi_hash = register_body_string_field(body, "abi_hash")?;
    let signature_hash = register_body_string_field(body, "signature_hash")?;

    match (crm_ns, crm_name, crm_ver, abi_hash, signature_hash) {
        (None, None, None, None, None) => Ok(None),
        (Some(crm_ns), Some(crm_name), Some(crm_ver), Some(abi_hash), Some(signature_hash)) => {
            let claimed = c2_contract::ExpectedRouteContract {
                route_name: route_name.to_string(),
                crm_ns: crm_ns.to_string(),
                crm_name: crm_name.to_string(),
                crm_ver: crm_ver.to_string(),
                abi_hash: abi_hash.to_string(),
                signature_hash: signature_hash.to_string(),
            };
            if let Err(err) = c2_contract::validate_expected_route_contract(&claimed) {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": "InvalidCrmTag",
                        "message": err.to_string(),
                    })),
                )
                    .into_response());
            }
            Ok(Some(claimed))
        }
        _ => Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "InvalidCrmTag",
                "message": "crm_ns, crm_name, crm_ver, abi_hash, and signature_hash must be supplied together",
            })),
        )
            .into_response()),
    }
}

fn register_body_string_field<'a>(
    body: &'a serde_json::Value,
    field: &'static str,
) -> Result<Option<&'a str>, Response> {
    match body.get(field) {
        Some(value) => value.as_str().map(Some).ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "InvalidCrmTag",
                    "message": format!("{field} must be a string"),
                })),
            )
                .into_response()
        }),
        None => Ok(None),
    }
}

fn validate_expected_crm_for_route(
    state: &RelayState,
    route_name: &str,
    expected: &c2_contract::ExpectedRouteContract,
) -> Result<(), Response> {
    let Some(route) = state.local_route(route_name) else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "ResourceNotFound",
                "route": route_name,
            })),
        )
            .into_response());
    };
    if route_matches_expected_crm(&route, expected) {
        Ok(())
    } else {
        Err(crm_contract_mismatch_response(route_name))
    }
}

fn validate_expected_crm_for_acquired_route(
    route_name: &str,
    route: &RouteEntry,
    expected: &c2_contract::ExpectedRouteContract,
) -> Result<(), Response> {
    if route_matches_expected_crm(route, expected) {
        Ok(())
    } else {
        Err(crm_contract_mismatch_response(route_name))
    }
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
/// Body: `{"name": "grid", "server_id": "...", "server_instance_id": "...", "address": "ipc://...", "crm_ns": "...", "crm_name": "...", "crm_ver": "...", "abi_hash": "...", "signature_hash": "..."}`
/// The CRM contract is attested against the upstream IPC handshake. Optional
/// claimed contract fields, when present, must match that handshake exactly.
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
    let server_instance_id = match body.get("server_instance_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => {
            return (StatusCode::BAD_REQUEST, "Missing \"server_instance_id\"").into_response();
        }
    };
    let claimed_contract = match register_contract_claim_from_body(&name, &body) {
        Ok(claim) => claim,
        Err(response) => return response,
    };

    if let Err(ControlError::InvalidServerInstanceId { reason }) =
        RouteAuthority::new(&state).validate_server_instance_id(&server_instance_id)
    {
        return (StatusCode::BAD_REQUEST, reason).into_response();
    }

    let replacement = match RouteAuthority::new(&state)
        .prepare_register(&name, &server_id, &server_instance_id, &address)
        .await
    {
        Ok(RegisterPreparation::Available { replacement }) => {
            replacement.map(|r| crate::relay::state::OwnerReplacementToken {
                route_name: r.route_name,
                server_id: r.server_id,
                server_instance_id: r.server_instance_id,
                ipc_address: r.ipc_address,
                existing_address: r.existing_address,
                token: r.token,
                evidence: r.evidence,
            })
        }
        Ok(RegisterPreparation::SameOwner) => None,
        Ok(RegisterPreparation::DuplicateAlive { existing_address })
        | Err(ControlError::AddressMismatch { existing_address })
        | Err(ControlError::DuplicateRoute { existing_address }) => {
            return duplicate_route_response(&name, &existing_address);
        }
        Err(ControlError::InvalidName { reason })
        | Err(ControlError::InvalidServerId { reason })
        | Err(ControlError::InvalidServerInstanceId { reason })
        | Err(ControlError::InvalidAddress { reason })
        | Err(ControlError::ContractMismatch { reason }) => {
            return (StatusCode::BAD_REQUEST, reason).into_response();
        }
        Err(ControlError::OwnerMismatch) | Err(ControlError::NotFound) => {
            return (StatusCode::CONFLICT, "Route owner is not replaceable").into_response();
        }
    };

    // Connect IPC client and attest the registered route contract.
    let (client, crm_ns, crm_name, crm_ver, abi_hash, signature_hash) = {
        let mut c = IpcClient::new(&address);
        if let Err(e) = c.connect().await {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": format!("Failed to connect upstream '{name}' at {address}: {e}")})),
            ).into_response();
        }
        let identity_matches = c.server_id() == Some(server_id.as_str())
            && c.server_instance_id() == Some(server_instance_id.as_str());
        if !identity_matches {
            close_client(c);
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": format!("IPC server identity mismatch for upstream '{name}' at {address}"),
                })),
            )
                .into_response();
        }
        if !c.has_route(&name) {
            close_client(c);
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("IPC upstream at {address} does not export route '{name}'"),
                })),
            )
                .into_response();
        }
        let contract = match claimed_contract.as_ref() {
            Some(claimed_contract) => match attest_ipc_route_contract(
                &c,
                &name,
                &claimed_contract.crm_ns,
                &claimed_contract.crm_name,
                &claimed_contract.crm_ver,
                &claimed_contract.abi_hash,
                &claimed_contract.signature_hash,
            ) {
                Ok(contract) => contract,
                Err(ControlError::ContractMismatch { reason }) => {
                    close_client(c);
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({ "error": reason })),
                    )
                        .into_response();
                }
                Err(ControlError::NotFound) => {
                    close_client(c);
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({
                            "error": format!("IPC upstream at {address} does not export route '{name}'"),
                        })),
                    )
                        .into_response();
                }
                Err(_) => unreachable!("route contract attestation returns only contract errors"),
            },
            None => match read_ipc_route_contract(&c, &name) {
                Ok(contract) => contract,
                Err(ControlError::NotFound) => {
                    close_client(c);
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({
                            "error": format!("IPC upstream at {address} does not export route '{name}'"),
                        })),
                    )
                        .into_response();
                }
                Err(ControlError::ContractMismatch { reason }) => {
                    close_client(c);
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({ "error": reason })),
                    )
                        .into_response();
                }
                Err(_) => unreachable!("route contract read returns only contract errors"),
            },
        };
        (
            Arc::new(c),
            contract.crm_ns,
            contract.crm_name,
            contract.crm_ver,
            contract.abi_hash,
            contract.signature_hash,
        )
    };

    let commit_client = client.clone();
    let entry = match state.commit_register_upstream(
        name.clone(),
        server_id,
        server_instance_id,
        address,
        crm_ns,
        crm_name,
        crm_ver,
        abi_hash,
        signature_hash,
        commit_client,
        replacement,
    ) {
        RegisterCommitResult::Registered { entry } => entry,
        RegisterCommitResult::SameOwner { entry } => {
            close_arc_client(client);
            return (
                StatusCode::OK,
                Json(serde_json::json!({"registered": entry.name})),
            )
                .into_response();
        }
        RegisterCommitResult::Duplicate { existing_address }
        | RegisterCommitResult::ConflictingOwner { existing_address } => {
            close_arc_client(client);
            return duplicate_route_response(&name, &existing_address);
        }
        RegisterCommitResult::Invalid { reason } => {
            close_arc_client(client);
            return (StatusCode::BAD_REQUEST, reason).into_response();
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

fn close_client(client: IpcClient) {
    tokio::spawn(async move {
        let mut client = client;
        client.close().await;
    });
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
                "crm_name": r.crm_name,
                "crm_ver": r.crm_ver,
                "abi_hash": r.abi_hash,
                "signature_hash": r.signature_hash,
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

/// `GET /_resolve/{name}` — resolve a complete expected CRM route contract.
async fn handle_resolve(
    Path(name): Path<String>,
    Query(query): Query<ResolveQuery>,
    State(state): State<Arc<RelayState>>,
    OptionalConnectInfo(remote_addr): OptionalConnectInfo,
) -> impl IntoResponse {
    if !valid_route_name(&name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "InvalidRouteName",
                "message": "route name must be non-empty, control-character-free, and fit the wire route-name limit",
            })),
        )
            .into_response();
    }
    let expose_ipc_address = remote_addr.is_some_and(|addr| addr.ip().is_loopback());
    let expected_crm = match (
        &query.crm_ns,
        &query.crm_name,
        &query.crm_ver,
        &query.abi_hash,
        &query.signature_hash,
    ) {
        (Some(crm_ns), Some(crm_name), Some(crm_ver), Some(abi_hash), Some(signature_hash)) => {
            let expected = c2_contract::ExpectedRouteContract {
                route_name: name.clone(),
                crm_ns: crm_ns.clone(),
                crm_name: crm_name.clone(),
                crm_ver: crm_ver.clone(),
                abi_hash: abi_hash.clone(),
                signature_hash: signature_hash.clone(),
            };
            if let Err(err) = c2_contract::validate_expected_route_contract(&expected) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": "InvalidCrmTag",
                        "message": err.to_string(),
                    })),
                )
                    .into_response();
            }
            expected
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "InvalidCrmTag",
                    "message": "crm_ns, crm_name, crm_ver, abi_hash, and signature_hash are required for relay resolve",
                })),
            )
                .into_response();
        }
    };
    let mut routes = state.resolve_matching(&expected_crm);
    if routes.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "ResourceNotFound", "name": name,
            })),
        )
            .into_response();
    }
    if !expose_ipc_address {
        for route in &mut routes {
            route.ipc_address = None;
            route.server_id = None;
            route.server_instance_id = None;
        }
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
    headers: HeaderMap,
) -> Response {
    let expected_crm = match expected_crm_from_headers(&route_name, &headers) {
        Ok(expected) => expected,
        Err(response) => return response,
    };
    if let Err(response) = validate_expected_crm_for_route(&state, &route_name, &expected_crm) {
        return response;
    }
    #[cfg(test)]
    run_data_plane_after_precheck_hook(&route_name);
    match acquire_request_client(state, &route_name).await {
        RequestClient::Ready { lease, route } => {
            if let Err(response) =
                validate_expected_crm_for_acquired_route(&route_name, &route, &expected_crm)
            {
                drop(lease);
                return response;
            }
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
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let expected_crm = match expected_crm_from_headers(&route_name, &headers) {
        Ok(expected) => expected,
        Err(response) => return response,
    };
    if let Err(response) = validate_expected_crm_for_route(&state, &route_name, &expected_crm) {
        return response;
    }
    #[cfg(test)]
    run_data_plane_after_precheck_hook(&route_name);
    let (lease, acquired_route) = match acquire_request_client(state.clone(), &route_name).await {
        RequestClient::Ready { lease, route } => (lease, route),
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
    if let Err(response) =
        validate_expected_crm_for_acquired_route(&route_name, &acquired_route, &expected_crm)
    {
        drop(lease);
        return response;
    }
    let client = lease.client();

    match client.call(&route_name, &method_name, &body).await {
        Ok(result) => materialized_response_or_error(
            &route_name,
            result.into_bytes_with_pool(client.server_pool_arc(), &client.reassembly_pool_arc()),
        ),
        Err(c2_ipc::IpcError::CrmError(err_bytes)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [("content-type", "application/octet-stream")],
            err_bytes,
        )
            .into_response(),
        Err(c2_ipc::IpcError::RouteNotFound(route)) => {
            drop(lease);
            remove_unreachable_route(&state, &acquired_route);
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

fn materialized_response_or_error(route_name: &str, result: Result<Vec<u8>, String>) -> Response {
    match result {
        Ok(bytes) => (
            StatusCode::OK,
            [("content-type", "application/octet-stream")],
            bytes,
        )
            .into_response(),
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "error": "UpstreamResponseUnavailable",
                "route": route_name,
                "message": format!("failed to materialize upstream response: {err}"),
            })),
        )
            .into_response(),
    }
}

async fn acquire_request_client(state: Arc<RelayState>, route_name: &str) -> RequestClient {
    match state.acquire_upstream(route_name).await {
        Ok((lease, route)) => RequestClient::Ready { lease, route },
        Err(UpstreamAcquireError::NotFound) => RequestClient::NotFound,
        Err(UpstreamAcquireError::Unreachable {
            route,
            address,
            error,
        }) => {
            eprintln!("[relay] Failed to acquire upstream '{route_name}' at {address}: {error}");
            remove_unreachable_route(&state, &route);
            match error {
                c2_ipc::IpcError::RouteNotFound(_) => RequestClient::NotFound,
                _ => RequestClient::Unreachable,
            }
        }
    }
}

fn remove_unreachable_route(state: &Arc<RelayState>, route: &RouteEntry) {
    if let Some((entry, removed_at, client)) =
        state.remove_unreachable_local_upstream_if_matches(route)
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
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::extract::ConnectInfo;
    use axum::http::{Request, StatusCode};
    use c2_ipc::IpcClient;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use tower::ServiceExt;

    use crate::relay::test_support::{
        register_echo_route, start_live_server, start_live_server_with_identity_and_contracts,
        start_live_server_with_routes, test_state_for_client,
    };
    use crate::relay::types::RouteInfo;

    const TEST_CRM_NS: &str = "test.relay";
    const TEST_CRM_NAME: &str = "RelayGrid";
    const TEST_CRM_VER: &str = "0.1.0";
    const TEST_ABI_HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const TEST_SIGNATURE_HASH: &str =
        "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    #[tokio::test]
    async fn materialized_response_error_is_not_silently_returned_as_empty_success() {
        let response =
            materialized_response_or_error("grid", Err("server pool not initialised".to_string()));

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["error"], "UpstreamResponseUnavailable");
        assert_eq!(payload["route"], "grid");
    }

    fn test_state() -> Arc<RelayState> {
        test_state_for_client()
    }

    fn register_body(name: &str, server_id: &str, address: &str) -> Body {
        Body::from(
            serde_json::json!({
                "name": name,
                "server_id": server_id,
                "server_instance_id": format!("{server_id}-instance"),
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

    async fn post_register_json(state: Arc<RelayState>, body: serde_json::Value) -> StatusCode {
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_register")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        status
    }

    async fn post_register_with_crm(
        state: Arc<RelayState>,
        name: &str,
        server_id: &str,
        address: &str,
        crm_ns: &str,
        crm_name: &str,
        crm_ver: &str,
    ) -> StatusCode {
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/_register")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": name,
                            "server_id": server_id,
                            "server_instance_id": format!("{server_id}-instance"),
                            "address": address,
                            "crm_ns": crm_ns,
                            "crm_name": crm_name,
                            "crm_ver": crm_ver,
                            "abi_hash": TEST_ABI_HASH,
                            "signature_hash": TEST_SIGNATURE_HASH,
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

    #[tokio::test]
    async fn register_rejects_incomplete_claimed_crm_tag_before_ipc_connect() {
        let state = Arc::new(RelayState::new(
            Arc::new(c2_config::RelayConfig {
                relay_id: "test-relay".into(),
                ..Default::default()
            }),
            Arc::new(crate::relay::test_support::NoopDisseminator),
        ));

        let status = post_register_with_crm(
            state.clone(),
            "grid",
            "server-grid",
            "ipc://grid",
            "test.mesh",
            "",
            "0.1.0",
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(state.resolve("grid").is_empty());
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
                    .header("content-type", "application/octet-stream")
                    .header("x-c2-expected-crm-ns", "test.echo")
                    .header("x-c2-expected-crm-name", "Echo")
                    .header("x-c2-expected-crm-ver", "0.1.0")
                    .header("x-c2-expected-abi-hash", TEST_ABI_HASH)
                    .header("x-c2-expected-signature-hash", TEST_SIGNATURE_HASH)
                    .body(Body::from(Vec::new()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        status
    }

    async fn post_call_with_expected_crm_tag(
        state: Arc<RelayState>,
        name: &str,
        method: &str,
        crm_ns: &str,
        crm_name: &str,
        crm_ver: &str,
    ) -> StatusCode {
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/{name}/{method}"))
                    .header("content-type", "application/octet-stream")
                    .header("x-c2-expected-crm-ns", crm_ns)
                    .header("x-c2-expected-crm-name", crm_name)
                    .header("x-c2-expected-crm-ver", crm_ver)
                    .header("x-c2-expected-abi-hash", TEST_ABI_HASH)
                    .header("x-c2-expected-signature-hash", TEST_SIGNATURE_HASH)
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
                    .header("x-c2-expected-crm-ns", "test.echo")
                    .header("x-c2-expected-crm-name", "Echo")
                    .header("x-c2-expected-crm-ver", "0.1.0")
                    .header("x-c2-expected-abi-hash", TEST_ABI_HASH)
                    .header("x-c2-expected-signature-hash", TEST_SIGNATURE_HASH)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        status
    }

    async fn get_probe_with_expected_crm_tag(
        state: Arc<RelayState>,
        name: &str,
        crm_ns: &str,
        crm_name: &str,
        crm_ver: &str,
    ) -> StatusCode {
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/_probe/{name}"))
                    .header("x-c2-expected-crm-ns", crm_ns)
                    .header("x-c2-expected-crm-name", crm_name)
                    .header("x-c2-expected-crm-ver", crm_ver)
                    .header("x-c2-expected-abi-hash", TEST_ABI_HASH)
                    .header("x-c2-expected-signature-hash", TEST_SIGNATURE_HASH)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        status
    }

    async fn get_resolve_without_query(state: Arc<RelayState>, name: &str) -> StatusCode {
        let app = build_router(state);
        let mut request = Request::builder()
            .method("GET")
            .uri(format!("/_resolve/{name}"))
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(ConnectInfo(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            12345,
        )));
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        status
    }

    async fn get_resolve_with_crm_tag(
        state: Arc<RelayState>,
        name: &str,
        crm_ns: &str,
        crm_name: &str,
        crm_ver: &str,
    ) -> StatusCode {
        let app = build_router(state);
        let mut request = Request::builder()
            .method("GET")
            .uri(format!(
                "/_resolve/{name}?crm_ns={crm_ns}&crm_name={crm_name}&crm_ver={crm_ver}&abi_hash={TEST_ABI_HASH}&signature_hash={TEST_SIGNATURE_HASH}"
            ))
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(ConnectInfo(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            12345,
        )));
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        status
    }

    async fn get_resolve_with_query(state: Arc<RelayState>, name: &str, query: &str) -> StatusCode {
        let app = build_router(state);
        let mut request = Request::builder()
            .method("GET")
            .uri(format!("/_resolve/{name}?{query}"))
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(ConnectInfo(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            12345,
        )));
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        status
    }

    async fn get_resolve_uri(state: Arc<RelayState>, uri: &str) -> StatusCode {
        let app = build_router(state);
        let mut request = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(ConnectInfo(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            12345,
        )));
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        status
    }

    #[tokio::test]
    async fn resolve_requires_expected_contract_query() {
        let state = test_state();

        assert_eq!(
            get_resolve_without_query(state, "grid").await,
            StatusCode::BAD_REQUEST,
        );
    }

    #[tokio::test]
    async fn resolve_rejects_partial_crm_tag_query_instead_of_downgrading_to_name_only() {
        let state = test_state();

        assert_eq!(
            get_resolve_with_query(state, "grid", "crm_ns=test.echo&crm_ver=0.1.0").await,
            StatusCode::BAD_REQUEST,
        );
    }

    #[tokio::test]
    async fn resolve_rejects_control_character_route_name() {
        let state = test_state();

        assert_eq!(
            get_resolve_uri(state, "/_resolve/grid%00hidden").await,
            StatusCode::BAD_REQUEST,
        );
    }

    #[tokio::test]
    async fn resolve_rejects_control_character_crm_tag_query() {
        let state = test_state();

        assert_eq!(
            get_resolve_with_query(
                state,
                "grid",
                "crm_ns=test.echo&crm_name=Echo%00Hidden&crm_ver=0.1.0",
            )
            .await,
            StatusCode::BAD_REQUEST,
        );
    }

    #[tokio::test]
    async fn resolve_with_expected_contract_hides_name_match_with_wrong_crm_name() {
        let state = test_state();
        let address = format!(
            "ipc://relay_resolve_crm_name_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let server = start_live_server(&address, "server-grid").await;

        assert_eq!(
            post_register(state.clone(), "grid", "server-grid", &address).await,
            StatusCode::CREATED
        );
        assert_eq!(
            get_resolve_with_crm_tag(state.clone(), "grid", "test.echo", "OtherEcho", "0.1.0",)
                .await,
            StatusCode::NOT_FOUND,
        );
        assert_eq!(
            get_resolve_with_crm_tag(state.clone(), "grid", "test.echo", "Echo", "0.1.0").await,
            StatusCode::OK,
        );

        server.shutdown();
    }

    #[tokio::test]
    async fn resolve_with_expected_contract_hides_name_match_with_wrong_hash() {
        let state = test_state();
        let address = format!(
            "ipc://relay_resolve_hash_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let server = start_live_server(&address, "server-grid").await;

        assert_eq!(
            post_register(state.clone(), "grid", "server-grid", &address).await,
            StatusCode::CREATED
        );
        assert_eq!(
            get_resolve_with_query(
                state.clone(),
                "grid",
                "crm_ns=test.echo&crm_name=Echo&crm_ver=0.1.0&abi_hash=fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210&signature_hash=abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
            )
            .await,
            StatusCode::NOT_FOUND,
        );
        assert_eq!(
            get_resolve_with_crm_tag(state.clone(), "grid", "test.echo", "Echo", "0.1.0").await,
            StatusCode::OK,
        );

        server.shutdown();
    }

    #[tokio::test]
    async fn relay_data_plane_requires_expected_contract_headers() {
        let state = test_state();
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/grid/ping")
                    .header("content-type", "application/octet-stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body.contains("expected CRM headers and hash headers are required"),
            "unexpected body: {body}"
        );
    }

    #[tokio::test]
    async fn relay_probe_requires_expected_contract_headers() {
        let state = test_state();
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/_probe/grid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body.contains("expected CRM headers and hash headers are required"),
            "unexpected body: {body}"
        );
    }

    #[tokio::test]
    async fn data_plane_rejects_partial_expected_hash_headers_before_acquire() {
        let state = test_state();
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/grid/ping")
                    .header("content-type", "application/octet-stream")
                    .header("x-c2-expected-crm-ns", "test.echo")
                    .header("x-c2-expected-crm-name", "Echo")
                    .header("x-c2-expected-crm-ver", "0.1.0")
                    .header("x-c2-expected-abi-hash", TEST_ABI_HASH)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body.contains("expected CRM headers and hash headers must be supplied together"),
            "unexpected body: {body}"
        );
    }

    async fn get_resolve_routes_from(
        state: Arc<RelayState>,
        name: &str,
        remote_addr: SocketAddr,
    ) -> (StatusCode, Vec<RouteInfo>) {
        let app = build_router(state);
        let mut request = Request::builder()
            .method("GET")
            .uri(format!(
                "/_resolve/{name}?crm_ns={TEST_CRM_NS}&crm_name={TEST_CRM_NAME}&crm_ver={TEST_CRM_VER}&abi_hash={TEST_ABI_HASH}&signature_hash={TEST_SIGNATURE_HASH}"
            ))
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(ConnectInfo(remote_addr));
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let routes = if status == StatusCode::OK {
            serde_json::from_slice(&body).unwrap()
        } else {
            Vec::new()
        };
        (status, routes)
    }

    #[tokio::test]
    async fn resolve_exposes_ipc_address_only_to_loopback_clients() {
        let state = test_state();
        state.commit_register_upstream(
            "grid".into(),
            "server-grid".into(),
            "inst-grid".into(),
            "ipc://grid".into(),
            TEST_CRM_NS.to_string(),
            TEST_CRM_NAME.to_string(),
            TEST_CRM_VER.to_string(),
            TEST_ABI_HASH.to_string(),
            TEST_SIGNATURE_HASH.to_string(),
            Arc::new(IpcClient::new("ipc://grid")),
            None,
        );

        let (status, routes) = get_resolve_routes_from(
            state.clone(),
            "grid",
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 12345),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(routes[0].ipc_address.as_deref(), Some("ipc://grid"));
        assert_eq!(routes[0].server_id.as_deref(), Some("server-grid"));
        assert_eq!(routes[0].server_instance_id.as_deref(), Some("inst-grid"));

        let (status, routes) = get_resolve_routes_from(
            state,
            "grid",
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)), 12345),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(routes[0].ipc_address, None);
        assert_eq!(routes[0].server_id, None);
        assert_eq!(routes[0].server_instance_id, None);
    }

    #[tokio::test]
    async fn register_rejects_invalid_server_id_at_control_boundary() {
        let state = test_state();
        let too_long = "s".repeat(c2_contract::MAX_WIRE_TEXT_BYTES + 1);
        assert_eq!(
            post_register(state.clone(), "grid", " ", "ipc://grid").await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            post_register(state.clone(), "grid", "bad/path", "ipc://grid").await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            post_register(state, "grid", &too_long, "ipc://grid").await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn register_rejects_invalid_server_instance_id_at_control_boundary() {
        let state = test_state();
        let too_long = "a".repeat(c2_contract::MAX_WIRE_TEXT_BYTES + 1);
        for server_instance_id in ["../bad", too_long.as_str()] {
            let app = build_router(state.clone());
            let response = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/_register")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            serde_json::json!({
                                "name": "grid",
                                "server_id": "server-grid",
                                "server_instance_id": server_instance_id,
                                "address": "ipc://grid",
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();

            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(String::from_utf8_lossy(&body).contains("server_instance_id"));
        }
    }

    #[tokio::test]
    async fn register_rejects_ipc_handshake_identity_mismatch() {
        let state = test_state();
        let address = format!(
            "ipc://relay_identity_mismatch_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let server = start_live_server(&address, "server-actual").await;

        assert_eq!(
            post_register(state, "grid", "server-claimed", &address).await,
            StatusCode::BAD_GATEWAY
        );

        server.shutdown();
    }

    #[tokio::test]
    async fn register_rejects_upstream_that_does_not_export_route() {
        let state = test_state();
        let address = format!(
            "ipc://relay_missing_route_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let server = start_live_server_with_routes(&address, "server-grid", &["counter"]).await;

        assert_eq!(
            post_register(state.clone(), "grid", "server-grid", &address).await,
            StatusCode::BAD_REQUEST
        );
        assert!(
            state.resolve("grid").is_empty(),
            "relay must not advertise a route that the upstream handshake did not export"
        );

        server.shutdown();
    }

    #[tokio::test]
    async fn register_derives_crm_contract_from_ipc_handshake() {
        let state = test_state();
        let address = format!(
            "ipc://relay_register_contract_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let server = start_live_server(&address, "server-grid").await;

        assert_eq!(
            post_register(state.clone(), "grid", "server-grid", &address).await,
            StatusCode::CREATED
        );

        let routes = state.resolve("grid");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].crm_ns, "test.echo");
        assert_eq!(routes[0].crm_ver, "0.1.0");

        server.shutdown();
    }

    #[tokio::test]
    async fn register_rejects_non_string_contract_claim_fields() {
        let state = test_state();
        let address = format!(
            "ipc://relay_register_non_string_contract_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let server = start_live_server(&address, "server-grid").await;

        let status = post_register_json(
            state.clone(),
            serde_json::json!({
                "name": "grid",
                "server_id": "server-grid",
                "server_instance_id": "server-grid-instance",
                "address": address,
                "crm_ns": 1,
                "crm_name": true,
                "crm_ver": ["0.1.0"],
                "abi_hash": {"value": TEST_ABI_HASH},
                "signature_hash": null,
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            state.resolve("grid").is_empty(),
            "relay must not silently ignore malformed claimed CRM contract fields"
        );

        server.shutdown();
    }

    #[tokio::test]
    async fn register_rejects_claimed_crm_contract_mismatch() {
        let state = test_state();
        let address = format!(
            "ipc://relay_register_contract_mismatch_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let server = start_live_server(&address, "server-grid").await;

        assert_eq!(
            post_register_with_crm(
                state.clone(),
                "grid",
                "server-grid",
                &address,
                "wrong.grid",
                "WrongGrid",
                "9.9.9",
            )
            .await,
            StatusCode::BAD_REQUEST
        );
        assert!(
            state.resolve("grid").is_empty(),
            "relay must not advertise a route with caller-claimed CRM metadata that disagrees with the IPC handshake"
        );

        server.shutdown();
    }

    #[tokio::test]
    async fn register_same_owner_rejects_claimed_crm_contract_mismatch() {
        let state = test_state();
        let address = format!(
            "ipc://relay_register_same_owner_contract_mismatch_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let server = start_live_server(&address, "server-grid").await;

        assert_eq!(
            post_register(state.clone(), "grid", "server-grid", &address).await,
            StatusCode::CREATED
        );
        assert_eq!(
            post_register_with_crm(
                state.clone(),
                "grid",
                "server-grid",
                &address,
                "wrong.grid",
                "WrongGrid",
                "9.9.9",
            )
            .await,
            StatusCode::BAD_REQUEST
        );

        let routes = state.resolve("grid");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].crm_ns, "test.echo");
        assert_eq!(routes[0].crm_ver, "0.1.0");

        server.shutdown();
    }

    #[tokio::test]
    async fn register_same_owner_without_claim_reattests_ipc_contract() {
        let state = test_state();
        let address = format!(
            "ipc://relay_register_same_owner_unclaimed_contract_mismatch_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let original_client = Arc::new(IpcClient::new(&address));
        original_client.force_connected(true);
        match state.commit_register_upstream(
            "grid".into(),
            "server-grid".into(),
            "server-grid-instance".into(),
            address.clone(),
            "test.echo".into(),
            "Echo".into(),
            "0.1.0".into(),
            TEST_ABI_HASH.to_string(),
            TEST_SIGNATURE_HASH.to_string(),
            original_client,
            None,
        ) {
            RegisterCommitResult::Registered { .. } => {}
            _ => panic!("unexpected initial registration result"),
        }
        let server = start_live_server_with_identity_and_contracts(
            &address,
            "server-grid",
            "server-grid-instance",
            &[(
                "grid",
                "test.other",
                "OtherGrid",
                "0.1.0",
                TEST_ABI_HASH,
                TEST_SIGNATURE_HASH,
            )],
        )
        .await;

        assert_eq!(
            post_register(state.clone(), "grid", "server-grid", &address).await,
            StatusCode::BAD_REQUEST
        );
        let routes = state.resolve("grid");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].crm_ns, "test.echo");
        assert_eq!(routes[0].crm_name, "Echo");

        server.shutdown();
    }

    #[tokio::test]
    async fn register_rejects_route_name_that_cannot_fit_wire_control() {
        let state = test_state();
        let long_name = "x".repeat(c2_contract::MAX_WIRE_TEXT_BYTES + 1);

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
            "inst-grid".into(),
            "ipc://missing-grid".into(),
            TEST_CRM_NS.to_string(),
            TEST_CRM_NAME.to_string(),
            TEST_CRM_VER.to_string(),
            TEST_ABI_HASH.to_string(),
            TEST_SIGNATURE_HASH.to_string(),
            stale_client,
            None,
        );

        assert_eq!(
            post_call_with_expected_crm_tag(
                state.clone(),
                "grid",
                "step",
                TEST_CRM_NS,
                TEST_CRM_NAME,
                TEST_CRM_VER,
            )
            .await,
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

    #[tokio::test]
    async fn call_with_expected_crm_tag_rejects_route_tag_mismatch_before_forwarding() {
        let state = test_state();
        let address = format!(
            "ipc://relay_call_wrong_crm_tag_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let server = start_live_server_with_identity_and_contracts(
            &address,
            "server-grid",
            "server-grid-instance",
            &[(
                "grid",
                "test.other",
                "OtherEcho",
                "0.1.0",
                TEST_ABI_HASH,
                TEST_SIGNATURE_HASH,
            )],
        )
        .await;

        assert_eq!(
            post_register(state.clone(), "grid", "server-grid", &address).await,
            StatusCode::CREATED
        );

        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/grid/ping")
                    .header("content-type", "application/octet-stream")
                    .header("x-c2-expected-crm-ns", "test.echo")
                    .header("x-c2-expected-crm-name", "Echo")
                    .header("x-c2-expected-crm-ver", "0.1.0")
                    .header("x-c2-expected-abi-hash", TEST_ABI_HASH)
                    .header("x-c2-expected-signature-hash", TEST_SIGNATURE_HASH)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body.contains("CRMContractMismatch"),
            "unexpected body: {body}"
        );

        server.shutdown();
    }

    async fn install_mismatched_route_swap_after_precheck(
        state: Arc<RelayState>,
        route_name: &str,
        new_address: &str,
    ) {
        let mut new_client = IpcClient::new(new_address);
        new_client.connect().await.unwrap();
        let new_client = Arc::new(new_client);
        let state_for_hook = state;
        let route_for_hook = route_name.to_string();
        let new_address_for_hook = new_address.to_string();
        set_data_plane_after_precheck_hook(route_name.to_string(), move || {
            if let crate::relay::state::UnregisterResult::Removed { client, .. } =
                state_for_hook.unregister_upstream(&route_for_hook, "server-old")
            {
                if let Some(client) = client {
                    tokio::spawn(async move { client.close_shared().await });
                }
            }
            match state_for_hook.commit_register_upstream(
                route_for_hook.clone(),
                "server-new".into(),
                "server-new-instance".into(),
                new_address_for_hook,
                "test.other".into(),
                "OtherEcho".into(),
                "0.1.0".into(),
                TEST_ABI_HASH.to_string(),
                TEST_SIGNATURE_HASH.to_string(),
                new_client,
                None,
            ) {
                RegisterCommitResult::Registered { .. }
                | RegisterCommitResult::SameOwner { .. } => {}
                RegisterCommitResult::Duplicate { existing_address }
                | RegisterCommitResult::ConflictingOwner { existing_address } => {
                    panic!(
                        "failed to install mismatched route in precheck hook: {existing_address}"
                    )
                }
                RegisterCommitResult::Invalid { reason } => {
                    panic!("failed to install mismatched route in precheck hook: {reason}")
                }
            }
        });
    }

    #[tokio::test]
    async fn call_revalidates_expected_crm_tag_against_acquired_route_snapshot() {
        let state = test_state();
        let suffix = unique_suffix();
        let route_name = format!("grid-toctou-call-{suffix}");
        let old_address = format!(
            "ipc://relay_call_toctou_old_{}_{}",
            std::process::id(),
            suffix
        );
        let new_address = format!(
            "ipc://relay_call_toctou_new_{}_{}",
            std::process::id(),
            suffix
        );
        let old_server = start_live_server_with_identity_and_contracts(
            &old_address,
            "server-old",
            "server-old-instance",
            &[(
                &route_name,
                "test.echo",
                "Echo",
                "0.1.0",
                TEST_ABI_HASH,
                TEST_SIGNATURE_HASH,
            )],
        )
        .await;
        let new_server = start_live_server_with_identity_and_contracts(
            &new_address,
            "server-new",
            "server-new-instance",
            &[(
                &route_name,
                "test.other",
                "OtherEcho",
                "0.1.0",
                TEST_ABI_HASH,
                TEST_SIGNATURE_HASH,
            )],
        )
        .await;

        assert_eq!(
            post_register(state.clone(), &route_name, "server-old", &old_address).await,
            StatusCode::CREATED
        );
        install_mismatched_route_swap_after_precheck(state.clone(), &route_name, &new_address)
            .await;

        assert_eq!(
            post_call_with_expected_crm_tag(
                state.clone(),
                &route_name,
                "ping",
                "test.echo",
                "Echo",
                "0.1.0",
            )
            .await,
            StatusCode::CONFLICT
        );

        old_server.shutdown();
        new_server.shutdown();
    }

    #[tokio::test]
    async fn probe_revalidates_expected_crm_tag_against_acquired_route_snapshot() {
        let state = test_state();
        let suffix = unique_suffix();
        let route_name = format!("grid-toctou-probe-{suffix}");
        let old_address = format!(
            "ipc://relay_probe_toctou_old_{}_{}",
            std::process::id(),
            suffix
        );
        let new_address = format!(
            "ipc://relay_probe_toctou_new_{}_{}",
            std::process::id(),
            suffix
        );
        let old_server = start_live_server_with_identity_and_contracts(
            &old_address,
            "server-old",
            "server-old-instance",
            &[(
                &route_name,
                "test.echo",
                "Echo",
                "0.1.0",
                TEST_ABI_HASH,
                TEST_SIGNATURE_HASH,
            )],
        )
        .await;
        let new_server = start_live_server_with_identity_and_contracts(
            &new_address,
            "server-new",
            "server-new-instance",
            &[(
                &route_name,
                "test.other",
                "OtherEcho",
                "0.1.0",
                TEST_ABI_HASH,
                TEST_SIGNATURE_HASH,
            )],
        )
        .await;

        assert_eq!(
            post_register(state.clone(), &route_name, "server-old", &old_address).await,
            StatusCode::CREATED
        );
        install_mismatched_route_swap_after_precheck(state.clone(), &route_name, &new_address)
            .await;

        assert_eq!(
            get_probe_with_expected_crm_tag(
                state.clone(),
                &route_name,
                "test.echo",
                "Echo",
                "0.1.0",
            )
            .await,
            StatusCode::CONFLICT
        );

        old_server.shutdown();
        new_server.shutdown();
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
        let old_server = start_live_server(&old_address, "server-old").await;
        register_echo_route(&old_server, "counter").await;
        let new_server = start_live_server(&new_address, "server-new").await;

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
        let stale_server = start_live_server(&stale_address, "server-grid").await;

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
        let stale_server = start_live_server(&stale_address, "server-grid").await;

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
            get_resolve_with_crm_tag(state.clone(), "grid", "test.echo", "Echo", "0.1.0").await,
            StatusCode::NOT_FOUND
        );

        stale_server.shutdown();
    }

    #[tokio::test]
    async fn probe_reconnect_rejects_same_route_name_with_different_server_instance_id() {
        let state = test_state();
        let address = format!(
            "ipc://relay_probe_wrong_instance_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let old_server = start_live_server(&address, "server-grid").await;

        assert_eq!(
            post_register(state.clone(), "grid", "server-grid", &address).await,
            StatusCode::CREATED
        );
        state.evict_connection("grid");
        old_server.shutdown();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let new_server = start_live_server_with_identity_and_contracts(
            &address,
            "server-grid",
            "server-grid-restarted",
            &[(
                "grid",
                "test.echo",
                "Echo",
                "0.1.0",
                TEST_ABI_HASH,
                TEST_SIGNATURE_HASH,
            )],
        )
        .await;

        assert_eq!(
            get_probe(state.clone(), "grid").await,
            StatusCode::BAD_GATEWAY
        );
        assert!(state.local_route("grid").is_none());

        new_server.shutdown();
    }

    #[tokio::test]
    async fn probe_reconnect_rejects_same_route_name_with_different_crm_tag() {
        let state = test_state();
        let address = format!(
            "ipc://relay_probe_wrong_crm_tag_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let old_server = start_live_server(&address, "server-grid").await;

        assert_eq!(
            post_register(state.clone(), "grid", "server-grid", &address).await,
            StatusCode::CREATED
        );
        state.evict_connection("grid");
        old_server.shutdown();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let new_server = start_live_server_with_identity_and_contracts(
            &address,
            "server-grid",
            "server-grid-instance",
            &[(
                "grid",
                "test.other",
                "OtherEcho",
                "0.1.0",
                TEST_ABI_HASH,
                TEST_SIGNATURE_HASH,
            )],
        )
        .await;

        assert_eq!(
            get_probe(state.clone(), "grid").await,
            StatusCode::BAD_GATEWAY
        );
        assert!(state.local_route("grid").is_none());

        new_server.shutdown();
    }

    #[tokio::test]
    async fn call_missing_upstream_route_removes_stale_local_route() {
        let state = test_state();
        let stale_address = format!(
            "ipc://relay_call_missing_route_{}_{}",
            std::process::id(),
            unique_suffix()
        );
        let stale_server = start_live_server(&stale_address, "server-grid").await;

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
            get_resolve_with_crm_tag(state.clone(), "grid", "test.echo", "Echo", "0.1.0").await,
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
        let old_server = start_live_server(&old_address, "server-old").await;
        let new_server = start_live_server(&new_address, "server-new").await;

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
        let old_server = start_live_server(&old_address, "server-old").await;
        let new_server = start_live_server(&new_address, "server-new").await;

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
        let server = start_live_server(&address, "server-grid").await;

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
                            .header("x-c2-expected-crm-ns", "test.echo")
                            .header("x-c2-expected-crm-name", "Echo")
                            .header("x-c2-expected-crm-ver", "0.1.0")
                            .header("x-c2-expected-abi-hash", TEST_ABI_HASH)
                            .header("x-c2-expected-signature-hash", TEST_SIGNATURE_HASH)
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
        let first_server = start_live_server(&first_address, "server-first").await;
        let second_server = start_live_server(&second_address, "server-second").await;

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
        let server = start_live_server(&address, "server-grid").await;

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
        let server = start_live_server(&address, "server-grid").await;

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
        let server = start_live_server(&address, "server-grid").await;

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
