//! Activity events + the persistent audit trail.
//!
//! rust-mcp-sdk has no middleware concept, so the Python `ActivityMiddleware`
//! becomes a dispatch wrapper (see `server.rs`): every tool call funnels
//! through one `call_tool`, which emits TWICE — a `start` event as the tool
//! begins (so a long eval or survey is visible while it runs, not only after
//! it lands) and an `ok`/`error` event when it settles; the two share a `seq`
//! so a watcher can pair them even with concurrent same-named calls in
//! flight. The settle event also carries `elapsed` (seconds) and a small
//! `result` summary (ok/refused/run_id when the tool returned them) — enough
//! for a pane to show durations and for the phase-2 TUI to attach the exact
//! run a tool registered. The sink must never break a tool call.
//!
//! [`audit_event`] is the persistent trail: settled MUTATING calls are
//! appended to `state_dir()/mcp/audit.jsonl` regardless of transport, so a
//! headless stdio session leaves the same record a TUI operator watches live.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use mandala_core::drift::state_dir;
use serde_json::Value;

/// A pluggable receiver for activity events (`{tool, args, status, detail,
/// seq}` plus `elapsed`/`result` on settle). The phase-2 TUI feeds its
/// activity pane from this; the stdio server default is no sink. The sink is
/// called synchronously on the dispatch path — it must be cheap and must not
/// panic.
pub type ActivitySink = Arc<dyn Fn(&Value) + Send + Sync>;

/// Tools whose settled calls land in the audit trail: everything that can
/// change fleet state (or swap what the server serves).
const AUDITED: [&str; 4] = ["deploy", "reboot", "restart_service", "reload"];

/// Unix epoch seconds as a float — the audit `ts`, parity with Python
/// `time.time()`.
fn now_epoch_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Append a settled mutating call to the per-user audit log
/// (`state_dir()/mcp/audit.jsonl`, one JSON object per line, `ts` first-class).
/// Best effort — an unwritable state dir must never sink a tool call.
pub fn audit_event(event: &Value) {
    let status = event.get("status").and_then(Value::as_str);
    let tool = event.get("tool").and_then(Value::as_str).unwrap_or("");
    if status == Some("start") || !AUDITED.contains(&tool) {
        return;
    }
    let Some(obj) = event.as_object() else {
        return;
    };
    let mut line = serde_json::Map::new();
    line.insert("ts".to_string(), Value::from(now_epoch_f64()));
    for (k, v) in obj {
        line.insert(k.clone(), v.clone());
    }
    let path = state_dir().join("mcp").join("audit.jsonl");
    let write = || -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        use std::io::Write;
        let mut fh = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(fh, "{}", Value::Object(line))?;
        fh.flush()
    };
    let _ = write();
}

/// The few result fields a watcher acts on (ok/refused/run_id), pulled from a
/// tool's structured result. `None` when the result carries none of them.
#[must_use]
pub fn result_summary(result: &Value) -> Option<Value> {
    let obj = result.as_object()?;
    let mut summary = serde_json::Map::new();
    for key in ["ok", "refused", "run_id"] {
        if let Some(v) = obj.get(key) {
            summary.insert(key.to_string(), v.clone());
        }
    }
    if summary.is_empty() {
        None
    } else {
        Some(Value::Object(summary))
    }
}
