//! A minimal, single-threaded warm Nix evaluator built on the **safe**
//! `nix-bindings` crate (which wraps libnixexpr-c / libnixflake-c). This module
//! contains no hand-rolled `unsafe` FFI: setup, flake locking, value navigation
//! and deep forcing all go through the crate's safe wrappers.
//!
//! The whole point of the worker is a *warm* `EvalState`: the store, the
//! evaluator, and nixpkgs setup are paid for once, then a locked flake is
//! cached ([`LockedFlake`] holds a GC-managed graph) so a repeated `aggregate`
//! re-derives its output attrset from the already-locked flake and re-forces
//! memoized thunks instead of re-locking and re-evaluating from cold.
//!
//! ## Parity with `nix eval --json`
//!
//! [`Evaluator::aggregate`] navigates to `<flake>#mandala` and walks the value
//! recursively, **forcing every node before reading its type** (see
//! [`Evaluator::value_to_json_at`]). That per-node force is the correctness
//! crux: attr/list getters hand back *thunks*, and reading a thunk's type
//! without forcing it yields [`ValueType::Thunk`] — the "shallow / pure-data"
//! symptom. Forcing each node produces the same fully-evaluated tree that
//! `nix eval --json` serializes.
//!
//! ### String context is discarded, never realised
//!
//! The safe crate's only string accessors ([`Value::as_string`] et al.) call
//! `nix_string_realise`, which **builds** the derivations in a string's context
//! (`realiseString` → `realiseContext` → `buildPaths`). `nix eval --json` does
//! the opposite: it prints a context string's bytes *without* building. The
//! aggregate carries such strings for real — e.g. 33 `ssh_host_*-cert.pub`
//! store paths — and `config.system.build.toplevel.outPath` is a whole NixOS
//! system's store path. Realising any of those would build them.
//!
//! So every string leaf is read through [`builtins.unsafeDiscardStringContext`]
//! first (see [`read_string_no_context`]): stripping the context leaves
//! `as_string` with nothing to build, and the discard never alters the string
//! bytes, so the output is byte-identical to `nix eval --json`. This reproduces
//! exactly what the previous raw-FFI worker did with the non-realising
//! `nix_get_string` accessor (which the safe layer does not expose).
//!
//! ## Flake fetching (git-aware, not `path:`)
//!
//! We lock through the flake API ([`LockMode::Virtual`]) with the flake
//! reference given as a bare filesystem path. On a git working tree that
//! resolves to `git+file://` — git-tracked files only — matching the CLI
//! `.#mandala` installable. (`builtins.getFlake "path:…"` was rejected: it
//! demands `--impure` on a dirty tree and then copies untracked cruft such as
//! `.git/fsmonitor--daemon.ipc`, which is not a regular file.)

// The safe crate's constructors hand back `Arc<Context>` / `Arc<Store>` /
// `Arc<FlakeSettings>` (its wrappers are `Send` but deliberately `!Sync`, so the
// C API's per-context error buffer can't be raced). The flake API *requires*
// these Arcs by signature. This is the crate's own documented pattern — it
// allows the very same lint in its test suite — and the worker pins all use to
// one thread, so the non-`Sync` Arc is sound.
#![allow(clippy::arc_with_non_send_sync)]

use std::collections::BTreeMap;
use std::sync::Arc;

use nix_bindings::flake::{
    FetchersSettings, FlakeReference, FlakeReferenceParseFlags, FlakeSettings, LockFlags, LockMode,
    LockedFlake,
};
use nix_bindings::{Context, EvalState, EvalStateBuilder, NixValueOps, Store, Value, ValueType};
use serde_json::Value as Json;

/// A human-readable evaluation error (the worker never surfaces raw bytes).
pub type EvalError = String;

/// Render any error type that carries a `Display` impl as an [`EvalError`]. The
/// safe crate's `Error` is `Display`, and the aggregate / toplevel paths carry
/// no secrets, so this stays diagnostic.
fn e2s<E: std::fmt::Display>(e: E) -> EvalError {
    e.to_string()
}

/// Force `val` via a shared reference. The Nix C API mutates the underlying
/// thunk on first force, but the operation is idempotent, so `&Value` is sound
/// ([`NixValueOps::force`] takes `&self`).
fn force(val: &Value) -> Result<(), EvalError> {
    NixValueOps::force(val).map_err(e2s)
}

