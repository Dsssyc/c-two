# Cross-Language Residue Review

**Date:** 2026-04-27  
**Status:** Discussion backlog  
**Scope:** Remaining repository issues that make C-Two harder to evolve beyond the current Python SDK.

This review captures the current cross-language residue found after the recent
repository, CLI, and native-binding refactors. It is intentionally not a fix
plan yet. Each item should be discussed and resolved into a concrete design or
implementation task before code changes.

## Summary

The main runtime boundaries are mostly in the right direction:

- `core/` is a standalone Rust workspace.
- `sdk/python/native/` depends on `core/*`; `core/` does not depend on the
  Python SDK or PyO3.
- `cli/` is a root Rust crate for the native `c3` binary and has no Python SDK
  dependency.
- CI already has separate Rust core and CLI jobs.

Remaining problems are concentrated in configuration ownership, inconsistent
entry-point defaults, workflow-policy test placement, and Python-first wording
in core/docs.

## Findings

### 1. IPC configuration still has two authorities

**Severity:** High  
**Type:** Architecture / cross-language maintainability

`c2-config` is described as the shared configuration authority:

- `core/foundation/c2-config/src/lib.rs`

But `core/foundation/c2-config/src/ipc.rs` still says the Rust `Default`
implementations exist only for Rust-side testing and that Python is
authoritative at runtime.

At the same time, `sdk/python/src/c_two/config/ipc.py` duplicates the IPC
defaults, validation, env merging, clamping, and derived `max_pool_memory`
behavior. The PyO3 constructors in `sdk/python/native/src/server_ffi.rs` and
`sdk/python/native/src/client_ffi.rs` receive scalar config values and rebuild
the Rust config structs from Python-owned values.

**Why this hurts cross-language support:**

Future SDKs would have to either copy Python's config builder behavior or use
Rust defaults that the code comments explicitly say are not authoritative.
That creates default drift and makes `c2-config` a shared struct crate rather
than a true configuration authority.

**Discussion questions:**

- Should `c2-config` become the canonical default and validation authority for
  all SDKs?
- Should SDKs pass only explicit overrides into Rust and let Rust merge them
  with canonical defaults?
- Should env parsing live in Rust for shared runtime variables, or should each
  SDK parse env vars but use Rust-generated defaults?

### 2. Relay ownership and idle-timeout defaults are now unified

**Severity:** Medium  
**Type:** Runtime behavior consistency

Resolved state:

- Python SDK no longer exposes embedded relay lifecycle APIs.
- Relay server configuration, including `C2_RELAY_IDLE_TIMEOUT`, belongs to the
  standalone Rust relay runtime started with `c3 relay`, Docker, or
  orchestration.
- The canonical idle timeout default is 60 seconds.
- Setting the idle timeout to `0` explicitly disables time-based eviction.
- Idle eviction is in-flight-safe.

**Why this hurts cross-language support:**

The original finding identified a cross-language ownership risk: SDK embedding
and CLI startup could have drifted into different relay lifecycle behavior.
The resolved model makes the relay a standalone Rust runtime with one default
matrix.

**Discussion questions:**

- Are docs, examples, and tests consistently using the standalone `c3 relay`
  workflow?
- Do language SDKs only own client-side relay address selection via
  `C2_RELAY_ADDRESS` or `cc.set_relay()` equivalents?

### 3. Workflow-policy tests still live under the Python SDK test tree

**Severity:** Medium  
**Type:** Repository ownership / CI governance

The CI workflow-policy job runs tests from `sdk/python/tests/unit/`, including:

- `test_cli_release_workflow.py`
- `test_check_cli_release.py`
- `test_c3_tool.py`

These tests validate root repository behavior, CLI release policy, and
developer CLI tooling rather than Python SDK behavior.

**Why this hurts cross-language support:**

This is not runtime coupling, but it keeps repository and CLI governance owned
by the Python SDK test layout. As more SDKs are added, root policy tests should
not be discovered or reasoned about as Python package tests.

**Discussion questions:**

- Should root workflow/tooling tests move to a repository-level test directory,
  such as `tests/repo/` or `tools/tests/`?
- Should CLI-specific helper tests move under `cli/tests/` if they can stay in
  Rust, or under `tools/tests/` if they remain Python scripts?
- Should `uv run --no-project --with pytest ...` remain the mechanism for
  policy tests, or should these be checked by a smaller script?

### 4. Core Rust comments remain Python-first

**Severity:** Low  
**Type:** Documentation / contributor guidance

Several core Rust modules describe pure runtime components as Python-specific:

- `core/transport/c2-ipc/src/lib.rs`: connects to a Python `ServerV2`.
- `core/transport/c2-ipc/src/client.rs`: connects to Python `ServerV2`; socket
  path comment says it matches Python Server.
