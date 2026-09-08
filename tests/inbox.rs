//! Inbox façade tests — unified inbound ingestion.
//!
//! Every test uses its own file-backed libSQL database (fresh tempdir, no
//! shared DB) to exercise the real dedup table + unique index.

use agentcal::{
    AgentCal, AgentCrm, InboxEvent, InboxStatus, IngestStatus, InteractionDirection,
    InteractionKind, LibSqlStore,
};

const AT_MS: i64 = 1_786_000_000_000; // fixed event time (≈ 2026-08-04)

/// Fresh file-backed CRM; the tempdir is returned so the DB file lives as
/// long as the test.
async fn file_crm() -> (tempfile::TempDir, AgentCrm) {
    let dir = tempfile::tempdir().unwrap();
    let store = LibSqlStore::open(dir.path().join("inbox.db"))
        .await
        .unwrap();
    (dir, AgentCrm::new(store))
}

async fn seeded_crm() -> (tempfile::TempDir, AgentCrm, String) {
    let (dir, crm) = file_crm().await;
    let contact = crm
        .create_contact("derrick", "Derrick", "Hodge")
        .await
        .unwrap()
        .with_phone("+16142600424")
        .with_email("hodge@agentmail.com");
    crm.update_contact(&contact).await.unwrap();
    let id = contact.id.clone();
    (dir, crm, id)
}

fn sms(from: &str, external_id: &str, body: &str) -> InboxEvent {
    InboxEvent::new("sms", external_id, from, body, AT_MS)
}

#[tokio::test]
async fn ingest_sms_resolves_and_files_interaction() {
    let (_dir, crm, contact_id) = seeded_crm().await;

    let outcome = crm
        .ingest_inbox_event(
            "derrick",
            sms("+16142600424", "ext-1", "Running late, see you at 3"),
        )
        .await
        .unwrap();

    assert_eq!(outcome.status, IngestStatus::Ingested);
    assert_eq!(outcome.contact_id.as_deref(), Some(contact_id.as_str()));
    assert_eq!(outcome.record.status, InboxStatus::Ingested);
    assert_eq!(outcome.record.channel, "sms");
    let interaction_id = outcome.interaction_id.clone().expect("interaction filed");
    assert_eq!(
        outcome.record.interaction_id.as_deref(),
        Some(interaction_id.as_str())
    );

    // Filed as an inbound interaction carrying the event time + provenance.
    let interactions = crm
        .interactions_for_contact("derrick", &contact_id)
        .await
        .unwrap();
    assert_eq!(interactions.len(), 1);
    let i = &interactions[0];
    assert_eq!(i.id, interaction_id);
    assert_eq!(i.direction, InteractionDirection::Inbound);
    assert_eq!(i.kind, InteractionKind::Sms);
    assert!(i.summary.contains("Running late"), "summary: {}", i.summary);
    assert_eq!(i.at.timestamp_millis(), AT_MS);
    assert_eq!(i.metadata["inbox_channel"], "sms");
    assert_eq!(i.metadata["inbox_external_id"], "ext-1");
}

#[tokio::test]
async fn duplicate_ingest_files_once() {
    let (_dir, crm, contact_id) = seeded_crm().await;

    let first = crm
        .ingest_inbox_event("derrick", sms("+16142600424", "ext-dup", "hello"))
        .await
        .unwrap();
    assert_eq!(first.status, IngestStatus::Ingested);

    let second = crm
        .ingest_inbox_event("derrick", sms("+16142600424", "ext-dup", "hello"))
        .await
        .unwrap();
    assert_eq!(second.status, IngestStatus::Duplicate);
    assert_eq!(second.record.id, first.record.id);
    assert_eq!(second.contact_id, first.contact_id);

    // Still exactly one interaction.
    let interactions = crm
        .interactions_for_contact("derrick", &contact_id)
        .await
        .unwrap();
    assert_eq!(interactions.len(), 1);

    // Same external_id on a *different* channel is a distinct event.
    let other = crm
        .ingest_inbox_event(
            "derrick",
            InboxEvent::new("whatsapp", "ext-dup", "+16142600424", "hello", AT_MS),
        )
        .await
        .unwrap();
    assert_eq!(other.status, IngestStatus::Ingested);
}

#[tokio::test]
async fn unknown_sender_files_nothing_but_the_ledger_row() {
    let (_dir, crm, _contact_id) = seeded_crm().await;
    let contacts_before = crm.list_contacts("derrick").await.unwrap().len();

    let outcome = crm
        .ingest_inbox_event(
            "derrick",
            sms("+19995550199", "ext-unknown", "Hi, new here"),
        )
        .await
        .unwrap();

    // Structured unknown-sender result: no contact/company/deal/interaction.
    assert_eq!(outcome.status, IngestStatus::UnknownSender);
    assert!(outcome.contact_id.is_none());
    assert!(outcome.interaction_id.is_none());
    assert_eq!(outcome.record.status, InboxStatus::UnknownSender);
    assert_eq!(
        crm.list_contacts("derrick").await.unwrap().len(),
        contacts_before,
        "inbound must not auto-create contacts"
    );

    // The ledger row exists and redelivery dedups.
    let listed = crm.list_inbox_events("derrick", None, 10).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].status, InboxStatus::UnknownSender);

    let retry = crm
        .ingest_inbox_event(
            "derrick",
            sms("+19995550199", "ext-unknown", "Hi, new here"),
        )
        .await
        .unwrap();
    assert_eq!(retry.status, IngestStatus::Duplicate);
    assert_eq!(retry.record.id, outcome.record.id);
}

