//! Pending-approval gate (ROADMAP Phase 1, item 3).
//!
//! Nothing the daemon can *send* on your behalf happens without a record
//! and, for high-risk actions, explicit approval. This module is the
//! transport-free core of that gate: risk tiers, the [`PendingApproval`]
//! record, the approve/reject/list business logic, and the enforcement helper
//! used by both `dispatch` (`src/rpc.rs`) and the `aware.*` send paths
//! (`src/bin/aware.rs`).
//!
//! # Risk tiers
//!
//! | Tier | Meaning | Methods |
//! |---|---|---|
//! | `read` | Auto-approve (no side effects beyond inbound logging) | `ping`, `approval.*`, `crm.summary`, `crm.resolve_by_phone`, `crm.resolve_by_email`, `crm.contact_context`, `crm.search`, `crm.vector_search`, `crm.get_company`, `crm.list_companies`, `crm.get_contact`, `crm.list_contacts`, `crm.get_deal`, `crm.list_deals`, `crm.list_deals_for_company`, `crm.interactions_for_contact`, `crm.attendee_for_contact`, `crm.contact_for_booking`, `cal.get_booking`, `cal.list_bookings`, `cal.upcoming`, `cal.summary`, `cal.get_slots`, `aware.sms`, `aware.whatsapp`, `aware.call` (inbound triage), `aware.search`, `aware.travel`, `aware.meeting`, `aware.briefing`, `aware.deals` |
//! | `low` | Auto-approve (low-risk local writes) | `crm.create_company`, `crm.create_contact`, `crm.update_contact`, `crm.create_deal`, `crm.advance_deal`, `crm.log_interaction`, `cal.create_calendar_simple`, `cal.add_window`, `cal.block`, `cal.unblock_all`, `cal.clear_windows`, `cal.create_link`, `cal.book`, `cal.confirm`, `cal.complete`, `cal.reschedule`, `cal.add_attendee`, `cal.remove_attendee`, `aware.capture` |
//! | `high` | Require approval (sends, deletes, external side-effects; also the default for unknown methods) | `aware.sms.send`, `aware.whatsapp.send`, `aware.email`, `aware.open`, `aware.sync_contacts`, `sync.logseq`, `cal.cancel`, `cal.delete_calendar`, `crm.delete_company`, `crm.delete_contact`, `crm.delete_deal`, anything unlisted |
//!
//! Fully-gated mode ([`ApprovalConfig::fully_gated`], env `COS_FULLY_GATED`)
//! widens the gate: every non-read (all `low` writes *and* `high` actions)
//! waits for approval. Reads still auto-approve.
//!
//! # Enforcement points
//!
//! Tiering alone does not block anything — each execution path must call
//! [`AgentCrm::check_send_allowed`] before performing a side effect:
//!
//! - `dispatch` enforces [`LIB_ENFORCED_METHODS`] (currently `cal.cancel`);
//!   without a valid `approval_id` param it enqueues a pending approval and
//!   returns [`AgentError::ApprovalRequired`].
//! - the `aware.*` send paths (`aware.sms.send`, `aware.whatsapp.send`,
//!   `aware.email`, `aware.open`) enforce the same helper in `src/bin/aware.rs`.
//!
//! The caller flow is therefore: call the method → receive
//! `ApprovalRequired(<id>)` → `approval.approve` → re-call the method with
//! the identical params plus `approval_id` → it executes.
//!
//! # Action-log merge interface
//!
//! A sibling change is building the canonical action log (`src/actions.rs`
//! with `record_action`). Until it lands, this module provides a minimal
//! compatible hook with the **agreed shape**
//! `ActionLogEntry { id, owner_id, actor, method, params_json, result, at_ms }`.
//! Every approval decision (request / auto-approve / approve / reject) is
//! appended via [`record_action`]. When `src/actions.rs` lands, it should
//! re-export or replace this hook — the field names and their meanings are
//! the contract (see `result`/`actor` notes on [`ActionLogEntry`]).

use std::sync::{Arc, Mutex};

use chrono::Utc;

use crate::crm::agent::AgentCrm;
use crate::crm::store::CrmStore;
use crate::error::{AgentError, Result};

/// Default time-to-live for a pending approval: 24 h in milliseconds.
pub const DEFAULT_PENDING_TTL_MS: i64 = 24 * 60 * 60 * 1000;

