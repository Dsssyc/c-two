//! Local endpoint administration for supervisors and maintenance tools.
//! Includes endpoint contexts and credentials, inspection, reaping and sweeps,
//! direct IPC probes and shutdown acknowledgements, and local I/O causes.

pub use c2_core::{
    DirectIpcShutdownOutcome, DirectIpcShutdownRouteOutcome, ENDPOINT_CREDENTIAL_MAX_BYTES,
    ENDPOINT_CREDENTIAL_SCHEMA_VERSION, EndpointBindCause, EndpointCredential,
    EndpointCredentialError, EndpointCredentialErrorKind, EndpointInspection, EndpointIoError,
    EndpointReapResult, EndpointSweep, EndpointSweepScope, EndpointUnverifiedReason, LocalEndpoint,
    LocalEndpointContext, LocalEndpointNamespace, LocalEndpointOptions, LocalIoCause,
    RuntimeIdentity, SweepBatch, SweepBudget, direct_ipc_endpoint, inspect_endpoint,
    ping_direct_ipc, reap_endpoint, shutdown_direct_ipc,
};
