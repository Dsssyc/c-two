from importlib.metadata import version
__version__ = version('c-two')

from . import error
from .crm.meta import crm, read, write, on_shutdown
from .crm.transferable import transferable
from .crm.transferable import transfer, hold, HeldResult
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
