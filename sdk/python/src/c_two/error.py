from __future__ import annotations
from enum import IntEnum, unique

from c_two import _native

_NATIVE_TO_PY_ERROR_NAMES = {
    "Unknown": "ERROR_UNKNOWN",
    "ResourceInputDeserializing": "ERROR_AT_RESOURCE_INPUT_DESERIALIZING",
    "ResourceOutputSerializing": "ERROR_AT_RESOURCE_OUTPUT_SERIALIZING",
    "ResourceFunctionExecuting": "ERROR_AT_RESOURCE_FUNCTION_EXECUTING",
    "ResourceInputFromBuffer": "ERROR_AT_RESOURCE_INPUT_FROM_BUFFER",
    "ClientInputSerializing": "ERROR_AT_CLIENT_INPUT_SERIALIZING",
    "ClientOutputDeserializing": "ERROR_AT_CLIENT_OUTPUT_DESERIALIZING",
    "ClientCallingResource": "ERROR_AT_CLIENT_CALLING_RESOURCE",
    "ClientOutputFromBuffer": "ERROR_AT_CLIENT_OUTPUT_FROM_BUFFER",
    "ResourceNotFound": "ERROR_RESOURCE_NOT_FOUND",
    "ResourceUnavailable": "ERROR_RESOURCE_UNAVAILABLE",
    "ResourceAlreadyRegistered": "ERROR_RESOURCE_ALREADY_REGISTERED",
    "StaleResource": "ERROR_STALE_RESOURCE",
    "RegistryUnavailable": "ERROR_REGISTRY_UNAVAILABLE",
    "WriteConflict": "ERROR_WRITE_CONFLICT",
}


def _load_error_code_members() -> dict[str, int]:
    registry = _native.error_registry()
    missing = sorted(set(_NATIVE_TO_PY_ERROR_NAMES) - set(registry))
    extra = sorted(set(registry) - set(_NATIVE_TO_PY_ERROR_NAMES))
    if missing or extra:
        raise RuntimeError(
            "Rust error registry does not match Python facade mapping "
            f"(missing={missing}, extra={extra})"
        )
    return {
        py_name: int(registry[native_name])
        for native_name, py_name in _NATIVE_TO_PY_ERROR_NAMES.items()
    }


ERROR_Code = unique(IntEnum("ERROR_Code", _load_error_code_members()))

class CCBaseError(Exception):
    """Base class for all C-Two-related errors."""

class CCError(CCBaseError):
    """
    General error class for C-Two.
    
    Parameters:
        code (ERROR_Code): The error code representing the type of error.
        message (str | None): Optional custom error message. Defaults to a generic message.
    """
    
    code: ERROR_Code
    message: str | None

    def __init__(self, code: ERROR_Code = ERROR_Code.ERROR_UNKNOWN, message: str | None = None):
        self.code = code
        self.message = message or 'Error occurred when using C-Two.'

    def __str__(self):
        return f'{self.code.name}: {self.message}'
    
    def __repr__(self):
        return f'CCError(code={self.code}, message={self.message})'

    @staticmethod
    def serialize(err: 'CCError' | None) -> bytes:
        """
        Serialize the error to canonical C-Two error wire bytes.
        """
        if err is None:
            return b''

        message = err.message or 'Error occurred when using C-Two.'
        return _native.encode_error_wire(int(err.code), message)

    @staticmethod
    def deserialize(data) -> 'CCError' | None:
        """
        Deserialize canonical C-Two error wire bytes to a Python error object.
        """
        try:
            decoded = _native.decode_error_wire_parts(data)
        except ValueError as exc:
            return CCError(
                code=ERROR_Code.ERROR_UNKNOWN,
                message=f'Malformed error payload: {exc}',
            )

        if decoded is None:
            return None

        code_value, message = decoded
        try:
            code = ERROR_Code(code_value)
        except ValueError:
            code = ERROR_Code.ERROR_UNKNOWN
        subclass = _CODE_TO_CLASS.get(code, CCError)
        obj = Exception.__new__(subclass)
        obj.code = code
        obj.message = message or 'Error occurred when using C-Two.'
        return obj

class ResourceDeserializeInput(CCError):
    def __init__(self, message: str | None = None):
        message = 'Error occurred when deserializing input at resource' + (f':\n{message}' if message else '')
        super().__init__(code=ERROR_Code.ERROR_AT_RESOURCE_INPUT_DESERIALIZING, message=message)

class ResourceInputFromBuffer(CCError):
    def __init__(self, message: str | None = None):
        message = 'Error occurred when constructing resource input from buffer' + (f':\n{message}' if message else '')
        super().__init__(code=ERROR_Code.ERROR_AT_RESOURCE_INPUT_FROM_BUFFER, message=message)

class ResourceSerializeOutput(CCError):
    def __init__(self, message: str | None = None):
        message = 'Error occurred when serializing output at resource' + (f':\n{message}' if message else '')
        super().__init__(code=ERROR_Code.ERROR_AT_RESOURCE_OUTPUT_SERIALIZING, message=message)

