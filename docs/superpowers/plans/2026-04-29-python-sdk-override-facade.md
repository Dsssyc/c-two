# Python SDK Override Facade Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace Python IPC config objects with typed override-only API surfaces backed by the Rust config resolver.

**Architecture:** Python stores only explicit code-level overrides, while Rust remains the owner of environment parsing, defaults, derived values, and validation. `shm_threshold` is exposed only through `set_transport_policy`, then passed as a global resolver override into server/client IPC resolution.

**Tech Stack:** Rust `c2-config` + PyO3 native module, Python SDK `TypedDict`, pytest, cargo tests.

---

### Task 1: Red Tests For The New Python API

**Files:**
- Modify: `sdk/python/tests/unit/test_ipc_config.py`
- Modify: `sdk/python/tests/integration/test_dynamic_pool.py`
- Modify: `sdk/python/tests/integration/test_heartbeat.py`
- Modify: `sdk/python/tests/integration/test_concurrency_safety.py`
- Modify: `sdk/python/tests/integration/test_server.py`

- [ ] **Step 1: Replace public config object tests**

Add tests that assert `c_two.config` no longer exports `BaseIPCConfig`, `ServerIPCConfig`, `ClientIPCConfig`, `build_server_config`, or `build_client_config`; assert it does export `BaseIPCOverrides`, `ServerIPCOverrides`, and `ClientIPCOverrides`.

- [ ] **Step 2: Add resolver-backed behavior tests**

Add tests for process env precedence, `set_transport_policy(shm_threshold=...)`, `set_server(ipc_overrides=...)`, `set_client(ipc_overrides=...)`, and rejection of `shm_threshold` / `max_pool_memory` in IPC overrides.

- [ ] **Step 3: Run the focused Python unit tests and confirm red**

Run: `env C2_ENV_FILE= uv run pytest sdk/python/tests/unit/test_ipc_config.py -q`

Expected: FAIL because old Python config objects and old API signatures still exist.

### Task 2: Rust Resolver And FFI Shape

**Files:**
- Modify: `core/foundation/c2-config/src/resolver.rs`
- Modify: `sdk/python/native/src/config_ffi.rs`

- [ ] **Step 1: Add Rust tests for forbidden role-level threshold overrides**

Add tests proving `RuntimeConfigOverrides.shm_threshold` still affects server/client resolved IPC configs, but `ServerIpcConfigOverrides` and `ClientIpcConfigOverrides` no longer have a `shm_threshold` field.

- [ ] **Step 2: Remove role-level `shm_threshold` fields**

Remove `shm_threshold` from `ServerIpcConfigOverrides` and `ClientIpcConfigOverrides`, and remove the resolver assignments that let role-level overrides replace the global value.

- [ ] **Step 3: Reject non-role Python IPC override fields in FFI**

Update `config_ffi.rs` so server/client IPC override dicts reject `shm_threshold` with an error that points to `set_transport_policy(shm_threshold=...)`. Do not special-case `max_pool_memory`: it is not an IPC override input, so it should be reported through the normal unknown-option path.

- [ ] **Step 4: Verify Rust-focused tests**

Run: `cargo test --manifest-path core/Cargo.toml -p c2-config --lib`

Run: `cargo test --manifest-path sdk/python/native/Cargo.toml --lib`

### Task 3: Python Override Schemas And Internal Resolution

**Files:**
- Modify: `sdk/python/src/c_two/config/ipc.py`
- Modify: `sdk/python/src/c_two/config/__init__.py`

- [ ] **Step 1: Replace dataclasses with `TypedDict` schemas**

Define `BaseIPCOverrides`, `ServerIPCOverrides`, and `ClientIPCOverrides` with per-field comments explaining each setting's meaning. These classes carry no defaults and no value validation.

- [ ] **Step 2: Add internal resolver helpers**

Add internal helpers that accept mapping-like override objects, reject unknown keys and forbidden keys, drop `None`, and return plain resolved dicts from native Rust resolver calls.

- [ ] **Step 3: Export only override schemas**

Update `c_two.config` exports so old public config objects/builders disappear.

### Task 4: Python Runtime API Migration

**Files:**
- Modify: `sdk/python/src/c_two/config/settings.py`
- Modify: `sdk/python/src/c_two/transport/registry.py`
- Modify: `sdk/python/src/c_two/transport/server/native.py`
- Modify: `sdk/python/src/c_two/transport/__init__.py`
- Modify: `sdk/python/src/c_two/__init__.py`

- [ ] **Step 1: Replace `set_config` with `set_transport_policy`**

Expose only `set_transport_policy(shm_threshold=...)` as the process-level global transport policy override. Do not keep `set_config` as an alias.

- [ ] **Step 2: Replace flat IPC kwargs with `ipc_overrides`**

Change `set_server` and `set_client` to accept only `ipc_overrides: ServerIPCOverrides | None` and `ipc_overrides: ClientIPCOverrides | None`.

- [ ] **Step 3: Make low-level `Server` resolver-backed**

Change `NativeServerBridge` to accept `ipc_overrides` instead of `ipc_config` and `shm_threshold`. Resolve through Rust during construction, then pass the resolved plain dict fields to `RustServer`.

- [ ] **Step 4: Configure client pool from resolved client overrides**

Make `_ensure_pool_config()` resolve client IPC config through Rust using process global policy plus stored client overrides before calling `RustClientPool.set_default_config`.

### Task 5: Test, Example, And Documentation Updates

**Files:**
- Modify: `sdk/python/tests/**/*.py`
- Modify: `examples/**/*.py`
- Modify: active README/docs that mention removed Python IPC APIs

- [ ] **Step 1: Replace integration test constructors**

Use `Server(..., ipc_overrides={...})` and `cc.set_server(ipc_overrides={...})` / `cc.set_client(ipc_overrides={...})` in tests.

- [ ] **Step 2: Remove stale public docs**

Remove or rewrite docs that show `cc.set_config`, IPC config dataclasses, `Server(..., ipc_config=...)`, `Server(..., shm_threshold=...)`, old `segment_size` / `max_segments`, or Python-side defaults.

- [ ] **Step 3: Keep examples teaching-focused**

Examples should show one clear entry path per file and remain suitable for manual smoke testing.

### Task 6: Verification

**Files:**
- All modified files

- [ ] **Step 1: Format Rust crates**

Run: `cargo fmt --manifest-path core/Cargo.toml --all`

Run: `cargo fmt --manifest-path sdk/python/native/Cargo.toml --all`

- [ ] **Step 2: Run Rust tests**

Run: `cargo test --manifest-path core/Cargo.toml -p c2-config --lib`

Run: `cargo test --manifest-path sdk/python/native/Cargo.toml --lib`

- [ ] **Step 3: Run focused Python tests**

Run: `env C2_ENV_FILE= uv run pytest sdk/python/tests/unit/test_ipc_config.py -q`

Run: `env C2_ENV_FILE= uv run pytest sdk/python/tests/unit/test_name_collision.py -q`

Run: `env C2_ENV_FILE= uv run pytest sdk/python/tests/integration/test_dynamic_pool.py sdk/python/tests/integration/test_heartbeat.py sdk/python/tests/integration/test_concurrency_safety.py sdk/python/tests/integration/test_server.py -q`

- [ ] **Step 4: Run repository diff hygiene**

Run: `git diff --check`
