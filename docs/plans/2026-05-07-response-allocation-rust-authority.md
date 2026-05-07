# Response Allocation Rust Authority Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move Python IPC response allocation decisions out of the Python SDK and into Rust-native response preparation without adding payload copies for large transferable outputs.

**Architecture:** Python remains responsible for CRM method invocation and serialization orchestration, then returns `None` or a buffer-protocol result to native. PyO3 converts that buffer through a Rust-owned response-preparation helper that decides whether to allocate server response SHM based on the canonical `ServerIpcConfig.shm_threshold`; `c2-server` continues to choose inline/chunked fallback when it receives owned inline bytes. Python no longer receives the response `MemPool`, compares against `shm_threshold`, returns SHM coordinate tuples, or materializes `memoryview` results with `bytes(...)` for transport.

**Tech Stack:** Rust `core/transport/c2-server`, PyO3 bindings in `sdk/python/native/src/server_ffi.rs`, Python SDK dispatch glue in `sdk/python/src/c_two/transport/server/native.py`, pytest unit/integration tests, Cargo tests.

**Implementation status:** Design only; not implemented.

---

## 0.x Clean-Cut Constraint

C-Two is in the 0.x line. Do not keep the current Python response-allocation protocol as a compatibility fallback. Remove the internal Python `response_pool` callback argument, Python `len(res_part) > shm_threshold` branch, Python SHM coordinate tuple returns, and PyO3 tuple parsing once native response preparation is wired. Public CRM behavior must stay stable, but internal SDK/native callback protocol can change cleanly.

## Corrected Scope For Issue 7

Issue7 is not about changing user-facing `@transferable` APIs, `cc.hold()`, or direct thread-local calls. It is specifically about the remote IPC server response path after a Python CRM method has already produced serialized output bytes or a buffer view:

```text
Python CRM dispatch -> serialized output buffer -> native response preparation -> IPC reply frame
```

Python may still call `output.serialize(...)` because serialization is SDK/language-specific. Rust must own the generic transport decision: empty vs inline vs response SHM vs chunked fallback.

## Current Problem

`NativeServerBridge._make_dispatcher()` currently embeds transport policy in Python:

```python
if response_pool is not None and len(res_part) > shm_threshold:
    alloc = response_pool.alloc(len(res_part))
    response_pool.write_from_buffer(res_part, alloc)
    return (seg_idx, alloc.offset, len(res_part), alloc.is_dedicated)

if isinstance(res_part, memoryview):
    res_part = bytes(res_part)
return res_part
```

This has four boundary problems:

1. Python owns `shm_threshold` response decisions even though the threshold and response pool belong to native IPC configuration.
2. Python receives a native response pool object only to allocate transport memory; this is not a language feature and every SDK would have to copy the same policy.
3. The internal tuple `(seg_idx, offset, data_size, is_dedicated)` is a stale SDK/native protocol that exists only because Python currently owns allocation.
4. The inline fallback converts `memoryview` to `bytes` in Python, making it easy to reintroduce avoidable copies when a safe native buffer path exists.

## Target Behavior

- Python dispatcher callable signature becomes `(route_name, method_idx, request_buffer)`.
- Python dispatcher returns only `None` or a bytes-like/buffer-protocol object for successful responses.
- PyO3 accepts `bytes`, `bytearray`, `memoryview`, and other `PyBuffer<u8>` exporters as response payloads.
- Rust-native response preparation uses `ServerIpcConfig.shm_threshold` and `c2-server` response pool authority to decide large-response SHM allocation.
- Large Python `bytes` and `memoryview` outputs copy directly from the Python buffer into response SHM; they must not first become a Rust `Vec<u8>` and then be copied again into SHM.
- Small outputs materialize once into `ResponseMeta::Inline(Vec<u8>)`, because the async send path must own bytes after leaving the GIL.
- If response SHM allocation cannot be obtained, native materializes the payload once as `ResponseMeta::Inline(Vec<u8>)`; existing `send_response_meta()` then performs chunked or inline fallback.
- If copying from the Python buffer into an already allocated SHM block fails, native frees that allocation before returning an internal CRM error; it must not send partial or corrupted response data.
- Direct IPC remains relay-independent. Thread-local same-process calls continue to bypass serialization and this remote response path entirely.

