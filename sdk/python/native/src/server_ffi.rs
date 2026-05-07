//! PyO3 bindings for the C-Two IPC server (`c2-server`).
//!
//! Exposes `RustServer` — a UDS server that hosts CRM routes and dispatches
//! method calls through Python callables.  The server runs on a background
//! OS thread with its own tokio runtime.
//!
//! **GIL handling**: `PyCrmCallback::invoke()` acquires the GIL internally
//! (called from `spawn_blocking` inside tokio). All blocking `PyServer`
//! methods release the GIL via `py.detach()`.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use pyo3::buffer::PyBuffer;
use pyo3::exceptions::{PyRuntimeError, PyTimeoutError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict};

use c2_config::{BaseIpcConfig, ServerIpcConfig};
use c2_error::{C2Error, ErrorCode};
use c2_mem::MemPool;
use c2_server::Server;
use c2_server::dispatcher::{CrmCallback, CrmError, CrmRoute, RequestData, ResponseMeta};
use c2_server::response::try_prepare_shm_response;
use c2_server::scheduler::{AccessLevel, ConcurrencyMode, Scheduler, SchedulerLimits};

use crate::route_concurrency_ffi::PyRouteConcurrency;
use crate::shm_buffer::PyShmBuffer;

pub(crate) fn parse_concurrency_mode(mode: &str) -> PyResult<ConcurrencyMode> {
    match mode {
        "parallel" => Ok(ConcurrencyMode::Parallel),
        "exclusive" => Ok(ConcurrencyMode::Exclusive),
        "read_parallel" => Ok(ConcurrencyMode::ReadParallel),
        other => Err(PyValueError::new_err(format!(
            "invalid concurrency mode '{other}', expected 'parallel', 'exclusive', or 'read_parallel'"
        ))),
    }
}

// ---------------------------------------------------------------------------
// PyCrmCallback — bridges a Python callable to the Rust CrmCallback trait
// ---------------------------------------------------------------------------

pub(crate) struct PyCrmCallback {
    py_callable: Py<PyAny>,
    shm_threshold: u64,
    max_payload_size: u64,
}

// SAFETY: Py<PyAny> is Send when accessed only under the GIL.
// `invoke()` always acquires the GIL via `Python::attach` before
// touching `py_callable`.
unsafe impl Send for PyCrmCallback {}
unsafe impl Sync for PyCrmCallback {}

