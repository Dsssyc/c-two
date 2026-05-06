//! PyO3 bindings for direct IPC control helpers from `c2-ipc`.

use std::time::Duration;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

fn timeout_from_seconds(timeout_seconds: f64) -> PyResult<Duration> {
    if !timeout_seconds.is_finite() || timeout_seconds < 0.0 {
        return Err(PyValueError::new_err(
            "timeout_seconds must be a non-negative finite number",
        ));
    }
    Ok(Duration::from_secs_f64(timeout_seconds))
}

#[pyfunction]
fn ipc_socket_path(address: &str) -> PyResult<String> {
    let path = c2_ipc::socket_path_from_ipc_address(address)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(path.to_string_lossy().into_owned())
}

#[pyfunction]
#[pyo3(signature = (address, timeout_seconds=0.5))]
fn ipc_ping(py: Python<'_>, address: &str, timeout_seconds: f64) -> PyResult<bool> {
    let timeout = timeout_from_seconds(timeout_seconds)?;
    let address = address.to_string();
    py.detach(move || {
        c2_ipc::ping(&address, timeout).map_err(|e| match e {
            c2_ipc::IpcError::Config(msg) => PyValueError::new_err(msg),
            other => PyRuntimeError::new_err(other.to_string()),
        })
    })
}

#[pyfunction]
#[pyo3(signature = (address, timeout_seconds=0.5))]
fn ipc_shutdown(py: Python<'_>, address: &str, timeout_seconds: f64) -> PyResult<bool> {
    let timeout = timeout_from_seconds(timeout_seconds)?;
    let address = address.to_string();
    py.detach(move || {
        c2_ipc::shutdown(&address, timeout).map_err(|e| match e {
            c2_ipc::IpcError::Config(msg) => PyValueError::new_err(msg),
            other => PyRuntimeError::new_err(other.to_string()),
        })
    })
}

pub fn register_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(ipc_socket_path, m)?)?;
    m.add_function(wrap_pyfunction!(ipc_ping, m)?)?;
    m.add_function(wrap_pyfunction!(ipc_shutdown, m)?)?;
    Ok(())
}
