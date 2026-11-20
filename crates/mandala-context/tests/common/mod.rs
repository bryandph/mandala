//! Isolated context-test environment; never relax production busy classification.
#![allow(dead_code)]
use mandala_context::{
    AcquireError, AcquireOptions, Acquired, ContextIdentity, ContextSession, HostConfig,
    HostConfigFactory,
};
use std::net::{Ipv4Addr, TcpStream};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Duration;

/// Test fixture provisioning only: injected roots are made by tests, never acquisition.
pub fn options(state: &Path) -> AcquireOptions {
    refusal_preflight();
    let root = state.join("context-election-test");
    std::fs::create_dir_all(state).unwrap();
    match std::fs::DirBuilder::new().mode(0o700).create(&root) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => panic!("create test coordination fixture: {e}"),
    }
    AcquireOptions {
        coordination_root: Some(std::fs::canonicalize(root).unwrap()),
        ..AcquireOptions::default()
    }
}

pub async fn acquire(
    identity: &ContextIdentity,
    state: &Path,
    name: &str,
    config: impl FnOnce() -> HostConfig,
) -> Result<Acquired, AcquireError> {
    mandala_context::acquire_with_options(identity, state, name, options(state), config).await
}

pub async fn session_acquire(
    identity: ContextIdentity,
    state: impl Into<PathBuf>,
    name: impl Into<String>,
    config: HostConfigFactory,
) -> Result<ContextSession, AcquireError> {
    let state = state.into();
    let options = options(&state);
    ContextSession::acquire_with_options(identity, state, name, config, options).await
}

/// LAB-206: a dead endpoint must refuse, not silently blackhole connects.
/// Run once per integration-test process, before any election assertion.
pub fn refusal_preflight() {
    static CHECK: Once = Once::new();
    CHECK.call_once(|| {
        for _ in 0..3 {
            // Bind without LISTEN and retain the reservation during probing.
            // Dropping a temporary listener permits port reuse, and a forked
            // pre-exec child can briefly retain its inherited LISTEN socket.
            let reserved = tokio::net::TcpSocket::new_v4().expect("reserve test socket");
            reserved.bind((Ipv4Addr::LOCALHOST, 0).into()).expect("reserve non-listening test port");
            let addr = reserved.local_addr().unwrap();
            let result = TcpStream::connect_timeout(&addr, Duration::from_millis(500));
            assert!(
                matches!(&result, Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused),
                "LAB-206 loopback-refusal preflight: unused {addr} must refuse, not timeout: {result:?}. On WSL run an immutable prebuilt test binary in `unshare --user --map-root-user --net` with `ip link set lo up`; use isolated HOME/state/runtime and injected coordination roots. Production busy-leader handling is unchanged."
            );
        }
    });
}
