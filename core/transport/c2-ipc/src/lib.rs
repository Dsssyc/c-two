//! Async IPC client for C-Two.
//!
//! Connects to a C-Two IPC server via Unix Domain Socket, performs
//! handshake, and forwards requests using buddy SHM.
//!
//! # Architecture
//!
//! ```text
//! HTTP handler
//!     → IpcClient::call(route, method_idx, payload)
//!         → send_task:  serialize frame → write UDS
//!         → recv_task:  read UDS → match request_id → oneshot → caller
//! ```

pub mod client;
pub mod control;
pub mod pool;
pub mod response;
pub mod shm;
pub mod sync_client;

#[cfg(test)]
mod tests;

pub use client::{ClientIpcConfig, IpcClient, IpcError, MethodTable, ServerPoolState};
pub use control::{ping, shutdown, socket_path_from_ipc_address};
pub use pool::ClientPool;
pub use response::ResponseData;
pub use shm::{MappedSegment, SegmentCache, ShmError};
pub use sync_client::SyncClient;
