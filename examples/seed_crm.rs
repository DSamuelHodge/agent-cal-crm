//! Seed the Chief of Staff CRM with real starter data.
//!
//! Usage: `cargo run --example seed_crm -- <path-to-db>` (default `.agentcal/cos.db`).
//! Idempotent: skips companies/contacts that already exist.

use std::path::PathBuf;

use agentcal::{
    AgentCrm, DealStage, InteractionDirection, InteractionInput, InteractionKind, LibSqlStore,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".agentcal/cos.db".to_string());
    let store = LibSqlStore::open(PathBuf::from(&path)).await?;
    let crm = AgentCrm::new(store);

    seed(&crm, "derrick").await?;
    println!("seeded {path}");
    let s = crm.summary("derrick").await?;
    println!("summary: {s:?}");
    Ok(())
}

async fn seed(crm: &AgentCrm, owner: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Company.
    let company = match crm
        .list_companies(owner)
        .await?
        .into_iter()
        .find(|c| c.name == "Hodge Luke")
    {
        Some(c) => c,
        None => {
            let c = crm
                .create_company(owner, "Hodge Luke", "AI Consulting")
                .await?;
            crm.update_company(&c.clone().with_stage(DealStage::ClosedWon))
                .await?;
            c
        }
    };

    // Derrick Hodge.
    if crm.resolve_by_phone(owner, "+16142600424").await?.is_none() {
        let c = crm
            .create_contact(owner, "Derrick", "Hodge")
            .await?
            .with_company(&company.id)
            .with_phone("+16142600424")
            .with_email("hodge@agentmail.com")
            .with_title("President and CEO")
            .with_vip();
        crm.update_contact(&c).await?;
        crm.log_interaction(
            owner,
            InteractionInput::new(&c.id, InteractionKind::Call)
                .with_direction(InteractionDirection::Inbound)
                .with_summary("Discovered via CoS search; CEO of Hodge Luke."),
        )
        .await?;
    }

    // Narahari Luke.
    if crm
        .resolve_by_email(owner, "narahari@hodgeluke.com")
        .await?
        .is_none()
    {
        let c = crm
            .create_contact(owner, "Narahari", "Luke")
            .await?
            .with_company(&company.id)
            .with_phone("+16145550199")
            .with_email("narahari@hodgeluke.com")
            .with_title("Co-Founder");
        crm.update_contact(&c).await?;
    }

    // An open deal.
    let has_deal = !crm
        .list_deals_for_company(owner, &company.id)
        .await?
        .is_empty();
    if !has_deal {
        crm.create_deal(
            owner,
            &company.id,
            "Enterprise AI scaling engagement",
            250_000.0,
        )
        .await?;
    }

    Ok(())
}
