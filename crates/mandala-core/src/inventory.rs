//! Inventory: the one typed read path onto a fleet.
//!
//! A parity port of the retired Python `mandala_fleet.inventory`. Everything the CLI
//! (and every engine) knows about a fleet comes from the versioned aggregate
//! the fleet flakeModule emits — `<flake>#mandala`, produced here by
//! [`crate::eval::Evaluator`]. One eval, pure data, gated by `schemaVersion`.
//!
//! The aggregate is `{ schemaVersion, members: { <name>: <member> }, groups:
//! { <group>: [<name>, …] } }`. Selector resolution (`resolve` / `to_limit`)
//! is the canonical fan-out algebra shared by `mandala deploy`, `ansible -l`,
//! and `.#deployBatch` — so a selector expands to the same set everywhere and
//! an unknown member is refused before anything launches.
//!
//! ## Serde fidelity
//!
//! Member records MUST round-trip losslessly: the section-4 MCP `members
//! full` / `host_eval` tools reproduce the *full* member JSON, so no unknown
//! field may be dropped. [`Member`] is therefore a transparent newtype over
//! the raw `serde_json::Map`, with typed accessors ([`Member::surfaces`],
//! [`Member::compact`]) layered on top — never a lossy typed projection. The
//! envelope itself ([`Aggregate`]) types `schemaVersion` / `members` /
//! `groups` explicitly and `#[serde(flatten)]`s any other top-level keys
//! (e.g. `projections`) into `extra`, so serializing an [`Aggregate`] back
//! reproduces the input value.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The aggregate schema version this build understands (the retired Python
/// package's `SUPPORTED_SCHEMA_VERSION`, carried forward). Anything
/// else is rejected at [`Inventory`] construction rather than misread.
pub const SUPPORTED_SCHEMA_VERSION: u64 = 1;

/// Validate a member selector as one bare RFC 1123 host label. Mandala keeps
/// network plane/domain data separate and constructs FQDNs in projections, so
/// dots and domain suffixes are deliberately not accepted here.
#[must_use]
pub fn is_valid_member_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=63).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
        && !name.eq_ignore_ascii_case("all")
}

/// The parity error type for inventory construction and selector resolution —
/// the Rust equivalent of the Python `InventoryError`. Each variant's
/// [`fmt::Display`] reproduces the Python message text verbatim, so callers
/// (the CLI, the MCP tool layer) surface identical strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InventoryError {
    /// The aggregate's `schemaVersion` is not [`SUPPORTED_SCHEMA_VERSION`].
    UnsupportedSchema {
        /// The version found in the aggregate (rendered as the Python `str()`
        /// of the JSON value; a missing/`null` version renders as `None`).
        found: String,
        /// The version this build supports.
        supported: u64,
    },
    /// The aggregate parsed but did not match the typed envelope shape.
    Malformed(String),
    /// Evaluating `<flake>#mandala` failed (surfaced from the evaluator).
    Eval(String),
    /// A selector contained no include part and no exclusion.
    EmptySelector,
    /// A selector resolved to the empty set (e.g. `all,!all`).
    ResolvesToNothing(String),
    /// A `@group` atom named a group not present in the aggregate.
    NoSuchGroup(String),
    /// A bare atom named neither `all`, a `@group`, nor a known member.
    NoSuchMember(String),
}

impl fmt::Display for InventoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchema { found, supported } => write!(
                f,
                "aggregate schemaVersion {found} unsupported (this CLI understands {supported})"
            ),
            Self::Malformed(msg) => write!(f, "malformed aggregate: {msg}"),
            Self::Eval(msg) => write!(f, "evaluating aggregate failed: {msg}"),
            Self::EmptySelector => write!(f, "empty selector"),
            Self::ResolvesToNothing(selector) => {
                write!(f, "selector resolves to no members: {selector}")
            }
            Self::NoSuchGroup(group) => write!(f, "no such group: {group}"),
            Self::NoSuchMember(member) => write!(f, "no such member: {member}"),
        }
    }
}

impl std::error::Error for InventoryError {}

/// One fleet member record — a transparent newtype over the raw JSON object so
/// nothing is lost on round-trip (see the module-level fidelity note). Typed
/// accessors read fields on demand.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Member(pub serde_json::Map<String, Value>);

