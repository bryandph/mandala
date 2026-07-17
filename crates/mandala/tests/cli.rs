//! End-to-end CLI parity tests: drive the built `mandala` binary with the
//! `MANDALA_AGGREGATE_FILE` fixture seam so the fleet views render without a
//! real flake eval (sandbox-safe: no nix, no ansible, no network).
//!
//! The `--json` outputs are asserted byte-for-byte against Python
//! `json.dumps(..., indent=2, sort_keys=True)` ground truth; the human table
//! outputs are only checked for their caption/rows presence (no byte-parity
//! claim vs Python `rich`).

use std::io::Write;
use std::process::Command;

/// The fixture aggregate — three members, two groups, deploy + ansible
/// projections — written to a temp file the binary reads through the seam.
const FIXTURE: &str = r#"{
  "schemaVersion": 1,
  "members": {
    "web": {"platform": "nixos", "architecture": "x86_64", "category": "server", "role": "web", "tags": ["edge", "public"], "deployment": {"ansible": {"enable": true}, "deployRs": {"enable": true}, "sops": {"recipient": "age1web"}}},
    "cache": {"platform": "nixos", "architecture": "aarch64", "category": "server", "role": "cache", "tags": [], "deployment": {"ansible": {"enable": true}}},
    "router": {"platform": "vyos", "architecture": "aarch64", "category": "network"}
  },
  "groups": {"k3s": ["cache", "web"], "gateway": ["router"]},
  "projections": {
    "deploy": {"nodes": ["web", "cache"]},
    "ansibleInventory": {"all": {"hosts": {"web": null, "cache": null}}}
  }
}"#;

/// Write the fixture to a unique temp file; the caller keeps it alive.
fn fixture_file() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "mandala-cli-e2e-{}-{:?}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(FIXTURE.as_bytes()).unwrap();
    path
}

/// Run `mandala <args…>` with the aggregate seam pointed at `fixture`; return
/// `(stdout, stderr, exit_code)`.
fn run(fixture: &std::path::Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_mandala"))
        .args(args)
        .env("MANDALA_AGGREGATE_FILE", fixture)
        // Never let a stray worker/subprocess backend fire in the sandbox.
        .env("MANDALA_EVAL", "subprocess")
        .output()
        .expect("spawn mandala");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn members_json_byte_parity() {
    let fx = fixture_file();
    let (stdout, _stderr, code) = run(&fx, &["members", "--json"]);
    assert_eq!(code, 0);
    // json.dumps(members, indent=2, sort_keys=True) + the echo newline.
    let expected = "{\n  \"cache\": {\n    \"architecture\": \"aarch64\",\n    \"category\": \"server\",\n    \"deployment\": {\n      \"ansible\": {\n        \"enable\": true\n      }\n    },\n    \"platform\": \"nixos\",\n    \"role\": \"cache\",\n    \"tags\": []\n  },\n  \"router\": {\n    \"architecture\": \"aarch64\",\n    \"category\": \"network\",\n    \"platform\": \"vyos\"\n  },\n  \"web\": {\n    \"architecture\": \"x86_64\",\n    \"category\": \"server\",\n    \"deployment\": {\n      \"ansible\": {\n        \"enable\": true\n      },\n      \"deployRs\": {\n        \"enable\": true\n      },\n      \"sops\": {\n        \"recipient\": \"age1web\"\n      }\n    },\n    \"platform\": \"nixos\",\n    \"role\": \"web\",\n    \"tags\": [\n      \"edge\",\n      \"public\"\n    ]\n  }\n}\n";
    assert_eq!(stdout, expected);
    let _ = std::fs::remove_file(&fx);
}

#[test]
fn groups_json_byte_parity() {
    let fx = fixture_file();
    let (stdout, _e, code) = run(&fx, &["groups", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(
        stdout,
        "{\n  \"gateway\": [\n    \"router\"\n  ],\n  \"k3s\": [\n    \"cache\",\n    \"web\"\n  ]\n}\n"
    );
    let _ = std::fs::remove_file(&fx);
}

#[test]
fn ansible_inventory_json_byte_parity() {
    let fx = fixture_file();
    let (stdout, _e, code) = run(&fx, &["ansible", "inventory"]);
    assert_eq!(code, 0);
    assert_eq!(
        stdout,
        "{\n  \"all\": {\n    \"hosts\": {\n      \"cache\": null,\n      \"web\": null\n    }\n  }\n}\n"
    );
    let _ = std::fs::remove_file(&fx);
}

#[test]
fn resolve_echoes_one_member_per_line() {
    let fx = fixture_file();
    let (stdout, _e, code) = run(&fx, &["resolve", "all,!@gateway"]);
    assert_eq!(code, 0);
    assert_eq!(stdout, "cache\nweb\n");
    let _ = std::fs::remove_file(&fx);
}

#[test]
fn deploy_nodes_sorted() {
    let fx = fixture_file();
    let (stdout, _e, code) = run(&fx, &["deploy", "nodes"]);
    assert_eq!(code, 0);
    // Projection order is web,cache; the command sorts.
    assert_eq!(stdout, "cache\nweb\n");
    let _ = std::fs::remove_file(&fx);
}

#[test]
fn deploy_batch_rejects_unknown_group_without_spawning_nix() {
    let fx = fixture_file();
    let (_o, stderr, code) = run(&fx, &["deploy", "batch", "nope"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("no such group: nope"), "stderr: {stderr}");
    let _ = std::fs::remove_file(&fx);
}

#[test]
fn version_prints_the_crate_version() {
    let fx = fixture_file();
    let (stdout, _e, code) = run(&fx, &["version"]);
    assert_eq!(code, 0);
    assert_eq!(stdout, "0.1.0\n");
    let _ = std::fs::remove_file(&fx);
}

#[test]
fn members_table_carries_caption_and_rows() {
    let fx = fixture_file();
    let (stdout, _e, code) = run(&fx, &["members"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("member"), "header missing: {stdout}");
    assert!(stdout.contains("cache"));
    assert!(
        stdout.contains("3 members — ads = ansible/deploy-rs/sops"),
        "caption missing: {stdout}"
    );
    let _ = std::fs::remove_file(&fx);
}
