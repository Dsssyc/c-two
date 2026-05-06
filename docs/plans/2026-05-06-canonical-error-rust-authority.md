# Canonical Error Rust Authority Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Rust `c2-error` the canonical owner of C-Two error code registry and error wire bytes while keeping Python exception classes as a thin SDK presentation layer.

**Architecture:** Rename the current `code:message` APIs from `legacy` to `wire` because this format is still the canonical C-Two error wire payload, not a compatibility-only path. Python builds its public `ERROR_Code` facade from `_native.error_registry()`, and `CCError.serialize()` / `CCError.deserialize()` delegate wire encoding and decoding to native PyO3 functions. Native encode avoids an intermediate Rust `Vec<u8>` by writing directly into the Python bytes allocation; native decode accepts Python buffer inputs directly and returns only `(code, message)` for the SDK hot path.

**Tech Stack:** Rust `core/foundation/c2-error`, PyO3 bindings in `sdk/python/native/src/error_ffi.rs`, Python SDK facade in `sdk/python/src/c_two/error.py`, pytest unit/integration tests, Cargo tests.

---

## 0.x Clean-Cut Constraint

C-Two is in the 0.x line. Do not preserve stale `legacy` API names as compatibility aliases. This implementation must remove or replace `to_legacy_bytes`, `from_legacy_bytes`, `_native.encode_error_legacy`, and `_native.decode_error_legacy` rather than carrying both naming schemes. The current `code:message` bytes format remains supported as the canonical wire format and should be named `wire`, not `legacy`.

## Copy And Allocation Constraints

- `CCError.deserialize(memoryview(...))` must pass the buffer directly into native decode. It must not call `memoryview.tobytes()`, `bytes(data)`, or `data.decode(...)` before FFI.
- The SDK hot-path native decoder must return `None` or `(code: int, message: str)`. Do not allocate a dict or return the error name for `CCError.deserialize()`.
- The native encoder must write the `code:message` payload directly into a Python bytes allocation using `PyBytes::new_with` or equivalent. Do not allocate a Rust `Vec<u8>` solely to copy it into Python bytes.
- Do not add a Python-visible native `C2Error` object. Python exception subclass presentation remains Python-owned.
- Rust owns wire correctness. Python owns exception subclass mapping and malformed-payload presentation.

## File Map

- Modify `core/foundation/c2-error/src/lib.rs`: rename legacy API to wire API, update decode error wording and Rust tests.
- Modify `sdk/python/native/src/error_ffi.rs`: expose `_native.error_registry()`, `_native.encode_error_wire()`, and `_native.decode_error_wire_parts()` with low-copy behavior; remove legacy-named PyO3 functions.
- Modify `sdk/python/src/c_two/error.py`: generate `ERROR_Code` from native registry; delegate serialization/deserialization to native wire codec; remove Python parser code.
- Modify `sdk/python/tests/unit/test_error.py`: cover generated enum parity, native delegation, subclass mapping, unknown-code semantics, and malformed-payload presentation.
- Modify `sdk/python/tests/unit/test_native_error_registry.py`: update native API names and canonical wire wording.
- Modify `sdk/python/tests/unit/test_sdk_boundary.py`: add source guard preventing Python from re-owning `code:message` parsing.
- Modify `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`: mark the canonical error item as implemented and document the copy constraints.

## Required Behavior After Implementation

- `ERROR_Code.ERROR_RESOURCE_ALREADY_REGISTERED.value == 703` remains true.
- `list(ERROR_Code)` is generated from Rust registry values, not hand-written numeric assignments.
- `CCError.serialize(ResourceAlreadyRegistered("grid exists")) == b"703:grid exists"` remains true.
- `CCError.deserialize(memoryview(b"9999:relay failed"))` returns a plain `CCError` with `ERROR_UNKNOWN` and message `Unknown error code 9999: relay failed`.
- Malformed wire payloads return `CCError(ERROR_UNKNOWN, "Malformed error payload: ...")` from Python public API instead of leaking a native `ValueError` through resource-call error propagation.
- `tests/repo` and Python SDK tests do not contain stale assertions that call the error wire format a Python format.

