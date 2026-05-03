"""Process-level CRM registry — the SOTA ``cc.register / cc.connect`` API.

Provides a singleton :class:`_ProcessRegistry` that manages:

1. **CRM registration** — registers CRM objects to a process-level
   :class:`Server`, making them accessible via IPC.
2. **Thread preference** — ``connect()`` returns a zero-serialization
   :class:`CRMProxy.thread_local` when the target CRM lives in the
   same process.
3. **Client pooling** — remote connections reuse Rust IPC/HTTP clients
   via :class:`RustClientPool` and :class:`RustHttpClientPool`.

Usage::

    import c_two as cc

    # Register CRMs with explicit names
    cc.register(Grid, grid_instance, name='grid')

    # Connect (same process → thread-local, no serialization)
    grid = cc.connect(Grid, name='grid')
    result = grid.subdivide_grids([1], [0])

    # Close the connection
    cc.close(grid)

    # Unregister when done
    cc.unregister('grid')

Module-level functions (:func:`register`, :func:`connect`, etc.) delegate
to the global :class:`_ProcessRegistry` singleton.
"""
from __future__ import annotations

import atexit
import logging
import os
import signal
import sys
import threading
import uuid
from typing import TypeVar

from c_two.config.ipc import (
    ClientIPCOverrides,
    ServerIPCOverrides,
    _normalize_client_ipc_overrides,
    _normalize_server_ipc_overrides,
    _resolve_client_ipc_config,
)
from c_two.config.settings import settings
from c_two.error import (
    ResourceAlreadyRegistered,
    ResourceNotFound,
    ResourceUnavailable,
    RegistryUnavailable,
)
from .client.proxy import CRMProxy
from .server.scheduler import ConcurrencyConfig
from .server.native import NativeServerBridge as Server

CRM = TypeVar('CRM')
log = logging.getLogger(__name__)


def _relay_control_error_status(exc: BaseException) -> int | None:
    value = getattr(exc, "status_code", None)
    return value if isinstance(value, int) else None


