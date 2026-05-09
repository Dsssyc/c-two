# Chunk Management Gaps Report

**Original Date**: 2026-04-04
**Last Updated**: 2026-05-08
**Severity**: Medium (bounded leaks today, potential memory-pressure/DoS risk under malformed or stalled chunk flows)
**Scope**: `c2-wire` chunk registry, `c2-server` chunked request dispatch, `c2-ipc` chunked response handling

## Summary

The original report is no longer accurate as written. The current Rust core now
has a shared `c2-wire::chunk::ChunkRegistry`, configurable
`ChunkAssembler::new(...)` limits, server-side periodic GC, and server
connection-close cleanup. Those changes close the old broad claim that chunked
transfer has no lifecycle manager.

The remaining issue is narrower: chunk lifecycle cleanup is still inconsistent
across server and client error paths. Server-side request assemblies are
eventually swept, but malformed chunk sequences often return without aborting
the affected assembly or replying to the caller. Client-side response assemblies
do not have a periodic GC task, and active client close can abort the receive
loop before its cleanup path runs.

## Current Baseline

| Area | Current State |
| --- | --- |
| Shared lifecycle manager | `core/protocol/c2-wire/src/chunk/registry.rs` owns insert/feed/finish/abort/GC/connection cleanup. |
| Per-request chunk limit | `ChunkAssembler::new()` receives `max_total_chunks` and `max_reassembly_bytes` from `ChunkConfig`. |
| Server request reassembly | `c2-server::Server` owns a `ChunkRegistry`, spawns periodic `gc_sweep()`, and calls `cleanup_connection(conn_id)` after connection drain. |
| Client response reassembly | `c2-ipc::IpcClient` owns a `ChunkRegistry` and calls `cleanup_connection(conn_id)` only when `recv_loop` exits normally. |
| Config projection | `ChunkConfig::from_base()` consumes `BaseIpcConfig` fields: `chunk_gc_interval_secs`, `chunk_assembler_timeout_secs`, `max_total_chunks`, and `max_reassembly_bytes`. |

## Affected Configuration Fields

| Field | Current Use | Remaining Gap |
| --- | --- | --- |
| `chunk_gc_interval_secs` | Drives the server `ChunkRegistry` GC task. | Not used by a client-side GC task. |
| `chunk_assembler_timeout_secs` | Used by `ChunkRegistry::gc_sweep()`. | Client only benefits on soft-limit-triggered sweep or natural connection cleanup. |
| `max_total_chunks` | Enforced as a per-request assembler limit. | Not a hard global active-assembly limit; request chunk headers are still 16-bit and need local sender validation. |
| `max_reassembly_bytes` | Enforced per request and used as a registry soft-limit trigger. | Not a hard total concurrent reassembly cap after GC fails to free enough entries. |

## Detailed Findings

### 1. Client Close Can Skip Chunk Cleanup

**Location**: `core/transport/c2-ipc/src/client.rs` - `IpcClient::close_shared()`

`close_shared()` sends a disconnect signal, drops the writer, aborts the receive
task, and wakes pending callers. If the receive task is aborted, the cleanup at
the end of `recv_loop()` does not run, so in-flight response assemblies remain
in the client `ChunkRegistry` until the client object and registry are dropped.

**Impact**: A long-lived shared client or pool can retain reassembly memory after
active close if a chunked response was incomplete.

### 2. Client Has No Periodic Chunk GC

**Location**: `core/transport/c2-ipc/src/client.rs` - client `ChunkRegistry`
ownership and `recv_loop()`

The server starts a GC task in `Server::run()`. The client does not start an
equivalent task. `ChunkRegistry::insert()` can trigger a sweep when soft limits
are reached, and `recv_loop()` cleans up on natural exit, but a server that sends
some reply chunks and then stalls while the connection remains open can leave the
client call pending and the partial assembly retained indefinitely.

**Impact**: A stalled or buggy server can hold client reassembly memory without
closing the connection.

