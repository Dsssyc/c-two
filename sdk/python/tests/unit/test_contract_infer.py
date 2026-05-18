import json
import sys
from pathlib import Path

import pytest

import c_two as cc


def test_infer_crm_from_resource_exports_portable_descriptor():
    class Resource:
        @cc.read
        def total(self, values: list[int]) -> int:
            return sum(values)

        def label(self, name: str, suffix: str | None = None) -> str | None:
            return name if suffix is None else f'{name}-{suffix}'

        def _helper(self) -> None:
            ...

    crm_class = cc.infer_crm_from_resource(
        Resource,
        namespace='test.infer',
        version='0.1.0',
        name='ResourceCRM',
        methods=['total', 'label'],
    )
    descriptor = json.loads(cc.export_contract_descriptor(crm_class))
    methods = {method['name']: method for method in descriptor['methods']}

    assert descriptor['crm'] == {
        'namespace': 'test.infer',
        'name': 'ResourceCRM',
        'version': '0.1.0',
    }
    assert list(methods) == ['total', 'label']
    assert methods['total']['access'] == 'read'
    assert methods['total']['wire']['input']['id'] == 'c-two.control.json'
    assert methods['label']['parameters'][1]['default'] == {
        'kind': 'json_scalar',
        'value': None,
    }


def test_infer_crm_requires_explicit_public_methods():
    class Resource:
        def public(self) -> None:
            ...

    with pytest.raises(ValueError, match='methods'):
        cc.infer_crm_from_resource(
            Resource,
            namespace='test.infer',
            version='0.1.0',
            methods=[],
        )
    with pytest.raises(ValueError, match='private'):
        cc.infer_crm_from_resource(
            Resource,
            namespace='test.infer',
            version='0.1.0',
            methods=['_hidden'],
        )


def test_infer_crm_preserves_method_transfer_metadata():
    codec_ref = cc.CodecRef(
        id='org.example.payload',
        version='1',
        schema='example.payload.v1',
    )

    @cc.transferable(codec_ref=codec_ref)
    class Payload:
        value: str

        def serialize(data: 'Payload') -> bytes:
            return data.value.encode()

        def deserialize(data: bytes) -> 'Payload':
            return Payload(data.decode())

    class Resource:
        @cc.transfer(input=Payload, output=Payload)
        def echo(self, payload: Payload) -> Payload:
            return payload

    crm_class = cc.infer_crm_from_resource(
        Resource,
        namespace='test.infer',
        version='0.1.0',
        name='PayloadCRM',
        methods=['echo'],
    )
    descriptor = json.loads(cc.export_contract_descriptor(crm_class))
    method = descriptor['methods'][0]

    assert method['wire']['input'] == codec_ref.to_wire_ref()
    assert method['wire']['output'] == codec_ref.to_wire_ref()


def test_infer_crm_rejects_ambiguous_resource_signatures():
    class Resource:
        def missing_param_annotation(self, value) -> int:
            return 1

        def missing_return_annotation(self, value: int):
            return value

        def varargs(self, *values: int) -> int:
            return sum(values)

        def kwargs(self, **values: int) -> int:
            return len(values)

        def keyword_only(self, *, value: int) -> int:
            return value

    cases = [
        ('missing_param_annotation', 'missing a type annotation'),
        ('missing_return_annotation', 'missing a return annotation'),
        ('varargs', 'varargs'),
        ('kwargs', 'varargs'),
        ('keyword_only', 'keyword-only'),
    ]
    for method_name, message in cases:
        with pytest.raises(TypeError, match=message):
            cc.infer_crm_from_resource(
                Resource,
                namespace='test.infer',
                version='0.1.0',
                methods=[method_name],
            )


def test_grid_resource_infer_smoke_exports_portable_subset(monkeypatch):
    pytest.importorskip('pyarrow', reason='grid Arrow codec tests require pyarrow')
    root = Path(__file__).resolve().parents[4]
    monkeypatch.syspath_prepend(str(root / 'examples/python'))
    for module_name in (
        'grid.nested_grid',
        'grid.transferables',
    ):
        sys.modules.pop(module_name, None)
    from grid.nested_grid import NestedGrid

    methods = [
        'get_schema',
        'subdivide_grids',
        'get_active_grid_infos',
        'hello',
        'none_hello',
    ]
    crm_class = cc.infer_crm_from_resource(
        NestedGrid,
        namespace='demo.grid.inferred',
        version='0.1.0',
        name='InferredGrid',
        methods=methods,
    )
    descriptor = json.loads(cc.export_contract_descriptor(crm_class))
    by_name = {method['name']: method for method in descriptor['methods']}
    encoded = json.dumps(descriptor, sort_keys=True)

    assert [method['name'] for method in descriptor['methods']] == methods
    assert by_name['get_schema']['wire']['output']['id'] == 'org.apache.arrow.ipc'
    assert by_name['subdivide_grids']['wire']['input']['id'] == 'c-two.control.json'
    assert by_name['subdivide_grids']['wire']['output']['id'] == 'c-two.control.json'
    assert by_name['get_active_grid_infos']['wire']['output']['id'] == 'c-two.control.json'
    assert 'python-pickle-default' not in encoded


def test_contract_infer_cli_writes_descriptor(tmp_path, monkeypatch):
    module_path = tmp_path / 'resource_module.py'
    module_path.write_text(
        '\n'.join([
            'class Resource:',
            '    def hello(self, name: str) -> str:',
            '        return name',
            '',
        ]),
    )
    out_path = tmp_path / 'contract.json'
    monkeypatch.syspath_prepend(str(tmp_path))

    from c_two.cli.contract import main

    assert main([
        'infer',
        'resource_module:Resource',
        '--namespace',
        'test.infer-cli',
        '--version',
        '0.1.0',
        '--name',
        'ResourceCRM',
        '--method',
        'hello',
        '--out',
        str(out_path),
    ]) == 0
    descriptor = json.loads(out_path.read_text())

    assert descriptor['schema'] == 'c-two.contract.v1'
    assert descriptor['crm']['name'] == 'ResourceCRM'
    assert descriptor['methods'][0]['name'] == 'hello'
