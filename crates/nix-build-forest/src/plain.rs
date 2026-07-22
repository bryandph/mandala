use crate::ForestSnapshot;

#[must_use]
pub fn render_live(snapshot: &ForestSnapshot) -> String {
    let c = snapshot.counts;
    let mut line = format!(
        "nix build: {}/{} built · {} building · {} waiting · {} fetching",
        c.built,
        c.total(),
        c.building,
        c.planned,
        c.downloading + c.substituting
    );
    if c.failed > 0 {
        line.push_str(&format!(" · {} failed", c.failed));
    }
    if let Some(activity) = snapshot.current_activity.last() {
        line.push_str(" — ");
        line.push_str(activity);
    }
    line
}

#[must_use]
pub fn render_final(snapshot: &ForestSnapshot) -> String {
    let c = snapshot.counts;
    let mut line = format!(
        "nix build finished: {} built, {} downloaded/substituted, {} failed in {:.1}s",
        c.built,
        snapshot.completed_downloads + snapshot.completed_substitutions,
        c.failed,
        snapshot.elapsed_ms as f64 / 1000.0
    );
    if !snapshot.failed_derivations.is_empty() {
        line.push_str(" (failed: ");
        line.push_str(&snapshot.failed_derivations.join(", "));
        line.push(')');
    }
    line
}

#[cfg(test)]
mod tests {
    use crate::BuildForest;

    use super::*;

    #[test]
    fn renderers_need_no_tty() {
        let mut forest = BuildForest::new();
        forest.feed_line(r#"@nix {"action":"start","id":1,"type":105,"fields":["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-demo.drv","",1,1],"text":"building demo"}"#);
        let live = render_live(&forest.snapshot());
        assert!(live.contains("1 building"));
        forest.feed_line(r#"@nix {"action":"stop","id":1}"#);
        let final_line = render_final(&forest.snapshot());
        assert!(final_line.contains("1 built"));
        assert!(!final_line.contains('\r'));
    }
}
