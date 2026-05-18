from __future__ import annotations

import inspect
import json
import math
import types
from typing import Any, ForwardRef, Union, get_args, get_origin, get_type_hints

from .contract import crm_contract_identity
from .codec import CodecRef, codec_ref_for_transferable, resolve_codec
from .meta import MethodAccess, get_method_access
from .methods import rpc_method_names
from .transferable import DEFAULT_PICKLE_PROTOCOL, Transferable

_DESCRIPTOR_SCHEMA = 'c-two.python.crm.descriptor.v2'
_PORTABLE_CONTRACT_SCHEMA = 'c-two.contract.v1'
_ABI_SCHEMA = 'c-two.python.crm.abi.v2'
_SIGNATURE_SCHEMA = 'c-two.python.crm.signature.v2'
_PICKLE_DEFAULT_REF = {
    'family': 'python-pickle-default',
    'kind': 'builtin',
    'python_min': '3.10',
    'portable': False,
    'version': f'pickle-protocol-{DEFAULT_PICKLE_PROTOCOL}',
}
_PRIMITIVES = {
    bool: 'bool',
    int: 'int',
    float: 'float',
    str: 'str',
    bytes: 'bytes',
    memoryview: 'memoryview',
    bytearray: 'bytearray',
}
_BARE_CONTAINERS = {list, dict, tuple, set, frozenset}


def build_contract_descriptor(
    crm_class: type,
    methods: list[str] | None = None,
    *,
    portable: bool = False,
) -> dict[str, Any]:
    crm_ns, crm_name, crm_ver = crm_contract_identity(crm_class)
    method_names = rpc_method_names(crm_class) if methods is None else list(methods)
    return {
        'crm': {
            'name': crm_name,
            'namespace': crm_ns,
            'version': crm_ver,
        },
        'methods': [
            _method_descriptor(crm_class, method_name, portable=portable)
            for method_name in method_names
        ],
        'schema': _DESCRIPTOR_SCHEMA,
    }


def build_contract_fingerprints(
    crm_class: type,
    methods: list[str] | None = None,
) -> tuple[str, str]:
    descriptor = build_contract_descriptor(crm_class, methods)
    abi_descriptor = {
        'crm': descriptor['crm'],
        'methods': [
            {
                'buffer': method['buffer'],
                'input': method['wire']['input'],
                'name': method['name'],
                'output': method['wire']['output'],
            }
            for method in descriptor['methods']
        ],
        'schema': _ABI_SCHEMA,
    }
    signature_descriptor = {
        'crm': descriptor['crm'],
        'methods': [
            {
                'access': method['access'],
                'buffer': method['buffer'],
                'name': method['name'],
                'parameters': method['parameters'],
                'return': method['return'],
                'transfer': method['transfer'],
            }
            for method in descriptor['methods']
        ],
        'schema': _SIGNATURE_SCHEMA,
    }
    return (_hash_descriptor(abi_descriptor), _hash_descriptor(signature_descriptor))


def build_portable_contract_descriptor(
    crm_class: type,
    methods: list[str] | None = None,
) -> dict[str, Any]:
    descriptor = build_contract_descriptor(crm_class, methods, portable=True)
    return {
        'crm': descriptor['crm'],
        'methods': [
            {
                'access': method['access'],
                'buffer': method['buffer'],
                'name': method['name'],
                'parameters': [
                    {
                        'default': param['default'],
                        'kind': param['kind'],
                        'name': param['name'],
                        'type': param['annotation'],
                    }
                    for param in method['parameters']
                ],
                'return': method['return'],
                'wire': method['wire'],
            }
            for method in descriptor['methods']
        ],
        'schema': _PORTABLE_CONTRACT_SCHEMA,
    }


def export_contract_descriptor(
    crm_class: type,
    methods: list[str] | None = None,
    *,
    pretty: bool = False,
) -> str:
    descriptor = build_portable_contract_descriptor(crm_class, methods)
    compact = json.dumps(descriptor, sort_keys=True, separators=(',', ':'))
    from c_two._native import validate_portable_contract_descriptor

    validate_portable_contract_descriptor(compact.encode())
    if pretty:
        return json.dumps(descriptor, sort_keys=True, indent=2) + '\n'
    return compact


