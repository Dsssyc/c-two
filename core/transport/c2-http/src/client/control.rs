use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};

use super::client::{HttpError, runtime};

const CONTROL_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_RETRY_ATTEMPTS: usize = 3;
const DEFAULT_RETRY_DELAY: Duration = Duration::from_secs(1);
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(30);

fn encode_segment(s: &str) -> String {
    utf8_percent_encode(s, CONTROL_SEGMENT).to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ResolveCacheKey {
    Name(String),
    Typed {
        name: String,
        crm_ns: String,
        crm_name: String,
        crm_ver: String,
    },
}

impl ResolveCacheKey {
    fn route_name(&self) -> &str {
        match self {
            Self::Name(name)
            | Self::Typed {
                name,
                crm_ns: _,
                crm_name: _,
                crm_ver: _,
            } => name,
        }
    }
}

fn resolve_cache_key(name: &str, expected_crm: Option<(&str, &str, &str)>) -> ResolveCacheKey {
    match expected_crm {
        Some((crm_ns, crm_name, crm_ver)) => ResolveCacheKey::Typed {
            name: name.to_string(),
            crm_ns: crm_ns.to_string(),
            crm_name: crm_name.to_string(),
            crm_ver: crm_ver.to_string(),
        },
        None => ResolveCacheKey::Name(name.to_string()),
    }
}

fn canonical_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_owned()
}

#[derive(Debug, Clone, Serialize)]
struct RegisterRequest<'a> {
    name: &'a str,
    server_id: &'a str,
    server_instance_id: &'a str,
    address: &'a str,
    crm_ns: &'a str,
    crm_name: &'a str,
    crm_ver: &'a str,
}

#[derive(Debug, Clone, Serialize)]
struct UnregisterRequest<'a> {
    name: &'a str,
    server_id: &'a str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayRouteInfo {
    pub name: String,
    pub relay_url: String,
    pub ipc_address: Option<String>,
    pub server_id: Option<String>,
    pub server_instance_id: Option<String>,
    pub crm_ns: String,
    pub crm_name: String,
    pub crm_ver: String,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    routes: Vec<RelayRouteInfo>,
    inserted_at: Instant,
}

#[derive(Debug, Clone, Copy)]
pub struct RelayControlClientConfig {
    pub timeout: Duration,
    pub retry_attempts: usize,
    pub retry_delay: Duration,
    pub cache_ttl: Duration,
}

impl Default for RelayControlClientConfig {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            retry_attempts: DEFAULT_RETRY_ATTEMPTS,
            retry_delay: DEFAULT_RETRY_DELAY,
            cache_ttl: DEFAULT_CACHE_TTL,
        }
    }
}

pub struct RelayControlClient {
    client: reqwest::Client,
    base_url: String,
    config: RelayControlClientConfig,
    cache: Mutex<HashMap<ResolveCacheKey, CacheEntry>>,
}

impl RelayControlClient {
    pub fn new(base_url: &str, use_proxy: bool) -> Result<Self, HttpError> {
        Self::new_with_config(base_url, use_proxy, RelayControlClientConfig::default())
    }

