use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::Result;
use super::mdk_trait::MdkClient;

/// Mock MDK client for development and testing.
/// Stores groups and messages in memory.
pub struct MockMdkClient {
    groups: Mutex<HashMap<String, bool>>, // group_id → active
    messages: Mutex<Vec<(String, String)>>, // (group_id, payload)
}

impl MockMdkClient {
    pub fn new() -> Self {
        Self {
            groups: Mutex::new(HashMap::new()),
            messages: Mutex::new(Vec::new()),
        }
    }

    /// Inspect sent messages (for testing).
    pub fn sent_messages(&self) -> Vec<(String, String)> {
        self.messages.lock().unwrap().clone()
    }

    /// Inspect active groups (for testing).
    pub fn active_groups(&self) -> Vec<String> {
        self.groups
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, active)| **active)
            .map(|(id, _)| id.clone())
            .collect()
    }
}

#[async_trait]
impl MdkClient for MockMdkClient {
    async fn create_group(&self, _customer_pubkey: &str) -> Result<String> {
        let group_id = uuid::Uuid::new_v4().to_string();
        self.groups.lock().unwrap().insert(group_id.clone(), true);
        Ok(group_id)
    }

    async fn send_message(&self, group_id: &str, payload: &str) -> Result<()> {
        self.messages
            .lock()
            .unwrap()
            .push((group_id.to_string(), payload.to_string()));
        Ok(())
    }

    async fn create_key_package(&self) -> Result<()> {
        Ok(())
    }

    async fn deactivate_group(&self, group_id: &str) -> Result<()> {
        if let Some(active) = self.groups.lock().unwrap().get_mut(group_id) {
            *active = false;
        }
        Ok(())
    }

    async fn purge_stale_groups(&self, _max_age_days: u64) -> Result<()> {
        // Mock: remove all inactive groups
        self.groups.lock().unwrap().retain(|_, active| *active);
        Ok(())
    }
}
