# Transfer Hook Error Taxonomy Repair Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Do not preserve compatibility aliases for old internal error taxonomy during 0.x development.

**Goal:** Split C-Two transfer hook failures by execution side, data direction, and hook type, and repair hold/from_buffer failure cleanup so native buffer leases never stay retained after failed zero-deserialization construction.

**Architecture:** Rust `c2-error` remains the canonical owner of error codes and wire values. Python projects the expanded registry into `ERROR_Code`, adds focused exception classes, and updates `transferable.py` so `deserialize()` and `from_buffer()` failures are distinguishable on both resource input and client output paths. Native buffer leases stay Rust-owned; Python only ensures every failure before a user-visible `HeldResult` or resource-retained view is cleaned up deterministically.

**Tech Stack:** Rust `c2-error`, PyO3 native registry projection, Python SDK transfer wrappers, pytest, Cargo workspace tests.

---

## Non-Negotiable Constraints

- C-Two is 0.x. Do not add compatibility shims, duplicate names, or aliases for superseded error taxonomy.
- `deserialize(...)` means bytes-to-object conversion.
- `from_buffer(...)` / `fromBuffer(...)` means transferable construction from a runtime buffer/view for zero-deserialization or zero-copy operation.
- Rust `c2-error` owns canonical code names and numeric values.
- Python must not hand-code numeric enum values except through native registry tests.
- Hold/from_buffer failure before returning `HeldResult` must release the memoryview and native response buffer immediately.
- Resource input from_buffer failure must release the request memoryview and native request buffer immediately.
- Direct IPC must remain relay-independent.
- Large SHM-backed payloads must not be materialized into Python `bytes` on remote IPC paths.

---

## Target Error Matrix

| Failure point | Execution side | Hook | Canonical Rust code | Python enum | Python exception |
| --- | --- | --- | --- | --- | --- |
| client request input outbound | client | `serialize()` | `ClientInputSerializing = 5` | `ERROR_AT_CLIENT_INPUT_SERIALIZING` | `ClientSerializeInput` |
| resource request input inbound | resource | `deserialize()` | `ResourceInputDeserializing = 1` | `ERROR_AT_RESOURCE_INPUT_DESERIALIZING` | `ResourceDeserializeInput` |
| resource request input inbound | resource | `from_buffer()` | `ResourceInputFromBuffer = 4` | `ERROR_AT_RESOURCE_INPUT_FROM_BUFFER` | `ResourceInputFromBuffer` |
| resource response output outbound | resource | `serialize()` | `ResourceOutputSerializing = 2` | `ERROR_AT_RESOURCE_OUTPUT_SERIALIZING` | `ResourceSerializeOutput` |
| client response output inbound | client | `deserialize()` | `ClientOutputDeserializing = 6` | `ERROR_AT_CLIENT_OUTPUT_DESERIALIZING` | `ClientDeserializeOutput` |
| client response output inbound | client | `from_buffer()` | `ClientOutputFromBuffer = 8` | `ERROR_AT_CLIENT_OUTPUT_FROM_BUFFER` | `ClientOutputFromBuffer` |
| resource function execution | resource | user method body | `ResourceFunctionExecuting = 3` | `ERROR_AT_RESOURCE_FUNCTION_EXECUTING` | `ResourceExecuteFunction` |
| transport/resource call failure | client | remote call | `ClientCallingResource = 7` | `ERROR_AT_CLIENT_CALLING_RESOURCE` | `ClientCallResource` |

Existing numeric values must remain unchanged. New values use existing gaps: `4` for resource-side input-from-buffer and `8` for client-side output-from-buffer.

---

## Files And Responsibilities

- `core/foundation/c2-error/src/lib.rs`
  - Add canonical `ErrorCode` variants and registry entries.
  - Keep wire codec format unchanged.
  - Add/update tests for numeric values, registry names, and roundtrip.

- `sdk/python/src/c_two/error.py`
  - Add native-to-Python enum name mappings.
  - Add `ResourceInputFromBuffer` and `ClientOutputFromBuffer` exception classes.
  - Add `_CODE_TO_CLASS` entries.

- `sdk/python/src/c_two/crm/transferable.py`
  - Track selected input/output hook kind, not only callable.
  - Raise hook-specific CC errors.
  - Release memoryviews/native buffers on from_buffer failure paths.
  - Ensure resource output serialization failures are converted to `ResourceSerializeOutput` instead of escaping as raw Python exceptions.

- `sdk/python/src/c_two/transport/server/native.py`
  - In hold mode, provide an `_release_fn` to the CRM wrapper even though it is not called on success.
  - The release function must release the memoryview first, then the request buffer.
  - Existing retained lease tracking may happen before wrapper invocation, but failure cleanup must clear it through `request_buf.release()`.

