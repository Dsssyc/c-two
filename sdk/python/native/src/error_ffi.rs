use pyo3::buffer::PyBuffer;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

use c2_error::{C2Error, ErrorCode};

#[pyfunction]
fn error_registry(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    for entry in ErrorCode::registry() {
        dict.set_item(entry.name, u16::from(entry.code))?;
    }
    Ok(dict.into_any().unbind())
}

#[pyfunction]
fn decode_error_wire_parts(py: Python<'_>, data: PyBuffer<u8>) -> PyResult<Option<Py<PyAny>>> {
    let mut bytes = vec![0_u8; data.len_bytes()];
    data.copy_to_slice(py, &mut bytes)?;
    let Some(err) =
        C2Error::from_wire_bytes(&bytes).map_err(|e| PyValueError::new_err(e.to_string()))?
    else {
        return Ok(None);
    };

    let tuple = (u16::from(err.code), err.message).into_pyobject(py)?;
    Ok(Some(tuple.into_any().unbind()))
}

#[pyfunction]
fn encode_error_wire<'py>(
    py: Python<'py>,
    code: u16,
    message: &str,
) -> PyResult<Bound<'py, PyBytes>> {
    let code = ErrorCode::try_from(code).unwrap_or(ErrorCode::Unknown);
    let code_text = u16::from(code).to_string();
    let message_bytes = message.as_bytes();
    let total_len = code_text.len() + 1 + message_bytes.len();

    PyBytes::new_with(py, total_len, |buf| {
        let code_bytes = code_text.as_bytes();
        buf[..code_bytes.len()].copy_from_slice(code_bytes);
        buf[code_bytes.len()] = b':';
        buf[code_bytes.len() + 1..].copy_from_slice(message_bytes);
        Ok(())
    })
}

pub fn register_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(error_registry, m)?)?;
    m.add_function(wrap_pyfunction!(decode_error_wire_parts, m)?)?;
    m.add_function(wrap_pyfunction!(encode_error_wire, m)?)?;
    Ok(())
}
