//! Unified inbound ingestion — the comms/inbox façade.
//!
//! One ingestion path for every inbound channel (SMS, WhatsApp,
//! notifications, …), mirroring the outbound `aware.*` actions in
//! `src/bin/aware.rs` (see `aware_sms` / `aware_capture` there for the
//! resolve-or-create ethos this follows).
//!
//! Transport-free: this module never touches HTTP or sockets. Transports
//! normalise their payloads into [`InboxEvent`] and call [`ingest`].
//!
//! Guarantees:
//! - Exactly-once per `(owner_id, channel, external_id)`, enforced by a
//!   unique index (libSQL) / keyed map (memory) plus insert-or-ignore on the
//!   write path. Concurrent duplicate delivery still yields a single ledger
//!   row; the pre-write load is a fast path, the unique index is the backstop.
//! - Conservative on unknown senders: **no contacts, companies, or deals are
//!   created on inbound**. An unresolvable sender yields a structured
//!   [`IngestStatus::UnknownSender`] outcome and the raw event is still filed
//!   in the ledger (so redelivery dedups and nothing is silently dropped).
//!   Creating CRM entities from inbound traffic stays an explicit outbound /
//!   capture decision (`aware_capture`), matching the existing
//!   "flag for capture, no contact created" precedent in `aware_sms`.
//!
//! Related work: a sibling agent is building an append-only action log
//! (`src/actions.rs::record_action`). It was absent from this checkout, so
//! ingestion does not record actions yet — wiring that in is a follow-up once
//! the log lands.

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crm::agent::AgentCrm;
use crate::crm::store::InteractionInput;
use crate::crm::types::{Contact, InteractionDirection, InteractionKind};
use crate::error::{AgentError, Result};
use crate::kill_switch::ChannelGate;

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

/// One inbound message from any transport, normalised by the caller.
///
/// `channel` is open-ended (`"sms"`, `"whatsapp"`, `"notification"`, …);
/// known channels map to an [`InteractionKind`] via [`kind_for_channel`],
/// unknown ones file as `Message`. `at_ms` is millis since the Unix epoch;
/// values `<= 0` mean "now".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxEvent {
    pub channel: String,
    pub external_id: String,
    pub from: String,
    #[serde(default)]
    pub body: String,
    pub at_ms: i64,
}

impl InboxEvent {
    pub fn new(
        channel: impl Into<String>,
        external_id: impl Into<String>,
        from: impl Into<String>,
        body: impl Into<String>,
        at_ms: i64,
    ) -> Self {
        Self {
            channel: channel.into(),
            external_id: external_id.into(),
            from: from.into(),
            body: body.into(),
            at_ms,
        }
    }
}

/// Ledger status of a filed [`InboxRecord`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InboxStatus {
    Ingested,
    UnknownSender,
}

/// One filed inbox event — the dedup ledger row. Immutable after insert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxRecord {
    pub id: String,
    pub owner_id: String,
    pub channel: String,
    pub external_id: String,
    pub from: String,
    #[serde(default)]
    pub body: String,
    pub at: DateTime<Utc>,
    pub status: InboxStatus,
    pub contact_id: Option<String>,
    pub interaction_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl InboxRecord {
    pub fn new(owner_id: &str, event: &InboxEvent, at: DateTime<Utc>) -> Self {
        let now = Utc::now();
        Self {
            id: new_id(),
            owner_id: owner_id.to_string(),
            channel: event.channel.clone(),
            external_id: event.external_id.clone(),
            from: event.from.clone(),
            body: event.body.clone(),
            at,
            status: InboxStatus::Ingested,
            contact_id: None,
            interaction_id: None,
            created_at: now,
        }
    }
}

/// Outcome of [`ingest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IngestStatus {
    /// Resolved and filed as an inbound interaction.
    Ingested,
    /// `(channel, external_id)` was already filed; no new interaction.
    Duplicate,
    /// Sender did not resolve; event filed, nothing else created.
    UnknownSender,
}

/// The result of [`ingest`]: machine-readable status plus the ledger row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestOutcome {
    pub status: IngestStatus,
    pub record: InboxRecord,
    pub contact_id: Option<String>,
    pub interaction_id: Option<String>,
}

/// Map a channel name to the interaction kind its events file as.
/// Unknown channels file as `Message` — ingestion never fails on a new
/// channel name.
pub fn kind_for_channel(channel: &str) -> InteractionKind {
    match channel.trim().to_lowercase().as_str() {
        "sms" => InteractionKind::Sms,
        "whatsapp" | "wa" => InteractionKind::Message,
        "email" | "mail" => InteractionKind::Email,
        "call" | "phone" | "voice" => InteractionKind::Call,
        "notification" | "push" => InteractionKind::Note,
        _ => InteractionKind::Message,
    }
}

/// Resolve an event sender to a contact: email addresses (`"@"`) go through
/// `resolve_by_email`, everything else through `resolve_by_phone` (which
/// handles digit-normalised phone matching).
async fn resolve_sender(crm: &AgentCrm, owner_id: &str, from: &str) -> Result<Option<Contact>> {
    if from.contains('@') {
        crm.resolve_by_email(owner_id, from).await
    } else {
        crm.resolve_by_phone(owner_id, from).await
    }
}