impl CrmCallback for PyCrmCallback {
    fn invoke(
        &self,
        route_name: &str,
        method_idx: u16,
        request: RequestData,
        response_pool: Arc<parking_lot::RwLock<MemPool>>,
    ) -> Result<ResponseMeta, CrmError> {
        Python::attach(|py| {
            // Convert RequestData → PyShmBuffer
            let shm_buf = match request {
                RequestData::Inline(data) => PyShmBuffer::from_inline(data),
                RequestData::Shm {
                    pool,
                    seg_idx,
                    offset,
                    data_size,
                    is_dedicated,
                } => PyShmBuffer::from_peer_shm(pool, seg_idx, offset, data_size, is_dedicated),
                RequestData::Handle { handle, pool } => PyShmBuffer::from_handle(handle, pool),
            };
            let buf_obj = Py::new(py, shm_buf)
                .map_err(|e| CrmError::InternalError(format!("failed to create ShmBuffer: {e}")))?;

            // Call Python: dispatcher(route_name, method_idx, shm_buffer)
            let args = (route_name, method_idx, buf_obj);
            match self.py_callable.call1(py, args) {
                Ok(result) => parse_response_meta(
                    py,
                    result,
                    &response_pool,
                    self.shm_threshold,
                    self.max_payload_size,
                ),
                Err(e) => {
                    // Check for .error_bytes attribute (CrmCallError from Python).
                    let val = e.value(py);
                    if let Ok(attr) = val.getattr("error_bytes") {
                        if let Ok(b) = attr.cast::<PyBytes>() {
                            return Err(CrmError::UserError(b.as_bytes().to_vec()));
                        }
                    }
                    Err(CrmError::InternalError(format!("{e}")))
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// PyServer — main pyclass
// ---------------------------------------------------------------------------

/// A Rust-native IPC server for hosting CRM routes.
///
/// ```python
/// from c_two._native import RustServer
/// srv = RustServer("ipc://my_server")
/// srv.register_route(
///     "grid", dispatcher_fn, ["step", "query"],
///     {0: "read", 1: "write"}, "read_parallel",
/// )
/// srv.start()
/// # ... serve requests ...
/// srv.shutdown()
/// ```
#[pyclass(name = "RustServer", frozen)]
pub struct PyServer {
    pub(crate) inner: Arc<Server>,
    rt: Mutex<Option<tokio::runtime::Runtime>>,
}

pub(crate) struct BuiltRoute {
    pub route: CrmRoute,
    pub route_handle: PyRouteConcurrency,
}

impl PyServer {
    pub(crate) fn build_route(
        &self,
        _py: Python<'_>,
        name: &str,
        dispatcher: Py<PyAny>,
        method_names: Vec<String>,
        access_map: &Bound<'_, PyDict>,
        concurrency_mode: &str,
        max_pending: Option<usize>,
        max_workers: Option<usize>,
    ) -> PyResult<BuiltRoute> {
        let mode = parse_concurrency_mode(concurrency_mode)?;
        let limits = SchedulerLimits::try_from_usize(max_pending, max_workers)
            .map_err(PyValueError::new_err)?;

        // Parse access_map: {int: "read"|"write"} → HashMap<u16, AccessLevel>
        let mut map = HashMap::new();
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
            map.insert(idx, level);
        }

        let scheduler = Scheduler::with_limits(mode, map, limits);
        let route_handle = PyRouteConcurrency::new(scheduler.clone());
        let callback = Arc::new(PyCrmCallback {
            py_callable: dispatcher,
            shm_threshold: self.inner.response_shm_threshold(),
            max_payload_size: self.inner.response_max_payload_size(),
        });

        let route = CrmRoute {
            name: name.to_string(),
            scheduler: Arc::new(scheduler),
            callback,
            method_names,
        };

        Ok(BuiltRoute {
            route,
            route_handle,
        })
    }

    pub(crate) fn register_built_route(&self, py: Python<'_>, route: CrmRoute) -> PyResult<()> {
        let server = Arc::clone(&self.inner);

        // Register requires async; use a short-lived runtime if the main
        // one hasn't started, or block_on the existing runtime's handle.
        py.detach(move || -> PyResult<()> {
            let rt_guard = self.rt.lock();
            if let Some(rt) = rt_guard.as_ref() {
                rt.block_on(server.register_route(route))
                    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            } else {
                // Server not started yet — create a temporary runtime for registration.
                let tmp_rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| {
                        PyRuntimeError::new_err(format!("failed to create runtime: {e}"))
                    })?;
                tmp_rt
                    .block_on(server.register_route(route))
                    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            }
            Ok(())
        })
    }

    pub(crate) fn unregister_route_blocking(&self, py: Python<'_>, name: &str) -> PyResult<bool> {
        let server = Arc::clone(&self.inner);
        let name = name.to_string();

        py.detach(move || {
            let rt_guard = self.rt.lock();
            if let Some(rt) = rt_guard.as_ref() {
                Ok(rt.block_on(server.unregister_route(&name)))
            } else {
                let tmp_rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| PyRuntimeError::new_err(format!("{e}")))?;
                Ok(tmp_rt.block_on(server.unregister_route(&name)))
            }
        })
    }

    pub(crate) fn runtime_is_running(&self) -> bool {
        self.inner.is_running()
    }

    fn stop_runtime_and_wait(&self, timeout: Duration) -> PyResult<()> {
        self.inner.shutdown();

        let rt = {
            let mut rt_guard = self.rt.lock();
            rt_guard.take()
        };

        let wait_result = if let Some(rt) = rt {
            let result = rt.block_on(self.inner.wait_until_stopped(timeout));
            rt.shutdown_background();
            result
        } else {
            Ok(())
        };
        self.inner.finalize_runtime_stopped();
        wait_result.map_err(|e| PyRuntimeError::new_err(e.to_string()))
    }

    fn start_runtime_and_wait(&self, timeout: Duration) -> PyResult<()> {
        let server_for_run = Arc::clone(&self.inner);
        let server_for_wait = Arc::clone(&self.inner);

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| PyRuntimeError::new_err(format!("failed to create runtime: {e}")))?;

        {
            let mut rt_guard = self.rt.lock();
            if rt_guard.is_some() {
                return Err(PyRuntimeError::new_err("server is already running"));
            }

            self.inner
                .begin_start_attempt()
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

            rt.spawn(async move {
                if let Err(e) = server_for_run.run().await {
                    eprintln!("c2-server error: {e}");
                }
            });

            *rt_guard = Some(rt);
        }

        let readiness = {
            let rt_guard = self.rt.lock();
            let rt = rt_guard
                .as_ref()
                .ok_or_else(|| PyRuntimeError::new_err("server runtime missing after start"))?;
            rt.block_on(server_for_wait.wait_until_ready(timeout))
        };

        match readiness {
            Ok(()) => Ok(()),
            Err(err) => {
                let message = err.to_string();
                let cleanup = self.stop_runtime_and_wait(Duration::from_secs(5));
                let message = if let Err(cleanup_err) = cleanup {
                    format!("{message}; cleanup failed: {cleanup_err}")
                } else {
                    message
                };
                if message.contains("did not become ready") {
                    Err(PyTimeoutError::new_err(message))
                } else {
                    Err(PyRuntimeError::new_err(message))
                }
            }
        }
    }
}

#[pymethods]
impl PyServer {
    /// Create a new server targeting the given IPC address.
    #[new]
    #[pyo3(signature = (
        address, shm_threshold, pool_enabled, pool_segment_size,
        max_pool_segments, reassembly_segment_size,
        reassembly_max_segments, max_frame_size, max_payload_size,
        max_pending_requests, pool_decay_seconds, heartbeat_interval,
        heartbeat_timeout, max_total_chunks, chunk_gc_interval,
        chunk_threshold_ratio, chunk_assembler_timeout,
        max_reassembly_bytes, chunk_size,
    ))]
    fn new(
        address: &str,
        shm_threshold: u64,
        pool_enabled: bool,
        pool_segment_size: u64,
        max_pool_segments: u32,
        reassembly_segment_size: u64,
        reassembly_max_segments: u32,
        max_frame_size: u64,
        max_payload_size: u64,
        max_pending_requests: u32,
        pool_decay_seconds: f64,
        heartbeat_interval: f64,
        heartbeat_timeout: f64,
        max_total_chunks: u32,
        chunk_gc_interval: f64,
        chunk_threshold_ratio: f64,
        chunk_assembler_timeout: f64,
        max_reassembly_bytes: u64,
        chunk_size: u64,
    ) -> PyResult<Self> {
        let config = ServerIpcConfig {
            base: BaseIpcConfig {
                pool_enabled,
                pool_segment_size,
                max_pool_segments,
                max_pool_memory: pool_segment_size
                    .checked_mul(u64::from(max_pool_segments))
                    .ok_or_else(|| {
                        PyRuntimeError::new_err("max_pool_memory derived value overflowed")
                    })?,
                reassembly_segment_size,
                reassembly_max_segments,
                max_total_chunks,
                chunk_gc_interval_secs: chunk_gc_interval,
                chunk_threshold_ratio,
                chunk_assembler_timeout_secs: chunk_assembler_timeout,
                max_reassembly_bytes,
                chunk_size,
            },
            shm_threshold,
            max_frame_size,
            max_payload_size,
            max_pending_requests,
            pool_decay_seconds,
            heartbeat_interval_secs: heartbeat_interval,
            heartbeat_timeout_secs: heartbeat_timeout,
        };
        config
            .validate()
            .map_err(|e| PyValueError::new_err(format!("invalid IPC server config: {e}")))?;
        let server =
            Server::new(address, config).map_err(|e| PyRuntimeError::new_err(format!("{e}")))?;
        Ok(Self {
            inner: Arc::new(server),
            rt: Mutex::new(None),
        })
    }

    /// Register a CRM route.
    ///
    /// `dispatcher` is a Python callable:
    /// `(route_name: str, method_idx: int, request_buffer: ShmBuffer) -> None | bytes-like`
    ///
    /// Return value conventions:
    /// - `None` → empty response
    /// - `bytes` / buffer-protocol object → native response selection
    ///
    /// `method_names` lists the CRM method names indexed by method_idx.
    /// `access_map` maps method_idx → "read" or "write".
    #[pyo3(signature = (name, dispatcher, method_names, access_map, concurrency_mode, max_pending=None, max_workers=None))]
    fn register_route(
        &self,
        py: Python<'_>,
        name: &str,
        dispatcher: Py<PyAny>,
        method_names: Vec<String>,
        access_map: &Bound<'_, PyDict>,
        concurrency_mode: &str,
        max_pending: Option<usize>,
        max_workers: Option<usize>,
    ) -> PyResult<PyRouteConcurrency> {
        let built = self.build_route(
            py,
            name,
            dispatcher,
            method_names,
            access_map,
            concurrency_mode,
            max_pending,
            max_workers,
        )?;
        let route_handle = built.route_handle.clone();
        self.register_built_route(py, built.route)?;
        Ok(route_handle)
    }

    /// Start the server on a background thread with a tokio runtime.
    ///
    /// The GIL is released while the runtime spins up.
    fn start(&self, py: Python<'_>) -> PyResult<()> {
        self.start_and_wait(py, 5.0)
    }

    /// Start the server and wait until the native IPC listener is ready.
    #[pyo3(signature = (timeout_seconds=5.0))]
    fn start_and_wait(&self, py: Python<'_>, timeout_seconds: f64) -> PyResult<()> {
        if !timeout_seconds.is_finite() || timeout_seconds < 0.0 {
            return Err(PyValueError::new_err(
                "timeout_seconds must be a non-negative finite number",
            ));
        }
        let timeout = Duration::from_secs_f64(timeout_seconds);
        py.detach(|| self.start_runtime_and_wait(timeout))
    }

    /// Gracefully shut down the server.
    ///
    /// Signals the accept loop to stop, then drops the tokio runtime
    /// (joining all worker threads). The GIL is released during shutdown.
    fn shutdown(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| self.stop_runtime_and_wait(Duration::from_secs(5)))
    }

    /// Remove a CRM route. Returns `True` if it existed.
    fn unregister_route(&self, py: Python<'_>, name: &str) -> PyResult<bool> {
        self.unregister_route_blocking(py, name)
    }

    /// Whether the server is currently running.
    #[getter]
    fn is_running(&self) -> bool {
        self.runtime_is_running()
    }

    /// Whether the server has reached native IPC readiness.
    #[getter]
    fn is_ready(&self) -> bool {
        self.inner.is_ready()
    }

    /// The filesystem path of the UDS socket.
    #[getter]
    fn socket_path(&self) -> String {
        self.inner.socket_path().to_string_lossy().into_owned()
    }
}