def _hash_descriptor(descriptor: dict[str, Any]) -> str:
    from c_two._native import contract_descriptor_sha256_hex

    payload = json.dumps(
        descriptor,
        sort_keys=True,
        separators=(',', ':'),
    ).encode()
    return contract_descriptor_sha256_hex(payload)


def _method_descriptor(
    crm_class: type,
    method_name: str,
    *,
    portable: bool,
) -> dict[str, Any]:
    method = getattr(crm_class, method_name)
    signature_target = inspect.unwrap(method)
    sig = inspect.signature(signature_target)
    type_hints = _resolved_type_hints(signature_target, method_name)
    transfer = getattr(method, '__cc_transfer__', None) or {}
    codec_context = getattr(method, '_codec_context', None) or {}
    input_transferable = getattr(method, '_input_transferable', None)
    output_transferable = getattr(method, '_output_transferable', None)
    buffer_mode = getattr(method, '_input_buffer_mode', transfer.get('buffer', 'view'))

    signature_params = [
        param
        for index, param in enumerate(sig.parameters.values())
        if not (index == 0 and param.name in {'self', 'cls'})
    ]
    input_annotation_ref = _codec_ref_for_annotation(input_transferable)
    input_annotation_transferable = input_transferable
    if len(signature_params) != 1:
        input_annotation_ref = None
        input_annotation_transferable = None

    params = []
    for param in signature_params:
        if param.kind in {
            inspect.Parameter.VAR_POSITIONAL,
            inspect.Parameter.VAR_KEYWORD,
        }:
            raise TypeError(
                f'{method_name} uses varargs parameter {param.name!r}; '
                'CRM RPC methods must have explicit parameters.',
            )
        if param.name not in type_hints:
            raise TypeError(
                f'{method_name}.{param.name} is missing a type annotation.',
            )
        annotation = type_hints[param.name]
        params.append({
            'annotation': _annotation_descriptor(
                annotation,
                method_name,
                codec_ref=input_annotation_ref,
                codec_context=codec_context,
                portable=portable,
                transferable_cls=input_annotation_transferable,
            ),
            'default': _default_descriptor(param.default, method_name, param.name),
            'kind': param.kind.name,
            'name': param.name,
        })

    if 'return' not in type_hints:
        raise TypeError(f'{method_name} is missing a return annotation.')
    return_annotation = type_hints['return']

    access = get_method_access(method)
    input_wire = _wire_ref_for_input(input_transferable, params)
    output_wire = _wire_ref_for_output(output_transferable, return_annotation)
    if portable:
        _ensure_portable_wire_ref(input_wire, method_name, 'input')
        _ensure_portable_wire_ref(output_wire, method_name, 'output')
    return {
        'access': 'read' if access is MethodAccess.READ else 'write',
        'buffer': buffer_mode,
        'name': method_name,
        'parameters': params,
        'return': _annotation_descriptor(
            return_annotation,
            method_name,
            codec_ref=_codec_ref_for_annotation(output_transferable),
            codec_context=codec_context,
            portable=portable,
            transferable_cls=output_transferable,
        ),
        'transfer': {
            'input': _transfer_annotation_descriptor(input_transferable),
            'output': _transfer_annotation_descriptor(output_transferable),
        },
        'wire': {
            'input': input_wire,
            'output': output_wire,
        },
    }


def _resolved_type_hints(func: object, method_name: str) -> dict[str, Any]:
    try:
        return get_type_hints(func, include_extras=True)
    except NameError as exc:
        raise ValueError(
            f'{method_name} contains an unresolved forward reference: {exc}',
        ) from exc
    except TypeError as exc:
        raise TypeError(
            f'{method_name} contains an unsupported annotation: {exc}',
        ) from exc


