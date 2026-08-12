//! [`AgentCal`] — the single façade agents call.
//!
//! Design goals (carried over from the Python original):
//!   • One import, one object.
//!   • Every method returns `Result<T, AgentError>` — agents branch on the
//!     `Result` instead of `{"ok": bool}` dicts.
//!   • No HTTP, no GUI, no background threads.
//!   • Fully serialisable state (JSON round-trip safe via serde).
//!
//! ```no_run
//! use agentcal::{AgentCal, MemoryStore, LinkParams};
//!
//! # async fn demo() -> agentcal::Result<()> {
//! let cal = AgentCal::new(MemoryStore::new());
//! cal.create_calendar_simple("alice", "Alice's Calendar").await?;
//! for dow in 0..5u8 {
//!     cal.add_window("alice", Some(dow), "09:00", "17:00", "").await?;
//! }
//! let link = cal
//!     .create_link("alice", LinkParams::new("30-min sync", 30))
//!     .await?;
//! let slots = cal.get_slots("alice", &link.id, None, None, Some(5)).await?;
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use chrono::{DateTime, Datelike, TimeZone, Utc};

use crate::availability::{self, CheckResult};
use crate::error::{AgentError, Result};
use crate::scheduler::{self, Booked, Rescheduled, Summary};
use crate::store::CalendarStore;
use crate::types::{
    Attendee, AvailabilityWindow, Booking, BookingLink, Calendar, ConflictPolicy, Status, TimeSlot,
};

/// Options for [`AgentCal::create_link`] (Rust's answer to Python's kwargs).
#[derive(Debug, Clone)]
pub struct LinkParams {
    pub duration_minutes: i64,
    pub title: String,
    pub max_attendees: usize,
    pub buffer_before_min: i64,
    pub buffer_after_min: i64,
    pub min_notice_hours: f64,
    pub max_days_ahead: i64,
    pub conflict_policy: ConflictPolicy,
    pub questions: Vec<String>,
    pub metadata: serde_json::Value,
}

impl LinkParams {
    /// Required fields; everything else takes the Python defaults.
    pub fn new(title: impl Into<String>, duration_minutes: i64) -> Self {
        Self {
            duration_minutes,
            title: title.into(),
            max_attendees: 1,
            buffer_before_min: 0,
            buffer_after_min: 0,
            min_notice_hours: 1.0,
            max_days_ahead: 30,
            conflict_policy: ConflictPolicy::Reject,
            questions: Vec::new(),
            metadata: serde_json::Value::Null,
        }
    }

    pub fn max_attendees(mut self, n: usize) -> Self {
        self.max_attendees = n;
        self
    }
    pub fn buffer_before_min(mut self, n: i64) -> Self {
        self.buffer_before_min = n;
        self
    }
    pub fn buffer_after_min(mut self, n: i64) -> Self {
        self.buffer_after_min = n;
        self
    }
    pub fn min_notice_hours(mut self, h: f64) -> Self {
        self.min_notice_hours = h;
        self
    }
    pub fn max_days_ahead(mut self, d: i64) -> Self {
        self.max_days_ahead = d;
        self
    }
    pub fn conflict_policy(mut self, p: ConflictPolicy) -> Self {
        self.conflict_policy = p;
        self
    }
    pub fn questions(mut self, q: Vec<String>) -> Self {
        self.questions = q;
        self
    }
    pub fn metadata(mut self, m: serde_json::Value) -> Self {
        self.metadata = m;
        self
    }
}

/// Parse `"HH:MM"` into `(hour, minute)`.
fn parse_hhmm(hhmm: &str) -> Result<(u32, u32)> {
    let mut parts = hhmm.split(':');
    let h = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| AgentError::Validation(format!("invalid time {hhmm:?}")))?;
    let m = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| AgentError::Validation(format!("invalid time {hhmm:?}")))?;
    Ok((h, m))
}

/// Reference-date prototype datetime (only time-of-day is meaningful).
pub struct AgentCal {
    store: Arc<dyn CalendarStore>,
    auto_save: bool,
    links: Arc<Mutex<HashMap<String, BookingLink>>>,
}