#[tokio::test]
async fn email_sender_resolves_by_email() {
    let (_dir, crm, contact_id) = seeded_crm().await;

    let outcome = crm
        .ingest_inbox_event(
            "derrick",
            InboxEvent::new(
                "email",
                "msg-1",
                "hodge@agentmail.com",
                "Q3 numbers attached",
                AT_MS,
            ),
        )
        .await
        .unwrap();

    assert_eq!(outcome.status, IngestStatus::Ingested);
    assert_eq!(outcome.contact_id.as_deref(), Some(contact_id.as_str()));
    let interactions = crm
        .interactions_for_contact("derrick", &contact_id)
        .await
        .unwrap();
    assert_eq!(interactions.len(), 1);
    assert_eq!(interactions[0].kind, InteractionKind::Email);
}

#[tokio::test]
async fn ingest_rejects_empty_fields() {
    let (_dir, crm, _contact_id) = seeded_crm().await;

    for event in [
        InboxEvent::new("", "e1", "+16142600424", "x", AT_MS),
        InboxEvent::new("sms", "", "+16142600424", "x", AT_MS),
        InboxEvent::new("sms", "e1", "  ", "x", AT_MS),
    ] {
        let err = crm.ingest_inbox_event("derrick", event).await.unwrap_err();
        assert!(err.to_string().contains("missing param"), "err: {err}");
    }
    let err = crm
        .ingest_inbox_event("", sms("+16142600424", "e1", "x"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("missing param"), "err: {err}");
}

// ── RPC surface ──────────────────────────────────────────────────────────────

async fn rpc_ctx() -> (tempfile::TempDir, AgentCal, AgentCrm) {
    let dir = tempfile::tempdir().unwrap();
    let store = LibSqlStore::open(dir.path().join("rpc.db")).await.unwrap();
    let cal = AgentCal::new(store.clone());
    let crm = AgentCrm::new(store);
    // Seed a contact with a phone number via the existing RPC path.
    let c = agentcal::rpc::dispatch(
        &cal,
        &crm,
        "crm.create_contact",
        &serde_json::json!({"owner": "derrick", "first_name": "Derrick", "last_name": "Hodge"}),
    )
    .await
    .unwrap();
    let mut full: agentcal::Contact = serde_json::from_value(c).unwrap();
    full.phone = "+16142600424".to_string();
    crm.update_contact(&full).await.unwrap();
    (dir, cal, crm)
}

#[tokio::test]
async fn rpc_inbox_ingest_and_list() {
    let (_dir, cal, crm) = rpc_ctx().await;
    async fn dispatch(
        cal: &AgentCal,
        crm: &AgentCrm,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, agentcal::AgentError> {
        agentcal::rpc::dispatch(cal, crm, method, &params).await
    }

    let r = dispatch(
        &cal,
        &crm,
        "inbox.ingest",
        serde_json::json!({
            "owner": "derrick",
            "event": {
                "channel": "sms",
                "external_id": "rpc-1",
                "from": "+16142600424",
                "body": "via rpc",
                "at_ms": AT_MS,
            },
        }),
    )
    .await
    .unwrap();
    assert_eq!(r["status"], "INGESTED");
    assert!(r["contact_id"].as_str().is_some());

    // Duplicate over RPC.
    let r2 = dispatch(
        &cal,
        &crm,
        "inbox.ingest",
        serde_json::json!({
            "owner": "derrick",
            "event": {
                "channel": "sms",
                "external_id": "rpc-1",
                "from": "+16142600424",
                "body": "via rpc",
                "at_ms": AT_MS,
            },
        }),
    )
    .await
    .unwrap();
    assert_eq!(r2["status"], "DUPLICATE");

    // Unknown sender over RPC.
    let r3 = dispatch(
        &cal,
        &crm,
        "inbox.ingest",
        serde_json::json!({
            "owner": "derrick",
            "event": {
                "channel": "whatsapp",
                "external_id": "rpc-2",
                "from": "+19995550199",
                "body": "stranger",
                "at_ms": AT_MS,
            },
        }),
    )
    .await
    .unwrap();
    assert_eq!(r3["status"], "UNKNOWN_SENDER");

    // List: unfiltered, channel-filtered, and limited.
    let all = dispatch(
        &cal,
        &crm,
        "inbox.list",
        serde_json::json!({"owner": "derrick"}),
    )
    .await
    .unwrap();
    assert_eq!(all.as_array().unwrap().len(), 2);

    let sms_only = dispatch(
        &cal,
        &crm,
        "inbox.list",
        serde_json::json!({"owner": "derrick", "channel": "sms"}),
    )
    .await
    .unwrap();
    assert_eq!(sms_only.as_array().unwrap().len(), 1);
    assert_eq!(sms_only[0]["external_id"], "rpc-1");

    let wa_only = dispatch(
        &cal,
        &crm,
        "inbox.list",
        serde_json::json!({"owner": "derrick", "channel": "whatsapp"}),
    )
    .await
    .unwrap();
    assert_eq!(wa_only.as_array().unwrap().len(), 1);

    let limited = dispatch(
        &cal,
        &crm,
        "inbox.list",
        serde_json::json!({"owner": "derrick", "limit": 1}),
    )
    .await
    .unwrap();
    assert_eq!(limited.as_array().unwrap().len(), 1);

    // Missing owner still errors (owner on everything).
    let err = dispatch(
        &cal,
        &crm,
        "inbox.ingest",
        serde_json::json!({"event": {"channel": "sms", "external_id": "x", "from": "y"}}),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("owner"), "err: {err}");
}
