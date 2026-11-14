//! VM approval uses the same native plan and is rechecked before launch.
use mandala_core::{engines::deploy, inventory::Inventory, registry};
use mandala_mcp::MandalaHandler;
use serde_json::{Value, json};
use std::sync::Arc;
mod common;

struct State {
    root: std::path::PathBuf,
    previous: Option<std::ffi::OsString>,
}
impl Drop for State {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var("MANDALA_FLEET_STATE", previous);
            } else {
                std::env::remove_var("MANDALA_FLEET_STATE");
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
#[tokio::test]
async fn vm_owner_backend_mcp_requires_exact_shared_preview_before_any_engine_launch() {
    let root = std::env::temp_dir().join(format!("mandala-mcp-owner-{}", std::process::id()));
    let state = State {
        root: root.clone(),
        previous: std::env::var_os("MANDALA_FLEET_STATE"),
    };
    unsafe {
        std::env::set_var("MANDALA_FLEET_STATE", &root);
    }
    let value: Value = serde_json::from_str(include_str!(
        "../../mandala-core/tests/fixtures/vm-inventory.json"
    ))
    .unwrap();
    let inventory = Inventory::from_value(value.clone()).unwrap();
    let plan = deploy::plan_run(&inventory, "guest-a", 4, true).unwrap();
    let required = plan.required_owner_approval().unwrap();
    let handler = MandalaHandler::with_effects(
        ".",
        Arc::new(common::FakeEffects {
            fake_deploy: true,
            ..Default::default()
        }),
    )
    .preloaded(inventory);
    for token in [None, Some("stale")] {
        let result = handler
            .call_tool(
                "deploy",
                json!({"selector":"guest-a","owner_approval":token})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .await
            .unwrap();
        let result = &serde_json::to_value(result).unwrap()["structuredContent"];
        assert_eq!(result["ok"], false);
        assert_eq!(result["preview"], plan.preview());
        assert_eq!(result["required_owner_approval"], required);
        assert!(!root.join("runs").exists());
    }
    let result = handler
        .call_tool(
            "deploy",
            json!({"selector":"guest-a","owner_approval":required})
                .as_object()
                .unwrap()
                .clone(),
        )
        .await
        .unwrap();
    let result = &serde_json::to_value(result).unwrap()["structuredContent"];
    assert_eq!(result["ok"], true);
    let run = registry::open_run(result["run_id"].as_str().unwrap()).unwrap();
    assert_eq!(run.info.meta["owner_approval_test"], required);
    let mut moved = value;
    moved["members"]["host-a"]["tags"] = json!(["changed"]);
    let moved = MandalaHandler::with_effects(".", Arc::new(common::FakeEffects::default()))
        .preloaded(Inventory::from_value(moved).unwrap());
    let response = moved
        .call_tool(
            "deploy",
            json!({"selector":"guest-a","owner_approval":required})
                .as_object()
                .unwrap()
                .clone(),
        )
        .await
        .unwrap();
    let response = &serde_json::to_value(response).unwrap()["structuredContent"];
    assert_eq!(response["ok"], false);
    assert_ne!(response["required_owner_approval"], required);
    let legacy = MandalaHandler::with_effects(".", Arc::new(common::FakeEffects::default()))
        .preloaded(common::base_inventory());
    let response = legacy
        .call_tool(
            "deploy",
            json!({"selector":"web","owner_approval":required})
                .as_object()
                .unwrap()
                .clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(response).unwrap()["structuredContent"]["ok"],
        false
    );
    assert_eq!(std::fs::read_dir(root.join("runs")).unwrap().count(), 1);
    drop(state);
}
