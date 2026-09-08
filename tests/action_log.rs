//! Action-log tests — append-only, owner-scoped record of issued actions.
//!
//! File-backed libSQL databases (one fresh tempdir per test — never a
//! shared/global DB path), following the `test_libsql_file_roundtrip` style.

use agentcal::rpc::dispatch;
use agentcal::{record_action, ActionActor, ActionLogStore, AgentCal, AgentCrm, LibSqlStore};

async fn ctx() -> (
    tempfile::TempDir,
    AgentCal,
    AgentCrm,
    LibSqlStore,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LibSqlStore::open(dir.path().join("cos.db"))
        .await
        .expect("open store");
    let cal = AgentCal::new(store.clone());
    let crm = AgentCrm::new(store.clone());
    (dir, cal, crm, store)
}

async fn call(
    cal: &AgentCal,
    crm: &AgentCrm,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, agentcal::AgentError> {
    dispatch(cal, crm, method, &params).await
}

#[tokio::test]
async fn record_and_list_roundtrip() {
    let (_dir, _cal, _crm, store) = ctx().await;
    record_action(
        &store,
        "derrick",
        ActionActor::Rule,
        "aware.sms.send",
        &serde_json::json!({"to": "+16142600424"}),
        "ok",
    )
    .await
    .unwrap();

    let entries = store.list_actions("derrick", 10).await.unwrap();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.owner_id, "derrick");
    assert_eq!(e.actor, ActionActor::Rule);
    assert_eq!(e.method, "aware.sms.send");
    assert_eq!(e.result, "ok");
    assert!(!e.id.is_empty());
    assert!(e.at_ms > 0);
    // Params survive as JSON text.
    let params: serde_json::Value = serde_json::from_str(&e.params_json).unwrap();
    assert_eq!(params["to"], "+16142600424");
}

#[tokio::test]
async fn record_redacts_secrets_before_storing() {
    let (_dir, _cal, _crm, store) = ctx().await;
    record_action(
        &store,
        "derrick",
        ActionActor::Llm,
        "aware.email",
        &serde_json::json!({
            "owner": "derrick",
            "token": "super-secret-value",
            "nested": {"password": "hunter2"},
            "subject": "hello",
        }),
        "ok",
    )
    .await
    .unwrap();

    let entries = store.list_actions("derrick", 10).await.unwrap();
    let raw = &entries[0].params_json;
    assert!(!raw.contains("super-secret-value"), "token leaked: {raw}");
    assert!(!raw.contains("hunter2"), "password leaked: {raw}");
    assert!(raw.contains("hello"));
}

#[tokio::test]
async fn log_is_owner_scoped_newest_first_and_limited() {
    let (_dir, _cal, _crm, store) = ctx().await;
    for i in 0..3 {
        record_action(
            &store,
            "derrick",
            ActionActor::Rpc,
            &format!("cal.op{i}"),
            &serde_json::json!({"owner": "derrick"}),
            "ok",
        )
        .await
        .unwrap();
    }
    record_action(
        &store,
        "other",
        ActionActor::Rpc,
        "cal.op9",
        &serde_json::json!({"owner": "other"}),
        "ok",
    )
    .await
    .unwrap();

    // Other owners are invisible.
    let entries = store.list_actions("derrick", 10).await.unwrap();
    assert_eq!(entries.len(), 3);
    // Newest first (rowid tiebreak keeps insertion order stable).
    assert_eq!(entries[0].method, "cal.op2");
    assert_eq!(entries[2].method, "cal.op0");
    // Limit respected.
    let entries = store.list_actions("derrick", 2).await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].method, "cal.op2");
}

#[tokio::test]
async fn query_filters_by_method() {
    let (_dir, _cal, _crm, store) = ctx().await;
    for method in ["ping", "ping", "cal.summary"] {
        record_action(
            &store,
            "derrick",
            ActionActor::Rpc,
            method,
            &serde_json::json!({"owner": "derrick"}),
            "ok",
        )
        .await
        .unwrap();
    }
    let pings = store
        .query_actions("derrick", Some("ping"), 10)
        .await
        .unwrap();
    assert_eq!(pings.len(), 2);
    assert!(pings.iter().all(|e| e.method == "ping"));
    let all = store.query_actions("derrick", None, 10).await.unwrap();
    assert_eq!(all.len(), 3);
}

#[tokio::test]
async fn dispatch_fills_log_with_ok_and_error_codes() {
    let (_dir, cal, crm, _store) = ctx().await;

    call(&cal, &crm, "ping", serde_json::json!({"owner": "derrick"}))
        .await
        .unwrap();
    call(
        &cal,
        &crm,
        "crm.create_company",
        serde_json::json!({"owner": "derrick", "name": "Hodge Luke", "industry": "AI"}),
    )
    .await
    .unwrap();
    let err = call(
        &cal,
        &crm,
        "crm.get_contact",
        serde_json::json!({"owner": "derrick", "contact_id": "nope"}),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("contact"));

    // Newest first: failed get_contact, create_company, ping.
    let listed = call(
        &cal,
        &crm,
        "action_log.list",
        serde_json::json!({"owner": "derrick", "limit": 10}),
    )
    .await
    .unwrap();
    let rows = listed.as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["method"], "crm.get_contact");
    assert_eq!(rows[0]["result"], "contact_not_found");
    assert_eq!(rows[1]["method"], "crm.create_company");
    assert_eq!(rows[1]["result"], "ok");
    assert_eq!(rows[2]["method"], "ping");
    assert_eq!(rows[2]["result"], "ok");
    assert_eq!(rows[0]["actor"], "rpc");

    // Query narrows to one method.
    let queried = call(
        &cal,
        &crm,
        "action_log.query",
        serde_json::json!({"owner": "derrick", "method": "ping", "limit": 10}),
    )
    .await
    .unwrap();
    let rows = queried.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["method"], "ping");
}

#[tokio::test]
async fn dispatch_log_redacts_rpc_params() {
    let (_dir, cal, crm, _store) = ctx().await;
    call(
        &cal,
        &crm,
        "ping",
        serde_json::json!({"owner": "derrick", "token": "tok-123"}),
    )
    .await
    .unwrap();
    let listed = call(
        &cal,
        &crm,
        "action_log.list",
        serde_json::json!({"owner": "derrick", "limit": 5}),
    )
    .await
    .unwrap();
    let raw = listed[0]["params_json"].as_str().unwrap();
    assert!(!raw.contains("tok-123"), "token leaked: {raw}");
}

#[tokio::test]
async fn unknown_method_logs_validation_code() {
    let (_dir, cal, crm, _store) = ctx().await;
    call(&cal, &crm, "nope.unknown", serde_json::json!({"owner": "derrick"}))
        .await
        .unwrap_err();
    let queried = call(
        &cal,
        &crm,
        "action_log.query",
        serde_json::json!({"owner": "derrick", "method": "nope.unknown"}),
    )
    .await
    .unwrap();
    assert_eq!(queried[0]["result"], "validation");
}

#[tokio::test]
async fn action_log_survives_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("cos.db");
    {
        let store = LibSqlStore::open(&db_path).await.expect("open store");
        record_action(
            &store,
            "derrick",
            ActionActor::Rule,
            "aware.sms.send",
            &serde_json::json!({"owner": "derrick"}),
            "ok",
        )
        .await
        .unwrap();
    }
    {
        let store = LibSqlStore::open(&db_path).await.expect("reopen store");
        let entries = store.list_actions("derrick", 10).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].method, "aware.sms.send");
    }
}
