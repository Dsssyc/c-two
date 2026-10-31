"""Typed shapes of the existing native runtime observations."""
from typing import Literal, TypedDict


class RouteCloseOutcome(TypedDict):
    route_name: str
    local_removed: bool
    active_drained: bool
    closed_reason: str
    close_error: str | None


class RelayCleanupError(TypedDict):
    route_name: str
    status_code: int | None
    message: str


class ClientCloseDetail(TypedDict):
    address: str
    reason: str


class ShutdownOutcome(TypedDict):
    removed_routes: list[str]
    route_outcomes: list[RouteCloseOutcome]
    relay_errors: list[RelayCleanupError]
    server_was_started: bool
    ipc_clients_drained: bool
    http_clients_drained: bool
    route_close_error: str | None
    runtime_barrier_error: str | None
    ipc_client_close_error: str | None
    ipc_clients_abnormally_closed: int
    ipc_client_close_details: list[ClientCloseDetail]
    completed: bool


class CallExecutionCounters(TypedDict):
    max_operations: int | None
    max_retained_bytes: int | None
    used_operations: int
    used_retained_bytes: int
    peak_operations: int
    peak_retained_bytes: int
    rejected_reservations: int
    closed: bool


class RetiredCallExecutionSnapshot(CallExecutionCounters):
    initialized: bool
    state: Literal['retired']


class CallExecutionSnapshot(CallExecutionCounters):
    retired: list[RetiredCallExecutionSnapshot]


class LifecyclePolicy(TypedDict):
    policy: Literal['persistent', 'owner_bound']
    owner_bound: bool
    owner_missing_grace_seconds: float | None


class ClientHeldLeases(TypedDict):
    response_shm_bytes: int
    response_file_bytes: int
    reassembly_bytes: int


class LifecycleSnapshot(TypedDict):
    policy: LifecyclePolicy
    phase: Literal[
        'persistent', 'armed', 'owner_missing', 'draining', 'finished', 'watcher_io_error',
    ]
    watcher_error: str | None
    terminal: bool
    listener_closed: bool
    work_drained: bool
    client_held_leases: ClientHeldLeases
