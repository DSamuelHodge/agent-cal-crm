//! JSON-RPC-flavoured dispatch over `AgentCal` + `AgentCrm`.
//!
//! This is the *transport-agnostic* RPC layer: it maps `{method, params}`
//! onto the two façades and returns a serialisable `Value`. The HTTP server
//! that carries these calls lives in the `cos` binary (`src/bin/cos.rs`), so
//! the library itself stays free of any network dependency — consistent with
//! the crate's "no HTTP in the library" stance.
//!
//! Every method takes an `owner` param (usually `"derrick"`). Responses are
//! plain `Result<Value>`; the caller wraps them in `{ok, result}` / error.
//!
//! # Methods
//! - `ping` → `{"pong": true}`
//! - `crm.summary`, `crm.resolve_by_phone`, `crm.resolve_by_email`
//! - `crm.contact_context`, `crm.search`, `crm.vector_search`
//! - `crm.create_company`, `crm.get_company`, `crm.list_companies`
//! - `crm.create_contact`, `crm.get_contact`, `crm.list_contacts`
//! - `crm.create_deal`, `crm.get_deal`, `crm.list_deals`, `crm.list_deals_for_company`, `crm.advance_deal`
//! - `crm.log_interaction`, `crm.interactions_for_contact`
//! - `crm.attendee_for_contact`, `crm.contact_for_booking`
//! - `cal.create_calendar_simple`, `cal.add_window`, `cal.block`
//! - `cal.create_link`, `cal.get_slots`, `cal.book`
//! - `cal.get_booking`, `cal.list_bookings`, `cal.upcoming`, `cal.cancel`, `cal.summary`
//! - `action_log.list`, `action_log.query`
//!   (`cal.cancel` is approval-gated: it needs a valid `approval_id` param or
//!   returns `ApprovalRequired`; see `crate::approvals`)
//! - `approval.request`, `approval.approve`, `approval.reject`, `approval.list`

use crate::agent_api::{AgentCal, LinkParams};
use crate::crm::agent::AgentCrm;
use crate::crm::types::{DealStage, InteractionDirection, InteractionKind};
use crate::crm::{InteractionInput, SearchHit};
use crate::error::{AgentError, Result};
use crate::parse_iso;
use crate::scheduler::Booked;
use crate::types::{Attendee, TimeSlot};

