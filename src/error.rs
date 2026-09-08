use thiserror::Error;

use serde::Serialize;
use serde_json::Value;

/// Convert a `Result<T, AgentError>` into a JSON dict representation.
/// Useful for compatibility with the Python `{"ok": bool, "data": ..., "reason": "..."}` format.
/// New code should prefer the `Result<T, AgentError>` API directly.
pub fn to_dict<T: Serialize>(result: std::result::Result<T, AgentError>) -> Value {
    match result {
        Ok(data) => serde_json::json!({
            "ok": true,
            "data": data,
        }),
        Err(reason) => serde_json::json!({
            "ok": false,
            "reason": reason.to_string(),
        }),
    }
}

/// Extract the inner data from `Ok`, or return `None` if `Err`.
/// A thin wrapper around [`Result::ok`] that keeps the Python-facing name
/// explicit for agents porting across.
#[allow(clippy::manual_ok_err)]
pub fn unwrap_or_none<T: Serialize>(result: std::result::Result<T, AgentError>) -> Option<T> {
    result.ok()
}

/// Consume the result, returning the data or calling a fallback function on error.
pub fn unwrap_or_else<T: Serialize>(
    result: std::result::Result<T, AgentError>,
    fallback: impl FnOnce(AgentError) -> T,
) -> T {
    match result {
        Ok(data) => data,
        Err(e) => fallback(e),
    }
}

/// Persistence-layer errors (libSQL, serde, etc.).
#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    LibSql(#[from] libsql::Error),

    #[error(transparent)]
    Serde(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}

/// Convenience alias for the public API.
pub type Result<T> = std::result::Result<T, AgentError>;

/// Error type for agentcal. Agents branch on `Result::is_err()` instead of
/// `{"ok": false}` — idiomatic Rust.

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("calendar {0:?} not found")]
    CalendarNotFound(String),

    #[error("calendar {0:?} already exists")]
    CalendarAlreadyExists(String),

    #[error("link {0:?} not found")]
    LinkNotFound(String),

    #[error("booking {0:?} not found")]
    BookingNotFound(String),

    #[error("{0}")]
    Conflict(String),

    #[error("{0}")]
    Validation(String),

    #[error("{0} already on this booking")]
    AttendeeExists(String),

    #[error("booking full — max_attendees reached")]
    BookingFull,

    #[error("contact {0:?} not found")]
    ContactNotFound(String),

    #[error("company {0:?} not found")]
    CompanyNotFound(String),

    #[error("deal {0:?} not found")]
    DealNotFound(String),

    #[error("crm validation: {0}")]
    CrmValidation(String),

    #[error("approval required: {0}")]
    ApprovalRequired(String),

    #[error("approval {0:?} not found")]
    ApprovalNotFound(String),

    #[error("approval {0}")]
    ApprovalNotApproved(String),

    #[error("daily send budget exhausted: {0}")]
    LimitExceeded(String),

    #[error("channel {0:?} is disabled (kill-switch)")]
    ChannelDisabled(String),

    #[error(transparent)]
    Store(StoreError),
}

impl From<StoreError> for AgentError {
    fn from(e: StoreError) -> Self {
        AgentError::Store(e)
    }
}

impl From<serde_json::Error> for AgentError {
    fn from(e: serde_json::Error) -> Self {
        AgentError::Validation(e.to_string())
    }
}

/// Machine-readable category for an [`AgentError`].
///
/// Exactly three values (ROADMAP has no finer taxonomy — Phase 2 §4 only
/// distinguishes not-found vs validation vs conflict, so this brief pins
/// these three):
/// - `"user"` — bad input or missing resource; caller can fix and retry.
/// - `"approval"` — the approval gate refused the call; caller must drive
///   the `approval.*` flow first.
/// - `"internal"` — the store failed; caller should retry / escalate.
pub type ErrorCategory = &'static str;