class _ProcessRegistry:
    """Process-level singleton managing CRM registration and discovery.

    Normally accessed via the module-level :func:`register` /
    :func:`connect` / :func:`unregister` functions.
    """

    _instance: _ProcessRegistry | None = None
    _instance_lock = threading.Lock()

    @classmethod
    def get(cls) -> _ProcessRegistry:
        """Return the global singleton, creating it on first access."""
        if cls._instance is None:
            with cls._instance_lock:
                if cls._instance is None:
                    cls._instance = cls()
        return cls._instance

    @classmethod
    def reset(cls) -> None:
        """Destroy the global singleton (for testing / process exit)."""
        with cls._instance_lock:
            inst = cls._instance
            cls._instance = None
        if inst is not None:
            inst.shutdown()

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._server: Server | None = None
        self._server_address: str | None = None
        self._server_id: str | None = None
        self._server_ipc_overrides: ServerIPCOverrides | None = None
        self._server_id_override: str | None = None
        self._client_config: dict[str, object] | None = None
        self._client_ipc_overrides: ClientIPCOverrides | None = None
        self._pool_config_applied: bool = False
        self._relay_control_client: object | None = None
        self._relay_control_address: str | None = None
        from c_two._native import RustClientPool, RustHttpClientPool
        self._pool = RustClientPool.instance()
        self._http_pool = RustHttpClientPool.instance()

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def set_relay(self, address: str) -> None:
        """Set the relay address for name resolution.

        Convenience wrapper — equivalent to setting ``C2_RELAY_ADDRESS``.
        """
        previous = settings.relay_address
        previous_addr = previous.rstrip('/') if previous else None
        relay_addr = address.rstrip('/')
        settings.relay_address = address
        if previous_addr != relay_addr:
            self._clear_relay_control_client()

    def set_transport_policy(self, *, shm_threshold: int | None = None) -> None:
        """Set process transport policy. Must be called before use."""
        with self._lock:
            if self._server is not None or self._pool_config_applied:
                import warnings
                warnings.warn(
                    'Active connections exist, set_transport_policy() ignored. '
                    'Call set_transport_policy() before register()/connect().',
                    UserWarning,
                    stacklevel=3,
                )
                return
            settings.shm_threshold = shm_threshold
            self._client_config = None

    def set_server(
        self,
        *,
        server_id: str | None = None,
        ipc_overrides: ServerIPCOverrides | None = None,
    ) -> None:
        """Configure IPC server. Must be called before register()."""
        with self._lock:
            if self._server is not None:
                import warnings
                warnings.warn(
                    'Server already started, set_server() ignored. '
                    'Call set_server() before register().',
                    UserWarning,
                    stacklevel=3,
                )
                return
            self._server_id_override = self._clean_server_id(server_id)
            self._server_ipc_overrides = _normalize_server_ipc_overrides(ipc_overrides)

    def set_client(self, *, ipc_overrides: ClientIPCOverrides | None = None) -> None:
        """Configure IPC client. Must be called before connect()."""
        with self._lock:
            if self._pool_config_applied:
                import warnings
                warnings.warn(
                    'Client connections already exist, set_client() ignored. '
                    'Call set_client() before connect().',
                    UserWarning,
                    stacklevel=3,
                )
                return
            self._client_ipc_overrides = _normalize_client_ipc_overrides(ipc_overrides)
            self._client_config = None  # rebuilt lazily

    def register(
        self,
        crm_class: type,
        crm_instance: object,
        *,
        name: str,
        concurrency: ConcurrencyConfig | None = None,
    ) -> str:
        """Register a CRM, making it available via :func:`connect`.

        On the first call, a :class:`Server` is created and started
        automatically (lazy init).

        If ``C2_RELAY_ADDRESS`` is set, the CRM is also registered with
        the relay server through the Rust relay control client. A
        connection failure or non-2xx response raises an error.

        Parameters
        ----------
        crm_class:
            ``@cc.crm``-decorated interface (contract) class.
        crm_instance:
            Concrete CRM object that implements *crm_class*.
        name:
            Unique routing name for this CRM instance.
        concurrency:
            Optional concurrency configuration for the scheduler.

        Returns
        -------
        str
            The *name* string (echoed back for convenience).
        """
        with self._lock:
            # Lazy-init server on first registration.
            if self._server is None:
                server_id = self._server_id_override or self._auto_server_id()
                addr = self._address_for_server_id(server_id)
                self._server = Server(
                    bind_address=addr,
                    ipc_overrides=self._server_ipc_overrides,
                )
                self._server_id = server_id
                self._server_address = addr
                # Rust pool uses its own defaults; skip Python IPCConfig.

            self._server.register_crm(crm_class, crm_instance, concurrency, name=name)

            # Start server if not yet running.
            if not self._server.is_started():
                self._server.start()

            server_address = self._server_address
            server_id = self._server_id

        log.debug('Registered CRM %s at %s', name, server_address)

        # Notify relay (outside lock to avoid deadlocks).
        crm_ns = getattr(crm_class, '__cc_namespace__', '')
        crm_ver = getattr(crm_class, '__cc_version__', '')
        try:
            self._relay_register(name, server_id, server_address, crm_ns, crm_ver)
        except Exception:
            self._rollback_registration(name)
            raise

        return name

    def connect(
        self,
        crm_class: type[CRM],
        *,
        name: str,
        address: str | None = None,
    ) -> CRM:
        """Obtain a CRM instance connected to a registered resource.

        **Thread preference**: when the target *name* is registered in
        this process and no explicit *address* is given, returns a
        zero-serialization local proxy.

        Parameters
        ----------
        crm_class:
            ``@cc.crm``-decorated interface (contract) class.
        name:
            Routing name of the target CRM.
        address:
            Explicit IPC server address (e.g. ``'ipc://remote'``).
            If ``None``, looks up the local registry.

        Returns
        -------
        object
            A CRM instance with ``.client`` set to an
            :class:`CRMProxy`.
        """
        with self._lock:
            server = self._server
            local = (
                server.get_local_slot_info(name)
                if address is None and server is not None
                else None
            )

        if address is None and local is not None:
            # Thread preference — same process, no serialization.
            crm_instance, scheduler, access_map = local
            proxy = CRMProxy.thread_local(
                crm_instance,
                scheduler=scheduler,
                access_map=access_map,
            )
        elif address is not None and address.startswith(('http://', 'https://')):
            # HTTP mode — cross-node via relay server.
            client = self._http_pool.acquire(address)
            proxy = CRMProxy.http(
                client,
                name,
                on_terminate=lambda addr=address: self._http_pool.release(addr),
            )
        elif address is not None:
            # Remote IPC via pooled RustClient.
            self._ensure_pool_config()
            client = self._pool.acquire(address)
            proxy = CRMProxy.ipc(
                client,
                name,
                on_terminate=lambda addr=address: self._pool.release(addr),
            )
        else:
            relay_addr = settings.relay_address
            if not relay_addr:
                raise LookupError(
                    f'Name {name!r} is not registered locally '
                    f'and no address was provided',
                )
            from c_two._native import RustRelayAwareHttpClient

            try:
                client = RustRelayAwareHttpClient(relay_addr, name)
            except Exception as exc:
                status = _relay_control_error_status(exc)
                if status == 404:
                    raise ResourceNotFound(f"Resource '{name}' not found") from exc
                if status is not None:
                    raise ResourceUnavailable(f"Relay error: {status}") from exc
                raise RegistryUnavailable(f"Relay unavailable: {exc}") from exc
            proxy = CRMProxy.http(
                client,
                name,
                on_terminate=lambda: client.close(),
            )

        crm = crm_class()
        crm.client = proxy
        return crm

    def close(self, crm: object) -> None:
        """Close a connection obtained from :func:`connect`.

        Terminates the underlying proxy and releases pool references.
        """
        proxy = getattr(crm, 'client', None)
        if proxy is not None and hasattr(proxy, 'terminate'):
            proxy.terminate()

    def unregister(self, name: str) -> None:
        """Remove a CRM from the registry and server.

        If ``C2_RELAY_ADDRESS`` is set, the CRM is also unregistered
        from the relay through the Rust relay control client.

        Parameters
        ----------
        name:
            The routing name used during :func:`register`.
        """
        with self._lock:
            server = self._server
            server_id = self._server_id
        if server is None:
            raise KeyError(f'Name not registered: {name!r}')

        server.unregister_crm(name)
        # Explicit unregister is part of the control plane. Local state is the
        # authority; relay cleanup is a remote discovery projection. Surface
        # relay failures so callers know that remote cleanup is uncertain.
        self._relay_unregister(name, server_id)

    def get_server_address(self) -> str | None:
        """IPC address of the auto-created server, or ``None``."""
        return self._server_address

    def get_server_id(self) -> str | None:
        """Server identity of the auto-created server, or ``None``."""
        return self._server_id

    @property
    def names(self) -> list[str]:
        """List of currently registered routing names."""
        with self._lock:
            server = self._server
        return server.names if server is not None else []

    def shutdown(self) -> None:
        """Full cleanup — shuts down server, terminates pooled clients.

        If ``C2_RELAY_ADDRESS`` is set, all registered CRMs are
        unregistered from the relay before shutting down.

        Called automatically at process exit via :func:`atexit`.
        """
        with self._lock:
            server = self._server
            names_to_unregister = list(server.names) if server is not None else []
            server_id = self._server_id
            self._server = None
            self._server_address = None
            self._server_id = None
            self._server_ipc_overrides = None
            self._server_id_override = None
            self._client_config = None
            self._client_ipc_overrides = None
            self._pool_config_applied = False

        self._clear_relay_control_client()

        # Best-effort relay unregistration (ignore failures during shutdown).
        for name in names_to_unregister:
            try:
                self._relay_unregister(name, server_id)
            except Exception:
                log.info(
                    'Relay unreachable during shutdown unregister of %s — '
                    'relay may have shut down already.', name,
                )

        if server is not None:
            try:
                server.shutdown()
            except Exception:
                log.warning('Error shutting down Server', exc_info=True)

        self._pool.shutdown_all()
        self._http_pool.shutdown_all()
    # ------------------------------------------------------------------
    # Serve (daemon mode)
    # ------------------------------------------------------------------

    _serve_stop: threading.Event | None = None

    def serve(self, blocking: bool = True) -> None:
        """Keep the process alive as a CRM resource server.

        Transitions the process from a computation script into a
        long-running resource service.  Installs signal handlers for
        ``SIGINT`` / ``SIGTERM`` and, when *blocking* is ``True``,
        blocks until a termination signal arrives, then calls
        :meth:`shutdown` for graceful cleanup.

        If no CRM has been registered yet, a warning is logged but
        the process still blocks (useful during development).

        Parameters
        ----------
        blocking:
            If ``True`` (default), block the calling thread until a
            termination signal is received.  If ``False``, install
            signal handlers and print the banner but return immediately
            — the caller is responsible for keeping the process alive.
        """
        if self._serve_stop is not None:
            return  # idempotent

        self._serve_stop = threading.Event()

        with self._lock:
            server = self._server
            names = list(server.names) if server is not None else []
            addr = self._server_address

        if not names:
            log.warning(
                'cc.serve() called with no CRM registered. '
                'The process will block but has nothing to serve.',
            )

        # Signal handlers — install BEFORE banner so that SIGINT is
        # handled correctly as soon as the caller sees the output.
        def _handle_signal(signum, frame):  # noqa: ARG001
            self._serve_stop.set()

        try:
            signal.signal(signal.SIGINT, _handle_signal)
            if sys.platform != 'win32':
                signal.signal(signal.SIGTERM, _handle_signal)
        except ValueError:
            # Not the main thread — signals cannot be registered.
            # Caller must arrange to call _serve_stop.set() externally.
            log.debug('cc.serve(): signal handlers skipped (not main thread)')

        # Print banner (after signal handlers are ready).
        self._print_serve_banner(names, addr)

        if not blocking:
            return

        try:
            self._serve_stop.wait()
        except KeyboardInterrupt:
            # On some platforms, SIGINT also raises KeyboardInterrupt
            # even when a custom handler is installed.
            pass

        # Graceful shutdown.
        print('\n Shutting down…')
        self.shutdown()
        self._serve_stop = None
        print(' Resource server stopped.')

    @staticmethod
    def _print_serve_banner(
        names: list[str],
        addr: str | None,
    ) -> None:
        """Print a human-readable startup banner to stdout."""
        lines = [' ── C-Two Resource Server ────────────────']
        if addr:
            lines.append(f'  IPC: {addr}')
        if names:
            lines.append('  CRM routes:')
            for n in names:
                lines.append(f'    • {n}')
        else:
            lines.append('  (no CRM routes registered)')
        lines.append(' ─────────────────────────────────────────')
        count = len(names)
        lines.append(f'  Serving {count} CRM resource(s). Ctrl+C to stop.')
        print('\n'.join(lines))

    # ------------------------------------------------------------------
    # Relay service discovery
    # ------------------------------------------------------------------

    def _relay_register(self, name: str, server_id: str, ipc_address: str,
                        crm_ns: str = '', crm_ver: str = ''):
        """Notify the relay about a new CRM registration."""
        relay_addr = settings.relay_address
        if not relay_addr:
            return  # No relay configured — standalone mode.
        try:
            self._relay_control_client_for(relay_addr).register(
                name,
                server_id,
                ipc_address,
                crm_ns,
                crm_ver,
            )
            log.info('Registered CRM %s with relay at %s', name, relay_addr)
        except Exception as exc:
            status = _relay_control_error_status(exc)
            if status == 409:
                raise ResourceAlreadyRegistered(
                    f'Route name already registered with relay: {name!r}',
                ) from exc
            if status is not None:
                raise RuntimeError(
                    f'Relay registration failed for {name!r}: HTTP {status}',
                ) from exc
            raise RuntimeError(
                f'Relay registration failed for {name!r}: relay unreachable',
            ) from exc

    def _relay_unregister(
        self,
        name: str,
        server_id: str | None,
    ) -> None:
        """Notify the relay about CRM unregistration."""
        relay_addr = settings.relay_address
        if not relay_addr:
            return
        if not server_id:
            raise RuntimeError(f'Cannot unregister {name!r} from relay without server_id')
        try:
            self._relay_control_client_for(relay_addr).unregister(name, server_id)
            log.info('Unregistered CRM %s from relay', name)
        except Exception as exc:
            status = _relay_control_error_status(exc)
            if status is not None:
                raise RuntimeError(
                    f'Relay unregistration failed for {name!r}: HTTP {status}',
                ) from exc
            raise

    def _relay_control_client_for(self, relay_addr: str):
        relay_addr = relay_addr.rstrip('/')
        with self._lock:
            if (
                self._relay_control_client is not None
                and self._relay_control_address == relay_addr
            ):
                return self._relay_control_client
            from c_two._native import RustRelayControlClient

            client = RustRelayControlClient(relay_addr)
            self._relay_control_client = client
            self._relay_control_address = relay_addr
            return client

    def _clear_relay_control_client(self) -> None:
        with self._lock:
            client = self._relay_control_client
            self._relay_control_client = None
            self._relay_control_address = None
        if client is not None and hasattr(client, "clear_cache"):
            client.clear_cache()

    # ------------------------------------------------------------------
    # Internals
    # ------------------------------------------------------------------

    def _build_client_config(self) -> dict[str, object]:
        if self._client_config is not None:
            return self._client_config
        self._client_config = _resolve_client_ipc_config(
            self._client_ipc_overrides,
            settings,
        )
        return self._client_config

    def _ensure_pool_config(self) -> None:
        """Apply pool default config exactly once (thread-safe)."""
        with self._lock:
            if self._pool_config_applied:
                return
            cfg = self._build_client_config()
            self._pool.set_default_config(
                shm_threshold=cfg['shm_threshold'],
                pool_enabled=cfg['pool_enabled'],
                pool_segment_size=cfg['pool_segment_size'],
                max_pool_segments=cfg['max_pool_segments'],
                reassembly_segment_size=cfg['reassembly_segment_size'],
                reassembly_max_segments=cfg['reassembly_max_segments'],
                max_total_chunks=cfg['max_total_chunks'],
                chunk_gc_interval=cfg['chunk_gc_interval'],
                chunk_threshold_ratio=cfg['chunk_threshold_ratio'],
                chunk_assembler_timeout=cfg['chunk_assembler_timeout'],
                max_reassembly_bytes=cfg['max_reassembly_bytes'],
                chunk_size=cfg['chunk_size'],
            )
            self._pool_config_applied = True

    def _rollback_registration(self, name: str) -> None:
        """Undo a local registration after relay registration fails."""
        with self._lock:
            server = self._server

        if server is None or name not in server.names:
            return

        try:
            server.unregister_crm(name)
        except Exception:
            log.warning('Failed to rollback local CRM %s', name, exc_info=True)
            return

        with self._lock:
            should_shutdown = self._server is server and not server.names

        if should_shutdown and server is not None:
            try:
                server.shutdown()
            except Exception:
                log.warning('Failed to shutdown server during rollback', exc_info=True)
            with self._lock:
                if self._server is server and not server.names:
                    self._server = None
                    self._server_address = None
                    self._server_id = None

    @staticmethod
    def _clean_server_id(server_id: str | None) -> str | None:
        if server_id is None:
            return None
        from c_two._native import validate_server_id

        validate_server_id(server_id)
        return server_id

    @staticmethod
    def _auto_server_id() -> str:
        return f'cc{os.getpid():x}{uuid.uuid4().hex}'

    @staticmethod
    def _address_for_server_id(server_id: str) -> str:
        return f'ipc://{server_id}'


