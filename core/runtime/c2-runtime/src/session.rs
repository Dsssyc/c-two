//! Rust-owned process runtime session.
//!
//! The session owns process identity, direct IPC client configuration state,
//! and route registration transactions. Python SDKs provide language-specific
//! callbacks and local direct-call bindings, but must not duplicate the runtime
//! authority implemented here.

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use parking_lot::Mutex;

use c2_http::client::{
    HttpError, RelayAwareClientConfig, RelayAwareHttpClient, RelayControlClient,
};
use c2_server::dispatcher::CrmRoute;

use crate::{
    RegisterOutcome, RelayCleanupError, RuntimeRouteSpec, ShutdownOutcome, UnregisterOutcome,
};
use crate::{auto_server_id, ipc_address_for_server_id, validate_server_id};

pub type ServerIpcConfigOverrides = c2_config::ServerIpcConfigOverrides;
pub type ClientIpcConfigOverrides = c2_config::ClientIpcConfigOverrides;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeIdentity {
    pub server_id: String,
    pub ipc_address: String,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeSessionOptions {
    pub server_id: Option<String>,
    pub server_ipc_overrides: Option<ServerIpcConfigOverrides>,
    pub client_ipc_overrides: Option<ClientIpcConfigOverrides>,
    pub shm_threshold: Option<u64>,
    pub relay_address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeSessionError {
    InvalidServerId(String),
    ClientConfigFrozen,
    DuplicateRoute(String),
    MissingRoute(String),
    RelayDuplicateRoute(String),
    MissingRelayAddress,
    RelayHttp { status_code: u16, message: String },
    Server(String),
    Relay(String),
}

impl fmt::Display for RuntimeSessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidServerId(message) => write!(f, "{message}"),
            Self::ClientConfigFrozen => write!(f, "client IPC configuration is frozen"),
            Self::DuplicateRoute(name) => write!(f, "route already registered: {name}"),
            Self::MissingRoute(name) => write!(f, "route not registered: {name}"),
            Self::RelayDuplicateRoute(name) => {
                write!(f, "relay route already registered: {name}")
            }
            Self::MissingRelayAddress => write!(f, "no relay address configured"),
            Self::RelayHttp {
                status_code,
                message,
            } => {
                write!(f, "relay HTTP {status_code}: {message}")
            }
            Self::Server(message) => write!(f, "server error: {message}"),
            Self::Relay(message) => write!(f, "relay error: {message}"),
        }
    }
}

impl Error for RuntimeSessionError {}

pub struct RuntimeSession {
    state: Mutex<RuntimeSessionState>,
}

impl fmt::Debug for RuntimeSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeSession").finish_non_exhaustive()
    }
}

struct RuntimeSessionState {
    server_id_override: Option<String>,
    server_ipc_overrides: Option<ServerIpcConfigOverrides>,
    client_ipc_overrides: Option<ClientIpcConfigOverrides>,
    shm_threshold: Option<u64>,
    client_config_frozen: bool,
    identity: Option<RuntimeIdentity>,
    relay_address_override: Option<String>,
    relay_projection: Option<RelayProjection>,
}

#[derive(Clone)]
struct RelayProjection {
    relay_address: String,
    relay_use_proxy: bool,
    control: Arc<RelayControlClient>,
}

impl RuntimeSession {
    pub fn new(options: RuntimeSessionOptions) -> Result<Self, RuntimeSessionError> {
        if let Some(server_id) = options.server_id.as_deref() {
            validate_server_id(server_id)?;
        }
        Ok(Self {
            state: Mutex::new(RuntimeSessionState {
                server_id_override: options.server_id,
                server_ipc_overrides: options.server_ipc_overrides,
                client_ipc_overrides: options.client_ipc_overrides,
                shm_threshold: options.shm_threshold,
                client_config_frozen: false,
                identity: None,
                relay_address_override: options
                    .relay_address
                    .map(|addr| canonical_relay_address(&addr)),
                relay_projection: None,
            }),
        })
    }