- `core/transport/c2-server/src/server.rs`: says it replaces the Python asyncio
  server and mentions `c2-ffi`.
- `core/transport/c2-server/src/dispatcher.rs`: pure Rust interfaces are
  documented in terms of `PyCrmCallback`, GIL, and Python conversion.
- `core/protocol/c2-wire/src/tests.rs` and related comments refer to Python
  fixtures rather than a language-neutral wire compatibility corpus.

**Why this hurts cross-language support:**

Most of this is not an actual dependency problem, but it sets the wrong mental
model for contributors. Future SDK authors should see the core as a
language-neutral runtime with Python as one binding, not as Rust internals for
Python.

**Discussion questions:**

- Which comments should be rewritten immediately as language-neutral runtime
  docs?
- Should Python-specific implementation notes be moved into
  `sdk/python/native/`?
- Should protocol fixture comments describe a canonical wire fixture corpus
  instead of saying "generated by Python"?

### 5. Public docs still present the product as Python-first

**Severity:** Low  
**Type:** Product positioning / docs

The root README still frames C-Two as a Python framework and roadmap item:

- tagline: "A resource-oriented RPC framework for Python"
- roadmap previously named Python as the unified configuration owner
- bottom tagline: "Built for scientific Python. Powered by Rust."

This may be accurate for the currently implemented SDK, but it conflicts with
the repository's cross-language direction.

**Why this hurts cross-language support:**

Docs shape contributor assumptions. If the repository is preparing to support
TypeScript, Rust-native, Go, or browser SDKs, the root README should separate
the product identity from the current Python SDK status.

**Discussion questions:**

- Should the root README describe C-Two as a language-neutral resource RPC
  runtime with Python as the first SDK?
- Should Python-specific examples remain dominant until another SDK exists?
- Should roadmap wording continue to track the Rust resolver as the
  configuration owner once migration work lands?

## Verification Notes

Commands run during the review:

```bash
cargo test --manifest-path cli/Cargo.toml
```

Result: passed, 14 CLI-related tests.

```bash
cargo test --manifest-path core/Cargo.toml --workspace
```

Result: did not complete in the current sandbox. Earlier workspace crates
passed, then `c2-mem` SHM tests failed because `shm_open` returned
`Operation not permitted (os error 1)`. This is recorded as an environment
limitation, not as a cross-language architecture finding.

## Proposed Discussion Order

1. Decide configuration authority (`c2-config` vs SDK-owned defaults).
2. Move root/CLI workflow-policy tests out of the Python SDK tree.
3. Rewrite Python-first core comments into language-neutral docs.
4. Reposition root README language once the desired product framing is clear.

Relay idle-timeout defaults were resolved separately: the canonical default is
60 seconds, and `0` explicitly disables time-based eviction.

## Issue Drafts

GitHub issue creation was attempted for these items, but the connected GitHub
integration returned `403 Resource not accessible by integration`. The drafts
below are ready to paste into GitHub once issue write access is available.

### Issue 1: Decide canonical IPC configuration ownership for cross-language SDKs

**Labels:** `architecture`, `cross-language`

#### Problem

IPC configuration currently has two competing authorities.

`core/foundation/c2-config/src/lib.rs` describes `c2-config` as the shared
configuration source of truth, but `core/foundation/c2-config/src/ipc.rs` says
Rust defaults exist only for Rust-side testing and that Python is authoritative
at runtime.

At the same time, `sdk/python/src/c_two/config/ipc.py` duplicates IPC defaults,
validation, env merging, clamping, and derived `max_pool_memory` behavior. PyO3
constructors in `sdk/python/native/src/server_ffi.rs` and
`sdk/python/native/src/client_ffi.rs` receive scalar config values and rebuild
Rust config structs from Python-owned values.

#### Why this matters

Future SDKs would either need to copy Python's config builder behavior or use
Rust defaults that the Rust comments say are not authoritative. That creates
default drift and prevents `c2-config` from serving as a true cross-language
configuration authority.

#### Discussion questions

- Should `c2-config` become the canonical default and validation authority for
  all SDKs?
- Should SDKs pass only explicit overrides into Rust and let Rust merge them
  with canonical defaults?
- Should env parsing live in Rust for shared runtime variables, or should each
  SDK parse env vars but use Rust-generated defaults?

#### Candidate acceptance criteria

- One documented authority for IPC defaults and validation.
- Python SDK no longer has to duplicate the full default matrix manually.
- Future SDKs have a clear path to obtain identical runtime defaults.

### Issue 2: Align standalone relay idle-timeout defaults and ownership

**Labels:** `runtime`, `cross-language`

#### Problem

Resolved plan and state:

- Python SDK no longer exposes embedded relay lifecycle APIs.
- Relay server configuration, including `C2_RELAY_IDLE_TIMEOUT`, belongs to the
  standalone Rust relay runtime.
