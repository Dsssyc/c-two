from __future__ import annotations

import dataclasses
import types
from dataclasses import dataclass
from typing import Any, ForwardRef, Union, get_args, get_origin, get_type_hints

from c_two.crm.codec import CodecBinding, CodecRef, use_codec
from c_two.crm.transferable import Transferable

try:
    import pyarrow as pa
except ModuleNotFoundError as exc:  # pragma: no cover - exercised only without optional dependency
    raise ModuleNotFoundError(
        'c_two.providers.arrow requires pyarrow. Install the c-two examples/dev '
        'dependencies or add pyarrow to your project environment.',
    ) from exc

ARROW_IPC_CODEC_ID = 'org.apache.arrow.ipc'
ARROW_IPC_CODEC_VERSION = '1'
ARROW_IPC_SCHEMA = 'arrow.ipc.schema.v1'
ARROW_IPC_MEDIA_TYPE = 'application/vnd.apache.arrow.stream'
ARROW_IPC_CAPABILITIES = ('bytes', 'buffer-view')

_RECORD_MARKER = '__c_two_arrow_record__'
_GENERATED_MODULE = 'c_two.providers.arrow.generated'


@dataclass(frozen=True)
class ArrowRecordOptions:
    schema_id: str


@dataclass(frozen=True)
class _ArrowFieldSpec:
    name: str
    arrow_type: Any
    type_text: str
    nullable: bool


@dataclass(frozen=True)
class _ArrowRecordSpec:
    record_type: type
    schema_id: str
    fields: tuple[_ArrowFieldSpec, ...]

    @property
    def schema_text(self) -> str:
        fields = ','.join(
            f'{field.name}:{field.type_text}{"?" if field.nullable else ""}'
            for field in self.fields
        )
        return f'{self.schema_id};fields={fields}'

    @property
    def batch_schema_text(self) -> str:
        fields = ','.join(
            f'{field.name}:{field.type_text}{"?" if field.nullable else ""}'
            for field in self.fields
        )
        return f'{self.schema_id}.batch;items={self.schema_id};fields={fields}'

    def arrow_schema(self) -> Any:
        return pa.schema([
            pa.field(field.name, field.arrow_type, nullable=field.nullable)
            for field in self.fields
        ])


def record(cls: type | None = None, *, schema_id: str | None = None):
    """Mark a dataclass-like record as an Arrow IPC payload type."""

    def wrap(target: type) -> type:
        if not isinstance(target, type):
            raise TypeError('@arrow.record must decorate a class.')
        if schema_id is not None and (not isinstance(schema_id, str) or not schema_id.strip()):
            raise ValueError('schema_id must be a non-empty string.')
        record_cls = target if dataclasses.is_dataclass(target) else dataclasses.dataclass(target)
        effective_schema_id = schema_id or f'{record_cls.__module__}.{record_cls.__qualname__}.arrow-ipc.v1'
        setattr(record_cls, _RECORD_MARKER, ArrowRecordOptions(schema_id=effective_schema_id))
        return record_cls

    if cls is not None:
        return wrap(cls)
    return wrap