    pub fn ensure_server(&self) -> Result<RuntimeIdentity, RuntimeSessionError> {
        let mut state = self.state.lock();
        if let Some(identity) = &state.identity {
            return Ok(identity.clone());
        }

        let server_id = match state.server_id_override.clone() {
            Some(server_id) => server_id,
            None => auto_server_id(),
        };
        validate_server_id(&server_id)?;
        let identity = RuntimeIdentity {
            ipc_address: ipc_address_for_server_id(&server_id),
            server_id,
        };
        state.identity = Some(identity.clone());
        Ok(identity)
    }

    pub fn server_id(&self) -> Option<String> {
        self.state
            .lock()
            .identity
            .as_ref()
            .map(|identity| identity.server_id.clone())
    }

    pub fn server_id_override(&self) -> Option<String> {
        self.state.lock().server_id_override.clone()
    }

    pub fn server_address(&self) -> Option<String> {
        self.state
            .lock()
            .identity
            .as_ref()
            .map(|identity| identity.ipc_address.clone())
    }

    pub fn server_ipc_overrides(&self) -> Option<ServerIpcConfigOverrides> {
        self.state.lock().server_ipc_overrides.clone()
    }

    pub fn set_server_options(
        &self,
        server_id: Option<String>,
        server_ipc_overrides: Option<ServerIpcConfigOverrides>,
    ) -> Result<(), RuntimeSessionError> {
        if let Some(server_id) = server_id.as_deref() {
            validate_server_id(server_id)?;
        }
        let mut state = self.state.lock();
        state.server_id_override = server_id;
        state.server_ipc_overrides = server_ipc_overrides;
        state.identity = None;
        Ok(())
    }

    pub fn client_ipc_overrides(&self) -> Option<ClientIpcConfigOverrides> {
        self.state.lock().client_ipc_overrides.clone()
    }

    pub fn set_client_ipc_overrides(
        &self,
        overrides: Option<ClientIpcConfigOverrides>,
    ) -> Result<(), RuntimeSessionError> {
        let mut state = self.state.lock();
        if state.client_config_frozen {
            return Err(RuntimeSessionError::ClientConfigFrozen);
        }
        state.client_ipc_overrides = overrides;
        Ok(())
    }

    pub fn shm_threshold_override(&self) -> Option<u64> {
        self.state.lock().shm_threshold
    }

    pub fn client_config_frozen(&self) -> bool {
        self.state.lock().client_config_frozen
    }

    pub fn mark_client_config_frozen(&self) {
        self.state.lock().client_config_frozen = true;
    }

    pub fn clear_server_identity(&self) {
        self.state.lock().identity = None;
    }

    fn current_identity(&self) -> Option<RuntimeIdentity> {
        self.state.lock().identity.clone()
    }

    pub fn set_relay_address(&self, relay_address: Option<String>) {
        let mut state = self.state.lock();
        let relay_address = relay_address.map(|addr| canonical_relay_address(&addr));
        if state.relay_address_override != relay_address {
            state.relay_projection = None;
        }
        state.relay_address_override = relay_address;
    }

    pub fn relay_address_override(&self) -> Option<String> {
        self.state.lock().relay_address_override.clone()
    }

    pub fn effective_relay_address(&self) -> Result<Option<String>, RuntimeSessionError> {
        if let Some(address) = self.state.lock().relay_address_override.clone() {
            return Ok(Some(address));
        }
        c2_config::ConfigResolver::resolve_relay_address(c2_config::ConfigSources::from_process())
            .map_err(|e| RuntimeSessionError::Relay(e.to_string()))
    }