/// Read a forced string `val` **without realising its context** (i.e. without
/// building the derivations the string references). `discard` must be the
/// `builtins.unsafeDiscardStringContext` function value: applying it strips the
/// context so the subsequent [`Value::as_string`] (which realises) has nothing
/// to build, while leaving the string bytes untouched. See the module docs.
fn read_string_no_context(val: &Value, discard: &Value) -> Result<String, EvalError> {
    let stripped = discard.call(val).map_err(e2s)?;
    stripped.as_string().map_err(e2s)
}

/// A warm Nix evaluator with a per-flake locked-flake cache.
///
/// Single-threaded by construction: the safe wrappers are `Send` but not
/// `Sync` (the underlying C API context is thread-local and every call writes
/// its error buffer), and the worker touches this from one pinned thread.
pub struct Evaluator {
    ctx: Arc<Context>,
    state: EvalState,
    flake_settings: Arc<FlakeSettings>,
    fetch_settings: FetchersSettings,
    /// flake path → locked flake (its outputs are re-derived per call from the
    /// warm `EvalState`, which memoizes the heavy nixpkgs / module-system eval).
    cache: BTreeMap<String, LockedFlake>,
}

impl Evaluator {
    /// Initialise the libraries, open the default store, and build a warm
    /// `EvalState` with flake support wired in.
    pub fn new() -> Result<Self, EvalError> {
        // `Context::new` runs the one-shot libutil / libstore / libexpr inits.
        let ctx = Arc::new(Context::new().map_err(e2s)?);

        // Experimental features: the CLI resolves these from global config PLUS
        // the target flake's `nixConfig.extra-experimental-features` (honoured
        // under `accept-flake-config`). The C flake settings do not replay a
        // flake's nixConfig, so we set the union the parent nixspace flake needs
        // explicitly — `pipe-operators` for the `mightyiam/files` modules that
        // use `|>`, on top of the standard flake stack. Set before the
        // `EvalState` is built so it takes effect. Overridable via
        // `MANDALA_EVAL_EXPERIMENTAL_FEATURES`.
        let features = std::env::var("MANDALA_EVAL_EXPERIMENTAL_FEATURES")
            .unwrap_or_else(|_| "nix-command flakes fetch-tree pipe-operators".to_string());
        ctx.set_setting("experimental-features", &features)
            .map_err(e2s)?;

        // NOTE deliberately NOT set: `warn-dirty = false`. The dirty-tree
        // warning is a libfetchers setting that `nix_setting_set` does not
        // reach (it lives on the C API's own `FetchersSettings` object, which
        // exposes no setter) — attempting `ctx.set_setting("warn-dirty", …)`
        // at ANY point raises an unknown-setting C++ exception that unwinds
        // through the Rust frame and ABORTS the worker on startup ("Rust
        // cannot catch foreign exceptions"), invisibly under the TUI's nulled
        // stderr (live 7.4 finding, mandala-native-tui). The warning is
        // stderr-only and stdout parity with `nix eval --json` is unaffected;
        // TUI-hosted workers null stderr (`Evaluator::quiet`), so the noise
        // never reaches the alternate screen. The spawn regression test
        // (`tests/spawn.rs`) guards the never-abort invariant.
        let store = Arc::new(Store::open(&ctx, None).map_err(e2s)?);
        let flake_settings = Arc::new(FlakeSettings::new(&ctx).map_err(e2s)?);
        let fetch_settings = FetchersSettings::new(&ctx).map_err(e2s)?;

        let state = EvalStateBuilder::new(&store)
            .map_err(e2s)?
            .with_flake_settings(&flake_settings)
            .map_err(e2s)?
            .build()
            .map_err(e2s)?;

        Ok(Self {
            ctx,
            state,
            flake_settings,
            fetch_settings,
            cache: BTreeMap::new(),
        })
    }