---

### Task 0: Commit Or Stash Existing Issue3 Work Before Starting

**Files:**
- Commit existing modifications before editing error authority files.

- [ ] **Step 1: Inspect the current worktree**

Run:

```bash
git status --short --branch
```

Expected if Issue3 fixes are still uncommitted:

```text
## dev-feature...origin/dev-feature
 M .github/workflows/ci.yml
 M sdk/python/tests/unit/test_sdk_boundary.py
 M tests/repo/test_check_version.py
 M tests/repo/test_cli_release_workflow.py
```

- [ ] **Step 2: Commit the completed Issue3 fix before starting Issue4**

Run:

```bash
git add .github/workflows/ci.yml \
  sdk/python/tests/unit/test_sdk_boundary.py \
  tests/repo/test_check_version.py \
  tests/repo/test_cli_release_workflow.py

git commit -m "test: finalize repo policy test migration"
```

Expected: a commit is created containing only Issue3 migration cleanup.

- [ ] **Step 3: Confirm a clean worktree for Issue4**

Run:

```bash
git status --short --branch
```

Expected:

```text
## dev-feature...origin/dev-feature [ahead 1]
```

or the equivalent clean branch status for the active local branch.

---

### Task 1: Rename Rust Error Codec APIs From Legacy To Wire

**Files:**
- Modify: `core/foundation/c2-error/src/lib.rs`
- Test: `core/foundation/c2-error/src/lib.rs`

- [ ] **Step 1: Write the failing Rust API-name test by updating existing tests**

In `core/foundation/c2-error/src/lib.rs`, replace the test module with this version. The important red condition is that tests call `to_wire_bytes()` and `from_wire_bytes()`, which do not exist before implementation.

```rust
#[cfg(test)]
mod tests {
    use super::{C2Error, ErrorCode};

    #[test]
    fn canonical_error_codes_match_wire_values() {
        assert_eq!(u16::from(ErrorCode::Unknown), 0);
        assert_eq!(u16::from(ErrorCode::ResourceInputDeserializing), 1);
        assert_eq!(u16::from(ErrorCode::ResourceOutputSerializing), 2);
        assert_eq!(u16::from(ErrorCode::ResourceFunctionExecuting), 3);
        assert_eq!(u16::from(ErrorCode::ClientInputSerializing), 5);
        assert_eq!(u16::from(ErrorCode::ClientOutputDeserializing), 6);
        assert_eq!(u16::from(ErrorCode::ClientCallingResource), 7);
        assert_eq!(u16::from(ErrorCode::ResourceNotFound), 701);
        assert_eq!(u16::from(ErrorCode::ResourceUnavailable), 702);
        assert_eq!(u16::from(ErrorCode::ResourceAlreadyRegistered), 703);
        assert_eq!(u16::from(ErrorCode::StaleResource), 704);
        assert_eq!(u16::from(ErrorCode::RegistryUnavailable), 705);
        assert_eq!(u16::from(ErrorCode::WriteConflict), 706);
    }

    #[test]
    fn c2_error_display_uses_code_name_and_message() {
        let err = C2Error::new(ErrorCode::ResourceAlreadyRegistered, "grid exists");
        assert_eq!(err.to_string(), "ResourceAlreadyRegistered: grid exists");
    }

    #[test]
    fn error_code_registry_exposes_names_and_codes_from_crate() {
        let registry: Vec<_> = ErrorCode::registry()
            .iter()
            .map(|entry| (entry.name, u16::from(entry.code)))
            .collect();

        assert_eq!(
            registry,
            vec![
                ("Unknown", 0),
                ("ResourceInputDeserializing", 1),
                ("ResourceOutputSerializing", 2),
                ("ResourceFunctionExecuting", 3),
                ("ClientInputSerializing", 5),
                ("ClientOutputDeserializing", 6),
                ("ClientCallingResource", 7),
                ("ResourceNotFound", 701),
                ("ResourceUnavailable", 702),
                ("ResourceAlreadyRegistered", 703),
                ("StaleResource", 704),
                ("RegistryUnavailable", 705),
                ("WriteConflict", 706),
            ],
        );
        assert_eq!(ErrorCode::WriteConflict.name(), "WriteConflict");
    }

    #[test]
    fn wire_encode_matches_canonical_error_wire_format() {
        let err = C2Error::new(ErrorCode::ResourceAlreadyRegistered, "grid exists");
        assert_eq!(err.to_wire_bytes(), b"703:grid exists");
    }

    #[test]
    fn wire_decode_empty_bytes_means_no_error() {
        assert_eq!(C2Error::from_wire_bytes(b"").unwrap(), None);
    }

    #[test]
    fn wire_decode_known_code_returns_canonical_error() {
        let err = C2Error::from_wire_bytes(b"701:missing grid")
            .unwrap()
            .unwrap();
        assert_eq!(err.code, ErrorCode::ResourceNotFound);
        assert_eq!(err.message, "missing grid");
    }

    #[test]
    fn wire_decode_preserves_colons_in_message() {
        let err = C2Error::from_wire_bytes(b"0:host:port:extra")
            .unwrap()
            .unwrap();
        assert_eq!(err.code, ErrorCode::Unknown);
        assert_eq!(err.message, "host:port:extra");
    }

    #[test]
    fn wire_decode_unknown_code_degrades_to_unknown_with_context() {
        let err = C2Error::from_wire_bytes(b"9999:low-level relay failure")
            .unwrap()
            .unwrap();
        assert_eq!(err.code, ErrorCode::Unknown);
        assert_eq!(
            err.message,
            "Unknown error code 9999: low-level relay failure"
        );
    }

    #[test]
    fn wire_decode_malformed_code_fails() {
        let err = C2Error::from_wire_bytes(b"abc:not a number").unwrap_err();
        assert_eq!(err.to_string(), "invalid C2 error wire code: abc");
    }
}
```