def _annotation_descriptor(
    annotation: Any,
    method_name: str,
    *,
    codec_ref: CodecRef | None = None,
    codec_context: dict[str, Any] | None = None,
    portable: bool = False,
    transferable_cls: type | None = None,
) -> dict[str, Any]:
    if annotation is inspect.Signature.empty:
        raise TypeError(f'{method_name} contains a missing annotation.')
    if annotation is Any:
        raise TypeError(f'{method_name} uses Any, which is not a stable RPC ABI.')
    if isinstance(annotation, (str, ForwardRef)):
        raise ValueError(
            f'{method_name} contains an unresolved forward reference.',
        )
    if annotation is None or annotation is type(None):
        return {'kind': 'none'}
    if annotation in _PRIMITIVES:
        return {'kind': 'primitive', 'name': _PRIMITIVES[annotation]}
    if annotation in _BARE_CONTAINERS:
        raise TypeError(f'{method_name} uses a bare container annotation.')
    origin = get_origin(annotation)
    args = get_args(annotation)
    if origin in {Union, types.UnionType}:
        items = [
            _annotation_descriptor(arg, method_name, codec_context=codec_context, portable=portable)
            for arg in args
        ]
        items.sort(key=_canonical_json)
        return {'items': items, 'kind': 'union'}
    if origin is list:
        if not args:
            raise TypeError(f'{method_name} uses a bare container annotation.')
        item_codec_ref = _item_codec_ref_for_transferable(transferable_cls)
        return {
            'item': _annotation_descriptor(
                args[0],
                method_name,
                codec_ref=item_codec_ref,
                codec_context=codec_context,
                portable=portable,
            ),
            'kind': 'list',
        }
    if origin is dict:
        if len(args) != 2:
            raise TypeError(f'{method_name} uses a bare container annotation.')
        return {
            'key': _annotation_descriptor(
                args[0],
                method_name,
                codec_context=codec_context,
                portable=portable,
            ),
            'kind': 'dict',
            'value': _annotation_descriptor(
                args[1],
                method_name,
                codec_context=codec_context,
                portable=portable,
            ),
        }
    if origin is tuple:
        if not args:
            raise TypeError(f'{method_name} uses a bare container annotation.')
        if len(args) == 2 and args[1] is Ellipsis:
            return {
                'item': _annotation_descriptor(
                    args[0],
                    method_name,
                    codec_context=codec_context,
                    portable=portable,
                ),
                'kind': 'tuple_variadic',
            }
        return {
            'items': [
                _annotation_descriptor(arg, method_name, codec_context=codec_context, portable=portable)
                for arg in args
            ],
            'kind': 'tuple',
        }

    if codec_ref is not None:
        return {
            'codec': codec_ref.to_wire_ref(),
            'kind': 'codec',
        }
    if _is_transferable_type(annotation):
        return {
            'abi_ref': _transferable_abi_ref(annotation),
            'kind': 'transferable',
        }
    resolution_context = dict(codec_context or {})
    resolution_context['method'] = method_name
    resolution_context['method_name'] = method_name
    resolution_context['position'] = 'annotation'
    binding = resolve_codec(annotation, resolution_context)
    if binding is not None:
        return {
            'codec': binding.codec_ref.to_wire_ref(),
            'kind': 'codec',
        }
    if not portable and isinstance(annotation, type):
        return _python_type_descriptor(annotation)

    raise TypeError(
        f'{method_name} contains unsupported annotation {annotation!r}.',
    )


def _python_type_descriptor(annotation: type) -> dict[str, str]:
    module = getattr(annotation, '__module__', None)
    name = getattr(annotation, '__name__', None)
    if not isinstance(module, str) or not module:
        module = '<unknown>'
    if not isinstance(name, str) or not name:
        name = '<anonymous>'
    return {
        'kind': 'python_type',
        'module': module,
        'name': name,
    }


def _item_codec_ref_for_transferable(transferable_cls: type | None) -> CodecRef | None:
    if transferable_cls is None:
        return None
    ref = getattr(transferable_cls, '_c2_item_codec_ref', None)
    if ref is None:
        return None
    if isinstance(ref, CodecRef):
        return ref
    return None


