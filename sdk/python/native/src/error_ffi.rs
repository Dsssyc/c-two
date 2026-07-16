use std::collections::BTreeMap;

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

    let details = PyDict::new(py);
    for (key, value) in err.details {
        details.set_item(key, value)?;
    }
    let tuple = (u16::from(err.code), err.message, details).into_pyobject(py)?;
    Ok(Some(tuple.into_any().unbind()))
}

#[pyfunction]
#[pyo3(signature = (code, message, details=None))]
fn encode_error_wire<'py>(
    py: Python<'py>,
    code: u16,
    message: &str,
    details: Option<Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyBytes>> {
    let code = ErrorCode::try_from(code).unwrap_or(ErrorCode::Unknown);
    let mut details_map = BTreeMap::new();
    if let Some(details) = details {
        for (key, value) in details.iter() {
            details_map.insert(key.extract::<String>()?, value.extract::<String>()?);
        }
    }
    let wire = C2Error::new(code, message)
        .with_details(details_map)
        .to_wire_bytes();
    Ok(PyBytes::new(py, &wire))
}

pub fn register_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(error_registry, m)?)?;
    m.add_function(wrap_pyfunction!(decode_error_wire_parts, m)?)?;
    m.add_function(wrap_pyfunction!(encode_error_wire, m)?)?;
    Ok(())
}
