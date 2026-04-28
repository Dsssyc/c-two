use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use c2_config::{
    ClientIpcConfig, ClientIpcConfigOverrides, ConfigResolver, ConfigSources,
    RuntimeConfigOverrides, ServerIpcConfig, ServerIpcConfigOverrides,
};

#[pyfunction]
#[pyo3(signature = (global_overrides=None))]
fn resolve_relay_client_config(
    py: Python<'_>,
    global_overrides: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
    let mut overrides = RuntimeConfigOverrides::default();
    apply_relay_client_overrides(&mut overrides, global_overrides)?;
    let resolved = ConfigResolver::resolve_relay_client(overrides, ConfigSources::from_process())
        .map_err(|e| PyValueError::new_err(e.to_string()))?;

    let dict = PyDict::new(py);
    dict.set_item("relay_address", resolved.relay_address)?;
    dict.set_item("relay_use_proxy", resolved.relay_use_proxy)?;
    Ok(dict.into_any().unbind())
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
    let server_overrides = server_overrides(overrides)?;
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

fn apply_relay_client_overrides(
    overrides: &mut RuntimeConfigOverrides,
    global: Option<&Bound<'_, PyDict>>,
) -> PyResult<()> {
    let Some(global) = global else {
        return Ok(());
    };
    overrides.relay_address = get_opt(global, "relay_address")?;
    overrides.relay_use_proxy = get_opt(global, "relay_use_proxy")?;
    Ok(())
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

fn server_overrides(dict: Option<&Bound<'_, PyDict>>) -> PyResult<ServerIpcConfigOverrides> {
    let Some(dict) = dict else {
        return Ok(ServerIpcConfigOverrides::default());
    };
    reject_derived_fields(dict)?;
    Ok(ServerIpcConfigOverrides {
        shm_threshold: get_opt(dict, "shm_threshold")?,
        pool_enabled: get_opt(dict, "pool_enabled")?,
        pool_segment_size: get_opt(dict, "pool_segment_size")?,
        max_pool_segments: get_opt(dict, "max_pool_segments")?,
        reassembly_segment_size: get_opt(dict, "reassembly_segment_size")?,
        reassembly_max_segments: get_opt(dict, "reassembly_max_segments")?,
        max_total_chunks: get_opt(dict, "max_total_chunks")?,
        chunk_gc_interval_secs: get_opt_alias(dict, "chunk_gc_interval_secs", "chunk_gc_interval")?,
        chunk_threshold_ratio: get_opt(dict, "chunk_threshold_ratio")?,
        chunk_assembler_timeout_secs: get_opt_alias(
            dict,
            "chunk_assembler_timeout_secs",
            "chunk_assembler_timeout",
        )?,
        max_reassembly_bytes: get_opt(dict, "max_reassembly_bytes")?,
        chunk_size: get_opt(dict, "chunk_size")?,
        max_frame_size: get_opt(dict, "max_frame_size")?,
        max_payload_size: get_opt(dict, "max_payload_size")?,
        max_pending_requests: get_opt(dict, "max_pending_requests")?,
        pool_decay_seconds: get_opt(dict, "pool_decay_seconds")?,
        heartbeat_interval_secs: get_opt_alias(
            dict,
            "heartbeat_interval_secs",
            "heartbeat_interval",
        )?,
        heartbeat_timeout_secs: get_opt_alias(dict, "heartbeat_timeout_secs", "heartbeat_timeout")?,
        ..Default::default()
    })
}

fn client_overrides(dict: Option<&Bound<'_, PyDict>>) -> PyResult<ClientIpcConfigOverrides> {
    let Some(dict) = dict else {
        return Ok(ClientIpcConfigOverrides::default());
    };
    reject_derived_fields(dict)?;
    Ok(ClientIpcConfigOverrides {
        shm_threshold: get_opt(dict, "shm_threshold")?,
        pool_enabled: get_opt(dict, "pool_enabled")?,
        pool_segment_size: get_opt(dict, "pool_segment_size")?,
        max_pool_segments: get_opt(dict, "max_pool_segments")?,
        reassembly_segment_size: get_opt(dict, "reassembly_segment_size")?,
        reassembly_max_segments: get_opt(dict, "reassembly_max_segments")?,
        max_total_chunks: get_opt(dict, "max_total_chunks")?,
        chunk_gc_interval_secs: get_opt_alias(dict, "chunk_gc_interval_secs", "chunk_gc_interval")?,
        chunk_threshold_ratio: get_opt(dict, "chunk_threshold_ratio")?,
        chunk_assembler_timeout_secs: get_opt_alias(
            dict,
            "chunk_assembler_timeout_secs",
            "chunk_assembler_timeout",
        )?,
        max_reassembly_bytes: get_opt(dict, "max_reassembly_bytes")?,
        chunk_size: get_opt(dict, "chunk_size")?,
        ..Default::default()
    })
}

fn reject_derived_fields(dict: &Bound<'_, PyDict>) -> PyResult<()> {
    if dict.contains("max_pool_memory")? {
        return Err(PyValueError::new_err(
            "max_pool_memory is derived from pool_segment_size * max_pool_segments",
        ));
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

fn get_opt_alias<T>(dict: &Bound<'_, PyDict>, primary: &str, alias: &str) -> PyResult<Option<T>>
where
    T: for<'a, 'py> FromPyObject<'a, 'py, Error = PyErr>,
{
    match get_opt(dict, primary)? {
        Some(value) => Ok(Some(value)),
        None => get_opt(dict, alias),
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

fn client_ipc_to_dict<'py>(py: Python<'py>, cfg: &ClientIpcConfig) -> PyResult<Bound<'py, PyDict>> {
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
    m.add_function(wrap_pyfunction!(resolve_relay_client_config, m)?)?;
    m.add_function(wrap_pyfunction!(resolve_shm_threshold, m)?)?;
    m.add_function(wrap_pyfunction!(resolve_server_ipc_config, m)?)?;
    m.add_function(wrap_pyfunction!(resolve_client_ipc_config, m)?)?;
    Ok(())
}
