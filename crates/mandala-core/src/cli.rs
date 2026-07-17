//! The clap CLI: the root fleet views plus compile-time engine composition.
//!
//! A parity port of `cli/src/mandala_fleet/cli.py`. The root command owns the
//! fleet-generic views that come straight off the inventory core — `members`,
//! `groups`, `resolve`, `drift`, `version`, `mcp` — and every effect engine
//! plugs in as a subcommand. Python discovered engines through the
//! `mandala.engines` entry-point group at runtime; the Rust design replaces
//! that with **compile-time registration** ([`Cli::register`]): an engine is a
//! name + a [`clap::Command`] + a `run(&Inventory, &ArgMatches)` handler, and
//! the public `mandala` binary registers only the fleet-generic built-ins
//! (`deploy`, `ansible`). A downstream operator binary (mandala-bph) links
//! `mandala-core` and registers its own engines on top, sharing the in-process
//! [`Inventory`] — see the `fleet-cli` spec.
//!
//! ## JSON byte-parity
//!
//! The `--json` outputs are a machine contract, so they reproduce the Python
//! `json.dumps(..., indent=2, sort_keys=True)` bytes exactly (two-space indent,
//! sorted keys — [`to_pretty_2space`]). The human tables are display only:
//! reasonable aligned output over the same status vocabulary, no byte-parity
//! claim. Note the indent asymmetry with the state files, which are `indent=1`
//! (see [`crate::drift::to_pretty_1space`]).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use chrono::Utc;
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde::Serialize;
use serde_json::Value;

use crate::VERSION;
use crate::drift;
use crate::eval::Evaluator;
use crate::inventory::Inventory;

/// An engine's dispatch handler: given the resolved [`Inventory`] and the
/// engine subcommand's parsed arguments, run and yield a process exit code.
/// (The global `--flake` value is reachable through the [`ArgMatches`] because
/// the root arg is `global`.)
pub type EngineRun = Box<dyn Fn(&Inventory, &ArgMatches) -> ExitCode>;

/// A registered effect engine: a name, its [`clap::Command`] subcommand tree,
/// and the handler that runs it. Construct with [`Engine::new`]; register with
/// [`Cli::register`].
pub struct Engine {
    name: String,
    command: Command,
    run: EngineRun,
}

impl Engine {
    /// Build an engine from its subcommand [`Command`] and handler. The
    /// registered spelling is the command's own name (single source of truth —
    /// `mandala <name> …` and the dispatch key can never skew).
    pub fn new(
        command: Command,
        run: impl Fn(&Inventory, &ArgMatches) -> ExitCode + 'static,
    ) -> Self {
        let name = command.get_name().to_string();
        Self {
            name,
            command,
            run: Box::new(run),
        }
    }

    /// The engine's subcommand name (its `mandala <name> …` spelling).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// The CLI: the built-in root views plus the registered engines and the stdio
/// MCP launcher. Assembled by the binary, then [`Cli::run`].
#[derive(Default)]
pub struct Cli {
    engines: Vec<Engine>,
    mcp: Option<Box<dyn Fn() -> ExitCode>>,
}

impl Cli {
    /// A CLI with only the built-in root views — no engines, no MCP launcher.
    /// This is the public/standalone shape the `fleet-cli` spec's no-engines
    /// scenario exercises.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an effect engine as a `mandala <name> …` subcommand.
    #[must_use]
    pub fn register(mut self, engine: Engine) -> Self {
        self.engines.push(engine);
        self
    }

    /// Set the launcher the `mcp` subcommand invokes (the bin wires this to the
    /// `mandala-mcp` stdio server — kept as a closure so `mandala-core` never
    /// depends on `mandala-mcp`, which would be a dependency cycle).
    #[must_use]
    pub fn mcp_launcher(mut self, launch: impl Fn() -> ExitCode + 'static) -> Self {
        self.mcp = Some(Box::new(launch));
        self
    }

