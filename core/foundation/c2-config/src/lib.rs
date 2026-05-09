//! Shared configuration types for C-Two.
//!
//! This crate is the single source of truth for all configuration structs
//! used across the C-Two transport layer (IPC, relay, memory pool).

mod identity;
mod ipc;
mod pool;
mod relay;
mod resolver;

pub use identity::{validate_ipc_region_id, validate_server_id};
pub use ipc::{
    BASE_IPC_OVERRIDE_KEYS, BaseIpcConfig, CLIENT_IPC_OVERRIDE_KEYS, ClientIpcConfig,
    FORBIDDEN_IPC_OVERRIDE_KEYS, SERVER_IPC_OVERRIDE_KEYS, ServerIpcConfig,
};
pub use pool::PoolConfig;
pub use relay::RelayConfig;
pub use resolver::{
    ClientIpcConfigOverrides, ConfigResolver, ConfigSources, EnvFilePolicy, EnvMap,
    RelayConfigOverrides, ResolvedRelayClientConfig, ResolvedRelayConfig, ResolvedRuntimeConfig,
    RuntimeConfigOverrides, ServerIpcConfigOverrides,
};
