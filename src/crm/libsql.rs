//! `CrmStore` implementation over the shared libSQL connection.
//!
//! CRM rows live in the same database file as calendars, so one `LibSqlStore`
//! serves both `AgentCal` (calendar) and `AgentCrm` (CRM) — the store is the
//! single source of truth for the whole CoS operating picture.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use libsql::{params, Row};

use super::{CrmStore, SearchHit};
use crate::crm::types::{
    embed_text, vector_literal, Company, Contact, CrmSummary, Deal, DealStage, Interaction,
    InteractionDirection, InteractionKind,
};
use crate::error::{Result, StoreError};
use crate::inbox::{InboxRecord, InboxStatus};
use crate::store::LibSqlStore;

// ── enum <-> str helpers ─────────────────────────────────────────────────────

fn stage_str(s: DealStage) -> &'static str {
    match s {
        DealStage::Lead => "LEAD",
        DealStage::Qualified => "QUALIFIED",
        DealStage::Proposal => "PROPOSAL",
        DealStage::Negotiation => "NEGOTIATION",
        DealStage::ClosedWon => "CLOSED_WON",
        DealStage::ClosedLost => "CLOSED_LOST",
    }
}

fn stage_from(s: &str) -> DealStage {
    match s {
        "QUALIFIED" => DealStage::Qualified,
        "PROPOSAL" => DealStage::Proposal,
        "NEGOTIATION" => DealStage::Negotiation,
        "CLOSED_WON" => DealStage::ClosedWon,
        "CLOSED_LOST" => DealStage::ClosedLost,
        _ => DealStage::Lead,
    }
}

fn kind_str(k: InteractionKind) -> &'static str {
    match k {
        InteractionKind::Call => "CALL",
        InteractionKind::Sms => "SMS",
        InteractionKind::Email => "EMAIL",
        InteractionKind::Meeting => "MEETING",
        InteractionKind::Note => "NOTE",
        InteractionKind::Message => "MESSAGE",
    }
}

fn kind_from(s: &str) -> InteractionKind {
    match s {
        "CALL" => InteractionKind::Call,
        "SMS" => InteractionKind::Sms,
        "EMAIL" => InteractionKind::Email,
        "MEETING" => InteractionKind::Meeting,
        "MESSAGE" => InteractionKind::Message,
        _ => InteractionKind::Note,
    }
}

fn direction_str(d: InteractionDirection) -> &'static str {
    match d {
        InteractionDirection::Inbound => "INBOUND",
        InteractionDirection::Outbound => "OUTBOUND",
    }
}

fn direction_from(s: &str) -> InteractionDirection {
    if s == "INBOUND" {
        InteractionDirection::Inbound
    } else {
        InteractionDirection::Outbound
    }
}

fn int_bool(v: bool) -> i64 {
    if v {
        1
    } else {
        0
    }
}

fn bool_from(v: i64) -> bool {
    v != 0
}