- The canonical idle timeout default is 60 seconds.
- Setting the idle timeout to `0` explicitly disables time-based eviction.
- Idle eviction is in-flight-safe.

#### Why this matters

Language SDKs should not own relay server lifecycle behavior. They keep HTTP
client bindings and point at an existing relay through relay address
configuration.

#### Discussion questions

- Are all public docs using `python tools/dev/c3_tool.py --build --link` for
  source-checkout relay-dependent tests and examples?
- Are relay server defaults documented only for the standalone Rust runtime?

#### Candidate acceptance criteria

- Relay default is explicit and consistent across `c3 relay`, `.env.example`,
  CLI docs, and SDK docs.
- SDK docs state that language SDKs do not embed or start relay servers.

### Issue 3: Move root and CLI workflow-policy tests out of the Python SDK test tree

**Labels:** `ci`, `cross-language`

#### Problem

The CI workflow-policy job runs repository and CLI policy tests from
`sdk/python/tests/unit/`, including:

- `test_cli_release_workflow.py`
- `test_check_cli_release.py`
- `test_c3_tool.py`

These tests validate root repository behavior, CLI release policy, and
developer CLI tooling rather than Python SDK behavior.

#### Why this matters

This is not runtime coupling, but it keeps repository and CLI governance owned
by the Python SDK test layout. As more SDKs are added, root policy tests should
not be discovered or reasoned about as Python package tests.

#### Discussion questions

- Should root workflow/tooling tests move to a repository-level test directory
  such as `tests/repo/` or `tools/tests/`?
- Should CLI-specific helper tests move under `cli/tests/` if they can stay in
  Rust, or under `tools/tests/` if they remain Python scripts?
- Should `uv run --no-project --with pytest ...` remain the mechanism for
  policy tests, or should these be checked by a smaller script?

#### Candidate acceptance criteria

- Python SDK tests contain Python SDK behavior tests only.
- Root workflow/release policy tests live in a repository-level or tool-level
  location.
- CI still runs those policy checks without requiring a full Python SDK
  environment.

### Issue 4: Rewrite Python-first comments in core Rust crates as language-neutral runtime docs

**Labels:** `documentation`, `cross-language`

#### Problem

Several core Rust modules still describe pure runtime components as
Python-specific:

- `core/transport/c2-ipc/src/lib.rs`: connects to a Python `ServerV2`.
- `core/transport/c2-ipc/src/client.rs`: connects to Python `ServerV2`; socket
  path comment says it matches Python Server.
- `core/transport/c2-server/src/server.rs`: says it replaces the Python asyncio
  server and mentions `c2-ffi`.
- `core/transport/c2-server/src/dispatcher.rs`: pure Rust interfaces are
  documented in terms of `PyCrmCallback`, GIL, and Python conversion.
- `core/protocol/c2-wire/src/tests.rs` and related comments refer to Python
  fixtures rather than a language-neutral wire compatibility corpus.

#### Why this matters

Most of this is not an actual dependency problem, but it sets the wrong mental
model for contributors. Future SDK authors should see `core/` as a
language-neutral runtime with Python as one binding, not as Rust internals for
Python.

#### Discussion questions

- Which comments should be rewritten immediately as language-neutral runtime
  docs?
- Should Python-specific implementation notes be moved into
  `sdk/python/native/`?
- Should protocol fixture comments describe a canonical wire fixture corpus
  instead of saying they are generated by Python?

#### Candidate acceptance criteria

- Core crate module docs describe runtime behavior in language-neutral terms.
- Python-specific binding notes live in Python binding code or tests.
- Compatibility fixtures are documented as canonical cross-language wire
  fixtures.

### Issue 5: Reposition root documentation for cross-language C-Two product identity

**Labels:** `documentation`, `cross-language`

#### Problem

The root README still frames C-Two as Python-first:

- tagline: "A resource-oriented RPC framework for Python"
- roadmap item previously named Python as the unified configuration owner
- footer: "Built for scientific Python. Powered by Rust."

This may be accurate for the currently implemented SDK, but it conflicts with
the repository's cross-language direction.

#### Why this matters

Docs shape contributor assumptions. If the repository is preparing to support
TypeScript, Rust-native, Go, or browser SDKs, the root README should separate
product identity from current Python SDK status.

#### Discussion questions

- Should the root README describe C-Two as a language-neutral resource RPC
  runtime with Python as the first SDK?
- Should Python-specific examples remain dominant until another SDK exists?
- Should roadmap wording continue to track the Rust resolver as the
  configuration owner once migration work lands?

#### Candidate acceptance criteria

- Root README distinguishes product/runtime identity from the current Python
  SDK.
- Python-specific claims remain accurate but do not imply Python owns the
  product architecture.
- Roadmap wording matches the intended cross-language direction.
