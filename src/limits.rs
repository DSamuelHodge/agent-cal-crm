//! Deterministic per-channel, per-day send budgets (Phase 2 limits ledger).
//!
//! Every counted send is keyed by `(owner, channel, day)` where `day` is a
//! UTC calendar day (`YYYY-MM-DD`). Budgets are fixed:
//!
//! | channel | RPC method   | budget/day |
//! |---------|--------------|------------|
//! | `sms`   | `sms.send`   | 100        |
//! | `email` | `email.send` | 200        |
//!
//! The ledger itself lives on the stores (`limit_usage` table / in-memory
//! map) behind [`crate::crm::store::CrmStore::record_use`] and
//! [`crate::crm::store::CrmStore::usage`]; enforcement happens in the RPC
//! dispatch wrapper (`crate::rpc::dispatch`), which denies sends at or over
//! budget with [`crate::error::AgentError::LimitExceeded`] and records one
//! unit of usage after every successful send. This module holds the pure
//! policy pieces (budgets, day handling, near-exhaustion math) so they are
//! unit-testable without a store.

use chrono::Utc;

/// Channel key for SMS sends (`sms.send`).
pub const SMS_CHANNEL: &str = "sms";
/// Channel key for email sends (`email.send`).
pub const EMAIL_CHANNEL: &str = "email";

/// Sends allowed per UTC day on the SMS channel.
pub const SMS_BUDGET: u64 = 100;
/// Sends allowed per UTC day on the email channel.
pub const EMAIL_BUDGET: u64 = 200;

/// Budget for a channel, or `None` for an unknown channel.
pub fn budget_for_channel(channel: &str) -> Option<u64> {
    match channel {
        SMS_CHANNEL => Some(SMS_BUDGET),
        EMAIL_CHANNEL => Some(EMAIL_BUDGET),
        _ => None,
    }
}

/// Channel for a send RPC method (`sms.send` → `sms`), or `None`.
pub fn method_channel(method: &str) -> Option<&'static str> {
    match method {
        "sms.send" => Some(SMS_CHANNEL),
        "email.send" => Some(EMAIL_CHANNEL),
        _ => None,
    }
}

/// Today as a UTC calendar day (`YYYY-MM-DD`).
pub fn today_utc_day() -> String {
    Utc::now().date_naive().format("%Y-%m-%d").to_string()
}

/// Whether `day` is a well-formed, canonical UTC calendar day (`YYYY-MM-DD`,
/// zero-padded month/day). `chrono`'s parser is lenient about padding, so we
/// reject anything that does not round-trip to the identical string.
pub fn is_valid_day(day: &str) -> bool {
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map(|d| d.format("%Y-%m-%d").to_string() == day)
        .unwrap_or(false)
}

/// Within 20% of exhaustion, i.e. `used` has reached 80% of `budget`.
///
/// Integer math only (`used * 5 >= budget * 4`), no floats: at exactly 80%
/// this returns `true`, just below it `false`.
pub fn near_exhaustion(used: u64, budget: u64) -> bool {
    used.saturating_mul(5) >= budget.saturating_mul(4)
}

/// Snapshot returned by `limit.query`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LimitStatus {
    pub channel: String,
    pub day: String,
    pub used: u64,
    pub budget: u64,
    pub near_exhaustion: bool,
}

impl LimitStatus {
    pub fn new(channel: &str, day: &str, used: u64, budget: u64) -> Self {
        Self {
            channel: channel.to_string(),
            day: day.to_string(),
            used,
            budget,
            near_exhaustion: near_exhaustion(used, budget),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_fixed_per_contract() {
        assert_eq!(budget_for_channel("sms"), Some(100));
        assert_eq!(budget_for_channel("email"), Some(200));
        assert_eq!(budget_for_channel("whatsapp"), None);
        assert_eq!(budget_for_channel(""), None);
    }

    #[test]
    fn send_methods_map_to_channels() {
        assert_eq!(method_channel("sms.send"), Some("sms"));
        assert_eq!(method_channel("email.send"), Some("email"));
        assert_eq!(method_channel("limit.query"), None);
        assert_eq!(method_channel("ping"), None);
    }

    #[test]
    fn near_exhaustion_boundary_is_80_percent() {
        // SMS budget: 80/100 trips the flag, 79 does not.
        assert!(!near_exhaustion(79, 100));
        assert!(near_exhaustion(80, 100));
        assert!(near_exhaustion(100, 100));
        assert!(near_exhaustion(150, 100));
        // Email budget: 160/200 trips the flag, 159 does not.
        assert!(!near_exhaustion(159, 200));
        assert!(near_exhaustion(160, 200));
        // Zero usage is never near exhaustion.
        assert!(!near_exhaustion(0, 100));
    }

    #[test]
    fn day_format_roundtrip() {
        let today = today_utc_day();
        assert!(is_valid_day(&today));
        assert!(is_valid_day("2026-09-08"));
        assert!(!is_valid_day("2026-9-8"));
        assert!(!is_valid_day("tomorrow"));
        assert!(!is_valid_day(""));
        assert!(!is_valid_day("2026-13-01"));
    }
}
