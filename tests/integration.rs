//! Integration tests for agentcal — ported from the Python `test_agentcal.py`.
//!
//! The whole behaviour suite runs against both `MemoryStore` and `LibSqlStore`
//! (in-memory libSQL) via the `behaviour_suite!` macro, plus a dedicated
//! persistence round-trip test against a file-backed libSQL database.

use agentcal::{AgentCal, Attendee, ConflictPolicy, LibSqlStore, LinkParams, MemoryStore, Status};
use chrono::{DateTime, Duration, TimeZone, Utc};

fn future(hours: f64) -> DateTime<Utc> {
    Utc::now() + Duration::milliseconds((hours * 3600.0 * 1000.0) as i64)
}

/// Build an AgentCal on an in-memory libSQL database.
async fn libsql_cal() -> AgentCal {
    AgentCal::new(LibSqlStore::in_memory().await.expect("libsql store"))
}

/// The full behaviour suite, generic over the store constructor.
macro_rules! behaviour_suite {
    ($modname:ident, $make:expr) => {
        mod $modname {
            use super::*;

            async fn make() -> AgentCal {
                $make().await
            }

            // ── Calendar management ────────────────────────────────────────

            #[tokio::test]
            async fn test_create_and_get() {
                let c = make().await;
                c.create_calendar_simple("alice", "Alice's Cal")
                    .await
                    .unwrap();
                let got = c.get_calendar("alice").await.unwrap();
                assert_eq!(got.name, "Alice's Cal");
            }

            #[tokio::test]
            async fn test_duplicate_owner_rejected() {
                let c = make().await;
                c.create_calendar_simple("alice", "A").await.unwrap();
                assert!(c.create_calendar_simple("alice", "B").await.is_err());
            }

            #[tokio::test]
            async fn test_delete() {
                let c = make().await;
                c.create_calendar_simple("alice", "A").await.unwrap();
                c.delete_calendar("alice").await.unwrap();
                assert!(c.get_calendar("alice").await.is_err());
            }

            #[tokio::test]
            async fn test_list_calendars() {
                let c = make().await;
                c.create_calendar_simple("alice", "A").await.unwrap();
                c.create_calendar_simple("bob", "B").await.unwrap();
                let ids = c.list_calendars().await.unwrap();
                assert!(ids.contains(&"alice".to_string()));
                assert!(ids.contains(&"bob".to_string()));
            }

            // ── Availability windows ───────────────────────────────────────

            #[tokio::test]
            async fn test_add_window() {
                let c = make().await;
                c.create_calendar_simple("alice", "Alice").await.unwrap();
                c.add_window("alice", None, "09:00", "17:00", "business hours")
                    .await
                    .unwrap();
            }

            #[tokio::test]
            async fn test_clear_windows() {
                let c = make().await;
                c.create_calendar_simple("alice", "Alice").await.unwrap();
                c.add_window("alice", None, "09:00", "17:00", "")
                    .await
                    .unwrap();
                c.clear_windows("alice").await.unwrap();
                let info = c.get_calendar("alice").await.unwrap();
                assert!(info.windows.is_empty());
            }

            #[tokio::test]
            async fn test_invalid_owner() {
                let c = make().await;
                assert!(c
                    .add_window("nobody", None, "09:00", "17:00", "")
                    .await
                    .is_err());
            }

            // ── Slot discovery ─────────────────────────────────────────────

            async fn setup_slots(c: &AgentCal) -> String {
                c.create_calendar_simple("alice", "Alice").await.unwrap();
                c.add_window("alice", None, "09:00", "17:00", "")
                    .await
                    .unwrap();
                let link = c
                    .create_link(
                        "alice",
                        LinkParams::new("30-min chat", 30).min_notice_hours(1.0),
                    )
                    .await
                    .unwrap();
                link.id
            }

            #[tokio::test]
            async fn test_slots_returned() {
                let c = make().await;
                let lid = setup_slots(&c).await;
                let slots = c
                    .get_slots("alice", &lid, Some(future(2.0)), Some(future(26.0)), None)
                    .await
                    .unwrap();
                assert!(!slots.is_empty());
            }

            #[tokio::test]
            async fn test_slot_duration_matches() {
                let c = make().await;
                let lid = setup_slots(&c).await;
                let slots = c
                    .get_slots("alice", &lid, Some(future(2.0)), Some(future(26.0)), None)
                    .await
                    .unwrap();
                for s in slots {
                    assert_eq!(s.duration_minutes(), 30);
                }
            }

            #[tokio::test]
            async fn test_blocked_period_excluded() {
                let c = make().await;
                let lid = setup_slots(&c).await;
                let start = future(5.0);
                let end = future(5.0) + Duration::hours(8);
                c.block("alice", start, end).await.unwrap();
                let slots = c
                    .get_slots("alice", &lid, Some(start), Some(end), None)
                    .await
                    .unwrap();
                assert!(slots.is_empty());
            }

            #[tokio::test]
            async fn test_limit_param() {
                let c = make().await;
                let lid = setup_slots(&c).await;
                let slots = c
                    .get_slots(
                        "alice",
                        &lid,
                        Some(future(2.0)),
                        Some(future(50.0)),
                        Some(5),
                    )
                    .await
                    .unwrap();
                assert!(slots.len() <= 5);
            }

            // ── Booking ────────────────────────────────────────────────────

            async fn setup_booking(c: &AgentCal) -> String {
                c.create_calendar_simple("alice", "Alice").await.unwrap();
                c.add_window("alice", None, "09:00", "17:00", "")
                    .await
                    .unwrap();
                c.create_link("alice", LinkParams::new("Chat", 30).min_notice_hours(0.0))
                    .await
                    .unwrap()
                    .id
            }

            async fn first_slot(c: &AgentCal, lid: &str) -> agentcal::TimeSlot {
                c.get_slots("alice", lid, None, None, Some(1))
                    .await
                    .unwrap()
                    .into_iter()
                    .next()
                    .expect("a slot")
            }

            #[tokio::test]
            async fn test_book_success() {
                let c = make().await;
                let lid = setup_booking(&c).await;
                let slot = first_slot(&c, &lid).await;
                let r = c
                    .book(
                        "alice",
                        &lid,
                        slot,
                        vec![Attendee::new("Bob", "bob@test.com")],
                        "",
                        serde_json::Value::Null,
                    )
                    .await
                    .unwrap();
                assert_eq!(r.booking.status, Status::Confirmed);
            }

            #[tokio::test]
            async fn test_double_book_rejected() {
                let c = make().await;
                let lid = setup_booking(&c).await;
                let slot = first_slot(&c, &lid).await;
                c.book(
                    "alice",
                    &lid,
                    slot.clone(),
                    vec![Attendee::new("Bob", "b@t.com")],
                    "",
                    serde_json::Value::Null,
                )
                .await
                .unwrap();
                assert!(c
                    .book(
                        "alice",
                        &lid,
                        slot,
                        vec![Attendee::new("Eve", "e@t.com")],
                        "",
                        serde_json::Value::Null
                    )
                    .await
                    .is_err());
            }

            #[tokio::test]
            async fn test_cancel() {
                let c = make().await;
                let lid = setup_booking(&c).await;
                let slot = first_slot(&c, &lid).await;
                let r = c
                    .book(
                        "alice",
                        &lid,
                        slot,
                        vec![Attendee::new("Bob", "b@t.com")],
                        "",
                        serde_json::Value::Null,
                    )
                    .await
                    .unwrap();
                let bid = r.booking.id;
                c.cancel("alice", &bid, "changed plans").await.unwrap();
                let b = c.get_booking("alice", &bid).await.unwrap();
                assert_eq!(b.status, Status::Cancelled);
            }

            #[tokio::test]
            async fn test_confirm() {
                let c = make().await;
                let lid = setup_booking(&c).await;
                let slot = first_slot(&c, &lid).await;
                let r = c
                    .book(
                        "alice",
                        &lid,
                        slot,
                        vec![Attendee::new("Bob", "b@t.com")],
                        "",
                        serde_json::Value::Null,
                    )
                    .await
                    .unwrap();
                let bid = r.booking.id;
                // Book creates CONFIRMED; confirm is only valid from PENDING,
                // so confirming an already-confirmed booking is an error.
                assert!(c.confirm("alice", &bid).await.is_err());
            }

            #[tokio::test]
            async fn test_complete() {
                let c = make().await;
                let lid = setup_booking(&c).await;
                let slot = first_slot(&c, &lid).await;
                let r = c
                    .book(
                        "alice",
                        &lid,
                        slot,
                        vec![Attendee::new("Bob", "b@t.com")],
                        "",
                        serde_json::Value::Null,
                    )
                    .await
                    .unwrap();
                let b = c.complete("alice", &r.booking.id).await.unwrap();
                assert_eq!(b.status, Status::Completed);
            }

            #[tokio::test]
            async fn test_reschedule() {
                let c = make().await;
                let lid = setup_booking(&c).await;
                let slots = c
                    .get_slots("alice", &lid, None, None, Some(3))
                    .await
                    .unwrap();
                let first = slots[0].clone();
                let second = slots[2].clone();
                let r = c
                    .book(
                        "alice",
                        &lid,
                        first,
                        vec![Attendee::new("Bob", "b@t.com")],
                        "",
                        serde_json::Value::Null,
                    )
                    .await
                    .unwrap();
                let r2 = c
                    .reschedule("alice", &r.booking.id, &lid, second.clone())
                    .await
                    .unwrap();
                assert_eq!(r2.booking.slot.start, second.start);
            }

            // ── Conflict policies ──────────────────────────────────────────

            async fn make_link_policy(c: &AgentCal, policy: ConflictPolicy) -> String {
                c.create_calendar_simple("alice", "Alice").await.unwrap();
                c.add_window("alice", None, "09:00", "17:00", "")
                    .await
                    .unwrap();
                c.create_link(
                    "alice",
                    LinkParams::new("Chat", 30)
                        .min_notice_hours(0.0)
                        .conflict_policy(policy),
                )
                .await
                .unwrap()
                .id
            }

            #[tokio::test]
            async fn test_warn_policy_allows_overlap() {
                let c = make().await;
                let lid = make_link_policy(&c, ConflictPolicy::Warn).await;
                let slot = first_slot(&c, &lid).await;
                c.book(
                    "alice",
                    &lid,
                    slot.clone(),
                    vec![Attendee::new("A", "a@a.com")],
                    "",
                    serde_json::Value::Null,
                )
                .await
                .unwrap();
                let r2 = c
                    .book(
                        "alice",
                        &lid,
                        slot,
                        vec![Attendee::new("B", "b@b.com")],
                        "",
                        serde_json::Value::Null,
                    )
                    .await
                    .unwrap();
                let m = r2.message.to_lowercase();
                assert!(
                    m.contains("conflict") || m.contains("overlap"),
                    "message: {m}"
                );
            }

            #[tokio::test]
            async fn test_overwrite_policy_cancels_previous() {
                let c = make().await;
                let lid = make_link_policy(&c, ConflictPolicy::Overwrite).await;
                let slot = first_slot(&c, &lid).await;
                let r1 = c
                    .book(
                        "alice",
                        &lid,
                        slot.clone(),
                        vec![Attendee::new("A", "a@a.com")],
                        "",
                        serde_json::Value::Null,
                    )
                    .await
                    .unwrap();
                let old_id = r1.booking.id;
                c.book(
                    "alice",
                    &lid,
                    slot,
                    vec![Attendee::new("B", "b@b.com")],
                    "",
                    serde_json::Value::Null,
                )
                .await
                .unwrap();
                let b = c.get_booking("alice", &old_id).await.unwrap();
                assert_eq!(b.status, Status::Cancelled);
            }

            // ── Group bookings ─────────────────────────────────────────────

            async fn setup_group(c: &AgentCal) -> String {
                c.create_calendar_simple("alice", "Alice").await.unwrap();
                c.add_window("alice", None, "09:00", "17:00", "")
                    .await
                    .unwrap();
                c.create_link(
                    "alice",
                    LinkParams::new("Workshop", 30)
                        .max_attendees(3)
                        .min_notice_hours(0.0),
                )
                .await
                .unwrap()
                .id
            }

            #[tokio::test]
            async fn test_multi_attendee_book() {
                let c = make().await;
                let lid = setup_group(&c).await;
                let slot = first_slot(&c, &lid).await;
                let r = c
                    .book(
                        "alice",
                        &lid,
                        slot,
                        vec![Attendee::new("A", "a@t.com"), Attendee::new("B", "b@t.com")],
                        "",
                        serde_json::Value::Null,
                    )
                    .await
                    .unwrap();
                assert_eq!(r.booking.attendees.len(), 2);
            }

            #[tokio::test]
            async fn test_add_attendee() {
                let c = make().await;
                let lid = setup_group(&c).await;
                let slot = first_slot(&c, &lid).await;
                let r = c
                    .book(
                        "alice",
                        &lid,
                        slot,
                        vec![Attendee::new("A", "a@t.com")],
                        "",
                        serde_json::Value::Null,
                    )
                    .await
                    .unwrap();
                let b = c
                    .add_attendee("alice", &r.booking.id, &lid, Attendee::new("B", "b@t.com"))
                    .await
                    .unwrap();
                assert_eq!(b.attendees.len(), 2);
            }

            #[tokio::test]
            async fn test_max_attendees_enforced() {
                let c = make().await;
                let lid = setup_group(&c).await;
                let slot = first_slot(&c, &lid).await;
                let r = c
                    .book(
                        "alice",
                        &lid,
                        slot,
                        vec![
                            Attendee::new("A", "a@t.com"),
                            Attendee::new("B", "b@t.com"),
                            Attendee::new("C", "c@t.com"),
                        ],
                        "",
                        serde_json::Value::Null,
                    )
                    .await
                    .unwrap();
                assert!(c
                    .add_attendee("alice", &r.booking.id, &lid, Attendee::new("D", "d@t.com"))
                    .await
                    .is_err());
            }

            #[tokio::test]
            async fn test_remove_attendee() {
                let c = make().await;
                let lid = setup_group(&c).await;
                let slot = first_slot(&c, &lid).await;
                let r = c
                    .book(
                        "alice",
                        &lid,
                        slot,
                        vec![Attendee::new("A", "a@t.com"), Attendee::new("B", "b@t.com")],
                        "",
                        serde_json::Value::Null,
                    )
                    .await
                    .unwrap();
                let b = c
                    .remove_attendee("alice", &r.booking.id, "a@t.com")
                    .await
                    .unwrap();
                assert_eq!(b.attendees.len(), 1);
            }

            // ── Mutual slots ───────────────────────────────────────────────

            async fn setup_mutual(c: &AgentCal) -> String {
                for owner in ["alice", "bob"] {
                    c.create_calendar_simple(owner, owner).await.unwrap();
                    c.add_window(owner, None, "09:00", "17:00", "")
                        .await
                        .unwrap();
                }
                c.create_link(
                    "alice",
                    LinkParams::new("Joint meeting", 30).min_notice_hours(0.0),
                )
                .await
                .unwrap()
                .id
            }

            #[tokio::test]
            async fn test_mutual_slots_non_empty() {
                let c = make().await;
                let lid = setup_mutual(&c).await;
                let slots = c
                    .find_mutual_slots(
                        &["alice", "bob"],
                        &lid,
                        Some(future(2.0)),
                        Some(future(26.0)),
                        None,
                    )
                    .await
                    .unwrap();
                assert!(!slots.is_empty());
            }

            #[tokio::test]
            async fn test_no_mutual_when_one_blocked() {
                let c = make().await;
                let lid = setup_mutual(&c).await;
                let start = future(2.0);
                let end = future(26.0);
                c.block("bob", start, end).await.unwrap();
                let slots = c
                    .find_mutual_slots(&["alice", "bob"], &lid, Some(start), Some(end), None)
                    .await
                    .unwrap();
                assert!(slots.is_empty());
            }

            // ── Query helpers ──────────────────────────────────────────────

            async fn setup_query(c: &AgentCal) -> String {
                c.create_calendar_simple("alice", "Alice").await.unwrap();
                c.add_window("alice", None, "09:00", "17:00", "")
                    .await
                    .unwrap();
                c.create_link("alice", LinkParams::new("Chat", 30).min_notice_hours(0.0))
                    .await
                    .unwrap()
                    .id
            }

            #[tokio::test]
            async fn test_upcoming() {
                let c = make().await;
                let lid = setup_query(&c).await;
                let slot = first_slot(&c, &lid).await;
                c.book(
                    "alice",
                    &lid,
                    slot,
                    vec![Attendee::new("Bob", "b@t.com")],
                    "",
                    serde_json::Value::Null,
                )
                .await
                .unwrap();
                let up = c.upcoming("alice", 10).await.unwrap();
                assert_eq!(up.len(), 1);
            }

            #[tokio::test]
            async fn test_summary() {
                let c = make().await;
                let _lid = setup_query(&c).await;
                let s = c.summary("alice").await.unwrap();
                assert!(s.total_bookings <= 1); // field exists
                assert_eq!(s.owner_id, "alice");
            }

            #[tokio::test]
            async fn test_list_bookings_by_status() {
                let c = make().await;
                let lid = setup_query(&c).await;
                let slot = first_slot(&c, &lid).await;
                let bk = c
                    .book(
                        "alice",
                        &lid,
                        slot,
                        vec![Attendee::new("Bob", "b@t.com")],
                        "",
                        serde_json::Value::Null,
                    )
                    .await
                    .unwrap();
                c.cancel("alice", &bk.booking.id, "").await.unwrap();
                let list = c
                    .list_bookings("alice", Some(Status::Cancelled), None, None)
                    .await
                    .unwrap();
                assert_eq!(list.len(), 1);
            }

            // ── ICS export ─────────────────────────────────────────────────

            async fn setup_ics(c: &AgentCal) -> (String, String) {
                c.create_calendar_simple("alice", "Alice").await.unwrap();
                c.add_window("alice", None, "09:00", "17:00", "business hours")
                    .await
                    .unwrap();
                let lid = c
                    .create_link("alice", LinkParams::new("Chat", 30).min_notice_hours(0.0))
                    .await
                    .unwrap()
                    .id;
                let slot = first_slot(c, &lid).await;
                let bk = c
                    .book(
                        "alice",
                        &lid,
                        slot,
                        vec![Attendee::new("Bob", "bob@t.com")],
                        "initial sync",
                        serde_json::Value::Null,
                    )
                    .await
                    .unwrap();
                (bk.booking.id, lid)
            }

            #[tokio::test]
            async fn test_to_ics_wraps_vcalendar() {
                let c = make().await;
                let _ = setup_ics(&c).await;
                let ics = c.to_ics("alice").await.unwrap();
                assert!(ics.starts_with("BEGIN:VCALENDAR\r\n"), "ics: {ics}");
                assert!(ics.trim_end().ends_with("END:VCALENDAR"), "ics: {ics}");
                assert!(ics.contains("VERSION:2.0"), "ics: {ics}");
                assert!(
                    ics.contains("PRODID:-//agentcal//agentcal.rs//EN"),
                    "ics: {ics}"
                );
                assert!(ics.contains("CALSCALE:GREGORIAN"), "ics: {ics}");
            }

            #[tokio::test]
            async fn test_to_ics_booking_event() {
                let c = make().await;
                let (bid, _lid) = setup_ics(&c).await;
                let ics = c.to_ics("alice").await.unwrap();
                assert!(ics.contains(&format!("UID:{bid}")), "ics: {ics}");
                assert!(ics.contains("SUMMARY:Chat"), "ics: {ics}");
                assert!(ics.contains("DESCRIPTION:initial sync"), "ics: {ics}");
                assert!(ics.contains("STATUS:CONFIRMED"), "ics: {ics}");
                assert!(
                    ics.contains("ATTENDEE;CN=Bob:mailto:bob@t.com"),
                    "ics: {ics}"
                );
            }

            #[tokio::test]
            async fn test_to_ics_datetime_format() {
                let c = make().await;
                let _ = setup_ics(&c).await;
                let ics = c.to_ics("alice").await.unwrap();
                let mut checked = 0;
                for line in ics.lines() {
                    for prefix in ["DTSTART:", "DTEND:"] {
                        if let Some(val) = line.strip_prefix(prefix) {
                            // UTC basic format: YYYYMMDDTHHMMSSZ (16 chars)
                            assert_eq!(val.len(), 16, "bad {prefix} in {line}");
                            assert!(val.ends_with('Z'), "expected UTC 'Z' suffix: {line}");
                            assert_eq!(val.as_bytes()[8], b'T', "missing 'T' separator: {line}");
                            checked += 1;
                        }
                    }
                }
                assert!(checked >= 2, "expected a DTSTART/DTEND pair, got {checked}");
            }

            #[tokio::test]
            async fn test_to_ics_empty_calendar() {
                let c = make().await;
                c.create_calendar_simple("alice", "Alice").await.unwrap();
                let ics = c.to_ics("alice").await.unwrap();
                assert!(ics.contains("BEGIN:VCALENDAR"), "ics: {ics}");
                assert!(ics.trim_end().ends_with("END:VCALENDAR"), "ics: {ics}");
                assert!(!ics.contains("BEGIN:VEVENT"), "no events expected: {ics}");
            }

            #[tokio::test]
            async fn test_to_ics_windows_present() {
                let c = make().await;
                let _ = setup_ics(&c).await; // 1 window + 1 booking
                let ics = c.to_ics("alice").await.unwrap();
                assert!(ics.contains("SUMMARY:business hours"), "ics: {ics}");
                assert_eq!(ics.matches("BEGIN:VEVENT").count(), 2, "ics: {ics}");
            }

            #[tokio::test]
            async fn test_to_ics_timezone() {
                let c = make().await;
                c.create_calendar(
                    "alice",
                    "Alice",
                    "America/New_York",
                    serde_json::Value::Null,
                )
                .await
                .unwrap();
                let ics = c.to_ics("alice").await.unwrap();
                assert!(ics.contains("X-WR-TIMEZONE:America/New_York"), "ics: {ics}");
            }

            #[tokio::test]
            async fn test_to_ics_unknown_owner() {
                let c = make().await;
                assert!(c.to_ics("nobody").await.is_err());
            }
        }
    };
}

