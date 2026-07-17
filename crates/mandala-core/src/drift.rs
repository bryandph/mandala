//! Deployed-generation drift: contract vs reported fleet state.
//!
//! A parity port of `cli/src/mandala_fleet/drift.py`. The data path mirrors the
//! survey pattern: the read-only state playbook (`mandala.fleet.state`) fans
//! out, reads each member's `/run/current-system` and `/run/booted-system`
//! links plus each system's boot-critical facts, and writes one JSON snapshot
//! per host on the controller. This module compares those snapshots against the
//! locally evaluated toplevels (`nixosConfigurations.<h>.config.system.build
//! .toplevel.outPath` — what `/run/current-system` points at after a successful
//! deploy), routing the expected-eval through [`crate::eval::Evaluator`].
//!
//! Everything here is read-only: snapshots are files, expectations are a nix
//! eval, refresh is a fact-gather playbook. Nothing mutates a host.
//!
//! Drift is EXACT out-path equality — deliberately strict: a moved contract IS
//! drift. Time gets the same strictness: snapshots older than the staleness
//! threshold judge as [`DriftStatus::Stale`] rather than pretending an old
//! observation is current.
//!
//! A booted/current split is judged by its boot-critical subset — kernel,
//! kernel-modules, initrd, kernel-params: what `switch-to-configuration` cannot
//! apply live (the same quad `nixos-needsreboot` compares). Only a change there
//! is [`DriftStatus::RebootPending`]; otherwise the new generation is fully live
//! and reports [`DriftStatus::Activated`].
//!
//! ## State-dir contract
//!
//! State lives under `$MANDALA_FLEET_STATE`, else `$XDG_STATE_HOME/mandala/
//! fleet`, else `~/.local/state/mandala/fleet` (resolved at CALL time, not
//! import time): per-user, persistent across reboots, and not a predictable
//! world-writable-parent `/tmp` path another local user could pre-seed.
//! Snapshots are keyed by FILENAME — the survey writes `<inventory_hostname>
//! .json` — never by a host field inside the file, so one file cannot
//! impersonate another host.
//!
//! ## Cross-implementation formats (fleet-state-formats)
//!
//! The `.expected.json` cache is a byte-level compatibility contract: the
//! Python porcelain must read what this writes and vice-versa. Python emits it
//! via `json.dumps(..., indent=1, sort_keys=True)` — one-space indent, sorted
//! keys — so [`save_expected`] matches that byte-for-byte (a 1-space
//! [`serde_json::ser::PrettyFormatter`] over sorted `BTreeMap`s), not
//! serde_json's default 2-space pretty.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{DateTime, NaiveDateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::eval::Evaluator;

/// The evaluated-expectations cache filename. Cached keyed by the contract's
/// git rev: equal CLEAN revs guarantee an identical contract, so the slow
/// toplevel eval is reusable until the repo actually moves — and a key mismatch
/// IS the "contract moved since last eval" signal.
const EXPECTED_CACHE: &str = ".expected.json";

/// Past this age a snapshot no longer supports any in-sync/drift claim
/// (mirrors the Python `DEFAULT_MAX_AGE = timedelta(hours=24)`).
#[must_use]
pub fn default_max_age() -> TimeDelta {
    TimeDelta::hours(24)
}

/// The parity error type for drift operations — the Rust equivalent of the
/// Python `ValueError` raised by `eval_expected`, plus the surfaced evaluator
/// error. Its [`std::fmt::Display`] reproduces the Python message text so
/// callers surface identical strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriftError {
    /// A member name failed the `^[A-Za-z0-9._-]+$` guard before eval (a name
    /// containing `''` would escape the Nix indented string).
    InvalidMemberName(String),
    /// Evaluating the expected toplevels failed (surfaced from the evaluator).
    Eval(String),
}

