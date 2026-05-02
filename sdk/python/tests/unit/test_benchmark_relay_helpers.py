from __future__ import annotations

import importlib.util
from pathlib import Path
from types import ModuleType


def _repo_root() -> Path:
    return Path(__file__).resolve().parents[4]


def _load_benchmark(script_name: str) -> ModuleType:
    path = _repo_root() / 'sdk/python/benchmarks' / script_name
    spec = importlib.util.spec_from_file_location(script_name.removesuffix('.py'), path)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_relay_qps_unregister_sends_server_id():
    module = _load_benchmark('relay_qps_benchmark.py')
    calls: list[tuple[str, dict[str, str]]] = []
    module._post_json = lambda url, payload: calls.append((url, payload))

    module._unregister_relay_upstream('http://127.0.0.1:8300', 'grid', 'srv-1')

    assert calls == [
        ('http://127.0.0.1:8300/_unregister', {'name': 'grid', 'server_id': 'srv-1'}),
    ]


def test_three_mode_unregister_sends_server_id():
    module = _load_benchmark('three_mode_benchmark.py')
    calls: list[tuple[str, dict[str, str]]] = []
    module._post_json = lambda url, payload: calls.append((url, payload))

    module._unregister_relay_upstream('http://127.0.0.1:8300', 'grid', 'srv-1')

    assert calls == [
        ('http://127.0.0.1:8300/_unregister', {'name': 'grid', 'server_id': 'srv-1'}),
    ]


def test_relay_qps_best_effort_unregister_passes_server_id():
    module = _load_benchmark('relay_qps_benchmark.py')
    calls: list[tuple[str, str, str]] = []
    module._unregister_relay_upstream = lambda relay_url, name, server_id: calls.append(
        (relay_url, name, server_id)
    )

    module._best_effort_unregister_relay_upstream('http://127.0.0.1:8300', 'grid', 'srv-1')

    assert calls == [('http://127.0.0.1:8300', 'grid', 'srv-1')]


def test_three_mode_best_effort_unregister_passes_server_id():
    module = _load_benchmark('three_mode_benchmark.py')
    calls: list[tuple[str, str, str]] = []
    module._unregister_relay_upstream = lambda relay_url, name, server_id: calls.append(
        (relay_url, name, server_id)
    )

    module._best_effort_unregister_relay_upstream('http://127.0.0.1:8300', 'grid', 'srv-1')

    assert calls == [('http://127.0.0.1:8300', 'grid', 'srv-1')]
