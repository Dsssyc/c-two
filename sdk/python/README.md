# C-Two Python SDK

This directory contains the published Python package for C-Two.

The Python SDK does not own the `c3` CLI. `c3` is built from the repository root
`cli/` package and released as an independent native tool.

## Development

Run Python SDK development commands from the repository root. The repository
does not pin a local `.python-version`; use any supported Python interpreter
from the package metadata, with Python 3.12 as the recommended baseline for
local development.

Install workspace dependencies and build the Python native extension:

```bash
uv sync
```

After changing Rust code under `core/` or `sdk/python/native/`, force uv to
rebuild the Python package:

```bash
uv sync --reinstall-package c-two
```

Run the Python SDK tests:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests -q --timeout=30
```

For source-checkout CLI usage, build and link the development binary:

```bash
python tools/dev/c3_tool.py --build --link
c3 --version
```

Run repository-level Rust checks separately:

```bash
cargo test --manifest-path core/Cargo.toml --workspace
cargo test --manifest-path cli/Cargo.toml
```

## Examples

Python examples live under `../../examples/python/`:

```bash
uv run python examples/python/local.py
```

## Benchmarks

Python-specific benchmarks live in `benchmarks/`:

```bash
C2_RELAY_ADDRESS= uv run python sdk/python/benchmarks/segment_size_benchmark.py
```