class ArrowCodecProvider:
    """C-Two codec provider for dataclass records encoded as Arrow IPC streams."""

    def __init__(self) -> None:
        self._single_cache: dict[type, CodecBinding] = {}
        self._batch_cache: dict[type, CodecBinding] = {}

    def candidates_for_type(
        self,
        annotation: object,
        context: object | None = None,
    ) -> CodecBinding | None:
        if _is_arrow_record(annotation):
            return self._single_binding(annotation)
        origin = get_origin(annotation)
        args = get_args(annotation)
        if origin is list and len(args) == 1 and _is_arrow_record(args[0]):
            return self._batch_binding(args[0])
        return None

    def _single_binding(self, record_type: type) -> CodecBinding:
        cached = self._single_cache.get(record_type)
        if cached is not None:
            return cached
        spec = _record_spec(record_type)
        schema = spec.arrow_schema()
        codec_ref = _codec_ref_for_schema(spec.schema_text)

        def serialize(value: object) -> bytes:
            if not isinstance(value, record_type):
                raise TypeError(
                    f'expected {record_type.__name__}, got {type(value).__name__}',
                )
            table = pa.Table.from_pylist([_record_to_row(value)], schema=schema)
            return _table_to_ipc(table)

        def deserialize(data: memoryview | bytes) -> object:
            rows = _ipc_to_rows(data)
            if len(rows) != 1:
                raise ValueError(
                    f'expected one Arrow IPC row for {record_type.__name__}, got {len(rows)}',
                )
            return record_type(**rows[0])

        transferable = _make_transferable(
            f'Arrow{record_type.__name__}Transferable',
            codec_ref,
            serialize,
            deserialize,
        )
        binding = CodecBinding(transferable=transferable, codec_ref=codec_ref)
        self._single_cache[record_type] = binding
        return binding

    def _batch_binding(self, record_type: type) -> CodecBinding:
        cached = self._batch_cache.get(record_type)
        if cached is not None:
            return cached
        spec = _record_spec(record_type)
        schema = spec.arrow_schema()
        item_binding = self._single_binding(record_type)
        codec_ref = _codec_ref_for_schema(spec.batch_schema_text)

        def serialize(values: list[object]) -> bytes:
            if not isinstance(values, list):
                raise TypeError(
                    f'expected list[{record_type.__name__}], got {type(values).__name__}',
                )
            for value in values:
                if not isinstance(value, record_type):
                    raise TypeError(
                        f'expected list[{record_type.__name__}], got item '
                        f'{type(value).__name__}',
                    )
            table = pa.Table.from_pylist([_record_to_row(value) for value in values], schema=schema)
            return _table_to_ipc(table)

        def deserialize(data: memoryview | bytes) -> list[object]:
            return [
                record_type(**row)
                for row in _ipc_to_rows(data)
            ]

        transferable = _make_transferable(
            f'Arrow{record_type.__name__}BatchTransferable',
            codec_ref,
            serialize,
            deserialize,
            item_codec_ref=item_binding.codec_ref,
        )
        binding = CodecBinding(transferable=transferable, codec_ref=codec_ref)
        self._batch_cache[record_type] = binding
        return binding


_DEFAULT_PROVIDER = ArrowCodecProvider()


def use_arrow(provider: ArrowCodecProvider | None = None) -> ArrowCodecProvider:
    """Register the Arrow IPC provider with C-Two's codec registry."""
    selected = provider or _DEFAULT_PROVIDER
    use_codec(selected)
    return selected


def _make_transferable(
    class_name: str,
    codec_ref: CodecRef,
    serialize,
    deserialize,
    *,
    item_codec_ref: CodecRef | None = None,
) -> type:
    attrs: dict[str, object] = {
        '__module__': _GENERATED_MODULE,
        '__cc_codec_ref__': codec_ref,
        'serialize': serialize,
        'deserialize': deserialize,
    }
    if item_codec_ref is not None:
        attrs['_c2_item_codec_ref'] = item_codec_ref
    return type(class_name, (Transferable,), attrs)


def _codec_ref_for_schema(schema_text: str) -> CodecRef:
    return CodecRef.from_schema(
        id=ARROW_IPC_CODEC_ID,
        version=ARROW_IPC_CODEC_VERSION,
        schema=ARROW_IPC_SCHEMA,
        schema_text=schema_text,
        capabilities=ARROW_IPC_CAPABILITIES,
        media_type=ARROW_IPC_MEDIA_TYPE,
    )


def _is_arrow_record(value: object) -> bool:
    return isinstance(value, type) and hasattr(value, _RECORD_MARKER)


