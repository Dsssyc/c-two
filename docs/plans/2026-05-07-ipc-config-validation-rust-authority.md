# IPC Config Validation Rust Authority Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove Python-owned IPC override key validation so Rust/native config parsing is the single validation authority while preserving typed Python override facades and clear user-facing errors.

**Status:** Implemented on `dev-feature`; final verification is tracked below.

**Architecture:** Python keeps `TypedDict` schemas and forwards override mappings without checking allowed or forbidden keys. `c2-config` owns the canonical IPC override field catalog, and the PyO3 native layer accepts any Python mapping, snapshots it into an owned dict, validates keys and forbidden fields, then builds Rust override structs. Runtime-session storage remains Rust-owned, so Python never keeps a second IPC config validation state.

**Tech Stack:** Rust `c2-config`, PyO3 0.28 native bindings, Python `TypedDict` facades, `uv`/`maturin`, pytest, Cargo workspace tests.

---

## Scope And Non-Negotiable Constraints

- C-Two is 0.x: remove obsolete Python validation helpers instead of keeping aliases or compatibility shims.
- Preserve direct IPC as relay-independent; this issue touches config parsing only and must not route IPC through relay state.
- Keep `shm_threshold` as a global transport policy override through `cc.set_transport_policy()` / `C2Settings`, not a server/client IPC role override.
- Keep Python `BaseIPCOverrides`, `ServerIPCOverrides`, and `ClientIPCOverrides` for typing and SDK documentation.
- Preserve copy/freeze semantics: mutating a caller-owned mapping after `cc.set_server()` / `cc.set_client()` must not mutate Rust runtime-session overrides.
- Preserve `None` value semantics: recognized keys with `None` are treated as unset; unknown or forbidden keys are still rejected even when their value is `None`.
- Semantic IPC key errors should be `ValueError` from native validation. Shape errors such as non-mapping overrides or non-string keys should be `TypeError` from native parsing.

## File Responsibility Map

- `core/foundation/c2-config/src/ipc.rs`: canonical IPC override key catalogs and existing IPC config validation.
- `core/foundation/c2-config/src/lib.rs`: exports the IPC override key catalogs for native bindings.
- `sdk/python/native/src/config_ffi.rs`: PyO3 mapping snapshot, key validation, value extraction, and resolver bindings.
- `sdk/python/native/src/runtime_session_ffi.rs`: accepts Python mappings for server/client override inputs and delegates parsing to `config_ffi`.
- `sdk/python/src/c_two/config/ipc.py`: typed override facade and native resolver forwarding only.
- `sdk/python/src/c_two/transport/registry.py`: passes override mappings directly into native `RuntimeSession`.
- `sdk/python/src/c_two/transport/server/native.py`: continues resolving low-level server config through native resolver.
- `sdk/python/tests/unit/test_ipc_config.py`: behavior tests for native-owned validation, mapping support, copy semantics, and `shm_threshold` role separation.
- `sdk/python/tests/unit/test_sdk_boundary.py`: source-level guard preventing Python IPC key validation from returning.
- `AGENTS.md` and `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`: document the final ownership boundary after implementation.

## Task 1: Add Red Tests For Native-Owned IPC Validation

**Files:**
- Modify: `sdk/python/tests/unit/test_ipc_config.py`
- Modify: `sdk/python/tests/unit/test_sdk_boundary.py`

- [x] **Step 1: Add behavior tests that require native validation**

Replace the existing unknown/forbidden-key assertions in `sdk/python/tests/unit/test_ipc_config.py` with tests that expect native `ValueError` wording and add mapping-shape coverage:

```python
def test_forbidden_server_ipc_override_fields_are_rejected_by_native():
    with pytest.raises(ValueError, match='shm_threshold is a global transport policy'):
        Server(
            bind_address='ipc://unit_bad_server_override',
            ipc_overrides={'shm_threshold': 1},
        )


def test_forbidden_client_ipc_override_fields_are_rejected_by_native():
    with pytest.raises(ValueError, match='shm_threshold is a global transport policy'):
        cc.set_client(ipc_overrides={'shm_threshold': 1})


def test_unknown_ipc_override_field_is_rejected_by_native():
    with pytest.raises(ValueError, match='unknown IPC override option: not_real'):
        Server(bind_address='ipc://unit_unknown_override', ipc_overrides={'not_real': 1})

    with pytest.raises(ValueError, match='unknown IPC override option: not_real'):
        cc.set_server(ipc_overrides={'not_real': 1})

    with pytest.raises(ValueError, match='unknown IPC override option: not_real'):
        cc.set_client(ipc_overrides={'not_real': 1})


def test_removed_max_pool_memory_override_is_rejected_by_native():
    with pytest.raises(ValueError, match='unknown IPC override option: max_pool_memory'):
        Server(
            bind_address='ipc://unit_removed_max_pool_memory',
            ipc_overrides={'max_pool_memory': 1024},
        )

    with pytest.raises(ValueError, match='unknown IPC override option: max_pool_memory'):
        cc.set_client(ipc_overrides={'max_pool_memory': 1024})


def test_ipc_overrides_must_be_mapping_native_error():
    with pytest.raises(TypeError, match='ipc_overrides must be a mapping'):
        cc.set_server(ipc_overrides=object())  # type: ignore[arg-type]

    with pytest.raises(TypeError, match='ipc_overrides must be a mapping'):
        cc.set_client(ipc_overrides=object())  # type: ignore[arg-type]


def test_ipc_override_keys_must_be_strings_native_error():
    with pytest.raises(TypeError, match='IPC override option names must be strings'):
        cc.set_client(ipc_overrides={1: 4096})  # type: ignore[dict-item]
```

- [x] **Step 2: Add mapping-subclass and copy/freeze coverage**

Add this test to the same file so the native parser must accept mappings, snapshot values once, and keep `None` semantics:

```python
def test_ipc_override_mapping_is_snapshotted_by_native():
    from collections import UserDict

    overrides = UserDict({'pool_segment_size': 2 * 1024 * 1024, 'chunk_size': None})
    cc.set_server(ipc_overrides=overrides)  # type: ignore[arg-type]
    overrides['pool_segment_size'] = 4 * 1024 * 1024

    registry = _ProcessRegistry.get()
    assert registry._runtime_session.server_ipc_overrides == {  # noqa: SLF001
        'pool_segment_size': 2 * 1024 * 1024,
    }
```

- [x] **Step 3: Add a source boundary guard**

Append this test to `sdk/python/tests/unit/test_sdk_boundary.py`:

```python
def test_python_ipc_config_facade_does_not_validate_override_keys():
    root = Path(__file__).resolve().parents[2] / 'src' / 'c_two'
    ipc_source = (root / 'config' / 'ipc.py').read_text(encoding='utf-8')
    registry_source = (root / 'transport' / 'registry.py').read_text(encoding='utf-8')

    forbidden = [
        '_SERVER_KEYS',
        '_CLIENT_KEYS',
        '_FORBIDDEN_IPC_KEYS',
        '_clean_ipc_overrides',
        '_normalize_server_ipc_overrides',
        '_normalize_client_ipc_overrides',
        'unknown IPC override',
        'shm_threshold is a global transport policy',
    ]
    offenders = [needle for needle in forbidden if needle in ipc_source or needle in registry_source]
    assert offenders == []
```

