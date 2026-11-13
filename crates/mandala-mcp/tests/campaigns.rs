//! Typed campaign dispatch is independent of the warm latest inventory. Host
//! effects are replaced at the shared effect seam; audit remains real/local.
use mandala_mcp::{MandalaHandler, tool_is_idempotent, tools::*};
use serde_json::{Value, json};
use std::sync::Arc;
mod common;

struct State {
    root: std::path::PathBuf,
    previous: Option<std::ffi::OsString>,
}
impl State {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("mandala-mcp-campaigns-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let previous = std::env::var_os("MANDALA_FLEET_STATE");
        // This integration binary has one test: its process-local seam cannot race.
        unsafe { std::env::set_var("MANDALA_FLEET_STATE", &root) };
        Self { root, previous }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var("MANDALA_FLEET_STATE", previous)
            } else {
                std::env::remove_var("MANDALA_FLEET_STATE")
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn campaign_tools_preserve_typed_intent_audit_and_no_replay_without_warm_inventory() {
    let state = State::new();
    let canned = json!({"ok":false,"refused":true,"required_confirm":"node-a,node-b","generation":7,"campaign":{"entries":[{"host":"node-a","status":"drift"},{"host":"node-b","status":"expectation-unknown"}],"summary":{"drift":1,"expectation-unknown":1}},"latest_source":{"entries":[{"host":"node-a","status":"expectation-unknown"}],"summary":{"expectation-unknown":1}}});
    let effects = Arc::new(common::FakeEffects {
        campaign_result: Some(canned.clone()),
        ..Default::default()
    });
    // No preloaded inventory and no fresh_inventory fake: any accidental use
    // of the warm/latest contract would panic rather than silently retarget.
    let handler = MandalaHandler::with_effects(".", effects.clone());
    let id = "c-20260102T030405_000001-123";
    let scenarios = [
        (
            "campaign_create",
            json!({"selector":"node-a,node-b","dry_activate":false,"confirm":"node-a,node-b","actor":"operator-alice","boot":true}),
        ),
        (
            "campaign_inspect",
            json!({"campaign_id":id,"offset":50,"limit":10,"check_source":false}),
        ),
        (
            "campaign_retry",
            json!({"campaign_id":id,"generation":7,"selector":"node-b","dry_activate":false,"confirm":"node-b"}),
        ),
        (
            "campaign_exclude",
            json!({"campaign_id":id,"generation":7,"actor":"operator-alice","reason":"maintenance","add":["node-b"],"confirm":id}),
        ),
        (
            "campaign_reboot",
            json!({"campaign_id":id,"generation":7,"serial":"2","drain":false,"confirm":"node-a,node-b"}),
        ),
        ("campaign_verify", json!({"campaign_id":id,"refresh":true})),
        (
            "campaign_close",
            json!({"campaign_id":id,"generation":7,"actor":"operator-alice","reason":"incomplete but abandoned","confirm":id}),
        ),
    ];
    for (tool, args) in &scenarios {
        let result = handler
            .call_tool_from(
                Some("operator-client"),
                tool,
                args.as_object().unwrap().clone(),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(result).unwrap()["structuredContent"],
            canned
        );
    }
    {
        let calls = effects.campaign_calls.lock().unwrap();
        assert_eq!(calls.len(), 7);
        for ((tool, input), (recorded, actual)) in scenarios.iter().zip(calls.iter()) {
            assert_eq!(tool, recorded);
            for (key, value) in input.as_object().unwrap() {
                assert_eq!(&actual[key], value);
            }
        }
    }
    let result = handler
        .call_tool(
            "drift",
            json!({"campaign_id":id,"statuses":["drift"]})
                .as_object()
                .unwrap()
                .clone(),
        )
        .await
        .unwrap();
    let result = serde_json::to_value(result).unwrap()["structuredContent"].clone();
    assert_eq!(result["campaign"]["entries"].as_array().unwrap().len(), 1);
    assert!(
        result["latest_source"]["entries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        result["campaign"]["summary"]["expectation-unknown"], 1,
        "filtering never rewrites whole comparison totals"
    );
    let calls_before = effects.campaign_calls.lock().unwrap().len();
    assert!(
        handler
            .call_tool(
                "campaign_retry",
                json!({"campaign_id":id,"generation":-1})
                    .as_object()
                    .unwrap()
                    .clone()
            )
            .await
            .is_err()
    );
    assert_eq!(
        effects.campaign_calls.lock().unwrap().len(),
        calls_before,
        "invalid generations cannot enter effects"
    );
    assert!(tool_is_idempotent("campaign_inspect"));
    for tool in [
        "campaign_create",
        "campaign_retry",
        "campaign_exclude",
        "campaign_reboot",
        "campaign_verify",
        "campaign_close",
    ] {
        assert!(
            !tool_is_idempotent(tool),
            "{tool} must never be replayed after failover"
        );
    }
    let create = serde_json::to_value(CampaignCreateTool::tool()).unwrap();
    let retry = serde_json::to_value(CampaignRetryTool::tool()).unwrap();
    let reboot = serde_json::to_value(CampaignRebootTool::tool()).unwrap();
    assert_eq!(
        create["inputSchema"]["properties"]["dry_activate"]["default"],
        true
    );
    assert_eq!(
        retry["inputSchema"]["properties"]["dry_activate"]["default"],
        true
    );
    assert_eq!(
        reboot["inputSchema"]["properties"]["drain"]["default"],
        true
    );
    let audit = std::fs::read_to_string(state.root.join("mcp/audit.jsonl")).unwrap();
    let audit: Vec<Value> = audit
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for tool in [
        "campaign_create",
        "campaign_retry",
        "campaign_exclude",
        "campaign_reboot",
        "campaign_verify",
        "campaign_close",
    ] {
        assert!(
            audit
                .iter()
                .any(|event| event["tool"] == tool && event["origin"] == "operator-client")
        );
    }
    assert!(
        !audit
            .iter()
            .any(|event| event["tool"] == "campaign_inspect")
    );
    let legacy =
        MandalaHandler::with_effects(".", effects.clone()).preloaded(common::base_inventory());
    let result = legacy
        .call_tool(
            "resolve",
            json!({"selector":"@k3s"}).as_object().unwrap().clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(result).unwrap()["structuredContent"]["limit"],
        "cache,web"
    );
    assert_eq!(
        effects.campaign_calls.lock().unwrap().len(),
        calls_before,
        "legacy reads cannot implicitly join campaigns"
    );
    assert!(!state.root.join("campaigns").exists());
}
