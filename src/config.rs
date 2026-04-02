use serde::Deserialize;

use crate::error::{PurserError, Result};

/// Top-level daemon configuration loaded from config.toml.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub relays: Vec<String>,
    pub merchant_npub: String,
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub polling: PollingConfig,
    #[serde(default)]
    pub rate_limits: RateLimitConfig,
    #[serde(default)]
    pub mdk: MdkConfig,
    #[serde(default)]
    pub storage: StorageConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    #[serde(rename = "type")]
    pub provider_type: String,
    pub methods: Vec<String>,
    pub api_key_env: String,
    #[serde(default)]
    pub location_id_env: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PollingConfig {
    #[serde(default = "default_initial_interval_secs")]
    pub initial_interval_secs: u64,
    #[serde(default = "default_max_interval_secs")]
    pub max_interval_secs: u64,
    #[serde(default = "default_payment_expiry_secs")]
    pub default_payment_expiry_secs: u64,
    #[serde(default = "default_partial_payment_margin")]
    pub partial_payment_margin_percent: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitConfig {
    #[serde(default = "default_max_orders_per_hour")]
    pub max_orders_per_hour: u32,
    #[serde(default = "default_max_failures_per_day")]
    pub max_failures_per_day: u32,
    #[serde(default = "default_block_duration_hours")]
    pub block_duration_hours: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MdkConfig {
    #[serde(default = "default_storage_type")]
    pub storage_type: String, // "sqlite" or "memory"
    #[serde(default = "default_key_package_interval_hours")]
    pub key_package_interval_hours: u64,
    #[serde(default = "default_group_purge_days")]
    pub group_purge_days: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_db_path")]
    pub db_path: String,
    #[serde(default = "default_db_size_warn_mb")]
    pub db_size_warn_mb: u64,
}

// Defaults
fn default_initial_interval_secs() -> u64 { 10 }
fn default_max_interval_secs() -> u64 { 300 }
fn default_payment_expiry_secs() -> u64 { 900 } // 15 minutes
fn default_partial_payment_margin() -> f64 { 2.0 }
fn default_max_orders_per_hour() -> u32 { 10 }
fn default_max_failures_per_day() -> u32 { 3 }
fn default_block_duration_hours() -> u64 { 24 }
fn default_storage_type() -> String { "sqlite".to_string() }
fn default_key_package_interval_hours() -> u64 { 6 }
fn default_group_purge_days() -> u64 { 7 }
fn default_db_path() -> String { "purser.db".to_string() }
fn default_db_size_warn_mb() -> u64 { 500 }

impl Default for PollingConfig {
    fn default() -> Self {
        Self {
            initial_interval_secs: default_initial_interval_secs(),
            max_interval_secs: default_max_interval_secs(),
            default_payment_expiry_secs: default_payment_expiry_secs(),
            partial_payment_margin_percent: default_partial_payment_margin(),
        }
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_orders_per_hour: default_max_orders_per_hour(),
            max_failures_per_day: default_max_failures_per_day(),
            block_duration_hours: default_block_duration_hours(),
        }
    }
}

impl Default for MdkConfig {
    fn default() -> Self {
        Self {
            storage_type: default_storage_type(),
            key_package_interval_hours: default_key_package_interval_hours(),
            group_purge_days: default_group_purge_days(),
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
            db_size_warn_mb: default_db_size_warn_mb(),
        }
    }
}

/// Load configuration from config.toml and .env.
///
/// 1. Reads the TOML file at `path`
/// 2. Loads `.env` (non-fatal if missing)
/// 3. Validates that every provider's `api_key_env` resolves in the environment
/// 4. Validates that `relays` is non-empty
/// 5. Validates that `merchant_npub` is non-empty
pub fn load_config(path: &str) -> Result<Config> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| PurserError::Config(format!("failed to read config file '{}': {}", path, e)))?;

    let config: Config = toml::from_str(&content)
        .map_err(|e| PurserError::Config(format!("failed to parse config TOML: {}", e)))?;

    // Load .env — non-fatal if missing
    dotenvy::dotenv().ok();

    // Validate provider env vars
    for provider in &config.providers {
        std::env::var(&provider.api_key_env).map_err(|_| {
            PurserError::Config(format!(
                "environment variable '{}' required by provider '{}' is not set",
                provider.api_key_env, provider.provider_type
            ))
        })?;
    }

    // Validate relays non-empty
    if config.relays.is_empty() {
        return Err(PurserError::Config("relays list must not be empty".to_string()));
    }

    // Validate merchant_npub non-empty
    if config.merchant_npub.is_empty() {
        return Err(PurserError::Config("merchant_npub must not be empty".to_string()));
    }

    tracing::info!(
        relay_count = config.relays.len(),
        provider_count = config.providers.len(),
        storage_type = %config.mdk.storage_type,
        "loaded configuration"
    );

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn valid_config_toml() -> &'static str {
        r#"
merchant_npub = "npub1testkey"
relays = ["wss://relay.damus.io", "wss://nos.lol"]

[[providers]]
type = "square"
methods = ["fiat"]
api_key_env = "TEST_SQUARE_KEY"

[[providers]]
type = "strike"
methods = ["lightning"]
api_key_env = "TEST_STRIKE_KEY"
"#
    }

    #[test]
    fn test_load_valid_config() {
        let dir = std::env::temp_dir().join(format!("purser_test_config_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.toml");
        let mut f = std::fs::File::create(&config_path).unwrap();
        f.write_all(valid_config_toml().as_bytes()).unwrap();

        // Set required env vars (unsafe in Rust 2024 edition)
        unsafe {
            std::env::set_var("TEST_SQUARE_KEY", "sk_test_123");
            std::env::set_var("TEST_STRIKE_KEY", "strike_test_456");
        }

        let config = load_config(config_path.to_str().unwrap()).unwrap();
        assert_eq!(config.relays.len(), 2);
        assert_eq!(config.providers.len(), 2);
        assert_eq!(config.merchant_npub, "npub1testkey");
        // Verify defaults are applied
        assert_eq!(config.polling.initial_interval_secs, 10);
        assert_eq!(config.mdk.storage_type, "sqlite");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Criteria #14: Unknown provider type is caught at config time.
    /// Note: The actual rejection happens in main::init_providers, but we
    /// test here that the config itself loads (it doesn't validate provider type)
    /// and the type value is preserved for init_providers to reject.
    #[test]
    fn test_config_preserves_unknown_provider_type() {
        let dir = std::env::temp_dir().join(format!("purser_test_unk_provider_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.toml");
        let mut f = std::fs::File::create(&config_path).unwrap();
        f.write_all(
            br#"
merchant_npub = "npub1test"
relays = ["wss://relay.example"]

[[providers]]
type = "nonexistent"
methods = ["fiat"]
api_key_env = "TEST_NONEXIST_KEY"
"#,
        )
        .unwrap();

        unsafe {
            std::env::set_var("TEST_NONEXIST_KEY", "fake_key");
        }

        let config = load_config(config_path.to_str().unwrap()).unwrap();
        assert_eq!(config.providers[0].provider_type, "nonexistent");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_load_config_missing_relays() {
        let dir = std::env::temp_dir().join(format!("purser_test_no_relays_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.toml");
        let mut f = std::fs::File::create(&config_path).unwrap();
        f.write_all(
            br#"
merchant_npub = "npub1test"
relays = []
providers = []
"#,
        )
        .unwrap();

        let result = load_config(config_path.to_str().unwrap());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("relays"), "expected relays error, got: {}", err);

        std::fs::remove_dir_all(&dir).ok();
    }
}
