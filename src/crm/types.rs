//! CRM data structures for the Chief of Staff engine.
//!
//! Mirrors the calendar `types.rs` conventions: pure serde data, JSON
//! round-trip safe, `new()` helpers, timestamps as `chrono::DateTime<Utc>`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now_utc() -> DateTime<Utc> {
    Utc::now()
}

// ── Enums ────────────────────────────────────────────────────────────────────

/// Deal pipeline stage. Ordered for stage transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DealStage {
    Lead,
    Qualified,
    Proposal,
    Negotiation,
    ClosedWon,
    ClosedLost,
}

impl DealStage {
    /// All stages in ascending pipeline order.
    pub const ALL: [DealStage; 6] = [
        DealStage::Lead,
        DealStage::Qualified,
        DealStage::Proposal,
        DealStage::Negotiation,
        DealStage::ClosedWon,
        DealStage::ClosedLost,
    ];

    /// Next stage in the pipeline (ClosedLost has no forward move).
    pub fn next(self) -> Option<DealStage> {
        match self {
            DealStage::Lead => Some(DealStage::Qualified),
            DealStage::Qualified => Some(DealStage::Proposal),
            DealStage::Proposal => Some(DealStage::Negotiation),
            DealStage::Negotiation => Some(DealStage::ClosedWon),
            DealStage::ClosedWon | DealStage::ClosedLost => None,
        }
    }

    /// True if the deal is active (still in the pipeline).
    pub fn is_open(self) -> bool {
        !matches!(self, DealStage::ClosedWon | DealStage::ClosedLost)
    }
}

/// How an interaction with a contact happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InteractionKind {
    Call,
    Sms,
    Email,
    Meeting,
    Note,
    Message,
}

/// Direction of an interaction relative to the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InteractionDirection {
    Inbound,
    Outbound,
}

