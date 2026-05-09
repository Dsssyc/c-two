from importlib.metadata import version
__version__ = version('c-two')

from . import error
from .config import BaseIPCOverrides, ClientIPCOverrides, ServerIPCOverrides
from .crm.meta import crm, read, write, on_shutdown
from .crm.transferable import transferable
from .crm.transferable import transfer, hold, HeldResult
from .transport.server.scheduler import ConcurrencyConfig, ConcurrencyMode
from .transport.registry import (
    set_transport_policy,
    set_server,
    set_client,
    set_relay_anchor,
    register,
    connect,
    close,
    unregister,
    server_address,
    server_id,
    shutdown,
    serve,
    hold_stats,
)

__all__ = [
    '__version__',
    'error',
    'BaseIPCOverrides',
    'ClientIPCOverrides',
    'ServerIPCOverrides',
    'crm',
    'read',
    'write',
    'on_shutdown',
    'transferable',
    'transfer',
    'hold',
    'HeldResult',
    'ConcurrencyConfig',
    'ConcurrencyMode',
    'set_transport_policy',
    'set_server',
    'set_client',
    'set_relay_anchor',
    'register',
    'connect',
    'close',
    'unregister',
    'server_address',
    'server_id',
    'shutdown',
    'serve',
    'hold_stats',
]
