# Python Native Binding Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the current PyO3-only `c2-ffi` crate out of `core/` and into the Python SDK as `sdk/python/native`, leaving `core/` language-neutral.

**Architecture:** `core/` remains the canonical Rust runtime workspace and contains no Python/PyO3 binding crate. The Python SDK owns its native extension crate under `sdk/python/native`, built by maturin as `c_two._native`. Future language SDKs get their own binding crates instead of sharing a Python-shaped FFI layer.

**Tech Stack:** Rust workspace, PyO3, maturin, uv, GitHub Actions, Python pytest.

---

## File Structure

Move:
- `core/bridge/c2-ffi/` -> `sdk/python/native/`

Modify:
- `core/Cargo.toml`: remove `bridge/c2-ffi` from workspace members.
- `sdk/python/native/Cargo.toml`: rename package/description and fix path dependencies from `../../foundation/...` to `../../../core/foundation/...`.
- `sdk/python/pyproject.toml`: set maturin `manifest-path = "native/Cargo.toml"`.
- `.github/workflows/ci.yml`: core tests no longer need `--exclude c2-ffi`.
- `.vscode/settings.json`: linked projects remain `core/Cargo.toml` and `cli/Cargo.toml`; do not add the Python native crate unless local editing requires it. rust-analyzer can discover it through maturin only when the SDK is opened separately.
- `.github/copilot-instructions.md`, `CONTRIBUTING.md`, `README.md`, `README.zh-CN.md`, `sdk/python/README.md`: update old path and ownership language.
- `sdk/python/tests/unit/test_cli_release_workflow.py`: update CI command assertions.
- Add or update a Python SDK packaging test to assert `manifest-path = "native/Cargo.toml"` and no `core/bridge/c2-ffi` reference remains in active build config.

Do not update historical archived docs under `docs/superpowers/plans/` or `docs/superpowers/specs/` unless they are explicitly presented as current instructions. They are implementation history and may contain old paths by design.

## Task 1: Add Regression Tests For Ownership And Paths

**Files:**
- Modify: `sdk/python/tests/unit/test_python_package_release_workflow.py`
- Modify: `sdk/python/tests/unit/test_cli_release_workflow.py`

- [x] **Step 1: Add Python SDK native manifest assertions**

Append this test to `sdk/python/tests/unit/test_python_package_release_workflow.py`:

```python
def test_python_pyproject_owns_native_binding_manifest():
    pyproject = (_repo_root() / "sdk" / "python" / "pyproject.toml").read_text(
        encoding="utf-8"
    )

    assert 'manifest-path = "native/Cargo.toml"' in pyproject
    assert "core/bridge/c2-ffi" not in pyproject
    assert 'module-name = "c_two._native"' in pyproject
```

- [x] **Step 2: Update CI command assertion**

In `sdk/python/tests/unit/test_cli_release_workflow.py`, replace:

```python
assert (
    "cargo test --manifest-path core/Cargo.toml --workspace --exclude c2-ffi"
    in ci_text
)
```

with:

```python
assert "cargo test --manifest-path core/Cargo.toml --workspace" in ci_text
assert "--exclude c2-ffi" not in ci_text
```

- [x] **Step 3: Run tests to verify RED**

Run:

```bash
uv run pytest \
  sdk/python/tests/unit/test_python_package_release_workflow.py::test_python_pyproject_owns_native_binding_manifest \
  sdk/python/tests/unit/test_cli_release_workflow.py::test_ci_runs_cli_rust_tests \
  -q --timeout=60
```

Expected: FAIL because `sdk/python/pyproject.toml` still points at `../../core/bridge/c2-ffi/Cargo.toml` and CI still excludes `c2-ffi`.

- [ ] **Step 4: Commit tests**

```bash
git add sdk/python/tests/unit/test_python_package_release_workflow.py sdk/python/tests/unit/test_cli_release_workflow.py
git commit -m "test: lock Python native binding ownership"
```

## Task 2: Move The PyO3 Crate Into The Python SDK

**Files:**
- Move: `core/bridge/c2-ffi/` -> `sdk/python/native/`
- Modify: `sdk/python/native/Cargo.toml`
- Modify: `core/Cargo.toml`
- Modify: `sdk/python/pyproject.toml`

- [ ] **Step 1: Move the crate**

Run:

```bash
mkdir -p sdk/python
git mv core/bridge/c2-ffi sdk/python/native
```

Expected: `sdk/python/native/Cargo.toml` exists and `core/bridge/c2-ffi/` no longer exists.

- [ ] **Step 2: Remove the Python binding crate from core workspace**

In `core/Cargo.toml`, remove this member:

```toml
"bridge/c2-ffi",
```

The members list should become:

```toml
members = [
    "foundation/c2-config",
    "foundation/c2-error",
    "foundation/c2-mem",
    "protocol/c2-wire",
    "transport/c2-ipc",
    "transport/c2-http",
    "transport/c2-server",
]
```

- [ ] **Step 3: Update Python native crate metadata and paths**

In `sdk/python/native/Cargo.toml`, replace the package metadata and dependency paths with:

