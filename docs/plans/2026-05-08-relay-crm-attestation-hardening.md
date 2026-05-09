# Relay CRM Attestation Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Harden relay CRM contract authority after the Phase 9 CRM metadata migration.

**Architecture:** Python continues to provide only expected CRM identity from decorated CRM classes. Rust relay/runtime code owns IPC handshake attestation, route metadata publication, relay target selection, contract-mismatch fallback policy, and route-cache refresh behavior.

**Tech Stack:** Rust `c2-http`, `c2-ipc`, `c2-runtime`, PyO3 native bindings, Python registry integration tests, `cargo test`, `uv run pytest`.

---

## Scope

This plan implements the strict-review findings from session
`019e065b-8b4b-7e23-abb2-a7d1ef669143`:

- HTTP `/_register` must derive or verify route CRM metadata from the IPC
  handshake route entry.
- CLI `c3 relay --upstream NAME=SERVER_ID@ADDRESS` must publish the attested
  route CRM metadata instead of empty strings.
- Relay-local IPC contract mismatch must be a hard contract error and must not
  fallback to HTTP.
- Relay route-name cache plus expected-CRM filtering must force one fresh
  resolve before returning a false no-compatible-route result.
- Explicit `address="http://..."` connect must be reviewed and either moved to
  the relay-aware contract-validation path or kept as a documented follow-up
  with a clear reason.

Out of scope:

- Transferable ABI hashes and full method-signature compatibility.
- Python-owned relay route selection or HTTP retry policy.
- Compatibility aliases for stale or empty production CRM route metadata.

## Phase 1: RED Tests For Relay Registration Attestation

**Files:**

- Modify: `core/transport/c2-http/src/relay/router.rs`
- Modify: `core/transport/c2-http/src/relay/server.rs`
- Modify: `core/transport/c2-http/src/relay/test_support.rs`

- [x] Add an HTTP `/_register` test proving omitted or matching `crm_ns` /
  `crm_ver` publishes the IPC handshake route contract.
- [x] Add an HTTP `/_register` test proving mismatched claimed CRM metadata is
  rejected before commit.
- [x] Add a command-loop `RegisterUpstream` test proving CLI upstream
  registration derives CRM metadata from the handshake route entry.
- [x] Verify the tests fail for the expected reasons before changing
  production code.

## Phase 2: Implement Relay Registration Attestation

**Files:**

- Modify: `core/transport/c2-http/src/relay/router.rs`
- Modify: `core/transport/c2-http/src/relay/server.rs`
- Modify: `core/transport/c2-http/src/relay/test_support.rs`
- Modify as needed: relay docs/benchmark helper references to upstream
  metadata.

- [x] Add a small Rust helper that reads the candidate IPC client's route
  contract for the requested route and compares optional claimed metadata.
- [x] Use the helper in HTTP `/_register` after duplicate/live-owner
  preparation and before final commit.
- [x] Use the helper in command-loop registration after candidate IPC
  attestation and before final commit.
- [x] Keep `skip_ipc_validation` restricted to tests; do not add production
  fallbacks for missing or empty route CRM metadata.
- [x] Run targeted cargo tests and perform a strict review of relay authority,
  stale-owner replacement, and TOCTOU behavior before moving on.

## Phase 3: RED Tests For Fallback And Cache Behavior

**Files:**

- Modify: `sdk/python/native/src/runtime_session_ffi.rs`
- Modify: `core/runtime/c2-runtime/src/session.rs`
- Modify: `core/transport/c2-http/src/client/relay_aware.rs`
- Modify: Python relay integration/unit tests as needed.

- [x] Add a native/Python test proving relay-local IPC contract mismatch does
  not fallback to HTTP.
- [x] Add a Rust relay-aware client test proving cached raw routes that are
  filtered out by expected CRM trigger one forced refresh before returning no
  compatible route.
- [x] Verify both tests fail for the expected reasons before production edits.

## Phase 4: Implement Fallback And Cache Hardening

**Files:**

- Modify: `sdk/python/native/src/runtime_session_ffi.rs`
- Modify: `core/runtime/c2-runtime/src/session.rs`
- Modify: `core/transport/c2-http/src/client/relay_aware.rs`
- Modify: `core/transport/c2-http/src/client/control.rs` only if the cache API
  needs a targeted invalidation hook.

- [x] Add a contract-mismatch error class/variant in the relay-local IPC
  acquire path and map it to a hard runtime error.
- [x] Restrict HTTP fallback to genuine IPC unavailable/route-missing cases
  before any CRM method call is sent.
- [x] Invalidate/refresh cached raw routes once when expected-CRM filtering
  empties a non-empty cached result.
- [x] Run targeted cargo/Python tests and perform strict review for security,
  performance, and fallback regressions.

## Phase 5: Explicit HTTP Validation Decision And Implementation

**Files:**

- Inspect: `sdk/python/src/c_two/transport/registry.py`
- Inspect/modify: `sdk/python/native/src/runtime_session_ffi.rs`
- Inspect/modify: `core/runtime/c2-runtime/src/session.rs`
- Inspect/modify: `core/transport/c2-http/src/client/relay_aware.rs`
- Modify tests/docs according to the decision.

- [x] Review direct `address="http://..."` semantics against the current relay
  transport model and existing tests.
- [x] If it can safely use the relay-aware path, add RED tests proving explicit
  HTTP connect rejects CRM mismatch before method call and preserves existing
  timeout/stale-route behavior.
- [x] Implement the smallest Rust/native/Python change to share the
  relay-aware contract-validation path.
- [x] Do not defer this slice: explicit HTTP connect now constructs a
  relay-aware client and resolves/probes with expected CRM before returning a
  proxy.
- [x] Complete the interrupted strict review for explicit HTTP error
  classification, forced-HTTP behavior, close lifecycle, and stale docs/tests.

## Phase 6: Final Strict Review And Verification

**Review gates:**

- Relay registration metadata is handshake-attested for HTTP and CLI upstream
  paths.
- Contract mismatch cannot be hidden by HTTP fallback.
- Cache refresh adds no normal-path extra work and eliminates false CRM-filtered
  404s.
- Direct IPC remains relay-independent.
- Python does not gain route-selection authority.

**Verification commands:**

- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay`
- `cargo test --manifest-path core/Cargo.toml -p c2-runtime`
- `cargo check --manifest-path sdk/python/native/Cargo.toml -q`
- `C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_http_relay.py sdk/python/tests/integration/test_registry.py sdk/python/tests/unit/test_ipc_config.py -q --timeout=30 -rs`
- `cargo fmt --manifest-path core/Cargo.toml --all --check`
- `cargo fmt --manifest-path sdk/python/native/Cargo.toml --all --check`
- `git diff --check`

**Final verification result:** complete on 2026-05-08.

- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay`: 170 passed.
- `cargo test --manifest-path core/Cargo.toml -p c2-runtime`: 13 passed.
- `cargo check --manifest-path sdk/python/native/Cargo.toml -q`: passed.
- `C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_http_relay.py sdk/python/tests/integration/test_registry.py sdk/python/tests/unit/test_ipc_config.py -q --timeout=30 -rs`: 89 passed.
- `cargo fmt --manifest-path core/Cargo.toml --all --check`: passed.
- `cargo fmt --manifest-path sdk/python/native/Cargo.toml --all --check`: passed.
- `git diff --check`: passed.
- Additional confidence pass: `cargo test --manifest-path core/Cargo.toml --workspace` passed across all core crates; `C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/ -q --timeout=30 -rs` reported 745 passed, 2 skipped.
