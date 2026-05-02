"""Tests for graceful relay shutdown handling.

Verifies that CRM processes do not crash when the relay is unreachable
during ``cc.unregister()`` or ``cc.shutdown()``.
"""
from __future__ import annotations

import logging
from unittest.mock import MagicMock, patch

import pytest

import c_two as cc
from c_two.config import settings
from c_two.transport.registry import _ProcessRegistry


# -- Helpers ---------------------------------------------------------------

@cc.crm(namespace='cc.test.relay_shutdown', version='0.1.0')
class IRelayShutdownCRM:
    def ping(self) -> str:
        ...


class RelayShutdownCRM:
    def ping(self) -> str:
        return 'pong'


# -- Tests: unregister tolerates relay absence -----------------------------

class TestUnregisterRelayAbsence:
    """Explicit unregister reports relay failures; shutdown stays best-effort."""

    def setup_method(self):
        self.registry = _ProcessRegistry()
        settings.relay_address = None

    def teardown_method(self):
        try:
            self.registry.shutdown()
        except Exception:
            pass
        settings.relay_address = None

    @patch.object(_ProcessRegistry, '_relay_register')
    @patch.object(_ProcessRegistry, '_relay_unregister')
    def test_unregister_no_relay(self, mock_unreg, mock_reg):
        """Unregister works when relay calls are no-ops."""
        self.registry.register(IRelayShutdownCRM, RelayShutdownCRM(), name='test')
        self.registry.unregister('test')  # Should not raise

    @patch.object(_ProcessRegistry, '_relay_register')
    @patch.object(_ProcessRegistry, '_relay_unregister', side_effect=ConnectionError('relay down'))
    def test_explicit_unregister_raises_when_relay_unreachable(self, mock_unreg, mock_reg):
        """Explicit unregister reports relay cleanup failure after local removal."""
        self.registry.register(IRelayShutdownCRM, RelayShutdownCRM(), name='test_down')

        with pytest.raises(ConnectionError, match='relay down'):
            self.registry.unregister('test_down')
        assert 'test_down' not in self.registry.names

    @patch.object(_ProcessRegistry, '_relay_register')
    @patch.object(_ProcessRegistry, '_relay_unregister', side_effect=ConnectionError('relay down'))
    def test_shutdown_relay_unreachable(self, mock_unreg, mock_reg, caplog):
        """Shutdown logs info when relay is unreachable (no error)."""
        self.registry.register(IRelayShutdownCRM, RelayShutdownCRM(), name='test_sd')

        with caplog.at_level(logging.INFO):
            self.registry.shutdown()  # Should not raise

        assert any('unreachable' in r.message.lower() or 'relay' in r.message.lower()
                    for r in caplog.records)

    def test_relay_unregister_surfaces_non_success_http_status(self):
        settings.relay_address = 'http://relay.test'

        class FailingRelayControl:
            def unregister(self, name, server_id):  # noqa: ARG002
                err = RuntimeError('HTTP 403: Forbidden')
                err.status_code = 403
                raise err

        self.registry._relay_control_client = FailingRelayControl()  # noqa: SLF001
        self.registry._relay_control_address = 'http://relay.test'  # noqa: SLF001

        with pytest.raises(RuntimeError, match='HTTP 403'):
            self.registry._relay_unregister('grid', 'server-grid')

    def test_relay_unregister_does_not_treat_404_as_success(self):
        settings.relay_address = 'http://relay.test'

        class MissingRelayControl:
            def unregister(self, name, server_id):  # noqa: ARG002
                err = RuntimeError('HTTP 404: Not Found')
                err.status_code = 404
                raise err

        self.registry._relay_control_client = MissingRelayControl()  # noqa: SLF001
        self.registry._relay_control_address = 'http://relay.test'  # noqa: SLF001

        with pytest.raises(RuntimeError, match='HTTP 404'):
            self.registry._relay_unregister('grid', 'server-grid')

    def test_relay_register_surfaces_non_duplicate_http_status(self):
        settings.relay_address = 'http://relay.test'

        class RejectingRelayControl:
            def register(self, name, server_id, ipc_address, crm_ns, crm_ver):  # noqa: ARG002
                err = RuntimeError('HTTP 403: Forbidden')
                err.status_code = 403
                raise err

        self.registry._relay_control_client = RejectingRelayControl()  # noqa: SLF001
        self.registry._relay_control_address = 'http://relay.test'  # noqa: SLF001

        with pytest.raises(RuntimeError, match='Relay registration failed.*HTTP 403'):
            self.registry._relay_register(
                'grid',
                'server-grid',
                'ipc://server-grid',
            )
