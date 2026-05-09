//! PyO3 bindings for the Rust-owned process runtime session.
//!
//! Phase 1 exposes server identity/address authority. Later phases will move
//! server lifecycle, route transactions, pools, and relay projection here.

use parking_lot::Mutex;
use pyo3::exceptions::{PyKeyError, PyLookupError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use std::sync::Arc;

use c2_http::client::RelayAwareHttpClient;
use c2_ipc::{ClientPool, IpcError, SyncClient};
use c2_mem::BufferLeaseTracker;
use c2_runtime::{
    RegisterOutcome, RelayCleanupError, RelayResolvedConnection, RuntimeRouteSpec, RuntimeSession,
    RuntimeSessionOptions, ShutdownOutcome, UnregisterOutcome,
};
use c2_server::scheduler::AccessLevel;

use crate::client_ffi::{PyRustClient, PyRustClientPool, call_sync_client};
use crate::config_ffi::{
    client_ipc_overrides_to_dict, client_ipc_to_dict, parse_client_ipc_overrides,
    parse_server_ipc_overrides, server_ipc_overrides_to_dict,
};
use crate::http_ffi::{
    PyRustHttpClient, acquire_http_client_from_global_pool, call_relay_aware_http_client,
    http_client_refcount_from_global_pool, release_http_client_from_global_pool,
    shutdown_http_clients_from_global_pool,
};
use crate::lease_ffi::PyBufferLeaseTracker;
use crate::server_ffi::{PyServer, parse_concurrency_mode};

enum RelayConnectedInner {
    Ipc {
        client: Arc<SyncClient>,
        pool: &'static ClientPool,
    },
    Http {
        client: Arc<RelayAwareHttpClient>,
    },
}

enum RelayIpcConnectError {
    Config(PyErr),
    ContractMismatch(PyErr),
    Unavailable,
}

#[pyclass(name = "RelayConnectedClient", frozen)]
pub struct PyRelayConnectedClient {
    mode: String,
    target: String,
    inner: RelayConnectedInner,
    closed: Mutex<bool>,
}

impl PyRelayConnectedClient {
    fn close_inner(&self) {
        let mut closed = self.closed.lock();
        if *closed {
            return;
        }
        *closed = true;
        match &self.inner {
            RelayConnectedInner::Ipc { pool, .. } => pool.release(&self.target),
            RelayConnectedInner::Http { .. } => {}
        }
    }
}

impl Drop for PyRelayConnectedClient {
    fn drop(&mut self) {
        self.close_inner();
    }
}

#[pymethods]
impl PyRelayConnectedClient {
    #[getter]
    fn mode(&self) -> &str {
        &self.mode
    }

    #[getter]
    fn target(&self) -> &str {
        &self.target
    }

    fn call<'py>(
        &self,
        py: Python<'py>,
        route_name: &str,
        method_name: &str,
        data: &[u8],
    ) -> PyResult<Py<PyAny>> {
        if *self.closed.lock() {
            return Err(PyRuntimeError::new_err("relay connected client is closed"));
        }
        match &self.inner {
            RelayConnectedInner::Ipc { client, .. } => {
                let response = call_sync_client(py, client, route_name, method_name, data)?;
                Ok(Py::new(py, response)?.into_any())
            }
            RelayConnectedInner::Http { client } => {
                Ok(call_relay_aware_http_client(py, client, method_name, data)?
                    .into_any()
                    .unbind())
            }
        }
    }

    fn close(&self) {
        self.close_inner();
    }
}

#[pyclass(name = "RuntimeSession", frozen)]
pub struct PyRuntimeSession {
    inner: RuntimeSession,
    lease_tracker: Arc<BufferLeaseTracker>,
    server_bridge: Mutex<Option<Py<PyAny>>>,
    pool: PyRustClientPool,
}

