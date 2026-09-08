//! Append-only, owner-scoped record of every issued action.
//!
//! This answers "what did the daemon *do*?" — who asked (a rule, the LLM, or
//! an RPC caller), which method ran, with what (redacted) params, and whether
//! it succeeded. It is deliberately distinct from data-provenance bookkeeping:
//! one row is appended per issued action and rows are never updated.
//!
//! # Shared contract
//!
//! [`record_action`] is the single entry point. Sibling workstreams
//! (approvals, inbox, …) call it with whatever store they hold:
//!
//! ```no_run
//! use agentcal::{record_action, ActionActor, LibSqlStore};
//!
//! # async fn demo(store: &LibSqlStore) -> agentcal::Result<()> {
//! record_action(
//!     store,
//!     "derrick",
//!     ActionActor::Rule,
//!     "aware.sms.send",
//!     &serde_json::json!({"to": "+16142600424"}),
//!     "ok",
//! )
//! .await?;
//! # Ok(())
//! # }
//! ```
//!
//! `result` is `"ok"` on success or a machine-readable error code
//! ([`error_code`]) on failure. Secret-looking param values are redacted
//! before they ever reach the store (see [`redact_params`]).

use async_trait::async_trait;

use crate::error::{AgentError, Result};

/// Who asked for the action. Serialised lowercase (`"rule"`, `"llm"`, `"rpc"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionActor {
    Rule,
    Llm,
    Rpc,
}

impl ActionActor {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionActor::Rule => "rule",
            ActionActor::Llm => "llm",
            ActionActor::Rpc => "rpc",
        }
    }

    /// Parse a stored actor string; unknown values fall back to `Rpc` so old
    /// rows stay readable if the set ever grows.
    pub fn parse(s: &str) -> Self {
        match s {
            "rule" => ActionActor::Rule,
            "llm" => ActionActor::Llm,
            _ => ActionActor::Rpc,
        }
    }
}

impl std::fmt::Display for ActionActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One row of the `action_log` table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ActionLogEntry {
    pub id: String,
    pub owner_id: String,
    pub actor: ActionActor,
    pub method: String,
    /// JSON-encoded params with secret-looking values replaced (see
    /// [`redact_params`]).
    pub params_json: String,
    /// `"ok"` on success, otherwise the [`error_code`] string.
    pub result: String,
    /// Unix epoch milliseconds.
    pub at_ms: i64,
}

/// Map an [`AgentError`] to a stable machine-readable code for the `result`
/// column (agents branch on this instead of string-matching messages).
pub fn error_code(e: &AgentError) -> &'static str {
    match e {
        AgentError::CalendarNotFound(_) => "calendar_not_found",
        AgentError::CalendarAlreadyExists(_) => "calendar_already_exists",
        AgentError::LinkNotFound(_) => "link_not_found",
        AgentError::BookingNotFound(_) => "booking_not_found",
        AgentError::Conflict(_) => "conflict",
        AgentError::Validation(_) => "validation",
        AgentError::AttendeeExists(_) => "attendee_exists",
        AgentError::BookingFull => "booking_full",
        AgentError::ContactNotFound(_) => "contact_not_found",
        AgentError::CompanyNotFound(_) => "company_not_found",
        AgentError::DealNotFound(_) => "deal_not_found",
        AgentError::CrmValidation(_) => "crm_validation",
        AgentError::ApprovalRequired(_) => "approval_required",
        AgentError::ApprovalNotFound(_) => "approval_not_found",
        AgentError::ApprovalNotApproved(_) => "approval_not_approved",
        AgentError::Store(_) => "store_error",
    }
}

/// Value substituted for secret-looking param values.
pub const REDACTED: &str = "[REDACTED]";

/// Param keys whose values are always redacted (case-insensitive substring).
const SECRET_KEY_PARTS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "api_key",
    "api-key",
    "apikey",
    "access_key",
    "private_key",
    "bearer",
    "authorization",
    "credential",
];

fn is_secret_key(key: &str) -> bool {
    let lowered = key.to_lowercase();
    SECRET_KEY_PARTS
        .iter()
        .any(|part| lowered.contains(part))
}

