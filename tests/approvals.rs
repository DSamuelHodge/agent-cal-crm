//! Pending-approval gate tests (ROADMAP Phase 1, item 3).
//!
//! Every test gets its own file-backed libSQL database (fresh `tempfile`
//! dir, no shared DB) and a unique owner id (the in-process action-log
//! buffer is global, so log assertions filter by owner).

use agentcal::approvals::{
    self, action_log_clear, action_log_snapshot, ApprovalConfig, ApprovalState, RiskTier,
};
use agentcal::error::AgentError;
use agentcal::rpc::dispatch;
use agentcal::{AgentCal, AgentCrm, LibSqlStore};

async fn file_ctx(owner_tag: &str) -> (tempfile::TempDir, AgentCal, AgentCrm, String) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = LibSqlStore::open(dir.path().join("approvals.db"))
        .await
        .unwrap();
    let cal = AgentCal::new(store.clone());
    let crm = AgentCrm::new(store);
    let owner = format!("appr_owner_{owner_tag}");
    (dir, cal, crm, owner)
}

async fn call(
    cal: &AgentCal,
    crm: &AgentCrm,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, AgentError> {
    dispatch(cal, crm, method, &params).await
}

fn approval_id_of(err: AgentError) -> String {
    match err {
        AgentError::ApprovalRequired(id) => id,
        other => panic!("expected ApprovalRequired, got: {other}"),
    }
}

/// Book a meeting and return `(link_id, slot, booking_id)`.
async fn setup_booking(
    cal: &AgentCal,
    crm: &AgentCrm,
    owner: &str,
) -> (String, serde_json::Value) {
    call(
        cal,
        crm,
        "cal.create_calendar_simple",
        serde_json::json!({"owner": owner, "name": "Gate Cal"}),
    )
    .await
    .unwrap();
    call(
        cal,
        crm,
        "cal.add_window",
        serde_json::json!({"owner": owner, "day_of_week": 0, "start": "09:00", "end": "17:00"}),
    )
    .await
    .unwrap();
    let link = call(
        cal,
        crm,
        "cal.create_link",
        serde_json::json!({"owner": owner, "title": "30-min", "duration_minutes": 30}),
    )
    .await
    .unwrap();
    let link_id = link["id"].as_str().unwrap().to_string();
    let slots = call(
        cal,
        crm,
        "cal.get_slots",
        serde_json::json!({"owner": owner, "link_id": link_id, "limit": 1}),
    )
    .await
    .unwrap();
    let booked = call(
        cal,
        crm,
        "cal.book",
        serde_json::json!({
            "owner": owner, "link_id": link_id, "slot": &slots[0],
            "attendees": [{"name": "Bo", "email": "bo@example.com"}],
            "notes": "gate test",
        }),
    )
    .await
    .unwrap();
    let bid = booked["booking"]["id"].as_str().unwrap().to_string();
    (link_id, serde_json::json!(bid))
}

#[test]
fn tier_table_spot_checks() {
    // Reads auto-approve.
    for m in [
        "ping",
        "crm.summary",
        "crm.search",
        "cal.get_slots",
        "cal.upcoming",
        "aware.sms",
        "aware.whatsapp",
        "aware.call",
        "aware.meeting",
        "approval.request",
        "approval.approve",
    ] {
        assert_eq!(approvals::risk_tier(m), RiskTier::Read, "{m}");
        assert!(
            !approvals::requires_approval(m, &ApprovalConfig::default()),
            "{m}"
        );
    }
    // Low-risk writes auto-approve in tiered mode, wait in fully-gated mode.
    for m in [
        "crm.create_contact",
        "crm.log_interaction",
        "crm.advance_deal",
        "cal.book",
        "cal.create_link",
        "aware.capture",
    ] {
        assert_eq!(approvals::risk_tier(m), RiskTier::LowRiskWrite, "{m}");
        assert!(
            !approvals::requires_approval(m, &ApprovalConfig::default()),
            "{m}"
        );
        assert!(
            approvals::requires_approval(
                m,
                &ApprovalConfig {
                    fully_gated: true,
                    ..ApprovalConfig::default()
                }
            ),
            "{m}"
        );
    }
    // Sends, deletes, external side-effects — and anything unknown — wait.
    for m in [
        "aware.sms.send",
        "aware.whatsapp.send",
        "aware.email",
        "aware.open",
        "aware.sync_contacts",
        "sync.logseq",
        "cal.cancel",
        "crm.delete_contact",
        "something.entirely.new",
    ] {
        assert_eq!(approvals::risk_tier(m), RiskTier::HighRisk, "{m}");
        assert!(
            approvals::requires_approval(m, &ApprovalConfig::default()),
            "{m}"
        );
    }
}

#[tokio::test]
async fn request_auto_approves_low_risk() {
    let (_dir, cal, crm, owner) = file_ctx("auto").await;
    action_log_clear();
    let r = call(
        &cal,
        &crm,
        "approval.request",
        serde_json::json!({
            "owner": owner,
            "method": "crm.create_contact",
            "params": {"owner": owner, "first_name": "Ada", "last_name": "L"},
        }),
    )
    .await
    .unwrap();
    assert_eq!(r["state"], "approved");
    assert_eq!(r["risk_tier"], "low");
    assert!(r["decided_at_ms"].is_number());
    let log = action_log_snapshot();
    assert!(
        log.iter().any(|e| e.owner_id == owner
            && e.method == "approval.request(crm.create_contact)"
            && e.result == "approved"),
        "auto-approve must hit the action log"
    );
}

#[tokio::test]
async fn request_pends_sends_and_dedups() {
    let (_dir, cal, crm, owner) = file_ctx("pend").await;
    let body = serde_json::json!({
        "owner": owner,
        "method": "aware.sms.send",
        "params": {"owner": owner, "recipient": "+15551234567", "text": "hi"},
    });
    let first = call(&cal, &crm, "approval.request", body.clone())
        .await
        .unwrap();
    assert_eq!(first["state"], "pending");
    assert_eq!(first["risk_tier"], "high");
    assert!(first["decided_at_ms"].is_null());
    // Identical request reuses the pending approval instead of duplicating.
    let second = call(&cal, &crm, "approval.request", body).await.unwrap();
    assert_eq!(first["id"], second["id"]);
    let list = call(
        &cal,
        &crm,
        "approval.list",
        serde_json::json!({"owner": owner, "state": "pending"}),
    )
    .await
    .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn cancel_pending_approve_executes_end_to_end() {
    let (_dir, cal, crm, owner) = file_ctx("e2e").await;
    action_log_clear();
    let (_link, bid_json) = setup_booking(&cal, &crm, &owner).await;
    let bid = bid_json.as_str().unwrap();

    // 1. pending: cancel without approval is refused with the approval id.
    let base = serde_json::json!({"owner": owner, "booking_id": bid, "reason": "moved"});
    let err = call(&cal, &crm, "cal.cancel", base.clone())
        .await
        .unwrap_err();
    let id = approval_id_of(err);

    // The booking is untouched while pending.
    let upcoming = call(&cal, &crm, "cal.upcoming", serde_json::json!({"owner": owner}))
        .await
        .unwrap();
    assert_eq!(upcoming.as_array().unwrap().len(), 1);

    // 2. approve: decision is recorded and returned with method + params.
    let approved = call(
        &cal,
        &crm,
        "approval.approve",
        serde_json::json!({"owner": owner, "id": id}),
    )
    .await
    .unwrap();
    assert_eq!(approved["state"], "approved");
    assert_eq!(approved["method"], "cal.cancel");

    // 3. executes: same params plus approval_id now succeed.
    let mut exec = base.clone();
    exec["approval_id"] = serde_json::json!(id);
    let cancelled = call(&cal, &crm, "cal.cancel", exec).await.unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    let upcoming = call(&cal, &crm, "cal.upcoming", serde_json::json!({"owner": owner}))
        .await
        .unwrap();
    assert_eq!(upcoming.as_array().unwrap().len(), 0);

    // Approval decisions appended to the action log with the agreed shape.
    let log = action_log_snapshot();
    let mine: Vec<_> = log.iter().filter(|e| e.owner_id == owner).collect();
    assert!(mine.iter().any(|e| e.actor == "rpc:approval.approve"
        && e.method == "approval.approve(cal.cancel)"
        && e.result == "approved"));
    for e in &mine {
        let v = serde_json::to_value(e).unwrap();
        for key in ["id", "owner_id", "actor", "method", "params_json", "result", "at_ms"] {
            assert!(v.get(key).is_some(), "action log entry missing {key}");
        }
    }
    let _ = ApprovalState::Approved;
}

#[tokio::test]
async fn reject_blocks_execution() {
    let (_dir, cal, crm, owner) = file_ctx("reject").await;
    let params = serde_json::json!({"owner": owner, "recipient": "+15551234567", "text": "no"});
    let pending = crm
        .request_approval(
            &owner,
            "aware.sms.send",
            &params,
            "test",
            &ApprovalConfig::default(),
        )
        .await
        .unwrap();
    let rejected = call(
        &cal,
        &crm,
        "approval.reject",
        serde_json::json!({"owner": owner, "id": pending.id, "reason": "too pushy"}),
    )
    .await
    .unwrap();
    assert_eq!(rejected["state"], "rejected");

    // Executing with a rejected approval id is refused.
    let mut exec = params.clone();
    exec["approval_id"] = serde_json::json!(pending.id);
    let err = crm
        .check_send_allowed(&owner, "aware.sms.send", &exec, &ApprovalConfig::default())
        .await
        .unwrap_err();
    match err {
        AgentError::ApprovalNotApproved(msg) => assert!(msg.contains("rejected")),
        other => panic!("expected ApprovalNotApproved, got: {other}"),
    }
    // Approving a settled approval is also refused.
    let err = call(
        &cal,
        &crm,
        "approval.approve",
        serde_json::json!({"owner": owner, "id": pending.id}),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AgentError::ApprovalNotApproved(_)));
}

#[tokio::test]
async fn send_gate_requires_matching_params() {
    let (_dir, _cal, crm, owner) = file_ctx("match").await;
    let cfg = ApprovalConfig::default();
    let params = serde_json::json!({"owner": owner, "recipient": "+15551234567", "text": "hello"});
    // No approval id → pending + ApprovalRequired carrying the id.
    let err = crm
        .check_send_allowed(&owner, "aware.sms.send", &params, &cfg)
        .await
        .unwrap_err();
    let id = approval_id_of(err);

    let approved = crm
        .approve_approval(&owner, &id, "test", &cfg)
        .await
        .unwrap();
    assert_eq!(approved.method, "aware.sms.send");

    // Same params + id → allowed.
    let mut ok_params = params.clone();
    ok_params["approval_id"] = serde_json::json!(id);
    crm.check_send_allowed(&owner, "aware.sms.send", &ok_params, &cfg)
        .await
        .unwrap();

    // Tampered text → refused.
    let mut evil = params.clone();
    evil["text"] = serde_json::json!("wire $1M instead");
    evil["approval_id"] = serde_json::json!(id);
    let err = crm
        .check_send_allowed(&owner, "aware.sms.send", &evil, &cfg)
        .await
        .unwrap_err();
    assert!(matches!(err, AgentError::Validation(_)));

    // Approval issued for another method → refused.
    let mut wrong = params.clone();
    wrong["approval_id"] = serde_json::json!(id);
    let err = crm
        .check_send_allowed(&owner, "aware.email", &wrong, &cfg)
        .await
        .unwrap_err();
    assert!(matches!(err, AgentError::Validation(_)));
}

#[tokio::test]
async fn approvals_are_owner_scoped() {
    let (_dir, cal, crm, owner) = file_ctx("scope").await;
    let other = format!("{owner}_b");
    let pending = crm
        .request_approval(
            &owner,
            "aware.sms.send",
            &serde_json::json!({"owner": owner, "text": "x"}),
            "test",
            &ApprovalConfig::default(),
        )
        .await
        .unwrap();
    // Another owner cannot see, approve, or use it.
    assert!(
        call(
            &cal,
            &crm,
            "approval.list",
            serde_json::json!({"owner": other}),
        )
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty()
    );
    let err = call(
        &cal,
        &crm,
        "approval.approve",
        serde_json::json!({"owner": other, "id": pending.id}),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AgentError::ApprovalNotFound(_)));
    // Unknown ids and bad state filters fail cleanly.
    let err = call(
        &cal,
        &crm,
        "approval.approve",
        serde_json::json!({"owner": owner, "id": "does-not-exist"}),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AgentError::ApprovalNotFound(_)));
    let err = call(
        &cal,
        &crm,
        "approval.list",
        serde_json::json!({"owner": owner, "state": "bogus"}),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AgentError::Validation(_)));
}