    /// The assembled root [`Command`]: the global `--flake` option, the built-in
    /// views, and every registered engine subcommand.
    fn root_command(&self) -> Command {
        let mut cmd = Command::new("mandala")
            .about("mandala fleet porcelain — engines plug in at compile time")
            .subcommand_required(true)
            .arg_required_else_help(true)
            .arg(
                Arg::new("flake")
                    .short('f')
                    .long("flake")
                    .global(true)
                    .default_value(".")
                    .help("Flake exposing the mandala aggregate"),
            )
            .subcommand(
                Command::new("members")
                    .about("List the merged member view (NixOS + facts-only members)")
                    .arg(json_flag()),
            )
            .subcommand(
                Command::new("groups")
                    .about("List taxonomy groups and their members (the fan-out spelling)")
                    .arg(json_flag()),
            )
            .subcommand(
                Command::new("resolve")
                    .about("Expand a selector (`@group`, member, comma-list) to member names")
                    .arg(
                        Arg::new("selector")
                            .required(true)
                            .help("Selector: @group, member, or comma/colon list"),
                    ),
            )
            .subcommand(
                Command::new("drift")
                    .about("Deployed-generation drift: contract vs reported fleet state")
                    .arg(
                        Arg::new("eval")
                            .long("eval")
                            .action(ArgAction::SetTrue)
                            .help("Evaluate expected toplevels (one slow nix eval)"),
                    )
                    .arg(
                        Arg::new("refresh")
                            .long("refresh")
                            .action(ArgAction::SetTrue)
                            .help("Run the read-only state survey (mandala.fleet.state) first"),
                    )
                    .arg(json_flag()),
            )
            .subcommand(Command::new("version").about("Print the CLI version"))
            .subcommand(
                Command::new("mcp").about("Run the fleet MCP server over stdio (headless)"),
            );
        for engine in &self.engines {
            cmd = cmd.subcommand(engine.command.clone());
        }
        cmd
    }

    /// Parse the process arguments and dispatch. Returns the process exit code.
    #[must_use]
    pub fn run(self) -> ExitCode {
        self.run_from(std::env::args_os())
    }

    /// Parse `args` (argv, program name first) and dispatch — the testable core
    /// of [`Cli::run`]. clap parse errors (and `--help`/no-args) are printed and
    /// mapped to their conventional exit codes (2 for a usage error, 0 for a
    /// help display) instead of aborting the process, so this is unit-testable.
    #[must_use]
    pub fn run_from<I, T>(self, args: I) -> ExitCode
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let matches = match self.root_command().try_get_matches_from(args) {
            Ok(m) => m,
            Err(err) => {
                let _ = err.print();
                return if err.use_stderr() {
                    ExitCode::from(2)
                } else {
                    ExitCode::SUCCESS
                };
            }
        };
        let flake = matches
            .get_one::<String>("flake")
            .map_or(".", String::as_str)
            .to_string();

        match matches.subcommand() {
            Some(("members", m)) => with_inventory(&flake, |inv| run_members(inv, m)),
            Some(("groups", m)) => with_inventory(&flake, |inv| run_groups(inv, m)),
            Some(("resolve", m)) => with_inventory(&flake, |inv| run_resolve(inv, m)),
            Some(("drift", m)) => with_inventory(&flake, |inv| run_drift(inv, &flake, m)),
            Some(("version", _)) => {
                println!("{VERSION}");
                ExitCode::SUCCESS
            }
            Some(("mcp", _)) => match &self.mcp {
                Some(launch) => launch(),
                None => {
                    eprintln!("mandala: the MCP server is not available in this build");
                    ExitCode::FAILURE
                }
            },
            Some((name, m)) => match self.engines.iter().find(|e| e.name == name) {
                Some(engine) => with_inventory(&flake, |inv| (engine.run)(inv, m)),
                // `subcommand_required` guarantees a matched name is one we
                // registered, so this is unreachable in practice.
                None => {
                    eprintln!("mandala: unknown command {name}");
                    ExitCode::FAILURE
                }
            },
            None => ExitCode::from(2),
        }
    }
}

/// The shared `--json` boolean flag.
fn json_flag() -> Arg {
    Arg::new("json")
        .long("json")
        .action(ArgAction::SetTrue)
        .help("Emit machine-readable JSON")
}

