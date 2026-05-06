# Thin SDK / Rust Core Boundary Reference

**Date:** 2026-05-04
**Status:** Architecture reference / implementation backlog
**Scope:** Identify Python SDK runtime state and mechanisms that should move
down into the Rust core so future language SDKs can stay thin.

## 0.x Constraint: Prefer Clean Cuts Over Compatibility Debt

C-Two is currently in the 0.x line. Implementation work from this document
should not preserve backwards compatibility for incorrect, experimental, or
superseded internal APIs unless a compatibility window is explicitly requested.
Do not leave dangling fallback behavior, compatibility shims, or zombie modules
after moving behavior into Rust. Remove obsolete code paths, tests, docs, and
module exports in the same change that replaces them.

## Non-Negotiable Runtime Constraint: IPC Must Not Depend On Relay

Direct single-node cross-process IPC must remain a complete standalone mode.
Relay is a discovery/forwarding layer, not the lead authority for IPC.

Required invariants:

- `cc.connect(..., address="ipc://...")` must bypass relay and connect through
  `c2-ipc` directly.
- `cc.register(...)` without `C2_RELAY_ADDRESS` must still start a local IPC
  server and expose `cc.server_address()`.
- A bad or unavailable relay must not break explicit direct IPC connections.
- Rust-side runtime work must build a standalone IPC session first; relay
  registration/name resolution is only an optional projection above that
  session.
- Thread-local same-process dispatch is an SDK/language optimization, not a
  replacement for cross-process IPC.

## Current Boundary Summary

C-Two already has the main language-neutral layers in Rust:

- `core/foundation/c2-config`: environment/default/config resolution.
- `core/foundation/c2-error`: canonical error code registry and wire bytes.
- `core/foundation/c2-mem`: SHM pool, buddy/dedicated/file-spill handles.
- `core/protocol/c2-wire`: wire frame, handshake, control, signal, chunk codec.
- `core/transport/c2-ipc`: direct IPC client, client pool, SHM/chunk transfer.
- `core/transport/c2-server`: direct IPC UDS server, dispatcher, scheduler.
- `core/transport/c2-http`: relay HTTP client, relay-aware route fallback,
  relay server behind the `relay` feature.

The Python SDK is already thinner than earlier versions, but it still owns
several generic runtime mechanisms that future SDKs would otherwise have to
reimplement. Those mechanisms should move to Rust without moving Python domain
logic, CRM decorators, or transferable serialization semantics.

## Keep In Python SDKs

These are language-specific and should not be forced into the Rust core:

- CRM decorators/metaclasses and Python method discovery:
  `sdk/python/src/c_two/crm/`.
- Python resource object invocation and Python exception presentation.
- Transferable dataclass conversion, `serialize`, `deserialize`, and
  `from_buffer` orchestration.
- Python proxy object construction and attaching `crm.client`.
- Thread-local same-process fast path that passes Python objects directly and
  skips serialization.
- SDK UX such as `cc.serve()` banner/signals, provided it does not own core
  transport semantics.

## Downshift Candidates

### P1. Remote IPC scheduler config and execution semantics

**Implemented ownership**

Status: implemented on `dev-feature` by
`docs/plans/2026-05-04-remote-ipc-scheduler-rust-config.md`.

Route concurrency is owned by Rust `c2-server`. Python passes the full typed
`ConcurrencyConfig` during route registration; PyO3 parses `mode`,
`max_pending`, and `max_workers` into the native route handle; Rust acquires
the per-route guard around callback invocation and exposes a read-only
snapshot to the SDK. Python keeps `ConcurrencyConfig` as the typed facade and
keeps `Scheduler.execution_guard()` only as a thin adapter for thread-local
direct calls.

Verified coverage from that implementation includes remote IPC `exclusive`,
`parallel`, and `read_parallel` behavior; large SHM-backed hold input arriving
as `memoryview` over `ShmBuffer`; explicit direct `ipc://` bypassing a bad
relay environment; and thread-local calls continuing to pass Python objects
directly. Remote `max_pending` and `max_workers` remain deferred because Rust
does not yet enforce those route-level limits.

**Why this is core behavior**

Read/write/exclusive/parallel dispatch semantics are not Python-specific. Every
language SDK needs identical behavior once calls cross IPC. If this remains in
Python, future SDKs must reimplement scheduler semantics and risk diverging.

**Zero-copy constraint from design review**

Do not reintroduce an extra Python/Rust bytes copy while moving scheduler
configuration into Rust.

Safe migration boundary:

```text
Python ConcurrencyConfig
  -> pass mode / max_pending / worker-limit intent as typed config
RustServer.register_route(...)
  -> construct Rust Scheduler(mode, access_map, limits)
Rust Scheduler
  -> acquire locks/permits and call callback.invoke(...)
callback.invoke(...)
  -> keep RequestData::Shm / RequestData::Handle / RequestData::Inline
PyO3
  -> keep PyShmBuffer and memoryview(request_buf)
```

