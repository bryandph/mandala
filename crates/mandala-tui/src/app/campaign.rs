use super::*;
use crate::campaign::{Prompt, Review};
use crossterm::event::{KeyCode, KeyModifiers};
use mandala_context::FleetContext;
use mandala_core::campaign::{self, AttemptKind, Store};
use serde_json::json;

async fn call(
    session: Option<mandala_context::ContextSession>,
    flake: String,
    tool: &str,
    args: Value,
) -> Result<Value, String> {
    let args = args.as_object().cloned().unwrap_or_default();
    if let Some(session) = session {
        session
            .call(tool, args, mandala_mcp::tool_is_idempotent(tool))
            .await
            .map(|value| value["structuredContent"].clone())
            .map_err(|error| error.to_string())
    } else {
        let effects = std::sync::Arc::new(mandala_mcp::effects::RealEffects::quiet(&flake));
        let handler = mandala_mcp::MandalaHandler::with_effects(flake, effects);
        handler
            .call_tool(tool, args)
            .await
            .map(|result| {
                serde_json::to_value(result).unwrap_or_default()["structuredContent"].clone()
            })
            .map_err(|error| error.to_string())
    }
}

impl App {
    fn campaign_request(&mut self, status: &str) -> u64 {
        self.campaign_seq += 1;
        if let Some(ScreenState::Campaign(view)) = self.state.screen.as_mut() {
            view.request = self.campaign_seq;
            view.busy = true;
            view.status = status.into();
            view.scroll = 0;
        }
        self.dirty = true;
        self.campaign_seq
    }
    fn campaign_call(&mut self, tool: &str, args: Value) {
        let request=self.campaign_request("Request in progress; closing detaches, it does not replay or cancel an accepted mutation");
        let tx = self.tx.clone();
        let tool = tool.to_string();
        let flake = self.cfg.flake.clone();
        let session = self.context.as_ref().map(|context| context.session.clone());
        tokio::spawn(async move {
            let result = call(session, flake, &tool, args.clone()).await;
            let _ = tx
                .send(AppEvent::CampaignResult {
                    request,
                    tool,
                    args,
                    result,
                })
                .await;
        });
    }
    fn campaign_capture(&mut self, target: String, boot: bool) {
        let request = self.campaign_request(
            "Capturing frozen target for review; no campaign or host effects yet",
        );
        let tx = self.tx.clone();
        let flake = self.cfg.flake.clone();
        let actor = std::env::var("USER").unwrap_or_else(|_| "operator".into());
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                campaign::source::Capture::new(&flake, &target, &actor, 4, boot)
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r.map(Box::new).map_err(|e| e.to_string()));
            let _ = tx
                .send(AppEvent::CampaignCaptured { request, result })
                .await;
        });
    }
    fn campaign_bind(&mut self, preview: bool, cache_push: Option<bool>, halt: bool) {
        let Some(capture) = self.campaign_capture.take() else {
            return;
        };
        let names =
            match capture
                .target
                .required_confirm(AttemptKind::Deploy, &capture.plan.targets, false)
            {
                Ok(names) => names,
                Err(error) => {
                    self.state.set_status(error.to_string(), true);
                    return;
                }
            };
        let request = self.campaign_request(if preview {
            "Binding preview without host effects"
        } else {
            "Creating and deploying reviewed target"
        });
        let tx = self.tx.clone();
        let flake = self.cfg.flake.clone();
        let session = self.context.as_ref().map(|context| context.session.clone());
        tokio::spawn(async move {
            let result=async{
                let bound=tokio::task::spawn_blocking(move||capture.bind(&Store::default())).await.map_err(|e|e.to_string())?.map_err(|e|e.to_string())?;
                if preview{campaign::views::inspect(&Store::default(),Some(&bound.campaign_id),0,50).map_err(|e|e.to_string())}
                else{call(session,flake,"campaign_retry",json!({"campaign_id":bound.campaign_id,"generation":0,"confirm":names,"dry_activate":false,"cache_push":cache_push,"halt_on_build_failure":halt})).await}
            }.await;
            let _ = tx
                .send(AppEvent::CampaignResult {
                    request,
                    tool: "campaign_create".into(),
                    args: json!({"preview":preview}),
                    result,
                })
                .await;
        });
    }

    pub(super) fn campaign_captured(
        &mut self,
        request: u64,
        result: Result<Box<campaign::source::Capture>, String>,
    ) {
        let Some(ScreenState::Campaign(view)) = self.state.screen.as_mut() else {
            return;
        };
        if view.request != request {
            return;
        }
        view.busy = false;
        match result {
            Ok(capture) => {
                view.payload = json!({"unbound_target":capture.target,"target_digest":capture.target.digest().ok(),"eligible_names":capture.plan.targets,"notice":"No campaign is bound and no host effects occur before confirmation"});
                if !capture.plan.vm.is_empty() {
                    view.payload["owner_preview"] = capture.plan.preview();
                }
                view.status = "Frozen target ready for explicit review".into();
                view.review = Some(Review::Create);
                view.id = None;
                self.campaign_capture = Some(capture);
            }
            Err(error) => view.status = error,
        }
        self.dirty = true;
    }
    pub(super) fn campaign_result(
        &mut self,
        request: u64,
        tool: String,
        args: Value,
        result: Result<Value, String>,
    ) -> Option<McpFollowUp> {
        let Some(ScreenState::Campaign(view)) = self.state.screen.as_mut() else {
            if let Ok(value) = result
                && let Some(run) = value["run_id"].as_str()
            {
                self.watch_run(run.into(), true);
                self.state.set_status(
                    format!("campaign run {run} continues; attach from Runs"),
                    true,
                );
            }
            return None;
        };
        if view.request != request {
            if let Ok(value) = result
                && let Some(run) = value["run_id"].as_str()
            {
                self.watch_run(run.into(), true);
            }
            return None;
        }
        view.busy = false;
        match result {
            Ok(mut value) => {
                if let Some(id) = value["campaign_id"].as_str() {
                    view.id = Some(id.into());
                }
                if value["confirmation_required"] == true {
                    let mut args = args;
                    args["confirm"] = value["required_confirm"].clone();
                    view.review = Some(Review::Call {
                        tool: tool.clone(),
                        args,
                    });
                }
                let run = value["run_id"].as_str().map(str::to_string);
                view.status = if value["ok"] == false {
                    "Refused or incomplete: inspect diagnostic; no automatic replay".into()
                } else {
                    "Result recorded; completion and lifecycle are separate".into()
                };
                if run.is_some() {
                    value["run_kind"] = Value::from(if tool == "campaign_reboot" {
                        "reboot"
                    } else if tool == "campaign_verify" {
                        "verify"
                    } else {
                        "deploy"
                    });
                    view.status =
                        "Run linked; a attaches through the existing run interface".into();
                }
                view.payload = value;
                self.dirty = true;
                if let Some(run) = run {
                    self.watch_run(run, true);
                }
            }
            Err(error) => view.status = format!("{error}; inspect ownership before retrying"),
        }
        self.dirty = true;
        None
    }

    pub(super) async fn on_campaign_key(
        &mut self,
        key: KeyEvent,
        size: (u16, u16),
    ) -> io::Result<()> {
        let Some(ScreenState::Campaign(mut view)) = self.state.screen.take() else {
            return Ok(());
        };
        let mut invoke = None;
        let mut capture = false;
        let mut bind = None;
        let mut attach = None;
        let mut dismiss = false;
        if view.busy {
            if key.code == KeyCode::Esc {
                self.campaign_capture = None;
                self.dirty = true;
                return Ok(());
            }
        } else if let Some(mut prompt) = view.prompt.take() {
            match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    let text = prompt.text.trim();
                    let actor = std::env::var("USER").unwrap_or_else(|_| "operator".into());
                    let request = if prompt.close && !text.is_empty() {
                        Some((
                            "campaign_close",
                            json!({"campaign_id":prompt.id,"generation":prompt.generation,"actor":actor,"reason":text,"confirm":prompt.id}),
                        ))
                    } else if !prompt.close {
                        text.split_once(char::is_whitespace).and_then(|(names,reason)|{
                            let reason=reason.trim();if reason.is_empty()||(!names.starts_with('+')&&!names.starts_with('-')){return None;}
                            let names:Vec<_>=names[1..].split(',').map(str::to_string).collect();
                            Some(("campaign_exclude",json!({"campaign_id":prompt.id,"generation":prompt.generation,"actor":actor,"reason":reason,"confirm":prompt.id,"add":if text.starts_with('+'){names.clone()}else{Vec::new()},"remove":if text.starts_with('-'){names}else{Vec::new()}})))
                        })
                    } else {
                        None
                    };
                    if let Some((tool, args)) = request {
                        view.review = Some(Review::Call {
                            tool: tool.into(),
                            args,
                        });
                        view.status = "Review reason, exact names and generation".into();
                    } else {
                        view.status =
                            "A nonempty reason and valid +names/-names are required".into();
                        view.prompt = Some(prompt);
                    }
                }
                KeyCode::Backspace => {
                    prompt.text.pop();
                    view.prompt = Some(prompt);
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if prompt.text.len() < 2048 {
                        prompt.text.push(c);
                    }
                    view.prompt = Some(prompt);
                }
                _ => view.prompt = Some(prompt),
            }
        } else if let Some(review) = view.review.take() {
            match key.code {
                KeyCode::Esc => {
                    self.campaign_capture = None;
                    view.status = "Review cancelled without dispatch".into();
                }
                KeyCode::Char('y') => match review {
                    Review::Create => bind = Some(false),
                    Review::Call { tool, args } => invoke = Some((tool, args)),
                },
                KeyCode::Char('p') if matches!(review, Review::Create) => bind = Some(true),
                _ => view.review = Some(review),
            }
        } else {
            match key.code {
                KeyCode::Esc => {
                    if view.id.take().is_some() {
                        view.cursor = 0;
                        view.offset = 0;
                        invoke = Some(("campaign_inspect".into(), json!({"check_source":false})));
                    } else {
                        dismiss = true;
                    }
                }
                KeyCode::Up => view.cursor = view.cursor.saturating_sub(1),
                KeyCode::Down => {
                    view.cursor = (view.cursor + 1).min(view.cursor_len().saturating_sub(1))
                }
                KeyCode::PageUp => view.scroll = view.scroll.saturating_sub(15),
                KeyCode::PageDown => view.scroll = view.scroll.saturating_add(15),
                KeyCode::Char('B') => view.boot = !view.boot,
                KeyCode::Char('h') => view.halt = !view.halt,
                KeyCode::Char('P') => {
                    view.cache_push = match view.cache_push {
                        None => Some(true),
                        Some(true) => Some(false),
                        Some(false) => None,
                    }
                }
                KeyCode::Char('1') => view.serial = "1".into(),
                KeyCode::Char('2') => view.serial = "2".into(),
                KeyCode::Char('3') => view.serial = "100%".into(),
                KeyCode::Char('g') => view.drain = !view.drain,
                KeyCode::Char('c') => capture = true,
                KeyCode::Enter if view.id.is_none() => {
                    if let Some(id) = view.selected_id().map(str::to_string) {
                        view.id = Some(id.clone());
                        view.cursor = 0;
                        view.offset = 0;
                        invoke = Some(("campaign_inspect".into(), json!({"campaign_id":id})));
                    }
                }
                KeyCode::Char('r') => {
                    invoke = Some((
                        "campaign_inspect".into(),
                        json!({"campaign_id":view.id,"offset":view.offset,"check_source":view.id.is_some()}),
                    ))
                }
                KeyCode::Char(c @ ('[' | ']')) => {
                    view.offset = if c == '[' {
                        view.offset.saturating_sub(50)
                    } else {
                        view.offset.saturating_add(50)
                    };
                    invoke = Some((
                        "campaign_inspect".into(),
                        json!({"campaign_id":view.id,"offset":view.offset,"check_source":false}),
                    ));
                }
                KeyCode::Char(c @ ('t' | 'T' | 'b' | 'v' | 'x' | 'z' | 'd' | 'E')) => {
                    if let Some(id) = view.id.clone() {
                        match Store::default().load(&id) {
                            Ok(campaign) => match c {
                                't' | 'T' => {
                                    let eligible = campaign.eligible(AttemptKind::Deploy, None)?;
                                    let names = campaign.required_confirm(
                                        AttemptKind::Deploy,
                                        &eligible,
                                        c == 'T',
                                    )?;
                                    if !campaign
                                        .target
                                        .vm_owner_plan
                                        .for_targets(&eligible)
                                        .is_empty()
                                    {
                                        view.payload["owner_preview"] =
                                            campaign.target.deploy_preview(&eligible, c == 'T')?;
                                    }
                                    view.review = Some(Review::Call {
                                        tool: "campaign_retry".into(),
                                        args: json!({"campaign_id":id,"generation":campaign.generation,"confirm":names,"dry_activate":c=='T',"cache_push":view.cache_push,"halt_on_build_failure":view.halt}),
                                    });
                                    view.status="Review immutable target and exact failed/pending members; successful siblings stay untouched".into();
                                }
                                'b' => {
                                    invoke = Some((
                                        "campaign_reboot".into(),
                                        json!({"campaign_id":id,"generation":campaign.generation,"serial":view.serial,"drain":view.drain}),
                                    ))
                                }
                                'v' => {
                                    invoke = Some((
                                        "campaign_verify".into(),
                                        json!({"campaign_id":id,"refresh":true}),
                                    ))
                                }
                                'x' | 'z' => {
                                    view.prompt = Some(Prompt {
                                        close: c == 'z',
                                        generation: campaign.generation,
                                        id,
                                        text: String::new(),
                                    })
                                }
                                'd' | 'E' => {
                                    invoke = Some((
                                        "drift".into(),
                                        json!({"campaign_id":id,"do_eval":c=='E'}),
                                    ))
                                }
                                _ => {}
                            },
                            Err(error) => view.status = error.to_string(),
                        }
                    }
                }
                KeyCode::Char('a') => {
                    attach = view
                        .selected_attempt()
                        .map(|(kind, id)| (kind.to_string(), id.to_string()))
                }
                _ => {}
            }
        }
        let boot = view.boot;
        let halt = view.halt;
        let cache = view.cache_push;
        self.state.screen = if dismiss {
            None
        } else {
            Some(ScreenState::Campaign(view))
        };
        self.dirty = true;
        if let Some((tool, args)) = invoke {
            self.campaign_call(&tool, args);
        }
        if capture {
            if let Some(target) = self.state.deploy_target() {
                self.campaign_capture(target, boot);
            } else if let Some(ScreenState::Campaign(view)) = self.state.screen.as_mut() {
                view.status = "Select deploy-capable members in the explorer first".into();
            }
        }
        if let Some(preview) = bind {
            self.campaign_bind(preview, cache, halt);
        }
        if let Some((_kind, id)) = attach {
            if let Some(row) = RunsState::load()
                .rows
                .into_iter()
                .find(|row| row.run_id == id)
            {
                self.attach_run_manual(row, size).await;
            } else if let Some(ScreenState::Campaign(view)) = self.state.screen.as_mut() {
                view.status = "Attempt run is unavailable; no substitute run was attached".into();
            }
        }
        Ok(())
    }

    pub(super) fn open_campaigns(&mut self) {
        self.campaign_capture = None;
        self.state.screen = Some(ScreenState::Campaign(Box::default()));
        self.campaign_call("campaign_inspect", json!({"check_source":false}));
    }
}

#[cfg(test)]
mod tests;