fn event_time(at_ms: i64) -> DateTime<Utc> {
    if at_ms <= 0 {
        Utc::now()
    } else {
        Utc.timestamp_millis_opt(at_ms)
            .single()
            .unwrap_or_else(Utc::now)
    }
}

fn validate(owner_id: &str, event: &InboxEvent) -> Result<()> {
    if owner_id.trim().is_empty() {
        return Err(AgentError::Validation("missing param: owner".into()));
    }
    if event.channel.trim().is_empty() {
        return Err(AgentError::Validation(
            "missing param: event.channel".into(),
        ));
    }
    if event.external_id.trim().is_empty() {
        return Err(AgentError::Validation(
            "missing param: event.external_id".into(),
        ));
    }
    if event.from.trim().is_empty() {
        return Err(AgentError::Validation("missing param: event.from".into()));
    }
    Ok(())
}

fn duplicate_of(record: InboxRecord) -> IngestOutcome {
    IngestOutcome {
        status: IngestStatus::Duplicate,
        contact_id: record.contact_id.clone(),
        interaction_id: record.interaction_id.clone(),
        record,
    }
}

/// Ingest one inbound event: dedup → resolve sender → file as an inbound
/// interaction. Exactly-once per `(owner_id, channel, external_id)`.
///
/// Unresolvable senders return [`IngestStatus::UnknownSender`] — the event is
/// still filed in the ledger, but no contact, company, deal, or interaction
/// is created.
pub async fn ingest(crm: &AgentCrm, owner_id: &str, event: InboxEvent) -> Result<IngestOutcome> {
    validate(owner_id, &event)?;

    // Fast-path dedup: already filed → return the existing row.
    if let Some(existing) = crm
        .load_inbox_event(owner_id, &event.channel, &event.external_id)
        .await?
    {
        return Ok(duplicate_of(existing));
    }

    let at = event_time(event.at_ms);
    match resolve_sender(crm, owner_id, &event.from).await? {
        Some(contact) => {
            let kind = kind_for_channel(&event.channel);
            let summary = format!("{}: {}", event.channel.trim(), event.body);
            let interaction = crm
                .log_interaction(
                    owner_id,
                    InteractionInput::new(&contact.id, kind)
                        .with_direction(InteractionDirection::Inbound)
                        .with_summary(summary)
                        .with_at(at)
                        .with_metadata(serde_json::json!({
                            "inbox_channel": event.channel,
                            "inbox_external_id": event.external_id,
                        })),
                )
                .await?;
            let mut record = InboxRecord::new(owner_id, &event, at);
            record.contact_id = Some(contact.id.clone());
            record.interaction_id = Some(interaction.id.clone());
            // Backstop: unique index wins any race; the loser returns Duplicate.
            // (A lost race can leave a second filed interaction — acceptable on
            // a single-daemon phone target; the ledger itself stays exactly-once.)
            if !crm.insert_inbox_event(&record).await? {
                let existing = crm
                    .load_inbox_event(owner_id, &event.channel, &event.external_id)
                    .await?
                    .unwrap_or(record);
                return Ok(duplicate_of(existing));
            }
            Ok(IngestOutcome {
                status: IngestStatus::Ingested,
                contact_id: Some(contact.id),
                interaction_id: Some(interaction.id),
                record,
            })
        }
        None => {
            let mut record = InboxRecord::new(owner_id, &event, at);
            record.status = InboxStatus::UnknownSender;
            if !crm.insert_inbox_event(&record).await? {
                let existing = crm
                    .load_inbox_event(owner_id, &event.channel, &event.external_id)
                    .await?
                    .unwrap_or(record);
                return Ok(duplicate_of(existing));
            }
            Ok(IngestOutcome {
                status: IngestStatus::UnknownSender,
                contact_id: None,
                interaction_id: None,
                record,
            })
        }
    }
}

/// Kill-switched ingestion: rejects before any I/O when `event.channel` is
/// disabled, so calling the agent function directly (bypassing dispatch)
/// still fails closed. This is the underlying send-path enforcement point;
/// [`crate::rpc::dispatch_with_gate`] enforces the same gate at dispatch.
pub async fn ingest_with_gate(
    crm: &AgentCrm,
    owner_id: &str,
    event: InboxEvent,
    gate: &ChannelGate,
) -> Result<IngestOutcome> {
    gate.ensure_enabled(&event.channel)?;
    ingest(crm, owner_id, event).await
}

/// List an owner's filed inbox records, newest first, optionally filtered by
/// channel and capped at `limit`.
pub async fn list(
    crm: &AgentCrm,
    owner_id: &str,
    channel: Option<&str>,
    limit: usize,
) -> Result<Vec<InboxRecord>> {
    if owner_id.trim().is_empty() {
        return Err(AgentError::Validation("missing param: owner".into()));
    }
    let channel = channel.filter(|c| !c.trim().is_empty());
    crm.list_inbox_events(owner_id, channel, limit).await
}
