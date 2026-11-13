//! Frozen source capture. Archive the complete graph, replace every locked
//! fetcher with its archived content, and evaluate only the resulting snapshot.
//! No host commands run here. The temporary roots survive until binding copies
//! them into the campaign's protected directory.

use super::*;
use crate::engines::deploy;
use crate::inventory::Inventory;
use std::process::Command;

fn output(program: &str, args: &[String]) -> io::Result<Vec<u8>> {
    let mut pinned_args;
    let args = if program == "nix"
        && args
            .first()
            .is_some_and(|arg| matches!(arg.as_str(), "eval" | "build"))
        && !args.iter().any(|arg| arg == "--impure")
    {
        pinned_args = args.to_vec();
        pinned_args.splice(1..1, ["--option".into(), "pure-eval".into(), "true".into()]);
        &pinned_args[..]
    } else {
        args
    };
    #[cfg(test)]
    if let Some(result) = tests::run(program, args) {
        return result;
    }
    let result = mandala_deploy::process::output_std(Command::new(program).args(args))?;
    if !result.status.success() {
        return Err(io::Error::other(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    Ok(result.stdout)
}

fn nix(args: &[&str]) -> io::Result<Value> {
    let bytes = output(
        "nix",
        &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    )?;
    serde_json::from_slice(&bytes).map_err(invalid_json)
}

fn nar_identity(path: &str, revision: Option<String>) -> io::Result<SourceNode> {
    if !store_path(path) {
        return Err(invalid("archive returned an invalid store path"));
    }
    let value = nix(&["path-info", "--json", path])?;
    // Nix releases have emitted both a path-keyed object and a list.
    let info = if let Some(values) = value.as_array() {
        values.first().unwrap_or(&Value::Null)
    } else {
        &value[path]
    };
    let hash = info["narHash"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("source NAR identity unavailable"))?;
    Ok(SourceNode {
        store_path: path.into(),
        content_digest: hash.into(),
        revision,
    })
}

fn root(path: &str, link: &Path) -> io::Result<()> {
    if !store_path(path) {
        return Err(invalid("invalid campaign GC root target"));
    }
    let parent = link
        .parent()
        .ok_or_else(|| invalid("missing GC root parent"))?;
    std::fs::create_dir_all(parent)?;
    if path.ends_with(".drv") {
        // Realising a .drv would BUILD its outputs. Instantiating a known
        // derivation value registers the derivation itself without building.
        let quoted = serde_json::to_string(path).map_err(invalid_json)?;
        let expr = format!(
            "{{ type = \"derivation\"; name = \"campaign-root\"; drvPath = {quoted}; outPath = {quoted}; outputName = \"out\"; }}"
        );
        output(
            "nix-instantiate",
            &[
                "--add-root".into(),
                link.display().to_string(),
                "--indirect".into(),
                "--expr".into(),
                expr,
            ],
        )?;
        return Ok(());
    }
    output(
        "nix-store",
        &[
            "--add-root".into(),
            link.display().to_string(),
            "--indirect".into(),
            "--realise".into(),
            path.into(),
        ],
    )?;
    Ok(())
}

/// Return each lock node's archived path. Follows edges resolve through the
/// root graph; they are not reinterpreted as new inputs or lost by flattening.
fn resolve_input<'a>(
    locks: &'a Value,
    edge: &'a Value,
    seen: &mut BTreeSet<String>,
) -> io::Result<&'a str> {
    if let Some(id) = edge.as_str() {
        return Ok(id);
    }
    let names = edge
        .as_array()
        .ok_or_else(|| invalid("unsupported lock input edge"))?;
    let mut id = locks["root"]
        .as_str()
        .ok_or_else(|| invalid("lock root absent"))?;
    for name in names {
        let name = name
            .as_str()
            .ok_or_else(|| invalid("invalid follows path"))?;
        let key = format!("{id}/{name}");
        if !seen.insert(key.clone()) {
            return Err(invalid("cyclic follows edge"));
        }
        let edge = &locks["nodes"][id]["inputs"][name];
        id = resolve_input(locks, edge, seen)?;
        seen.remove(&key);
    }
    Ok(id)
}

fn archived_nodes(locks: &Value, archive: &Value) -> io::Result<BTreeMap<String, String>> {
    fn visit(
        locks: &Value,
        id: &str,
        archive: &Value,
        found: &mut BTreeMap<String, String>,
    ) -> io::Result<()> {
        let path = archive["path"]
            .as_str()
            .ok_or_else(|| invalid("archive path absent"))?;
        if let Some(prior) = found.insert(id.into(), path.into()) {
            if prior != path {
                return Err(invalid(
                    "one input resolved to inconsistent archived content",
                ));
            }
            return Ok(());
        }
        if let Some(inputs) = locks["nodes"][id]["inputs"].as_object() {
            for (name, edge) in inputs {
                let child = resolve_input(locks, edge, &mut BTreeSet::new())?;
                if archive["inputs"][name].is_null() {
                    // Follows often has no redundant archive subtree. Its
                    // referenced node is captured through the owning edge.
                    if edge.is_array() {
                        continue;
                    }
                    return Err(invalid("archive omitted a locked input"));
                }
                visit(locks, child, &archive["inputs"][name], found)?;
            }
        }
        Ok(())
    }
    let mut found = BTreeMap::new();
    let root = locks["root"]
        .as_str()
        .ok_or_else(|| invalid("lock root absent"))?;
    visit(locks, root, archive, &mut found)?;
    for id in locks["nodes"]
        .as_object()
        .ok_or_else(|| invalid("lock nodes absent"))?
        .keys()
    {
        if !found.contains_key(id) {
            return Err(invalid("locked input has no archived identity"));
        }
    }
    Ok(found)
}

fn input_identities(
    locks: &Value,
    nodes: &BTreeMap<String, SourceNode>,
) -> io::Result<BTreeMap<String, SourceNode>> {
    fn walk(
        id: &str,
        prefix: &str,
        locks: &Value,
        nodes: &BTreeMap<String, SourceNode>,
        seen: &mut BTreeSet<String>,
        out: &mut BTreeMap<String, SourceNode>,
    ) -> io::Result<()> {
        // Lock graph node IDs identify shared content. A recursion edge can
        // refer to an ancestor; record that edge but do not recurse forever.
        if !seen.insert(id.into()) {
            return Ok(());
        }
        if let Some(inputs) = locks["nodes"][id]["inputs"].as_object() {
            for (name, edge) in inputs {
                let full = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                };
                let child = resolve_input(locks, edge, &mut BTreeSet::new())?.to_string();
                out.insert(
                    full.clone(),
                    nodes
                        .get(&child)
                        .ok_or_else(|| invalid("input content absent"))?
                        .clone(),
                );
                walk(&child, &full, locks, nodes, seen, out)?;
            }
        }
        seen.remove(id);
        Ok(())
    }
    let mut out = BTreeMap::new();
    walk(
        locks["root"]
            .as_str()
            .ok_or_else(|| invalid("lock root absent"))?,
        "",
        locks,
        nodes,
        &mut BTreeSet::new(),
        &mut out,
    )?;
    Ok(out)
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(test)]
    let mapped = tests::mapped_source(from);
    #[cfg(test)]
    let from = mapped.as_deref().unwrap_or(from);
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let destination = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &destination)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), destination)?;
        } else if kind.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(entry.path())?, destination)?;
            #[cfg(not(unix))]
            return Err(invalid("campaign source symlinks require Unix"));
        } else {
            return Err(invalid("unsupported source entry"));
        }
    }
    Ok(())
}

