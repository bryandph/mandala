use std::collections::BTreeMap;
use std::io;
use std::time::{Duration, Instant};

use nix_build_forest::{BuildForest, DrvReader, ForestSnapshot};
use serde_json::Value;

const SMALL: &[u8] = include_bytes!("fixtures/small.nix-log.zst");
const MEDIUM: &[u8] = include_bytes!("fixtures/medium.nix-log.zst");
const FAILURE: &[u8] = include_bytes!("fixtures/failure.nix-log.zst");

fn replay(bytes: &[u8]) -> BuildForest {
    let decoded = zstd::stream::decode_all(bytes).expect("valid compressed fixture");
    let stream = String::from_utf8(decoded).expect("fixture is UTF-8");
    let mut forest = BuildForest::new();
    for line in stream.lines() {
        forest.feed_line(line);
    }
    forest
}

fn assert_outcome(snapshot: &ForestSnapshot, built: usize, failed: usize) {
    assert_eq!(snapshot.counts.built, built);
    assert_eq!(snapshot.counts.failed, failed);
    assert_eq!(snapshot.completed_downloads, 0);
    assert_eq!(snapshot.completed_substitutions, 0);
    assert_eq!(snapshot.unknown_activity_types, BTreeMap::new());
    assert_eq!(snapshot.unknown_result_types, BTreeMap::new());
    assert_eq!(snapshot.unknown_actions, 0);
    assert_eq!(snapshot.malformed_messages, 0);
    assert_eq!(snapshot.ignored_lines, 0);
}

#[test]
fn recorded_small_outcome() {
    assert_outcome(&replay(SMALL).snapshot(), 1, 0);
}

#[test]
fn recorded_medium_outcome() {
    assert_outcome(&replay(MEDIUM).snapshot(), 4, 0);
}

#[test]
fn recorded_failure_attributes_the_drv() {
    let snapshot = replay(FAILURE).snapshot();
    assert_outcome(&snapshot, 0, 1);
    assert_eq!(snapshot.failed_derivations, ["mandala-forest-failure-v4"]);
    assert_eq!(snapshot.errors.len(), 1);
}

#[derive(Default)]
struct MapReader(BTreeMap<String, String>);

impl DrvReader for MapReader {
    async fn read_drv(&self, path: &str) -> io::Result<String> {
        self.0
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path.to_string()))
    }
}

#[tokio::test]
async fn recorded_medium_graph_has_the_observed_shape() {
    let leaf_a = "/nix/store/ywkyv5g91gf5hk0mxj9f4h8cx993xhyl-mandala-forest-leaf-a-v4.drv";
    let leaf_b = "/nix/store/zpcwa73c0qhs5sv13r6gjzhh4rqiyac7-mandala-forest-leaf-b-v4.drv";
    let middle = "/nix/store/rf51fv47189k24018ihgv409n3j0yqx4-mandala-forest-middle-v4.drv";
    let root = "/nix/store/3ay34q2x8r88isgxmm6qvqhh5ljb6m2y-mandala-forest-root-v4.drv";
    let leaf_drv = |name: &str| {
        format!(
            r#"Derive([("out","/nix/store/{name}-out","")],[],[],"aarch64-darwin","/builder",[],[])"#
        )
    };
    let mut reader = MapReader::default();
    reader.0.insert(leaf_a.into(), leaf_drv("leaf-a"));
    reader.0.insert(leaf_b.into(), leaf_drv("leaf-b"));
    reader.0.insert(
        middle.into(),
        format!(
            r#"Derive([("out","/nix/store/middle-out","")],[("{leaf_a}",["out"]),("{leaf_b}",["out"])],[],"aarch64-darwin","/builder",[],[])"#
        ),
    );
    reader.0.insert(
        root.into(),
        format!(
            r#"Derive([("out","/nix/store/root-out","")],[("{middle}",["out"])],[],"aarch64-darwin","/builder",[],[])"#
        ),
    );

    let mut forest = replay(MEDIUM);
    forest.resolve_pending(&reader).await;
    let snapshot = forest.snapshot();
    assert_eq!(snapshot.roots, [root]);
    let by_path: BTreeMap<&str, &Vec<String>> = snapshot
        .nodes
        .iter()
        .map(|node| (node.path.as_str(), &node.inputs))
        .collect();
    assert_eq!(by_path[root], &[middle]);
    assert_eq!(by_path[middle], &[leaf_a, leaf_b]);
    assert!(by_path[leaf_a].is_empty());
    assert!(by_path[leaf_b].is_empty());
}

#[test]
fn nom_parity_evidence_matches_the_golden_outcomes() {
    let evidence: Value = serde_json::from_str(include_str!("fixtures/nom-parity.json")).unwrap();
    for (name, bytes) in [("small", SMALL), ("medium", MEDIUM), ("failure", FAILURE)] {
        let snapshot = replay(bytes).snapshot();
        let expected = &evidence["corpus"][name]["forest"];
        assert_eq!(
            snapshot.counts.built as u64,
            expected["built"].as_u64().unwrap()
        );
        assert_eq!(
            snapshot.counts.failed as u64,
            expected["failed"].as_u64().unwrap()
        );
        assert_eq!(
            snapshot.completed_downloads,
            expected["downloads"].as_u64().unwrap()
        );
    }
}

#[test]
fn synthetic_volume_stream_is_bounded() {
    let started = Instant::now();
    let mut forest = BuildForest::new();
    let drv = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-volume.drv";
    for id in 1..=25_000u64 {
        forest.feed_line(&format!(
            r#"@nix {{"action":"start","id":{id},"type":105,"fields":["{drv}","",1,1]}}"#
        ));
        forest.feed_line(&format!(r#"@nix {{"action":"stop","id":{id}}}"#));
    }
    let snapshot = forest.snapshot();
    assert_eq!(snapshot.nodes.len(), 1);
    assert!(snapshot.transfers.is_empty());
    assert!(snapshot.current_activity.is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "50k messages took {:?}",
        started.elapsed()
    );
}
