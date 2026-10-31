use c2_config::{
    ConfigResolver, ConfigSources, LocalEndpoint, LocalEndpointContext, LocalEndpointOptions,
};
use std::time::Duration;

use crate::{LifecycleError, Runtime};

/// Per-route fact returned by a direct IPC shutdown acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectIpcShutdownRouteOutcome {
    pub route_name: String,
    pub active_drained: bool,
    pub closed_reason: String,
}

/// Language-neutral direct IPC shutdown acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectIpcShutdownOutcome {
    pub acknowledged: bool,
    pub shutdown_started: bool,
    pub server_stopped: bool,
    pub route_outcomes: Vec<DirectIpcShutdownRouteOutcome>,
}

/// Capture process configuration once for an independent admin operation.
fn process_context() -> Result<LocalEndpointContext, LifecycleError> {
    ConfigResolver::resolve_local_endpoint(
        LocalEndpointOptions::default(),
        ConfigSources::from_process(),
    )
    .map_err(|error| LifecycleError::Configuration(error.to_string()))
}

/// Derive an endpoint without local I/O. `None` resolves process defaults once.
pub fn direct_ipc_endpoint(
    address: &str,
    context: Option<&LocalEndpointContext>,
) -> Result<LocalEndpoint, LifecycleError> {
    let context = context.cloned().map_or_else(process_context, Ok)?;
    context
        .endpoint(address)
        .map_err(|error| LifecycleError::Configuration(error.to_string()))
}

/// Probe using the supplied context, or one process-default snapshot for all retries.
pub fn ping_direct_ipc(
    address: &str,
    context: Option<&LocalEndpointContext>,
    timeout: Duration,
) -> Result<bool, LifecycleError> {
    let context = context.cloned().map_or_else(process_context, Ok)?;
    // A malformed probe target has no reachable server. Configuration and
    // native-name derivation failures for valid targets must remain errors.
    if LocalEndpoint::validate_address(address).is_err() {
        return Ok(false);
    }
    c2_ipc::ping_with_context(address, &context, timeout)
        .map_err(|error| LifecycleError::Configuration(error.to_string()))
}

/// Initiate direct IPC shutdown without waiting for the owner-side drain.
/// All retries retain the supplied context, or one process-default snapshot.
pub fn shutdown_direct_ipc(
    address: &str,
    context: Option<&LocalEndpointContext>,
    timeout: Duration,
) -> Result<DirectIpcShutdownOutcome, LifecycleError> {
    let context = context.cloned().map_or_else(process_context, Ok)?;
    if LocalEndpoint::validate_address(address).is_err() {
        return Ok(DirectIpcShutdownOutcome {
            acknowledged: false,
            shutdown_started: false,
            server_stopped: false,
            route_outcomes: Vec::new(),
        });
    }
    c2_ipc::shutdown_with_context(address, &context, timeout)
        .map(DirectIpcShutdownOutcome::from)
        .map_err(|error| LifecycleError::Configuration(error.to_string()))
}

impl Runtime {
    /// Probe inside this Runtime's frozen local domain. Does not freeze memory policy.
    pub fn ping_direct_ipc(
        &self,
        address: &str,
        timeout: Duration,
    ) -> Result<bool, LifecycleError> {
        ping_direct_ipc(
            address,
            Some(&self.freeze_local_endpoint_context()?),
            timeout,
        )
    }

    /// Initiate admin shutdown inside this Runtime's frozen local domain.
    /// The acknowledgement is initiation only; Host shutdown observes completion.
    pub fn shutdown_direct_ipc(
        &self,
        address: &str,
        timeout: Duration,
    ) -> Result<DirectIpcShutdownOutcome, LifecycleError> {
        shutdown_direct_ipc(
            address,
            Some(&self.freeze_local_endpoint_context()?),
            timeout,
        )
    }
}

impl From<c2_ipc::DirectShutdownAck> for DirectIpcShutdownOutcome {
    fn from(outcome: c2_ipc::DirectShutdownAck) -> Self {
        Self {
            acknowledged: outcome.acknowledged,
            shutdown_started: outcome.shutdown_started,
            server_stopped: outcome.server_stopped,
            route_outcomes: outcome
                .route_outcomes
                .into_iter()
                .map(|route| DirectIpcShutdownRouteOutcome {
                    route_name: route.route_name,
                    active_drained: route.active_drained,
                    closed_reason: route.closed_reason,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_address_is_a_core_configuration_error() {
        let error = direct_ipc_endpoint("tcp://not-ipc", None)
            .expect_err("non-IPC address must be rejected");
        assert!(matches!(error, LifecycleError::Configuration(_)));
    }

    #[test]
    fn absent_server_probe_is_false() {
        let address = format!("ipc://core-control-absent-{}", std::process::id());
        #[cfg(unix)]
        let context = {
            use std::os::unix::fs::DirBuilderExt;
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .unwrap()
                .join("target/control-test-ipc")
                .join(std::process::id().to_string());
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&root)
                .unwrap();
            LocalEndpointContext::with_unix_root(&root).unwrap()
        };
        #[cfg(windows)]
        let context = LocalEndpointContext::default_for_platform().unwrap();
        direct_ipc_endpoint(&address, Some(&context)).expect("valid address");

        assert!(
            !ping_direct_ipc(&address, Some(&context), Duration::from_millis(10))
                .expect("absent server is not an error")
        );
    }

    #[test]
    fn malformed_probe_targets_are_negative_results() {
        let context = LocalEndpointContext::default_for_platform().unwrap();
        for address in [
            "tcp://not-ipc",
            "ipc://",
            "ipc://../escape",
            "ipc://bad/name",
            "ipc://bad\\name",
            "ipc://.",
            "ipc://..",
            "ipc:// leading",
            "ipc://trailing ",
            "ipc://bad\nname",
        ] {
            assert!(!ping_direct_ipc(address, Some(&context), Duration::ZERO).unwrap());
            let outcome = shutdown_direct_ipc(address, Some(&context), Duration::ZERO).unwrap();
            assert!(!outcome.acknowledged);
            assert!(!outcome.shutdown_started);
            assert!(!outcome.server_stopped);
            assert!(outcome.route_outcomes.is_empty());
            assert!(direct_ipc_endpoint(address, Some(&context)).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn probes_accept_a_long_directory_and_report_an_unconfirmed_target() {
        let root = format!("/tmp/{}", "x".repeat(200));
        let context = LocalEndpointContext::with_unix_root(std::path::Path::new(&root)).unwrap();
        let runtime = Runtime::new(crate::RuntimeOptions::default()).unwrap();
        runtime
            .set_local_endpoint_with_sources(
                LocalEndpointOptions {
                    unix_root: Some(root.into()),
                },
                ConfigSources::empty(),
            )
            .unwrap();
        for result in [
            ping_direct_ipc("ipc://capacity", Some(&context), Duration::ZERO).map(|_| ()),
            shutdown_direct_ipc("ipc://capacity", Some(&context), Duration::ZERO).map(|_| ()),
            runtime
                .ping_direct_ipc("ipc://capacity", Duration::ZERO)
                .map(|_| ()),
            runtime
                .shutdown_direct_ipc("ipc://capacity", Duration::ZERO)
                .map(|_| ()),
        ] {
            assert!(result.is_ok());
        }
        assert!(!ping_direct_ipc("ipc://capacity", Some(&context), Duration::ZERO).unwrap());
        let outcome =
            shutdown_direct_ipc("ipc://capacity", Some(&context), Duration::ZERO).unwrap();
        assert!(!outcome.acknowledged);
        assert!(!outcome.shutdown_started);
        assert!(!outcome.server_stopped);
    }
}