#[pymethods]
impl PyRuntimeSession {
    #[new]
    #[pyo3(signature = (server_id=None, server_ipc_overrides=None, client_ipc_overrides=None, shm_threshold=None))]
    fn new(
        server_id: Option<String>,
        server_ipc_overrides: Option<&Bound<'_, PyAny>>,
        client_ipc_overrides: Option<&Bound<'_, PyAny>>,
        shm_threshold: Option<u64>,
    ) -> PyResult<Self> {
        let server_ipc_overrides = match server_ipc_overrides {
            Some(overrides) => Some(parse_server_ipc_overrides(Some(overrides))?),
            None => None,
        };
        let client_ipc_overrides = match client_ipc_overrides {
            Some(overrides) => Some(parse_client_ipc_overrides(Some(overrides))?),
            None => None,
        };
        let inner = RuntimeSession::new(RuntimeSessionOptions {
            server_id,
            server_ipc_overrides,
            client_ipc_overrides,
            shm_threshold,
            relay_anchor_address: None,
        })
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
        Ok(Self {
            inner,
            lease_tracker: Arc::new(BufferLeaseTracker::default()),
            server_bridge: Mutex::new(None),
            pool: PyRustClientPool::global(),
        })
    }

    fn lease_tracker(&self) -> PyBufferLeaseTracker {
        PyBufferLeaseTracker::from_arc(Arc::clone(&self.lease_tracker))
    }

