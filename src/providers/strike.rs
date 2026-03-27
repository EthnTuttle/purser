use async_trait::async_trait;
use std::time::Duration;

use crate::error::Result;
use crate::providers::{
    PaymentMethod, PaymentProvider, PaymentStatus, PollConfig, ProviderPaymentRequest,
    RateLimitStrategy, ValidatedOrder,
};

pub struct StrikeProvider {
    _api_key: String,
}

impl StrikeProvider {
    pub fn new(api_key: String) -> Self {
        Self { _api_key: api_key }
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

    async fn create_payment(&self, _order: &ValidatedOrder) -> Result<ProviderPaymentRequest> {
        todo!("Issue #6: Strike create_payment")
    }

    async fn check_status(&self, _payment_id: &str) -> Result<PaymentStatus> {
        todo!("Issue #6: Strike check_status")
    }

    async fn cancel_payment(&self, _payment_id: &str) -> Result<()> {
        todo!("Issue #6: Strike cancel_payment")
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
