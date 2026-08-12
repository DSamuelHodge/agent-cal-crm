//! Stateful booking operations on a [`Calendar`].
//! Pure functions (no I/O) — they mutate the in-memory calendar and return
//! `Result` so callers can branch on outcome.

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::availability::{check_slot, conflicts};
use crate::error::{AgentError, Result};
use crate::types::{Attendee, Booking, BookingLink, Calendar, ConflictPolicy, Status, TimeSlot};

/// Outcome of a successful `book` — the new booking plus conflict metadata
/// (preserves the Python `message` channel for agent reasoning).
#[derive(Debug, Clone)]
pub struct Booked {
    pub booking: Booking,
    pub message: String,
    /// IDs of bookings cancelled by the `overwrite` policy.
    pub overwritten: Vec<String>,
}

/// Outcome of a successful `reschedule`.
#[derive(Debug, Clone)]
pub struct Rescheduled {
    pub booking: Booking,
    pub old_slot: TimeSlot,
}

/// High-level calendar stats for agent reasoning.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub owner_id: String,
    pub name: String,
    pub timezone: String,
    pub total_bookings: usize,
    pub active_bookings: usize,
    pub upcoming_today: usize,
    pub availability_windows: usize,
    pub blocked_periods: usize,
}

// ── Booking lifecycle ────────────────────────────────────────────────────────

/// Attempt to create a [`Booking`].
///
/// Handles `ConflictPolicy` (reject / warn / overwrite).
pub fn book(
    calendar: &mut Calendar,
    link: &BookingLink,
    slot: TimeSlot,
    attendees: Vec<Attendee>,
    notes: impl Into<String>,
    metadata: serde_json::Value,
) -> Result<Booked> {
    let check = check_slot(calendar, &slot, link);
    if !check.ok {
        return Err(AgentError::Conflict(
            check
                .reason
                .unwrap_or_else(|| "slot unavailable".to_string()),
        ));
    }

    let overlapping: Vec<Booking> = conflicts(&slot, calendar).into_iter().cloned().collect();

    let mut overwritten: Vec<String> = Vec::new();
    if !overlapping.is_empty() && link.conflict_policy == ConflictPolicy::Overwrite {
        let ids: Vec<String> = overlapping.iter().map(|b| b.id.clone()).collect();
        for b in calendar.bookings.iter_mut() {
            if ids.contains(&b.id) {
                b.status = Status::Cancelled;
                overwritten.push(b.id.clone());
            }
        }
    }
    // Warn: leave conflicts in place and proceed.

    if attendees.len() > link.max_attendees {
        return Err(AgentError::Validation(format!(
            "attendee count {} exceeds max_attendees={}",
            attendees.len(),
            link.max_attendees
        )));
    }

    let booking = Booking::new(
        slot,
        link.title.clone(),
        calendar.owner_id.clone(),
        attendees,
        notes.into(),
        metadata,
        Status::Confirmed,
    );
    calendar.bookings.push(booking.clone());

    let mut msg = "booked".to_string();
    if !overwritten.is_empty() {
        msg += &format!("; overwritten {} conflict(s)", overwritten.len());
    }
    if !check.conflicts.is_empty() && link.conflict_policy == ConflictPolicy::Warn {
        msg += &format!("; WARNING: {} overlap(s)", check.conflicts.len());
    }

    Ok(Booked {
        booking,
        message: msg,
        overwritten,
    })
}

/// Cancel a booking by ID.
pub fn cancel(calendar: &mut Calendar, booking_id: &str, reason: &str) -> Result<Booking> {
    for b in calendar.bookings.iter_mut() {
        if b.id == booking_id {
            if b.status == Status::Cancelled {
                return Err(AgentError::Validation(
                    "booking already cancelled".to_string(),
                ));
            }
            b.status = Status::Cancelled;
            b.notes = format!("{}\nCancelled: {}", b.notes, reason)
                .trim()
                .to_string();
            return Ok(b.clone());
        }
    }
    Err(AgentError::BookingNotFound(booking_id.to_string()))
}

/// Transition a PENDING booking to CONFIRMED.
pub fn confirm(calendar: &mut Calendar, booking_id: &str) -> Result<Booking> {
    for b in calendar.bookings.iter_mut() {
        if b.id == booking_id {
            if b.status != Status::Pending {
                return Err(AgentError::Validation(
                    format!("cannot confirm — status is {:?}", b.status).to_lowercase(),
                ));
            }
            b.status = Status::Confirmed;
            return Ok(b.clone());
        }
    }
    Err(AgentError::BookingNotFound(booking_id.to_string()))
}

/// Mark a booking as completed.
pub fn complete(calendar: &mut Calendar, booking_id: &str) -> Result<Booking> {
    for b in calendar.bookings.iter_mut() {
        if b.id == booking_id {
            if !matches!(b.status, Status::Pending | Status::Confirmed) {
                return Err(AgentError::Validation(
                    format!("cannot complete — status is {:?}", b.status).to_lowercase(),
                ));
            }
            b.status = Status::Completed;
            return Ok(b.clone());
        }
    }
    Err(AgentError::BookingNotFound(booking_id.to_string()))
}

