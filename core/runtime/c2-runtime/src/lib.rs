//! Language-neutral process runtime session for C-Two.
//!
//! Phase 1 owns process server identity and canonical `ipc://` address
//! derivation. Later phases extend this session into direct IPC server/client
//! lifecycle, route transactions, and relay projection ownership.

mod identity;
mod outcome;
mod session;

pub use identity::{auto_server_id, ipc_address_for_server_id, validate_server_id};
pub use outcome::{
    RegisterOutcome, RelayCleanupError, RuntimeRouteSpec, ShutdownOutcome, UnregisterOutcome,
};
pub use session::{RuntimeIdentity, RuntimeSession, RuntimeSessionError, RuntimeSessionOptions};