// Run the entire suite against the in-memory store…
behaviour_suite!(memory, || async { AgentCal::new(MemoryStore::new()) });
// …and against libSQL (in-memory database).
behaviour_suite!(libsql, libsql_cal);

// ── libSQL file persistence round-trip ───────────────────────────────────────
// Reproduces the Python `TestJSONFileStore::test_roundtrip`, but proves that
// links AND bookings survive a process restart (which the JSON version did not
// do for links).

#[tokio::test]
async fn test_libsql_file_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("agentcal.db");

    let bid;
    let lid;
    {
        let store = LibSqlStore::open(&db_path).await.expect("open store");
        let c = AgentCal::new(store);
        c.create_calendar_simple("alice", "Alice").await.unwrap();
        c.add_window("alice", None, "09:00", "17:00", "")
            .await
            .unwrap();
        let link = c
            .create_link("alice", LinkParams::new("Chat", 30).min_notice_hours(0.0))
            .await
            .unwrap();
        lid = link.id;
        let slot = c
            .get_slots("alice", &lid, None, None, Some(1))
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let bk = c
            .book(
                "alice",
                &lid,
                slot,
                vec![Attendee::new("Bob", "b@t.com")],
                "",
                serde_json::Value::Null,
            )
            .await
            .unwrap();
        bid = bk.booking.id;
    }

    // Reload from disk with a fresh store + facade.
    {
        let store = LibSqlStore::open(&db_path).await.expect("reopen store");
        let c2 = AgentCal::new(store);
        let b = c2
            .get_booking("alice", &bid)
            .await
            .expect("booking persisted");
        assert_eq!(b.id, bid);
        // Links persisted too (improvement over the Python JSON store).
        let link = c2.get_link(&lid).await.expect("link persisted");
        assert_eq!(link.id, lid);
        assert_eq!(link.title, "Chat");
    }
}