/// Lib-level methods whose execution `dispatch` gates behind an approval.
/// (Bin-level `aware.*` sends are gated separately in `src/bin/aware.rs`.)
pub const LIB_ENFORCED_METHODS: &[&str] = &["cal.cancel"];

/// Risk tier of an RPC method. See the module-level tier table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskTier {
    /// Reads (and the `approval.*` mechanism itself): always auto-approve.
    Read,
    /// Low-risk local writes: auto-approve unless fully-gated.
    LowRiskWrite,
    /// Sends, deletes, external side-effects, and anything unknown: approval.
    HighRisk,
}

impl RiskTier {
    pub fn as_str(self) -> &'static str {
        match self {
            RiskTier::Read => "read",
            RiskTier::LowRiskWrite => "low",
            RiskTier::HighRisk => "high",
        }
    }
}

/// Explicit risk-tier table: reads and low-risk writes are listed; sends,
/// deletes, external side-effects, and unknown methods fall through to
/// [`RiskTier::HighRisk`] (safe default).
pub fn risk_tier(method: &str) -> RiskTier {
    match method {
        // ── Reads: auto-approve ──────────────────────────────────────────
        "ping" | "approval.request" | "approval.approve" | "approval.reject" | "approval.list"
        | "crm.summary"
        | "crm.resolve_by_phone"
        | "crm.resolve_by_email"
        | "crm.contact_context"
        | "crm.search"
        | "crm.vector_search"
        | "crm.get_company"
        | "crm.list_companies"
        | "crm.get_contact"
        | "crm.list_contacts"
        | "crm.get_deal"
        | "crm.list_deals"
        | "crm.list_deals_for_company"
        | "crm.interactions_for_contact"
        | "crm.attendee_for_contact"
        | "crm.contact_for_booking"
        | "cal.get_booking"
        | "cal.list_bookings"
        | "cal.upcoming"
        | "cal.summary"
        | "cal.get_slots"
        // Inbound triage / read-only awareness (notify + inbound log only).
        | "aware.sms"
        | "aware.whatsapp"
        | "aware.call"
        | "aware.search"
        | "aware.travel"
        | "aware.meeting"
        | "aware.briefing"
        | "aware.deals" => RiskTier::Read,

        // ── Low-risk writes: auto-approve unless fully-gated ─────────────
        "crm.create_company"
        | "crm.create_contact"
        | "crm.update_contact"
        | "crm.create_deal"
        | "crm.advance_deal"
        | "crm.log_interaction"
        | "cal.create_calendar_simple"
        | "cal.add_window"
        | "cal.clear_windows"
        | "cal.block"
        | "cal.unblock_all"
        | "cal.create_link"
        | "cal.book"
        | "cal.confirm"
        | "cal.complete"
        | "cal.reschedule"
        | "cal.add_attendee"
        | "cal.remove_attendee"
        | "aware.capture" => RiskTier::LowRiskWrite,

        // ── Sends, deletes, external side-effects, unknown: approval ─────
        _ => RiskTier::HighRisk,
    }
}

/// Lifecycle state of a [`PendingApproval`]. Stored lowercase in SQLite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalState {
    Pending,
    Approved,
    Rejected,
    Expired,
}

impl ApprovalState {
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalState::Pending => "pending",
            ApprovalState::Approved => "approved",
            ApprovalState::Rejected => "rejected",
            ApprovalState::Expired => "expired",
        }
    }

    pub fn parse(s: &str) -> Option<ApprovalState> {
        match s {
            "pending" => Some(ApprovalState::Pending),
            "approved" => Some(ApprovalState::Approved),
            "rejected" => Some(ApprovalState::Rejected),
            "expired" => Some(ApprovalState::Expired),
            _ => None,
        }
    }
}

/// One row of the `pending_approvals` table: a request to perform `method`
/// with `params_json` on behalf of `owner_id`, awaiting (or having received)
/// a human decision.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PendingApproval {
    pub id: String,
    pub owner_id: String,
    pub method: String,
    pub params_json: serde_json::Value,
    pub risk_tier: String,
    pub state: String,
    pub created_at_ms: i64,
    pub decided_at_ms: Option<i64>,
}

