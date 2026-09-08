//! CRM persistence trait — the store is the single source of truth for
//! contacts, companies, deals and interactions (parallel to `CalendarStore`).

use async_trait::async_trait;

use chrono::{DateTime, Utc};

use crate::crm::types::{
    Company, Contact, CrmSummary, Deal, Interaction, InteractionDirection, InteractionKind,
};
use crate::error::Result;
use crate::inbox::InboxRecord;

/// Minimal interface every CRM store must implement.
#[async_trait]
pub trait CrmStore: Send + Sync {
    // ── Companies ──────────────────────────────────────────────────────────
    async fn save_company(&self, company: &Company) -> Result<()>;
    async fn load_company(&self, owner_id: &str, company_id: &str) -> Result<Option<Company>>;
    async fn delete_company(&self, owner_id: &str, company_id: &str) -> Result<bool>;
    async fn list_companies(&self, owner_id: &str) -> Result<Vec<Company>>;

    // ── Contacts ───────────────────────────────────────────────────────────
    async fn save_contact(&self, contact: &Contact) -> Result<()>;
    async fn load_contact(&self, owner_id: &str, contact_id: &str) -> Result<Option<Contact>>;
    async fn delete_contact(&self, owner_id: &str, contact_id: &str) -> Result<bool>;
    async fn list_contacts(&self, owner_id: &str) -> Result<Vec<Contact>>;

    // ── Deals ──────────────────────────────────────────────────────────────
    async fn save_deal(&self, deal: &Deal) -> Result<()>;
    async fn load_deal(&self, owner_id: &str, deal_id: &str) -> Result<Option<Deal>>;
    async fn delete_deal(&self, owner_id: &str, deal_id: &str) -> Result<bool>;
    async fn list_deals(&self, owner_id: &str) -> Result<Vec<Deal>>;
    async fn list_deals_for_company(&self, owner_id: &str, company_id: &str) -> Result<Vec<Deal>>;

    // ── Interactions ───────────────────────────────────────────────────────
    async fn save_interaction(&self, interaction: &Interaction) -> Result<()>;
    async fn list_interactions_for_contact(
        &self,
        owner_id: &str,
        contact_id: &str,
    ) -> Result<Vec<Interaction>>;

    // ── Cross-entity lookups ───────────────────────────────────────────────
    /// Resolve a contact by exact phone number (SMS-triage hook).
    async fn resolve_by_phone(&self, owner_id: &str, phone: &str) -> Result<Option<Contact>>;
    /// Resolve a contact by exact email address.
    async fn resolve_by_email(&self, owner_id: &str, email: &str) -> Result<Option<Contact>>;
    /// List contacts belonging to a company.
    async fn contacts_for_company(&self, owner_id: &str, company_id: &str) -> Result<Vec<Contact>>;

    // ── Inbox ──────────────────────────────────────────────────────────────
    /// Insert an inbox record, ignoring the write when an event with the same
    /// `(owner_id, channel, external_id)` is already filed. Returns `true`
    /// when the row was inserted, `false` on a dedup conflict. Records are
    /// immutable after insert — there is intentionally no update path.
    async fn insert_inbox_event(&self, record: &InboxRecord) -> Result<bool>;
    /// Load one inbox record by its dedup key.
    async fn load_inbox_event(
        &self,
        owner_id: &str,
        channel: &str,
        external_id: &str,
    ) -> Result<Option<InboxRecord>>;
    /// List an owner's inbox records, newest first, optionally filtered by
    /// channel, capped at `limit`.
    async fn list_inbox_events(
        &self,
        owner_id: &str,
        channel: Option<&str>,
        limit: usize,
    ) -> Result<Vec<InboxRecord>>;

    // ── Search ─────────────────────────────────────────────────────────────
    /// Full-text search over contacts + companies + deals (FTS5 where
    /// available, indexed LIKE fallback).
    async fn search_crm(&self, owner_id: &str, query: &str, limit: usize)
        -> Result<Vec<SearchHit>>;
    /// Vector similarity search over contacts (embedding → nearest neighbours).
    async fn vector_search(
        &self,
        owner_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>>;

    // ── Summary ────────────────────────────────────────────────────────────
    async fn crm_summary(&self, owner_id: &str) -> Result<CrmSummary>;
}

/// A single result from `search_crm`, tagged with its entity type.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SearchHit {
    pub entity: String, // "contact" | "company" | "deal"
    pub id: String,
    pub label: String,
    pub snippet: String,
}

impl SearchHit {
    pub fn contact(c: &Contact) -> Self {
        Self {
            entity: "contact".into(),
            id: c.id.clone(),
            label: c.display_name(),
            snippet: format!(
                "{} · {} · {}",
                c.title,
                c.company_id.as_deref().unwrap_or(""),
                c.notes
            ),
        }
    }

    pub fn company(c: &Company) -> Self {
        Self {
            entity: "company".into(),
            id: c.id.clone(),
            label: c.name.clone(),
            snippet: format!("{} · {:.2} · {}", c.industry, c.deal_value, c.notes),
        }
    }

    pub fn deal(d: &Deal) -> Self {
        Self {
            entity: "deal".into(),
            id: d.id.clone(),
            label: d.name.clone(),
            snippet: format!("{:?} · {:.2} · {}", d.stage, d.amount, d.next_action),
        }
    }
}

/// Re-export the interaction enums used by the façade for convenience.
pub use crate::crm::types::{InteractionDirection as CrmDirection, InteractionKind as CrmKind};

/// Convenience: an interaction descriptor for `log_interaction`.
#[derive(Debug, Clone)]
pub struct InteractionInput {
    pub contact_id: String,
    pub deal_id: Option<String>,
    pub kind: InteractionKind,
    pub direction: InteractionDirection,
    pub summary: String,
    /// Override the interaction timestamp (defaults to now when `None`).
    /// Used by the inbox façade to file the event's own time, not the
    /// filing time.
    pub at: Option<DateTime<Utc>>,
    /// Stored verbatim on the interaction (defaults to null). The inbox
    /// façade stamps `inbox_channel` / `inbox_external_id` here so a filed
    /// interaction traces back to its source event.
    pub metadata: serde_json::Value,
}

impl InteractionInput {
    pub fn new(contact_id: impl Into<String>, kind: InteractionKind) -> Self {
        Self {
            contact_id: contact_id.into(),
            deal_id: None,
            kind,
            direction: InteractionDirection::Outbound,
            summary: String::new(),
            at: None,
            metadata: serde_json::Value::Null,
        }
    }

    pub fn with_deal(mut self, deal_id: impl Into<String>) -> Self {
        self.deal_id = Some(deal_id.into());
        self
    }

    pub fn with_direction(mut self, direction: InteractionDirection) -> Self {
        self.direction = direction;
        self
    }

    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }

    pub fn with_at(mut self, at: DateTime<Utc>) -> Self {
        self.at = Some(at);
        self
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }
}
