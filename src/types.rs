//! Canonical data structures for agentcal.
//!
//! Every domain type derives `serde::Serialize` / `serde::Deserialize` so
//! state stays JSON round-trip safe (embeddable in prompts / vector DBs).
//! Timestamps are always `chrono::DateTime<Utc>` (ISO-8601 on the wire).

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

// ── Enums ────────────────────────────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pending,
    Confirmed,
    Cancelled,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecurrenceRule {
    None,
    Daily,
    Weekly,
    Monthly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConflictPolicy {
    /// Hard block — never double-book.
    Reject,
    /// Allow but surface the conflict.
    Warn,
    /// Cancel the conflicting booking first.
    Overwrite,
}

// ── Core types ───────────────────────────────────────────────────────────────

/// A half-open interval `[start, end)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSlot {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl TimeSlot {
    pub fn new(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Self, crate::error::AgentError> {
        if end <= start {
            return Err(crate::error::AgentError::Validation(format!(
                "end ({end}) must be after start ({start})"
            )));
        }
        Ok(Self { start, end })
    }

    /// Construct without validating (end may be <= start). Prefer `TimeSlot::new`.
    pub fn unchecked(start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        Self { start, end }
    }

    pub fn duration(&self) -> Duration {
        self.end - self.start
    }

    pub fn duration_minutes(&self) -> i64 {
        self.duration().num_minutes()
    }

    pub fn overlaps(&self, other: &TimeSlot) -> bool {
        self.start < other.end && other.start < self.end
    }

    pub fn contains(&self, other: &TimeSlot) -> bool {
        self.start <= other.start && other.end <= self.end
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attendee {
    pub id: String,
    pub name: String,
    pub email: String,
    /// Agent-owned payload.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl Attendee {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            id: new_id(),
            name: name.into(),
            email: email.into(),
            metadata: serde_json::Value::Null,
        }
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }
}

/// Recurring window when a calendar owner accepts bookings.
/// `day_of_week`: 0=Monday … 6=Sunday (`None` ⇒ every day).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailabilityWindow {
    /// Prototype start/end on a reference day.
    pub slot: TimeSlot,
    #[serde(default)]
    pub day_of_week: Option<u8>,
    #[serde(default)]
    pub label: String,
}

impl AvailabilityWindow {
    pub fn new(slot: TimeSlot, day_of_week: Option<u8>, label: impl Into<String>) -> Self {
        Self {
            slot,
            day_of_week,
            label: label.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Booking {
    pub id: String,
    pub title: String,
    pub owner_id: String,
    pub slot: TimeSlot,
    pub status: Status,
    #[serde(default)]
    pub attendees: Vec<Attendee>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

impl Booking {
    pub fn new(
        slot: TimeSlot,
        title: impl Into<String>,
        owner_id: impl Into<String>,
        attendees: Vec<Attendee>,
        notes: impl Into<String>,
        metadata: serde_json::Value,
        status: Status,
    ) -> Self {
        Self {
            id: new_id(),
            title: title.into(),
            owner_id: owner_id.into(),
            slot,
            status,
            attendees,
            notes: notes.into(),
            metadata,
            created_at: Utc::now(),
        }
    }
}

/// Calendly-style shareable booking config.
/// Agents pass this around instead of a URL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BookingLink {
    pub id: String,
    pub owner_id: String,
    pub title: String,
    pub duration_minutes: i64,
    pub max_attendees: usize,
    pub buffer_before_min: i64,
    pub buffer_after_min: i64,
    pub min_notice_hours: f64,
    pub max_days_ahead: i64,
    pub recurrence: RecurrenceRule,
    #[serde(default)]
    pub questions: Vec<String>,
    pub conflict_policy: ConflictPolicy,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl BookingLink {
    pub fn new(
        owner_id: impl Into<String>,
        duration_minutes: i64,
        title: impl Into<String>,
    ) -> Self {
        Self {
            id: new_id(),
            owner_id: owner_id.into(),
            title: title.into(),
            duration_minutes,
            max_attendees: 1,
            buffer_before_min: 0,
            buffer_after_min: 0,
            min_notice_hours: 1.0,
            max_days_ahead: 30,
            recurrence: RecurrenceRule::None,
            questions: Vec::new(),
            conflict_policy: ConflictPolicy::Reject,
            metadata: serde_json::Value::Null,
        }
    }
}

/// Owner-level container: availability rules + all bookings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Calendar {
    pub owner_id: String,
    pub name: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default = "default_reference_date")]
    pub reference_date: NaiveDate,
    #[serde(default)]
    pub windows: Vec<AvailabilityWindow>,
    #[serde(default)]
    pub bookings: Vec<Booking>,
    /// OOO / breaks.
    #[serde(default)]
    pub blocked: Vec<TimeSlot>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

fn default_timezone() -> String {
    "UTC".to_string()
}

fn default_reference_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()
}

impl Calendar {
    pub fn new(owner_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            owner_id: owner_id.into(),
            name: name.into(),
            timezone: default_timezone(),
            reference_date: default_reference_date(),
            windows: Vec::new(),
            bookings: Vec::new(),
            blocked: Vec::new(),
            metadata: serde_json::Value::Null,
        }
    }