- [x] **Step 4: Run the focused tests and verify they fail for the intended reason**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_ipc_config.py sdk/python/tests/unit/test_sdk_boundary.py -q --timeout=30
```

Expected before implementation: failures showing Python still raises `TypeError` for unknown keys and the new boundary guard finds `_normalize_*` / `_clean_ipc_overrides` in Python source.

## Task 2: Move IPC Override Key Catalogs To `c2-config`

**Files:**
- Modify: `core/foundation/c2-config/src/ipc.rs`
- Modify: `core/foundation/c2-config/src/lib.rs`
- Modify: `sdk/python/native/src/config_ffi.rs`

- [x] **Step 1: Add canonical override key catalogs in Rust core**

Add the public constants near the IPC struct definitions in `core/foundation/c2-config/src/ipc.rs`:

```rust
pub const BASE_IPC_OVERRIDE_KEYS: &[&str] = &[
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

pub const SERVER_IPC_OVERRIDE_KEYS: &[&str] = &[
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

pub const CLIENT_IPC_OVERRIDE_KEYS: &[&str] = BASE_IPC_OVERRIDE_KEYS;

pub const FORBIDDEN_IPC_OVERRIDE_KEYS: &[&str] = &[
    "shm_threshold",
];
```

- [x] **Step 2: Export the catalogs from `c2-config`**

Change the export list in `core/foundation/c2-config/src/lib.rs`:

```rust
pub use ipc::{
    BaseIpcConfig, ClientIpcConfig, ServerIpcConfig, BASE_IPC_OVERRIDE_KEYS,
    CLIENT_IPC_OVERRIDE_KEYS, FORBIDDEN_IPC_OVERRIDE_KEYS, SERVER_IPC_OVERRIDE_KEYS,
};
```

- [x] **Step 3: Replace PyO3-local key constants**

In `sdk/python/native/src/config_ffi.rs`, remove `BASE_IPC_KEYS`, `SERVER_IPC_KEYS`, and `CLIENT_IPC_KEYS`, then call the exported constants:

```rust
reject_unknown_ipc_fields(dict, c2_config::SERVER_IPC_OVERRIDE_KEYS)?;
reject_unknown_ipc_fields(dict, c2_config::CLIENT_IPC_OVERRIDE_KEYS)?;
```

Update the forbidden check to use `c2_config::FORBIDDEN_IPC_OVERRIDE_KEYS`:

```rust
fn reject_forbidden_ipc_fields(dict: &Bound<'_, PyDict>) -> PyResult<()> {
    for key in c2_config::FORBIDDEN_IPC_OVERRIDE_KEYS {
        if dict.contains(*key)? {
            return Err(PyValueError::new_err(
                "shm_threshold is a global transport policy; use set_transport_policy(shm_threshold=...)",
            ));
        }
    }
    Ok(())
}
```

- [x] **Step 4: Verify Rust core still compiles**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-config
```

Expected: all `c2-config` tests pass.

## Task 3: Make Native PyO3 Parsing Accept Mappings And Own Shape Validation

**Files:**
- Modify: `sdk/python/native/src/config_ffi.rs`
- Modify: `sdk/python/native/src/runtime_session_ffi.rs`

- [x] **Step 1: Import mapping and type-error support**

In `sdk/python/native/src/config_ffi.rs`, update imports:

```rust
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyMapping};
```

- [x] **Step 2: Snapshot arbitrary Python mappings inside native**

Add this helper before `parse_server_ipc_overrides`:

```rust
fn copy_ipc_overrides<'py>(
    overrides: Option<&Bound<'py, PyAny>>,
) -> PyResult<Option<Bound<'py, PyDict>>> {
    let Some(overrides) = overrides else {
        return Ok(None);
    };
    if overrides.is_none() {
        return Ok(None);
    }
    let mapping = overrides.cast::<PyMapping>().map_err(|_| {
        PyTypeError::new_err("ipc_overrides must be a mapping")
    })?;
    let copied = PyDict::new(overrides.py());
    copied.update(mapping)?;
    Ok(Some(copied))
}
```

- [x] **Step 3: Split public mapping parsers from dict parsers**

Refactor server parsing so public callers pass `PyAny` and internal dict parsing remains simple:

```rust
pub(crate) fn parse_server_ipc_overrides(
    overrides: Option<&Bound<'_, PyAny>>,
) -> PyResult<ServerIpcConfigOverrides> {
    let Some(dict) = copy_ipc_overrides(overrides)? else {
        return Ok(ServerIpcConfigOverrides::default());
    };
    parse_server_ipc_overrides_dict(&dict)
}

fn parse_server_ipc_overrides_dict(
    dict: &Bound<'_, PyDict>,
) -> PyResult<ServerIpcConfigOverrides> {
    reject_forbidden_ipc_fields(dict)?;
    reject_unknown_ipc_fields(dict, c2_config::SERVER_IPC_OVERRIDE_KEYS)?;
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
```

Apply the same pattern to client parsing:

```rust
pub(crate) fn parse_client_ipc_overrides(
    overrides: Option<&Bound<'_, PyAny>>,
) -> PyResult<ClientIpcConfigOverrides> {
    client_overrides(overrides)
}

fn client_overrides(
    overrides: Option<&Bound<'_, PyAny>>,
) -> PyResult<ClientIpcConfigOverrides> {
    let Some(dict) = copy_ipc_overrides(overrides)? else {
        return Ok(ClientIpcConfigOverrides::default());
    };
    reject_forbidden_ipc_fields(&dict)?;
    reject_unknown_ipc_fields(&dict, c2_config::CLIENT_IPC_OVERRIDE_KEYS)?;
    Ok(ClientIpcConfigOverrides {
        pool_enabled: get_opt(&dict, "pool_enabled")?,
        pool_segment_size: get_opt(&dict, "pool_segment_size")?,
        max_pool_segments: get_opt(&dict, "max_pool_segments")?,
        reassembly_segment_size: get_opt(&dict, "reassembly_segment_size")?,
        reassembly_max_segments: get_opt(&dict, "reassembly_max_segments")?,
        max_total_chunks: get_opt(&dict, "max_total_chunks")?,
        chunk_gc_interval_secs: get_opt(&dict, "chunk_gc_interval")?,
        chunk_threshold_ratio: get_opt(&dict, "chunk_threshold_ratio")?,
        chunk_assembler_timeout_secs: get_opt(&dict, "chunk_assembler_timeout")?,
        max_reassembly_bytes: get_opt(&dict, "max_reassembly_bytes")?,
        chunk_size: get_opt(&dict, "chunk_size")?,
        ..Default::default()
    })
}
```

- [x] **Step 4: Make non-string keys fail clearly in native**

Replace `reject_unknown_ipc_fields` with:

```rust
fn reject_unknown_ipc_fields(dict: &Bound<'_, PyDict>, allowed: &[&str]) -> PyResult<()> {
    for item in dict.keys().iter() {
        let key: String = item.extract().map_err(|_| {
            PyTypeError::new_err("IPC override option names must be strings")
        })?;
        if !allowed.contains(&key.as_str()) {
            return Err(PyValueError::new_err(format!(
                "unknown IPC override option: {key}"
            )));
        }
    }
    Ok(())
}
```

- [x] **Step 5: Change exported native functions to accept mappings**

Update the `resolve_server_ipc_config` and `resolve_client_ipc_config` signatures in `sdk/python/native/src/config_ffi.rs`:

```rust
fn resolve_server_ipc_config(
    py: Python<'_>,
    overrides: Option<&Bound<'_, PyAny>>,
    global_overrides: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
```

```rust
fn resolve_client_ipc_config(
    py: Python<'_>,
    overrides: Option<&Bound<'_, PyAny>>,
    global_overrides: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
```

- [x] **Step 6: Change `RuntimeSession` PyO3 input signatures to mappings**

In `sdk/python/native/src/runtime_session_ffi.rs`, update the constructor and setters from `Option<&Bound<'_, PyDict>>` to `Option<&Bound<'_, PyAny>>`:

```rust
server_ipc_overrides: Option<&Bound<'_, PyAny>>,
client_ipc_overrides: Option<&Bound<'_, PyAny>>,
```

```rust
server_ipc_overrides: Option<&Bound<'_, PyAny>>,
```

```rust
fn set_client_ipc_overrides(&self, overrides: Option<&Bound<'_, PyAny>>) -> PyResult<bool> {
```

When an existing call site has a `Bound<'_, PyDict>`, pass `Some(overrides.as_any())` into the parser.

- [x] **Step 7: Verify native bindings compile**

Run:

```bash
cargo check --manifest-path sdk/python/native/Cargo.toml -q
```

Expected: command exits successfully. If PyO3 reports a cast or update API mismatch, use the local PyO3 0.28 API shape already present in `~/.cargo/registry/src/.../pyo3-0.28.3/src/types/mapping.rs`: `Bound<'py, PyAny>::cast::<PyMapping>()` and `PyDictMethods::update(&Bound<'_, PyMapping>)`.

## Task 4: Remove Python IPC Key Validation Helpers

**Files:**
- Modify: `sdk/python/src/c_two/config/ipc.py`
- Modify: `sdk/python/src/c_two/transport/registry.py`

- [x] **Step 1: Thin `config/ipc.py` to types plus native forwarding**

Remove `_SERVER_KEYS`, `_CLIENT_KEYS`, `_FORBIDDEN_IPC_KEYS`, `_normalize_server_ipc_overrides`, `_normalize_client_ipc_overrides`, and `_clean_ipc_overrides`. Keep only the `TypedDict` classes, `_resolve_*` resolver calls, `_native_resolver`, and `_shm_overrides`:

```python
def _resolve_server_ipc_config(
    ipc_overrides: ServerIPCOverrides | Mapping[str, object] | None = None,
    settings: C2Settings | None = None,
) -> dict[str, Any]:
    """Resolve server IPC config through Rust from explicit overrides only."""
    native = _native_resolver()
    return dict(native.resolve_server_ipc_config(
        ipc_overrides,
        _shm_overrides(settings),
    ))


def _resolve_client_ipc_config(
    ipc_overrides: ClientIPCOverrides | Mapping[str, object] | None = None,
    settings: C2Settings | None = None,
) -> dict[str, Any]:
    """Resolve client IPC config through Rust from explicit overrides only."""
    native = _native_resolver()
    return dict(native.resolve_client_ipc_config(
        ipc_overrides,
        _shm_overrides(settings),
    ))
```

- [x] **Step 2: Stop normalizing overrides in `registry.py`**

Replace the IPC config import with:

```python
from c_two.config.ipc import ClientIPCOverrides, ServerIPCOverrides
```

Change `set_server()` to pass the caller mapping directly to native:

```python
self._runtime_session.set_server_options(server_id, ipc_overrides)
```

Change `set_client()` to pass directly:

```python
if not self._runtime_session.set_client_ipc_overrides(ipc_overrides):
```

- [x] **Step 3: Verify no Python validation helpers remain**

Run:

```bash
rg -n "_SERVER_KEYS|_CLIENT_KEYS|_FORBIDDEN_IPC_KEYS|_clean_ipc_overrides|_normalize_server_ipc_overrides|_normalize_client_ipc_overrides|unknown IPC override|shm_threshold is a global transport policy" sdk/python/src/c_two
```

Expected: no matches in Python SDK source. Matches in `sdk/python/native/src/config_ffi.rs` are expected because native owns validation.

## Task 5: Update Docs For The New Config Boundary

**Files:**
- Modify: `AGENTS.md`
- Modify: `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`

- [x] **Step 1: Update `AGENTS.md` config guidance**

In the Config Layer section, add this sentence after the IPC override schema paragraph:

```markdown
IPC override key validation belongs to Rust/native config parsing. Python `ipc.py`
may expose typed override schemas and forward mappings, but it must not keep
allowed-key or forbidden-key validation tables.
```

- [x] **Step 2: Mark issue8 implemented in the thin-sdk boundary document**

After code and tests pass, change the issue8 ledger row to `Implemented` and point to this plan. Replace the issue8 section with an implemented-ownership summary that says Python `ipc.py` is type facade plus native forwarding, `c2-config` owns the key catalog, and PyO3 snapshots Python mappings before validation.

- [x] **Step 3: Run docs whitespace verification**

Run:

```bash
git diff --check
```

Expected: no whitespace errors.

## Task 6: Full Verification And Review

**Files:**
- Verify: Rust workspace, native extension, focused Python tests, full Python suite.

- [x] **Step 1: Run Rust config and native checks**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-config
cargo check --manifest-path sdk/python/native/Cargo.toml -q
```

Expected: both commands pass.

- [x] **Step 2: Rebuild the Python native extension**

Run:

```bash
uv sync --reinstall-package c-two
```

Expected: maturin rebuild succeeds.

- [x] **Step 3: Run focused Python tests**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_ipc_config.py sdk/python/tests/unit/test_runtime_session.py sdk/python/tests/unit/test_sdk_boundary.py -q --timeout=30 -rs
```

Expected: all selected tests pass; no skipped tests in these unit files are caused by relay availability.

- [x] **Step 4: Run full Python suite**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/ -q --timeout=30 -rs
```

Expected: full suite passes. Relay-dependent tests may skip only if the local `c3` relay binary is not built; build it with `python tools/dev/c3_tool.py --build --link` before treating relay coverage as complete.

- [x] **Step 5: Run source-level zombie-code audit**

Run:

```bash
rg -n "_SERVER_KEYS|_CLIENT_KEYS|_FORBIDDEN_IPC_KEYS|_clean_ipc_overrides|_normalize_server_ipc_overrides|_normalize_client_ipc_overrides" sdk/python/src/c_two sdk/python/tests
rg -n "unknown IPC override|shm_threshold is a global transport policy" sdk/python/src/c_two
```

Expected: first command has no matches outside this implementation plan if docs are included intentionally; second command has no matches in Python source. Native Rust matches are expected and are the new authority.

- [x] **Step 6: Review for regressions before handoff**

Check these invariants manually in the diff:

- Python `TypedDict` schemas still expose documented keys and exclude `shm_threshold` and `max_pool_memory`.
- `cc.set_server()` and `cc.set_client()` no longer import or call Python normalization helpers.
- Runtime-session override properties still project copies from Rust-owned state.
- `None` values for recognized keys are ignored by `get_opt()`.
- Unknown and forbidden keys are rejected before value extraction.
- Direct IPC code paths, relay projection, response allocation, and buffer lease tracking are untouched except for config parsing inputs.

- [ ] **Step 7: Commit (optional; not run unless explicitly requested)**

Run:

```bash
git add \
  core/foundation/c2-config/src/ipc.rs \
  core/foundation/c2-config/src/lib.rs \
  sdk/python/native/src/config_ffi.rs \
  sdk/python/native/src/runtime_session_ffi.rs \
  sdk/python/src/c_two/config/ipc.py \
  sdk/python/src/c_two/transport/registry.py \
  sdk/python/tests/unit/test_ipc_config.py \
  sdk/python/tests/unit/test_sdk_boundary.py \
  AGENTS.md \
  docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md

git commit -m "refactor(config): delegate IPC override validation to Rust"
```

Expected: commit succeeds on `dev-feature` with no unrelated files staged.
