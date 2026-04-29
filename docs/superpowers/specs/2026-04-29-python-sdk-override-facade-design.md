# Python SDK Override Facade Design

**Date:** 2026-04-29  
**Status:** Draft for review  
**Scope:** Remove Python IPC config objects as configuration authorities and keep Python SDK configuration as typed code-level overrides into the Rust resolver.

## Problem

The Rust `c2-config` resolver is now the intended owner of environment loading, default values, derived values, and validation. The Python SDK still exposes `BaseIPCConfig`, `ServerIPCConfig`, and `ClientIPCConfig` dataclasses that duplicate IPC defaults and validation. The main `cc.register()` path resolves through Rust, but lower-level APIs such as `c_two.transport.Server(...)` can still instantiate `ServerIPCConfig()` directly and bypass `.env` / process environment values.

That leaves Python with a second configuration authority. It also makes future SDKs harder to design because they may copy Python's object model instead of treating Rust as the single source of truth.

## Goal

Replace Python IPC config objects with typed override-only interfaces:

- Python SDKs collect explicit code-level overrides only.
- Missing override fields mean "not specified"; they do not imply Python defaults.
- Rust resolver continues to own env-file loading, process env parsing, defaults, derived fields, and validation.
- Low-level Python transport APIs cannot bypass the resolver by constructing IPC config dataclasses.
- `shm_threshold` remains a global transport policy override, not a server/client IPC override.

The effective precedence remains:

```text
code overrides > process environment variables > env file > Rust defaults
```

## Non-Goals

- Do not preserve compatibility aliases for removed Python config objects. The project is pre-1.0 and the old API creates architectural debt.
- Do not introduce a Python resolver, Python `EnvCatalog`, or Python default matrix.
- Do not expose `max_pool_memory` as an override. It remains derived by Rust from `pool_segment_size * max_pool_segments`.
- Do not make `shm_threshold` part of `ServerIPCOverrides` or `ClientIPCOverrides`.
- Do not change the Rust runtime structs' need to carry resolved `shm_threshold`; this design only changes public override ownership.

## Public Python API

### Global Transport Policy

Replace the broad `cc.set_config(...)` name with a narrower transport policy API:

```python
cc.set_transport_policy(shm_threshold=8192)
```

`shm_threshold` controls the inline UDS vs SHM promotion policy. It affects request and response transport behavior on the local machine/process, so it should be a global transport policy override instead of a server/client IPC setting.

`cc.set_config(...)` should be removed rather than kept as an alias.

### IPC Overrides

Expose typed override schemas using `TypedDict(total=False)`. This keeps the desired `ipc_overrides={...}` call shape while preserving key and value type hints.

```python
from typing import TypedDict


class BaseIPCOverrides(TypedDict, total=False):
    pool_enabled: bool
    pool_segment_size: int
    max_pool_segments: int
    reassembly_segment_size: int
    reassembly_max_segments: int
    max_total_chunks: int
    chunk_gc_interval: float
    chunk_threshold_ratio: float
    chunk_assembler_timeout: float
    max_reassembly_bytes: int
    chunk_size: int


class ServerIPCOverrides(BaseIPCOverrides, total=False):
    max_frame_size: int
    max_payload_size: int
    max_pending_requests: int
    pool_decay_seconds: float
    heartbeat_interval: float
    heartbeat_timeout: float


class ClientIPCOverrides(BaseIPCOverrides, total=False):
    pass
```

These types are override schemas only. They carry no defaults, no env parsing, and no validation beyond runtime unknown-key rejection and native Rust resolver validation.

The following fields are intentionally absent:

- `shm_threshold`: global transport policy, not IPC role config.
- `max_pool_memory`: derived by Rust.

### High-Level SDK API

High-level configuration should accept typed IPC override objects:

```python
cc.set_transport_policy(shm_threshold=8192)

cc.set_server(ipc_overrides={
    "pool_segment_size": 512 * 1024 * 1024,
    "max_pool_segments": 2,
})

cc.set_client(ipc_overrides={
    "reassembly_segment_size": 64 * 1024 * 1024,
})
```

`cc.set_server(...)` and `cc.set_client(...)` should not accept flat IPC kwargs after this migration. Keeping both flat kwargs and `ipc_overrides` would create two Python code-level override shapes that future SDKs might copy.

### Low-Level Server API

`c_two.transport.Server` should keep explicit non-IPC constructor fields, but replace `ipc_config` and `shm_threshold` with resolver-backed override inputs:

```python
from c_two.config import ServerIPCOverrides
from c_two.transport import Server

ipc_overrides: ServerIPCOverrides = {
    "heartbeat_interval": 10.0,
    "heartbeat_timeout": 30.0,
}

server = Server(
    bind_address="ipc://resource",
    ipc_overrides=ipc_overrides,
)
```

