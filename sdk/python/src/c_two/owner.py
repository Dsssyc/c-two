"""Owner supervision for launchers: control channels, recovery, takeover and owned children."""
from ._native import (
    OwnerBootstrap,
    OwnerControl, OwnerRecoveryTicket, PreparedOwnerTakeover, OwnerStatus,
    OwnerControlError, OwnedChild,
)
from .config.lifecycle import LifecycleConfig
from .transport.registry import owner_control_pair, prepare_owner_takeover, spawn_owned_child

__all__ = [
    'OwnerBootstrap', 'OwnerControl', 'OwnerRecoveryTicket', 'PreparedOwnerTakeover',
    'OwnerStatus', 'OwnerControlError', 'OwnedChild', 'owner_control_pair',
    'prepare_owner_takeover', 'spawn_owned_child', 'LifecycleConfig',
]
