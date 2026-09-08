//! No-op store — for agents that manage their own state externally.

use async_trait::async_trait;

use super::CalendarStore;
use crate::actions::{ActionLogEntry, ActionLogStore};
use crate::error::Result;
use crate::types::{BookingLink, Calendar};

/// No-op store: every read returns empty, every write is dropped.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullStore;

impl NullStore {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CalendarStore for NullStore {
    async fn save(&self, _calendar: &Calendar) -> Result<()> {
        Ok(())
    }

    async fn load(&self, _owner_id: &str) -> Result<Option<Calendar>> {
        Ok(None)
    }

    async fn delete(&self, _owner_id: &str) -> Result<bool> {
        Ok(false)
    }

    async fn list_ids(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn save_link(&self, _link: &BookingLink) -> Result<()> {
        Ok(())
    }

    async fn load_link(&self, _link_id: &str) -> Result<Option<BookingLink>> {
        Ok(None)
    }

    async fn load_links(&self, _owner_id: &str) -> Result<Vec<BookingLink>> {
        Ok(Vec::new())
    }

    async fn delete_link(&self, _link_id: &str) -> Result<bool> {
        Ok(false)
    }
}

#[async_trait]
impl ActionLogStore for NullStore {
    async fn append_action(&self, _entry: &ActionLogEntry) -> Result<()> {
        Ok(())
    }

    async fn list_actions(&self, _owner_id: &str, _limit: usize) -> Result<Vec<ActionLogEntry>> {
        Ok(Vec::new())
    }

    async fn query_actions(
        &self,
        _owner_id: &str,
        _method: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<ActionLogEntry>> {
        Ok(Vec::new())
    }
}