// ---------------------------------------------------------------------------
// parse_response_meta — convert Python return value to ResponseMeta
// ---------------------------------------------------------------------------

/// Convert Python dispatcher return value into ResponseMeta.
///
/// Expected return types:
/// - `None` → `ResponseMeta::Empty`
/// - `bytes` / buffer-protocol object → native response selection
fn parse_response_meta(
    py: Python<'_>,
    result: Py<PyAny>,
    response_pool: &parking_lot::RwLock<MemPool>,
    shm_threshold: u64,
    max_payload_size: u64,
) -> Result<ResponseMeta, CrmError> {
    let result = result.bind(py);

    // None → Empty
    if result.is_none() {
        return Ok(ResponseMeta::Empty);
    }

    // bytes → direct SHM preparation for large payloads, otherwise owned data.
    if let Ok(bytes) = result.cast::<PyBytes>() {
        let data = bytes.as_bytes();
        if data.is_empty() {
            return Ok(ResponseMeta::Empty);
        }
        ensure_response_len_within_limit(data.len(), max_payload_size)?;
        if let Some(meta) =
            try_prepare_shm_response(response_pool, shm_threshold, data.len(), |dst| {
                dst.copy_from_slice(data);
                Ok(())
            })
            .map_err(resource_output_error)?
        {
            return Ok(meta);
        }
        return Ok(ResponseMeta::Inline(copy_slice_to_response_vec(data)?));
    }

    // Generic buffer exporter → direct SHM preparation for large payloads.
    if let Ok(buffer) = PyBuffer::<u8>::get(result) {
        let len = buffer.len_bytes();
        if len == 0 {
            return Ok(ResponseMeta::Empty);
        }
        ensure_response_len_within_limit(len, max_payload_size)?;
        if let Some(meta) = try_prepare_shm_response(response_pool, shm_threshold, len, |dst| {
            buffer.copy_to_slice(py, dst).map_err(|e| e.to_string())
        })
        .map_err(resource_output_error)?
        {
            return Ok(meta);
        }
        let mut data = allocate_response_vec(len)?;
        buffer
            .copy_to_slice(py, &mut data)
            .map_err(|e| resource_output_error(format!("failed to copy response buffer: {e}")))?;
        return Ok(ResponseMeta::Inline(data));
    }

    Err(resource_output_error(
        "dispatcher must return None or a bytes-like buffer",
    ))
}

