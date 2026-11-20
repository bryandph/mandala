//! Opt-in evidence for the broader leader-owned blocking-work lifecycle gate.
//! Aborting an async dispatch does not stop an inner started spawn_blocking job.
mod common;
use mandala_context::{Acquired, HostConfig};
use serde_json::json;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::{Notify, broadcast};

struct Release(Arc<(Mutex<bool>, Condvar)>);
impl Release {
    fn release(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}
impl Drop for Release {
    fn drop(&mut self) {
        self.release();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "coordinator cancellation-scope gate: started blocking work currently outlives its async dispatch"]
async fn blocking_job_must_finish_before_a_replacement_factory_runs() {
    common::refusal_preflight();
    let root = std::env::temp_dir().join(format!(
        "context-blocking-scope-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("flake")).unwrap();
    let state = root.join("state");
    let identity =
        mandala_context::ContextIdentity::with_port_range(root.join("flake"), 29956, 4).unwrap();
    let gate = Release(Arc::new((Mutex::new(false), Condvar::new())));
    let live = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let finished = Arc::new(Notify::new());
    let (events, _) = broadcast::channel(8);
    let block_gate = gate.0.clone();
    let block_live = live.clone();
    let block_entered = entered.clone();
    let block_finished = finished.clone();
    let config = HostConfig::new(
        Arc::new(move |_, _, _| {
            let gate = block_gate.clone();
            let live = block_live.clone();
            let entered = block_entered.clone();
            let finished = block_finished.clone();
            Box::pin(async move {
                tokio::task::spawn_blocking(move || {
                    live.fetch_add(1, Ordering::SeqCst);
                    entered.notify_one();
                    let mut released = gate.0.lock().unwrap();
                    while !*released {
                        released = gate.1.wait(released).unwrap();
                    }
                    live.fetch_sub(1, Ordering::SeqCst);
                    finished.notify_one();
                    Ok(json!("original"))
                })
                .await
                .unwrap()
            })
        }),
        events,
    );
    let Acquired::Leader(host) = common::acquire(&identity, &state, "original", || config)
        .await
        .unwrap()
    else {
        panic!("original leads");
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
    drop(host);
    let mut observed = 0;
    let Acquired::Leader(replacement) = common::acquire(&identity, &state, "replacement", || {
        observed = live.load(Ordering::SeqCst);
        let (events, _) = broadcast::channel(8);
        HostConfig::new(
            Arc::new(|_, _, _| Box::pin(async { Ok(json!("replacement")) })),
            events,
        )
    })
    .await
    .unwrap() else {
        panic!("replacement leads");
    };
    assert!(
        call.await.unwrap().is_err(),
        "outer wire call actually terminated"
    );
    // Always release and finish the owned blocking fixture before asserting:
    // this is evidence, not a leaked blocking thread that hangs runtime drop.
    gate.release();
    tokio::time::timeout(Duration::from_secs(2), finished.notified())
        .await
        .unwrap();
    replacement.shutdown(Duration::ZERO).await;
    std::fs::remove_dir_all(root).unwrap();
    assert_eq!(
        observed, 0,
        "leader-owned blocking job still running when replacement factory entered"
    );
}
