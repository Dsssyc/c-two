//! UDS server — accept loop, per-connection frame handler, CRM dispatch.
//!
//! CRM method execution is delegated to [`CrmCallback`] implementations
//! supplied by language bindings or native runtime adapters.
//!
//! ## Lock conventions
//!
//! This module uses **two different RwLock types**:
//! - `tokio::sync::RwLock` — async lock for `Dispatcher` (must `.await`)
//! - `parking_lot::RwLock` — sync lock for `MemPool` (blocking, no `.await`)

use std::io::ErrorKind;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, RwLock, watch};
use tracing::{debug, info, warn};

/// Monotonic counter ensuring each `Server` instance gets a unique SHM prefix,
/// even when multiple servers are created within the same PID (e.g. benchmarks
/// or tests that reset the registry).
///
/// Uses 32-bit range (~4 billion unique prefixes).  Response pool prefix
/// format: `/cc3r{pid:08x}{gen:08x}` (21 chars); reassembly pool prefix
/// format: `/cc3s{pid:08x}{gen:08x}` (21 chars).  With `_b{idx:04x}`
/// suffix the max SHM name is 27 chars (within macOS 31-char limit).
static RESPONSE_POOL_GEN: AtomicU64 = AtomicU64::new(0);

use c2_error::{C2Error, ErrorCode};
use c2_mem::MemPool;
use c2_mem::config::PoolConfig;
use c2_wire::buddy::{
    BUDDY_PAYLOAD_SIZE, BuddyPayload, decode_buddy_payload, encode_buddy_payload,
};
use c2_wire::chunk::{REPLY_CHUNK_META_SIZE, decode_chunk_header, encode_reply_chunk_meta};
use c2_wire::control::{ReplyControl, decode_call_control, encode_reply_control};
use c2_wire::flags::{
    FLAG_BUDDY, FLAG_CHUNK_LAST, FLAG_CHUNKED, FLAG_HANDSHAKE, FLAG_REPLY_V2, FLAG_RESPONSE,
    FLAG_SIGNAL,
};
use c2_wire::frame::{self, decode_frame_body, encode_frame};
pub use c2_wire::handshake::ServerIdentity;
use c2_wire::handshake::{
    CAP_CALL_V2, CAP_CHUNKED, CAP_METHOD_IDX, MAX_METHODS, MAX_ROUTES, MethodEntry, RouteInfo,
    decode_handshake, encode_server_handshake, validate_name_len,
};
use c2_wire::msg_type::{DISCONNECT_ACK_BYTES, MsgType, PONG_BYTES, SHUTDOWN_ACK_BYTES};

use crate::config::ServerIpcConfig;
use crate::connection::Connection;
use crate::dispatcher::{
    CrmError, CrmRoute, Dispatcher, RequestData, ResponseMeta, cleanup_request,
};
use crate::heartbeat::run_heartbeat;
use crate::response::buddy_response_data_size;
use crate::scheduler::{Scheduler, SchedulerAcquireError};

const IPC_SOCK_DIR: &str = "/tmp/c_two_ipc";

fn error_wire(code: ErrorCode, message: impl Into<String>) -> Vec<u8> {
    C2Error::new(code, message).to_wire_bytes()
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors produced by the server.
#[derive(Debug)]
pub enum ServerError {
    Io(std::io::Error),
    Config(String),
    Protocol(String),
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::Config(msg) => write!(f, "config error: {msg}"),
            Self::Protocol(msg) => write!(f, "protocol error: {msg}"),
        }
    }
}

impl std::error::Error for ServerError {}

impl From<std::io::Error> for ServerError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Native lifecycle state for the IPC server accept loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerLifecycleState {
    Initialized,
    Starting,
    Ready,
    Stopping,
    Stopped,
    Failed(String),
}

impl ServerLifecycleState {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    pub fn is_running(&self) -> bool {
        matches!(self, Self::Starting | Self::Ready | Self::Stopping)
    }
}

/// The main IPC server.
///
/// Binds a UDS socket, accepts connections, and dispatches CRM calls through
/// the [`Dispatcher`].  Each connection runs in its own tokio task with a
/// dedicated heartbeat probe.
pub struct Server {
    identity: ServerIdentity,
    config: ServerIpcConfig,
    socket_path: PathBuf,
    /// **tokio async RwLock** — guards CRM dispatch table; requires `.read().await`
    /// / `.write().await`.  Do NOT confuse with `parking_lot::RwLock` below.
    dispatcher: RwLock<Dispatcher>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    lifecycle_tx: watch::Sender<ServerLifecycleState>,
    socket_bound: AtomicBool,
    conn_counter: AtomicU64,
    /// Sharded chunk reassembly lifecycle manager.
    chunk_registry: Arc<c2_wire::chunk::ChunkRegistry>,
    /// **parking_lot sync RwLock** — guards SHM memory pool; blocking `.read()`
    /// / `.write()` (no `.await`).  Safe to hold briefly inside tokio tasks.
    response_pool: Arc<parking_lot::RwLock<MemPool>>,
}

impl Server {
    /// Create a new server for the given IPC address.
    ///
    /// Address format: `ipc://region_id`
    /// → socket at `/tmp/c_two_ipc/region_id.sock`
    pub fn new(address: &str, config: ServerIpcConfig) -> Result<Self, ServerError> {
        let identity = ServerIdentity {
            server_id: server_id_from_ipc_address(address)?,
            server_instance_id: uuid::Uuid::new_v4().simple().to_string(),
        };
        Self::new_with_identity(address, config, identity)
    }

    /// Create a new server with an explicit identity.
    pub fn new_with_identity(
        address: &str,
        config: ServerIpcConfig,
        identity: ServerIdentity,
    ) -> Result<Self, ServerError> {
        config.validate().map_err(ServerError::Config)?;
        validate_server_identity(&identity)?;
        let socket_path = parse_socket_path(address)?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let reassembly_cfg = PoolConfig {
            segment_size: config.reassembly_segment_size as usize,
            min_block_size: 4096,
            max_segments: config.reassembly_max_segments as usize,
            max_dedicated_segments: 4,
            dedicated_crash_timeout_secs: 5.0,
            buddy_idle_decay_secs: config.pool_decay_seconds,
            spill_threshold: 0.8,
            spill_dir: PathBuf::from("/tmp/c_two_reassembly"),
        };
        let reassembly_pool = {
            let pid = std::process::id();
            let ra_gen = RESPONSE_POOL_GEN.fetch_add(1, Ordering::Relaxed) as u32;
            let prefix = format!("/cc3s{:08x}{:08x}", pid, ra_gen);
            MemPool::new_with_prefix(reassembly_cfg, prefix)
        };
        let chunk_config = c2_wire::chunk::ChunkConfig::from_base(&config);
        let chunk_registry = Arc::new(c2_wire::chunk::ChunkRegistry::new(
            Arc::new(parking_lot::RwLock::new(reassembly_pool)),
            chunk_config,
        ));
        let response_cfg = PoolConfig {
            segment_size: config.pool_segment_size as usize,
            min_block_size: 4096,
            max_segments: config.max_pool_segments as usize,
            max_dedicated_segments: 4,
            dedicated_crash_timeout_secs: 5.0,
            buddy_idle_decay_secs: config.pool_decay_seconds,
            spill_threshold: 0.8,
            spill_dir: PathBuf::from("/tmp/c_two_response_spill"),
        };
        let mut response_pool = {
            let pid = std::process::id();
            let generation = RESPONSE_POOL_GEN.fetch_add(1, Ordering::Relaxed);
            let prefix = format!("/cc3r{:08x}{:08x}", pid, generation as u32);
            MemPool::new_with_prefix(response_cfg, prefix)
        };
        response_pool
            .ensure_ready()
            .map_err(|e| ServerError::Config(format!("response pool init: {e}")))?;
        let (lifecycle_tx, _lifecycle_rx) = watch::channel(ServerLifecycleState::Initialized);
        Ok(Self {
            identity,
            config,
            socket_path,
            dispatcher: RwLock::new(Dispatcher::new()),
            shutdown_tx,
            shutdown_rx,
            lifecycle_tx,
            socket_bound: AtomicBool::new(false),
            conn_counter: AtomicU64::new(0),
            chunk_registry,
            response_pool: Arc::new(parking_lot::RwLock::new(response_pool)),
        })
    }

    /// Identity announced in server→client handshake ACKs.
    pub fn identity(&self) -> &ServerIdentity {
        &self.identity
    }

    /// Stable logical server identity.
    pub fn server_id(&self) -> &str {
        &self.identity.server_id
    }

    /// Per-server-incarnation identity.
    pub fn server_instance_id(&self) -> &str {
        &self.identity.server_instance_id
    }

    /// Register a CRM route with the dispatcher.
    pub async fn register_route(&self, route: CrmRoute) -> Result<(), ServerError> {
        validate_route_for_wire(&route)?;
        let mut dispatcher = self.dispatcher.write().await;
        if dispatcher.resolve(&route.name).is_some() {
            return Err(ServerError::Protocol(format!(
                "route already registered: {}",
                route.name
            )));
        }
        if dispatcher.len() >= MAX_ROUTES && dispatcher.resolve(&route.name).is_none() {
            return Err(ServerError::Protocol(format!(
                "route count exceeds wire limit: {} > {}",
                dispatcher.len() + 1,
                MAX_ROUTES
            )));
        }
        dispatcher.register(route);
        Ok(())
    }

    /// Remove a CRM route. Returns `true` if it existed.
    ///
    /// The route handle is marked closed under the same dispatcher write lock
    /// that removes the route, so local handle clones and remote route lookup
    /// cannot observe an open-but-unregistered window.
    pub async fn unregister_route(&self, name: &str) -> bool {
        let mut dispatcher = self.dispatcher.write().await;
        let removed = dispatcher.unregister(name);
        if let Some(route) = removed.as_ref() {
            route.scheduler.close();
            true
        } else {
            false
        }
    }

    /// Filesystem path of the bound UDS socket.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    fn remove_owned_socket_file(&self) {
        if self.socket_bound.swap(false, Ordering::AcqRel) {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }

    fn set_lifecycle_state(&self, state: ServerLifecycleState) {
        self.lifecycle_tx.send_replace(state);
    }

