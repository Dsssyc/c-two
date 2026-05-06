"""Unit tests for concurrency config and thin native scheduler adapter."""
from __future__ import annotations

from contextlib import nullcontext
from types import SimpleNamespace

import pytest

from c_two.transport.server.scheduler import (
    ConcurrencyConfig,
    ConcurrencyMode,
    Scheduler,
)


class TestConcurrencyConfig:
    def test_default_mode(self):
        cfg = ConcurrencyConfig()
        assert cfg.mode is ConcurrencyMode.READ_PARALLEL
        assert cfg.max_workers is None
        assert cfg.max_pending is None

    def test_string_mode_coerces_to_enum(self):
        cfg = ConcurrencyConfig(mode='parallel')
        assert cfg.mode is ConcurrencyMode.PARALLEL

    def test_invalid_mode(self):
        with pytest.raises(ValueError):
            ConcurrencyConfig(mode='bogus')

    def test_bad_max_workers(self):
        with pytest.raises(ValueError, match='max_workers'):
            ConcurrencyConfig(max_workers=0)

    def test_bad_max_pending(self):
        with pytest.raises(ValueError, match='max_pending'):
            ConcurrencyConfig(max_pending=0)


class FakeNativeConcurrency:
    def __init__(self, *, unconstrained=False, snapshot=None):
        self.is_unconstrained = unconstrained
        self._snapshot = snapshot or SimpleNamespace(
            mode='parallel',
            max_pending=None,
            max_workers=None,
            pending=0,
            active_workers=0,
            closed=False,
            is_unconstrained=unconstrained,
        )
        self.guard_calls: list[int] = []
        self.closed = False

    def snapshot(self):
        return self._snapshot

    def execution_guard(self, method_idx: int):
        self.guard_calls.append(method_idx)
        return nullcontext()

    def close(self):
        self.closed = True


class TestSchedulerAdapter:
    def test_method_idx_projects_method_index(self):
        sched = Scheduler(FakeNativeConcurrency(), {'op': 3})
        assert sched.method_idx('op') == 3

    def test_snapshot_forwards_to_native(self):
        snap = SimpleNamespace(mode='read_parallel', closed=False)
        sched = Scheduler(FakeNativeConcurrency(snapshot=snap), {'op': 0})
        assert sched.snapshot() is snap

    def test_execution_guard_forwards_method_idx_to_native(self):
        native = FakeNativeConcurrency()
        sched = Scheduler(native, {'op': 7})
        with sched.execution_guard(sched.method_idx('op')):
            pass
        assert native.guard_calls == [7]

    def test_is_unconstrained_projects_native_fast_path(self):
        sched = Scheduler(FakeNativeConcurrency(unconstrained=True), {'op': 0})
        assert sched.is_unconstrained is True

    def test_shutdown_closes_native_handle(self):
        native = FakeNativeConcurrency()
        sched = Scheduler(native, {'op': 0})
        sched.shutdown()
        assert native.closed is True