    /// Ensure `flake` is locked (in `virtual` mode: compute the lock in memory,
    /// never write `flake.lock` — read-only eval semantics, matching
    /// `nix eval`) and cached.
    fn ensure_locked(&mut self, flake: &str) -> Result<(), EvalError> {
        if self.cache.contains_key(flake) {
            return Ok(());
        }

        // Base directory = the flake path itself; the reference is the bare
        // path (no `path:` prefix) → git+file on a working tree.
        let parse_flags = FlakeReferenceParseFlags::new(&self.ctx, &self.flake_settings)
            .map_err(e2s)?
            .set_base_directory(flake)
            .map_err(e2s)?;
        let (flake_ref, _fragment) = FlakeReference::parse(
            &self.ctx,
            &self.fetch_settings,
            &self.flake_settings,
            &parse_flags,
            flake,
        )
        .map_err(e2s)?;

        let lock_flags = LockFlags::new(&self.ctx, &self.flake_settings)
            .map_err(e2s)?
            .set_mode(LockMode::Virtual)
            .map_err(e2s)?;
        let locked = LockedFlake::lock(
            &self.ctx,
            &self.fetch_settings,
            &self.flake_settings,
            &self.state,
            &lock_flags,
            &flake_ref,
        )
        .map_err(e2s)?;

        self.cache.insert(flake.to_string(), locked);
        Ok(())
    }

