use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::catalog::Product;
use crate::config::Config;
use crate::providers::PaymentMethod;

/// Central application state shared across async tasks.
pub struct AppState {
    pub config: Config,
    pub catalog: Vec<Product>,
    pub pending_payments: RwLock<HashMap<String, PendingPayment>>,
    pub providers: Vec<Arc<dyn crate::providers::PaymentProvider>>,
}

/// A payment that has been created but not yet confirmed/expired.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingPayment {
    pub order_id: String,
    pub customer_pubkey: String,
    pub provider_name: String,
    pub payment_id: String,
    pub payment_method: PaymentMethod,
    pub amount: String,
    pub currency: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub status: PendingPaymentStatus,
    /// MDK group identifier for this checkout session.
    pub group_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingPaymentStatus {
    AwaitingPayment,
    PendingQuote,
}