/// Dispatch one RPC call against the shared façades.
///
/// `cal` and `crm` share the same store, so a call here sees the whole
/// CoS operating picture (calendar + CRM) on one file.
///
/// Every call is appended to the owner-scoped action log (actor `rpc`) with
/// its redacted params and result code. Logging is best-effort: a logging
/// failure never fails the call itself.
pub async fn dispatch(
    cal: &AgentCal,
    crm: &AgentCrm,
    method: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value> {
    let outcome = dispatch_inner(cal, crm, method, params).await;
    let result_code = match &outcome {
        Ok(_) => "ok",
        Err(e) => crate::actions::error_code(e),
    };
    let owner_id = params
        .get("owner")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let _ = crate::actions::record_action(
        cal.action_store(),
        owner_id,
        crate::actions::ActionActor::Rpc,
        method,
        params,
        result_code,
    )
    .await;
    outcome
}

/// The actual method table. `dispatch` wraps this with action logging.
async fn dispatch_inner(
    cal: &AgentCal,
    crm: &AgentCrm,
    method: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value> {
    let p = params;
    match method {
        "ping" => Ok(serde_json::json!({"pong": true})),

        // ── CRM: query ──────────────────────────────────────────────────
        "crm.summary" => Ok(serde_json::to_value(crm.summary(owner(p)?).await?)?),
        "crm.resolve_by_phone" => {
            let contact = crm
                .resolve_by_phone(owner(p)?, str_param(p, "phone")?)
                .await?;
            match contact {
                Some(c) => Ok(serde_json::to_value(c)?),
                None => Err(AgentError::ContactNotFound("no match".into())),
            }
        }
        "crm.resolve_by_email" => {
            let contact = crm
                .resolve_by_email(owner(p)?, str_param(p, "email")?)
                .await?;
            match contact {
                Some(c) => Ok(serde_json::to_value(c)?),
                None => Err(AgentError::ContactNotFound("no match".into())),
            }
        }
        "crm.contact_context" => {
            let cid = str_param(p, "contact_id")?;
            Ok(crm.contact_context(owner(p)?, cid).await?)
        }
        "crm.search" => {
            let hits: Vec<SearchHit> = crm
                .search(
                    owner(p)?,
                    str_param(p, "query")?,
                    int_param(p, "limit").unwrap_or(10),
                )
                .await?;
            Ok(serde_json::to_value(hits)?)
        }
        "crm.vector_search" => {
            let hits: Vec<SearchHit> = crm
                .vector_search(
                    owner(p)?,
                    str_param(p, "query")?,
                    int_param(p, "limit").unwrap_or(10),
                )
                .await?;
            Ok(serde_json::to_value(hits)?)
        }

        // ── CRM: companies ──────────────────────────────────────────────
        "crm.create_company" => {
            let co = crm
                .create_company(
                    owner(p)?,
                    str_param(p, "name")?,
                    str_param(p, "industry").unwrap_or(""),
                )
                .await?;
            Ok(serde_json::to_value(co)?)
        }
        "crm.get_company" => {
            let cid = str_param(p, "company_id")?;
            Ok(serde_json::to_value(
                crm.get_company(owner(p)?, cid).await?,
            )?)
        }
        "crm.list_companies" => Ok(serde_json::to_value(crm.list_companies(owner(p)?).await?)?),

        // ── CRM: contacts ───────────────────────────────────────────────
        "crm.create_contact" => {
            let contact = crm
                .create_contact(
                    owner(p)?,
                    str_param(p, "first_name")?,
                    str_param(p, "last_name")?,
                )
                .await?;
            Ok(serde_json::to_value(contact)?)
        }
        "crm.get_contact" => {
            let cid = str_param(p, "contact_id")?;
            Ok(serde_json::to_value(
                crm.get_contact(owner(p)?, cid).await?,
            )?)
        }
        "crm.update_contact" => {
            let contact: crate::crm::types::Contact = serde_json::from_value(p["contact"].clone())?;
            Ok(serde_json::to_value(crm.update_contact(&contact).await?)?)
        }
        "crm.list_contacts" => Ok(serde_json::to_value(crm.list_contacts(owner(p)?).await?)?),

        // ── CRM: deals ──────────────────────────────────────────────────
        "crm.create_deal" => {
            let d = crm
                .create_deal(
                    owner(p)?,
                    str_param(p, "company_id")?,
                    str_param(p, "name")?,
                    f64_param(p, "amount").unwrap_or(0.0),
                )
                .await?;
            Ok(serde_json::to_value(d)?)
        }
        "crm.get_deal" => Ok(serde_json::to_value(
            crm.get_deal(owner(p)?, str_param(p, "deal_id")?).await?,
        )?),
        "crm.list_deals" => Ok(serde_json::to_value(crm.list_deals(owner(p)?).await?)?),
        "crm.list_deals_for_company" => Ok(serde_json::to_value(
            crm.list_deals_for_company(owner(p)?, str_param(p, "company_id")?)
                .await?,
        )?),
        "crm.advance_deal" => {
            let stage: DealStage = match str_param(p, "stage")? {
                "LEAD" => DealStage::Lead,
                "QUALIFIED" => DealStage::Qualified,
                "PROPOSAL" => DealStage::Proposal,
                "NEGOTIATION" => DealStage::Negotiation,
                "CLOSED_WON" => DealStage::ClosedWon,
                "CLOSED_LOST" => DealStage::ClosedLost,
                other => {
                    return Err(AgentError::CrmValidation(format!(
                        "unknown deal stage: {other}"
                    )))
                }
            };
            let d = crm
                .advance_deal(owner(p)?, str_param(p, "deal_id")?, stage)
                .await?;
            Ok(serde_json::to_value(d)?)
        }

        // ── CRM: interactions ───────────────────────────────────────────
        "crm.log_interaction" => {
            let kind = match str_param(p, "kind")
                .unwrap_or("SMS")
                .to_uppercase()
                .as_str()
            {
                "CALL" => InteractionKind::Call,
                "SMS" => InteractionKind::Sms,
                "EMAIL" => InteractionKind::Email,
                "MEETING" => InteractionKind::Meeting,
                "MESSAGE" => InteractionKind::Message,
                _ => InteractionKind::Note,
            };
            let direction = match str_param(p, "direction")
                .unwrap_or("OUTBOUND")
                .to_uppercase()
                .as_str()
            {
                "INBOUND" => InteractionDirection::Inbound,
                _ => InteractionDirection::Outbound,
            };
            let mut input = InteractionInput::new(str_param(p, "contact_id")?, kind);
            input = input.with_direction(direction);
            if let Some(summary) = p.get("summary").and_then(|v| v.as_str()) {
                input = input.with_summary(summary);
            }
            if let Some(deal_id) = p.get("deal_id").and_then(|v| v.as_str()) {
                input = input.with_deal(deal_id);
            }
            let i = crm.log_interaction(owner(p)?, input).await?;
            Ok(serde_json::to_value(i)?)
        }
        "crm.interactions_for_contact" => Ok(serde_json::to_value(
            crm.interactions_for_contact(owner(p)?, str_param(p, "contact_id")?)
                .await?,
        )?),

        // ── CRM ⇄ calendar linkage ──────────────────────────────────────
        "crm.attendee_for_contact" => Ok(serde_json::to_value(
            crm.attendee_for_contact(owner(p)?, str_param(p, "contact_id")?)
                .await?,
        )?),
        "crm.contact_for_booking" => {
            let booking: crate::types::Booking = serde_json::from_value(p["booking"].clone())?;
            Ok(serde_json::to_value(
                crm.contact_for_booking(owner(p)?, &booking).await?,
            )?)
        }

        // ── Cal: setup ──────────────────────────────────────────────────
        "cal.create_calendar_simple" => {
            let cal_obj = cal
                .create_calendar_simple(owner(p)?, str_param(p, "name").unwrap_or("CoS"))
                .await?;
            Ok(serde_json::to_value(cal_obj)?)
        }
        "cal.add_window" => {
            let w = cal
                .add_window(
                    owner(p)?,
                    p.get("day_of_week")
                        .and_then(|v| v.as_u64())
                        .map(|v| v as u8),
                    str_param(p, "start")?,
                    str_param(p, "end")?,
                    str_param(p, "label").unwrap_or(""),
                )
                .await?;
            Ok(serde_json::to_value(w)?)
        }
        "cal.block" => {
            let slot = cal
                .block(
                    owner(p)?,
                    parse_iso(str_param(p, "start")?)?,
                    parse_iso(str_param(p, "end")?)?,
                )
                .await?;
            Ok(serde_json::to_value(slot)?)
        }

        // ── Cal: links + slots ──────────────────────────────────────────
        "cal.create_link" => {
            let mut params = LinkParams::new(
                str_param(p, "title")?,
                i64_param(p, "duration_minutes").unwrap_or(30),
            );
            if let Some(n) = f64_param(p, "min_notice_hours") {
                params = params.min_notice_hours(n);
            }
            if let Some(d) = i64_param(p, "max_days_ahead") {
                params = params.max_days_ahead(d);
            }
            let link = cal.create_link(owner(p)?, params).await?;
            Ok(serde_json::to_value(link)?)
        }
        "cal.get_slots" => {
            let slots = cal
                .get_slots(
                    owner(p)?,
                    str_param(p, "link_id")?,
                    p.get("from")
                        .and_then(|v| v.as_str())
                        .and_then(|s| parse_iso(s).ok()),
                    p.get("to")
                        .and_then(|v| v.as_str())
                        .and_then(|s| parse_iso(s).ok()),
                    p.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize),
                )
                .await?;
            Ok(serde_json::to_value(slots)?)
        }
        "cal.book" => {
            let slot = parse_slot(p)?;
            let attendees = parse_attendees(p)?;
            let notes = str_param(p, "notes").unwrap_or("");
            let metadata = p
                .get("metadata")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let booked: Booked = cal
                .book(
                    owner(p)?,
                    str_param(p, "link_id")?,
                    slot,
                    attendees,
                    notes,
                    metadata,
                )
                .await?;
            Ok(serde_json::to_value(booked)?)
        }

        // ── Cal: query ──────────────────────────────────────────────────
        "cal.get_booking" => Ok(serde_json::to_value(
            cal.get_booking(owner(p)?, str_param(p, "booking_id")?)
                .await?,
        )?),
        "cal.list_bookings" => {
            let status = p.get("status").and_then(|v| v.as_str()).and_then(|s| {
                match s.to_uppercase().as_str() {
                    "PENDING" => Some(crate::types::Status::Pending),
                    "CONFIRMED" => Some(crate::types::Status::Confirmed),
                    "CANCELLED" => Some(crate::types::Status::Cancelled),
                    "COMPLETED" => Some(crate::types::Status::Completed),
                    _ => None,
                }
            });
            let bookings = cal
                .list_bookings(
                    owner(p)?,
                    status,
                    p.get("from")
                        .and_then(|v| v.as_str())
                        .and_then(|s| parse_iso(s).ok()),
                    p.get("to")
                        .and_then(|v| v.as_str())
                        .and_then(|s| parse_iso(s).ok()),
                )
                .await?;
            Ok(serde_json::to_value(bookings)?)
        }
        "cal.upcoming" => Ok(serde_json::to_value(
            cal.upcoming(owner(p)?, int_param(p, "limit").unwrap_or(10))
                .await?,
        )?),
        "cal.cancel" => {
            // Destructive lib method: gated behind an approval. Without a valid
            // `approval_id` param this enqueues (or reuses) a pending approval
            // and returns `ApprovalRequired(approval_id)`; the caller approves
            // via `approval.approve` and retries with the same params plus
            // `approval_id`.
            crm.check_send_allowed(
                owner(p)?,
                "cal.cancel",
                p,
                &crate::approvals::ApprovalConfig::from_env(),
            )
            .await?;
            Ok(serde_json::to_value(
                cal.cancel(
                    owner(p)?,
                    str_param(p, "booking_id")?,
                    str_param(p, "reason").unwrap_or(""),
                )
                .await?,
            )?)
        }
        "cal.summary" => Ok(serde_json::to_value(cal.summary(owner(p)?).await?)?),

        // ── Action log ──────────────────────────────────────────────────
        "action_log.list" => {
            let limit = int_param(p, "limit").unwrap_or(50).min(500);
            Ok(serde_json::to_value(
                cal.action_store().list_actions(owner(p)?, limit).await?,
            )?)
        }
        "action_log.query" => {
            let limit = int_param(p, "limit").unwrap_or(50).min(500);
            let filter = p.get("method").and_then(|v| v.as_str());
            Ok(serde_json::to_value(
                cal.action_store()
                    .query_actions(owner(p)?, filter, limit)
                    .await?,
            )?)
        }
        // ── Approvals (Phase 1 safety substrate) ──────────────────────────
        "approval.request" => {
            let method = str_param(p, "method")?;
            let inner = p.get("params").cloned().unwrap_or(serde_json::Value::Null);
            let approval = crm
                .request_approval(
                    owner(p)?,
                    method,
                    &inner,
                    "rpc:approval.request",
                    &crate::approvals::ApprovalConfig::from_env(),
                )
                .await?;
            Ok(serde_json::to_value(approval)?)
        }
        "approval.approve" => {
            let approval = crm
                .approve_approval(
                    owner(p)?,
                    str_param(p, "id")?,
                    "rpc:approval.approve",
                    &crate::approvals::ApprovalConfig::from_env(),
                )
                .await?;
            Ok(serde_json::to_value(approval)?)
        }
        "approval.reject" => {
            let reason = p.get("reason").and_then(|v| v.as_str());
            let approval = crm
                .reject_approval(
                    owner(p)?,
                    str_param(p, "id")?,
                    "rpc:approval.reject",
                    reason,
                    &crate::approvals::ApprovalConfig::from_env(),
                )
                .await?;
            Ok(serde_json::to_value(approval)?)
        }
        "approval.list" => {
            let state = p.get("state").and_then(|v| v.as_str());
            let approvals = crm
                .list_approvals(
                    owner(p)?,
                    state,
                    &crate::approvals::ApprovalConfig::from_env(),
                )
                .await?;
            Ok(serde_json::to_value(approvals)?)
        }

        other => Err(AgentError::Validation(format!(
            "unknown RPC method: {other}"
        ))),
    }
}

// ── param helpers ─────────────────────────────────────────────────────────────

fn owner(p: &serde_json::Value) -> Result<&str> {
    str_param(p, "owner")
}

fn str_param<'a>(p: &'a serde_json::Value, key: &str) -> Result<&'a str> {
    p.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| AgentError::Validation(format!("missing string param: {key}")))
}