    pub fn lifecycle_state(&self) -> ServerLifecycleState {
        self.lifecycle_tx.borrow().clone()
    }

    pub fn is_ready(&self) -> bool {
        self.lifecycle_state().is_ready()
    }

    pub fn is_running(&self) -> bool {
        self.lifecycle_state().is_running()
    }

    /// Fence a new native start attempt.
    ///
    /// This resets the one-shot shutdown signal and moves stale terminal
    /// lifecycle states (`Stopped` / `Failed`) back to `Starting` before the
    /// async accept loop is spawned. Callers that start `run()` on a background
    /// runtime should invoke this synchronously before they begin waiting for
    /// readiness, otherwise a waiter can observe the previous attempt's
    /// terminal state before the spawned task is polled.
    pub fn begin_start_attempt(&self) -> Result<(), ServerError> {
        match self.lifecycle_state() {
            ServerLifecycleState::Initialized
            | ServerLifecycleState::Stopped
            | ServerLifecycleState::Failed(_) => {
                self.shutdown_tx.send_replace(false);
                self.set_lifecycle_state(ServerLifecycleState::Starting);
                Ok(())
            }
            state => Err(ServerError::Config(format!(
                "server cannot start while lifecycle state is {state:?}",
            ))),
        }
    }

    pub async fn wait_until_ready(&self, timeout: Duration) -> Result<(), ServerError> {
        let mut rx = self.lifecycle_tx.subscribe();
        let wait = async {
            loop {
                let state = rx.borrow().clone();
                match state {
                    ServerLifecycleState::Ready => return Ok(()),
                    ServerLifecycleState::Failed(message) => {
                        return Err(ServerError::Config(format!(
                            "server failed to start: {message}",
                        )));
                    }
                    ServerLifecycleState::Stopped => {
                        return Err(ServerError::Config(
                            "server stopped before becoming ready".to_string(),
                        ));
                    }
                    ServerLifecycleState::Initialized
                    | ServerLifecycleState::Starting
                    | ServerLifecycleState::Stopping => {}
                }
                rx.changed().await.map_err(|_| {
                    ServerError::Config("server readiness channel closed".to_string())
                })?;
            }
        };

        tokio::time::timeout(timeout, wait).await.map_err(|_| {
            ServerError::Config(format!(
                "server did not become ready within {:.3}s",
                timeout.as_secs_f64(),
            ))
        })?
    }

    pub async fn wait_until_stopped(&self, timeout: Duration) -> Result<(), ServerError> {
        let mut rx = self.lifecycle_tx.subscribe();
        let wait = async {
            loop {
                let state = rx.borrow().clone();
                match state {
                    ServerLifecycleState::Initialized
                    | ServerLifecycleState::Stopped
                    | ServerLifecycleState::Failed(_) => return Ok(()),
                    ServerLifecycleState::Starting
                    | ServerLifecycleState::Ready
                    | ServerLifecycleState::Stopping => {}
                }
                rx.changed().await.map_err(|_| {
                    ServerError::Config("server lifecycle channel closed".to_string())
                })?;
            }
        };

        tokio::time::timeout(timeout, wait).await.map_err(|_| {
            ServerError::Config(format!(
                "server did not stop within {:.3}s",
                timeout.as_secs_f64(),
            ))
        })?
    }

    /// Mark runtime-backed server work as stopped after its runtime is gone.
    ///
    /// This is a shutdown cleanup fence for runtime owners. It does not erase
    /// startup failure diagnostics, but it prevents a force-dropped runtime from
    /// leaving `Starting`, `Ready`, or `Stopping` as a stale non-terminal state.
    pub fn finalize_runtime_stopped(&self) {
        self.remove_owned_socket_file();
        match self.lifecycle_state() {
            ServerLifecycleState::Starting
            | ServerLifecycleState::Ready
            | ServerLifecycleState::Stopping => {
                self.set_lifecycle_state(ServerLifecycleState::Stopped);
            }
            ServerLifecycleState::Initialized
            | ServerLifecycleState::Stopped
            | ServerLifecycleState::Failed(_) => {}
        }
    }

    /// Get a shared reference to the response pool (for zero-copy dispatch).
    pub fn response_pool_arc(&self) -> Arc<parking_lot::RwLock<MemPool>> {
        Arc::clone(&self.response_pool)
    }

    /// Return the configured response SHM threshold.
    pub fn response_shm_threshold(&self) -> u64 {
        self.config.shm_threshold
    }

    /// Return the configured maximum logical response payload size.
    pub fn response_max_payload_size(&self) -> u64 {
        self.config.max_payload_size
    }

    /// Return true if a route is currently registered.
    pub async fn contains_route(&self, name: &str) -> bool {
        self.dispatcher.read().await.resolve(name).is_some()
    }

    /// Get the chunk registry (for FFI or external use).
    pub fn chunk_registry(&self) -> &Arc<c2_wire::chunk::ChunkRegistry> {
        &self.chunk_registry
    }

    /// Run the accept loop.  Blocks until [`shutdown`](Self::shutdown) is called.
    pub async fn run(self: &Arc<Self>) -> Result<(), ServerError> {
        if !matches!(self.lifecycle_state(), ServerLifecycleState::Starting) {
            self.begin_start_attempt()?;
        }

        let startup = async {
            std::fs::create_dir_all(IPC_SOCK_DIR)?;
            remove_stale_socket_file(&self.socket_path)?;
            let listener = UnixListener::bind(&self.socket_path)?;
            self.socket_bound.store(true, Ordering::Release);
            Ok::<UnixListener, ServerError>(listener)
        }
        .await;

        let listener = match startup {
            Ok(listener) => listener,
            Err(err) => {
                self.set_lifecycle_state(ServerLifecycleState::Failed(err.to_string()));
                return Err(err);
            }
        };

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(&self.socket_path, std::fs::Permissions::from_mode(0o600));
        }

        self.set_lifecycle_state(ServerLifecycleState::Ready);
        info!(path = %self.socket_path.display(), "server listening");

        // Spawn periodic GC sweep for expired chunk assemblies.
        let gc_registry = self.chunk_registry.clone();
        let gc_interval = self.chunk_registry.config().gc_interval;
        let mut gc_shutdown_rx = self.shutdown_rx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(gc_interval);
            interval.tick().await; // skip first immediate tick
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let stats = gc_registry.gc_sweep();
                        if stats.expired > 0 {
                            info!(expired = stats.expired, freed = stats.freed_bytes, "chunk GC sweep");
                        }
                    }
                    _ = gc_shutdown_rx.changed() => break,
                }
            }
        });

        let mut shutdown_rx = self.shutdown_rx.clone();
        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, _)) => {
                            let server = Arc::clone(self);
                            tokio::spawn(handle_connection(server, stream));
                        }
                        Err(e) => warn!("accept error: {e}"),
                    }
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!("server shutting down");
                        self.set_lifecycle_state(ServerLifecycleState::Stopping);
                        break;
                    }
                }
            }
        }

        self.remove_owned_socket_file();
        self.set_lifecycle_state(ServerLifecycleState::Stopped);
        Ok(())
    }

    /// Signal the server to stop and remove the socket file.
    pub fn shutdown(&self) {
        if self.is_running() {
            self.set_lifecycle_state(ServerLifecycleState::Stopping);
        }
        let _ = self.shutdown_tx.send(true);
        self.remove_owned_socket_file();
    }
}

fn validate_route_for_wire(route: &CrmRoute) -> Result<(), ServerError> {
    validate_name_len("route name", &route.name)
        .map_err(|e| ServerError::Protocol(e.to_string()))?;
    if route.method_names.len() > MAX_METHODS {
        return Err(ServerError::Protocol(format!(
            "method count exceeds wire limit: {} > {}",
            route.method_names.len(),
            MAX_METHODS
        )));
    }
    for method_name in &route.method_names {
        validate_name_len("method name", method_name)
            .map_err(|e| ServerError::Protocol(e.to_string()))?;
    }
    Ok(())
}

fn validate_server_identity(identity: &ServerIdentity) -> Result<(), ServerError> {
    c2_config::validate_server_id(&identity.server_id).map_err(ServerError::Config)?;
    validate_identity_wire_len("server_id", &identity.server_id).map_err(ServerError::Config)?;
    validate_identity_component("server_instance_id", &identity.server_instance_id)
        .map_err(ServerError::Config)
}

fn validate_identity_wire_len(label: &str, value: &str) -> Result<(), String> {
    let actual = value.len();
    if actual > c2_wire::handshake::MAX_HANDSHAKE_NAME_BYTES {
        return Err(format!(
            "{label} cannot exceed {} bytes",
            c2_wire::handshake::MAX_HANDSHAKE_NAME_BYTES
        ));
    }
    Ok(())
}

