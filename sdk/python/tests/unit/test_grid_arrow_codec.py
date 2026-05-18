import json
import sys
from pathlib import Path

import pytest

import c_two as cc
from c_two.crm.codec import _clear_codec_registry_for_tests
from c_two.crm.descriptor import build_contract_descriptor


@pytest.fixture(autouse=True)
def clear_codec_registry():
    _clear_codec_registry_for_tests()
    try:
        yield
    finally:
        _clear_codec_registry_for_tests()


def _load_grid_contract(monkeypatch):
    pytest.importorskip('pyarrow', reason='grid Arrow codec tests require pyarrow')
    root = Path(__file__).resolve().parents[4]
    monkeypatch.syspath_prepend(str(root / 'examples/python'))
    for module_name in (
        'grid.grid_contract',
        'grid.transferables',
    ):
        sys.modules.pop(module_name, None)
    from grid.grid_contract import Grid
    from c_two.providers import arrow

    return Grid, arrow.ARROW_IPC_CODEC_ID


def test_grid_arrow_payloads_emit_provider_codec_refs(monkeypatch):
    Grid, arrow_id = _load_grid_contract(monkeypatch)
    root = Path(__file__).resolve().parents[4]
    transferables_source = (root / 'examples/python/grid/transferables.py').read_text()

    descriptor = build_contract_descriptor(Grid, methods=['get_schema', 'get_grid_infos'])
    methods = {method['name']: method for method in descriptor['methods']}

    assert 'schema_id=' not in transferables_source
    assert 'GridAttributeBatch' not in transferables_source
    assert methods['get_schema']['wire']['output']['id'] == arrow_id
    assert methods['get_grid_infos']['wire']['output']['id'] == arrow_id
    assert methods['get_grid_infos']['return']['kind'] == 'list'
    assert methods['get_grid_infos']['return']['item']['kind'] == 'codec'
    assert methods['get_grid_infos']['return']['item']['codec']['id'] == arrow_id
    assert methods['get_schema']['wire']['output']['id'] == arrow_id
    assert methods['get_grid_infos']['wire']['input']['id'] == 'c-two.control.json'
    assert getattr(Grid.get_grid_infos, '_output_transferable').__module__ == 'c_two.providers.arrow.generated'
    assert 'custom_schema' not in json.dumps(descriptor, sort_keys=True)


def test_grid_schema_method_can_export_as_portable_arrow_subset(monkeypatch):
    Grid, arrow_id = _load_grid_contract(monkeypatch)

    descriptor = json.loads(cc.export_contract_descriptor(Grid, methods=['get_schema']))

    assert descriptor['schema'] == 'c-two.contract.v1'
    assert descriptor['methods'][0]['wire']['output']['id'] == arrow_id
    assert 'python-pickle-default' not in json.dumps(descriptor, sort_keys=True)


def test_grid_control_params_export_with_portable_control_json(monkeypatch):
    Grid, _arrow_id = _load_grid_contract(monkeypatch)

    descriptor = json.loads(cc.export_contract_descriptor(Grid, methods=['get_grid_infos']))
    method = descriptor['methods'][0]

    assert method['wire']['input']['id'] == 'c-two.control.json'
    assert 'python-pickle-default' not in json.dumps(descriptor, sort_keys=True)


def test_full_grid_contract_exports_as_portable_descriptor(monkeypatch):
    Grid, arrow_id = _load_grid_contract(monkeypatch)

    descriptor = json.loads(cc.export_contract_descriptor(Grid))
    methods = {method['name']: method for method in descriptor['methods']}
    encoded = json.dumps(descriptor, sort_keys=True)

    assert descriptor['schema'] == 'c-two.contract.v1'
    assert methods['get_schema']['wire']['output']['id'] == arrow_id
    assert methods['get_grid_infos']['wire']['output']['id'] == arrow_id
    assert methods['subdivide_grids']['wire']['input']['id'] == 'c-two.control.json'
    assert methods['subdivide_grids']['wire']['output']['id'] == 'c-two.control.json'
    assert methods['hello']['wire']['input']['id'] == 'c-two.control.json'
    assert methods['none_hello']['wire']['output']['id'] == 'c-two.control.json'
    assert 'python-pickle-default' not in encoded