/// Build the inventory for a command that needs it, then run `f`; on an
/// eval/seam failure, print the message and exit non-zero (the Python core
/// raises through Typer instead — same net effect, one error line + rc 1).
fn with_inventory(flake: &str, f: impl FnOnce(&Inventory) -> ExitCode) -> ExitCode {
    match load_inventory(flake) {
        Ok(inv) => f(&inv),
        Err(msg) => {
            eprintln!("mandala: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// Resolve the inventory: honour the `MANDALA_AGGREGATE_FILE` test seam (a path
/// to a JSON aggregate — lets integration tests inject a fixture without a real
/// flake eval), else evaluate `<flake>#mandala` through the [`Evaluator`]
/// (`MANDALA_EVAL` selects worker vs subprocess).
fn load_inventory(flake: &str) -> Result<Inventory, String> {
    if let Ok(path) = std::env::var("MANDALA_AGGREGATE_FILE")
        && !path.is_empty()
    {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading MANDALA_AGGREGATE_FILE {path}: {e}"))?;
        let value: Value =
            serde_json::from_str(&text).map_err(|e| format!("parsing {path}: {e}"))?;
        return Inventory::from_value(value).map_err(|e| e.to_string());
    }
    let mut evaluator = Evaluator::from_env();
    Inventory::from_evaluator(&mut evaluator, flake).map_err(|e| e.to_string())
}

// ---- built-in root views ---------------------------------------------------

fn run_members(inv: &Inventory, m: &ArgMatches) -> ExitCode {
    if m.get_flag("json") {
        // Python: json.dumps(inv.members, indent=2, sort_keys=True).
        println!("{}", to_pretty_2space(inv.members()));
        return ExitCode::SUCCESS;
    }
    let rows: Vec<Vec<String>> = inv
        .members()
        .iter()
        .map(|(name, member)| {
            vec![
                name.clone(),
                field_or(member.get("platform"), "?"),
                field_or(member.get("architecture"), "?"),
                field_or(member.get("category"), "?"),
                role_or_dash(member.get("role")),
                join_tags(member.get("tags")),
                member.surfaces(),
            ]
        })
        .collect();
    print_table(
        &[
            "member", "platform", "arch", "category", "role", "tags", "ads",
        ],
        &rows,
        &format!("{} members — ads = ansible/deploy-rs/sops", rows.len()),
    );
    ExitCode::SUCCESS
}

fn run_groups(inv: &Inventory, m: &ArgMatches) -> ExitCode {
    if m.get_flag("json") {
        println!("{}", to_pretty_2space(inv.groups()));
        return ExitCode::SUCCESS;
    }
    let rows: Vec<Vec<String>> = inv
        .groups()
        .iter()
        .map(|(group, names)| {
            let mut sorted = names.clone();
            sorted.sort();
            vec![group.clone(), sorted.len().to_string(), sorted.join(" ")]
        })
        .collect();
    print_table(
        &["group", "n", "members"],
        &rows,
        &format!(
            "{} groups — one spelling: @group, ansible -l, deployBatch",
            rows.len()
        ),
    );
    ExitCode::SUCCESS
}

fn run_resolve(inv: &Inventory, m: &ArgMatches) -> ExitCode {
    let selector = m.get_one::<String>("selector").map_or("", String::as_str);
    match inv.resolve(selector) {
        Ok(names) => {
            for name in names {
                println!("{name}");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("mandala: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run_drift(inv: &Inventory, flake: &str, m: &ArgMatches) -> ExitCode {
    let do_eval = m.get_flag("eval");
    let refresh = m.get_flag("refresh");
    let as_json = m.get_flag("json");
    let nodes = inv.deploy_nodes();

    if refresh {
        // Prefer the ./ansible working dir when it carries the collection cfg.
        let ansible_dir = if Path::new("ansible/ansible.cfg").is_file() {
            Path::new("ansible")
        } else {
            Path::new(".")
        };
        match drift::refresh_snapshots(ansible_dir, None, None) {
            Ok(rc) if rc != 0 => {
                eprintln!("state survey exited {rc} (continuing with whatever was captured)");
            }
            Ok(_) => {}
            Err(err) => eprintln!("state survey failed to start: {err}"),
        }
    }

    // Expected toplevels: re-evaluated on --eval, else reused from the
    // rev-keyed cache when the contract hasn't moved since the last eval.
    let rev = drift::repo_rev(flake);
    let state = drift::state_dir();
    let (cached_rev, cached) = drift::load_expected(&state);
    let mut expected: Option<BTreeMap<String, String>> = None;
    if do_eval {
        let mut evaluator = Evaluator::from_env();
        match drift::eval_expected(&mut evaluator, flake, &nodes) {
            Ok(exp) => {
                let _ = drift::save_expected(rev.as_deref(), &exp, &state);
                expected = Some(exp);
            }
            Err(err) => {
                eprintln!("expected-toplevel eval failed:");
                for line in err
                    .to_string()
                    .lines()
                    .rev()
                    .take(8)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                {
                    eprintln!("  {line}");
                }
                return ExitCode::FAILURE;
            }
        }
    } else if drift::cache_fresh(cached_rev.as_deref(), rev.as_deref()) {
        expected = Some(cached);
    }

    let snapshots = drift::read_snapshots(&state);
    let entries = drift::compare(
        &nodes,
        &snapshots,
        expected.as_ref(),
        Some(drift::default_max_age()),
        Utc::now(),
    );

    if as_json {
        println!("{}", to_pretty_2space(&entries));
        return ExitCode::SUCCESS;
    }

    let rows: Vec<Vec<String>> = entries
        .iter()
        .map(|e| {
            vec![
                e.host.clone(),
                e.status.as_str().to_string(),
                short_store(e.current.as_deref()),
                short_store(e.expected.as_deref()),
                short_store(e.booted.as_deref()),
                e.captured_at
                    .as_deref()
                    .map_or_else(|| "-".to_string(), |c| c.chars().take(19).collect()),
            ]
        })
        .collect();
    print_table(
        &[
            "member", "status", "current", "expected", "booted", "captured",
        ],
        &rows,
        &drift_caption(
            expected.is_some(),
            do_eval,
            rev.as_deref(),
            cached_rev.as_deref(),
        ),
    );
    ExitCode::SUCCESS
}

/// The drift table caption for the four expected-cache states — a 1:1 port of
/// the Python `cli.py` drift caption branches, factored out so the caption
/// vocabulary is unit-testable without git/nix/ansible.
#[must_use]
pub fn drift_caption(
    expected_known: bool,
    do_eval: bool,
    rev: Option<&str>,
    cached_rev: Option<&str>,
) -> String {
    if expected_known {
        let base = format!("expected @ {}", drift::short_rev(rev));
        if do_eval {
            base
        } else {
            format!("{base} (cached)")
        }
    } else if cached_rev.is_some() {
        format!(
            "expected cache stale: evaluated @ {}, repo now @ {} — the contract moved; pass --eval",
            drift::short_rev(cached_rev),
            drift::short_rev(rev),
        )
    } else {
        "expected not evaluated — pass --eval for real drift judgement".to_string()
    }
}

// ---- JSON + table helpers --------------------------------------------------

/// Serialize with a two-space pretty formatter and sorted keys — byte-identical
/// to Python `json.dumps(value, indent=2, sort_keys=True)`. The value is first
/// lowered to a [`serde_json::Value`], whose objects are `BTreeMap`-backed (no
/// serde_json `preserve_order` in this workspace), so EVERY object — including
/// derived structs like [`crate::drift::DriftEntry`], which otherwise serialize
/// in field-declaration order — emits key-sorted. The 2-space formatter matches
/// serde_json's default pretty, made explicit here for the byte contract.
#[must_use]
pub fn to_pretty_2space<T: Serialize>(value: &T) -> String {
    let value = serde_json::to_value(value).expect("value is serializable to JSON");
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"  ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    value
        .serialize(&mut ser)
        .expect("serializing to a Vec never fails");
    String::from_utf8(buf).expect("serde_json emits valid UTF-8")
}

/// A member field as its string value, or `default` when absent/non-string
/// (Python `m.get(key, "?")` for the `?`-defaulted columns).
fn field_or(value: Option<&Value>, default: &str) -> String {
    value
        .and_then(Value::as_str)
        .map_or_else(|| default.to_string(), str::to_string)
}

/// The `role` column: the role string, or `-` when absent/empty (Python
/// `m.get("role") or "-"`).
fn role_or_dash(value: Option<&Value>) -> String {
    match value.and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => "-".to_string(),
    }
}

/// The `tags` column: the tag strings space-joined (Python `" ".join(m.get(
/// "tags", []))`).
fn join_tags(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

/// Shorten a store path for the drift table: strip `/nix/store/`, keep the
/// first 20 chars; `None` renders as `-` (Python `short`).
fn short_store(path: Option<&str>) -> String {
    let p = path.unwrap_or("-");
    let p = p.strip_prefix("/nix/store/").unwrap_or(p);
    p.chars().take(20).collect()
}

/// Print a left-aligned, space-padded table with a trailing caption line. This
/// is human-facing display only (no byte-parity claim vs Python `rich`).
fn print_table(headers: &[&str], rows: &[Vec<String>], caption: &str) {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }
    }
    let render = |cells: &[String]| -> String {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let pad = widths.get(i).copied().unwrap_or(0);
                let gap = pad.saturating_sub(c.chars().count());
                format!("{c}{}", " ".repeat(gap))
            })
            .collect::<Vec<_>>()
            .join("  ")
    };
    let header_cells: Vec<String> = headers.iter().map(|h| (*h).to_string()).collect();
    println!("{}", render(&header_cells).trim_end());
    for row in rows {
        println!("{}", render(row).trim_end());
    }
    println!("{caption}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A fixture inventory: three members with distinct surface flags, two
    /// groups, deploy + ansible projections — enough to exercise every root
    /// view and both built-in engines without a flake eval.
    fn fixture() -> Inventory {
        Inventory::from_value(json!({
            "schemaVersion": 1,
            "members": {
                "web": {
                    "platform": "nixos", "architecture": "x86_64",
                    "category": "server", "role": "web", "tags": ["edge", "public"],
                    "deployment": {"ansible": {"enable": true}, "deployRs": {"enable": true}, "sops": {"recipient": "age1web"}},
                },
                "cache": {
                    "platform": "nixos", "architecture": "aarch64",
                    "category": "server", "role": "cache", "tags": [],
                    "deployment": {"ansible": {"enable": true}},
                },
                "router": {"platform": "vyos", "architecture": "aarch64", "category": "network"},
            },
            "groups": {"k3s": ["cache", "web"], "gateway": ["router"]},
            "projections": {
                "deploy": {"nodes": ["web", "cache"]},
                "ansibleInventory": {"all": {"hosts": {"web": null, "cache": null}}},
            },
        }))
        .expect("fixture aggregate is valid")
    }

    // ---- members --json byte-parity -----------------------------------------

    #[test]
    fn members_json_is_python_byte_parity() {
        // python3 -c 'import json,sys; ...json.dumps(members, indent=2,
        // sort_keys=True)' over the fixture's members dict.
        let expected = "{\n  \"cache\": {\n    \"architecture\": \"aarch64\",\n    \"category\": \"server\",\n    \"deployment\": {\n      \"ansible\": {\n        \"enable\": true\n      }\n    },\n    \"platform\": \"nixos\",\n    \"role\": \"cache\",\n    \"tags\": []\n  },\n  \"router\": {\n    \"architecture\": \"aarch64\",\n    \"category\": \"network\",\n    \"platform\": \"vyos\"\n  },\n  \"web\": {\n    \"architecture\": \"x86_64\",\n    \"category\": \"server\",\n    \"deployment\": {\n      \"ansible\": {\n        \"enable\": true\n      },\n      \"deployRs\": {\n        \"enable\": true\n      },\n      \"sops\": {\n        \"recipient\": \"age1web\"\n      }\n    },\n    \"platform\": \"nixos\",\n    \"role\": \"web\",\n    \"tags\": [\n      \"edge\",\n      \"public\"\n    ]\n  }\n}";
        assert_eq!(to_pretty_2space(fixture().members()), expected);
    }

    #[test]
    fn groups_json_is_python_byte_parity() {
        // json.dumps({"gateway":["router"],"k3s":["cache","web"]}, indent=2,
        // sort_keys=True)
        let expected = "{\n  \"gateway\": [\n    \"router\"\n  ],\n  \"k3s\": [\n    \"cache\",\n    \"web\"\n  ]\n}";
        assert_eq!(to_pretty_2space(fixture().groups()), expected);
    }

    #[test]
    fn ansible_inventory_json_is_python_byte_parity() {
        // json.dumps({"all":{"hosts":{"cache":None,"web":None}}}, indent=2,
        // sort_keys=True)
        let expected = "{\n  \"all\": {\n    \"hosts\": {\n      \"cache\": null,\n      \"web\": null\n    }\n  }\n}";
        assert_eq!(
            to_pretty_2space(fixture().ansible_inventory().unwrap()),
            expected
        );
    }

    #[test]
    fn drift_json_is_python_byte_parity() {
        // A single no-snapshot entry, serialized as the Python asdict list.
        // json.dumps([{ "booted":None,"captured_at":None,"current":None,
        //   "expected":None,"host":"web","status":"no-snapshot"}], indent=2,
        //   sort_keys=True)
        let entries = drift::compare(
            &["web".to_string()],
            &BTreeMap::new(),
            None,
            Some(drift::default_max_age()),
            Utc::now(),
        );
        let expected = "[\n  {\n    \"booted\": null,\n    \"captured_at\": null,\n    \"current\": null,\n    \"expected\": null,\n    \"host\": \"web\",\n    \"status\": \"no-snapshot\"\n  }\n]";
        assert_eq!(to_pretty_2space(&entries), expected);
    }

    // ---- drift caption cases ------------------------------------------------

    #[test]
    fn drift_caption_covers_the_four_states() {
        // evaluated (--eval): "expected @ <rev>"
        assert_eq!(
            drift_caption(true, true, Some("abcdef1234567890"), None),
            "expected @ abcdef12345"
        );
        // cached (no --eval, cache fresh): "… (cached)"
        assert_eq!(
            drift_caption(
                true,
                false,
                Some("abcdef1234567890"),
                Some("abcdef1234567890")
            ),
            "expected @ abcdef12345 (cached)"
        );
        // stale (no --eval, cache present but rev moved)
        assert_eq!(
            drift_caption(false, false, Some("newnewnew000"), Some("oldoldold111")),
            "expected cache stale: evaluated @ oldoldold11, repo now @ newnewnew00 — the contract moved; pass --eval"
        );
        // not evaluated (no --eval, no cache)
        assert_eq!(
            drift_caption(false, false, None, None),
            "expected not evaluated — pass --eval for real drift judgement"
        );
    }

    // ---- resolve + engine dispatch via run_from -----------------------------

    /// Serializes every seam-using test: `MANDALA_AGGREGATE_FILE` is a
    /// process-global, so two seam tests running concurrently would clobber
    /// each other's var (the registry/drift env-test discipline).
    static SEAM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Point the aggregate seam at a temp file holding the fixture, so
    /// `run_from` builds the fixture inventory without a flake eval. Holds the
    /// [`SEAM_LOCK`] for the test's duration; removes the file and unsets the
    /// env var on drop.
    struct SeamGuard {
        path: std::path::PathBuf,
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    impl Drop for SeamGuard {
        fn drop(&mut self) {
            unsafe { std::env::remove_var("MANDALA_AGGREGATE_FILE") };
            let _ = std::fs::remove_file(&self.path);
        }
    }
    fn seam() -> SeamGuard {
        let lock = SEAM_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = std::env::temp_dir().join(format!(
            "mandala-cli-fixture-{}-{:?}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let agg = serde_json::to_value(fixture().aggregate()).unwrap();
        std::fs::write(&path, serde_json::to_string(&agg).unwrap()).unwrap();
        unsafe { std::env::set_var("MANDALA_AGGREGATE_FILE", &path) };
        SeamGuard { path, _lock: lock }
    }

    #[test]
    fn resolve_and_engine_dispatch_over_the_seam() {
        // Env-mutating: serialized so the seam var never races another test.
        let _g = seam();

        // resolve exits 0 for a known selector.
        let rc = Cli::new().run_from(["mandala", "resolve", "@k3s"]);
        assert_eq!(rc, ExitCode::SUCCESS);

        // engine dispatch: `deploy nodes` and `ansible inventory` reach their
        // handlers with the fixture inventory.
        let rc = Cli::new()
            .register(crate::engines::deploy::engine())
            .register(crate::engines::ansible::engine())
            .run_from(["mandala", "deploy", "nodes"]);
        assert_eq!(rc, ExitCode::SUCCESS);

        let rc = Cli::new()
            .register(crate::engines::ansible::engine())
            .run_from(["mandala", "ansible", "inventory"]);
        assert_eq!(rc, ExitCode::SUCCESS);
    }

    #[test]
    fn version_needs_no_inventory() {
        // No seam set: version must not touch the evaluator.
        assert_eq!(
            Cli::new().run_from(["mandala", "version"]),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn no_engines_standalone_runs_the_builtins() {
        // The fleet-cli spec: the public binary stands alone with zero engines.
        let _g = seam();
        let cli = Cli::new();
        assert_eq!(cli.root_command().get_subcommands().count(), 6); // members/groups/resolve/drift/version/mcp
        assert_eq!(
            Cli::new().run_from(["mandala", "members", "--json"]),
            ExitCode::SUCCESS
        );
        // An unknown engine is a usage error (exit 2), not a crash.
        assert_eq!(
            Cli::new().run_from(["mandala", "flux", "apply"]),
            ExitCode::from(2)
        );
    }
}
