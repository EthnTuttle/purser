use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time;

use crate::error::{PurserError, Result};
use crate::providers::{
    PaymentMethod, PaymentProvider, PaymentStatus, PollConfig, ProviderPaymentRequest,
    RateLimitStrategy, ValidatedOrder,
};

const SQUARE_API_VERSION: &str = "2024-01-18";
const DEFAULT_BASE_URL: &str = "https://connect.squareup.com/v2";
const DEFAULT_EXPIRY_SECS: u64 = 900;

pub struct SquareProvider {
    api_key: String,
    client: reqwest::Client,
    base_url: String,
    location_id: String,
    default_expiry_secs: u64,
    rate_limit_remaining: AtomicI64,
}

impl SquareProvider {
    pub fn new(
        api_key: String,
        location_id: String,
        base_url: Option<String>,
        expiry_secs: Option<u64>,
    ) -> Self {
        Self {
            api_key,
            client: reqwest::Client::new(),
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            location_id,
            default_expiry_secs: expiry_secs.unwrap_or(DEFAULT_EXPIRY_SECS),
            rate_limit_remaining: AtomicI64::new(-1),
        }
    }

    /// Returns the last observed X-RateLimit-Remaining value, or -1 if not yet seen.
    pub fn rate_limit_remaining(&self) -> i64 {
        self.rate_limit_remaining.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Helper functions (testable without HTTP)
// ---------------------------------------------------------------------------

/// Convert a dollar-amount string (e.g. "59.99") to cents as i64.
fn dollars_to_cents(dollars: &str) -> Result<i64> {
    let trimmed = dollars.trim();
    if trimmed.starts_with('-') {
        return Err(PurserError::Provider {
            provider: "square".to_string(),
            message: format!("negative amount not allowed: '{}'", dollars),
        });
    }
    // Split on decimal point
    let parts: Vec<&str> = trimmed.split('.').collect();
    match parts.len() {
        1 => {
            // No decimal, e.g. "100"
            let whole: i64 = parts[0]
                .parse()
                .map_err(|e| PurserError::Provider {
                    provider: "square".to_string(),
                    message: format!("invalid dollar amount '{}': {}", dollars, e),
                })?;
            Ok(whole * 100)
        }
        2 => {
            let whole: i64 = parts[0]
                .parse()
                .map_err(|e| PurserError::Provider {
                    provider: "square".to_string(),
                    message: format!("invalid dollar amount '{}': {}", dollars, e),
                })?;
            // Pad or truncate fractional part to exactly 2 digits
            let frac_str = parts[1];
            let frac_padded = match frac_str.len() {
                0 => "00".to_string(),
                1 => format!("{}0", frac_str),
                2 => frac_str.to_string(),
                _ => frac_str[..2].to_string(), // truncate to 2 decimal places
            };
            let frac: i64 = frac_padded
                .parse()
                .map_err(|e| PurserError::Provider {
                    provider: "square".to_string(),
                    message: format!("invalid dollar amount '{}': {}", dollars, e),
                })?;
            Ok(whole * 100 + frac)
        }
        _ => Err(PurserError::Provider {
            provider: "square".to_string(),
            message: format!("invalid dollar amount: '{}'", dollars),
        }),
    }
}

/// Build the JSON request body for Square's Payment Links API.
fn build_create_payment_body(order: &ValidatedOrder, location_id: &str) -> Result<Value> {
    let line_items: Result<Vec<Value>> = order
        .items
        .iter()
        .map(|item| {
            let cents = dollars_to_cents(&item.unit_price_usd)?;
            Ok(serde_json::json!({
                "name": item.name,
                "quantity": item.quantity.to_string(),
                "base_price_money": {
                    "amount": cents,
                    "currency": "USD"
                }
            }))
        })
        .collect();

    let body = serde_json::json!({
        "idempotency_key": order.order_id,
        "order": {
            "location_id": location_id,
            "line_items": line_items?
        }
    });

    Ok(body)
}

/// Parse a successful create-payment-link response into a ProviderPaymentRequest.
fn parse_payment_link_response(
    json: &Value,
    total_usd: &str,
    expires_at: DateTime<Utc>,
) -> Result<ProviderPaymentRequest> {
    let payment_link = json.get("payment_link").ok_or_else(|| PurserError::Provider {
        provider: "square".to_string(),
        message: "response missing 'payment_link' field".to_string(),
    })?;

    let url = payment_link
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| PurserError::Provider {
            provider: "square".to_string(),
            message: "response missing 'payment_link.url' field".to_string(),
        })?;

    let id = payment_link
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| PurserError::Provider {
            provider: "square".to_string(),
            message: "response missing 'payment_link.id' field".to_string(),
        })?;