#[tokio::test]
async fn stale_pendings_expire() {
    let (_dir, cal, crm, owner) = file_ctx("expiry").await;
    let cfg = ApprovalConfig::default();
    let pending = crm
        .request_approval(
            &owner,
            "aware.sms.send",
            &serde_json::json!({"owner": owner, "text": "x"}),
            "test",
            &cfg,
        )
        .await
        .unwrap();
    // Explicit sweep with a far-future clock expires it.
    let n = crm
        .sweep_expired_approvals(&owner, pending.created_at_ms + 60_000, 1)
        .await
        .unwrap();
    assert_eq!(n, 1);
    let expired = crm
        .list_approvals(&owner, Some("expired"), &cfg)
        .await
        .unwrap();
    assert_eq!(expired.len(), 1);
    // Approving an expired approval is refused.
    let err = call(
        &cal,
        &crm,
        "approval.approve",
        serde_json::json!({"owner": owner, "id": pending.id}),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AgentError::ApprovalNotApproved(_)));

    // Lazy expiry: a negative-TTL config expires on next access, deterministically.
    let pending2 = crm
        .request_approval(
            &owner,
            "aware.sms.send",
            &serde_json::json!({"owner": owner, "text": "y"}),
            "test",
            &cfg,
        )
        .await
        .unwrap();
    let lst = crm
        .list_approvals(
            &owner,
            Some("pending"),
            &ApprovalConfig {
                pending_ttl_ms: -1,
                ..ApprovalConfig::default()
            },
        )
        .await
        .unwrap();
    assert!(!lst.iter().any(|a| a.id == pending2.id));
}

#[tokio::test]
async fn fully_gated_mode_holds_low_risk_writes() {
    let (_dir, cal, crm, owner) = file_ctx("gated").await;
    let gated = ApprovalConfig {
        fully_gated: true,
        ..ApprovalConfig::default()
    };
    // Direct (non-RPC) request under full gating: low-risk write stays pending.
    let held = crm
        .request_approval(
            &owner,
            "crm.create_contact",
            &serde_json::json!({"owner": owner}),
            "test",
            &gated,
        )
        .await
        .unwrap();
    assert_eq!(held.state, "pending");
    // …while reads still auto-approve.
    let read = crm
        .request_approval(&owner, "crm.summary", &serde_json::json!({}), "test", &gated)
        .await
        .unwrap();
    assert_eq!(read.state, "approved");
    // And the gate enforces it on the execution path.
    let err = crm
        .check_send_allowed(
            &owner,
            "crm.create_contact",
            &serde_json::json!({"owner": owner}),
            &gated,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, AgentError::ApprovalRequired(_)));
    let _ = cal;
}