    /// The cached flake's output attrset, re-derived against the warm
    /// `EvalState` (the expensive eval is memoized in the state, so this is
    /// effectively a re-force of already-evaluated thunks).
    fn outputs<'s>(&'s self, flake: &str) -> Result<Value<'s>, EvalError> {
        let locked = self
            .cache
            .get(flake)
            .ok_or_else(|| format!("flake {flake:?} not locked"))?;
        locked
            .output_attrs(&self.flake_settings, &self.state)
            .map_err(e2s)
    }

    /// The `builtins.unsafeDiscardStringContext` function value, used to read
    /// context strings without building (see [`read_string_no_context`]).
    fn discard_fn(&self) -> Result<Value<'_>, EvalError> {
        self.state
            .eval_from_string("builtins.unsafeDiscardStringContext", "<eval>")
            .map_err(e2s)
    }

    /// Recursively force `val` and convert to a `serde_json::Value`, exactly the
    /// tree `nix eval --json` would serialize. Forces every node first — the
    /// deep-force that keeps nested/derivation values from coming back shallow.
    ///
    /// `depth`/`path` guard against a runaway walk (a shared-thunk graph that
    /// never bottoms out, or a mis-navigation into a self-referential flake
    /// node): the aggregate is only a handful of levels deep, so exceeding the
    /// cap is a bug, reported with the attribute breadcrumb rather than a
    /// stack-overflow abort.
    fn value_to_json_at(
        &self,
        val: &Value,
        discard: &Value,
        depth: usize,
        path: &mut Vec<String>,
    ) -> Result<Json, EvalError> {
        const MAX_DEPTH: usize = 128;
        if depth > MAX_DEPTH {
            return Err(format!(
                "value_to_json exceeded depth {MAX_DEPTH} at .{} (cycle or mis-navigation)",
                path.join(".")
            ));
        }
        force(val)?;
        match val.value_type() {
            ValueType::Null => Ok(Json::Null),
            ValueType::Bool => Ok(Json::Bool(val.as_bool().map_err(e2s)?)),
            ValueType::Int => Ok(Json::from(val.as_int().map_err(e2s)?)),
            ValueType::Float => Ok(Json::from(val.as_float().map_err(e2s)?)),
            ValueType::String => Ok(Json::String(read_string_no_context(val, discard)?)),
            ValueType::Path => {
                // `as_path` uses the non-building `nix_get_path_string`.
                let p = val.as_path().map_err(e2s)?;
                Ok(Json::String(p.to_string_lossy().into_owned()))
            }
            ValueType::Attrs => {
                // Replicate nix's `printValueAsJSON` attrset coercion, so that
                // derivations and `__toString` sets serialize the way
                // `nix eval --json` does (their string / store path) rather than
                // recursing into their frequently self-referential structure — a
                // derivation's `.all` output list contains the derivation itself,
                // which is the cycle that would overflow the walk. nix tries
                // `__toString` first, then falls back to `outPath`.
                if val.has_attr("__toString").map_err(e2s)? {
                    let f = val.get_attr("__toString").map_err(e2s)?;
                    let out = f.call(val).map_err(e2s)?;
                    return self.value_to_json_at(&out, discard, depth + 1, path);
                }
                if val.has_attr("outPath").map_err(e2s)? {
                    let op = val.get_attr("outPath").map_err(e2s)?;
                    return self.value_to_json_at(&op, discard, depth + 1, path);
                }
                // serde_json's default Map is key-sorted on serialization, and
                // the C API's attr iteration is symbol-sorted — both agree with
                // `nix eval --json`.
                let mut map = serde_json::Map::new();
                for entry in val.attrs().map_err(e2s)? {
                    let (key, child) = entry.map_err(e2s)?;
                    path.push(key.clone());
                    let child_json = self.value_to_json_at(&child, discard, depth + 1, path);
                    path.pop();
                    map.insert(key, child_json?);
                }
                Ok(Json::Object(map))
            }
            ValueType::List => {
                let mut arr = Vec::new();
                for (i, entry) in val.list_iter().map_err(e2s)?.enumerate() {
                    let child = entry.map_err(e2s)?;
                    path.push(format!("[{i}]"));
                    let child_json = self.value_to_json_at(&child, discard, depth + 1, path);
                    path.pop();
                    arr.push(child_json?);
                }
                Ok(Json::Array(arr))
            }
            other => Err(format!(
                "cannot serialize nix value of type {other} to JSON"
            )),
        }
    }

    /// `<flake>#mandala`, deeply forced to JSON — parity with
    /// `nix eval --no-warn-dirty --json <flake>#mandala`.
    pub fn aggregate(&mut self, flake: &str) -> Result<Json, EvalError> {
        self.ensure_locked(flake)?;
        let discard = self.discard_fn()?;
        let outputs = self.outputs(flake)?;
        force(&outputs)?;
        let mandala = outputs.get_attr("mandala").map_err(e2s)?;
        self.value_to_json_at(&mandala, &discard, 0, &mut Vec::new())
    }

    /// Force `cur`, then walk `keys` one attribute at a time and read the final
    /// value as a context-free string. Written as a recursion (not a
    /// reassignment loop) so each intermediate value stays alive on the stack
    /// while its child borrows it.
    fn nav_to_string(
        &self,
        cur: &Value,
        keys: &[&str],
        discard: &Value,
    ) -> Result<String, EvalError> {
        force(cur)?;
        match keys.split_first() {
            None => read_string_no_context(cur, discard),
            Some((head, rest)) => {
                let child = cur.get_attr(head).map_err(e2s)?;
                self.nav_to_string(&child, rest, discard)
            }
        }
    }

    /// `<flake>#nixosConfigurations.<member>.config.system.build.toplevel.outPath`.
    /// Returns `None` if the member has no `nixosConfigurations` entry.
    pub fn host_toplevel(
        &mut self,
        flake: &str,
        member: &str,
    ) -> Result<Option<String>, EvalError> {
        let bytes = member.as_bytes();
        if !(1..=63).contains(&bytes.len())
            || !bytes.first().is_some_and(u8::is_ascii_alphanumeric)
            || !bytes.last().is_some_and(u8::is_ascii_alphanumeric)
            || !bytes
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
            || member.eq_ignore_ascii_case("all")
        {
            return Err(format!("refusing invalid member name {member:?}"));
        }
        self.ensure_locked(flake)?;
        let discard = self.discard_fn()?;
        let outputs = self.outputs(flake)?;
        force(&outputs)?;
        let cfgs = outputs.get_attr("nixosConfigurations").map_err(e2s)?;
        force(&cfgs)?;
        // A missing member is `None`; a present one navigates to its outPath.
        let host = match cfgs.get_attr(member) {
            Ok(h) => h,
            Err(_) => return Ok(None),
        };
        let path = self.nav_to_string(
            &host,
            &["config", "system", "build", "toplevel", "outPath"],
            &discard,
        )?;
        Ok(Some(path))
    }

    /// Expected toplevel out-paths for `members` (parity with
    /// `drift.eval_expected`), one warm navigation per member.
    pub fn expected_toplevels(
        &mut self,
        flake: &str,
        members: &[String],
    ) -> Result<BTreeMap<String, String>, EvalError> {
        let mut out = BTreeMap::new();
        for m in members {
            if let Some(path) = self.host_toplevel(flake, m)? {
                out.insert(m.clone(), path);
            }
        }
        Ok(out)
    }

    /// Drop the warm locked-flake cache so a moved contract is re-locked and
    /// re-evaluated on the next request (stale-state discipline). Dropping each
    /// [`LockedFlake`] releases its GC-managed graph.
    pub fn reload(&mut self) {
        self.cache.clear();
    }
}
