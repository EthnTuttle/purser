use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::{mpsc, RwLock};
use tokio::time::Instant;

use crate::error::{PurserError, Result};
use crate::providers::{PaymentProvider, PaymentStatus};
use crate::state::PendingPayment;

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

struct PendingPollEntry {
    payment: PendingPayment,
    provider: Arc<dyn PaymentProvider>,
    current_interval: Duration,
    last_poll: Instant,
}

/// Events emitted by the polling engine to notify the rest of the system.
#[derive(Debug, Clone)]
pub enum PollingEvent {
    Completed {
        order_id: String,
        amount_paid: String,
        lightning_preimage: Option<String>,
    },
    Expired {
        order_id: String,
    },
    Failed {
        order_id: String,
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// PollingEngine
// ---------------------------------------------------------------------------

/// The polling engine drives payment status checks for all pending payments.
/// It is generic over PaymentProvider — it does not import Square or Strike directly.
pub struct PollingEngine {
    pending: Arc<RwLock<HashMap<String, PendingPollEntry>>>,
    providers: Arc<Vec<Arc<dyn PaymentProvider>>>,
    margin_percent: f64,
    event_tx: mpsc::Sender<PollingEvent>,
}

impl PollingEngine {
    /// Create a new polling engine.
    ///
    /// Returns the engine and a receiver for polling events.
    pub fn new(
        providers: Vec<Arc<dyn PaymentProvider>>,
        margin_percent: f64,
    ) -> (Self, mpsc::Receiver<PollingEvent>) {
        let (event_tx, event_rx) = mpsc::channel(256);
        let engine = Self {
            pending: Arc::new(RwLock::new(HashMap::new())),
            providers: Arc::new(providers),
            margin_percent,
            event_tx,
        };
        (engine, event_rx)
    }

    /// Register a pending payment for polling.
    pub async fn register(&self, payment: &PendingPayment) -> Result<()> {
        let provider = self
            .providers
            .iter()
            .find(|p| p.name() == payment.provider_name)
            .cloned()
            .ok_or_else(|| {
                PurserError::Provider {
                    provider: payment.provider_name.clone(),
                    message: format!(
                        "no provider registered with name '{}'",
                        payment.provider_name
                    ),
                }
            })?;

        let poll_config = provider.poll_config();
        let entry = PendingPollEntry {
            payment: payment.clone(),
            provider,
            current_interval: poll_config.initial_interval,
            last_poll: Instant::now(),
        };

        self.pending
            .write()
            .await
            .insert(payment.order_id.clone(), entry);

        Ok(())
    }

    /// Remove a payment from the pending set (on confirmation, expiry, or cancellation).
    pub async fn remove(&self, order_id: &str) -> Result<()> {
        self.pending
            .write()
            .await
            .remove(order_id)
            .ok_or_else(|| PurserError::PaymentNotFound(order_id.to_string()))?;
        Ok(())
    }

    /// Start the polling loop. Runs until cancelled.
    /// Calls check_status on providers for each pending payment according to their poll_config.
    pub async fn run(&self) -> Result<()> {
        loop {
            // Determine which entries need polling now.
            let entries_to_poll: Vec<(String, String, Arc<dyn PaymentProvider>)>;
            let mut expired_entries: Vec<(String, String)> = Vec::new();
            {
                let pending = self.pending.read().await;
                if pending.is_empty() {
                    drop(pending);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }

                let now_monotonic = Instant::now();
                let now_wall = Utc::now();

                // Collect entries that are due for a poll or have expired.
                let mut to_poll = Vec::new();
                for (order_id, entry) in pending.iter() {
                    // Check wall-clock expiry first.
                    if now_wall > entry.payment.expires_at {
                        expired_entries.push((
                            order_id.clone(),
                            entry.payment.payment_id.clone(),
                        ));
                        continue;
                    }

                    // Check if this entry is due for polling.
                    if now_monotonic.duration_since(entry.last_poll) >= entry.current_interval {
                        to_poll.push((
                            order_id.clone(),
                            entry.payment.payment_id.clone(),
                            Arc::clone(&entry.provider),
                        ));
                    }
                }
                entries_to_poll = to_poll;
            }

            // Handle expired entries.
            for (order_id, payment_id) in expired_entries {
                // Try to cancel with the provider; find the provider from the entry.
                let provider = {
                    let pending = self.pending.read().await;
                    pending.get(&order_id).map(|e| Arc::clone(&e.provider))
                };
                if let Some(provider) = provider {
                    if let Err(e) = provider.cancel_payment(&payment_id).await {
                        tracing::error!(
                            order_id = %order_id,
                            "failed to cancel expired payment: {e}"
                        );
                    }
                }
                let _ = self.event_tx.send(PollingEvent::Expired {
                    order_id: order_id.clone(),
                }).await;
                self.pending.write().await.remove(&order_id);
            }

            // Poll entries that are due.
            for (order_id, payment_id, provider) in entries_to_poll {
                let status_result = provider.check_status(&payment_id).await;
                match status_result {
                    Ok(PaymentStatus::Completed {
                        amount_paid,
                        lightning_preimage,
                    }) => {
                        // Check partial payment tolerance.
                        let within_margin = {
                            let pending = self.pending.read().await;
                            if let Some(entry) = pending.get(&order_id) {
                                check_partial_payment(
                                    &amount_paid,
                                    &entry.payment.amount,
                                    self.margin_percent,
                                    &order_id,
                                )
                            } else {
                                true
                            }
                        };

                        if within_margin {
                            let _ = self.event_tx.send(PollingEvent::Completed {
                                order_id: order_id.clone(),
                                amount_paid,
                                lightning_preimage,
                            }).await;
                        } else {
                            // Even outside margin, still emit Completed — the merchant
                            // can decide. The warning was already logged.
                            let _ = self.event_tx.send(PollingEvent::Completed {
                                order_id: order_id.clone(),
                                amount_paid,
                                lightning_preimage,
                            }).await;
                        }
                        self.pending.write().await.remove(&order_id);
                    }
                    Ok(PaymentStatus::Pending) => {
                        // Apply backoff.
                        let mut pending = self.pending.write().await;
                        if let Some(entry) = pending.get_mut(&order_id) {
                            let poll_config = entry.provider.poll_config();
                            let new_interval = Duration::from_secs_f64(
                                entry.current_interval.as_secs_f64()
                                    * poll_config.backoff_multiplier,
                            );
                            entry.current_interval = new_interval.min(poll_config.max_interval);
                            entry.last_poll = Instant::now();
                        }
                    }
                    Ok(PaymentStatus::Failed { reason }) => {
                        let _ = self.event_tx.send(PollingEvent::Failed {
                            order_id: order_id.clone(),
                            reason,
                        }).await;
                        self.pending.write().await.remove(&order_id);
                    }
                    Ok(PaymentStatus::Expired) => {
                        let _ = self.event_tx.send(PollingEvent::Failed {
                            order_id: order_id.clone(),
                            reason: "payment expired (provider reported)".to_string(),
                        }).await;
                        self.pending.write().await.remove(&order_id);
                    }
                    Err(e) => {
                        tracing::error!(
                            order_id = %order_id,
                            "provider check_status error: {e}"
                        );
                        // Apply backoff but don't remove.
                        let mut pending = self.pending.write().await;
                        if let Some(entry) = pending.get_mut(&order_id) {
                            let poll_config = entry.provider.poll_config();
                            let new_interval = Duration::from_secs_f64(
                                entry.current_interval.as_secs_f64()
                                    * poll_config.backoff_multiplier,
                            );
                            entry.current_interval = new_interval.min(poll_config.max_interval);
                            entry.last_poll = Instant::now();
                        }
                    }
                }
            }

            // Sleep until the next poll is due.
            let sleep_duration = {
                let pending = self.pending.read().await;
                if pending.is_empty() {
                    Duration::from_secs(1)
                } else {
                    let now = Instant::now();
                    pending
                        .values()
                        .map(|entry| {
                            let elapsed = now.duration_since(entry.last_poll);
                            if elapsed >= entry.current_interval {
                                Duration::ZERO
                            } else {
                                entry.current_interval - elapsed
                            }
                        })
                        .min()
                        .unwrap_or(Duration::from_secs(1))
                }
            };

            if sleep_duration > Duration::ZERO {
                tokio::time::sleep(sleep_duration).await;
            }
        }
    }

    /// Get the current count of pending payments.
    pub async fn pending_count(&self) -> usize {
        self.pending.read().await.len()
    }
}

/// Check whether a partial payment is within the configured margin.
/// Returns `true` if within tolerance (or if amounts can't be parsed).
fn check_partial_payment(
    amount_paid: &str,
    expected_amount: &str,
    margin_percent: f64,
    order_id: &str,
) -> bool {
    let paid: f64 = match amount_paid.parse() {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!(
                order_id = %order_id,
                "could not parse amount_paid '{}' as f64",
                amount_paid
            );
            return true;
        }
    };
    let expected: f64 = match expected_amount.parse() {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!(
                order_id = %order_id,
                "could not parse expected amount '{}' as f64",
                expected_amount
            );
            return true;
        }
    };