    fn hold_stats<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        PyBufferLeaseTracker::from_arc(Arc::clone(&self.lease_tracker)).stats_dict(py)
    }

    fn sweep_hold_leases<'py>(
        &self,
        py: Python<'py>,
        threshold_seconds: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        PyBufferLeaseTracker::from_arc(Arc::clone(&self.lease_tracker))
            .sweep_retained_list(py, threshold_seconds)
    }

    fn ensure_server<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let identity = self
            .inner
            .ensure_server()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let dict = PyDict::new(py);
        dict.set_item("server_id", identity.server_id)?;
        dict.set_item("server_instance_id", identity.server_instance_id)?;
        dict.set_item("ipc_address", identity.ipc_address)?;
        Ok(dict)
    }

    #[getter]
    fn server_id(&self) -> Option<String> {
        self.inner.server_id()
    }

    #[getter]
    fn server_id_override(&self) -> Option<String> {
        self.inner.server_id_override()
    }

    #[getter]
    fn server_address(&self) -> Option<String> {
        self.inner.server_address()
    }

    #[getter]
    fn server_ipc_overrides<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        self.inner
            .server_ipc_overrides()
            .map(|overrides| server_ipc_overrides_to_dict(py, &overrides))
            .transpose()
    }

    fn set_server_options(
        &self,
        server_id: Option<String>,
        server_ipc_overrides: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let server_ipc_overrides = match server_ipc_overrides {
            Some(overrides) => Some(parse_server_ipc_overrides(Some(overrides))?),
            None => None,
        };
        self.inner
            .set_server_options(server_id, server_ipc_overrides)
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        self.server_bridge.lock().take();
        Ok(())
    }

    fn set_relay_anchor_address(&self, relay_anchor_address: Option<String>) {
        self.inner.set_relay_anchor_address(relay_anchor_address);
    }

    #[getter]
    fn relay_anchor_address_override(&self) -> Option<String> {
        self.inner.relay_anchor_address_override()
    }

    #[getter]
    fn effective_relay_anchor_address(&self) -> PyResult<Option<String>> {
        self.inner
            .effective_relay_anchor_address()
            .map_err(runtime_error_to_py)
    }

    #[getter]
    fn client_ipc_overrides<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        self.inner
            .client_ipc_overrides()
            .map(|overrides| client_ipc_overrides_to_dict(py, &overrides))
            .transpose()
    }

    #[getter]
    fn client_ipc_config<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let mut runtime_overrides = c2_config::RuntimeConfigOverrides::default();
        runtime_overrides.client_ipc = self.inner.client_ipc_overrides().unwrap_or_default();
        runtime_overrides.shm_threshold = self.inner.shm_threshold_override();
        let resolved = c2_config::ConfigResolver::resolve_client_ipc(
            runtime_overrides.client_ipc.clone(),
            runtime_overrides,
            c2_config::ConfigSources::from_process(),
        )
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
        client_ipc_to_dict(py, &resolved)
    }

    #[getter]
    fn client_config_frozen(&self) -> bool {
        self.inner.client_config_frozen()
    }

    fn set_client_ipc_overrides(&self, overrides: Option<&Bound<'_, PyAny>>) -> PyResult<bool> {
        let overrides = match overrides {
            Some(overrides) => Some(parse_client_ipc_overrides(Some(overrides))?),
            None => None,
        };
        match self.inner.set_client_ipc_overrides(overrides) {
            Ok(()) => Ok(true),
            Err(c2_runtime::RuntimeSessionError::ClientConfigFrozen) => Ok(false),
            Err(e) => Err(PyValueError::new_err(e.to_string())),
        }
    }

    fn ensure_server_bridge<'py>(&self, py: Python<'py>) -> PyResult<Py<PyAny>> {
        if let Some(server) = self.server_bridge.lock().as_ref() {
            return Ok(server.clone_ref(py));
        }

        let identity = self
            .inner
            .ensure_server()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let module = py.import("c_two.transport.server.native")?;
        let class = module.getattr("NativeServerBridge")?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("bind_address", &identity.ipc_address)?;
        kwargs.set_item("server_id", &identity.server_id)?;
        kwargs.set_item("server_instance_id", &identity.server_instance_id)?;
        if let Some(overrides) = self.inner.server_ipc_overrides() {
            kwargs.set_item(
                "ipc_overrides",
                server_ipc_overrides_to_dict(py, &overrides)?,
            )?;
        }
        kwargs.set_item("lease_tracker", self.lease_tracker())?;
        let server = class.call((), Some(&kwargs))?;
        let server_obj = server.unbind();
        *self.server_bridge.lock() = Some(server_obj.clone_ref(py));
        Ok(server_obj)
    }

    #[pyo3(signature = (server_bridge, name, dispatcher, method_names, access_map, concurrency_mode, max_pending=None, max_workers=None, crm_ns="", crm_name="", crm_ver="", relay_anchor_address=None))]
    fn register_route<'py>(
        &self,
        py: Python<'py>,
        server_bridge: Py<PyAny>,
        name: &str,
        dispatcher: Py<PyAny>,
        method_names: Vec<String>,
        access_map: &Bound<'_, PyDict>,
        concurrency_mode: &str,
        max_pending: Option<usize>,
        max_workers: Option<usize>,
        crm_ns: &str,
        crm_name: &str,
        crm_ver: &str,
        relay_anchor_address: Option<&str>,
    ) -> PyResult<(
        Bound<'py, PyDict>,
        crate::route_concurrency_ffi::PyRouteConcurrency,
    )> {
        let server_ref = server_bridge.bind(py).extract::<PyRef<PyServer>>()?;
        let built = server_ref.build_route(
            py,
            name,
            dispatcher,
            method_names.clone(),
            access_map,
            concurrency_mode,
            max_pending,
            max_workers,
            crm_ns,
            crm_name,
            crm_ver,
        )?;
        let mut native_access_map = std::collections::HashMap::new();
        for (key, value) in access_map.iter() {
            let idx: u16 = key.extract()?;
            let level_str: String = value.extract()?;
            let level = match level_str.as_str() {
                "read" => AccessLevel::Read,
                "write" => AccessLevel::Write,
                other => {
                    return Err(PyRuntimeError::new_err(format!(
                        "invalid access level '{other}', expected 'read' or 'write'"
                    )));
                }
            };
            native_access_map.insert(idx, level);
        }
        let spec = RuntimeRouteSpec {
            name: name.to_string(),
            crm_ns: crm_ns.to_string(),
            crm_name: crm_name.to_string(),
            crm_ver: crm_ver.to_string(),
            method_names,
            access_map: native_access_map,
            concurrency_mode: parse_concurrency_mode(concurrency_mode)?,
            max_pending,
            max_workers,
        };
        let relay_use_proxy = resolve_relay_use_proxy_if_needed(&self.inner, relay_anchor_address)?;
        let outcome = self
            .inner
            .register_route(
                &server_ref.inner,
                built.route,
                spec,
                relay_anchor_address,
                relay_use_proxy,
            )
            .map_err(runtime_error_to_py)?;
        Ok((register_outcome_to_dict(py, outcome)?, built.route_handle))
    }

    #[pyo3(signature = (server_bridge, name, relay_anchor_address=None))]
    fn unregister_route<'py>(
        &self,
        py: Python<'py>,
        server_bridge: Py<PyAny>,
        name: &str,
        relay_anchor_address: Option<&str>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let server_ref = server_bridge
            .bind(py)
            .extract::<PyRef<PyServer>>()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let relay_use_proxy = resolve_relay_use_proxy_if_needed(&self.inner, relay_anchor_address)?;
        let outcome = self
            .inner
            .unregister_route(
                &server_ref.inner,
                name,
                relay_anchor_address,
                relay_use_proxy,
            )
            .map_err(runtime_error_to_py)?;
        unregister_outcome_to_dict(py, outcome)
    }

    #[pyo3(signature = (server_bridge=None, route_names=None, relay_anchor_address=None))]
    fn shutdown<'py>(
        &self,
        py: Python<'py>,
        server_bridge: Option<Py<PyAny>>,
        route_names: Option<Vec<String>>,
        relay_anchor_address: Option<&str>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let relay_use_proxy = resolve_relay_use_proxy_if_needed(&self.inner, relay_anchor_address)?;
        let route_names = route_names.unwrap_or_default();
        let server_bridge = match server_bridge {
            Some(server_bridge) => Some(server_bridge),
            None => self
                .server_bridge
                .lock()
                .as_ref()
                .map(|server| server.clone_ref(py)),
        };
        let mut outcome = if let Some(server_bridge) = server_bridge {
            let server_ref = server_bridge
                .bind(py)
                .extract::<PyRef<PyServer>>()
                .map_err(|e| PyValueError::new_err(e.to_string()))?;
            let mut outcome = self.inner.shutdown(
                Some(&server_ref.inner),
                route_names,
                relay_anchor_address,
                relay_use_proxy,
            );
            outcome.server_was_started = server_ref.runtime_is_running();
            outcome
        } else {
            self.inner
                .shutdown(None, route_names, relay_anchor_address, relay_use_proxy)
        };
        py.detach(|| self.pool.inner.shutdown_all());
        outcome.ipc_clients_drained = true;
        py.detach(|| shutdown_http_clients_from_global_pool());
        outcome.http_clients_drained = true;
        self.server_bridge.lock().take();
        shutdown_outcome_to_dict(py, outcome)
    }

    fn clear_server_identity(&self) {
        self.inner.clear_server_identity();
        self.server_bridge.lock().take();
    }

    fn clear_relay_projection_cache(&self) {
        self.inner.clear_relay_projection_cache();
    }

    #[pyo3(signature = (address, route_name="", expected_crm_ns="", expected_crm_name="", expected_crm_ver=""))]
    fn acquire_ipc_client(
        &self,
        py: Python<'_>,
        address: &str,
        route_name: &str,
        expected_crm_ns: &str,
        expected_crm_name: &str,
        expected_crm_ver: &str,
    ) -> PyResult<PyRustClient> {
        let addr = address.to_string();
        let pool = self.pool.inner;
        let mut runtime_overrides = c2_config::RuntimeConfigOverrides::default();
        runtime_overrides.client_ipc = self.inner.client_ipc_overrides().unwrap_or_default();
        runtime_overrides.shm_threshold = self.inner.shm_threshold_override();
        let cfg = c2_config::ConfigResolver::resolve_client_ipc(
            runtime_overrides.client_ipc.clone(),
            runtime_overrides,
            c2_config::ConfigSources::from_process(),
        )
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let client = py
            .detach(move || pool.acquire(&addr, Some(&cfg)))
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e}")))?;
        if !route_name.is_empty() {
            if let Err(err) = client.validate_route_contract(
                route_name,
                expected_crm_ns,
                expected_crm_name,
                expected_crm_ver,
            ) {
                self.pool.inner.release(address);
                return Err(pyo3::exceptions::PyRuntimeError::new_err(format!("{err}")));
            }
        }
        self.inner.mark_client_config_frozen();
        Ok(PyRustClient::from_arc(client))
    }

    fn release_ipc_client(&self, address: &str) {
        self.pool.inner.release(address);
    }

    fn ipc_client_refcount(&self, address: &str) -> usize {
        self.pool.inner.refcount(address)
    }

    fn acquire_http_client(&self, py: Python<'_>, address: &str) -> PyResult<PyRustHttpClient> {
        let addr = address.to_string();
        let client = py
            .detach(move || {
                let use_proxy = c2_config::ConfigResolver::resolve_relay_use_proxy(
                    c2_config::ConfigSources::from_process(),
                )
                .map_err(|e| c2_http::client::HttpError::Transport(e.to_string()))?;
                let call_timeout_secs = c2_config::ConfigResolver::resolve_relay_call_timeout_secs(
                    c2_config::ConfigSources::from_process(),
                )
                .map_err(|e| c2_http::client::HttpError::Transport(e.to_string()))?;
                acquire_http_client_from_global_pool(&addr, use_proxy, call_timeout_secs)
            })
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e}")))?;
        Ok(PyRustHttpClient::from_arc(client))
    }

    #[pyo3(signature = (address, route_name, expected_crm_ns="", expected_crm_name="", expected_crm_ver=""))]
    fn connect_explicit_relay_http(
        &self,
        py: Python<'_>,
        address: &str,
        route_name: &str,
        expected_crm_ns: &str,
        expected_crm_name: &str,
        expected_crm_ver: &str,
    ) -> PyResult<PyRelayConnectedClient> {
        let use_proxy = resolve_relay_use_proxy_if_needed(&self.inner, Some(address))?;
        let max_attempts = c2_config::ConfigResolver::resolve_relay_route_max_attempts(
            c2_config::ConfigSources::from_process(),
        )
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let call_timeout_secs = c2_config::ConfigResolver::resolve_relay_call_timeout_secs(
            c2_config::ConfigSources::from_process(),
        )
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let (client, relay_url) = py
            .detach(|| {
                self.inner.connect_explicit_relay_http_client(
                    address,
                    route_name,
                    use_proxy,
                    max_attempts,
                    call_timeout_secs,
                    expected_crm_ns,
                    expected_crm_name,
                    expected_crm_ver,
                )
            })
            .map_err(runtime_error_to_py)?;
        Ok(PyRelayConnectedClient {
            mode: "http".to_string(),
            target: relay_url,
            inner: RelayConnectedInner::Http {
                client: Arc::new(client),
            },
            closed: Mutex::new(false),
        })
    }

    fn release_http_client(&self, address: &str) {
        release_http_client_from_global_pool(address);
    }

    fn http_client_refcount(&self, address: &str) -> usize {
        http_client_refcount_from_global_pool(address)
    }

    fn shutdown_http_clients(&self, py: Python<'_>) {
        py.detach(|| shutdown_http_clients_from_global_pool());
    }

    #[pyo3(signature = (route_name, expected_crm_ns="", expected_crm_name="", expected_crm_ver=""))]
    fn connect_via_relay(
        &self,
        py: Python<'_>,
        route_name: &str,
        expected_crm_ns: &str,
        expected_crm_name: &str,
        expected_crm_ver: &str,
    ) -> PyResult<PyRelayConnectedClient> {
        let relay_anchor_address = self
            .inner
            .effective_relay_anchor_address()
            .map_err(runtime_error_to_py)?
            .ok_or_else(|| PyLookupError::new_err("no relay address configured"))?;
        let use_proxy =
            resolve_relay_use_proxy_if_needed(&self.inner, Some(relay_anchor_address.as_str()))?;
        let max_attempts = c2_config::ConfigResolver::resolve_relay_route_max_attempts(
            c2_config::ConfigSources::from_process(),
        )
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let call_timeout_secs = c2_config::ConfigResolver::resolve_relay_call_timeout_secs(
            c2_config::ConfigSources::from_process(),
        )
        .map_err(|e| PyValueError::new_err(e.to_string()))?;

        let target = py
            .detach(|| {
                self.inner.resolve_relay_connection(
                    route_name,
                    use_proxy,
                    max_attempts,
                    call_timeout_secs,
                    expected_crm_ns,
                    expected_crm_name,
                    expected_crm_ver,
                )
            })
            .map_err(runtime_error_to_py)?;
        match target {
            RelayResolvedConnection::Ipc {
                address,
                server_id,
                server_instance_id,
            } => {
                match self.acquire_relay_ipc_client(
                    py,
                    route_name,
                    &address,
                    &server_id,
                    &server_instance_id,
                    expected_crm_ns,
                    expected_crm_name,
                    expected_crm_ver,
                ) {
                    Ok(client) => Ok(PyRelayConnectedClient {
                        mode: "ipc".to_string(),
                        target: address,
                        inner: RelayConnectedInner::Ipc {
                            client,
                            pool: self.pool.inner,
                        },
                        closed: Mutex::new(false),
                    }),
                    Err(RelayIpcConnectError::Config(err)) => Err(err),
                    Err(RelayIpcConnectError::ContractMismatch(err)) => Err(err),
                    Err(RelayIpcConnectError::Unavailable) => self.acquire_relay_http_client(
                        py,
                        route_name,
                        use_proxy,
                        max_attempts,
                        call_timeout_secs,
                        expected_crm_ns,
                        expected_crm_name,
                        expected_crm_ver,
                    ),
                }
            }
            RelayResolvedConnection::Http { client, relay_url } => Ok(PyRelayConnectedClient {
                mode: "http".to_string(),
                target: relay_url,
                inner: RelayConnectedInner::Http {
                    client: Arc::new(client),
                },
                closed: Mutex::new(false),
            }),
        }
    }

    fn shutdown_ipc_clients(&self, py: Python<'_>) {
        py.detach(|| self.pool.inner.shutdown_all());
    }
}