    pub fn register_route(
        &self,
        server: &Arc<c2_server::Server>,
        route: CrmRoute,
        spec: RuntimeRouteSpec,
        relay_address: Option<&str>,
        relay_use_proxy: bool,
    ) -> Result<RegisterOutcome, RuntimeSessionError> {
        let identity = self.ensure_server()?;
        let route_name = spec.name.clone();
        let effective_relay_address = self.effective_relay_address_arg(relay_address)?;
        if route.name != spec.name {
            return Err(RuntimeSessionError::Server(format!(
                "route/spec name mismatch: route={:?}, spec={:?}",
                route.name, spec.name
            )));
        }
        if route.method_names != spec.method_names {
            return Err(RuntimeSessionError::Server(format!(
                "route/spec method_names mismatch for {route_name}"
            )));
        }
        let scheduler_snapshot = route.scheduler.snapshot();
        if scheduler_snapshot.mode != spec.concurrency_mode
            || scheduler_snapshot.max_pending != spec.max_pending
            || scheduler_snapshot.max_workers != spec.max_workers
        {
            return Err(RuntimeSessionError::Server(format!(
                "route/spec scheduler mismatch for {route_name}"
            )));
        }

        let rt = Self::server_runtime()?;
        rt.block_on(server.register_route(route)).map_err(|e| {
            if e.to_string().contains("already registered") {
                RuntimeSessionError::DuplicateRoute(route_name.clone())
            } else {
                RuntimeSessionError::Server(e.to_string())
            }
        })?;

        let mut relay_registered = false;
        if let Some(relay_address) = effective_relay_address.as_deref() {
            let projection = match self.relay_projection_for_address(relay_address, relay_use_proxy)
            {
                Ok(projection) => projection,
                Err(err) => {
                    let _ = rt.block_on(server.unregister_route(&route_name));
                    return Err(err);
                }
            };
            if let Err(err) = projection.control.register(
                &spec.name,
                &identity.server_id,
                &identity.ipc_address,
                &spec.crm_ns,
                &spec.crm_ver,
            ) {
                let _ = rt.block_on(server.unregister_route(&route_name));
                return match err {
                    HttpError::ServerError(409, _body) => {
                        Err(RuntimeSessionError::RelayDuplicateRoute(route_name.clone()))
                    }
                    HttpError::ServerError(status_code, body) => {
                        Err(RuntimeSessionError::RelayHttp {
                            status_code,
                            message: body,
                        })
                    }
                    other => Err(RuntimeSessionError::Relay(other.to_string())),
                };
            }
            relay_registered = true;
        }

        Ok(RegisterOutcome {
            route_name,
            server_id: identity.server_id,
            ipc_address: identity.ipc_address,
            relay_registered,
        })
    }

    pub fn unregister_route(
        &self,
        server: &Arc<c2_server::Server>,
        name: &str,
        relay_address: Option<&str>,
        relay_use_proxy: bool,
    ) -> Result<UnregisterOutcome, RuntimeSessionError> {
        let route_name = name.to_string();
        let effective_relay_address = self.effective_relay_address_arg(relay_address)?;
        let rt = Self::server_runtime()?;
        let local_removed = rt.block_on(server.unregister_route(&route_name));
        if !local_removed {
            return Err(RuntimeSessionError::MissingRoute(route_name));
        }

        let relay_error = if let Some(relay_address) = effective_relay_address.as_deref() {
            let identity = self.ensure_server()?;
            self.relay_cleanup(
                Some(relay_address),
                relay_use_proxy,
                &route_name,
                &identity.server_id,
            )
        } else {
            None
        };
        Ok(UnregisterOutcome {
            route_name,
            local_removed,
            relay_error,
        })
    }

