//! Idempotent starter data for the Chief of Staff operating picture.

use crate::crm::agent::AgentCrm;
use crate::crm::types::DealStage;
use crate::crm::{InteractionDirection, InteractionInput, InteractionKind};
use crate::error::Result;

/// Seed the CoS with real starter data. Idempotent: skips companies,
/// contacts and deals that already exist.
pub async fn seed_cos(crm: &AgentCrm, owner: &str) -> Result<()> {
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

    if crm
        .list_deals_for_company(owner, &company.id)
        .await?
        .is_empty()
    {
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