fn validate_identity_component(label: &str, value: &str) -> Result<(), String> {
    validate_identity_wire_len(label, value)?;
    if value.is_empty() {
        return Err(format!("{label} cannot be empty"));
    }
    if value.trim() != value {
        return Err(format!(
            "{label} cannot contain leading or trailing whitespace"
        ));
    }
    if value == "." || value == ".." || value.contains('/') || value.contains('\\') {
        return Err(format!("{label} cannot contain path separators"));
    }
    if !value.is_ascii() {
        return Err(format!("{label} must be ASCII"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{label} cannot contain control characters"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Address helpers
// ---------------------------------------------------------------------------

fn server_id_from_ipc_address(address: &str) -> Result<String, ServerError> {
    let region = address
        .strip_prefix("ipc://")
        .ok_or_else(|| ServerError::Config(format!("invalid IPC address: {address}")))?;
    validate_region_id(region).map_err(ServerError::Config)?;
    Ok(region.to_string())
}

fn parse_socket_path(address: &str) -> Result<PathBuf, ServerError> {
    let region = address
        .strip_prefix("ipc://")
        .ok_or_else(|| ServerError::Config(format!("invalid IPC address: {address}")))?;
    validate_region_id(region).map_err(ServerError::Config)?;
    Ok(PathBuf::from(IPC_SOCK_DIR).join(format!("{region}.sock")))
}

fn remove_stale_socket_file(socket_path: &Path) -> Result<(), ServerError> {
    if !socket_path.exists() {
        return Ok(());
    }

    match StdUnixStream::connect(socket_path) {
        Ok(_) => Err(ServerError::Config(format!(
            "IPC socket {} already has an active listener",
            socket_path.display(),
        ))),
        Err(err)
            if matches!(
                err.kind(),
                ErrorKind::ConnectionRefused | ErrorKind::NotFound
            ) =>
        {
            let _ = std::fs::remove_file(socket_path);
            Ok(())
        }
        Err(err) => Err(ServerError::Io(err)),
    }
}

fn validate_region_id(region: &str) -> Result<(), String> {
    c2_config::validate_ipc_region_id(region)
}

// ---------------------------------------------------------------------------
// Per-connection handler
// ---------------------------------------------------------------------------

enum SignalAction {
    Continue,
    Disconnect,
    Shutdown,
}

async fn handle_connection(server: Arc<Server>, stream: UnixStream) {
    let conn_id = server.conn_counter.fetch_add(1, Ordering::Relaxed);
    let conn = Arc::new(Connection::new(conn_id));

    let (mut reader, write_half) = stream.into_split();
    let writer = Arc::new(Mutex::new(write_half));

    // Start heartbeat task.
    let hb_handle = {
        let c = Arc::clone(&conn);
        let w = Arc::clone(&writer);
        let cfg = server.config.clone();
        tokio::spawn(async move { run_heartbeat(c, w, &cfg).await })
    };

    debug!(conn_id, "connection accepted");

    let max_frame = server.config.max_frame_size;

    loop {
        // 1. Read 4-byte total_len prefix.
        let mut len_buf = [0u8; 4];
        if reader.read_exact(&mut len_buf).await.is_err() {
            break; // EOF or broken pipe
        }
        let total_len = u32::from_le_bytes(len_buf);

        if total_len < 12 || (total_len as u64) > max_frame {
            warn!(conn_id, total_len, "invalid frame length");
            break;
        }

        // 2. Read body (request_id + flags + payload).
        let mut body = vec![0u8; total_len as usize];
        if reader.read_exact(&mut body).await.is_err() {
            break;
        }

        // 3. Decode header + payload.
        let (header, payload) = match decode_frame_body(&body, total_len) {
            Ok(v) => v,
            Err(e) => {
                warn!(conn_id, ?e, "frame decode error");
                break;
            }
        };

        conn.touch();
        let flags = header.flags;
        let request_id = header.request_id;

        // 4. Dispatch by frame type.
        if header.is_handshake() {
            if let Err(e) = handle_handshake(&server, &conn, payload, request_id, &writer).await {
                warn!(conn_id, %e, "handshake failed");
                break;
            }
        } else if header.is_signal() {
            match handle_signal(payload, request_id, &writer, &server).await {
                SignalAction::Continue => {}
                SignalAction::Disconnect | SignalAction::Shutdown => break,
            }
        } else if header.is_ctrl() {
            debug!(conn_id, "ctrl frame ignored");
        } else if header.is_call_v2() {
            if c2_wire::flags::is_chunked(flags) {
                let srv = Arc::clone(&server);
                let cn = Arc::clone(&conn);
                let wr = Arc::clone(&writer);
                let pl = payload.to_vec();
                tokio::spawn(async move {
                    dispatch_chunked_call(&srv, &cn, request_id, flags, &pl, &wr).await;
                });
                continue;
            }
            if c2_wire::flags::is_buddy(flags) {
                let srv = Arc::clone(&server);
                let cn = Arc::clone(&conn);
                let wr = Arc::clone(&writer);
                let pl = payload.to_vec();
                tokio::spawn(async move {
                    dispatch_buddy_call(&srv, &cn, request_id, &pl, &wr).await;
                });
                continue;
            }

            let srv = Arc::clone(&server);
            let cn = Arc::clone(&conn);
            let wr = Arc::clone(&writer);
            let pl = payload.to_vec();
            tokio::spawn(async move {
                dispatch_call(&srv, &cn, request_id, &pl, &wr).await;
            });
        } else {
            warn!(conn_id, flags, "unknown frame type");
        }
    }

    debug!(conn_id, "connection closing, draining in-flight");
    hb_handle.abort();
    conn.wait_idle().await;
    server.chunk_registry.cleanup_connection(conn_id);
    debug!(conn_id, "connection closed");
}

// ---------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------

async fn handle_handshake(
    server: &Server,
    conn: &Connection,
    payload: &[u8],
    request_id: u64,
    writer: &Arc<Mutex<OwnedWriteHalf>>,
) -> Result<(), ServerError> {
    let client_hs = decode_handshake(payload)
        .map_err(|e| ServerError::Protocol(format!("handshake decode: {e:?}")))?;

    // Store client SHM metadata for buddy frame resolution.
    conn.init_peer_shm(client_hs.prefix, client_hs.segments);
    conn.set_handshake_done(true);

    if client_hs.capability_flags & CAP_CHUNKED != 0 {
        conn.set_chunked_capable(true);
    }

    // Collect response pool segment info for handshake.
    let (server_segments, server_prefix) = {
        let pool = server.response_pool.read();
        let count = pool.segment_count();
        let mut segs = Vec::with_capacity(count);
        for i in 0..count {
            if let Some(name) = pool.segment_name(i) {
                if let Some(seg) = pool.segment(i) {
                    segs.push((name.to_string(), seg.allocator().data_size() as u32));
                }
            }
        }
        let prefix = pool.prefix().to_string();
        (segs, prefix)
    };

    let dispatcher = server.dispatcher.read().await;
    let routes: Vec<RouteInfo> = dispatcher
        .routes_snapshot()
        .iter()
        .map(|r| RouteInfo {
            name: r.name.clone(),
            methods: r
                .method_names
                .iter()
                .enumerate()
                .map(|(i, n)| MethodEntry {
                    name: n.clone(),
                    index: i as u16,
                })
                .collect(),
        })
        .collect();
    drop(dispatcher);

    let cap = CAP_CALL_V2 | CAP_METHOD_IDX | CAP_CHUNKED;
    let hs_bytes = encode_server_handshake(
        &server_segments,
        cap,
        &routes,
        &server_prefix,
        server.identity(),
    )
    .map_err(|e| ServerError::Protocol(e.to_string()))?;
    let frame = encode_frame(request_id, FLAG_HANDSHAKE | FLAG_RESPONSE, &hs_bytes);

    writer.lock().await.write_all(&frame).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Signal handling
// ---------------------------------------------------------------------------

async fn handle_signal(
    payload: &[u8],
    request_id: u64,
    writer: &Arc<Mutex<OwnedWriteHalf>>,
    server: &Server,
) -> SignalAction {
    let sig = match payload.first().and_then(|&b| MsgType::from_byte(b)) {
        Some(s) => s,
        None => return SignalAction::Continue,
    };

    let (reply, action) = match sig {
        MsgType::Ping => (&PONG_BYTES[..], SignalAction::Continue),
        MsgType::ShutdownClient => (&SHUTDOWN_ACK_BYTES[..], SignalAction::Shutdown),
        MsgType::Disconnect => (&DISCONNECT_ACK_BYTES[..], SignalAction::Disconnect),
        _ => return SignalAction::Continue,
    };

    let frame = encode_frame(request_id, FLAG_RESPONSE | FLAG_SIGNAL, reply);
    let _ = writer.lock().await.write_all(&frame).await;

    if matches!(action, SignalAction::Shutdown) {
        server.shutdown();
    }
    action
}

#[derive(Debug)]
enum RouteExecutionError {
    Acquire(SchedulerAcquireError),
    Crm(CrmError),
}

async fn execute_route_request<F>(
    scheduler: Scheduler,
    method_idx: u16,
    request: RequestData,
    f: F,
) -> Result<ResponseMeta, RouteExecutionError>
where
    F: FnOnce(RequestData) -> Result<ResponseMeta, CrmError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let guard = match scheduler.blocking_acquire(method_idx) {
            Ok(guard) => guard,
            Err(err) => {
                cleanup_request(request);
                return Err(RouteExecutionError::Acquire(err));
            }
        };
        let _guard = guard;
        f(request).map_err(RouteExecutionError::Crm)
    })
    .await
    .expect("route execution task panicked")
}

async fn send_route_execution_result(
    server: &Server,
    writer: &Arc<Mutex<OwnedWriteHalf>>,
    request_id: u64,
    result: Result<ResponseMeta, RouteExecutionError>,
) {
    match result {
        Ok(meta) => {
            if let Err(err) = send_response_meta(
                &server.response_pool,
                writer,
                request_id,
                meta,
                server.config.shm_threshold,
                server.config.chunk_size as usize,
                server.config.max_payload_size,
            )
            .await
            {
                match err {
                    ResponseSendError::UserVisible(message) => {
                        write_reply(
                            writer,
                            request_id,
                            &ReplyControl::Error(error_wire(
                                ErrorCode::ResourceOutputSerializing,
                                message,
                            )),
                        )
                        .await;
                    }
                    ResponseSendError::Transport(message) => {
                        warn!(request_id, error = %message, "response send failed");
                    }
                }
            }
        }
        Err(RouteExecutionError::Crm(CrmError::UserError(b))) => {
            write_reply(writer, request_id, &ReplyControl::Error(b)).await;
        }
        Err(RouteExecutionError::Crm(CrmError::InternalError(s))) => {
            write_reply(
                writer,
                request_id,
                &ReplyControl::Error(error_wire(ErrorCode::ResourceFunctionExecuting, s)),
            )
            .await;
        }
        Err(RouteExecutionError::Acquire(SchedulerAcquireError::Closed)) => {
            write_reply(
                writer,
                request_id,
                &ReplyControl::Error(error_wire(ErrorCode::ResourceUnavailable, "route closed")),
            )
            .await;
        }
        Err(RouteExecutionError::Acquire(SchedulerAcquireError::Capacity { field, limit })) => {
            let msg = format!("route concurrency capacity exceeded: {field}={limit}");
            write_reply(
                writer,
                request_id,
                &ReplyControl::Error(error_wire(ErrorCode::ResourceUnavailable, msg)),
            )
            .await;
        }
    }
}

// ---------------------------------------------------------------------------
// CRM call dispatch (inline, non-buddy, non-chunked)
// ---------------------------------------------------------------------------

async fn dispatch_call(
    server: &Server,
    conn: &Connection,
    request_id: u64,
    payload: &[u8],
    writer: &Arc<Mutex<OwnedWriteHalf>>,
) {
    conn.flight_inc();

    let (ctrl, consumed) = match decode_call_control(payload, 0) {
        Ok(v) => v,
        Err(e) => {
            warn!(conn_id = conn.conn_id(), ?e, "call control decode error");
            conn.flight_dec();
            return;
        }
    };

    let route = server.dispatcher.read().await.resolve(&ctrl.route_name);
    let route = match route {
        Some(r) => r,
        None => {
            write_reply(
                writer,
                request_id,
                &ReplyControl::RouteNotFound(ctrl.route_name),
            )
            .await;
            conn.flight_dec();
            return;
        }
    };

    let callback = Arc::clone(&route.callback);
    let name = ctrl.route_name;
    let idx = ctrl.method_idx;
    let args = payload[consumed..].to_vec();
    let resp_pool = Arc::clone(&server.response_pool);

    let scheduler = route.scheduler.as_ref().clone();
    let request = RequestData::Inline(args);
    let result = execute_route_request(scheduler, idx, request, move |request| {
        callback.invoke(&name, idx, request, resp_pool)
    })
    .await;

    send_route_execution_result(server, writer, request_id, result).await;

    conn.flight_dec();
}

// ---------------------------------------------------------------------------
// CRM call dispatch — buddy SHM
// ---------------------------------------------------------------------------

async fn dispatch_buddy_call(
    server: &Server,
    conn: &Arc<Connection>,
    request_id: u64,
    payload: &[u8],
    writer: &Arc<Mutex<OwnedWriteHalf>>,
) {
    conn.flight_inc();

    // 1. Decode buddy pointer (11 bytes).
    let (bp, _bp_consumed) = match decode_buddy_payload(payload) {
        Ok(v) => v,
        Err(e) => {
            warn!(conn_id = conn.conn_id(), ?e, "buddy payload decode error");
            conn.flight_dec();
            return;
        }
    };

    // 2. Decode call control (route_name + method_idx) after buddy header.
    let (ctrl, ctrl_consumed) = match decode_call_control(payload, BUDDY_PAYLOAD_SIZE) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                conn_id = conn.conn_id(),
                ?e,
                "buddy call control decode error"
            );
            conn.flight_dec();
            return;
        }
    };

    // 3. Ensure peer SHM segment is mapped and get pool Arc (single lock).
    let peer_pool = match conn.ensure_and_get_peer_pool(bp.seg_idx, bp.data_size, bp.is_dedicated) {
        Ok(p) => p,
        Err(e) => {
            warn!(conn_id = conn.conn_id(), %e, "ensure_and_get_peer_pool failed");
            let msg = format!("buddy SHM segment open: {e}");
            write_reply(
                writer,
                request_id,
                &ReplyControl::Error(error_wire(ErrorCode::ResourceInputDeserializing, msg)),
            )
            .await;
            conn.flight_dec();
            return;
        }
    };

    // 4. Check for extra inline args appended after control header.
    let inline_start = BUDDY_PAYLOAD_SIZE + ctrl_consumed;
    let extra_args = if inline_start < payload.len() {
        &payload[inline_start..]
    } else {
        &[]
    };

    // 5. Build request — zero-copy SHM or fallback to inline if extra args present.
    let request = if extra_args.is_empty() {
        RequestData::Shm {
            pool: peer_pool,
            seg_idx: bp.seg_idx,
            offset: bp.offset,
            data_size: bp.data_size,
            is_dedicated: bp.is_dedicated,
        }
    } else {
        // Rare edge case: extra inline args after buddy payload — fall back to copy.
        warn!(
            conn_id = conn.conn_id(),
            extra_len = extra_args.len(),
            "buddy call has trailing inline args, falling back to copy"
        );
        let args = match conn.read_peer_data(bp.seg_idx, bp.offset, bp.data_size, bp.is_dedicated) {
            Ok(data) => data,
            Err(e) => {
                warn!(conn_id = conn.conn_id(), %e, "buddy SHM read failed (fallback)");
                let msg = format!("buddy SHM read: {e}");
                write_reply(
                    writer,
                    request_id,
                    &ReplyControl::Error(error_wire(ErrorCode::ResourceInputDeserializing, msg)),
                )
                .await;
                conn.flight_dec();
                return;
            }
        };
        let free_result =
            conn.free_peer_block(bp.seg_idx, bp.offset, bp.data_size, bp.is_dedicated);
        if let c2_mem::FreeResult::SegmentIdle { .. } = free_result {
            let conn2 = Arc::clone(&conn);
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                conn2.gc_peer_buddy();
            });
        }
        let mut combined = args;
        combined.extend_from_slice(extra_args);
        RequestData::Inline(combined)
    };

    // 6. Route + execute.
    let route = server.dispatcher.read().await.resolve(&ctrl.route_name);
    let route = match route {
        Some(r) => r,
        None => {
            cleanup_request(request);
            write_reply(
                writer,
                request_id,
                &ReplyControl::RouteNotFound(ctrl.route_name),
            )
            .await;
            conn.flight_dec();
            return;
        }
    };

    let callback = Arc::clone(&route.callback);
    let name = ctrl.route_name;
    let idx = ctrl.method_idx;
    let resp_pool = Arc::clone(&server.response_pool);

    let scheduler = route.scheduler.as_ref().clone();
    let result = execute_route_request(scheduler, idx, request, move |request| {
        callback.invoke(&name, idx, request, resp_pool)
    })
    .await;

    send_route_execution_result(server, writer, request_id, result).await;

    conn.flight_dec();
}

