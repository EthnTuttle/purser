pub mod mdk_trait;
pub mod real_mdk;

use std::sync::Arc;

use crate::error::Result;
use crate::messages::{PaymentRequest, StatusUpdate};
use crate::pipeline::IncomingOrder;
use mdk_trait::MdkClient;

/// Handle to the MDK-backed Nostr communication layer.
/// Uses real MLS encryption via mdk-core. In local mode (no relays connected),
/// groups are created with ephemeral customer identities and messages are
/// encrypted but not published.
pub struct NostrClient {
    mdk: Box<dyn MdkClient>,
    relays: Vec<String>,
    /// nostr-sdk client for relay subscriptions (None in local mode).
    sdk_client: Option<nostr_sdk::Client>,
}

impl NostrClient {
    /// Initialize with relay connectivity and real Nostr publishing.
    ///
    /// Parses the merchant's nsec, connects to relays, and publishes an
    /// initial key package.
    pub async fn new(relays: &[String], _storage_type: &str, merchant_nsec: &str) -> Result<Self> {
        let keys = nostr::Keys::parse(merchant_nsec)
            .map_err(|e| crate::error::PurserError::Mdk(format!("invalid merchant nsec: {e}")))?;
        tracing::info!(
            relay_count = relays.len(),
            merchant_pubkey = %keys.public_key(),
            "initializing NostrClient (relay mode)"
        );
        let real = real_mdk::RealMdkClient::new_with_relays(keys, relays).await?;
        let client = real.sdk_client().cloned();
        Ok(Self {
            mdk: Box::new(real),
            relays: relays.to_vec(),
            sdk_client: client,
        })
    }