Unsafe migration boundary:

```text
Rust scheduler
  -> materialize SHM/handle payload as Python bytes before dispatch
  -> call Python with bytes instead of ShmBuffer/memoryview
```

That unsafe boundary would add the copy that was previously rejected. The P1
work is only to move scheduler *configuration and concurrency enforcement* into
Rust; it must not change the `RequestData -> PyShmBuffer -> memoryview` data
path.

**Thread-local caveat**

Thread-local dispatch is different from remote IPC. It passes Python objects
directly and intentionally skips serialization. Do not route thread-local calls
through Rust bytes dispatch for symmetry. If thread-local concurrency needs to
share semantics with Rust, expose a tiny native read/write permit or keep a thin
Python guard. Treat this as a separate SDK design discussion because each
language SDK may have its own same-process fast path.

**Implementation notes**

1. Native `RustServer.register_route(...)` accepts a concurrency mode string
   from Python.
2. PyO3 parses Python mode strings into
   `c2_server::scheduler::ConcurrencyMode`.
3. `access_map` remains method-index-to-read/write metadata.
4. Rust applies the mode when constructing the per-route scheduler.
5. Python scheduler state is not an authority; it only forwards to the native
   handle for thread-local direct calls and snapshot/debug projection.
6. `max_pending` / `max_workers` are enforced by Rust for both local and
   remote execution paths.

**Required tests**

- Remote IPC `ConcurrencyMode::Exclusive` serializes all calls.
- Remote IPC `ConcurrencyMode::Parallel` permits concurrent writes.
- Remote IPC `ConcurrencyMode::ReadParallel` permits concurrent reads and
  serializes writes.
- Large SHM-backed requests still arrive in Python as `ShmBuffer`/
  `memoryview`, not `bytes`.
- Thread-local calls still skip serialization and still pass Python objects.

### P1. Direct IPC probe/control helpers

**Current Python ownership**

`sdk/python/src/c_two/transport/client/util.py` manually builds UDS control
frames for `ping()` and `shutdown()` using copied frame constants and raw socket
handling.

**Why this is core behavior**

Signal frames, UDS path derivation, and IPC liveness probes are wire/IPC
control-plane behavior. They are language-neutral and already partially defined
in Rust through `c2-wire` constants and `c2-server` signal handling.

**Implementation sketch**

1. Add `c2-ipc` helpers such as `ping(address, timeout)` and
   `shutdown(address, timeout)`.
2. Use Rust `c2-wire` frame/signal constants as the only source of truth.
3. Expose thin PyO3 wrappers from `sdk/python/native`.
4. Replace Python raw socket code with native calls.

**IPC/no-relay impact**

This strengthens direct IPC independence. These helpers must live under
`c2-ipc`/native bindings, not `c2-http` or relay code.

**Required tests**

- `ping("ipc://...")` returns true against a direct IPC server with no relay.
- `shutdown("ipc://...")` stops a direct IPC server with no relay.
- Invalid IPC addresses are rejected through Rust validation.

### P1. Process runtime/session state

**Current Python ownership**

`sdk/python/src/c_two/transport/registry.py` owns process-level state:

- local server object, server id, server address;
- server/client IPC overrides;
- pool default config application;
- relay control client cache;
- lazy server creation;
- registration rollback;
- shutdown ordering.

**Why this is core behavior**

Most of this is not Python-specific. A future SDK should not have to reproduce
server identity generation, standalone IPC session lifecycle, client pool
configuration, and registration transaction rules.

**Correct target shape**

```text
Rust RuntimeSession
  - standalone local IPC server identity/address
  - direct IPC server lifecycle
  - direct IPC client pool and default config
  - route registration handles
  - local unregister/shutdown transaction rules
  - optional RelayProjection for relay register/unregister/resolve
```

Relay projection must not own the base session. The base IPC session exists and
works when relay is absent.

**Implementation sketch**

1. Add a native runtime/session object that can create or hold a direct IPC
   server independent of relay.
2. Let Python provide only language-specific route callback metadata and CRM
   class information.
3. Move server id/address generation into Rust config/identity utilities.
4. Move pool default config application into Rust session setup.
5. Keep Python registry as a facade around the native session and Python proxy
   construction.
6. Remove old Python fallback paths once native session behavior is complete.

**Required tests**

- No relay: `register -> server_address -> connect(address)` works across
  processes.
- Bad relay env: explicit direct IPC address still works.
- Relay register failure does not leave split-brain local/Rust route state.
- Explicit unregister preserves local correctness even if relay cleanup fails.

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

### P2. Hold-mode lease tracking

**Current Python ownership**

`sdk/python/src/c_two/transport/server/hold_registry.py` tracks hold-mode SHM
buffers with weakrefs, ages, warning thresholds, and stats.

**Why this is partly core behavior**

