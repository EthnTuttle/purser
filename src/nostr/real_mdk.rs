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

/// MDK client backed by mdk-core for real MLS encryption.
///
/// Operates in two modes:
/// - **Relay mode**: connected to Nostr relays for publishing and fetching events.
/// - **Local mode**: MLS encryption works in-memory without relay I/O.
///   Used in tests and when no relays are configured. In local mode, `create_group`
///   generates an ephemeral customer identity so MLS groups are still real.
pub struct RealMdkClient {
    mdk: MDK<MdkMemoryStorage>,
    merchant_keys: Keys,
    /// None in local/test mode — relay operations are skipped.
    nostr_client: Option<Client>,
    relay_urls: Vec<RelayUrl>,
    /// Maps hex-encoded nostr_group_id → MLS GroupId
    groups: Mutex<HashMap<String, GroupId>>,
}

impl RealMdkClient {
    /// Create a relay-connected client. Connects to relays and publishes an initial key package.
    pub async fn new_with_relays(merchant_keys: Keys, relays: &[String]) -> Result<Self> {
        let mdk = MDK::new(MdkMemoryStorage::default());

        let relay_urls: Vec<RelayUrl> = relays
            .iter()
            .filter_map(|r| RelayUrl::parse(r).ok())
            .collect();

        if relay_urls.is_empty() {
            return Err(PurserError::Mdk("no valid relay URLs provided".into()));
        }

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
            nostr_client: Some(nostr_client),
            relay_urls,
            groups: Mutex::new(HashMap::new()),
        };

        client.create_key_package().await?;
        Ok(client)
    }

    /// Create a local-only client for testing. No relay connection — MLS operations
    /// work in-memory with real encryption but no network I/O.
    pub fn new_local(merchant_keys: Keys) -> Self {
        // MDK requires at least one relay URL in group config.
        // Use a placeholder that is never connected to.
        let placeholder_relay = RelayUrl::parse("wss://localhost:0").expect("valid relay URL");
        Self {
            mdk: MDK::new(MdkMemoryStorage::default()),
            merchant_keys,
            nostr_client: None,
            relay_urls: vec![placeholder_relay],
            groups: Mutex::new(HashMap::new()),
        }
    }

    /// Get the underlying nostr-sdk client (for relay subscriptions). None in local mode.
    pub fn sdk_client(&self) -> Option<&Client> {
        self.nostr_client.as_ref()
    }

    /// Publish an event to relays if connected. No-op in local mode.
    async fn publish(&self, event: &Event) -> Result<()> {
        if let Some(client) = &self.nostr_client {
            client
                .send_event(event)
                .await
                .map_err(|e| PurserError::Nostr(format!("publish: {e}")))?;
        }
        Ok(())
    }

    /// Fetch the latest key package event (Kind:443) for a given pubkey from relays.
    async fn fetch_key_package(&self, pubkey: &PublicKey) -> Result<Event> {
        let client = self
            .nostr_client
            .as_ref()
            .ok_or_else(|| PurserError::Mdk("cannot fetch key packages in local mode".into()))?;

        use nostr::Filter;
        use std::time::Duration;

        let filter = Filter::new()
            .author(*pubkey)
            .kind(Kind::MlsKeyPackage)
            .limit(1);

        let events = client
            .fetch_events(filter, Duration::from_secs(10))
            .await
            .map_err(|e| PurserError::Nostr(format!("fetch key packages: {e}")))?;

        events
            .into_iter()
            .next()
            .ok_or_else(|| PurserError::Mdk(format!("no key package found for {pubkey}")))
    }

    /// Generate a local customer identity and key package event (for local/test mode).
    fn generate_local_customer_key_package(&self) -> Result<(Keys, Event)> {
        let customer_keys = Keys::generate();
        let customer_mdk = MDK::new(MdkMemoryStorage::default());

        let (kp_encoded, tags, _) = customer_mdk
            .create_key_package_for_event(&customer_keys.public_key(), self.relay_urls.clone())
            .map_err(|e| PurserError::Mdk(format!("create customer key package: {e}")))?;

        let kp_event = EventBuilder::new(Kind::MlsKeyPackage, kp_encoded)
            .tags(tags)
            .sign_with_keys(&customer_keys)
            .map_err(|e| PurserError::Nostr(format!("sign customer key package: {e}")))?;

        Ok((customer_keys, kp_event))
    }
}