/// Move an existing booking to `new_slot`.
/// Validates `new_slot` as if it were a fresh booking.
pub fn reschedule(
    calendar: &mut Calendar,
    booking_id: &str,
    new_slot: TimeSlot,
    link: &BookingLink,
) -> Result<Rescheduled> {
    let idx = calendar
        .bookings
        .iter()
        .position(|b| b.id == booking_id)
        .ok_or_else(|| AgentError::BookingNotFound(booking_id.to_string()))?;

    if calendar.bookings[idx].status == Status::Cancelled {
        return Err(AgentError::Validation(
            "cannot reschedule a cancelled booking".to_string(),
        ));
    }

    // Temporarily mark target cancelled so it doesn't conflict with itself.
    let original_status = calendar.bookings[idx].status;
    calendar.bookings[idx].status = Status::Cancelled;
    let check = check_slot(calendar, &new_slot, link);
    calendar.bookings[idx].status = original_status; // restore

    if !check.ok {
        return Err(AgentError::Conflict(
            check
                .reason
                .unwrap_or_else(|| "slot unavailable".to_string()),
        ));
    }

    let old_slot = calendar.bookings[idx].slot.clone();
    calendar.bookings[idx].slot = new_slot;
    Ok(Rescheduled {
        booking: calendar.bookings[idx].clone(),
        old_slot,
    })
}

/// Add an attendee to an existing booking (group bookings).
pub fn add_attendee(
    calendar: &mut Calendar,
    booking_id: &str,
    attendee: Attendee,
    link: &BookingLink,
) -> Result<Booking> {
    for b in calendar.bookings.iter_mut() {
        if b.id == booking_id {
            if b.status == Status::Cancelled {
                return Err(AgentError::Validation("booking is cancelled".to_string()));
            }
            if b.attendees.len() >= link.max_attendees {
                return Err(AgentError::BookingFull);
            }
            if b.attendees.iter().any(|a| a.email == attendee.email) {
                return Err(AgentError::AttendeeExists(attendee.email.clone()));
            }
            b.attendees.push(attendee);
            return Ok(b.clone());
        }
    }
    Err(AgentError::BookingNotFound(booking_id.to_string()))
}

/// Remove an attendee by email.
pub fn remove_attendee(calendar: &mut Calendar, booking_id: &str, email: &str) -> Result<Booking> {
    for b in calendar.bookings.iter_mut() {
        if b.id == booking_id {
            let before = b.attendees.len();
            b.attendees.retain(|a| a.email != email);
            if b.attendees.len() == before {
                return Err(AgentError::Validation(format!(
                    "{email:?} not found on booking"
                )));
            }
            return Ok(b.clone());
        }
    }
    Err(AgentError::BookingNotFound(booking_id.to_string()))
}

// ── Query helpers ────────────────────────────────────────────────────────────

pub fn get_booking(calendar: &Calendar, booking_id: &str) -> Result<Booking> {
    calendar
        .bookings
        .iter()
        .find(|b| b.id == booking_id)
        .cloned()
        .ok_or_else(|| AgentError::BookingNotFound(booking_id.to_string()))
}

pub fn list_bookings(
    calendar: &Calendar,
    status: Option<Status>,
    from_dt: Option<DateTime<Utc>>,
    to_dt: Option<DateTime<Utc>>,
) -> Result<Vec<Booking>> {
    let mut results: Vec<Booking> = calendar.bookings.clone();

    if let Some(status) = status {
        results.retain(|b| b.status == status);
    }
    if let Some(from) = from_dt {
        results.retain(|b| b.slot.end > from);
    }
    if let Some(to) = to_dt {
        results.retain(|b| b.slot.start < to);
    }
    Ok(results)
}

pub fn upcoming(calendar: &Calendar, limit: usize) -> Result<Vec<Booking>> {
    let now = Utc::now();
    let mut results: Vec<Booking> = calendar
        .active_bookings()
        .into_iter()
        .filter(|b| b.slot.start >= now)
        .cloned()
        .collect();
    results.sort_by_key(|b| b.slot.start);
    results.truncate(limit);
    Ok(results)
}

pub fn summary(calendar: &Calendar) -> Result<Summary> {
    let now = Utc::now();
    let active = calendar.active_bookings();
    Ok(Summary {
        owner_id: calendar.owner_id.clone(),
        name: calendar.name.clone(),
        timezone: calendar.timezone.clone(),
        total_bookings: calendar.bookings.len(),
        active_bookings: active.len(),
        upcoming_today: active
            .iter()
            .filter(|b| b.slot.start.date_naive() == now.date_naive())
            .count(),
        availability_windows: calendar.windows.len(),
        blocked_periods: calendar.blocked.len(),
    })
}
