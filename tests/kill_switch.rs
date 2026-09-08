//! Channel kill-switch tests (ROADMAP Phase 2.5).
//!
//! Every test uses its own file-backed libSQL database (fresh tempdir, no
//! shared DB) and a unique owner id. NO globals, NO shared static state.

use agentcal::error::AgentError;
use agentcal::rpc::{dispatch, dispatch_with_gate};
use agentcal::{
    AgentCal, AgentCrm, ChannelGate, InboxEvent, IngestStatus, KNOWN_CHANNELS, LibSqlStore,
};

const AT_MS: i64 = 1_786_000_000_000; // fixed event time (≈ 2026-08-04)

async fn file_ctx(owner_tag: &str) -> (tempfile::TempDir, AgentCal, AgentCrm, String) {
    let dir = tempfile::tempdir().unwrap();
    let store = LibSqlStore::open(dir.path().join("kill_switch.db"))
        .await
        .unwrap();
    let cal = AgentCal::new(store.clone());
    let crm = AgentCrm::new(store);
    let owner = format!("ks_owner_{owner_tag}");
    (dir, cal, crm, owner)
}

fn sms_event(external_id: &str, from: &str, body: &str) -> InboxEvent {
    InboxEvent::new("sms", external_id, from, body, AT_MS)
}

fn channel_disabled_id(err: AgentError) -> String {
    match err {
        AgentError::ChannelDisabled(ch) => ch,
        other => panic!("expected ChannelDisabled, got: {other}"),
    }
}

// (a) Disabled channel rejects on the underlying send path directly —
// bypassing dispatch entirely. This is the hard requirement: the gate lives
// in the agent function, not just the RPC wrapper.
#[tokio::test]
async fn disabled_channel_rejects_direct_send_path() {
    let (_dir, _cal, crm, owner) = file_ctx("direct").await;
    let gate = ChannelGate::with_disabled(&["sms"]);

    // Direct against `inbox::ingest_with_gate` (the send-path function).
    let err = agentcal::inbox::ingest_with_gate(
        &crm,
        &owner,
        sms_event("ks-direct-1", "+16142600424", "hello"),
        &gate,
    )
    .await
    .unwrap_err();
    assert_eq!(channel_disabled_id(err), "sms");

    // Same via the façade wrapper.
    let err = crm
        .ingest_inbox_event_with_gate(&owner, sms_event("ks-direct-2", "+16142600424", "hi"), &gate)
        .await
        .unwrap_err();
    assert_eq!(channel_disabled_id(err), "sms");

    // Rejection happens before any I/O: nothing was filed.
    let listed = crm.list_inbox_events(&owner, None, 10).await.unwrap();
    assert!(listed.is_empty(), "killed channel must file nothing");

    // Error code wired for the machine-readable boundary.
    assert_eq!(
        agentcal::actions::error_code(&AgentError::ChannelDisabled("sms".into())),
        "channel_disabled"
    );
}