/// Per-variant metadata for the central error taxonomy (Phase 2.1).
///
/// `code` is STABILITY-PINNED (see `error_code` in `src/actions.rs` and the
/// `error_codes_stable` test) — never rename a code string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ErrorInfo {
    /// Stable snake_case code (e.g. `"booking_not_found"`).
    pub code: &'static str,
    /// One of `"user" | "approval" | "internal"`.
    pub category: ErrorCategory,
    /// One-line human meaning of the error.
    pub meaning: &'static str,
    /// Most common reason this error surfaces.
    pub likely_cause: &'static str,
}

/// Map an [`AgentError`] to its [`ErrorInfo`].
///
/// Single source of truth for code + category + meaning + cause. The match
/// is EXHAUSTIVE over `&AgentError` on purpose (coordination contract):
/// sibling streams adding `LimitExceeded` / `ChannelDisabled` (or any future
/// variant) get a compile error here instead of a silent catalog omission.
pub fn error_info(e: &AgentError) -> ErrorInfo {
    match e {
        AgentError::CalendarNotFound(_) => ErrorInfo {
            code: "calendar_not_found",
            category: "user",
            meaning: "The requested calendar does not exist.",
            likely_cause: "Wrong calendar id, or the calendar was never created for this owner.",
        },
        AgentError::CalendarAlreadyExists(_) => ErrorInfo {
            code: "calendar_already_exists",
            category: "user",
            meaning: "A calendar with that identifier already exists.",
            likely_cause: "Retried create with the same id, or a name/id collision.",
        },
        AgentError::LinkNotFound(_) => ErrorInfo {
            code: "link_not_found",
            category: "user",
            meaning: "The requested booking link does not exist.",
            likely_cause: "Wrong link id, or the link was deleted.",
        },
        AgentError::BookingNotFound(_) => ErrorInfo {
            code: "booking_not_found",
            category: "user",
            meaning: "The requested booking does not exist.",
            likely_cause: "Wrong booking id, the booking was cancelled/purged, or the wrong owner.",
        },
        AgentError::Conflict(_) => ErrorInfo {
            code: "conflict",
            category: "user",
            meaning: "The request conflicts with existing state.",
            likely_cause: "Overlapping booking/window, or a concurrent write under a reject policy.",
        },
        AgentError::Validation(_) => ErrorInfo {
            code: "validation",
            category: "user",
            meaning: "A request parameter failed validation.",
            likely_cause: "Missing or malformed param (bad datetime, empty string, wrong type).",
        },
        AgentError::AttendeeExists(_) => ErrorInfo {
            code: "attendee_exists",
            category: "user",
            meaning: "That attendee is already on this booking.",
            likely_cause: "Retried add with the same email, or a duplicated attendee list.",
        },
        AgentError::BookingFull => ErrorInfo {
            code: "booking_full",
            category: "user",
            meaning: "The booking reached its attendee limit.",
            likely_cause: "Event at capacity; raise the limit or pick another slot.",
        },
        AgentError::ContactNotFound(_) => ErrorInfo {
            code: "contact_not_found",
            category: "user",
            meaning: "The requested contact does not exist.",
            likely_cause: "Wrong contact id, or the contact belongs to another owner.",
        },
        AgentError::CompanyNotFound(_) => ErrorInfo {
            code: "company_not_found",
            category: "user",
            meaning: "The requested company does not exist.",
            likely_cause: "Wrong company id, or the company belongs to another owner.",
        },
        AgentError::DealNotFound(_) => ErrorInfo {
            code: "deal_not_found",
            category: "user",
            meaning: "The requested deal does not exist.",
            likely_cause: "Wrong deal id, or the deal belongs to another owner.",
        },
        AgentError::CrmValidation(_) => ErrorInfo {
            code: "crm_validation",
            category: "user",
            meaning: "A CRM field failed validation.",
            likely_cause: "Missing required CRM field, unknown stage, or invalid enum value.",
        },
        AgentError::ApprovalRequired(_) => ErrorInfo {
            code: "approval_required",
            category: "approval",
            meaning: "This action needs approval before it can run.",
            likely_cause: "High-risk method called without a valid approval_id; request approval first.",
        },
        AgentError::ApprovalNotFound(_) => ErrorInfo {
            code: "approval_not_found",
            category: "approval",
            meaning: "The referenced approval does not exist.",
            likely_cause: "Wrong approval id, pruned record, or the wrong owner.",
        },
        AgentError::ApprovalNotApproved(_) => ErrorInfo {
            code: "approval_not_approved",
            category: "approval",
            meaning: "The approval is not in the approved state.",
            likely_cause: "Still pending, rejected, or expired; approve it with matching params, then retry.",
        },
        AgentError::LimitExceeded(_) => ErrorInfo {
            code: "limit_exceeded",
            category: "user",
            meaning: "The daily send budget for this channel is exhausted.",
            likely_cause: "Too many sends today; low-risk agent traffic is capped per UTC day — retry tomorrow.",
        },
        AgentError::ChannelDisabled(_) => ErrorInfo {
            code: "channel_disabled",
            category: "internal",
            meaning: "The channel is kill-switched off.",
            likely_cause: "Re-enable the channel or use another channel for this send.",
        },
        AgentError::Store(_) => ErrorInfo {
            code: "store_error",
            category: "internal",
            meaning: "The persistence layer failed.",
            likely_cause: "Disk/IO failure, corrupt row, or migration issue; retry, then inspect the store.",
        },
    }
}

