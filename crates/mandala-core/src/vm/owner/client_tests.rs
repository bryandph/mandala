use super::*;
use std::process::Stdio;

fn command(script: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .args(["-c", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

#[tokio::test]
async fn vm_owner_backend_client_rejects_helper_version_before_lock() {
    let result = OwnerClient::spawn(command("read -r line; printf '%s\\n' '{\"version\":99,\"ok\":true,\"error\":null,\"journal\":null}'")).await;
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("controller 1, helper 99")
    );
}

#[tokio::test]
async fn vm_owner_backend_client_unknown_response_poison_prevents_replay() {
    let mut client = OwnerClient::spawn(command("read -r line; printf '%s\\n' '{\"version\":1,\"ok\":true,\"error\":null,\"journal\":null}'; read -r line; exec sleep 600")).await.unwrap();
    let error = client
        .exchange(Request::HostStart, Duration::from_millis(20))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("unknown"));
    let error = client
        .exchange(
            Request::Recover {
                auto_rollback: true,
                primary: "lost".into(),
            },
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("reconnect"));
}