```toml
[package]
name = "c2-python-native"
edition = "2024"
version = "0.1.0"
description = "PyO3 native extension for the C-Two Python SDK"

[lib]
name = "c2_ffi"
crate-type = ["cdylib", "lib"]

[features]
default = ["python"]
python = ["dep:pyo3"]

[dependencies]
c2-config = { path = "../../../core/foundation/c2-config" }
c2-error = { path = "../../../core/foundation/c2-error" }
c2-mem = { path = "../../../core/foundation/c2-mem" }
c2-wire = { path = "../../../core/protocol/c2-wire" }
c2-http = { path = "../../../core/transport/c2-http", features = ["relay"] }
c2-ipc = { path = "../../../core/transport/c2-ipc" }
c2-server = { path = "../../../core/transport/c2-server" }
parking_lot = "0.12"
pyo3 = { version = "0.28", features = ["extension-module"], optional = true }
tokio = { version = "1", features = ["full"] }
serde_json = "1"
```

Use direct `parking_lot = "0.12"` because this crate is no longer a member of the `core` workspace and cannot inherit `workspace = true`.

- [ ] **Step 4: Update the PyO3 module docs**

In `sdk/python/native/src/lib.rs`, replace the opening module comment with:

```rust
//! C-Two Python SDK native extension.
//!
//! This crate exposes the Rust runtime to Python as `c_two._native`.
//! It is intentionally owned by `sdk/python`, not `core`, because it uses
//! PyO3 and Python-specific buffer, exception, and module semantics.
```

- [ ] **Step 5: Update maturin manifest path**

In `sdk/python/pyproject.toml`, replace:

```toml
manifest-path = "../../core/bridge/c2-ffi/Cargo.toml"
```

with:

```toml
manifest-path = "native/Cargo.toml"
```

- [ ] **Step 6: Verify moved crate compiles as a Python native crate**

Run:

```bash
cargo check --manifest-path sdk/python/native/Cargo.toml --features python
```

Expected: PASS.

- [ ] **Step 7: Verify Rust core workspace no longer sees PyO3 binding crate**

Run:

```bash
cargo metadata --manifest-path core/Cargo.toml --no-deps --format-version 1 \
  | python -c 'import json,sys; names=[p["name"] for p in json.load(sys.stdin)["packages"]]; assert "c2-ffi" not in names and "c2-python-native" not in names; print(names)'
```

Expected: output lists only core runtime crates.

- [ ] **Step 8: Run tests from Task 1 to verify GREEN**

Run:

```bash
uv run pytest \
  sdk/python/tests/unit/test_python_package_release_workflow.py::test_python_pyproject_owns_native_binding_manifest \
  sdk/python/tests/unit/test_cli_release_workflow.py::test_ci_runs_cli_rust_tests \
  -q --timeout=60
```

Expected: PASS.

- [ ] **Step 9: Commit migration**

```bash
git add core/Cargo.toml sdk/python/pyproject.toml sdk/python/native
git add -u core/bridge/c2-ffi
git commit -m "refactor: move PyO3 native binding into Python SDK"
```

## Task 3: Update CI And Development Commands

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `.github/copilot-instructions.md`
- Modify: `CONTRIBUTING.md`
- Modify: `sdk/python/README.md`

- [ ] **Step 1: Update core CI command**

In `.github/workflows/ci.yml`, replace:

```yaml
run: cargo test --manifest-path core/Cargo.toml --workspace --exclude c2-ffi
```

with:

```yaml
run: cargo test --manifest-path core/Cargo.toml --workspace
```

- [ ] **Step 2: Ensure workflow policy tests still include native path tests**

In `.github/workflows/ci.yml`, keep these tests in the workflow-policy command:

```yaml
sdk/python/tests/unit/test_cli_release_workflow.py
sdk/python/tests/unit/test_python_package_release_workflow.py
```

No new test entry is required if Task 1 placed assertions in those existing files.

- [ ] **Step 3: Update Python SDK README development section**

In `sdk/python/README.md`, replace the current `Development` command block with:

```markdown
From the repository root, rebuild the Python native extension and install
Python development dependencies:

```bash
uv sync --reinstall-package c-two
```

Run Python SDK tests:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests -q --timeout=30
```

Run repository-level Rust checks separately:

```bash
cargo test --manifest-path core/Cargo.toml --workspace
cargo test --manifest-path cli/Cargo.toml
```
```

Keep the existing `c3_tool.py --build --link` CLI section as the source-checkout CLI workflow.

- [ ] **Step 4: Update contributor docs**

In `CONTRIBUTING.md`, replace language that says `c2-ffi` is excluded because it needs Python linkage with:

```markdown
# Rust core tests
cargo test --manifest-path core/Cargo.toml --workspace

