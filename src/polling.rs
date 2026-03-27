use crate::error::Result;
use crate::state::PendingPayment;

/// The polling engine drives payment status checks for all pending payments.
/// It is generic over PaymentProvider — it does not import Square or Strike directly.
pub struct PollingEngine {
    _private: (),
}

impl PollingEngine {
    /// Create a new polling engine.
    pub fn new() -> Self {
        todo!("Issue #7: initialize polling engine")
    }

    /// Register a pending payment for polling.
    pub async fn register(&self, _payment: &PendingPayment) -> Result<()> {
        todo!("Issue #7: register payment for polling")
    }

    /// Remove a payment from the pending set (on confirmation, expiry, or cancellation).
    pub async fn remove(&self, _order_id: &str) -> Result<()> {
        todo!("Issue #7: remove payment from polling")
    }

    /// Start the polling loop. Runs until cancelled.
    /// Calls check_status on providers for each pending payment according to their poll_config.
    pub async fn run(&self) -> Result<()> {
        todo!("Issue #7: implement polling loop")
    }

    /// Get the current count of pending payments.
    pub async fn pending_count(&self) -> usize {
        todo!("Issue #7: return pending count")
    }
}