// (b) Dispatch-level rejection: a disabled channel fails at the TOP of the
// dispatch path (before send-gating/approval logic) via dispatch_with_gate.
#[tokio::test]
async fn dispatch_rejects_disabled_channel() {
    let (_dir, cal, crm, owner) = file_ctx("dispatch").await;
    let gate = ChannelGate::with_disabled(&["sms"]);

    let params = serde_json::json!({
        "owner": owner,
        "event": {
            "channel": "sms",
            "external_id": "ks-dispatch-1",
            "from": "+16142600424",
            "body": "via dispatch",
            "at_ms": AT_MS,
        },
    });
    let err = dispatch_with_gate(&cal, &crm, &gate, "inbox.ingest", &params)
        .await
        .unwrap_err();
    assert_eq!(channel_disabled_id(err), "sms");

    // Alias collapses to the canonical channel: killing "whatsapp" also kills "wa".
    let wa_gate = ChannelGate::with_disabled(&["whatsapp"]);
    let wa_params = serde_json::json!({
        "owner": owner,
        "event": {
            "channel": "wa",
            "external_id": "ks-dispatch-wa",
            "from": "+16142600424",
            "body": "alias",
            "at_ms": AT_MS,
        },
    });
    let err = dispatch_with_gate(&cal, &crm, &wa_gate, "inbox.ingest", &wa_params)
        .await
        .unwrap_err();
    assert_eq!(channel_disabled_id(err), "whatsapp");

    // Same call through the default `dispatch` (all enabled) does NOT reject:
    // unknown sender files as UNKNOWN_SENDER rather than erroring.
    let ok = dispatch(&cal, &crm, "inbox.ingest", &params).await.unwrap();
    assert_eq!(ok["status"], "UNKNOWN_SENDER");

    // The rejection is action-logged as `channel_disabled`.
    let log = cal
        .action_store()
        .query_actions(&owner, Some("inbox.ingest"), 10)
        .await
        .unwrap();
    assert!(
        log.iter().any(|e| e.result == "channel_disabled"),
        "dispatch rejection must be logged as channel_disabled"
    );
    let _ = IngestStatus::Ingested;
}

// (c) Enabled by default for all known channels.
#[tokio::test]
async fn enabled_by_default_for_all_known_channels() {
    assert_eq!(KNOWN_CHANNELS, &["sms", "email", "whatsapp", "call", "push"]);
    let gate = ChannelGate::new();
    for ch in KNOWN_CHANNELS {
        assert!(gate.is_enabled(ch), "{ch} must be enabled by default");
    }

    // …and dispatch over the default gate accepts each channel (unknown
    // senders file as UNKNOWN_SENDER, which proves the gate passed).
    let (_dir, cal, crm, owner) = file_ctx("default").await;
    for (i, ch) in KNOWN_CHANNELS.iter().enumerate() {
        let params = serde_json::json!({
            "owner": owner,
            "event": {
                "channel": ch,
                "external_id": format!("ks-default-{i}"),
                "from": "+19995550199",
                "body": "default-on",
                "at_ms": AT_MS,
            },
        });
        let r = dispatch(&cal, &crm, "inbox.ingest", &params).await.unwrap();
        assert_eq!(r["status"], "UNKNOWN_SENDER", "channel {ch}");
    }

    // Enable/disable round-trips (the minimal wiring for tests/future config).
    let mut gate = ChannelGate::new();
    gate.disable("sms");
    assert!(!gate.is_enabled("sms"));
    assert!(gate.is_enabled("email"));
    gate.enable("sms");
    assert!(gate.is_enabled("sms"));
}

// (d) `is_enabled` is a single expression — one HashSet lookup, no loops,
// no I/O. The source comment on `ChannelGate::is_enabled`
// (src/kill_switch.rs) states this; this test pins it by inspecting the
// function body: it must contain the lookup and no loop/await/branch
// keywords or extra statements. Keep the body to one expression.
#[test]
fn is_enabled_is_a_single_expression() {
    let src = include_str!("../src/kill_switch.rs");
    let start = src
        .find("pub fn is_enabled")
        .expect("ChannelGate::is_enabled must exist");
    let body = &src[start..];
    let open = body.find('{').expect("is_enabled body must open");
    let mut depth = 0usize;
    let mut end = None;
    for (i, c) in body.char_indices() {
        if i < open {
            continue;
        }
        if c == '{' {
            depth += 1;
        } else if c == '}' {
            depth -= 1;
            if depth == 0 {
                end = Some(i);
                break;
            }
        }
    }
    let inner = &body[open + 1..end.expect("is_enabled body must close")];
    assert!(
        inner.contains("disabled.contains"),
        "is_enabled must be a HashSet lookup"
    );
    for banned in ["for ", "loop ", "while ", "await", "match ", ";", "if "] {
        assert!(
            !inner.contains(banned),
            "is_enabled body must stay a single expression (found {banned:?})"
        );
    }
}