    /// Bookings that are still live (pending or confirmed).
    pub fn active_bookings(&self) -> Vec<&Booking> {
        self.bookings
            .iter()
            .filter(|b| matches!(b.status, Status::Pending | Status::Confirmed))
            .collect()
    }
}

// ── ICS Export ──────────────────────────────────────────────────────────────

/// Format a DateTime<Utc> as an ICS date-time value: YYYYMMDDTHHMMSSZ
pub(crate) fn ics_datetime(dt: &DateTime<Utc>) -> String {
    dt.format("%Y%m%dT%H%M%SZ").to_string()
}

/// Format a TimeSlot as ICS DTSTART/DTEND lines.
pub(crate) fn ics_slot(slot: &TimeSlot) -> (String, String) {
    (ics_datetime(&slot.start), ics_datetime(&slot.end))
}

/// Generate a VEVENT string for a Booking.
pub(crate) fn ics_booking_event(b: &Booking) -> String {
    let (dtstart, dtend) = ics_slot(&b.slot);
    let mut lines = Vec::new();
    lines.push("BEGIN:VEVENT".to_string());
    lines.push(format!("UID:{}", b.id));
    lines.push(format!("DTSTART:{}", dtstart));
    lines.push(format!("DTEND:{}", dtend));
    lines.push(format!("SUMMARY:{}", b.title));
    lines.push(format!("DESCRIPTION:{}", b.notes));
    lines.push(format!("STATUS:{}", ics_status(b.status)));
    // Attendee emails
    for attendee in &b.attendees {
        let cn = ics_escape(&attendee.name);
        lines.push(format!("ATTENDEE;CN={cn}:mailto:{}", attendee.email));
    }
    // Note: recurrence is configured via BookingLink, not per-booking
    lines.push("END:VEVENT".to_string());
    lines.join("\r\n")
}

/// RFC 5545 `STATUS` value for a [`Status`] (upper-cased, ICS flavour).
fn ics_status(s: Status) -> &'static str {
    match s {
        Status::Pending => "PENDING",
        Status::Confirmed => "CONFIRMED",
        Status::Cancelled => "CANCELLED",
        Status::Completed => "COMPLETED",
    }
}

/// Escape a text value for an ICS property value (per RFC 5545 §3.3.11).
fn ics_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
}

/// Generate VEVENT strings for AvailabilityWindow items.
pub(crate) fn ics_availability_events(windows: &[AvailabilityWindow]) -> String {
    if windows.is_empty() {
        return String::new();
    }
    let mut events = Vec::new();
    for window in windows {
        let (dtstart, dtend) = ics_slot(&window.slot);
        let mut line = Vec::new();
        line.push("BEGIN:VEVENT".to_string());
        line.push(format!("UID:window-{}", window.label.replace(" ", "-")));
        line.push(format!("DTSTART:{}", dtstart));
        line.push(format!("DTEND:{}", dtend));
        line.push(format!("SUMMARY:{}", ics_escape(&window.label)));
        line.push("STATUS:CONFIRMED".to_string());
        line.push("END:VEVENT".to_string());
        events.push(line.join("\r\n"));
    }
    events.join("\r\n")
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Pending => write!(f, "Pending"),
            Status::Confirmed => write!(f, "Confirmed"),
            Status::Cancelled => write!(f, "Cancelled"),
            Status::Completed => write!(f, "Completed"),
        }
    }
}