- [ ] **Step 2: Run the Rust error crate tests and verify the expected failure**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-error
```

Expected: FAIL because `C2Error::to_wire_bytes` and `C2Error::from_wire_bytes` are not defined.

- [ ] **Step 3: Implement the wire-named Rust API and remove legacy-named methods**

In `core/foundation/c2-error/src/lib.rs`, replace `C2ErrorDecodeError` display text and the `C2Error` encode/decode methods with this implementation:

```rust
impl fmt::Display for C2ErrorDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            C2ErrorDecodeError::InvalidUtf8 => f.write_str("invalid C2 error wire UTF-8"),
            C2ErrorDecodeError::MissingSeparator => {
                f.write_str("invalid C2 error wire payload: missing ':' separator")
            }
            C2ErrorDecodeError::InvalidCode(code) => {
                write!(f, "invalid C2 error wire code: {code}")
            }
        }
    }
}

impl std::error::Error for C2ErrorDecodeError {}

impl C2Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn unknown(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unknown, message)
    }

    pub fn to_wire_bytes(&self) -> Vec<u8> {
        format!("{}:{}", u16::from(self.code), self.message).into_bytes()
    }

    pub fn from_wire_bytes(data: &[u8]) -> Result<Option<Self>, C2ErrorDecodeError> {
        if data.is_empty() {
            return Ok(None);
        }

        let raw = std::str::from_utf8(data).map_err(|_| C2ErrorDecodeError::InvalidUtf8)?;
        let (code_raw, message) = raw
            .split_once(':')
            .ok_or(C2ErrorDecodeError::MissingSeparator)?;
        let code_value = code_raw
            .parse::<u16>()
            .map_err(|_| C2ErrorDecodeError::InvalidCode(code_raw.to_string()))?;

        match ErrorCode::try_from(code_value) {
            Ok(code) => Ok(Some(C2Error::new(code, message))),
            Err(()) => Ok(Some(C2Error::unknown(format!(
                "Unknown error code {code_value}: {message}"
            )))),
        }
    }
}
```

- [ ] **Step 4: Update all Rust references to the old method names**

Run:

```bash
rg -n "to_legacy_bytes|from_legacy_bytes" core sdk/python/native
```

Replace each remaining Rust call with `to_wire_bytes` or `from_wire_bytes`. No compatibility wrappers should remain.

- [ ] **Step 5: Run Rust error tests and verify green**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-error
```