fn opt_id(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

// ── row decoding ────────────────────────────────────────────────────────────

fn decode_company(row: &Row) -> Result<Company> {
    let tags: Vec<String> =
        serde_json::from_value(crate::store::get_json(row, 7)?).unwrap_or_default();
    Ok(Company {
        id: crate::store::get_text(row, 0)?,
        owner_id: crate::store::get_text(row, 1)?,
        name: crate::store::get_text(row, 2)?,
        industry: crate::store::get_text(row, 3)?,
        website: crate::store::get_text(row, 4)?,
        stage: stage_from(&crate::store::get_text(row, 5)?),
        deal_value: crate::store::get_real(row, 6)?,
        tags,
        notes: crate::store::get_text(row, 8)?,
        metadata: crate::store::get_json(row, 9)?,
        created_at: crate::store::get_dt(row, 10)?,
        updated_at: crate::store::get_dt(row, 11)?,
    })
}

fn decode_contact(row: &Row) -> Result<Contact> {
    let tags: Vec<String> =
        serde_json::from_value(crate::store::get_json(row, 8)?).unwrap_or_default();
    Ok(Contact {
        id: crate::store::get_text(row, 0)?,
        owner_id: crate::store::get_text(row, 1)?,
        first_name: crate::store::get_text(row, 2)?,
        last_name: crate::store::get_text(row, 3)?,
        email: crate::store::get_text(row, 4)?,
        phone: crate::store::get_text(row, 5)?,
        company_id: opt_id(&crate::store::get_text(row, 6)?),
        title: crate::store::get_text(row, 7)?,
        tags,
        is_vip: bool_from(crate::store::get_int(row, 9)?),
        notes: crate::store::get_text(row, 10)?,
        metadata: crate::store::get_json(row, 11)?,
        created_at: crate::store::get_dt(row, 12)?,
        updated_at: crate::store::get_dt(row, 13)?,
    })
}

fn decode_deal(row: &Row) -> Result<Deal> {
    let expected_close = {
        let s = crate::store::get_text(row, 8)?;
        if s.is_empty() {
            None
        } else {
            Some(
                DateTime::parse_from_rfc3339(&s)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or(Utc::now()),
            )
        }
    };
    Ok(Deal {
        id: crate::store::get_text(row, 0)?,
        owner_id: crate::store::get_text(row, 1)?,
        company_id: crate::store::get_text(row, 2)?,
        contact_id: opt_id(&crate::store::get_text(row, 3)?),
        name: crate::store::get_text(row, 4)?,
        stage: stage_from(&crate::store::get_text(row, 5)?),
        amount: crate::store::get_real(row, 6)?,
        probability: crate::store::get_real(row, 7)?,
        expected_close,
        next_action: crate::store::get_text(row, 9)?,
        notes: crate::store::get_text(row, 10)?,
        metadata: crate::store::get_json(row, 11)?,
        created_at: crate::store::get_dt(row, 12)?,
        updated_at: crate::store::get_dt(row, 13)?,
    })
}

fn decode_interaction(row: &Row) -> Result<Interaction> {
    Ok(Interaction {
        id: crate::store::get_text(row, 0)?,
        owner_id: crate::store::get_text(row, 1)?,
        contact_id: crate::store::get_text(row, 2)?,
        deal_id: opt_id(&crate::store::get_text(row, 3)?),
        kind: kind_from(&crate::store::get_text(row, 4)?),
        direction: direction_from(&crate::store::get_text(row, 5)?),
        at: crate::store::get_dt(row, 6)?,
        summary: crate::store::get_text(row, 7)?,
        metadata: crate::store::get_json(row, 8)?,
    })
}

fn decode_inbox(row: &Row) -> Result<InboxRecord> {
    let status = match crate::store::get_text(row, 7)?.as_str() {
        "UNKNOWN_SENDER" => InboxStatus::UnknownSender,
        _ => InboxStatus::Ingested,
    };
    Ok(InboxRecord {
        id: crate::store::get_text(row, 0)?,
        owner_id: crate::store::get_text(row, 1)?,
        channel: crate::store::get_text(row, 2)?,
        external_id: crate::store::get_text(row, 3)?,
        from: crate::store::get_text(row, 4)?,
        body: crate::store::get_text(row, 5)?,
        at: crate::store::get_dt(row, 6)?,
        status,
        contact_id: opt_id(&crate::store::get_text(row, 8)?),
        interaction_id: opt_id(&crate::store::get_text(row, 9)?),
        created_at: crate::store::get_dt(row, 10)?,
    })
}

fn inbox_status_str(s: InboxStatus) -> &'static str {
    match s {
        InboxStatus::Ingested => "INGESTED",
        InboxStatus::UnknownSender => "UNKNOWN_SENDER",
    }
}

fn to_json(v: &serde_json::Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".to_string())
}

fn fts_escape(s: &str) -> String {
    s.replace(['"', '\''], " ")
}

/// Build a safe FTS5 `MATCH` expression from free-text input.
/// Each word is double-quoted as a phrase-literal with a `*` prefix operator
/// (`"hodg"*`), so special characters are inert AND partial words still match.
/// Terms are OR'ed so any word hitting is enough.
fn fts_match(query: &str) -> String {
    let words: Vec<String> = query
        .split_whitespace()
        .map(|w| w.trim_matches(['"', '\'', '(', ')', ',', '.', ':', ';', '*', '-']))
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{}\"*", w.replace('"', " ")))
        .collect();
    if words.is_empty() {
        "\"\"".to_string()
    } else {
        words.join(" OR ")
    }
}

