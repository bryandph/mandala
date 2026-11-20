//! A stalled accept task must not release leadership ahead of actual closure.
mod common;
use mandala_context::{Acquired, ContextIdentity, HostConfig, election};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

fn config() -> HostConfig {
    let (events, _) = broadcast::channel(8);
    HostConfig::new(
        Arc::new(|_, _, _| Box::pin(async { Ok(serde_json::Value::Null) })),
        events,
    )
}

#[tokio::test]
async fn dropped_host_retains_guard_until_its_unscheduled_listener_owner_exits() {
    common::refusal_preflight();
    let root = std::env::temp_dir().join(format!("context-order-{}", std::process::id()));
    std::fs::create_dir_all(root.join("flake")).unwrap();
    let state = root.join("state");
    let options = common::options(&state);
    let identity = ContextIdentity::with_port_range(root.join("flake"), 29916, 4).unwrap();
    let Acquired::Leader(host) = common::acquire(&identity, &state, "first", config)
        .await
        .unwrap()
    else {
        panic!("leader");
    };
    let port = host.port();
    drop(host);
    // Current-thread runtime cannot run teardown between these statements.
    assert!(
        election::try_acquire(&identity, &options)
            .unwrap()
            .is_none()
    );
    assert!(
        tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .is_err(),
        "listener still actually held"
    );
    tokio::task::yield_now().await;
    let Acquired::Leader(host) = common::acquire(&identity, &state, "replacement", config)
        .await
        .unwrap()
    else {
        panic!("replacement");
    };
    assert_eq!(
        host.port(),
        port,
        "listener closed before replacement acquired"
    );
    host.shutdown(Duration::ZERO).await;
    std::fs::remove_dir_all(root).unwrap();
}
