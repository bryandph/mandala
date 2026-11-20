//! Registered blocking work retains the lease; only audited snapshots may outlive it.
mod common;
use mandala_context::{AcquireOptions, Acquired, ContextIdentity, HostConfig, election, work};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::{Notify, broadcast};

#[derive(Clone)]
struct Gate(Arc<(Mutex<bool>, Condvar)>);
impl Gate {
    fn new() -> Self {
        Self(Arc::new((Mutex::new(false), Condvar::new())))
    }
    fn release(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
    fn wait(&self) {
        let mut released = self.0.0.lock().unwrap();
        while !*released {
            released = self.0.1.wait(released).unwrap();
        }
    }
}
struct Release(Gate);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}
struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn fixture(tag: &str, port: u16) -> (PathBuf, ContextIdentity, PathBuf, AcquireOptions) {
    common::refusal_preflight();
    let root = std::env::temp_dir().join(format!(
        "context-blocking-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("flake")).unwrap();
    let state = root.join("state");
    let options = common::options(&state);
    (
        root.clone(),
        ContextIdentity::with_port_range(root.join("flake"), port, 4).unwrap(),
        state,
        options,
    )
}
fn config() -> HostConfig {
    let (events, _) = broadcast::channel(8);
    HostConfig::new(
        Arc::new(|_, _, _| Box::pin(async { Ok(json!("replacement")) })),
        events,
    )
}
fn blocked_config(
    registered: bool,
    gate: Gate,
    live: Arc<AtomicUsize>,
    entered: Arc<Notify>,
    finished: Arc<Notify>,
    parent_dropped: Arc<AtomicBool>,
) -> HostConfig {
    let (events, _) = broadcast::channel(8);
    HostConfig::new(
        Arc::new(move |_, _, _| {
            let gate = gate.clone();
            let live = live.clone();
            let entered = entered.clone();
            let finished = finished.clone();
            let dropped = parent_dropped.clone();
            Box::pin(async move {
                let _dropped = Dropped(dropped);
                assert!(
                    work::current_scope().is_some(),
                    "dispatch ownership propagated"
                );
                let function = move || {
                    live.fetch_add(1, Ordering::SeqCst);
                    entered.notify_one();
                    gate.wait();
                    live.fetch_sub(1, Ordering::SeqCst);
                    finished.notify_one();
                    json!("original")
                };
                if registered {
                    work::spawn_blocking(function)
                        .await
                        .map_err(|e| e.to_string())
                } else {
                    work::read_only_snapshot(function)
                        .await
                        .map_err(|e| e.to_string())
                }
            })
        }),
        events,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn running_registered_job_finishes_before_replacement_factory() {
    let (root, identity, state, options) = fixture("running", 29956);
    let gate = Gate::new();
    let _release = Release(gate.clone());
    let live = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let finished = Arc::new(Notify::new());
    let parent_dropped = Arc::new(AtomicBool::new(false));
    let cfg = blocked_config(
        true,
        gate.clone(),
        live.clone(),
        entered.clone(),
        finished.clone(),
        parent_dropped.clone(),
    );
    let Acquired::Leader(host) = common::acquire(&identity, &state, "original", || {
        assert!(
            work::current_scope().is_some(),
            "factory ownership propagated"
        );
        cfg
    })
    .await
    .unwrap() else {
        panic!("leader");
    };
    let Acquired::Follower(client) =
        common::acquire(&identity, &state, "caller", || panic!("follower factory"))
            .await
            .unwrap()
    else {
        panic!("follower");
    };
    let call = tokio::spawn(async move { client.call("blocked", serde_json::Map::new()).await });
    entered.notified().await;
    host.shutdown(Duration::ZERO).await;
    assert!(call.await.unwrap().is_err());
    assert!(
        parent_dropped.load(Ordering::SeqCst),
        "outer async future actually destroyed"
    );
    assert_eq!(live.load(Ordering::SeqCst), 1);
    assert!(
        election::try_acquire(&identity, &options)
            .unwrap()
            .is_none(),
        "retired lease still excludes while blocking work runs"
    );
    let attempted = Arc::new(Notify::new());
    let mut candidate_options = options.clone();
    candidate_options.election_attempt = Some(attempted.clone());
    let factories = Arc::new(AtomicUsize::new(0));
    let count = factories.clone();
    let still_live = live.clone();
    let candidate_id = identity.clone();
    let candidate_state = state.clone();
    let candidate = tokio::spawn(async move {
        mandala_context::acquire_with_options(
            &candidate_id,
            &candidate_state,
            "replacement",
            candidate_options,
            || {
                count.fetch_add(1, Ordering::SeqCst);
                assert_eq!(
                    still_live.load(Ordering::SeqCst),
                    0,
                    "actual blocking work ended before factory"
                );
                config()
            },
        )
        .await
        .unwrap()
    });
    attempted.notified().await;
    assert_eq!(factories.load(Ordering::SeqCst), 0);
    assert!(!candidate.is_finished());
    gate.release();
    tokio::time::timeout(Duration::from_secs(2), finished.notified())
        .await
        .unwrap();
    let Acquired::Leader(replacement) = tokio::time::timeout(Duration::from_secs(5), candidate)
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("replacement");
    };
    assert_eq!(factories.load(Ordering::SeqCst), 1);
    replacement.shutdown(Duration::ZERO).await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn registered_job_panic_cleans_its_registration_before_unlock() {
    let (root, identity, state, options) = fixture("panic", 29972);
    let (events, _) = broadcast::channel(8);
    let cfg = HostConfig::new(
        Arc::new(|_, _, _| {
            Box::pin(async {
                work::spawn_blocking(|| -> Value { panic!("fixture registered job panic") })
                    .await
                    .map_err(|e| e.to_string())
            })
        }),
        events,
    );
    let Acquired::Leader(host) = common::acquire(&identity, &state, "original", || cfg)
        .await
        .unwrap()
    else {
        panic!("leader");
    };
    let Acquired::Follower(client) =
        common::acquire(&identity, &state, "caller", || panic!("follower factory"))
            .await
            .unwrap()
    else {
        panic!("follower");
    };
    assert!(client.call("panic", serde_json::Map::new()).await.is_err());
    host.shutdown(Duration::ZERO).await;
    assert!(
        election::try_acquire(&identity, &options)
            .unwrap()
            .is_some(),
        "panicked job did not leak registration or lease"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn runtime_cancellation_keeps_the_guard_until_running_work_ends() {
    let (root, identity, state, options) = fixture("runtime", 29968);
    let gate = Gate::new();
    let _release = Release(gate.clone());
    let live = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let finished = Arc::new(Notify::new());
    let parent_dropped = Arc::new(AtomicBool::new(false));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let cfg = blocked_config(
            true,
            gate.clone(),
            live.clone(),
            entered.clone(),
            finished,
            parent_dropped.clone(),
        );
        let Acquired::Leader(host) = common::acquire(&identity, &state, "original", || cfg)
            .await
            .unwrap()
        else {
            panic!("leader");
        };
        let Acquired::Follower(client) =
            common::acquire(&identity, &state, "caller", || panic!("follower factory"))
                .await
                .unwrap()
        else {
            panic!("follower");
        };
        tokio::spawn(async move {
            let _ = client.call("blocked", serde_json::Map::new()).await;
        });
        entered.notified().await;
        drop(host);
        // Do not give the owner an opportunity to finish normal retirement.
    });
    runtime.shutdown_timeout(Duration::ZERO);
    assert!(parent_dropped.load(Ordering::SeqCst));
    assert_eq!(live.load(Ordering::SeqCst), 1);
    assert!(
        election::try_acquire(&identity, &options)
            .unwrap()
            .is_none(),
        "job retains the descriptor/mutex independently of a cancelled runtime owner"
    );
    gate.release();
    let replacement_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    replacement_runtime.block_on(async {
        let Acquired::Leader(replacement) =
            common::acquire(&identity, &state, "replacement", || {
                assert_eq!(live.load(Ordering::SeqCst), 0);
                config()
            })
            .await
            .unwrap()
        else {
            panic!("replacement");
        };
        replacement.shutdown(Duration::ZERO).await;
    });
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn queued_registered_job_never_starts_after_teardown() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (root, identity, state, _) = fixture("queued", 29960);
        let pool_gate = Gate::new();
        let _release = Release(pool_gate.clone());
        let entered = Arc::new(Notify::new());
        let blocker_gate = pool_gate.clone();
        let blocker_entered = entered.clone();
        // Deliberately occupy the only blocking worker, outside any lease.
        let blocker = tokio::task::spawn_blocking(move || {
            blocker_entered.notify_one();
            blocker_gate.wait();
        });
        entered.notified().await;
        let started = Arc::new(AtomicUsize::new(0));
        let queued = Arc::new(Notify::new());
        let calls = started.clone();
        let queued_notice = queued.clone();
        let (events, _) = broadcast::channel(8);
        let cfg = HostConfig::new(
            Arc::new(move |_, _, _| {
                let calls = calls.clone();
                let queued = queued_notice.clone();
                Box::pin(async move {
                    let pending = work::spawn_blocking(move || {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Value::Null
                    });
                    // Registration/queueing is synchronous, not a timing guess.
                    queued.notify_one();
                    pending.await.map_err(|e| e.to_string())
                })
            }),
            events,
        );
        let Acquired::Leader(host) = common::acquire(&identity, &state, "original", || cfg)
            .await
            .unwrap()
        else {
            panic!("leader");
        };
        let Acquired::Follower(client) =
            common::acquire(&identity, &state, "caller", || panic!("follower factory"))
                .await
                .unwrap()
        else {
            panic!("follower");
        };
        let call = tokio::spawn(async move { client.call("queued", serde_json::Map::new()).await });
        queued.notified().await;
        host.shutdown(Duration::ZERO).await;
        assert!(call.await.unwrap().is_err());
        assert_eq!(started.load(Ordering::SeqCst), 0);
        pool_gate.release();
        blocker.await.unwrap();
        let Acquired::Leader(replacement) =
            common::acquire(&identity, &state, "replacement", || {
                assert_eq!(
                    started.load(Ordering::SeqCst),
                    0,
                    "cancelled queued job did not execute when the pool became available"
                );
                config()
            })
            .await
            .unwrap()
        else {
            panic!("replacement");
        };
        assert_eq!(started.load(Ordering::SeqCst), 0);
        replacement.shutdown(Duration::ZERO).await;
        std::fs::remove_dir_all(root).unwrap();
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_only_snapshot_result_is_discarded_and_does_not_block_takeover() {
    let (root, identity, state, options) = fixture("snapshot", 29964);
    let gate = Gate::new();
    let _release = Release(gate.clone());
    let live = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let finished = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let cfg = blocked_config(
        false,
        gate.clone(),
        live.clone(),
        entered.clone(),
        finished.clone(),
        dropped.clone(),
    );
    let Acquired::Leader(host) = common::acquire(&identity, &state, "original", || cfg)
        .await
        .unwrap()
    else {
        panic!("leader");
    };
    let Acquired::Follower(client) =
        common::acquire(&identity, &state, "caller", || panic!("follower factory"))
            .await
            .unwrap()
    else {
        panic!("follower");
    };
    let call = tokio::spawn(async move { client.call("snapshot", serde_json::Map::new()).await });
    entered.notified().await;
    host.shutdown(Duration::ZERO).await;
    assert!(call.await.unwrap().is_err());
    assert!(dropped.load(Ordering::SeqCst));
    let Acquired::Leader(replacement) =
        mandala_context::acquire_with_options(&identity, &state, "replacement", options, || {
            assert_eq!(
                live.load(Ordering::SeqCst),
                1,
                "approved read-only computation does not pin the lease"
            );
            config()
        })
        .await
        .unwrap()
    else {
        panic!("replacement");
    };
    gate.release();
    tokio::time::timeout(Duration::from_secs(2), finished.notified())
        .await
        .unwrap();
    replacement.shutdown(Duration::ZERO).await;
    std::fs::remove_dir_all(root).unwrap();
}
