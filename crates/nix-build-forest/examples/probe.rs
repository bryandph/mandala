use nix_build_forest::{BuildForest, FsDrvReader};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: cargo run -p nix-build-forest --example probe -- /nix/store/….drv")?;
    let mut forest = BuildForest::new();
    forest.feed_line(&format!(
        r#"@nix {{"action":"start","id":1,"type":105,"fields":["{path}","",1,1]}}"#
    ));
    forest.resolve_pending(&FsDrvReader).await;
    let snapshot = forest.snapshot();
    let unavailable = snapshot
        .nodes
        .iter()
        .filter(|node| node.note.is_some())
        .count();
    println!(
        "{}",
        serde_json::json!({
            "root": path,
            "roots": snapshot.roots,
            "nodes": snapshot.nodes.len(),
            "unavailable": unavailable,
            "elapsed_ms": snapshot.elapsed_ms,
        })
    );
    Ok(())
}
