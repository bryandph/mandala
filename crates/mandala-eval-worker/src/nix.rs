//! A minimal, single-threaded wrapper over the Nix C API (libnixexpr-c +
//! libnixflake-c) via the raw `nix-bindings-sys` FFI.
//!
//! The whole point of the worker is a *warm* `EvalState`: the store, the
//! evaluator, and nixpkgs setup are paid for once, then a locked flake's
//! output attrset is cached (pinned as a GC root with `nix_gc_incref`) so a
//! repeated `aggregate` re-forces a memoized thunk instead of re-locking and
//! re-evaluating from cold.
//!
//! ## Parity with `nix eval --json`
//!
//! [`Evaluator::aggregate`] navigates to `<flake>#mandala` and walks the value
//! recursively, **forcing every node before reading its type** (see
//! [`Evaluator::value_to_json`]). That per-node force is the correctness crux:
//! the C API's attr/list getters hand back *thunks*, and reading a thunk's type
//! without forcing it yields `NIX_TYPE_THUNK` — the "shallow / pure-data"
//! symptom. Forcing each node produces the same fully-evaluated tree that
//! `nix eval --json` serializes.
//!
//! ## Flake fetching (git-aware, not `path:`)
//!
//! We lock through the flake C API (`nix_flake_lock` in `virtual` mode) with
//! the flake reference given as a bare filesystem path. On a git working tree
//! that resolves to `git+file://` — git-tracked files only — matching the CLI
//! `.#mandala` installable. (`builtins.getFlake "path:…"` was rejected: it
//! demands `--impure` on a dirty tree and then copies untracked cruft such as
//! `.git/fsmonitor--daemon.ipc`, which is not a regular file.)

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]

use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_void};
use std::os::raw::{c_char, c_uint};
use std::ptr;

use nix_bindings_sys as ffi;
use serde_json::Value as Json;

/// A human-readable evaluation error (the worker never surfaces raw bytes).
pub type EvalError = String;

/// `nix_get_string` / `nix_err_msg` callback: append the delivered bytes to a
/// `Vec<u8>` handed through `user_data`. May be invoked more than once.
extern "C" fn collect_cb(start: *const c_char, n: c_uint, user_data: *mut c_void) {
    if start.is_null() || user_data.is_null() {
        return;
    }
    let buf = unsafe { &mut *user_data.cast::<Vec<u8>>() };
    let slice = unsafe { std::slice::from_raw_parts(start.cast::<u8>(), n as usize) };
    buf.extend_from_slice(slice);
}

/// Read the last error message off a context (never emits secret bytes; the
/// aggregate and toplevel paths carry no secrets, but this stays diagnostic).
unsafe fn last_error(ctx: *mut ffi::nix_c_context) -> String {
    let mut n: c_uint = 0;
    let p = unsafe { ffi::nix_err_msg(ptr::null_mut(), ctx, &mut n) };
    if p.is_null() {
        return "unknown nix error".to_string();
    }
    let slice = unsafe { std::slice::from_raw_parts(p.cast::<u8>(), n as usize) };
    String::from_utf8_lossy(slice).into_owned()
}

/// A warm Nix evaluator with a per-flake locked-outputs cache.
///
/// Single-threaded by construction: the C API context is thread-local and every
/// call writes its error buffer, so the worker touches this from one thread.
pub struct Evaluator {
    ctx: *mut ffi::nix_c_context,
    state: *mut ffi::EvalState,
    flake_settings: *mut ffi::nix_flake_settings,
    fetch_settings: *mut ffi::nix_fetchers_settings,
    /// flake path → pinned (`nix_gc_incref`'d) output attrset value.
    cache: BTreeMap<String, *mut ffi::nix_value>,
}