fn archive(flake: &str) -> io::Result<(Value, Value)> {
    let metadata = nix(&[
        "flake",
        "metadata",
        "--json",
        "--refresh",
        "--no-write-lock-file",
        flake,
    ])?;
    let archived = nix(&[
        "flake",
        "archive",
        "--json",
        "--refresh",
        "--no-write-lock-file",
        flake,
    ])?;
    if metadata["path"] != archived["path"] {
        return Err(invalid(
            "source changed during capture; create a new campaign",
        ));
    }
    Ok((metadata, archived))
}

fn local_checkout(reference: &str) -> Option<PathBuf> {
    let raw = reference.strip_prefix("git+").unwrap_or(reference);
    let raw = raw
        .strip_prefix("file://")
        .or_else(|| raw.strip_prefix("path:"))
        .unwrap_or(raw);
    let raw = raw.split('?').next()?;
    Path::new(raw).canonicalize().ok()
}

fn capture_submodules(
    checkout: &Path,
    archived: &Path,
    prefix: &str,
    roots: &Path,
    out: &mut BTreeMap<String, SubmoduleIdentity>,
) -> io::Result<()> {
    let git = |directory: &Path, args: &[&str]| -> io::Result<Vec<u8>> {
        let mut argv = vec!["-C".into(), directory.display().to_string()];
        argv.extend(args.iter().map(|s| s.to_string()));
        output("git", &argv)
    };
    // Non-Git path inputs have no gitlinks. A .git file is also supported
    // (worktrees and initialized nested submodules).
    if !checkout.join(".git").exists() {
        return Ok(());
    }
    let entries = git(checkout, &["ls-files", "--stage", "-z"])?;
    for entry in entries
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let entry = std::str::from_utf8(entry).map_err(|error| invalid(error.to_string()))?;
        let (header, relative) = entry
            .split_once('\t')
            .ok_or_else(|| invalid("invalid git index entry"))?;
        let fields: Vec<_> = header.split_whitespace().collect();
        if fields.first().copied() != Some("160000") {
            continue;
        }
        if fields.len() != 3
            || fields[2] != "0"
            || Path::new(relative)
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(invalid("unresolved or unsafe source submodule"));
        }
        let path = checkout.join(relative);
        let archived_module = archived.join(relative);
        if !archived_module.is_dir() {
            return Err(invalid(
                "source archive omitted a submodule; enable submodule capture in the source reference",
            ));
        }
        let revision = String::from_utf8(git(&path, &["rev-parse", "HEAD"])?)
            .map_err(|error| invalid(error.to_string()))?
            .trim()
            .to_string();
        let dirty = !git(&path, &["status", "--porcelain", "--untracked-files=all"])?.is_empty();
        // This controlled fetch captures only an explicitly named local Git
        // tree. It is not an impure evaluation of the fleet's outputs.
        let url = format!("file://{}", path.display());
        let expr = format!(
            "builtins.fetchTree {{ type = \"git\"; url = {}; submodules = true; }}",
            serde_json::to_string(&url).map_err(invalid_json)?
        );
        let tree = nix(&["eval", "--impure", "--json", "--expr", &expr])?;
        let store = tree["outPath"]
            .as_str()
            .ok_or_else(|| invalid("submodule source identity unavailable"))?;
        let name = if prefix.is_empty() {
            relative.into()
        } else {
            format!("{prefix}/{relative}")
        };
        root(store, &roots.join(format!("submodule-{}", out.len())))?;
        let archived_hash = String::from_utf8(output(
            "nix",
            &[
                "hash".into(),
                "path".into(),
                "--type".into(),
                "sha256".into(),
                "--sri".into(),
                archived_module.display().to_string(),
            ],
        )?)
        .map_err(|error| invalid(error.to_string()))?;
        let content = nar_identity(store, Some(revision.clone()))?;
        if archived_hash.trim() != content.content_digest {
            return Err(invalid(
                "submodule checkout differs from the captured source; source changed or capture incomplete",
            ));
        }
        out.insert(
            name.clone(),
            SubmoduleIdentity {
                gitlink_revision: fields[1].into(),
                checkout_revision: revision,
                dirty,
                content,
            },
        );
        capture_submodules(&path, &archived_module, &name, roots, out)?;
    }
    Ok(())
}

