//! Frozen source capture. Archive the complete graph, replace every locked
//! fetcher with its archived content, and evaluate only the resulting snapshot.
//! No host commands run here. The temporary roots survive until binding copies
//! them into the campaign's protected directory.

use super::*;
use crate::engines::deploy;
use crate::inventory::Inventory;
use std::process::Command;
mod relative;

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

fn source_revision(value: &Value) -> Option<String> {
    value["revision"]
        .as_str()
        .or_else(|| value["rev"].as_str())
        .or_else(|| {
            value["dirtyRevision"]
                .as_str()
                .and_then(|value| value.strip_suffix("-dirty"))
        })
        .or_else(|| {
            value["dirtyRev"]
                .as_str()
                .and_then(|value| value.strip_suffix("-dirty"))
        })
        .filter(|value| !value.is_empty())
        .map(str::to_string)
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

fn is_relative(node: &Value) -> bool {
    node["locked"]["type"].as_str() == Some("path")
        && node["locked"]["path"]
            .as_str()
            .is_some_and(|path| !path.starts_with('/'))
}

fn store_root(path: &str) -> io::Result<String> {
    let name = path
        .strip_prefix("/nix/store/")
        .and_then(|path| path.split('/').next())
        .ok_or_else(|| invalid("archive path outside the store"))?;
    let path = format!("/nix/store/{name}");
    if !store_path(&path) {
        return Err(invalid("invalid archived store root"));
    }
    Ok(path)
}

fn archived_nodes(locks: &Value, archive: &Value) -> io::Result<BTreeMap<String, String>> {
    fn visit(
        locks: &Value,
        id: &str,
        archive: &Value,
        found: &mut BTreeMap<String, String>,
    ) -> io::Result<()> {
        let path = if is_relative(&locks["nodes"][id]) {
            String::new()
        } else {
            store_root(
                archive["path"]
                    .as_str()
                    .ok_or_else(|| invalid("archive path absent"))?,
            )?
        };
        if let Some(prior) = found.insert(id.into(), path.clone()) {
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
    found.retain(|id, _| !is_relative(&locks["nodes"][id]));
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
    if store_root(
        metadata["path"]
            .as_str()
            .ok_or_else(|| invalid("source metadata path absent"))?,
    )? != store_root(
        archived["path"]
            .as_str()
            .ok_or_else(|| invalid("source archive path absent"))?,
    )? {
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
            Path::new(&store_root(&paths[root_id])?),
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

#[derive(Debug)]
struct TempRoots(PathBuf);
impl Drop for TempRoots {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn claim_capture_dir(base: &Path) -> io::Result<TempRoots> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    reject_symlink(base)?;
    std::fs::create_dir_all(base)?;
    loop {
        let path = base.join(format!(
            "{}-{}-{}",
            Utc::now().format("%Y%m%dT%H%M%S_%9f"),
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
                }
                return Ok(TempRoots(path));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

#[derive(Debug)]
struct FrozenSource {
    source: SourceIdentity,
    roots: TempRoots,
    metadata: Value,
    archived: Value,
    paths: BTreeMap<String, String>,
}
fn capture_state_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = tests::state_root() {
        return path;
    }
    drift::state_dir()
}

fn safe_reference(reference: &str) -> bool {
    if reference.split_once("://").is_some_and(|(_, rest)| {
        rest.split(['/', '?', '#'])
            .next()
            .is_some_and(|authority| authority.contains('@'))
    }) {
        return false;
    }
    // Keep only known Nix identity/selection query keys in durable provenance.
    // Opaque download/auth queries can be credentials even without 'token'
    // in their name; authentication belongs in the fetcher's configuration.
    reference.split_once('?').is_none_or(|(_, query)| {
        query
            .split('#')
            .next()
            .unwrap_or("")
            .split('&')
            .all(|part| {
                let key = part.split('=').next().unwrap_or("").to_ascii_lowercase();
                matches!(
                    key.as_str(),
                    "rev"
                        | "ref"
                        | "narhash"
                        | "dir"
                        | "shallow"
                        | "submodules"
                        | "allrefs"
                        | "lfs"
                        | "lastmodified"
                        | "revcount"
                )
            })
    })
}

fn supported_nix() -> io::Result<()> {
    let version = String::from_utf8(output("nix", &["--version".into()])?)
        .map_err(|error| invalid(error.to_string()))?;
    if !version
        .split_whitespace()
        .any(|word| word.starts_with("2.34."))
    {
        return Err(invalid(
            "campaign capture requires supported Nix 2.34.x sourceInfo replay; refusing to degrade source semantics",
        ));
    }
    Ok(())
}

fn fetched_info(parameters: &Value, archived: &str, parameter_file: &Path) -> io::Result<Value> {
    std::fs::write(parameter_file, canonical(parameters)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parameter_file, std::fs::Permissions::from_mode(0o600))?;
    }
    let quoted = serde_json::to_string(&parameter_file.display().to_string())
        .map_err(invalid_json)?
        .replace("${", "\\${");
    let expression = format!(
        "let locked = builtins.fromJSON (builtins.readFile {quoted}); tree = builtins.fetchTree (builtins.removeAttrs locked [ \"dir\" \"__final\" ]); in {{ path = toString tree.outPath; metadata = builtins.removeAttrs tree [ \"outPath\" ]; }}"
    );
    let info = nix(&["eval", "--impure", "--json", "--expr", &expression])?;
    let path = info["path"]
        .as_str()
        .ok_or_else(|| invalid("fetched sourceInfo path absent"))?;
    let identity = nar_identity(path, source_revision(&info["metadata"]))?;
    let archived = nar_identity(archived, None)?;
    if !info["metadata"].is_object()
        || info["metadata"]["narHash"].as_str() != Some(identity.content_digest.as_str())
        || identity.content_digest != archived.content_digest
    {
        return Err(invalid(
            "source changed or incomplete fetcher metadata during capture",
        ));
    }
    Ok(info)
}

struct CapturedGraph {
    manifest: Value,
    trees: BTreeMap<String, SourceNode>,
    paths: BTreeMap<String, String>,
}

fn captured_graph(metadata: &Value, archived: &Value, roots: &Path) -> io::Result<CapturedGraph> {
    let locks = &metadata["locks"];
    let root_id = locks["root"]
        .as_str()
        .ok_or_else(|| invalid("lock root absent"))?;
    let archived_paths = archived_nodes(locks, archived)?;
    let entries = locks["nodes"]
        .as_object()
        .ok_or_else(|| invalid("lock nodes absent"))?;
    let mut sources = BTreeMap::new();
    for (index, (id, node)) in entries.iter().enumerate() {
        if is_relative(node) {
            continue;
        }
        let parameters = if id == root_id {
            metadata["locked"].clone()
        } else {
            let mut parameters = node["info"].as_object().cloned().unwrap_or_default();
            parameters.extend(
                node["locked"]
                    .as_object()
                    .ok_or_else(|| invalid("lock node fetcher absent"))?
                    .clone(),
            );
            Value::Object(parameters)
        };
        if !parameters.is_object() {
            return Err(invalid("root fetcher metadata absent"));
        }
        let archived = archived_paths
            .get(id)
            .ok_or_else(|| invalid("archived node identity absent"))?;
        root(archived, &roots.join(format!("archive-{index}")))?;
        let info = fetched_info(
            &parameters,
            archived,
            &roots
                .parent()
                .unwrap()
                .join(format!("fetcher-{index}.json")),
        )?;
        let path = info["path"].as_str().unwrap();
        root(path, &roots.join(format!("source-{index}")))?;
        sources.insert(id.clone(), info);
    }
    // Nix 2.34 suppresses root revision fields when --no-write-lock-file
    // leaves an effective lock update unwritten (flake.forceDirty).
    let root_info = sources
        .get_mut(root_id)
        .ok_or_else(|| invalid("root sourceInfo absent"))?;
    root_info["fetcher_metadata"] = root_info["metadata"].clone();
    let root_dir = metadata["locked"]["dir"].as_str().unwrap_or("");
    let old_lock = Path::new(root_info["path"].as_str().unwrap())
        .join(root_dir)
        .join("flake.lock");
    let force_dirty = match std::fs::read(&old_lock) {
        Ok(bytes) => {
            let mut old: Value = serde_json::from_slice(&bytes).map_err(invalid_json)?;
            let mut new = locks.clone();
            // Encoding-version changes alone do not change the parsed graph.
            old.as_object_mut()
                .ok_or_else(|| invalid("invalid original lock file"))?
                .remove("version");
            new.as_object_mut().unwrap().remove("version");
            old != new
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            entries.len() != 1
                || entries[root_id]["inputs"]
                    .as_object()
                    .is_some_and(|inputs| !inputs.is_empty())
        }
        Err(error) => return Err(error),
    };
    if force_dirty {
        for key in ["rev", "shortRev", "revCount"] {
            root_info["metadata"].as_object_mut().unwrap().remove(key);
        }
    }
    let mut graph_nodes = serde_json::Map::new();
    for (id, node) in entries {
        let inputs = node["inputs"]
            .as_object()
            .map(|inputs| {
                inputs
                    .iter()
                    .map(|(name, edge)| {
                        Ok((
                            name.clone(),
                            Value::from(resolve_input(locks, edge, &mut BTreeSet::new())?),
                        ))
                    })
                    .collect::<io::Result<serde_json::Map<_, _>>>()
            })
            .transpose()?
            .unwrap_or_default();
        let relative = is_relative(node);
        let parent = if relative {
            Some(resolve_input(locks, &node["parent"], &mut BTreeSet::new())?.to_string())
        } else {
            None
        };
        let dir = if id == root_id {
            metadata["locked"]["dir"].as_str().unwrap_or("")
        } else {
            node["locked"]["dir"].as_str().unwrap_or("")
        };
        graph_nodes.insert(
            id.clone(),
            serde_json::json!({
                "inputs":inputs,"flake":node["flake"].as_bool().unwrap_or(true),
                "relative":if relative{node["locked"]["path"].clone()}else{Value::Null},
                "parent":parent,"dir":dir,"source":sources.get(id),"relative_override":null,"relative_dir":"",
            }),
        );
    }
    let mut graph = serde_json::json!({"schema_version":1,"root":root_id,"nodes":graph_nodes});
    fn source<'a>(
        id: &str,
        graph: &'a Value,
        seen: &mut BTreeSet<String>,
    ) -> io::Result<&'a Value> {
        if !seen.insert(id.into()) {
            return Err(invalid("cyclic relative source parent"));
        }
        let node = &graph["nodes"][id];
        if node["relative"].is_null() {
            Ok(&node["source"])
        } else {
            source(
                node["parent"]
                    .as_str()
                    .ok_or_else(|| invalid("relative input parent absent"))?,
                graph,
                seen,
            )
        }
    }
    fn flake_path(id: &str, graph: &Value, seen: &mut BTreeSet<String>) -> io::Result<PathBuf> {
        if !seen.insert(id.into()) {
            return Err(invalid("cyclic relative input path"));
        }
        let node = &graph["nodes"][id];
        let path = if let Some(relative) = node["relative"].as_str() {
            flake_path(node["parent"].as_str().unwrap(), graph, seen)?.join(relative)
        } else {
            PathBuf::from(
                node["source"]["path"]
                    .as_str()
                    .ok_or_else(|| invalid("source path absent"))?,
            )
            .join(node["dir"].as_str().unwrap_or(""))
        };
        Ok(path)
    }
    let mut identities = BTreeMap::new();
    let mut paths = BTreeMap::new();
    for id in entries.keys() {
        let info = source(id, &graph, &mut BTreeSet::new())?;
        identities.insert(
            id.clone(),
            SourceNode {
                store_path: info["path"]
                    .as_str()
                    .ok_or_else(|| invalid("incomplete sourceInfo"))?
                    .into(),
                content_digest: info["metadata"]["narHash"]
                    .as_str()
                    .ok_or_else(|| invalid("incomplete source Nar identity"))?
                    .into(),
                revision: source_revision(&info["fetcher_metadata"])
                    .or_else(|| source_revision(&info["metadata"])),
            },
        );
        let path = relative::bounded_path(
            &flake_path(id, &graph, &mut BTreeSet::new())?,
            Path::new(info["path"].as_str().unwrap()),
        )?;
        paths.insert(id.clone(), path.display().to_string());
    }
    relative::annotate(&mut graph, locks, &identities, &paths)?;
    Ok(CapturedGraph {
        manifest: graph,
        trees: identities,
        paths,
    })
}

fn freeze_source(flake: &str) -> io::Result<FrozenSource> {
    if !safe_reference(flake) {
        return Err(invalid(
            "campaign source references require credential-free identity/selection parameters; configure authentication separately",
        ));
    }
    supported_nix()?;
    let (metadata, archived) = archive(flake)?;
    let temporary = claim_capture_dir(&capture_state_dir().join("campaign-captures"))?;
    let CapturedGraph {
        manifest: graph,
        trees: nodes,
        paths,
    } = captured_graph(&metadata, &archived, &temporary.0.join("roots"))?;
    let root_id = metadata["locks"]["root"].as_str().unwrap();
    let submodules = submodule_identities(
        flake,
        &metadata["locks"],
        &paths,
        &temporary.0.join("roots"),
    )?;
    let capsule = temporary.0.join("source");
    std::fs::create_dir_all(&capsule)?;
    std::fs::write(capsule.join("graph.json"), canonical(&graph)?)?;
    std::fs::write(capsule.join("replay.nix"), include_str!("replay.nix"))?;
    std::fs::write(
        capsule.join("flake.nix"),
        "{ outputs = _: import ./replay.nix (builtins.fromJSON (builtins.readFile ./graph.json)); }\n",
    )?;
    let frozen_path = String::from_utf8(output(
        "nix",
        &[
            "store".into(),
            "add-path".into(),
            "--name".into(),
            "campaign-source".into(),
            capsule.display().to_string(),
        ],
    )?)
    .map_err(|error| invalid(error.to_string()))?
    .trim()
    .to_string();
    root(&frozen_path, &temporary.0.join("roots/frozen"))?;
    let revision = nodes[root_id].revision.clone();
    let local = local_checkout(flake);
    let source = SourceIdentity {
        checkout: if flake.contains('?') || flake.starts_with("git+") || flake.starts_with("path:")
        {
            flake.into()
        } else {
            local.map_or_else(|| flake.into(), |path| path.display().to_string())
        },
        revision: revision.clone(),
        dirty: graph["nodes"][root_id]["source"]["metadata"]["dirtyRev"]
            .as_str()
            .is_some(),
        root: nodes[root_id].clone(),
        graph_digest: digest(&canonical(&graph)?),
        snapshot: nar_identity(&frozen_path, revision)?,
        lock_digest: digest(&canonical(&metadata["locks"])?),
        inputs: input_identities(&metadata["locks"], &nodes)?,
        submodules,
    };
    let (after_metadata, after_archive) = archive(flake)?;
    if metadata["locked"] != after_metadata["locked"]
        || metadata["locks"] != after_metadata["locks"]
        || archived != after_archive
        || source.submodules
            != submodule_identities(
                flake,
                &metadata["locks"],
                &paths,
                &temporary.0.join("checked-initial"),
            )?
    {
        return Err(invalid("source changed during capture; no campaign bound"));
    }
    Ok(FrozenSource {
        source,
        roots: temporary,
        metadata,
        archived,
        paths,
    })
}

/// Compare source content without evaluating inventory, settings or host targets.
pub fn observe(flake: &str) -> io::Result<SourceIdentity> {
    Ok(freeze_source(flake)?.source.clone())
}

pub fn available(target: &Target) -> io::Result<()> {
    let mut paths = BTreeSet::from([
        target.source.snapshot.store_path.clone(),
        target.source.root.store_path.clone(),
    ]);
    paths.extend(
        target
            .source
            .inputs
            .values()
            .map(|node| node.store_path.clone()),
    );
    paths.extend(
        target
            .source
            .submodules
            .values()
            .map(|module| module.content.store_path.clone()),
    );
    paths.extend(
        target
            .hosts
            .values()
            .flat_map(|host| [host.derivation.clone(), host.system_derivation.clone()]),
    );
    let mut args = vec!["path-info".into(), "--json".into()];
    args.extend(paths);
    output("nix", &args).map(|_| ()).map_err(|error| {
        invalid(format!(
            "campaign target-unavailable; no replacement target evaluated: {error}"
        ))
    })
}

pub fn bound_inventory(source: &SourceIdentity) -> io::Result<Inventory> {
    source.validate()?;
    let reference = format!("path:{}#mandala", source.snapshot.store_path);
    Inventory::from_value(nix(&[
        "eval",
        "--json",
        "--no-write-lock-file",
        &reference,
    ])?)
    .map_err(|error| invalid(error.to_string()))
}

/// A rooted read-only evaluation snapshot, independent of a warm context.
pub struct ReadSource(FrozenSource);
impl ReadSource {
    pub fn capture(flake: &str) -> io::Result<Self> {
        Ok(Self(freeze_source(flake)?))
    }
    pub fn identity(&self) -> &SourceIdentity {
        &self.0.source
    }
    pub fn expected(&self, names: &[String]) -> crate::drift::Toplevels {
        let mut out = crate::drift::Toplevels::default();
        for name in names {
            if !inventory::is_valid_member_name(name) {
                out.errors
                    .insert(name.clone(), "invalid member name".into());
                continue;
            }
            let reference = format!(
                "path:{}#nixosConfigurations.{name}.config.system.build.toplevel.outPath",
                self.0.source.snapshot.store_path
            );
            match nix(&["eval", "--json", "--no-write-lock-file", &reference]) {
                Ok(value) => match value.as_str().filter(|path| store_path(path)) {
                    Some(path) => {
                        out.paths.insert(name.clone(), path.into());
                    }
                    None => {
                        out.errors
                            .insert(name.clone(), "unknown system expectation".into());
                    }
                },
                Err(error) => {
                    out.errors.insert(name.clone(), error.to_string());
                }
            }
        }
        out
    }
}

fn system_identities(
    reference: &str,
    names: &[String],
) -> io::Result<BTreeMap<String, (String, String)>> {
    if names.is_empty() {
        return Ok(BTreeMap::new());
    }
    let names = names
        .iter()
        .map(|name| serde_json::to_string(name).map_err(invalid_json))
        .collect::<io::Result<Vec<_>>>()?
        .join(" ");
    let expression = format!(
        "cfgs: builtins.listToAttrs (map (name: {{ inherit name; value = {{ path = cfgs.${{name}}.config.system.build.toplevel.outPath; drv = cfgs.${{name}}.config.system.build.toplevel.drvPath; }}; }}) [ {names} ])"
    );
    let value = nix(&[
        "eval",
        "--json",
        "--no-write-lock-file",
        &format!("{reference}#nixosConfigurations"),
        "--apply",
        &expression,
    ])?;
    value
        .as_object()
        .ok_or_else(|| invalid("system identity evaluation returned no map"))?
        .iter()
        .map(|(name, value)| {
            let path = value["path"]
                .as_str()
                .filter(|path| store_path(path))
                .ok_or_else(|| invalid(format!("unknown system expectation for {name}")))?;
            let drv = value["drv"]
                .as_str()
                .filter(|path| store_path(path) && path.ends_with(".drv"))
                .ok_or_else(|| invalid(format!("unknown system derivation for {name}")))?;
            Ok((name.clone(), (path.into(), drv.into())))
        })
        .collect()
}

fn guard_live_derivations(flake: &str, hosts: &BTreeMap<String, HostTarget>) -> io::Result<()> {
    if hosts.is_empty() {
        return Ok(());
    }
    let mut args = vec![
        "build".into(),
        "--dry-run".into(),
        "--json".into(),
        "--no-write-lock-file".into(),
    ];
    args.extend(
        hosts
            .keys()
            .map(|name| format!("{flake}#deploy.nodes.{name}.profiles.system.path")),
    );
    let profiles: Vec<Value> =
        serde_json::from_slice(&output("nix", &args)?).map_err(invalid_json)?;
    let systems = system_identities(flake, &hosts.keys().cloned().collect::<Vec<_>>())?;
    let mut mismatches = Vec::new();
    for (index, (name, host)) in hosts.iter().enumerate() {
        if profiles
            .get(index)
            .and_then(|value| value["drvPath"].as_str())
            != Some(host.derivation.as_str())
            || systems.get(name).map(|(_, drv)| drv.as_str())
                != Some(host.system_derivation.as_str())
        {
            mismatches.push(name.clone());
        }
    }
    if profiles.len() != hosts.len() || !mismatches.is_empty() {
        return Err(invalid(format!(
            "campaign creation refused for {}: source evaluation depends on fetcher metadata the frozen graph can't reproduce (live/frozen profile or system drvPath mismatch)",
            if mismatches.is_empty() {
                hosts.keys().cloned().collect::<Vec<_>>().join(",")
            } else {
                mismatches.join(",")
            }
        )));
    }
    Ok(())
}

#[derive(Debug)]
pub struct Capture {
    pub target: Target,
    pub plan: deploy::DeployPlan,
    actor: String,
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
            vm: Default::default(),
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
            actor: actor.into(),
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
        crate::vm::ensure_campaign_supported(&inv, selector).map_err(|e| invalid(e.to_string()))?;
        let (members, operator_exclusions) = bound_selection(&inv, selector)?;
        let mut plan =
            match deploy::plan_run_with_boot(&inv, &members.join(","), throttle, false, boot) {
                Ok(plan) => plan,
                Err(deploy::DeployRunError::NoDeployableMembers(_)) => deploy::DeployPlan {
                    limit: members.join(","),
                    targets: Vec::new(),
                    skipped: members.clone(),
                    settings: BTreeMap::new(),
                    vm: Default::default(),
                    dry_activate: false,
                    boot,
                    halt_on_build_failure: false,
                    publish: Default::default(),
                    throttle,
                },
                Err(error) => return Err(invalid(error.to_string())),
            };
        let systems = system_identities(&reference, &plan.targets)?;
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
            let (system, system_drv) = systems
                .get(name)
                .ok_or_else(|| invalid("unknown system expectation"))?;
            let system_drv = system_drv.clone();
            if profile != system {
                let closure = String::from_utf8(output(
                    "nix-store",
                    &["--query".into(), "--requisites".into(), drv.into()],
                )?)
                .map_err(|error| invalid(error.to_string()))?;
                if !closure.lines().any(|path| path == system_drv) {
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
            root(
                &system_drv,
                &capture
                    .frozen
                    .roots
                    .0
                    .join("roots")
                    .join(format!("system-derivation-{index}")),
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
                    system_derivation: system_drv.clone(),
                    output: output_name.clone(),
                    settings: plan.settings[name].clone(),
                    activation,
                },
            );
        }
        guard_live_derivations(flake, &capture.target.hosts)?;
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
            || capture.frozen.metadata["locked"] != after_metadata["locked"]
            || capture.frozen.metadata["locks"] != after_metadata["locks"]
            || capture.target.source.submodules
                != submodule_identities(
                    flake,
                    &capture.frozen.metadata["locks"],
                    &capture.frozen.paths,
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
        store.create_with_actor(self.target.clone(), Utc::now(), &self.actor, |path| {
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
