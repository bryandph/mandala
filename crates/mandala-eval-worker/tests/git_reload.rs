//! End-to-end reload regression against the real libnix eval worker.

use std::fs;
use std::path::Path;
use std::process::Command;

use mandala_core::eval::{Backend, Evaluator};

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "core.fsmonitor=false", "-C"])
        .arg(repo)
        .args(args)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed with {status}");
}

fn commit(repo: &Path, marker: &str) {
    fs::write(
        repo.join("flake.nix"),
        format!("{{ outputs = {{ self }}: {{ mandala = {{ marker = \"{marker}\"; }}; }}; }}\n"),
    )
    .expect("write flake");
    git(repo, &["add", "flake.nix"]);
    git(
        repo,
        &[
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=Mandala Test",
            "-c",
            "user.email=mandala@example.invalid",
            "commit",
            "-q",
            "-m",
            marker,
        ],
    );
}

#[test]
fn reload_observes_a_moved_git_flake() {
    let dir = std::env::temp_dir().join(format!(
        "mandala-git-reload-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("temp repo");
    git(&dir, &["init", "-q"]);
    commit(&dir, "A");
    let cache = dir.join("cache");
    fs::create_dir_all(&cache).expect("writable Nix cache");

    // This integration-test binary owns its environment and contains one
    // test, so no sibling can race this worker override.
    unsafe {
        std::env::set_var(
            "MANDALA_EVAL_WORKER",
            env!("CARGO_BIN_EXE_mandala-eval-worker"),
        );
        std::env::set_var("XDG_CACHE_HOME", &cache);
    }

    let mut evaluator = Evaluator::new(Backend::Worker).quiet();
    let first = evaluator
        .aggregate(dir.to_str().unwrap())
        .expect("commit A");
    assert_eq!(first["marker"], "A");

    commit(&dir, "B");
    evaluator.reload().expect("replace worker");
    let second = evaluator
        .aggregate(dir.to_str().unwrap())
        .expect("commit B");
    assert_eq!(second["marker"], "B");

    drop(evaluator);
    unsafe {
        std::env::remove_var("MANDALA_EVAL_WORKER");
        std::env::remove_var("XDG_CACHE_HOME");
    }
    fs::remove_dir_all(&dir).expect("remove temp repo");
}
