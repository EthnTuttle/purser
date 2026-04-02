pub mod mdk_trait;
pub mod mock_mdk;

use crate::error::Result;
use crate::messages::{PaymentRequest, StatusUpdate};
use mdk_trait::MdkClient;
use mock_mdk::MockMdkClient;

/// Handle to the MDK-backed Nostr communication layer.
/// Delegates all operations through the MdkClient trait.
pub struct NostrClient {
    mdk: Box<dyn MdkClient>,
    relays: Vec<String>,
}

// TODO: Replace MockMdkClient with real MDK when mdk-core is available
impl NostrClient {
    /// Initialize with configured relays and storage backend.
    pub async fn new(relays: &[String], _storage_type: &str) -> Result<Self> {
        tracing::info!(relay_count = relays.len(), "initializing NostrClient (mock MDK)");
        Ok(Self {
            mdk: Box::new(MockMdkClient::new()),
            relays: relays.to_vec(),
        })
    }

    /// Create a 1:1 MLS group for a checkout session.
    pub async fn create_checkout_group(
        &self,
        customer_pubkey: &str,
    ) -> Result<String> {
        let group_id = self.mdk.create_group(customer_pubkey).await?;
        tracing::info!(group_id = %group_id, customer = %customer_pubkey, "created checkout group");
        Ok(group_id)
    }

    /// Send a payment-request message to a checkout group.
    pub async fn send_payment_request(
        &self,
        group_id: &str,
        payment_request: &PaymentRequest,
    ) -> Result<()> {
        let json = serde_json::to_string(payment_request)
            .map_err(|e| crate::error::PurserError::Nostr(format!("serialize payment-request: {e}")))?;
        self.mdk.send_message(group_id, &json).await
    }

    /// Send a status-update message to a checkout group.
    pub async fn send_status_update(
        &self,
        group_id: &str,
        status_update: &StatusUpdate,
    ) -> Result<()> {
        let json = serde_json::to_string(status_update)
            .map_err(|e| crate::error::PurserError::Nostr(format!("serialize status-update: {e}")))?;
        self.mdk.send_message(group_id, &json).await
    }

    /// Send an error message to a checkout group.
    pub async fn send_error(
        &self,
        group_id: &str,
        error_message: &str,
    ) -> Result<()> {
        let json = serde_json::json!({
            "version": 1,
            "type": "error",
            "message": error_message
        })
        .to_string();
        self.mdk.send_message(group_id, &json).await
    }

    /// Mark a checkout group as inactive (after terminal status).
    pub async fn deactivate_group(&self, group_id: &str) -> Result<()> {
        self.mdk.deactivate_group(group_id).await
    }

    /// Regenerate key packages for MDK.
    pub async fn regenerate_key_packages(&self) -> Result<()> {
        self.mdk.create_key_package().await
    }

    /// Purge stale MLS groups older than the given number of days.
    pub async fn purge_stale_groups(&self, max_age_days: u64) -> Result<()> {
        self.mdk.purge_stale_groups(max_age_days).await
    }

    /// Get configured relays.
    pub fn relays(&self) -> &[String] {
        &self.relays
    }

    /// Get a reference to the underlying MdkClient (for testing).
    #[cfg(test)]
    fn mock_mdk(&self) -> &MockMdkClient {
        // Safe in tests: we know the concrete type is MockMdkClient
        let ptr = &*self.mdk as *const dyn MdkClient as *const MockMdkClient;
        unsafe { &*ptr }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::OrderStatus;
    use chrono::Utc;
    use std::collections::HashMap;

    async fn make_client() -> NostrClient {
        NostrClient::new(&["wss://relay1.example".into(), "wss://relay2.example".into()], "memory")
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_create_checkout_group() {
        let client = make_client().await;
        let group_id = client.create_checkout_group("npub1customer").await.unwrap();
        assert!(!group_id.is_empty());
        assert!(uuid::Uuid::parse_str(&group_id).is_ok());
    }

    #[tokio::test]
    async fn test_send_payment_request() {
        let client = make_client().await;
        let group_id = client.create_checkout_group("npub1customer").await.unwrap();

        let pr = PaymentRequest {
            version: 1,
            msg_type: "payment-request".to_string(),
            order_id: "order-123".to_string(),
            payment_provider: "square".to_string(),
            payment_id: "pay-456".to_string(),
            payment_details: HashMap::from([("checkout_url".to_string(), "https://example.com".to_string())]),
            amount: "59.99".to_string(),
            currency: "USD".to_string(),
            expires_at: Utc::now(),
        };

        client.send_payment_request(&group_id, &pr).await.unwrap();

        let messages = client.mock_mdk().sent_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].0, group_id);
        let parsed: serde_json::Value = serde_json::from_str(&messages[0].1).unwrap();
        assert_eq!(parsed["type"], "payment-request");
        assert_eq!(parsed["order_id"], "order-123");
    }