// ── Company ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Company {
    pub id: String,
    pub owner_id: String,
    pub name: String,
    pub industry: String,
    pub website: String,
    pub stage: DealStage,
    pub deal_value: f64,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Company {
    pub fn new(
        owner_id: impl Into<String>,
        name: impl Into<String>,
        industry: impl Into<String>,
    ) -> Self {
        let now = now_utc();
        Self {
            id: new_id(),
            owner_id: owner_id.into(),
            name: name.into(),
            industry: industry.into(),
            website: String::new(),
            stage: DealStage::Lead,
            deal_value: 0.0,
            tags: Vec::new(),
            notes: String::new(),
            metadata: serde_json::Value::Null,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_stage(mut self, stage: DealStage) -> Self {
        self.stage = stage;
        self
    }
}

// ── Contact ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contact {
    pub id: String,
    pub owner_id: String,
    pub first_name: String,
    pub last_name: String,
    pub email: String,
    pub phone: String,
    /// FK → `Company.id` (nullable).
    pub company_id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub is_vip: bool,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Contact {
    pub fn new(
        owner_id: impl Into<String>,
        first_name: impl Into<String>,
        last_name: impl Into<String>,
    ) -> Self {
        let now = now_utc();
        Self {
            id: new_id(),
            owner_id: owner_id.into(),
            first_name: first_name.into(),
            last_name: last_name.into(),
            email: String::new(),
            phone: String::new(),
            company_id: None,
            title: String::new(),
            tags: Vec::new(),
            is_vip: false,
            notes: String::new(),
            metadata: serde_json::Value::Null,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_company(mut self, company_id: impl Into<String>) -> Self {
        self.company_id = Some(company_id.into());
        self
    }

    pub fn with_phone(mut self, phone: impl Into<String>) -> Self {
        self.phone = phone.into();
        self
    }

    /// Add an alternate phone number (stored in `metadata.alt_phones`).
    /// `resolve_by_phone` matches against every number, not just `phone`.
    pub fn with_alt_phone(mut self, phone: impl Into<String>) -> Self {
        let phones = self
            .metadata
            .get("alt_phones")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut phones = phones;
        phones.push(phone.into());
        self.metadata["alt_phones"] = serde_json::json!(phones);
        self
    }

    /// Every number on this contact: primary `phone` + `metadata.alt_phones`.
    pub fn all_phones(&self) -> Vec<String> {
        let mut out = vec![self.phone.clone()];
        if let Some(arr) = self.metadata.get("alt_phones").and_then(|v| v.as_array()) {
            out.extend(arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())));
        }
        out
    }

    pub fn with_email(mut self, email: impl Into<String>) -> Self {
        self.email = email.into();
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    pub fn with_vip(mut self) -> Self {
        self.is_vip = true;
        self
    }

    pub fn with_notes(mut self, notes: impl Into<String>) -> Self {
        self.notes = notes.into();
        self
    }

    pub fn display_name(&self) -> String {
        format!("{} {}", self.first_name, self.last_name)
            .trim()
            .to_string()
    }
}

// ── Deal ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deal {
    pub id: String,
    pub owner_id: String,
    pub company_id: String,
    /// Optional primary contact on the deal.
    pub contact_id: Option<String>,
    pub name: String,
    pub stage: DealStage,
    pub amount: f64,
    pub probability: f64,
    pub expected_close: Option<DateTime<Utc>>,
    #[serde(default)]
    pub next_action: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Deal {
    pub fn new(
        owner_id: impl Into<String>,
        company_id: impl Into<String>,
        name: impl Into<String>,
        amount: f64,
    ) -> Self {
        let now = now_utc();
        Self {
            id: new_id(),
            owner_id: owner_id.into(),
            company_id: company_id.into(),
            contact_id: None,
            name: name.into(),
            stage: DealStage::Lead,
            amount,
            probability: 0.1,
            expected_close: None,
            next_action: String::new(),
            notes: String::new(),
            metadata: serde_json::Value::Null,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_contact(mut self, contact_id: impl Into<String>) -> Self {
        self.contact_id = Some(contact_id.into());
        self
    }
}

// ── Interaction ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Interaction {
    pub id: String,
    pub owner_id: String,
    pub contact_id: String,
    /// Optional FK → `Deal.id`.
    pub deal_id: Option<String>,
    pub kind: InteractionKind,
    pub direction: InteractionDirection,
    pub at: DateTime<Utc>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl Interaction {
    pub fn new(
        owner_id: impl Into<String>,
        contact_id: impl Into<String>,
        kind: InteractionKind,
        direction: InteractionDirection,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            id: new_id(),
            owner_id: owner_id.into(),
            contact_id: contact_id.into(),
            deal_id: None,
            kind,
            direction,
            at: now_utc(),
            summary: summary.into(),
            metadata: serde_json::Value::Null,
        }
    }

    pub fn with_deal(mut self, deal_id: impl Into<String>) -> Self {
        self.deal_id = Some(deal_id.into());
        self
    }
}

// ── Summary ──────────────────────────────────────────────────────────────────

/// Compact per-owner CRM stats for agent reasoning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrmSummary {
    pub owner_id: String,
    pub companies: usize,
    pub contacts: usize,
    pub deals: usize,
    pub open_deals: usize,
    pub closed_won: usize,
    pub vip_contacts: usize,
    pub interactions: usize,
}

impl CrmSummary {
    pub fn empty(owner_id: impl Into<String>) -> Self {
        Self {
            owner_id: owner_id.into(),
            companies: 0,
            contacts: 0,
            deals: 0,
            open_deals: 0,
            closed_won: 0,
            vip_contacts: 0,
            interactions: 0,
        }
    }
}

// ── Vector helpers ───────────────────────────────────────────────────────────

/// Build a 64-dim F32 vector from text via a simple char-hash bag-of-words.
///
/// This is a deterministic local embedding for retrieval that requires no
/// external model. Callers can replace it with a real embedding model and
/// store the output in the same `F32_BLOB(64)` column.
pub fn embed_text(text: &str) -> Vec<f32> {
    let dim = 64usize;
    let mut v = vec![0f32; dim];
    let lower = text.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();
    for tok in tokens {
        let mut h: u64 = 14695981039346656037;
        for b in tok.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(1099511628211);
        }
        let idx = (h as usize) % dim;
        v[idx] += 1.0;
    }
    // L2 normalize.
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-9 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

/// Format an f32 vector as a libSQL `vector32('[..]')` argument body.
/// (Bound as a parameter, so no surrounding SQL quotes — just the JSON array.)
pub fn vector_literal(v: &[f32]) -> String {
    let inner: Vec<String> = v.iter().map(|x| format!("{x:.6}")).collect();
    format!("[{}]", inner.join(","))
}
