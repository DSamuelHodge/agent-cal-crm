//! agentcal — a calendar engine built for agents.
//!
//! No GUI, no HTTP, no background threads. libSQL-backed persistence with an
//! in-memory option. Every public call returns `Result<T, AgentError>` so
//! agents branch on the `Result` instead of `{"ok": bool}` dicts.
//!
//! # Quick start
//! ```no_run
//! use agentcal::{AgentCal, MemoryStore, LinkParams, Attendee};
//!
//! #[tokio::main]
//! async fn main() -> agentcal::Result<()> {
//!     let cal = AgentCal::new(MemoryStore::new());
//!     cal.create_calendar_simple("alice", "Alice's Calendar").await?;
//!     for dow in 0..5u8 {
//!         cal.add_window("alice", Some(dow), "09:00", "17:00", "").await?;
//!     }
//!     let link = cal.create_link("alice", LinkParams::new("30-min sync", 30)).await?;
//!     let slots = cal.get_slots("alice", &link.id, None, None, Some(5)).await?;
//!     let _booked = cal
//!         .book("alice", &link.id, slots[0].clone(),
//!               vec![Attendee::new("Bob", "bob@example.com")], "Kick-off", serde_json::Value::Null)
//!         .await?;
//!     Ok(())
//! }
//! ```

pub mod agent_api;
pub mod availability;
pub mod crm;
pub mod error;
pub mod scheduler;
pub mod store;
pub mod types;

pub use agent_api::{AgentCal, LinkParams};
pub use availability::CheckResult;
pub use crm::{
    AgentCrm, Company, Contact, CrmSummary, Deal, DealStage, Interaction, InteractionDirection,
    InteractionInput, InteractionKind,
};
pub use error::{AgentError, Result, StoreError};
pub use scheduler::{Booked, Rescheduled, Summary};
pub use store::{CalendarStore, LibSqlStore, MemoryStore, NullStore};
pub use types::{
    Attendee, AvailabilityWindow, Booking, BookingLink, Calendar, ConflictPolicy, RecurrenceRule,
    Status, TimeSlot,
};

/// Re-export the CRM store trait for store implementors.
pub use crm::store::{CrmStore, SearchHit};

use chrono::{DateTime, Utc};

/// Parse an ISO-8601 / RFC-3339 timestamp into `DateTime<Utc>`.
pub fn parse_iso(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .or_else(|_| DateTime::parse_from_rfc3339(&format!("{s}Z")).map(|d| d.with_timezone(&Utc)))
        .map_err(|e| AgentError::Validation(format!("invalid datetime {s:?}: {e}")))
}