def _record_spec(record_type: type) -> _ArrowRecordSpec:
    if not dataclasses.is_dataclass(record_type):
        raise TypeError(f'{record_type.__name__} must be a dataclass.')
    options = getattr(record_type, _RECORD_MARKER, None)
    if not isinstance(options, ArrowRecordOptions):
        raise TypeError(f'{record_type.__name__} is not marked with @arrow.record.')
    try:
        type_hints = get_type_hints(record_type, include_extras=True)
    except (NameError, TypeError) as exc:
        raise TypeError(
            f'Failed to resolve Arrow record annotations for {record_type.__name__}: {exc}',
        ) from exc
    fields = []
    for field in dataclasses.fields(record_type):
        annotation = type_hints.get(field.name)
        if annotation is None:
            raise TypeError(
                f'Arrow record field {record_type.__name__}.{field.name} '
                'is missing a type annotation.',
            )
        arrow_type, type_text, nullable = _arrow_type_for_annotation(
            annotation,
            f'{record_type.__name__}.{field.name}',
        )
        fields.append(_ArrowFieldSpec(field.name, arrow_type, type_text, nullable))
    return _ArrowRecordSpec(
        record_type=record_type,
        schema_id=options.schema_id,
        fields=tuple(fields),
    )


def _arrow_type_for_annotation(annotation: object, path: str) -> tuple[object, str, bool]:
    if isinstance(annotation, (str, ForwardRef)):
        raise TypeError(f'Unsupported Arrow record annotation at {path}: unresolved reference.')
    origin = get_origin(annotation)
    args = get_args(annotation)
    if origin is dataclasses.InitVar:
        raise TypeError(f'Unsupported Arrow record annotation at {path}: InitVar.')
    if origin is Union or origin is types.UnionType:
        non_none = [arg for arg in args if arg is not type(None)]
        if len(non_none) == 1 and len(non_none) != len(args):
            arrow_type, type_text, _nullable = _arrow_type_for_annotation(non_none[0], path)
            return arrow_type, type_text, True
        raise TypeError(f'Unsupported Arrow record annotation at {path}: union.')
    if annotation is bool:
        return pa.bool_(), 'bool', False
    if annotation is int:
        return pa.int64(), 'int64', False
    if annotation is float:
        return pa.float64(), 'float64', False
    if annotation is str:
        return pa.string(), 'string', False
    if annotation is bytes:
        return pa.binary(), 'binary', False
    if origin is list:
        if len(args) != 1:
            raise TypeError(f'Unsupported Arrow record annotation at {path}: bare list.')
        item_type, item_text, item_nullable = _arrow_type_for_annotation(args[0], f'{path}[]')
        if item_nullable:
            item_field = pa.field('item', item_type, nullable=True)
            return pa.list_(item_field), f'list<{item_text}?>', False
        return pa.list_(item_type), f'list<{item_text}>', False
    name = getattr(annotation, '__name__', repr(annotation))
    raise TypeError(f'Unsupported Arrow record annotation at {path}: {name}.')


def _record_to_row(value: object) -> dict[str, object]:
    return {
        field.name: getattr(value, field.name)
        for field in dataclasses.fields(value)
    }


def _table_to_ipc(table: object) -> bytes:
    sink = pa.BufferOutputStream()
    with pa.ipc.new_stream(sink, table.schema) as writer:
        writer.write_table(table)
    return sink.getvalue().to_pybytes()


def _ipc_to_rows(data: memoryview | bytes) -> list[dict[str, object]]:
    buffer = pa.py_buffer(data)
    with pa.ipc.open_stream(buffer) as reader:
        table = reader.read_all()
    return table.to_pylist()


__all__ = [
    'ARROW_IPC_CAPABILITIES',
    'ARROW_IPC_CODEC_ID',
    'ARROW_IPC_CODEC_VERSION',
    'ARROW_IPC_MEDIA_TYPE',
    'ARROW_IPC_SCHEMA',
    'ArrowCodecProvider',
    'ArrowRecordOptions',
    'record',
    'use_arrow',
]
