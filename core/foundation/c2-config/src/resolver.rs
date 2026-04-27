use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::{BaseIpcConfig, ClientIpcConfig, RelayConfig, ServerIpcConfig};

pub type EnvMap = BTreeMap<String, String>;

#[derive(Debug, Clone)]
pub enum EnvFilePolicy {
    FromC2EnvFile,
    Disabled,
    Path(PathBuf),
}

#[derive(Debug, Clone)]
pub struct ConfigSources {
    pub env_file: EnvFilePolicy,
    pub process_env: EnvMap,
}

impl ConfigSources {
    pub fn empty() -> Self {
        Self {
            env_file: EnvFilePolicy::Disabled,
            process_env: EnvMap::new(),
        }
    }

    pub fn from_process() -> Self {
        Self {
            env_file: EnvFilePolicy::FromC2EnvFile,
            process_env: std::env::vars().collect(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct BaseIpcConfigOverrides {
    pub pool_enabled: Option<bool>,
    pub pool_segment_size: Option<u64>,
    pub max_pool_segments: Option<u32>,
    pub reassembly_segment_size: Option<u64>,
    pub reassembly_max_segments: Option<u32>,
    pub max_total_chunks: Option<u32>,
    pub chunk_gc_interval_secs: Option<f64>,
    pub chunk_threshold_ratio: Option<f64>,
    pub chunk_assembler_timeout_secs: Option<f64>,
    pub max_reassembly_bytes: Option<u64>,
    pub chunk_size: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct ServerIpcConfigOverrides {
    pub base: BaseIpcConfigOverrides,
    pub shm_threshold: Option<u64>,
    pub pool_enabled: Option<bool>,
    pub pool_segment_size: Option<u64>,
    pub max_pool_segments: Option<u32>,
    pub reassembly_segment_size: Option<u64>,
    pub reassembly_max_segments: Option<u32>,
    pub max_total_chunks: Option<u32>,
    pub chunk_gc_interval_secs: Option<f64>,
    pub chunk_threshold_ratio: Option<f64>,
    pub chunk_assembler_timeout_secs: Option<f64>,
    pub max_reassembly_bytes: Option<u64>,
    pub chunk_size: Option<u64>,
    pub max_frame_size: Option<u64>,
    pub max_payload_size: Option<u64>,
    pub max_pending_requests: Option<u32>,
    pub pool_decay_seconds: Option<f64>,
    pub heartbeat_interval_secs: Option<f64>,
    pub heartbeat_timeout_secs: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct ClientIpcConfigOverrides {
    pub base: BaseIpcConfigOverrides,
    pub shm_threshold: Option<u64>,
    pub pool_enabled: Option<bool>,
    pub pool_segment_size: Option<u64>,
    pub max_pool_segments: Option<u32>,
    pub reassembly_segment_size: Option<u64>,
    pub reassembly_max_segments: Option<u32>,
    pub max_total_chunks: Option<u32>,
    pub chunk_gc_interval_secs: Option<f64>,
    pub chunk_threshold_ratio: Option<f64>,
    pub chunk_assembler_timeout_secs: Option<f64>,
    pub max_reassembly_bytes: Option<u64>,
    pub chunk_size: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct RelayConfigOverrides {
    pub bind: Option<String>,
    pub relay_id: Option<String>,
    pub advertise_url: Option<String>,
    pub seeds: Option<Vec<String>>,
    pub idle_timeout_secs: Option<u64>,
    pub anti_entropy_interval_secs: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeConfigOverrides {
    pub relay_address: Option<String>,
    pub relay: RelayConfigOverrides,
    pub server_ipc: ServerIpcConfigOverrides,
    pub client_ipc: ClientIpcConfigOverrides,
    pub shm_threshold: Option<u64>,
    pub relay_use_proxy: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct ResolvedRuntimeConfig {
    pub relay_address: Option<String>,
    pub relay: RelayConfig,
    pub server_ipc: ServerIpcConfig,
    pub client_ipc: ClientIpcConfig,
    pub shm_threshold: u64,
    pub relay_use_proxy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    message: String,
}

impl ConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

pub struct ConfigResolver;

impl ConfigResolver {
    pub fn resolve(
        overrides: RuntimeConfigOverrides,
        sources: ConfigSources,
    ) -> Result<ResolvedRuntimeConfig, ConfigError> {
        let env = resolve_env(sources)?;
        reject_removed_env(&env)?;

        let shm_threshold = match overrides.shm_threshold {
            Some(value) => value,
            None => parse_optional_u64(&env, "C2_SHM_THRESHOLD")
                .transpose()?
                .unwrap_or(4096),
        };
        if shm_threshold == 0 {
            return Err(ConfigError::new("shm_threshold must be > 0"));
        }

        let relay_address = overrides
            .relay_address
            .or_else(|| optional_string(&env, "C2_RELAY_ADDRESS"));
        let relay_use_proxy = match overrides.relay_use_proxy {
            Some(value) => value,
            None => parse_optional_bool(&env, "C2_RELAY_USE_PROXY")
                .transpose()?
                .unwrap_or(false),
        };

        let relay = resolve_relay_config(&env, overrides.relay)?;
        let relay = RelayConfig {
            use_proxy: relay_use_proxy,
            ..relay
        };
        let server_ipc = resolve_server_ipc_config(&env, overrides.server_ipc, shm_threshold)?;
        let client_ipc = resolve_client_ipc_config(&env, overrides.client_ipc, shm_threshold)?;

        Ok(ResolvedRuntimeConfig {
            relay_address,
            relay,
            server_ipc,
            client_ipc,
            shm_threshold,
            relay_use_proxy,
        })
    }
}

fn resolve_env(sources: ConfigSources) -> Result<EnvMap, ConfigError> {
    let mut env = EnvMap::new();

    let env_file = match sources.env_file {
        EnvFilePolicy::Disabled => None,
        EnvFilePolicy::Path(path) => Some(path),
        EnvFilePolicy::FromC2EnvFile => {
            match sources.process_env.get("C2_ENV_FILE").map(|v| v.trim()) {
                Some("") => None,
                Some(path) => Some(PathBuf::from(path)),
                None => Some(PathBuf::from(".env")),
            }
        }
    };

    if let Some(path) = env_file {
        match dotenvy::from_path_iter(&path) {
            Ok(iter) => {
                for item in iter {
                    let (key, value) = item.map_err(|e| {
                        ConfigError::new(format!(
                            "failed to parse env file {}: {e}",
                            path.display()
                        ))
                    })?;
                    env.insert(key, value);
                }
            }
            Err(dotenvy::Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(ConfigError::new(format!(
                    "failed to load env file {}: {e}",
                    path.display(),
                )));
            }
        }
    }

    for (key, value) in sources.process_env {
        env.insert(key, value);
    }

    Ok(env)
}

fn reject_removed_env(env: &EnvMap) -> Result<(), ConfigError> {
    if env.contains_key("C2_IPC_MAX_POOL_MEMORY") {
        return Err(ConfigError::new(
            "C2_IPC_MAX_POOL_MEMORY is no longer supported; max_pool_memory is derived from C2_IPC_POOL_SEGMENT_SIZE * C2_IPC_MAX_POOL_SEGMENTS",
        ));
    }
    Ok(())
}

fn resolve_relay_config(
    env: &EnvMap,
    overrides: RelayConfigOverrides,
) -> Result<RelayConfig, ConfigError> {
    let mut cfg = RelayConfig::default();

    if let Some(v) = optional_string(env, "C2_RELAY_BIND") {
        cfg.bind = v;
    }
    if let Some(v) = optional_string(env, "C2_RELAY_ID") {
        cfg.relay_id = v;
    }
    if let Some(v) = optional_string(env, "C2_RELAY_ADVERTISE_URL") {
        cfg.advertise_url = v;
    }
    if let Some(v) = optional_string(env, "C2_RELAY_SEEDS") {
        cfg.seeds = parse_list(&v);
    }
    if let Some(v) = parse_optional_u64(env, "C2_RELAY_IDLE_TIMEOUT").transpose()? {
        cfg.idle_timeout_secs = v;
    }
    if let Some(v) = parse_optional_f64(env, "C2_RELAY_ANTI_ENTROPY_INTERVAL").transpose()? {
        cfg.anti_entropy_interval = duration_from_secs("C2_RELAY_ANTI_ENTROPY_INTERVAL", v)?;
    }

    if let Some(v) = clean_string(overrides.bind) {
        cfg.bind = v;
    }
    if let Some(v) = clean_string(overrides.relay_id) {
        cfg.relay_id = v;
    }
    if let Some(v) = clean_string(overrides.advertise_url) {
        cfg.advertise_url = v;
    }
    if let Some(v) = overrides.seeds {
        cfg.seeds = v;
    }
    if let Some(v) = overrides.idle_timeout_secs {
        cfg.idle_timeout_secs = v;
    }
    if let Some(v) = overrides.anti_entropy_interval_secs {
        cfg.anti_entropy_interval = duration_from_secs("anti_entropy_interval_secs", v)?;
    }

    Ok(cfg)
}

fn resolve_server_ipc_config(
    env: &EnvMap,
    overrides: ServerIpcConfigOverrides,
    shm_threshold: u64,
) -> Result<ServerIpcConfig, ConfigError> {
    let mut cfg = ServerIpcConfig::default();
    cfg.shm_threshold = shm_threshold;

    apply_base_env(&mut cfg.base, env)?;
    if let Some(v) = parse_optional_u64(env, "C2_IPC_MAX_FRAME_SIZE").transpose()? {
        cfg.max_frame_size = v;
    }
    if let Some(v) = parse_optional_u64(env, "C2_IPC_MAX_PAYLOAD_SIZE").transpose()? {
        cfg.max_payload_size = v;
    }
    if let Some(v) = parse_optional_u32(env, "C2_IPC_MAX_PENDING_REQUESTS").transpose()? {
        cfg.max_pending_requests = v;
    }
    if let Some(v) = parse_optional_f64(env, "C2_IPC_POOL_DECAY_SECONDS").transpose()? {
        cfg.pool_decay_seconds = v;
    }
    if let Some(v) = parse_optional_f64(env, "C2_IPC_HEARTBEAT_INTERVAL").transpose()? {
        cfg.heartbeat_interval_secs = v;
    }
    if let Some(v) = parse_optional_f64(env, "C2_IPC_HEARTBEAT_TIMEOUT").transpose()? {
        cfg.heartbeat_timeout_secs = v;
    }

    apply_base_overrides(&mut cfg.base, &overrides.base);
    apply_flat_base_overrides_to_server(&mut cfg, &overrides);
    if let Some(v) = overrides.shm_threshold {
        cfg.shm_threshold = v;
    }
    if let Some(v) = overrides.max_frame_size {
        cfg.max_frame_size = v;
    }
    if let Some(v) = overrides.max_payload_size {
        cfg.max_payload_size = v;
    }
    if let Some(v) = overrides.max_pending_requests {
        cfg.max_pending_requests = v;
    }
    if let Some(v) = overrides.pool_decay_seconds {
        cfg.pool_decay_seconds = v;
    }
    if let Some(v) = overrides.heartbeat_interval_secs {
        cfg.heartbeat_interval_secs = v;
    }
    if let Some(v) = overrides.heartbeat_timeout_secs {
        cfg.heartbeat_timeout_secs = v;
    }

    derive_base(&mut cfg.base)?;
    cfg.validate().map_err(ConfigError::new)?;
    Ok(cfg)
}

fn resolve_client_ipc_config(
    env: &EnvMap,
    overrides: ClientIpcConfigOverrides,
    shm_threshold: u64,
) -> Result<ClientIpcConfig, ConfigError> {
    let mut cfg = ClientIpcConfig::default();
    cfg.shm_threshold = shm_threshold;

    apply_base_env(&mut cfg.base, env)?;
    apply_base_overrides(&mut cfg.base, &overrides.base);
    apply_flat_base_overrides_to_client(&mut cfg, &overrides);
    if let Some(v) = overrides.shm_threshold {
        cfg.shm_threshold = v;
    }

    derive_base(&mut cfg.base)?;
    cfg.validate().map_err(ConfigError::new)?;
    Ok(cfg)
}

fn apply_base_env(cfg: &mut BaseIpcConfig, env: &EnvMap) -> Result<(), ConfigError> {
    if let Some(v) = parse_optional_bool(env, "C2_IPC_POOL_ENABLED").transpose()? {
        cfg.pool_enabled = v;
    }
    if let Some(v) = parse_optional_u64(env, "C2_IPC_POOL_SEGMENT_SIZE").transpose()? {
        cfg.pool_segment_size = v;
    }
    if let Some(v) = parse_optional_u32(env, "C2_IPC_MAX_POOL_SEGMENTS").transpose()? {
        cfg.max_pool_segments = v;
    }
    if let Some(v) = parse_optional_u64(env, "C2_IPC_REASSEMBLY_SEGMENT_SIZE").transpose()? {
        cfg.reassembly_segment_size = v;
    }
    if let Some(v) = parse_optional_u32(env, "C2_IPC_REASSEMBLY_MAX_SEGMENTS").transpose()? {
        cfg.reassembly_max_segments = v;
    }
    if let Some(v) = parse_optional_u32(env, "C2_IPC_MAX_TOTAL_CHUNKS").transpose()? {
        cfg.max_total_chunks = v;
    }
    if let Some(v) = parse_optional_f64(env, "C2_IPC_CHUNK_GC_INTERVAL").transpose()? {
        cfg.chunk_gc_interval_secs = v;
    }
    if let Some(v) = parse_optional_f64(env, "C2_IPC_CHUNK_THRESHOLD_RATIO").transpose()? {
        cfg.chunk_threshold_ratio = v;
    }
    if let Some(v) = parse_optional_f64(env, "C2_IPC_CHUNK_ASSEMBLER_TIMEOUT").transpose()? {
        cfg.chunk_assembler_timeout_secs = v;
    }
    if let Some(v) = parse_optional_u64(env, "C2_IPC_MAX_REASSEMBLY_BYTES").transpose()? {
        cfg.max_reassembly_bytes = v;
    }
    if let Some(v) = parse_optional_u64(env, "C2_IPC_CHUNK_SIZE").transpose()? {
        cfg.chunk_size = v;
    }
    Ok(())
}

fn apply_base_overrides(cfg: &mut BaseIpcConfig, overrides: &BaseIpcConfigOverrides) {
    if let Some(v) = overrides.pool_enabled {
        cfg.pool_enabled = v;
    }
    if let Some(v) = overrides.pool_segment_size {
        cfg.pool_segment_size = v;
    }
    if let Some(v) = overrides.max_pool_segments {
        cfg.max_pool_segments = v;
    }
    if let Some(v) = overrides.reassembly_segment_size {
        cfg.reassembly_segment_size = v;
    }
    if let Some(v) = overrides.reassembly_max_segments {
        cfg.reassembly_max_segments = v;
    }
    if let Some(v) = overrides.max_total_chunks {
        cfg.max_total_chunks = v;
    }
    if let Some(v) = overrides.chunk_gc_interval_secs {
        cfg.chunk_gc_interval_secs = v;
    }
    if let Some(v) = overrides.chunk_threshold_ratio {
        cfg.chunk_threshold_ratio = v;
    }
    if let Some(v) = overrides.chunk_assembler_timeout_secs {
        cfg.chunk_assembler_timeout_secs = v;
    }
    if let Some(v) = overrides.max_reassembly_bytes {
        cfg.max_reassembly_bytes = v;
    }
    if let Some(v) = overrides.chunk_size {
        cfg.chunk_size = v;
    }
}

fn apply_flat_base_overrides_to_server(
    cfg: &mut ServerIpcConfig,
    overrides: &ServerIpcConfigOverrides,
) {
    if let Some(v) = overrides.pool_enabled {
        cfg.base.pool_enabled = v;
    }
    if let Some(v) = overrides.pool_segment_size {
        cfg.base.pool_segment_size = v;
    }
    if let Some(v) = overrides.max_pool_segments {
        cfg.base.max_pool_segments = v;
    }
    if let Some(v) = overrides.reassembly_segment_size {
        cfg.base.reassembly_segment_size = v;
    }
    if let Some(v) = overrides.reassembly_max_segments {
        cfg.base.reassembly_max_segments = v;
    }
    if let Some(v) = overrides.max_total_chunks {
        cfg.base.max_total_chunks = v;
    }
    if let Some(v) = overrides.chunk_gc_interval_secs {
        cfg.base.chunk_gc_interval_secs = v;
    }
    if let Some(v) = overrides.chunk_threshold_ratio {
        cfg.base.chunk_threshold_ratio = v;
    }
    if let Some(v) = overrides.chunk_assembler_timeout_secs {
        cfg.base.chunk_assembler_timeout_secs = v;
    }
    if let Some(v) = overrides.max_reassembly_bytes {
        cfg.base.max_reassembly_bytes = v;
    }
    if let Some(v) = overrides.chunk_size {
        cfg.base.chunk_size = v;
    }
}

fn apply_flat_base_overrides_to_client(
    cfg: &mut ClientIpcConfig,
    overrides: &ClientIpcConfigOverrides,
) {
    if let Some(v) = overrides.pool_enabled {
        cfg.base.pool_enabled = v;
    }
    if let Some(v) = overrides.pool_segment_size {
        cfg.base.pool_segment_size = v;
    }
    if let Some(v) = overrides.max_pool_segments {
        cfg.base.max_pool_segments = v;
    }
    if let Some(v) = overrides.reassembly_segment_size {
        cfg.base.reassembly_segment_size = v;
    }
    if let Some(v) = overrides.reassembly_max_segments {
        cfg.base.reassembly_max_segments = v;
    }
    if let Some(v) = overrides.max_total_chunks {
        cfg.base.max_total_chunks = v;
    }
    if let Some(v) = overrides.chunk_gc_interval_secs {
        cfg.base.chunk_gc_interval_secs = v;
    }
    if let Some(v) = overrides.chunk_threshold_ratio {
        cfg.base.chunk_threshold_ratio = v;
    }
    if let Some(v) = overrides.chunk_assembler_timeout_secs {
        cfg.base.chunk_assembler_timeout_secs = v;
    }
    if let Some(v) = overrides.max_reassembly_bytes {
        cfg.base.max_reassembly_bytes = v;
    }
    if let Some(v) = overrides.chunk_size {
        cfg.base.chunk_size = v;
    }
}

fn derive_base(cfg: &mut BaseIpcConfig) -> Result<(), ConfigError> {
    cfg.max_pool_memory = cfg
        .pool_segment_size
        .checked_mul(u64::from(cfg.max_pool_segments))
        .ok_or_else(|| ConfigError::new("max_pool_memory derived value overflowed"))?;
    Ok(())
}

fn optional_string(env: &EnvMap, key: &str) -> Option<String> {
    env.get(key).and_then(|value| {
        let value = value.trim();
        if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        }
    })
}

fn clean_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        if value.is_empty() { None } else { Some(value) }
    })
}

fn parse_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_optional_u64(env: &EnvMap, key: &str) -> Option<Result<u64, ConfigError>> {
    optional_string(env, key).map(|value| {
        value
            .parse::<u64>()
            .map_err(|e| ConfigError::new(format!("{key} must be an unsigned integer: {e}")))
    })
}

fn parse_optional_u32(env: &EnvMap, key: &str) -> Option<Result<u32, ConfigError>> {
    optional_string(env, key).map(|value| {
        value
            .parse::<u32>()
            .map_err(|e| ConfigError::new(format!("{key} must be an unsigned integer: {e}")))
    })
}

fn parse_optional_f64(env: &EnvMap, key: &str) -> Option<Result<f64, ConfigError>> {
    optional_string(env, key).map(|value| {
        let parsed = value
            .parse::<f64>()
            .map_err(|e| ConfigError::new(format!("{key} must be a number: {e}")))?;
        validate_finite(key, parsed)
    })
}

fn parse_optional_bool(env: &EnvMap, key: &str) -> Option<Result<bool, ConfigError>> {
    optional_string(env, key).map(|value| match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Ok(true),
        "0" | "false" | "no" => Ok(false),
        _ => Err(ConfigError::new(format!(
            "{key} must be a boolean (1/0, true/false, yes/no)"
        ))),
    })
}

fn duration_from_secs(name: &str, secs: f64) -> Result<Duration, ConfigError> {
    let secs = validate_finite(name, secs)?;
    if secs < 0.0 {
        return Err(ConfigError::new(format!("{name} must be >= 0")));
    }
    Ok(Duration::from_secs_f64(secs))
}

fn validate_finite(name: &str, value: f64) -> Result<f64, ConfigError> {
    if !value.is_finite() {
        return Err(ConfigError::new(format!("{name} must be finite")));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn env(entries: &[(&str, &str)]) -> EnvMap {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn resolver_uses_canonical_defaults() {
        let resolved =
            ConfigResolver::resolve(RuntimeConfigOverrides::default(), ConfigSources::empty())
                .expect("defaults should resolve");

        assert_eq!(resolved.shm_threshold, 4096);
        assert_eq!(
            resolved.server_ipc.reassembly_segment_size,
            64 * 1024 * 1024
        );
        assert_eq!(
            resolved.client_ipc.reassembly_segment_size,
            64 * 1024 * 1024
        );
        assert_eq!(resolved.server_ipc.max_pool_memory, 268_435_456 * 4);
        assert_eq!(resolved.client_ipc.max_pool_memory, 268_435_456 * 4);
        assert_eq!(resolved.relay.bind, "0.0.0.0:8080");
        assert_eq!(resolved.relay.idle_timeout_secs, 0);
        assert!(!resolved.relay_use_proxy);
    }

    #[test]
    fn code_overrides_beat_process_env_which_beats_env_file() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let env_file = tempdir.path().join(".env");
        fs::write(
            &env_file,
            [
                "C2_SHM_THRESHOLD=8192",
                "C2_IPC_POOL_SEGMENT_SIZE=1048576",
                "C2_RELAY_BIND=127.0.0.1:7000",
            ]
            .join("\n"),
        )
        .expect("write env file");

        let mut overrides = RuntimeConfigOverrides::default();
        overrides.shm_threshold = Some(16_384);
        overrides.server_ipc.pool_segment_size = Some(4_194_304);

        let sources = ConfigSources {
            env_file: EnvFilePolicy::Path(env_file),
            process_env: env(&[
                ("C2_SHM_THRESHOLD", "12288"),
                ("C2_IPC_POOL_SEGMENT_SIZE", "2097152"),
                ("C2_RELAY_BIND", "127.0.0.1:8000"),
            ]),
        };

        let resolved = ConfigResolver::resolve(overrides, sources).expect("resolve");

        assert_eq!(resolved.shm_threshold, 16_384);
        assert_eq!(resolved.server_ipc.pool_segment_size, 4_194_304);
        assert_eq!(resolved.server_ipc.max_pool_memory, 4_194_304 * 4);
        assert_eq!(resolved.relay.bind, "127.0.0.1:8000");
    }

    #[test]
    fn env_file_empty_string_disables_env_file_loading() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        fs::write(tempdir.path().join(".env"), "C2_SHM_THRESHOLD=8192\n").expect("write env file");

        let sources = ConfigSources {
            env_file: EnvFilePolicy::FromC2EnvFile,
            process_env: env(&[("C2_ENV_FILE", "")]),
        };

        let resolved =
            ConfigResolver::resolve(RuntimeConfigOverrides::default(), sources).expect("resolve");

        assert_eq!(resolved.shm_threshold, 4096);
    }

    #[test]
    fn max_pool_memory_env_is_rejected() {
        let sources = ConfigSources {
            env_file: EnvFilePolicy::Disabled,
            process_env: env(&[("C2_IPC_MAX_POOL_MEMORY", "1")]),
        };

        let err = ConfigResolver::resolve(RuntimeConfigOverrides::default(), sources)
            .expect_err("removed env var should fail");

        assert!(err.to_string().contains("C2_IPC_MAX_POOL_MEMORY"));
    }

    #[test]
    fn server_pool_segment_must_not_exceed_payload_limit() {
        let mut overrides = RuntimeConfigOverrides::default();
        overrides.server_ipc.pool_segment_size = Some(2 * 1024 * 1024);
        overrides.server_ipc.max_payload_size = Some(1024 * 1024);

        let err = ConfigResolver::resolve(overrides, ConfigSources::empty())
            .expect_err("oversized pool segment should fail");

        assert!(err.to_string().contains("pool_segment_size"));
        assert!(err.to_string().contains("max_payload_size"));
    }

    #[test]
    fn invalid_env_value_names_variable() {
        let sources = ConfigSources {
            env_file: EnvFilePolicy::Disabled,
            process_env: env(&[("C2_IPC_POOL_SEGMENT_SIZE", "not-a-number")]),
        };

        let err = ConfigResolver::resolve(RuntimeConfigOverrides::default(), sources)
            .expect_err("invalid value should fail");

        assert!(err.to_string().contains("C2_IPC_POOL_SEGMENT_SIZE"));
    }

    #[test]
    fn non_finite_float_env_value_is_rejected() {
        let sources = ConfigSources {
            env_file: EnvFilePolicy::Disabled,
            process_env: env(&[("C2_RELAY_ANTI_ENTROPY_INTERVAL", "NaN")]),
        };

        let err = ConfigResolver::resolve(RuntimeConfigOverrides::default(), sources)
            .expect_err("non-finite floats should fail");

        assert!(err.to_string().contains("C2_RELAY_ANTI_ENTROPY_INTERVAL"));
        assert!(err.to_string().contains("finite"));
    }

    #[test]
    fn non_finite_float_override_is_rejected() {
        let mut overrides = RuntimeConfigOverrides::default();
        overrides.server_ipc.chunk_gc_interval_secs = Some(f64::INFINITY);

        let err = ConfigResolver::resolve(overrides, ConfigSources::empty())
            .expect_err("non-finite override should fail");

        assert!(err.to_string().contains("chunk_gc_interval_secs"));
        assert!(err.to_string().contains("finite"));
    }

    #[test]
    fn relay_seeds_and_proxy_are_parsed() {
        let sources = ConfigSources {
            env_file: EnvFilePolicy::Disabled,
            process_env: env(&[
                ("C2_RELAY_SEEDS", " http://a:8080, ,http://b:8080 "),
                ("C2_RELAY_USE_PROXY", "yes"),
            ]),
        };

        let resolved =
            ConfigResolver::resolve(RuntimeConfigOverrides::default(), sources).expect("resolve");

        assert_eq!(
            resolved.relay.seeds,
            vec!["http://a:8080".to_string(), "http://b:8080".to_string()]
        );
        assert!(resolved.relay_use_proxy);
        assert!(resolved.relay.use_proxy);
    }
}
