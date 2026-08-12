//! libSQL-backed persistence — the replacement for the Python JSONFileStore.
//!
//! Normalized relational schema (one row per entity), so concurrent agents
//! can share a single database file without whole-document write contention.
//! This is the core advantage over the JSON store.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use tokio::sync::Mutex;

use libsql::{params, Connection, Row, Value};

use super::CalendarStore;
use crate::error::{AgentError, Result, StoreError};
use crate::types::{
    Attendee, AvailabilityWindow, Booking, BookingLink, Calendar, ConflictPolicy, RecurrenceRule,
    Status, TimeSlot,
};

const SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS calendars (
    owner_id  TEXT PRIMARY KEY,
    name      TEXT NOT NULL,
    timezone  TEXT NOT NULL DEFAULT 'UTC',
    metadata  TEXT NOT NULL DEFAULT 'null'
);

CREATE TABLE IF NOT EXISTS availability_windows (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    owner_id    TEXT NOT NULL,
    day_of_week INTEGER,
    start_hh    INTEGER NOT NULL,
    start_mm    INTEGER NOT NULL,
    end_hh      INTEGER NOT NULL,
    end_mm      INTEGER NOT NULL,
    label       TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS blocked_periods (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    owner_id  TEXT NOT NULL,
    start_iso TEXT NOT NULL,
    end_iso   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS booking_links (
    id                TEXT PRIMARY KEY,
    owner_id          TEXT NOT NULL,
    title             TEXT NOT NULL,
    duration_minutes  INTEGER NOT NULL,
    max_attendees     INTEGER NOT NULL DEFAULT 1,
    buffer_before_min INTEGER NOT NULL DEFAULT 0,
    buffer_after_min  INTEGER NOT NULL DEFAULT 0,
    min_notice_hours  REAL    NOT NULL DEFAULT 1.0,
    max_days_ahead    INTEGER NOT NULL DEFAULT 30,
    recurrence        TEXT    NOT NULL DEFAULT 'none',
    questions         TEXT    NOT NULL DEFAULT '[]',
    conflict_policy   TEXT    NOT NULL DEFAULT 'reject',
    metadata          TEXT    NOT NULL DEFAULT 'null'
);

CREATE TABLE IF NOT EXISTS bookings (
    id         TEXT PRIMARY KEY,
    owner_id   TEXT NOT NULL,
    title      TEXT NOT NULL,
    start_iso  TEXT NOT NULL,
    end_iso    TEXT NOT NULL,
    status     TEXT NOT NULL DEFAULT 'pending',
    notes      TEXT NOT NULL DEFAULT '',
    metadata   TEXT NOT NULL DEFAULT 'null',
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS attendees (
    id         TEXT PRIMARY KEY,
    booking_id TEXT NOT NULL,
    name       TEXT NOT NULL,
    email      TEXT NOT NULL,
    metadata   TEXT NOT NULL DEFAULT 'null'
);

CREATE INDEX IF NOT EXISTS idx_windows_owner  ON availability_windows(owner_id);
CREATE INDEX IF NOT EXISTS idx_blocked_owner  ON blocked_periods(owner_id);
CREATE INDEX IF NOT EXISTS idx_links_owner    ON booking_links(owner_id);
CREATE INDEX IF NOT EXISTS idx_bookings_owner ON bookings(owner_id);
CREATE INDEX IF NOT EXISTS idx_attendees_booking ON attendees(booking_id);

-- ── CRM ──────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS companies (
    id         TEXT PRIMARY KEY,
    owner_id   TEXT NOT NULL,
    name       TEXT NOT NULL,
    industry   TEXT NOT NULL DEFAULT '',
    website    TEXT NOT NULL DEFAULT '',
    stage      TEXT NOT NULL DEFAULT 'LEAD',
    deal_value REAL NOT NULL DEFAULT 0,
    tags       TEXT NOT NULL DEFAULT '[]',
    notes      TEXT NOT NULL DEFAULT '',
    metadata   TEXT NOT NULL DEFAULT 'null',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS contacts (
    id         TEXT PRIMARY KEY,
    owner_id   TEXT NOT NULL,
    first_name TEXT NOT NULL DEFAULT '',
    last_name  TEXT NOT NULL DEFAULT '',
    email      TEXT NOT NULL DEFAULT '',
    phone      TEXT NOT NULL DEFAULT '',
    company_id TEXT,
    title      TEXT NOT NULL DEFAULT '',
    tags       TEXT NOT NULL DEFAULT '[]',
    is_vip     INTEGER NOT NULL DEFAULT 0,
    notes      TEXT NOT NULL DEFAULT '',
    metadata   TEXT NOT NULL DEFAULT 'null',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    embedding  F32_BLOB(64)
);

CREATE TABLE IF NOT EXISTS deals (
    id             TEXT PRIMARY KEY,
    owner_id       TEXT NOT NULL,
    company_id     TEXT NOT NULL,
    contact_id     TEXT,
    name           TEXT NOT NULL,
    stage          TEXT NOT NULL DEFAULT 'LEAD',
    amount         REAL NOT NULL DEFAULT 0,
    probability    REAL NOT NULL DEFAULT 0.1,
    expected_close TEXT,
    next_action    TEXT NOT NULL DEFAULT '',
    notes          TEXT NOT NULL DEFAULT '',
    metadata       TEXT NOT NULL DEFAULT 'null',
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS interactions (
    id         TEXT PRIMARY KEY,
    owner_id   TEXT NOT NULL,
    contact_id TEXT NOT NULL,
    deal_id    TEXT,
    kind       TEXT NOT NULL DEFAULT 'NOTE',
    direction  TEXT NOT NULL DEFAULT 'OUTBOUND',
    at         TEXT NOT NULL,
    summary    TEXT NOT NULL DEFAULT '',
    metadata   TEXT NOT NULL DEFAULT 'null'
);

CREATE VIRTUAL TABLE IF NOT EXISTS crm_fts USING fts5(
    owner_id UNINDEXED, entity, id UNINDEXED, label, snippet
);

CREATE INDEX IF NOT EXISTS idx_companies_owner  ON companies(owner_id);
CREATE INDEX IF NOT EXISTS idx_contacts_owner   ON contacts(owner_id);
CREATE INDEX IF NOT EXISTS idx_contacts_company ON contacts(company_id);
CREATE INDEX IF NOT EXISTS idx_contacts_phone   ON contacts(phone);
CREATE INDEX IF NOT EXISTS idx_contacts_email   ON contacts(email);
CREATE INDEX IF NOT EXISTS idx_deals_owner      ON deals(owner_id);
CREATE INDEX IF NOT EXISTS idx_deals_company    ON deals(company_id);
CREATE INDEX IF NOT EXISTS idx_interactions_owner   ON interactions(owner_id);
CREATE INDEX IF NOT EXISTS idx_interactions_contact ON interactions(contact_id);
CREATE INDEX IF NOT EXISTS idx_contacts_embedding ON contacts (libsql_vector_idx(embedding));
"#;

/// libSQL-backed [`CalendarStore`].
#[derive(Clone)]
pub struct LibSqlStore {
    conn: Arc<Mutex<Connection>>,
}

impl LibSqlStore {
    /// Open (or create) a libSQL database file at `path` and initialise the schema.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_builder(path).await
    }

    /// Open via a [`libsql::Builder`] (local file).
    pub async fn open_with_builder(path: impl AsRef<Path>) -> Result<Self> {
        let db = libsql::Builder::new_local(path.as_ref())
            .build()
            .await
            .map_err(StoreError::from)?;
        Self::init(db).await
    }

    /// In-memory database (ephemeral — useful for tests).
    pub async fn in_memory() -> Result<Self> {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .map_err(StoreError::from)?;
        Self::init(db).await
    }

    async fn init(db: libsql::Database) -> Result<Self> {
        let conn = db.connect().map_err(StoreError::from)?;
        conn.execute_batch(SCHEMA).await.map_err(StoreError::from)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Wrap an existing open [`libsql::Database`] (schema initialised here).
    pub async fn from_database(db: libsql::Database) -> Result<Self> {
        Self::init(db).await
    }

    /// Access to the underlying connection (used by the CRM store impl).
    pub(crate) fn connection(&self) -> &Arc<Mutex<Connection>> {
        &self.conn
    }
}

// ── enum <-> str helpers ─────────────────────────────────────────────────────

fn status_str(s: Status) -> &'static str {
    match s {
        Status::Pending => "pending",
        Status::Confirmed => "confirmed",
        Status::Cancelled => "cancelled",
        Status::Completed => "completed",
    }
}

fn status_from(s: &str) -> Status {
    match s {
        "confirmed" => Status::Confirmed,
        "cancelled" => Status::Cancelled,
        "completed" => Status::Completed,
        _ => Status::Pending,
    }
}

fn policy_str(p: ConflictPolicy) -> &'static str {
    match p {
        ConflictPolicy::Reject => "reject",
        ConflictPolicy::Warn => "warn",
        ConflictPolicy::Overwrite => "overwrite",
    }
}

fn policy_from(s: &str) -> ConflictPolicy {
    match s {
        "warn" => ConflictPolicy::Warn,
        "overwrite" => ConflictPolicy::Overwrite,
        _ => ConflictPolicy::Reject,
    }
}

fn recurrence_str(r: RecurrenceRule) -> &'static str {
    match r {
        RecurrenceRule::None => "none",
        RecurrenceRule::Daily => "daily",
        RecurrenceRule::Weekly => "weekly",
        RecurrenceRule::Monthly => "monthly",
    }
}

fn recurrence_from(s: &str) -> RecurrenceRule {
    match s {
        "daily" => RecurrenceRule::Daily,
        "weekly" => RecurrenceRule::Weekly,
        "monthly" => RecurrenceRule::Monthly,
        _ => RecurrenceRule::None,
    }
}

// ── row decoding helpers ─────────────────────────────────────────────────────

pub(crate) fn get_text(row: &Row, idx: i32) -> Result<String> {
    match row.get_value(idx).map_err(StoreError::from)? {
        Value::Text(s) => Ok(s),
        Value::Null => Ok(String::new()),
        other => Err(AgentError::Store(StoreError::Other(format!(
            "expected text at col {idx}, got {other:?}"
        )))),
    }
}

pub(crate) fn get_int(row: &Row, idx: i32) -> Result<i64> {
    match row.get_value(idx).map_err(StoreError::from)? {
        Value::Integer(i) => Ok(i),
        Value::Real(r) => Ok(r as i64),
        Value::Null => Ok(0),
        other => Err(AgentError::Store(StoreError::Other(format!(
            "expected integer at col {idx}, got {other:?}"
        )))),
    }
}

pub(crate) fn get_real(row: &Row, idx: i32) -> Result<f64> {
    match row.get_value(idx).map_err(StoreError::from)? {
        Value::Real(r) => Ok(r),
        Value::Integer(i) => Ok(i as f64),
        Value::Null => Ok(0.0),
        other => Err(AgentError::Store(StoreError::Other(format!(
            "expected real at col {idx}, got {other:?}"
        )))),
    }
}

pub(crate) fn get_opt_int(row: &Row, idx: i32) -> Result<Option<i64>> {
    match row.get_value(idx).map_err(StoreError::from)? {
        Value::Integer(i) => Ok(Some(i)),
        Value::Real(r) => Ok(Some(r as i64)),
        Value::Null => Ok(None),
        other => Err(AgentError::Store(StoreError::Other(format!(
            "expected integer|null at col {idx}, got {other:?}"
        )))),
    }
}

pub(crate) fn get_json(row: &Row, idx: i32) -> Result<serde_json::Value> {
    let s = get_text(row, idx)?;
    if s.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(&s).map_err(|e| AgentError::Store(StoreError::Serde(e)))
}

pub(crate) fn get_dt(row: &Row, idx: i32) -> Result<DateTime<Utc>> {
    let s = get_text(row, idx)?;
    DateTime::parse_from_rfc3339(&s)
        .map(|d| d.with_timezone(&Utc))
        .or_else(|_| DateTime::parse_from_rfc3339(&format!("{s}Z")).map(|d| d.with_timezone(&Utc)))
        .map_err(|e| AgentError::Store(StoreError::Other(format!("bad datetime {s:?}: {e}"))))
}

fn to_json_string(v: &serde_json::Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".to_string())
}

/// Reference date for availability-window prototype slots. Only the time-of-day
/// is meaningful for slot generation, so a fixed date keeps things deterministic.
fn proto_dt(hh: i64, mm: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(1970, 1, 1, hh as u32, mm as u32, 0)
        .single()
        .expect("valid proto datetime")
}

fn decode_link(row: &Row) -> Result<BookingLink> {
    let questions: Vec<String> = serde_json::from_value(get_json(row, 10)?).unwrap_or_default();
    Ok(BookingLink {
        id: get_text(row, 0)?,
        owner_id: get_text(row, 1)?,
        title: get_text(row, 2)?,
        duration_minutes: get_int(row, 3)?,
        max_attendees: get_int(row, 4)? as usize,
        buffer_before_min: get_int(row, 5)?,
        buffer_after_min: get_int(row, 6)?,
        min_notice_hours: get_real(row, 7)?,
        max_days_ahead: get_int(row, 8)?,
        recurrence: recurrence_from(&get_text(row, 9)?),
        questions,
        conflict_policy: policy_from(&get_text(row, 11)?),
        metadata: get_json(row, 12)?,
    })
}

#[async_trait]
impl CalendarStore for LibSqlStore {
    async fn save(&self, calendar: &Calendar) -> Result<()> {
        let conn = self.conn.lock().await;
        let tx = conn.transaction().await.map_err(StoreError::from)?;
        let owner = &calendar.owner_id;

        // Wipe the owner's calendar-scoped rows (links are managed separately).
        tx.execute(
            "DELETE FROM attendees WHERE booking_id IN (SELECT id FROM bookings WHERE owner_id = ?)",
            params![owner.as_str()],
        )
        .await
        .map_err(StoreError::from)?;
        tx.execute(
            "DELETE FROM bookings WHERE owner_id = ?",
            params![owner.as_str()],
        )
        .await
        .map_err(StoreError::from)?;
        tx.execute(
            "DELETE FROM availability_windows WHERE owner_id = ?",
            params![owner.as_str()],
        )
        .await
        .map_err(StoreError::from)?;
        tx.execute(
            "DELETE FROM blocked_periods WHERE owner_id = ?",
            params![owner.as_str()],
        )
        .await
        .map_err(StoreError::from)?;
        tx.execute(
            "DELETE FROM calendars WHERE owner_id = ?",
            params![owner.as_str()],
        )
        .await
        .map_err(StoreError::from)?;

        // Insert the calendar.
        tx.execute(
            "INSERT INTO calendars (owner_id, name, timezone, metadata) VALUES (?, ?, ?, ?)",
            params![
                owner.as_str(),
                calendar.name.as_str(),
                calendar.timezone.as_str(),
                to_json_string(&calendar.metadata),
            ],
        )
        .await
        .map_err(StoreError::from)?;

        // Windows.
        for w in &calendar.windows {
            tx.execute(
                "INSERT INTO availability_windows
                 (owner_id, day_of_week, start_hh, start_mm, end_hh, end_mm, label)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    owner.as_str(),
                    w.day_of_week.map(|d| d as i64),
                    w.slot
                        .start
                        .format("%H")
                        .to_string()
                        .parse::<i64>()
                        .unwrap_or(0),
                    w.slot
                        .start
                        .format("%M")
                        .to_string()
                        .parse::<i64>()
                        .unwrap_or(0),
                    w.slot
                        .end
                        .format("%H")
                        .to_string()
                        .parse::<i64>()
                        .unwrap_or(0),
                    w.slot
                        .end
                        .format("%M")
                        .to_string()
                        .parse::<i64>()
                        .unwrap_or(0),
                    w.label.as_str(),
                ],
            )
            .await
            .map_err(StoreError::from)?;
        }

        // Blocked periods.
        for b in &calendar.blocked {
            tx.execute(
                "INSERT INTO blocked_periods (owner_id, start_iso, end_iso) VALUES (?, ?, ?)",
                params![owner.as_str(), b.start.to_rfc3339(), b.end.to_rfc3339(),],
            )
            .await
            .map_err(StoreError::from)?;
        }

        // Bookings + attendees.
        for bk in &calendar.bookings {
            tx.execute(
                "INSERT INTO bookings
                 (id, owner_id, title, start_iso, end_iso, status, notes, metadata, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    bk.id.as_str(),
                    owner.as_str(),
                    bk.title.as_str(),
                    bk.slot.start.to_rfc3339(),
                    bk.slot.end.to_rfc3339(),
                    status_str(bk.status),
                    bk.notes.as_str(),
                    to_json_string(&bk.metadata),
                    bk.created_at.to_rfc3339(),
                ],
            )
            .await
            .map_err(StoreError::from)?;

            for a in &bk.attendees {
                tx.execute(
                    "INSERT INTO attendees (id, booking_id, name, email, metadata) VALUES (?, ?, ?, ?, ?)",
                    params![
                        a.id.as_str(),
                        bk.id.as_str(),
                        a.name.as_str(),
                        a.email.as_str(),
                        to_json_string(&a.metadata),
                    ],
                )
                .await
                .map_err(StoreError::from)?;
            }
        }

        tx.commit().await.map_err(StoreError::from)?;
        Ok(())
    }

    async fn load(&self, owner_id: &str) -> Result<Option<Calendar>> {
        let conn = self.conn.lock().await;

        // Calendar row.
        let mut rows = conn
            .query(
                "SELECT owner_id, name, timezone, metadata FROM calendars WHERE owner_id = ?",
                params![owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        let Some(row) = rows.next().await.map_err(StoreError::from)? else {
            return Ok(None);
        };
        let mut cal = Calendar::new(get_text(&row, 0)?, get_text(&row, 1)?);
        cal.timezone = get_text(&row, 2)?;
        cal.metadata = get_json(&row, 3)?;

        // Windows.
        let mut rows = conn
            .query(
                "SELECT day_of_week, start_hh, start_mm, end_hh, end_mm, label
                 FROM availability_windows WHERE owner_id = ? ORDER BY id",
                params![owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        while let Some(row) = rows.next().await.map_err(StoreError::from)? {
            let day_of_week = get_opt_int(&row, 0)?.map(|d| d as u8);
            let slot = TimeSlot::unchecked(
                proto_dt(get_int(&row, 1)?, get_int(&row, 2)?),
                proto_dt(get_int(&row, 3)?, get_int(&row, 4)?),
            );
            cal.windows.push(AvailabilityWindow::new(
                slot,
                day_of_week,
                get_text(&row, 5)?,
            ));
        }

        // Blocked periods.
        let mut rows = conn
            .query(
                "SELECT start_iso, end_iso FROM blocked_periods WHERE owner_id = ? ORDER BY id",
                params![owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        while let Some(row) = rows.next().await.map_err(StoreError::from)? {
            cal.blocked
                .push(TimeSlot::unchecked(get_dt(&row, 0)?, get_dt(&row, 1)?));
        }

        // Bookings.
        let mut rows = conn
            .query(
                "SELECT id, title, start_iso, end_iso, status, notes, metadata, created_at
                 FROM bookings WHERE owner_id = ? ORDER BY start_iso",
                params![owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        while let Some(row) = rows.next().await.map_err(StoreError::from)? {
            let booking_id = get_text(&row, 0)?;
            let mut booking = Booking {
                id: booking_id.clone(),
                title: get_text(&row, 1)?,
                owner_id: owner_id.to_string(),
                slot: TimeSlot::unchecked(get_dt(&row, 2)?, get_dt(&row, 3)?),
                status: status_from(&get_text(&row, 4)?),
                attendees: Vec::new(),
                notes: get_text(&row, 5)?,
                metadata: get_json(&row, 6)?,
                created_at: get_dt(&row, 7)?,
            };

            // Attendees for this booking.
            let mut arows = conn
                .query(
                    "SELECT id, name, email, metadata FROM attendees WHERE booking_id = ? ORDER BY rowid",
                    params![booking_id.as_str()],
                )
                .await
                .map_err(StoreError::from)?;
            while let Some(arow) = arows.next().await.map_err(StoreError::from)? {
                booking.attendees.push(Attendee {
                    id: get_text(&arow, 0)?,
                    name: get_text(&arow, 1)?,
                    email: get_text(&arow, 2)?,
                    metadata: get_json(&arow, 3)?,
                });
            }

            cal.bookings.push(booking);
        }

        Ok(Some(cal))
    }

    async fn delete(&self, owner_id: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let tx = conn.transaction().await.map_err(StoreError::from)?;
        tx.execute(
            "DELETE FROM attendees WHERE booking_id IN (SELECT id FROM bookings WHERE owner_id = ?)",
            params![owner_id],
        )
        .await
        .map_err(StoreError::from)?;
        tx.execute("DELETE FROM bookings WHERE owner_id = ?", params![owner_id])
            .await
            .map_err(StoreError::from)?;
        tx.execute(
            "DELETE FROM availability_windows WHERE owner_id = ?",
            params![owner_id],
        )
        .await
        .map_err(StoreError::from)?;
        tx.execute(
            "DELETE FROM blocked_periods WHERE owner_id = ?",
            params![owner_id],
        )
        .await
        .map_err(StoreError::from)?;
        tx.execute(
            "DELETE FROM booking_links WHERE owner_id = ?",
            params![owner_id],
        )
        .await
        .map_err(StoreError::from)?;
        let n = tx
            .execute(
                "DELETE FROM calendars WHERE owner_id = ?",
                params![owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        tx.commit().await.map_err(StoreError::from)?;
        Ok(n > 0)
    }

    async fn list_ids(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        let mut rows = conn
            .query(
                "SELECT owner_id FROM calendars ORDER BY owner_id",
                params![],
            )
            .await
            .map_err(StoreError::from)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(StoreError::from)? {
            out.push(get_text(&row, 0)?);
        }
        Ok(out)
    }

    async fn save_link(&self, link: &BookingLink) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO booking_links
             (id, owner_id, title, duration_minutes, max_attendees, buffer_before_min,
              buffer_after_min, min_notice_hours, max_days_ahead, recurrence, questions,
              conflict_policy, metadata)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
              owner_id=excluded.owner_id, title=excluded.title,
              duration_minutes=excluded.duration_minutes, max_attendees=excluded.max_attendees,
              buffer_before_min=excluded.buffer_before_min, buffer_after_min=excluded.buffer_after_min,
              min_notice_hours=excluded.min_notice_hours, max_days_ahead=excluded.max_days_ahead,
              recurrence=excluded.recurrence, questions=excluded.questions,
              conflict_policy=excluded.conflict_policy, metadata=excluded.metadata",
            params![
                link.id.as_str(),
                link.owner_id.as_str(),
                link.title.as_str(),
                link.duration_minutes,
                link.max_attendees as i64,
                link.buffer_before_min,
                link.buffer_after_min,
                link.min_notice_hours,
                link.max_days_ahead,
                recurrence_str(link.recurrence),
                serde_json::to_string(&link.questions).unwrap_or_else(|_| "[]".to_string()),
                policy_str(link.conflict_policy),
                to_json_string(&link.metadata),
            ],
        )
        .await
        .map_err(StoreError::from)?;
        Ok(())
    }

    async fn load_link(&self, link_id: &str) -> Result<Option<BookingLink>> {
        let conn = self.conn.lock().await;
        let mut rows = conn
            .query(
                "SELECT id, owner_id, title, duration_minutes, max_attendees, buffer_before_min,
                        buffer_after_min, min_notice_hours, max_days_ahead, recurrence, questions,
                        conflict_policy, metadata
                 FROM booking_links WHERE id = ?",
                params![link_id],
            )
            .await
            .map_err(StoreError::from)?;
        match rows.next().await.map_err(StoreError::from)? {
            Some(row) => Ok(Some(decode_link(&row)?)),
            None => Ok(None),
        }
    }

    async fn load_links(&self, owner_id: &str) -> Result<Vec<BookingLink>> {
        let conn = self.conn.lock().await;
        let mut rows = conn
            .query(
                "SELECT id, owner_id, title, duration_minutes, max_attendees, buffer_before_min,
                        buffer_after_min, min_notice_hours, max_days_ahead, recurrence, questions,
                        conflict_policy, metadata
                 FROM booking_links WHERE owner_id = ? ORDER BY rowid",
                params![owner_id],
            )
            .await
            .map_err(StoreError::from)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(StoreError::from)? {
            out.push(decode_link(&row)?);
        }
        Ok(out)
    }

    async fn delete_link(&self, link_id: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute("DELETE FROM booking_links WHERE id = ?", params![link_id])
            .await
            .map_err(StoreError::from)?;
        Ok(n > 0)
    }
}