impl std::fmt::Display for DriftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidMemberName(name) => {
                write!(f, "refusing to eval: invalid member name {name:?}")
            }
            Self::Eval(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for DriftError {}

/// The snapshot/cache directory, resolved at call time.
///
/// `$MANDALA_FLEET_STATE` wins; else `$XDG_STATE_HOME/mandala/fleet`; else
/// `~/.local/state/mandala/fleet`. Empty env values are treated as unset (the
/// Python `os.environ.get(...) or default` truthiness).
#[must_use]
pub fn state_dir() -> PathBuf {
    if let Ok(env) = std::env::var("MANDALA_FLEET_STATE")
        && !env.is_empty()
    {
        return PathBuf::from(env);
    }
    let xdg = std::env::var("XDG_STATE_HOME")
        .ok()
        .filter(|s| !s.is_empty());
    let base = match xdg {
        Some(x) => PathBuf::from(x),
        None => home_dir().join(".local/state"),
    };
    base.join("mandala").join("fleet")
}

/// The user's home directory (`Path.home()` reads `$HOME` on unix).
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Deployed-generation drift status. The string values are a stable contract —
/// the MCP `drift` tool surfaces them verbatim — so they are pinned via serde
/// `rename` and [`DriftStatus::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriftStatus {
    /// Current == expected (and, if booted, nothing awaiting a reboot).
    #[serde(rename = "in-sync")]
    InSync,
    /// Current != expected: a moved contract (EXACT out-path inequality).
    #[serde(rename = "drift")]
    Drift,
    /// booted != current AND a boot-critical fact moved — awaits a reboot.
    #[serde(rename = "reboot-pending")]
    RebootPending,
    /// booted != current but nothing boot-critical moved — fully live.
    #[serde(rename = "activated")]
    Activated,
    /// Snapshot too old to support a judgement.
    #[serde(rename = "stale")]
    Stale,
    /// Snapshot exists but lacks the system links (a broken fact-gather).
    #[serde(rename = "incomplete")]
    Incomplete,
    /// Never surveyed.
    #[serde(rename = "no-snapshot")]
    NoSnapshot,
    /// The survey could not reach the host.
    #[serde(rename = "unreachable")]
    Unreachable,
}

impl DriftStatus {
    /// Every status, for exhaustive iteration (the parity of `set(DriftStatus)`).
    pub const ALL: [DriftStatus; 8] = [
        DriftStatus::InSync,
        DriftStatus::Drift,
        DriftStatus::RebootPending,
        DriftStatus::Activated,
        DriftStatus::Stale,
        DriftStatus::Incomplete,
        DriftStatus::NoSnapshot,
        DriftStatus::Unreachable,
    ];

    /// The stable string value (what the MCP `drift` tool surfaces).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DriftStatus::InSync => "in-sync",
            DriftStatus::Drift => "drift",
            DriftStatus::RebootPending => "reboot-pending",
            DriftStatus::Activated => "activated",
            DriftStatus::Stale => "stale",
            DriftStatus::Incomplete => "incomplete",
            DriftStatus::NoSnapshot => "no-snapshot",
            DriftStatus::Unreachable => "unreachable",
        }
    }

    /// The one styling vocabulary for every presentation surface (rich CLI
    /// table, TUI drift tab) — kept beside the enum, and exhaustive by an
    /// unavoidable `match`, so a new status CANNOT ship without a style (the
    /// UIs would otherwise KeyError on it). Parity with the Python
    /// `STATUS_STYLE` dict.
    #[must_use]
    pub fn style(self) -> &'static str {
        match self {
            DriftStatus::InSync => "green",
            DriftStatus::Drift => "bold red",
            DriftStatus::RebootPending => "yellow",
            DriftStatus::Activated => "dim green",
            DriftStatus::Stale => "dim yellow",
            DriftStatus::Incomplete => "dim red",
            DriftStatus::NoSnapshot => "dim",
            DriftStatus::Unreachable => "magenta",
        }
    }
}

impl std::fmt::Display for DriftStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One host's drift verdict — the port of the Python `DriftEntry` dataclass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DriftEntry {
    /// Inventory hostname (the snapshot file stem / deploy node name).
    pub host: String,
    /// The judged status.
    pub status: DriftStatus,
    /// The locally evaluated expected toplevel out-path, if known.
    pub expected: Option<String>,
    /// The host's reported `/run/current-system` target.
    pub current: Option<String>,
    /// The host's reported `/run/booted-system` target.
    pub booted: Option<String>,
    /// The snapshot's capture timestamp (ISO-8601), if recorded.
    pub captured_at: Option<String>,
}

/// A per-host state snapshot written by the state playbook. Typed for the
/// fields drift reads; extra fields (the survey carries more) are tolerated by
/// serde's default unknown-field behavior. The boot facts and `unreachable`
/// are kept as raw [`Value`] to reproduce the Python truthiness / `isinstance`
/// checks exactly (a non-object `current_boot` judges conservatively rather
/// than dropping the whole snapshot).
#[derive(Debug, Clone, Deserialize)]
pub struct Snapshot {
    /// `/run/current-system` target (`None`/absent → incomplete survey).
    #[serde(default)]
    pub current: Option<String>,
    /// `/run/booted-system` target.
    #[serde(default)]
    pub booted: Option<String>,
    /// ISO-8601 capture time (naive assumed UTC — the playbook writes UTC).
    #[serde(default)]
    pub captured_at: Option<String>,
    /// Truthy when the survey could not reach the host.
    #[serde(default)]
    pub unreachable: Value,
    /// Boot-critical facts of `/run/current-system` (object of the quad).
    #[serde(default)]
    pub current_boot: Value,
    /// Boot-critical facts of `/run/booted-system` (object of the quad).
    #[serde(default)]
    pub booted_boot: Value,
}