Expected: PASS.

- [ ] **Step 6: Commit Task 1**

Run:

```bash
git add core/foundation/c2-error/src/lib.rs
git commit -m "refactor(error): rename canonical codec to wire"
```

---

### Task 2: Replace PyO3 Legacy Error Functions With Low-Copy Wire Functions

**Files:**
- Modify: `sdk/python/native/src/error_ffi.rs`
- Test: `sdk/python/tests/unit/test_native_error_registry.py`

- [ ] **Step 1: Write failing Python tests for the new native API names and behavior**

Replace `sdk/python/tests/unit/test_native_error_registry.py` with:

```python
from c_two import _native


def test_native_error_registry_exposes_canonical_codes():
    registry = _native.error_registry()
    assert registry["Unknown"] == 0
    assert registry["ResourceNotFound"] == 701
    assert registry["ResourceUnavailable"] == 702
    assert registry["ResourceAlreadyRegistered"] == 703
    assert registry["StaleResource"] == 704
    assert registry["RegistryUnavailable"] == 705
    assert registry["WriteConflict"] == 706


def test_native_decode_error_wire_parts_known_code():
    decoded = _native.decode_error_wire_parts(memoryview(b"703:grid exists"))
    assert decoded == (703, "grid exists")


def test_native_decode_error_wire_parts_unknown_code_degrades():
    decoded = _native.decode_error_wire_parts(memoryview(b"9999:relay exploded"))
    assert decoded == (0, "Unknown error code 9999: relay exploded")


def test_native_decode_error_wire_parts_empty_bytes_returns_none():
    assert _native.decode_error_wire_parts(memoryview(b"")) is None


def test_native_decode_error_wire_parts_rejects_malformed_payloads():
    for payload in (b"abc:not a number", b"3", b"\xff"):
        try:
            _native.decode_error_wire_parts(memoryview(payload))
        except ValueError as exc:
            assert "C2 error wire" in str(exc)
        else:
            raise AssertionError(f"expected ValueError for {payload!r}")


def test_native_encode_error_wire_matches_canonical_wire_format():
    assert _native.encode_error_wire(703, "grid exists") == b"703:grid exists"


def test_native_error_ffi_does_not_export_legacy_codec_names():
    assert not hasattr(_native, "encode_error_legacy")
    assert not hasattr(_native, "decode_error_legacy")
```

- [ ] **Step 2: Run the native error registry tests and verify the expected failure**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/unit/test_native_error_registry.py -q --timeout=30
```

Expected: FAIL because `_native.encode_error_wire` and `_native.decode_error_wire_parts` are not exported yet.

- [ ] **Step 3: Replace `sdk/python/native/src/error_ffi.rs` with wire-named low-copy bindings**

Use this implementation:

```rust
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
    let Some(err) = C2Error::from_wire_bytes(&bytes)
        .map_err(|e| PyValueError::new_err(e.to_string()))?
    else {
        return Ok(None);
    };

    let tuple = (u16::from(err.code), err.message).into_pyobject(py)?;
    Ok(Some(tuple.into_any().unbind()))
}