    pub fn new_with_config(
        base_url: &str,
        use_proxy: bool,
        config: RelayControlClientConfig,
    ) -> Result<Self, HttpError> {
        let client = crate::relay_client_builder_with_proxy(use_proxy)
            .timeout(config.timeout)
            .build()
            .map_err(|e| HttpError::Transport(e.to_string()))?;
        Ok(Self {
            client,
            base_url: canonical_base_url(base_url),
            config,
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn register(
        &self,
        name: &str,
        server_id: &str,
        server_instance_id: &str,
        address: &str,
        crm_ns: &str,
        crm_name: &str,
        crm_ver: &str,
    ) -> Result<(), HttpError> {
        let request = RegisterRequest {
            name,
            server_id,
            server_instance_id,
            address,
            crm_ns,
            crm_name,
            crm_ver,
        };
        runtime().handle().block_on(self.post_json_with_retry(
            "/_register",
            &request,
            &[200, 201],
        ))?;
        self.invalidate(name);
        Ok(())
    }

    pub fn unregister(&self, name: &str, server_id: &str) -> Result<(), HttpError> {
        let request = UnregisterRequest { name, server_id };
        runtime().handle().block_on(self.post_json_with_retry(
            "/_unregister",
            &request,
            &[200, 204],
        ))?;
        self.invalidate(name);
        Ok(())
    }

    pub fn resolve(&self, name: &str) -> Result<Vec<RelayRouteInfo>, HttpError> {
        runtime().handle().block_on(self.resolve_async(name))
    }

    pub async fn resolve_async(&self, name: &str) -> Result<Vec<RelayRouteInfo>, HttpError> {
        self.resolve_with_query_async(name, None).await
    }

    pub async fn resolve_matching_async(
        &self,
        route_name: &str,
        crm_ns: &str,
        crm_name: &str,
        crm_ver: &str,
    ) -> Result<Vec<RelayRouteInfo>, HttpError> {
        self.resolve_with_query_async(route_name, Some((crm_ns, crm_name, crm_ver)))
            .await
    }

    async fn resolve_with_query_async(
        &self,
        name: &str,
        expected_crm: Option<(&str, &str, &str)>,
    ) -> Result<Vec<RelayRouteInfo>, HttpError> {
        let cache_key = resolve_cache_key(name, expected_crm);
        if let Some(routes) = self.cached(&cache_key) {
            return Ok(routes);
        }

        let path = match expected_crm {
            Some((crm_ns, crm_name, crm_ver)) => format!(
                "/_resolve/{}?crm_ns={}&crm_name={}&crm_ver={}",
                encode_segment(name),
                encode_segment(crm_ns),
                encode_segment(crm_name),
                encode_segment(crm_ver),
            ),
            None => format!("/_resolve/{}", encode_segment(name)),
        };
        let resp = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .send()
            .await
            .map_err(|e| HttpError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let routes = match status {
            200 => resp
                .json::<Vec<RelayRouteInfo>>()
                .await
                .map_err(|e| HttpError::Transport(e.to_string()))?,
            404 => {
                let text = resp.text().await.unwrap_or_default();
                return Err(HttpError::ServerError(404, text));
            }
            code => {
                let text = resp.text().await.unwrap_or_default();
                return Err(HttpError::ServerError(code, text));
            }
        };

        self.cache.lock().insert(
            cache_key,
            CacheEntry {
                routes: routes.clone(),
                inserted_at: Instant::now(),
            },
        );
        Ok(routes)
    }

    pub fn clear_cache(&self) {
        self.cache.lock().clear();
    }

    pub fn invalidate(&self, name: &str) {
        self.cache.lock().retain(|key, _| key.route_name() != name);
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    async fn post_json_with_retry<T>(
        &self,
        path: &str,
        payload: &T,
        success_codes: &[u16],
    ) -> Result<(), HttpError>
    where
        T: Serialize + ?Sized,
    {
        let attempts = self.config.retry_attempts.max(1);
        let mut last_server_error = None;
        for attempt in 0..attempts {
            match self.post_json_once(path, payload, success_codes).await {
                Ok(()) => return Ok(()),
                Err(HttpError::ServerError(code, body)) if code >= 500 => {
                    last_server_error = Some(HttpError::ServerError(code, body));
                }
                Err(HttpError::Transport(message)) => {
                    last_server_error = Some(HttpError::Transport(message));
                }
                Err(err) => return Err(err),
            }

            if attempt + 1 < attempts {
                tokio::time::sleep(self.config.retry_delay).await;
            }
        }
        Err(last_server_error
            .unwrap_or_else(|| HttpError::Transport("relay control request failed".to_string())))
    }

    async fn post_json_once<T>(
        &self,
        path: &str,
        payload: &T,
        success_codes: &[u16],
    ) -> Result<(), HttpError>
    where
        T: Serialize + ?Sized,
    {
        let resp = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .json(payload)
            .send()
            .await
            .map_err(|e| HttpError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if success_codes.contains(&status) {
            return Ok(());
        }
        let text = resp.text().await.unwrap_or_default();
        Err(HttpError::ServerError(status, text))
    }

    fn cached(&self, key: &ResolveCacheKey) -> Option<Vec<RelayRouteInfo>> {
        let mut cache = self.cache.lock();
        let entry = cache.get(key)?;
        if entry.inserted_at.elapsed() >= self.config.cache_ttl {
            cache.remove(key);
            return None;
        }
        Some(entry.routes.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_base_url() {
        let client = RelayControlClient::new(" http://relay.test// ", false).unwrap();
        assert_eq!(client.base_url(), "http://relay.test");
    }

    #[test]
    fn route_cache_key_distinguishes_name_only_from_typed_identity_with_delimiters() {
        assert_ne!(
            resolve_cache_key("grid\0test.ns\0Grid\00.1.0", None),
            resolve_cache_key("grid", Some(("test.ns", "Grid", "0.1.0"))),
        );
    }

    #[test]
    fn route_cache_can_be_invalidated() {
        let client = RelayControlClient::new("http://relay.test", false).unwrap();
        client.cache.lock().insert(
            resolve_cache_key("grid", None),
            CacheEntry {
                routes: vec![RelayRouteInfo {
                    name: "grid".into(),
                    relay_url: "http://relay-a.test".into(),
                    ipc_address: None,
                    server_id: None,
                    server_instance_id: None,
                    crm_ns: "".into(),
                    crm_name: "".into(),
                    crm_ver: "".into(),
                }],
                inserted_at: Instant::now(),
            },
        );

        assert_eq!(
            client.cached(&resolve_cache_key("grid", None)).unwrap()[0].relay_url,
            "http://relay-a.test"
        );
        client.invalidate("grid");
        assert!(client.cached(&resolve_cache_key("grid", None)).is_none());
    }

    #[cfg(feature = "relay")]
    #[test]
    fn resolve_maps_404_status_from_real_relay_router() {
        let (addr, server) = runtime().handle().block_on(async {
            let state = crate::relay::test_support::test_state_for_client();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let app = crate::relay::router::build_router(state);
            let server = tokio::spawn(async move {
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .await
                .unwrap();
            });
            (addr, server)
        });

        let client = RelayControlClient::new(&format!("http://{addr}"), false).unwrap();
        let err = match client.resolve("missing/name") {
            Ok(_) => panic!("missing route should return a status error"),
            Err(err) => err,
        };

        server.abort();
        match err {
            HttpError::ServerError(404, body) => {
                assert!(
                    body.contains("ResourceNotFound"),
                    "unexpected 404 body: {body}"
                );
            }
            other => panic!("unexpected error: {other}"),
        }
    }
}
