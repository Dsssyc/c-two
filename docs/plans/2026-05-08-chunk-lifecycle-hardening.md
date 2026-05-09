# Chunk Lifecycle Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status:** Planned.

**Goal:** Make chunked request/response lifecycle cleanup deterministic on both server and client paths, with Rust-owned validation, GC, terminal error replies, and hard admission limits.

**Architecture:** Keep chunk lifecycle authority in Rust core. `c2-wire::ChunkRegistry` remains the shared lifecycle manager; `c2-server` owns chunked request rejection and cleanup; `c2-ipc` owns chunked response cleanup and sender-side metadata validation. Python SDKs must not add cleanup registries, retry logic, or transport-specific validation.

**Tech Stack:** Rust crates `c2-wire`, `c2-server`, `c2-ipc`, `c2-config`; Tokio async tests; Cargo workspace tests; Python pytest only for SDK/native regression smoke after Rust behavior changes.

---

## Source Issue And Boundary Context

This plan is the next actionable robustness item after the
`docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md` downshift ledger review.
It is not a Python SDK downshift item: Issue 9 in that ledger was reviewed and
closed as "no downshift action" because Rust already owns the language-neutral
method-table wire authority. This plan instead implements the Rust-owned chunk
lifecycle gaps scoped in `docs/issues/chunk-management-gaps.md`.

The old broad claim that chunk lifecycle management does not exist is obsolete.
Current code already has:

- `c2-wire::chunk::ChunkRegistry` with insert/feed/finish/abort/GC/connection cleanup.
- `ChunkAssembler::new(...)` receiving `max_total_chunks` and
  `max_reassembly_bytes` from `ChunkConfig`.
- Server-side periodic `gc_sweep()` in `c2-server::Server::run()`.
- Server-side `cleanup_connection(conn_id)` after connection drain.

The remaining gap is path completeness: active client close, stalled client
response reassembly, terminal feed errors, rejected server request replies,
registry-level admission, and sender-side `u16` chunk metadata validation.

## Non-Negotiable Constraints

- Keep this entirely Rust-owned. Do not add Python cleanup registries or
  Python-side chunk metadata validation.
- Preserve direct IPC as relay-independent.
- Preserve zero-copy boundaries for completed chunked payloads:
  `RequestData::Handle` / `ResponseData::Handle` must continue to carry
  native `MemHandle` plus native pool ownership.
- Do not retry or replay CRM calls after any chunk has been sent.
- Do not treat phase splits as partial behavior. Every phase that changes a
  path must leave that path with deterministic cleanup and focused tests.

## File Responsibility Map

- `core/protocol/c2-wire/src/chunk/registry.rs`: admission rules, GC behavior,
  active-count/byte accounting, and unit tests.
- `core/protocol/c2-wire/src/chunk/config.rs`: configuration comments and any
  renamed hard-limit semantics.
- `core/protocol/c2-wire/src/chunk/header.rs`: request/reply chunk metadata
  width expectations and tests.
- `core/transport/c2-ipc/src/client.rs`: sender-side request chunk validation,
  client response chunk processing, client GC task, active close cleanup, and
  client tests.
- `core/transport/c2-server/src/server.rs`: chunked request rejection replies,
  terminal assembly aborts, and server tests.
- `core/foundation/c2-config/src/ipc.rs`: config validation only if hard-limit
  semantics need stronger invariants.
- `docs/issues/chunk-management-gaps.md`: update status after implementation.
- `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`: update the addendum
  once this plan is implemented.

## Task 0: Baseline Confirmation

**Files:**
- Read: `docs/issues/chunk-management-gaps.md`
- Read: `core/protocol/c2-wire/src/chunk/registry.rs`
- Read: `core/transport/c2-ipc/src/client.rs`
- Read: `core/transport/c2-server/src/server.rs`

- [ ] **Step 1: Confirm current worktree state**

Run:

```bash
git status --short --branch
```

Expected: branch is `dev-feature`. If previous relay/doc changes are still
uncommitted, keep them intentionally grouped or commit them before production
code edits.

- [ ] **Step 2: Confirm current chunk lifecycle facts**

Run:

```bash
rg -n "ChunkRegistry|gc_sweep|cleanup_connection|call_chunked|decode_reply_chunk_meta|dispatch_chunked_call" \
  core/protocol/c2-wire/src/chunk/registry.rs \
  core/transport/c2-ipc/src/client.rs \
  core/transport/c2-server/src/server.rs
```

