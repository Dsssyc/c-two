# Python Examples Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split Python examples into clear local, direct IPC, and relay-only entry points that work as teaching material and manual smoke tests.

**Architecture:** Keep the shared Grid contract and resource implementation. Replace the current dual-mode `client.py` and `crm_process.py` with explicit `ipc_*` and `relay_*` scripts, and keep relay URL resolution inside an example-local helper backed by the Rust resolver.

**Tech Stack:** Python example scripts, `argparse`, C-Two Python SDK, Rust-backed Python config facade, pytest integration tests.

---

## File Structure

- Create `examples/python/ipc_resource.py`: direct IPC Grid resource process; no relay config.
- Create `examples/python/ipc_client.py`: direct IPC Grid client; requires explicit `ipc://...` address.
- Create `examples/python/relay_resource.py`: relay-only Grid resource process; uses `--relay-url` or Rust-resolved env config.
- Modify `examples/python/relay_client.py`: relay-only Grid client docs and argument behavior.
- Modify `examples/python/relay_config.py`: example-local relay URL helper used by relay examples.
- Delete `examples/python/client.py`: remove dual-mode client entry point.
- Delete `examples/python/crm_process.py`: remove dual-mode resource entry point.
- Modify `examples/python/relay_mesh/README.md`: mark mesh as advanced follow-up.
- Modify `examples/README.md`: document local, direct IPC, and single-relay smoke paths.
- Modify `sdk/python/tests/unit/test_python_examples_syntax.py`: syntax and parser/config boundary tests.
- Modify `sdk/python/tests/integration/test_python_examples.py`: end-to-end IPC and relay smoke tests with new script names.

## Task 1: Unit Tests for Example Boundaries

**Files:**
- Modify: `sdk/python/tests/unit/test_python_examples_syntax.py`
- Test target: new example parse/config semantics

- [ ] **Step 1: Add failing unit tests**

Add helpers that import example modules with controlled argv/env, then assert:

```python
def test_ipc_client_requires_explicit_address():
    result = _run_example_parse_args("ipc_client.py", [])
    assert result.returncode == 2
    assert "address" in result.stderr


def test_ipc_client_ignores_relay_env_file(tmp_path):
    env_file = tmp_path / ".env"
    env_file.write_text("C2_RELAY_ANCHOR_ADDRESS=http://127.0.0.1:9999\n", encoding="utf-8")
    result = _run_example_parse_args(
        "ipc_client.py",
        ["ipc://manual-address"],
        env={"C2_ENV_FILE": str(env_file), "C2_RELAY_ANCHOR_ADDRESS": "http://127.0.0.1:8888"},
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "address=ipc://manual-address"


def test_relay_client_explicit_url_overrides_env_file(tmp_path):
    env_file = tmp_path / ".env"
    env_file.write_text("C2_RELAY_ANCHOR_ADDRESS=http://127.0.0.1:9137\n", encoding="utf-8")
    result = _run_example_parse_args(
        "relay_client.py",
        ["--relay-url", "http://127.0.0.1:9140"],
        env={"C2_ENV_FILE": str(env_file)},
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "relay_url=http://127.0.0.1:9140"


def test_relay_client_defaults_to_local_relay_when_unconfigured():
    result = _run_example_parse_args("relay_client.py", [], env={"C2_ENV_FILE": "", "C2_RELAY_ANCHOR_ADDRESS": ""})
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "relay_url=http://127.0.0.1:8300"
```

- [ ] **Step 2: Verify tests fail**

Run:

```bash
env C2_ENV_FILE= uv run pytest sdk/python/tests/unit/test_python_examples_syntax.py -q
```

Expected: failures because `ipc_client.py` and `relay_resource.py` do not exist yet and old dual-mode files still exist.

- [ ] **Step 3: Keep only behavior-level assertions**

The tests should inspect parsed arguments and compile/import behavior, not internal implementation details. Do not assert that a specific helper function exists except for the public `parse_args()` entry point in executable scripts.

## Task 2: Integration Tests for New Smoke Paths

**Files:**
- Modify: `sdk/python/tests/integration/test_python_examples.py`
- Test target: runnable direct IPC and relay workflows

- [ ] **Step 1: Replace old workflow tests with new failing tests**

Use these workflows:

```python
def test_ipc_client_workflow_uses_explicit_ipc_address():
    # Start examples/python/ipc_resource.py.
    # Wait for "Grid CRM registered at".
    # Extract the printed ipc:// address.
    # Run examples/python/ipc_client.py <address>.
    # Assert return code 0 and "IPC client done." in stdout.


def test_relay_client_workflow_uses_explicit_relay_url(start_c3_relay):
    # Start c3 relay fixture.
    # Start examples/python/relay_resource.py --relay-url <fixture-url>.
    # Wait for "Grid CRM registered".
    # Run examples/python/relay_client.py --relay-url <fixture-url>.
    # Assert return code 0 and "[Client] Done." in stdout.


def test_ipc_client_requires_address_without_relay():
    # Run examples/python/ipc_client.py without args.
    # Assert argparse exit code 2 and address-related stderr.
```

- [ ] **Step 2: Verify tests fail**

Run:

```bash
env C2_ENV_FILE= uv run pytest sdk/python/tests/integration/test_python_examples.py -q
```

Expected: failures because the new scripts are not implemented yet.

## Task 3: Implement Split Example Scripts

**Files:**
- Create: `examples/python/ipc_resource.py`
- Create: `examples/python/ipc_client.py`
- Create: `examples/python/relay_resource.py`
- Modify: `examples/python/relay_client.py`
- Modify: `examples/python/relay_config.py`
- Delete: `examples/python/client.py`
- Delete: `examples/python/crm_process.py`