    let mut payment_details = HashMap::new();
    payment_details.insert("checkout_url".to_string(), url.to_string());

    Ok(ProviderPaymentRequest {
        payment_id: id.to_string(),
        payment_details,
        amount: total_usd.to_string(),
        currency: "USD".to_string(),
        expires_at,
    })
}

/// Parse a check-status response to determine payment status.
fn parse_status_response(json: &Value) -> Result<PaymentStatus> {
    // The payment link response contains the link info and possibly related order info
    // Verify payment_link field exists in the response
    let _payment_link = json.get("payment_link").ok_or_else(|| PurserError::Provider {
        provider: "square".to_string(),
        message: "status response missing 'payment_link' field".to_string(),
    })?;

    // Check for associated order status to determine payment completion.
    // Square payment links have related_resources with order state.
    if let Some(order) = json.get("related_resources").and_then(|r| r.get("orders")).and_then(|o| o.as_array()).and_then(|a| a.first()) {
        let state = order
            .get("state")
            .and_then(|s| s.as_str())
            .unwrap_or("");
        match state {
            "COMPLETED" => {
                let amount_paid = order
                    .get("total_money")
                    .and_then(|m| m.get("amount"))
                    .and_then(|a| a.as_i64())
                    .map(|cents| format!("{}.{:02}", cents / 100, cents % 100))
                    .unwrap_or_default();
                return Ok(PaymentStatus::Completed {
                    amount_paid,
                    lightning_preimage: None,
                });
            }
            "CANCELED" => {
                return Ok(PaymentStatus::Failed {
                    reason: "order canceled".to_string(),
                });
            }
            _ => {}
        }
    }

    // Fallback: check payment_link's own fields
    // If no completed order is associated, it's still pending
    Ok(PaymentStatus::Pending)
}

#[async_trait]
impl PaymentProvider for SquareProvider {
    fn name(&self) -> &str {
        "square"
    }

    fn supported_methods(&self) -> Vec<PaymentMethod> {
        vec![PaymentMethod::Fiat]
    }