Expected: matches show server GC and cleanup exist, client cleanup only exists
at `recv_loop()` natural exit, and `call_chunked()` casts chunk counts into
`u16`.

## Task 1: Add RED Tests For Client Cleanup And Sender Validation

**Files:**
- Modify: `core/transport/c2-ipc/src/client.rs`

- [ ] **Step 1: Test active close cleanup**

Add a Tokio test under the existing `#[cfg(test)]` module in
`core/transport/c2-ipc/src/client.rs`:

```rust
#[tokio::test]
async fn close_shared_cleans_inflight_chunk_assemblies() {
    let client = IpcClient::new("ipc://chunk_close_cleanup");
    client
        .chunk_registry
        .insert(client.conn_id, 7, 2, 1024)
        .unwrap();
    assert_eq!(client.chunk_registry.active_count(), 1);

    client.close_shared().await;

    assert_eq!(client.chunk_registry.active_count(), 0);
    assert_eq!(client.chunk_registry.total_bytes(), 0);
}
```

Expected before implementation: fails because `close_shared()` does not call
`cleanup_connection()`.

- [ ] **Step 2: Test request chunk metadata validation**

Add a unit test that does not need a live socket by extracting a private helper
in the implementation phase. First write the desired assertion as a RED test:

```rust
#[test]
fn validate_outgoing_chunk_metadata_rejects_unrepresentable_counts() {
    let mut cfg = ClientIpcConfig::default();
    cfg.base.chunk_size = 1;
    cfg.base.max_total_chunks = u32::from(u16::MAX) + 1;
    let err = validate_outgoing_chunk_metadata(u16::MAX as usize + 1, &cfg).unwrap_err();
    assert!(err.to_string().contains("exceeds request chunk metadata limit"));
}

#[test]
fn validate_outgoing_chunk_metadata_rejects_configured_chunk_limit() {
    let mut cfg = ClientIpcConfig::default();
    cfg.base.chunk_size = 128;
    cfg.base.max_total_chunks = 2;
    let err = validate_outgoing_chunk_metadata(3 * 128, &cfg).unwrap_err();
    assert!(err.to_string().contains("exceeds max_total_chunks"));
}
```

Expected before implementation: fails because
`validate_outgoing_chunk_metadata(...)` does not exist.

- [ ] **Step 3: Run the focused RED tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-ipc chunk_metadata -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-ipc close_shared_cleans_inflight_chunk_assemblies -- --nocapture
```

Expected: both fail for the intended missing behavior.

## Task 2: Implement Client Close Cleanup And Sender Validation

**Files:**
- Modify: `core/transport/c2-ipc/src/client.rs`

- [ ] **Step 1: Add sender-side chunk metadata validation**

Add a private helper near `call_chunked()`:

```rust
fn validate_outgoing_chunk_metadata(data_len: usize, config: &ClientIpcConfig) -> Result<usize, IpcError> {
    let chunk_size = usize::try_from(config.chunk_size)
        .map_err(|_| IpcError::Config("chunk_size exceeds platform usize".into()))?;
    if chunk_size == 0 {
        return Err(IpcError::Config("chunk_size must be > 0".into()));
    }
    if data_len == 0 {
        return Err(IpcError::Handshake("chunked call requires non-empty payload".into()));
    }
    let total_chunks = data_len.div_ceil(chunk_size);
    if total_chunks > u16::MAX as usize {
        return Err(IpcError::Config(format!(
            "chunk count {total_chunks} exceeds request chunk metadata limit {}",
            u16::MAX
        )));
    }
    if total_chunks > config.max_total_chunks as usize {
        return Err(IpcError::Config(format!(
            "chunk count {total_chunks} exceeds max_total_chunks {}",
            config.max_total_chunks
        )));
    }
    Ok(total_chunks)
}
```

Use this helper at the start of `call_chunked()` before registering a pending
call or writing any frames.

- [ ] **Step 2: Clean up client assemblies during active close**

In `IpcClient::close_shared()`, after aborting the receive task and before
waking pending callers, add:

```rust
self.chunk_registry.cleanup_connection(self.conn_id);
```

This cleanup must be idempotent so natural `recv_loop()` exit can also call it
without double-freeing.

- [ ] **Step 3: Run focused client tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-ipc close_shared_cleans_inflight_chunk_assemblies -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-ipc validate_outgoing_chunk_metadata -- --nocapture
```

Expected: tests pass.

## Task 3: Add Client Periodic Chunk GC