### 3. Chunk Feed Errors Do Not Immediately Abort Assemblies

**Locations**:

- `core/transport/c2-server/src/server.rs` - `dispatch_chunked_call()`
- `core/transport/c2-ipc/src/client.rs` - `recv_loop()`

After an assembly is inserted, a duplicate chunk, out-of-range chunk index, or
oversized chunk returns an error from `ChunkRegistry::feed()`. Both server and
client currently log or report the error, but they do not consistently abort the
affected assembly immediately.

Server-side periodic GC will eventually release the assembly. Client-side
cleanup depends on later soft-limit sweeps, natural receive-loop exit, or object
drop.

**Impact**: Malformed chunk streams retain memory longer than necessary. On the
client side, the retention can be indefinite while the connection stays open.

### 4. Rejected Chunked Requests Can Leave Callers Waiting

**Location**: `core/transport/c2-server/src/server.rs` - `dispatch_chunked_call()`

When the server rejects a chunked request during header decode, call-control
decode, first-chunk validation, registry insert, or feed, it often returns after
logging without writing an error reply. A client that already registered a
pending call can then wait until the connection closes.

This is especially visible when client-side chunk metadata is invalid or exceeds
server limits. The server is correct to reject the request, but it should surface
a terminal reply for the request when the request id is known.

**Impact**: Resource cleanup and caller wakeup become coupled to connection
shutdown instead of request failure.

### 5. Registry Soft Limits Are Not Hard Admission Limits

**Location**: `core/protocol/c2-wire/src/chunk/registry.rs`

`ChunkRegistry::insert()` sweeps expired assemblies when active count or tracked
bytes reaches the configured soft limit, but it continues with allocation even
if the sweep does not reduce pressure. Per-request limits still apply, but the
registry does not enforce a hard concurrent active-count or total-byte cap.

**Impact**: Multiple concurrent assemblies can push memory pressure beyond the
intended registry-level cap, especially when file spill is available.

### 6. Request Chunk Header Width Needs Sender-Side Validation

**Locations**:

- `core/protocol/c2-wire/src/chunk/header.rs`
- `core/transport/c2-ipc/src/client.rs` - `IpcClient::call_chunked()`

Request chunks encode `chunk_idx` and `total_chunks` as `u16`. `call_chunked()`
computes `total_chunks` as `usize` and casts to `u16` without local validation.
Defaults usually keep this safe because `max_total_chunks` is 512, but the wire
format still requires explicit sender-side rejection before writing any chunks.

**Impact**: Misconfiguration or unusually large payloads can produce truncated
chunk metadata. The server may reject or misinterpret the stream, and the client
can be left waiting if no terminal error reply is sent.

## Recommended Repair Plan

Implementation planning now lives in
`docs/plans/2026-05-08-chunk-lifecycle-hardening.md`. Keep that plan as the
execution checklist; keep this issue document as the problem statement and
status record.

### Phase 1: Add Focused RED Tests

Add tests before implementation so every cleanup path has evidence:

1. Client close cleanup:
   - Insert an assembly into an `IpcClient` chunk registry.
   - Call `close_shared()`.
   - Assert `active_count() == 0` even when the receive task is absent or aborted.
2. Client feed error cleanup:
   - Build a registry with one active response assembly.
   - Feed a duplicate or invalid chunk through the client response path.
   - Assert the pending caller is completed with an error and the assembly is
     aborted.
3. Server rejected request reply:
   - Send a malformed or over-limit chunked request frame to `dispatch_chunked_call()`.
   - Assert the registry does not retain the assembly and an error reply is
     written when `request_id` is known.
4. Registry hard admission:
   - Configure a low active-count or byte cap.
   - Insert enough non-expired assemblies to exceed the cap.
   - Assert insert rejects after a GC sweep if pressure remains above the cap.
5. Request chunk sender validation:
   - Configure a chunk size / payload combination that would exceed either
     `u16::MAX` or `max_total_chunks`.
   - Assert `call_chunked()` returns a local error before writing partial chunks.