def _default_descriptor(value: object, method_name: str, param_name: str) -> dict[str, Any]:
    if value is inspect.Parameter.empty:
        return {'kind': 'missing'}
    if value is None:
        return {'kind': 'json_scalar', 'value': None}
    if isinstance(value, bool):
        return {'kind': 'json_scalar', 'value': value}
    if isinstance(value, int) and not isinstance(value, bool):
        return {'kind': 'json_scalar', 'value': value}
    if isinstance(value, float):
        if not math.isfinite(value):
            raise ValueError(
                f'{method_name}.{param_name} has a non-finite default value.',
            )
        return {'kind': 'json_scalar', 'value': value}
    if isinstance(value, str):
        return {'kind': 'json_scalar', 'value': value}
    raise TypeError(
        f'{method_name}.{param_name} has unsupported default value '
        f'{value!r}; only JSON scalar defaults are allowed.',
    )


def _wire_ref_for_input(
    input_transferable: type | None,
    params: list[dict[str, Any]],
) -> dict[str, Any] | None:
    if input_transferable is not None:
        return _transferable_abi_ref(input_transferable)
    if not params:
        return None
    return dict(_PICKLE_DEFAULT_REF)


def _wire_ref_for_output(
    output_transferable: type | None,
    return_annotation: Any,
) -> dict[str, Any] | None:
    if output_transferable is not None:
        return _transferable_abi_ref(output_transferable)
    if return_annotation is None or return_annotation is type(None):
        return None
    return dict(_PICKLE_DEFAULT_REF)


def _transfer_annotation_descriptor(transferable_cls: type | None) -> dict[str, Any] | None:
    if transferable_cls is None:
        return None
    return _transferable_abi_ref(transferable_cls)


def _codec_ref_for_annotation(transferable_cls: type | None) -> CodecRef | None:
    if transferable_cls is None or _is_default_transferable(transferable_cls):
        return None
    if not _is_transferable_type(transferable_cls):
        return None
    return codec_ref_for_transferable(transferable_cls)


def _transferable_abi_ref(transferable_cls: type) -> dict[str, Any]:
    if _is_default_transferable(transferable_cls):
        return dict(_PICKLE_DEFAULT_REF)
    if not _is_transferable_type(transferable_cls):
        raise TypeError(
            f'{transferable_cls!r} is not a Transferable subclass.',
        )
    codec_ref = codec_ref_for_transferable(transferable_cls)
    if codec_ref is not None:
        return codec_ref.to_wire_ref()

    abi = getattr(transferable_cls, '__cc_transferable_abi__', None) or {}
    abi_id = abi.get('abi_id')
    abi_schema = abi.get('abi_schema')
    if abi_id:
        return {'kind': 'custom_id', 'value': abi_id}
    if abi_schema:
        return {
            'kind': 'custom_schema',
            'sha256': _hash_descriptor({'abi_schema': abi_schema}),
        }
    if _has_custom_transfer_hooks(transferable_cls):
        raise ValueError(
            f'{transferable_cls.__name__} defines custom transfer hooks but '
            'does not declare an explicit ABI.',
        )
    raise ValueError(
        f'{transferable_cls.__name__} appears in a remote signature, but a '
        'dataclass-field ABI is not defined yet.',
    )


def _is_default_transferable(transferable_cls: type) -> bool:
    return (
        getattr(transferable_cls, '__module__', None) == 'Default'
        and hasattr(transferable_cls, '_original_func')
    )


def _is_transferable_type(value: object) -> bool:
    return (
        isinstance(value, type)
        and issubclass(value, Transferable)
        and value is not Transferable
    )


def _has_custom_transfer_hooks(transferable_cls: type) -> bool:
    return any(
        name in getattr(transferable_cls, '__dict__', {})
        for name in ('serialize', 'deserialize', 'from_buffer')
    )


def _ensure_portable_wire_ref(
    wire_ref: dict[str, Any] | None,
    method_name: str,
    position: str,
) -> None:
    if wire_ref is None:
        return
    if wire_ref.get('family') == 'python-pickle-default' or wire_ref.get('portable') is False:
        raise ValueError(
            f'{method_name} cannot be exported as a portable contract: '
            f'{position} uses python-pickle-default.',
        )


def _canonical_json(value: object) -> str:
    return json.dumps(value, sort_keys=True, separators=(',', ':'))
