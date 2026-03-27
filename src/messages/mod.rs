pub mod validation;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Protocol version for all message schemas.
pub const PROTOCOL_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// 3.3.1 — order (customer → merchant)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub version: u32,
    #[serde(rename = "type")]
    pub msg_type: String, // "order"
    pub order_id: String,
    pub items: Vec<OrderItem>,
    pub order_type: OrderType,
    pub shipping: Option<Shipping>,
    pub service_details: Option<ServiceDetails>,
    pub contact: Option<String>,
    pub payment_method: String,
    pub currency: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderItem {
    pub product_id: String,
    pub name: String,
    pub quantity: u32,
    pub variant: Option<String>,
    pub unit_price_usd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OrderType {
    Physical,
    Service,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shipping {
    pub name: String,
    pub address_line_1: String,
    pub address_line_2: Option<String>,
    pub city: String,
    pub state: String,
    pub zip: String,
    pub country: String, // ISO 3166-1 alpha-2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDetails {
    pub description: String,
    pub preferred_date: Option<String>, // ISO 8601
    pub notes: Option<String>,
}

// ---------------------------------------------------------------------------
// 3.3.2 — payment-request (merchant → customer)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentRequest {
    pub version: u32,
    #[serde(rename = "type")]
    pub msg_type: String, // "payment-request"
    pub order_id: String,
    pub payment_provider: String,
    pub payment_id: String,
    pub payment_details: HashMap<String, String>,
    pub amount: String,
    pub currency: String,
    pub expires_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// 3.3.3 — status-update (merchant → customer)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusUpdate {
    pub version: u32,
    #[serde(rename = "type")]
    pub msg_type: String, // "status-update"
    pub order_id: String,
    pub status: OrderStatus,
    pub payment_provider: String,
    pub payment_id: String,
    pub amount: String,
    pub currency: String,
    pub timestamp: DateTime<Utc>,
    pub lightning_preimage: Option<String>,
    pub tracking: Option<Tracking>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OrderStatus {
    Paid,
    Shipped,
    Failed,
    Expired,
    Refunded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tracking {
    pub carrier: String,
    pub tracking_number: String,
}
