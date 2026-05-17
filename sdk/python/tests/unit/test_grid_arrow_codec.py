import json
import sys
from pathlib import Path

import pytest

import c_two as cc
from c_two.crm.descriptor import build_contract_descriptor


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
    from grid.transferables import (
        ARROW_IPC_CODEC_ID,
        GRID_ATTRIBUTE_BATCH_CODEC_REF,
        GRID_SCHEMA_CODEC_REF,
    )

    return Grid, ARROW_IPC_CODEC_ID, GRID_SCHEMA_CODEC_REF, GRID_ATTRIBUTE_BATCH_CODEC_REF


def test_grid_arrow_payloads_emit_explicit_codec_refs(monkeypatch):
    Grid, arrow_id, schema_ref, attribute_batch_ref = _load_grid_contract(monkeypatch)

    descriptor = build_contract_descriptor(Grid, methods=['get_schema', 'get_grid_infos'])
    methods = {method['name']: method for method in descriptor['methods']}

    assert methods['get_schema']['wire']['output'] == schema_ref.to_wire_ref()
    assert methods['get_grid_infos']['wire']['output'] == attribute_batch_ref.to_wire_ref()
    assert methods['get_grid_infos']['return']['kind'] == 'list'
    assert methods['get_grid_infos']['return']['item']['abi_ref']['id'] == arrow_id
    assert methods['get_schema']['wire']['output']['id'] == arrow_id
    assert methods['get_grid_infos']['wire']['input']['id'] == 'c-two.control.json'
    assert 'custom_schema' not in json.dumps(descriptor, sort_keys=True)


def test_grid_schema_method_can_export_as_portable_arrow_subset(monkeypatch):
    Grid, _arrow_id, schema_ref, _attribute_batch_ref = _load_grid_contract(monkeypatch)

    descriptor = json.loads(cc.export_contract_descriptor(Grid, methods=['get_schema']))

    assert descriptor['schema'] == 'c-two.contract.v1'
    assert descriptor['methods'][0]['wire']['output'] == schema_ref.to_wire_ref()
    assert 'python-pickle-default' not in json.dumps(descriptor, sort_keys=True)


def test_grid_control_params_export_with_portable_control_json(monkeypatch):
    Grid, _arrow_id, _schema_ref, _attribute_batch_ref = _load_grid_contract(monkeypatch)

    descriptor = json.loads(cc.export_contract_descriptor(Grid, methods=['get_grid_infos']))
    method = descriptor['methods'][0]

    assert method['wire']['input']['id'] == 'c-two.control.json'
    assert 'python-pickle-default' not in json.dumps(descriptor, sort_keys=True)


def test_full_grid_contract_exports_as_portable_descriptor(monkeypatch):
    Grid, _arrow_id, schema_ref, attribute_batch_ref = _load_grid_contract(monkeypatch)

    descriptor = json.loads(cc.export_contract_descriptor(Grid))
    methods = {method['name']: method for method in descriptor['methods']}
    encoded = json.dumps(descriptor, sort_keys=True)

    assert descriptor['schema'] == 'c-two.contract.v1'
    assert methods['get_schema']['wire']['output'] == schema_ref.to_wire_ref()
    assert methods['get_grid_infos']['wire']['output'] == attribute_batch_ref.to_wire_ref()
    assert methods['subdivide_grids']['wire']['input']['id'] == 'c-two.control.json'
    assert methods['subdivide_grids']['wire']['output']['id'] == 'c-two.control.json'
    assert methods['hello']['wire']['input']['id'] == 'c-two.control.json'
    assert methods['none_hello']['wire']['output']['id'] == 'c-two.control.json'
    assert 'python-pickle-default' not in encoded