## File Responsibility Map

- Modify `core/transport/c2-server/src/dispatcher.rs`: keep `ResponseMeta` as the language-neutral callback result; update comments so `Inline` means owned response data awaiting native transport selection, not necessarily wire-inline.
- Create or modify `core/transport/c2-server/src/response.rs`: add a pure Rust helper such as `try_prepare_shm_response(...)` that owns the threshold/allocation/write/free decision for large response buffers.
- Modify `core/transport/c2-server/src/lib.rs`: export the new response helper module if needed by PyO3.
- Modify `core/transport/c2-server/src/server.rs`: expose response threshold through a small method such as `response_shm_threshold()` and keep `send_response_meta()` as the final inline/SHM/chunk dispatcher.
- Modify `sdk/python/native/src/server_ffi.rs`: remove cached `PyMemPool` response-pool object, use the `response_pool` argument already supplied to `CrmCallback::invoke()`, call Python dispatcher with three arguments, parse buffer-protocol returns through native response preparation, and remove tuple parsing.
- Modify `sdk/python/src/c_two/transport/server/native.py`: remove `_shm_threshold` response decisions and `response_pool` parameter from the dispatcher; return serialized output buffers directly.
- Modify `sdk/python/tests/unit/test_sdk_boundary.py`: add source-level guards preventing Python response allocation authority and PyO3 tuple compatibility from returning.
- Add `sdk/python/tests/integration/test_response_allocation_ipc.py`: cover large bytes and large memoryview outputs through direct IPC and hold-mode storage accounting.
- Modify `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`: mark issue7 implemented only after code and tests land.
- Modify `AGENTS.md`: after implementation, document that Rust owns response allocation/transport selection and Python dispatch must not receive the response pool.

---

## Task 0: Baseline Audit

**Files:**
- Read: `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`
- Read: `sdk/python/src/c_two/transport/server/native.py`
- Read: `sdk/python/native/src/server_ffi.rs`
- Read: `core/transport/c2-server/src/dispatcher.rs`
- Read: `core/transport/c2-server/src/server.rs`
- Read: `sdk/python/tests/integration/test_buffer_lease_ipc.py`
- Read: `sdk/python/tests/integration/test_ipc_buddy_reply.py`

- [ ] **Step 1: Confirm the worktree baseline**

Run:

```bash
git status --short --branch
```

Expected: branch is `dev-feature`. If previous issue6 changes are still uncommitted, commit or intentionally keep them grouped before editing issue7 code.

- [ ] **Step 2: Record current Python response-allocation ownership**

Run:

```bash
rg -n "response_pool|shm_threshold|len\(res_part\)|write_from_buffer|bytes\(res_part\)|seg_idx|is_dedicated|PyTuple" \
  sdk/python/src/c_two/transport/server/native.py \
  sdk/python/native/src/server_ffi.rs
```

Expected before implementation: matches show Python dispatcher threshold/allocation logic and PyO3 tuple parsing. Expected after implementation: these matches disappear from the Python dispatcher and `parse_response_meta()` path except for unrelated config construction or comments that do not preserve the old protocol.

- [ ] **Step 3: Run baseline focused tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server
cargo check --manifest-path sdk/python/native/Cargo.toml -q
C2_RELAY_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_sdk_boundary.py \
  sdk/python/tests/integration/test_ipc_buddy_reply.py \
  sdk/python/tests/integration/test_buffer_lease_ipc.py \
  -q --timeout=30 -rs