#[pyfunction]
fn encode_error_wire<'py>(py: Python<'py>, code: u16, message: &str) -> PyResult<Bound<'py, PyBytes>> {
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
```

**Implementation note:** `PyBuffer<u8>` avoids a Python-side pre-copy from `memoryview`. PyO3 still copies into a temporary Rust `Vec<u8>` for decode because `PyBuffer` does not expose a single safe borrowed contiguous slice for every exporter. This is acceptable for small error payloads and avoids the larger Python `tobytes()` copy. Encode writes directly into the final Python bytes allocation.

- [ ] **Step 4: Run Cargo check for the native extension**

Run:

```bash
cargo check --manifest-path sdk/python/native/Cargo.toml -q
```

Expected: PASS.

- [ ] **Step 5: Rebuild the Python native extension and run native error tests**

Run:

```bash
uv sync --reinstall-package c-two
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/unit/test_native_error_registry.py -q --timeout=30
```

Expected: PASS.

- [ ] **Step 6: Assert old native names are gone by source search**

Run:

```bash
rg -n "encode_error_legacy|decode_error_legacy|legacy_error" sdk/python/native sdk/python/tests/unit/test_native_error_registry.py
```

Expected: no matches.

- [ ] **Step 7: Commit Task 2**

Run:

```bash
git add sdk/python/native/src/error_ffi.rs sdk/python/tests/unit/test_native_error_registry.py
git commit -m "refactor(error): expose canonical wire codec through native"
```

---

### Task 3: Generate Python `ERROR_Code` From Native Registry And Delegate Codec Calls

**Files:**
- Modify: `sdk/python/src/c_two/error.py`
- Modify: `sdk/python/tests/unit/test_error.py`

- [ ] **Step 1: Write failing tests for native-generated enum and delegated codec calls**

In `sdk/python/tests/unit/test_error.py`, replace `TestNativeErrorRegistryParity` with this expanded class:

```python
class TestNativeErrorRegistryParity:
    def test_python_error_codes_match_native_registry(self):
        from c_two import _native

        native = _native.error_registry()
        expected = {
            "ERROR_UNKNOWN": ("Unknown", 0),
            "ERROR_AT_RESOURCE_INPUT_DESERIALIZING": ("ResourceInputDeserializing", 1),
            "ERROR_AT_RESOURCE_OUTPUT_SERIALIZING": ("ResourceOutputSerializing", 2),
            "ERROR_AT_RESOURCE_FUNCTION_EXECUTING": ("ResourceFunctionExecuting", 3),
            "ERROR_AT_CLIENT_INPUT_SERIALIZING": ("ClientInputSerializing", 5),
            "ERROR_AT_CLIENT_OUTPUT_DESERIALIZING": ("ClientOutputDeserializing", 6),
            "ERROR_AT_CLIENT_CALLING_RESOURCE": ("ClientCallingResource", 7),
            "ERROR_RESOURCE_NOT_FOUND": ("ResourceNotFound", 701),
            "ERROR_RESOURCE_UNAVAILABLE": ("ResourceUnavailable", 702),
            "ERROR_RESOURCE_ALREADY_REGISTERED": ("ResourceAlreadyRegistered", 703),
            "ERROR_STALE_RESOURCE": ("StaleResource", 704),
            "ERROR_REGISTRY_UNAVAILABLE": ("RegistryUnavailable", 705),
            "ERROR_WRITE_CONFLICT": ("WriteConflict", 706),
        }

        assert set(ERROR_Code.__members__) == set(expected)
        for py_name, (native_name, value) in expected.items():
            assert native[native_name] == value
            assert ERROR_Code[py_name].value == value

    def test_serialize_uses_native_wire_encoder(self, monkeypatch):
        calls = []

        def fake_encode(code, message):
            calls.append((code, message))
            return b"3:from-native"

        monkeypatch.setattr(error._native, "encode_error_wire", fake_encode)

        err = CCError(ERROR_Code.ERROR_AT_RESOURCE_FUNCTION_EXECUTING, "boom")
        assert CCError.serialize(err) == b"3:from-native"
        assert calls == [(3, "boom")]

    def test_deserialize_uses_native_wire_decoder_without_python_tobytes(self, monkeypatch):
        class NoToBytes:
            def tobytes(self):
                raise AssertionError("deserialize must not call tobytes")

        calls = []

        def fake_decode(data):
            calls.append(data)
            return (703, "grid exists")

        monkeypatch.setattr(error._native, "decode_error_wire_parts", fake_decode)

        result = CCError.deserialize(NoToBytes())
        assert isinstance(result, ResourceAlreadyRegistered)
        assert result.message == "grid exists"
        assert len(calls) == 1
        assert isinstance(calls[0], NoToBytes)
