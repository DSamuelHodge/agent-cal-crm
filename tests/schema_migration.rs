//! QA audit suite — schema & migration integrity.
//!
//! Covers the additive Chief-of-Staff data model (downloads/files 001-007)
//! layered onto the existing CRM/calendar schema:
//!   * every legacy + new object is created in one batch
//!   * the batch is idempotent (safe to re-run on every daemon start)
//!   * re-opening an existing DB preserves data and seed rows
//!   * FK constraints declared across the new dimensions are enforceable
//!   * the F32_BLOB(64) embedding column round-trips
//!   * the `review_queue` view and unique channel lookup behave as specified
//!
//! Uses `LibSqlStore::from_database` + `store.database()` so every case runs
//! against the *real* SCHEMA string (no copy-paste of DDL in tests).

use agentcal::LibSqlStore;
use libsql::{params, Builder, Connection, Value};

/// The names of the legacy CRM/calendar tables that must survive the migration.
const LEGACY_TABLES: &[&str] = &[
    "calendars",
    "availability_windows",
    "blocked_periods",
    "booking_links",
    "bookings",
    "attendees",
    "companies",
    "contacts",
    "deals",
    "interactions",
    "crm_fts",
];

/// The 12 new objects added by the Chief-of-Staff model (001-007).
const NEW_TABLES: &[&str] = &[
    "principals",
    "entities",
    "entity_channels",
    "entity_aliases",
    "entity_relationships",
    "audit_log",
    "access_policies",
    "engagements",
    "projects",
    "commitments",
    "relationship_care",
    "key_dates",
];

const NEW_VIEWS: &[&str] = &["review_queue"];

/// Open a file-backed store (file, not `:memory:`, so independent connections
/// clearly share one database — the production shape).
async fn file_store() -> (tempfile::TempDir, LibSqlStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cos.db");
    let db = Builder::new_local(path.to_str().unwrap())
        .build()
        .await
        .expect("build db");
    let store = LibSqlStore::from_database(db).await.expect("init store");
    (dir, store)
}

/// A fresh connection to the same underlying database (via the stored handle).
async fn conn(store: &LibSqlStore) -> Connection {
    store.database().expect("db handle").connect().expect("connect")
}

/// All table + view names in the schema.
async fn object_names(c: &Connection) -> Vec<String> {
    let mut rows = c
        .query("SELECT name FROM sqlite_master WHERE type IN ('table','view') ORDER BY name", params![])
        .await
        .expect("query sqlite_master");
    let mut out = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        if let Value::Text(s) = row.get_value(0).expect("name") {
            out.push(s);
        }
    }
    out
}

async fn scalar_i64(c: &Connection, sql: &str) -> i64 {
    let mut rows = c.query(sql, params![]).await.expect("query");
    let row = rows.next().await.expect("next").expect("row");
    match row.get_value(0).expect("col") {
        Value::Integer(i) => i,
        Value::Real(r) => r as i64,
        other => panic!("expected integer, got {other:?}"),
    }
}

fn f32_blob(dim: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(dim * 4);
    for i in 0..dim {
        bytes.extend_from_slice(&(i as f32).to_le_bytes());
    }
    bytes
}

// ── 1. Complete schema in one batch ──────────────────────────────────────────

#[tokio::test]
async fn fresh_schema_creates_all_objects() {
    let (_dir, store) = file_store().await;
    let c = conn(&store).await;
    let names = object_names(&c).await;

    for t in LEGACY_TABLES.iter().chain(NEW_TABLES.iter()) {
        assert!(names.contains(&t.to_string()), "missing table: {t}");
    }
    for v in NEW_VIEWS {
        assert!(names.contains(&v.to_string()), "missing view: {v}");
    }
}

// ── 2. Idempotent re-init (runs on every daemon start) ───────────────────────