```

Expected: baseline passes before changing behavior. If failures appear, debug them before starting issue7 so response-allocation changes are not mixed with unrelated regressions.

---

## Task 1: Add Rust Response SHM Preparation Helper

**Files:**
- Create or modify: `core/transport/c2-server/src/response.rs`
- Modify: `core/transport/c2-server/src/lib.rs`
- Test: inline unit tests in `core/transport/c2-server/src/response.rs`

- [ ] **Step 1: Write failing Rust tests for native response preparation**

Add tests that assert these exact behaviors:

1. A payload with `len <= shm_threshold` returns `Ok(None)` and does not allocate from the response pool.
2. A payload with `len > shm_threshold` writes directly into the response pool and returns `ResponseMeta::ShmAlloc { ... }` with matching bytes at the allocated coordinates.
3. If the write callback fails after allocation, the helper frees the allocation before returning an error.
4. If pool allocation fails, the helper returns `Ok(None)` so the caller can fall back to owned inline/chunked data.

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server response:: -q
```

Expected: tests fail because the helper does not exist yet.

- [ ] **Step 2: Implement `try_prepare_shm_response`**

Implement a helper with this responsibility and shape:

```rust
pub fn try_prepare_shm_response<F>(
    response_pool: &parking_lot::RwLock<c2_mem::MemPool>,
    shm_threshold: u64,
    len: usize,
    write_into: F,
) -> Result<Option<ResponseMeta>, String>
where
    F: FnOnce(&mut [u8]) -> Result<(), String>;
```

Required behavior:

- Return `Ok(None)` when `len == 0`; the PyO3 caller maps empty responses to `ResponseMeta::Empty` before calling this helper.
- Return `Ok(None)` when `len as u64 <= shm_threshold`.
- Return `Ok(None)` on pool allocation failure so `send_response_meta()` can still choose chunked/inline fallback from owned bytes.
- After successful allocation, obtain a mutable slice into the allocated block and invoke `write_into(dst)` exactly once.
- If `data_ptr` or `write_into` fails, free the allocation before returning `Err(...)`.
- Return `ResponseMeta::ShmAlloc` with `seg_idx`, `offset`, `data_size`, and `is_dedicated` copied from the allocation and requested payload length.
- Reject lengths that cannot be represented by the current buddy response wire `u32 data_size`; do not silently truncate.

- [ ] **Step 3: Export the helper module**

If a new `response.rs` module is used, add it to `core/transport/c2-server/src/lib.rs`:

```rust
pub mod response;
```

Do not expose Python-specific types from this module. The helper must be reusable by future Rust, Go, and Fortran SDK bindings as a length + write-callback abstraction.

