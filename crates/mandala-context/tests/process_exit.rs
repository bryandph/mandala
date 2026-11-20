//! Process-exit failover with in-flight reads and mutations, isolated roots.
mod common;
use mandala_context::{
    Acquired, CallError, ContextClient, ContextIdentity, FleetContext, HostConfig, discovery,
};
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::broadcast;

struct ParkedChild(i32);
impl Drop for ParkedChild {
    fn drop(&mut self) {
        unsafe {
            nix::libc::kill(self.0, nix::libc::SIGKILL);
        }
    }
}

#[test]
fn endpoint_process() {
    let Some(root) = std::env::var_os("MANDALA_ENDPOINT_FIXTURE") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let port = std::env::var("MANDALA_ENDPOINT_PORT")
        .ok()
        .map(|port| port.parse().unwrap())
        .unwrap_or(29920);
    let identity = ContextIdentity::with_port_range(root.join("flake"), port, 4).unwrap();
    let state = root.join("state");
    let options = common::options(&state);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (events, _) = broadcast::channel(8);
        let config = HostConfig::new(
            Arc::new(|_, _, _| {
                Box::pin(async {
                    println!("ENTERED");
                    std::io::stdout().flush().unwrap();
                    std::future::pending().await
                })
            }),
            events,
        );
        let Acquired::Leader(_host) =
            mandala_context::acquire_with_options(&identity, &state, "original", options, || {
                config
            })
            .await
            .unwrap()
        else {
            panic!("leader");
        };
        if let Ok(mode) = std::env::var("MANDALA_ENDPOINT_FORK") {
            use nix::libc;
            let failed = mode == "failed-exec";
            let mut ready = [0; 2];
            assert_eq!(unsafe { libc::pipe(ready.as_mut_ptr()) }, 0);
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0);
            if pid == 0 {
                unsafe {
                    libc::close(ready[0]);
                    libc::signal(libc::SIGHUP, libc::SIG_IGN);
                    if failed {
                        libc::execl(
                            c"/does-not-exist/context-test".as_ptr(),
                            c"context-test".as_ptr(),
                            std::ptr::null::<libc::c_char>(),
                        );
                    }
                    libc::write(ready[1], b"!".as_ptr().cast(), 1);
                    libc::close(ready[1]);
                    loop {
                        libc::pause();
                    }
                }
            }
            unsafe {
                libc::close(ready[1]);
            }
            let mut ack = [0_u8];
            assert_eq!(
                unsafe { libc::read(ready[0], ack.as_mut_ptr().cast(), 1) },
                1
            );
            unsafe {
                libc::close(ready[0]);
            }
            println!("FORK {pid}");
        }
        println!("READY");
        std::io::stdout().flush().unwrap();
        std::future::pending::<()>().await;
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serving_parent_exit_allows_replacement_while_preexec_child_remains_alive() {
    use nix::libc;
    common::refusal_preflight();
    for mode in ["fork", "failed-exec"] {
        let root =
            std::env::temp_dir().join(format!("context-parent-{mode}-{}", std::process::id()));
        std::fs::create_dir_all(root.join("flake")).unwrap();
        let state = root.join("state");
        let identity = ContextIdentity::with_port_range(root.join("flake"), 29944, 4).unwrap();
        let mut parent = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "endpoint_process", "--nocapture"])
            .env("MANDALA_ENDPOINT_FIXTURE", &root)
            .env("MANDALA_ENDPOINT_PORT", "29944")
            .env("MANDALA_ENDPOINT_FORK", mode)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(parent.stdout.take().unwrap());
        let mut fork = None;
        loop {
            let mut line = String::new();
            assert!(output.read_line(&mut line).unwrap() > 0);
            if let Some(pid) = line.strip_prefix("FORK ") {
                fork = Some(pid.trim().parse::<i32>().unwrap());
            }
            if line.trim() == "READY" {
                break;
            }
        }
        let fork = fork.unwrap();
        let _cleanup_child = ParkedChild(fork);
        let Acquired::Follower(client) = common::acquire(&identity, &state, "before-exit", || {
            panic!("parent excludes factory")
        })
        .await
        .unwrap() else {
            panic!("live parent must serve");
        };
        assert!(client.ping().await);
        parent.kill().unwrap();
        parent.wait().unwrap();
        assert_eq!(
            unsafe { libc::kill(fork, 0) },
            0,
            "child held before successful exec"
        );
        let (events, _) = broadcast::channel(8);
        let Acquired::Leader(host) = common::acquire(&identity, &state, "replacement", || {
            HostConfig::new(
                Arc::new(|_, _, _| Box::pin(async { Ok(json!("replacement")) })),
                events,
            )
        })
        .await
        .unwrap() else {
            panic!("replacement leads");
        };
        let d = discovery::read(&state, identity.key()).unwrap();
        let client =
            ContextClient::connect(d.addr().unwrap(), &d.token, "observer", identity.flake())
                .await
                .unwrap();
        assert_eq!(
            client.call("read", serde_json::Map::new()).await.unwrap(),
            json!("replacement")
        );
        assert_eq!(
            unsafe { libc::kill(fork, 0) },
            0,
            "child still alive while replacement serves"
        );
        unsafe {
            libc::kill(fork, libc::SIGKILL);
        }
        host.shutdown(Duration::ZERO).await;
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn process_exit_promotes_once_and_retries_only_reads() {
    common::refusal_preflight();
    let root = std::env::temp_dir().join(format!(
        "context-process-exit-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("flake")).unwrap();
    let state = root.join("state");
    let identity = ContextIdentity::with_port_range(root.join("flake"), 29920, 4).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "endpoint_process", "--nocapture"])
        .env("MANDALA_ENDPOINT_FIXTURE", &root)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert!(output.read_line(&mut line).unwrap() > 0);
        if line.trim() == "READY" {
            break;
        }
    }
    let factories = Arc::new(AtomicUsize::new(0));
    let retries = Arc::new(AtomicUsize::new(0));
    let mut sessions = Vec::new();
    for i in 0..32 {
        let factories = factories.clone();
        let retries = retries.clone();
        sessions.push(
            common::session_acquire(
                identity.clone(),
                &state,
                format!("candidate-{i}"),
                Arc::new(move || {
                    factories.fetch_add(1, Ordering::SeqCst);
                    let retries = retries.clone();
                    let (events, _) = broadcast::channel(8);
                    HostConfig::new(
                        Arc::new(move |_, _, _| {
                            let retries = retries.clone();
                            Box::pin(async move {
                                retries.fetch_add(1, Ordering::SeqCst);
                                Ok(json!("replacement"))
                            })
                        }),
                        events,
                    )
                }),
            )
            .await
            .unwrap(),
        );
    }
    let mut calls = Vec::new();
    for (i, session) in sessions.iter().enumerate() {
        let session = session.clone();
        calls.push(tokio::spawn(async move {
            (
                i % 2 == 0,
                session
                    .call("in-flight", serde_json::Map::new(), i % 2 == 0)
                    .await,
            )
        }));
    }
    for _ in 0..32 {
        loop {
            let mut line = String::new();
            assert!(output.read_line(&mut line).unwrap() > 0);
            if line.trim() == "ENTERED" {
                break;
            }
        }
    }
    child.kill().unwrap();
    child.wait().unwrap();
    for call in calls {
        let (read, result) = tokio::time::timeout(Duration::from_secs(10), call)
            .await
            .unwrap()
            .unwrap();
        if read {
            assert_eq!(result.unwrap(), json!("replacement"));
        } else {
            assert!(matches!(
                result,
                Err(CallError::Failover { retried: false, .. })
            ));
        }
    }
    assert_eq!(factories.load(Ordering::SeqCst), 1);
    assert_eq!(
        retries.load(Ordering::SeqCst),
        16,
        "one retry per read, no mutation replay"
    );
    let d = discovery::read(&state, identity.key()).unwrap();
    let mut endpoints = 0;
    for port in identity.ports() {
        if let Ok(client) = ContextClient::connect(
            (std::net::Ipv4Addr::LOCALHOST, port).into(),
            &d.token,
            "observer",
            identity.flake(),
        )
        .await
        {
            endpoints += 1;
            assert_eq!(
                client.call("read", serde_json::Map::new()).await.unwrap(),
                json!("replacement")
            );
        }
    }
    assert_eq!(endpoints, 1);
    for session in sessions {
        session.shutdown(Duration::ZERO).await;
    }
    std::fs::remove_dir_all(root).unwrap();
}