impl PyRuntimeSession {
    fn acquire_relay_ipc_client(
        &self,
        py: Python<'_>,
        route_name: &str,
        address: &str,
        expected_server_id: &str,
        expected_server_instance_id: &str,
        expected_crm_ns: &str,
        expected_crm_name: &str,
        expected_crm_ver: &str,
    ) -> Result<Arc<SyncClient>, RelayIpcConnectError> {
        let addr = address.to_string();
        let pool = self.pool.inner;
        let mut runtime_overrides = c2_config::RuntimeConfigOverrides::default();
        runtime_overrides.client_ipc = self.inner.client_ipc_overrides().unwrap_or_default();
        runtime_overrides.shm_threshold = self.inner.shm_threshold_override();
        let cfg = c2_config::ConfigResolver::resolve_client_ipc(
            runtime_overrides.client_ipc.clone(),
            runtime_overrides,
            c2_config::ConfigSources::from_process(),
        )
        .map_err(|e| RelayIpcConnectError::Config(PyValueError::new_err(e.to_string())))?;
        let client_result = py.detach({
            let addr = addr.clone();
            move || pool.acquire(&addr, Some(&cfg))
        });
        let client = match client_result {
            Ok(client) => client,
            Err(IpcError::Config(message)) => {
                return Err(RelayIpcConnectError::Config(PyValueError::new_err(message)));
            }
            Err(_) => return Err(RelayIpcConnectError::Unavailable),
        };

        let identity_matches = client.server_identity().is_some_and(|identity| {
            identity.server_id == expected_server_id
                && identity.server_instance_id == expected_server_instance_id
        });
        if !identity_matches {
            pool.release(&addr);
            return Err(RelayIpcConnectError::Unavailable);
        }

        if !client
            .route_names()
            .into_iter()
            .any(|registered| registered == route_name)
        {
            pool.release(&addr);
            return Err(RelayIpcConnectError::Unavailable);
        }
        if let Err(err) = client.validate_route_contract(
            route_name,
            expected_crm_ns,
            expected_crm_name,
            expected_crm_ver,
        ) {
            pool.release(&addr);
            return Err(RelayIpcConnectError::ContractMismatch(
                PyRuntimeError::new_err(err.to_string()),
            ));
        }
        self.inner.mark_client_config_frozen();
        Ok(client)
    }

