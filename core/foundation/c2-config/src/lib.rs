//! Shared configuration types for C-Two.
//!
//! This crate is the single source of truth for all configuration structs
//! used across the C-Two transport layer (IPC, relay, memory pool).

mod ipc;
mod pool;
mod relay;
mod resolver;

pub use ipc::{BaseIpcConfig, ClientIpcConfig, ServerIpcConfig};
pub use pool::PoolConfig;
pub use relay::RelayConfig;
pub use resolver::{
    ClientIpcConfigOverrides, ConfigResolver, ConfigSources, EnvFilePolicy, EnvMap,
    RelayConfigOverrides, ResolvedRuntimeConfig, RuntimeConfigOverrides, ServerIpcConfigOverrides,
};
