//! Order processing pipeline — Issue #9
//!
//! Wires together validation, rate limiting, provider routing, polling, and
//! Nostr messaging into two top-level async functions:
//!
//! - [`process_order`]: incoming order → validate → create payment → send payment-request
//! - [`handle_polling_event`]: polling event → build status update → send + cleanup

use std::sync::Arc;

use chrono::Utc;

use crate::error::{PurserError, Result};
use crate::messages::validation::{is_duplicate_order, validate_order};
use crate::messages::{OrderStatus, PaymentRequest, StatusUpdate, PROTOCOL_VERSION};
use crate::nostr::NostrClient;
use crate::polling::{PollingEngine, PollingEvent};
use crate::providers::PaymentMethod;
use crate::ratelimit::RateLimiter;
use crate::state::{AppState, PendingPayment, PendingPaymentStatus};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Shared dependencies for the pipeline.
pub struct PipelineContext {
    pub state: Arc<AppState>,
    pub nostr: Arc<NostrClient>,
    pub rate_limiter: Arc<RateLimiter>,
    pub polling_engine: Arc<PollingEngine>,
}

/// An incoming order from the Nostr transport layer.
#[allow(dead_code)]
pub struct IncomingOrder {
    pub raw_json: String,
    pub customer_pubkey: String,
}

// ---------------------------------------------------------------------------
// process_order
// ---------------------------------------------------------------------------

