use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use c2_config::{
    ClientIpcConfig, ClientIpcConfigOverrides, ConfigResolver, ConfigSources,
    RuntimeConfigOverrides, ServerIpcConfig, ServerIpcConfigOverrides,
};

#[pyfunction]
fn resolve_relay_address() -> PyResult<Option<String>> {
    c2_config::ConfigResolver::resolve_relay_address(ConfigSources::from_process())
        .map_err(|e| PyValueError::new_err(e.to_string()))
}

#[pyfunction]
fn resolve_relay_use_proxy() -> PyResult<bool> {
    c2_config::ConfigResolver::resolve_relay_use_proxy(ConfigSources::from_process())
        .map_err(|e| PyValueError::new_err(e.to_string()))
}

#[pyfunction]
#[pyo3(signature = (global_overrides=None))]
fn resolve_shm_threshold(global_overrides: Option<&Bound<'_, PyDict>>) -> PyResult<u64> {
    let mut overrides = RuntimeConfigOverrides::default();
    apply_shm_overrides(&mut overrides, global_overrides)?;
    ConfigResolver::resolve_shm_threshold(overrides.shm_threshold, ConfigSources::from_process())
        .map_err(|e| PyValueError::new_err(e.to_string()))
}

#[pyfunction]
#[pyo3(signature = (overrides=None, global_overrides=None))]
fn resolve_server_ipc_config(
    py: Python<'_>,
    overrides: Option<&Bound<'_, PyDict>>,
    global_overrides: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
    let mut runtime = RuntimeConfigOverrides::default();
    let server_overrides = parse_server_ipc_overrides(overrides)?;
    apply_shm_overrides(&mut runtime, global_overrides)?;
    let resolved = ConfigResolver::resolve_server_ipc(
        server_overrides,
        runtime,
        ConfigSources::from_process(),
    )
    .map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(server_ipc_to_dict(py, &resolved)?.into_any().unbind())
}

#[pyfunction]
#[pyo3(signature = (overrides=None, global_overrides=None))]
fn resolve_client_ipc_config(
    py: Python<'_>,
    overrides: Option<&Bound<'_, PyDict>>,
    global_overrides: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
    let mut runtime = RuntimeConfigOverrides::default();
    let client_overrides = client_overrides(overrides)?;
    apply_shm_overrides(&mut runtime, global_overrides)?;
    let resolved = ConfigResolver::resolve_client_ipc(
        client_overrides,
        runtime,
        ConfigSources::from_process(),
    )
    .map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(client_ipc_to_dict(py, &resolved)?.into_any().unbind())
}

#[pyfunction]
fn validate_server_id(server_id: &str) -> PyResult<()> {
    c2_config::validate_server_id(server_id).map_err(PyValueError::new_err)
}

#[pyfunction]
fn validate_ipc_region_id(region_id: &str) -> PyResult<()> {
    c2_config::validate_ipc_region_id(region_id).map_err(PyValueError::new_err)
}

fn apply_shm_overrides(
    overrides: &mut RuntimeConfigOverrides,
    global: Option<&Bound<'_, PyDict>>,
) -> PyResult<()> {
    let Some(global) = global else {
        return Ok(());
    };
    overrides.shm_threshold = get_opt(global, "shm_threshold")?;
    Ok(())
}

pub(crate) fn parse_server_ipc_overrides(
    dict: Option<&Bound<'_, PyDict>>,
) -> PyResult<ServerIpcConfigOverrides> {
    let Some(dict) = dict else {
        return Ok(ServerIpcConfigOverrides::default());
    };
    reject_forbidden_ipc_fields(dict)?;
    reject_unknown_ipc_fields(dict, SERVER_IPC_KEYS)?;
    Ok(ServerIpcConfigOverrides {
        pool_enabled: get_opt(dict, "pool_enabled")?,
        pool_segment_size: get_opt(dict, "pool_segment_size")?,
        max_pool_segments: get_opt(dict, "max_pool_segments")?,
        reassembly_segment_size: get_opt(dict, "reassembly_segment_size")?,
        reassembly_max_segments: get_opt(dict, "reassembly_max_segments")?,
        max_total_chunks: get_opt(dict, "max_total_chunks")?,
        chunk_gc_interval_secs: get_opt(dict, "chunk_gc_interval")?,
        chunk_threshold_ratio: get_opt(dict, "chunk_threshold_ratio")?,
        chunk_assembler_timeout_secs: get_opt(dict, "chunk_assembler_timeout")?,
        max_reassembly_bytes: get_opt(dict, "max_reassembly_bytes")?,
        chunk_size: get_opt(dict, "chunk_size")?,
        max_frame_size: get_opt(dict, "max_frame_size")?,
        max_payload_size: get_opt(dict, "max_payload_size")?,
        max_pending_requests: get_opt(dict, "max_pending_requests")?,
        pool_decay_seconds: get_opt(dict, "pool_decay_seconds")?,
        heartbeat_interval_secs: get_opt(dict, "heartbeat_interval")?,
        heartbeat_timeout_secs: get_opt(dict, "heartbeat_timeout")?,
        ..Default::default()
    })
}

