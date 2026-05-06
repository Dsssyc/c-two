//! PyO3 bindings for the Rust-owned process runtime session.
//!
//! Phase 1 exposes server identity/address authority. Later phases will move
//! server lifecycle, route transactions, pools, and relay projection here.

use parking_lot::Mutex;
use pyo3::exceptions::{PyKeyError, PyLookupError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use c2_runtime::{
    RegisterOutcome, RelayCleanupError, RuntimeRouteSpec, RuntimeSession, RuntimeSessionOptions,
    ShutdownOutcome, UnregisterOutcome,
};
use c2_server::scheduler::AccessLevel;

use crate::client_ffi::{PyRustClient, PyRustClientPool};
use crate::config_ffi::{
    client_ipc_overrides_to_dict, client_ipc_to_dict, parse_client_ipc_overrides,
    parse_server_ipc_overrides, server_ipc_overrides_to_dict,
};
use crate::http_ffi::{
    PyRustHttpClient, PyRustRelayAwareHttpClient, acquire_http_client_from_global_pool,
    http_client_refcount_from_global_pool, release_http_client_from_global_pool,
    shutdown_http_clients_from_global_pool,
};
use crate::server_ffi::{PyServer, parse_concurrency_mode};

#[pyclass(name = "RuntimeSession", frozen)]
pub struct PyRuntimeSession {
    inner: RuntimeSession,
    server_bridge: Mutex<Option<Py<PyAny>>>,
    pool: PyRustClientPool,
}

#[pymethods]
impl PyRuntimeSession {
    #[new]
    #[pyo3(signature = (server_id=None, server_ipc_overrides=None, client_ipc_overrides=None, shm_threshold=None))]
    fn new(
        server_id: Option<String>,
        server_ipc_overrides: Option<&Bound<'_, PyDict>>,
        client_ipc_overrides: Option<&Bound<'_, PyDict>>,
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
            relay_address: None,
        })
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
        Ok(Self {
            inner,
            server_bridge: Mutex::new(None),
            pool: PyRustClientPool::global(),
        })
    }

    fn ensure_server<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let identity = self
            .inner
            .ensure_server()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let dict = PyDict::new(py);
        dict.set_item("server_id", identity.server_id)?;
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
        server_ipc_overrides: Option<&Bound<'_, PyDict>>,
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

    fn set_relay_address(&self, relay_address: Option<String>) {
        self.inner.set_relay_address(relay_address);
    }

    #[getter]
    fn relay_address_override(&self) -> Option<String> {
        self.inner.relay_address_override()
    }

    #[getter]
    fn effective_relay_address(&self) -> PyResult<Option<String>> {
        self.inner
            .effective_relay_address()
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

    fn set_client_ipc_overrides(&self, overrides: Option<&Bound<'_, PyDict>>) -> PyResult<bool> {
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
        kwargs.set_item("bind_address", identity.ipc_address)?;
        if let Some(overrides) = self.inner.server_ipc_overrides() {
            kwargs.set_item(
                "ipc_overrides",
                server_ipc_overrides_to_dict(py, &overrides)?,
            )?;
        }
        let server = class.call((), Some(&kwargs))?;
        let server_obj = server.unbind();
        *self.server_bridge.lock() = Some(server_obj.clone_ref(py));
        Ok(server_obj)
    }

    #[pyo3(signature = (server_bridge, name, dispatcher, method_names, access_map, concurrency_mode, max_pending=None, max_workers=None, crm_ns="", crm_ver="", relay_address=None))]
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
        crm_ver: &str,
        relay_address: Option<&str>,
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
            crm_ver: crm_ver.to_string(),
            method_names,
            access_map: native_access_map,
            concurrency_mode: parse_concurrency_mode(concurrency_mode)?,
            max_pending,
            max_workers,
        };
        let relay_use_proxy = resolve_relay_use_proxy_if_needed(&self.inner, relay_address)?;
        let outcome = self
            .inner
            .register_route(
                &server_ref.inner,
                built.route,
                spec,
                relay_address,
                relay_use_proxy,
            )
            .map_err(runtime_error_to_py)?;
        Ok((register_outcome_to_dict(py, outcome)?, built.route_handle))
    }

    #[pyo3(signature = (server_bridge, name, relay_address=None))]
    fn unregister_route<'py>(
        &self,
        py: Python<'py>,
        server_bridge: Py<PyAny>,
        name: &str,
        relay_address: Option<&str>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let server_ref = server_bridge
            .bind(py)
            .extract::<PyRef<PyServer>>()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let relay_use_proxy = resolve_relay_use_proxy_if_needed(&self.inner, relay_address)?;
        let outcome = self
            .inner
            .unregister_route(&server_ref.inner, name, relay_address, relay_use_proxy)
            .map_err(runtime_error_to_py)?;
        unregister_outcome_to_dict(py, outcome)
    }

    #[pyo3(signature = (server_bridge=None, route_names=None, relay_address=None))]
    fn shutdown<'py>(
        &self,
        py: Python<'py>,
        server_bridge: Option<Py<PyAny>>,
        route_names: Option<Vec<String>>,
        relay_address: Option<&str>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let relay_use_proxy = resolve_relay_use_proxy_if_needed(&self.inner, relay_address)?;
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
                relay_address,
                relay_use_proxy,
            );
            outcome.server_was_started = server_ref.runtime_is_running();
            outcome
        } else {
            self.inner
                .shutdown(None, route_names, relay_address, relay_use_proxy)
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

    fn acquire_ipc_client(&self, py: Python<'_>, address: &str) -> PyResult<PyRustClient> {
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
                acquire_http_client_from_global_pool(&addr, use_proxy)
            })
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e}")))?;
        Ok(PyRustHttpClient::from_arc(client))
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

    fn connect_via_relay(
        &self,
        py: Python<'_>,
        route_name: &str,
    ) -> PyResult<PyRustRelayAwareHttpClient> {
        let relay_address = self
            .inner
            .effective_relay_address()
            .map_err(runtime_error_to_py)?
            .ok_or_else(|| PyLookupError::new_err("no relay address configured"))?;
        let use_proxy =
            resolve_relay_use_proxy_if_needed(&self.inner, Some(relay_address.as_str()))?;
        let max_attempts = c2_config::ConfigResolver::resolve_relay_route_max_attempts(
            c2_config::ConfigSources::from_process(),
        )
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let client = py
            .detach(|| {
                self.inner
                    .connect_via_relay(route_name, use_proxy, max_attempts)
            })
            .map_err(runtime_error_to_py)?;
        Ok(PyRustRelayAwareHttpClient::from_arc(client))
    }

    fn shutdown_ipc_clients(&self, py: Python<'_>) {
        py.detach(|| self.pool.inner.shutdown_all());
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
    relay_address: Option<&str>,
) -> PyResult<bool> {
    let has_relay = match relay_address {
        Some(_) => true,
        None => session
            .effective_relay_address()
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
    m.add_class::<PyRuntimeSession>()?;
    Ok(())
}
