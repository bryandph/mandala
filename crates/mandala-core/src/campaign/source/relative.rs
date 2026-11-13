//! SourceInfo overrides for relative inputs loaded during Nix 2.34 locking.
//! Kept nodes inherit their parent. Newly loaded/refetched nodes instead get
//! the metadata of their relative path reference and a canonical subdirectory.
use super::*;

fn lock_at(path: &Path) -> io::Result<Value> {
    match std::fs::read(path.join("flake.lock")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(invalid_json),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(serde_json::json!({"root":"root","nodes":{"root":{}}}))
        }
        Err(error) => Err(error),
    }
}

pub(super) fn bounded_path(path: &Path, root: &Path) -> io::Result<PathBuf> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                if !result.pop() {
                    return Err(invalid("relative source escapes its archive"));
                }
            }
            std::path::Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    if !result.starts_with(root) {
        return Err(invalid("relative source escapes its archive"));
    }
    Ok(result)
}

fn quote(value: &str) -> io::Result<String> {
    Ok(serde_json::to_string(value)
        .map_err(invalid_json)?
        .replace("${", "\\${"))
}

fn declared_overrides(tree: &SourceNode, path: &Path) -> io::Result<Vec<Vec<String>>> {
    let directory = path
        .strip_prefix(&tree.store_path)
        .map_err(|_| invalid("source declaration path outside archive"))?
        .display()
        .to_string();
    let expression = format!(
        r#"let
      source = builtins.fetchTree {{ type = "path"; path = {}; narHash = {}; }};
      authored = import (source.outPath + "/" + {} + "/flake.nix");
      walk = prefix: entries: builtins.concatLists (map (name: let
        entry = entries.${{name}};
        path = prefix ++ [name];
        hasRef = builtins.any (key: key != "inputs" && key != "flake") (builtins.attrNames entry);
      in (if hasRef then [path] else []) ++ walk path (entry.inputs or {{}})) (builtins.attrNames entries));
    in builtins.concatLists (map (name: walk [name] ((authored.inputs.${{name}}).inputs or {{}})) (builtins.attrNames (authored.inputs or {{}})))"#,
        quote(&tree.store_path)?,
        quote(&tree.content_digest)?,
        quote(&directory)?
    );
    serde_json::from_value(nix(&["eval", "--json", "--expr", &expression])?).map_err(invalid_json)
}

fn relative_metadata(locked: &Value) -> io::Result<Value> {
    let mut metadata = serde_json::Map::new();
    for key in [
        "narHash",
        "revCount",
        "dirtyRev",
        "dirtyShortRev",
        "lastModified",
    ] {
        if !locked[key].is_null() {
            metadata.insert(key.into(), locked[key].clone());
        }
    }
    if let Some(revision) = locked["rev"].as_str() {
        metadata.insert("rev".into(), Value::from(revision));
        metadata.insert(
            "shortRev".into(),
            Value::from(revision.chars().take(7).collect::<String>()),
        );
    }
    if let Some(seconds) = locked["lastModified"].as_i64() {
        let date = chrono::DateTime::from_timestamp(seconds, 0)
            .ok_or_else(|| invalid("unsupported relative-source timestamp"))?;
        metadata.insert(
            "lastModifiedDate".into(),
            Value::from(date.format("%Y%m%d%H%M%S").to_string()),
        );
    }
    Ok(Value::Object(metadata))
}

struct Walk<'a> {
    graph: &'a mut Value,
    locks: &'a Value,
    trees: &'a BTreeMap<String, SourceNode>,
    paths: &'a BTreeMap<String, String>,
    overrides: BTreeSet<Vec<String>>,
    seen: BTreeSet<String>,
}
impl Walk<'_> {
    fn node(
        &mut self,
        id: &str,
        old: &Value,
        old_id: &str,
        context: &[String],
        scope: &[String],
        trust: bool,
    ) -> io::Result<()> {
        if !self.seen.insert(id.into()) {
            return Ok(());
        }
        if !trust {
            for mut path in declared_overrides(&self.trees[id], Path::new(&self.paths[id]))? {
                let mut absolute = scope.to_vec();
                absolute.append(&mut path);
                self.overrides.insert(absolute);
            }
        }
        let children = self.locks["nodes"][id]["inputs"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        for (name, edge) in children {
            let Some(child) = edge.as_str() else { continue };
            let new = &self.locks["nodes"][child];
            let old_child = old["nodes"][old_id]["inputs"][&name].as_str();
            let old_node = old_child.map(|id| &old["nodes"][id]);
            let parent = old_node
                .and_then(|node| node["parent"].as_array())
                .map(|path| {
                    let mut path_prefix =
                        context.iter().cloned().map(Value::from).collect::<Vec<_>>();
                    path_prefix.extend(path.iter().cloned());
                    Value::Array(path_prefix)
                })
                .unwrap_or(Value::Null);
            let kept = old_node
                .is_some_and(|node| node["original"] == new["original"] && parent == new["parent"]);
            let mut child_scope = scope.to_vec();
            child_scope.push(name);
            let refetch = kept
                && !trust
                && old_node
                    .and_then(|node| node["inputs"].as_object())
                    .is_some_and(|inputs| {
                        inputs.iter().any(|(name, edge)| {
                            let mut path = child_scope.clone();
                            path.push(name.clone());
                            edge.is_array() && !self.overrides.contains(&path)
                        })
                    });
            if is_relative(new) && (!kept || refetch) {
                self.graph["nodes"][child]["relative_override"] =
                    relative_metadata(&new["locked"])?;
                let directory = Path::new(&self.paths[child])
                    .strip_prefix(&self.trees[child].store_path)
                    .map_err(|_| invalid("relative source outside archive"))?;
                self.graph["nodes"][child]["relative_dir"] =
                    Value::from(directory.display().to_string());
            }
            if !self.graph["nodes"][child]["flake"]
                .as_bool()
                .unwrap_or(true)
            {
                continue;
            }
            if kept {
                self.node(
                    child,
                    old,
                    old_child.unwrap(),
                    context,
                    &child_scope,
                    !refetch,
                )?;
            } else {
                let own = lock_at(Path::new(&self.paths[child]))?;
                let own_root = own["root"]
                    .as_str()
                    .ok_or_else(|| invalid("input lock root absent"))?
                    .to_string();
                self.node(child, &own, &own_root, &child_scope, &child_scope, false)?;
            }
        }
        Ok(())
    }
}

pub(super) fn annotate(
    graph: &mut Value,
    locks: &Value,
    trees: &BTreeMap<String, SourceNode>,
    paths: &BTreeMap<String, String>,
) -> io::Result<()> {
    if !locks["nodes"]
        .as_object()
        .unwrap()
        .values()
        .any(is_relative)
    {
        return Ok(());
    }
    let root = locks["root"].as_str().unwrap();
    let old = lock_at(Path::new(&paths[root]))?;
    let old_root = old["root"]
        .as_str()
        .ok_or_else(|| invalid("original lock root absent"))?
        .to_string();
    Walk {
        graph,
        locks,
        trees,
        paths,
        overrides: BTreeSet::new(),
        seen: BTreeSet::new(),
    }
    .node(root, &old, &old_root, &[], &[], false)
}
