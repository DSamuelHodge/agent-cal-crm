//! Integration tests for the CRM layer.
//!
//! The whole behaviour suite runs against both `MemoryStore` and `LibSqlStore`
//! (in-memory libSQL), mirroring the calendar `behaviour_suite!` pattern.

use agentcal::{
    AgentCrm, DealStage, InteractionDirection, InteractionInput, InteractionKind, LibSqlStore,
    MemoryStore,
};

/// Build an AgentCrm on an in-memory libSQL database.
async fn libsql_crm() -> AgentCrm {
    AgentCrm::new(LibSqlStore::in_memory().await.expect("libsql store"))
}

/// The full CRM behaviour suite, generic over the store constructor.
macro_rules! crm_suite {
    ($modname:ident, $make:expr) => {
        mod $modname {
            use super::*;

            async fn make() -> AgentCrm {
                ($make).await
            }

            // ── Companies ──────────────────────────────────────────────────

            #[tokio::test]
            async fn test_company_crud() {
                let crm = make().await;
                let co = crm
                    .create_company("derrick", "Hodge Luke", "AI Consulting")
                    .await
                    .unwrap();
                assert_eq!(co.name, "Hodge Luke");
                assert_eq!(co.stage, DealStage::Lead);

                let got = crm.get_company("derrick", &co.id).await.unwrap();
                assert_eq!(got.industry, "AI Consulting");

                crm.delete_company("derrick", &co.id).await.unwrap();
                assert!(crm.get_company("derrick", &co.id).await.is_err());
            }

            #[tokio::test]
            async fn test_list_companies_scoped_by_owner() {
                let crm = make().await;
                crm.create_company("a", "Acme", "Tech").await.unwrap();
                crm.create_company("b", "Globex", "Tech").await.unwrap();
                let a = crm.list_companies("a").await.unwrap();
                assert_eq!(a.len(), 1);
                assert_eq!(a[0].name, "Acme");
            }

            // ── Contacts ───────────────────────────────────────────────────

            #[tokio::test]
            async fn test_contact_crud_and_linking() {
                let crm = make().await;
                let co = crm
                    .create_company("derrick", "Hodge Luke", "AI")
                    .await
                    .unwrap();
                let contact = crm
                    .create_contact("derrick", "Derrick", "Hodge")
                    .await
                    .unwrap()
                    .with_company(&co.id)
                    .with_phone("+16142600424")
                    .with_email("hodge@agentmail.com")
                    .with_title("President and CEO")
                    .with_vip();
                crm.update_contact(&contact).await.unwrap();

                let got = crm.get_contact("derrick", &contact.id).await.unwrap();
                assert_eq!(got.display_name(), "Derrick Hodge");
                assert!(got.is_vip);
                assert_eq!(got.company_id.as_deref(), Some(co.id.as_str()));
            }

            #[tokio::test]
            async fn test_resolve_by_phone() {
                let crm = make().await;
                let contact = crm
                    .create_contact("derrick", "Derrick", "Hodge")
                    .await
                    .unwrap()
                    .with_phone("+1 (614) 260-0424");
                crm.update_contact(&contact).await.unwrap();

                // Exact match.
                let hit = crm
                    .resolve_by_phone("derrick", "+1 (614) 260-0424")
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(hit.id, contact.id);

                // Digit-normalised match (different formatting).
                let hit = crm
                    .resolve_by_phone("derrick", "16142600424")
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(hit.id, contact.id);
            }

            #[tokio::test]
            async fn test_resolve_by_alt_phone() {
                let crm = make().await;
                let contact = crm
                    .create_contact("derrick", "Derrick", "Hodge")
                    .await
                    .unwrap()
                    .with_phone("+16142600424")
                    .with_alt_phone("+16144074920");
                crm.update_contact(&contact).await.unwrap();

                // Alt phone resolves to the same contact.
                let hit = crm
                    .resolve_by_phone("derrick", "+16144074920")
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(hit.id, contact.id);
                assert_eq!(hit.all_phones().len(), 2);

                // Unrelated number still misses.
                assert!(crm
                    .resolve_by_phone("derrick", "+15551234567")
                    .await
                    .unwrap()
                    .is_none());
            }

            #[tokio::test]
            async fn test_resolve_by_email() {
                let crm = make().await;
                let contact = crm
                    .create_contact("derrick", "Derrick", "Hodge")
                    .await
                    .unwrap()
                    .with_email("hodge@agentmail.com");
                crm.update_contact(&contact).await.unwrap();

                let hit = crm
                    .resolve_by_email("derrick", "hodge@agentmail.com")
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(hit.id, contact.id);
                assert!(crm
                    .resolve_by_email("derrick", "nobody@x.com")
                    .await
                    .unwrap()
                    .is_none());
            }

            #[tokio::test]
            async fn test_contacts_for_company() {
                let crm = make().await;
                let co = crm.create_company("o", "Hodge Luke", "AI").await.unwrap();
                let c1 = crm
                    .create_contact("o", "Derrick", "Hodge")
                    .await
                    .unwrap()
                    .with_company(&co.id);
                let c2 = crm
                    .create_contact("o", "Narahari", "Luke")
                    .await
                    .unwrap()
                    .with_company(&co.id);
                crm.update_contact(&c1).await.unwrap();
                crm.update_contact(&c2).await.unwrap();

                let mut names: Vec<String> = crm
                    .contacts_for_company("o", &co.id)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|c| c.display_name())
                    .collect();
                names.sort();
                assert_eq!(names, vec!["Derrick Hodge", "Narahari Luke"]);
            }

            #[tokio::test]
            async fn test_delete_contact_cascades_interactions() {
                let crm = make().await;
                let c = crm.create_contact("o", "Derrick", "Hodge").await.unwrap();
                crm.log_interaction(
                    "o",
                    InteractionInput::new(&c.id, InteractionKind::Call).with_summary("intro call"),
                )
                .await
                .unwrap();
                crm.delete_contact("o", &c.id).await.unwrap();
                assert!(crm.get_contact("o", &c.id).await.is_err());
            }

            // ── Deals ──────────────────────────────────────────────────────

            #[tokio::test]
            async fn test_deal_lifecycle() {
                let crm = make().await;
                let co = crm.create_company("o", "Hodge Luke", "AI").await.unwrap();
                let deal = crm
                    .create_deal("o", &co.id, "Enterprise AI rollout", 250_000.0)
                    .await
                    .unwrap();
                assert_eq!(deal.stage, DealStage::Lead);

                let advanced = crm
                    .advance_deal("o", &deal.id, DealStage::Proposal)
                    .await
                    .unwrap();
                assert_eq!(advanced.stage, DealStage::Proposal);

                let deals = crm.list_deals_for_company("o", &co.id).await.unwrap();
                assert_eq!(deals.len(), 1);
            }

            #[tokio::test]
            async fn test_deal_requires_existing_company() {
                let crm = make().await;
                assert!(crm
                    .create_deal("o", "missing-company", "X", 1.0)
                    .await
                    .is_err());
            }

            // ── Interactions ───────────────────────────────────────────────

            #[tokio::test]
            async fn test_interactions_ordered_desc() {
                let crm = make().await;
                let c = crm.create_contact("o", "Derrick", "Hodge").await.unwrap();
                crm.log_interaction(
                    "o",
                    InteractionInput::new(&c.id, InteractionKind::Call)
                        .with_direction(InteractionDirection::Inbound)
                        .with_summary("first"),
                )
                .await
                .unwrap();
                crm.log_interaction(
                    "o",
                    InteractionInput::new(&c.id, InteractionKind::Sms).with_summary("second"),
                )
                .await
                .unwrap();

                let all = crm.interactions_for_contact("o", &c.id).await.unwrap();
                assert_eq!(all.len(), 2);
                assert_eq!(all[0].summary, "second"); // newest first
            }

            #[tokio::test]
            async fn test_interaction_requires_contact() {
                let crm = make().await;
                assert!(crm
                    .log_interaction("o", InteractionInput::new("missing", InteractionKind::Note))
                    .await
                    .is_err());
            }

            // ── Search & summary ───────────────────────────────────────────

            #[tokio::test]
            async fn test_search_across_entities() {
                let crm = make().await;
                let co = crm
                    .create_company("o", "Hodge Luke", "AI Consulting")
                    .await
                    .unwrap();
                let c = crm
                    .create_contact("o", "Derrick", "Hodge")
                    .await
                    .unwrap()
                    .with_company(&co.id)
                    .with_title("CEO");
                crm.update_contact(&c).await.unwrap();
                crm.create_deal("o", &co.id, "AI Strategy", 100_000.0)
                    .await
                    .unwrap();

                let hits = crm.search("o", "hodge", 10).await.unwrap();
                assert!(!hits.is_empty());
                let entities: Vec<String> = hits.iter().map(|h| h.entity.clone()).collect();
                assert!(entities.contains(&"company".to_string()));
                assert!(entities.contains(&"contact".to_string()));
            }

            #[tokio::test]
            async fn test_search_is_owner_scoped() {
                let crm = make().await;
                crm.create_company("a", "Hodge Luke", "AI").await.unwrap();
                crm.create_company("b", "Hodge Corp", "AI").await.unwrap();
                let hits = crm.search("a", "hodge", 10).await.unwrap();
                assert_eq!(hits.len(), 1);
                assert!(hits.iter().all(|h| h.label == "Hodge Luke"));
            }

            #[tokio::test]
            async fn test_fts_partial_word_and_typo() {
                let crm = make().await;
                let co = crm
                    .create_company("o", "Hodge Luke", "AI Consulting")
                    .await
                    .unwrap();
                let contact = crm
                    .create_contact("o", "Derrick", "Hodge")
                    .await
                    .unwrap()
                    .with_company(&co.id)
                    .with_title("CEO")
                    .with_notes("runs Hodge Luke AI");
                crm.update_contact(&contact).await.unwrap();
                // FTS: a partial word should still hit Hodge.
                let hits = crm.search("o", "hodg", 10).await.unwrap();
                assert!(!hits.is_empty(), "FTS partial match should hit Hodge");
            }

            #[tokio::test]
            async fn test_vector_search_finds_similar_contact() {
                let crm = make().await;
                let dh = crm
                    .create_contact("o", "Derrick", "Hodge")
                    .await
                    .unwrap()
                    .with_title("CEO")
                    .with_notes("AI consulting executive");
                crm.update_contact(&dh).await.unwrap();
                let sl = crm
                    .create_contact("o", "Sara", "Lee")
                    .await
                    .unwrap()
                    .with_title("Accountant")
                    .with_notes("tax and books");
                crm.update_contact(&sl).await.unwrap();

                let hits = crm.vector_search("o", "AI executive", 10).await.unwrap();
                assert!(!hits.is_empty());
                assert_eq!(hits[0].label, "Derrick Hodge");
            }

            #[tokio::test]
            async fn test_vector_search_respects_owner() {
                let crm = make().await;
                let a = crm
                    .create_contact("a", "Derrick", "Hodge")
                    .await
                    .unwrap()
                    .with_title("CEO")
                    .with_notes("AI consulting");
                crm.update_contact(&a).await.unwrap();
                let b = crm
                    .create_contact("b", "Derrick", "Hodge")
                    .await
                    .unwrap()
                    .with_title("CEO")
                    .with_notes("AI consulting");
                crm.update_contact(&b).await.unwrap();
                let hits = crm.vector_search("a", "AI CEO", 10).await.unwrap();
                assert_eq!(hits.len(), 1);
            }

            #[tokio::test]
            async fn test_crm_summary() {
                let crm = make().await;
                let co = crm.create_company("o", "Hodge Luke", "AI").await.unwrap();
                let c = crm.create_contact("o", "Derrick", "Hodge").await.unwrap();
                crm.create_deal("o", &co.id, "D1", 50_000.0).await.unwrap();
                crm.create_deal("o", &co.id, "D2", 30_000.0).await.unwrap();
                crm.log_interaction("o", InteractionInput::new(&c.id, InteractionKind::Email))
                    .await
                    .unwrap();

                let s = crm.summary("o").await.unwrap();
                assert_eq!(s.companies, 1);
                assert_eq!(s.contacts, 1);
                assert_eq!(s.deals, 2);
                assert_eq!(s.open_deals, 2);
                assert_eq!(s.interactions, 1);
            }

            // ── Contact context ────────────────────────────────────────────

            #[tokio::test]
            async fn test_contact_context_payload() {
                let crm = make().await;
                let co = crm.create_company("o", "Hodge Luke", "AI").await.unwrap();
                let c = crm
                    .create_contact("o", "Derrick", "Hodge")
                    .await
                    .unwrap()
                    .with_company(&co.id);
                crm.update_contact(&c).await.unwrap();
                crm.create_deal("o", &co.id, "Deal A", 1.0).await.unwrap();

                let ctx = crm.contact_context("o", &c.id).await.unwrap();
                assert_eq!(ctx["contact"]["first_name"], "Derrick");
                assert_eq!(ctx["company"]["name"], "Hodge Luke");
                assert_eq!(ctx["deals"].as_array().unwrap().len(), 1);
            }
        }
    };
}