    #[tokio::test]
    async fn test_send_status_update() {
        let client = make_client().await;
        let group_id = client.create_checkout_group("npub1customer").await.unwrap();

        let su = StatusUpdate {
            version: 1,
            msg_type: "status-update".to_string(),
            order_id: "order-123".to_string(),
            status: OrderStatus::Paid,
            payment_provider: "strike".to_string(),
            payment_id: "inv-789".to_string(),
            amount: "100.00".to_string(),
            currency: "USD".to_string(),
            timestamp: Utc::now(),
            lightning_preimage: Some("preimage123".to_string()),
            tracking: None,
            message: None,
        };

        client.send_status_update(&group_id, &su).await.unwrap();

        let messages = client.mock_mdk().sent_messages();
        assert_eq!(messages.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(&messages[0].1).unwrap();
        assert_eq!(parsed["type"], "status-update");
        assert_eq!(parsed["status"], "paid");
    }

    #[tokio::test]
    async fn test_send_error() {
        let client = make_client().await;
        let group_id = client.create_checkout_group("npub1customer").await.unwrap();

        client.send_error(&group_id, "invalid order format").await.unwrap();

        let messages = client.mock_mdk().sent_messages();
        assert_eq!(messages.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(&messages[0].1).unwrap();
        assert_eq!(parsed["type"], "error");
        assert_eq!(parsed["version"], 1);
        assert_eq!(parsed["message"], "invalid order format");
    }

    #[tokio::test]
    async fn test_deactivate_group() {
        let client = make_client().await;
        let group_id = client.create_checkout_group("npub1customer").await.unwrap();
        client.deactivate_group(&group_id).await.unwrap();
    }

    #[tokio::test]
    async fn test_regenerate_key_packages() {
        let client = make_client().await;
        client.regenerate_key_packages().await.unwrap();
    }

    #[tokio::test]
    async fn test_purge_stale_groups() {
        let client = make_client().await;
        client.purge_stale_groups(7).await.unwrap();
    }

    /// Criteria #6: Relay connection — daemon initializes with configured relays.
    #[tokio::test]
    async fn test_connect_with_multiple_relays() {
        let relays = vec![
            "wss://relay1.example".into(),
            "wss://relay2.example".into(),
            "wss://relay3.example".into(),
        ];
        let client = NostrClient::new(&relays, "memory").await.unwrap();
        assert_eq!(client.relays().len(), 3);
    }

    /// Criteria #8: MLS group creation — customer pubkey triggers group creation
    /// which returns a valid UUID group_id and allows message sending.
    #[tokio::test]
    async fn test_mls_group_creation_and_messaging() {
        let client = make_client().await;
        let group_id = client.create_checkout_group("npub1newcustomer").await.unwrap();
        assert!(!group_id.is_empty());
        assert!(uuid::Uuid::parse_str(&group_id).is_ok());

        // Should be able to send messages to the new group.
        client.send_error(&group_id, "test message").await.unwrap();

        // Group should be active.
        let active = client.mock_mdk().active_groups();
        assert!(active.contains(&group_id));
    }

    /// Criteria #30: Customer offline — messages are published to relay regardless
    /// of customer connectivity (the daemon just sends, relay stores).
    #[tokio::test]
    async fn test_message_sent_regardless_of_customer_state() {
        let client = make_client().await;
        let group_id = client.create_checkout_group("npub1offline").await.unwrap();

        // Sending status update should succeed even if customer is "offline"
        // (the mock always succeeds, matching the real behavior where messages
        // are published to relays for later retrieval).
        let su = StatusUpdate {
            version: 1,
            msg_type: "status-update".to_string(),
            order_id: "order-offline".to_string(),
            status: OrderStatus::Paid,
            payment_provider: "strike".to_string(),
            payment_id: "inv-offline".to_string(),
            amount: "50.00".to_string(),
            currency: "USD".to_string(),
            timestamp: Utc::now(),
            lightning_preimage: None,
            tracking: None,
            message: None,
        };
        client.send_status_update(&group_id, &su).await.unwrap();

        // Message was sent (will be stored on relay for customer to retrieve).
        let messages = client.mock_mdk().sent_messages();
        assert_eq!(messages.len(), 1);
    }

    #[tokio::test]
    async fn test_relays_stored() {
        let client = make_client().await;
        assert_eq!(client.relays().len(), 2);
        assert_eq!(client.relays()[0], "wss://relay1.example");
    }
}
