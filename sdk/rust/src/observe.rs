//! Read-only runtime diagnostics for monitoring and troubleshooting.
//! Includes connection events, call admission and concurrency snapshots, memory
//! statistics, host lifecycle snapshots, and observed paths and routes.

pub use c2_core::{
    CallExecutionObservation, CallExecutionObserver, CallExecutionSnapshot, ConnectionClosed,
    ConnectionEndReason, ConnectionEvent, ConnectionEvents, ConnectionEventsError,
    ConnectionObservation, ConnectionSide, HostClientHeldLeases, HostLifecyclePhase,
    HostLifecycleSnapshot, MemoryCellStats, MemoryScopeStats, ObservedPath, ObservedRoute,
    PathCounters, RouteConcurrencySnapshot, RuntimeMemoryStats,
};