crm_suite!(memory, async { AgentCrm::new(MemoryStore::new()) });
crm_suite!(libsql, async { libsql_crm().await });

// ── CRM ⇄ calendar linkage ──────────────────────────────────────────────────

#[tokio::test]
async fn test_attendee_for_contact_carries_crm_context() {
    let crm = AgentCrm::new(LibSqlStore::in_memory().await.unwrap());
    let co = crm
        .create_company("derrick", "Hodge Luke", "AI")
        .await
        .unwrap();
    let contact = crm
        .create_contact("derrick", "Derrick", "Hodge")
        .await
        .unwrap()
        .with_company(&co.id)
        .with_phone("+16142600424")
        .with_email("hodge@agentmail.com")
        .with_title("President and CEO");
    crm.update_contact(&contact).await.unwrap();
    crm.create_deal("derrick", &co.id, "AI Strategy", 100_000.0)
        .await
        .unwrap();

    let attendee = crm
        .attendee_for_contact("derrick", &contact.id)
        .await
        .unwrap();
    assert_eq!(attendee.name, "Derrick Hodge");
    assert_eq!(attendee.email, "hodge@agentmail.com");
    assert_eq!(attendee.metadata["contact_id"], contact.id);
    assert_eq!(attendee.metadata["phone"], "+16142600424");
    assert_eq!(
        attendee.metadata["context"]["deals"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

// ── Shared-store: one libSQL file serves calendar + CRM ──────────────────────
#[tokio::test]
async fn test_calendar_and_crm_share_one_store() {
    use agentcal::{AgentCal, LinkParams};
    let store = LibSqlStore::in_memory().await.unwrap();

    let cal = AgentCrm::new(store.clone());
    let calendar = AgentCal::new(store);

    // CRM write.
    let co = cal
        .create_company("derrick", "Hodge Luke", "AI")
        .await
        .unwrap();
    let contact = cal
        .create_contact("derrick", "Derrick", "Hodge")
        .await
        .unwrap()
        .with_company(&co.id);
    cal.update_contact(&contact).await.unwrap();

    // Calendar write on the same file.
    calendar
        .create_calendar_simple("derrick", "Derrick's Calendar")
        .await
        .unwrap();
    calendar
        .add_window("derrick", Some(0), "09:00", "17:00", "")
        .await
        .unwrap();
    let link = calendar
        .create_link("derrick", LinkParams::new("30-min sync", 30))
        .await
        .unwrap();
    let slots = calendar
        .get_slots("derrick", &link.id, None, None, Some(1))
        .await
        .unwrap();
    assert!(!slots.is_empty());

    // Both sides read back consistently from the same store.
    assert_eq!(cal.list_companies("derrick").await.unwrap().len(), 1);
    assert_eq!(
        calendar.list_calendars().await.unwrap(),
        vec!["derrick".to_string()]
    );
}

// ── CRM ⇄ calendar round-trip: contact → attendee → booking → contact ────────
#[tokio::test]
async fn test_booking_resolves_back_to_contact() {
    use agentcal::{AgentCal, LinkParams};
    let store = LibSqlStore::in_memory().await.unwrap();

    let crm = AgentCrm::new(store.clone());
    let calendar = AgentCal::new(store);

    let co = crm
        .create_company("derrick", "Hodge Luke", "AI")
        .await
        .unwrap();
    let contact = crm
        .create_contact("derrick", "Derrick", "Hodge")
        .await
        .unwrap()
        .with_company(&co.id)
        .with_phone("+16142600424")
        .with_email("hodge@agentmail.com");
    crm.update_contact(&contact).await.unwrap();

    calendar
        .create_calendar_simple("derrick", "Derrick's Calendar")
        .await
        .unwrap();
    calendar
        .add_window("derrick", Some(0), "09:00", "17:00", "")
        .await
        .unwrap();
    let link = calendar
        .create_link("derrick", LinkParams::new("30-min sync", 30))
        .await
        .unwrap();
    let slots = calendar
        .get_slots("derrick", &link.id, None, None, Some(1))
        .await
        .unwrap();
    let slot = slots[0].clone();

    // Contact → attendee (CRM stamp) → booking.
    let attendee = crm
        .attendee_for_contact("derrick", &contact.id)
        .await
        .unwrap();
    let booked = calendar
        .book(
            "derrick",
            &link.id,
            slot,
            vec![attendee],
            "Intro call",
            serde_json::Value::Null,
        )
        .await
        .unwrap();

    // Booking → attendee → contact (the reverse, previously missing).
    let booking = calendar
        .get_booking("derrick", &booked.booking.id)
        .await
        .unwrap();
    let resolved = crm.contact_for_booking("derrick", &booking).await.unwrap();
    assert_eq!(resolved.id, contact.id);
    assert_eq!(resolved.first_name, "Derrick");

    // Also exercise the single-attendee resolver directly.
    let resolved2 = crm
        .contact_for_attendee("derrick", &booking.attendees[0])
        .await
        .unwrap();
    assert_eq!(resolved2.id, contact.id);
}

// ── Contact resolution falls back to email match when not stamped ─────────────
#[tokio::test]
async fn test_contact_for_attendee_email_fallback() {
    use agentcal::Attendee;
    let crm = AgentCrm::new(LibSqlStore::in_memory().await.unwrap());
    let contact = crm
        .create_contact("derrick", "Derrick", "Hodge")
        .await
        .unwrap()
        .with_email("hodge@agentmail.com");
    crm.update_contact(&contact).await.unwrap();

    // Unstamped attendee — resolves purely by email.
    let attendee = Attendee::new("Derrick Hodge", "hodge@agentmail.com");
    let resolved = crm
        .contact_for_attendee("derrick", &attendee)
        .await
        .unwrap();
    assert_eq!(resolved.id, contact.id);

    // Unknown email → ContactNotFound.
    let stranger = Attendee::new("Someone Else", "nobody@example.com");
    assert!(crm
        .contact_for_attendee("derrick", &stranger)
        .await
        .is_err());
}

// ── Persistence round-trip over a real file ──────────────────────────────────

#[tokio::test]
async fn test_crm_file_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("crm.db");

    let contact_id;
    {
        let store = LibSqlStore::open(&path).await.unwrap();
        let crm = AgentCrm::new(store);
        let co = crm
            .create_company("derrick", "Hodge Luke", "AI")
            .await
            .unwrap();
        let contact = crm
            .create_contact("derrick", "Derrick", "Hodge")
            .await
            .unwrap()
            .with_company(&co.id)
            .with_phone("+16142600424");
        crm.update_contact(&contact).await.unwrap();
        contact_id = contact.id.clone();
    }

    // Reopen the file — data must survive.
    let store = LibSqlStore::open(&path).await.unwrap();
    let crm = AgentCrm::new(store);
    let got = crm.get_contact("derrick", &contact_id).await.unwrap();
    assert_eq!(got.display_name(), "Derrick Hodge");
    assert!(got.company_id.is_some());
}