### Phase 2: Make Client Cleanup Symmetric With Server Cleanup

- Call `self.chunk_registry.cleanup_connection(self.conn_id)` in
  `IpcClient::close_shared()` after aborting the receive task and before waking
  pending callers.
- Add a client-side GC task with the same `ChunkConfig::gc_interval` /
  `gc_sweep()` pattern used by `Server::run()`.
- Store the client GC task handle and abort it during close/drop so it does not
  outlive the client.

### Phase 3: Abort Assemblies On Terminal Chunk Errors

- On server request chunk feed/finish/insert terminal errors, abort
  `(conn_id, request_id)` before returning.
- On client response chunk feed/finish terminal errors, abort
  `(conn_id, request_id)` before completing the pending call with an error.
- Keep duplicate first-chunk insert behavior explicit: the newly allocated
  duplicate assembler is already aborted by `ChunkRegistry::insert()`, but the
  original assembly should either survive for out-of-order benign behavior or be
  aborted if the duplicate first chunk is treated as terminal protocol failure.

### Phase 4: Return Terminal Error Replies For Rejected Chunked Requests

- When `request_id` is known, send `ReplyControl::Error(error_wire(...))` for
  malformed or rejected chunked request streams that cannot be dispatched.
- Use resource-side input/deserialization error codes for malformed request
  payloads; use resource-unavailable or protocol/config errors only where the
  failure is unrelated to user payload decoding.
- Do not retry or replay CRM calls. A failed chunked request should complete
  exactly once with an error.

### Phase 5: Enforce Registry-Level Hard Limits

- Extend `ChunkConfig` with explicit hard semantics for active assemblies and
  total tracked reassembly bytes, or reinterpret the existing `soft_limit` /
  `max_reassembly_bytes` as hard admission after one GC sweep.
- Keep per-request limits in `ChunkAssembler::new()`.
- Return a typed error from `ChunkRegistry::insert()` when admission remains over
  limit after GC, and make server/client callers surface that error.

### Phase 6: Validate Request Chunk Metadata Before Sending

- In `IpcClient::call_chunked()`, reject locally when:
  - `chunk_size == 0` after config resolution.
  - `total_chunks == 0` for non-empty data.
  - `total_chunks > u16::MAX`.
  - `total_chunks > self.config.max_total_chunks`.
- Register the pending call only after all local metadata validation has passed.
- Prefer a local `IpcError::Config` or `IpcError::Handshake` class that existing
  native/Python mappings already handle; do not add Python-only validation.

## Verification Targets

- `cargo test --manifest-path core/Cargo.toml -p c2-wire chunk -- --nocapture`
- `cargo test --manifest-path core/Cargo.toml -p c2-ipc chunk -- --nocapture`
- `cargo test --manifest-path core/Cargo.toml -p c2-server chunk -- --nocapture`
- `cargo test --manifest-path core/Cargo.toml -p c2-config chunk -- --nocapture`
- `cargo test --manifest-path core/Cargo.toml --workspace --all-targets`
- `C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_ipc_config.py -q --timeout=30 -rs`

## Related Files

- `core/protocol/c2-wire/src/assembler.rs` - per-request reassembly buffer and
  per-request size/chunk validation
- `core/protocol/c2-wire/src/chunk/registry.rs` - shared lifecycle manager,
  GC, and connection cleanup
- `core/protocol/c2-wire/src/chunk/config.rs` - chunk config projection from
  `BaseIpcConfig`
- `core/protocol/c2-wire/src/chunk/header.rs` - request/reply chunk metadata
  wire widths
- `core/transport/c2-server/src/server.rs` - server chunked request dispatch,
  periodic GC, and chunked reply writing
- `core/transport/c2-ipc/src/client.rs` - client chunked request sending,
  response reassembly, close behavior, and pending-call wakeup
- `core/foundation/c2-config/src/ipc.rs` - canonical chunk-related config
  defaults and validation