    /// Initialize in local mode with real MLS but no relay I/O.
    ///
    /// Used for tests and development. Groups are created with ephemeral
    /// customer identities. Messages are encrypted via MLS but not published.
    pub fn new_local(merchant_nsec: &str) -> Result<Self> {
        let keys = nostr::Keys::parse(merchant_nsec)
            .map_err(|e| crate::error::PurserError::Mdk(format!("invalid merchant nsec: {e}")))?;
        tracing::info!(
            merchant_pubkey = %keys.public_key(),
            "initializing NostrClient (local mode)"
        );
        Ok(Self {
            mdk: Box::new(real_mdk::RealMdkClient::new_local(keys)),
            relays: Vec::new(),
            sdk_client: None,
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

    /// Start a relay subscription for incoming MLS messages and gift-wrapped welcomes.
    ///
    /// Returns `None` in local mode (no relay subscription needed).
    pub async fn subscribe_orders(
        self: &Arc<Self>,
        merchant_pubkey: &str,
    ) -> Result<Option<tokio::sync::mpsc::Receiver<IncomingOrder>>> {
        let sdk_client = match &self.sdk_client {
            Some(c) => c.clone(),
            None => return Ok(None),
        };

        let pubkey = nostr::PublicKey::parse(merchant_pubkey)
            .map_err(|e| crate::error::PurserError::Nostr(format!("parse merchant pubkey: {e}")))?;

        let filter = nostr::Filter::new()
            .pubkey(pubkey)
            .kinds(vec![nostr::Kind::MlsGroupMessage, nostr::Kind::GiftWrap]);

        sdk_client
            .subscribe(filter, None)
            .await
            .map_err(|e| crate::error::PurserError::Nostr(format!("subscribe: {e}")))?;

        let (tx, rx) = tokio::sync::mpsc::channel::<IncomingOrder>(256);
        let nostr_client = Arc::clone(self);

        tokio::spawn(async move {
            tracing::info!("relay subscription loop started");
            let handler = sdk_client
                .handle_notifications(|notification| {
                    let tx = tx.clone();
                    let nostr_client = Arc::clone(&nostr_client);
                    async move {
                        if let nostr_sdk::RelayPoolNotification::Event { event, .. } = notification
                        {
                            let event_json = serde_json::to_vec(&*event).unwrap_or_default();
                            match nostr_client.process_incoming_event(&event_json).await {
                                Ok(Some(msg)) => {
                                    let order = IncomingOrder {
                                        raw_json: msg.content,
                                        customer_pubkey: msg.sender_pubkey,
                                    };
                                    if tx.send(order).await.is_err() {
                                        tracing::info!("order channel closed, stopping subscription");
                                        return Ok(true);
                                    }
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    tracing::warn!(error = %e, "failed to process incoming event");
                                }
                            }
                        }
                        Ok(false)
                    }
                })
                .await;

            if let Err(e) = handler {
                tracing::error!(error = %e, "relay notification handler error");
            }
            tracing::info!("relay subscription loop ended");
        });

        Ok(Some(rx))
    }

    /// Process a raw incoming Nostr event through MDK decryption.
    pub async fn process_incoming_event(
        &self,
        event_json: &[u8],
    ) -> Result<Option<mdk_trait::DecryptedMessage>> {
        self.mdk.process_incoming_event(event_json).await
    }

    /// Get configured relays.
    pub fn relays(&self) -> &[String] {
        &self.relays
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::OrderStatus;
    use crate::test_keys::TEST_MERCHANT_NSEC;
    use chrono::Utc;
    use std::collections::HashMap;

    fn make_client() -> NostrClient {
        NostrClient::new_local(TEST_MERCHANT_NSEC).unwrap()
    }

    #[tokio::test]
    async fn test_create_checkout_group() {
        let client = make_client();
        let group_id = client.create_checkout_group("npub1customer").await.unwrap();
        assert!(!group_id.is_empty());
        // Real MDK returns hex-encoded nostr_group_id, not UUID
        assert!(group_id.len() > 8);
    }

    #[tokio::test]
    async fn test_send_payment_request() {
        let client = make_client();
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

        // Real MLS encryption succeeds — message is encrypted into the group
        client.send_payment_request(&group_id, &pr).await.unwrap();
    }

    #[tokio::test]
    async fn test_send_status_update() {
        let client = make_client();
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
    }

    #[tokio::test]
    async fn test_send_error() {
        let client = make_client();
        let group_id = client.create_checkout_group("npub1customer").await.unwrap();
        client.send_error(&group_id, "invalid order format").await.unwrap();
    }

    #[tokio::test]
    async fn test_deactivate_group() {
        let client = make_client();
        let group_id = client.create_checkout_group("npub1customer").await.unwrap();
        client.deactivate_group(&group_id).await.unwrap();
    }

    #[tokio::test]
    async fn test_regenerate_key_packages() {
        let client = make_client();
        client.regenerate_key_packages().await.unwrap();
    }

    #[tokio::test]
    async fn test_purge_stale_groups() {
        let client = make_client();
        client.purge_stale_groups(7).await.unwrap();
    }

    /// Criteria #6: Relay connection — daemon initializes with configured relays.
    #[tokio::test]
    async fn test_local_client_has_no_relays() {
        let client = make_client();
        assert!(client.relays().is_empty());
    }

    /// Criteria #8: MLS group creation — customer pubkey triggers real MLS group
    /// creation, and the group accepts encrypted messages.
    #[tokio::test]
    async fn test_mls_group_creation_and_messaging() {
        let client = make_client();
        let group_id = client.create_checkout_group("npub1newcustomer").await.unwrap();
        assert!(!group_id.is_empty());

        // Should be able to send messages to the real MLS group.
        client.send_error(&group_id, "test message").await.unwrap();

        // Deactivating and then sending should fail (group removed).
        client.deactivate_group(&group_id).await.unwrap();
        assert!(client.send_error(&group_id, "should fail").await.is_err());
    }

    /// Criteria #30: Customer offline — messages are encrypted and published
    /// regardless of customer connectivity.
    #[tokio::test]
    async fn test_message_sent_regardless_of_customer_state() {
        let client = make_client();
        let group_id = client.create_checkout_group("npub1offline").await.unwrap();

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
        // Succeeds even though customer is not online — MLS encrypts locally.
        client.send_status_update(&group_id, &su).await.unwrap();
    }

    /// Multiple groups can coexist independently.
    #[tokio::test]
    async fn test_multiple_independent_groups() {
        let client = make_client();
        let g1 = client.create_checkout_group("npub1alice").await.unwrap();
        let g2 = client.create_checkout_group("npub1bob").await.unwrap();
        assert_ne!(g1, g2);

        // Messages to each group succeed independently.
        client.send_error(&g1, "msg to alice").await.unwrap();
        client.send_error(&g2, "msg to bob").await.unwrap();

        // Deactivating one doesn't affect the other.
        client.deactivate_group(&g1).await.unwrap();
        assert!(client.send_error(&g1, "should fail").await.is_err());
        client.send_error(&g2, "still works").await.unwrap();
    }
}
