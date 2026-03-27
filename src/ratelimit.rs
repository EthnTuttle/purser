use crate::config::RateLimitConfig;
use crate::error::Result;

/// Per-pubkey rate limiter using in-memory sliding windows.
/// All state resets on daemon restart.
pub struct RateLimiter {
    _private: (),
}

impl RateLimiter {
    /// Create a new rate limiter with the given configuration.
    pub fn new(_config: RateLimitConfig) -> Self {
        todo!("Issue #8: initialize rate limiter")
    }

    /// Check if an order attempt from this pubkey is allowed.
    /// Returns Ok(()) if allowed, or an appropriate error if rate limited.
    pub fn check_order_allowed(&self, _customer_pubkey: &str) -> Result<()> {
        todo!("Issue #8: check rate limit")
    }

    /// Record a successful order attempt for this pubkey.
    pub fn record_order_attempt(&self, _customer_pubkey: &str) {
        todo!("Issue #8: record order attempt")
    }

    /// Record a failed/expired order for this pubkey.
    pub fn record_failure(&self, _customer_pubkey: &str) {
        todo!("Issue #8: record failure")
    }

    /// Check if this pubkey has an active (pending) checkout session.
    pub fn has_active_session(&self, _customer_pubkey: &str) -> bool {
        todo!("Issue #8: check active session")
    }

    /// Mark that this pubkey has an active checkout session.
    pub fn set_active_session(&self, _customer_pubkey: &str) {
        todo!("Issue #8: set active session")
    }

    /// Clear the active session for this pubkey.
    pub fn clear_active_session(&self, _customer_pubkey: &str) {
        todo!("Issue #8: clear active session")
    }
}
