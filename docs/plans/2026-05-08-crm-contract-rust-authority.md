# CRM Contract Rust Authority Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status:** Implemented and verified on 2026-05-08.

**Goal:** Move language-neutral CRM contract identity and route-method validation into Rust core while keeping language-specific method discovery and Python object invocation in the SDK.

**Architecture:** SDKs extract a CRM contract descriptor from their language-native CRM definition and pass it to Rust during registration/connect. Rust owns canonical route metadata, IPC handshake projection, relay route filtering, direct IPC contract checks, and O(1) method-index validity checks before invoking a language callback. Python keeps only Python binding glue: discovering Python methods, building Python dispatch tables, and invoking Python resource methods.

**Tech Stack:** Rust `c2-wire`, `c2-server`, `c2-ipc`, `c2-http`, `c2-runtime`, PyO3 native bindings, Python registry tests.

---

## Scope

This plan implements the first production slice of CRM-as-runtime-protocol:

- CRM namespace/version become part of direct IPC route metadata, not only relay metadata.
- Rust route registration validates that route metadata and runtime route specs agree.
- Rust server rejects invalid method indexes before scheduler acquisition and before language callback invocation.
- Rust IPC clients expose route CRM metadata and can validate an acquired route against an expected CRM namespace/version.
- Name-based relay connections filter candidate routes by expected CRM namespace/version in Rust.
- Python `cc.connect(CRMClass, name=...)` passes expected CRM identity into native runtime for thread-local, direct IPC, and relay-backed connections.

Out of scope for this slice:

- Transferable ABI hashes.
- Cross-language IDL/codegen.
- Full method signature compatibility.
- Explicit `address="http://..."` contract validation was deferred from this
  slice and is addressed by the follow-up relay CRM attestation hardening plan.

## Phase 1: RED Tests And Documentation Correction

**Files:**

- Modify: `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`
- Modify: `core/protocol/c2-wire/src/tests.rs`
- Modify: `core/transport/c2-server/src/server.rs`
- Modify: `core/transport/c2-ipc/src/client.rs`
- Modify: `core/transport/c2-http/src/client/relay_aware.rs`
- Modify: `sdk/python/tests/integration/test_registry.py`
- Modify: `sdk/python/tests/unit/test_runtime_session.py`

- [x] Write failing tests proving IPC handshakes carry route CRM namespace/version.
- [x] Write failing tests proving direct IPC connect rejects CRM identity mismatch.
- [x] Write failing tests proving relay route selection ignores mismatched CRM identities.
- [x] Write failing tests proving invalid method indexes are rejected before callbacks run.
- [x] Update the thin-SDK ledger to reopen Issue 9 as CRM contract descriptor / validation work.

## Phase 2: Wire And Route Metadata

**Files:**

- Modify: `core/protocol/c2-wire/src/handshake.rs`
- Modify: `core/protocol/c2-wire/src/tests.rs`
- Modify: `sdk/python/native/src/wire_ffi.rs`
- Modify: `sdk/python/tests/unit/test_wire.py`
- Modify: `sdk/python/tests/unit/test_security.py`

- [x] Add `crm_ns` and `crm_ver` to `c2_wire::handshake::RouteInfo`.
- [x] Bump handshake version and update canonical fixture tests.
- [x] Encode/decode route CRM metadata before method entries.
- [x] Expose route CRM metadata through PyO3 `RouteInfo`.
- [x] Review wire bounds checks for overlong CRM metadata and malformed buffers.

## Phase 3: Server Contract Catalog And Method Index Guard

**Files:**

- Modify: `core/transport/c2-server/src/dispatcher.rs`
- Modify: `core/transport/c2-server/src/server.rs`
- Modify: `core/runtime/c2-runtime/src/session.rs`
- Modify: `core/runtime/c2-runtime/src/outcome.rs`
- Modify: `sdk/python/native/src/server_ffi.rs`
- Modify: `sdk/python/native/src/runtime_session_ffi.rs`
- Modify: `sdk/python/src/c_two/transport/server/native.py`

