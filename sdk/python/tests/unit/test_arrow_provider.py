import dataclasses
import json

import pytest

import c_two as cc
from c_two.crm.descriptor import build_contract_descriptor

pytest.importorskip('pyarrow', reason='Arrow provider tests require pyarrow')


@pytest.fixture(autouse=True)
def clear_codec_registry():
    from c_two.crm.codec import _clear_codec_registry_for_tests

    _clear_codec_registry_for_tests()
    try:
        yield
    finally:
        _clear_codec_registry_for_tests()


def test_arrow_record_provider_generates_deterministic_codec_and_round_trips():
    from c_two.providers import arrow

    @arrow.record(schema_id='test.point.arrow-ipc.v1')
    class Point:
        x: float
        y: float
        name: str

    assert dataclasses.is_dataclass(Point)

    provider_a = arrow.ArrowCodecProvider()
    provider_b = arrow.ArrowCodecProvider()
    binding_a = provider_a.candidates_for_type(Point, {'position': 'output'})
    binding_b = provider_b.candidates_for_type(Point, {'position': 'output'})

    assert binding_a is not None
    assert binding_b is not None
    assert binding_a.codec_ref == binding_b.codec_ref
    assert binding_a.codec_ref.id == arrow.ARROW_IPC_CODEC_ID
    assert binding_a.codec_ref.schema == arrow.ARROW_IPC_SCHEMA
    assert binding_a.codec_ref.media_type == arrow.ARROW_IPC_MEDIA_TYPE

    payload = Point(x=1.25, y=2.5, name='origin')
    restored = binding_a.transferable.deserialize(binding_a.transferable.serialize(payload))

    assert restored == payload


def test_arrow_provider_rejects_unsupported_record_annotations():
    from c_two.providers import arrow

    @arrow.record(schema_id='test.bad.arrow-ipc.v1')
    class BadRecord:
        values: set[int]

    provider = arrow.ArrowCodecProvider()

    with pytest.raises(TypeError, match='Unsupported Arrow record annotation.*set'):
        provider.candidates_for_type(BadRecord, {'position': 'output'})


def test_arrow_provider_supports_list_record_wire_without_manual_transfer():
    from c_two.providers import arrow

    @arrow.record(schema_id='test.cell.arrow-ipc.v1')
    class Cell:
        level: int
        global_id: int
        active: bool

    arrow.use_arrow()

    @cc.crm(namespace='test.arrow-provider', version='0.1.0')
    class CellStore:
        def get_cell(self, global_id: int) -> Cell:
            ...

        def list_cells(self, level: int) -> list[Cell]:
            ...

        def list_ids(self) -> list[int]:
            ...

    descriptor = json.loads(cc.export_contract_descriptor(CellStore))
    methods = {method['name']: method for method in descriptor['methods']}

    assert methods['get_cell']['wire']['output']['id'] == arrow.ARROW_IPC_CODEC_ID
    assert methods['get_cell']['return']['kind'] == 'codec'
    assert methods['list_cells']['wire']['output']['id'] == arrow.ARROW_IPC_CODEC_ID
    assert methods['list_cells']['return']['kind'] == 'list'
    assert methods['list_cells']['return']['item']['kind'] == 'codec'
    assert methods['list_cells']['return']['item']['codec']['id'] == arrow.ARROW_IPC_CODEC_ID
    assert methods['list_ids']['wire']['output']['id'] == 'c-two.control.json'
    assert getattr(CellStore.list_cells, '_output_transferable').__module__ == 'c_two.providers.arrow.generated'

    from c_two.crm.codec import _clear_codec_registry_for_tests

    _clear_codec_registry_for_tests()
    rebuilt = build_contract_descriptor(CellStore, methods=['list_cells'], portable=True)
    assert rebuilt['methods'][0]['return']['item']['codec']['id'] == arrow.ARROW_IPC_CODEC_ID

    restored = CellStore.list_cells._output_transferable.deserialize(
        CellStore.list_cells._output_transferable.serialize([
            Cell(level=1, global_id=10, active=True),
            Cell(level=1, global_id=11, active=False),
        ]),
    )

    assert restored == [
        Cell(level=1, global_id=10, active=True),
        Cell(level=1, global_id=11, active=False),
    ]


def test_arrow_provider_descriptor_helper_keeps_plain_dataclass_portable():
    from c_two.providers import arrow

    @arrow.record(schema_id='test.sample.arrow-ipc.v1')
    class Sample:
        value: int

    arrow.use_arrow()

    @cc.crm(namespace='test.arrow-provider', version='0.1.0')
    class SampleStore:
        def echo(self, sample: Sample) -> Sample:
            ...

    descriptor = build_contract_descriptor(SampleStore, portable=True)
    method = descriptor['methods'][0]

    assert method['parameters'][0]['annotation']['kind'] == 'codec'
    assert method['wire']['input']['id'] == arrow.ARROW_IPC_CODEC_ID
    assert method['wire']['output']['id'] == arrow.ARROW_IPC_CODEC_ID