// ---------------------------------------------------------------------------
// CRM call dispatch — chunked reassembly
// ---------------------------------------------------------------------------

async fn dispatch_chunked_call(
    server: &Server,
    conn: &Arc<Connection>,
    request_id: u64,
    flags: u32,
    payload: &[u8],
    writer: &Arc<Mutex<OwnedWriteHalf>>,
) {
    let is_buddy = c2_wire::flags::is_buddy(flags);
    let mut offset: usize = 0;

    // 1. If buddy-backed chunk, read data from SHM first.
    // NOTE: Per-chunk data must be copied into the reassembly buffer — zero-copy
    // is applied only to the final assembled result (RequestData::Handle above).
    let shm_data: Option<Vec<u8>>;
    if is_buddy {
        let (bp, bp_consumed) = match decode_buddy_payload(payload) {
            Ok(v) => v,
            Err(e) => {
                warn!(conn_id = conn.conn_id(), ?e, "chunked buddy decode error");
                return;
            }
        };
        offset = bp_consumed;
        match conn.read_peer_data(bp.seg_idx, bp.offset, bp.data_size, bp.is_dedicated) {
            Ok(data) => {
                let free_result =
                    conn.free_peer_block(bp.seg_idx, bp.offset, bp.data_size, bp.is_dedicated);
                if let c2_mem::FreeResult::SegmentIdle { .. } = free_result {
                    let conn2 = Arc::clone(&conn);
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                        conn2.gc_peer_buddy();
                    });
                }
                shm_data = Some(data);
            }
            Err(e) => {
                warn!(conn_id = conn.conn_id(), %e, "chunked SHM read failed");
                return;
            }
        }
    } else {
        shm_data = None;
    }

    // 2. Decode chunk header.
    let (chunk_idx, total_chunks, ch_consumed) = match decode_chunk_header(payload, offset) {
        Ok(v) => v,
        Err(e) => {
            warn!(conn_id = conn.conn_id(), ?e, "chunk header decode error");
            return;
        }
    };
    offset += ch_consumed;

    // 3. On first chunk, decode call control and register with ChunkRegistry.
    if chunk_idx == 0 {
        let (ctrl, ctrl_consumed) = match decode_call_control(payload, offset) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    conn_id = conn.conn_id(),
                    ?e,
                    "chunked call control decode error"
                );
                return;
            }
        };
        offset += ctrl_consumed;

        // Determine chunk_size from this first chunk's data length.
        let first_data = if let Some(ref sd) = shm_data {
            sd.as_slice()
        } else {
            &payload[offset..]
        };
        let chunk_size = first_data.len();
        if chunk_size == 0 {
            warn!(
                conn_id = conn.conn_id(),
                "chunked: first chunk has zero data"
            );
            return;
        }

        if let Err(e) = server.chunk_registry.insert(
            conn.conn_id(),
            request_id,
            total_chunks as usize,
            chunk_size,
        ) {
            warn!(conn_id = conn.conn_id(), %e, "chunk insert failed");
            return;
        }
        server.chunk_registry.set_route_info(
            conn.conn_id(),
            request_id,
            ctrl.route_name,
            ctrl.method_idx,
        );
    }

    // 4. Get chunk data.
    let chunk_data: &[u8] = if let Some(ref sd) = shm_data {
        sd.as_slice()
    } else {
        &payload[offset..]
    };

    // 5. Feed chunk to registry.
    let complete =
        match server
            .chunk_registry
            .feed(conn.conn_id(), request_id, chunk_idx as usize, chunk_data)
        {
            Ok(complete) => complete,
            Err(e) => {
                warn!(conn_id = conn.conn_id(), %e, "chunk feed error");
                return;
            }
        };

    // 6. If complete, finish and dispatch.
    if complete {
        let finished = match server.chunk_registry.finish(conn.conn_id(), request_id) {
            Ok(f) => f,
            Err(e) => {
                warn!(conn_id = conn.conn_id(), %e, "chunk finish failed");
                return;
            }
        };
        let route_name = finished.route_name.unwrap_or_default();
        let method_idx = finished.method_idx.unwrap_or(0);

        let pool_arc = server.chunk_registry.pool().clone();
        let request = RequestData::Handle {
            handle: finished.handle,
            pool: pool_arc,
        };

        // FlightGuard: increments on create, decrements on drop.
        let _flight = crate::connection::FlightGuard::new(conn);

        let route = server.dispatcher.read().await.resolve(&route_name);
        let route = match route {
            Some(r) => r,
            None => {
                cleanup_request(request);
                write_reply(writer, request_id, &ReplyControl::RouteNotFound(route_name)).await;
                return;
            }
        };

        let callback = Arc::clone(&route.callback);
        let name = route_name;
        let idx = method_idx;
        let resp_pool = Arc::clone(&server.response_pool);

        let scheduler = route.scheduler.as_ref().clone();
        let result = execute_route_request(scheduler, idx, request, move |request| {
            callback.invoke(&name, idx, request, resp_pool)
        })
        .await;

        send_route_execution_result(server, writer, request_id, result).await;
    }
}