class ResourceExecuteFunction(CCError):
    def __init__(self, message: str | None = None):
        message = 'Error occurred when executing function at resource' + (f':\n{message}' if message else '')
        super().__init__(code=ERROR_Code.ERROR_AT_RESOURCE_FUNCTION_EXECUTING, message=message)

class ClientSerializeInput(CCError):
    def __init__(self, message: str | None = None):
        message = 'Error occurred when serializing input at client' + (f':\n{message}' if message else '')
        super().__init__(code=ERROR_Code.ERROR_AT_CLIENT_INPUT_SERIALIZING, message=message)

class ClientDeserializeOutput(CCError):
    def __init__(self, message: str | None = None):
        message = 'Error occurred when deserializing output at client' + (f':\n{message}' if message else '')
        super().__init__(code=ERROR_Code.ERROR_AT_CLIENT_OUTPUT_DESERIALIZING, message=message)

class ClientOutputFromBuffer(CCError):
    def __init__(self, message: str | None = None):
        message = 'Error occurred when constructing client output from buffer' + (f':\n{message}' if message else '')
        super().__init__(code=ERROR_Code.ERROR_AT_CLIENT_OUTPUT_FROM_BUFFER, message=message)

class ClientCallResource(CCError):
    def __init__(self, message: str | None = None):
        message = 'Error occurred when calling resource from client' + (f':\n{message}' if message else '')
        super().__init__(code=ERROR_Code.ERROR_AT_CLIENT_CALLING_RESOURCE, message=message)

class ResourceNotFound(CCError):
    """Raised when a named resource cannot be resolved by any relay."""
    ERROR_CODE = 701

    def __init__(self, message: str | None = None):
        super().__init__(code=ERROR_Code.ERROR_RESOURCE_NOT_FOUND, message=message or 'Resource not found')

class ResourceUnavailable(CCError):
    """Raised when a resource exists but is not reachable."""
    ERROR_CODE = 702

    def __init__(self, message: str | None = None, detail: str | None = None):
        msg = message or 'Resource unavailable'
        if detail:
            msg = f'{msg}: {detail}'
        super().__init__(code=ERROR_Code.ERROR_RESOURCE_UNAVAILABLE, message=msg)

class ResourceAlreadyRegistered(CCError):
    """Raised when a relay rejects duplicate registration for a resource name."""
    ERROR_CODE = 703

    def __init__(self, message: str | None = None):
        super().__init__(
            code=ERROR_Code.ERROR_RESOURCE_ALREADY_REGISTERED,
            message=message or 'Resource already registered',
        )

class StaleResource(CCError):
    """Raised when a stale resource cannot serve a read under the active policy."""
    ERROR_CODE = 704

    def __init__(self, message: str | None = None):
        super().__init__(code=ERROR_Code.ERROR_STALE_RESOURCE, message=message or 'Stale resource')

class RegistryUnavailable(CCError):
    """Raised when no relay is available for name resolution."""
    ERROR_CODE = 705

    def __init__(self, message: str | None = None):
        super().__init__(code=ERROR_Code.ERROR_REGISTRY_UNAVAILABLE, message=message or 'Registry unavailable')

class WriteConflict(CCError):
    """Raised when a write cannot acquire the required resource coordination."""
    ERROR_CODE = 706

    def __init__(self, message: str | None = None):
        super().__init__(code=ERROR_Code.ERROR_WRITE_CONFLICT, message=message or 'Write conflict')

_CODE_TO_CLASS: dict[int, type] = {
    ERROR_Code.ERROR_AT_RESOURCE_INPUT_DESERIALIZING: ResourceDeserializeInput,
    ERROR_Code.ERROR_AT_RESOURCE_INPUT_FROM_BUFFER:    ResourceInputFromBuffer,
    ERROR_Code.ERROR_AT_RESOURCE_OUTPUT_SERIALIZING:  ResourceSerializeOutput,
    ERROR_Code.ERROR_AT_RESOURCE_FUNCTION_EXECUTING:  ResourceExecuteFunction,
    ERROR_Code.ERROR_AT_CLIENT_INPUT_SERIALIZING:     ClientSerializeInput,
    ERROR_Code.ERROR_AT_CLIENT_OUTPUT_DESERIALIZING:  ClientDeserializeOutput,
    ERROR_Code.ERROR_AT_CLIENT_OUTPUT_FROM_BUFFER:    ClientOutputFromBuffer,
    ERROR_Code.ERROR_AT_CLIENT_CALLING_RESOURCE:      ClientCallResource,
    ERROR_Code.ERROR_RESOURCE_NOT_FOUND:               ResourceNotFound,
    ERROR_Code.ERROR_RESOURCE_UNAVAILABLE:             ResourceUnavailable,
    ERROR_Code.ERROR_RESOURCE_ALREADY_REGISTERED:      ResourceAlreadyRegistered,
    ERROR_Code.ERROR_STALE_RESOURCE:                   StaleResource,
    ERROR_Code.ERROR_REGISTRY_UNAVAILABLE:             RegistryUnavailable,
    ERROR_Code.ERROR_WRITE_CONFLICT:                   WriteConflict,
}