/// Python truthiness for a JSON value: `null`/absent, `false`, `0`, `""`, and
/// empty containers are falsy; everything else is truthy. Matches the `if
/// d.get(...)` checks in the Python `surfaces()`.
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

impl Member {
    /// Borrow a top-level field of the raw member record.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    /// Compact management-surface flags: `a`(nsible) `d`(eploy-rs) `s`(ops),
    /// each replaced by `-` when the surface is absent. Parity with the Python
    /// module-level `surfaces()` — reads `deployment.ansible.enable`,
    /// `deployment.deployRs.enable`, and `deployment.sops.recipient`.
    #[must_use]
    pub fn surfaces(&self) -> String {
        let deployment = self.0.get("deployment");
        let flag = |key: &str, field: &str| -> bool {
            deployment
                .and_then(|d| d.get(key))
                .and_then(|s| s.get(field))
                .is_some_and(json_truthy)
        };
        [
            if flag("ansible", "enable") { 'a' } else { '-' },
            if flag("deployRs", "enable") { 'd' } else { '-' },
            if flag("sops", "recipient") { 's' } else { '-' },
        ]
        .into_iter()
        .collect()
    }

    /// The compact member view used by the MCP `members` tool (default,
    /// non-`full`): the present fields among `platform`, `architecture`,
    /// `category`, `role`, `tags`, plus computed `surfaces`. Absent fields are
    /// omitted, matching the Python `{k: m[k] for k in keep if k in m}`.
    #[must_use]
    pub fn compact(&self) -> serde_json::Map<String, Value> {
        const KEEP: [&str; 5] = ["platform", "architecture", "category", "role", "tags"];
        let mut out = serde_json::Map::new();
        for key in KEEP {
            if let Some(v) = self.0.get(key) {
                out.insert(key.to_string(), v.clone());
            }
        }
        out.insert("surfaces".to_string(), Value::String(self.surfaces()));
        out
    }
}

/// Free-function form of [`Member::surfaces`], for parity with the Python
/// module-level `surfaces(member)`.
#[must_use]
pub fn surfaces(member: &Member) -> String {
    member.surfaces()
}

/// The typed aggregate envelope. `schemaVersion` is kept as a raw [`Value`] so
/// the gate can report the exact found value (and so a non-integer version
/// round-trips), while any top-level keys beyond the three named ones are
/// captured in `extra` for lossless re-serialization.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Aggregate {
    /// The aggregate schema version (see [`SUPPORTED_SCHEMA_VERSION`]).
    #[serde(rename = "schemaVersion")]
    pub schema_version: Value,
    /// Every fleet member, keyed by name (sorted; matches nix's JSON output).
    pub members: BTreeMap<String, Member>,
    /// Taxonomy groups and their member names — the `@group` fan-out spelling.
    pub groups: BTreeMap<String, Vec<String>>,
    /// Any other top-level aggregate keys (e.g. `projections`), preserved
    /// verbatim so an [`Aggregate`] serializes back to its source value.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// Render a `schemaVersion` value the way the Python `str()` interpolation
