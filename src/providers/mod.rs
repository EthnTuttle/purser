pub mod square;
pub mod strike;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

use crate::error::Result;

// ---------------------------------------------------------------------------
// §2.3 — PaymentProvider trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait PaymentProvider: Send + Sync {
    /// Unique identifier, e.g. "square", "strike"
    fn name(&self) -> &str;

    /// Payment methods this provider handles, e.g. ["fiat"], ["lightning"]
    fn supported_methods(&self) -> Vec<PaymentMethod>;

    /// Create a payment or invoice from a validated order.
    /// Returns provider-specific payment details (URLs, invoices, etc.).
    async fn create_payment(&self, order: &ValidatedOrder) -> Result<ProviderPaymentRequest>;

    /// Check the current status of a previously created payment.
    async fn check_status(&self, payment_id: &str) -> Result<PaymentStatus>;

    /// Cancel or deactivate a payment (e.g. on expiry timeout).
    async fn cancel_payment(&self, payment_id: &str) -> Result<()>;

    /// Provider-specific polling configuration.
    fn poll_config(&self) -> PollConfig;
}

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum PaymentMethod {
    Fiat,
    Lightning,
    Onchain,
    Ecash,
}

impl PaymentMethod {
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "fiat" => Some(Self::Fiat),
            "lightning" => Some(Self::Lightning),
            "onchain" => Some(Self::Onchain),
            "ecash" => Some(Self::Ecash),
            _ => None,
        }
    }
}

/// A validated order ready for payment creation.
/// Produced by the validation layer, consumed by providers.
#[derive(Debug, Clone)]
pub struct ValidatedOrder {
    pub order_id: String,
    pub customer_pubkey: String,
    pub items: Vec<ValidatedOrderItem>,
    pub order_type: crate::messages::OrderType,
    pub shipping: Option<crate::messages::Shipping>,
    pub service_details: Option<crate::messages::ServiceDetails>,
    pub contact: Option<String>,
    pub payment_method: PaymentMethod,
    pub total_usd: String,
    pub currency: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ValidatedOrderItem {
    pub product_id: String,
    pub name: String,
    pub quantity: u32,
    pub variant: Option<String>,
    pub unit_price_usd: String,
}

/// Response from a provider after creating a payment.
#[derive(Debug, Clone)]
pub struct ProviderPaymentRequest {
    pub payment_id: String,
    pub payment_details: HashMap<String, String>,
    pub amount: String,
    pub currency: String,
    pub expires_at: DateTime<Utc>,
}

/// Status returned by check_status().
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaymentStatus {
    Pending,
    Completed {
        amount_paid: String,
        lightning_preimage: Option<String>,
    },
    Failed {
        reason: String,
    },
    Expired,
}

/// Provider-specific polling configuration.
#[derive(Debug, Clone)]
pub struct PollConfig {
    /// Initial polling interval after payment creation.
    pub initial_interval: Duration,
    /// Back-off multiplier applied after each poll with no status change.
    pub backoff_multiplier: f64,
    /// Maximum polling interval.
    pub max_interval: Duration,
    /// Strategy for rate limit adaptation.
    pub rate_limit_strategy: RateLimitStrategy,
}

#[derive(Debug, Clone)]
pub enum RateLimitStrategy {
    /// Monitor a response header; double interval if remaining falls below threshold percentage.
    HeaderMonitor {
        header_name: String,
        threshold_percent: f64,
    },
    /// Fixed budget: max N requests per time window.
    FixedBudget {
        max_requests: u32,
        window: Duration,
    },
    /// No special rate limit handling.
    None,
}