pub(crate) fn parse_client_ipc_overrides(
    dict: Option<&Bound<'_, PyDict>>,
) -> PyResult<ClientIpcConfigOverrides> {
    client_overrides(dict)
}

pub(crate) fn server_ipc_overrides_to_dict<'py>(
    py: Python<'py>,
    overrides: &ServerIpcConfigOverrides,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    if let Some(value) = overrides.base.pool_enabled {
        dict.set_item("pool_enabled", value)?;
    }
    if let Some(value) = overrides.base.pool_segment_size {
        dict.set_item("pool_segment_size", value)?;
    }
    if let Some(value) = overrides.base.max_pool_segments {
        dict.set_item("max_pool_segments", value)?;
    }
    if let Some(value) = overrides.base.reassembly_segment_size {
        dict.set_item("reassembly_segment_size", value)?;
    }
    if let Some(value) = overrides.base.reassembly_max_segments {
        dict.set_item("reassembly_max_segments", value)?;
    }
    if let Some(value) = overrides.base.max_total_chunks {
        dict.set_item("max_total_chunks", value)?;
    }
    if let Some(value) = overrides.base.chunk_gc_interval_secs {
        dict.set_item("chunk_gc_interval", value)?;
    }
    if let Some(value) = overrides.base.chunk_threshold_ratio {
        dict.set_item("chunk_threshold_ratio", value)?;
    }
    if let Some(value) = overrides.base.chunk_assembler_timeout_secs {
        dict.set_item("chunk_assembler_timeout", value)?;
    }
    if let Some(value) = overrides.base.max_reassembly_bytes {
        dict.set_item("max_reassembly_bytes", value)?;
    }
    if let Some(value) = overrides.base.chunk_size {
        dict.set_item("chunk_size", value)?;
    }
    if let Some(value) = overrides.pool_enabled {
        dict.set_item("pool_enabled", value)?;
    }
    if let Some(value) = overrides.pool_segment_size {
        dict.set_item("pool_segment_size", value)?;
    }
    if let Some(value) = overrides.max_pool_segments {
        dict.set_item("max_pool_segments", value)?;
    }
    if let Some(value) = overrides.reassembly_segment_size {
        dict.set_item("reassembly_segment_size", value)?;
    }
    if let Some(value) = overrides.reassembly_max_segments {
        dict.set_item("reassembly_max_segments", value)?;
    }
    if let Some(value) = overrides.max_total_chunks {
        dict.set_item("max_total_chunks", value)?;
    }
    if let Some(value) = overrides.chunk_gc_interval_secs {
        dict.set_item("chunk_gc_interval", value)?;
    }
    if let Some(value) = overrides.chunk_threshold_ratio {
        dict.set_item("chunk_threshold_ratio", value)?;
    }
    if let Some(value) = overrides.chunk_assembler_timeout_secs {
        dict.set_item("chunk_assembler_timeout", value)?;
    }
    if let Some(value) = overrides.max_reassembly_bytes {
        dict.set_item("max_reassembly_bytes", value)?;
    }
    if let Some(value) = overrides.chunk_size {
        dict.set_item("chunk_size", value)?;
    }
    if let Some(value) = overrides.max_frame_size {
        dict.set_item("max_frame_size", value)?;
    }
    if let Some(value) = overrides.max_payload_size {
        dict.set_item("max_payload_size", value)?;
    }
    if let Some(value) = overrides.max_pending_requests {
        dict.set_item("max_pending_requests", value)?;
    }
    if let Some(value) = overrides.pool_decay_seconds {
        dict.set_item("pool_decay_seconds", value)?;
    }
    if let Some(value) = overrides.heartbeat_interval_secs {
        dict.set_item("heartbeat_interval", value)?;
    }
    if let Some(value) = overrides.heartbeat_timeout_secs {
        dict.set_item("heartbeat_timeout", value)?;
    }
    Ok(dict)
}