impl PendingApproval {
    pub fn new(
        owner_id: &str,
        method: &str,
        params_json: serde_json::Value,
        tier: RiskTier,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            owner_id: owner_id.to_string(),
            method: method.to_string(),
            params_json,
            risk_tier: tier.as_str().to_string(),
            state: ApprovalState::Pending.as_str().to_string(),
            created_at_ms: now_ms(),
            decided_at_ms: None,
        }
    }

    pub fn state_enum(&self) -> ApprovalState {
        ApprovalState::parse(&self.state).unwrap_or(ApprovalState::Pending)
    }

    pub fn is_stale(&self, now_ms: i64, ttl_ms: i64) -> bool {
        self.state_enum() == ApprovalState::Pending && now_ms.saturating_sub(self.created_at_ms) > ttl_ms
    }
}

/// Gate configuration.
#[derive(Debug, Clone)]
pub struct ApprovalConfig {
    /// When true, every non-read (all writes *and* sends) waits for approval.
    /// Tiered mode (false): only high-risk actions wait.
    pub fully_gated: bool,
    /// Milliseconds a pending approval stays valid before lazy expiry.
    pub pending_ttl_ms: i64,
}

impl Default for ApprovalConfig {
    fn default() -> Self {
        Self {
            fully_gated: false,
            pending_ttl_ms: DEFAULT_PENDING_TTL_MS,
        }
    }
}

impl ApprovalConfig {
    /// Daemon configuration from the environment:
    /// `COS_FULLY_GATED=1|true` and `COS_APPROVAL_TTL_MS=<millis>`.
    pub fn from_env() -> Self {
        let fully_gated = std::env::var("COS_FULLY_GATED")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let pending_ttl_ms = std::env::var("COS_APPROVAL_TTL_MS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_PENDING_TTL_MS);
        Self {
            fully_gated,
            pending_ttl_ms,
        }
    }
}

/// Does `method` require an approval under `config`?
pub fn requires_approval(method: &str, config: &ApprovalConfig) -> bool {
    match risk_tier(method) {
        RiskTier::HighRisk => true,
        RiskTier::LowRiskWrite => config.fully_gated,
        RiskTier::Read => false,
    }
}

/// Is this lib-level method execution-gated inside `dispatch`?
pub fn is_lib_enforced(method: &str) -> bool {
    LIB_ENFORCED_METHODS.contains(&method)
}

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Canonical fingerprint of the params an approval covers: the params object
/// minus the `approval_id` bearer field, serialised canonically
/// (`serde_json::Map` is a `BTreeMap`, so key order is stable).
pub fn params_fingerprint(params: &serde_json::Value) -> String {
    let mut v = params.clone();
    if let Some(obj) = v.as_object_mut() {
        obj.remove("approval_id");
    }
    serde_json::to_string(&v).unwrap_or_default()
}

// ── Action-log hook (merge interface for the sibling action_log change) ──────

/// Agreed cross-PR shape for one logged action.
///
/// Contract assumptions for the coordinator to reconcile with `src/actions.rs`:
/// - `params_json` is the full params object (minus nothing).
/// - `result` is a short human-readable outcome string: the new approval
///   state (`"pending"`, `"approved"`, `"rejected"`, `"expired"`), with an
///   optional `": {detail}"` suffix (e.g. the reject reason).
/// - `actor` names the entry point: `"rpc:approval.request"`,
///   `"rpc:approval.approve"`, `"rpc:approval.reject"`, or
///   `"gate:{method}"` for pendings auto-created by enforcement.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActionLogEntry {
    pub id: String,
    pub owner_id: String,
    pub actor: String,
    pub method: String,
    pub params_json: serde_json::Value,
    pub result: String,
    pub at_ms: i64,
}

impl ActionLogEntry {
    pub fn new(
        owner_id: &str,
        actor: &str,
        method: &str,
        params_json: serde_json::Value,
        result: &str,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            owner_id: owner_id.to_string(),
            actor: actor.to_string(),
            method: method.to_string(),
            params_json,
            result: result.to_string(),
            at_ms: now_ms(),
        }
    }
}

static ACTION_LOG: Mutex<Vec<ActionLogEntry>> = Mutex::new(Vec::new());

/// Minimal local append hook. The sibling `src/actions.rs::record_action`
/// (persistent action log) should re-export or replace this function; until
/// then decisions are buffered in-process and mirrored to stderr so the
/// daemon record exists even without the persistent facility.
pub fn record_action(entry: ActionLogEntry) {
    eprintln!(
        "[action] {} owner={} actor={} method={} result={}",
        entry.at_ms, entry.owner_id, entry.actor, entry.method, entry.result
    );
    if let Ok(mut log) = ACTION_LOG.lock() {
        log.push(entry);
    }
}