/// Python truthiness for a JSON value: `null`/absent, `false`, `0`, `""`, and
/// empty containers are falsy; everything else is truthy.
fn json_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Per-host state JSON written by the state playbook, keyed by file stem (the
/// inventory hostname the survey wrote it under). Globs `*.json` (dot-files are
/// excluded, matching Python `pathlib.glob`), sorted; unparseable files are
/// skipped.
#[must_use]
pub fn read_snapshots(directory: &Path) -> BTreeMap<String, Snapshot> {
    let mut snapshots: BTreeMap<String, Snapshot> = BTreeMap::new();
    let read_dir = match std::fs::read_dir(directory) {
        Ok(rd) => rd,
        Err(_) => return snapshots,
    };
    let mut paths: Vec<PathBuf> = read_dir
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            // `glob("*.json")` matches a `.json` extension but never a
            // dot-file (`.expected.json` is the cache, not a snapshot).
            let is_json = p.extension().and_then(|e| e.to_str()) == Some("json");
            let hidden = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'));
            is_json && !hidden
        })
        .collect();
    paths.sort();
    for path in paths {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(snap) = serde_json::from_str::<Snapshot>(&text) {
            snapshots.insert(stem.to_string(), snap);
        }
    }
    snapshots
}

/// Whether a member name is safe to interpolate into the eval expression
/// (`^[A-Za-z0-9._-]+$` fullmatch: non-empty, ASCII alphanumeric, `.`/`_`/`-`).
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

/// Locally evaluated toplevel out-paths for the given members, routed through
/// the shared [`Evaluator`]. Host names are validated before they enter the
/// Nix expression: the aggregate is a versioned trust boundary, and a name
/// containing `''` would otherwise escape the indented string.
///
/// # Errors
/// [`DriftError::InvalidMemberName`] if any name fails the guard (before any
/// eval is attempted); [`DriftError::Eval`] if evaluation fails.
pub fn eval_expected(
    evaluator: &mut Evaluator,
    flake: &str,
    hosts: &[String],
) -> Result<BTreeMap<String, String>, DriftError> {
    for name in hosts {
        if !valid_name(name) {
            return Err(DriftError::InvalidMemberName(name.clone()));
        }
    }
    evaluator
        .expected_toplevels(flake, hosts)
        .map_err(DriftError::Eval)
}

/// The contract's git rev, `-dirty`-suffixed when the tree is. Cheap. `None`
/// on any git failure (mirrors the Python `except (OSError, CalledProcessError)`).
#[must_use]
pub fn repo_rev(flake: &str) -> Option<String> {
    let rev = git_output(flake, &["rev-parse", "HEAD"])?;
    let dirty = git_output(flake, &["status", "--porcelain"])?;
    let rev = rev.trim();
    if dirty.trim().is_empty() {
        Some(rev.to_string())
    } else {
        Some(format!("{rev}-dirty"))
    }
}