pub(crate) fn client_ipc_overrides_to_dict<'py>(
    py: Python<'py>,
    overrides: &ClientIpcConfigOverrides,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    if let Some(value) = overrides.base.pool_enabled {
        dict.set_item("pool_enabled", value)?;
    }
    if let Some(value) = overrides.base.pool_segment_size {
        dict.set_item("pool_segment_size", value)?;
    }
    if let Some(value) = overrides.base.max_pool_segments {
        dict.set_item("max_pool_segments", value)?;
    }
    if let Some(value) = overrides.base.reassembly_segment_size {
        dict.set_item("reassembly_segment_size", value)?;
    }
    if let Some(value) = overrides.base.reassembly_max_segments {
        dict.set_item("reassembly_max_segments", value)?;
    }
    if let Some(value) = overrides.base.max_total_chunks {
        dict.set_item("max_total_chunks", value)?;
    }
    if let Some(value) = overrides.base.chunk_gc_interval_secs {
        dict.set_item("chunk_gc_interval", value)?;
    }
    if let Some(value) = overrides.base.chunk_threshold_ratio {
        dict.set_item("chunk_threshold_ratio", value)?;
    }
    if let Some(value) = overrides.base.chunk_assembler_timeout_secs {
        dict.set_item("chunk_assembler_timeout", value)?;
    }
    if let Some(value) = overrides.base.max_reassembly_bytes {
        dict.set_item("max_reassembly_bytes", value)?;
    }
    if let Some(value) = overrides.base.chunk_size {
        dict.set_item("chunk_size", value)?;
    }
    if let Some(value) = overrides.pool_enabled {
        dict.set_item("pool_enabled", value)?;
    }
    if let Some(value) = overrides.pool_segment_size {
        dict.set_item("pool_segment_size", value)?;
    }
    if let Some(value) = overrides.max_pool_segments {
        dict.set_item("max_pool_segments", value)?;
    }
    if let Some(value) = overrides.reassembly_segment_size {
        dict.set_item("reassembly_segment_size", value)?;
    }
    if let Some(value) = overrides.reassembly_max_segments {
        dict.set_item("reassembly_max_segments", value)?;
    }
    if let Some(value) = overrides.max_total_chunks {
        dict.set_item("max_total_chunks", value)?;
    }
    if let Some(value) = overrides.chunk_gc_interval_secs {
        dict.set_item("chunk_gc_interval", value)?;
    }
    if let Some(value) = overrides.chunk_threshold_ratio {
        dict.set_item("chunk_threshold_ratio", value)?;
    }
    if let Some(value) = overrides.chunk_assembler_timeout_secs {
        dict.set_item("chunk_assembler_timeout", value)?;
    }
    if let Some(value) = overrides.max_reassembly_bytes {
        dict.set_item("max_reassembly_bytes", value)?;
    }
    if let Some(value) = overrides.chunk_size {
        dict.set_item("chunk_size", value)?;
    }
    Ok(dict)
}

fn client_overrides(dict: Option<&Bound<'_, PyDict>>) -> PyResult<ClientIpcConfigOverrides> {
    let Some(dict) = dict else {
        return Ok(ClientIpcConfigOverrides::default());
    };
    reject_forbidden_ipc_fields(dict)?;
    reject_unknown_ipc_fields(dict, CLIENT_IPC_KEYS)?;
    Ok(ClientIpcConfigOverrides {
        pool_enabled: get_opt(dict, "pool_enabled")?,
        pool_segment_size: get_opt(dict, "pool_segment_size")?,
        max_pool_segments: get_opt(dict, "max_pool_segments")?,
        reassembly_segment_size: get_opt(dict, "reassembly_segment_size")?,
        reassembly_max_segments: get_opt(dict, "reassembly_max_segments")?,
        max_total_chunks: get_opt(dict, "max_total_chunks")?,
        chunk_gc_interval_secs: get_opt(dict, "chunk_gc_interval")?,
        chunk_threshold_ratio: get_opt(dict, "chunk_threshold_ratio")?,
        chunk_assembler_timeout_secs: get_opt(dict, "chunk_assembler_timeout")?,
        max_reassembly_bytes: get_opt(dict, "max_reassembly_bytes")?,
        chunk_size: get_opt(dict, "chunk_size")?,
        ..Default::default()
    })
}

const BASE_IPC_KEYS: &[&str] = &[
    "pool_enabled",
    "pool_segment_size",
    "max_pool_segments",
    "reassembly_segment_size",
    "reassembly_max_segments",
    "max_total_chunks",
    "chunk_gc_interval",
    "chunk_threshold_ratio",
    "chunk_assembler_timeout",
    "max_reassembly_bytes",
    "chunk_size",
];

