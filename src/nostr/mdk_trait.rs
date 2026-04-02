use async_trait::async_trait;
use crate::error::Result;

/// A decrypted incoming message from a customer.
#[derive(Debug, Clone)]
pub struct DecryptedMessage {
    /// The decrypted content (JSON order payload).
    pub content: String,
    /// The sender's public key (hex-encoded).
    pub sender_pubkey: String,
}

/// Trait abstracting the MDK API surface.
/// When mdk-core becomes available, implement this trait with real MDK calls.
#[async_trait]
pub trait MdkClient: Send + Sync {
    /// Create a 1:1 MLS group for a checkout session.
    async fn create_group(&self, customer_pubkey: &str) -> Result<String>;

    /// Send an encrypted message to a group.
    async fn send_message(&self, group_id: &str, payload: &str) -> Result<()>;

    /// Generate a new key package for MDK.
    async fn create_key_package(&self) -> Result<()>;

    /// Mark a group as inactive.
    async fn deactivate_group(&self, group_id: &str) -> Result<()>;

    /// Purge groups older than the given number of days.
    async fn purge_stale_groups(&self, max_age_days: u64) -> Result<()>;

    /// Process a raw incoming Nostr event (Kind:445 MLS message or Kind:1059 gift-wrapped welcome).
    /// Returns `Some(DecryptedMessage)` if the event contained an application message,
    /// or `None` for commits, proposals, and welcome processing.
    async fn process_incoming_event(&self, event_json: &[u8]) -> Result<Option<DecryptedMessage>>;
}