fn submodule_identities(
    flake: &str,
    locks: &Value,
    paths: &BTreeMap<String, String>,
    roots: &Path,
) -> io::Result<BTreeMap<String, SubmoduleIdentity>> {
    let root_id = locks["root"]
        .as_str()
        .ok_or_else(|| invalid("lock root absent"))?;
    let local = local_checkout(flake);
    let mut submodules = BTreeMap::new();
    if let Some(checkout) = &local {
        capture_submodules(
            checkout,
            Path::new(&paths[root_id]),
            "",
            roots,
            &mut submodules,
        )?;
    }
    for (id, node) in locks["nodes"]
        .as_object()
        .ok_or_else(|| invalid("lock nodes absent"))?
    {
        if id == root_id {
            continue;
        }
        if let Some(path) = node["original"]["path"]
            .as_str()
            .or_else(|| node["locked"]["path"].as_str())
        {
            let path = if Path::new(path).is_absolute() {
                PathBuf::from(path)
            } else {
                local
                    .as_ref()
                    .map_or_else(|| PathBuf::from(path), |root| root.join(path))
            };
            capture_submodules(
                &path,
                Path::new(&paths[id]),
                &format!("input:{id}"),
                roots,
                &mut submodules,
            )?;
        }
    }
    Ok(submodules)
}