    pub fn shutdown(
        &self,
        server: Option<&Arc<c2_server::Server>>,
        route_names: Vec<String>,
        relay_address: Option<&str>,
        relay_use_proxy: bool,
    ) -> ShutdownOutcome {
        let effective_relay_address = self
            .effective_relay_address_arg(relay_address)
            .ok()
            .flatten();
        let identity = if effective_relay_address.is_some() && !route_names.is_empty() {
            self.current_identity()
                .or_else(|| self.ensure_server().ok())
        } else {
            self.current_identity()
        };
        let server_was_started = server.map(|server| server.is_running()).unwrap_or(false);
        let mut outcome = ShutdownOutcome {
            server_was_started,
            ..Default::default()
        };

        if let Some(server) = server {
            if let Ok(rt) = Self::server_runtime() {
                for route_name in &route_names {
                    let removed = rt.block_on(server.unregister_route(route_name));
                    if removed {
                        outcome.removed_routes.push(route_name.clone());
                    }
                    if let Some(identity) = identity.as_ref() {
                        if let Some(relay_error) = self.relay_cleanup(
                            effective_relay_address.as_deref(),
                            relay_use_proxy,
                            route_name,
                            &identity.server_id,
                        ) {
                            outcome.relay_errors.push(relay_error);
                        }
                    }
                }
            } else {
                for route_name in &route_names {
                    if let Some(identity) = identity.as_ref() {
                        if let Some(relay_error) = self.relay_cleanup(
                            effective_relay_address.as_deref(),
                            relay_use_proxy,
                            route_name,
                            &identity.server_id,
                        ) {
                            outcome.relay_errors.push(relay_error);
                        }
                    }
                }
            }
            server.shutdown();
        } else if let Some(identity) = identity.as_ref() {
            for route_name in &route_names {
                if let Some(relay_error) = self.relay_cleanup(
                    effective_relay_address.as_deref(),
                    relay_use_proxy,
                    route_name,
                    &identity.server_id,
                ) {
                    outcome.relay_errors.push(relay_error);
                }
            }
        }

        outcome
    }

    pub fn connect_via_relay(
        &self,
        route_name: &str,
        relay_use_proxy: bool,
        max_attempts: usize,
    ) -> Result<Arc<RelayAwareHttpClient>, RuntimeSessionError> {
        let projection = self.relay_projection(relay_use_proxy)?;
        let client = RelayAwareHttpClient::new_with_control(
            Arc::clone(&projection.control),
            route_name,
            projection.relay_use_proxy,
            RelayAwareClientConfig { max_attempts },
        );
        client.connect().map_err(runtime_http_error)?;
        Ok(Arc::new(client))
    }

    pub fn clear_relay_projection_cache(&self) {
        if let Some(projection) = self.state.lock().relay_projection.as_ref() {
            projection.control.clear_cache();
        }
    }

    fn relay_projection(
        &self,
        relay_use_proxy: bool,
    ) -> Result<RelayProjection, RuntimeSessionError> {
        let relay_address = self
            .effective_relay_address()?
            .ok_or(RuntimeSessionError::MissingRelayAddress)?;
        self.relay_projection_for_address(&relay_address, relay_use_proxy)
    }

    fn relay_projection_for_address(
        &self,
        relay_address: &str,
        relay_use_proxy: bool,
    ) -> Result<RelayProjection, RuntimeSessionError> {
        let relay_address = canonical_relay_address(relay_address);
        {
            let state = self.state.lock();
            if let Some(projection) = state.relay_projection.as_ref() {
                if projection.relay_address == relay_address
                    && projection.relay_use_proxy == relay_use_proxy
                {
                    return Ok(projection.clone());
                }
            }
        }

        let control = Arc::new(
            RelayControlClient::new(&relay_address, relay_use_proxy)
                .map_err(|e| RuntimeSessionError::Relay(e.to_string()))?,
        );
        let projection = RelayProjection {
            relay_address,
            relay_use_proxy,
            control,
        };
        self.state.lock().relay_projection = Some(projection.clone());
        Ok(projection)
    }