- `sdk/python/tests/unit/test_error.py`
  - Cover new native registry projection and Python exception roundtrip.

- `sdk/python/tests/unit/test_transferable.py`
  - Cover wrapper-level hook selection and error class mapping with mock buffers.

- `sdk/python/tests/integration/test_buffer_lease_ipc.py`
  - Cover real direct IPC hold/from_buffer failure cleanup for client output and resource input.

- `sdk/python/tests/integration/test_error_propagation.py` or a focused new integration file if existing patterns are cleaner
  - Cover resource output serialize failure propagation if not already present.

- `AGENTS.md`
  - Update Error Handling guidance with the split transfer hook taxonomy.

- `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`
  - Mark this repair as part of Issue4/Issue5 correctness: Rust owns error registry and buffer lease accounting; SDKs only choose hook-specific presentation errors.

---

## Task 1: Add Failing Rust Error Registry Tests

**Files:**
- Modify: `core/foundation/c2-error/src/lib.rs`

- [ ] **Step 1: Add tests for new canonical codes**

In `core/foundation/c2-error/src/lib.rs`, update the existing `canonical_error_codes_match_wire_values` test to include:

```rust
assert_eq!(u16::from(ErrorCode::ResourceInputFromBuffer), 4);
assert_eq!(u16::from(ErrorCode::ClientOutputFromBuffer), 8);
```

Update the existing registry test expected list to include the new names and values:

```rust
("ResourceInputFromBuffer", 4),
("ClientOutputFromBuffer", 8),
```

Add this focused conversion test if no equivalent exists:

```rust
#[test]
fn from_buffer_error_codes_round_trip_from_wire_values() {
    assert_eq!(
        ErrorCode::try_from(4),
        Ok(ErrorCode::ResourceInputFromBuffer),
    );
    assert_eq!(
        ErrorCode::try_from(8),
        Ok(ErrorCode::ClientOutputFromBuffer),
    );
}
```

- [ ] **Step 2: Run the Rust package test and verify RED**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-error
```

Expected: FAIL because `ErrorCode::ResourceInputFromBuffer` and `ErrorCode::ClientOutputFromBuffer` do not exist yet.

---

## Task 2: Implement Rust Canonical Error Codes

**Files:**
- Modify: `core/foundation/c2-error/src/lib.rs`

- [ ] **Step 1: Add enum variants without changing existing values**

Update `ErrorCode`:

```rust
pub enum ErrorCode {
    Unknown = 0,
    ResourceInputDeserializing = 1,
    ResourceOutputSerializing = 2,
    ResourceFunctionExecuting = 3,
    ResourceInputFromBuffer = 4,
    ClientInputSerializing = 5,
    ClientOutputDeserializing = 6,
    ClientCallingResource = 7,
    ClientOutputFromBuffer = 8,
    ResourceNotFound = 701,
    ResourceUnavailable = 702,
    ResourceAlreadyRegistered = 703,
    StaleResource = 704,
    RegistryUnavailable = 705,
    WriteConflict = 706,
}
```

- [ ] **Step 2: Add registry entries**

Add entries near the related side of the matrix:

```rust
ErrorCodeEntry {
    code: ErrorCode::ResourceInputFromBuffer,
    name: "ResourceInputFromBuffer",
},
ErrorCodeEntry {
    code: ErrorCode::ClientOutputFromBuffer,
    name: "ClientOutputFromBuffer",
},
```

- [ ] **Step 3: Update `TryFrom<u16>`**

Add:

```rust
4 => Ok(ErrorCode::ResourceInputFromBuffer),
8 => Ok(ErrorCode::ClientOutputFromBuffer),
```

- [ ] **Step 4: Run Rust error tests and verify GREEN**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-error
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add core/foundation/c2-error/src/lib.rs
git commit -m "feat(error): add from-buffer transfer error codes"
```

---

## Task 3: Add Failing Python Error Facade Tests

**Files:**
- Modify: `sdk/python/tests/unit/test_error.py`

- [ ] **Step 1: Extend enum value tests**

Update the numeric value test:

```python
assert ERROR_Code.ERROR_AT_RESOURCE_INPUT_FROM_BUFFER == 4
assert ERROR_Code.ERROR_AT_CLIENT_OUTPUT_FROM_BUFFER == 8
```

Update enum length from `13` to `15`.

- [ ] **Step 2: Extend registry projection expected mapping**

In the native registry projection test, add:

```python
"ERROR_AT_RESOURCE_INPUT_FROM_BUFFER": ("ResourceInputFromBuffer", 4),
"ERROR_AT_CLIENT_OUTPUT_FROM_BUFFER": ("ClientOutputFromBuffer", 8),
```

- [ ] **Step 3: Add exception roundtrip tests**