// ── Pure-function unit checks (no store) ─────────────────────────────────────

#[test]
fn test_timeslot_validation() {
    let start = Utc.with_ymd_and_hms(2026, 1, 1, 9, 0, 0).unwrap();
    let end = Utc.with_ymd_and_hms(2026, 1, 1, 8, 0, 0).unwrap();
    assert!(agentcal::TimeSlot::new(start, end).is_err());
}

#[test]
fn test_parse_iso() {
    let dt = agentcal::parse_iso("2026-08-03T09:00:00+00:00").unwrap();
    assert_eq!(dt, Utc.with_ymd_and_hms(2026, 8, 3, 9, 0, 0).unwrap());
}

// confirm(): Pending → Confirmed transition, exercised directly on an in-memory
// Calendar via the pure scheduler fn (the facade's `book` creates CONFIRMED).
#[test]
fn test_confirm_pending_transition() {
    use agentcal::scheduler;
    use agentcal::{Booking, Calendar};

    let mut cal = Calendar::new("alice", "Alice");
    let slot = agentcal::TimeSlot::new(future(2.0), future(2.0) + Duration::minutes(30)).unwrap();
    cal.bookings.push(Booking::new(
        slot,
        "Chat",
        "alice",
        vec![],
        "",
        serde_json::Value::Null,
        Status::Pending,
    ));
    let bid = cal.bookings[0].id.clone();

    let confirmed = scheduler::confirm(&mut cal, &bid).unwrap();
    assert_eq!(confirmed.status, Status::Confirmed);
    // Confirming again (now Confirmed) is rejected.
    assert!(scheduler::confirm(&mut cal, &bid).is_err());
}