    fn server_runtime() -> Result<tokio::runtime::Runtime, RuntimeSessionError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| RuntimeSessionError::Server(format!("failed to create runtime: {e}")))?;
        Ok(rt)
    }

    fn relay_cleanup(
        &self,
        relay_address: Option<&str>,
        relay_use_proxy: bool,
        route_name: &str,
        server_id: &str,
    ) -> Option<RelayCleanupError> {
        let relay_address = relay_address?;
        let projection = match self.relay_projection_for_address(relay_address, relay_use_proxy) {
            Ok(projection) => projection,
            Err(err) => {
                return Some(RelayCleanupError {
                    route_name: route_name.to_string(),
                    status_code: None,
                    message: err.to_string(),
                });
            }
        };
        match projection.control.unregister(route_name, server_id) {
            Ok(()) => None,
            Err(HttpError::ServerError(status_code, body)) => Some(RelayCleanupError {
                route_name: route_name.to_string(),
                status_code: Some(status_code),
                message: if body.is_empty() {
                    format!("HTTP {status_code}")
                } else {
                    body
                },
            }),
            Err(err) => Some(RelayCleanupError {
                route_name: route_name.to_string(),
                status_code: None,
                message: err.to_string(),
            }),
        }
    }

    fn effective_relay_address_arg(
        &self,
        relay_address: Option<&str>,
    ) -> Result<Option<String>, RuntimeSessionError> {
        match relay_address {
            Some(address) => Ok(Some(address.to_string())),
            None => self.effective_relay_address(),
        }
    }
}

fn canonical_relay_address(address: &str) -> String {
    address.trim().trim_end_matches('/').to_string()
}