impl AgentCal {
    /// Wrap any [`CalendarStore`]. Persistence is shared across clones.
    pub fn new<S: CalendarStore + 'static>(store: S) -> Self {
        Self {
            store: Arc::new(store),
            auto_save: true,
            links: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Build from an already-shared store handle.
    pub fn from_shared(store: Arc<dyn CalendarStore>) -> Self {
        Self {
            store,
            auto_save: true,
            links: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Builder-style control over auto-persist after each mutating call.
    pub fn with_auto_save(mut self, auto_save: bool) -> Self {
        self.auto_save = auto_save;
        self
    }

    // ── Internal ───────────────────────────────────────────────────────────

    async fn require_cal(&self, owner_id: &str) -> Result<Calendar> {
        self.store
            .load(owner_id)
            .await?
            .ok_or_else(|| AgentError::CalendarNotFound(owner_id.to_string()))
    }

    async fn require_link(&self, link_id: &str) -> Result<BookingLink> {
        // Check cache first
        if let Some(link) = self.links.lock().await.get(link_id).cloned() {
            return Ok(link);
        }
        // Fall back to store
        let link = self
            .store
            .load_link(link_id)
            .await?
            .ok_or_else(|| AgentError::LinkNotFound(link_id.to_string()))?;
        // Populate cache
        self.links
            .lock()
            .await
            .insert(link_id.to_string(), link.clone());
        Ok(link)
    }

    async fn persist(&self, cal: &Calendar) -> Result<()> {
        if self.auto_save {
            self.store.save(cal).await?;
        }
        Ok(())
    }

    // ── Calendar management ────────────────────────────────────────────────

    /// Create and persist a new [`Calendar`].
    pub async fn create_calendar(
        &self,
        owner_id: &str,
        name: &str,
        timezone: &str,
        metadata: serde_json::Value,
    ) -> Result<Calendar> {
        if self.store.load(owner_id).await?.is_some() {
            return Err(AgentError::CalendarAlreadyExists(owner_id.to_string()));
        }
        let mut cal = Calendar::new(owner_id, name);
        cal.timezone = timezone.to_string();
        cal.metadata = metadata;
        self.store.save(&cal).await?;
        Ok(cal)
    }

    /// Convenience: `create_calendar(owner_id, name, "UTC", Value::Null)`.
    pub async fn create_calendar_simple(&self, owner_id: &str, name: &str) -> Result<Calendar> {
        self.create_calendar(owner_id, name, "UTC", serde_json::Value::Null)
            .await
    }

    pub async fn get_calendar(&self, owner_id: &str) -> Result<Calendar> {
        self.require_cal(owner_id).await
    }

    pub async fn delete_calendar(&self, owner_id: &str) -> Result<()> {
        let existed = self.store.delete(owner_id).await?;
        if !existed {
            return Err(AgentError::CalendarNotFound(owner_id.to_string()));
        }
        Ok(())
    }

    pub async fn list_calendars(&self) -> Result<Vec<String>> {
        self.store.list_ids().await
    }

    // ── Availability windows ───────────────────────────────────────────────

    /// Add a recurring availability window.
    /// `day_of_week`: 0=Mon … 6=Sun, `None` = every day.
    pub async fn add_window(
        &self,
        owner_id: &str,
        day_of_week: Option<u8>,
        start_hhmm: &str,
        end_hhmm: &str,
        label: &str,
    ) -> Result<AvailabilityWindow> {
        let mut cal = self.require_cal(owner_id).await?;
        let (sh, sm) = parse_hhmm(start_hhmm)?;
        let (eh, em) = parse_hhmm(end_hhmm)?;
        let slot_start = Utc
            .with_ymd_and_hms(
                cal.reference_date.year(),
                cal.reference_date.month(),
                cal.reference_date.day(),
                sh,
                sm,
                0,
            )
            .single()
            .expect("valid prototype start");
        let slot_end = Utc
            .with_ymd_and_hms(
                cal.reference_date.year(),
                cal.reference_date.month(),
                cal.reference_date.day(),
                eh,
                em,
                0,
            )
            .single()
            .expect("valid prototype end");
        let window =
            AvailabilityWindow::new(TimeSlot::new(slot_start, slot_end)?, day_of_week, label);
        cal.windows.push(window.clone());
        self.persist(&cal).await?;
        Ok(window)
    }

    pub async fn clear_windows(&self, owner_id: &str) -> Result<()> {
        let mut cal = self.require_cal(owner_id).await?;
        cal.windows.clear();
        self.persist(&cal).await
    }

    // ── Blocked periods (OOO / breaks / holidays) ─────────────────────────

    /// Block a time range — no bookings allowed during this window.
    pub async fn block(
        &self,
        owner_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<TimeSlot> {
        let mut cal = self.require_cal(owner_id).await?;
        let slot = TimeSlot::new(start, end)?;
        cal.blocked.push(slot.clone());
        self.persist(&cal).await?;
        Ok(slot)
    }

    pub async fn unblock_all(&self, owner_id: &str) -> Result<()> {
        let mut cal = self.require_cal(owner_id).await?;
        cal.blocked.clear();
        self.persist(&cal).await
    }

    // ── Booking links ──────────────────────────────────────────────────────

    /// Create a [`BookingLink`] (Calendly-style booking config). Persisted.
    pub async fn create_link(&self, owner_id: &str, params: LinkParams) -> Result<BookingLink> {
        self.require_cal(owner_id).await?;
        let mut link = BookingLink::new(owner_id, params.duration_minutes, params.title);
        link.max_attendees = params.max_attendees;
        link.buffer_before_min = params.buffer_before_min;
        link.buffer_after_min = params.buffer_after_min;
        link.min_notice_hours = params.min_notice_hours;
        link.max_days_ahead = params.max_days_ahead;
        link.conflict_policy = params.conflict_policy;
        link.questions = params.questions;
        link.metadata = params.metadata;
        self.store.save_link(&link).await?;
        // Write-through cache
        self.links
            .lock()
            .await
            .insert(link.id.clone(), link.clone());
        Ok(link)
    }

    pub async fn get_link(&self, link_id: &str) -> Result<BookingLink> {
        self.require_link(link_id).await
    }

    pub async fn list_links(&self, owner_id: &str) -> Result<Vec<BookingLink>> {
        self.store.load_links(owner_id).await
    }

    // ── Slot discovery ─────────────────────────────────────────────────────

    /// Return available [`TimeSlot`]s.
    pub async fn get_slots(
        &self,
        owner_id: &str,
        link_id: &str,
        from_dt: Option<DateTime<Utc>>,
        to_dt: Option<DateTime<Utc>>,
        limit: Option<usize>,
    ) -> Result<Vec<TimeSlot>> {
        let cal = self.require_cal(owner_id).await?;
        let link = self.require_link(link_id).await?;
        let mut slots = availability::available_slots(&cal, &link, from_dt, to_dt);
        if let Some(limit) = limit {
            slots.truncate(limit);
        }
        Ok(slots)
    }

    /// Validate a specific slot without booking it.
    pub async fn check_slot(
        &self,
        owner_id: &str,
        link_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<CheckResult> {
        let cal = self.require_cal(owner_id).await?;
        let link = self.require_link(link_id).await?;
        let slot = TimeSlot::new(start, end)?;
        Ok(availability::check_slot(&cal, &slot, &link))
    }

    /// Find slots free for ALL listed owners (multi-person scheduling).
    pub async fn find_mutual_slots(
        &self,
        owner_ids: &[&str],
        link_id: &str,
        from_dt: Option<DateTime<Utc>>,
        to_dt: Option<DateTime<Utc>>,
        limit: Option<usize>,
    ) -> Result<Vec<TimeSlot>> {
        let link = self.require_link(link_id).await?;
        let mut cals = Vec::with_capacity(owner_ids.len());
        for oid in owner_ids {
            cals.push(self.require_cal(oid).await?);
        }
        let mut slots = availability::find_mutual_slot(&cals, &link, from_dt, to_dt);
        if let Some(limit) = limit {
            slots.truncate(limit);
        }
        Ok(slots)
    }

    // ── Booking operations ─────────────────────────────────────────────────

    /// Create a booking.
    pub async fn book(
        &self,
        owner_id: &str,
        link_id: &str,
        slot: TimeSlot,
        attendees: Vec<Attendee>,
        notes: &str,
        metadata: serde_json::Value,
    ) -> Result<Booked> {
        let mut cal = self.require_cal(owner_id).await?;
        let link = self.require_link(link_id).await?;
        let result = scheduler::book(&mut cal, &link, slot, attendees, notes, metadata)?;
        self.persist(&cal).await?;
        Ok(result)
    }

    pub async fn cancel(&self, owner_id: &str, booking_id: &str, reason: &str) -> Result<Booking> {
        let mut cal = self.require_cal(owner_id).await?;
        let b = scheduler::cancel(&mut cal, booking_id, reason)?;
        self.persist(&cal).await?;
        Ok(b)
    }

    pub async fn confirm(&self, owner_id: &str, booking_id: &str) -> Result<Booking> {
        let mut cal = self.require_cal(owner_id).await?;
        let b = scheduler::confirm(&mut cal, booking_id)?;
        self.persist(&cal).await?;
        Ok(b)
    }

    pub async fn complete(&self, owner_id: &str, booking_id: &str) -> Result<Booking> {
        let mut cal = self.require_cal(owner_id).await?;
        let b = scheduler::complete(&mut cal, booking_id)?;
        self.persist(&cal).await?;
        Ok(b)
    }

    pub async fn reschedule(
        &self,
        owner_id: &str,
        booking_id: &str,
        link_id: &str,
        new_slot: TimeSlot,
    ) -> Result<Rescheduled> {
        let mut cal = self.require_cal(owner_id).await?;
        let link = self.require_link(link_id).await?;
        let r = scheduler::reschedule(&mut cal, booking_id, new_slot, &link)?;
        self.persist(&cal).await?;
        Ok(r)
    }

    pub async fn add_attendee(
        &self,
        owner_id: &str,
        booking_id: &str,
        link_id: &str,
        attendee: Attendee,
    ) -> Result<Booking> {
        let mut cal = self.require_cal(owner_id).await?;
        let link = self.require_link(link_id).await?;
        let b = scheduler::add_attendee(&mut cal, booking_id, attendee, &link)?;
        self.persist(&cal).await?;
        Ok(b)
    }

    pub async fn remove_attendee(
        &self,
        owner_id: &str,
        booking_id: &str,
        email: &str,
    ) -> Result<Booking> {
        let mut cal = self.require_cal(owner_id).await?;
        let b = scheduler::remove_attendee(&mut cal, booking_id, email)?;
        self.persist(&cal).await?;
        Ok(b)
    }

    // ── Query ──────────────────────────────────────────────────────────────

    pub async fn get_booking(&self, owner_id: &str, booking_id: &str) -> Result<Booking> {
        let cal = self.require_cal(owner_id).await?;
        scheduler::get_booking(&cal, booking_id)
    }

    pub async fn list_bookings(
        &self,
        owner_id: &str,
        status: Option<Status>,
        from_dt: Option<DateTime<Utc>>,
        to_dt: Option<DateTime<Utc>>,
    ) -> Result<Vec<Booking>> {
        let cal = self.require_cal(owner_id).await?;
        scheduler::list_bookings(&cal, status, from_dt, to_dt)
    }

    pub async fn upcoming(&self, owner_id: &str, limit: usize) -> Result<Vec<Booking>> {
        let cal = self.require_cal(owner_id).await?;
        scheduler::upcoming(&cal, limit)
    }

    pub async fn summary(&self, owner_id: &str) -> Result<Summary> {
        let cal = self.require_cal(owner_id).await?;
        scheduler::summary(&cal)
    }

    /// Export the calendar as an [RFC 5545](https://tools.ietf.org/html/rfc5545) ICS string.
    ///
    /// # Example
    /// ```no_run
    /// use agentcal::{AgentCal, MemoryStore};
    ///
    /// async fn demo() -> agentcal::Result<()> {
    ///     let cal = AgentCal::new(MemoryStore::new());
    ///     cal.create_calendar_simple("alice", "Alice's Calendar").await?;
    ///     let link = cal.create_link("alice", agentcal::LinkParams::new("30-min sync", 30)).await?;
    ///     let slot = cal.get_slots("alice", &link.id, None, None, Some(1)).await?;
    ///     cal.book("alice", &link.id, slot[0].clone(), vec![], "Meeting", serde_json::Value::Null).await?;
    ///     let ics = cal.to_ics("alice").await?;
    ///     println!("{}", ics);
    ///     Ok(())
    /// }
    /// ```
    pub async fn to_ics(&self, owner_id: &str) -> Result<String> {
        let cal = self.require_cal(owner_id).await?;

        let mut ics_lines = vec![
            "BEGIN:VCALENDAR".to_string(),
            "PRODID:-//agentcal//agentcal.rs//EN".to_string(),
            "VERSION:2.0".to_string(),
            "CALSCALE:GREGORIAN".to_string(),
        ];
        // Optional timezone
        if !cal.timezone.is_empty() && cal.timezone != "UTC" {
            ics_lines.push(format!("X-WR-TIMEZONE:{}", cal.timezone));
        }

        // Bookings as VEVENTs
        let booking_events: String = cal
            .bookings
            .iter()
            .map(crate::types::ics_booking_event)
            .filter(|e| !e.is_empty())
            .collect::<Vec<_>>()
            .join("\r\n");

        if !booking_events.is_empty() {
            ics_lines.push("".to_string()); // blank line before events
            ics_lines.push(booking_events);
        }

        // Availability windows as VEVENTs (optional)
        if !cal.windows.is_empty() {
            ics_lines.push("".to_string()); // blank line
            let window_events: String = crate::types::ics_availability_events(&cal.windows);
            if !window_events.is_empty() {
                ics_lines.push(window_events);
            }
        }

        ics_lines.push("END:VCALENDAR".to_string());

        Ok(ics_lines.join("\r\n"))
    }
}
