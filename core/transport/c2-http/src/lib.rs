//! C-Two HTTP transport layer.
//!
//! - `client`: HTTP client for connecting to relay servers (always available)
//! - `relay`: HTTP relay server bridging HTTP→IPC (requires `relay` feature)

pub mod client;

#[cfg(feature = "relay")]
pub mod relay;

/// Should reqwest clients in c-two honor system HTTP_PROXY env vars?
///
/// Default: **false** — c-two relay traffic is private mesh infrastructure,
/// and routing it through a forward proxy is known to corrupt percent-encoded
/// path segments (e.g., `%2F` in resource names) and leak internal control
/// data. Set `C2_RELAY_USE_PROXY=1` to opt in to using the system proxy.
pub fn relay_use_proxy() -> bool {
    matches!(
        std::env::var("C2_RELAY_USE_PROXY")
            .ok()
            .as_deref()
            .map(str::trim),
        Some("1") | Some("true") | Some("True") | Some("TRUE") | Some("yes") | Some("YES"),
    )
}

/// Build a reqwest::ClientBuilder pre-configured for c-two relay traffic.
/// Applies `.no_proxy()` unless the user opted in via `C2_RELAY_USE_PROXY=1`.
pub fn relay_client_builder() -> reqwest::ClientBuilder {
    relay_client_builder_with_proxy(relay_use_proxy())
}

/// Build a relay client builder with an explicit proxy policy.
pub fn relay_client_builder_with_proxy(use_proxy: bool) -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder();
    if use_proxy {
        builder
    } else {
        builder.no_proxy()
    }
}
