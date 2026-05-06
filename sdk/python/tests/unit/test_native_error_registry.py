from c_two import _native


def test_native_error_registry_exposes_canonical_codes():
    registry = _native.error_registry()
    assert registry["Unknown"] == 0
    assert registry["ResourceNotFound"] == 701
    assert registry["ResourceInputFromBuffer"] == 4
    assert registry["ClientOutputFromBuffer"] == 8
    assert registry["ResourceUnavailable"] == 702
    assert registry["ResourceAlreadyRegistered"] == 703
    assert registry["StaleResource"] == 704
    assert registry["RegistryUnavailable"] == 705
    assert registry["WriteConflict"] == 706


def test_native_decode_error_wire_parts_known_code():
    decoded = _native.decode_error_wire_parts(memoryview(b"703:grid exists"))
    assert decoded == (703, "grid exists")


def test_native_decode_error_wire_parts_unknown_code_degrades():
    decoded = _native.decode_error_wire_parts(memoryview(b"9999:relay exploded"))
    assert decoded == (0, "Unknown error code 9999: relay exploded")


def test_native_decode_error_wire_parts_empty_bytes_returns_none():
    assert _native.decode_error_wire_parts(memoryview(b"")) is None


def test_native_decode_error_wire_parts_rejects_malformed_payloads():
    for payload in (b"abc:not a number", b"3", b"\xff"):
        try:
            _native.decode_error_wire_parts(memoryview(payload))
        except ValueError as exc:
            assert "C2 error wire" in str(exc)
        else:
            raise AssertionError(f"expected ValueError for {payload!r}")


def test_native_encode_error_wire_matches_canonical_wire_format():
    assert _native.encode_error_wire(703, "grid exists") == b"703:grid exists"


def test_native_error_ffi_does_not_export_legacy_codec_names():
    suffix = "legacy"
    assert not hasattr(_native, f"encode_error_{suffix}")
    assert not hasattr(_native, f"decode_error_{suffix}")