// ── ICS round-trip (structure sanity on the emitted text) ────────────────────

#[tokio::test]
async fn test_ics_roundtrip_structure() {
    let c = AgentCal::new(MemoryStore::new());
    c.create_calendar_simple("alice", "Alice").await.unwrap();
    c.add_window("alice", None, "09:00", "17:00", "business hours")
        .await
        .unwrap();
    let lid = c
        .create_link("alice", LinkParams::new("Chat", 30).min_notice_hours(0.0))
        .await
        .unwrap()
        .id;
    let slot = c
        .get_slots("alice", &lid, None, None, Some(1))
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    c.book(
        "alice",
        &lid,
        slot,
        vec![Attendee::new("Bob", "bob@t.com")],
        "initial sync",
        serde_json::Value::Null,
    )
    .await
    .unwrap();

    let ics = c.to_ics("alice").await.unwrap();

    // Balanced BEGIN/END pairs.
    assert_eq!(
        ics.matches("BEGIN:VEVENT").count(),
        ics.matches("END:VEVENT").count(),
        "unbalanced VEVENTs:\n{ics}"
    );
    // 1 booking + 1 availability window.
    assert_eq!(ics.matches("BEGIN:VEVENT").count(), 2, "ics: {ics}");

    // Every VEVENT carries the RFC-required core properties.
    for event in ics.split("BEGIN:VEVENT").skip(1) {
        for prop in ["UID:", "DTSTART:", "DTEND:", "SUMMARY:", "END:VEVENT"] {
            assert!(event.contains(prop), "missing {prop} in:\n{event}");
        }
    }
}