fn i64_param(p: &serde_json::Value, key: &str) -> Option<i64> {
    p.get(key).and_then(|v| v.as_i64())
}

fn f64_param(p: &serde_json::Value, key: &str) -> Option<f64> {
    p.get(key).and_then(|v| v.as_f64())
}

fn int_param(p: &serde_json::Value, key: &str) -> Option<usize> {
    p.get(key)
        .and_then(|v| v.as_i64())
        .map(|v| v.max(0) as usize)
}

fn parse_slot(p: &serde_json::Value) -> Result<TimeSlot> {
    let slot = p
        .get("slot")
        .ok_or_else(|| AgentError::Validation("missing param: slot".into()))?;
    let start = parse_iso(
        slot.get("start")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentError::Validation("slot.start missing".into()))?,
    )?;
    let end = parse_iso(
        slot.get("end")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentError::Validation("slot.end missing".into()))?,
    )?;
    TimeSlot::new(start, end).map_err(|_| AgentError::Validation("invalid slot".into()))
}

/// Parse attendees from `{name, email, ...}` (id filled if absent).
fn parse_attendees(p: &serde_json::Value) -> Result<Vec<Attendee>> {
    let mut out = Vec::new();
    let arr = match p.get("attendees") {
        None => return Ok(out),
        Some(v) => v
            .as_array()
            .ok_or_else(|| AgentError::Validation("attendees must be an array".into()))?,
    };
    for a in arr {
        let name = a
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentError::Validation("attendee.name missing".into()))?;
        let email = a
            .get("email")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentError::Validation("attendee.email missing".into()))?;
        let mut attendee = Attendee::new(name, email);
        if let Some(m) = a.get("metadata") {
            attendee = attendee.with_metadata(m.clone());
        }
        out.push(attendee);
    }
    Ok(out)
}