struct TempRoots(PathBuf);
impl Drop for TempRoots {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct FrozenSource {
    source: SourceIdentity,
    roots: TempRoots,
    metadata: Value,
    archived: Value,
}
fn capture_state_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = tests::state_root() {
        return path;
    }
    drift::state_dir()
}

fn freeze_source(flake: &str) -> io::Result<FrozenSource> {
    // Refuse credentials rather than accidentally persisting them in
    // provenance or diagnostics. Authentication belongs in fetcher config.
    if flake.contains("token=")
        || flake.contains("password=")
        || flake.split_once("://").is_some_and(|(_, rest)| {
            rest.split('/').next().is_some_and(|authority| {
                authority
                    .split_once('@')
                    .is_some_and(|(userinfo, _)| userinfo.contains(':'))
            })
        })
    {
        return Err(invalid(
            "campaign source references must not contain credentials",
        ));
    }
    let (metadata, archived) = archive(flake)?;
    let temporary = TempRoots(capture_state_dir().join("campaign-captures").join(format!(
        "{}-{}",
        Utc::now().format("%Y%m%dT%H%M%S_%9f"),
        std::process::id()
    )));
    std::fs::create_dir_all(&temporary.0)?;
    let locks = &metadata["locks"];
    let paths = archived_nodes(locks, &archived)?;
    let root_id = locks["root"]
        .as_str()
        .ok_or_else(|| invalid("lock root absent"))?;
    let mut nodes = BTreeMap::new();
    for (index, (id, path)) in paths.iter().enumerate() {
        root(
            path,
            &temporary.0.join("roots").join(format!("source-{index}")),
        )?;
        let revision = metadata["locks"]["nodes"][id]["locked"]["rev"]
            .as_str()
            .map(str::to_string);
        nodes.insert(id.clone(), nar_identity(path, revision)?);
    }
    let mut frozen_lock = locks.clone();
    for (id, node) in &nodes {
        if id != root_id {
            frozen_lock["nodes"][id]["locked"] = serde_json::json!({"type":"path", "path":node.store_path, "narHash":node.content_digest});
        }
    }
    let tree = temporary.0.join("source");
    copy_tree(Path::new(&paths[root_id]), &tree)?;
    let lock_file = tree.join("flake.lock");
    if lock_file.exists() {
        std::fs::remove_file(&lock_file)?;
    }
    std::fs::write(lock_file, canonical(&frozen_lock)?)?;
    let frozen_path = String::from_utf8(output(
        "nix",
        &[
            "store".into(),
            "add-path".into(),
            "--name".into(),
            "campaign-source".into(),
            tree.display().to_string(),
        ],
    )?)
    .map_err(|error| invalid(error.to_string()))?
    .trim()
    .to_string();
    root(&frozen_path, &temporary.0.join("roots/frozen"))?;
    let revision = metadata["revision"].as_str().map(str::to_string);
    let local = local_checkout(flake);
    let submodules = submodule_identities(flake, locks, &paths, &temporary.0.join("roots"))?;
    let source = SourceIdentity {
        checkout: if flake.contains('?') || flake.starts_with("git+") || flake.starts_with("path:")
        {
            flake.to_string()
        } else {
            local.map_or_else(|| flake.to_string(), |path| path.display().to_string())
        },
        revision: revision.clone(),
        dirty: metadata.get("dirtyRevision").is_some(),
        snapshot: nar_identity(&frozen_path, revision)?,
        lock_digest: digest(&canonical(locks)?),
        inputs: input_identities(locks, &nodes)?,
        submodules,
    };

    let (after_metadata, after_archive) = archive(flake)?;
    if archived != after_archive
        || metadata["locks"] != after_metadata["locks"]
        || source.submodules
            != submodule_identities(flake, locks, &paths, &temporary.0.join("checked-initial"))?
    {
        return Err(invalid("source changed during capture; no campaign bound"));
    }
    Ok(FrozenSource {
        source,
        roots: temporary,
        metadata,
        archived,
    })
}

/// Compare source content without evaluating inventory, settings or host targets.
pub fn observe(flake: &str) -> io::Result<SourceIdentity> {
    Ok(freeze_source(flake)?.source.clone())
}

pub struct Capture {
    pub target: Target,
    pub plan: deploy::DeployPlan,
    frozen: FrozenSource,
}

