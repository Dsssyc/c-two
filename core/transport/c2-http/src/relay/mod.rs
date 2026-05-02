//! C-Two HTTP relay server — bridges HTTP requests to IPC.
//!
//! This module is behind the `relay` feature gate. Enable it with:
//! ```toml
//! c2-http = { path = "...", features = ["relay"] }
//! ```

pub(crate) mod authority;
pub(crate) mod background;
pub(crate) mod conn_pool;
pub(crate) mod disseminator;
pub(crate) mod gossip;
pub(crate) mod peer;
pub(crate) mod peer_handlers;
pub(crate) mod route_table;
pub(crate) mod router;
pub mod server;
pub(crate) mod state;
pub(crate) mod types;
pub(crate) mod url;

pub use c2_config::RelayConfig;
pub use server::{RelayControlError, RelayServer};