Import the new classes:

```python
from c_two.error import ResourceInputFromBuffer, ClientOutputFromBuffer
```

Add:

```python
def test_from_buffer_errors_round_trip_to_specific_subclasses():
    resource_err = ResourceInputFromBuffer("bad input view")
    restored_resource = CCError.deserialize(CCError.serialize(resource_err))
    assert isinstance(restored_resource, ResourceInputFromBuffer)
    assert restored_resource.code == ERROR_Code.ERROR_AT_RESOURCE_INPUT_FROM_BUFFER
    assert "constructing resource input from buffer" in restored_resource.message
    assert "bad input view" in restored_resource.message

    client_err = ClientOutputFromBuffer("bad output view")
    restored_client = CCError.deserialize(CCError.serialize(client_err))
    assert isinstance(restored_client, ClientOutputFromBuffer)
    assert restored_client.code == ERROR_Code.ERROR_AT_CLIENT_OUTPUT_FROM_BUFFER
    assert "constructing client output from buffer" in restored_client.message
    assert "bad output view" in restored_client.message
```

- [ ] **Step 4: Run Python error tests and verify RED**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_error.py -q --timeout=30
```

Expected: FAIL because Python mapping/classes do not exist yet.

---

## Task 4: Implement Python Error Facade Classes

**Files:**
- Modify: `sdk/python/src/c_two/error.py`
- Test: `sdk/python/tests/unit/test_error.py`

- [ ] **Step 1: Add native registry mappings**

Add to `_NATIVE_TO_PY_ERROR_NAMES`:

```python
"ResourceInputFromBuffer": "ERROR_AT_RESOURCE_INPUT_FROM_BUFFER",
"ClientOutputFromBuffer": "ERROR_AT_CLIENT_OUTPUT_FROM_BUFFER",
```

- [ ] **Step 2: Add exception classes**

Add after `ResourceDeserializeInput`:

```python
class ResourceInputFromBuffer(CCError):
    def __init__(self, message: str | None = None):
        message = 'Error occurred when constructing resource input from buffer' + (f':\n{message}' if message else '')
        super().__init__(code=ERROR_Code.ERROR_AT_RESOURCE_INPUT_FROM_BUFFER, message=message)
```

Add after `ClientDeserializeOutput`:

```python
class ClientOutputFromBuffer(CCError):
    def __init__(self, message: str | None = None):
        message = 'Error occurred when constructing client output from buffer' + (f':\n{message}' if message else '')
        super().__init__(code=ERROR_Code.ERROR_AT_CLIENT_OUTPUT_FROM_BUFFER, message=message)
```

- [ ] **Step 3: Add `_CODE_TO_CLASS` entries**

Add:

```python
ERROR_Code.ERROR_AT_RESOURCE_INPUT_FROM_BUFFER: ResourceInputFromBuffer,
ERROR_Code.ERROR_AT_CLIENT_OUTPUT_FROM_BUFFER: ClientOutputFromBuffer,
```

- [ ] **Step 4: Run Python error tests and verify GREEN**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_error.py -q --timeout=30
```

Expected: PASS.

- [ ] **Step 5: Run native registry boundary tests**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_native_error_registry.py sdk/python/tests/unit/test_sdk_boundary.py -q --timeout=30
```

Expected: PASS. If `test_sdk_boundary.py` asserts the old registry count or mapping, update it to require the new Rust-owned entries and no Python parser fallback.

- [ ] **Step 6: Commit**

```bash
git add sdk/python/src/c_two/error.py sdk/python/tests/unit/test_error.py sdk/python/tests/unit/test_native_error_registry.py sdk/python/tests/unit/test_sdk_boundary.py
git commit -m "feat(python): expose from-buffer transfer errors"
```

---

## Task 5: Add Failing Unit Tests For Client Output Hook Split And Cleanup

**Files:**
- Modify: `sdk/python/tests/unit/test_transferable.py`

- [ ] **Step 1: Add a mock retained response**

In `TestComToCrmBufferModes`, add a helper that supports `track_retained()` and deterministic release:

```python
def _make_retained_response(self, data: bytes, tracker):
    class RetainedResponse(bytes):
        def __new__(cls, payload):
            return super().__new__(cls, payload)

        def __init__(self, payload):
            self.released = False
            self.lease = None

        def track_retained(self, tracker, route_name, method_name, direction='client_response'):
            self.lease = tracker.track_retained(
                route_name,
                method_name,
                direction,
                'inline',
                len(self),
            )

        def release(self):
            self.released = True
            if self.lease is not None:
                self.lease.release()
                self.lease = None

    return RetainedResponse(data)