    fn acquire_relay_http_client(
        &self,
        py: Python<'_>,
        route_name: &str,
        use_proxy: bool,
        max_attempts: usize,
        call_timeout_secs: f64,
        expected_crm_ns: &str,
        expected_crm_name: &str,
        expected_crm_ver: &str,
    ) -> PyResult<PyRelayConnectedClient> {
        let (client, relay_url) = py
            .detach(|| {
                self.inner.connect_relay_http_client(
                    route_name,
                    use_proxy,
                    max_attempts,
                    call_timeout_secs,
                    expected_crm_ns,
                    expected_crm_name,
                    expected_crm_ver,
                )
            })
            .map_err(runtime_error_to_py)?;
        Ok(PyRelayConnectedClient {
            mode: "http".to_string(),
            target: relay_url,
            inner: RelayConnectedInner::Http {
                client: Arc::new(client),
            },
            closed: Mutex::new(false),
        })
    }
}

fn runtime_error_to_py(err: c2_runtime::RuntimeSessionError) -> PyErr {
    match err {
        c2_runtime::RuntimeSessionError::InvalidServerId(message) => PyValueError::new_err(message),
        c2_runtime::RuntimeSessionError::ClientConfigFrozen => {
            PyValueError::new_err(err.to_string())
        }
        c2_runtime::RuntimeSessionError::DuplicateRoute(message) => {
            let exc = PyRuntimeError::new_err(message);
            Python::attach(|py| {
                exc.value(py).setattr("status_code", 409).ok();
            });
            exc
        }
        c2_runtime::RuntimeSessionError::MissingRoute(message) => PyKeyError::new_err(message),
        c2_runtime::RuntimeSessionError::MissingRelayAddress => {
            PyLookupError::new_err(err.to_string())
        }
        c2_runtime::RuntimeSessionError::RelayDuplicateRoute(message) => {
            let exc = PyRuntimeError::new_err(message);
            Python::attach(|py| {
                let value = exc.value(py);
                value.setattr("status_code", 409).ok();
                value.setattr("relay_duplicate", true).ok();
            });
            exc
        }
        c2_runtime::RuntimeSessionError::RelayHttp {
            status_code,
            message,
        } => {
            let exc = PyRuntimeError::new_err(format!("HTTP {status_code}: {message}"));
            Python::attach(|py| {
                let value = exc.value(py);
                value.setattr("status_code", status_code).ok();
                value.setattr("body", message).ok();
            });
            exc
        }
        c2_runtime::RuntimeSessionError::Server(message) => PyRuntimeError::new_err(message),
        c2_runtime::RuntimeSessionError::Relay(message) => {
            PyRuntimeError::new_err(format!("relay error: {message}"))
        }
    }
}

