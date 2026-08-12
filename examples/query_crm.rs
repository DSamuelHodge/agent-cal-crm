//! Ad-hoc query example: prove FTS + vector search against a seeded DB file.
//! Run: cargo run --example query_crm -- .agentcal/cos.db
use agentcal::AgentCrm;
use agentcal::LibSqlStore;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".agentcal/cos.db".to_string());
    let crm = AgentCrm::new(LibSqlStore::open(&path).await?);

    println!("== FTS search 'hodge' ==");
    for h in crm.search("derrick", "hodge", 10).await? {
        println!("  [{}] {} — {}", h.entity, h.label, h.snippet);
    }
    println!("== FTS search 'hodg' (prefix) ==");
    for h in crm.search("derrick", "hodg", 10).await? {
        println!("  [{}] {} — {}", h.entity, h.label, h.snippet);
    }
    println!("== vector search 'AI consulting' ==");
    for h in crm.vector_search("derrick", "AI consulting", 10).await? {
        println!("  [{}] {} — {}", h.entity, h.label, h.snippet);
    }
    Ok(())
}