#[async_trait]
impl MdkClient for RealMdkClient {
    async fn create_group(&self, customer_pubkey: &str) -> Result<String> {
        // Get the customer's key package — from relay or generated locally
        let (customer_pk, kp_event) = if self.nostr_client.is_some() {
            let pk = PublicKey::parse(customer_pubkey)
                .map_err(|e| PurserError::Mdk(format!("invalid customer pubkey: {e}")))?;
            let kp = self.fetch_key_package(&pk).await?;
            (pk, kp)
        } else {
            // Local mode: generate ephemeral customer identity
            let (customer_keys, kp_event) = self.generate_local_customer_key_package()?;
            (customer_keys.public_key(), kp_event)
        };

        let config = NostrGroupConfigData::new(
            format!("checkout-{}", uuid::Uuid::new_v4()),
            "Purser checkout session".to_owned(),
            None,
            None,
            None,
            self.relay_urls.clone(),
            vec![self.merchant_keys.public_key(), customer_pk],
        );

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

        self.mdk
            .merge_pending_commit(&mls_group_id)
            .map_err(|e| PurserError::Mdk(format!("merge pending commit: {e}")))?;

        // Gift-wrap and publish welcome messages (relay mode only)
        if self.nostr_client.is_some() {
            for welcome_rumor in &result.welcome_rumors {
                let gift_wrapped = EventBuilder::gift_wrap(
                    &self.merchant_keys,
                    &customer_pk,
                    welcome_rumor.clone(),
                    [],
                )
                .await
                .map_err(|e| PurserError::Nostr(format!("gift wrap welcome: {e}")))?;

                self.publish(&gift_wrapped).await?;
            }
        }

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

        let rumor = EventBuilder::new(Kind::Custom(9), payload)
            .build(self.merchant_keys.public_key());

        let message_event = self
            .mdk
            .create_message(&mls_group_id, rumor)
            .map_err(|e| PurserError::Mdk(format!("create message: {e}")))?;

        self.publish(&message_event).await?;
        tracing::debug!(group_id = %group_id, "sent encrypted message");
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

        let kp_event = EventBuilder::new(Kind::MlsKeyPackage, kp_encoded)
            .tags(tags)
            .sign(&self.merchant_keys)
            .await
            .map_err(|e| PurserError::Nostr(format!("sign key package: {e}")))?;

        self.publish(&kp_event).await?;
        tracing::info!("created key package");
        Ok(())
    }

    async fn deactivate_group(&self, group_id: &str) -> Result<()> {
        self.groups.lock().unwrap().remove(group_id);
        tracing::info!(group_id = %group_id, "deactivated group");
        Ok(())
    }

    async fn purge_stale_groups(&self, _max_age_days: u64) -> Result<()> {
        tracing::debug!("purge_stale_groups called (no-op with memory storage)");
        Ok(())
    }

    async fn process_incoming_event(&self, event_json: &[u8]) -> Result<Option<DecryptedMessage>> {
        let event: Event = serde_json::from_slice(event_json)
            .map_err(|e| PurserError::Nostr(format!("deserialize event: {e}")))?;

        match event.kind {
            Kind::GiftWrap => {
                let unwrapped = nostr::nips::nip59::extract_rumor(&self.merchant_keys, &event)
                    .await
                    .map_err(|e| PurserError::Mdk(format!("unwrap gift wrap: {e}")))?;

                self.mdk
                    .process_welcome(&event.id, &unwrapped.rumor)
                    .map_err(|e| PurserError::Mdk(format!("process welcome: {e}")))?;

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
                        tracing::debug!("non-application MLS message (commit/proposal)");
                        Ok(None)
                    }
                }
            }

            other => {
                tracing::warn!(kind = %other, "unexpected event kind");
                Ok(None)
            }
        }
    }
}
