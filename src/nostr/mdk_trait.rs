use async_trait::async_trait;
use crate::error::Result;

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
}
