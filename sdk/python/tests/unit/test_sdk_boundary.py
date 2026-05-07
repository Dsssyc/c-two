"""Source-level guards for the Python SDK responsibility boundary."""
from __future__ import annotations

import ast
from pathlib import Path


def test_registry_does_not_own_relay_control_plane_mechanisms():
    """Relay control-plane HTTP/retry/cache behavior belongs in Rust core."""
    source_path = (
        Path(__file__).resolve().parents[2]
        / "src"
        / "c_two"
        / "transport"
        / "registry.py"
    )
    tree = ast.parse(source_path.read_text())

    forbidden_imports: list[str] = []
    forbidden_calls: list[str] = []
    forbidden_classes: list[str] = []
    forbidden_names: list[str] = []
    forbidden_route_fields: list[str] = []
    forbidden_pool_names: list[str] = []

    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                if alias.name in {"urllib.request", "urllib.error"}:
                    forbidden_imports.append(alias.name)
        elif isinstance(node, ast.ImportFrom):
            if node.module in {"urllib.request", "urllib.error"}:
                forbidden_imports.append(node.module)
        elif isinstance(node, ast.ClassDef):
            if node.name == "_RouteCache":
                forbidden_classes.append(node.name)
        elif isinstance(node, ast.FunctionDef):
            if node.name == "_is_local_relay_url":
                forbidden_names.append(node.name)
        elif isinstance(node, ast.Name):
            if node.id in {"RustHttpClientPool", "_http_pool"}:
                forbidden_pool_names.append(node.id)
        elif isinstance(node, ast.Attribute):
            if node.attr == "_http_pool":
                forbidden_pool_names.append(node.attr)
        elif isinstance(node, ast.Call):
            func = node.func
            if isinstance(func, ast.Attribute):
                full_name = _attribute_name(func)
                if full_name in {
                    "urllib.request.urlopen",
                    "urllib.request.Request",
                    "time.sleep",
                }:
                    forbidden_calls.append(full_name)
                if full_name == "route.get" and _first_literal_arg(node) == "ipc_address":
                    forbidden_route_fields.append("route.get('ipc_address')")
            elif isinstance(func, ast.Name) and func.id == "dict":
                continue
        elif isinstance(node, ast.Subscript):
            if (
                isinstance(node.value, ast.Name)
                and node.value.id == "route"
                and _literal_slice(node) == "ipc_address"
            ):
                forbidden_route_fields.append("route['ipc_address']")

    assert forbidden_imports == []
    assert forbidden_classes == []
    assert forbidden_names == []
    assert forbidden_calls == []
    assert forbidden_route_fields == []
    assert forbidden_pool_names == []


def _attribute_name(node: ast.Attribute) -> str:
    parts: list[str] = [node.attr]
    value = node.value
    while isinstance(value, ast.Attribute):
        parts.append(value.attr)
        value = value.value
    if isinstance(value, ast.Name):
        parts.append(value.id)
    return ".".join(reversed(parts))


def _first_literal_arg(node: ast.Call) -> object | None:
    if not node.args:
        return None
    arg = node.args[0]
    if isinstance(arg, ast.Constant):
        return arg.value
    return None


def _literal_slice(node: ast.Subscript) -> object | None:
    if isinstance(node.slice, ast.Constant):
        return node.slice.value
    return None


def test_import_does_not_expose_logo_banner():
    import c_two

    assert not hasattr(c_two, "LOGO" + "_UNICODE")


def test_error_facade_does_not_reimplement_wire_codec():
    source_path = Path(__file__).resolve().parents[2] / "src" / "c_two" / "error.py"
    source = source_path.read_text(encoding="utf-8")

    legacy = "legacy"
    forbidden = [
        ".tobytes()",
        ".decode('utf-8')",
        '.decode("utf-8")',
        ".split(':', 1)",
        '.split(":", 1)',
        "int(code_raw)",
        "Unknown error code {code_value}",
        "invalid UTF-8",
        "missing ':' separator",
        "invalid code",
        f"encode_error_{legacy}",
        f"decode_error_{legacy}",
        f"to_{legacy}_bytes",
        f"from_{legacy}_bytes",
    ]

    offenders = [needle for needle in forbidden if needle in source]
    assert offenders == []
    assert "_native.error_registry" in source
    assert "_native.encode_error_wire" in source
    assert "_native.decode_error_wire_parts" in source


def test_python_does_not_own_buffer_lease_accounting():
    root = Path(__file__).resolve().parents[2] / "src" / "c_two"
    offenders = []
    forbidden = [
        "class " + "Hold" + "Registry",
        "weakref." + "ref(request_buf",
        "_hold" + "_registry",
        "_entr" + "ies",
        "total_held_bytes " + "+=",
    ]
    for path in root.rglob("*.py"):
        text = path.read_text(encoding="utf-8")
        for needle in forbidden:
            if needle in text:
                offenders.append(f"{path.relative_to(root)}:{needle}")
    assert offenders == []


def test_python_server_bridge_does_not_own_readiness_polling():
    import inspect
    from c_two.transport.server.native import NativeServerBridge

    source = inspect.getsource(NativeServerBridge)
    forbidden = [
        "os.path.exists",
        "while not os.path",
        "self._started",
        "_started =",
    ]
    offenders = [needle for needle in forbidden if needle in source]
    assert offenders == []

    start_source = inspect.getsource(NativeServerBridge.start)
    assert "start_and_wait" in start_source

    shutdown_source = inspect.getsource(NativeServerBridge.shutdown)
    assert "if self.is_started()" not in shutdown_source


def test_runtime_session_does_not_infer_started_from_socket_file():
    from pathlib import Path

    root = Path(__file__).resolve().parents[4]
    session_rs = root / "core" / "runtime" / "c2-runtime" / "src" / "session.rs"
    source = session_rs.read_text(encoding="utf-8")
    assert "socket_path().exists()" not in source


def test_python_server_dispatcher_does_not_own_response_allocation():
    import inspect
    from c_two.transport.server.native import NativeServerBridge

    source = inspect.getsource(NativeServerBridge._make_dispatcher)
    forbidden = [
        "response_pool",
        "len(res_part) >",
        "write_from_buffer",
        "bytes(res_part)",
        "seg_idx",
        "is_dedicated",
    ]
    offenders = [needle for needle in forbidden if needle in source]
    assert offenders == []


def test_native_server_response_parser_does_not_accept_shm_coordinate_tuples():
    root = Path(__file__).resolve().parents[4]
    server_ffi = root / "sdk" / "python" / "native" / "src" / "server_ffi.rs"
    source = server_ffi.read_text(encoding="utf-8")
    start = source.index("fn parse_response_meta")
    end = source.index("// ---------------------------------------------------------------------------", start)
    parser_source = source[start:end]

    forbidden = [
        "PyTuple",
        "seg_idx: int",
        "offset: int",
        "data_size: int",
        "is_dedicated: bool",
        "seg_idx, offset, data_size",
    ]
    offenders = [needle for needle in forbidden if needle in parser_source]
    assert offenders == []
