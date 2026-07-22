use crate::{ActivityType, BuildLogLine, DerivationStatus, ForestSnapshot, Transfer};

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
    if let Some(eta) = snapshot
        .nodes
        .iter()
        .filter_map(|node| node.eta_seconds)
        .max()
    {
        line.push_str(&format!(" · ETA ~{eta}s"));
    }
    let active: Vec<String> = snapshot
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node.status,
                DerivationStatus::Building
                    | DerivationStatus::Downloading
                    | DerivationStatus::Substituting
            )
        })
        .take(3)
        .map(|node| {
            node.host
                .as_ref()
                .map_or_else(|| node.name.clone(), |host| format!("{}@{host}", node.name))
        })
        .collect();
    if !active.is_empty() {
        line.push_str(" — active: ");
        line.push_str(&active.join(", "));
    } else if let Some(activity) = snapshot.current_activity.last() {
        line.push_str(" — ");
        line.push_str(activity);
    }
    if let Some(transfer) = snapshot.transfers.first() {
        line.push_str(" · ");
        line.push_str(&render_transfer(transfer));
    }
    if let Some(log) = snapshot.recent_logs.last() {
        line.push_str(" · log: ");
        line.push_str(&render_log(log));
    }
    line
}

#[must_use]
pub fn render_log(log: &BuildLogLine) -> String {
    format!("[{}] {}", log.name, log.line)
}

fn render_transfer(transfer: &Transfer) -> String {
    let verb = match transfer.kind {
        ActivityType::FileTransfer => "downloading",
        ActivityType::CopyPath => "copying",
        ActivityType::Substitute => "substituting",
        _ => "transferring",
    };
    let item = transfer.path.as_deref().unwrap_or("item");
    let mut text = format!("{verb} {item}");
    if let Some(progress) = transfer.progress {
        text.push_str(&format!(" {}/{}", progress.done, progress.expected));
    }
    match (&transfer.source, &transfer.destination) {
        (Some(source), Some(destination)) => text.push_str(&format!(" {source} -> {destination}")),
        (Some(source), None) => text.push_str(&format!(" from {source}")),
        (None, Some(destination)) => text.push_str(&format!(" to {destination}")),
        (None, None) => {}
    }
    text
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
    use std::collections::BTreeMap;

    use crate::BuildForest;

    use super::*;

    #[test]
    fn renderers_need_no_tty() {
        let mut forest =
            BuildForest::with_duration_estimates(BTreeMap::from([("demo".to_string(), 5_000)]));
        forest.feed_line(r#"@nix {"action":"start","id":1,"type":105,"fields":["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-demo.drv","",1,1],"text":"building demo"}"#);
        let live = render_live(&forest.snapshot());
        assert!(live.contains("1 building"));
        assert!(live.contains("ETA ~5s"));
        forest.feed_line(r#"@nix {"action":"stop","id":1}"#);
        let final_line = render_final(&forest.snapshot());
        assert!(final_line.contains("1 built"));
        assert!(!final_line.contains('\r'));
    }
}
