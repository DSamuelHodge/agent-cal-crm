//! Slot generation, conflict detection, and availability checks.
//! Pure functions — no side effects, no I/O.

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Timelike, Utc};

use crate::types::{AvailabilityWindow, Booking, BookingLink, Calendar, ConflictPolicy, TimeSlot};

/// Result of validating a specific slot against a calendar + link.
#[derive(Debug, Clone)]
pub struct CheckResult {
    pub ok: bool,
    pub conflicts: Vec<Booking>,
    pub reason: Option<String>,
}

impl CheckResult {
    fn invalid(reason: impl Into<String>) -> Self {
        Self {
            ok: false,
            conflicts: Vec::new(),
            reason: Some(reason.into()),
        }
    }
}

fn now_utc() -> DateTime<Utc> {
    Utc::now()
}

/// All fitting slots for `window` on `target`.
/// Stride = `slot_minutes + buffer_after`.
fn window_slots_for_date(
    window: &AvailabilityWindow,
    target: NaiveDate,
    slot_minutes: i64,
    buffer_before: i64,
    buffer_after: i64,
) -> Vec<TimeSlot> {
    let proto_start = &window.slot.start;
    let proto_end = &window.slot.end;

    let window_start = Utc
        .with_ymd_and_hms(
            target.year(),
            target.month(),
            target.day(),
            proto_start.hour(),
            proto_start.minute(),
            0,
        )
        .single()
        .expect("valid window_start");
    let window_end = Utc
        .with_ymd_and_hms(
            target.year(),
            target.month(),
            target.day(),
            proto_end.hour(),
            proto_end.minute(),
            0,
        )
        .single()
        .expect("valid window_end");

    let stride = Duration::minutes(slot_minutes + buffer_after);
    let duration = Duration::minutes(slot_minutes);
    let mut cursor = window_start + Duration::minutes(buffer_before);

    let mut out = Vec::new();
    while cursor + duration <= window_end {
        out.push(TimeSlot::unchecked(cursor, cursor + duration));
        cursor += stride;
    }
    out
}

/// Active bookings overlapping `slot`.
pub(crate) fn conflicts<'a>(slot: &TimeSlot, calendar: &'a Calendar) -> Vec<&'a Booking> {
    calendar
        .active_bookings()
        .into_iter()
        .filter(|b| b.slot.overlaps(slot))
        .collect()
}

fn is_blocked(slot: &TimeSlot, calendar: &Calendar) -> bool {
    calendar.blocked.iter().any(|b| b.overlaps(slot))
}

/// Return all bookable [`TimeSlot`]s for `link` within `[from_dt, to_dt]`.
///
/// - Respects availability windows (day-of-week + hours).
/// - Excludes blocked periods and active bookings.
/// - Enforces `min_notice_hours` and `max_days_ahead` from `link`.
/// - Applies `buffer_before` / `buffer_after` padding.
pub fn available_slots(
    calendar: &Calendar,
    link: &BookingLink,
    from_dt: Option<DateTime<Utc>>,
    to_dt: Option<DateTime<Utc>>,
) -> Vec<TimeSlot> {
    let now = now_utc();
    let earliest = std::cmp::max(
        from_dt.unwrap_or(now),
        now + Duration::milliseconds((link.min_notice_hours * 3600.0 * 1000.0) as i64),
    );
    let latest = to_dt.unwrap_or_else(|| now + Duration::days(link.max_days_ahead));

    if calendar.windows.is_empty() {
        return Vec::new();
    }

    let mut slots = Vec::new();
    let mut cursor = earliest.date_naive();
    let end_date = latest.date_naive();

    while cursor <= end_date {
        for window in &calendar.windows {
            if let Some(dow) = window.day_of_week {
                if cursor.weekday().num_days_from_monday() != dow as u32 {
                    continue;
                }
            }
            for slot in window_slots_for_date(
                window,
                cursor,
                link.duration_minutes,
                link.buffer_before_min,
                link.buffer_after_min,
            ) {
                if slot.start < earliest || slot.end > latest {
                    continue;
                }
                if is_blocked(&slot, calendar) {
                    continue;
                }
                if !conflicts(&slot, calendar).is_empty() {
                    continue;
                }
                slots.push(slot);
            }
        }
        cursor += Duration::days(1);
    }

    slots
}

/// Validate a specific slot against a calendar + link.
pub fn check_slot(calendar: &Calendar, slot: &TimeSlot, link: &BookingLink) -> CheckResult {
    let now = now_utc();

    let duration = Duration::minutes(link.duration_minutes);
    if slot.duration() != duration {
        return CheckResult::invalid(format!(
            "slot duration {}m != required {}m",
            slot.duration().num_minutes(),
            link.duration_minutes
        ));
    }

    if slot.start < now + Duration::milliseconds((link.min_notice_hours * 3600.0 * 1000.0) as i64) {
        return CheckResult::invalid(format!(
            "slot starts within min_notice_hours={}",
            link.min_notice_hours
        ));
    }

    if slot.start > now + Duration::days(link.max_days_ahead) {
        return CheckResult::invalid(format!(
            "slot beyond max_days_ahead={}",
            link.max_days_ahead
        ));
    }

    if is_blocked(slot, calendar) {
        return CheckResult::invalid("slot overlaps a blocked period");
    }

    let conflicted: Vec<Booking> = conflicts(slot, calendar).into_iter().cloned().collect();
    let n = conflicted.len();
    if n > 0 {
        match link.conflict_policy {
            ConflictPolicy::Reject => {
                return CheckResult::invalid(format!("{n} conflict(s) and policy=reject"));
            }
            // Warn / Overwrite — flag but still ok.
            policy => {
                return CheckResult {
                    ok: true,
                    conflicts: conflicted,
                    reason: Some(format!("{n} conflict(s) — policy={:?}", policy).to_lowercase()),
                };
            }
        }
    }

    CheckResult {
        ok: true,
        conflicts: conflicted,
        reason: None,
    }
}

/// Return slots free across **all** calendars (multi-person scheduling).
pub fn find_mutual_slot(
    calendars: &[Calendar],
    link: &BookingLink,
    from_dt: Option<DateTime<Utc>>,
    to_dt: Option<DateTime<Utc>>,
) -> Vec<TimeSlot> {
    if calendars.is_empty() {
        return Vec::new();
    }

    let mut sets: Vec<Vec<(DateTime<Utc>, DateTime<Utc>)>> = calendars
        .iter()
        .map(|cal| {
            available_slots(cal, link, from_dt, to_dt)
                .into_iter()
                .map(|s| (s.start, s.end))
                .collect()
        })
        .collect();

    let first = sets.remove(0);
    let common: Vec<(DateTime<Utc>, DateTime<Utc>)> = first
        .into_iter()
        .filter(|pair| sets.iter().all(|set| set.contains(pair)))
        .collect();

    let mut out: Vec<TimeSlot> = common
        .into_iter()
        .map(|(start, end)| TimeSlot::unchecked(start, end))
        .collect();
    out.sort_by_key(|s| s.start);
    out.dedup_by(|a, b| a.start == b.start && a.end == b.end);
    out
}