// ── Error helpers (Python-compat wrappers in `error`) ────────────────────────

#[test]
fn test_to_dict_ok() {
    let v = agentcal::error::to_dict(Ok(42i32));
    assert_eq!(v["ok"], true);
    assert_eq!(v["data"], 42);
}

#[test]
fn test_to_dict_err() {
    let err: agentcal::Result<()> = Err(agentcal::AgentError::CalendarNotFound("alice".into()));
    let v = agentcal::error::to_dict(err);
    assert_eq!(v["ok"], false);
    assert_eq!(v["reason"], "calendar \"alice\" not found");
}

#[test]
fn test_unwrap_or_none() {
    let ok: agentcal::Result<i32> = Ok(7);
    assert_eq!(agentcal::error::unwrap_or_none(ok), Some(7));

    let err: agentcal::Result<i32> = Err(agentcal::AgentError::BookingFull);
    assert_eq!(agentcal::error::unwrap_or_none(err), None);
}

#[test]
fn test_unwrap_or_else() {
    let ok: agentcal::Result<i32> = Ok(5);
    assert_eq!(agentcal::error::unwrap_or_else(ok, |_| 99), 5);

    let err: agentcal::Result<i32> = Err(agentcal::AgentError::Conflict("nope".into()));
    let got = agentcal::error::unwrap_or_else(err, |e| {
        assert_eq!(e.to_string(), "nope");
        99
    });
    assert_eq!(got, 99);
}

#[test]
fn test_status_display() {
    assert_eq!(Status::Pending.to_string(), "Pending");
    assert_eq!(Status::Confirmed.to_string(), "Confirmed");
    assert_eq!(Status::Cancelled.to_string(), "Cancelled");
    assert_eq!(Status::Completed.to_string(), "Completed");
}