# Python SDK native extension and tests
uv sync --reinstall-package c-two
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests -q --timeout=30
```

- [ ] **Step 5: Update Copilot instructions**

In `.github/copilot-instructions.md`, replace the old bridge table row:

```markdown
| bridge | `c2-ffi` | PyO3 bindings: `mem_ffi`, `wire_ffi`, `ipc_ffi`, `server_ffi`, `client_ffi`, `relay_ffi`, `http_ffi` |
```

with:

```markdown
| sdk/python/native | `c2-python-native` | PyO3 bindings for Python package module `c_two._native` |
```

Also replace any instruction that says core Rust tests must exclude `c2-ffi` with the plain core workspace command:

```bash
cargo test --manifest-path core/Cargo.toml --workspace
```

- [ ] **Step 6: Run workflow policy tests**

Run:

```bash
uv run --no-project --with pytest --with pytest-timeout --with packaging pytest \
  sdk/python/tests/unit/test_cli_release_workflow.py \
  sdk/python/tests/unit/test_python_package_release_workflow.py \
  sdk/python/tests/unit/test_check_version.py::TestCheckVersion \
  sdk/python/tests/unit/test_check_cli_release.py \
  sdk/python/tests/unit/test_c3_tool.py \
  -q --timeout=30
```

Expected: PASS.

- [ ] **Step 7: Commit CI and docs**

```bash
git add .github/workflows/ci.yml .github/copilot-instructions.md CONTRIBUTING.md sdk/python/README.md
git commit -m "docs: separate core and Python native development flows"
```

## Task 4: Update Active Path References And Editor Config

**Files:**
- Modify: `README.md`
- Modify: `README.zh-CN.md`
- Modify: `.vscode/settings.json`
- Modify: `sdk/python/tests/unit/test_python_package_release_workflow.py` if additional active path assertions are needed.

- [ ] **Step 1: Update root README commands**

In `README.md`, replace any source-checkout CLI build instruction that says:

```bash
cargo build --manifest-path cli/Cargo.toml
```

with:

```bash
python tools/dev/c3_tool.py --build --link
```

Do the same in `README.zh-CN.md`.

- [ ] **Step 2: Keep VS Code linked projects language-neutral**

Ensure `.vscode/settings.json` contains:

```json
{
  "rust-analyzer.linkedProjects": [
    "core/Cargo.toml",
    "cli/Cargo.toml"
  ]
}
```

Do not add `sdk/python/native/Cargo.toml` to the default repository-level setting unless rust-analyzer fails to inspect files under `sdk/python/native`. The default should avoid making the Python SDK native crate look like part of the core runtime.

- [ ] **Step 3: Search active files for old paths**

Run:

```bash
rg "core/bridge/c2-ffi|bridge/c2-ffi|--exclude c2-ffi|src/c_two/_native/Cargo.toml" \
  .github CONTRIBUTING.md README.md README.zh-CN.md sdk/python pyproject.toml .vscode
```

Expected: no output.

- [ ] **Step 4: Commit active path updates**

```bash
git add README.md README.zh-CN.md .vscode/settings.json sdk/python/tests/unit/test_python_package_release_workflow.py
git commit -m "chore: update active native binding paths"
```

## Task 5: Full Verification

**Files:** no intended source changes.

- [ ] **Step 1: Verify core Rust workspace**

Run:

```bash
cargo test --manifest-path core/Cargo.toml --workspace
```

Expected: PASS. This proves `core/` is language-neutral and no longer needs `--exclude c2-ffi`.

- [ ] **Step 2: Verify Python native extension build**

Run:

```bash
uv sync --reinstall-package c-two
```

Expected: PASS and rebuilds `c_two._native` from `sdk/python/native/Cargo.toml`.

- [ ] **Step 3: Verify Python tests**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests -q --timeout=30
```

Expected: PASS.

- [ ] **Step 4: Verify CLI tests**

Run:

```bash
cargo test --manifest-path cli/Cargo.toml
```

Expected: PASS.

- [ ] **Step 5: Verify path cleanup**

Run:

```bash
rg "core/bridge/c2-ffi|bridge/c2-ffi|--exclude c2-ffi|src/c_two/_native/Cargo.toml" \
  .github CONTRIBUTING.md README.md README.zh-CN.md sdk/python pyproject.toml .vscode
```

Expected: no output.

- [ ] **Step 6: Verify formatting and diff hygiene**

Run:

```bash
cargo fmt --manifest-path core/Cargo.toml -- --check
cargo fmt --manifest-path cli/Cargo.toml -- --check
git diff --check
```

Expected: all PASS.

- [ ] **Step 7: Final commit if needed**

If any verification-only fixes were required:

```bash
git add <changed-files>
git commit -m "fix: complete Python native binding migration"
```

If no files changed, do not create an empty commit.

## Self-Review

- Spec coverage: The plan moves the PyO3 binding crate into `sdk/python/native`, removes it from the core workspace, updates maturin, CI, docs, editor config, and active path references.
- Scope: The plan intentionally does not create a generic C ABI. That should be a separate design once another SDK requires it.
- Risk controls: The plan uses TDD path assertions first, preserves `c_two._native` as the Python module name, and verifies core, Python, and CLI separately.
- Dirty worktree note: Before executing, inspect `git status --short --branch`. The current main worktree may contain unrelated `.vscode`, CLI banner, and architecture image changes. Do not overwrite or revert them.
