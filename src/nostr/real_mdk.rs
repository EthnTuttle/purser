use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;

use mdk_core::prelude::*;
use mdk_memory_storage::MdkMemoryStorage;
use nostr::event::builder::EventBuilder;
use nostr::{Event, Keys, Kind, PublicKey, RelayUrl};
use nostr_sdk::Client;

use super::mdk_trait::{DecryptedMessage, MdkClient};
use crate::error::{PurserError, Result};

/// Real MDK client backed by mdk-core for MLS encryption and nostr-sdk for relay I/O.
pub struct RealMdkClient {
    mdk: MDK<MdkMemoryStorage>,
    merchant_keys: Keys,
    nostr_client: Client,
    relay_urls: Vec<RelayUrl>,
    /// Maps hex-encoded nostr_group_id → MLS GroupId
    groups: Mutex<HashMap<String, GroupId>>,
}

impl RealMdkClient {
    /// Create a new RealMdkClient, connect to relays, and publish an initial key package.
    pub async fn new(merchant_keys: Keys, relays: &[String]) -> Result<Self> {
        let mdk = MDK::new(MdkMemoryStorage::default());

        // Parse relay URLs
        let relay_urls: Vec<RelayUrl> = relays
            .iter()
            .filter_map(|r| RelayUrl::parse(r).ok())
            .collect();

        if relay_urls.is_empty() {
            return Err(PurserError::Mdk("no valid relay URLs provided".into()));
        }

        // Create nostr-sdk client with merchant keys for signing
        let nostr_client = Client::new(merchant_keys.clone());
        for url in &relay_urls {
            nostr_client
                .add_relay(url.clone())
                .await
                .map_err(|e| PurserError::Nostr(format!("add relay: {e}")))?;
        }
        nostr_client.connect().await;

        let client = Self {
            mdk,
            merchant_keys,
            nostr_client,
            relay_urls,
            groups: Mutex::new(HashMap::new()),
        };

        // Publish initial key package so customers can find us
        client.create_key_package().await?;

        Ok(client)
    }

    /// Get a reference to the underlying nostr-sdk client (for relay subscriptions).
    pub fn sdk_client(&self) -> &Client {
        &self.nostr_client
    }

    /// Fetch the latest key package event (Kind:443) for a given pubkey from relays.
    async fn fetch_key_package(&self, pubkey: &PublicKey) -> Result<Event> {
        use nostr::Filter;
        use std::time::Duration;

        let filter = Filter::new()
            .author(*pubkey)
            .kind(Kind::MlsKeyPackage)
            .limit(1);

        let events = self
            .nostr_client
            .fetch_events(filter, Duration::from_secs(10))
            .await
            .map_err(|e| PurserError::Nostr(format!("fetch key packages: {e}")))?;

        events
            .into_iter()
            .next()
            .ok_or_else(|| PurserError::Mdk(format!("no key package found for {pubkey}")))
    }
}

#[async_trait]
impl MdkClient for RealMdkClient {
    async fn create_group(&self, customer_pubkey: &str) -> Result<String> {
        // Parse the customer's public key
        let customer_pk = PublicKey::parse(customer_pubkey)
            .map_err(|e| PurserError::Mdk(format!("invalid customer pubkey: {e}")))?;

        // Fetch customer's key package from relays
        let kp_event = self.fetch_key_package(&customer_pk).await?;

        // Create MLS group config for this checkout session
        let config = NostrGroupConfigData::new(
            format!("checkout-{}", uuid::Uuid::new_v4()),
            "Purser checkout session".to_owned(),
            None, // image_hash
            None, // image_key
            None, // image_nonce
            self.relay_urls.clone(),
            vec![self.merchant_keys.public_key(), customer_pk],
        );

        // Create the MLS group with the customer
        let result = self
            .mdk
            .create_group(
                &self.merchant_keys.public_key(),
                vec![kp_event],
                config,
            )
            .map_err(|e| PurserError::Mdk(format!("create group: {e}")))?;

        let group = result.group;
        let mls_group_id = GroupId::from_slice(group.mls_group_id.as_slice());
        let nostr_group_id = hex::encode(&group.nostr_group_id);

        // Merge the pending commit to finalize the group locally
        self.mdk
            .merge_pending_commit(&mls_group_id)
            .map_err(|e| PurserError::Mdk(format!("merge pending commit: {e}")))?;

        // Gift-wrap and publish welcome messages to the customer
        for welcome_rumor in &result.welcome_rumors {
            let gift_wrapped = EventBuilder::gift_wrap(
                &self.merchant_keys,
                &customer_pk,
                welcome_rumor.clone(),
                [],
            )
            .await
            .map_err(|e| PurserError::Nostr(format!("gift wrap welcome: {e}")))?;

            self.nostr_client
                .send_event(&gift_wrapped)
                .await
                .map_err(|e| PurserError::Nostr(format!("publish welcome: {e}")))?;
        }

        // Track the group mapping
        self.groups
            .lock()
            .unwrap()
            .insert(nostr_group_id.clone(), mls_group_id);

        tracing::info!(
            nostr_group_id = %nostr_group_id,
            customer = %customer_pubkey,
            "created MLS checkout group"
        );

        Ok(nostr_group_id)
    }