    if expected == 0.0 {
        return true;
    }

    let diff_percent = ((paid - expected) / expected).abs() * 100.0;
    if diff_percent > margin_percent {
        tracing::warn!(
            order_id = %order_id,
            amount_paid = %amount_paid,
            expected_amount = %expected_amount,
            diff_percent = %diff_percent,
            margin_percent = %margin_percent,
            "payment amount outside margin tolerance"
        );
        return false;
    }

    if diff_percent > 0.0 {
        tracing::warn!(
            order_id = %order_id,
            amount_paid = %amount_paid,
            expected_amount = %expected_amount,
            diff_percent = %diff_percent,
            "partial payment discrepancy (within margin)"
        );
    }

    true
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{
        PaymentMethod, PaymentStatus, PollConfig, ProviderPaymentRequest, RateLimitStrategy,
        ValidatedOrder,
    };
    use crate::state::PendingPaymentStatus;
    use async_trait::async_trait;
    use chrono::{Duration as ChronoDuration, Utc};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Mutex;

    // -----------------------------------------------------------------------
    // MockProvider
    // -----------------------------------------------------------------------

    struct MockProvider {
        name: String,
        status_responses: Mutex<Vec<std::result::Result<PaymentStatus, PurserError>>>,
        check_count: AtomicUsize,
        cancel_count: AtomicUsize,
        poll_config: PollConfig,
    }

