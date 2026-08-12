use libsql::Builder;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = Builder::new_local(":memory:").build().await?;
    let conn = db.connect()?;
    conn.execute_batch(
        "CREATE TABLE items (label TEXT, embedding F32_BLOB(4));
         INSERT INTO items (label, embedding) VALUES
           ('napoleon', vector32('[0.800, 0.579, 0.481, 0.229]')),
           ('gladiator', vector32('[0.698, 0.140, 0.073, 0.125]'));
         CREATE INDEX items_idx ON items (libsql_vector_idx(embedding));",
    )
    .await?;
    let mut rows = conn
        .query(
            "SELECT label, vector_distance_cos(embedding, vector32('[0.7,0.5,0.4,0.2]')) AS d
         FROM items ORDER BY d ASC LIMIT 1",
            (),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let label: String = row.get(0)?;
        let d: f64 = row.get(1)?;
        println!("nearest: {label} (cos {d:.3})");
    }
    let mut rows = conn
        .query(
            "SELECT label FROM vector_top_k('items_idx', vector32('[0.7,0.5,0.4,0.2]'), 1)
         JOIN items ON items.rowid = id",
            (),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let label: String = row.get(0)?;
        println!("top_k: {label}");
    }
    Ok(())
}