Removed constructor inputs:

- `ipc_config`
- `shm_threshold`

If a caller needs to tune `shm_threshold`, they must call `cc.set_transport_policy(...)` before creating servers or clients.

## Internal Data Flow

Python stores only code-level override maps:

1. `cc.set_transport_policy(...)` stores global transport policy overrides.
2. `cc.set_server(ipc_overrides=...)` stores server IPC code overrides.
3. `cc.set_client(ipc_overrides=...)` stores client IPC code overrides.
4. `cc.register()` resolves server IPC config through Rust using the stored global policy and server IPC overrides.
5. `cc.connect()` resolves client IPC config through Rust using the stored global policy and client IPC overrides.
6. `Server(bind_address=..., ipc_overrides=...)` resolves server IPC config through Rust before constructing `RustServer`.

The resolved Rust output may still be represented inside Python as a private implementation detail or plain dict passed to FFI constructors. It must not become a public config object that users instantiate.

## Rust Resolver Changes

Rust should keep `RuntimeConfigOverrides.shm_threshold`.

Rust should remove role-level `shm_threshold` overrides:

- Remove `ServerIpcConfigOverrides.shm_threshold`.
- Remove `ClientIpcConfigOverrides.shm_threshold`.

The resolver should still inject the resolved global `shm_threshold` into `ServerIpcConfig` and `ClientIpcConfig`, because the lower-level transport runtime needs that resolved value to decide inline vs SHM behavior.

FFI parsing should reject `shm_threshold` if it appears inside server/client IPC override dicts and tell users to use `set_transport_policy(shm_threshold=...)`.

## Deletions

Remove these public Python classes:

- `BaseIPCConfig`
- `ServerIPCConfig`
- `ClientIPCConfig`

Remove or replace these public builders:

- `build_server_config`
- `build_client_config`

The native resolver helpers may remain internal if the Python runtime needs them, but they should not expose a Python config-object construction path.

## Error Handling

Python should perform only structural API checks:

- unknown override keys
- `ipc_overrides` must be mapping-like
- `shm_threshold` is not allowed inside IPC overrides
- removed parameters such as `ipc_config` produce `TypeError`

Value validation should remain in Rust resolver and Rust config structs. Python should not duplicate range checks such as `max_pool_segments <= 255` or duration representability.

## Documentation Changes

Update public docs and developer docs to describe:

- `cc.set_transport_policy(shm_threshold=...)` as the global inline-vs-SHM policy.
- `cc.set_server(ipc_overrides=...)` and `cc.set_client(ipc_overrides=...)` as code-level IPC overrides.
- `Server(bind_address=..., ipc_overrides=...)` as the low-level server tuning path.
- Rust resolver owns env variables, defaults, derived fields, and validation.

Remove stale docs that show:

- `cc.set_config(...)`
- `ServerIPCConfig(...)`, `ClientIPCConfig(...)`, or `BaseIPCConfig(...)`
- `Server(..., ipc_config=...)`
- `Server(..., shm_threshold=...)`
- `segment_size` / `max_segments`
- `C2_IPC_SEGMENT_SIZE` / `C2_IPC_MAX_SEGMENTS`
- references to Python config using `pydantic`

## Testing

Required tests:

- `C2_IPC_POOL_SEGMENT_SIZE` affects `c_two.transport.Server(bind_address=...)` when no `ipc_overrides` are supplied.
- `C2_SHM_THRESHOLD` affects low-level `Server(bind_address=...)` through Rust resolver.
- `cc.set_transport_policy(shm_threshold=...)` beats `C2_SHM_THRESHOLD`.
- `cc.set_server(ipc_overrides=...)` beats process env IPC settings.
- `cc.set_client(ipc_overrides=...)` beats process env IPC settings.
- `shm_threshold` inside `ipc_overrides` is rejected.
- `max_pool_memory` inside `ipc_overrides` is rejected.
- Removed `ipc_config` / `shm_threshold` constructor parameters are rejected.
- Public Python module no longer exports `BaseIPCConfig`, `ServerIPCConfig`, `ClientIPCConfig`, `build_server_config`, or `build_client_config`.

## Acceptance Criteria

- No Python public IPC config object duplicates Rust defaults or validation.
- Every Python server/client runtime path resolves IPC config through Rust before constructing Rust transport objects.
- `shm_threshold` has one public code-level override path: `set_transport_policy`.
- IPC override types are type-hintable and have explicit field names.
- Runtime docs and active developer docs do not present stale config APIs or removed env variables.
