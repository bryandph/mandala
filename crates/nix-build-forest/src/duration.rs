use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{DerivationStatus, ForestSnapshot};

pub const DURATION_CACHE_RELATIVE_PATH: &str = "build/durations.json";
const CACHE_VERSION: u64 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
struct BuildDuration {
    samples: u64,
    total_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DurationCache {
    version: u64,
    durations: BTreeMap<String, BuildDuration>,
}

impl Default for DurationCache {
    fn default() -> Self {
        Self {
            version: CACHE_VERSION,
            durations: BTreeMap::new(),
        }
    }
}

impl DurationCache {
    /// Load the versioned cache. A missing file is an empty cache; malformed
    /// or future-version data is reported so callers can ignore it without
    /// silently overwriting information they do not understand.
    pub fn load(path: &Path) -> io::Result<Self> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        let cache: Self = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if cache.version != CACHE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported duration-cache version {}", cache.version),
            ));
        }
        Ok(cache)
    }

    /// Atomically replace the cache with deterministic, newline-terminated
    /// JSON. The temp file lives beside the destination so rename stays on one
    /// filesystem.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        bytes.push(b'\n');
        let temp = path.with_extension("json.tmp");
        fs::write(&temp, bytes)?;
        fs::rename(temp, path)
    }

    /// Average historical duration in milliseconds for every derivation name.
    #[must_use]
    pub fn estimates_ms(&self) -> BTreeMap<String, u64> {
        self.durations
            .iter()
            .filter(|(_, duration)| duration.samples > 0)
            .map(|(name, duration)| (name.clone(), duration.total_ms / duration.samples))
            .collect()
    }

    /// Fold successful durations from a terminal snapshot into the history.
    /// Failed builds are excluded because their elapsed time is not an
    /// estimate of how long a successful realization takes.
    pub fn observe_snapshot(&mut self, snapshot: &ForestSnapshot) {
        for node in &snapshot.nodes {
            if node.status != DerivationStatus::Built {
                continue;
            }
            let Some(elapsed_ms) = node
                .started_ms
                .zip(node.finished_ms)
                .map(|(start, finish)| finish.saturating_sub(start))
                .filter(|elapsed| *elapsed > 0)
            else {
                continue;
            };
            let duration = self.durations.entry(node.name.clone()).or_default();
            duration.samples = duration.samples.saturating_add(1);
            duration.total_ms = duration.total_ms.saturating_add(elapsed_ms);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::{DerivationNode, ForestCounts};

    use super::*;

    fn snapshot(name: &str, elapsed_ms: u64) -> ForestSnapshot {
        ForestSnapshot {
            version: 1,
            elapsed_ms,
            nodes: vec![DerivationNode {
                path: format!("/nix/store/hash-{name}.drv"),
                name: name.to_string(),
                status: DerivationStatus::Built,
                inputs: Vec::new(),
                outputs: BTreeMap::new(),
                host: None,
                last_activity: None,
                log_tail: Vec::new(),
                note: None,
                started_ms: Some(100),
                finished_ms: Some(100 + elapsed_ms),
                eta_seconds: None,
            }],
            roots: Vec::new(),
            counts: ForestCounts {
                built: 1,
                ..ForestCounts::default()
            },
            activity: Default::default(),
            completed_downloads: 0,
            completed_substitutions: 0,
            transfers: Vec::new(),
            recent_logs: Vec::new(),
            expectations: Vec::new(),
            current_activity: Vec::new(),
            failed_derivations: Vec::new(),
            errors: Vec::new(),
            unknown_activity_types: BTreeMap::new(),
            unknown_result_types: BTreeMap::new(),
            unknown_actions: 0,
            malformed_messages: 0,
            ignored_lines: 0,
        }
    }

    #[test]
    fn cache_bytes_and_average_are_golden() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "mandala-duration-cache-{}-{stamp}",
            std::process::id()
        ));
        let path = dir.join(DURATION_CACHE_RELATIVE_PATH);
        let mut cache = DurationCache::default();
        cache.observe_snapshot(&snapshot("demo", 1_000));
        cache.observe_snapshot(&snapshot("demo", 3_000));
        cache.save(&path).unwrap();

        let bytes = fs::read_to_string(&path).unwrap();
        assert_eq!(
            bytes,
            "{\n  \"version\": 1,\n  \"durations\": {\n    \"demo\": {\n      \"samples\": 2,\n      \"total_ms\": 4000\n    }\n  }\n}\n"
        );
        assert_eq!(DurationCache::load(&path).unwrap().estimates_ms()["demo"], 2_000);
        fs::remove_dir_all(dir).unwrap();
    }
}