impl Evaluator {
    /// Initialise the libraries, open the default store, and build a warm
    /// `EvalState` with flake support wired in.
    pub fn new() -> Result<Self, EvalError> {
        unsafe {
            let ctx = ffi::nix_c_context_create();
            if ctx.is_null() {
                return Err("nix_c_context_create returned null".to_string());
            }
            if ffi::nix_libutil_init(ctx) != ffi::nix_err_NIX_OK {
                return Err(format!("libutil init: {}", last_error(ctx)));
            }
            if ffi::nix_libstore_init(ctx) != ffi::nix_err_NIX_OK {
                return Err(format!("libstore init: {}", last_error(ctx)));
            }
            if ffi::nix_libexpr_init(ctx) != ffi::nix_err_NIX_OK {
                return Err(format!("libexpr init: {}", last_error(ctx)));
            }

            // Experimental features: the CLI resolves these from global config
            // PLUS the target flake's `nixConfig.extra-experimental-features`
            // (honoured under `accept-flake-config`). The C flake settings do
            // not replay a flake's nixConfig, so we set the union the parent
            // nixspace flake needs explicitly — `pipe-operators` for the
            // `mightyiam/files` modules that use `|>`, on top of the standard
            // flake stack. Overridable via `MANDALA_EVAL_EXPERIMENTAL_FEATURES`.
            let features = std::env::var("MANDALA_EVAL_EXPERIMENTAL_FEATURES")
                .unwrap_or_else(|_| "nix-command flakes fetch-tree pipe-operators".to_string());
            let cfeatures =
                CString::new(features).map_err(|_| "bad experimental-features".to_string())?;
            if ffi::nix_setting_set(ctx, c"experimental-features".as_ptr(), cfeatures.as_ptr())
                != ffi::nix_err_NIX_OK
            {
                return Err(format!("set experimental-features: {}", last_error(ctx)));
            }

            let fetch_settings = ffi::nix_fetchers_settings_new(ctx);
            if fetch_settings.is_null() {
                return Err(format!("fetchers settings: {}", last_error(ctx)));
            }
            let flake_settings = ffi::nix_flake_settings_new(ctx);
            if flake_settings.is_null() {
                return Err(format!("flake settings: {}", last_error(ctx)));
            }

            let store = ffi::nix_store_open(ctx, ptr::null(), ptr::null_mut());
            if store.is_null() {
                return Err(format!("store open: {}", last_error(ctx)));
            }

            let builder = ffi::nix_eval_state_builder_new(ctx, store);
            if builder.is_null() {
                return Err(format!("eval state builder: {}", last_error(ctx)));
            }
            if ffi::nix_eval_state_builder_load(ctx, builder) != ffi::nix_err_NIX_OK {
                return Err(format!("eval state builder load: {}", last_error(ctx)));
            }
            if ffi::nix_flake_settings_add_to_eval_state_builder(ctx, flake_settings, builder)
                != ffi::nix_err_NIX_OK
            {
                return Err(format!("flake settings → builder: {}", last_error(ctx)));
            }
            let state = ffi::nix_eval_state_build(ctx, builder);
            ffi::nix_eval_state_builder_free(builder);
            if state.is_null() {
                return Err(format!("eval state build: {}", last_error(ctx)));
            }

            Ok(Self {
                ctx,
                state,
                flake_settings,
                fetch_settings,
                cache: BTreeMap::new(),
            })
        }
    }

    /// Lock `flake` and return its (cached, GC-pinned) output attrset value.
    fn flake_outputs(&mut self, flake: &str) -> Result<*mut ffi::nix_value, EvalError> {
        if let Some(v) = self.cache.get(flake) {
            return Ok(*v);
        }
        let outputs = unsafe { self.lock_outputs(flake)? };
        unsafe { ffi::nix_gc_incref(self.ctx, outputs.cast()) };
        self.cache.insert(flake.to_string(), outputs);
        Ok(outputs)
    }

    unsafe fn lock_outputs(&self, flake: &str) -> Result<*mut ffi::nix_value, EvalError> {
        unsafe {
            let parse_flags =
                ffi::nix_flake_reference_parse_flags_new(self.ctx, self.flake_settings);
            ffi::nix_flake_reference_parse_flags_set_base_directory(
                self.ctx,
                parse_flags,
                flake.as_ptr().cast(),
                flake.len(),
            );

            let mut flake_ref: *mut ffi::nix_flake_reference = ptr::null_mut();
            let mut fragment: Vec<u8> = Vec::new();
            // Bare path (no `path:` prefix) → git+file on a working tree.
            let err = ffi::nix_flake_reference_and_fragment_from_string(
                self.ctx,
                self.fetch_settings,
                self.flake_settings,
                parse_flags,
                flake.as_ptr().cast(),
                flake.len(),
                &mut flake_ref,
                Some(collect_cb),
                (&mut fragment as *mut Vec<u8>).cast(),
            );
            ffi::nix_flake_reference_parse_flags_free(parse_flags);
            if err != ffi::nix_err_NIX_OK || flake_ref.is_null() {
                return Err(format!("parse flake ref: {}", last_error(self.ctx)));
            }

            let lock_flags = ffi::nix_flake_lock_flags_new(self.ctx, self.flake_settings);
            // `virtual`: compute the lock in memory, never write flake.lock —
            // read-only eval semantics, matching `nix eval`.
            ffi::nix_flake_lock_flags_set_mode_virtual(self.ctx, lock_flags);
            let locked = ffi::nix_flake_lock(
                self.ctx,
                self.fetch_settings,
                self.flake_settings,
                self.state,
                lock_flags,
                flake_ref,
            );
            ffi::nix_flake_lock_flags_free(lock_flags);
            ffi::nix_flake_reference_free(flake_ref);
            if locked.is_null() {
                return Err(format!("lock flake: {}", last_error(self.ctx)));
            }

            let outputs = ffi::nix_locked_flake_get_output_attrs(
                self.ctx,
                self.flake_settings,
                self.state,
                locked,
            );
            ffi::nix_locked_flake_free(locked);
            if outputs.is_null() {
                return Err(format!("flake output attrs: {}", last_error(self.ctx)));
            }
            Ok(outputs)
        }
    }

