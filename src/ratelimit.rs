use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::RateLimitConfig;
use crate::error::{PurserError, Result};

/// Per-pubkey rate limiter using in-memory sliding windows.
/// All state resets on daemon restart.
pub struct RateLimiter {
    config: RateLimitConfig,
    /// Per-pubkey timestamps of order attempts (sliding 1-hour window).
    order_attempts: Mutex<HashMap<String, VecDeque<Instant>>>,
    /// Per-pubkey timestamps of failed/expired orders (sliding 24-hour window).
    failures: Mutex<HashMap<String, VecDeque<Instant>>>,
    /// Pubkeys blocked until a given instant.
    blocked_until: Mutex<HashMap<String, Instant>>,
    /// Pubkeys with an active (pending) checkout session.
    active_sessions: Mutex<HashSet<String>>,
}

impl RateLimiter {
    /// Create a new rate limiter with the given configuration.
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            order_attempts: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            blocked_until: Mutex::new(HashMap::new()),
            active_sessions: Mutex::new(HashSet::new()),
        }
    }

    /// Check if an order attempt from this pubkey is allowed.
    /// Returns Ok(()) if allowed, or an appropriate error if rate limited.
    pub fn check_order_allowed(&self, customer_pubkey: &str) -> Result<()> {
        // 1. Check if blocked
        {
            let mut blocked = self.blocked_until.lock().unwrap();
            if let Some(&block_time) = blocked.get(customer_pubkey) {
                if Instant::now() < block_time {
                    return Err(PurserError::RateLimited(
                        "temporarily blocked due to repeated failures".to_string(),
                    ));
                }
                // Block expired, remove it
                blocked.remove(customer_pubkey);
            }
        }

        // 2. Check hourly order limit
        {
            let mut attempts = self.order_attempts.lock().unwrap();
            let one_hour_ago = Instant::now() - Duration::from_secs(3600);
            if let Some(deque) = attempts.get_mut(customer_pubkey) {
                // Prune old entries
                while deque.front().is_some_and(|t| *t < one_hour_ago) {
                    deque.pop_front();
                }
                if deque.len() >= self.config.max_orders_per_hour as usize {
                    return Err(PurserError::RateLimited(
                        "too many orders this hour".to_string(),
                    ));
                }
            }
        }

        // 3. Check failure limit (may trigger a new block)
        {
            let mut failures = self.failures.lock().unwrap();
            let twenty_four_hours_ago = Instant::now() - Duration::from_secs(86400);
            if let Some(deque) = failures.get_mut(customer_pubkey) {
                while deque.front().is_some_and(|t| *t < twenty_four_hours_ago) {
                    deque.pop_front();
                }
                if deque.len() >= self.config.max_failures_per_day as usize {
                    // Block the pubkey
                    let block_duration =
                        Duration::from_secs(self.config.block_duration_hours * 3600);
                    self.blocked_until
                        .lock()
                        .unwrap()
                        .insert(customer_pubkey.to_string(), Instant::now() + block_duration);
                    return Err(PurserError::RateLimited(
                        "too many failed orders, temporarily blocked".to_string(),
                    ));
                }
            }
        }

        // 4. Check concurrent session
        {
            let sessions = self.active_sessions.lock().unwrap();
            if sessions.contains(customer_pubkey) {
                return Err(PurserError::ConcurrentSession(
                    customer_pubkey.to_string(),
                ));
            }
        }

        Ok(())
    }

    /// Record a successful order attempt for this pubkey.
    pub fn record_order_attempt(&self, customer_pubkey: &str) {
        let mut attempts = self.order_attempts.lock().unwrap();
        let deque = attempts
            .entry(customer_pubkey.to_string())
            .or_insert_with(VecDeque::new);
        deque.push_back(Instant::now());

        // Prune entries older than 1 hour
        let one_hour_ago = Instant::now() - Duration::from_secs(3600);
        while deque.front().is_some_and(|t| *t < one_hour_ago) {
            deque.pop_front();
        }
    }

    /// Record a failed/expired order for this pubkey.
    pub fn record_failure(&self, customer_pubkey: &str) {
        let mut failures = self.failures.lock().unwrap();
        let deque = failures
            .entry(customer_pubkey.to_string())
            .or_insert_with(VecDeque::new);
        deque.push_back(Instant::now());

        // Prune entries older than 24 hours
        let twenty_four_hours_ago = Instant::now() - Duration::from_secs(86400);
        while deque.front().is_some_and(|t| *t < twenty_four_hours_ago) {
            deque.pop_front();
        }

        // If failures exceed threshold, block the pubkey
        if deque.len() >= self.config.max_failures_per_day as usize {
            let block_duration = Duration::from_secs(self.config.block_duration_hours * 3600);
            self.blocked_until
                .lock()
                .unwrap()
                .insert(customer_pubkey.to_string(), Instant::now() + block_duration);
        }
    }

    /// Check if this pubkey has an active (pending) checkout session.
    pub fn has_active_session(&self, customer_pubkey: &str) -> bool {
        self.active_sessions.lock().unwrap().contains(customer_pubkey)
    }

    /// Mark that this pubkey has an active checkout session.
    pub fn set_active_session(&self, customer_pubkey: &str) {
        self.active_sessions
            .lock()
            .unwrap()
            .insert(customer_pubkey.to_string());
    }

    /// Clear the active session for this pubkey.
    pub fn clear_active_session(&self, customer_pubkey: &str) {
        self.active_sessions.lock().unwrap().remove(customer_pubkey);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> RateLimitConfig {
        RateLimitConfig {
            max_orders_per_hour: 10,
            max_failures_per_day: 3,
            block_duration_hours: 24,
        }
    }

    #[test]
    fn test_order_within_limit() {
        let limiter = RateLimiter::new(default_config());
        // 10 orders allowed (check before recording each)
        for i in 0..10 {
            assert!(limiter.check_order_allowed("pubkey-a").is_ok(), "order {i} should be allowed");
            limiter.record_order_attempt("pubkey-a");
        }
    }

    #[test]
    fn test_order_exceeds_limit() {
        let limiter = RateLimiter::new(default_config());
        for _ in 0..10 {
            limiter.record_order_attempt("pubkey-a");
        }
        let result = limiter.check_order_allowed("pubkey-a");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("too many orders"));
    }

    #[test]
    fn test_failure_triggers_block() {
        let limiter = RateLimiter::new(default_config());
        for _ in 0..3 {
            limiter.record_failure("pubkey-b");
        }
        let result = limiter.check_order_allowed("pubkey-b");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("blocked"));
    }

    #[test]
    fn test_concurrent_session_rejected() {
        let limiter = RateLimiter::new(default_config());
        limiter.set_active_session("pubkey-c");
        let result = limiter.check_order_allowed("pubkey-c");
        assert!(result.is_err());
        match result.unwrap_err() {
            PurserError::ConcurrentSession(pk) => assert_eq!(pk, "pubkey-c"),
            other => panic!("expected ConcurrentSession, got: {other}"),
        }
    }

    #[test]
    fn test_clear_session_allows_new() {
        let limiter = RateLimiter::new(default_config());
        limiter.set_active_session("pubkey-d");
        assert!(limiter.has_active_session("pubkey-d"));
        limiter.clear_active_session("pubkey-d");
        assert!(!limiter.has_active_session("pubkey-d"));
        assert!(limiter.check_order_allowed("pubkey-d").is_ok());
    }

    #[test]
    fn test_independent_pubkeys() {
        let limiter = RateLimiter::new(default_config());
        for _ in 0..10 {
            limiter.record_order_attempt("pubkey-e");
        }
        assert!(limiter.check_order_allowed("pubkey-e").is_err());
        assert!(limiter.check_order_allowed("pubkey-f").is_ok());
    }

    #[test]
    fn test_blocked_pubkey_rejected() {
        let limiter = RateLimiter::new(default_config());
        limiter
            .blocked_until
            .lock()
            .unwrap()
            .insert("pubkey-g".to_string(), Instant::now() + Duration::from_secs(3600));
        let result = limiter.check_order_allowed("pubkey-g");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("blocked"));
    }

    #[test]
    fn test_has_active_session() {
        let limiter = RateLimiter::new(default_config());
        assert!(!limiter.has_active_session("pubkey-h"));
        limiter.set_active_session("pubkey-h");
        assert!(limiter.has_active_session("pubkey-h"));
    }
}
