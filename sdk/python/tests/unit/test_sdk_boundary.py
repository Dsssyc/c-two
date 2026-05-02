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