fn runtime_http_error(err: HttpError) -> RuntimeSessionError {
    match err {
        HttpError::ServerError(status_code, message) => RuntimeSessionError::RelayHttp {
            status_code,
            message,
        },
        other => RuntimeSessionError::Relay(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    use c2_server::config::ServerIpcConfig;
    use c2_server::dispatcher::{CrmCallback, CrmError, RequestData, ResponseMeta};
    use c2_server::scheduler::{ConcurrencyMode, Scheduler};

    static TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct NoopCallback;

    impl CrmCallback for NoopCallback {
        fn invoke(
            &self,
            _route_name: &str,
            _method_idx: u16,
            _request: RequestData,
            _response_pool: Arc<parking_lot::RwLock<c2_mem::MemPool>>,
        ) -> Result<ResponseMeta, CrmError> {
            Ok(ResponseMeta::Empty)
        }
    }

    fn unique_route_name(prefix: &str) -> String {
        let n = TEST_ID.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{}-{n}", std::process::id())
    }

    fn test_server(prefix: &str) -> Arc<c2_server::Server> {
        let name = unique_route_name(prefix);
        Arc::new(
            c2_server::Server::new(&format!("ipc://{name}"), ServerIpcConfig::default())
                .expect("test server should construct"),
        )
    }

    fn dummy_route(name: &str) -> CrmRoute {
        CrmRoute {
            name: name.to_string(),
            scheduler: Arc::new(Scheduler::new(
                ConcurrencyMode::ReadParallel,
                HashMap::new(),
            )),
            callback: Arc::new(NoopCallback),
            method_names: vec!["ping".to_string()],
        }
    }

    fn register_dummy(server: &Arc<c2_server::Server>, name: &str) {
        let rt = RuntimeSession::server_runtime().expect("runtime");
        rt.block_on(server.register_route(dummy_route(name)))
            .expect("dummy route should register");
    }

    #[test]
    fn explicit_server_id_derives_ipc_address() {
        let session = RuntimeSession::new(RuntimeSessionOptions {
            server_id: Some("unit-server".to_string()),
            server_ipc_overrides: None,
            client_ipc_overrides: None,
            shm_threshold: None,
            relay_address: None,
        })
        .expect("session should accept valid server id");

        assert_eq!(session.server_id(), None);
        assert_eq!(session.server_address(), None);

        let identity = session.ensure_server().expect("ensure server");
        assert_eq!(identity.server_id, "unit-server");
        assert_eq!(identity.ipc_address, "ipc://unit-server");
        assert_eq!(session.server_id().as_deref(), Some("unit-server"));
        assert_eq!(
            session.server_address().as_deref(),
            Some("ipc://unit-server")
        );
    }

    #[test]
    fn invalid_server_id_is_rejected() {
        for bad in ["", " ", "bad/name", "bad\\name", ".", "..", "bad\nid"] {
            let err = RuntimeSession::new(RuntimeSessionOptions {
                server_id: Some(bad.to_string()),
                server_ipc_overrides: None,
                client_ipc_overrides: None,
                shm_threshold: None,
                relay_address: None,
            })
            .expect_err("invalid server id should be rejected");
            assert!(
                err.to_string().contains("server_id"),
                "unexpected error: {err}"
            );
        }
    }

    #[test]
    fn auto_server_id_is_valid_and_address_matches() {
        let session = RuntimeSession::new(RuntimeSessionOptions::default()).expect("session");
        let identity = session.ensure_server().expect("ensure server");

        c2_config::validate_server_id(&identity.server_id).expect("generated id should validate");
        assert!(identity.server_id.starts_with("cc"));
        assert_eq!(
            identity.ipc_address,
            format!("ipc://{}", identity.server_id)
        );
        assert_eq!(session.ensure_server().expect("idempotent"), identity);
    }

    #[test]
    fn clear_server_identity_preserves_explicit_override() {
        let session = RuntimeSession::new(RuntimeSessionOptions {
            server_id: Some("unit-retry".to_string()),
            server_ipc_overrides: None,
            client_ipc_overrides: None,
            shm_threshold: None,
            relay_address: None,
        })
        .expect("session should accept valid server id");

        let first = session.ensure_server().expect("ensure server");
        assert_eq!(first.server_id, "unit-retry");
        session.clear_server_identity();
        assert_eq!(session.server_id(), None);
        assert_eq!(session.server_address(), None);

        let second = session.ensure_server().expect("ensure server again");
        assert_eq!(second.server_id, "unit-retry");
        assert_eq!(second.ipc_address, "ipc://unit-retry");
    }

    #[test]
    fn server_ipc_overrides_are_session_owned() {
        let overrides = ServerIpcConfigOverrides {
            pool_segment_size: Some(2 * 1024 * 1024),
            max_pool_segments: Some(2),
            ..Default::default()
        };
        let session = RuntimeSession::new(RuntimeSessionOptions {
            server_id: Some("unit-server-overrides".to_string()),
            server_ipc_overrides: Some(overrides),
            client_ipc_overrides: None,
            shm_threshold: None,
            relay_address: None,
        })
        .expect("session should accept valid options");

        let projected = session
            .server_ipc_overrides()
            .expect("overrides should be stored");
        assert_eq!(projected.pool_segment_size, Some(2 * 1024 * 1024));
        assert_eq!(projected.max_pool_segments, Some(2));
    }

    #[test]
    fn client_ipc_overrides_are_session_owned() {
        let overrides = ClientIpcConfigOverrides {
            reassembly_segment_size: Some(16 * 1024 * 1024),
            ..Default::default()
        };
        let session = RuntimeSession::new(RuntimeSessionOptions {
            server_id: None,
            server_ipc_overrides: None,
            client_ipc_overrides: Some(overrides),
            shm_threshold: None,
            relay_address: None,
        })
        .expect("session should accept valid options");

        let projected = session
            .client_ipc_overrides()
            .expect("overrides should be stored");
        assert_eq!(projected.reassembly_segment_size, Some(16 * 1024 * 1024));
    }

    #[test]
    fn relay_address_override_is_canonicalized_and_clear_cache_is_safe() {
        let session = RuntimeSession::new(RuntimeSessionOptions {
            relay_address: Some(" http://relay.test/ ".to_string()),
            ..Default::default()
        })
        .expect("session");

        assert_eq!(
            session.relay_address_override().as_deref(),
            Some("http://relay.test"),
        );

        session.clear_relay_projection_cache();
        session.set_relay_address(Some("http://relay-b.test/".to_string()));
        assert_eq!(
            session.relay_address_override().as_deref(),
            Some("http://relay-b.test"),
        );
    }

    #[test]
    fn unregister_missing_route_reports_missing_route() {
        let session = RuntimeSession::new(RuntimeSessionOptions {
            server_id: Some(unique_route_name("missing-session")),
            ..Default::default()
        })
        .expect("session");
        let server = test_server("missing-server");
        let err = session
            .unregister_route(&server, "absent", None, false)
            .expect_err("missing route should error");
        assert_eq!(err, RuntimeSessionError::MissingRoute("absent".to_string()));
    }

    #[test]
    fn unregister_without_relay_does_not_publish_lazy_identity() {
        let route_name = unique_route_name("no-relay-unregister");
        let session = RuntimeSession::new(RuntimeSessionOptions::default()).expect("session");
        let server = test_server("no-relay-unregister-server");
        register_dummy(&server, &route_name);

        let outcome = session
            .unregister_route(&server, &route_name, None, false)
            .expect("local unregister should succeed");

        assert!(outcome.local_removed);
        assert!(outcome.relay_error.is_none());
        assert_eq!(session.server_id(), None);
        assert_eq!(session.server_address(), None);
    }

    #[test]
    fn unregister_relay_failure_returns_structured_outcome_after_local_remove() {
        let route_name = unique_route_name("relay-fail-route");
        let session = RuntimeSession::new(RuntimeSessionOptions {
            server_id: Some(unique_route_name("relay-fail-session")),
            ..Default::default()
        })
        .expect("session");
        let server = test_server("relay-fail-server");
        register_dummy(&server, &route_name);

        let outcome = session
            .unregister_route(&server, &route_name, Some("http://127.0.0.1:9"), false)
            .expect("local unregister should succeed despite relay failure");
        assert_eq!(outcome.route_name, route_name);
        assert!(outcome.local_removed);
        let relay_error = outcome.relay_error.expect("relay error should be captured");
        assert_eq!(relay_error.route_name, outcome.route_name);
        assert_eq!(relay_error.status_code, None);
        assert!(!relay_error.message.is_empty());

        let err = session
            .unregister_route(&server, &outcome.route_name, None, false)
            .expect_err("route should have already been removed locally");
        assert!(matches!(err, RuntimeSessionError::MissingRoute(_)));
    }

    #[test]
    fn shutdown_is_idempotent_and_reports_removed_routes_once() {
        let first = unique_route_name("shutdown-a");
        let second = unique_route_name("shutdown-b");
        let session = RuntimeSession::new(RuntimeSessionOptions {
            server_id: Some(unique_route_name("shutdown-session")),
            ..Default::default()
        })
        .expect("session");
        let server = test_server("shutdown-server");
        register_dummy(&server, &first);
        register_dummy(&server, &second);

        let first_outcome = session.shutdown(
            Some(&server),
            vec![first.clone(), second.clone()],
            None,
            false,
        );
        assert_eq!(first_outcome.removed_routes, vec![first, second]);
        assert!(first_outcome.relay_errors.is_empty());

        let second_outcome = session.shutdown(
            Some(&server),
            first_outcome.removed_routes.clone(),
            None,
            false,
        );
        assert!(second_outcome.removed_routes.is_empty());
        assert!(second_outcome.relay_errors.is_empty());
    }

    #[test]
    fn shutdown_without_relay_does_not_publish_lazy_identity() {
        let route_name = unique_route_name("shutdown-no-relay");
        let session = RuntimeSession::new(RuntimeSessionOptions::default()).expect("session");
        let server = test_server("shutdown-no-relay-server");
        register_dummy(&server, &route_name);

        let outcome = session.shutdown(Some(&server), vec![route_name], None, false);

        assert_eq!(outcome.removed_routes.len(), 1);
        assert!(outcome.relay_errors.is_empty());
        assert_eq!(session.server_id(), None);
        assert_eq!(session.server_address(), None);
    }
}
