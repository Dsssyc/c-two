//! HTTP client for C-Two relay transport.
//!
//! Provides [`HttpClient`] for making CRM calls through an HTTP relay
//! server, and [`HttpClientPool`] for reference-counted connection pooling.

mod client;
mod control;
mod pool;
mod relay_aware;

pub use client::{HttpClient, HttpError};
pub use control::{RelayControlClient, RelayRouteInfo};
pub use pool::HttpClientPool;
pub use relay_aware::{RelayAwareClientConfig, RelayAwareHttpClient, RelayResolvedTarget};