```

- [ ] **Step 2: Add test for output `from_buffer()` failure**

```python
def test_hold_output_from_buffer_failure_raises_specific_error_and_releases_response(self):
    from c_two import _native
    from c_two import error
    from c_two.crm.transferable import _build_transfer_wrapper

    tracker = _native.BufferLeaseTracker()

    @cc.transferable
    class BadFromBufferOut:
        def serialize(val: int) -> bytes:
            return pickle.dumps(val)

        def deserialize(data) -> int:
            return pickle.loads(bytes(data))

        def from_buffer(data: memoryview) -> int:
            raise RuntimeError('bad output from_buffer')

    response = self._make_retained_response(pickle.dumps(42), tracker)

    class MockClient:
        supports_direct_call = False
        lease_tracker = tracker
        route_name = 'mock_route'

        def call(self, method, data):
            return response

    class MockCRM:
        direction = '->'
        client = MockClient()

    def fn(self) -> int:
        ...

    wrapped = _build_transfer_wrapper(fn, input=None, output=BadFromBufferOut)
    with pytest.raises(error.ClientOutputFromBuffer) as exc_info:
        wrapped(MockCRM(), _c2_buffer='hold')

    assert 'bad output from_buffer' in str(exc_info.value)
    assert response.released
    assert tracker.stats()['active_holds'] == 0
```

- [ ] **Step 3: Add test that output `deserialize()` failure remains ordinary deserialize error**

```python
def test_view_output_deserialize_failure_remains_client_deserialize_output(self):
    from c_two import error
    from c_two.crm.transferable import _build_transfer_wrapper

    @cc.transferable
    class BadDeserializeOut:
        def serialize(val: int) -> bytes:
            return pickle.dumps(val)

        def deserialize(data) -> int:
            raise RuntimeError('bad output deserialize')

    crm, mock_resp = self._make_icrm(pickle.dumps(42))

    def fn(self) -> int:
        ...

    wrapped = _build_transfer_wrapper(fn, input=None, output=BadDeserializeOut)
    with pytest.raises(error.ClientDeserializeOutput) as exc_info:
        wrapped(crm)

    assert 'bad output deserialize' in str(exc_info.value)
    assert mock_resp.released
```

- [ ] **Step 4: Run focused tests and verify RED**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_transferable.py::TestComToCrmBufferModes -q --timeout=30
```

Expected: FAIL because `ClientOutputFromBuffer` is not raised and the hold failure path does not release response deterministically.

---

## Task 6: Implement Client Output Hook Split And Cleanup

**Files:**
- Modify: `sdk/python/src/c_two/crm/transferable.py`
- Test: `sdk/python/tests/unit/test_transferable.py`

- [ ] **Step 1: Track output hook kind**

At the top of `com_to_crm`, replace the single `output_fn` selection with explicit hook metadata:

```python
if output is not None:
    if _c2_buffer == 'hold' and hasattr(output, 'from_buffer') and callable(output.from_buffer):
        output_fn = output.from_buffer
        output_hook = 'from_buffer'
    else:
        output_fn = output.deserialize
        output_hook = 'deserialize'
else:
    output_fn = None
    output_hook = None
```

- [ ] **Step 2: Move retained tracking after successful `from_buffer()` construction**

In the `hasattr(response, 'release')` branch, make the hold path follow this shape:

```python
mv = memoryview(response)
if _c2_buffer == 'hold':
    try:
        result = output_fn(mv)
        if output_hook == 'from_buffer' and hasattr(response, 'track_retained'):
            tracker = getattr(client, 'lease_tracker', None)
            if tracker is not None:
                route_name = getattr(client, 'route_name', '')
                response.track_retained(
                    tracker,
                    route_name,
                    method_name,
                    'client_response',
                )
    except Exception as exc:
        mv.release()
        try:
            response.release()
        except Exception:
            pass
        if output_hook == 'from_buffer':
            raise error.ClientOutputFromBuffer(str(exc)) from exc
        raise

    def release_cb():
        mv.release()
        try:
            response.release()
        except Exception:
            pass

    return HeldResult(result, release_cb)
```

The non-hold/view branch must continue using `try/finally` and must still raise `ClientDeserializeOutput` through the existing outer exception handler.

- [ ] **Step 3: Preserve outer CC error passthrough**

Keep this existing guard:

```python
except error.CCBaseError:
    raise
```

This ensures `ClientOutputFromBuffer` is not rewrapped as `ClientDeserializeOutput`.