// ---------------------------------------------------------------------------
// Reply helpers
// ---------------------------------------------------------------------------

const REPLY_FLAGS: u32 = FLAG_RESPONSE | FLAG_REPLY_V2;

fn checked_frame_total_len(payload_len: usize, context: &str) -> Result<u32, String> {
    let total_len = 12usize
        .checked_add(payload_len)
        .ok_or_else(|| format!("{context} length overflow"))?;
    u32::try_from(total_len)
        .map_err(|_| format!("{context} length {total_len} exceeds u32 frame limit"))
}

fn inline_reply_total_len(data_len: usize) -> Result<u32, String> {
    let ctrl_len = encode_reply_control(&ReplyControl::Success).len();
    let payload_len = ctrl_len
        .checked_add(data_len)
        .ok_or_else(|| "inline reply frame length overflow".to_string())?;
    checked_frame_total_len(payload_len, "inline reply frame")
}

fn reply_chunk_count(data_len: usize, chunk_size: usize) -> Result<u32, String> {
    if chunk_size == 0 {
        return Err("chunk_size must be > 0".to_string());
    }
    if data_len == 0 {
        return Ok(0);
    }
    let chunks = data_len.div_ceil(chunk_size);
    u32::try_from(chunks).map_err(|_| {
        format!(
            "chunk count {chunks} exceeds reply chunk metadata limit {}",
            u32::MAX
        )
    })
}

fn ensure_response_meta_len(
    data_len: usize,
    max_payload_size: u64,
) -> Result<(), ResponseSendError> {
    let data_len_u64 = u64::try_from(data_len).unwrap_or(u64::MAX);
    if data_len_u64 > max_payload_size {
        return Err(ResponseSendError::UserVisible(format!(
            "response payload size {data_len} exceeds max_payload_size {max_payload_size}"
        )));
    }
    Ok(())
}

#[derive(Debug)]
enum BuddyReplyError {
    Fallback(String),
    Fatal(String),
}

impl std::fmt::Display for BuddyReplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fallback(message) | Self::Fatal(message) => f.write_str(message),
        }
    }
}

#[derive(Debug)]
enum ResponseSendError {
    UserVisible(String),
    Transport(String),
}

impl std::fmt::Display for ResponseSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UserVisible(message) | Self::Transport(message) => f.write_str(message),
        }
    }
}

async fn write_reply(writer: &Arc<Mutex<OwnedWriteHalf>>, request_id: u64, ctrl: &ReplyControl) {
    let payload = encode_reply_control(ctrl);
    let frame = encode_frame(request_id, REPLY_FLAGS, &payload);
    let _ = writer.lock().await.write_all(&frame).await;
}

/// Write a success reply: control header (STATUS_SUCCESS) + result data.
/// Uses stack buffer for small responses (≤1024B total frame) to avoid heap allocation.
async fn write_reply_with_data(
    writer: &Arc<Mutex<OwnedWriteHalf>>,
    request_id: u64,
    data: &[u8],
) -> Result<(), ResponseSendError> {
    let ctrl_bytes = encode_reply_control(&ReplyControl::Success);
    let payload_len = ctrl_bytes.len().checked_add(data.len()).ok_or_else(|| {
        ResponseSendError::UserVisible("inline reply frame length overflow".to_string())
    })?;
    let total_len = inline_reply_total_len(data.len()).map_err(ResponseSendError::UserVisible)?;
    let frame_size = frame::HEADER_SIZE + payload_len;

    if frame_size <= 1024 {
        // Stack buffer: single write_all, zero heap allocation
        let mut buf = [0u8; 1024];
        buf[0..4].copy_from_slice(&total_len.to_le_bytes());
        buf[4..12].copy_from_slice(&request_id.to_le_bytes());
        buf[12..16].copy_from_slice(&REPLY_FLAGS.to_le_bytes());
        let mut off = frame::HEADER_SIZE;
        buf[off..off + ctrl_bytes.len()].copy_from_slice(&ctrl_bytes);
        off += ctrl_bytes.len();
        buf[off..off + data.len()].copy_from_slice(data);
        off += data.len();
        writer
            .lock()
            .await
            .write_all(&buf[..off])
            .await
            .map_err(|e| ResponseSendError::Transport(format!("inline reply write failed: {e}")))?;
    } else {
        // Large response: heap Vec (existing path)
        let mut payload = Vec::with_capacity(payload_len);
        payload.extend_from_slice(&ctrl_bytes);
        payload.extend_from_slice(data);
        let frame = encode_frame(request_id, REPLY_FLAGS, &payload);
        writer
            .lock()
            .await
            .write_all(&frame)
            .await
            .map_err(|e| ResponseSendError::Transport(format!("inline reply write failed: {e}")))?;
    }
    Ok(())
}

/// Write a success reply via buddy SHM: allocate from response pool, write
/// data, send 11-byte pointer frame. The caller chooses inline or chunked
/// fallback when SHM is unavailable.
async fn write_buddy_reply_with_data(
    response_pool: &parking_lot::RwLock<MemPool>,
    writer: &Arc<Mutex<OwnedWriteHalf>>,
    request_id: u64,
    data: &[u8],
) -> Result<(), BuddyReplyError> {
    let data_size = buddy_response_data_size(data.len()).ok_or_else(|| {
        BuddyReplyError::Fallback(format!(
            "response payload size {} exceeds buddy response wire limit {}",
            data.len(),
            u32::MAX
        ))
    })?;

    // 1. Allocate from response pool.
    let alloc = {
        let mut pool = response_pool.write();
        pool.alloc(data.len())
    };
    let alloc = match alloc {
        Ok(a) => a,
        Err(e) => {
            return Err(BuddyReplyError::Fallback(format!("alloc failed: {e}")));
        }
    };

    // 2. Write data to SHM (single lock scope).
    let write_ok = {
        let pool = response_pool.read();
        match pool.data_ptr(&alloc) {
            Ok(ptr) => {
                unsafe {
                    std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
                }
                true
            }
            Err(_) => false,
        }
    };
    if !write_ok {
        {
            let mut pool = response_pool.write();
            let _ = pool.free(&alloc);
        }
        return Err(BuddyReplyError::Fallback("data_ptr failed".into()));
    }

    // 3. Encode buddy payload + reply control.
    let bp = BuddyPayload {
        seg_idx: alloc.seg_idx as u16,
        offset: alloc.offset,
        data_size,
        is_dedicated: alloc.is_dedicated,
    };
    let buddy_bytes = encode_buddy_payload(&bp);
    let ctrl_bytes = encode_reply_control(&ReplyControl::Success);

    let mut payload = Vec::with_capacity(BUDDY_PAYLOAD_SIZE + ctrl_bytes.len());
    payload.extend_from_slice(&buddy_bytes);
    payload.extend_from_slice(&ctrl_bytes);

    // 4. Send frame with FLAG_BUDDY.
    let flags = FLAG_RESPONSE | FLAG_REPLY_V2 | FLAG_BUDDY;
    let frame = encode_frame(request_id, flags, &payload);
    if let Err(err) = writer.lock().await.write_all(&frame).await {
        let mut pool = response_pool.write();
        let _ = pool.free(&alloc);
        return Err(BuddyReplyError::Fatal(format!(
            "buddy reply write failed: {err}"
        )));
    }

    // 5. Server-side free for dedicated segments: the client will lazy-open
    //    and read from SHM before gc_delay expires.  Buddy allocs use SHM
    //    atomics so the client's free_at is already cross-process visible.
    if alloc.is_dedicated {
        let mut pool = response_pool.write();
        let _ = pool.free(&alloc);
    }

    Ok(())
}

/// Write a success reply via chunked transfer: split data into chunks,
/// each with reply chunk meta header. Uses FLAG_RESPONSE | FLAG_REPLY_V2 | FLAG_CHUNKED.
/// Last chunk also sets FLAG_CHUNK_LAST.
async fn write_chunked_reply(
    writer: &Arc<Mutex<OwnedWriteHalf>>,
    request_id: u64,
    data: &[u8],
    chunk_size: usize,
) -> Result<(), ResponseSendError> {
    let total_chunks =
        reply_chunk_count(data.len(), chunk_size).map_err(ResponseSendError::UserVisible)?;
    let total_size = data.len() as u64;

    for (idx, chunk) in data.chunks(chunk_size).enumerate() {
        let chunk_idx = u32::try_from(idx).map_err(|_| {
            ResponseSendError::UserVisible(format!(
                "chunk index {idx} exceeds reply chunk metadata limit"
            ))
        })?;
        let meta = encode_reply_chunk_meta(total_size, total_chunks, chunk_idx);
        let mut flags = REPLY_FLAGS | FLAG_CHUNKED;
        if chunk_idx == total_chunks - 1 {
            flags |= FLAG_CHUNK_LAST;
        }

        // Build frame: header + meta + chunk data
        let payload_len = REPLY_CHUNK_META_SIZE + chunk.len();
        let total_len = checked_frame_total_len(payload_len, "chunked reply frame")
            .map_err(ResponseSendError::UserVisible)?;

        let mut frame = Vec::with_capacity(frame::HEADER_SIZE + payload_len);
        frame.extend_from_slice(&total_len.to_le_bytes());
        frame.extend_from_slice(&request_id.to_le_bytes());
        frame.extend_from_slice(&flags.to_le_bytes());
        frame.extend_from_slice(&meta);
        frame.extend_from_slice(chunk);

        writer.lock().await.write_all(&frame).await.map_err(|e| {
            ResponseSendError::Transport(format!("chunked reply write failed: {e}"))
        })?;
    }
    Ok(())
}