/// Snapshot the in-process action-log buffer (tests / debugging).
pub fn action_log_snapshot() -> Vec<ActionLogEntry> {
    ACTION_LOG.lock().map(|l| l.clone()).unwrap_or_default()
}

/// Clear the in-process action-log buffer (tests).
pub fn action_log_clear() {
    if let Ok(mut log) = ACTION_LOG.lock() {
        log.clear();
    }
}

fn log_decision(
    owner_id: &str,
    actor: &str,
    rpc_method: &str,
    target_method: &str,
    params_json: serde_json::Value,
    result: &str,
) {
    record_action(ActionLogEntry::new(
        owner_id,
        actor,
        &format!("{rpc_method}({target_method})"),
        params_json,
        result,
    ));
}

// ── Business logic (lives here so `crm/agent.rs` stays untouched) ────────────

impl AgentCrm {
    fn approval_store(&self) -> &Arc<dyn CrmStore> {
        self.crm_store()
    }

    /// Enqueue an approval for `method` + `params`.
    ///
    /// Low-risk methods auto-approve immediately (state `approved`); the rest
    /// stay `pending`. Reuses an identical still-pending approval instead of
    /// creating duplicates. Returns the approval (its `id` is the bearer the
    /// caller passes back as `approval_id`).
    pub async fn request_approval(
        &self,
        owner_id: &str,
        method: &str,
        params: &serde_json::Value,
        actor: &str,
        config: &ApprovalConfig,
    ) -> Result<PendingApproval> {
        self.sweep_expired_approvals(owner_id, now_ms(), config.pending_ttl_ms)
            .await?;
        let tier = risk_tier(method);
        let covered = {
            let mut v = params.clone();
            if let Some(obj) = v.as_object_mut() {
                obj.remove("approval_id");
            }
            v
        };

        // De-duplicate: an identical still-pending approval is reused.
        let want_fp = params_fingerprint(params);
        let existing = self
            .approval_store()
            .list_approvals(owner_id, Some(ApprovalState::Pending.as_str()))
            .await?;
        if let Some(hit) = existing
            .into_iter()
            .find(|a| a.method == method && params_fingerprint(&a.params_json) == want_fp)
        {
            return Ok(hit);
        }

        let mut approval = PendingApproval::new(owner_id, method, covered.clone(), tier);
        if !requires_approval(method, config) {
            approval.state = ApprovalState::Approved.as_str().to_string();
            approval.decided_at_ms = Some(now_ms());
        }
        self.approval_store().save_approval(&approval).await?;
        log_decision(
            owner_id,
            actor,
            "approval.request",
            method,
            covered,
            &approval.state.clone(),
        );
        Ok(approval)
    }

    /// Approve a pending approval. Records the decision in the action log and
    /// returns the approval (with `method` + `params_json`) so the caller can
    /// execute the gated action.
    pub async fn approve_approval(
        &self,
        owner_id: &str,
        id: &str,
        actor: &str,
        config: &ApprovalConfig,
    ) -> Result<PendingApproval> {
        self.sweep_expired_approvals(owner_id, now_ms(), config.pending_ttl_ms)
            .await?;
        let mut approval = self
            .approval_store()
            .load_approval(owner_id, id)
            .await?
            .ok_or_else(|| AgentError::ApprovalNotFound(id.to_string()))?;
        if approval.state_enum() != ApprovalState::Pending {
            return Err(AgentError::ApprovalNotApproved(format!(
                "{id} is {}",
                approval.state
            )));
        }
        approval.state = ApprovalState::Approved.as_str().to_string();
        approval.decided_at_ms = Some(now_ms());
        self.approval_store().save_approval(&approval).await?;
        log_decision(
            owner_id,
            actor,
            "approval.approve",
            &approval.method.clone(),
            approval.params_json.clone(),
            ApprovalState::Approved.as_str(),
        );
        Ok(approval)
    }