impl Capture {
    /// Capture, root and evaluate the entire graph with a fresh subprocess
    /// evaluator. Input fetchers become immutable store-path lock nodes, so a
    /// nested absolute path cannot escape back into a mutable checkout.
    pub fn new(
        flake: &str,
        selector: &str,
        actor: &str,
        throttle: i64,
        boot: bool,
    ) -> io::Result<Self> {
        if actor.trim().is_empty() {
            return Err(invalid("campaign creation requires an actor"));
        }
        let frozen = freeze_source(flake)?;
        let frozen_path = frozen.source.snapshot.store_path.clone();
        let empty_plan = deploy::DeployPlan {
            limit: String::new(),
            targets: Vec::new(),
            skipped: Vec::new(),
            settings: BTreeMap::new(),
            dry_activate: false,
            boot,
            halt_on_build_failure: false,
            publish: Default::default(),
            throttle,
        };
        let mut capture = Self {
            target: Target {
                source: frozen.source.clone(),
                selection: Selection {
                    selector: selector.into(),
                    members: Vec::new(),
                    deploy_members: Vec::new(),
                },
                hosts: BTreeMap::new(),
                initial_exclusions: BTreeMap::new(),
            },
            plan: empty_plan,
            frozen,
        };
        let reference = format!("path:{frozen_path}");
        let aggregate_ref = format!("{reference}#mandala");
        let inv = Inventory::from_value(nix(&[
            "eval",
            "--json",
            "--no-write-lock-file",
            &aggregate_ref,
        ])?)
        .map_err(|error| invalid(error.to_string()))?;
        let (members, operator_exclusions) = bound_selection(&inv, selector)?;
        let mut plan =
            match deploy::plan_run_with_boot(&inv, &members.join(","), throttle, false, boot) {
                Ok(plan) => plan,
                Err(deploy::DeployRunError::NoDeployableMembers(_)) => deploy::DeployPlan {
                    limit: members.join(","),
                    targets: Vec::new(),
                    skipped: members.clone(),
                    settings: BTreeMap::new(),
                    dry_activate: false,
                    boot,
                    halt_on_build_failure: false,
                    publish: Default::default(),
                    throttle,
                },
                Err(error) => return Err(invalid(error.to_string())),
            };
        let mut systems = BTreeMap::new();
        for name in &plan.targets {
            let system_ref = format!(
                "{reference}#nixosConfigurations.{name}.config.system.build.toplevel.outPath"
            );
            let system = nix(&["eval", "--json", "--no-write-lock-file", &system_ref])?;
            systems.insert(
                name.clone(),
                system
                    .as_str()
                    .ok_or_else(|| invalid("unknown system expectation"))?
                    .to_string(),
            );
        }
        let installables: Vec<_> = plan
            .targets
            .iter()
            .map(|name| format!("{reference}#deploy.nodes.{name}.profiles.system.path"))
            .collect();
        let mut args = vec![
            "build".into(),
            "--dry-run".into(),
            "--json".into(),
            "--no-write-lock-file".into(),
        ];
        args.extend(installables);
        let profiles: Vec<Value> = if plan.targets.is_empty() {
            Vec::new()
        } else {
            serde_json::from_slice(&output("nix", &args)?).map_err(invalid_json)?
        };
        if profiles.len() != plan.targets.len() {
            return Err(invalid(
                "campaign profile result count differs from membership",
            ));
        }
        for (index, (name, value)) in plan.targets.iter().zip(&profiles).enumerate() {
            let drv = value["drvPath"]
                .as_str()
                .filter(|s| s.ends_with(".drv"))
                .ok_or_else(|| invalid("unresolved profile derivation"))?;
            let outputs = value["outputs"]
                .as_object()
                .filter(|outputs| outputs.len() == 1)
                .ok_or_else(|| invalid("profile requires one known output"))?;
            let (output_name, profile) = outputs.iter().next().expect("one output");
            let profile = profile
                .as_str()
                .ok_or_else(|| invalid("unknown static profile output"))?;
            let system = systems
                .get(name)
                .ok_or_else(|| invalid("unknown system expectation"))?;
            if profile != system {
                let system_reference = format!(
                    "{reference}#nixosConfigurations.{name}.config.system.build.toplevel.drvPath"
                );
                let system_drv = String::from_utf8(output(
                    "nix",
                    &[
                        "eval".into(),
                        "--raw".into(),
                        "--no-write-lock-file".into(),
                        system_reference,
                    ],
                )?)
                .map_err(|error| invalid(error.to_string()))?;
                let closure = String::from_utf8(output(
                    "nix-store",
                    &["--query".into(), "--requisites".into(), drv.into()],
                )?)
                .map_err(|error| invalid(error.to_string()))?;
                if !closure.lines().any(|path| path == system_drv.trim()) {
                    return Err(invalid(
                        "profile wrapper does not reference the captured system derivation",
                    ));
                }
            }
            root(
                drv,
                &capture
                    .frozen
                    .roots
                    .0
                    .join("roots")
                    .join(format!("derivation-{index}")),
            )?;
            let activation = if boot || plan.settings[name]["activation"].as_str() == Some("boot") {
                ActivationMode::Boot
            } else {
                ActivationMode::Switch
            };
            capture.target.hosts.insert(
                name.clone(),
                HostTarget {
                    profile: profile.into(),
                    system_toplevel: system.clone(),
                    derivation: drv.into(),
                    output: output_name.clone(),
                    settings: plan.settings[name].clone(),
                    activation,
                },
            );
        }
        capture.target.selection = Selection {
            selector: selector.into(),
            members,
            deploy_members: plan.targets.clone(),
        };
        let timestamp = Utc::now();
        for name in &plan.skipped {
            capture.target.initial_exclusions.insert(
                name.clone(),
                Exclusion {
                    reason: "non-deploy-capable".into(),
                    actor: "controller".into(),
                    timestamp,
                    kind: ExclusionKind::NonDeployCapable,
                },
            );
        }
        for (name, reason) in operator_exclusions {
            if capture.target.hosts.contains_key(&name) {
                capture.target.initial_exclusions.insert(
                    name,
                    Exclusion {
                        reason,
                        actor: actor.into(),
                        timestamp,
                        kind: ExclusionKind::Operator,
                    },
                );
            }
        }
        plan.targets
            .retain(|name| !capture.target.initial_exclusions.contains_key(name));
        plan.settings.retain(|name, _| plan.targets.contains(name));
        plan.limit = plan.targets.join(",");
        capture.plan = plan;
        let (after_metadata, after_archive) = archive(flake)?;
        if capture.frozen.archived != after_archive
            || capture.frozen.metadata["locks"] != after_metadata["locks"]
            || capture.target.source.submodules
                != submodule_identities(
                    flake,
                    &capture.frozen.metadata["locks"],
                    &archived_nodes(&capture.frozen.metadata["locks"], &capture.frozen.archived)?,
                    &capture.frozen.roots.0.join("checked-final"),
                )?
        {
            return Err(invalid("source changed during capture; no campaign bound"));
        }
        capture.target.validate()?;
        Ok(capture)
    }