    /// Force `val` and fetch a named attribute; the caller must know `val` is an
    /// attrset (force it first). Returns the (unforced) child value.
    unsafe fn attr(
        &self,
        val: *mut ffi::nix_value,
        name: &str,
    ) -> Result<*mut ffi::nix_value, EvalError> {
        let cname = CString::new(name).map_err(|_| format!("bad attr name {name:?}"))?;
        let child = unsafe { ffi::nix_get_attr_byname(self.ctx, val, self.state, cname.as_ptr()) };
        if child.is_null() {
            return Err(format!("attribute '{name}' not found"));
        }
        Ok(child)
    }

    unsafe fn has_attr(&self, val: *mut ffi::nix_value, name: &str) -> bool {
        match CString::new(name) {
            Ok(c) => unsafe { ffi::nix_has_attr_byname(self.ctx, val, self.state, c.as_ptr()) },
            Err(_) => false,
        }
    }

    unsafe fn force(&self, val: *mut ffi::nix_value) -> Result<(), EvalError> {
        if unsafe { ffi::nix_value_force(self.ctx, self.state, val) } != ffi::nix_err_NIX_OK {
            return Err(format!("force: {}", unsafe { last_error(self.ctx) }));
        }
        Ok(())
    }

    unsafe fn read_string(&self, val: *mut ffi::nix_value) -> Result<String, EvalError> {
        let mut buf: Vec<u8> = Vec::new();
        let err = unsafe {
            ffi::nix_get_string(
                self.ctx,
                val,
                Some(collect_cb),
                (&mut buf as *mut Vec<u8>).cast(),
            )
        };
        if err != ffi::nix_err_NIX_OK {
            return Err(format!("get_string: {}", unsafe { last_error(self.ctx) }));
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
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
    unsafe fn value_to_json(&self, val: *mut ffi::nix_value) -> Result<Json, EvalError> {
        unsafe { self.value_to_json_at(val, 0, &mut Vec::new()) }
    }

    unsafe fn value_to_json_at(
        &self,
        val: *mut ffi::nix_value,
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
        unsafe { self.force(val)? };
        let t = unsafe { ffi::nix_get_type(self.ctx, val) };
        match t {
            ffi::ValueType_NIX_TYPE_NULL => Ok(Json::Null),
            ffi::ValueType_NIX_TYPE_BOOL => {
                Ok(Json::Bool(unsafe { ffi::nix_get_bool(self.ctx, val) }))
            }
            ffi::ValueType_NIX_TYPE_INT => {
                Ok(Json::from(unsafe { ffi::nix_get_int(self.ctx, val) }))
            }
            ffi::ValueType_NIX_TYPE_FLOAT => {
                Ok(Json::from(unsafe { ffi::nix_get_float(self.ctx, val) }))
            }
            ffi::ValueType_NIX_TYPE_STRING => Ok(Json::String(unsafe { self.read_string(val)? })),
            ffi::ValueType_NIX_TYPE_PATH => {
                let p = unsafe { ffi::nix_get_path_string(self.ctx, val) };
                if p.is_null() {
                    return Err(format!("get_path_string: {}", unsafe {
                        last_error(self.ctx)
                    }));
                }
                Ok(Json::String(
                    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned(),
                ))
            }
            ffi::ValueType_NIX_TYPE_ATTRS => {
                // Replicate nix's `printValueAsJSON` attrset coercion, so that
                // derivations and `__toString` sets serialize the way
                // `nix eval --json` does (their string / store path) rather than
                // recursing into their frequently self-referential structure — a
                // derivation's `.all` output list contains the derivation itself,
                // which is the cycle that overflowed the walk. nix tries
                // `__toString` first, then falls back to `outPath`.
                if unsafe { self.has_attr(val, "__toString") } {
                    let f = unsafe { self.attr(val, "__toString")? };
                    let out = unsafe { ffi::nix_alloc_value(self.ctx, self.state) };
                    if out.is_null() {
                        return Err("nix_alloc_value returned null".to_string());
                    }
                    if unsafe { ffi::nix_value_call(self.ctx, self.state, f, val, out) }
                        != ffi::nix_err_NIX_OK
                    {
                        return Err(format!("__toString call: {}", unsafe {
                            last_error(self.ctx)
                        }));
                    }
                    return unsafe { self.value_to_json_at(out, depth + 1, path) };
                }
                if unsafe { self.has_attr(val, "outPath") } {
                    let op = unsafe { self.attr(val, "outPath")? };
                    return unsafe { self.value_to_json_at(op, depth + 1, path) };
                }
                let n = unsafe { ffi::nix_get_attrs_size(self.ctx, val) };
                // serde_json's default Map is key-sorted on serialization, and
                // nix_get_attr_byidx already yields symbol-sorted order — both
                // agree with `nix eval --json`.
                let mut map = serde_json::Map::new();
                for i in 0..n {
                    let mut name_ptr: *const c_char = ptr::null();
                    let child = unsafe {
                        ffi::nix_get_attr_byidx(self.ctx, val, self.state, i, &mut name_ptr)
                    };
                    if child.is_null() || name_ptr.is_null() {
                        return Err(format!("attr #{i} invalid"));
                    }
                    let key = unsafe { CStr::from_ptr(name_ptr) }
                        .to_string_lossy()
                        .into_owned();
                    path.push(key.clone());
                    let child_json = unsafe { self.value_to_json_at(child, depth + 1, path) };
                    path.pop();
                    map.insert(key, child_json?);
                }
                Ok(Json::Object(map))
            }
            ffi::ValueType_NIX_TYPE_LIST => {
                let n = unsafe { ffi::nix_get_list_size(self.ctx, val) };
                let mut arr = Vec::with_capacity(n as usize);
                for i in 0..n {
                    let child = unsafe { ffi::nix_get_list_byidx(self.ctx, val, self.state, i) };
                    if child.is_null() {
                        return Err(format!("list #{i} invalid"));
                    }
                    path.push(format!("[{i}]"));
                    let child_json = unsafe { self.value_to_json_at(child, depth + 1, path) };
                    path.pop();
                    arr.push(child_json?);
                }
                Ok(Json::Array(arr))
            }
            other => Err(format!(
                "cannot serialize nix value of type tag {other} to JSON"
            )),
        }
    }

    /// `<flake>#mandala`, deeply forced to JSON — parity with
    /// `nix eval --no-warn-dirty --json <flake>#mandala`.
    pub fn aggregate(&mut self, flake: &str) -> Result<Json, EvalError> {
        let outputs = self.flake_outputs(flake)?;
        unsafe {
            self.force(outputs)?;
            let m = self.attr(outputs, "mandala")?;
            self.value_to_json(m)
        }
    }

    /// `<flake>#nixosConfigurations.<member>.config.system.build.toplevel.outPath`.
    /// Returns `None` if the member has no `nixosConfigurations` entry.
    pub fn host_toplevel(
        &mut self,
        flake: &str,
        member: &str,
    ) -> Result<Option<String>, EvalError> {
        if !member
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
        {
            return Err(format!("refusing invalid member name {member:?}"));
        }
        let outputs = self.flake_outputs(flake)?;
        unsafe {
            self.force(outputs)?;
            let cfgs = self.attr(outputs, "nixosConfigurations")?;
            self.force(cfgs)?;
            let host = match self.attr(cfgs, member) {
                Ok(h) => h,
                Err(_) => return Ok(None),
            };
            let mut cur = host;
            for key in ["config", "system", "build", "toplevel", "outPath"] {
                self.force(cur)?;
                cur = self.attr(cur, key)?;
            }
            self.force(cur)?;
            Ok(Some(self.read_string(cur)?))
        }
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

    /// Drop the warm outputs cache so a moved contract is re-locked and
    /// re-evaluated on the next request (stale-state discipline).
    pub fn reload(&mut self) {
        let ctx = self.ctx;
        for (_, v) in std::mem::take(&mut self.cache) {
            unsafe { ffi::nix_gc_decref(ctx, v.cast()) };
        }
    }
}