- [ ] **Step 4: Run Rust helper tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server response:: -q
cargo test --manifest-path core/Cargo.toml -p c2-server -q
```

Expected: all `c2-server` tests pass.

- [ ] **Step 5: Strict review Task 1**

Review the diff and verify:

- No Python or PyO3 dependency was introduced into `core/transport/c2-server`.
- Allocation failure is fallback-capable; copy/write failure is not silently ignored after a partial write.
- The helper frees allocations on all post-allocation error paths.
- No new response wire format was introduced.

---

## Task 2: Move PyO3 Response Parsing To Native Buffer Preparation

**Files:**
- Modify: `sdk/python/native/src/server_ffi.rs`
- Modify: `core/transport/c2-server/src/server.rs` if a threshold accessor is needed
- Test: `cargo check --manifest-path sdk/python/native/Cargo.toml -q`

- [ ] **Step 1: Add native response threshold projection**

Expose a small method on `c2_server::Server` if needed:

```rust
pub fn response_shm_threshold(&self) -> u64 {
    self.config.shm_threshold
}
```

Use this in `PyServer::build_route()` to store the threshold on `PyCrmCallback`. Do not pass the threshold to Python.

- [ ] **Step 2: Remove cached Python `MemPool` response object**

Remove `response_pool_obj: Py<PyAny>` from `PyCrmCallback`, remove `PyMemPool::from_arc(...)` construction in `PyServer::build_route()`, and remove the now-unused `use crate::mem_ffi::PyMemPool;` import if no other code needs it. Keep the `response_pool` parameter passed by the language-neutral `CrmCallback::invoke()` trait and forward that pool into `parse_response_meta(...)`.

The callback implementation should keep the existing `RequestData` to
`PyShmBuffer` conversion, but the Python call and parser call should use this
shape:

```rust
let args = (route_name, method_idx, buf_obj);
match self.py_callable.call1(py, args) {
    Ok(result) => parse_response_meta(py, result, &response_pool, self.shm_threshold),
    Err(e) => {
        // Keep the existing CrmCallError .error_bytes mapping here.
    }
}
```

- [ ] **Step 3: Replace tuple parser with buffer parser**

Change `parse_response_meta(...)` so it accepts:

- `None` -> `ResponseMeta::Empty`
- `PyBytes` -> direct SHM preparation for large payloads, otherwise `ResponseMeta::Inline(bytes.as_bytes().to_vec())`
- generic `PyBuffer<u8>` -> direct SHM preparation for large payloads, otherwise one `Vec<u8>` materialization

Remove `PyTuple` parsing entirely. A tuple return should now produce an internal error that says the dispatcher must return `None` or a bytes-like buffer, not SHM coordinates.

- [ ] **Step 4: Preserve low-copy behavior for large Python bytes and memoryview**

For large `PyBytes`, call `try_prepare_shm_response(...)` with a writer closure that copies from `bytes.as_bytes()` directly into the SHM destination slice.

For large generic `PyBuffer<u8>`, call `try_prepare_shm_response(...)` with a writer closure that calls `buffer.copy_to_slice(py, dst)` directly into the SHM destination slice.

Only materialize a Rust `Vec<u8>` when the helper returns `Ok(None)` or the payload is at/below threshold.

- [ ] **Step 5: Compile native bindings**

Run:

```bash
cargo fmt --manifest-path core/Cargo.toml --all
cargo fmt --manifest-path sdk/python/native/Cargo.toml
cargo check --manifest-path sdk/python/native/Cargo.toml -q
```

Expected: compile succeeds without unused imports. If PyO3 `PyBuffer` acquisition requires a different 0.28 API shape, adjust the binding code without moving parsing back into Python.

- [ ] **Step 6: Strict review Task 2**

Review the diff and verify:

- `server_ffi.rs` no longer imports `PyTuple` for response parsing.
- `server_ffi.rs` no longer constructs or stores a Python `MemPool` object for response allocation.
- Large buffer paths write directly into response SHM and do not call `.to_vec()` before `try_prepare_shm_response(...)`.
- The old tuple protocol is removed, not aliased.

---

## Task 3: Thin Python Dispatcher Return Path

**Files:**
- Modify: `sdk/python/src/c_two/transport/server/native.py`
- Modify: `sdk/python/src/c_two/transport/server/reply.py` if type hints need broadening
- Test: `sdk/python/tests/unit/test_sdk_boundary.py`

- [ ] **Step 1: Update dispatcher signature and comments**

Change `_make_dispatcher()` documentation and nested `dispatch()` signature from:

```python
def dispatch(_route_name: str, method_idx: int, request_buf: object, response_pool: object) -> object:
```

to:

```python
def dispatch(_route_name: str, method_idx: int, request_buf: object) -> object:
```

The docstring should state that the dispatcher returns `None` or serialized output bytes-like data, and native Rust chooses the transport allocation.

- [ ] **Step 2: Remove Python response threshold/allocation logic**

Delete the branch that reads `shm_threshold`, calls `response_pool.alloc(...)`, calls `response_pool.write_from_buffer(...)`, and returns `(seg_idx, offset, data_size, is_dedicated)`.

After `unpack_resource_result(result)`, the success path should only be:

```python
if err_part:
    raise CrmCallError(err_part)
if not res_part:
    return None
return res_part
```

Do not add `bytes(res_part)` in Python. Buffer validation belongs to PyO3.

- [ ] **Step 3: Remove unused `_shm_threshold` server-bridge state**

If `_shm_threshold` is no longer used outside response allocation, remove the attribute assignment from `NativeServerBridge.__init__()`. Keep passing the resolved threshold into `RustServer(...)`; Rust still needs it.

- [ ] **Step 4: Run Python boundary tests**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/unit/test_sdk_boundary.py -q --timeout=30
```

