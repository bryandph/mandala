//! Isolated context-test environment; never relax production busy classification.
#![allow(dead_code)]
use mandala_context::{
    AcquireError, AcquireOptions, Acquired, ContextIdentity, ContextSession, HostConfig,
    HostConfigFactory,
};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
mod refusal;
pub use refusal::refusal_preflight;

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
