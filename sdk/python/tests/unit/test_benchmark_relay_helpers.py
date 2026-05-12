from __future__ import annotations

import importlib.util
from pathlib import Path
from types import ModuleType

from c_two.crm.contract import crm_contract


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


def test_relay_qps_register_sends_instance_and_contract_fields():
    module = _load_benchmark('relay_qps_benchmark.py')
    calls: list[tuple[str, dict[str, str]]] = []
    module._post_json = lambda url, payload: calls.append((url, payload))
    expected = crm_contract(module.Hello)

    module._register_relay_upstream(
        'http://127.0.0.1:8300',
        'grid',
        'ipc://grid',
        'srv-1',
        'inst-1',
        expected,
    )

    assert calls == [
        (
            'http://127.0.0.1:8300/_register',
            {
                'name': 'grid',
                'server_id': 'srv-1',
                'server_instance_id': 'inst-1',
                'address': 'ipc://grid',
                'crm_ns': expected.crm_ns,
                'crm_name': expected.crm_name,
                'crm_ver': expected.crm_ver,
                'abi_hash': expected.abi_hash,
                'signature_hash': expected.signature_hash,
            },
        ),
    ]


def test_relay_qps_hey_args_include_expected_contract_headers():
    module = _load_benchmark('relay_qps_benchmark.py')
    expected = crm_contract(module.Hello)

    args = module._expected_contract_hey_args(expected)

    assert args == [
        '-H',
        f'x-c2-expected-crm-ns: {expected.crm_ns}',
        '-H',
        f'x-c2-expected-crm-name: {expected.crm_name}',
        '-H',
        f'x-c2-expected-crm-ver: {expected.crm_ver}',
        '-H',
        f'x-c2-expected-abi-hash: {expected.abi_hash}',
        '-H',
        f'x-c2-expected-signature-hash: {expected.signature_hash}',
    ]


def test_three_mode_unregister_sends_server_id():
    module = _load_benchmark('three_mode_benchmark.py')
    calls: list[tuple[str, dict[str, str]]] = []
    module._post_json = lambda url, payload: calls.append((url, payload))

    module._unregister_relay_upstream('http://127.0.0.1:8300', 'grid', 'srv-1')

    assert calls == [
        ('http://127.0.0.1:8300/_unregister', {'name': 'grid', 'server_id': 'srv-1'}),
    ]


def test_three_mode_register_sends_instance_and_contract_fields():
    module = _load_benchmark('three_mode_benchmark.py')
    calls: list[tuple[str, dict[str, str]]] = []
    module._post_json = lambda url, payload: calls.append((url, payload))
    expected = crm_contract(module.Echo)

    module._register_relay_upstream(
        'http://127.0.0.1:8300',
        'grid',
        'ipc://grid',
        'srv-1',
        'inst-1',
        expected,
    )

    assert calls == [
        (
            'http://127.0.0.1:8300/_register',
            {
                'name': 'grid',
                'server_id': 'srv-1',
                'server_instance_id': 'inst-1',
                'address': 'ipc://grid',
                'crm_ns': expected.crm_ns,
                'crm_name': expected.crm_name,
                'crm_ver': expected.crm_ver,
                'abi_hash': expected.abi_hash,
                'signature_hash': expected.signature_hash,
            },
        ),
    ]


def test_three_mode_dict_echo_contract_is_descriptor_safe():
    module = _load_benchmark('three_mode_benchmark.py')

    contract = crm_contract(module.DictEcho)

    assert len(contract.abi_hash) == 64
    assert len(contract.signature_hash) == 64


def test_segment_size_dict_echo_contract_is_descriptor_safe():
    module = _load_benchmark('segment_size_benchmark.py')

    contract = crm_contract(module.DictEcho)

    assert len(contract.abi_hash) == 64
    assert len(contract.signature_hash) == 64


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