**Files:**
- Modify: `core/transport/c2-ipc/src/client.rs`

- [ ] **Step 1: Add RED test for client GC**

Add a Tokio test that creates a client with a very small
`chunk_assembler_timeout_secs` and `chunk_gc_interval_secs`, inserts an
assembly, starts the client GC helper, waits long enough for expiry, then
asserts the registry is empty:

```rust
#[tokio::test]
async fn client_chunk_gc_sweeps_expired_assemblies() {
    let mut cfg = ClientIpcConfig::default();
    cfg.base.chunk_assembler_timeout_secs = 0.01;
    cfg.base.chunk_gc_interval_secs = 0.01;
    let client = IpcClient::with_pool(
        "ipc://chunk_gc",
        Arc::new(StdMutex::new(MemPool::new(PoolConfig::default()))),
        cfg,
    );
    client
        .chunk_registry
        .insert(client.conn_id, 11, 2, 1024)
        .unwrap();

    client.start_chunk_gc_task();
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    client.stop_chunk_gc_task();

    assert_eq!(client.chunk_registry.active_count(), 0);
}
```

Expected before implementation: fails because the helper/handle does not exist.

- [ ] **Step 2: Implement GC task ownership**

Add a field to `IpcClient`:

```rust
chunk_gc_handle: Arc<StdMutex<Option<tokio::task::JoinHandle<()>>>>,
```

Initialize it in both constructors. Add private methods:

```rust
fn start_chunk_gc_task(&self) {
    let mut guard = self.chunk_gc_handle.lock().unwrap();
    if guard.is_some() {
        return;
    }
    let registry = self.chunk_registry.clone();
    let interval_duration = registry.config().gc_interval;
    *guard = Some(tokio::spawn(async move {
        let mut interval = tokio::time::interval(interval_duration);
        interval.tick().await;
        loop {
            interval.tick().await;
            let _ = registry.gc_sweep();
        }
    }));
}

fn stop_chunk_gc_task(&self) {
    if let Some(handle) = self.chunk_gc_handle.lock().unwrap().take() {
        handle.abort();
    }
}
```

Call `start_chunk_gc_task()` after successful handshake/receive-loop spawn, and
call `stop_chunk_gc_task()` from `close_shared()`.

- [ ] **Step 3: Run focused client GC tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-ipc client_chunk_gc_sweeps_expired_assemblies -- --nocapture
```

Expected: test passes without hanging.

## Task 4: Make Client Response Feed Errors Terminal And Clean

**Files:**
- Modify: `core/transport/c2-ipc/src/client.rs`

- [ ] **Step 1: Extract a testable response chunk handler**

Move the chunked-response branch in `recv_loop()` into a private helper that
takes `rid`, `recv_buf`, `pending`, `chunk_registry`, and `conn_id`. The helper
must return a boolean indicating whether the frame was handled as chunked.

The behavior must stay identical before cleanup changes.

- [ ] **Step 2: Add RED test for duplicate/invalid chunk cleanup**

Use the helper to simulate:

1. first reply chunk creates an assembly;
2. duplicate first chunk or invalid feed returns an error;
3. pending caller receives an error;
4. registry active count returns to zero.

Expected before cleanup implementation: the pending caller may receive an
error, but registry active count remains nonzero.

- [ ] **Step 3: Abort on terminal response chunk errors**

On `feed()` and `finish()` errors, call:

```rust
chunk_registry.abort(conn_id, rid as u64);
```

before completing the pending call with an error.

- [ ] **Step 4: Run focused client response tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-ipc chunked_reply -- --nocapture
```

Expected: tests pass.

## Task 5: Make Server Rejected Chunked Requests Terminal And Clean

**Files:**
- Modify: `core/transport/c2-server/src/server.rs`

- [ ] **Step 1: Add RED tests for server rejected chunk behavior**

Add tests under `core/transport/c2-server/src/server.rs` that exercise
`dispatch_chunked_call()` with:

- duplicate chunk feed after an assembly exists;
- registry insert rejection from too many chunks;
- malformed first chunk call-control payload.

Use a test writer and decode the reply frame. Expected behavior after repair:
the registry is empty for the request and a `ReplyControl::Error(...)` frame is
written when the request id is known.

- [ ] **Step 2: Add a rejection helper**

Add a helper near `dispatch_chunked_call()`:

```rust
async fn reject_chunked_call(
    server: &Server,
    conn: &Connection,
    writer: &Arc<Mutex<OwnedWriteHalf>>,
    request_id: u64,
    message: impl Into<String>,
    abort_assembly: bool,
) {
    if abort_assembly {
        server.chunk_registry.abort(conn.conn_id(), request_id);
    }
    write_reply(
        writer,
        request_id,
        &ReplyControl::Error(error_wire(
            ErrorCode::ResourceInputDeserializing,
            message.into(),
        )),
    )
    .await;
}
```

Use this helper for terminal decode/insert/feed/finish errors where the request
cannot be dispatched.

- [ ] **Step 3: Run focused server tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server chunked -- --nocapture
```

Expected: chunked server tests pass.

## Task 6: Enforce Registry Hard Admission After GC

**Files:**
- Modify: `core/protocol/c2-wire/src/chunk/registry.rs`
- Modify: `core/protocol/c2-wire/src/chunk/config.rs`

- [ ] **Step 1: Add RED tests for hard active-count and byte limits**

Add tests in `registry.rs`:

```rust
#[test]
fn insert_rejects_when_active_limit_remains_after_gc() {
    let pool = test_pool();
    let cfg = ChunkConfig {
        soft_limit: 1,
        assembler_timeout: Duration::from_secs(60),
        ..ChunkConfig::default()
    };
    let reg = ChunkRegistry::new(pool, cfg);
    reg.insert(1, 1, 1, 1024).unwrap();
    let err = reg.insert(1, 2, 1, 1024).unwrap_err();
    assert!(err.contains("active chunk assembly limit"));
    assert_eq!(reg.active_count(), 1);
}

#[test]
fn insert_rejects_when_total_byte_limit_remains_after_gc() {
    let pool = test_pool();
    let cfg = ChunkConfig {
        max_reassembly_bytes: 1024,
        max_bytes_per_request: 1024,
        assembler_timeout: Duration::from_secs(60),
        ..ChunkConfig::default()
    };
    let reg = ChunkRegistry::new(pool, cfg);
    reg.insert(1, 1, 1, 1024).unwrap();
    let err = reg.insert(1, 2, 1, 1024).unwrap_err();
    assert!(err.contains("total reassembly byte limit"));
    assert_eq!(reg.active_count(), 1);
}
```

Expected before implementation: second insert succeeds.

- [ ] **Step 2: Reject when pressure remains after GC**

In `ChunkRegistry::insert()`, after `gc_sweep()`, recheck active count and
tracked bytes before allocating a new assembler. Return an error if either
limit remains exceeded.

Do not change per-request limits in `ChunkAssembler::new(...)`.

- [ ] **Step 3: Run focused registry tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-wire chunk::registry -- --nocapture
```

Expected: registry tests pass.

## Task 7: Documentation And Full Verification

**Files:**
- Modify: `docs/issues/chunk-management-gaps.md`
- Modify: `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`
- Modify: `docs/plans/2026-05-08-chunk-lifecycle-hardening.md`

- [ ] **Step 1: Update issue status**

After code lands, update `docs/issues/chunk-management-gaps.md` with an
implementation status section listing which findings were closed and any
remaining follow-up.

- [ ] **Step 2: Update boundary reference**

In `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`, change the chunk
lifecycle hardening note from planned to implemented only after tests pass.

- [ ] **Step 3: Run full focused verification**

Run:

```bash
cargo fmt --manifest-path core/Cargo.toml --all --check
cargo test --manifest-path core/Cargo.toml -p c2-wire chunk -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-ipc chunk -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-server chunk -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-config chunk -- --nocapture
cargo test --manifest-path core/Cargo.toml --workspace --all-targets
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_ipc_config.py -q --timeout=30 -rs
git diff --check
```

Expected: all commands pass. If a command fails, record the failure in this
plan before continuing and do not mark the plan implemented.

## Completion Criteria

- Client active close always releases in-flight response assemblies.
- Client response chunk assemblies are periodically GC'd while the connection
  remains open.
- Client response feed/finish terminal errors abort the affected assembly and
  wake the pending caller exactly once.
- Server rejected chunked requests abort affected assemblies and return a
  terminal error reply when `request_id` is known.
- `ChunkRegistry` rejects new admission after GC when active-count or total-byte
  pressure remains over the configured limit.
- `IpcClient::call_chunked()` validates request chunk metadata before pending
  registration or any write.
- No Python SDK code owns chunk cleanup, chunk retry, chunk admission, or chunk
  metadata validation.
