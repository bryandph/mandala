//! The per-context discovery file: metadata only, never a lock.
//!
//! `<state_dir>/mcp/contexts/<key>.json`, mode 0600, exactly
//! `{flake, pid, token, url}` (fleet-context spec). The *bind* is the lock;
//! this file tells clients where the live endpoint is (`url` records the
//! actually-bound port — authoritative over the derived port) and with which
//! bearer token to hello. Liveness is judged only by connecting: the recorded
//! `pid` is advisory (a recycled pid must neither fake liveness nor block a
//! claim), and a stale file is simply overwritten by the next leader.
//!
//! Byte format: the shared 1-space sorted-keys writer
//! ([`mandala_core::drift::to_pretty_1space`] over a `BTreeMap`), the same
//! `json.dumps(indent=1, sort_keys=True)` discipline as `meta.json` and
//! `.expected.json` (fleet-state-formats).

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use mandala_core::drift::to_pretty_1space;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One context's published coordination metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discovery {
    /// The endpoint address of record, `tcp://127.0.0.1:<bound port>`.
    pub url: String,
    /// The context's bearer token — minted at first claim, *reused* by every
    /// later leader of the same context so it stays stable across restarts.
    pub token: String,
    /// The leader's pid — advisory only, never a liveness judgement.
    pub pid: u32,
    /// The canonical flake path this context is scoped to.
    pub flake: String,
}

impl Discovery {
    /// The endpoint as a socket address, `None` if the url is malformed.
    #[must_use]
    pub fn addr(&self) -> Option<SocketAddr> {
        self.url.strip_prefix("tcp://")?.parse().ok()
    }
}

/// The contexts directory under a mandala state dir.
#[must_use]
pub fn contexts_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("mcp").join("contexts")
}

/// One context's discovery-file path.
#[must_use]
pub fn discovery_path(state_dir: &Path, key: &str) -> PathBuf {
    contexts_dir(state_dir).join(format!("{key}.json"))
}

/// Read a context's discovery file. `None` on absence or any parse failure —
/// an unreadable file is treated exactly like a stale one (the claim path
/// rewrites it; no manual cleanup).
#[must_use]
pub fn read(state_dir: &Path, key: &str) -> Option<Discovery> {
    let text = std::fs::read_to_string(discovery_path(state_dir, key)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write a context's discovery file: 0600 from creation (the token is a
/// bearer secret), tmp-then-rename so readers never see a torn write.
///
/// # Errors
/// Any filesystem error creating the directory or writing the file.
pub fn write(state_dir: &Path, key: &str, discovery: &Discovery) -> io::Result<()> {
    let dir = contexts_dir(state_dir);
    std::fs::create_dir_all(&dir)?;
    // Sorted keys regardless of serde_json's `preserve_order` feature state
    // (the drift/registry writers' same construction).
    let mut root: BTreeMap<String, Value> = BTreeMap::new();
    root.insert("url".to_string(), Value::from(discovery.url.clone()));
    root.insert("token".to_string(), Value::from(discovery.token.clone()));
    root.insert("pid".to_string(), Value::from(discovery.pid));
    root.insert("flake".to_string(), Value::from(discovery.flake.clone()));
    let bytes = to_pretty_1space(&root).map_err(io::Error::other)?;

    let tmp = dir.join(format!("{key}.json.tmp"));
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut fh = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        fh.write_all(&bytes)?;
        fh.flush()?;
    }
    std::fs::rename(&tmp, discovery_path(state_dir, key))
}

/// Mint a fresh bearer token: 32 bytes of OS randomness, hex-encoded.
///
/// # Panics
/// The OS randomness source is unavailable (nothing sane to fall back to for
/// a credential).
#[must_use]
pub fn mint_token() -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("OS randomness unavailable");
    buf.iter().fold(String::with_capacity(64), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mandala-context-disc-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample() -> Discovery {
        Discovery {
            url: "tcp://127.0.0.1:27160".to_string(),
            token: "feedbeef".to_string(),
            pid: 4242,
            flake: "/tmp/checkout".to_string(),
        }
    }

    #[test]
    fn roundtrip_mode_0600_and_byte_format() {
        let dir = scratch("rt");
        write(&dir, "demo-abc", &sample()).unwrap();

        let path = discovery_path(&dir, "demo-abc");
        // 0600: readable only by the operator (the token is a bearer secret).
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "discovery must be private, got {mode:o}");

        // The exact byte contract: 1-space indent, sorted keys, no trailing
        // newline — `json.dumps(indent=1, sort_keys=True)`.
        let bytes = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            bytes,
            "{\n \"flake\": \"/tmp/checkout\",\n \"pid\": 4242,\n \"token\": \"feedbeef\",\n \"url\": \"tcp://127.0.0.1:27160\"\n}"
        );

        assert_eq!(read(&dir, "demo-abc"), Some(sample()));
        assert_eq!(
            sample().addr(),
            Some("127.0.0.1:27160".parse().unwrap())
        );
    }

    #[test]
    fn absent_or_garbage_reads_as_none() {
        let dir = scratch("bad");
        assert_eq!(read(&dir, "missing"), None);
        std::fs::create_dir_all(contexts_dir(&dir)).unwrap();
        std::fs::write(discovery_path(&dir, "torn"), "{not json").unwrap();
        assert_eq!(read(&dir, "torn"), None, "garbage == stale, never an error");
    }

    #[test]
    fn tokens_are_64_hex_and_unique() {
        let a = mint_token();
        let b = mint_token();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