/// Run `git -C <flake> <args>`; `None` if the process fails to spawn, exits
/// non-zero, or emits non-UTF-8.
fn git_output(flake: &str, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(flake)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// Abbreviate a rev for display, keeping the `-dirty` suffix — losing it makes
/// "cache @ X, repo @ X" read as a contradiction. `None` renders as `?`.
#[must_use]
pub fn short_rev(rev: Option<&str>) -> String {
    match rev {
        None => "?".to_string(),
        Some(r) => {
            if let Some(base) = r.strip_suffix("-dirty") {
                let head: String = base.chars().take(11).collect();
                format!("{head}-dirty")
            } else {
                r.chars().take(11).collect()
            }
        }
    }
}

/// A cached expectation is reusable only for the SAME CLEAN rev — dirty trees
/// have unknowable content and never match.
#[must_use]
pub fn cache_fresh(cached_rev: Option<&str>, current_rev: Option<&str>) -> bool {
    match (cached_rev, current_rev) {
        (Some(cached), Some(current)) => cached == current && !current.ends_with("-dirty"),
        _ => false,
    }
}

/// `(rev the cache was evaluated at, host -> toplevel out-path)`. A missing or
/// unparseable cache reads as `(None, {})`.
#[must_use]
pub fn load_expected(directory: &Path) -> (Option<String>, BTreeMap<String, String>) {
    let Ok(text) = std::fs::read_to_string(directory.join(EXPECTED_CACHE)) else {
        return (None, BTreeMap::new());
    };
    let Ok(data) = serde_json::from_str::<Value>(&text) else {
        return (None, BTreeMap::new());
    };
    let rev = data.get("rev").and_then(Value::as_str).map(str::to_string);
    let toplevels = data
        .get("toplevels")
        .and_then(Value::as_object)
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    (rev, toplevels)
}

/// Persist the rev-keyed expected cache.
///
/// **Byte-format contract (fleet-state-formats):** the Python porcelain writes
/// this via `json.dumps(..., indent=1, sort_keys=True)` and reads it back, so
/// this writer reproduces that exactly — a one-space [`serde_json::ser::
/// PrettyFormatter`] over sorted `BTreeMap`s, no trailing newline. serde_json's
/// default pretty is two-space; matching the Python bytes is load-bearing for
/// cross-implementation interop.
///
/// # Errors
/// Any filesystem error creating the directory or writing the file.
pub fn save_expected(
    rev: Option<&str>,
    toplevels: &BTreeMap<String, String>,
    directory: &Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(directory)?;
    // Build over sorted maps so serialization is key-sorted regardless of
    // serde_json's `preserve_order` feature state.
    let mut root: BTreeMap<String, Value> = BTreeMap::new();
    root.insert(
        "rev".to_string(),
        rev.map_or(Value::Null, |r| Value::String(r.to_string())),
    );
    let tl: serde_json::Map<String, Value> = toplevels
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    root.insert("toplevels".to_string(), Value::Object(tl));

    let bytes = to_pretty_1space(&root)?;
    std::fs::write(directory.join(EXPECTED_CACHE), bytes)
}

/// Serialize with a one-space pretty formatter (Python `indent=1`). Shared
/// with [`crate::registry`] (`meta.json`) and mandala-context's discovery
/// writer, so every state-dir JSON file carries the same `json.dumps(
/// indent=1, sort_keys=True)` bytes — see the module docstring's byte-format
/// contract. Pass sorted maps (`BTreeMap`): the formatter fixes indentation,
/// the map type fixes key order.
///
/// # Errors
/// `serde_json` serialization failures (unrepresentable values).
pub fn to_pretty_1space<T: Serialize>(value: &T) -> serde_json::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    value.serialize(&mut ser)?;
    Ok(buf)
}

/// Whether a snapshot is too old to support a judgement. `None` `max_age` or an
/// empty/absent timestamp disables the check; an unparseable timestamp is
/// conservatively too old; a naive timestamp is assumed UTC (the playbook
/// writes UTC).
fn too_old(captured_at: Option<&str>, max_age: Option<TimeDelta>, now: DateTime<Utc>) -> bool {
    let (max_age, captured_at) = match (max_age, captured_at) {
        (Some(ma), Some(ca)) if !ca.is_empty() => (ma, ca),
        _ => return false,
    };
    let Some(when) = parse_iso(captured_at) else {
        return true; // unparseable timestamp can't support a judgement
    };
    now.signed_duration_since(when) > max_age
}

/// Parse an ISO-8601 timestamp (offset-aware or naive) to UTC. Naive timestamps
/// are assumed UTC. Returns `None` if neither form parses.
fn parse_iso(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(naive) = s.parse::<NaiveDateTime>() {
        return Some(DateTime::from_naive_utc_and_offset(naive, Utc));
    }
    None
}

/// The boot-critical subset of a toplevel (see the module docstring): compared
/// between booted and current to decide reboot-pending vs activated.
const BOOT_CRITICAL: [&str; 4] = ["kernel", "kernel_modules", "initrd", "kernel_params"];

/// Whether booted -> current crosses a boot-critical change.
///
/// Conservative: a snapshot without boot facts (written by a pre-upgrade
/// survey) or with a fact missing/empty on either side judges as changed — an
/// unproven reboot-safety claim must not soften reboot-pending. If either side
/// is not an object, it judges as changed.
fn boot_critical_changed(snap: &Snapshot) -> bool {
    let (Some(current), Some(booted)) =
        (snap.current_boot.as_object(), snap.booted_boot.as_object())
    else {
        return true;
    };
    for key in BOOT_CRITICAL {
        let (a, b) = (current.get(key), booted.get(key));
        // Missing or empty on either side is a change (conservative).
        if !a.is_some_and(json_truthy) || !b.is_some_and(json_truthy) {
            return true;
        }
        let (a, b) = (a.unwrap(), b.unwrap());
        if key == "kernel_params" {
            // The cmdline is compared as a token sequence — the survey's echo
            // wrapper may introduce surrounding whitespace.
            match (a.as_str(), b.as_str()) {
                (Some(a), Some(b)) => {
                    if normalize_tokens(a) != normalize_tokens(b) {
                        return true;
                    }
                }
                _ => {
                    if a != b {
                        return true;
                    }
                }
            }
        } else if a != b {
            return true;
        }
    }
    false
}

