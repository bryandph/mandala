//! Per-member isolation against the real libnix eval worker: one
//! member whose configuration throws is reported against that member, the
//! others still evaluate, and a member with no configuration is absent.

use std::fs;
use std::path::Path;
use std::process::Command;

use mandala_core::eval::{Backend, Evaluator};

const GOOD: &str = "/nix/store/00000000000000000000000000000000-good";

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "core.fsmonitor=false", "-C"])
        .arg(repo)
        .args(args)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed with {status}");
}

#[test]
fn one_broken_member_never_hides_the_others() {
    let dir = std::env::temp_dir().join(format!(
        "mandala-member-isolation-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("temp repo");
    git(&dir, &["init", "-q"]);
    fs::write(
        dir.join("flake.nix"),
        format!(
            r#"{{
  outputs = {{ self }}: {{
    nixosConfigurations = {{
      good.config.system.build.toplevel.outPath = "{GOOD}";
      bad.config.system.build.toplevel = throw "bad host is broken";
    }};
  }};
}}
"#
        ),
    )
    .expect("write flake");
    git(&dir, &["add", "flake.nix"]);
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

    // Not `.quiet()`: keep a dying worker's real reason on the test's stderr.
    let mut evaluator = Evaluator::new(Backend::Worker);
    let members = ["good", "bad", "absent"].map(String::from);
    let toplevels = evaluator
        .expected_toplevels(dir.to_str().unwrap(), &members)
        .expect("a broken member does not fail the batch");
    assert_eq!(toplevels.paths.get("good").map(String::as_str), Some(GOOD));
    assert!(
        toplevels
            .errors
            .get("bad")
            .is_some_and(|e| e.contains("bad host is broken")),
        "bad's error is reported against bad: {toplevels:?}"
    );
    assert!(!toplevels.paths.contains_key("bad"));
    assert!(!toplevels.paths.contains_key("absent"));
    assert!(!toplevels.errors.contains_key("absent"));

    drop(evaluator);
    unsafe {
        std::env::remove_var("MANDALA_EVAL_WORKER");
        std::env::remove_var("XDG_CACHE_HOME");
    }
    fs::remove_dir_all(&dir).expect("remove temp repo");
}