    impl MockProvider {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                status_responses: Mutex::new(Vec::new()),
                check_count: AtomicUsize::new(0),
                cancel_count: AtomicUsize::new(0),
                poll_config: PollConfig {
                    initial_interval: Duration::from_millis(10),
                    backoff_multiplier: 2.0,
                    max_interval: Duration::from_secs(60),
                    rate_limit_strategy: RateLimitStrategy::None,
                },
            }
        }

        fn with_responses(
            name: &str,
            responses: Vec<std::result::Result<PaymentStatus, PurserError>>,
        ) -> Self {
            let mut mock = Self::new(name);
            mock.status_responses = Mutex::new(responses);
            mock
        }

        fn with_poll_config(mut self, config: PollConfig) -> Self {
            self.poll_config = config;
            self
        }
    }

    #[async_trait]
    impl PaymentProvider for MockProvider {
        fn name(&self) -> &str {
            &self.name
        }

        fn supported_methods(&self) -> Vec<PaymentMethod> {
            vec![PaymentMethod::Lightning]
        }

        async fn create_payment(
            &self,
            _order: &ValidatedOrder,
        ) -> Result<ProviderPaymentRequest> {
            unimplemented!("not needed for polling tests")
        }

        async fn check_status(&self, _payment_id: &str) -> Result<PaymentStatus> {
            self.check_count.fetch_add(1, Ordering::SeqCst);
            let mut responses = self.status_responses.lock().await;
            if responses.is_empty() {
                Ok(PaymentStatus::Pending)
            } else {
                responses.remove(0)
            }
        }

        async fn cancel_payment(&self, _payment_id: &str) -> Result<()> {
            self.cancel_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn poll_config(&self) -> PollConfig {
            self.poll_config.clone()
        }
    }

    // -----------------------------------------------------------------------
    // Helper to create a PendingPayment
    // -----------------------------------------------------------------------

    fn make_pending_payment(order_id: &str, provider_name: &str, amount: &str) -> PendingPayment {
        PendingPayment {
            order_id: order_id.to_string(),
            customer_pubkey: "npub_test".to_string(),
            provider_name: provider_name.to_string(),
            payment_id: format!("pay_{order_id}"),
            payment_method: PaymentMethod::Lightning,
            amount: amount.to_string(),
            currency: "USD".to_string(),
            created_at: Utc::now(),
            expires_at: Utc::now() + ChronoDuration::minutes(15),
            status: PendingPaymentStatus::AwaitingPayment,
            group_id: "group_test".to_string(),
        }
    }

    fn make_expired_payment(order_id: &str, provider_name: &str) -> PendingPayment {
        PendingPayment {
            order_id: order_id.to_string(),
            customer_pubkey: "npub_test".to_string(),
            provider_name: provider_name.to_string(),
            payment_id: format!("pay_{order_id}"),
            payment_method: PaymentMethod::Lightning,
            amount: "100.00".to_string(),
            currency: "USD".to_string(),
            created_at: Utc::now() - ChronoDuration::minutes(20),
            expires_at: Utc::now() - ChronoDuration::minutes(1),
            status: PendingPaymentStatus::AwaitingPayment,
            group_id: "group_test".to_string(),
        }
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_register_and_pending_count() {
        let provider: Arc<dyn PaymentProvider> = Arc::new(MockProvider::new("mock"));
        let (engine, _rx) = PollingEngine::new(vec![provider], 2.0);

        let p1 = make_pending_payment("order-1", "mock", "100.00");
        let p2 = make_pending_payment("order-2", "mock", "200.00");

        engine.register(&p1).await.unwrap();
        engine.register(&p2).await.unwrap();

        assert_eq!(engine.pending_count().await, 2);
    }

    #[tokio::test]
    async fn test_remove_payment() {
        let provider: Arc<dyn PaymentProvider> = Arc::new(MockProvider::new("mock"));
        let (engine, _rx) = PollingEngine::new(vec![provider], 2.0);

        let p1 = make_pending_payment("order-1", "mock", "100.00");
        engine.register(&p1).await.unwrap();
        assert_eq!(engine.pending_count().await, 1);

        engine.remove("order-1").await.unwrap();
        assert_eq!(engine.pending_count().await, 0);
    }

    #[tokio::test]
    async fn test_remove_nonexistent() {
        let provider: Arc<dyn PaymentProvider> = Arc::new(MockProvider::new("mock"));
        let (engine, _rx) = PollingEngine::new(vec![provider], 2.0);

        let result = engine.remove("nonexistent").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            PurserError::PaymentNotFound(id) => assert_eq!(id, "nonexistent"),
            other => panic!("expected PaymentNotFound, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_polling_event_completed() {
        let provider: Arc<dyn PaymentProvider> = Arc::new(MockProvider::with_responses(
            "mock",
            vec![Ok(PaymentStatus::Completed {
                amount_paid: "100.00".to_string(),
                lightning_preimage: Some("preimage123".to_string()),
            })],
        ));
        let (engine, mut rx) = PollingEngine::new(vec![provider], 2.0);

        let payment = make_pending_payment("order-c", "mock", "100.00");
        engine.register(&payment).await.unwrap();

        // Spawn the run loop briefly.
        let engine_ref = &engine;
        let handle = tokio::spawn({
            let pending = Arc::clone(&engine_ref.pending);
            let providers = Arc::clone(&engine_ref.providers);
            let event_tx = engine_ref.event_tx.clone();
            let margin = engine_ref.margin_percent;
            async move {
                let eng = PollingEngine {
                    pending,
                    providers,
                    margin_percent: margin,
                    event_tx,
                };
                eng.run().await
            }
        });

        // Wait for the event.
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for event")
            .expect("channel closed");

        match event {
            PollingEvent::Completed {
                order_id,
                amount_paid,
                lightning_preimage,
            } => {
                assert_eq!(order_id, "order-c");
                assert_eq!(amount_paid, "100.00");
                assert_eq!(lightning_preimage, Some("preimage123".to_string()));
            }
            other => panic!("expected Completed, got: {other:?}"),
        }

        handle.abort();
    }

    #[tokio::test]
    async fn test_polling_event_expired() {
        let provider: Arc<dyn PaymentProvider> = Arc::new(MockProvider::new("mock"));
        let (engine, mut rx) = PollingEngine::new(vec![provider], 2.0);

        let payment = make_expired_payment("order-exp", "mock");
        engine.register(&payment).await.unwrap();

        let handle = tokio::spawn({
            let pending = Arc::clone(&engine.pending);
            let providers = Arc::clone(&engine.providers);
            let event_tx = engine.event_tx.clone();
            let margin = engine.margin_percent;
            async move {
                let eng = PollingEngine {
                    pending,
                    providers,
                    margin_percent: margin,
                    event_tx,
                };
                eng.run().await
            }
        });

        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for event")
            .expect("channel closed");

        match event {
            PollingEvent::Expired { order_id } => {
                assert_eq!(order_id, "order-exp");
            }
            other => panic!("expected Expired, got: {other:?}"),
        }

        handle.abort();
    }

    #[tokio::test]
    async fn test_backoff_increases() {
        let provider: Arc<dyn PaymentProvider> = Arc::new(MockProvider::with_responses(
            "mock",
            vec![
                Ok(PaymentStatus::Pending),
                Ok(PaymentStatus::Pending),
                Ok(PaymentStatus::Pending),
            ],
        ));
        let (engine, _rx) = PollingEngine::new(vec![Arc::clone(&provider)], 2.0);

        let payment = make_pending_payment("order-bo", "mock", "100.00");
        engine.register(&payment).await.unwrap();

        let initial_interval = {
            let pending = engine.pending.read().await;
            pending.get("order-bo").unwrap().current_interval
        };

        // Spawn run loop briefly to let a couple polls happen.
        let handle = tokio::spawn({
            let pending = Arc::clone(&engine.pending);
            let providers = Arc::clone(&engine.providers);
            let event_tx = engine.event_tx.clone();
            let margin = engine.margin_percent;
            async move {
                let eng = PollingEngine {
                    pending,
                    providers,
                    margin_percent: margin,
                    event_tx,
                };
                eng.run().await
            }
        });

        // Wait long enough for at least 2 polls (initial_interval is 10ms, backoff *2).
        tokio::time::sleep(Duration::from_millis(200)).await;

        let current_interval = {
            let pending = engine.pending.read().await;
            pending.get("order-bo").map(|e| e.current_interval)
        };

        handle.abort();

        // Interval should have grown.
        if let Some(interval) = current_interval {
            assert!(
                interval > initial_interval,
                "expected interval to grow from {initial_interval:?}, but got {interval:?}"
            );
        }
        // If the entry was removed (unlikely), the test still passes — it means
        // all responses were consumed.
    }

    #[tokio::test]
    async fn test_partial_payment_within_margin() {
        // 98.00 paid for 100.00 expected = 2% short, within 2% margin.
        let provider: Arc<dyn PaymentProvider> = Arc::new(MockProvider::with_responses(
            "mock",
            vec![Ok(PaymentStatus::Completed {
                amount_paid: "98.00".to_string(),
                lightning_preimage: None,
            })],
        ));
        let (engine, mut rx) = PollingEngine::new(vec![provider], 2.0);

        let payment = make_pending_payment("order-pm", "mock", "100.00");
        engine.register(&payment).await.unwrap();

        let handle = tokio::spawn({
            let pending = Arc::clone(&engine.pending);
            let providers = Arc::clone(&engine.providers);
            let event_tx = engine.event_tx.clone();
            let margin = engine.margin_percent;
            async move {
                let eng = PollingEngine {
                    pending,
                    providers,
                    margin_percent: margin,
                    event_tx,
                };
                eng.run().await
            }
        });

        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");

        match event {
            PollingEvent::Completed {
                order_id,
                amount_paid,
                ..
            } => {
                assert_eq!(order_id, "order-pm");
                assert_eq!(amount_paid, "98.00");
            }
            other => panic!("expected Completed, got: {other:?}"),
        }

        handle.abort();
    }

    #[tokio::test]
    async fn test_idle_state() {
        let provider = Arc::new(MockProvider::new("mock"));
        let check_count_before = provider.check_count.load(Ordering::SeqCst);

        let (engine, _rx) = PollingEngine::new(vec![Arc::clone(&provider) as Arc<dyn PaymentProvider>], 2.0);

        // Run the engine with no pending payments for a bit.
        let handle = tokio::spawn({
            let pending = Arc::clone(&engine.pending);
            let providers = Arc::clone(&engine.providers);
            let event_tx = engine.event_tx.clone();
            let margin = engine.margin_percent;
            async move {
                let eng = PollingEngine {
                    pending,
                    providers,
                    margin_percent: margin,
                    event_tx,
                };
                eng.run().await
            }
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        handle.abort();

        let check_count_after = provider.check_count.load(Ordering::SeqCst);
        assert_eq!(
            check_count_before, check_count_after,
            "expected zero check_status calls when idle"
        );
    }
}