/// Whitespace-normalize a token sequence: `" ".join(s.split())`.
fn normalize_tokens(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Drift table over the deploy-rs members, in sorted host order.
///
/// The status decision tree per host: no snapshot → [`DriftStatus::NoSnapshot`];
/// `unreachable` → [`DriftStatus::Unreachable`]; no `current` link →
/// [`DriftStatus::Incomplete`]; too old → [`DriftStatus::Stale`]; `expected`
/// known and `current != expected` → [`DriftStatus::Drift`]; a booted/current
/// split → [`DriftStatus::RebootPending`] if a boot-critical fact moved else
/// [`DriftStatus::Activated`]; otherwise [`DriftStatus::InSync`].
///
/// `max_age` is `Some(default_max_age())` in normal use; `None` disables the
/// staleness check. `expected` is the loaded/evaluated cache (`None` == empty).
#[must_use]
pub fn compare(
    deploy_nodes: &[String],
    snapshots: &BTreeMap<String, Snapshot>,
    expected: Option<&BTreeMap<String, String>>,
    max_age: Option<TimeDelta>,
    now: DateTime<Utc>,
) -> Vec<DriftEntry> {
    let empty = BTreeMap::new();
    let expected = expected.unwrap_or(&empty);
    let mut nodes = deploy_nodes.to_vec();
    nodes.sort();

    let mut entries = Vec::with_capacity(nodes.len());
    for host in nodes {
        let Some(snap) = snapshots.get(&host) else {
            entries.push(DriftEntry {
                host,
                status: DriftStatus::NoSnapshot,
                expected: None,
                current: None,
                booted: None,
                captured_at: None,
            });
            continue;
        };
        let current = snap.current.clone();
        let booted = snap.booted.clone();
        let captured_at = snap.captured_at.clone();
        let exp = expected.get(&host).cloned();

        let status = if json_truthy(&snap.unreachable) {
            DriftStatus::Unreachable
        } else if current.is_none() {
            // Reached the host but got no system links — distinct from
            // "never surveyed" (a broken fact-gather).
            DriftStatus::Incomplete
        } else if too_old(captured_at.as_deref(), max_age, now) {
            DriftStatus::Stale
        } else if exp.is_some() && current != exp {
            DriftStatus::Drift
        } else if booted.as_ref().is_some_and(|b| !b.is_empty()) && booted != current {
            if boot_critical_changed(snap) {
                DriftStatus::RebootPending
            } else {
                DriftStatus::Activated
            }
        } else {
            DriftStatus::InSync
        };

        entries.push(DriftEntry {
            host,
            status,
            expected: exp,
            current,
            booted,
            captured_at,
        });
    }
    entries
}

/// Run the read-only state playbook (fact-gather; mutates nothing) with
/// `MANDALA_FLEET_STATE` pointed at the snapshot directory. Returns the
/// playbook's exit code (`-1` if it was killed by a signal).
///
/// # Errors
/// Any error spawning `ansible-playbook`.
pub fn refresh_snapshots(
    ansible_dir: &Path,
    directory: Option<&Path>,
    limit: Option<&str>,
) -> std::io::Result<i32> {
    let state = directory.map_or_else(state_dir, PathBuf::from);
    let mut cmd = Command::new("ansible-playbook");
    cmd.arg("mandala.fleet.state");
    if let Some(l) = limit {
        cmd.arg("-l").arg(l);
    }
    cmd.current_dir(ansible_dir);
    cmd.env("MANDALA_FLEET_STATE", &state);
    let status = cmd.status()?;
    Ok(status.code().unwrap_or(-1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `2026-06-12T12:00:00+00:00` — the Python suite's fixed NOW.
    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-12T12:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// Write one snapshot file (port of the Python `_snap` helper): defaults
    /// with `host`/`unreachable`/`current`/`booted`/`captured_at`, overrides
    /// merged on top.
    fn write_snap(dir: &Path, stem: &str, overrides: Value) {
        let mut body = json!({
            "host": stem,
            "unreachable": false,
            "current": "/nix/store/aaa-x",
            "booted": "/nix/store/aaa-x",
            "captured_at": now().to_rfc3339(),
        });
        let obj = body.as_object_mut().unwrap();
        for (k, v) in overrides.as_object().unwrap() {
            obj.insert(k.clone(), v.clone());
        }
        std::fs::write(
            dir.join(format!("{stem}.json")),
            serde_json::to_string(&body).unwrap(),
        )
        .unwrap();
    }

    fn boot(overrides: Value) -> Value {
        let mut facts = json!({
            "kernel": "/nix/store/k1",
            "kernel_modules": "/nix/store/m1",
            "initrd": "/nix/store/i1",
            "kernel_params": "root=x loglevel=4",
        });
        let obj = facts.as_object_mut().unwrap();
        for (k, v) in overrides.as_object().unwrap() {
            obj.insert(k.clone(), v.clone());
        }
        facts
    }

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mandala-drift-test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn expect(hosts: &[&str]) -> BTreeMap<String, String> {
        hosts
            .iter()
            .map(|h| ((*h).to_string(), "/nix/store/aaa-x".to_string()))
            .collect()
    }

    // ---- test_expected_cache_roundtrip_and_freshness ------------------------

    #[test]
    fn expected_cache_roundtrip_and_freshness() {
        let dir = tmp();
        let mut tl = BTreeMap::new();
        tl.insert("web".to_string(), "/nix/store/aaa-x".to_string());
        save_expected(Some("rev1"), &tl, &dir).unwrap();
        let (rev, toplevels) = load_expected(&dir);
        assert_eq!(rev.as_deref(), Some("rev1"));
        assert_eq!(toplevels, tl);

        assert!(cache_fresh(Some("rev1"), Some("rev1")));
        assert!(!cache_fresh(Some("rev1"), Some("rev2"))); // contract moved
        assert!(!cache_fresh(Some("rev1-dirty"), Some("rev1-dirty"))); // dirty never matches
        assert!(!cache_fresh(None, Some("rev1")));
        assert!(!cache_fresh(Some("rev1"), None));
    }

    /// The `.expected.json` byte-format is a cross-implementation contract:
    /// assert this writer's bytes match Python `json.dumps(indent=1,
    /// sort_keys=True)` exactly (one-space indent, sorted keys, no trailing
    /// newline). Ground truth captured from CPython.
    #[test]
    fn expected_cache_byte_format_matches_python() {
        let dir = tmp();
        let mut tl = BTreeMap::new();
        tl.insert("web".to_string(), "/nix/store/aaa-x".to_string());
        tl.insert("app".to_string(), "/nix/store/bbb-y".to_string());
        save_expected(Some("rev1"), &tl, &dir).unwrap();
        let bytes = std::fs::read_to_string(dir.join(EXPECTED_CACHE)).unwrap();
        // python3 -c 'import json; print(json.dumps({"rev":"rev1",
        //   "toplevels":{"web":"/nix/store/aaa-x","app":"/nix/store/bbb-y"}},
        //   indent=1, sort_keys=True))'
        let expected = "{\n \"rev\": \"rev1\",\n \"toplevels\": {\n  \"app\": \"/nix/store/bbb-y\",\n  \"web\": \"/nix/store/aaa-x\"\n }\n}";
        assert_eq!(bytes, expected);

        // A None rev renders as JSON null, empty toplevels as `{}`.
        let dir2 = tmp();
        save_expected(None, &BTreeMap::new(), &dir2).unwrap();
        let none_bytes = std::fs::read_to_string(dir2.join(EXPECTED_CACHE)).unwrap();
        let none_expected = "{\n \"rev\": null,\n \"toplevels\": {}\n}";
        assert_eq!(none_bytes, none_expected);
    }

    /// The reverse direction: a Python-written file (embedded bytes) is read
    /// back by [`load_expected`] — proves cross-implementation read parity.
    #[test]
    fn load_expected_reads_python_bytes() {
        let dir = tmp();
        let python_bytes =
            "{\n \"rev\": \"abc123\",\n \"toplevels\": {\n  \"web\": \"/nix/store/zzz\"\n }\n}";
        std::fs::write(dir.join(EXPECTED_CACHE), python_bytes).unwrap();
        let (rev, toplevels) = load_expected(&dir);
        assert_eq!(rev.as_deref(), Some("abc123"));
        assert_eq!(
            toplevels.get("web").map(String::as_str),
            Some("/nix/store/zzz")
        );

        // Missing cache reads as (None, {}).
        let empty_dir = tmp();
        let (rev, toplevels) = load_expected(&empty_dir);
        assert_eq!(rev, None);
        assert!(toplevels.is_empty());
    }

    // ---- test_snapshots_keyed_by_filename_not_embedded_host -----------------

    #[test]
    fn snapshots_keyed_by_filename_not_embedded_host() {
        // A file claiming to be another host must not impersonate it.
        let dir = tmp();
        write_snap(
            &dir,
            "evil",
            json!({"host": "web", "current": "/nix/store/fake-x"}),
        );
        let snapshots = read_snapshots(&dir);
        assert!(!snapshots.contains_key("web"));
        assert_eq!(
            snapshots["evil"].current.as_deref(),
            Some("/nix/store/fake-x")
        );
    }

    #[test]
    fn read_snapshots_skips_the_dotfile_cache_and_unparseable() {
        let dir = tmp();
        write_snap(&dir, "good", json!({}));
        // The `.expected.json` cache is a dot-file: glob("*.json") skips it.
        save_expected(Some("rev1"), &BTreeMap::new(), &dir).unwrap();
        // An unparseable snapshot is skipped, not fatal.
        std::fs::write(dir.join("junk.json"), "{not json").unwrap();
        let snapshots = read_snapshots(&dir);
        assert_eq!(snapshots.keys().collect::<Vec<_>>(), vec!["good"]);
    }

    // ---- test_stale_and_incomplete_are_distinct_judgements ------------------

    #[test]
    fn stale_and_incomplete_are_distinct_judgements() {
        let dir = tmp();
        write_snap(
            &dir,
            "old",
            json!({"captured_at": (now() - TimeDelta::days(3)).to_rfc3339()}),
        );
        write_snap(&dir, "broken", json!({"current": null, "booted": null}));
        write_snap(&dir, "fresh", json!({}));
        let entries: BTreeMap<String, DriftEntry> = compare(
            &["old", "broken", "fresh", "never"].map(String::from),
            &read_snapshots(&dir),
            Some(&expect(&["old", "broken", "fresh"])),
            Some(default_max_age()),
            now(),
        )
        .into_iter()
        .map(|e| (e.host.clone(), e))
        .collect();
        assert_eq!(entries["old"].status, DriftStatus::Stale);
        assert_eq!(entries["broken"].status, DriftStatus::Incomplete);
        assert_eq!(entries["never"].status, DriftStatus::NoSnapshot);
        assert_eq!(entries["fresh"].status, DriftStatus::InSync);
    }

    // ---- test_drift_and_reboot_pending --------------------------------------

    #[test]
    fn drift_and_reboot_pending() {
        let dir = tmp();
        write_snap(
            &dir,
            "moved",
            json!({"current": "/nix/store/bbb-x", "booted": "/nix/store/bbb-x"}),
        );
        write_snap(
            &dir,
            "pending",
            json!({"current": "/nix/store/aaa-x", "booted": "/nix/store/zzz-old"}),
        );
        let entries: BTreeMap<String, DriftEntry> = compare(
            &["moved", "pending"].map(String::from),
            &read_snapshots(&dir),
            Some(&expect(&["moved", "pending"])),
            Some(default_max_age()),
            now(),
        )
        .into_iter()
        .map(|e| (e.host.clone(), e))
        .collect();
        assert_eq!(entries["moved"].status, DriftStatus::Drift);
        assert_eq!(entries["pending"].status, DriftStatus::RebootPending);
    }

    // ---- test_activated_only_when_nothing_boot_critical_moved ---------------

    #[test]
    fn activated_only_when_nothing_boot_critical_moved() {
        let dir = tmp();
        let old = "/nix/store/zzz-old";
        // Identical boot-critical quad: the new generation is fully live.
        write_snap(
            &dir,
            "act",
            json!({"booted": old, "current_boot": boot(json!({})), "booted_boot": boot(json!({}))}),
        );
        // Whitespace-only cmdline delta is the survey's echo wrapper.
        write_snap(
            &dir,
            "ws",
            json!({
                "booted": old,
                "booted_boot": boot(json!({})),
                "current_boot": boot(json!({"kernel_params": " root=x  loglevel=4 "})),
            }),
        );
        // Any of the quad moving keeps the strict judgement.
        write_snap(
            &dir,
            "kern",
            json!({
                "booted": old,
                "booted_boot": boot(json!({})),
                "current_boot": boot(json!({"kernel": "/nix/store/k2"})),
            }),
        );
        write_snap(
            &dir,
            "initrd",
            json!({
                "booted": old,
                "booted_boot": boot(json!({})),
                "current_boot": boot(json!({"initrd": "/nix/store/i2"})),
            }),
        );
        // Pre-boot-facts and half-missing facts stay conservative.
        write_snap(&dir, "legacy", json!({"booted": old}));
        write_snap(
            &dir,
            "partial",
            json!({
                "booted": old,
                "booted_boot": boot(json!({})),
                "current_boot": boot(json!({"kernel": ""})),
            }),
        );
        let hosts = ["act", "ws", "kern", "initrd", "legacy", "partial"];
        let entries: BTreeMap<String, DriftEntry> = compare(
            &hosts.map(String::from),
            &read_snapshots(&dir),
            Some(&expect(&hosts)),
            Some(default_max_age()),
            now(),
        )
        .into_iter()
        .map(|e| (e.host.clone(), e))
        .collect();
        assert_eq!(entries["act"].status, DriftStatus::Activated);
        assert_eq!(entries["ws"].status, DriftStatus::Activated);
        for host in ["kern", "initrd", "legacy", "partial"] {
            assert_eq!(
                entries[host].status,
                DriftStatus::RebootPending,
                "host {host} should be reboot-pending"
            );
        }
    }

    // ---- test_eval_expected_rejects_hostile_names ---------------------------

    #[test]
    fn eval_expected_rejects_hostile_names() {
        // A name with '' would escape the Nix indented string. Reject before
        // any subprocess is spawned (the subprocess backend is never reached).
        let mut ev = Evaluator::new(crate::eval::Backend::Subprocess);
        let err = eval_expected(
            &mut ev,
            ".",
            &["ok-host".to_string(), "bad''(import <nixpkgs>)".to_string()],
        )
        .unwrap_err();
        assert!(matches!(err, DriftError::InvalidMemberName(_)));
        assert!(err.to_string().contains("invalid member name"));
    }

    // ---- test_short_rev_keeps_dirty_suffix ----------------------------------

    #[test]
    fn short_rev_keeps_dirty_suffix() {
        let long = "a".repeat(40);
        assert_eq!(short_rev(Some(&long)), "a".repeat(11));
        assert_eq!(
            short_rev(Some(&format!("{}-dirty", "a".repeat(40)))),
            format!("{}-dirty", "a".repeat(11))
        );
        assert_eq!(short_rev(None), "?");
    }

    // ---- test_every_status_has_a_style --------------------------------------

    #[test]
    fn every_status_has_a_style() {
        // The CLI table and TUI drift tab index the style directly; the
        // exhaustive `match` in `style()` makes a missing style impossible.
        let expected: BTreeMap<&str, &str> = BTreeMap::from([
            ("in-sync", "green"),
            ("drift", "bold red"),
            ("reboot-pending", "yellow"),
            ("activated", "dim green"),
            ("stale", "dim yellow"),
            ("incomplete", "dim red"),
            ("no-snapshot", "dim"),
            ("unreachable", "magenta"),
        ]);
        for status in DriftStatus::ALL {
            assert_eq!(expected.get(status.as_str()), Some(&status.style()));
        }
        assert_eq!(DriftStatus::ALL.len(), expected.len());
    }

    // ---- test_state_dir_resolved_at_call_time -------------------------------
    //
    // Env-mutating; kept in one serialized test so parallel tests never race on
    // the process environment.
    #[test]
    fn state_dir_resolved_at_call_time() {
        let root = tmp();
        let a = root.join("a");
        let b = root.join("b");
        let xdg = root.join("xdg");
        // SAFETY: single-threaded within this test; no other test reads these
        // vars concurrently (they are unique to drift's state_dir).
        unsafe {
            std::env::set_var("MANDALA_FLEET_STATE", &a);
        }
        assert_eq!(state_dir(), a);
        unsafe {
            std::env::set_var("MANDALA_FLEET_STATE", &b);
        }
        assert_eq!(state_dir(), b); // not frozen at import
        unsafe {
            std::env::remove_var("MANDALA_FLEET_STATE");
            std::env::set_var("XDG_STATE_HOME", &xdg);
        }
        assert_eq!(state_dir(), xdg.join("mandala").join("fleet"));
        unsafe {
            std::env::remove_var("XDG_STATE_HOME");
        }
    }

    // ---- _too_old / parse_iso edge cases (beyond the Python suite) ----------

    #[test]
    fn too_old_handles_naive_and_unparseable() {
        // Naive timestamp (no offset) is assumed UTC.
        assert!(!too_old(
            Some("2026-06-12T11:00:00"),
            Some(default_max_age()),
            now()
        ));
        // 3 days old (naive) is stale.
        assert!(too_old(
            Some("2026-06-09T11:00:00"),
            Some(default_max_age()),
            now()
        ));
        // Unparseable → too old (can't support a judgement).
        assert!(too_old(Some("not-a-date"), Some(default_max_age()), now()));
        // No max_age or empty timestamp disables the check.
        assert!(!too_old(Some("not-a-date"), None, now()));
        assert!(!too_old(Some(""), Some(default_max_age()), now()));
        assert!(!too_old(None, Some(default_max_age()), now()));
    }
}
