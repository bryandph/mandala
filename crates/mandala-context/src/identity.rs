//! Context identity: canonical flake path → discovery key + a deterministic
//! loopback port sequence.
//!
//! Leadership is scoped per canonical flake path (fleet-context spec), so the
//! identity starts from `std::fs::canonicalize` — two worktrees of the same
//! repo canonicalize to different paths and get independent contexts, while
//! `.` and an absolute path to the same checkout converge on one.
//!
//! The derived port is a *stable starting point*, not an address of record:
//! the FNV-1a hash of the canonical path picks one port inside
//! [`PORT_BASE`]`..+`[`PORT_RANGE`], and [`ContextIdentity::ports`] walks the
//! whole range from there (wrapping), so a collision — another context, or a
//! foreign service, already on the derived port — deterministically probes
//! the next port and every candidate for the same flake walks the same
//! sequence. The *actual* bound port is recorded in the discovery file, which
//! is what clients trust (`crate::discovery`).

use std::io;
use std::path::Path;

/// First port of the coordination range. Chosen below the Linux ephemeral
/// floor (32768) and far from common service ports, so a derived port is
/// rarely squatted; the walk handles it when it is.
pub const PORT_BASE: u16 = 27155;

/// Number of ports in the coordination range (the walk's wrap modulus).
pub const PORT_RANGE: u16 = 256;

/// One context's identity: the canonical flake path, its discovery-file key,
/// and the deterministic port sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextIdentity {
    flake: String,
    key: String,
    hash: u64,
    base: u16,
    range: u16,
}

impl ContextIdentity {
    /// Identity for a flake checkout, with the production port range.
    ///
    /// # Errors
    /// Canonicalization fails (the path must exist — a context is only ever
    /// scoped to a locally available checkout).
    pub fn for_flake(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::with_port_range(path, PORT_BASE, PORT_RANGE)
    }

    /// Identity with an explicit port range — the tests' seam for forcing
    /// small ranges and deliberate collisions; production callers use
    /// [`ContextIdentity::for_flake`].
    ///
    /// # Errors
    /// Canonicalization fails.
    ///
    /// # Panics
    /// `range` is zero (a context must have at least one candidate port).
    pub fn with_port_range(path: impl AsRef<Path>, base: u16, range: u16) -> io::Result<Self> {
        assert!(range > 0, "port range must be non-empty");
        let canonical = std::fs::canonicalize(path.as_ref())?;
        let flake = canonical.to_string_lossy().into_owned();
        let hash = fnv1a64(flake.as_bytes());
        let stem = canonical
            .file_name()
            .map(|n| sanitize(&n.to_string_lossy()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "flake".to_string());
        Ok(Self {
            key: format!("{stem}-{hash:016x}"),
            flake,
            hash,
            base,
            range,
        })
    }

    /// The canonical flake path (the leadership scope, verbatim in hello /
    /// welcome / discovery frames).
    #[must_use]
    pub fn flake(&self) -> &str {
        &self.flake
    }

    /// The discovery-file key: `<sanitized stem>-<hash16>` — filesystem-safe,
    /// collision-free per canonical path, human-greppable.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The stable starting port derived from the canonical path.
    #[must_use]
    pub fn derived_port(&self) -> u16 {
        self.base + port_offset(self.hash, self.range)
    }

    /// The full deterministic probe sequence: the derived port first, then
    /// each next port wrapping within the range. Every candidate for the same
    /// flake walks this same order — that is what makes collision handling
    /// converge.
    pub fn ports(&self) -> impl Iterator<Item = u16> + '_ {
        let offset = port_offset(self.hash, self.range);
        (0..self.range).map(move |i| self.base + ((offset + i) % self.range))
    }
}

/// The hash's offset into the port range.
fn port_offset(hash: u64, range: u16) -> u16 {
    #[allow(clippy::cast_possible_truncation)]
    let offset = (hash % u64::from(range)) as u16;
    offset
}

/// FNV-1a, 64-bit: tiny, dependency-free, and *stable across processes and
/// releases* — which `DefaultHasher` does not promise. Not cryptographic;
/// only port dispersion and file naming ride on it.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Keep a path stem filesystem/log-safe: alphanumerics, `-`, `_`, `.` pass;
/// anything else becomes `-`.
fn sanitize(stem: &str) -> String {
    stem.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mandala-context-id-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn identity_is_deterministic_and_canonical() {
        let dir = tmp_dir("det");
        let a = ContextIdentity::for_flake(&dir).unwrap();
        let b = ContextIdentity::for_flake(&dir).unwrap();
        assert_eq!(a, b, "same path, same identity — across constructions");
        assert!(a.derived_port() >= PORT_BASE);
        assert!(a.derived_port() < PORT_BASE + PORT_RANGE);
        // A relative spelling of the same checkout converges on the same
        // identity (canonicalization is the scope).
        let cwd = std::env::current_dir().unwrap();
        if let Ok(rel) = dir.strip_prefix(&cwd) {
            let c = ContextIdentity::for_flake(rel).unwrap();
            assert_eq!(a, c);
        }
    }

    #[test]
    fn port_sequence_starts_derived_and_covers_the_range_once() {
        let dir = tmp_dir("seq");
        let id = ContextIdentity::with_port_range(&dir, 28000, 8).unwrap();
        let ports: Vec<u16> = id.ports().collect();
        assert_eq!(ports.len(), 8);
        assert_eq!(ports[0], id.derived_port());
        let mut sorted = ports.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 8, "every port exactly once: {ports:?}");
        assert!(ports.iter().all(|p| (28000..28008).contains(p)));
    }

    #[test]
    fn key_is_filesystem_safe_and_stem_tagged() {
        let dir = tmp_dir("key with spaces!");
        let id = ContextIdentity::for_flake(&dir).unwrap();
        assert!(
            id.key()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
            "unsafe key: {}",
            id.key()
        );
        assert!(id.key().contains("key-with-spaces"), "key: {}", id.key());
    }

    #[test]
    fn missing_path_is_refused() {
        assert!(ContextIdentity::for_flake("/nonexistent/mandala/nowhere").is_err());
    }
}