```

- [ ] **Step 2: Run the focused Python error tests and verify expected failure**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/unit/test_error.py::TestNativeErrorRegistryParity -q --timeout=30
```

Expected: FAIL because `error._native.encode_error_wire`, `error._native.decode_error_wire_parts`, and registry-generated enum behavior are not wired into `error.py` yet.

- [ ] **Step 3: Replace the top of `sdk/python/src/c_two/error.py` with native-generated enum setup**

At the top of `sdk/python/src/c_two/error.py`, replace the hand-written enum with:

```python
from __future__ import annotations
from enum import IntEnum, unique

from c_two import _native

_NATIVE_TO_PY_ERROR_NAMES = {
    "Unknown": "ERROR_UNKNOWN",
    "ResourceInputDeserializing": "ERROR_AT_RESOURCE_INPUT_DESERIALIZING",
    "ResourceOutputSerializing": "ERROR_AT_RESOURCE_OUTPUT_SERIALIZING",
    "ResourceFunctionExecuting": "ERROR_AT_RESOURCE_FUNCTION_EXECUTING",
    "ClientInputSerializing": "ERROR_AT_CLIENT_INPUT_SERIALIZING",
    "ClientOutputDeserializing": "ERROR_AT_CLIENT_OUTPUT_DESERIALIZING",
    "ClientCallingResource": "ERROR_AT_CLIENT_CALLING_RESOURCE",
    "ResourceNotFound": "ERROR_RESOURCE_NOT_FOUND",
    "ResourceUnavailable": "ERROR_RESOURCE_UNAVAILABLE",
    "ResourceAlreadyRegistered": "ERROR_RESOURCE_ALREADY_REGISTERED",
    "StaleResource": "ERROR_STALE_RESOURCE",
    "RegistryUnavailable": "ERROR_REGISTRY_UNAVAILABLE",
    "WriteConflict": "ERROR_WRITE_CONFLICT",
}


def _load_error_code_members() -> dict[str, int]:
    registry = _native.error_registry()
    missing = sorted(set(_NATIVE_TO_PY_ERROR_NAMES) - set(registry))
    extra = sorted(set(registry) - set(_NATIVE_TO_PY_ERROR_NAMES))
    if missing or extra:
        raise RuntimeError(
            "Rust error registry does not match Python facade mapping "
            f"(missing={missing}, extra={extra})"
        )
    return {
        py_name: int(registry[native_name])
        for native_name, py_name in _NATIVE_TO_PY_ERROR_NAMES.items()
    }


ERROR_Code = unique(IntEnum("ERROR_Code", _load_error_code_members()))
```

- [ ] **Step 4: Replace `CCError.serialize()` and `CCError.deserialize()` with native delegation**

In `sdk/python/src/c_two/error.py`, replace both static methods with:

```python
    @staticmethod
    def serialize(err: 'CCError' | None) -> bytes:
        """
        Serialize the error to canonical C-Two error wire bytes.
        """
        if err is None:
            return b''

        message = err.message or 'Error occurred when using C-Two.'
        return _native.encode_error_wire(int(err.code), message)

    @staticmethod
    def deserialize(data) -> 'CCError' | None:
        """
        Deserialize canonical C-Two error wire bytes to a Python error object.
        """
        try:
            decoded = _native.decode_error_wire_parts(data)
        except ValueError as exc:
            return CCError(
                code=ERROR_Code.ERROR_UNKNOWN,
                message=f'Malformed error payload: {exc}',
            )

        if decoded is None:
            return None

        code_value, message = decoded
        try:
            code = ERROR_Code(code_value)
        except ValueError:
            code = ERROR_Code.ERROR_UNKNOWN
            message = f'Unknown error code {code_value}: {message or ""}'
        subclass = _CODE_TO_CLASS.get(code, CCError)
        obj = Exception.__new__(subclass)
        obj.code = code
        obj.message = message or 'Error occurred when using C-Two.'
        return obj
```