fn resource_output_error(message: impl Into<String>) -> CrmError {
    CrmError::UserError(
        C2Error::new(ErrorCode::ResourceOutputSerializing, message.into()).to_wire_bytes(),
    )
}

fn ensure_response_len_within_limit(len: usize, max_payload_size: u64) -> Result<(), CrmError> {
    let len_u64 = u64::try_from(len).unwrap_or(u64::MAX);
    if len_u64 > max_payload_size {
        return Err(resource_output_error(format!(
            "response payload size {len} exceeds max_payload_size {max_payload_size}"
        )));
    }
    Ok(())
}

fn allocate_response_vec(len: usize) -> Result<Vec<u8>, CrmError> {
    let mut data = Vec::new();
    data.try_reserve_exact(len)
        .map_err(|e| resource_output_error(format!("failed to allocate response buffer: {e}")))?;
    data.resize(len, 0);
    Ok(data)
}

fn copy_slice_to_response_vec(data: &[u8]) -> Result<Vec<u8>, CrmError> {
    let mut owned = Vec::new();
    owned.try_reserve_exact(data.len()).map_err(|e| {
        resource_output_error(format!("failed to allocate response fallback buffer: {e}"))
    })?;
    owned.extend_from_slice(data);
    Ok(owned)
}

// ---------------------------------------------------------------------------
// Module registration
// ---------------------------------------------------------------------------

/// Register server classes on the parent module.
pub fn register_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyServer>()?;
    Ok(())
}
