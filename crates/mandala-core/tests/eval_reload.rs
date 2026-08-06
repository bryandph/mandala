//! Reload lifecycle regression: a worker-backed evaluator must replace the
//! process (and therefore the Nix EvalState), not ask a warm worker to clear a
//! shallower cache.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use mandala_core::eval::{Backend, Evaluator};

fn write_worker(path: &Path) {
    fs::write(
        path,
        r#"#!/bin/sh
counter=${MANDALA_RELOAD_TEST_COUNTER:?}
n=0
if test -f "$counter"; then
  n=$(sed -n '1p' "$counter")
fi
n=$((n + 1))
printf '%s\n' "$n" > "$counter"
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  printf '{"id":%s,"ok":true,"value":%s}\n' "$id" "$n"
done
"#,
    )
    .expect("write worker stub");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod worker stub");
}

#[test]
fn reload_replaces_the_worker_process() {
    let dir = std::env::temp_dir().join(format!("mandala-eval-reload-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("temp dir");
    let worker = dir.join("worker.sh");
    let counter = dir.join("counter");
    write_worker(&worker);

    // This integration-test binary owns its environment; no sibling test
    // mutates these variables.
    unsafe {
        std::env::set_var("MANDALA_EVAL_WORKER", &worker);
        std::env::set_var("MANDALA_RELOAD_TEST_COUNTER", &counter);
    }

    let mut evaluator = Evaluator::new(Backend::Worker).quiet();
    assert_eq!(evaluator.aggregate(".").expect("first worker"), 1);
    evaluator.reload().expect("reload");
    assert_eq!(evaluator.aggregate(".").expect("replacement worker"), 2);

    unsafe {
        std::env::remove_var("MANDALA_EVAL_WORKER");
        std::env::remove_var("MANDALA_RELOAD_TEST_COUNTER");
    }
    fs::remove_dir_all(&dir).expect("remove temp dir");
}
