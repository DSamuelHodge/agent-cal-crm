//! Pluggable persistence for agentcal.
//!
//! Unlike the Python version, [`BookingLink`]s are persisted too — the store
//! is the single source of truth for both calendars and links, which is what
//! makes concurrent multi-agent access safe.

pub(crate) mod libsql_store;
mod memory;
mod null;

pub use libsql_store::LibSqlStore;
pub use memory::MemoryStore;
pub use null::NullStore;

pub(crate) use libsql_store::{get_dt, get_int, get_json, get_opt_int, get_real, get_text};

use async_trait::async_trait;

use crate::actions::ActionLogStore;
use crate::error::Result;
use crate::types::{BookingLink, Calendar};

/// Minimal interface every store must implement.
#[async_trait]
pub trait CalendarStore: ActionLogStore {
    // ── Calendars ───────────────────────────────────────────────────────────
    async fn save(&self, calendar: &Calendar) -> Result<()>;
    async fn load(&self, owner_id: &str) -> Result<Option<Calendar>>;
    async fn delete(&self, owner_id: &str) -> Result<bool>;
    async fn list_ids(&self) -> Result<Vec<String>>;

    // ── Booking links ───────────────────────────────────────────────────────
    async fn save_link(&self, link: &BookingLink) -> Result<()>;
    async fn load_link(&self, link_id: &str) -> Result<Option<BookingLink>>;
    async fn load_links(&self, owner_id: &str) -> Result<Vec<BookingLink>>;
    async fn delete_link(&self, link_id: &str) -> Result<bool>;
}
