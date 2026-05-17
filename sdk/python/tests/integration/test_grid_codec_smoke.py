from __future__ import annotations

import sys
from pathlib import Path

import pytest

import c_two as cc
from c_two.config.settings import settings
from c_two.crm.codec import _clear_codec_registry_for_tests
from c_two.transport.registry import _ProcessRegistry


@pytest.fixture(autouse=True)
def clean_runtime():
    previous_relay = settings.relay_anchor_address
    _ProcessRegistry.reset()
    _clear_codec_registry_for_tests()
    try:
        yield
    finally:
        _ProcessRegistry.reset()
        settings.relay_anchor_address = previous_relay
        _clear_codec_registry_for_tests()


def _load_grid(monkeypatch):
    pytest.importorskip('pandas', reason='grid smoke tests require examples dependencies')
    pytest.importorskip('pyarrow', reason='grid smoke tests require examples dependencies')
    root = Path(__file__).resolve().parents[4]
    monkeypatch.syspath_prepend(str(root / 'examples/python'))
    for module_name in (
        'grid.grid_contract',
        'grid.nested_grid',
        'grid.transferables',
    ):
        sys.modules.pop(module_name, None)
    from grid.grid_contract import Grid
    from grid.nested_grid import NestedGrid

    return Grid, NestedGrid


def _make_grid_resource(NestedGrid):
    return NestedGrid(
        epsg=4326,
        bounds=[0.0, 0.0, 4.0, 4.0],
        first_size=[2.0, 2.0],
        subdivide_rules=[[2, 2], [2, 2]],
    )


def _exercise_grid(grid) -> None:
    schema = grid.get_schema()
    assert schema.epsg == 4326
    assert schema.bounds == [0.0, 0.0, 4.0, 4.0]
    assert schema.subdivide_rules == [[2, 2], [2, 2]]

    infos = grid.get_grid_infos(1, [0, 1])
    assert [info.global_id for info in infos] == [0, 1]
    assert all(info.activate for info in infos)
    assert infos[0].min_x == pytest.approx(0.0)
    assert infos[0].max_x == pytest.approx(2.0)

    keys = grid.subdivide_grids([1], [0])
    assert keys == ['2-0', '2-1', '2-4', '2-5']


def test_grid_arrow_payloads_work_thread_local(monkeypatch):
    Grid, NestedGrid = _load_grid(monkeypatch)
    cc.register(Grid, _make_grid_resource(NestedGrid), name='grid-arrow-thread')

    grid = cc.connect(Grid, name='grid-arrow-thread')
    try:
        assert grid.client._mode == 'thread'  # noqa: SLF001
        _exercise_grid(grid)
    finally:
        cc.close(grid)


def test_grid_arrow_payloads_work_direct_ipc_with_bad_relay(monkeypatch):
    Grid, NestedGrid = _load_grid(monkeypatch)
    cc.register(Grid, _make_grid_resource(NestedGrid), name='grid-arrow-ipc')
    address = cc.server_address()
    assert address is not None

    previous_relay = settings.relay_anchor_address
    settings.relay_anchor_address = 'http://127.0.0.1:9'
    try:
        grid = cc.connect(Grid, name='grid-arrow-ipc', address=address)
    finally:
        settings.relay_anchor_address = previous_relay

    try:
        assert grid.client._mode == 'ipc'  # noqa: SLF001
        _exercise_grid(grid)
    finally:
        cc.close(grid)


def test_grid_arrow_payloads_work_explicit_http_relay(monkeypatch, start_c3_relay):
    Grid, NestedGrid = _load_grid(monkeypatch)
    relay = start_c3_relay()
    cc.set_relay_anchor(relay.url)
    cc.register(Grid, _make_grid_resource(NestedGrid), name='grid-arrow-relay')

    grid = cc.connect(Grid, name='grid-arrow-relay', address=relay.url)
    try:
        assert grid.client._mode == 'http'  # noqa: SLF001
        _exercise_grid(grid)
    finally:
        cc.close(grid)