# ------------------------------------------------------------------
# Module-level API (delegates to singleton)
# ------------------------------------------------------------------

def set_transport_policy(*, shm_threshold: int | None = None) -> None:
    """Set process transport policy. Call before register()/connect()."""
    _ProcessRegistry.get().set_transport_policy(shm_threshold=shm_threshold)


def set_server(
    *,
    server_id: str | None = None,
    ipc_overrides: ServerIPCOverrides | None = None,
) -> None:
    """Configure IPC server. Call before register()."""
    _ProcessRegistry.get().set_server(server_id=server_id, ipc_overrides=ipc_overrides)


def set_client(*, ipc_overrides: ClientIPCOverrides | None = None) -> None:
    """Configure IPC client. Call before connect()."""
    _ProcessRegistry.get().set_client(ipc_overrides=ipc_overrides)


def set_relay(address: str) -> None:
    """Set the relay address for name resolution."""
    _ProcessRegistry.get().set_relay(address)


def register(
    crm_class: type,
    crm_instance: object,
    *,
    name: str,
    concurrency: ConcurrencyConfig | None = None,
) -> str:
    """Register a CRM in the current process.

    See :meth:`_ProcessRegistry.register`.
    """
    return _ProcessRegistry.get().register(
        crm_class,
        crm_instance,
        name=name,
        concurrency=concurrency,
    )