/// would: a missing/`null` version as `None`, a string without quotes, and any
/// other value via its JSON form.
fn schema_display(found: Option<&Value>) -> String {
    match found {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// A typed, validated view over one flake's aggregate output — the Rust
/// counterpart of the Python `Inventory`. The eval half (producing the
/// aggregate JSON) lives in [`crate::eval::Evaluator`]; this type takes that
/// value, gates its `schemaVersion`, and exposes the member/group views plus
/// the selector algebra.
#[derive(Debug, Clone)]
pub struct Inventory {
    aggregate: Aggregate,
}

impl Inventory {
    /// Build an inventory from an already-evaluated aggregate value, gating on
    /// `schemaVersion` exactly as the Python core does: the version must equal
    /// [`SUPPORTED_SCHEMA_VERSION`] or construction fails with a message naming
    /// the found vs supported version.
    ///
    /// # Errors
    /// [`InventoryError::UnsupportedSchema`] if the version does not match;
    /// [`InventoryError::Malformed`] if the value is not a well-formed
    /// aggregate envelope.
    pub fn from_value(value: Value) -> Result<Self, InventoryError> {
        // Gate the schema BEFORE typed deserialization, so a version mismatch
        // yields the "found vs supported" message rather than a serde error.
        let found = value.get("schemaVersion");
        if found.and_then(Value::as_u64) != Some(SUPPORTED_SCHEMA_VERSION) {
            return Err(InventoryError::UnsupportedSchema {
                found: schema_display(found),
                supported: SUPPORTED_SCHEMA_VERSION,
            });
        }
        let aggregate: Aggregate =
            serde_json::from_value(value).map_err(|e| InventoryError::Malformed(e.to_string()))?;
        for (key, member) in &aggregate.members {
            if !is_valid_member_name(key) {
                return Err(InventoryError::Malformed(format!(
                    "member key {key:?} is not a bare RFC 1123 hostname"
                )));
            }
            match member.get("name").and_then(Value::as_str) {
                Some(name) if name == key => {}
                Some(name) => {
                    return Err(InventoryError::Malformed(format!(
                        "member key {key:?} does not match host.name {name:?}"
                    )));
                }
                None => {
                    return Err(InventoryError::Malformed(format!(
                        "member {key:?} has no string host.name"
                    )));
                }
            }
        }
        for (group, members) in &aggregate.groups {
            for member in members {
                if !aggregate.members.contains_key(member) {
                    return Err(InventoryError::Malformed(format!(
                        "group {group:?} references unknown member {member:?}"
                    )));
                }
            }
        }
        Ok(Self { aggregate })
    }

    /// Evaluate `<flake>#mandala` through the supplied evaluator and build a
    /// validated inventory from it — the join of the eval half and the typed
    /// half.
    ///
    /// # Errors
    /// [`InventoryError::Eval`] if evaluation fails, else the errors of
    /// [`Inventory::from_value`].
    pub fn from_evaluator(
        evaluator: &mut crate::eval::Evaluator,
        flake: &str,
    ) -> Result<Self, InventoryError> {
        let value = evaluator.aggregate(flake).map_err(InventoryError::Eval)?;
        Self::from_value(value)
    }

    /// The validated aggregate (for the full-dump surfaces: MCP `members
    /// full`, `host_eval`, and re-serialization).
    #[must_use]
    pub fn aggregate(&self) -> &Aggregate {
        &self.aggregate
    }

    /// The member records, keyed by name.
    #[must_use]
    pub fn members(&self) -> &BTreeMap<String, Member> {
        &self.aggregate.members
    }

    /// The taxonomy groups and their member names.
    #[must_use]
    pub fn groups(&self) -> &BTreeMap<String, Vec<String>> {
        &self.aggregate.groups
    }

    /// Expand a selector to a sorted, de-duplicated member set.
    ///
    /// Parts are split on `,` or ansible's `:`, stripped, and empties skipped.
    /// A `!`-prefixed part *excludes*; other parts *union* into the include
    /// set. Each atom is resolved by [`Inventory::resolve_part`]. If no include
    /// part was seen, a bare exclusion implies `all` as the base set (and a
    /// selector with neither is `empty selector`). The result is
    /// `sorted(include − exclude)`; an empty result is an error.
    ///
    /// # Errors
    /// [`InventoryError::EmptySelector`], [`InventoryError::ResolvesToNothing`],
    /// or the atom errors of [`Inventory::resolve_part`].
    pub fn resolve(&self, selector: &str) -> Result<Vec<String>, InventoryError> {
        let mut include: BTreeSet<String> = BTreeSet::new();
        let mut exclude: BTreeSet<String> = BTreeSet::new();
        let mut saw_include = false;

        // `selector.replace(':', ',').split(',')` — split on either separator.
        for raw in selector.split([',', ':']) {
            let part = raw.trim();
            if part.is_empty() {
                continue;
            }
            // Python strips the whole part once, then checks the `!` prefix
            // WITHOUT re-stripping — so any inner whitespace is preserved and
            // fed to `resolve_part` (which will reject it as unknown).
            match part.strip_prefix('!') {
                Some(atom) => exclude.extend(self.resolve_part(atom)?),
                None => {
                    saw_include = true;
                    include.extend(self.resolve_part(part)?);
                }
            }
        }

        if !saw_include {
            if exclude.is_empty() {
                return Err(InventoryError::EmptySelector);
            }
            // Bare exclusion (`!vishnu`) implies `all` as the base set.
            include = self.aggregate.members.keys().cloned().collect();
        }

        // BTreeSet difference yields sorted, de-duplicated names.
        let resolved: Vec<String> = include.difference(&exclude).cloned().collect();
        if resolved.is_empty() {
            return Err(InventoryError::ResolvesToNothing(selector.to_string()));
        }
        Ok(resolved)
    }

    /// Resolve one selector atom: `all` → every member; `@group` → the group's
    /// members (or [`InventoryError::NoSuchGroup`]); otherwise a member name
    /// (or [`InventoryError::NoSuchMember`]).
    ///
    /// # Errors
    /// [`InventoryError::NoSuchGroup`] or [`InventoryError::NoSuchMember`].
    fn resolve_part(&self, part: &str) -> Result<Vec<String>, InventoryError> {
        if part == "all" {
            return Ok(self.aggregate.members.keys().cloned().collect());
        }
        if let Some(group) = part.strip_prefix('@') {
            return self
                .aggregate
                .groups
                .get(group)
                .cloned()
                .ok_or_else(|| InventoryError::NoSuchGroup(group.to_string()));
        }
        if !self.aggregate.members.contains_key(part) {
            return Err(InventoryError::NoSuchMember(part.to_string()));
        }
        Ok(vec![part.to_string()])
    }

    /// A selector's canonical ansible `--limit` string: the fully-resolved,
    /// sorted member set comma-joined. This is the confirm token the gated MCP
    /// actions require, and the exact set a deploy fans out to — plain lists
    /// are canonicalized (sorted) and validated, never passed through verbatim.
    ///
    /// # Errors
    /// Propagates [`Inventory::resolve`].
    pub fn to_limit(&self, selector: &str) -> Result<String, InventoryError> {
        Ok(self.resolve(selector)?.join(","))
    }

    /// The top-level `projections` value (`None` if the aggregate carries no
    /// projections). Parity with the Python `inv.aggregate.get("projections",
    /// {})`.
    #[must_use]
    pub fn projections(&self) -> Option<&Value> {
        self.aggregate.extra.get("projections")
    }

    /// The deploy-rs node names from the deploy projection, in the aggregate's
    /// order (unsorted — [`crate::drift::compare`] and the `deploy nodes`
    /// command sort as needed). Parity with the Python
    /// `aggregate["projections"]["deploy"]["nodes"]`.
    #[must_use]
    pub fn deploy_nodes(&self) -> Vec<String> {
        self.projections()
            .and_then(|p| p.get("deploy"))
            .and_then(|d| d.get("nodes"))
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The projected ansible dynamic-inventory value (`None` if the ansible
    /// flakeModule was not imported). Parity with the Python
    /// `aggregate["projections"]["ansibleInventory"]`.
    #[must_use]
    pub fn ansible_inventory(&self) -> Option<&Value> {
        self.projections().and_then(|p| p.get("ansibleInventory"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The injected aggregate from the retired Python `test_inventory.py` (`_inv`):
    /// members web/cache/router, groups k3s=[cache,web], gateway=[router].
    fn test_inv() -> Inventory {
        Inventory::from_value(json!({
            "schemaVersion": 1,
            "members": {
                "web": {"name": "web"},
                "cache": {"name": "cache"},
                "router": {"name": "router"},
            },
            "groups": {"k3s": ["cache", "web"], "gateway": ["router"]},
            "projections": {},
        }))
        .expect("fixture aggregate is valid")
    }

    // ---- selector algebra: ports of test_inventory.py -----------------------

    #[test]
    fn group_selector_expands_to_projected_members() {
        // test_group_selector_expands_to_projected_members
        assert_eq!(test_inv().resolve("@k3s").unwrap(), ["cache", "web"]);
    }

    #[test]
    fn member_and_union_selectors() {
        // test_member_and_union_selectors
        let inv = test_inv();
        assert_eq!(inv.resolve("router").unwrap(), ["router"]);
        assert_eq!(
            inv.resolve("@k3s,router").unwrap(),
            ["cache", "router", "web"]
        );
    }

    #[test]
    fn unknown_selectors_fail_by_name() {
        // test_unknown_selectors_fail_by_name
        let inv = test_inv();
        assert_eq!(
            inv.resolve("@nope").unwrap_err(),
            InventoryError::NoSuchGroup("nope".to_string())
        );
        assert_eq!(
            inv.resolve("@nope").unwrap_err().to_string(),
            "no such group: nope"
        );
        assert_eq!(
            inv.resolve("ghost").unwrap_err(),
            InventoryError::NoSuchMember("ghost".to_string())
        );
        assert_eq!(
            inv.resolve("ghost").unwrap_err().to_string(),
            "no such member: ghost"
        );
    }

    #[test]
    fn to_limit_always_pins_the_resolved_set() {
        // test_to_limit_always_pins_the_resolved_set
        let inv = test_inv();
        assert_eq!(inv.to_limit("@k3s").unwrap(), "cache,web");
        // Plain lists are canonicalized (sorted) and validated.
        assert_eq!(inv.to_limit("web,cache").unwrap(), "cache,web");
        assert_eq!(
            inv.to_limit("ghost").unwrap_err().to_string(),
            "no such member: ghost"
        );
    }

    #[test]
    fn all_keyword_and_exclusions() {
        // test_all_keyword_and_exclusions
        let inv = test_inv();
        assert_eq!(inv.resolve("all").unwrap(), ["cache", "router", "web"]);
        assert_eq!(inv.resolve("all,!router").unwrap(), ["cache", "web"]);
        assert_eq!(inv.resolve("all,!@k3s").unwrap(), ["router"]);
        // A bare exclusion implies `all` as the base set.
        assert_eq!(inv.resolve("!@k3s").unwrap(), ["router"]);
        assert_eq!(
            inv.resolve("all,!all").unwrap_err(),
            InventoryError::ResolvesToNothing("all,!all".to_string())
        );
        assert_eq!(
            inv.resolve("all,!all").unwrap_err().to_string(),
            "selector resolves to no members: all,!all"
        );
        assert_eq!(inv.resolve("").unwrap_err(), InventoryError::EmptySelector);
    }

    #[test]
    fn ansible_colon_spelling() {
        // test_ansible_colon_spelling — `:` separators work like commas.
        let inv = test_inv();
        assert_eq!(inv.resolve("all:!router").unwrap(), ["cache", "web"]);
        assert_eq!(inv.to_limit("@k3s:router").unwrap(), "cache,router,web");
    }

    // ---- extra edge cases (beyond the Python suite) -------------------------

    #[test]
    fn whitespace_and_empty_parts_are_stripped() {
        let inv = test_inv();
        // Empty parts (leading/trailing/doubled separators) are skipped.
        assert_eq!(
            inv.resolve(" @k3s , router ").unwrap(),
            ["cache", "router", "web"]
        );
        assert_eq!(inv.resolve(",,router,,").unwrap(), ["router"]);
    }

    #[test]
    fn union_dedups_overlapping_atoms() {
        // `all` unioned with a member it already contains stays one entry.
        assert_eq!(
            test_inv().resolve("all,cache").unwrap(),
            ["cache", "router", "web"]
        );
    }

    #[test]
    fn bare_exclusion_of_a_member_implies_all() {
        assert_eq!(test_inv().resolve("!router").unwrap(), ["cache", "web"]);
    }

    // ---- schemaVersion gate -------------------------------------------------

    #[test]
    fn schema_gate_accepts_the_supported_version() {
        assert!(
            Inventory::from_value(json!({
                "schemaVersion": 1, "members": {}, "groups": {}
            }))
            .is_ok()
        );
    }

    #[test]
    fn schema_gate_rejects_a_newer_version_naming_both() {
        let err = Inventory::from_value(json!({
            "schemaVersion": 2, "members": {}, "groups": {}
        }))
        .unwrap_err();
        assert_eq!(
            err,
            InventoryError::UnsupportedSchema {
                found: "2".to_string(),
                supported: 1
            }
        );
        assert_eq!(
            err.to_string(),
            "aggregate schemaVersion 2 unsupported (this CLI understands 1)"
        );
    }

    #[test]
    fn schema_gate_rejects_a_missing_version_as_none() {
        let err = Inventory::from_value(json!({"members": {}, "groups": {}})).unwrap_err();
        assert_eq!(
            err.to_string(),
            "aggregate schemaVersion None unsupported (this CLI understands 1)"
        );
    }

    #[test]
    fn member_names_are_bare_rfc1123_labels() {
        for valid in ["web", "web-1", "1-web", "A1"] {
            assert!(is_valid_member_name(valid), "rejected {valid:?}");
        }
        for invalid in [
            "",
            "all",
            "-web",
            "web-",
            "web_node",
            "web.example.test",
            "../web",
            "web,cache",
            "web:cache",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(!is_valid_member_name(invalid), "accepted {invalid:?}");
        }
    }

    #[test]
    fn inventory_rejects_unsafe_or_mismatched_member_identity() {
        for (key, name) in [("web.example", "web.example"), ("web", "cache")] {
            let err = Inventory::from_value(json!({
                "schemaVersion": 1,
                "members": {key: {"name": name}},
                "groups": {},
            }))
            .unwrap_err();
            assert!(err.to_string().contains("malformed aggregate"));
        }
    }

    // ---- surfaces() ---------------------------------------------------------

    #[test]
    fn surfaces_reads_the_three_deployment_flags() {
        let full = Member(
            json!({
                "deployment": {
                    "ansible": {"enable": true},
                    "deployRs": {"enable": true},
                    "sops": {"recipient": "age1xyz"},
                }
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        assert_eq!(full.surfaces(), "ads");
        assert_eq!(surfaces(&full), "ads");

        let partial = Member(
            json!({
                "deployment": {
                    "ansible": {"enable": true},
                    "deployRs": {"enable": false},
                    "sops": {"recipient": ""},
                }
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        // deployRs disabled and an empty recipient are both falsy.
        assert_eq!(partial.surfaces(), "a--");

        let bare = Member(serde_json::Map::new());
        assert_eq!(bare.surfaces(), "---");
    }

    #[test]
    fn compact_keeps_present_fields_plus_surfaces() {
        let m = Member(
            json!({
                "platform": "nixos",
                "architecture": "aarch64",
                "role": "worker",
                "extra": {"dropped": false},
                "deployment": {"ansible": {"enable": true}},
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        let compact = m.compact();
        assert_eq!(compact.get("platform"), Some(&json!("nixos")));
        assert_eq!(compact.get("architecture"), Some(&json!("aarch64")));
        assert_eq!(compact.get("role"), Some(&json!("worker")));
        // Absent keep-fields (category, tags) are omitted; non-keep fields drop.
        assert!(!compact.contains_key("category"));
        assert!(!compact.contains_key("tags"));
        assert!(!compact.contains_key("extra"));
        assert_eq!(compact.get("surfaces"), Some(&json!("a--")));
    }

    // ---- serde fidelity -----------------------------------------------------

    #[test]
    fn member_records_round_trip_losslessly() {
        // A member carrying fields beyond the typed/known ones must survive a
        // parse → serialize cycle unchanged (MCP `members full` / `host_eval`
        // reproduce the full record).
        let input = json!({
            "schemaVersion": 1,
            "members": {
                "web": {
                    "name": "web",
                    "platform": "nixos",
                    "architecture": "x86_64",
                    "category": "server",
                    "role": "web",
                    "tags": ["public", "edge"],
                    "deployment": {
                        "ansible": {"enable": true},
                        "deployRs": {"enable": false},
                        "sops": {"recipient": "age1web"},
                    },
                    "unknownNested": {"deep": [1, 2, {"x": true}]},
                    "extraScalar": 42,
                }
            },
            "groups": {"web": ["web"]},
            "projections": {"ansibleInventory": {"all": {}}},
        });
        let inv = Inventory::from_value(input.clone()).unwrap();
        let round_tripped = serde_json::to_value(inv.aggregate()).unwrap();
        // Value equality is order-independent for objects, so this asserts no
        // field (typed, unknown, or top-level `projections`) was dropped.
        assert_eq!(round_tripped, input);
    }

    #[test]
    fn resolution_uses_member_keys_only_of_the_typed_view() {
        // Resolution works purely over member names and groups, regardless of
        // member body contents.
        let inv = Inventory::from_value(json!({
            "schemaVersion": 1,
            "members": {
                "alpha": {"name": "alpha", "junk": 1},
                "beta": {"name": "beta"},
            },
            "groups": {"pair": ["alpha", "beta"]},
        }))
        .unwrap();
        assert_eq!(inv.resolve("@pair").unwrap(), ["alpha", "beta"]);
        assert_eq!(inv.members().len(), 2);
        assert_eq!(inv.groups()["pair"], ["alpha", "beta"]);
    }
}
