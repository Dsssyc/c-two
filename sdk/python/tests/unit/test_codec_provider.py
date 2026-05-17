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


def test_codec_ref_serializes_stably_and_validates_fields():
    ref = cc.CodecRef(
        id='org.example.payload',
        version='1',
        schema='example.schema.v1',
        schema_sha256='a' * 64,
        capabilities=('buffer-view', 'bytes'),
        media_type='application/vnd.example.payload',
    )

    assert ref.to_wire_ref() == {
        'capabilities': ['buffer-view', 'bytes'],
        'id': 'org.example.payload',
        'kind': 'codec_ref',
        'media_type': 'application/vnd.example.payload',
        'portable': True,
        'schema': 'example.schema.v1',
        'schema_sha256': 'a' * 64,
        'version': '1',
    }

    with pytest.raises(ValueError, match='id'):
        cc.CodecRef(id=' ', version='1')
    with pytest.raises(ValueError, match='schema_sha256'):
        cc.CodecRef(id='org.example.payload', version='1', schema_sha256='bad')
    with pytest.raises(ValueError, match='duplicate capability'):
        cc.CodecRef(id='org.example.payload', version='1', capabilities=('bytes', 'bytes'))


def test_codec_provider_resolution_happens_before_pickle_fallback():
    class Payload:
        def __init__(self, value: int):
            self.value = value

    payload_ref = cc.CodecRef(
        id='org.example.payload',
        version='1',
        schema='example.payload.v1',
        schema_sha256='b' * 64,
        capabilities=('bytes',),
    )

    @cc.transferable(codec_ref=payload_ref)
    class PayloadCodec:
        def serialize(value: Payload) -> bytes:
            return str(value.value).encode()

        def deserialize(data: bytes) -> Payload:
            return Payload(int(bytes(data)))

    def provider(typ: type, context: object):
        if typ is Payload:
            return PayloadCodec
        return None

    cc.use_codec(provider)

    @cc.crm(namespace='test.codec-provider', version='0.1.0')
    class PayloadContract:
        def echo(self, value: Payload) -> Payload:
            ...

    from c_two.crm.descriptor import build_contract_descriptor

    descriptor = build_contract_descriptor(PayloadContract)
    method = descriptor['methods'][0]

    assert method['wire']['input'] == payload_ref.to_wire_ref()
    assert method['wire']['output'] == payload_ref.to_wire_ref()
    assert method['parameters'][0]['annotation'] == {
        'codec': payload_ref.to_wire_ref(),
        'kind': 'codec',
    }
    assert method['return'] == {
        'codec': payload_ref.to_wire_ref(),
        'kind': 'codec',
    }
    assert getattr(PayloadContract.echo, '_input_transferable') is PayloadCodec
    assert getattr(PayloadContract.echo, '_output_transferable') is PayloadCodec


def test_explicit_codec_binding_wins_over_provider_candidate():
    class Payload:
        def __init__(self, value: int):
            self.value = value

    provider_ref = cc.CodecRef(id='org.example.provider', version='1', schema_sha256='c' * 64)
    bound_ref = cc.CodecRef(id='org.example.bound', version='1', schema_sha256='d' * 64)

    @cc.transferable(codec_ref=provider_ref)
    class ProviderCodec:
        def serialize(value: Payload) -> bytes:
            return str(value.value).encode()

        def deserialize(data: bytes) -> Payload:
            return Payload(int(bytes(data)))

    @cc.transferable(codec_ref=bound_ref)
    class BoundCodec:
        def serialize(value: Payload) -> bytes:
            return str(value.value).encode()

        def deserialize(data: bytes) -> Payload:
            return Payload(int(bytes(data)))

    def provider(typ: type, context: object):
        if typ is Payload:
            return ProviderCodec
        return None

    cc.use_codec(provider)
    cc.bind_codec(Payload, BoundCodec)

    @cc.crm(namespace='test.codec-provider', version='0.1.0')
    class PayloadContract:
        def echo(self, value: Payload) -> Payload:
            ...

    from c_two.crm.descriptor import build_contract_descriptor

    descriptor = build_contract_descriptor(PayloadContract)
    method = descriptor['methods'][0]

    assert method['wire']['input'] == bound_ref.to_wire_ref()
    assert method['wire']['output'] == bound_ref.to_wire_ref()
    assert getattr(PayloadContract.echo, '_input_transferable') is BoundCodec
    assert getattr(PayloadContract.echo, '_output_transferable') is BoundCodec


def test_one_argument_codec_provider_is_supported():
    class Payload:
        def __init__(self, value: int):
            self.value = value

    payload_ref = cc.CodecRef(id='org.example.one-arg', version='1', schema_sha256='f' * 64)

    @cc.transferable(codec_ref=payload_ref)
    class PayloadCodec:
        def serialize(value: Payload) -> bytes:
            return str(value.value).encode()

        def deserialize(data: bytes) -> Payload:
            return Payload(int(bytes(data)))

    def provider(typ: type):
        if typ is Payload:
            return PayloadCodec
        return None

    cc.use_codec(provider)

    @cc.crm(namespace='test.codec-provider', version='0.1.0')
    class PayloadContract:
        def echo(self, value: Payload) -> Payload:
            ...

    assert getattr(PayloadContract.echo, '_input_transferable') is PayloadCodec
    assert getattr(PayloadContract.echo, '_output_transferable') is PayloadCodec


def test_provider_codec_binding_supplies_ref_for_plain_transferable_adapter():
    from c_two.crm.transferable import Transferable

    class Payload:
        def __init__(self, value: int):
            self.value = value

    binding_ref = cc.CodecRef(id='org.example.binding', version='1', schema_sha256='e' * 64)

    class PayloadAdapter(Transferable):
        def serialize(value: Payload) -> bytes:
            return str(value.value).encode()

        def deserialize(data: bytes) -> Payload:
            return Payload(int(bytes(data)))

    def provider(typ: type, context: object):
        if typ is Payload:
            return cc.CodecBinding(PayloadAdapter, binding_ref)
        return None

    cc.use_codec(provider)

    @cc.crm(namespace='test.codec-provider', version='0.1.0')
    class PayloadContract:
        def echo(self, value: Payload) -> Payload:
            ...

    from c_two.crm.descriptor import build_contract_descriptor

    descriptor = build_contract_descriptor(PayloadContract)
    method = descriptor['methods'][0]

    assert method['wire']['input'] == binding_ref.to_wire_ref()
    assert method['wire']['output'] == binding_ref.to_wire_ref()
    assert method['parameters'][0]['annotation']['codec'] == binding_ref.to_wire_ref()
    assert method['return']['codec'] == binding_ref.to_wire_ref()


def test_portable_descriptor_rejects_pickle_only_wire_refs():
    @cc.crm(namespace='test.codec-provider', version='0.1.0')
    class PickleOnlyContract:
        def echo(self, value: bytes) -> bytes:
            ...

    from c_two.crm.descriptor import build_contract_descriptor

    nonportable = build_contract_descriptor(PickleOnlyContract)
    assert nonportable['methods'][0]['wire']['input']['family'] == 'python-pickle-default'

    with pytest.raises(ValueError, match='portable contract.*python-pickle-default'):
        build_contract_descriptor(PickleOnlyContract, portable=True)