#[tokio::test]
async fn schema_is_idempotent_on_reinit() {
    let (dir, store) = file_store().await;
    // Re-open the same DB twice — mirrors `init()` running SCHEMA on each boot.
    let db = Builder::new_local(dir.path().join("cos.db").to_str().unwrap())
        .build()
        .await
        .expect("rebuild db");
    let again = LibSqlStore::from_database(db).await.expect("re-init");
    drop(again);

    let c = conn(&store).await;
    // Seed rows must not duplicate (INSERT OR IGNORE).
    assert_eq!(scalar_i64(&c, "SELECT COUNT(*) FROM principals").await, 2);
    // Object count must not grow (IF NOT EXISTS).
    let names = object_names(&c).await;
    assert_eq!(
        names.iter().filter(|n| *n == "engagements").count(),
        1,
        "engagements table duplicated"
    );
}

// ── 3. Principal seed rows ────────────────────────────────────────────────────

#[tokio::test]
async fn principals_seed_rows_have_correct_authority() {
    let (_dir, store) = file_store().await;
    let c = conn(&store).await;

    let mut rows = c
        .query(
            "SELECT id, principal_type, default_authority FROM principals ORDER BY id",
            params![],
        )
        .await
        .expect("query principals");
    let mut got: Vec<(String, String, String)> = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        let t = |i: i32| match row.get_value(i).expect("col") {
            Value::Text(s) => s,
            other => panic!("expected text, got {other:?}"),
        };
        got.push((t(0), t(1), t(2)));
    }
    assert_eq!(
        got,
        vec![
            ("agent".to_string(), "agent".to_string(), "limited".to_string()),
            ("derrick".to_string(), "owner".to_string(), "full".to_string()),
        ]
    );
}

// ── 4. Re-open preserves existing CRM data ───────────────────────────────────

#[tokio::test]
async fn reopen_preserves_existing_crm_data() {
    let (dir, store) = file_store().await;
    let c = conn(&store).await;

    // Write legacy data through raw SQL (schema is unchanged for these tables).
    c.execute(
        "INSERT INTO companies (id, owner_id, name, created_at, updated_at) VALUES (?,?,?,?,?)",
        params!["co-1", "derrick", "Acme", "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"],
    )
    .await
    .expect("insert company");
    c.execute(
        "INSERT INTO contacts (id, owner_id, first_name, last_name, created_at, updated_at) VALUES (?,?,?,?,?,?)",
        params!["ct-1", "derrick", "Ada", "Lovelace", "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"],
    )
    .await
    .expect("insert contact");
    drop(store);

    // Re-open the same path (fresh daemon boot).
    let db = Builder::new_local(dir.path().join("cos.db").to_str().unwrap())
        .build()
        .await
        .expect("rebuild db");
    let reopened = LibSqlStore::from_database(db).await.expect("reopen");
    let c2 = conn(&reopened).await;

    assert_eq!(scalar_i64(&c2, "SELECT COUNT(*) FROM companies").await, 1);
    assert_eq!(scalar_i64(&c2, "SELECT COUNT(*) FROM contacts").await, 1);
    assert_eq!(scalar_i64(&c2, "SELECT COUNT(*) FROM principals").await, 2);
    // New model still present after re-open.
    let names = object_names(&c2).await;
    assert!(names.contains(&"engagements".to_string()));
}

// ── 5. FK constraints across dimensions are enforceable ──────────────────────

#[tokio::test]
async fn engagements_foreign_keys_are_enforced() {
    let (_dir, store) = file_store().await;
    let c = conn(&store).await;
    // New connections default to FK off; the daemon's own connection enables
    // them via `PRAGMA foreign_keys = ON;` in SCHEMA. Mirror that here.
    c.execute_batch("PRAGMA foreign_keys = ON;")
        .await
        .expect("enable fk");

    c.execute(
        "INSERT INTO projects (id, owner_id, name) VALUES (?,?,?)",
        params!["proj-1", "derrick", "Q3 launch"],
    )
    .await
    .expect("insert project");

    // Valid: project exists.
    c.execute(
        "INSERT INTO engagements (id, owner_id, channel_type, direction, initiated_by, project_id) \
         VALUES (?,?,?,?,?,?)",
        params!["eng-1", "derrick", "call", "outbound", "owner", "proj-1"],
    )
    .await
    .expect("valid engagement");

    // Invalid: dangling project reference must be rejected.
    let bad = c
        .execute(
            "INSERT INTO engagements (id, owner_id, channel_type, direction, initiated_by, project_id) \
             VALUES (?,?,?,?,?,?)",
            params!["eng-2", "derrick", "call", "outbound", "owner", "missing"],
        )
        .await;
    assert!(bad.is_err(), "dangling project_id must violate FK");
}