    async fn create_payment(&self, order: &ValidatedOrder) -> Result<ProviderPaymentRequest> {
        let body = build_create_payment_body(order, &self.location_id)?;

        let url = format!("{}/online-checkout/payment-links", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("Square-Version", SQUARE_API_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|e| PurserError::Provider {
                provider: "square".to_string(),
                message: format!("HTTP request failed: {}", e),
            })?;

        // Track rate limit header
        if let Some(remaining) = response
            .headers()
            .get("X-RateLimit-Remaining")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
        {
            self.rate_limit_remaining.store(remaining, Ordering::Relaxed);
        }

        let status = response.status();
        let json: Value = response.json().await.map_err(|e| PurserError::Provider {
            provider: "square".to_string(),
            message: format!("failed to parse response JSON: {}", e),
        })?;

        if !status.is_success() {
            return Err(PurserError::Provider {
                provider: "square".to_string(),
                message: format!("Square API error ({}): {}", status, json),
            });
        }

        let expires_at =
            Utc::now() + Duration::seconds(self.default_expiry_secs as i64);

        parse_payment_link_response(&json, &order.total_usd, expires_at)
    }

    async fn check_status(&self, payment_id: &str) -> Result<PaymentStatus> {
        let url = format!(
            "{}/online-checkout/payment-links/{}",
            self.base_url, payment_id
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("Square-Version", SQUARE_API_VERSION)
            .send()
            .await
            .map_err(|e| PurserError::Provider {
                provider: "square".to_string(),
                message: format!("HTTP request failed: {}", e),
            })?;

        // Track rate limit header
        if let Some(remaining) = response
            .headers()
            .get("X-RateLimit-Remaining")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
        {
            self.rate_limit_remaining.store(remaining, Ordering::Relaxed);
        }

        let status = response.status();
        let json: Value = response.json().await.map_err(|e| PurserError::Provider {
            provider: "square".to_string(),
            message: format!("failed to parse response JSON: {}", e),
        })?;

        if !status.is_success() {
            return Err(PurserError::Provider {
                provider: "square".to_string(),
                message: format!("Square API error ({}): {}", status, json),
            });
        }

        parse_status_response(&json)
    }

    async fn cancel_payment(&self, payment_id: &str) -> Result<()> {
        let url = format!(
            "{}/online-checkout/payment-links/{}",
            self.base_url, payment_id
        );

        let response = self
            .client
            .delete(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("Square-Version", SQUARE_API_VERSION)
            .send()
            .await
            .map_err(|e| PurserError::Provider {
                provider: "square".to_string(),
                message: format!("HTTP request failed: {}", e),
            })?;

        // Track rate limit header
        if let Some(remaining) = response
            .headers()
            .get("X-RateLimit-Remaining")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
        {
            self.rate_limit_remaining.store(remaining, Ordering::Relaxed);
        }

        let status = response.status();
        if !status.is_success() {
            let json: Value =
                response.json().await.map_err(|e| PurserError::Provider {
                    provider: "square".to_string(),
                    message: format!("failed to parse response JSON: {}", e),
                })?;
            return Err(PurserError::Provider {
                provider: "square".to_string(),
                message: format!("Square API error ({}): {}", status, json),
            });
        }

        Ok(())
    }

    fn poll_config(&self) -> PollConfig {
        PollConfig {
            initial_interval: time::Duration::from_secs(10),
            backoff_multiplier: 3.0,
            max_interval: time::Duration::from_secs(300),
            rate_limit_strategy: RateLimitStrategy::HeaderMonitor {
                header_name: "X-RateLimit-Remaining".to_string(),
                threshold_percent: 20.0,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::OrderType;
    use crate::providers::ValidatedOrderItem;

    fn sample_order() -> ValidatedOrder {
        ValidatedOrder {
            order_id: "test-order-001".to_string(),
            customer_pubkey: "npub1test".to_string(),
            items: vec![
                ValidatedOrderItem {
                    product_id: "heltec-v3-single".to_string(),
                    name: "Heltec V3 Meshtastic Device".to_string(),
                    quantity: 2,
                    variant: Some("single".to_string()),
                    unit_price_usd: "59.99".to_string(),
                },
                ValidatedOrderItem {
                    product_id: "cable-usb-c".to_string(),
                    name: "USB-C Cable".to_string(),
                    quantity: 1,
                    variant: None,
                    unit_price_usd: "9.99".to_string(),
                },
            ],
            order_type: OrderType::Physical,
            shipping: None,
            service_details: None,
            contact: None,
            payment_method: PaymentMethod::Fiat,
            total_usd: "129.97".to_string(),
            currency: "USD".to_string(),
            message: None,
        }
    }

    #[test]
    fn test_dollars_to_cents() {
        assert_eq!(dollars_to_cents("59.99").unwrap(), 5999);
        assert_eq!(dollars_to_cents("0.50").unwrap(), 50);
        assert_eq!(dollars_to_cents("100.00").unwrap(), 10000);
        assert_eq!(dollars_to_cents("0.01").unwrap(), 1);
        assert_eq!(dollars_to_cents("1000").unwrap(), 100000);
        assert_eq!(dollars_to_cents("1.5").unwrap(), 150);
        assert!(dollars_to_cents("-10.00").is_err());
    }

    #[test]
    fn test_create_payment_request_body() {
        let order = sample_order();
        let body = build_create_payment_body(&order, "LOC_ABC123").unwrap();

        // Verify idempotency key
        assert_eq!(body["idempotency_key"], "test-order-001");

        // Verify line items
        let items = body["order"]["line_items"].as_array().unwrap();
        assert_eq!(items.len(), 2);

        assert_eq!(items[0]["name"], "Heltec V3 Meshtastic Device");
        assert_eq!(items[0]["quantity"], "2");
        assert_eq!(items[0]["base_price_money"]["amount"], 5999);
        assert_eq!(items[0]["base_price_money"]["currency"], "USD");

        assert_eq!(items[1]["name"], "USB-C Cable");
        assert_eq!(items[1]["quantity"], "1");
        assert_eq!(items[1]["base_price_money"]["amount"], 999);
        assert_eq!(items[1]["base_price_money"]["currency"], "USD");

        // Verify location_id uses the passed value
        assert_eq!(body["order"]["location_id"], "LOC_ABC123");
    }

    #[test]
    fn test_parse_payment_link_response() {
        let json: Value = serde_json::json!({
            "payment_link": {
                "id": "link_abc123",
                "url": "https://square.link/u/abc123",
                "version": 1
            }
        });

        let expires_at = Utc::now() + Duration::seconds(900);
        let result = parse_payment_link_response(&json, "129.97", expires_at).unwrap();

        assert_eq!(result.payment_id, "link_abc123");
        assert_eq!(
            result.payment_details.get("checkout_url").unwrap(),
            "https://square.link/u/abc123"
        );
        assert_eq!(result.amount, "129.97");
        assert_eq!(result.currency, "USD");
        assert_eq!(result.expires_at, expires_at);
    }

    #[test]
    fn test_parse_status_completed() {
        let json: Value = serde_json::json!({
            "payment_link": {
                "id": "link_abc123",
                "url": "https://square.link/u/abc123"
            },
            "related_resources": {
                "orders": [{
                    "state": "COMPLETED",
                    "total_money": {
                        "amount": 12997,
                        "currency": "USD"
                    }
                }]
            }
        });

        let status = parse_status_response(&json).unwrap();
        assert_eq!(
            status,
            PaymentStatus::Completed {
                amount_paid: "129.97".to_string(),
                lightning_preimage: None,
            }
        );
    }

    #[test]
    fn test_parse_status_pending() {
        let json: Value = serde_json::json!({
            "payment_link": {
                "id": "link_abc123",
                "url": "https://square.link/u/abc123"
            }
        });

        let status = parse_status_response(&json).unwrap();
        assert_eq!(status, PaymentStatus::Pending);
    }

    #[test]
    fn test_expires_at_set() {
        let before = Utc::now();
        let provider = SquareProvider::new("test-key".to_string(), "LOC_TEST".to_string(), None, None);

        // Simulate what create_payment does for expiry
        let expires_at =
            Utc::now() + Duration::seconds(provider.default_expiry_secs as i64);
        let after = Utc::now();

        // expires_at should be ~15 minutes (900 seconds) from now
        let diff_from_before = (expires_at - before).num_seconds();
        let diff_from_after = (expires_at - after).num_seconds();

        assert!(
            diff_from_before >= 899 && diff_from_before <= 901,
            "expected ~900 seconds from before, got {}",
            diff_from_before
        );
        assert!(
            diff_from_after >= 899 && diff_from_after <= 901,
            "expected ~900 seconds from after, got {}",
            diff_from_after
        );
    }
}