Do not add any UTF-8 decode, split, or integer parsing logic in Python.

- [ ] **Step 5: Run focused Python error tests and verify green**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/unit/test_error.py -q --timeout=30
```

Expected: PASS.

- [ ] **Step 6: Run mesh error tests to verify subclass presentation remains intact**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/unit/test_mesh_errors.py -q --timeout=30
```

Expected: PASS.

- [ ] **Step 7: Commit Task 3**

Run:

```bash
git add sdk/python/src/c_two/error.py sdk/python/tests/unit/test_error.py
git commit -m "refactor(python): delegate error wire codec to native"
```

---

### Task 4: Add Boundary Guards Against Python Re-Owning Error Wire Parsing

**Files:**
- Modify: `sdk/python/tests/unit/test_sdk_boundary.py`

- [ ] **Step 1: Add the failing boundary test**

Append this test to `sdk/python/tests/unit/test_sdk_boundary.py`:

```python

def test_error_facade_does_not_reimplement_wire_codec():
    source_path = Path(__file__).resolve().parents[2] / "src" / "c_two" / "error.py"
    source = source_path.read_text(encoding="utf-8")

    forbidden = [
        ".tobytes()",
        ".decode('utf-8')",
        '.decode("utf-8")',
        ".split(':', 1)",
        '.split(":", 1)',
        "int(code_raw)",
        "Unknown error code {code_value}",
        "invalid UTF-8",
        "missing ':' separator",
        "invalid code",
        "encode_error_legacy",
        "decode_error_legacy",
        "to_legacy_bytes",
        "from_legacy_bytes",
    ]

    offenders = [needle for needle in forbidden if needle in source]
    assert offenders == []
    assert "_native.error_registry" in source
    assert "_native.encode_error_wire" in source
    assert "_native.decode_error_wire_parts" in source
```

- [ ] **Step 2: Run the boundary test and verify it passes after Tasks 1-3**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/unit/test_sdk_boundary.py::test_error_facade_does_not_reimplement_wire_codec -q --timeout=30
```

Expected: PASS.

- [ ] **Step 3: Commit Task 4**

Run:

```bash
git add sdk/python/tests/unit/test_sdk_boundary.py
git commit -m "test: guard native error wire authority"
```

---

### Task 5: Update Thin SDK Boundary Document For Implemented Error Authority

**Files:**
- Modify: `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`

- [ ] **Step 1: Replace the Canonical Error section with implemented ownership wording**

In `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`, replace the section starting at `### P1. Canonical error registry and legacy bytes` through its required tests with:

```markdown
### P1. Canonical error registry and wire bytes

**Implemented ownership**

Status: implemented on `dev-feature` by
`docs/plans/2026-05-06-canonical-error-rust-authority.md`.

Rust `core/foundation/c2-error` owns the canonical error code registry and the
canonical `code:message` error wire bytes. The format is named `wire`, not
`legacy`, because it is still the active C-Two error payload format. Python
keeps only SDK presentation: `ERROR_Code` as a public enum facade generated
from `_native.error_registry()`, Python exception subclasses, and subclass
mapping for `CCError.deserialize()`.

**Copy constraints**

- Python `CCError.deserialize(...)` passes the input buffer directly to native
  decode and does not call `memoryview.tobytes()` or `bytes(data)` first.
- Native decode returns only `(code, message)` for the SDK hot path. It does
  not allocate a dict or return the error name for normal Python exception
  construction.
- Native encode writes `code:message` directly into the Python bytes allocation
  and avoids an intermediate Rust `Vec<u8>` where practical.
- Python malformed-payload presentation remains SDK-owned: native parser errors
  become `CCError(ERROR_UNKNOWN, "Malformed error payload: ...")` instead of
  leaking native `ValueError` through user-facing call paths.

**Verified coverage**

- All Python `ERROR_Code` values match the Rust registry.
- `CCError.serialize()` uses native `encode_error_wire()`.
- `CCError.deserialize()` uses native `decode_error_wire_parts()` without a
  Python pre-copy.
- Unknown numeric codes map to `ERROR_UNKNOWN` with canonical Rust context.
- Malformed wire payloads keep Python public behavior.
- Boundary tests prevent Python from reimplementing the `code:message` parser.
```