// ── 6. F32_BLOB(64) embedding round-trip ──────────────────────────────────────

#[tokio::test]
async fn entity_embedding_round_trips() {
    let (_dir, store) = file_store().await;
    let c = conn(&store).await;

    let blob = f32_blob(64);
    c.execute(
        "INSERT INTO entities (id, owner_id, entity_type, display_name, embedding) \
         VALUES (?,?,?,?,?)",
        params!["ent-1", "derrick", "person", "Ada", blob.clone()],
    )
    .await
    .expect("insert entity with embedding");

    let mut rows = c
        .query("SELECT embedding FROM entities WHERE id = 'ent-1'", params![])
        .await
        .expect("query embedding");
    let row = rows.next().await.expect("row").expect("some");
    match row.get_value(0).expect("embedding") {
        Value::Blob(b) => {
            assert_eq!(b.len(), 64 * 4, "F32_BLOB(64) must be 256 bytes");
            assert_eq!(b, blob, "embedding bytes must round-trip verbatim");
        }
        other => panic!("expected blob, got {other:?}"),
    }
}

// ── 7. review_queue view surfaces low-confidence inferences only ──────────────

#[tokio::test]
async fn review_queue_filters_low_confidence() {
    let (_dir, store) = file_store().await;
    let c = conn(&store).await;

    c.execute(
        "INSERT INTO audit_log (id, table_name, record_id, source_type, confidence) \
         VALUES (?,?,?,?,?)",
        params!["a1", "entities", "ent-1", "agent_inference", 0.5],
    )
    .await
    .expect("insert low-confidence log");
    c.execute(
        "INSERT INTO audit_log (id, table_name, record_id, source_type, confidence) \
         VALUES (?,?,?,?,?)",
        params!["a2", "entities", "ent-2", "agent_inference", 0.95],
    )
    .await
    .expect("insert high-confidence log");

    let mut rows = c
        .query("SELECT id FROM review_queue ORDER BY id", params![])
        .await
        .expect("query view");
    let mut ids = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        if let Value::Text(s) = row.get_value(0).expect("id") {
            ids.push(s);
        }
    }
    assert_eq!(ids, vec!["a1".to_string()]);
}

// ── 8. Unique channel lookup ──────────────────────────────────────────────────

#[tokio::test]
async fn entity_channels_enforce_unique_normalized_lookup() {
    let (_dir, store) = file_store().await;
    let c = conn(&store).await;

    c.execute(
        "INSERT INTO entities (id, owner_id, entity_type, display_name) VALUES (?,?,?,?)",
        params!["ent-1", "derrick", "person", "Ada"],
    )
    .await
    .expect("entity 1");
    c.execute(
        "INSERT INTO entities (id, owner_id, entity_type, display_name) VALUES (?,?,?,?)",
        params!["ent-2", "derrick", "person", "Grace"],
    )
    .await
    .expect("entity 2");

    c.execute(
        "INSERT INTO entity_channels (id, entity_id, channel_type, value_raw, value_normalized) \
         VALUES (?,?,?,?,?)",
        params!["ch-1", "ent-1", "phone", "+1 555 0001", "+15550001"],
    )
    .await
    .expect("first channel");

    let dup = c
        .execute(
            "INSERT INTO entity_channels (id, entity_id, channel_type, value_raw, value_normalized) \
             VALUES (?,?,?,?,?)",
            params!["ch-2", "ent-2", "phone", "555-0001", "+15550001"],
        )
        .await;
    assert!(
        dup.is_err(),
        "duplicate (channel_type, value_normalized) must violate unique index"
    );
}
