use super::*;
use crate::campaign::CampaignView;
const ID: &str = "c-20260102T030405_000001-123";
const RUN: &str = "20260102T030406_000001-123";
fn app() -> App {
    let mut app = App::new(AppState::new(), ExplorerConfig::default());
    app.state.screen = Some(ScreenState::Campaign(Box::new(CampaignView {
        request: 2,
        ..Default::default()
    })));
    app
}
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn view(app: &App) -> &CampaignView {
    match app.state.screen.as_ref().unwrap() {
        ScreenState::Campaign(view) => view,
        _ => panic!("campaign screen expected"),
    }
}

#[tokio::test]
async fn cancelling_review_never_dispatches_or_creates_a_background_request() {
    let mut app = app();
    if let Some(ScreenState::Campaign(view)) = app.state.screen.as_mut() {
        view.review = Some(Review::Call {
            tool: "campaign_retry".into(),
            args: json!({"campaign_id":ID,"generation":7,"confirm":"node-a","dry_activate":false}),
        });
    }
    app.on_campaign_key(key(KeyCode::Esc), (120, 60))
        .await
        .unwrap();
    assert!(view(&app).review.is_none());
    assert_eq!(app.campaign_seq, 0);
    assert!(app.rx.try_recv().is_err());
    assert!(app.campaign_capture.is_none());
}

#[tokio::test]
async fn reason_prompt_preserves_reviewed_generation_until_confirmation() {
    let mut app = app();
    if let Some(ScreenState::Campaign(view)) = app.state.screen.as_mut() {
        view.id = Some(ID.into());
        view.prompt = Some(Prompt {
            close: false,
            generation: 7,
            id: ID.into(),
            text: "+node-b maintenance".into(),
        });
        view.payload = json!({"generation":99});
    }
    app.on_campaign_key(key(KeyCode::Enter), (120, 60))
        .await
        .unwrap();
    let Some(Review::Call { tool, args }) = &view(&app).review else {
        panic!("review required")
    };
    assert_eq!(tool, "campaign_exclude");
    assert_eq!(args["generation"], 7);
    assert_eq!(args["confirm"], ID);
    assert_eq!(args["add"], json!(["node-b"]));
    assert_eq!(args["reason"], "maintenance");
    assert_eq!(app.campaign_seq, 0);
    assert!(
        app.rx.try_recv().is_err(),
        "reason entry cannot perform the mutation before y"
    );
}

#[test]
fn stale_capture_and_call_results_cannot_replace_a_newer_review() {
    let mut app = app();
    if let Some(ScreenState::Campaign(view)) = app.state.screen.as_mut() {
        view.busy = true;
        view.payload = json!({"marker":"newer"});
    }
    app.campaign_captured(1, Err("stale source failure".into()));
    app.campaign_result(
        1,
        "campaign_inspect".into(),
        json!({}),
        Ok(json!({"marker":"old"})),
    );
    assert!(view(&app).busy);
    assert_eq!(view(&app).payload["marker"], "newer");
    app.campaign_result(
        2,
        "campaign_inspect".into(),
        json!({}),
        Ok(json!({"ok":true,"campaign_id":ID,"generation":7})),
    );
    assert!(!view(&app).busy);
    assert_eq!(view(&app).id.as_deref(), Some(ID));
}

#[test]
fn reboot_preflight_refusal_requires_explicit_review_with_original_order_and_drain() {
    let mut app = app();
    let args = json!({"campaign_id":ID,"generation":7,"serial":"2","drain":false});
    app.campaign_result(2,"campaign_reboot".into(),args,Ok(json!({"ok":false,"confirmation_required":true,"required_confirm":"node-b","names":["node-b"],"generation":7})));
    let Some(Review::Call { tool, args }) = &view(&app).review else {
        panic!("fresh review required")
    };
    assert_eq!(tool, "campaign_reboot");
    assert_eq!(args["confirm"], "node-b");
    assert_eq!(args["generation"], 7);
    assert_eq!(args["serial"], "2");
    assert_eq!(args["drain"], false);
    assert!(app.rx.try_recv().is_err());
    assert_eq!(app.campaign_seq, 0);
}

#[test]
fn run_results_keep_exact_attach_links_and_detached_accepted_runs_are_watched() {
    let mut app = app();
    app.campaign_result(
        2,
        "campaign_verify".into(),
        json!({}),
        Ok(json!({"ok":true,"campaign_id":ID,"run_id":RUN})),
    );
    assert_eq!(view(&app).selected_attempt(), Some(("verify", RUN)));
    assert!(app.watched_runs.iter().any(|(id, _)| id == RUN));
    app.campaign_result(
        1,
        "campaign_retry".into(),
        json!({}),
        Ok(json!({"ok":true,"run_id":"20260102T030407_000001-123"})),
    );
    assert_eq!(
        view(&app).selected_attempt(),
        Some(("verify", RUN)),
        "late accepted work cannot clobber the current screen"
    );
    assert!(
        app.watched_runs
            .iter()
            .any(|(id, _)| id == "20260102T030407_000001-123")
    );
}
