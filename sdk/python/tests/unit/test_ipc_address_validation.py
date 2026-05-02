import pytest

from c_two.transport.client import util


@pytest.mark.parametrize(
    'address',
    [
        'ipc://../escape',
        'ipc://bad/name',
        'ipc://bad\\name',
        'ipc://.',
        'ipc://..',
        'ipc:// leading',
        'ipc://trailing ',
        'ipc://bad\nname',
        'tcp://not-ipc',
    ],
)
def test_client_util_rejects_path_like_ipc_region(address):
    with pytest.raises(ValueError):
        util._socket_path_from_address(address)


def test_client_util_accepts_plain_ipc_region():
    assert util._socket_path_from_address('ipc://unit-server').endswith(
        '/tmp/c_two_ipc/unit-server.sock'
    )


def test_client_util_uses_native_validation(monkeypatch):
    calls = []

    def fake_validate(region_id: str) -> None:
        calls.append(region_id)
        raise ValueError('native validation sentinel')

    monkeypatch.setattr(util, '_validate_region_id', fake_validate)

    with pytest.raises(ValueError, match='native validation sentinel'):
        util._socket_path_from_address('ipc://unit-server')

    assert calls == ['unit-server']
