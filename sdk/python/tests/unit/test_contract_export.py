import json

import pytest

import c_two as cc


@pytest.fixture(autouse=True)
def clear_codec_registry():
    from c_two.crm.codec import _clear_codec_registry_for_tests

    _clear_codec_registry_for_tests()
    try:
        yield
    finally:
        _clear_codec_registry_for_tests()


def test_export_contract_descriptor_uses_c_two_contract_schema_and_rejects_pickle_for_unsupported_bytes():
    @cc.crm(namespace='test.contract-export', version='0.1.0')
    class PickleOnly:
        def echo(self, value: bytes) -> bytes:
            ...

    with pytest.raises(ValueError, match='python-pickle-default'):
        cc.export_contract_descriptor(PickleOnly)


def test_export_contract_descriptor_uses_portable_control_json_for_primitives():
    @cc.crm(namespace='test.contract-export', version='0.1.0')
    class Control:
        def echo(self, values: list[int], label: str | None = None) -> list[str | None]:
            ...

    exported = cc.export_contract_descriptor(Control)
    descriptor = json.loads(exported)
    method = descriptor['methods'][0]

    assert descriptor['schema'] == 'c-two.contract.v1'
    assert method['wire']['input']['id'] == 'c-two.control.json'
    assert method['wire']['output']['id'] == 'c-two.control.json'
    assert 'python-pickle-default' not in exported


def test_export_contract_descriptor_returns_canonical_portable_json():
    class Payload:
        def __init__(self, value: int):
            self.value = value

    payload_ref = cc.CodecRef(id='org.example.export', version='1', schema_sha256='a' * 64)

    @cc.transferable(codec_ref=payload_ref)
    class PayloadCodec:
        def serialize(value: Payload) -> bytes:
            return str(value.value).encode()

        def deserialize(data: bytes) -> Payload:
            return Payload(int(bytes(data)))

    cc.bind_codec(Payload, PayloadCodec)

    @cc.crm(namespace='test.contract-export', version='0.1.0')
    class Portable:
        def echo(self, value: Payload) -> Payload:
            ...

    exported = cc.export_contract_descriptor(Portable)
    descriptor = json.loads(exported)

    assert exported == json.dumps(descriptor, sort_keys=True, separators=(',', ':'))
    assert descriptor['schema'] == 'c-two.contract.v1'
    assert descriptor['methods'][0]['wire']['input'] == payload_ref.to_wire_ref()
    assert 'python-pickle-default' not in exported


def test_export_contract_descriptor_rejects_legacy_custom_abi_after_native_validation():
    @cc.transferable(abi_id='org.example.legacy.raw.v1')
    class LegacyCodec:
        value: int

        def serialize(value: 'LegacyCodec') -> bytes:
            return b''

        def deserialize(data: bytes) -> 'LegacyCodec':
            return LegacyCodec(1)

    @cc.crm(namespace='test.contract-export', version='0.1.0')
    class LegacyPortable:
        @cc.transfer(input=LegacyCodec, output=LegacyCodec)
        def echo(self, value: LegacyCodec) -> LegacyCodec:
            ...

    with pytest.raises(ValueError, match='codec_ref'):
        cc.export_contract_descriptor(LegacyPortable)


def test_contract_export_cli_writes_descriptor(tmp_path, monkeypatch):
    module_path = tmp_path / 'portable_contract_module.py'
    module_path.write_text(
        '\n'.join([
            'import c_two as cc',
            '@cc.crm(namespace="test.contract-export-cli", version="0.1.0")',
            'class Ping:',
            '    def ping(self) -> None:',
            '        ...',
            '',
        ]),
    )
    out_path = tmp_path / 'contract.json'
    monkeypatch.syspath_prepend(str(tmp_path))

    from c_two.cli.contract import main

    assert main(['export', 'portable_contract_module:Ping', '--out', str(out_path)]) == 0
    descriptor = json.loads(out_path.read_text())

    assert descriptor['schema'] == 'c-two.contract.v1'
    assert descriptor['crm']['name'] == 'Ping'
    assert descriptor['methods'][0]['name'] == 'ping'
