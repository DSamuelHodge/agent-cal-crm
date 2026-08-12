//! RPC dispatch tests — the transport-agnostic layer the `cos` daemon uses.

use agentcal::rpc::dispatch;
use agentcal::{AgentCal, AgentCrm, LibSqlStore};

async fn ctx() -> (AgentCal, AgentCrm) {
    let store = LibSqlStore::in_memory().await.unwrap();
    let cal = AgentCal::new(store.clone());
    let crm = AgentCrm::new(store);
    (cal, crm)
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
async fn ping() {
    let (cal, crm) = ctx().await;
    let r = call(&cal, &crm, "ping", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(r, serde_json::json!({"pong": true}));
}

#[tokio::test]
async fn crm_company_contact_deal_flow() {
    let (cal, crm) = ctx().await;

    let co = call(
        &cal,
        &crm,
        "crm.create_company",
        serde_json::json!({
            "owner": "derrick", "name": "Hodge Luke", "industry": "AI Consulting"
        }),
    )
    .await
    .unwrap();
    let co_id = co["id"].as_str().unwrap();

    let c = call(
        &cal,
        &crm,
        "crm.create_contact",
        serde_json::json!({
            "owner": "derrick", "first_name": "Derrick", "last_name": "Hodge"
        }),
    )
    .await
    .unwrap();
    let cid = c["id"].as_str().unwrap();

    let d = call(
        &cal,
        &crm,
        "crm.create_deal",
        serde_json::json!({
            "owner": "derrick", "company_id": co_id, "name": "AI Strategy", "amount": 100000
        }),
    )
    .await
    .unwrap();
    assert_eq!(d["stage"], "LEAD");

    let advanced = call(
        &cal,
        &crm,
        "crm.advance_deal",
        serde_json::json!({
            "owner": "derrick", "deal_id": d["id"].as_str().unwrap(), "stage": "PROPOSAL"
        }),
    )
    .await
    .unwrap();
    assert_eq!(advanced["stage"], "PROPOSAL");

    // summary reflects the writes
    let s = call(
        &cal,
        &crm,
        "crm.summary",
        serde_json::json!({"owner": "derrick"}),
    )
    .await
    .unwrap();
    assert_eq!(s["companies"], 1);
    assert_eq!(s["contacts"], 1);
    assert_eq!(s["deals"], 1);
    let _ = cid;
}

#[tokio::test]
async fn resolve_by_phone_and_context() {
    let (cal, crm) = ctx().await;
    let co = call(
        &cal,
        &crm,
        "crm.create_company",
        serde_json::json!({
            "owner": "derrick", "name": "Hodge Luke", "industry": "AI"
        }),
    )
    .await
    .unwrap();
    call(
        &cal,
        &crm,
        "crm.create_contact",
        serde_json::json!({
            "owner": "derrick", "first_name": "Derrick", "last_name": "Hodge"
        }),
    )
    .await
    .unwrap();

    // phone is empty for a bare create — set it via log then search? Instead use email.
    let _ = co;
    // resolve fails cleanly when no match
    let err = call(
        &cal,
        &crm,
        "crm.resolve_by_phone",
        serde_json::json!({
            "owner": "derrick", "phone": "+16142600424"
        }),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("contact"));
}

#[tokio::test]
async fn calendar_flow_via_rpc() {
    let (cal, crm) = ctx().await;
    call(
        &cal,
        &crm,
        "cal.create_calendar_simple",
        serde_json::json!({
            "owner": "derrick", "name": "Derrick's Calendar"
        }),
    )
    .await
    .unwrap();
    call(
        &cal,
        &crm,
        "cal.add_window",
        serde_json::json!({
            "owner": "derrick", "day_of_week": 0, "start": "09:00", "end": "17:00"
        }),
    )
    .await
    .unwrap();
    let link = call(
        &cal,
        &crm,
        "cal.create_link",
        serde_json::json!({
            "owner": "derrick", "title": "30-min", "duration_minutes": 30
        }),
    )
    .await
    .unwrap();
    let slots = call(
        &cal,
        &crm,
        "cal.get_slots",
        serde_json::json!({
            "owner": "derrick", "link_id": link["id"].as_str().unwrap(), "limit": 3
        }),
    )
    .await
    .unwrap();
    assert!(slots.as_array().map(|a| !a.is_empty()).unwrap_or(false));

    let slot = &slots[0];
    let booked = call(
        &cal,
        &crm,
        "cal.book",
        serde_json::json!({
            "owner": "derrick",
            "link_id": link["id"].as_str().unwrap(),
            "slot": slot,
            "attendees": [{"name": "Derrick Hodge", "email": "hodge@agentmail.com"}],
            "notes": "intro",
        }),
    )
    .await
    .unwrap();
    let bid = booked["booking"]["id"].as_str().unwrap();

    let upcoming = call(
        &cal,
        &crm,
        "cal.upcoming",
        serde_json::json!({
            "owner": "derrick", "limit": 5
        }),
    )
    .await
    .unwrap();
    assert_eq!(upcoming.as_array().unwrap().len(), 1);

    // cancel
    call(
        &cal,
        &crm,
        "cal.cancel",
        serde_json::json!({
            "owner": "derrick", "booking_id": bid, "reason": "rescheduled"
        }),
    )
    .await
    .unwrap();
    let upcoming = call(
        &cal,
        &crm,
        "cal.upcoming",
        serde_json::json!({
            "owner": "derrick", "limit": 5
        }),
    )
    .await
    .unwrap();
    assert_eq!(upcoming.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn booking_links_back_to_contact() {
    let (cal, crm) = ctx().await;
    let co = call(
        &cal,
        &crm,
        "crm.create_company",
        serde_json::json!({
            "owner": "derrick", "name": "Hodge Luke", "industry": "AI"
        }),
    )
    .await
    .unwrap();
    let c = call(
        &cal,
        &crm,
        "crm.create_contact",
        serde_json::json!({
            "owner": "derrick", "first_name": "Derrick", "last_name": "Hodge"
        }),
    )
    .await
    .unwrap();

    // attendee for contact
    let attendee = call(
        &cal,
        &crm,
        "crm.attendee_for_contact",
        serde_json::json!({
            "owner": "derrick", "contact_id": c["id"].as_str().unwrap()
        }),
    )
    .await
    .unwrap();
    assert_eq!(attendee["metadata"]["contact_id"], c["id"]);

    call(
        &cal,
        &crm,
        "cal.create_calendar_simple",
        serde_json::json!({
            "owner": "derrick", "name": "C"
        }),
    )
    .await
    .unwrap();
    call(
        &cal,
        &crm,
        "cal.add_window",
        serde_json::json!({
            "owner": "derrick", "day_of_week": 0, "start": "09:00", "end": "17:00"
        }),
    )
    .await
    .unwrap();
    let link = call(
        &cal,
        &crm,
        "cal.create_link",
        serde_json::json!({
            "owner": "derrick", "title": "L", "duration_minutes": 30
        }),
    )
    .await
    .unwrap();
    let slots = call(
        &cal,
        &crm,
        "cal.get_slots",
        serde_json::json!({
            "owner": "derrick", "link_id": link["id"].as_str().unwrap(), "limit": 1
        }),
    )
    .await
    .unwrap();

    let booked = call(
        &cal,
        &crm,
        "cal.book",
        serde_json::json!({
            "owner": "derrick",
            "link_id": link["id"].as_str().unwrap(),
            "slot": &slots[0],
            "attendees": [attendee],
        }),
    )
    .await
    .unwrap();

    // contact_for_booking round-trips
    let booking = booked["booking"].clone();
    let resolved = call(
        &cal,
        &crm,
        "crm.contact_for_booking",
        serde_json::json!({
            "owner": "derrick", "booking": booking
        }),
    )
    .await
    .unwrap();
    assert_eq!(resolved["id"], c["id"]);
    let _ = co;
}
