//! Lease teardown observes actual future destruction, not abort notification.
mod common;
use mandala_context::{
    AcquireOptions, Acquired, ContextIdentity, ContextSession, FleetContext, HostConfig,
    acquire_with_options, election,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::{Notify, broadcast};

fn fixture(tag: &str, port: u16) -> (PathBuf, ContextIdentity, PathBuf, AcquireOptions) {
    common::refusal_preflight();
    let root = std::env::temp_dir().join(format!(
        "context-lease-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("flake")).unwrap();
    let state = root.join("state");
    let options = common::options(&state);
    let identity = ContextIdentity::with_port_range(root.join("flake"), port, 4).unwrap();
    (root, identity, state, options)
}
fn config() -> HostConfig {
    let (events, _) = broadcast::channel(8);
    HostConfig::new(
        Arc::new(|_, _, _| Box::pin(async { Ok(json!("replacement")) })),
        events,
    )
}
struct Live(Arc<AtomicUsize>);
impl Drop for Live {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
fn blocked(live: Arc<AtomicUsize>, entered: Arc<Notify>) -> HostConfig {
    let (events, _) = broadcast::channel(8);
    HostConfig::new(
        Arc::new(move |_, _, _| {
            let live = live.clone();
            let entered = entered.clone();
            Box::pin(async move {
                live.fetch_add(1, Ordering::SeqCst);
                let _live = Live(live);
                entered.notify_one();
                std::future::pending().await
            })
        }),
        events,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wire_drop_terminates_calls_and_background_before_new_factory() {
    let (root, identity, state, options) = fixture("wire", 29900);
    let live = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let background_live = Arc::new(AtomicUsize::new(0));
    let background_entered = Arc::new(Notify::new());
    let cfg = blocked(live.clone(), entered.clone());
    let background_count = background_live.clone();
    let background_ready = background_entered.clone();
    let Acquired::Leader(host) =
        acquire_with_options(&identity, &state, "first", options.clone(), || {
            assert!(
                election::try_acquire(&identity, &options)
                    .unwrap()
                    .is_none(),
                "factory owns both guards"
            );
            cfg.background_tasks
                .lock()
                .unwrap()
                .push(tokio::spawn(async move {
                    background_count.fetch_add(1, Ordering::SeqCst);
                    let _live = Live(background_count);
                    background_ready.notify_one();
                    std::future::pending::<()>().await;
                }));
            cfg
        })
        .await
        .unwrap()
    else {
        panic!("leader");
    };
    background_entered.notified().await;
    let follower = common::acquire(&identity, &state, "wire", || panic!("follower factory"))
        .await
        .unwrap();
    let Acquired::Follower(client) = follower else {
        panic!("follower");
    };
    let call = tokio::spawn(async move { client.call("blocked", serde_json::Map::new()).await });
    entered.notified().await;
    let inode = std::fs::metadata(
        options
            .coordination_root
            .as_ref()
            .unwrap()
            .join(format!("{}.lock", identity.key())),
    )
    .unwrap();
    drop(host);
    let replacement =
        acquire_with_options(&identity, &state, "replacement", options.clone(), || {
            assert_eq!(
                live.load(Ordering::SeqCst),
                0,
                "wire future actually terminated"
            );
            assert_eq!(
                background_live.load(Ordering::SeqCst),
                0,
                "watcher future actually terminated"
            );
            config()
        })
        .await
        .unwrap();
    assert!(call.await.unwrap().is_err());
    let Acquired::Leader(host) = replacement else {
        panic!("replacement");
    };
    host.shutdown(Duration::ZERO).await;
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        inode.ino(),
        std::fs::metadata(
            options
                .coordination_root
                .as_ref()
                .unwrap()
                .join(format!("{}.lock", identity.key()))
        )
        .unwrap()
        .ino()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_call_is_terminated_after_bounded_drain() {
    let (root, identity, state, options) = fixture("local", 29904);
    let live = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let count = live.clone();
    let ready = entered.clone();
    let session = ContextSession::acquire_with_options(
        identity.clone(),
        &state,
        "local",
        Arc::new(move || blocked(count.clone(), ready.clone())),
        options.clone(),
    )
    .await
    .unwrap();
    let caller = session.clone();
    let call =
        tokio::spawn(async move { caller.call("blocked", serde_json::Map::new(), false).await });
    entered.notified().await;
    session.shutdown(Duration::from_millis(20)).await;
    assert_eq!(live.load(Ordering::SeqCst), 0);
    assert!(call.await.unwrap().is_err());
    assert!(
        election::try_acquire(&identity, &options)
            .unwrap()
            .is_some()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn factory_panic_and_publication_error_release_only_after_listener_close() {
    let (root, identity, state, options) = fixture("errors", 29908);
    let id = identity.clone();
    let opts = options.clone();
    let dir = state.clone();
    let panic = tokio::spawn(async move {
        acquire_with_options(&id, &dir, "panic", opts, || {
            panic!("fixture factory failure")
        })
        .await
    });
    assert!(matches!(panic.await, Err(e) if e.is_panic()));
    assert!(
        election::try_acquire(&identity, &options)
            .unwrap()
            .is_some()
    );
    let bad_state = root.join("not-directory");
    std::fs::write(&bad_state, []).unwrap();
    assert!(
        acquire_with_options(
            &identity,
            &bad_state,
            "publication-error",
            options.clone(),
            config
        )
        .await
        .is_err()
    );
    let Acquired::Leader(host) =
        acquire_with_options(&identity, &state, "recovered", options, config)
            .await
            .unwrap()
    else {
        panic!("recovered");
    };
    assert_eq!(
        host.port(),
        identity.derived_port(),
        "old listener closed before replacement claimed"
    );
    host.shutdown(Duration::ZERO).await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_shutdown_still_finishes_guarded_teardown() {
    let (root, identity, state, options) = fixture("cancel-shutdown", 29932);
    let live = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let count = live.clone();
    let ready = entered.clone();
    let session = ContextSession::acquire_with_options(
        identity.clone(),
        &state,
        "local",
        Arc::new(move || blocked(count.clone(), ready.clone())),
        options.clone(),
    )
    .await
    .unwrap();
    let observer = session.clone();
    let caller = session.clone();
    let call =
        tokio::spawn(async move { caller.call("blocked", serde_json::Map::new(), false).await });
    entered.notified().await;
    let shutdown = tokio::spawn(session.shutdown(Duration::from_secs(60)));
    while observer.is_leader().await {
        tokio::task::yield_now().await;
    }
    shutdown.abort();
    let _ = shutdown.await;
    let Acquired::Leader(host) =
        acquire_with_options(&identity, &state, "replacement", options, || {
            assert_eq!(live.load(Ordering::SeqCst), 0);
            config()
        })
        .await
        .unwrap()
    else {
        panic!("replacement");
    };
    assert!(call.await.unwrap().is_err());
    host.shutdown(Duration::ZERO).await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn different_discovery_roots_share_exclusion_and_fail_bounded() {
    let (root, identity, state, options) = fixture("different-state", 29928);
    let Acquired::Leader(host) =
        acquire_with_options(&identity, &state, "first", options.clone(), config)
            .await
            .unwrap()
    else {
        panic!("leader");
    };
    let factories = AtomicUsize::new(0);
    let result = tokio::time::timeout(
        Duration::from_secs(27),
        acquire_with_options(
            &identity,
            &root.join("other-state"),
            "isolated-metadata",
            options.clone(),
            || {
                factories.fetch_add(1, Ordering::SeqCst);
                config()
            },
        ),
    )
    .await
    .unwrap();
    assert!(matches!(
        result,
        Err(mandala_context::AcquireError::CoordinationBusy)
    ));
    assert_eq!(factories.load(Ordering::SeqCst), 0);
    let client = mandala_context::ContextClient::connect(
        (std::net::Ipv4Addr::LOCALHOST, host.port()).into(),
        &mandala_context::discovery::read(&state, identity.key())
            .unwrap()
            .token,
        "observer",
        identity.flake(),
    )
    .await
    .unwrap();
    assert_eq!(
        client.call("read", serde_json::Map::new()).await.unwrap(),
        json!("replacement")
    );
    host.shutdown(Duration::ZERO).await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exhausted_foreign_ports_never_invoke_factory_and_release_candidate_guard() {
    let (root, identity, state, options) = fixture("exhausted", 29936);
    let mut occupants = Vec::new();
    for port in identity.ports() {
        occupants.push(mandala_context::RunningHost::start(
            tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap(),
            mandala_context::discovery::mint_token(),
            "/foreign-fixture".into(),
            config(),
        ));
    }
    let result = acquire_with_options(&identity, &state, "candidate", options.clone(), || {
        panic!("exhausted candidate factory must not run")
    })
    .await;
    assert!(matches!(
        result,
        Err(mandala_context::AcquireError::PortsExhausted)
    ));
    assert!(
        election::try_acquire(&identity, &options)
            .unwrap()
            .is_some()
    );
    assert!(mandala_context::discovery::read(&state, identity.key()).is_none());
    for occupant in occupants {
        occupant.shutdown(Duration::ZERO).await;
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_acquisition_releases_its_guard_without_factory() {
    let (root, identity, state, mut options) = fixture("cancel", 29912);
    let listener =
        tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, identity.derived_port()))
            .await
            .unwrap();
    let attempted = Arc::new(Notify::new());
    options.election_attempt = Some(attempted.clone());
    let id = identity.clone();
    let opts = options.clone();
    let candidate = tokio::spawn(async move {
        acquire_with_options(&id, &state, "cancel", opts, || {
            panic!("must not reach factory")
        })
        .await
    });
    attempted.notified().await;
    assert!(
        election::try_acquire(&identity, &options)
            .unwrap()
            .is_none()
    );
    candidate.abort();
    let _ = candidate.await;
    assert!(
        election::try_acquire(&identity, &options)
            .unwrap()
            .is_some()
    );
    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}