fn resolve_relay_use_proxy_if_needed(
    session: &RuntimeSession,
    relay_anchor_address: Option<&str>,
) -> PyResult<bool> {
    let has_relay = match relay_anchor_address {
        Some(_) => true,
        None => session
            .effective_relay_anchor_address()
            .map_err(runtime_error_to_py)?
            .is_some(),
    };
    if !has_relay {
        return Ok(false);
    }
    c2_config::ConfigResolver::resolve_relay_use_proxy(c2_config::ConfigSources::from_process())
        .map_err(|e| PyValueError::new_err(e.to_string()))
}

fn register_outcome_to_dict<'py>(
    py: Python<'py>,
    outcome: RegisterOutcome,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("route_name", outcome.route_name)?;
    dict.set_item("server_id", outcome.server_id)?;
    dict.set_item("server_instance_id", outcome.server_instance_id)?;
    dict.set_item("ipc_address", outcome.ipc_address)?;
    dict.set_item("relay_registered", outcome.relay_registered)?;
    Ok(dict)
}

fn relay_cleanup_error_to_dict<'py>(
    py: Python<'py>,
    error: RelayCleanupError,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("route_name", error.route_name)?;
    dict.set_item("status_code", error.status_code)?;
    dict.set_item("message", error.message)?;
    Ok(dict)
}