Expected: existing boundary tests pass before adding issue7-specific guards.

- [ ] **Step 5: Strict review Task 3**

Review the diff and verify:

- Python no longer imports or references response SHM allocation concepts in the dispatcher.
- Python still releases request buffers exactly as before in view and hold input modes.
- Error wrapping via `CrmCallError(err_part)` is unchanged.
- Thread-local direct-call behavior is untouched.

---

## Task 4: Add Boundary And Integration Coverage

**Files:**
- Modify: `sdk/python/tests/unit/test_sdk_boundary.py`
- Add: `sdk/python/tests/integration/test_response_allocation_ipc.py`
- Possibly modify: `sdk/python/tests/integration/test_ipc_buddy_reply.py` only if overlapping assertions should move to the new file

- [ ] **Step 1: Add source guard for Python response allocation authority**

Add a test to `test_sdk_boundary.py` that inspects `NativeServerBridge` source and fails if the dispatcher contains:

```text
response_pool
len(res_part) >
write_from_buffer
bytes(res_part)
seg_idx
is_dedicated
```

Scope the search to `NativeServerBridge._make_dispatcher` so unrelated config or docs do not cause false positives.

- [ ] **Step 2: Add source guard for PyO3 tuple compatibility**

Add a test that reads `sdk/python/native/src/server_ffi.rs` and fails if `parse_response_meta` still accepts tuple SHM coordinates. The guard should reject source patterns such as `PyTuple`, `ShmAlloc` construction from tuple fields, and the error text `seg_idx, offset, data_size` inside the parser.

- [ ] **Step 3: Add large bytes response integration test**

Create `sdk/python/tests/integration/test_response_allocation_ipc.py` with a direct IPC CRM that returns a large `bytes` payload above `settings.shm_threshold`. Use `cc.hold(proxy.method)(...)` and assert `cc.hold_stats()` reports one retained `shm`, `handle`, or `file_spill` client-response lease rather than inline storage.

- [ ] **Step 4: Add large memoryview response integration test**

In the same file, define a custom transferable whose `serialize()` returns `memoryview(value.data)` for output. Return a large payload above threshold and assert the held result round-trips through `from_buffer()` without Python-side materialization and with retained storage reported as `shm`, `handle`, or `file_spill`.

- [ ] **Step 5: Preserve small inline behavior**

Add a small payload case below threshold and assert hold stats report `inline` storage. This prevents the fix from forcing SHM promotion for every response just to simplify allocation ownership.

- [ ] **Step 6: Run focused tests**

Run:

```bash
uv sync --reinstall-package c-two
C2_RELAY_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_sdk_boundary.py \
  sdk/python/tests/integration/test_response_allocation_ipc.py \
  sdk/python/tests/integration/test_ipc_buddy_reply.py \
  sdk/python/tests/integration/test_buffer_lease_ipc.py \
  -q --timeout=30 -rs
```

Expected: all focused tests pass, with no relay dependency.

- [ ] **Step 7: Strict review Task 4**

Review test coverage and verify:

- Tests fail on the old Python allocation branch.
- Tests cover both `bytes` and `memoryview` output exporters.
- Tests distinguish small inline retained results from large SHM/handle/file-spill retained results.
- Tests do not depend on relay.

---

## Task 5: Documentation And Full Verification

**Files:**
- Modify: `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`
- Modify: `docs/plans/2026-05-07-response-allocation-rust-authority.md`
- Modify: `AGENTS.md`

- [ ] **Step 1: Update implementation status after code lands**

Only after Tasks 1-4 pass, update this plan's status to `Implemented on dev-feature` and add final implementation notes for any review-driven repairs.

- [ ] **Step 2: Update the thin-SDK boundary document**

In `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`, mark issue7 implemented and link to this plan. Do not mark issue8 or issue9 implemented.

