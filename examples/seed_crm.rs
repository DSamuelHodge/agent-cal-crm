//! Seed the Chief of Staff CRM with real starter data.
//!
//! Usage: `cargo run --example seed_crm -- <path-to-db>` (default `.agentcal/cos.db`).
//! Idempotent: skips companies/contacts that already exist.

use std::path::PathBuf;

use agentcal::{AgentCrm, LibSqlStore};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".agentcal/cos.db".to_string());
    let store = LibSqlStore::open(PathBuf::from(&path)).await?;
    let crm = AgentCrm::new(store);

    agentcal::seed::seed_cos(&crm, "derrick").await?;
    println!("seeded {path}");
    let s = crm.summary("derrick").await?;
    println!("summary: {s:?}");
    Ok(())
}
