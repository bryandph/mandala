//! Bounded, replayable subset of Nix's internal logger. No raw archive is kept.
use serde_json::Value;
use std::collections::HashSet;

/// Hard per-run build.jsonl budget, including envelope bytes and final status.
pub const BUILD_LOG_MAX_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const TERMINAL_RESERVE: u64 = 64 * 1024;

#[derive(Debug, Default)]
pub(crate) struct BuildLogFilter {
    activities: HashSet<u64>,
}

impl BuildLogFilter {
    pub fn retain(&mut self, line: &str) -> bool {
        let Ok(Some(message)) = nix_build_forest::parse_nix_line(line) else {
            return false;
        };
        let id = message.id.unwrap_or(0);
        match message.action.as_str() {
            // Exclude QueryPathInfo (109), which accounts for most narinfo chatter.
            "start" if matches!(message.type_code, Some(100..=105 | 108 | 110..=112)) => {
                self.activities.insert(id);
                true
            }
            "stop" => self.activities.remove(&id),
            // Keep phase/progress/expected and diagnostic results, not build-log lines.
            "result" => {
                self.activities.contains(&id) && matches!(message.type_code, Some(102..=106 | 108))
            }
            "msg" => message.level.is_some_and(|level| level <= 1),
            _ => false,
        }
    }
}

/// Shorten oversized diagnostic fields before serializing. Preserve JSON and
/// UTF-8 boundaries; never cut a serialized JSONL record in the middle.
pub(crate) fn bound_strings(value: &mut Value) {
    match value {
        Value::String(text) if text.len() > 8192 => {
            let mut end = 8192;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push_str("… [truncated]");
        }
        Value::Array(values) => values.iter_mut().for_each(bound_strings),
        Value::Object(values) => values.values_mut().for_each(bound_strings),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_stream_compacts_and_cap_preserves_terminal_state() {
        use crate::runner::{EventTailer, EventWriter};
        use serde_json::json;
        let dir =
            std::env::temp_dir().join(format!("mandala-build-log-cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let writer = EventWriter::new(&dir, "build", "controller", "build").unwrap();
        let before = std::cell::Cell::new(0usize);
        let emit = |line: String| {
            before.set(before.get() + line.len() + 1);
            writer
                .emit(
                    "nixlog",
                    [("line".into(), Value::from(line))].into_iter().collect(),
                )
                .unwrap()
        };
        for _ in 0..20_000 {
            let line = format!(
                "@nix {}",
                json!({"action":"result","id":99,"type":101,"fields":["x".repeat(1024)]})
            );
            emit(line);
        }
        for kind in [100, 105] {
            emit(format!(
                "@nix {}",
                json!({"action":"start","id":kind,"type":kind,"fields":["/nix/store/example.drv"]})
            ));
            emit(format!(
                "@nix {}",
                json!({"action":"result","id":kind,"type":105,"fields":[1,2,0,0]})
            ));
            emit(format!("@nix {}", json!({"action":"stop","id":kind})));
        }
        emit(r#"@nix {"action":"msg","level":0,"msg":"synthetic failure"}"#.into());
        let compact = std::fs::metadata(dir.join("build.jsonl")).unwrap().len();
        eprintln!(
            "LAB-106 synthetic raw_internal_json={} compact_jsonl={compact} bytes",
            before.get()
        );
        assert!(compact < 4096);
        let text = std::fs::read_to_string(dir.join("build.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 7);
        assert!(text.contains("synthetic failure"));
        // Even retained errors/ordinary stderr cannot exceed the hard cap.
        for _ in 0..3000 {
            emit(format!(
                "@nix {}",
                json!({"action":"msg","level":0,"msg":"e".repeat(8192)})
            ));
        }
        writer
            .emit(
                "progress",
                json!({"built":1,"finished":1,"errors":3001,"final":true})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        writer
            .emit(
                "status",
                json!({"state":"done","rc":1}).as_object().unwrap().clone(),
            )
            .unwrap();
        let size = std::fs::metadata(dir.join("build.jsonl")).unwrap().len();
        eprintln!("LAB-106 capped={size} limit={BUILD_LOG_MAX_BYTES} bytes");
        assert!(size <= BUILD_LOG_MAX_BYTES);
        let text = std::fs::read_to_string(dir.join("build.jsonl")).unwrap();
        assert_eq!(text.matches("build log size cap reached").count(), 1);
        assert!(
            text.lines()
                .all(|line| serde_json::from_str::<Value>(line).is_ok())
        );
        let mut tailer = EventTailer::new(&dir);
        tailer.poll();
        assert!(tailer.build.done);
        assert_eq!(tailer.build.rc, Some(1));
        assert_eq!(tailer.build.errors, 3001);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn drops_narinfo_and_build_lines_but_keeps_lifecycle_results_and_errors() {
        let mut filter = BuildLogFilter::default();
        for id in 1..=20_000 {
            assert!(!filter.retain(&format!(
                "@nix {{\"action\":\"start\",\"id\":{id},\"type\":109}}"
            )));
            assert!(!filter.retain(&format!("@nix {{\"action\":\"stop\",\"id\":{id}}}")));
        }
        for kind in [100, 101, 103, 105, 108] {
            assert!(filter.retain(&format!(
                "@nix {{\"action\":\"start\",\"id\":7,\"type\":{kind}}}"
            )));
            assert!(
                !filter.retain(r#"@nix {"action":"result","id":7,"type":101,"fields":["line"]}"#)
            );
            for result in [102, 103, 104, 105, 106, 108] {
                assert!(filter.retain(&format!(
                    "@nix {{\"action\":\"result\",\"id\":7,\"type\":{result}}}"
                )));
            }
            assert!(filter.retain(r#"@nix {"action":"stop","id":7}"#));
        }
        assert!(filter.retain(r#"@nix {"action":"msg","level":0,"msg":"failed"}"#));
        assert!(!filter.retain(r#"@nix {"action":"msg","level":3,"msg":"chatter"}"#));
        assert!(filter.activities.is_empty());
    }
}