- [x] Add CRM namespace/version to `CrmRoute`.
- [x] Validate route/spec CRM metadata in `RuntimeSession::register_route()`.
- [x] Project CRM metadata into IPC handshake route entries.
- [x] Reject invalid `method_idx` before scheduler acquisition and language callback invocation on inline, SHM, and chunked request paths.
- [x] Ensure invalid SHM/chunked method-index paths release request storage before returning an error.

## Phase 4: Client And Relay Contract Matching

**Files:**

- Modify: `core/transport/c2-ipc/src/client.rs`
- Modify: `sdk/python/native/src/client_ffi.rs`
- Modify: `sdk/python/native/src/runtime_session_ffi.rs`
- Modify: `core/runtime/c2-runtime/src/session.rs`
- Modify: `core/transport/c2-http/src/client/relay_aware.rs`
- Modify: `sdk/python/src/c_two/transport/registry.py`
- Modify: `sdk/python/src/c_two/transport/server/native.py`

- [x] Store route CRM namespace/version in IPC client route metadata.
- [x] Add a Rust-owned route contract validation helper for direct IPC clients.
- [x] Pass expected CRM namespace/version from Python `cc.connect()` to native direct IPC and relay paths.
- [x] Filter relay candidates by expected CRM namespace/version in Rust before selecting local IPC or HTTP targets.
- [x] Validate same-process thread-local connects against the registered route CRM identity before returning a zero-serialization proxy.

## Implementation Review Notes

- Wire review: handshake version is bumped to v8, canonical fixtures include
  per-route CRM namespace/version, and overlong CRM metadata is rejected through
  the same one-byte length guard used by route/method names.
- Server review: remote dispatch hot paths add only an O(1)
  `method_idx < method_count` check after route resolution. Invalid inline,
  SHM, and chunked requests return an error before scheduler acquisition or
  language callback invocation; SHM/chunked request storage is released through
  `cleanup_request()`.
- Client/relay review: explicit IPC validation is Rust-owned in `c2-ipc` /
  native RuntimeSession. Relay-aware route selection filters by expected CRM
  identity before local IPC preference or HTTP target probing.
- Python boundary review: Python extracts CRM identity from the decorated class
  and passes it to native; it does not own relay selection, IPC handshake
  trust, or remote method-index validity. Same-process thread-local connects
  compare against the local slot identity before returning a proxy.
- Remediation: low-level `Server(crm_class=..., crm_instance=...)` registration
  originally emitted empty CRM metadata and broke explicit IPC connections.
  Native `PyServer.register_route()` now accepts CRM metadata, and
  `NativeServerBridge` passes the slot identity in both RuntimeSession and
  low-level registration paths.

## Phase 5: Strict Review, Remediation, And Verification

**Review gates after each phase:**

- Performance: remote call hot paths do not add string matching or relay resolve work beyond an O(1) method-index bounds check.
- Security: relay responses cannot steer clients to CRM-incompatible routes; invalid method indexes cannot enter SDK callbacks.
- Logic: direct IPC remains relay-independent; name-based relay fallback still happens only before any CRM method call is sent.
- Wire: malformed handshakes fail bounds checks instead of silently defaulting metadata.
- SDK boundary: Python still owns Python method discovery and invocation only; Rust owns language-neutral metadata validation.

**Final verification commands:**

- `cargo test --manifest-path core/Cargo.toml -p c2-wire`
- `cargo test --manifest-path core/Cargo.toml -p c2-server`
- `cargo test --manifest-path core/Cargo.toml -p c2-ipc`
- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay`
- `cargo test --manifest-path core/Cargo.toml -p c2-runtime`
- `cargo check --manifest-path sdk/python/native/Cargo.toml -q`
- `C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_wire.py sdk/python/tests/unit/test_security.py sdk/python/tests/unit/test_runtime_session.py sdk/python/tests/integration/test_registry.py -q --timeout=30 -rs`
- `cargo fmt --manifest-path core/Cargo.toml --all --check`
- `cargo fmt --manifest-path sdk/python/native/Cargo.toml --all --check`
- `git diff --check`
