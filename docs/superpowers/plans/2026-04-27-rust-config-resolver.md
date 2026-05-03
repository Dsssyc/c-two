# Rust Config Resolver Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move runtime config resolution into Rust so SDKs and CLI share env-file parsing, env parsing, defaults, derived values, and validation.

**Architecture:** Add a resolver module to `c2-config`, expose resolved config helpers through PyO3, and turn Python config dataclasses into compatibility views over Rust-resolved values. CLI relay flags become code overrides into the shared resolver. Public env/config support removes `C2_IPC_MAX_POOL_MEMORY` and derives `max_pool_memory`.

**Tech Stack:** Rust 2024, `c2-config`, `dotenvy`, clap, PyO3, Python dataclasses, pytest, cargo test.

---

### Task 1: Rust Resolver Core

**Files:**
- Modify: `core/foundation/c2-config/Cargo.toml`
- Modify: `core/foundation/c2-config/src/lib.rs`
- Modify: `core/foundation/c2-config/src/ipc.rs`
- Create: `core/foundation/c2-config/src/resolver.rs`

- [ ] Add failing resolver tests for precedence, removed `C2_IPC_MAX_POOL_MEMORY`, derived `max_pool_memory`, unified reassembly default, and parse errors.
- [ ] Implement `ConfigResolver`, override structs, env-file loading, env parsing, merge, derived fields, and validation.
- [ ] Update `ServerIpcConfig::default()` reassembly default to 64 MiB.
- [ ] Run `cargo test --manifest-path core/Cargo.toml -p c2-config`.

### Task 2: CLI Resolver Integration

**Files:**
- Modify: `cli/Cargo.toml`
- Modify: `cli/src/main.rs`
- Modify: `cli/src/relay.rs`
- Modify: `cli/tests/relay_args.rs`

- [ ] Add failing CLI tests proving `.env`, process env, and CLI flag precedence still work through the resolver.
- [ ] Remove CLI-local `.env` loading and clap env bindings for relay config.
- [ ] Pass relay flag overrides into `ConfigResolver`.
- [ ] Run `cargo test --manifest-path cli/Cargo.toml`.

### Task 3: Python Native Resolver Binding

**Files:**
- Modify: `sdk/python/native/src/lib.rs`
- Create: `sdk/python/native/src/config_ffi.rs`
- Modify: `sdk/python/native/src/client_ffi.rs`
- Modify: `sdk/python/native/src/server_ffi.rs`

- [ ] Add native resolver functions returning Python dictionaries for server/client/global config.
- [ ] Derive `max_pool_memory` in Rust and remove it from PyO3 constructor signatures where practical.
- [ ] Keep compatibility where needed for existing Python call paths.
- [ ] Run `cargo test --manifest-path sdk/python/native/Cargo.toml`.

### Task 4: Python SDK Config Migration

**Files:**
- Modify: `sdk/python/pyproject.toml`
- Modify: `sdk/python/src/c_two/config/settings.py`
- Modify: `sdk/python/src/c_two/config/ipc.py`
- Modify: `sdk/python/src/c_two/config/__init__.py`
- Modify: `sdk/python/src/c_two/transport/registry.py`
- Modify: `sdk/python/src/c_two/transport/server/native.py`
- Modify: `sdk/python/tests/unit/test_ipc_config.py`
- Modify: `sdk/python/tests/unit/test_python_examples_syntax.py`

- [ ] Add failing Python tests for Rust-resolved env parsing, kwargs precedence, removed `max_pool_memory` override, 64 MiB server/client reassembly defaults, and removal of `C2_TEST_PYTHON310`.
- [ ] Replace pydantic settings with a lightweight compatibility facade for code overrides.
- [ ] Make `build_server_config()` and `build_client_config()` call native Rust resolver.
- [ ] Remove `pydantic-settings` dependency.
- [ ] Run focused Python tests.

### Task 5: Docs And Verification

**Files:**
- Modify: `.env.example`
- Modify: `cli/README.md`
- Modify: `sdk/python/README.md`
- Modify: `docs/superpowers/specs/2026-04-27-rust-config-resolver-design.md` if implementation decisions change.

- [ ] Update environment variable docs and remove stale variables.
- [ ] Run targeted Rust, CLI, and Python verification.
- [ ] Record any blocked verification with exact error output.