/// True when the query has no token worth running through FTS.
fn fts_stopword(query: &str) -> bool {
    query
        .split_whitespace()
        .all(|w| w.chars().all(|c| !c.is_alphanumeric()))
}

#[async_trait]
impl CrmStore for LibSqlStore {
    // ── Companies ──────────────────────────────────────────────────────────
    async fn save_company(&self, company: &Company) -> Result<()> {
        let conn = self.connection().lock().await;
        conn.execute(
            "INSERT INTO companies
             (id, owner_id, name, industry, website, stage, deal_value, tags, notes, metadata, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
               owner_id=excluded.owner_id, name=excluded.name, industry=excluded.industry,
               website=excluded.website, stage=excluded.stage, deal_value=excluded.deal_value,
               tags=excluded.tags, notes=excluded.notes, metadata=excluded.metadata,
               updated_at=excluded.updated_at",
            params![
                company.id.as_str(),
                company.owner_id.as_str(),
                company.name.as_str(),
                company.industry.as_str(),
                company.website.as_str(),
                stage_str(company.stage),
                company.deal_value,
                to_json(&serde_json::json!(company.tags)),
                company.notes.as_str(),
                to_json(&company.metadata),
                company.created_at.to_rfc3339(),
                company.updated_at.to_rfc3339(),
            ],
        )
        .await
        .map_err(StoreError::from)?;
        self.fts_upsert(
            &conn,
            &company.owner_id,
            "company",
            &company.id,
            &company.name,
            &format!(
                "{} / {} / {}",
                company.industry, company.deal_value, company.notes
            ),
        )
        .await?;
        Ok(())
    }

    async fn load_company(&self, owner_id: &str, company_id: &str) -> Result<Option<Company>> {
        let conn = self.connection().lock().await;
        let mut rows = conn
            .query(
                "SELECT id, owner_id, name, industry, website, stage, deal_value, tags,
                        notes, metadata, created_at, updated_at
                 FROM companies WHERE id = ? AND owner_id = ?",
                params![company_id, owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        match rows.next().await.map_err(StoreError::from)? {
            Some(row) => Ok(Some(decode_company(&row)?)),
            None => Ok(None),
        }
    }

    async fn delete_company(&self, owner_id: &str, company_id: &str) -> Result<bool> {
        let conn = self.connection().lock().await;
        let n = conn
            .execute(
                "DELETE FROM companies WHERE id = ? AND owner_id = ?",
                params![company_id, owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        if n > 0 {
            conn.execute("DELETE FROM crm_fts WHERE id = ?", params![company_id])
                .await
                .map_err(StoreError::from)?;
        }
        Ok(n > 0)
    }

    async fn list_companies(&self, owner_id: &str) -> Result<Vec<Company>> {
        let conn = self.connection().lock().await;
        let mut rows = conn
            .query(
                "SELECT id, owner_id, name, industry, website, stage, deal_value, tags,
                        notes, metadata, created_at, updated_at
                 FROM companies WHERE owner_id = ? ORDER BY name",
                params![owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(StoreError::from)? {
            out.push(decode_company(&row)?);
        }
        Ok(out)
    }

    // ── Contacts ───────────────────────────────────────────────────────────
    async fn save_contact(&self, contact: &Contact) -> Result<()> {
        let conn = self.connection().lock().await;
        let emb = embed_text(&format!(
            "{} {} {} {} {}",
            contact.first_name,
            contact.last_name,
            contact.title,
            contact.notes,
            contact.tags.join(" ")
        ));
        conn.execute(
            "INSERT INTO contacts
             (id, owner_id, first_name, last_name, email, phone, company_id, title,
              tags, is_vip, notes, metadata, created_at, updated_at, embedding)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, vector32(?))
             ON CONFLICT(id) DO UPDATE SET
               owner_id=excluded.owner_id, first_name=excluded.first_name, last_name=excluded.last_name,
               email=excluded.email, phone=excluded.phone, company_id=excluded.company_id,
               title=excluded.title, tags=excluded.tags, is_vip=excluded.is_vip,
               notes=excluded.notes, metadata=excluded.metadata, updated_at=excluded.updated_at,
               embedding=excluded.embedding",
            params![
                contact.id.as_str(),
                contact.owner_id.as_str(),
                contact.first_name.as_str(),
                contact.last_name.as_str(),
                contact.email.as_str(),
                contact.phone.as_str(),
                contact.company_id.as_deref().unwrap_or(""),
                contact.title.as_str(),
                to_json(&serde_json::json!(contact.tags)),
                int_bool(contact.is_vip),
                contact.notes.as_str(),
                to_json(&contact.metadata),
                contact.created_at.to_rfc3339(),
                contact.updated_at.to_rfc3339(),
                vector_literal(&emb),
            ],
        )
        .await
        .map_err(StoreError::from)?;
        let label = format!(
            "{} {} / {}",
            contact.first_name, contact.last_name, contact.title
        );
        let snippet = format!("{} / {} / {}", contact.email, contact.phone, contact.notes);
        self.fts_upsert(
            &conn,
            &contact.owner_id,
            "contact",
            &contact.id,
            &label,
            &snippet,
        )
        .await?;
        Ok(())
    }

    async fn load_contact(&self, owner_id: &str, contact_id: &str) -> Result<Option<Contact>> {
        let conn = self.connection().lock().await;
        let mut rows = conn
            .query(
                "SELECT id, owner_id, first_name, last_name, email, phone, company_id, title,
                        tags, is_vip, notes, metadata, created_at, updated_at
                 FROM contacts WHERE id = ? AND owner_id = ?",
                params![contact_id, owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        match rows.next().await.map_err(StoreError::from)? {
            Some(row) => Ok(Some(decode_contact(&row)?)),
            None => Ok(None),
        }
    }

    async fn delete_contact(&self, owner_id: &str, contact_id: &str) -> Result<bool> {
        let conn = self.connection().lock().await;
        conn.execute(
            "DELETE FROM interactions WHERE contact_id = ? AND owner_id = ?",
            params![contact_id, owner_id],
        )
        .await
        .map_err(StoreError::from)?;
        let n = conn
            .execute(
                "DELETE FROM contacts WHERE id = ? AND owner_id = ?",
                params![contact_id, owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        if n > 0 {
            conn.execute("DELETE FROM crm_fts WHERE id = ?", params![contact_id])
                .await
                .map_err(StoreError::from)?;
        }
        Ok(n > 0)
    }

    async fn list_contacts(&self, owner_id: &str) -> Result<Vec<Contact>> {
        let conn = self.connection().lock().await;
        let mut rows = conn
            .query(
                "SELECT id, owner_id, first_name, last_name, email, phone, company_id, title,
                        tags, is_vip, notes, metadata, created_at, updated_at
                 FROM contacts WHERE owner_id = ? ORDER BY last_name, first_name",
                params![owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(StoreError::from)? {
            out.push(decode_contact(&row)?);
        }
        Ok(out)
    }

    // ── Deals ──────────────────────────────────────────────────────────────
    async fn save_deal(&self, deal: &Deal) -> Result<()> {
        let conn = self.connection().lock().await;
        conn.execute(
            "INSERT INTO deals
             (id, owner_id, company_id, contact_id, name, stage, amount, probability,
              expected_close, next_action, notes, metadata, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
               owner_id=excluded.owner_id, company_id=excluded.company_id, contact_id=excluded.contact_id,
               name=excluded.name, stage=excluded.stage, amount=excluded.amount,
               probability=excluded.probability, expected_close=excluded.expected_close,
               next_action=excluded.next_action, notes=excluded.notes, metadata=excluded.metadata,
               updated_at=excluded.updated_at",
            params![
                deal.id.as_str(),
                deal.owner_id.as_str(),
                deal.company_id.as_str(),
                deal.contact_id.as_deref().unwrap_or(""),
                deal.name.as_str(),
                stage_str(deal.stage),
                deal.amount,
                deal.probability,
                deal.expected_close.map(|d| d.to_rfc3339()).unwrap_or_default(),
                deal.next_action.as_str(),
                deal.notes.as_str(),
                to_json(&deal.metadata),
                deal.created_at.to_rfc3339(),
                deal.updated_at.to_rfc3339(),
            ],
        )
        .await
        .map_err(StoreError::from)?;
        self.fts_upsert(
            &conn,
            &deal.owner_id,
            "deal",
            &deal.id,
            &deal.name,
            &format!(
                "{:?} / {:.2} / {}",
                deal.stage, deal.amount, deal.next_action
            ),
        )
        .await?;
        Ok(())
    }

    async fn load_deal(&self, owner_id: &str, deal_id: &str) -> Result<Option<Deal>> {
        let conn = self.connection().lock().await;
        let mut rows = conn
            .query(
                "SELECT id, owner_id, company_id, contact_id, name, stage, amount, probability,
                        expected_close, next_action, notes, metadata, created_at, updated_at
                 FROM deals WHERE id = ? AND owner_id = ?",
                params![deal_id, owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        match rows.next().await.map_err(StoreError::from)? {
            Some(row) => Ok(Some(decode_deal(&row)?)),
            None => Ok(None),
        }
    }

    async fn delete_deal(&self, owner_id: &str, deal_id: &str) -> Result<bool> {
        let conn = self.connection().lock().await;
        let n = conn
            .execute(
                "DELETE FROM deals WHERE id = ? AND owner_id = ?",
                params![deal_id, owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        if n > 0 {
            conn.execute("DELETE FROM crm_fts WHERE id = ?", params![deal_id])
                .await
                .map_err(StoreError::from)?;
        }
        Ok(n > 0)
    }

    async fn list_deals(&self, owner_id: &str) -> Result<Vec<Deal>> {
        let conn = self.connection().lock().await;
        let mut rows = conn
            .query(
                "SELECT id, owner_id, company_id, contact_id, name, stage, amount, probability,
                        expected_close, next_action, notes, metadata, created_at, updated_at
                 FROM deals WHERE owner_id = ? ORDER BY updated_at DESC",
                params![owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(StoreError::from)? {
            out.push(decode_deal(&row)?);
        }
        Ok(out)
    }

    async fn list_deals_for_company(&self, owner_id: &str, company_id: &str) -> Result<Vec<Deal>> {
        let conn = self.connection().lock().await;
        let mut rows = conn
            .query(
                "SELECT id, owner_id, company_id, contact_id, name, stage, amount, probability,
                        expected_close, next_action, notes, metadata, created_at, updated_at
                 FROM deals WHERE owner_id = ? AND company_id = ? ORDER BY updated_at DESC",
                params![owner_id, company_id],
            )
            .await
            .map_err(StoreError::from)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(StoreError::from)? {
            out.push(decode_deal(&row)?);
        }
        Ok(out)
    }

    // ── Interactions ───────────────────────────────────────────────────────
    async fn save_interaction(&self, interaction: &Interaction) -> Result<()> {
        let conn = self.connection().lock().await;
        conn.execute(
            "INSERT INTO interactions
             (id, owner_id, contact_id, deal_id, kind, direction, at, summary, metadata)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                interaction.id.as_str(),
                interaction.owner_id.as_str(),
                interaction.contact_id.as_str(),
                interaction.deal_id.as_deref().unwrap_or(""),
                kind_str(interaction.kind),
                direction_str(interaction.direction),
                interaction.at.to_rfc3339(),
                interaction.summary.as_str(),
                to_json(&interaction.metadata),
            ],
        )
        .await
        .map_err(StoreError::from)?;
        Ok(())
    }

    async fn list_interactions_for_contact(
        &self,
        owner_id: &str,
        contact_id: &str,
    ) -> Result<Vec<Interaction>> {
        let conn = self.connection().lock().await;
        let mut rows = conn
            .query(
                "SELECT id, owner_id, contact_id, deal_id, kind, direction, at, summary, metadata
                 FROM interactions WHERE owner_id = ? AND contact_id = ? ORDER BY at DESC",
                params![owner_id, contact_id],
            )
            .await
            .map_err(StoreError::from)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(StoreError::from)? {
            out.push(decode_interaction(&row)?);
        }
        Ok(out)
    }

    // ── Cross-entity lookups ───────────────────────────────────────────────
    async fn resolve_by_phone(&self, owner_id: &str, phone: &str) -> Result<Option<Contact>> {
        let normalized = phone
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect::<String>();
        let conn = self.connection().lock().await;
        let mut rows = conn
            .query(
                "SELECT id, owner_id, first_name, last_name, email, phone, company_id, title,
                        tags, is_vip, notes, metadata, created_at, updated_at
                 FROM contacts WHERE owner_id = ? AND phone = ?",
                params![owner_id, phone],
            )
            .await
            .map_err(StoreError::from)?;
        if let Some(row) = rows.next().await.map_err(StoreError::from)? {
            return Ok(Some(decode_contact(&row)?));
        }
        // Fall back to a digit-normalised match (handles +1, dashes, spaces).
        let mut rows = conn
            .query(
                "SELECT id, owner_id, first_name, last_name, email, phone, company_id, title,
                        tags, is_vip, notes, metadata, created_at, updated_at
                 FROM contacts WHERE owner_id = ?",
                params![owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        while let Some(row) = rows.next().await.map_err(StoreError::from)? {
            let c = decode_contact(&row)?;
            for candidate in c.all_phones() {
                let digits = candidate
                    .chars()
                    .filter(|ch| ch.is_ascii_digit())
                    .collect::<String>();
                if candidate == phone || (!digits.is_empty() && digits.ends_with(&normalized)) {
                    return Ok(Some(c));
                }
            }
        }
        Ok(None)
    }

    async fn resolve_by_email(&self, owner_id: &str, email: &str) -> Result<Option<Contact>> {
        let conn = self.connection().lock().await;
        let mut rows = conn
            .query(
                "SELECT id, owner_id, first_name, last_name, email, phone, company_id, title,
                        tags, is_vip, notes, metadata, created_at, updated_at
                 FROM contacts WHERE owner_id = ? AND email = ?",
                params![owner_id, email],
            )
            .await
            .map_err(StoreError::from)?;
        match rows.next().await.map_err(StoreError::from)? {
            Some(row) => Ok(Some(decode_contact(&row)?)),
            None => Ok(None),
        }
    }

    async fn contacts_for_company(&self, owner_id: &str, company_id: &str) -> Result<Vec<Contact>> {
        let conn = self.connection().lock().await;
        let mut rows = conn
            .query(
                "SELECT id, owner_id, first_name, last_name, email, phone, company_id, title,
                        tags, is_vip, notes, metadata, created_at, updated_at
                 FROM contacts WHERE owner_id = ? AND company_id = ? ORDER BY last_name",
                params![owner_id, company_id],
            )
            .await
            .map_err(StoreError::from)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(StoreError::from)? {
            out.push(decode_contact(&row)?);
        }
        Ok(out)
    }

    // ── Inbox ──────────────────────────────────────────────────────────────
    async fn insert_inbox_event(&self, record: &InboxRecord) -> Result<bool> {
        let conn = self.connection().lock().await;
        let n = conn
            .execute(
                "INSERT OR IGNORE INTO inbox_events
                 (id, owner_id, channel, external_id, sender, body, at, status,
                  contact_id, interaction_id, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    record.id.as_str(),
                    record.owner_id.as_str(),
                    record.channel.as_str(),
                    record.external_id.as_str(),
                    record.from.as_str(),
                    record.body.as_str(),
                    record.at.to_rfc3339(),
                    inbox_status_str(record.status),
                    record.contact_id.as_deref().unwrap_or(""),
                    record.interaction_id.as_deref().unwrap_or(""),
                    record.created_at.to_rfc3339(),
                ],
            )
            .await
            .map_err(StoreError::from)?;
        Ok(n > 0)
    }

    async fn load_inbox_event(
        &self,
        owner_id: &str,
        channel: &str,
        external_id: &str,
    ) -> Result<Option<InboxRecord>> {
        let conn = self.connection().lock().await;
        let mut rows = conn
            .query(
                "SELECT id, owner_id, channel, external_id, sender, body, at, status,
                        contact_id, interaction_id, created_at
                 FROM inbox_events
                 WHERE owner_id = ? AND channel = ? AND external_id = ?",
                params![owner_id, channel, external_id],
            )
            .await
            .map_err(StoreError::from)?;
        match rows.next().await.map_err(StoreError::from)? {
            Some(row) => Ok(Some(decode_inbox(&row)?)),
            None => Ok(None),
        }
    }

    async fn list_inbox_events(
        &self,
        owner_id: &str,
        channel: Option<&str>,
        limit: usize,
    ) -> Result<Vec<InboxRecord>> {
        let conn = self.connection().lock().await;
        let mut out = Vec::new();
        if let Some(channel) = channel {
            let mut rows = conn
                .query(
                    "SELECT id, owner_id, channel, external_id, sender, body, at, status,
                            contact_id, interaction_id, created_at
                     FROM inbox_events
                     WHERE owner_id = ? AND channel = ?
                     ORDER BY at DESC, rowid DESC LIMIT ?",
                    params![owner_id, channel, limit as i64],
                )
                .await
                .map_err(StoreError::from)?;
            while let Some(row) = rows.next().await.map_err(StoreError::from)? {
                out.push(decode_inbox(&row)?);
            }
        } else {
            let mut rows = conn
                .query(
                    "SELECT id, owner_id, channel, external_id, sender, body, at, status,
                            contact_id, interaction_id, created_at
                     FROM inbox_events
                     WHERE owner_id = ?
                     ORDER BY at DESC, rowid DESC LIMIT ?",
                    params![owner_id, limit as i64],
                )
                .await
                .map_err(StoreError::from)?;
            while let Some(row) = rows.next().await.map_err(StoreError::from)? {
                out.push(decode_inbox(&row)?);
            }
        }
        Ok(out)
    }

    // ── Search ─────────────────────────────────────────────────────────────
    async fn search_crm(
        &self,
        owner_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let conn = self.connection().lock().await;
        let like = format!("%{}%", fts_escape(query));
        let mut out = Vec::new();

        // FTS5 first: one ranked query across contacts + companies + deals.
        let trimmed = query.trim();
        if !trimmed.is_empty() && !fts_stopword(trimmed) {
            match conn
                .query(
                    "SELECT entity, id, label, snippet
                     FROM crm_fts
                     WHERE owner_id = ? AND crm_fts MATCH ?
                     ORDER BY bm25(crm_fts)
                     LIMIT ?",
                    params![owner_id, fts_match(trimmed), limit as i64],
                )
                .await
            {
                Ok(mut rows) => {
                    while let Some(row) = rows.next().await.map_err(StoreError::from)? {
                        out.push(SearchHit {
                            entity: row.get(0).map_err(StoreError::from)?,
                            id: row.get(1).map_err(StoreError::from)?,
                            label: row.get(2).map_err(StoreError::from)?,
                            snippet: row.get(3).map_err(StoreError::from)?,
                        });
                    }
                }
                Err(_) => { /* malformed MATCH → fall through to LIKE */ }
            }
        }
        if out.len() >= limit {
            return Ok(out.into_iter().take(limit).collect());
        }

        // Contacts (LIKE fallback).
        let mut rows = conn
            .query(
                "SELECT id, owner_id, first_name, last_name, email, phone, company_id, title,
                        tags, is_vip, notes, metadata, created_at, updated_at
                 FROM contacts
                 WHERE owner_id = ?
                   AND (first_name LIKE ? OR last_name LIKE ? OR email LIKE ? OR phone LIKE ? OR notes LIKE ?)
                 LIMIT ?",
                params![owner_id, like.clone(), like.clone(), like.clone(), like.clone(), like.clone(), limit as i64],
            )
            .await
            .map_err(StoreError::from)?;
        while let Some(row) = rows.next().await.map_err(StoreError::from)? {
            out.push(SearchHit::contact(&decode_contact(&row)?));
        }

        // Companies.
        let mut rows = conn
            .query(
                "SELECT id, owner_id, name, industry, website, stage, deal_value, tags,
                        notes, metadata, created_at, updated_at
                 FROM companies
                 WHERE owner_id = ? AND (name LIKE ? OR industry LIKE ? OR notes LIKE ?)
                 LIMIT ?",
                params![
                    owner_id,
                    like.clone(),
                    like.clone(),
                    like.clone(),
                    limit as i64
                ],
            )
            .await
            .map_err(StoreError::from)?;
        while let Some(row) = rows.next().await.map_err(StoreError::from)? {
            out.push(SearchHit::company(&decode_company(&row)?));
        }

        // Deals.
        let mut rows = conn
            .query(
                "SELECT id, owner_id, company_id, contact_id, name, stage, amount, probability,
                        expected_close, next_action, notes, metadata, created_at, updated_at
                 FROM deals
                 WHERE owner_id = ? AND (name LIKE ? OR next_action LIKE ? OR notes LIKE ?)
                 LIMIT ?",
                params![
                    owner_id,
                    like.clone(),
                    like.clone(),
                    like.clone(),
                    limit as i64
                ],
            )
            .await
            .map_err(StoreError::from)?;
        while let Some(row) = rows.next().await.map_err(StoreError::from)? {
            out.push(SearchHit::deal(&decode_deal(&row)?));
        }

        // Dedup (FTS + LIKE can both hit the same row) and cap at limit.
        let mut seen = std::collections::HashSet::new();
        let mut deduped = Vec::new();
        for hit in out {
            let key = (hit.entity.clone(), hit.id.clone());
            if seen.insert(key) {
                deduped.push(hit);
                if deduped.len() >= limit {
                    break;
                }
            }
        }
        Ok(deduped)
    }

    async fn vector_search(
        &self,
        owner_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let conn = self.connection().lock().await;
        let q = vector_literal(&embed_text(query));
        let mut out = Vec::new();
        let mut rows = conn
            .query(
                "SELECT id, owner_id, first_name, last_name, email, phone, company_id, title,
                        tags, is_vip, notes, metadata, created_at, updated_at
                 FROM contacts
                 WHERE owner_id = ?
                 ORDER BY vector_distance_cos(embedding, vector32(?))
                 LIMIT ?",
                params![owner_id, q, limit as i64],
            )
            .await
            .map_err(StoreError::from)?;
        while let Some(row) = rows.next().await.map_err(StoreError::from)? {
            out.push(SearchHit::contact(&decode_contact(&row)?));
        }
        Ok(out)
    }

    // ── Summary ────────────────────────────────────────────────────────────
    async fn crm_summary(&self, owner_id: &str) -> Result<CrmSummary> {
        let conn = self.connection().lock().await;
        let mut s = CrmSummary::empty(owner_id);

        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM companies WHERE owner_id = ?",
                params![owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        if let Some(row) = rows.next().await.map_err(StoreError::from)? {
            s.companies = crate::store::get_int(&row, 0)? as usize;
        }

        let mut rows = conn
            .query(
                "SELECT COUNT(*), SUM(is_vip) FROM contacts WHERE owner_id = ?",
                params![owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        if let Some(row) = rows.next().await.map_err(StoreError::from)? {
            s.contacts = crate::store::get_int(&row, 0)? as usize;
            s.vip_contacts = crate::store::get_opt_int(&row, 1)?.unwrap_or(0) as usize;
        }

        let mut rows = conn
            .query(
                "SELECT COUNT(*), SUM(CASE WHEN stage IN ('LEAD','QUALIFIED','PROPOSAL','NEGOTIATION') THEN 1 ELSE 0 END),
                        SUM(CASE WHEN stage = 'CLOSED_WON' THEN 1 ELSE 0 END)
                 FROM deals WHERE owner_id = ?",
                params![owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        if let Some(row) = rows.next().await.map_err(StoreError::from)? {
            s.deals = crate::store::get_int(&row, 0)? as usize;
            s.open_deals = crate::store::get_opt_int(&row, 1)?.unwrap_or(0) as usize;
            s.closed_won = crate::store::get_opt_int(&row, 2)?.unwrap_or(0) as usize;
        }

        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM interactions WHERE owner_id = ?",
                params![owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        if let Some(row) = rows.next().await.map_err(StoreError::from)? {
            s.interactions = crate::store::get_int(&row, 0)? as usize;
        }

        Ok(s)
    }
}

// ── FTS helpers ──────────────────────────────────────────────────────────────

impl LibSqlStore {
    /// Insert a row into the CRM FTS index (contacts/companies/deals share one
    /// FTS5 table keyed by entity type).
    pub(crate) async fn fts_upsert(
        &self,
        conn: &libsql::Connection,
        owner_id: &str,
        entity: &str,
        id: &str,
        label: &str,
        snippet: &str,
    ) -> Result<()> {
        conn.execute("DELETE FROM crm_fts WHERE id = ?", params![id])
            .await
            .map_err(StoreError::from)?;
        conn.execute(
            "INSERT INTO crm_fts (owner_id, entity, id, label, snippet) VALUES (?, ?, ?, ?, ?)",
            params![owner_id, entity, id, label, snippet],
        )
        .await
        .map_err(StoreError::from)?;
        Ok(())
    }
}