/// Build the full error catalog: one [`ErrorInfo`] per [`AgentError`]
/// variant, in enum-declaration order.
///
/// The guard `match` below is deliberately EXHAUSTIVE (same coordination
/// contract as [`error_info`]): adding a variant without extending the
/// exemplar list is a compile error, not a silent omission. Do NOT add the
/// sibling streams' `LimitExceeded` / `ChannelDisabled` variants here —
/// they land in their own PRs and will extend this list then.
pub fn error_catalog() -> Vec<ErrorInfo> {
    let exemplars: Vec<AgentError> = vec![
        AgentError::CalendarNotFound(String::new()),
        AgentError::CalendarAlreadyExists(String::new()),
        AgentError::LinkNotFound(String::new()),
        AgentError::BookingNotFound(String::new()),
        AgentError::Conflict(String::new()),
        AgentError::Validation(String::new()),
        AgentError::AttendeeExists(String::new()),
        AgentError::BookingFull,
        AgentError::ContactNotFound(String::new()),
        AgentError::CompanyNotFound(String::new()),
        AgentError::DealNotFound(String::new()),
        AgentError::CrmValidation(String::new()),
        AgentError::ApprovalRequired(String::new()),
        AgentError::ApprovalNotFound(String::new()),
        AgentError::ApprovalNotApproved(String::new()),
        AgentError::Store(StoreError::Other(String::new())),
    ];
    // Exhaustiveness guard: every variant must be named here.
    for e in &exemplars {
        match e {
            AgentError::CalendarNotFound(_)
            | AgentError::CalendarAlreadyExists(_)
            | AgentError::LinkNotFound(_)
            | AgentError::BookingNotFound(_)
            | AgentError::Conflict(_)
            | AgentError::Validation(_)
            | AgentError::AttendeeExists(_)
            | AgentError::BookingFull
            | AgentError::ContactNotFound(_)
            | AgentError::CompanyNotFound(_)
            | AgentError::DealNotFound(_)
            | AgentError::CrmValidation(_)
            | AgentError::ApprovalRequired(_)
            | AgentError::ApprovalNotFound(_)
            | AgentError::ApprovalNotApproved(_)
            | AgentError::LimitExceeded(_)
            | AgentError::ChannelDisabled(_)
            | AgentError::Store(_) => {}
        }
    }
    exemplars.iter().map(error_info).collect()
}
