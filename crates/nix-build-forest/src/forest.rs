use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Instant;

use serde::Serialize;
use serde_json::Value;

use crate::drv::{Derivation, DrvReader, parse_derivation};
use crate::msg::{ActivityType, NixMessage, ResultType, parse_nix_line};
use crate::sort::{ActivityProjection, DEFAULT_ACTIVITY_ROW_BUDGET, activity_projection};

pub const DERIVATION_LOG_TAIL_LIMIT: usize = 64;
pub const RECENT_LOG_LIMIT: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DerivationStatus {
    Unknown,
    Planned,
    Building,
    Downloading,
    Substituting,
    Built,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DerivationNode {
    pub path: String,
    pub name: String,
    pub status: DerivationStatus,
    pub inputs: Vec<String>,
    pub outputs: BTreeMap<String, String>,
    pub host: Option<String>,
    pub last_activity: Option<String>,
    pub log_tail: Vec<String>,
    pub note: Option<String>,
    pub started_ms: Option<u64>,
    pub finished_ms: Option<u64>,
    /// Reserved for the optional duration-cache extension.
    pub eta_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ActivityProgress {
    pub done: u64,
    pub expected: u64,
    pub running: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BuildLogLine {
    pub derivation: String,
    pub name: String,
    pub line: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ActivityExpectation {
    pub kind: ActivityType,
    pub expected: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Transfer {
    pub id: u64,
    pub kind: ActivityType,
    pub path: Option<String>,
    pub host: Option<String>,
    pub source: Option<String>,
    pub destination: Option<String>,
    pub progress: Option<ActivityProgress>,
    pub text: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ForestCounts {
    pub unknown: usize,
    pub planned: usize,
    pub building: usize,
    pub downloading: usize,
    pub substituting: usize,
    pub built: usize,
    pub failed: usize,
}

impl ForestCounts {
    #[must_use]
    pub fn total(self) -> usize {
        self.unknown
            + self.planned
            + self.building
            + self.downloading
            + self.substituting
            + self.built
            + self.failed
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForestSnapshot {
    pub version: u64,
    pub elapsed_ms: u64,
    pub nodes: Vec<DerivationNode>,
    pub roots: Vec<String>,
    pub counts: ForestCounts,
    pub activity: ActivityProjection,
    pub completed_downloads: u64,
    pub completed_substitutions: u64,
    pub transfers: Vec<Transfer>,
    pub recent_logs: Vec<BuildLogLine>,
    pub expectations: Vec<ActivityExpectation>,
    pub current_activity: Vec<String>,
    pub failed_derivations: Vec<String>,
    pub errors: Vec<String>,
    pub unknown_activity_types: BTreeMap<i64, u64>,
    pub unknown_result_types: BTreeMap<i64, u64>,
    pub unknown_actions: u64,
    pub malformed_messages: u64,
    pub ignored_lines: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedOutcome {
    Ignored,
    Accepted,
    Malformed,
}

#[derive(Debug, Clone)]
struct NodeState {
    status: DerivationStatus,
    inputs: BTreeSet<String>,
    outputs: BTreeMap<String, String>,
    host: Option<String>,
    last_activity: Option<String>,
    log_tail: VecDeque<String>,
    note: Option<String>,
    started_ms: Option<u64>,
    finished_ms: Option<u64>,
}

impl Default for NodeState {
    fn default() -> Self {
        Self {
            status: DerivationStatus::Unknown,
            inputs: BTreeSet::new(),
            outputs: BTreeMap::new(),
            host: None,
            last_activity: None,
            log_tail: VecDeque::new(),
            note: None,
            started_ms: None,
            finished_ms: None,
        }
    }
}

#[derive(Debug, Clone)]
struct ActiveActivity {
    kind: ActivityType,
    path: Option<String>,
    host: Option<String>,
    source: Option<String>,
    destination: Option<String>,
    progress: Option<ActivityProgress>,
    expectations: BTreeMap<ActivityType, u64>,
    text: String,
}

#[derive(Debug, Clone)]
pub struct BuildForest {
    started: Instant,
    version: u64,
    nodes: BTreeMap<String, NodeState>,
    output_to_drv: BTreeMap<String, String>,
    activities: BTreeMap<u64, ActiveActivity>,
    pending_drvs: BTreeSet<String>,
    drv_cache: BTreeMap<String, Option<Derivation>>,
    errors: Vec<String>,
    unknown_activity_types: BTreeMap<i64, u64>,
    unknown_result_types: BTreeMap<i64, u64>,
    unknown_actions: u64,
    malformed_messages: u64,
    ignored_lines: u64,
    completed_downloads: u64,
    completed_substitutions: u64,
    duration_estimates_ms: BTreeMap<String, u64>,
    recent_logs: VecDeque<BuildLogLine>,
    log_sequence: u64,
}

impl Default for BuildForest {
    fn default() -> Self {
        Self::new()
    }
}

impl BuildForest {
    #[must_use]
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            version: 0,
            nodes: BTreeMap::new(),
            output_to_drv: BTreeMap::new(),
            activities: BTreeMap::new(),
            pending_drvs: BTreeSet::new(),
            drv_cache: BTreeMap::new(),
            errors: Vec::new(),
            unknown_activity_types: BTreeMap::new(),
            unknown_result_types: BTreeMap::new(),
            unknown_actions: 0,
            malformed_messages: 0,
            ignored_lines: 0,
            completed_downloads: 0,
            completed_substitutions: 0,
            duration_estimates_ms: BTreeMap::new(),
            recent_logs: VecDeque::new(),
            log_sequence: 0,
        }
    }

    #[must_use]
    pub fn with_duration_estimates(duration_estimates_ms: BTreeMap<String, u64>) -> Self {
        Self {
            duration_estimates_ms,
            ..Self::new()
        }
    }

    fn elapsed_ms(&self) -> u64 {
        self.started
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    pub fn feed_line(&mut self, line: &str) -> FeedOutcome {
        match parse_nix_line(line) {
            Ok(Some(message)) => {
                self.feed_message(message);
                FeedOutcome::Accepted
            }
            Ok(None) => {
                self.ignored_lines += 1;
                FeedOutcome::Ignored
            }
            Err(_) => {
                self.malformed_messages += 1;
                FeedOutcome::Malformed
            }
        }
    }

    pub fn feed_message(&mut self, message: NixMessage) {
        self.version = self.version.saturating_add(1);
        match message.action.as_str() {
            "start" => self.start(message),
            "stop" => self.stop(message.id),
            "result" => self.result(message),
            "msg" => self.message(message),
            _ => self.unknown_actions = self.unknown_actions.saturating_add(1),
        }
    }

    fn start(&mut self, message: NixMessage) {
        let Some(id) = message.id else { return };
        let kind = message.activity_type().unwrap_or(ActivityType::Unknown);
        if let ActivityType::Other(code) = kind {
            *self.unknown_activity_types.entry(code).or_default() += 1;
        }
        let path = message
            .fields
            .first()
            .and_then(Value::as_str)
            .map(str::to_string);
        let host = activity_host(kind, &message.fields);
        let (source, destination) = activity_endpoints(kind, &message.fields);
        let text = message.text.unwrap_or_default();

        match kind {
            ActivityType::Build => {
                if let Some(path) = path.as_deref().filter(|path| path.ends_with(".drv")) {
                    let now = self.elapsed_ms();
                    let node = self.nodes.entry(path.to_string()).or_default();
                    node.status = DerivationStatus::Building;
                    node.host.clone_from(&host);
                    node.started_ms.get_or_insert(now);
                    node.last_activity = (!text.is_empty()).then(|| text.clone());
                    self.pending_drvs.insert(path.to_string());
                }
            }
            ActivityType::CopyPath | ActivityType::FileTransfer => {
                self.mark_transfer_node(path.as_deref(), DerivationStatus::Downloading, &host);
            }
            ActivityType::Substitute => {
                self.mark_transfer_node(path.as_deref(), DerivationStatus::Substituting, &host);
            }
            _ => {}
        }
        self.activities.insert(
            id,
            ActiveActivity {
                kind,
                path,
                host,
                source,
                destination,
                progress: None,
                expectations: BTreeMap::new(),
                text,
            },
        );
    }

    fn stop(&mut self, id: Option<u64>) {
        let Some(id) = id else { return };
        let Some(activity) = self.activities.remove(&id) else {
            return;
        };
        let now = self.elapsed_ms();
        match activity.kind {
            ActivityType::Build => {
                if let Some(path) = activity.path {
                    let node = self.nodes.entry(path).or_default();
                    if node.status != DerivationStatus::Failed {
                        node.status = DerivationStatus::Built;
                        node.finished_ms = Some(now);
                    }
                }
            }
            ActivityType::CopyPath | ActivityType::FileTransfer | ActivityType::Substitute => {
                if let Some(output) = activity.path
                    && let Some(path) = self.output_to_drv.get(&output).cloned()
                {
                    let node = self.nodes.entry(path).or_default();
                    if node.status != DerivationStatus::Failed {
                        node.status = DerivationStatus::Built;
                        node.finished_ms = Some(now);
                    }
                }
                match activity.kind {
                    ActivityType::CopyPath => {
                        self.completed_downloads = self.completed_downloads.saturating_add(1);
                    }
                    ActivityType::Substitute => {
                        self.completed_substitutions =
                            self.completed_substitutions.saturating_add(1);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn result(&mut self, message: NixMessage) {
        let result = message.result_type();
        if let Some(ResultType::Other(code)) = result {
            *self.unknown_result_types.entry(code).or_default() += 1;
        }
        let Some(id) = message.id else { return };
        let Some(activity_path) = self
            .activities
            .get(&id)
            .map(|activity| activity.path.clone())
        else {
            return;
        };
        match result {
            Some(ResultType::BuildLogLine | ResultType::PostBuildLogLine) => {
                if let Some(path) = activity_path.as_deref()
                    && let Some(text) = message.fields.first().and_then(Value::as_str)
                {
                    self.push_log(path, text);
                }
            }
            Some(ResultType::SetPhase | ResultType::FetchStatus) => {
                if let Some(path) = activity_path.as_deref()
                    && let Some(text) = message.fields.first().and_then(Value::as_str)
                {
                    self.nodes
                        .entry(path.to_string())
                        .or_default()
                        .last_activity = Some(text.to_string());
                }
            }
            Some(ResultType::Progress) => {
                if let Some(progress) = activity_progress(&message.fields)
                    && let Some(activity) = self.activities.get_mut(&id)
                {
                    activity.progress = Some(progress);
                }
            }
            Some(ResultType::SetExpected) => {
                if let Some((kind, expected)) = activity_expectation(&message.fields)
                    && let Some(activity) = self.activities.get_mut(&id)
                {
                    activity.expectations.insert(kind, expected);
                }
            }
            _ => {}
        }
    }

    fn push_log(&mut self, path: &str, text: &str) {
        let node = self.nodes.entry(path.to_string()).or_default();
        node.last_activity = Some(text.to_string());
        push_bounded(
            &mut node.log_tail,
            text.to_string(),
            DERIVATION_LOG_TAIL_LIMIT,
        );
        push_bounded(
            &mut self.recent_logs,
            BuildLogLine {
                derivation: path.to_string(),
                name: derivation_name(path),
                line: text.to_string(),
            },
            RECENT_LOG_LIMIT,
        );
        self.log_sequence = self.log_sequence.saturating_add(1);
    }

    #[must_use]
    pub fn log_sequence(&self) -> u64 {
        self.log_sequence
    }

    #[must_use]
    pub fn latest_log(&self) -> Option<&BuildLogLine> {
        self.recent_logs.back()
    }

    fn message(&mut self, message: NixMessage) {
        let Some(text) = message.message_text().map(str::to_string) else {
            return;
        };
        let paths = drv_paths(&text);
        if message.level == Some(0) {
            self.errors.push(text);
            let now = self.elapsed_ms();
            for path in paths {
                let node = self.nodes.entry(path).or_default();
                node.status = DerivationStatus::Failed;
                node.finished_ms = Some(now);
            }
            return;
        }
        for path in paths {
            let node = self.nodes.entry(path.clone()).or_default();
            if node.status == DerivationStatus::Unknown {
                node.status = DerivationStatus::Planned;
            }
            self.pending_drvs.insert(path);
        }
    }

    fn mark_transfer_node(
        &mut self,
        output: Option<&str>,
        status: DerivationStatus,
        host: &Option<String>,
    ) {
        let Some(output) = output else { return };
        let Some(path) = self.output_to_drv.get(output).cloned() else {
            return;
        };
        let now = self.elapsed_ms();
        let node = self.nodes.entry(path).or_default();
        node.status = status;
        node.host.clone_from(host);
        node.started_ms.get_or_insert(now);
    }

    /// Resolve every currently known `.drv`, recursively. Reads are async and
    /// every success/failure is memoized. A missing or malformed derivation is
    /// retained as a leaf with a note and never fails the message stream.
    pub async fn resolve_pending<R: DrvReader>(&mut self, reader: &R) {
        loop {
            let paths = self.take_pending_derivations();
            if paths.is_empty() {
                break;
            }
            for path in paths {
                let parsed = match reader.read_drv(&path).await {
                    Ok(contents) => match parse_derivation(&contents) {
                        Ok(drv) => Some(drv),
                        Err(error) => {
                            self.apply_derivation_error(&path, format!("unparseable drv: {error}"));
                            None
                        }
                    },
                    Err(error) => {
                        self.apply_derivation_error(&path, format!("drv unavailable: {error}"));
                        None
                    }
                };
                if let Some(drv) = parsed {
                    self.apply_derivation(path, drv);
                }
            }
        }
    }

    /// Drain unresolved derivation paths for an external/background reader.
    /// Applying a result may enqueue more inputs, so callers should repeat
    /// until this returns an empty vector.
    pub fn take_pending_derivations(&mut self) -> Vec<String> {
        let mut paths = Vec::new();
        while let Some(path) = self.pending_drvs.pop_first() {
            if !self.drv_cache.contains_key(&path) {
                paths.push(path);
            }
        }
        paths
    }

    /// Merge one successfully parsed background derivation read.
    pub fn apply_derivation(&mut self, path: String, drv: Derivation) {
        self.backfill(&path, &drv);
        self.drv_cache.insert(path, Some(drv));
        self.version = self.version.saturating_add(1);
    }

    /// Memoize a failed background derivation read without dropping the leaf.
    pub fn apply_derivation_error(&mut self, path: &str, note: String) {
        self.nodes.entry(path.to_string()).or_default().note = Some(note);
        self.drv_cache.insert(path.to_string(), None);
        self.version = self.version.saturating_add(1);
    }

    fn backfill(&mut self, path: &str, drv: &Derivation) {
        let node = self.nodes.entry(path.to_string()).or_default();
        node.inputs.extend(drv.input_drvs.keys().cloned());
        node.outputs.clone_from(&drv.outputs);
        for output in drv.outputs.values() {
            self.output_to_drv.insert(output.clone(), path.to_string());
        }
        for input in drv.input_drvs.keys() {
            self.nodes.entry(input.clone()).or_default();
            if !self.drv_cache.contains_key(input) {
                self.pending_drvs.insert(input.clone());
            }
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> ForestSnapshot {
        let mut parents = BTreeSet::new();
        for node in self.nodes.values() {
            parents.extend(node.inputs.iter().cloned());
        }
        let roots = self
            .nodes
            .keys()
            .filter(|path| !parents.contains(*path))
            .cloned()
            .collect();
        let elapsed_ms = self.elapsed_ms();
        let nodes: Vec<DerivationNode> = self
            .nodes
            .iter()
            .map(|(path, node)| {
                let name = derivation_name(path);
                let eta_seconds = if matches!(
                    node.status,
                    DerivationStatus::Built | DerivationStatus::Failed
                ) {
                    None
                } else {
                    self.duration_estimates_ms.get(&name).map(|estimate| {
                        let spent = node
                            .started_ms
                            .map_or(0, |started| elapsed_ms.saturating_sub(started));
                        estimate.saturating_sub(spent).div_ceil(1_000)
                    })
                };
                DerivationNode {
                    path: path.clone(),
                    name,
                    status: node.status,
                    inputs: node.inputs.iter().cloned().collect(),
                    outputs: node.outputs.clone(),
                    host: node.host.clone(),
                    last_activity: node.last_activity.clone(),
                    log_tail: node.log_tail.iter().cloned().collect(),
                    note: node.note.clone(),
                    started_ms: node.started_ms,
                    finished_ms: node.finished_ms,
                    eta_seconds,
                }
            })
            .collect();
        let counts = nodes
            .iter()
            .fold(ForestCounts::default(), |mut counts, node| {
                match node.status {
                    DerivationStatus::Unknown => counts.unknown += 1,
                    DerivationStatus::Planned => counts.planned += 1,
                    DerivationStatus::Building => counts.building += 1,
                    DerivationStatus::Downloading => counts.downloading += 1,
                    DerivationStatus::Substituting => counts.substituting += 1,
                    DerivationStatus::Built => counts.built += 1,
                    DerivationStatus::Failed => counts.failed += 1,
                }
                counts
            });
        let transfers = self
            .activities
            .iter()
            .filter(|(_, activity)| {
                matches!(
                    activity.kind,
                    ActivityType::CopyPath | ActivityType::FileTransfer | ActivityType::Substitute
                )
            })
            .map(|(id, activity)| Transfer {
                id: *id,
                kind: activity.kind,
                path: activity.path.clone(),
                host: activity.host.clone(),
                source: activity.source.clone(),
                destination: activity.destination.clone(),
                progress: activity.progress,
                text: activity.text.clone(),
            })
            .collect();
        let expectations = self
            .activities
            .values()
            .flat_map(|activity| {
                activity
                    .expectations
                    .iter()
                    .map(|(kind, expected)| ActivityExpectation {
                        kind: *kind,
                        expected: *expected,
                    })
            })
            .collect();
        let current_activity = self
            .activities
            .values()
            .filter(|activity| !activity.text.is_empty())
            .map(|activity| activity.text.clone())
            .collect();
        let failed_derivations = nodes
            .iter()
            .filter(|node| node.status == DerivationStatus::Failed)
            .map(|node| node.name.clone())
            .collect();
        let mut snapshot = ForestSnapshot {
            version: self.version,
            elapsed_ms,
            nodes,
            roots,
            counts,
            activity: ActivityProjection::default(),
            completed_downloads: self.completed_downloads,
            completed_substitutions: self.completed_substitutions,
            transfers,
            recent_logs: self.recent_logs.iter().cloned().collect(),
            expectations,
            current_activity,
            failed_derivations,
            errors: self.errors.clone(),
            unknown_activity_types: self.unknown_activity_types.clone(),
            unknown_result_types: self.unknown_result_types.clone(),
            unknown_actions: self.unknown_actions,
            malformed_messages: self.malformed_messages,
            ignored_lines: self.ignored_lines,
        };
        snapshot.activity = activity_projection(&snapshot, DEFAULT_ACTIVITY_ROW_BUDGET);
        snapshot
    }
}

fn push_bounded<T>(queue: &mut VecDeque<T>, value: T, limit: usize) {
    if queue.len() == limit {
        queue.pop_front();
    }
    queue.push_back(value);
}

fn activity_progress(fields: &[Value]) -> Option<ActivityProgress> {
    Some(ActivityProgress {
        done: fields.first()?.as_u64()?,
        expected: fields.get(1)?.as_u64()?,
        running: fields.get(2)?.as_u64()?,
        failed: fields.get(3)?.as_u64()?,
    })
}

fn activity_expectation(fields: &[Value]) -> Option<(ActivityType, u64)> {
    let kind = ActivityType::from_code(fields.first()?.as_i64()?);
    let expected = fields.get(1)?.as_u64()?;
    Some((kind, expected))
}

fn activity_endpoints(kind: ActivityType, fields: &[Value]) -> (Option<String>, Option<String>) {
    let value = |index| {
        fields
            .get(index)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
    };
    match kind {
        ActivityType::CopyPath => (value(1).map(str::to_string), value(2).map(str::to_string)),
        ActivityType::FileTransfer => (value(0).map(str::to_string), None),
        ActivityType::Substitute => (value(1).map(str::to_string), None),
        _ => (None, None),
    }
}

fn activity_host(kind: ActivityType, fields: &[Value]) -> Option<String> {
    let index = match kind {
        ActivityType::Build | ActivityType::Substitute => 1,
        ActivityType::CopyPath => 2,
        _ => return None,
    };
    fields
        .get(index)
        .and_then(Value::as_str)
        .filter(|host| !host.is_empty())
        .map(str::to_string)
}

fn derivation_name(path: &str) -> String {
    let base = path
        .rsplit_once('/')
        .map_or(path, |(_, basename)| basename)
        .strip_suffix(".drv")
        .unwrap_or(path);
    match base.split_once('-') {
        Some((hash, name)) if hash.len() == 32 => name.to_string(),
        _ => base.to_string(),
    }
}

fn drv_paths(text: &str) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for (start, _) in text.match_indices("/nix/store/") {
        let suffix = &text[start..];
        let end = suffix
            .char_indices()
            .find_map(|(at, c)| {
                (c.is_whitespace() || matches!(c, '\'' | '"' | '\u{1b}' | ')' | ']' | ','))
                    .then_some(at)
            })
            .unwrap_or(suffix.len());
        let candidate = suffix[..end].trim_end_matches(['.', ':', ';']);
        if candidate.ends_with(".drv") {
            paths.insert(candidate.to_string());
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

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
    async fn graph_backfills_and_missing_drv_degrades_to_leaf() {
        let root = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-root.drv";
        let child = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-child.drv";
        let missing = "/nix/store/cccccccccccccccccccccccccccccccc-missing.drv";
        let mut reader = MapReader::default();
        reader.0.insert(
            root.into(),
            format!(
                r#"Derive([("out","/nix/store/root-out","")],[("{child}",["out"]),("{missing}",["out"])],[],"aarch64-darwin","/builder",[],[])"#
            ),
        );
        reader.0.insert(
            child.into(),
            r#"Derive([("out","/nix/store/child-out","")],[],[],"aarch64-darwin","/builder",[],[])"#.into(),
        );
        let mut forest = BuildForest::new();
        forest.feed_line(&format!(
            r#"@nix {{"action":"start","id":1,"type":105,"fields":["{root}","",1,1]}}"#
        ));
        forest.resolve_pending(&reader).await;
        let snapshot = forest.snapshot();
        assert_eq!(snapshot.roots, [root]);
        assert_eq!(snapshot.nodes.len(), 3);
        let leaf = snapshot
            .nodes
            .iter()
            .find(|node| node.path == missing)
            .unwrap();
        assert!(leaf.note.as_deref().unwrap().contains("drv unavailable"));
    }

    #[test]
    fn failed_message_wins_over_build_stop() {
        let drv = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-failure.drv";
        let mut forest = BuildForest::new();
        forest.feed_line(&format!(
            r#"@nix {{"action":"start","id":1,"type":105,"fields":["{drv}","",1,1]}}"#
        ));
        forest.feed_line(r#"@nix {"action":"stop","id":1}"#);
        forest.feed_line(&format!(
            r#"@nix {{"action":"msg","level":0,"msg":"error: Cannot build '{drv}'."}}"#
        ));
        let snapshot = forest.snapshot();
        assert_eq!(snapshot.counts.failed, 1);
        assert_eq!(snapshot.counts.built, 0);
        assert_eq!(snapshot.failed_derivations, ["failure"]);
    }

    #[test]
    fn unknown_protocol_is_counted_not_fatal() {
        let mut forest = BuildForest::new();
        assert_eq!(
            forest.feed_line(r#"@nix {"action":"start","id":1,"type":999,"extra":true}"#),
            FeedOutcome::Accepted
        );
        forest.feed_line(r#"@nix {"action":"future-action"}"#);
        let snapshot = forest.snapshot();
        assert_eq!(snapshot.unknown_activity_types[&999], 1);
        assert_eq!(snapshot.unknown_actions, 1);
    }

    #[test]
    fn build_logs_are_owned_and_bounded() {
        let drv = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-chatty.drv";
        let mut forest = BuildForest::new();
        forest.feed_line(&format!(
            r#"@nix {{"action":"start","id":1,"type":105,"fields":["{drv}","",1,1]}}"#
        ));
        for index in 0..100 {
            forest.feed_line(&format!(
                r#"@nix {{"action":"result","id":1,"type":101,"fields":["line-{index}"]}}"#
            ));
        }
        let snapshot = forest.snapshot();
        let node = snapshot.nodes.iter().find(|node| node.path == drv).unwrap();
        assert_eq!(node.log_tail.len(), DERIVATION_LOG_TAIL_LIMIT);
        assert_eq!(node.log_tail.first().map(String::as_str), Some("line-36"));
        assert_eq!(node.log_tail.last().map(String::as_str), Some("line-99"));
        assert_eq!(snapshot.recent_logs.len(), 100);
        assert_eq!(forest.log_sequence(), 100);
        assert_eq!(
            forest.latest_log().map(|log| log.line.as_str()),
            Some("line-99")
        );
    }

    #[test]
    fn concurrent_build_logs_preserve_interleaved_ownership() {
        let first = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-first.drv";
        let second = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-second.drv";
        let mut forest = BuildForest::new();
        for (id, drv) in [(1, first), (2, second)] {
            forest.feed_line(&format!(
                r#"@nix {{"action":"start","id":{id},"type":105,"fields":["{drv}","",1,1]}}"#
            ));
        }
        forest.feed_line(
            r#"@nix {"action":"result","id":1,"type":101,"fields":["first-a"]}"#,
        );
        forest.feed_line(
            r#"@nix {"action":"result","id":2,"type":101,"fields":["second-a"]}"#,
        );
        forest.feed_line(
            r#"@nix {"action":"result","id":1,"type":101,"fields":["first-b"]}"#,
        );
        let snapshot = forest.snapshot();
        assert_eq!(
            snapshot
                .recent_logs
                .iter()
                .map(|log| (log.derivation.as_str(), log.line.as_str()))
                .collect::<Vec<_>>(),
            [(first, "first-a"), (second, "second-a"), (first, "first-b")]
        );
    }

    #[test]
    fn transfer_endpoints_progress_and_expectations_are_typed() {
        let mut forest = BuildForest::new();
        forest.feed_line(
            r#"@nix {"action":"start","id":1,"type":100,"fields":["/nix/store/item","ssh://source","ssh://dest"],"text":"copying item"}"#,
        );
        forest.feed_line(r#"@nix {"action":"result","id":1,"type":105,"fields":[5,10,1,0]}"#);
        forest.feed_line(
            r#"@nix {"action":"start","id":2,"type":102,"fields":[],"text":"realising"}"#,
        );
        forest.feed_line(r#"@nix {"action":"result","id":2,"type":106,"fields":[100,3]}"#);
        let snapshot = forest.snapshot();
        let transfer = snapshot.transfers.first().unwrap();
        assert_eq!(transfer.source.as_deref(), Some("ssh://source"));
        assert_eq!(transfer.destination.as_deref(), Some("ssh://dest"));
        assert_eq!(
            transfer.progress,
            Some(ActivityProgress {
                done: 5,
                expected: 10,
                running: 1,
                failed: 0,
            })
        );
        assert_eq!(
            snapshot.expectations,
            [ActivityExpectation {
                kind: ActivityType::CopyPath,
                expected: 3,
            }]
        );
    }

    #[test]
    fn incomplete_progress_and_endpoints_remain_absent() {
        let mut forest = BuildForest::new();
        forest.feed_line(
            r#"@nix {"action":"start","id":1,"type":101,"fields":[],"text":"transfer"}"#,
        );
        forest.feed_line(r#"@nix {"action":"result","id":1,"type":105,"fields":[1]}"#);
        let transfer = forest.snapshot().transfers.into_iter().next().unwrap();
        assert_eq!(transfer.source, None);
        assert_eq!(transfer.destination, None);
        assert_eq!(transfer.progress, None);
    }
}
