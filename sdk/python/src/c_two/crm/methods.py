from __future__ import annotations

import inspect

from .meta import get_shutdown_method


def rpc_method_names(crm_class: type) -> list[str]:
    """Return the public RPC method names for a CRM contract class."""
    shutdown_method = get_shutdown_method(crm_class)
    names = [
        name
        for name, _ in inspect.getmembers(crm_class, predicate=inspect.isfunction)
        if not name.startswith('_') and name != shutdown_method
    ]
    return sorted(names)
