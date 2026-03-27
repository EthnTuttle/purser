use crate::error::Result;
use crate::messages::{PaymentRequest, StatusUpdate};

/// Handle to the MDK-backed Nostr communication layer.
pub struct NostrClient {
    // MDK instance, relay connections, etc.
    _private: (),
}

impl NostrClient {
    /// Initialize MDK with configured relays and storage backend.
    pub async fn new(_relays: &[String], _storage_type: &str) -> Result<Self> {
        todo!("Issue #4: initialize MDK client")
    }

    /// Create a 1:1 MLS group for a checkout session.
    pub async fn create_checkout_group(
        &self,
        _customer_pubkey: &str,
    ) -> Result<String> {
        todo!("Issue #4: create MLS group")
    }

    /// Send a payment-request message to a checkout group.
    pub async fn send_payment_request(
        &self,
        _group_id: &str,
        _payment_request: &PaymentRequest,
    ) -> Result<()> {
        todo!("Issue #4: send payment request via MDK")
    }

    /// Send a status-update message to a checkout group.
    pub async fn send_status_update(
        &self,
        _group_id: &str,
        _status_update: &StatusUpdate,
    ) -> Result<()> {
        todo!("Issue #4: send status update via MDK")
    }

    /// Send an error message to a checkout group.
    pub async fn send_error(
        &self,
        _group_id: &str,
        _error_message: &str,
    ) -> Result<()> {
        todo!("Issue #4: send error via MDK")
    }

    /// Mark a checkout group as inactive (after terminal status).
    pub async fn deactivate_group(&self, _group_id: &str) -> Result<()> {
        todo!("Issue #4: deactivate MLS group")
    }

    /// Regenerate key packages for MDK.
    pub async fn regenerate_key_packages(&self) -> Result<()> {
        todo!("Issue #4: regenerate key packages")
    }

    /// Purge stale MLS groups older than the given number of days.
    pub async fn purge_stale_groups(&self, _max_age_days: u64) -> Result<()> {
        todo!("Issue #4: purge stale groups")
    }
}