/// Dispatch a `ResponseMeta` to the appropriate reply path.
async fn send_response_meta(
    response_pool: &parking_lot::RwLock<MemPool>,
    writer: &Arc<Mutex<OwnedWriteHalf>>,
    request_id: u64,
    meta: ResponseMeta,
    shm_threshold: u64,
    chunk_size: usize,
    max_payload_size: u64,
) -> Result<(), ResponseSendError> {
    match meta {
        ResponseMeta::Inline(data) => {
            ensure_response_meta_len(data.len(), max_payload_size)?;
            smart_reply_with_data(
                response_pool,
                writer,
                request_id,
                &data,
                shm_threshold,
                chunk_size,
            )
            .await?;
        }
        ResponseMeta::Empty => {
            write_reply_with_data(writer, request_id, &[]).await?;
        }
        ResponseMeta::ShmAlloc {
            seg_idx,
            offset,
            data_size,
            is_dedicated,
        } => {
            if u64::from(data_size) > max_payload_size {
                let mut pool = response_pool.write();
                let _ = pool.free_at(seg_idx as u32, offset, data_size, is_dedicated);
                return Err(ResponseSendError::UserVisible(format!(
                    "response payload size {data_size} exceeds max_payload_size {max_payload_size}"
                )));
            }
            // CRM already wrote into our response pool — send buddy pointer.
            let bp = BuddyPayload {
                seg_idx,
                offset,
                data_size,
                is_dedicated,
            };
            let buddy_bytes = encode_buddy_payload(&bp);
            let ctrl_bytes = encode_reply_control(&ReplyControl::Success);
            let mut payload = Vec::with_capacity(BUDDY_PAYLOAD_SIZE + ctrl_bytes.len());
            payload.extend_from_slice(&buddy_bytes);
            payload.extend_from_slice(&ctrl_bytes);
            let flags = FLAG_RESPONSE | FLAG_REPLY_V2 | FLAG_BUDDY;
            let frame = encode_frame(request_id, flags, &payload);
            if let Err(err) = writer.lock().await.write_all(&frame).await {
                let mut pool = response_pool.write();
                let _ = pool.free_at(seg_idx as u32, offset, data_size, is_dedicated);
                return Err(ResponseSendError::Transport(format!(
                    "prepared SHM reply write failed: {err}"
                )));
            }

            // Server-side free for dedicated segments (same as write_buddy_reply_with_data).
            if is_dedicated {
                let mut pool = response_pool.write();
                let _ = pool.free_at(seg_idx as u32, offset, data_size, true);
            }
        }
    }
    Ok(())
}