const SERVER_IPC_KEYS: &[&str] = &[
    "pool_enabled",
    "pool_segment_size",
    "max_pool_segments",
    "reassembly_segment_size",
    "reassembly_max_segments",
    "max_total_chunks",
    "chunk_gc_interval",
    "chunk_threshold_ratio",
    "chunk_assembler_timeout",
    "max_reassembly_bytes",
    "chunk_size",
    "max_frame_size",
    "max_payload_size",
    "max_pending_requests",
    "pool_decay_seconds",
    "heartbeat_interval",
    "heartbeat_timeout",
];

const CLIENT_IPC_KEYS: &[&str] = BASE_IPC_KEYS;

fn reject_forbidden_ipc_fields(dict: &Bound<'_, PyDict>) -> PyResult<()> {
    if dict.contains("shm_threshold")? {
        return Err(PyValueError::new_err(
            "shm_threshold is a global transport policy; use set_transport_policy(shm_threshold=...)",
        ));
    }
    Ok(())
}

fn reject_unknown_ipc_fields(dict: &Bound<'_, PyDict>, allowed: &[&str]) -> PyResult<()> {
    for item in dict.keys().iter() {
        let key: String = item.extract()?;
        if !allowed.contains(&key.as_str()) {
            return Err(PyValueError::new_err(format!(
                "unknown IPC override option: {key}"
            )));
        }
    }
    Ok(())
}

fn get_opt<T>(dict: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<T>>
where
    T: for<'a, 'py> FromPyObject<'a, 'py, Error = PyErr>,
{
    match dict.get_item(key)? {
        Some(value) if !value.is_none() => Ok(Some(value.extract::<T>()?)),
        _ => Ok(None),
    }
}

fn server_ipc_to_dict<'py>(py: Python<'py>, cfg: &ServerIpcConfig) -> PyResult<Bound<'py, PyDict>> {
    let dict = base_ipc_to_dict(py, &cfg.base)?;
    dict.set_item("shm_threshold", cfg.shm_threshold)?;
    dict.set_item("max_frame_size", cfg.max_frame_size)?;
    dict.set_item("max_payload_size", cfg.max_payload_size)?;
    dict.set_item("max_pending_requests", cfg.max_pending_requests)?;
    dict.set_item("pool_decay_seconds", cfg.pool_decay_seconds)?;
    dict.set_item("heartbeat_interval", cfg.heartbeat_interval_secs)?;
    dict.set_item("heartbeat_timeout", cfg.heartbeat_timeout_secs)?;
    Ok(dict)
}

pub(crate) fn client_ipc_to_dict<'py>(
    py: Python<'py>,
    cfg: &ClientIpcConfig,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = base_ipc_to_dict(py, &cfg.base)?;
    dict.set_item("shm_threshold", cfg.shm_threshold)?;
    Ok(dict)
}

fn base_ipc_to_dict<'py>(
    py: Python<'py>,
    cfg: &c2_config::BaseIpcConfig,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("pool_enabled", cfg.pool_enabled)?;
    dict.set_item("pool_segment_size", cfg.pool_segment_size)?;
    dict.set_item("max_pool_segments", cfg.max_pool_segments)?;
    dict.set_item("max_pool_memory", cfg.max_pool_memory)?;
    dict.set_item("reassembly_segment_size", cfg.reassembly_segment_size)?;
    dict.set_item("reassembly_max_segments", cfg.reassembly_max_segments)?;
    dict.set_item("max_total_chunks", cfg.max_total_chunks)?;
    dict.set_item("chunk_gc_interval", cfg.chunk_gc_interval_secs)?;
    dict.set_item("chunk_threshold_ratio", cfg.chunk_threshold_ratio)?;
    dict.set_item("chunk_assembler_timeout", cfg.chunk_assembler_timeout_secs)?;
    dict.set_item("max_reassembly_bytes", cfg.max_reassembly_bytes)?;
    dict.set_item("chunk_size", cfg.chunk_size)?;
    Ok(dict)
}

pub fn register_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(resolve_relay_address, m)?)?;
    m.add_function(wrap_pyfunction!(resolve_relay_use_proxy, m)?)?;
    m.add_function(wrap_pyfunction!(resolve_shm_threshold, m)?)?;
    m.add_function(wrap_pyfunction!(resolve_server_ipc_config, m)?)?;
    m.add_function(wrap_pyfunction!(resolve_client_ipc_config, m)?)?;
    m.add_function(wrap_pyfunction!(validate_server_id, m)?)?;
    m.add_function(wrap_pyfunction!(validate_ipc_region_id, m)?)?;
    Ok(())
}
