//! Relay-aware HTTP client with route failover.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::json;

use super::{HttpClient, HttpClientPool, HttpError, RelayControlClient, RelayRouteInfo};

#[derive(Debug, Deserialize)]
struct RelayErrorBody {
    error: String,
}

#[derive(Debug, Clone, Copy)]
pub struct RelayAwareClientConfig {
    pub max_attempts: usize,
}

impl Default for RelayAwareClientConfig {
    fn default() -> Self {
        Self { max_attempts: 3 }
    }
}

pub struct RelayAwareHttpClient {
    control: Arc<RelayControlClient>,
    pool: &'static HttpClientPool,
    route_name: String,
    use_proxy: bool,
    config: RelayAwareClientConfig,
    current: Mutex<Option<String>>,
}

impl RelayAwareHttpClient {
    pub fn new(
        relay_url: &str,
        route_name: &str,
        use_proxy: bool,
        config: RelayAwareClientConfig,
    ) -> Result<Self, HttpError> {
        let control = Arc::new(RelayControlClient::new(relay_url, use_proxy)?);
        Ok(Self::new_with_control(
            control, route_name, use_proxy, config,
        ))
    }

    pub fn new_with_control(
        control: Arc<RelayControlClient>,
        route_name: &str,
        use_proxy: bool,
        config: RelayAwareClientConfig,
    ) -> Self {
        Self {
            control,
            pool: HttpClientPool::instance(),
            route_name: route_name.to_string(),
            use_proxy,
            config,
            current: Mutex::new(None),
        }
    }

    pub fn call(&self, method_name: &str, data: &[u8]) -> Result<Vec<u8>, HttpError> {
        super::client::runtime()
            .handle()
            .block_on(self.call_async(method_name, data))
    }

    pub fn connect(&self) -> Result<(), HttpError> {
        super::client::runtime()
            .handle()
            .block_on(self.connect_async())
    }

