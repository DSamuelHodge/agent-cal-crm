//! Proof that the embedded libsql build supports native vector search
//! (F32_BLOB, vector32, vector_top_k via DiskANN). Run: cargo run --example vector_demo
use libsql::Builder;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = Builder::new_local(":memory:").build().await?;
    let conn = db.connect()?;
    conn.execute_batch(
        "CREATE TABLE movies (title TEXT, embedding F32_BLOB(4));
         INSERT INTO movies (title, embedding) VALUES
           ('Napoleon', vector32('[0.800, 0.579, 0.481, 0.229]')),
           ('Gladiator', vector32('[0.698, 0.140, 0.073, 0.125]'));
         CREATE INDEX movies_idx ON movies (libsql_vector_idx(embedding));",
    )
    .await?;
    let mut rows = conn
        .query(
            "SELECT title FROM vector_top_k('movies_idx', vector32('[0.7,0.5,0.4,0.2]'), 1)
         JOIN movies ON movies.rowid = id",
            (),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let title: String = row.get(0)?;
        println!("top_k: {title}");
    }
    Ok(())
}