/// Process an incoming order through the full pipeline.
///
/// On parse failure the function sends an error to a new checkout group and
/// returns `Ok(())` (garbage resilience). All other failures propagate as
/// `PurserError`.
pub async fn process_order(
    ctx: &PipelineContext,
    raw_json: &str,
    customer_pubkey: &str,
) -> Result<()> {
    // 1. Parse raw JSON into an Order.
    let order: crate::messages::Order = match serde_json::from_str(raw_json) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(customer = %customer_pubkey, error = %e, "failed to parse order JSON");
            let group_id = ctx.nostr.create_checkout_group(customer_pubkey).await?;
            ctx.nostr
                .send_error(&group_id, &format!("invalid order format: {e}"))
                .await?;
            return Ok(());
        }
    };

    // 2. Rate-limit check.
    ctx.rate_limiter.check_order_allowed(customer_pubkey)?;

    // 3. Duplicate-order check.
    if is_duplicate_order(&order.order_id, customer_pubkey) {
        let pending = ctx.state.pending_payments.read().await;
        if let Some(existing) = pending.get(&order.order_id) {
            let payment_request = PaymentRequest {
                version: PROTOCOL_VERSION,
                msg_type: "payment-request".to_string(),
                order_id: existing.order_id.clone(),
                payment_provider: existing.provider_name.clone(),
                payment_id: existing.payment_id.clone(),
                payment_details: Default::default(),
                amount: existing.amount.clone(),
                currency: existing.currency.clone(),
                expires_at: existing.expires_at,
            };
            ctx.nostr
                .send_payment_request(&existing.group_id, &payment_request)
                .await?;
        }
        return Ok(());
    }

    // 4. Validate the order against schema + catalog.
    let validated_order =
        validate_order(&order, &ctx.state.catalog, customer_pubkey)?;

    // 5. Route to a provider whose supported_methods() contains the order's
    //    payment_method.
    let payment_method =
        PaymentMethod::from_str_loose(&order.payment_method).ok_or_else(|| {
            PurserError::UnsupportedPaymentMethod(order.payment_method.clone())
        })?;

    let provider = ctx
        .state
        .providers
        .iter()
        .find(|p| p.supported_methods().contains(&payment_method))
        .ok_or_else(|| {
            PurserError::UnsupportedPaymentMethod(order.payment_method.clone())
        })?;

    // 6–7. Record the attempt and set the active session.
    ctx.rate_limiter.record_order_attempt(customer_pubkey);
    ctx.rate_limiter.set_active_session(customer_pubkey);

    // 8. Create the payment with the provider.
    let provider_response = match provider.create_payment(&validated_order).await {
        Ok(resp) => resp,
        Err(e) => {
            ctx.rate_limiter.clear_active_session(customer_pubkey);
            ctx.rate_limiter.record_failure(customer_pubkey);
            return Err(e);
        }
    };

    // 9. Create checkout group.
    let group_id = ctx.nostr.create_checkout_group(customer_pubkey).await?;

    // 10. Build PendingPayment.
    let status = if validated_order.total_usd == "0.00" {
        PendingPaymentStatus::PendingQuote
    } else {
        PendingPaymentStatus::AwaitingPayment
    };

    let pending_payment = PendingPayment {
        order_id: order.order_id.clone(),
        customer_pubkey: customer_pubkey.to_string(),
        provider_name: provider.name().to_string(),
        payment_id: provider_response.payment_id.clone(),
        payment_method,
        amount: provider_response.amount.clone(),
        currency: provider_response.currency.clone(),
        created_at: Utc::now(),
        expires_at: provider_response.expires_at,
        status,
        group_id: group_id.clone(),
    };

    // 11. Insert into pending_payments.
    ctx.state
        .pending_payments
        .write()
        .await
        .insert(order.order_id.clone(), pending_payment.clone());

    // 12. Register with the polling engine.
    ctx.polling_engine.register(&pending_payment).await?;

    // 13. Build and send PaymentRequest.
    let payment_request = PaymentRequest {
        version: PROTOCOL_VERSION,
        msg_type: "payment-request".to_string(),
        order_id: order.order_id.clone(),
        payment_provider: provider.name().to_string(),
        payment_id: provider_response.payment_id,
        payment_details: provider_response.payment_details,
        amount: provider_response.amount,
        currency: provider_response.currency,
        expires_at: provider_response.expires_at,
    };

    ctx.nostr
        .send_payment_request(&group_id, &payment_request)
        .await?;

    tracing::info!(
        order_id = %order.order_id,
        provider = %provider.name(),
        customer = %customer_pubkey,
        "order processed successfully"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// handle_polling_event
// ---------------------------------------------------------------------------

/// React to a polling event by updating state, sending status updates, and
/// cleaning up resources.
pub async fn handle_polling_event(
    ctx: &PipelineContext,
    event: PollingEvent,
) -> Result<()> {
    match event {
        PollingEvent::Completed {
            order_id,
            amount_paid,
            lightning_preimage,
        } => {
            let pending = ctx.state.pending_payments.read().await;
            let payment = match pending.get(&order_id) {
                Some(p) => p.clone(),
                None => {
                    tracing::warn!(order_id = %order_id, "completed event for unknown payment (race condition)");
                    return Ok(());
                }
            };
            drop(pending);

            let status_update = StatusUpdate {
                version: PROTOCOL_VERSION,
                msg_type: "status-update".to_string(),
                order_id: order_id.clone(),
                status: OrderStatus::Paid,
                payment_provider: payment.provider_name.clone(),
                payment_id: payment.payment_id.clone(),
                amount: amount_paid,
                currency: payment.currency.clone(),
                timestamp: Utc::now(),
                lightning_preimage,
                tracking: None,
                message: None,
            };

            ctx.nostr
                .send_status_update(&payment.group_id, &status_update)
                .await?;
            ctx.nostr.deactivate_group(&payment.group_id).await?;
            ctx.state.pending_payments.write().await.remove(&order_id);
            ctx.rate_limiter
                .clear_active_session(&payment.customer_pubkey);

            tracing::info!(order_id = %order_id, "payment completed");
        }

        PollingEvent::Expired { order_id } => {
            let pending = ctx.state.pending_payments.read().await;
            let payment = match pending.get(&order_id) {
                Some(p) => p.clone(),
                None => {
                    tracing::warn!(order_id = %order_id, "expired event for unknown payment (race condition)");
                    return Ok(());
                }
            };
            drop(pending);

            let status_update = StatusUpdate {
                version: PROTOCOL_VERSION,
                msg_type: "status-update".to_string(),
                order_id: order_id.clone(),
                status: OrderStatus::Expired,
                payment_provider: payment.provider_name.clone(),
                payment_id: payment.payment_id.clone(),
                amount: payment.amount.clone(),
                currency: payment.currency.clone(),
                timestamp: Utc::now(),
                lightning_preimage: None,
                tracking: None,
                message: None,
            };

            ctx.nostr
                .send_status_update(&payment.group_id, &status_update)
                .await?;
            ctx.nostr.deactivate_group(&payment.group_id).await?;
            ctx.state.pending_payments.write().await.remove(&order_id);
            ctx.rate_limiter
                .clear_active_session(&payment.customer_pubkey);
            ctx.rate_limiter.record_failure(&payment.customer_pubkey);

            tracing::info!(order_id = %order_id, "payment expired");
        }

        PollingEvent::Failed { order_id, reason } => {
            let pending = ctx.state.pending_payments.read().await;
            let payment = match pending.get(&order_id) {
                Some(p) => p.clone(),
                None => {
                    tracing::warn!(order_id = %order_id, "failed event for unknown payment (race condition)");
                    return Ok(());
                }
            };
            drop(pending);

            let status_update = StatusUpdate {
                version: PROTOCOL_VERSION,
                msg_type: "status-update".to_string(),
                order_id: order_id.clone(),
                status: OrderStatus::Failed,
                payment_provider: payment.provider_name.clone(),
                payment_id: payment.payment_id.clone(),
                amount: payment.amount.clone(),
                currency: payment.currency.clone(),
                timestamp: Utc::now(),
                lightning_preimage: None,
                tracking: None,
                message: Some(reason),
            };

            ctx.nostr
                .send_status_update(&payment.group_id, &status_update)
                .await?;
            ctx.nostr.deactivate_group(&payment.group_id).await?;
            ctx.state.pending_payments.write().await.remove(&order_id);
            ctx.rate_limiter
                .clear_active_session(&payment.customer_pubkey);
            ctx.rate_limiter.record_failure(&payment.customer_pubkey);

            tracing::info!(order_id = %order_id, "payment failed");
        }
    }

    Ok(())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Product, ProductType};
    use crate::config::{
        Config, MdkConfig, PollingConfig, ProviderConfig, RateLimitConfig, StorageConfig,
    };
    #[allow(unused_imports)]
    use crate::messages::{Order, OrderItem, OrderType, Shipping};
    use crate::providers::{
        PaymentMethod, PollConfig, ProviderPaymentRequest, RateLimitStrategy, ValidatedOrder,
    };
    use async_trait::async_trait;
    use chrono::{Duration as ChronoDuration, Utc};
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;

    // -----------------------------------------------------------------------
    // MockProvider
    // -----------------------------------------------------------------------

    struct MockProvider {
        provider_name: String,
        methods: Vec<PaymentMethod>,
        create_response: Mutex<Option<Result<ProviderPaymentRequest>>>,
    }

    impl MockProvider {
        fn new(name: &str, methods: Vec<PaymentMethod>) -> Self {
            Self {
                provider_name: name.to_string(),
                methods,
                create_response: Mutex::new(None),
            }
        }

        fn with_create_response(mut self, resp: Result<ProviderPaymentRequest>) -> Self {
            self.create_response = Mutex::new(Some(resp));
            self
        }
    }

    #[async_trait]
    impl crate::providers::PaymentProvider for MockProvider {
        fn name(&self) -> &str {
            &self.provider_name
        }

        fn supported_methods(&self) -> Vec<PaymentMethod> {
            self.methods.clone()
        }

        async fn create_payment(
            &self,
            _order: &ValidatedOrder,
        ) -> Result<ProviderPaymentRequest> {
            let mut guard = self.create_response.lock().await;
            if let Some(resp) = guard.take() {
                return resp;
            }
            // Default successful response.
            Ok(ProviderPaymentRequest {
                payment_id: "pay-mock-001".to_string(),
                payment_details: HashMap::from([(
                    "checkout_url".to_string(),
                    "https://example.com/pay".to_string(),
                )]),
                amount: "59.99".to_string(),
                currency: "USD".to_string(),
                expires_at: Utc::now() + ChronoDuration::minutes(15),
            })
        }

        async fn check_status(
            &self,
            _payment_id: &str,
        ) -> Result<crate::providers::PaymentStatus> {
            Ok(crate::providers::PaymentStatus::Pending)
        }

        async fn cancel_payment(&self, _payment_id: &str) -> Result<()> {
            Ok(())
        }

        fn poll_config(&self) -> PollConfig {
            PollConfig {
                initial_interval: Duration::from_secs(10),
                backoff_multiplier: 2.0,
                max_interval: Duration::from_secs(300),
                rate_limit_strategy: RateLimitStrategy::None,
            }
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn make_catalog() -> Vec<Product> {
        vec![
            Product {
                id: "heltec-v3".to_string(),
                name: "Heltec V3".to_string(),
                product_type: ProductType::Physical,
                price_usd: "59.99".to_string(),
                variants: vec!["single".to_string()],
                active: true,
            },
            Product {
                id: "custom-dev".to_string(),
                name: "Custom Development".to_string(),
                product_type: ProductType::Service,
                price_usd: "0.00".to_string(),
                variants: vec![],
                active: true,
            },
        ]
    }

    fn make_config() -> Config {
        Config {
            relays: vec!["wss://relay.example".to_string()],
            merchant_npub: "npub1merchant".to_string(),
            providers: vec![ProviderConfig {
                provider_type: "mock".to_string(),
                methods: vec!["fiat".to_string()],
                api_key_env: "MOCK_KEY".to_string(),
                location_id_env: None,
            }],
            polling: PollingConfig::default(),
            rate_limits: RateLimitConfig::default(),
            mdk: MdkConfig::default(),
            storage: StorageConfig::default(),
        }
    }

    async fn make_context(
        providers: Vec<Arc<dyn crate::providers::PaymentProvider>>,
    ) -> PipelineContext {
        let state = Arc::new(AppState {
            config: make_config(),
            catalog: make_catalog(),
            pending_payments: tokio::sync::RwLock::new(HashMap::new()),
            providers: providers.clone(),
        });

        let nostr = Arc::new(
            NostrClient::new(&["wss://relay.example".to_string()], "memory")
                .await
                .unwrap(),
        );

        let rate_limiter = Arc::new(RateLimiter::new(RateLimitConfig::default()));

        let (polling_engine, _rx) = PollingEngine::new(
            providers,
            2.0,
        );
        let polling_engine = Arc::new(polling_engine);

        PipelineContext {
            state,
            nostr,
            rate_limiter,
            polling_engine,
        }
    }

    /// Build a valid fiat order JSON string for the heltec-v3 product.
    fn valid_fiat_order_json(order_id: &str) -> String {
        serde_json::to_string(&serde_json::json!({
            "version": 1,
            "type": "order",
            "order_id": order_id,
            "items": [{
                "product_id": "heltec-v3",
                "name": "Heltec V3",
                "quantity": 1,
                "variant": "single",
                "unit_price_usd": "59.99"
            }],
            "order_type": "physical",
            "shipping": {
                "name": "Alice",
                "address_line_1": "123 Main St",
                "city": "Austin",
                "state": "TX",
                "zip": "78701",
                "country": "US"
            },
            "payment_method": "fiat",
            "currency": "USD"
        }))
        .unwrap()
    }

    /// Build a valid lightning order JSON string for the heltec-v3 product.
    fn valid_lightning_order_json(order_id: &str) -> String {
        serde_json::to_string(&serde_json::json!({
            "version": 1,
            "type": "order",
            "order_id": order_id,
            "items": [{
                "product_id": "heltec-v3",
                "name": "Heltec V3",
                "quantity": 1,
                "variant": "single",
                "unit_price_usd": "59.99"
            }],
            "order_type": "physical",
            "shipping": {
                "name": "Alice",
                "address_line_1": "123 Main St",
                "city": "Austin",
                "state": "TX",
                "zip": "78701",
                "country": "US"
            },
            "payment_method": "lightning",
            "currency": "USD"
        }))
        .unwrap()
    }

    /// Build a custom-quote (total "0.00") order JSON string.
    fn custom_quote_order_json(order_id: &str) -> String {
        serde_json::to_string(&serde_json::json!({
            "version": 1,
            "type": "order",
            "order_id": order_id,
            "items": [{
                "product_id": "custom-dev",
                "name": "Custom Development",
                "quantity": 1,
                "unit_price_usd": "0.00"
            }],
            "order_type": "service",
            "service_details": {
                "description": "Build me a Nostr app"
            },
            "payment_method": "fiat",
            "currency": "USD"
        }))
        .unwrap()
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_happy_path_fiat() {
        let provider: Arc<dyn crate::providers::PaymentProvider> =
            Arc::new(MockProvider::new("mock-fiat", vec![PaymentMethod::Fiat]));
        let ctx = make_context(vec![provider]).await;

        let json = valid_fiat_order_json("fiat-order-001");
        let result = process_order(&ctx, &json, "npub1fiatcustomer").await;
        assert!(result.is_ok(), "process_order failed: {:?}", result.err());

        // Verify payment is pending.
        let pending = ctx.state.pending_payments.read().await;
        assert!(pending.contains_key("fiat-order-001"));
        let pp = pending.get("fiat-order-001").unwrap();
        assert_eq!(pp.customer_pubkey, "npub1fiatcustomer");
        assert_eq!(pp.status, PendingPaymentStatus::AwaitingPayment);
    }

    #[tokio::test]
    async fn test_happy_path_lightning() {
        let provider: Arc<dyn crate::providers::PaymentProvider> = Arc::new(
            MockProvider::new("mock-ln", vec![PaymentMethod::Lightning])
                .with_create_response(Ok(ProviderPaymentRequest {
                    payment_id: "ln-inv-001".to_string(),
                    payment_details: HashMap::from([(
                        "lightning_invoice".to_string(),
                        "lnbc1...".to_string(),
                    )]),
                    amount: "59.99".to_string(),
                    currency: "USD".to_string(),
                    expires_at: Utc::now() + ChronoDuration::minutes(15),
                })),
        );
        let ctx = make_context(vec![provider]).await;

        let json = valid_lightning_order_json("ln-order-001");
        let result = process_order(&ctx, &json, "npub1lncustomer").await;
        assert!(result.is_ok(), "process_order failed: {:?}", result.err());

        let pending = ctx.state.pending_payments.read().await;
        let pp = pending.get("ln-order-001").unwrap();
        assert_eq!(pp.provider_name, "mock-ln");
        assert_eq!(pp.payment_id, "ln-inv-001");
    }

    #[tokio::test]
    async fn test_rate_limited_rejected() {
        let provider: Arc<dyn crate::providers::PaymentProvider> =
            Arc::new(MockProvider::new("mock-fiat", vec![PaymentMethod::Fiat]));
        let ctx = make_context(vec![provider]).await;

        // Exhaust the hourly limit (default 10).
        for _ in 0..10 {
            ctx.rate_limiter
                .record_order_attempt("npub1ratelimit");
        }

        let json = valid_fiat_order_json("rl-order-001");
        let result = process_order(&ctx, &json, "npub1ratelimit").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PurserError::RateLimited(_)));
    }

    #[tokio::test]
    async fn test_concurrent_session_rejected() {
        let provider: Arc<dyn crate::providers::PaymentProvider> =
            Arc::new(MockProvider::new("mock-fiat", vec![PaymentMethod::Fiat]));
        let ctx = make_context(vec![provider]).await;

        ctx.rate_limiter.set_active_session("npub1concurrent");

        let json = valid_fiat_order_json("cs-order-001");
        let result = process_order(&ctx, &json, "npub1concurrent").await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PurserError::ConcurrentSession(_)
        ));
    }

    #[tokio::test]
    async fn test_unsupported_payment_method() {
        // Provider only supports Fiat, but order requests Lightning.
        let provider: Arc<dyn crate::providers::PaymentProvider> =
            Arc::new(MockProvider::new("mock-fiat", vec![PaymentMethod::Fiat]));
        let ctx = make_context(vec![provider]).await;

        let json = valid_lightning_order_json("unsup-order-001");
        let result = process_order(&ctx, &json, "npub1unsup").await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PurserError::UnsupportedPaymentMethod(_)
        ));
    }

    #[tokio::test]
    async fn test_garbage_json() {
        let provider: Arc<dyn crate::providers::PaymentProvider> =
            Arc::new(MockProvider::new("mock-fiat", vec![PaymentMethod::Fiat]));
        let ctx = make_context(vec![provider]).await;

        let result =
            process_order(&ctx, "this is not json at all!!!", "npub1garbage").await;
        // Should NOT panic and should return Ok (garbage resilience).
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_provider_error_clears_session() {
        let provider: Arc<dyn crate::providers::PaymentProvider> = Arc::new(
            MockProvider::new("mock-fiat", vec![PaymentMethod::Fiat]).with_create_response(
                Err(PurserError::Provider {
                    provider: "mock-fiat".to_string(),
                    message: "API unavailable".to_string(),
                }),
            ),
        );
        let ctx = make_context(vec![provider]).await;

        let json = valid_fiat_order_json("perr-order-001");
        let result = process_order(&ctx, &json, "npub1perr").await;
        assert!(result.is_err());

        // Session should be cleared after provider error.
        assert!(!ctx.rate_limiter.has_active_session("npub1perr"));
    }

    #[tokio::test]
    async fn test_custom_quote_status() {
        let provider: Arc<dyn crate::providers::PaymentProvider> = Arc::new(
            MockProvider::new("mock-fiat", vec![PaymentMethod::Fiat]).with_create_response(Ok(
                ProviderPaymentRequest {
                    payment_id: "quote-pay-001".to_string(),
                    payment_details: HashMap::new(),
                    amount: "0.00".to_string(),
                    currency: "USD".to_string(),
                    expires_at: Utc::now() + ChronoDuration::minutes(15),
                },
            )),
        );
        let ctx = make_context(vec![provider]).await;

        let json = custom_quote_order_json("quote-order-001");
        let result = process_order(&ctx, &json, "npub1quote").await;
        assert!(result.is_ok(), "process_order failed: {:?}", result.err());

        let pending = ctx.state.pending_payments.read().await;
        let pp = pending.get("quote-order-001").unwrap();
        assert_eq!(pp.status, PendingPaymentStatus::PendingQuote);
    }

    #[tokio::test]
    async fn test_polling_event_completed() {
        let provider: Arc<dyn crate::providers::PaymentProvider> =
            Arc::new(MockProvider::new("mock-fiat", vec![PaymentMethod::Fiat]));
        let ctx = make_context(vec![provider]).await;

        // Seed a pending payment.
        let pp = PendingPayment {
            order_id: "poll-c-001".to_string(),
            customer_pubkey: "npub1pollc".to_string(),
            provider_name: "mock-fiat".to_string(),
            payment_id: "pay-poll-c".to_string(),
            payment_method: PaymentMethod::Fiat,
            amount: "59.99".to_string(),
            currency: "USD".to_string(),
            created_at: Utc::now(),
            expires_at: Utc::now() + ChronoDuration::minutes(15),
            status: PendingPaymentStatus::AwaitingPayment,
            group_id: "group-poll-c".to_string(),
        };
        ctx.rate_limiter.set_active_session("npub1pollc");
        ctx.state
            .pending_payments
            .write()
            .await
            .insert("poll-c-001".to_string(), pp);
        // Create the group so send_status_update can find it.
        ctx.nostr.create_checkout_group("npub1pollc").await.ok();

        let event = PollingEvent::Completed {
            order_id: "poll-c-001".to_string(),
            amount_paid: "59.99".to_string(),
            lightning_preimage: Some("preimage-abc".to_string()),
        };

        let result = handle_polling_event(&ctx, event).await;
        assert!(result.is_ok());

        // Payment should be removed.
        assert!(!ctx.state.pending_payments.read().await.contains_key("poll-c-001"));
        // Session should be cleared.
        assert!(!ctx.rate_limiter.has_active_session("npub1pollc"));
    }

    #[tokio::test]
    async fn test_polling_event_expired() {
        let provider: Arc<dyn crate::providers::PaymentProvider> =
            Arc::new(MockProvider::new("mock-fiat", vec![PaymentMethod::Fiat]));
        let ctx = make_context(vec![provider]).await;

        let pp = PendingPayment {
            order_id: "poll-e-001".to_string(),
            customer_pubkey: "npub1polle".to_string(),
            provider_name: "mock-fiat".to_string(),
            payment_id: "pay-poll-e".to_string(),
            payment_method: PaymentMethod::Fiat,
            amount: "59.99".to_string(),
            currency: "USD".to_string(),
            created_at: Utc::now(),
            expires_at: Utc::now() + ChronoDuration::minutes(15),
            status: PendingPaymentStatus::AwaitingPayment,
            group_id: "group-poll-e".to_string(),
        };
        ctx.rate_limiter.set_active_session("npub1polle");
        ctx.state
            .pending_payments
            .write()
            .await
            .insert("poll-e-001".to_string(), pp);
        ctx.nostr.create_checkout_group("npub1polle").await.ok();

        let event = PollingEvent::Expired {
            order_id: "poll-e-001".to_string(),
        };

        let result = handle_polling_event(&ctx, event).await;
        assert!(result.is_ok());

        assert!(!ctx.state.pending_payments.read().await.contains_key("poll-e-001"));
        assert!(!ctx.rate_limiter.has_active_session("npub1polle"));
    }

    #[tokio::test]
    async fn test_polling_event_failed() {
        let provider: Arc<dyn crate::providers::PaymentProvider> =
            Arc::new(MockProvider::new("mock-fiat", vec![PaymentMethod::Fiat]));
        let ctx = make_context(vec![provider]).await;

        let pp = PendingPayment {
            order_id: "poll-f-001".to_string(),
            customer_pubkey: "npub1pollf".to_string(),
            provider_name: "mock-fiat".to_string(),
            payment_id: "pay-poll-f".to_string(),
            payment_method: PaymentMethod::Fiat,
            amount: "59.99".to_string(),
            currency: "USD".to_string(),
            created_at: Utc::now(),
            expires_at: Utc::now() + ChronoDuration::minutes(15),
            status: PendingPaymentStatus::AwaitingPayment,
            group_id: "group-poll-f".to_string(),
        };
        ctx.rate_limiter.set_active_session("npub1pollf");
        ctx.state
            .pending_payments
            .write()
            .await
            .insert("poll-f-001".to_string(), pp);
        ctx.nostr.create_checkout_group("npub1pollf").await.ok();

        let event = PollingEvent::Failed {
            order_id: "poll-f-001".to_string(),
            reason: "card declined".to_string(),
        };

        let result = handle_polling_event(&ctx, event).await;
        assert!(result.is_ok());

        assert!(!ctx.state.pending_payments.read().await.contains_key("poll-f-001"));
        assert!(!ctx.rate_limiter.has_active_session("npub1pollf"));
    }

    /// Criteria #23 (full): Duplicate order from same pubkey with active
    /// session is rejected as ConcurrentSession (rate limiter runs before
    /// duplicate check). When session is cleared, the duplicate check returns
    /// the existing payment-request without creating a new payment.
    #[tokio::test]
    async fn test_duplicate_order_returns_existing_payment_request() {
        let provider: Arc<dyn crate::providers::PaymentProvider> =
            Arc::new(MockProvider::new("mock-fiat", vec![PaymentMethod::Fiat]));
        let ctx = make_context(vec![provider]).await;

        // First order succeeds.
        let json = valid_fiat_order_json("dup-order-001");
        let result = process_order(&ctx, &json, "npub1dup").await;
        assert!(result.is_ok());

        // Verify payment is pending.
        let pending_count = ctx.state.pending_payments.read().await.len();
        assert_eq!(pending_count, 1);

        // Clear the active session (simulating payment completion or manual clear)
        // so the duplicate check can run instead of ConcurrentSession.
        ctx.rate_limiter.clear_active_session("npub1dup");

        // Second order with same order_id — enters duplicate branch, returns
        // existing payment-request, no new payment created.
        let result2 = process_order(&ctx, &json, "npub1dup").await;
        assert!(result2.is_ok(), "duplicate order failed: {:?}", result2.err());

        let pending_count_after = ctx.state.pending_payments.read().await.len();
        assert_eq!(pending_count_after, 1, "duplicate order should not create a second payment");
    }

    /// Criteria #18: Processing time < 200ms (daemon overhead, excluding
    /// external API round-trip — MockProvider returns instantly).
    #[tokio::test]
    async fn test_processing_time_under_200ms() {
        let provider: Arc<dyn crate::providers::PaymentProvider> =
            Arc::new(MockProvider::new("mock-fiat", vec![PaymentMethod::Fiat]));
        let ctx = make_context(vec![provider]).await;

        let json = valid_fiat_order_json("timing-order-001");
        let start = tokio::time::Instant::now();
        let result = process_order(&ctx, &json, "npub1timing").await;
        let elapsed = start.elapsed();

        assert!(result.is_ok());
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "processing took {:?}, expected < 200ms",
            elapsed
        );
    }

    /// Criteria #27 (full): Provider returning errors does not crash the pipeline
    /// and the error is propagated cleanly.
    #[tokio::test]
    async fn test_provider_api_down_does_not_crash() {
        let provider: Arc<dyn crate::providers::PaymentProvider> = Arc::new(
            MockProvider::new("mock-fiat", vec![PaymentMethod::Fiat]).with_create_response(
                Err(PurserError::Provider {
                    provider: "mock-fiat".to_string(),
                    message: "500 Internal Server Error".to_string(),
                }),
            ),
        );
        let ctx = make_context(vec![provider]).await;

        let json = valid_fiat_order_json("err-order-001");
        let result = process_order(&ctx, &json, "npub1err").await;

        // Should propagate as an error, not panic.
        assert!(result.is_err());
        match result.unwrap_err() {
            PurserError::Provider { provider, message } => {
                assert_eq!(provider, "mock-fiat");
                assert!(message.contains("500"));
            }
            other => panic!("expected Provider error, got: {other:?}"),
        }

        // No pending payments should exist.
        assert!(ctx.state.pending_payments.read().await.is_empty());
    }

    #[tokio::test]
    async fn test_polling_event_missing_payment() {
        let provider: Arc<dyn crate::providers::PaymentProvider> =
            Arc::new(MockProvider::new("mock-fiat", vec![PaymentMethod::Fiat]));
        let ctx = make_context(vec![provider]).await;

        // No pending payment exists for this order_id — simulates a race condition.
        let event = PollingEvent::Completed {
            order_id: "nonexistent-order".to_string(),
            amount_paid: "100.00".to_string(),
            lightning_preimage: None,
        };

        let result = handle_polling_event(&ctx, event).await;
        // Should not panic, should return Ok.
        assert!(result.is_ok());
    }
}
