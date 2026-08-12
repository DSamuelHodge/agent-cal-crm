//! CRM for the Chief of Staff engine — contacts, companies, deals, interactions.
//!
//! Mirrors the calendar module's structure:
//!   • `types`  — pure serde data (Contact, Company, Deal, Interaction, …)
//!   • `store`  — the `CrmStore` trait (what every store must implement)
//!   • `libsql` — `CrmStore` over the shared libSQL connection
//!   • `agent`  — `AgentCrm` façade, the one struct agents call

pub mod agent;
pub mod libsql;
pub mod store;
pub mod types;

pub use agent::AgentCrm;
pub use store::{CrmStore, InteractionInput, SearchHit};
pub use types::{
    Company, Contact, CrmSummary, Deal, DealStage, Interaction, InteractionDirection,
    InteractionKind,
};