    async fn send_message(&self, group_id: &str, payload: &str) -> Result<()> {
        let mls_group_id = self
            .groups
            .lock()
            .unwrap()
            .get(group_id)
            .cloned()
            .ok_or_else(|| PurserError::Mdk(format!("unknown group: {group_id}")))?;

        // Build a Kind:9 rumor with the payload as content
        let rumor = EventBuilder::new(Kind::Custom(9), payload)
            .build(self.merchant_keys.public_key());

        // Encrypt via MLS and get a Kind:445 event
        let message_event = self
            .mdk
            .create_message(&mls_group_id, rumor)
            .map_err(|e| PurserError::Mdk(format!("create message: {e}")))?;

        // Publish to relays
        self.nostr_client
            .send_event(&message_event)
            .await
            .map_err(|e| PurserError::Nostr(format!("publish message: {e}")))?;

        tracing::debug!(group_id = %group_id, "sent encrypted message to relay");

        Ok(())
    }

    async fn create_key_package(&self) -> Result<()> {
        let (kp_encoded, tags, _) = self
            .mdk
            .create_key_package_for_event(
                &self.merchant_keys.public_key(),
                self.relay_urls.clone(),
            )
            .map_err(|e| PurserError::Mdk(format!("create key package: {e}")))?;

        // Build and sign the Kind:443 key package event
        let kp_event = EventBuilder::new(Kind::MlsKeyPackage, kp_encoded)
            .tags(tags)
            .sign(&self.merchant_keys)
            .await
            .map_err(|e| PurserError::Nostr(format!("sign key package: {e}")))?;

        // Publish to relays
        self.nostr_client
            .send_event(&kp_event)
            .await
            .map_err(|e| PurserError::Nostr(format!("publish key package: {e}")))?;

        tracing::info!("published key package to relays");
        Ok(())
    }

    async fn deactivate_group(&self, group_id: &str) -> Result<()> {
        self.groups.lock().unwrap().remove(group_id);
        tracing::info!(group_id = %group_id, "deactivated group");
        Ok(())
    }

    async fn purge_stale_groups(&self, _max_age_days: u64) -> Result<()> {
        // Get all groups from MDK storage and remove old ones
        // For now, we just clear inactive groups from our tracking map.
        // Full time-based purge requires MDK group metadata with timestamps,
        // which can be added when MDK exposes group creation timestamps.
        tracing::debug!("purge_stale_groups called (no-op with memory storage)");
        Ok(())
    }

    async fn process_incoming_event(&self, event_json: &[u8]) -> Result<Option<DecryptedMessage>> {
        let event: Event = serde_json::from_slice(event_json)
            .map_err(|e| PurserError::Nostr(format!("deserialize event: {e}")))?;

        match event.kind {
            // Gift-wrapped welcome (NIP-59 Kind:1059)
            Kind::GiftWrap => {
                let unwrapped = nostr::nips::nip59::extract_rumor(&self.merchant_keys, &event)
                    .await
                    .map_err(|e| PurserError::Mdk(format!("unwrap gift wrap: {e}")))?;

                // Process welcome through MDK
                self.mdk
                    .process_welcome(&event.id, &unwrapped.rumor)
                    .map_err(|e| PurserError::Mdk(format!("process welcome: {e}")))?;

                // Auto-accept all welcomes (checkout groups are always accepted)
                let welcomes = self
                    .mdk
                    .get_pending_welcomes(None)
                    .map_err(|e| PurserError::Mdk(format!("get pending welcomes: {e}")))?;

                for welcome in &welcomes {
                    self.mdk
                        .accept_welcome(welcome)
                        .map_err(|e| PurserError::Mdk(format!("accept welcome: {e}")))?;
                    tracing::info!(group_name = %welcome.group_name, "accepted MLS welcome");
                }

                Ok(None)
            }

            // MLS encrypted message (Kind:445)
            Kind::MlsGroupMessage => {
                let result = self
                    .mdk
                    .process_message(&event)
                    .map_err(|e| PurserError::Mdk(format!("process message: {e}")))?;

                match result {
                    MessageProcessingResult::ApplicationMessage(msg) => {
                        let sender = msg.pubkey.to_string();
                        tracing::debug!(sender = %sender, "decrypted application message");
                        Ok(Some(DecryptedMessage {
                            content: msg.content,
                            sender_pubkey: sender,
                        }))
                    }
                    _ => {
                        tracing::debug!(kind = ?result, "non-application MLS message (commit/proposal)");
                        Ok(None)
                    }
                }
            }

            other => {
                tracing::warn!(kind = %other, "unexpected event kind in incoming event");
                Ok(None)
            }
        }
    }
}