    async fn connect_async(&self) -> Result<(), HttpError> {
        let attempts = self.config.max_attempts.max(1);
        let mut last_error = None;
        let mut excluded_routes = HashSet::new();

        for attempt in 0..attempts {
            let routes = match self.resolve_routes_async(attempt > 0).await {
                Ok(routes) if !routes.is_empty() => routes,
                Ok(_) => {
                    return Err(HttpError::ServerError(
                        404,
                        relay_error_body("ResourceNotFound", &self.route_name),
                    ));
                }
                Err(err) => {
                    if resolve_retryable(&err) && attempt + 1 < attempts {
                        last_error = Some(err);
                        continue;
                    }
                    return Err(err);
                }
            };

            let ordered = self.order_routes(routes, &excluded_routes);
            if ordered.is_empty() {
                return Err(last_error.unwrap_or_else(|| {
                    HttpError::ServerError(
                        404,
                        relay_error_body("ResourceNotFound", &self.route_name),
                    )
                }));
            }

            for route in ordered {
                let relay_url = route.relay_url.trim_end_matches('/').to_string();
                let client = match self
                    .pool
                    .acquire_with_proxy_policy(&relay_url, self.use_proxy)
                {
                    Ok(client) => RelayPoolGuard {
                        pool: self.pool,
                        relay_url: relay_url.clone(),
                        client,
                    },
                    Err(err) => {
                        last_error = Some(err);
                        continue;
                    }
                };

                match client.client.probe_route_async(&self.route_name).await {
                    Ok(()) => {
                        *self.current.lock() = Some(relay_url);
                        return Ok(());
                    }
                    Err(err) if route_is_stale(&err) => {
                        self.control.invalidate(&self.route_name);
                        *self.current.lock() = None;
                        excluded_routes.insert(relay_url);
                        last_error = Some(err);
                    }
                    Err(err) => {
                        last_error = Some(err);
                    }
                }
            }

            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        Err(last_error.unwrap_or_else(|| {
            HttpError::ServerError(404, relay_error_body("ResourceNotFound", &self.route_name))
        }))
    }

    pub(crate) async fn call_async(
        &self,
        method_name: &str,
        data: &[u8],
    ) -> Result<Vec<u8>, HttpError> {
        let attempts = self.config.max_attempts.max(1);
        let mut last_error = None;
        let mut excluded_routes = HashSet::new();

        for attempt in 0..attempts {
            let routes = match self.resolve_routes_async(attempt > 0).await {
                Ok(routes) if !routes.is_empty() => routes,
                Ok(_) => {
                    return Err(HttpError::ServerError(
                        404,
                        relay_error_body("ResourceNotFound", &self.route_name),
                    ));
                }
                Err(err) => {
                    if resolve_retryable(&err) && attempt + 1 < attempts {
                        last_error = Some(err);
                        continue;
                    }
                    return Err(err);
                }
            };

            let ordered = self.order_routes(routes, &excluded_routes);
            if ordered.is_empty() {
                return Err(last_error.unwrap_or_else(|| {
                    HttpError::ServerError(
                        404,
                        relay_error_body("ResourceNotFound", &self.route_name),
                    )
                }));
            }
            for route in ordered {
                let relay_url = route.relay_url.trim_end_matches('/').to_string();
                let client = match self
                    .pool
                    .acquire_with_proxy_policy(&relay_url, self.use_proxy)
                {
                    Ok(client) => RelayPoolGuard {
                        pool: self.pool,
                        relay_url: relay_url.clone(),
                        client,
                    },
                    Err(err) => {
                        last_error = Some(err);
                        continue;
                    }
                };

                match client
                    .client
                    .call_async(&self.route_name, method_name, data)
                    .await
                {
                    Ok(bytes) => {
                        *self.current.lock() = Some(relay_url);
                        return Ok(bytes);
                    }
                    Err(HttpError::CrmError(err)) => return Err(HttpError::CrmError(err)),
                    Err(err) if route_is_stale(&err) => {
                        self.control.invalidate(&self.route_name);
                        *self.current.lock() = None;
                        excluded_routes.insert(relay_url);
                        last_error = Some(err);
                        break;
                    }
                    Err(err) => return Err(err),
                }
            }

            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        Err(last_error.unwrap_or_else(|| {
            HttpError::ServerError(404, relay_error_body("ResourceNotFound", &self.route_name))
        }))
    }

    async fn resolve_routes_async(
        &self,
        force_refresh: bool,
    ) -> Result<Vec<RelayRouteInfo>, HttpError> {
        if force_refresh {
            self.control.invalidate(&self.route_name);
        }
        self.control.resolve_async(&self.route_name).await
    }

    fn order_routes(
        &self,
        routes: Vec<RelayRouteInfo>,
        excluded_routes: &HashSet<String>,
    ) -> Vec<RelayRouteInfo> {
        let routes = routes
            .into_iter()
            .filter(|route| !excluded_routes.contains(route.relay_url.trim_end_matches('/')))
            .collect::<Vec<_>>();
        let current = self.current.lock().clone();
        let Some(current) = current else {
            return routes;
        };
        let mut preferred = Vec::new();
        let mut rest = Vec::new();
        for route in routes {
            if route.relay_url.trim_end_matches('/') == current {
                preferred.push(route);
            } else {
                rest.push(route);
            }
        }
        preferred.extend(rest);
        preferred
    }
}

struct RelayPoolGuard {
    pool: &'static HttpClientPool,
    relay_url: String,
    client: Arc<HttpClient>,
}

impl Drop for RelayPoolGuard {
    fn drop(&mut self) {
        self.pool.release(&self.relay_url);
    }
}

fn route_is_stale(err: &HttpError) -> bool {
    match err {
        HttpError::ServerError(404, body) => relay_error_is(body, "ResourceNotFound"),
        HttpError::ServerError(502, body) => relay_error_is(body, "UpstreamUnavailable"),
        _ => false,
    }
}

fn relay_error_is(body: &str, expected: &str) -> bool {
    serde_json::from_str::<RelayErrorBody>(body).is_ok_and(|parsed| parsed.error == expected)
}

fn relay_error_body(error: &str, route_name: &str) -> String {
    json!({
        "error": error,
        "route": route_name,
    })
    .to_string()
}

fn resolve_retryable(err: &HttpError) -> bool {
    matches!(
        err,
        HttpError::Transport(_) | HttpError::ServerError(500..=599, _)
    )
}

#[cfg(all(test, feature = "relay"))]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        body::Bytes,
        extract::{Path, State},
        http::StatusCode,
        response::{IntoResponse, Response},
        routing::{get, post},
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)]
    struct RegistryState {
        stale_url: String,
        live_url: String,
        resolve_count: Arc<AtomicUsize>,
    }

    async fn registry_resolve(
        State(state): State<RegistryState>,
        Path(name): Path<String>,
    ) -> Response {
        state.resolve_count.fetch_add(1, Ordering::SeqCst);
        Json(vec![
            RelayRouteInfo {
                name: name.clone(),
                relay_url: state.stale_url.clone(),
                ipc_address: None,
                crm_ns: String::new(),
                crm_ver: String::new(),
            },
            RelayRouteInfo {
                name,
                relay_url: state.live_url.clone(),
                ipc_address: None,
                crm_ns: String::new(),
                crm_ver: String::new(),
            },
        ])
        .into_response()
    }

    async fn stale_call() -> Response {
        (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": "UpstreamUnavailable"})),
        )
            .into_response()
    }

    async fn generic_bad_gateway() -> Response {
        (StatusCode::BAD_GATEWAY, "proxy exploded").into_response()
    }

    async fn transient_unavailable() -> Response {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "relay temporarily unavailable",
        )
            .into_response()
    }

    async fn transient_resolve_error(
        State(state): State<RegistryState>,
        Path(name): Path<String>,
    ) -> Response {
        let count = state.resolve_count.fetch_add(1, Ordering::SeqCst);
        if count == 0 {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "temporary registry error",
            )
                .into_response();
        }
        Json(vec![RelayRouteInfo {
            name,
            relay_url: state.live_url.clone(),
            ipc_address: None,
            crm_ns: String::new(),
            crm_ver: String::new(),
        }])
        .into_response()
    }

    async fn misleading_bad_gateway() -> Response {
        (
            StatusCode::BAD_GATEWAY,
            "proxy exploded while mentioning UpstreamUnavailable",
        )
            .into_response()
    }

    async fn stale_not_found() -> Response {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "ResourceNotFound"})),
        )
            .into_response()
    }

    async fn generic_not_found() -> Response {
        (StatusCode::NOT_FOUND, "missing ResourceNotFound marker").into_response()
    }

    async fn live_call(Path((_route, _method)): Path<(String, String)>, body: Bytes) -> Response {
        let mut out = b"ok:".to_vec();
        out.extend_from_slice(&body);
        (StatusCode::OK, out).into_response()
    }

    async fn spawn_app(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (url, handle)
    }

    #[tokio::test]
    async fn call_re_resolves_and_tries_next_route_after_stale_route() {
        let (stale_url, stale_handle) =
            spawn_app(Router::new().route("/{route}/{method}", post(stale_call))).await;
        let (live_url, live_handle) =
            spawn_app(Router::new().route("/{route}/{method}", post(live_call))).await;
        let resolve_count = Arc::new(AtomicUsize::new(0));
        let registry_state = RegistryState {
            stale_url,
            live_url,
            resolve_count: resolve_count.clone(),
        };
        let (registry_url, registry_handle) = spawn_app(
            Router::new()
                .route("/_resolve/{name}", get(registry_resolve))
                .with_state(registry_state),
        )
        .await;

        let client = RelayAwareHttpClient::new(
            &registry_url,
            "grid",
            false,
            RelayAwareClientConfig { max_attempts: 3 },
        )
        .unwrap();

        let bytes = client.call_async("step", b"payload").await.unwrap();
        assert_eq!(bytes, b"ok:payload");
        assert!(
            resolve_count.load(Ordering::SeqCst) >= 2,
            "stale route should invalidate cache and re-resolve"
        );

        registry_handle.abort();
        stale_handle.abort();
        live_handle.abort();
    }

    #[tokio::test]
    async fn connect_selects_reachable_route_before_first_call() {
        let stale_url = "http://127.0.0.1:9".to_string();
        let (live_url, live_handle) = spawn_app(
            Router::new()
                .route("/_probe/{route}", get(|| async { StatusCode::OK }))
                .route("/{route}/{method}", post(live_call)),
        )
        .await;
        let resolve_count = Arc::new(AtomicUsize::new(0));
        let registry_state = RegistryState {
            stale_url,
            live_url: live_url.clone(),
            resolve_count: resolve_count.clone(),
        };
        let (registry_url, registry_handle) = spawn_app(
            Router::new()
                .route("/_resolve/{name}", get(registry_resolve))
                .with_state(registry_state),
        )
        .await;

        let client = RelayAwareHttpClient::new(
            &registry_url,
            "grid",
            false,
            RelayAwareClientConfig { max_attempts: 3 },
        )
        .unwrap();

        client.connect_async().await.unwrap();
        assert_eq!(client.current.lock().as_deref(), Some(live_url.as_str()));
        let bytes = client.call_async("step", b"payload").await.unwrap();
        assert_eq!(bytes, b"ok:payload");

        registry_handle.abort();
        live_handle.abort();
    }

    #[tokio::test]
    async fn generic_502_is_not_treated_as_stale_route() {
        let (bad_url, bad_handle) =
            spawn_app(Router::new().route("/{route}/{method}", post(generic_bad_gateway))).await;
        let (live_url, live_handle) =
            spawn_app(Router::new().route("/{route}/{method}", post(live_call))).await;
        let resolve_count = Arc::new(AtomicUsize::new(0));
        let registry_state = RegistryState {
            stale_url: bad_url,
            live_url,
            resolve_count: resolve_count.clone(),
        };
        let (registry_url, registry_handle) = spawn_app(
            Router::new()
                .route("/_resolve/{name}", get(registry_resolve))
                .with_state(registry_state),
        )
        .await;

        let client = RelayAwareHttpClient::new(
            &registry_url,
            "grid",
            false,
            RelayAwareClientConfig { max_attempts: 1 },
        )
        .unwrap();

        let err = client.call_async("step", b"payload").await.unwrap_err();
        assert!(matches!(err, HttpError::ServerError(502, _)));
        assert_eq!(resolve_count.load(Ordering::SeqCst), 1);

        registry_handle.abort();
        bad_handle.abort();
        live_handle.abort();
    }

    #[tokio::test]
    async fn misleading_502_text_is_not_treated_as_stale_route() {
        let (bad_url, bad_handle) =
            spawn_app(Router::new().route("/{route}/{method}", post(misleading_bad_gateway))).await;
        let (live_url, live_handle) =
            spawn_app(Router::new().route("/{route}/{method}", post(live_call))).await;
        let resolve_count = Arc::new(AtomicUsize::new(0));
        let registry_state = RegistryState {
            stale_url: bad_url,
            live_url,
            resolve_count: resolve_count.clone(),
        };
        let (registry_url, registry_handle) = spawn_app(
            Router::new()
                .route("/_resolve/{name}", get(registry_resolve))
                .with_state(registry_state),
        )
        .await;

        let client = RelayAwareHttpClient::new(
            &registry_url,
            "grid",
            false,
            RelayAwareClientConfig { max_attempts: 1 },
        )
        .unwrap();

        let err = client.call_async("step", b"payload").await.unwrap_err();
        assert!(matches!(err, HttpError::ServerError(502, _)));
        assert_eq!(resolve_count.load(Ordering::SeqCst), 1);

        registry_handle.abort();
        bad_handle.abort();
        live_handle.abort();
    }

    #[tokio::test]
    async fn structured_404_is_treated_as_stale_route() {
        let (stale_url, stale_handle) =
            spawn_app(Router::new().route("/{route}/{method}", post(stale_not_found))).await;
        let (live_url, live_handle) =
            spawn_app(Router::new().route("/{route}/{method}", post(live_call))).await;
        let resolve_count = Arc::new(AtomicUsize::new(0));
        let registry_state = RegistryState {
            stale_url,
            live_url,
            resolve_count: resolve_count.clone(),
        };
        let (registry_url, registry_handle) = spawn_app(
            Router::new()
                .route("/_resolve/{name}", get(registry_resolve))
                .with_state(registry_state),
        )
        .await;

        let client = RelayAwareHttpClient::new(
            &registry_url,
            "grid",
            false,
            RelayAwareClientConfig { max_attempts: 3 },
        )
        .unwrap();

        let bytes = client.call_async("step", b"payload").await.unwrap();
        assert_eq!(bytes, b"ok:payload");
        assert!(resolve_count.load(Ordering::SeqCst) >= 2);

        registry_handle.abort();
        stale_handle.abort();
        live_handle.abort();
    }

    #[tokio::test]
    async fn generic_404_is_not_treated_as_stale_route() {
        let (bad_url, bad_handle) =
            spawn_app(Router::new().route("/{route}/{method}", post(generic_not_found))).await;
        let (live_url, live_handle) =
            spawn_app(Router::new().route("/{route}/{method}", post(live_call))).await;
        let resolve_count = Arc::new(AtomicUsize::new(0));
        let registry_state = RegistryState {
            stale_url: bad_url,
            live_url,
            resolve_count: resolve_count.clone(),
        };
        let (registry_url, registry_handle) = spawn_app(
            Router::new()
                .route("/_resolve/{name}", get(registry_resolve))
                .with_state(registry_state),
        )
        .await;

        let client = RelayAwareHttpClient::new(
            &registry_url,
            "grid",
            false,
            RelayAwareClientConfig { max_attempts: 1 },
        )
        .unwrap();

        let err = client.call_async("step", b"payload").await.unwrap_err();
        assert!(matches!(err, HttpError::ServerError(404, _)));
        assert_eq!(resolve_count.load(Ordering::SeqCst), 1);

        registry_handle.abort();
        bad_handle.abort();
        live_handle.abort();
    }

    #[tokio::test]
    async fn ambiguous_call_failure_is_not_replayed() {
        let (bad_url, bad_handle) =
            spawn_app(Router::new().route("/{route}/{method}", post(transient_unavailable))).await;
        let (live_url, live_handle) =
            spawn_app(Router::new().route("/{route}/{method}", post(live_call))).await;
        let resolve_count = Arc::new(AtomicUsize::new(0));
        let registry_state = RegistryState {
            stale_url: bad_url,
            live_url,
            resolve_count: resolve_count.clone(),
        };
        let (registry_url, registry_handle) = spawn_app(
            Router::new()
                .route("/_resolve/{name}", get(registry_resolve))
                .with_state(registry_state),
        )
        .await;

        let client = RelayAwareHttpClient::new(
            &registry_url,
            "grid",
            false,
            RelayAwareClientConfig { max_attempts: 3 },
        )
        .unwrap();

        let err = client.call_async("step", b"payload").await.unwrap_err();
        assert!(matches!(err, HttpError::ServerError(503, _)));
        assert_eq!(
            resolve_count.load(Ordering::SeqCst),
            1,
            "ambiguous data-plane failures must not replay CRM calls"
        );

        registry_handle.abort();
        bad_handle.abort();
        live_handle.abort();
    }

    #[tokio::test]
    async fn connect_retries_transient_resolve_5xx() {
        let (live_url, live_handle) =
            spawn_app(Router::new().route("/_probe/{route}", get(|| async { StatusCode::OK })))
                .await;
        let resolve_count = Arc::new(AtomicUsize::new(0));
        let registry_state = RegistryState {
            stale_url: String::new(),
            live_url: live_url.clone(),
            resolve_count: resolve_count.clone(),
        };
        let (registry_url, registry_handle) = spawn_app(
            Router::new()
                .route("/_resolve/{name}", get(transient_resolve_error))
                .with_state(registry_state),
        )
        .await;

        let client = RelayAwareHttpClient::new(
            &registry_url,
            "grid",
            false,
            RelayAwareClientConfig { max_attempts: 3 },
        )
        .unwrap();

        client.connect_async().await.unwrap();
        assert_eq!(client.current.lock().as_deref(), Some(live_url.as_str()));
        assert_eq!(resolve_count.load(Ordering::SeqCst), 2);

        registry_handle.abort();
        live_handle.abort();
    }
}
