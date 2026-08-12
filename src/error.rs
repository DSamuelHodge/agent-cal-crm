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

    #[error(transparent)]
    Store(StoreError),
}

impl From<StoreError> for AgentError {
    fn from(e: StoreError) -> Self {
        AgentError::Store(e)
    }
}