    /// Publish a reviewed target and preserve all temporary source/derivation
    /// roots. Binding alone does not mark any host successful or start a run.
    pub fn bind(&self, store: &Store) -> io::Result<Campaign> {
        store.create_with(self.target.clone(), Utc::now(), |path| {
            let directory = path.join("roots");
            for entry in std::fs::read_dir(self.frozen.roots.0.join("roots"))? {
                let entry = entry?;
                let path = std::fs::read_link(entry.path())?;
                root(
                    path.to_str()
                        .ok_or_else(|| invalid("non-UTF8 source root"))?,
                    &directory.join(entry.file_name()),
                )?;
            }
            File::open(directory)?.sync_all()
        })
    }
}

fn bound_selection(
    inv: &Inventory,
    selector: &str,
) -> io::Result<(Vec<String>, BTreeMap<String, String>)> {
    let terms: Vec<_> = selector
        .split([',', ':'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let included: Vec<_> = terms
        .iter()
        .filter(|s| !s.starts_with('!'))
        .copied()
        .collect();
    let selector_without_exclusions = if included.is_empty() {
        "all".into()
    } else {
        included.join(",")
    };
    let members = inv
        .resolve(&selector_without_exclusions)
        .map_err(|error| invalid(error.to_string()))?;
    let mut exclusions = BTreeMap::new();
    for term in terms.iter().filter(|s| s.starts_with('!')) {
        for name in inv
            .resolve(&term[1..])
            .map_err(|error| invalid(error.to_string()))?
        {
            if members.contains(&name) {
                exclusions.entry(name).or_insert_with(|| term.to_string());
            }
        }
    }
    Ok((members, exclusions))
}

#[cfg(test)]
mod tests;