fn unregister_outcome_to_dict<'py>(
    py: Python<'py>,
    outcome: UnregisterOutcome,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("route_name", outcome.route_name)?;
    dict.set_item("local_removed", outcome.local_removed)?;
    match outcome.relay_error {
        Some(error) => dict.set_item("relay_error", relay_cleanup_error_to_dict(py, error)?)?,
        None => dict.set_item("relay_error", py.None())?,
    }
    Ok(dict)
}

fn shutdown_outcome_to_dict<'py>(
    py: Python<'py>,
    outcome: ShutdownOutcome,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("removed_routes", outcome.removed_routes)?;
    let relay_errors = outcome
        .relay_errors
        .into_iter()
        .map(|error| relay_cleanup_error_to_dict(py, error).map(|dict| dict.unbind()))
        .collect::<PyResult<Vec<_>>>()?;
    dict.set_item("relay_errors", PyList::new(py, relay_errors)?)?;
    dict.set_item("server_was_started", outcome.server_was_started)?;
    dict.set_item("ipc_clients_drained", outcome.ipc_clients_drained)?;
    dict.set_item("http_clients_drained", outcome.http_clients_drained)?;
    Ok(dict)
}

pub fn register_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyRelayConnectedClient>()?;
    m.add_class::<PyRuntimeSession>()?;
    Ok(())
}