/// Deep-clone `params` with every secret-looking value replaced by
/// [`REDACTED`]. Walks nested objects and arrays; non-string secrets are
/// replaced too.
pub fn redact_params(params: &serde_json::Value) -> serde_json::Value {
    match params {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| {
                    if is_secret_key(k) {
                        (k.clone(), serde_json::Value::String(REDACTED.into()))
                    } else {
                        (k.clone(), redact_params(v))
                    }
                })
                .collect(),
        ),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(redact_params).collect())
        }
        other => other.clone(),
    }
}

/// Persistence capability for the action log. Implemented by every store
/// (`LibSqlStore`, `MemoryStore`, `NullStore`) so any façade can log.
#[async_trait]
pub trait ActionLogStore: Send + Sync {
    async fn append_action(&self, entry: &ActionLogEntry) -> Result<()>;
    /// Newest entries first, owner-scoped, capped at `limit`.
    async fn list_actions(&self, owner_id: &str, limit: usize) -> Result<Vec<ActionLogEntry>>;
    /// Newest entries first, owner-scoped, optionally filtered to one method.
    async fn query_actions(
        &self,
        owner_id: &str,
        method: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ActionLogEntry>>;
}

/// Append one action-log row: generates the id/timestamp, redacts `params`,
/// and stores the entry. Never mutates existing rows.
///
/// `result` should be `"ok"` on success or an [`error_code`] string.
pub async fn record_action(
    store: &dyn ActionLogStore,
    owner_id: &str,
    actor: ActionActor,
    method: &str,
    params: &serde_json::Value,
    result: &str,
) -> Result<()> {
    let entry = ActionLogEntry {
        id: uuid::Uuid::new_v4().to_string(),
        owner_id: owner_id.to_string(),
        actor,
        method: method.to_string(),
        params_json: serde_json::to_string(&redact_params(params))
            .unwrap_or_else(|_| "{}".to_string()),
        result: result.to_string(),
        at_ms: chrono::Utc::now().timestamp_millis(),
    };
    store.append_action(&entry).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_roundtrip() {
        for actor in [ActionActor::Rule, ActionActor::Llm, ActionActor::Rpc] {
            assert_eq!(ActionActor::parse(actor.as_str()), actor);
        }
        // Forward-compat: unknown strings decode as Rpc.
        assert_eq!(ActionActor::parse("human"), ActionActor::Rpc);
        // Serde form is lowercase.
        assert_eq!(
            serde_json::to_value(ActionActor::Llm).unwrap(),
            serde_json::json!("llm")
        );
    }

    #[test]
    fn redact_top_level_and_nested() {
        let params = serde_json::json!({
            "owner": "derrick",
            "token": "abc123",
            "password": "hunter2",
            "api_key": "key-1",
            "nested": {"client_secret": "shh", "label": "keep me"},
            "list": [{"auth_token": "t"}, {"name": "x"}],
        });
        let redacted = redact_params(&params);
        assert_eq!(redacted["owner"], "derrick");
        assert_eq!(redacted["token"], REDACTED);
        assert_eq!(redacted["password"], REDACTED);
        assert_eq!(redacted["api_key"], REDACTED);
        assert_eq!(redacted["nested"]["client_secret"], REDACTED);
        assert_eq!(redacted["nested"]["label"], "keep me");
        assert_eq!(redacted["list"][0]["auth_token"], REDACTED);
        assert_eq!(redacted["list"][1]["name"], "x");
        // Non-string secrets are replaced too.
        let redacted = redact_params(&serde_json::json!({"token": {"a": 1}}));
        assert_eq!(redacted["token"], REDACTED);
    }

    #[test]
    fn redact_is_case_insensitive() {
        let redacted = redact_params(&serde_json::json!({"AuthToken": "t", "AUTHOR": "keep"}));
        assert_eq!(redacted["AuthToken"], REDACTED);
        assert_eq!(redacted["AUTHOR"], "keep");
    }

    #[test]
    fn error_codes_stable() {
        assert_eq!(
            error_code(&AgentError::BookingNotFound("x".into())),
            "booking_not_found"
        );
        assert_eq!(
            error_code(&AgentError::Validation("x".into())),
            "validation"
        );
        assert_eq!(
            error_code(&AgentError::ContactNotFound("x".into())),
            "contact_not_found"
        );
        assert_eq!(error_code(&AgentError::BookingFull), "booking_full");
    }
}
