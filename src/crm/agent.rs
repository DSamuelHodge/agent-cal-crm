//! [`AgentCrm`] — the single façade agents call for CRM operations.
//!
//! Mirrors `AgentCal`: one object, every method returns `Result<T, AgentError>`,
//! fully serialisable state, pluggable store. Because `AgentCal` and `AgentCrm`
//! share the same store, one libSQL file is the whole CoS operating picture.

use std::sync::Arc;

use chrono::Utc;

use crate::crm::store::{CrmStore, InteractionInput, SearchHit};
use crate::crm::types::{Company, Contact, CrmSummary, Deal, DealStage, Interaction};
use crate::error::{AgentError, Result};

/// CRM façade. Cheap to clone; multiple agents share the same store.
pub struct AgentCrm {
    store: Arc<dyn CrmStore>,
}

impl AgentCrm {
    /// Wrap any [`CrmStore`].
    pub fn new<S: CrmStore + 'static>(store: S) -> Self {
        Self {
            store: Arc::new(store),
        }
    }

    /// Build from an already-shared store handle.
    pub fn from_shared(store: Arc<dyn CrmStore>) -> Self {
        Self { store }
    }

    // ── Companies ──────────────────────────────────────────────────────────

    pub async fn create_company(
        &self,
        owner_id: &str,
        name: &str,
        industry: &str,
    ) -> Result<Company> {
        let company = Company::new(owner_id, name, industry);
        self.store.save_company(&company).await?;
        Ok(company)
    }

    pub async fn get_company(&self, owner_id: &str, company_id: &str) -> Result<Company> {
        self.store
            .load_company(owner_id, company_id)
            .await?
            .ok_or_else(|| AgentError::CompanyNotFound(company_id.to_string()))
    }

    pub async fn update_company(&self, company: &Company) -> Result<Company> {
        self.store.save_company(company).await?;
        Ok(company.clone())
    }

    pub async fn delete_company(&self, owner_id: &str, company_id: &str) -> Result<bool> {
        self.store.delete_company(owner_id, company_id).await
    }

    pub async fn list_companies(&self, owner_id: &str) -> Result<Vec<Company>> {
        self.store.list_companies(owner_id).await
    }

    // ── Contacts ───────────────────────────────────────────────────────────

    pub async fn create_contact(
        &self,
        owner_id: &str,
        first_name: &str,
        last_name: &str,
    ) -> Result<Contact> {
        let contact = Contact::new(owner_id, first_name, last_name);
        self.store.save_contact(&contact).await?;
        Ok(contact)
    }

    pub async fn get_contact(&self, owner_id: &str, contact_id: &str) -> Result<Contact> {
        self.store
            .load_contact(owner_id, contact_id)
            .await?
            .ok_or_else(|| AgentError::ContactNotFound(contact_id.to_string()))
    }

    pub async fn update_contact(&self, contact: &Contact) -> Result<Contact> {
        self.store.save_contact(contact).await?;
        Ok(contact.clone())
    }

    pub async fn delete_contact(&self, owner_id: &str, contact_id: &str) -> Result<bool> {
        self.store.delete_contact(owner_id, contact_id).await
    }

    pub async fn list_contacts(&self, owner_id: &str) -> Result<Vec<Contact>> {
        self.store.list_contacts(owner_id).await
    }

    /// The SMS-triage hook: resolve a phone number to a contact.
    pub async fn resolve_by_phone(&self, owner_id: &str, phone: &str) -> Result<Option<Contact>> {
        self.store.resolve_by_phone(owner_id, phone).await
    }

    pub async fn resolve_by_email(&self, owner_id: &str, email: &str) -> Result<Option<Contact>> {
        self.store.resolve_by_email(owner_id, email).await
    }

    pub async fn contacts_for_company(
        &self,
        owner_id: &str,
        company_id: &str,
    ) -> Result<Vec<Contact>> {
        self.store.contacts_for_company(owner_id, company_id).await
    }

    // ── Deals ──────────────────────────────────────────────────────────────

    pub async fn create_deal(
        &self,
        owner_id: &str,
        company_id: &str,
        name: &str,
        amount: f64,
    ) -> Result<Deal> {
        // The company must exist.
        self.get_company(owner_id, company_id).await?;
        let deal = Deal::new(owner_id, company_id, name, amount);
        self.store.save_deal(&deal).await?;
        Ok(deal)
    }

    pub async fn get_deal(&self, owner_id: &str, deal_id: &str) -> Result<Deal> {
        self.store
            .load_deal(owner_id, deal_id)
            .await?
            .ok_or_else(|| AgentError::DealNotFound(deal_id.to_string()))
    }

    pub async fn update_deal(&self, deal: &Deal) -> Result<Deal> {
        self.store.save_deal(deal).await?;
        Ok(deal.clone())
    }

    /// Advance a deal to `next_stage` (only if it's a valid forward move).
    pub async fn advance_deal(
        &self,
        owner_id: &str,
        deal_id: &str,
        next_stage: DealStage,
    ) -> Result<Deal> {
        let mut deal = self.get_deal(owner_id, deal_id).await?;
        if deal.stage == DealStage::ClosedWon || deal.stage == DealStage::ClosedLost {
            return Err(AgentError::CrmValidation(format!(
                "deal already closed ({:?})",
                deal.stage
            )));
        }
        deal.stage = next_stage;
        deal.updated_at = Utc::now();
        self.store.save_deal(&deal).await?;
        Ok(deal)
    }

    pub async fn delete_deal(&self, owner_id: &str, deal_id: &str) -> Result<bool> {
        self.store.delete_deal(owner_id, deal_id).await
    }

    pub async fn list_deals(&self, owner_id: &str) -> Result<Vec<Deal>> {
        self.store.list_deals(owner_id).await
    }

    pub async fn list_deals_for_company(
        &self,
        owner_id: &str,
        company_id: &str,
    ) -> Result<Vec<Deal>> {
        self.store
            .list_deals_for_company(owner_id, company_id)
            .await
    }

    // ── Interactions ───────────────────────────────────────────────────────

    pub async fn log_interaction(
        &self,
        owner_id: &str,
        input: InteractionInput,
    ) -> Result<Interaction> {
        self.get_contact(owner_id, &input.contact_id).await?;
        let mut interaction = Interaction::new(
            owner_id,
            &input.contact_id,
            input.kind,
            input.direction,
            &input.summary,
        )
        .let_deal(input.deal_id);
        if let Some(at) = input.at {
            interaction.at = at;
        }
        if !input.metadata.is_null() {
            interaction.metadata = input.metadata;
        }
        self.store.save_interaction(&interaction).await?;
        Ok(interaction)
    }

    pub async fn interactions_for_contact(
        &self,
        owner_id: &str,
        contact_id: &str,
    ) -> Result<Vec<Interaction>> {
        self.store
            .list_interactions_for_contact(owner_id, contact_id)
            .await
    }

    // ── Inbox ────────────────────────────────────────────────────────────
    // Thin wrappers over the store ledger; the orchestration (resolve →
    // file → dedup) lives in `crate::inbox` so there is exactly one
    // ingestion path.

    /// File one inbound event (see `crate::inbox::ingest`).
    pub async fn ingest_inbox_event(
        &self,
        owner_id: &str,
        event: crate::inbox::InboxEvent,
    ) -> Result<crate::inbox::IngestOutcome> {
        crate::inbox::ingest(self, owner_id, event).await
    }

    /// List filed inbox records, newest first.
    pub async fn list_inbox_events(
        &self,
        owner_id: &str,
        channel: Option<&str>,
        limit: usize,
    ) -> Result<Vec<crate::inbox::InboxRecord>> {
        self.store.list_inbox_events(owner_id, channel, limit).await
    }

    /// Insert-or-ignore one inbox record (dedup on
    /// `(owner_id, channel, external_id)`). Used by `crate::inbox::ingest`.
    pub async fn insert_inbox_event(&self, record: &crate::inbox::InboxRecord) -> Result<bool> {
        self.store.insert_inbox_event(record).await
    }

    /// Load one inbox record by its dedup key. Used by `crate::inbox::ingest`.
    pub async fn load_inbox_event(
        &self,
        owner_id: &str,
        channel: &str,
        external_id: &str,
    ) -> Result<Option<crate::inbox::InboxRecord>> {
        self.store
            .load_inbox_event(owner_id, channel, external_id)
            .await
    }

    // ── Search & summary ───────────────────────────────────────────────────

    pub async fn search(
        &self,
        owner_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        self.store.search_crm(owner_id, query, limit).await
    }

    /// Nearest-neighbour search over contact embeddings.
    pub async fn vector_search(
        &self,
        owner_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        self.store.vector_search(owner_id, query, limit).await
    }

    pub async fn summary(&self, owner_id: &str) -> Result<CrmSummary> {
        self.store.crm_summary(owner_id).await
    }

    /// Full context for a contact: the person + company + open deals + recent
    /// interactions. The agent's "who is this / what's in flight" answer.
    pub async fn contact_context(
        &self,
        owner_id: &str,
        contact_id: &str,
    ) -> Result<serde_json::Value> {
        let contact = self.get_contact(owner_id, contact_id).await?;
        let company = match &contact.company_id {
            Some(cid) => self.get_company(owner_id, cid).await.ok(),
            None => None,
        };
        let deals = match &contact.company_id {
            Some(cid) => self
                .list_deals_for_company(owner_id, cid)
                .await
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let interactions = self
            .interactions_for_contact(owner_id, contact_id)
            .await
            .unwrap_or_default();

        Ok(serde_json::json!({
            "contact": contact,
            "company": company,
            "deals": deals,
            "recent_interactions": interactions.iter().take(5).collect::<Vec<_>>(),
        }))
    }

    // ── CRM ⇄ calendar linkage ────────────────────────────────────────────

    /// Build a calendar [`crate::Attendee`] that carries this contact's id in
    /// metadata, so a booking is traceable back to the CRM person.
    pub async fn attendee_for_contact(
        &self,
        owner_id: &str,
        contact_id: &str,
    ) -> Result<crate::Attendee> {
        let contact = self.get_contact(owner_id, contact_id).await?;
        let mut attendee = crate::Attendee::new(contact.display_name(), contact.email);
        let context = self.contact_context(owner_id, contact_id).await?;
        attendee.metadata = serde_json::json!({
            "contact_id": contact.id,
            "phone": contact.phone,
            "context": context,
        });
        Ok(attendee)
    }

    /// Reverse of [`AgentCrm::attendee_for_contact`]: resolve a calendar
    /// [`crate::Attendee`] back to its CRM [`Contact`].
    ///
    /// Uses the `contact_id` stamp written by `attendee_for_contact` when
    /// present; otherwise falls back to an email match. Errors with
    /// [`crate::AgentError::ContactNotFound`] when neither resolves.
    pub async fn contact_for_attendee(
        &self,
        owner_id: &str,
        attendee: &crate::Attendee,
    ) -> Result<Contact> {
        if let Some(id) = attendee.metadata.get("contact_id").and_then(|v| v.as_str()) {
            if let Ok(contact) = self.get_contact(owner_id, id).await {
                return Ok(contact);
            }
        }
        if !attendee.email.is_empty() {
            if let Some(contact) = self.resolve_by_email(owner_id, &attendee.email).await? {
                return Ok(contact);
            }
        }
        Err(AgentError::ContactNotFound(attendee.id.clone()))
    }

    /// Resolve the CRM [`Contact`] behind a booking's first CRM-linked
    /// attendee (see [`AgentCrm::attendee_for_contact`]). Returns
    /// `ContactNotFound` when the booking has no attendees stamped with a
    /// `contact_id`.
    pub async fn contact_for_booking(
        &self,
        owner_id: &str,
        booking: &crate::Booking,
    ) -> Result<Contact> {
        for attendee in &booking.attendees {
            if attendee
                .metadata
                .get("contact_id")
                .and_then(|v| v.as_str())
                .is_some()
            {
                return self.contact_for_attendee(owner_id, attendee).await;
            }
        }
        Err(AgentError::ContactNotFound(booking.id.clone()))
    }
}

/// Small helper: attach an optional deal id to a new interaction.
trait LetDeal {
    fn let_deal(self, deal_id: Option<String>) -> Self;
}

impl LetDeal for Interaction {
    fn let_deal(mut self, deal_id: Option<String>) -> Self {
        self.deal_id = deal_id;
        self
    }
}
