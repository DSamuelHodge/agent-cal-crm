//! Fully-gated daemon mode via environment (`COS_FULLY_GATED`).
//!
//! Lives in its own test binary on purpose: it mutates process-global env,
//! which would race with parallel `dispatch` tests in `tests/approvals.rs`.

use agentcal::approvals::ApprovalConfig;
use agentcal::rpc::dispatch;
use agentcal::{AgentCal, AgentCrm, LibSqlStore};

/// Restore-guard for one env var (restores the previous value on drop, even
/// across assertion panics).
struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, val: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, val);
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

#[tokio::test]
async fn fully_gated_env_holds_sends_and_writes() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = LibSqlStore::open(dir.path().join("env.db")).await.unwrap();
    let cal = AgentCal::new(store.clone());
    let crm = AgentCrm::new(store);

    // Sanity: tiered default auto-approves a low-risk write request.
    let r = dispatch(
        &cal,
        &crm,
        "approval.request",
        &serde_json::json!({
            "owner": "env_owner",
            "method": "crm.create_contact",
            "params": {"owner": "env_owner"},
        }),
    )
    .await
    .unwrap();
    assert_eq!(r["state"], "approved");

    // Fully-gated mode: the same request stays pending…
    let _guard = EnvGuard::set("COS_FULLY_GATED", "1");
    assert!(ApprovalConfig::from_env().fully_gated);
    let r = dispatch(
        &cal,
        &crm,
        "approval.request",
        &serde_json::json!({
            "owner": "env_owner",
            "method": "crm.create_contact",
            "params": {"owner": "env_owner", "first_name": "G"},
        }),
    )
    .await
    .unwrap();
    assert_eq!(r["state"], "pending");
    // …while reads still auto-approve.
    let r = dispatch(
        &cal,
        &crm,
        "approval.request",
        &serde_json::json!({
            "owner": "env_owner",
            "method": "crm.summary",
            "params": {"owner": "env_owner"},
        }),
    )
    .await
    .unwrap();
    assert_eq!(r["state"], "approved");
}