- [ ] **Step 1: Add direct IPC resource**

Implement `ipc_resource.py` as a small Grid resource process:

```python
"""Direct IPC Grid resource process."""
from __future__ import annotations

import logging
import sys
from pathlib import Path

EXAMPLES_ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(EXAMPLES_ROOT))

import c_two as cc
from grid.grid_contract import Grid
from grid.nested_grid import NestedGrid

logging.basicConfig(level=logging.INFO)


def _grid() -> NestedGrid:
    return NestedGrid(
        2326,
        [808357.5, 824117.5, 838949.5, 843957.5],
        [64.0, 64.0],
        [[478, 310], [2, 2], [2, 2], [2, 2], [2, 2], [2, 2], [1, 1]],
    )


def main() -> None:
    cc.register(Grid, _grid(), name="examples/grid")
    print(f"Grid CRM registered at {cc.server_address()}", flush=True)
    cc.serve()


if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Add direct IPC client**

Implement `ipc_client.py` with required positional address and no relay config reads:

```python
def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Connect to the Grid CRM through direct IPC.")
    parser.add_argument("address", help="Server IPC address printed by examples/python/ipc_resource.py.")
    return parser.parse_args(argv)
```

The main flow should call:

```python
grid = cc.connect(Grid, name="examples/grid", address=args.address)
```

and print `IPC client done.` before exiting.

- [ ] **Step 3: Add relay resource**

Implement `relay_resource.py` with relay-only behavior:

```python
def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Start the Grid CRM and register it with an HTTP relay.")
    parser.add_argument("--relay-url", default=resolved_relay_url(DEFAULT_RELAY_URL))
    return parser.parse_args(argv)
```

The main flow should call `cc.set_relay_anchor(args.relay_url)` before `cc.register(...)`.

- [ ] **Step 4: Update relay client**

Keep `relay_client.py` relay-only. Its `parse_args(argv=None)` should use:

```python
parser.add_argument("--relay-url", default=resolved_relay_url(DEFAULT_RELAY_URL))
```

and main should always set the relay and connect by name:

```python
cc.set_relay_anchor(args.relay_url)
grid = cc.connect(Grid, name="examples/grid")
```

- [ ] **Step 5: Remove dual-mode entry points**

Remove `examples/python/client.py` and `examples/python/crm_process.py`. Do not keep compatibility wrappers, because the old names express the ambiguous dual-mode behavior we are removing.

- [ ] **Step 6: Run unit and integration tests**

Run:

```bash
env C2_ENV_FILE= uv run pytest sdk/python/tests/unit/test_python_examples_syntax.py sdk/python/tests/integration/test_python_examples.py -q
```

Expected: all example unit/integration tests pass or relay tests skip only if `c3` is not built.

## Task 4: Documentation Update

**Files:**
- Modify: `examples/README.md`
- Modify: `examples/python/relay_mesh/README.md`
- Optionally modify: `sdk/python/README.md` if it still points to obsolete command names.

- [ ] **Step 1: Update examples README commands**

Document:

```bash
uv sync --group examples
python tools/dev/c3_tool.py --build --link
uv run python examples/python/local.py
uv run python examples/python/ipc_resource.py
uv run python examples/python/ipc_client.py ipc://...
c3 relay --bind 127.0.0.1:8300
uv run python examples/python/relay_resource.py --relay-url http://127.0.0.1:8300
uv run python examples/python/relay_client.py --relay-url http://127.0.0.1:8300
```

- [ ] **Step 2: Update mesh README**

State that `examples/python/relay_mesh/` is the advanced relay mesh example and not the basic relay smoke path. Keep the existing mesh commands.

- [ ] **Step 3: Search for stale references**

Run:

```bash
rg -n "examples/python/(client|crm_process)\\.py|crm_process\\.py|python/client\\.py" README.md README.zh-CN.md examples sdk docs/superpowers/specs docs/superpowers/plans
```

Update live docs and tests. Historical logs/specs/plans may remain as historical records.

## Task 5: Final Verification and Commit

**Files:**
- All changed example, test, and documentation files

- [ ] **Step 1: Run syntax checks**

Run:

```bash
python -m py_compile examples/python/*.py examples/python/relay_mesh/*.py
```

- [ ] **Step 2: Run focused pytest checks**

Run:

```bash
env C2_ENV_FILE= uv run pytest sdk/python/tests/unit/test_python_examples_syntax.py sdk/python/tests/integration/test_python_examples.py -q
```

- [ ] **Step 3: Review diff**

Run:

```bash
git diff -- examples sdk/python/tests docs/superpowers/plans/2026-04-28-python-examples-redesign.md
```

Confirm no unrelated Rust config or SDK IPC fallback cleanup is staged for the examples commit.

- [ ] **Step 4: Commit examples redesign only**

Stage only the plan, examples, example docs, and Python example tests:

```bash
git add docs/superpowers/plans/2026-04-28-python-examples-redesign.md examples sdk/python/tests/unit/test_python_examples_syntax.py sdk/python/tests/integration/test_python_examples.py
git commit -m "refactor(examples): split python transport examples"
```

Do not stage unrelated existing changes in Rust config or Python IPC config.

## Self-Review

- Spec coverage: the plan covers local preservation, IPC-only scripts, relay-only scripts, relay URL precedence, mesh documentation, tests, and removal of dual-mode entry points.
- Placeholder scan: no `TBD`, `TODO`, or unspecified implementation step remains.
- Type consistency: every script exposes `parse_args(argv: list[str] | None = None)` where tests need controlled argv, and runtime entry points call `main()`.