- [ ] **Step 2: Search for stale legacy naming in active error docs and tests**

Run:

```bash
rg -n "legacy bytes|legacy_error|encode_error_legacy|decode_error_legacy|to_legacy_bytes|from_legacy_bytes|Python wire format|python wire format" docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md sdk/python/tests core/foundation/c2-error sdk/python/native/src/error_ffi.rs sdk/python/src/c_two/error.py
```

Expected: no matches. Mentions in older historical plan files outside this active section are not part of this task.

- [ ] **Step 3: Commit Task 5**

Run:

```bash
git add docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md
git commit -m "docs: mark error wire codec rust-owned"
```

---

### Task 6: Run Full Error Authority Verification

**Files:**
- Verify all changed files.

- [ ] **Step 1: Verify Rust error crate**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-error
```

Expected: PASS.

- [ ] **Step 2: Verify Python error unit and boundary tests**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_error.py \
  sdk/python/tests/unit/test_native_error_registry.py \
  sdk/python/tests/unit/test_mesh_errors.py \
  sdk/python/tests/unit/test_sdk_boundary.py \
  -q --timeout=30
```

Expected: PASS.

- [ ] **Step 3: Verify error propagation integration**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/integration/test_error_propagation.py -q --timeout=30
```

Expected: PASS.

- [ ] **Step 4: Verify native extension build**

Run:

```bash
cargo check --manifest-path sdk/python/native/Cargo.toml -q
```

Expected: PASS.

- [ ] **Step 5: Verify full Rust workspace**

Run:

```bash
cargo test --manifest-path core/Cargo.toml --workspace
```

Expected: PASS.

- [ ] **Step 6: Verify full Python SDK test suite**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/ -q --timeout=30 -rs
```

Expected: PASS, allowing only documented skips for optional example dependencies or unavailable local relay binary.

- [ ] **Step 7: Verify whitespace and stale symbol cleanup**

Run:

```bash
git diff --check
rg -n "to_legacy_bytes|from_legacy_bytes|encode_error_legacy|decode_error_legacy|legacy_error" core/foundation/c2-error sdk/python/native sdk/python/src/c_two/error.py sdk/python/tests/unit/test_error.py sdk/python/tests/unit/test_native_error_registry.py sdk/python/tests/unit/test_sdk_boundary.py
```

Expected: `git diff --check` exits 0 and `rg` prints no matches.

- [ ] **Step 8: Commit final verification notes only if a docs file was updated during verification**

If Task 6 finds a documentation correction, commit it with:

```bash
git add docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md docs/plans/2026-05-06-canonical-error-rust-authority.md
git commit -m "docs: refine error authority verification"
```

If no files changed during verification, do not create an empty commit.

---

## Self-Review

- Spec coverage: The plan covers Rust registry/wire ownership, Python enum facade generation, native encode/decode delegation, copy constraints, removal of `legacy` API names, malformed payload presentation, unknown-code behavior, boundary guard tests, docs update, and full verification.
- Placeholder scan: This plan contains no `TBD`, `TODO`, or undefined follow-up steps. Every code-changing task includes exact files, code snippets, commands, and expected outcomes.
- Type consistency: The same names are used throughout: `to_wire_bytes`, `from_wire_bytes`, `_native.encode_error_wire`, `_native.decode_error_wire_parts`, `ERROR_Code`, and `CCError.serialize` / `CCError.deserialize`.