- [ ] **Step 3: Update AGENTS.md guidance**

Add concise guidance that Rust owns response allocation and transport selection. The Python server dispatcher may return serialized buffers, but must not receive native response pools, compare response length against `shm_threshold`, return SHM coordinate tuples, or materialize `memoryview` outputs for transport.

- [ ] **Step 4: Run full verification**

Run:

```bash
cargo fmt --manifest-path core/Cargo.toml --all -- --check
cargo fmt --manifest-path sdk/python/native/Cargo.toml -- --check
cargo check --manifest-path sdk/python/native/Cargo.toml -q
cargo test --manifest-path core/Cargo.toml --workspace
uv sync --reinstall-package c-two
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/ -q --timeout=30 -rs
git diff --check
rg -n "response_pool|write_from_buffer|bytes\(res_part\)|PyTuple|seg_idx, offset, data_size|is_dedicated\) tuple" \
  sdk/python/src/c_two/transport/server/native.py \
  sdk/python/native/src/server_ffi.rs \
  sdk/python/tests/unit/test_sdk_boundary.py
```

Expected:

- Formatting/check/test commands pass.
- The final `rg` only shows boundary-test forbidden strings or unrelated comments that do not preserve the old protocol. If active code still has Python response-pool allocation or PyO3 tuple parsing, the implementation is not complete.

- [ ] **Step 5: Final strict review before commit/merge**

Review the final diff and verify:

- Python SDK got thinner: no response pool object, threshold comparison, or SHM tuple protocol remains in dispatcher code.
- Rust owns response allocation decisions and cleanup of failed allocations.
- Large response path does not add a Python-buffer -> Rust `Vec` -> SHM double copy.
- Small response path remains inline and does not force SHM promotion.
- Direct IPC remains relay-independent.
- No compatibility aliases or zombie protocol branches were left behind.

---

## Design Risk Review

### Risk: Rust-core helper accidentally becomes PyO3-specific

Keep Python buffer handling in `sdk/python/native/src/server_ffi.rs`. The `c2-server` helper should only know `len`, `MemPool`, threshold, and a write callback. This keeps the mechanism reusable for Rust, Go, and Fortran SDKs.

### Risk: Large memoryview output gets copied twice

The large-buffer path must call `try_prepare_shm_response(...)` before materializing a `Vec<u8>`. A source-level review should reject any implementation that calls `.to_vec()`, `PyBytes::new`, or Python `bytes(...)` before attempting SHM preparation for `len > shm_threshold`.

### Risk: Allocation failure becomes a functional failure

Allocation failure should not fail the CRM call by itself. It should fall back to owned inline data so existing `send_response_meta()` can choose chunked reply when the payload is too large for inline. Only copy/write failure after an allocation should return an error, because the response data cannot be trusted.

### Risk: Tuple compatibility lingers as zombie code

The tuple protocol is an internal artifact of Python-owned allocation. Keeping it after native response preparation would leave two allocation authorities. Remove tuple parsing and update boundary tests to keep it removed.

### Risk: Empty buffers are confused with `None`

Current behavior treats empty `res_part` as no response. Preserve that behavior unless a separate public API decision changes empty-payload semantics. This issue should not alter CRM return semantics.

### Risk: Thread-local direct calls regress

Thread-local proxy calls skip serialization and should not enter this path. Do not route same-process direct calls through native response allocation for symmetry.

## Completion Criteria

Issue7 is complete only when all of the following are true:

- Python dispatcher accepts no response pool and performs no response allocation decision.
- PyO3 response parser accepts `None` or buffer-protocol payloads and rejects coordinate tuples.
- Rust-native response preparation owns threshold-based SHM allocation and cleanup.
- Large `bytes` and large `memoryview` responses use SHM/handle/file-spill retained storage in direct IPC hold-mode tests.
- Small responses remain inline.
- Full Rust and Python test suites pass.
- `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md` and `AGENTS.md` reflect the new authority boundary.
