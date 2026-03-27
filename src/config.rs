use serde::Deserialize;

use crate::error::Result;

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
pub fn load_config(_path: &str) -> Result<Config> {
    todo!("Issue #2: implement config loading")
}
