use async_trait::async_trait;
use std::collections::HashMap;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use serde_json::{json, Value};

use crate::error::{PurserError, Result};
use crate::providers::{
    PaymentMethod, PaymentProvider, PaymentStatus, PollConfig, ProviderPaymentRequest,
    RateLimitStrategy, ValidatedOrder,
};

pub struct StrikeProvider {
    api_key: String,
    client: reqwest::Client,
    base_url: String,
}

impl StrikeProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::Client::new(),
            base_url: "https://api.strike.me/v1".to_string(),
        }
    }

    /// Build the JSON body for the Strike Create Invoice API call.
    fn build_create_invoice_body(order: &ValidatedOrder) -> Value {
        json!({
            "correlationId": order.order_id,
            "description": format!("Order {}", order.order_id),
            "amount": {
                "amount": order.total_usd,
                "currency": "USD"
            }
        })
    }

    /// Extract the `invoiceId` from a Strike Create Invoice response.
    fn parse_invoice_response(body: &Value) -> Result<String> {
        body.get("invoiceId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| PurserError::Provider {
                provider: "strike".to_string(),
                message: "missing invoiceId in invoice response".to_string(),
            })
    }

    /// Extract the `lnInvoice` (bolt11) and `expirationInSec` from a Strike Quote response.
    fn parse_quote_response(body: &Value) -> Result<(String, u64)> {
        let bolt11 = body
            .get("lnInvoice")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| PurserError::Provider {
                provider: "strike".to_string(),
                message: "missing lnInvoice in quote response".to_string(),
            })?;

        let expiration_secs = body
            .get("expirationInSec")
            .and_then(|v| v.as_u64())
            .unwrap_or(600); // default 10 minutes

        Ok((bolt11, expiration_secs))
    }

    /// Map the Strike invoice `state` string to a `PaymentStatus`.
    fn map_invoice_state(body: &Value) -> Result<PaymentStatus> {
        let state = body
            .get("state")
            .and_then(|v| v.as_str())
            .ok_or_else(|| PurserError::Provider {
                provider: "strike".to_string(),
                message: "missing state in invoice response".to_string(),
            })?;

        match state {
            "PAID" => {
                let amount_paid = body
                    .get("amount")
                    .and_then(|a| a.get("amount"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("0")
                    .to_string();

                Ok(PaymentStatus::Completed {
                    amount_paid,
                    lightning_preimage: None,
                })
            }
            "UNPAID" => Ok(PaymentStatus::Pending),
            "CANCELLED" => Ok(PaymentStatus::Failed {
                reason: "invoice cancelled".to_string(),
            }),
            other => Ok(PaymentStatus::Failed {
                reason: format!("unknown invoice state: {other}"),
            }),
        }
    }
}

#[async_trait]
impl PaymentProvider for StrikeProvider {
    fn name(&self) -> &str {
        "strike"
    }

    fn supported_methods(&self) -> Vec<PaymentMethod> {
        vec![PaymentMethod::Lightning]
    }

    async fn create_payment(&self, order: &ValidatedOrder) -> Result<ProviderPaymentRequest> {
        // Step 1: Create invoice
        let invoice_body = Self::build_create_invoice_body(order);

        let invoice_resp = self
            .client
            .post(format!("{}/invoices", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&invoice_body)
            .send()
            .await
            .map_err(|e| PurserError::Provider {
                provider: "strike".to_string(),
                message: format!("failed to create invoice: {e}"),
            })?;

        if !invoice_resp.status().is_success() {
            let status = invoice_resp.status();
            let text = invoice_resp.text().await.unwrap_or_default();
            return Err(PurserError::Provider {
                provider: "strike".to_string(),
                message: format!("create invoice failed ({status}): {text}"),
            });
        }

        let invoice_json: Value =
            invoice_resp
                .json()
                .await
                .map_err(|e| PurserError::Provider {
                    provider: "strike".to_string(),
                    message: format!("failed to parse invoice response: {e}"),
                })?;

        let invoice_id = Self::parse_invoice_response(&invoice_json)?;

        // Step 2: Create quote to get bolt11
        let quote_resp = self
            .client
            .post(format!("{}/invoices/{}/quote", self.base_url, invoice_id))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| PurserError::Provider {
                provider: "strike".to_string(),
                message: format!("failed to create quote: {e}"),
            })?;

        if !quote_resp.status().is_success() {
            let status = quote_resp.status();
            let text = quote_resp.text().await.unwrap_or_default();
            return Err(PurserError::Provider {
                provider: "strike".to_string(),
                message: format!("create quote failed ({status}): {text}"),
            });
        }

        let quote_json: Value = quote_resp.json().await.map_err(|e| PurserError::Provider {
            provider: "strike".to_string(),
            message: format!("failed to parse quote response: {e}"),
        })?;

        let (bolt11, expiration_secs) = Self::parse_quote_response(&quote_json)?;

        let mut payment_details = HashMap::new();
        payment_details.insert("lightning_invoice".to_string(), bolt11);

        Ok(ProviderPaymentRequest {
            payment_id: invoice_id,
            payment_details,
            amount: order.total_usd.clone(),
            currency: "USD".to_string(),
            expires_at: Utc::now()
                + TimeDelta::try_seconds(expiration_secs as i64).unwrap_or(TimeDelta::seconds(600)),
        })
    }

    async fn check_status(&self, payment_id: &str) -> Result<PaymentStatus> {
        let resp = self
            .client
            .get(format!("{}/invoices/{}", self.base_url, payment_id))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .map_err(|e| PurserError::Provider {
                provider: "strike".to_string(),
                message: format!("failed to check invoice status: {e}"),
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(PurserError::Provider {
                provider: "strike".to_string(),
                message: format!("check status failed ({status}): {text}"),
            });
        }

        let body: Value = resp.json().await.map_err(|e| PurserError::Provider {
            provider: "strike".to_string(),
            message: format!("failed to parse status response: {e}"),
        })?;

        Self::map_invoice_state(&body)
    }

    async fn cancel_payment(&self, payment_id: &str) -> Result<()> {
        let resp = self
            .client
            .patch(format!(
                "{}/invoices/{}/cancel",
                self.base_url, payment_id
            ))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .map_err(|e| PurserError::Provider {
                provider: "strike".to_string(),
                message: format!("failed to cancel invoice: {e}"),
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(PurserError::Provider {
                provider: "strike".to_string(),
                message: format!("cancel invoice failed ({status}): {text}"),
            });
        }

        Ok(())
    }

    fn poll_config(&self) -> PollConfig {
        PollConfig {
            initial_interval: Duration::from_secs(10),
            backoff_multiplier: 3.0,
            max_interval: Duration::from_secs(300),
            rate_limit_strategy: RateLimitStrategy::FixedBudget {
                max_requests: 1000,
                window: Duration::from_secs(600),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::OrderType;
    use serde_json::json;

    fn make_test_order() -> ValidatedOrder {
        ValidatedOrder {
            order_id: "test-order-123".to_string(),
            customer_pubkey: "npub1abc".to_string(),
            items: vec![crate::providers::ValidatedOrderItem {
                product_id: "prod-1".to_string(),
                name: "Test Product".to_string(),
                quantity: 1,
                variant: None,
                unit_price_usd: "59.99".to_string(),
            }],
            order_type: OrderType::Physical,
            shipping: None,
            service_details: None,
            contact: None,
            payment_method: PaymentMethod::Lightning,
            total_usd: "59.99".to_string(),
            currency: "USD".to_string(),
            message: None,
        }
    }

    #[test]
    fn test_create_invoice_request_body() {
        let order = make_test_order();
        let body = StrikeProvider::build_create_invoice_body(&order);

        assert_eq!(body["correlationId"], "test-order-123");
        assert_eq!(body["description"], "Order test-order-123");
        assert_eq!(body["amount"]["amount"], "59.99");
        assert_eq!(body["amount"]["currency"], "USD");
    }

    #[test]
    fn test_parse_invoice_response() {
        let response = json!({
            "invoiceId": "inv-abc-123",
            "correlationId": "test-order-123",
            "state": "UNPAID"
        });

        let invoice_id = StrikeProvider::parse_invoice_response(&response).unwrap();
        assert_eq!(invoice_id, "inv-abc-123");
    }

    #[test]
    fn test_parse_invoice_response_missing_id() {
        let response = json!({
            "correlationId": "test-order-123",
            "state": "UNPAID"
        });

        let result = StrikeProvider::parse_invoice_response(&response);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_quote_response() {
        let response = json!({
            "quoteId": "quote-xyz",
            "lnInvoice": "lnbc1pvjluezpp5qqqsyqcyq5rqwzqf...",
            "expirationInSec": 900
        });

        let (bolt11, expiry) = StrikeProvider::parse_quote_response(&response).unwrap();
        assert_eq!(bolt11, "lnbc1pvjluezpp5qqqsyqcyq5rqwzqf...");
        assert_eq!(expiry, 900);
    }

    #[test]
    fn test_parse_quote_response_default_expiry() {
        let response = json!({
            "quoteId": "quote-xyz",
            "lnInvoice": "lnbc1someinvoice..."
        });

        let (bolt11, expiry) = StrikeProvider::parse_quote_response(&response).unwrap();
        assert_eq!(bolt11, "lnbc1someinvoice...");
        assert_eq!(expiry, 600); // default
    }

    #[test]
    fn test_parse_quote_response_missing_invoice() {
        let response = json!({
            "quoteId": "quote-xyz",
            "expirationInSec": 900
        });

        let result = StrikeProvider::parse_quote_response(&response);
        assert!(result.is_err());
    }

    #[test]
    fn test_check_status_paid() {
        let body = json!({
            "invoiceId": "inv-abc-123",
            "state": "PAID",
            "amount": {
                "amount": "59.99",
                "currency": "USD"
            }
        });

        let status = StrikeProvider::map_invoice_state(&body).unwrap();
        match status {
            PaymentStatus::Completed { amount_paid, lightning_preimage } => {
                assert_eq!(amount_paid, "59.99");
                assert!(lightning_preimage.is_none());
            }
            other => panic!("expected Completed, got {:?}", other),
        }
    }

    #[test]
    fn test_check_status_unpaid() {
        let body = json!({
            "invoiceId": "inv-abc-123",
            "state": "UNPAID"
        });

        let status = StrikeProvider::map_invoice_state(&body).unwrap();
        assert_eq!(status, PaymentStatus::Pending);
    }

    #[test]
    fn test_check_status_cancelled() {
        let body = json!({
            "invoiceId": "inv-abc-123",
            "state": "CANCELLED"
        });

        let status = StrikeProvider::map_invoice_state(&body).unwrap();
        match status {
            PaymentStatus::Failed { reason } => {
                assert_eq!(reason, "invoice cancelled");
            }
            other => panic!("expected Failed, got {:?}", other),
        }
    }

    #[test]
    fn test_check_status_unknown_state() {
        let body = json!({
            "invoiceId": "inv-abc-123",
            "state": "SOMETHING_WEIRD"
        });

        let status = StrikeProvider::map_invoice_state(&body).unwrap();
        match status {
            PaymentStatus::Failed { reason } => {
                assert!(reason.contains("unknown invoice state"));
            }
            other => panic!("expected Failed, got {:?}", other),
        }
    }

    #[test]
    fn test_check_status_missing_state() {
        let body = json!({
            "invoiceId": "inv-abc-123"
        });

        let result = StrikeProvider::map_invoice_state(&body);
        assert!(result.is_err());
    }

    #[test]
    fn test_provider_name() {
        let provider = StrikeProvider::new("test-key".to_string());
        assert_eq!(provider.name(), "strike");
    }

    #[test]
    fn test_supported_methods() {
        let provider = StrikeProvider::new("test-key".to_string());
        let methods = provider.supported_methods();
        assert_eq!(methods, vec![PaymentMethod::Lightning]);
    }

    #[test]
    fn test_new_default_base_url() {
        let provider = StrikeProvider::new("my-api-key".to_string());
        assert_eq!(provider.base_url, "https://api.strike.me/v1");
        assert_eq!(provider.api_key, "my-api-key");
    }
}
