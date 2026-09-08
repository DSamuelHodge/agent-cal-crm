//! Central error taxonomy tests (ROADMAP Phase 2.1).
//!
//! File-backed libSQL databases (one fresh tempdir per test — never a
//! shared/global DB path). No process-global mutable state anywhere here.

use agentcal::error::{error_catalog, error_info};
use agentcal::rpc::dispatch;
use agentcal::{AgentCal, AgentCrm, AgentError, LibSqlStore, StoreError};

fn all_variants() -> Vec<(AgentError, &'static str, &'static str)> {
    vec![
        (AgentError::CalendarNotFound("c".into()), "calendar_not_found", "user"),
        (AgentError::CalendarAlreadyExists("c".into()), "calendar_already_exists", "user"),
        (AgentError::LinkNotFound("l".into()), "link_not_found", "user"),
        (AgentError::BookingNotFound("b".into()), "booking_not_found", "user"),
        (AgentError::Conflict("x".into()), "conflict", "user"),
        (AgentError::Validation("x".into()), "validation", "user"),
        (AgentError::AttendeeExists("a@b.c".into()), "attendee_exists", "user"),
        (AgentError::BookingFull, "booking_full", "user"),
        (AgentError::ContactNotFound("c".into()), "contact_not_found", "user"),
        (AgentError::CompanyNotFound("c".into()), "company_not_found", "user"),
        (AgentError::DealNotFound("d".into()), "deal_not_found", "user"),
        (AgentError::CrmValidation("x".into()), "crm_validation", "user"),
        (AgentError::ApprovalRequired("id".into()), "approval_required", "approval"),
        (AgentError::ApprovalNotFound("id".into()), "approval_not_found", "approval"),
        (AgentError::ApprovalNotApproved("id".into()), "approval_not_approved", "approval"),
        (
            AgentError::Store(StoreError::Other("boom".into())),
            "store_error",
            "internal",
        ),
    ]
}

#[test]
fn every_variant_maps_to_its_pinned_code() {
    for (e, code, _) in all_variants() {
        assert_eq!(agentcal::actions::error_code(&e), code, "{e:?}");
        assert_eq!(error_info(&e).code, code, "{e:?}");
    }
}

#[test]
fn error_info_categories_match_contract() {
    for (e, _, category) in all_variants() {
        let info = error_info(&e);
        assert_eq!(info.category, category, "{e:?}");
        assert!(
            matches!(info.category, "user" | "approval" | "internal"),
            "unexpected category for {e:?}"
        );
        assert!(!info.meaning.is_empty(), "{e:?}");
        assert!(!info.likely_cause.is_empty(), "{e:?}");
    }
}

#[test]
fn error_catalog_returns_16_entries_with_all_four_keys() {
    let catalog = error_catalog();
    assert_eq!(catalog.len(), 16);
    let value = serde_json::to_value(&catalog).unwrap();
    let arr = value.as_array().unwrap();
    assert_eq!(arr.len(), 16);
    let mut codes = std::collections::HashSet::new();
    for entry in arr {
        for key in ["code", "category", "meaning", "likely_cause"] {
            let v = entry.get(key).unwrap_or_else(|| panic!("missing {key} in {entry}"));
            assert!(v.is_string(), "{key} must be a string in {entry}");
            assert!(!v.as_str().unwrap().is_empty(), "{key} must be non-empty in {entry}");
        }
        assert!(codes.insert(entry["code"].as_str().unwrap().to_string()), "duplicate code {entry}");
    }
    // Enum-declaration order: spot-check endpoints.
    assert_eq!(arr[0]["code"], "calendar_not_found");
    assert_eq!(arr[15]["code"], "store_error");
}

async fn file_ctx() -> (tempfile::TempDir, AgentCal, AgentCrm) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LibSqlStore::open(dir.path().join("errors.db"))
        .await
        .expect("open store");
    let cal = AgentCal::new(store.clone());
    let crm = AgentCrm::new(store);
    (dir, cal, crm)
}

#[tokio::test]
async fn rpc_error_catalog_serves_16_entries_owner_optional() {
    let (_dir, cal, crm) = file_ctx().await;
    // No params at all.
    let no_params = dispatch(&cal, &crm, "error.catalog", &serde_json::json!({}))
        .await
        .unwrap();
    // With owner — accepted but ignored, same payload.
    let with_owner = dispatch(
        &cal,
        &crm,
        "error.catalog",
        &serde_json::json!({"owner": "derrick"}),
    )
    .await
    .unwrap();
    let other_owner = dispatch(
        &cal,
        &crm,
        "error.catalog",
        &serde_json::json!({"owner": "someone-else"}),
    )
    .await
    .unwrap();
    assert_eq!(no_params, with_owner);
    assert_eq!(with_owner, other_owner);

    let arr = with_owner.as_array().unwrap();
    assert_eq!(arr.len(), 16);
    for entry in arr {
        for key in ["code", "category", "meaning", "likely_cause"] {
            assert!(entry.get(key).is_some(), "catalog entry missing {key}: {entry}");
        }
    }
    // Codes match the pinned unit table.
    let expected: Vec<&str> = all_variants().into_iter().map(|(_, c, _)| c).collect();
    let got: Vec<&str> = arr.iter().map(|e| e["code"].as_str().unwrap()).collect();
    assert_eq!(got, expected);
}