/// Choose buddy SHM or inline reply based on data size and threshold.
async fn smart_reply_with_data(
    response_pool: &parking_lot::RwLock<MemPool>,
    writer: &Arc<Mutex<OwnedWriteHalf>>,
    request_id: u64,
    data: &[u8],
    shm_threshold: u64,
    chunk_size: usize,
) -> Result<(), ResponseSendError> {
    if data.len() as u64 > shm_threshold {
        // Try buddy SHM first only when the buddy wire metadata can represent
        // the payload. Larger responses must go straight to chunked fallback.
        if buddy_response_data_size(data.len()).is_some() {
            match write_buddy_reply_with_data(response_pool, writer, request_id, data).await {
                Ok(()) => return Ok(()),
                Err(BuddyReplyError::Fallback(_)) => {}
                Err(BuddyReplyError::Fatal(message)) => {
                    return Err(ResponseSendError::Transport(message));
                }
            }
        }

        // SHM failed or is not representable. Use chunked for large data and
        // for any data that cannot fit in a single inline frame.
        if data.len() > chunk_size || inline_reply_total_len(data.len()).is_err() {
            return write_chunked_reply(writer, request_id, data, chunk_size).await;
        }
    }

    if inline_reply_total_len(data.len()).is_err() {
        return write_chunked_reply(writer, request_id, data, chunk_size).await;
    }
    write_reply_with_data(writer, request_id, data).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::dispatcher::{CrmCallback, CrmError, CrmRoute};
    use crate::scheduler::{ConcurrencyMode, Scheduler};

    // -- address parsing --

    #[test]
    fn parse_ipc_address() {
        let p = parse_socket_path("ipc://my_region").unwrap();
        assert_eq!(p, PathBuf::from("/tmp/c_two_ipc/my_region.sock"));
    }

    #[test]
    fn parse_legacy_v3_rejected() {
        assert!(parse_socket_path("ipc-v3://region42").is_err());
    }

    #[test]
    fn parse_invalid_scheme() {
        assert!(parse_socket_path("tcp://host").is_err());
    }

    #[test]
    fn parse_empty_region() {
        assert!(parse_socket_path("ipc://").is_err());
    }

    #[test]
    fn parse_rejects_path_like_region() {
        for address in [
            "ipc://../escape",
            "ipc://bad/name",
            "ipc://bad\\name",
            "ipc://.",
            "ipc://..",
            "ipc:// leading",
            "ipc://trailing ",
            "ipc://bad\nname",
        ] {
            assert!(
                parse_socket_path(address).is_err(),
                "address should be rejected: {address:?}"
            );
        }
    }

    // -- server construction --

    #[test]
    fn server_new_default_config() {
        let s = Server::new("ipc://test_srv", ServerIpcConfig::default()).unwrap();
        assert_eq!(s.socket_path(), Path::new("/tmp/c_two_ipc/test_srv.sock"));
    }

    #[test]
    fn server_new_derives_stable_server_id_and_instance_identity() {
        let first = Server::new("ipc://identity_srv", ServerIpcConfig::default()).unwrap();
        let second = Server::new("ipc://identity_srv", ServerIpcConfig::default()).unwrap();

        assert_eq!(first.server_id(), "identity_srv");
        assert_eq!(first.identity().server_id, "identity_srv");
        assert_eq!(first.server_instance_id().len(), 32);
        assert_ne!(first.server_instance_id(), second.server_instance_id());
    }

    #[test]
    fn server_new_with_identity_uses_validated_identity() {
        let identity = c2_wire::handshake::ServerIdentity {
            server_id: "server-explicit".to_string(),
            server_instance_id: "instance-explicit".to_string(),
        };

        let server = Server::new_with_identity(
            "ipc://identity_explicit",
            ServerIpcConfig::default(),
            identity.clone(),
        )
        .unwrap();

        assert_eq!(server.identity(), &identity);
        assert_eq!(server.server_id(), "server-explicit");
        assert_eq!(server.server_instance_id(), "instance-explicit");
    }

    #[test]
    fn server_new_with_identity_rejects_invalid_instance_id() {
        let too_long = "a".repeat(c2_wire::handshake::MAX_HANDSHAKE_NAME_BYTES + 1);
        for server_instance_id in ["../bad", "实例", too_long.as_str()] {
            let identity = c2_wire::handshake::ServerIdentity {
                server_id: "server-explicit".to_string(),
                server_instance_id: server_instance_id.to_string(),
            };

            let err = Server::new_with_identity(
                "ipc://identity_bad",
                ServerIpcConfig::default(),
                identity,
            )
            .err()
            .expect("invalid identity should be rejected");

            assert!(err.to_string().contains("server_instance_id"));
        }
    }

    #[test]
    fn server_new_with_identity_rejects_server_id_too_long_for_wire() {
        let identity = c2_wire::handshake::ServerIdentity {
            server_id: "s".repeat(c2_wire::handshake::MAX_HANDSHAKE_NAME_BYTES + 1),
            server_instance_id: "instance-explicit".to_string(),
        };

        let err = Server::new_with_identity(
            "ipc://identity_bad_server_id",
            ServerIpcConfig::default(),
            identity,
        )
        .err()
        .expect("overlong server_id should be rejected");

        assert!(err.to_string().contains("server_id"));
    }

    #[test]
    fn server_new_bad_address() {
        assert!(Server::new("http://bad", ServerIpcConfig::default()).is_err());
    }

    #[test]
    fn server_new_bad_config() {
        let cfg = ServerIpcConfig {
            max_payload_size: 0,
            ..ServerIpcConfig::default()
        };
        assert!(Server::new("ipc://x", cfg).is_err());
    }

    fn unique_readiness_address(prefix: &str) -> String {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        format!("ipc://{prefix}_{}_{}", std::process::id(), n)
    }

    fn unique_response_pool_prefix(label: &str) -> String {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        format!("/c2sw{:04x}{:04x}", std::process::id() as u16, n as u16) + label
    }

    fn small_response_pool(label: &str) -> parking_lot::RwLock<MemPool> {
        parking_lot::RwLock::new(MemPool::new_with_prefix(
            PoolConfig {
                segment_size: 64 * 1024,
                min_block_size: 4096,
                max_segments: 1,
                max_dedicated_segments: 1,
                dedicated_crash_timeout_secs: 0.0,
                ..PoolConfig::default()
            },
            unique_response_pool_prefix(label),
        ))
    }

    fn closed_writer() -> Arc<Mutex<OwnedWriteHalf>> {
        let (client, server) = StdUnixStream::pair().unwrap();
        client.shutdown(std::net::Shutdown::Both).unwrap();
        server.set_nonblocking(true).unwrap();
        let server = UnixStream::from_std(server).unwrap();
        let (_read_half, write_half) = server.into_split();
        Arc::new(Mutex::new(write_half))
    }

    #[tokio::test]
    async fn wait_until_ready_times_out_before_start() {
        let server = Arc::new(
            Server::new(
                &unique_readiness_address("ready_timeout"),
                ServerIpcConfig::default(),
            )
            .unwrap(),
        );

        let err = server
            .wait_until_ready(Duration::from_millis(1))
            .await
            .expect_err("server that was never started must time out");

        assert!(err.to_string().contains("did not become ready"));
        assert_eq!(server.lifecycle_state(), ServerLifecycleState::Initialized);
        assert!(!server.is_ready());
        assert!(!server.is_running());
    }

    #[tokio::test]
    async fn run_sets_ready_then_shutdown_sets_stopped() {
        let server = Arc::new(
            Server::new(
                &unique_readiness_address("ready_state"),
                ServerIpcConfig::default(),
            )
            .unwrap(),
        );
        let runner = {
            let server = Arc::clone(&server);
            tokio::spawn(async move { server.run().await })
        };

        server
            .wait_until_ready(Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(server.lifecycle_state(), ServerLifecycleState::Ready);
        assert!(server.is_ready());
        assert!(server.is_running());
        assert!(StdUnixStream::connect(server.socket_path()).is_ok());

        server.shutdown();
        runner.await.unwrap().unwrap();
        assert_eq!(server.lifecycle_state(), ServerLifecycleState::Stopped);
        assert!(!server.is_ready());
        assert!(!server.is_running());
    }

    #[tokio::test]
    async fn active_socket_is_not_unlinked_by_second_server() {
        let address = unique_readiness_address("active_socket");
        let first = Arc::new(Server::new(&address, ServerIpcConfig::default()).unwrap());
        let first_runner = {
            let first = Arc::clone(&first);
            tokio::spawn(async move { first.run().await })
        };
        first
            .wait_until_ready(Duration::from_secs(2))
            .await
            .unwrap();
        assert!(StdUnixStream::connect(first.socket_path()).is_ok());

        let second = Arc::new(Server::new(&address, ServerIpcConfig::default()).unwrap());
        let second_result = tokio::time::timeout(Duration::from_millis(200), {
            let second = Arc::clone(&second);
            async move { second.run().await }
        })
        .await;

        match second_result {
            Ok(Err(err)) => {
                let message = err.to_string();
                assert!(
                    message.contains("already has an active listener")
                        || message.contains("address already in use"),
                    "unexpected error: {message}",
                );
            }
            Ok(Ok(())) => panic!("second server unexpectedly started and stopped cleanly"),
            Err(_) => {
                second.shutdown();
                panic!("second server hung instead of rejecting the active socket");
            }
        }

        assert!(first.is_ready());
        assert!(StdUnixStream::connect(first.socket_path()).is_ok());
        first.shutdown();
        first_runner.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn shutdown_after_failed_bind_does_not_unlink_active_socket() {
        let address = unique_readiness_address("failed_bind_shutdown");
        let first = Arc::new(Server::new(&address, ServerIpcConfig::default()).unwrap());
        let first_runner = {
            let first = Arc::clone(&first);
            tokio::spawn(async move { first.run().await })
        };
        first
            .wait_until_ready(Duration::from_secs(2))
            .await
            .unwrap();
        assert!(StdUnixStream::connect(first.socket_path()).is_ok());

        let second = Arc::new(Server::new(&address, ServerIpcConfig::default()).unwrap());
        let second_result = tokio::time::timeout(Duration::from_millis(200), {
            let second = Arc::clone(&second);
            async move { second.run().await }
        })
        .await;
        let err = second_result
            .expect("second server hung instead of rejecting the active socket")
            .expect_err("second bind must fail");
        assert!(
            err.to_string().contains("active listener")
                || err.to_string().contains("address already in use"),
            "unexpected error: {err}",
        );
        second.shutdown();

        assert!(first.is_ready());
        assert!(StdUnixStream::connect(first.socket_path()).is_ok());
        first.shutdown();
        first_runner.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn same_server_can_start_again_after_shutdown() {
        let server = Arc::new(
            Server::new(
                &unique_readiness_address("restart_after_shutdown"),
                ServerIpcConfig::default(),
            )
            .unwrap(),
        );

        server.begin_start_attempt().unwrap();
        let first_runner = {
            let server = Arc::clone(&server);
            tokio::spawn(async move { server.run().await })
        };
        server
            .wait_until_ready(Duration::from_secs(2))
            .await
            .unwrap();
        assert!(StdUnixStream::connect(server.socket_path()).is_ok());

        server.shutdown();
        first_runner.await.unwrap().unwrap();
        assert_eq!(server.lifecycle_state(), ServerLifecycleState::Stopped);

        server.begin_start_attempt().unwrap();
        let second_runner = {
            let server = Arc::clone(&server);
            tokio::spawn(async move { server.run().await })
        };
        server
            .wait_until_ready(Duration::from_secs(2))
            .await
            .unwrap();
        assert!(StdUnixStream::connect(server.socket_path()).is_ok());

        server.shutdown();
        second_runner.await.unwrap().unwrap();
        assert_eq!(server.lifecycle_state(), ServerLifecycleState::Stopped);
    }

    #[tokio::test]
    async fn failed_bind_can_retry_after_socket_released() {
        let address = unique_readiness_address("retry_after_failed_bind");
        let first = Arc::new(Server::new(&address, ServerIpcConfig::default()).unwrap());
        first.begin_start_attempt().unwrap();
        let first_runner = {
            let first = Arc::clone(&first);
            tokio::spawn(async move { first.run().await })
        };
        first
            .wait_until_ready(Duration::from_secs(2))
            .await
            .unwrap();

        let second = Arc::new(Server::new(&address, ServerIpcConfig::default()).unwrap());
        second.begin_start_attempt().unwrap();
        let failed = second
            .run()
            .await
            .expect_err("active socket should reject the first attempt");
        assert!(
            failed.to_string().contains("active listener")
                || failed.to_string().contains("address already in use"),
            "unexpected error: {failed}",
        );
        assert!(matches!(
            second.lifecycle_state(),
            ServerLifecycleState::Failed(_)
        ));

        first.shutdown();
        first_runner.await.unwrap().unwrap();

        second.begin_start_attempt().unwrap();
        let second_runner = {
            let second = Arc::clone(&second);
            tokio::spawn(async move { second.run().await })
        };
        second
            .wait_until_ready(Duration::from_secs(2))
            .await
            .unwrap();
        assert!(StdUnixStream::connect(second.socket_path()).is_ok());

        second.shutdown();
        second_runner.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn wait_until_stopped_fences_restart_after_shutdown() {
        let server = Arc::new(
            Server::new(
                &unique_readiness_address("wait_stop_restart"),
                ServerIpcConfig::default(),
            )
            .unwrap(),
        );

        server.begin_start_attempt().unwrap();
        let runner = {
            let server = Arc::clone(&server);
            tokio::spawn(async move { server.run().await })
        };
        server
            .wait_until_ready(Duration::from_secs(2))
            .await
            .unwrap();

        server.shutdown();
        server
            .wait_until_stopped(Duration::from_secs(2))
            .await
            .unwrap();
        server
            .begin_start_attempt()
            .expect("stopped lifecycle should permit a new start attempt");

        runner.await.unwrap().unwrap();
    }

    #[test]
    fn finalize_runtime_stopped_terminalizes_running_states_but_preserves_failed() {
        let server = Server::new(
            &unique_readiness_address("finalize_runtime"),
            ServerIpcConfig::default(),
        )
        .unwrap();

        server.set_lifecycle_state(ServerLifecycleState::Stopping);
        server.finalize_runtime_stopped();
        assert_eq!(server.lifecycle_state(), ServerLifecycleState::Stopped);

        server.set_lifecycle_state(ServerLifecycleState::Failed("bind failed".to_string()));
        server.finalize_runtime_stopped();
        assert_eq!(
            server.lifecycle_state(),
            ServerLifecycleState::Failed("bind failed".to_string()),
        );
    }

    // -- route registration --

    struct Echo;
    impl CrmCallback for Echo {
        fn invoke(
            &self,
            _: &str,
            _: u16,
            _request: RequestData,
            _response_pool: Arc<parking_lot::RwLock<MemPool>>,
        ) -> Result<ResponseMeta, CrmError> {
            Ok(ResponseMeta::Inline(b"echo".to_vec()))
        }
    }

    fn make_route(name: &str) -> CrmRoute {
        CrmRoute {
            name: name.into(),
            scheduler: Arc::new(Scheduler::new(
                ConcurrencyMode::ReadParallel,
                HashMap::new(),
            )),
            callback: Arc::new(Echo),
            method_names: vec!["step".into(), "query".into()],
        }
    }

    #[tokio::test]
    async fn register_unregister_route() {
        let s = Arc::new(Server::new("ipc://reg_test", ServerIpcConfig::default()).unwrap());

        let route = make_route("grid");
        let scheduler = route.scheduler.as_ref().clone();
        s.register_route(route).await.unwrap();
        assert!(s.dispatcher.read().await.resolve("grid").is_some());
        assert!(s.contains_route("grid").await);

        assert!(s.unregister_route("grid").await);
        assert!(s.dispatcher.read().await.resolve("grid").is_none());
        assert!(!s.contains_route("grid").await);
        assert!(scheduler.snapshot().closed);
        assert_eq!(
            scheduler.try_acquire(0).unwrap_err(),
            crate::scheduler::SchedulerAcquireError::Closed,
        );
    }

    #[tokio::test]
    async fn duplicate_route_registration_is_rejected() {
        let s = Arc::new(Server::new("ipc://dup_route_test", ServerIpcConfig::default()).unwrap());

        s.register_route(make_route("grid")).await.unwrap();
        let err = s.register_route(make_route("grid")).await.unwrap_err();

        assert!(err.to_string().contains("already registered"));
    }

    #[tokio::test]
    async fn rejected_acquire_cleans_materialized_request() {
        use crate::scheduler::{SchedulerAcquireError, SchedulerLimits};
        use c2_mem::PoolConfig;
        use std::num::NonZeroUsize;

        let pool = Arc::new(parking_lot::RwLock::new(MemPool::new_with_prefix(
            PoolConfig {
                segment_size: 4096,
                min_block_size: 128,
                max_segments: 1,
                max_dedicated_segments: 0,
                dedicated_crash_timeout_secs: 0.0,
                ..PoolConfig::default()
            },
            format!("/cc2s{:04x}{:04x}", std::process::id() as u16, 0xaceu16,),
        )));
        pool.write().ensure_ready().unwrap();
        let alloc = pool.write().alloc(128).unwrap();
        let request = RequestData::Shm {
            pool: Arc::clone(&pool),
            seg_idx: alloc.seg_idx as u16,
            offset: alloc.offset,
            data_size: 128,
            is_dedicated: alloc.is_dedicated,
        };
        let scheduler = Scheduler::with_limits(
            ConcurrencyMode::Parallel,
            HashMap::new(),
            SchedulerLimits {
                max_pending: Some(NonZeroUsize::new(1).unwrap()),
                max_workers: Some(NonZeroUsize::new(1).unwrap()),
            },
        );
        let _first = scheduler.try_acquire(0).unwrap();

        let err = execute_route_request(scheduler.clone(), 0, request, |_request| {
            panic!("callback must not run when acquire fails")
        })
        .await
        .unwrap_err();

        assert!(matches!(
            err,
            RouteExecutionError::Acquire(SchedulerAcquireError::Capacity {
                field: "max_pending",
                limit: 1,
            })
        ));
        let reused = pool.write().alloc(128).unwrap();
        assert_eq!(reused.offset, alloc.offset);
    }

    #[tokio::test]
    async fn register_route_rejects_wire_invalid_route_name() {
        let s = Arc::new(Server::new("ipc://long_route_test", ServerIpcConfig::default()).unwrap());
        let route = make_route(&"x".repeat(c2_wire::handshake::MAX_HANDSHAKE_NAME_BYTES + 1));

        let err = s.register_route(route).await.unwrap_err();

        assert!(err.to_string().contains("route name"));
        assert!(s.dispatcher.read().await.is_empty());
    }

    #[tokio::test]
    async fn register_route_rejects_too_many_methods() {
        let s =
            Arc::new(Server::new("ipc://method_count_test", ServerIpcConfig::default()).unwrap());
        let mut route = make_route("grid");
        route.method_names = (0..=c2_wire::handshake::MAX_METHODS)
            .map(|i| format!("m{i}"))
            .collect();

        let err = s.register_route(route).await.unwrap_err();

        assert!(err.to_string().contains("method count"));
        assert!(s.dispatcher.read().await.is_empty());
    }

    #[tokio::test]
    async fn register_route_rejects_route_count_overflow_under_write_lock() {
        let s =
            Arc::new(Server::new("ipc://route_count_test", ServerIpcConfig::default()).unwrap());

        for i in 0..c2_wire::handshake::MAX_ROUTES {
            s.register_route(make_route(&format!("route_{i}")))
                .await
                .unwrap();
        }

        let err = s.register_route(make_route("overflow")).await.unwrap_err();

        assert!(err.to_string().contains("route count"));
        assert!(s.dispatcher.read().await.resolve("overflow").is_none());
    }

    // -- shutdown --

    #[tokio::test]
    async fn shutdown_sets_signal() {
        let s = Server::new("ipc://shut_test", ServerIpcConfig::default()).unwrap();
        let mut rx = s.shutdown_rx.clone();
        s.shutdown();
        rx.changed().await.unwrap();
        assert!(*rx.borrow());
    }

    // -- error display --

    #[test]
    fn server_error_display() {
        let e = ServerError::Config("bad".into());
        assert!(format!("{e}").contains("bad"));

        let e2 = ServerError::Protocol("oops".into());
        assert!(format!("{e2}").contains("oops"));
    }

    // -- buddy payload decode + call control --

    #[test]
    fn decode_buddy_then_call_control() {
        use c2_wire::buddy::{BUDDY_PAYLOAD_SIZE, BuddyPayload, encode_buddy_payload};
        use c2_wire::control::encode_call_control;

        let bp = BuddyPayload {
            seg_idx: 0,
            offset: 4096,
            data_size: 256,
            is_dedicated: false,
        };
        let bp_bytes = encode_buddy_payload(&bp);
        let ctrl_bytes = encode_call_control("grid", 1).unwrap();

        let mut payload = Vec::new();
        payload.extend_from_slice(&bp_bytes);
        payload.extend_from_slice(&ctrl_bytes);

        // Decode buddy part.
        let (decoded_bp, bp_consumed) = decode_buddy_payload(&payload).unwrap();
        assert_eq!(decoded_bp, bp);
        assert_eq!(bp_consumed, BUDDY_PAYLOAD_SIZE);

        // Decode call control after buddy header.
        let (ctrl, _) = decode_call_control(&payload, BUDDY_PAYLOAD_SIZE).unwrap();
        assert_eq!(ctrl.route_name, "grid");
        assert_eq!(ctrl.method_idx, 1);
    }

    // -- chunked reassembly via ChunkRegistry --

    #[test]
    fn chunked_reassembly_via_registry() {
        use parking_lot::RwLock;
        use std::sync::Arc;

        let reassembly_cfg = c2_mem::config::PoolConfig {
            segment_size: 64 * 1024,
            min_block_size: 4096,
            max_segments: 2,
            max_dedicated_segments: 2,
            dedicated_crash_timeout_secs: 0.0,
            buddy_idle_decay_secs: 0.0,
            spill_threshold: 1.0,
            spill_dir: std::env::temp_dir().join("c2_srv_chunk_test"),
        };
        let pool = Arc::new(RwLock::new(c2_mem::MemPool::new(reassembly_cfg)));
        let registry = c2_wire::chunk::ChunkRegistry::new(
            pool.clone(),
            c2_wire::chunk::ChunkConfig::default(),
        );

        let conn_id = 99u64;
        let request_id = 42u64;
        let total_chunks = 3usize;
        let chunk_size = 8usize;

        registry
            .insert(conn_id, request_id, total_chunks, chunk_size)
            .unwrap();
        registry.set_route_info(conn_id, request_id, "grid".into(), 0);

        // Feed chunks.
        assert!(!registry.feed(conn_id, request_id, 0, b"aaaaaaaa").unwrap());
        assert!(!registry.feed(conn_id, request_id, 1, b"bbbbbbbb").unwrap());
        assert!(registry.feed(conn_id, request_id, 2, b"cc").unwrap());

        // Finish.
        let finished = registry.finish(conn_id, request_id).unwrap();
        assert_eq!(finished.route_name.as_deref(), Some("grid"));
        assert_eq!(finished.method_idx, Some(0));
        assert_eq!(finished.handle.len(), 18); // 8+8+2
        let p = pool.read();
        let slice = p.handle_slice(&finished.handle);
        assert_eq!(&slice[0..8], b"aaaaaaaa");
        assert_eq!(&slice[8..16], b"bbbbbbbb");
        assert_eq!(&slice[16..18], b"cc");
        drop(p);
        pool.write().release_handle(finished.handle);
    }

    #[test]
    fn buddy_response_wire_limit_rejects_oversized_payloads() {
        assert_eq!(
            crate::response::buddy_response_data_size(u32::MAX as usize),
            Some(u32::MAX)
        );
        assert_eq!(
            crate::response::buddy_response_data_size(u32::MAX as usize + 1),
            None
        );
    }

    #[test]
    fn reply_chunk_count_rejects_unrepresentable_chunk_counts() {
        assert!(reply_chunk_count(0, 0).unwrap_err().contains("chunk_size"));
        assert_eq!(reply_chunk_count(0, 128).unwrap(), 0);
        assert_eq!(reply_chunk_count(1025, 512).unwrap(), 3);

        let err = reply_chunk_count(u32::MAX as usize + 1, 1).unwrap_err();
        assert!(err.contains("chunk count"));
    }

    #[test]
    fn inline_reply_frame_len_rejects_unrepresentable_frames() {
        assert!(inline_reply_total_len(16).is_ok());

        let err = inline_reply_total_len(u32::MAX as usize).unwrap_err();
        assert!(err.contains("inline reply frame"));
    }

    #[tokio::test]
    async fn buddy_reply_write_failure_frees_allocated_response_block() {
        let pool = small_response_pool("a");
        let writer = closed_writer();
        let payload = b"x".repeat(8192);

        let err = write_buddy_reply_with_data(&pool, &writer, 7, payload.as_slice())
            .await
            .unwrap_err();

        assert!(err.to_string().contains("buddy reply write failed"));
        assert_eq!(pool.read().stats().alloc_count, 0);
    }

    #[tokio::test]
    async fn prepared_shm_reply_write_failure_frees_allocated_response_block() {
        let pool = small_response_pool("b");
        let alloc = pool.write().alloc(8192).unwrap();
        let writer = closed_writer();

        let err = send_response_meta(
            &pool,
            &writer,
            7,
            ResponseMeta::ShmAlloc {
                seg_idx: alloc.seg_idx as u16,
                offset: alloc.offset,
                data_size: 8192,
                is_dedicated: alloc.is_dedicated,
            },
            1024,
            4096,
            16 * 1024,
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("prepared SHM reply write failed"));
        assert_eq!(pool.read().stats().alloc_count, 0);
    }

    #[tokio::test]
    async fn inline_response_over_max_payload_is_rejected_before_transport() {
        let pool = small_response_pool("c");
        let writer = closed_writer();

        let err = send_response_meta(
            &pool,
            &writer,
            7,
            ResponseMeta::Inline(b"x".repeat(1025)),
            1024,
            4096,
            1024,
        )
        .await
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("response payload size 1025 exceeds max_payload_size 1024")
        );
        assert_eq!(pool.read().stats().alloc_count, 0);
    }

    #[tokio::test]
    async fn prepared_shm_response_over_max_payload_is_rejected_and_freed() {
        let pool = small_response_pool("d");
        let alloc = pool.write().alloc(8192).unwrap();
        let writer = closed_writer();

        let err = send_response_meta(
            &pool,
            &writer,
            7,
            ResponseMeta::ShmAlloc {
                seg_idx: alloc.seg_idx as u16,
                offset: alloc.offset,
                data_size: 8192,
                is_dedicated: alloc.is_dedicated,
            },
            1024,
            4096,
            4096,
        )
        .await
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("response payload size 8192 exceeds max_payload_size 4096")
        );
        assert_eq!(pool.read().stats().alloc_count, 0);
    }

    #[tokio::test]
    async fn smart_reply_treats_buddy_write_failure_as_fatal() {
        let pool = small_response_pool("e");
        let writer = closed_writer();
        let payload = b"x".repeat(8192);

        let err = smart_reply_with_data(&pool, &writer, 7, payload.as_slice(), 1024, 4096)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("buddy reply write failed"));
        assert_eq!(pool.read().stats().alloc_count, 0);
    }

    // -- handshake extraction --

    #[test]
    fn handshake_extracts_client_info() {
        use c2_wire::handshake::{
            CAP_CALL_V2, CAP_CHUNKED, decode_handshake, encode_client_handshake,
        };

        let segments = vec![("seg0".into(), 4096u32), ("seg1".into(), 8192u32)];
        let cap = CAP_CALL_V2 | CAP_CHUNKED;
        let hs_bytes = encode_client_handshake(&segments, cap, "/cc3b_test").unwrap();

        let decoded = decode_handshake(&hs_bytes).unwrap();
        assert_eq!(decoded.prefix, "/cc3b_test");
        assert_eq!(decoded.segments.len(), 2);
        assert_eq!(decoded.segments[0].0, "seg0");
        assert_eq!(decoded.segments[0].1, 4096);
        assert_eq!(decoded.capability_flags & CAP_CHUNKED, CAP_CHUNKED);
    }
}