def connect(
    crm_class: type[CRM],
    *,
    name: str,
    address: str | None = None,
) -> CRM:
    """Obtain a CRM proxy for a registered resource.

    See :meth:`_ProcessRegistry.connect`.
    """
    return _ProcessRegistry.get().connect(crm_class, name=name, address=address)


def close(crm: object) -> None:
    """Close a connection obtained from :func:`connect`.

    See :meth:`_ProcessRegistry.close`.
    """
    _ProcessRegistry.get().close(crm)


def unregister(name: str) -> None:
    """Remove a CRM from the registry.

    See :meth:`_ProcessRegistry.unregister`.
    """
    _ProcessRegistry.get().unregister(name)


def server_address() -> str | None:
    """IPC address of the auto-created server, or ``None``."""
    return _ProcessRegistry.get().get_server_address()


def server_id() -> str | None:
    """Server identity of the auto-created server, or ``None``."""
    return _ProcessRegistry.get().get_server_id()


def shutdown() -> None:
    """Full cleanup — shuts down server and pooled clients.

    See :meth:`_ProcessRegistry.shutdown`.
    """
    _ProcessRegistry.get().shutdown()


def serve(blocking: bool = True) -> None:
    """Keep the process alive as a CRM resource server.

    Transitions the calling process from a computation script into a
    long-running resource service.  Blocks until ``SIGINT`` or
    ``SIGTERM``, then calls :func:`shutdown` for graceful cleanup.

    See :meth:`_ProcessRegistry.serve`.
    """
    _ProcessRegistry.get().serve(blocking=blocking)


def hold_stats() -> dict:
    """Return hold-mode SHM tracking statistics.

    Returns dict with:
    - active_holds: number of currently held SHM buffers
    - total_held_bytes: total bytes pinned in SHM
    - oldest_hold_seconds: age of oldest active hold
    """
    inst = _ProcessRegistry._instance
    server = inst._server if inst else None
    if server is None:
        return {'active_holds': 0, 'total_held_bytes': 0, 'oldest_hold_seconds': 0}
    return server.hold_stats()


# Auto-cleanup on process exit.
atexit.register(lambda: _ProcessRegistry.reset())