The generic part is lease accounting: which SHM handles are retained, how long
they have been retained, and how much memory is pinned. Python weakrefs are only
one binding-specific notification mechanism.

**Implementation sketch**

1. Add a Rust/native hold lease tracker keyed by SHM handle identity.
2. Keep Python weakref callbacks only as binding hooks that release or mark
   leases.
3. Expose `hold_stats()` from native accounting.
4. Remove duplicate Python-only sweep state after native tracking is complete.

**Required tests**

- Hold-mode SHM memory is counted while a Python view is retained.
- Stats decrease after release/GC.
- Warnings include route, method, bytes, and age.

### P2. Server route slot lifecycle and readiness

**Current Python ownership**

`NativeServerBridge.start()` starts Rust server and polls for socket-file
existence. Python also maintains `_started` separately from native state.

**Why this is core behavior**

Server readiness is native IPC server lifecycle state. Polling socket files in
every SDK would duplicate platform-specific details.

**Implementation sketch**

1. Add a native `start_and_wait(timeout)` or readiness notification to
   `RustServer`.
2. Expose native `is_running`/readiness as the single source of truth.
3. Remove Python socket-file polling and redundant `_started` state where
   possible.

**Required tests**

- `start()` returns only after direct IPC connection can be established.
- Timeout reports a useful error without leaving stale socket files.

### P2. Response allocation decision

**Current Python ownership**

Python dispatcher checks `len(res_part) > shm_threshold`, allocates response
pool memory through the native pool object, writes bytes, and returns SHM
coordinates.

**Why this may move lower**

The response transport choice is generic: inline vs buddy SHM vs chunked reply.
Rust already performs this decision for `ResponseMeta::Inline` in
`c2-server`. Python should ideally return result bytes or memoryview and let
native server choose the reply path.

**Copy constraint**

Moving this lower must not introduce extra copies for large transferable output.
If Python serialization already produced bytes, Rust can choose SHM reply from
those bytes. If Python can expose a memoryview, native should consume that view
directly where safe instead of forcing `bytes(memoryview)`.

**Required tests**

- Large Python result uses SHM reply.
- `memoryview` result does not get forced through an avoidable Python bytes
  copy when a safe direct native write path exists.

### P2. IPC config validation duplication

**Current Python ownership**

`sdk/python/src/c_two/config/ipc.py` keeps allowed-key and forbidden-key checks
that duplicate native resolver checks in `sdk/python/native/src/config_ffi.rs`.

**Implementation sketch**

1. Keep Python `TypedDict` definitions for SDK type hints.
2. Pass override mappings directly to native resolver.
3. Let native resolver produce consistent validation errors.
4. Remove duplicate Python key-validation paths.

**Required tests**

- Unknown server/client IPC override errors still point to the offending key.
- `shm_threshold` remains a global transport policy override, not an IPC role
  override.

### P3. Native method table facade

**Current Python ownership**

`sdk/python/src/c_two/transport/wire.py` retains a Python `MethodTable` for
server dispatch mapping.

**Why low priority**

Method discovery is language-specific, and the Python table is small. Rust
already owns client handshake method tables. Moving this is mostly cleanup, not
a major cross-language risk.

**Implementation sketch**

Expose a native method table type only if it simplifies route registration or
removes duplicated wire limits. Do not prioritize this ahead of scheduler,
runtime/session, IPC control helpers, and error/config canonicalization.

## Suggested Execution Order

1. Remote IPC scheduler config, with explicit zero-copy guardrails.
2. Direct IPC probe/control helpers in `c2-ipc`.
3. Canonical Python error facade over `c2-error`.
4. Native runtime/session design for standalone IPC plus optional relay
   projection.
5. Hold lease tracking and server readiness cleanup.
6. Config validation deduplication.
7. Native method table cleanup only if still useful.

## Regression Matrix For Future Work

Every implementation spawned from this reference should preserve:

- No relay direct IPC cross-process success.
- Bad relay does not affect explicit `ipc://` connection.
- Name-only connect without relay fails clearly when the resource is not local.
- Relay-aware name resolution uses Rust relay-aware HTTP client when configured.
- Thread-local same-process calls do not serialize Python objects.
- SHM-backed remote IPC requests arrive as `ShmBuffer`/`memoryview`.
- Large result replies keep using Rust transport selection and avoid avoidable
  copies.

## Notes For Future SDK Authors

The intended SDK shape is:

```text
Language SDK
  - language contract/decorator/interface model
  - language object invocation
  - language serialization hooks
  - thin typed config facade
  - thin proxy facade

Rust core/native
  - config/default/env resolution
  - direct IPC server/client/session lifecycle
  - wire protocol
  - SHM/chunk/file-spill memory mechanisms
  - remote IPC scheduling
  - relay-aware route fallback
  - canonical error registry/encoding
```

Do not move language-specific transferable behavior into Rust merely for
symmetry. Do move any behavior that a second SDK would otherwise have to copy
verbatim.
