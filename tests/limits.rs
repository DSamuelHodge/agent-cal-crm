//! Deterministic send-budget integration tests (Phase 2 limits ledger).
//!
//! Every test uses its own file-backed libSQL database (fresh tempdir, no
//! shared DB, no process-global mutable state) so counters are exercised
//! against the real `limit_usage` table and its upsert-increment path.

use agentcal::{AgentCal, AgentCrm, LibSqlStore};

/// Fresh file-backed cal + crm; the tempdir is returned so the DB lives as
/// long as the test.
async fn file_ctx() -> (tempfile::TempDir, AgentCal, AgentCrm, String) {
    let dir = tempfile::tempdir().unwrap();
    let store = LibSqlStore::open(dir.path().join("limits.db"))
        .await
        .unwrap();
    let cal = AgentCal::new(store.clone());
    let crm = AgentCrm::new(store);
    let owner = format!("limit_owner_{}", chrono::Utc::now().timestamp_millis());
    (dir, cal, crm, owner)
}

fn sms(owner: &str, recipient: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "owner": owner,
        "recipient": recipient,
        "text": text,
    })
}

#[tokio::test]
async fn budget_denies_at_exactly_one_over() {
    let (_dir, cal, crm, owner) = file_ctx().await;
    let (sms_budget, _) = (agentcal::limits::SMS_BUDGET, agentcal::limits::EMAIL_BUDGET);

    // Send exactly budget-worth successfully.
    for _ in 0..sms_budget {
        let out = agentcal::rpc::dispatch(&cal, &crm, "sms.send", &sms(&owner, "+16142600424", "hi"))
            .await
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    // The very next send trips the budget.
    let err = agentcal::rpc::dispatch(&cal, &crm, "sms.send", &sms(&owner, "+16142600424", "hi"))
        .await
        .unwrap_err();
    assert_eq!(agentcal::actions::error_code(&err), "limit_exceeded");

    // A different channel is independent.
    let out = agentcal::rpc::dispatch(
        &cal,
        &crm,
        "email.send",
        &sms(&owner, "a@b.com", "hello"),
    )
    .await
    .unwrap();
    assert_eq!(out["ok"], true);
}

#[tokio::test]
async fn counters_survive_restart() {
    // Write with one store, drop it, reopen the same file, usage persists.
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("limits.db");

    let (cal, crm) = {
        let store = LibSqlStore::open(&db_path).await.unwrap();
        (AgentCal::new(store.clone()), AgentCrm::new(store))
    };
    let owner = "restart_owner";
    for _ in 0..7 {
        agentcal::rpc::dispatch(&cal, &crm, "sms.send", &sms(owner, "+16142600424", "x"))
            .await
            .unwrap();
    }

    // Drop everything, reopen the same file.
    drop(cal);
    drop(crm);
    let store = LibSqlStore::open(&db_path).await.unwrap();
    let cal = AgentCal::new(store.clone());
    let crm = AgentCrm::new(store);

    let out = agentcal::rpc::dispatch(
        &cal,
        &crm,
        "limit.query",
        &serde_json::json!({ "owner": owner, "channel": "sms" }),
    )
    .await
    .unwrap();
    assert_eq!(out["used"], 7);
    assert_eq!(out["budget"], agentcal::limits::SMS_BUDGET);
    assert_eq!(out["near_exhaustion"], false);
}

#[tokio::test]
async fn limit_query_reports_near_exhaustion() {
    let (_dir, cal, crm, owner) = file_ctx().await;
    let budget = agentcal::limits::SMS_BUDGET;

    // Fill to 80% — just at the near-exhaustion boundary.
    for _ in 0..(budget * 4 / 5) {
        agentcal::rpc::dispatch(&cal, &crm, "sms.send", &sms(&owner, "+16142600424", "x"))
            .await
            .unwrap();
    }
    let out = agentcal::rpc::dispatch(
        &cal,
        &crm,
        "limit.query",
        &serde_json::json!({ "owner": owner, "channel": "sms" }),
    )
    .await
    .unwrap();
    assert_eq!(out["used"], budget * 4 / 5);
    assert_eq!(out["near_exhaustion"], true);

    // Below the boundary (79%) is not flagged.
    let (_dir2, cal2, crm2, owner2) = file_ctx().await;
    for _ in 0..(budget * 4 / 5 - 1) {
        agentcal::rpc::dispatch(&cal2, &crm2, "sms.send", &sms(&owner2, "+16142600424", "x"))
            .await
            .unwrap();
    }
    let out2 = agentcal::rpc::dispatch(
        &cal2,
        &crm2,
        "limit.query",
        &serde_json::json!({ "owner": owner2, "channel": "sms" }),
    )
    .await
    .unwrap();
    assert_eq!(out2["near_exhaustion"], false);
}

#[tokio::test]
async fn record_only_on_success() {
    let (_dir, cal, crm, owner) = file_ctx().await;

    // Validation failures must not consume budget.
    let err = agentcal::rpc::dispatch(&cal, &crm, "sms.send", &sms(&owner, "", "no recipient"))
        .await
        .unwrap_err();
    assert_eq!(agentcal::actions::error_code(&err), "validation");

    let out = agentcal::rpc::dispatch(
        &cal,
        &crm,
        "limit.query",
        &serde_json::json!({ "owner": owner, "channel": "sms" }),
    )
    .await
    .unwrap();
    assert_eq!(out["used"], 0);
}