- [ ] **Step 4: Run focused unit tests and verify GREEN**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_transferable.py::TestComToCrmBufferModes -q --timeout=30
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add sdk/python/src/c_two/crm/transferable.py sdk/python/tests/unit/test_transferable.py
git commit -m "fix(python): split client output from-buffer errors"
```

---

## Task 7: Add Failing Tests For Resource Input Hook Split And Request Cleanup

**Files:**
- Modify: `sdk/python/tests/integration/test_buffer_lease_ipc.py`
- Optionally modify: `sdk/python/tests/unit/test_transferable.py`

- [ ] **Step 1: Add direct IPC resource input from_buffer failure test**

Add to `sdk/python/tests/integration/test_buffer_lease_ipc.py`:

```python
@cc.transferable
class BadInputFromBuffer:
    data: memoryview | bytes

    def serialize(value: "BadInputFromBuffer") -> bytes:
        return bytes(value.data)

    def deserialize(data: memoryview) -> "BadInputFromBuffer":
        return BadInputFromBuffer(bytes(data))

    def from_buffer(data: memoryview) -> "BadInputFromBuffer":
        raise RuntimeError("bad resource input from_buffer")


@cc.crm(namespace="cc.test.bad_input_from_buffer", version="0.1.0")
class BadInputCRM:
    @cc.transfer(input=BadInputFromBuffer, output=BytesView, buffer="hold")
    def echo(self, data: BadInputFromBuffer) -> BytesView:
        ...


class BadInputResource:
    def echo(self, data: BadInputFromBuffer) -> BytesView:
        return BytesView(b"unreachable")


def test_resource_input_from_buffer_failure_uses_specific_error_and_releases_request(monkeypatch):
    settings.shm_threshold = 1024
    monkeypatch.delenv("C2_RELAY_ANCHOR_ADDRESS", raising=False)
    cc.register(BadInputCRM, BadInputResource(), name="bad_input_from_buffer")
    cc.serve(blocking=False)
    address = cc.server_address()
    assert address is not None
    client = cc.connect(BadInputCRM, name="bad_input_from_buffer", address=address)
    try:
        with pytest.raises(cc.ResourceInputFromBuffer) as exc_info:
            client.echo(BadInputFromBuffer(b"x" * 8192))
        assert "bad resource input from_buffer" in str(exc_info.value)
        assert cc.hold_stats()["by_direction"]["resource_input"]["active_holds"] == 0
    finally:
        cc.close(client)
```

If `cc.ResourceInputFromBuffer` is not exported at top level, update imports/exports in the implementation task rather than weakening this test.

- [ ] **Step 2: Add unit-level wrapper test for resource input `deserialize()` remaining separate**

In `sdk/python/tests/unit/test_transferable.py`, add a test near `test_release_called_on_exception`:

```python
def test_resource_input_deserialize_failure_remains_resource_deserialize_input(self):
    from c_two import error

    @cc.transferable
    class BadDeserializeIn:
        def serialize(val: int) -> bytes:
            return pickle.dumps(val)

        def deserialize(data) -> int:
            raise RuntimeError('bad resource input deserialize')

    wrapped = self._setup(BadDeserializeIn, buffer='view')
    crm = self._make_icrm()
    err_bytes, result_bytes = wrapped(
        crm,
        pickle.dumps(42),
        _release_fn=lambda: None,
    )
    restored = error.CCError.deserialize(err_bytes)
    assert isinstance(restored, error.ResourceDeserializeInput)
    assert 'bad resource input deserialize' in str(restored)
    assert result_bytes == b''
```

- [ ] **Step 3: Run focused tests and verify RED**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_buffer_lease_ipc.py::test_resource_input_from_buffer_failure_uses_specific_error_and_releases_request sdk/python/tests/unit/test_transferable.py::TestCrmToComBufferModes::test_resource_input_deserialize_failure_remains_resource_deserialize_input -q --timeout=30
```

Expected: the new from_buffer test fails because the current code reports `ResourceDeserializeInput` or leaks a retained request lease.

---

## Task 8: Implement Resource Input Hook Split And Hold Request Cleanup

**Files:**
- Modify: `sdk/python/src/c_two/crm/transferable.py`
- Modify: `sdk/python/src/c_two/transport/server/native.py`
- Modify if needed: `sdk/python/src/c_two/__init__.py`
- Test: `sdk/python/tests/integration/test_buffer_lease_ipc.py`
- Test: `sdk/python/tests/unit/test_transferable.py`

- [ ] **Step 1: Track input hook kind in `crm_to_com`**

Replace the single `input_fn` selection with:

```python
if input is not None:
    if buffer == 'hold' and hasattr(input, 'from_buffer') and callable(input.from_buffer):
        input_fn = input.from_buffer
        input_hook = 'from_buffer'
    else:
        input_fn = input.deserialize
        input_hook = 'deserialize'
else:
    input_fn = None
    input_hook = None
```

- [ ] **Step 2: Split input failure mapping**

In `crm_to_com`, preserve `stage = 'deserialize_input'`, but in the exception handler use hook kind:

```python
if stage == 'deserialize_input':
    if input_hook == 'from_buffer':
        err = error.ResourceInputFromBuffer(str(e))
    else:
        err = error.ResourceDeserializeInput(str(e))
elif stage == 'execute_function':
    err = error.ResourceExecuteFunction(str(e))
else:
    err = error.ResourceSerializeOutput(str(e))
```

Keep the existing `_release_fn` cleanup in the exception handler, but ensure it sets `_release_fn = None` after successful cleanup to prevent double release if additional handling is added later.

- [ ] **Step 3: Pass release function for server hold path**

In `sdk/python/src/c_two/transport/server/native.py`, change the hold branch from direct `result = method(mv)` to this shape:

```python
mv = memoryview(request_buf)
released = False

def release_fn():
    nonlocal released
    if not released:
        released = True
        mv.release()
        try:
            request_buf.release()
        except Exception:
            pass

try:
    result = method(mv, _release_fn=release_fn)
except Exception:
    if not released:
        release_fn()
    raise
```

Do not call `release_fn()` on successful hold-mode dispatch. Success means `from_buffer()` may have returned an object that retains a memoryview-backed value.

- [ ] **Step 4: Keep retained lease tracking compatible with failure cleanup**

The existing pre-call tracking block may remain:

```python
if lease_tracker is not None and hasattr(request_buf, 'track_retained'):
    request_buf.track_retained(...)
```

Because `request_buf.release()` clears the lease when from_buffer fails. Do not move lease ownership into Python weakrefs or a new Python tracker.

- [ ] **Step 5: Export new exceptions if top-level `cc.*` tests use them**

If `sdk/python/src/c_two/__init__.py` re-exports error classes explicitly, add:

```python
ResourceInputFromBuffer,
ClientOutputFromBuffer,
```

Do not add deprecated alias names.

- [ ] **Step 6: Run focused tests and verify GREEN**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_buffer_lease_ipc.py::test_resource_input_from_buffer_failure_uses_specific_error_and_releases_request sdk/python/tests/unit/test_transferable.py::TestCrmToComBufferModes::test_resource_input_deserialize_failure_remains_resource_deserialize_input -q --timeout=30
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add sdk/python/src/c_two/crm/transferable.py sdk/python/src/c_two/transport/server/native.py sdk/python/src/c_two/__init__.py sdk/python/tests/integration/test_buffer_lease_ipc.py sdk/python/tests/unit/test_transferable.py
git commit -m "fix(python): split resource input from-buffer errors"
```

---

## Task 9: Add And Fix Resource Output Serialization Error Coverage

**Files:**
- Modify: `sdk/python/tests/unit/test_transferable.py`
- Modify if needed: `sdk/python/tests/integration/test_error_propagation.py`
- Modify: `sdk/python/src/c_two/crm/transferable.py`

- [ ] **Step 1: Add failing unit test for output serialize failure**

In the CRM-to-resource wrapper tests, add:

```python
def test_resource_output_serialize_failure_is_resource_serialize_output(self):
    from c_two import error

    @cc.transferable
    class BadOutput:
        def serialize(value: int) -> bytes:
            raise RuntimeError('bad resource output serialize')

        def deserialize(data) -> int:
            return 0

    def fn(self, x: int) -> int:
        ...

    input_trans = create_default_transferable(fn, is_input=True)
    wrapped = _build_transfer_wrapper(fn, input=input_trans, output=BadOutput)

    class Resource:
        def echo(self, x: int) -> int:
            return x

    class Contract:
        direction = '<-'
        resource = Resource()

    err_bytes, result_bytes = wrapped(
        Contract(),
        pickle.dumps(42),
        _release_fn=lambda: None,
    )
    restored = error.CCError.deserialize(err_bytes)
    assert isinstance(restored, error.ResourceSerializeOutput)
    assert 'bad resource output serialize' in str(restored)
    assert result_bytes == b''
```

Adjust the method/resource names to match the existing helper structure in `test_transferable.py`; do not weaken the expected error class.

- [ ] **Step 2: Run focused test and verify RED**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_transferable.py::TestCrmToComBufferModes::test_resource_output_serialize_failure_is_resource_serialize_output -q --timeout=30
```

Expected: FAIL if output serialization currently escapes outside the wrapper error packaging.

- [ ] **Step 3: Move output serialization into protected stage**

In `crm_to_com`, include output serialization under a `try` stage. The final shape should be:

```python
try:
    ...
    stage = 'execute_function'
    result = crm_method(*deserialized_args)
    err = None

    stage = 'serialize_output'
    serialized_result = b''
    if output_transferable is not None and result is not None:
        serialized_result = (
            output_transferable(*result) if isinstance(result, tuple)
            else output_transferable(result)
        )
except Exception as e:
    result = None
    serialized_result = b''
    if _release_fn is not None:
        try:
            _release_fn()
            _release_fn = None
        except Exception:
            pass
    if stage == 'deserialize_input':
        ...
    elif stage == 'execute_function':
        err = error.ResourceExecuteFunction(str(e))
    else:
        err = error.ResourceSerializeOutput(str(e))

serialized_error = error.CCError.serialize(err)
return (serialized_error, serialized_result)
```

Ensure `serialized_result` is initialized before the `try` so the return path is always defined.

- [ ] **Step 4: Run focused test and verify GREEN**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_transferable.py::TestCrmToComBufferModes::test_resource_output_serialize_failure_is_resource_serialize_output -q --timeout=30
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add sdk/python/src/c_two/crm/transferable.py sdk/python/tests/unit/test_transferable.py sdk/python/tests/integration/test_error_propagation.py
git commit -m "fix(python): wrap resource output serialization errors"
```

---

## Task 10: Add Integration Tests For Client Output FromBuffer Cleanup Across Inline And SHM

**Files:**
- Modify: `sdk/python/tests/integration/test_buffer_lease_ipc.py`

- [ ] **Step 1: Add bad output transferable and CRM**

Add:

```python
@cc.transferable
class BadOutputFromBuffer:
    data: memoryview | bytes

    def serialize(value: "BadOutputFromBuffer") -> bytes:
        return bytes(value.data)

    def deserialize(data: memoryview) -> "BadOutputFromBuffer":
        return BadOutputFromBuffer(bytes(data))

    def from_buffer(data: memoryview) -> "BadOutputFromBuffer":
        raise RuntimeError("bad client output from_buffer")


@cc.crm(namespace="cc.test.bad_output_from_buffer", version="0.1.0")
class BadOutputCRM:
    @cc.transfer(output=BadOutputFromBuffer)
    def payload(self, size: int) -> BadOutputFromBuffer:
        ...


class BadOutputResource:
    def payload(self, size: int) -> BadOutputFromBuffer:
        return BadOutputFromBuffer(b"x" * size)
```

- [ ] **Step 2: Add parametrized inline/SHM test**

```python
@pytest.mark.parametrize(
    ("size", "expected_storage"),
    [
        (16, "inline"),
        (8192, "shm_or_handle"),
    ],
)
def test_client_output_from_buffer_failure_releases_response_for_inline_and_shm(monkeypatch, size, expected_storage):
    settings.shm_threshold = 1024
    monkeypatch.delenv("C2_RELAY_ANCHOR_ADDRESS", raising=False)
    cc.register(BadOutputCRM, BadOutputResource(), name=f"bad_output_from_buffer_{size}")
    cc.serve(blocking=False)
    address = cc.server_address()
    assert address is not None
    client = cc.connect(BadOutputCRM, name=f"bad_output_from_buffer_{size}", address=address)
    try:
        with pytest.raises(cc.ClientOutputFromBuffer) as exc_info:
            cc.hold(client.payload)(size)
        assert "bad client output from_buffer" in str(exc_info.value)
        stats = cc.hold_stats()
        assert stats["by_direction"]["client_response"]["active_holds"] == 0
        assert stats["active_holds"] == 0
    finally:
        cc.close(client)
```

- [ ] **Step 3: Run focused integration test**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_buffer_lease_ipc.py::test_client_output_from_buffer_failure_releases_response_for_inline_and_shm -q --timeout=30
```

Expected: PASS after Task 6 implementation.

- [ ] **Step 4: Commit**

```bash
git add sdk/python/tests/integration/test_buffer_lease_ipc.py
git commit -m "test: cover from-buffer failure lease cleanup"
```

---

## Task 11: Update Architecture Documentation

**Files:**
- Modify: `AGENTS.md`
- Modify: `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`
- Modify if helpful: `docs/plans/2026-05-06-unified-buffer-lease-rust-authority.md`

- [ ] **Step 1: Update `AGENTS.md` Error Handling section**

Add this guidance under Error Handling:

```markdown
Transfer hook errors are split by hook type. `deserialize()` failures remain
`ERROR_AT_RESOURCE_INPUT_DESERIALIZING` or
`ERROR_AT_CLIENT_OUTPUT_DESERIALIZING`. `from_buffer()` / `fromBuffer()`
failures use `ERROR_AT_RESOURCE_INPUT_FROM_BUFFER` or
`ERROR_AT_CLIENT_OUTPUT_FROM_BUFFER`; do not label zero-deserialization buffer
view construction as deserialization. On any from_buffer failure before a
resource/user receives a retained value, release the native memoryview and
buffer owner immediately so Rust lease stats do not retain stale holds.
```

- [ ] **Step 2: Update thin SDK boundary document**

In `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`, add a status note under Issue4 and Issue5:

```markdown
Follow-up repair: transfer hook errors now distinguish bytes deserialization
from buffer-backed from_buffer construction on both resource input and client
output. Rust `c2-error` owns the canonical codes; SDKs project the registry and
select the precise hook-specific error without re-owning the wire codec.
```

For Issue5, add:

```markdown
Hold-mode lease cleanup includes failure paths: client output from_buffer and
resource input from_buffer failures release their memoryview/native buffer owner
before propagating a CCError, preventing retained lease stats from being pinned
by traceback-local response/request objects.
```

- [ ] **Step 3: Run doc grep guard**

Run:

```bash
rg -n "from_buffer.*deserializ|deserializ.*from_buffer|ClientOutputDeserializing.*from_buffer|ResourceInputDeserializing.*from_buffer" AGENTS.md docs/plans sdk/python/src sdk/python/tests core/foundation/c2-error/src/lib.rs
```

Expected: no misleading statements that classify `from_buffer()` failures as deserialization. Mentions that explicitly contrast the two are acceptable.

- [ ] **Step 4: Commit**

```bash
git add AGENTS.md docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md docs/plans/2026-05-06-unified-buffer-lease-rust-authority.md
git commit -m "docs: document transfer hook error taxonomy"
```

---

## Task 12: Full Verification And Strict Review

**Files:**
- No source changes unless verification exposes a bug.

- [ ] **Step 1: Run Rust tests**

```bash
cargo test --manifest-path core/Cargo.toml -p c2-error
cargo test --manifest-path core/Cargo.toml --workspace
```

Expected: both commands PASS.

- [ ] **Step 2: Rebuild Python native extension**

```bash
uv sync --reinstall-package c-two
```

Expected: build succeeds.

- [ ] **Step 3: Run focused Python suites**

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_error.py \
  sdk/python/tests/unit/test_native_error_registry.py \
  sdk/python/tests/unit/test_transferable.py \
  sdk/python/tests/unit/test_sdk_boundary.py \
  sdk/python/tests/integration/test_buffer_lease_ipc.py \
  -q --timeout=30
```

Expected: PASS.

- [ ] **Step 4: Run full Python test suite**

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/ -q --timeout=30 -rs
```

Expected: PASS. Skips must be understood and documented in the final report.

- [ ] **Step 5: Run formatting/diff checks**

```bash
git diff --check
rg -n "ERROR_AT_RESOURCE_INPUT_FROM_BUFFER|ERROR_AT_CLIENT_OUTPUT_FROM_BUFFER|ResourceInputFromBuffer|ClientOutputFromBuffer" core/foundation/c2-error sdk/python/src sdk/python/tests AGENTS.md docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md
rg -n "from_buffer.*deserializ|deserializ.*from_buffer" sdk/python/src/c_two AGENTS.md docs/plans
```

Expected: `git diff --check` passes; first `rg` shows canonical definitions/tests/docs; second `rg` has no misleading classification except explicit contrast text.

- [ ] **Step 6: Strict review checklist**

Review the final diff manually and confirm:

- Rust registry includes every `ErrorCode` variant exactly once.
- Python `_NATIVE_TO_PY_ERROR_NAMES` has no missing or extra native names.
- No Python hard-coded numeric enum source of truth was introduced.
- `from_buffer()` failures are not wrapped as deserialize errors.
- `deserialize()` failures are not wrapped as from_buffer errors.
- Client output hold failure releases response before raising.
- Resource input hold failure releases request buffer before returning remote error.
- Resource output serialization errors return `ResourceSerializeOutput` over the wire.
- Direct IPC tests run with `C2_RELAY_ANCHOR_ADDRESS=` and do not require relay.
- No Python weakref hold registry is reintroduced.
- No compatibility aliases or zombie legacy modules are added.

- [ ] **Step 7: Final commit if verification-only edits were needed**

If verification required fixes after prior commits:

```bash
git add <changed files>
git commit -m "fix: stabilize transfer hook error taxonomy"
```

---

## Completion Criteria

This repair is complete only when:

- `ResourceInputFromBuffer = 4` and `ClientOutputFromBuffer = 8` are canonical Rust error codes.
- Python `ERROR_Code` projects both codes from `_native.error_registry()`.
- Public Python exceptions exist for both new codes.
- The six transfer hook failure classes in the target matrix are covered by tests.
- Client output `from_buffer()` failure leaves `cc.hold_stats()["active_holds"] == 0` while the exception object is still alive.
- Resource input `from_buffer()` failure leaves `cc.hold_stats()["by_direction"]["resource_input"]["active_holds"] == 0` after the failed call.
- Existing direct IPC and relay-independent behavior remains intact.
- Full Rust and Python verification commands pass, with any skips explicitly explained.
