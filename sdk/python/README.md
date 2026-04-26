# C-Two Python SDK

This directory contains the published Python package for C-Two.

The Python SDK does not own the `c3` CLI. `c3` is built from the repository root
`cli/` package and released as an independent native tool.

## Development

From the repository root, rebuild the Python native extension and install
Python development dependencies:

```bash
uv sync --reinstall-package c-two
```

Run Python SDK tests:

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