    /// Reject a pending approval. `reason` is recorded in the action log only
    /// (the `pending_approvals` row carries no reason column, per schema).
    pub async fn reject_approval(
        &self,
        owner_id: &str,
        id: &str,
        actor: &str,
        reason: Option<&str>,
        config: &ApprovalConfig,
    ) -> Result<PendingApproval> {
        self.sweep_expired_approvals(owner_id, now_ms(), config.pending_ttl_ms)
            .await?;
        let mut approval = self
            .approval_store()
            .load_approval(owner_id, id)
            .await?
            .ok_or_else(|| AgentError::ApprovalNotFound(id.to_string()))?;
        if approval.state_enum() != ApprovalState::Pending {
            return Err(AgentError::ApprovalNotApproved(format!(
                "{id} is {}",
                approval.state
            )));
        }
        approval.state = ApprovalState::Rejected.as_str().to_string();
        approval.decided_at_ms = Some(now_ms());
        self.approval_store().save_approval(&approval).await?;
        let result = match reason {
            Some(r) if !r.is_empty() => format!("rejected: {r}"),
            _ => ApprovalState::Rejected.as_str().to_string(),
        };
        log_decision(
            owner_id,
            actor,
            "approval.reject",
            &approval.method.clone(),
            approval.params_json.clone(),
            &result,
        );
        Ok(approval)
    }

    /// List approvals for `owner_id`, optionally filtered by state.
    /// Runs a lazy expiry sweep first so stale pendings read as `expired`.
    pub async fn list_approvals(
        &self,
        owner_id: &str,
        state: Option<&str>,
        config: &ApprovalConfig,
    ) -> Result<Vec<PendingApproval>> {
        self.sweep_expired_approvals(owner_id, now_ms(), config.pending_ttl_ms)
            .await?;
        if let Some(s) = state {
            if ApprovalState::parse(s).is_none() {
                return Err(AgentError::Validation(format!(
                    "unknown approval state: {s}"
                )));
            }
        }
        self.approval_store().list_approvals(owner_id, state).await
    }

    /// Mark stale `pending` rows `expired`. Returns the number expired.
    pub async fn sweep_expired_approvals(
        &self,
        owner_id: &str,
        now: i64,
        ttl_ms: i64,
    ) -> Result<usize> {
        self.approval_store()
            .expire_stale_approvals(owner_id, now, ttl_ms)
            .await
    }

    /// Enforcement helper for execution paths (lib `dispatch` + bin sends).
    ///
    /// - Methods that need no approval under `config` pass silently.
    /// - Otherwise `params` must carry a valid `approval_id` covering the
    ///   same owner + method + params (minus `approval_id` itself); on success
    ///   returns `Ok(())` and the caller executes.
    /// - Without (or with an invalid) `approval_id`, enqueues (or reuses) a
    ///   pending approval and returns [`AgentError::ApprovalRequired`] whose
    ///   payload is the approval id.
    pub async fn check_send_allowed(
        &self,
        owner_id: &str,
        method: &str,
        params: &serde_json::Value,
        config: &ApprovalConfig,
    ) -> Result<()> {
        if !requires_approval(method, config) {
            return Ok(());
        }
        match params.get("approval_id").and_then(|v| v.as_str()) {
            None => {
                let pending = self
                    .request_approval(owner_id, method, params, &format!("gate:{method}"), config)
                    .await?;
                Err(AgentError::ApprovalRequired(pending.id))
            }
            Some(id) => {
                let approval = self
                    .approval_store()
                    .load_approval(owner_id, id)
                    .await?
                    .ok_or_else(|| AgentError::ApprovalNotFound(id.to_string()))?;
                if approval.is_stale(now_ms(), config.pending_ttl_ms) {
                    let mut expired = approval.clone();
                    expired.state = ApprovalState::Expired.as_str().to_string();
                    expired.decided_at_ms = Some(now_ms());
                    self.approval_store().save_approval(&expired).await?;
                    return Err(AgentError::ApprovalNotApproved(format!(
                        "{id} is {}",
                        expired.state
                    )));
                }
                if approval.state_enum() != ApprovalState::Approved {
                    return Err(AgentError::ApprovalNotApproved(format!(
                        "{id} is {}",
                        approval.state
                    )));
                }
                if approval.method != method {
                    return Err(AgentError::Validation(format!(
                        "approval {id} was issued for method {}, not {method}",
                        approval.method
                    )));
                }
                if params_fingerprint(&approval.params_json) != params_fingerprint(params) {
                    return Err(AgentError::Validation(format!(
                        "approval {id} does not cover these params"
                    )));
                }
                Ok(())
            }
        }
    }
}
