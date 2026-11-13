//! Campaign effects remain outside the warm inventory evaluator. Real fleet
//! continuations use the existing durable supervisor and engine run-id handshake.

use super::*;
use mandala_core::campaign::{self, AttemptKind, Store, api, execution, views};
use serde_json::{Value, json};

fn required<'a>(args: &'a Value, key: &str) -> io::Result<&'a str> {
    args[key]
        .as_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("missing {key}")))
}
fn generation(args: &Value) -> io::Result<u64> {
    args["generation"]
        .as_u64()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing campaign generation"))
}
fn strings(args: &Value, key: &str) -> io::Result<Vec<String>> {
    if args[key].is_null() {
        return Ok(Vec::new());
    }
    serde_json::from_value(args[key].clone()).map_err(io::Error::other)
}
fn refusal(error: io::Error) -> Value {
    json!({"ok":false,"refused":true,"conflict":error.kind()==io::ErrorKind::WouldBlock,"error":error.to_string()})
}

fn call(flake: &str, operation: &str, args: &Value) -> io::Result<Value> {
    let store = Store::default();
    let id = args["campaign_id"].as_str();
    match operation {
        "campaign_create" => api::create(
            &store,
            flake,
            api::CreateRequest {
                selector: required(args, "selector")?,
                actor: args["actor"].as_str().unwrap_or("mcp-client"),
                boot: args["boot"].as_bool().unwrap_or(false),
                preview: args["dry_activate"].as_bool().unwrap_or(true),
                confirm: args["confirm"].as_str(),
                throttle: args["throttle"].as_i64().unwrap_or(4),
            },
        ),
        "campaign_inspect" => api::inspect(
            &store,
            id,
            args["offset"].as_u64().unwrap_or(0) as usize,
            args["limit"].as_u64().unwrap_or(50).min(100) as usize,
            args["check_source"].as_bool().unwrap_or(true),
        ),
        "campaign_retry" => {
            let id = required(args, "campaign_id")?;
            if args["reconcile_only"].as_bool().unwrap_or(false) {
                execution::reconcile_at(&store, id, generation(args)?)?;
                return views::inspect(&store, Some(id), 0, 100);
            }
            let narrow = api::narrow(&store, id, args["selector"].as_str())?;
            api::review(
                &store,
                id,
                generation(args)?,
                AttemptKind::Deploy,
                narrow.as_ref(),
                args["dry_activate"].as_bool().unwrap_or(true),
                args["confirm"].as_str(),
            )
        }
        "campaign_reboot" => {
            let id = required(args, "campaign_id")?;
            let narrow = api::narrow(&store, id, args["selector"].as_str())?;
            execution::review_reboot(
                &store,
                id,
                &execution::RebootRequest {
                    generation: generation(args)?,
                    confirm: args["confirm"].as_str(),
                    narrow: narrow.as_ref(),
                    serial: args["serial"].as_str().unwrap_or("1"),
                    drain: args["drain"].as_bool().unwrap_or(true),
                },
            )
        }
        "campaign_exclude" => {
            let reason = required(args, "reason")?;
            api::exclude(
                &store,
                required(args, "campaign_id")?,
                api::PolicyRequest {
                    generation: generation(args)?,
                    actor: required(args, "actor")?,
                    reason,
                    confirm: args["confirm"].as_str(),
                },
                strings(args, "add")?
                    .into_iter()
                    .map(|name| (name, reason.into()))
                    .collect(),
                strings(args, "remove")?,
            )
        }
        "campaign_close" => api::close(
            &store,
            required(args, "campaign_id")?,
            generation(args)?,
            required(args, "actor")?,
            required(args, "reason")?,
            args["confirm"].as_str(),
        ),
        "campaign_verify" => api::verify(
            &store,
            required(args, "campaign_id")?,
            args["refresh"].as_bool().unwrap_or(true),
        ),
        "campaign_drift" => {
            let id = required(args, "campaign_id")?;
            let survey = if args["refresh"].as_bool().unwrap_or(false) {
                let c = store.load(id)?;
                let names = c.eligible(AttemptKind::Verify, None)?;
                if names.is_empty() {
                    None
                } else {
                    Some(api::survey(&store, &c, &names))
                }
            } else {
                None
            };
            let mut result = api::drift(
                &store,
                id,
                flake,
                args["do_eval"].as_bool().unwrap_or(false),
            )?;
            if let Some(survey) = survey {
                match survey {
                    Ok(survey) => {
                        if survey.rc != 0 {
                            result["ok"] = Value::Bool(false);
                            result["campaign"]["complete"] = Value::Bool(false);
                        }
                        result["survey"] =
                            serde_json::to_value(survey).map_err(io::Error::other)?;
                    }
                    Err(error) => {
                        result["ok"] = Value::Bool(false);
                        result["campaign"]["complete"] = Value::Bool(false);
                        result["survey_error"] = Value::from(error.to_string());
                    }
                }
            }
            Ok(result)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unknown campaign operation",
        )),
    }
}

fn should_start(operation: &str, args: &Value, result: &Value) -> bool {
    match operation {
        "campaign_create" => result["ok"] == true && result["ready_to_deploy"] == true,
        "campaign_retry" => {
            result["ok"] == true
                && !args["reconcile_only"].as_bool().unwrap_or(false)
                && result["names"]
                    .as_array()
                    .is_some_and(|names| !names.is_empty())
        }
        "campaign_reboot" => result["ok"] == true && result["no_op"] != true,
        _ => false,
    }
}

impl RealEffects {
    pub(super) async fn campaign_call(
        &self,
        flake: &str,
        operation: &str,
        args: Value,
    ) -> io::Result<Value> {
        let input = args.clone();
        let owned_flake = flake.to_string();
        let owned_operation = operation.to_string();
        let mut result =
            match tokio::task::spawn_blocking(move || call(&owned_flake, &owned_operation, &input))
                .await
                .map_err(io::Error::other)?
            {
                Ok(value) => value,
                Err(error) => return Ok(refusal(error)),
            };
        let start = should_start(operation, &args, &result);
        if !start {
            return Ok(result);
        }
        let id = result["campaign_id"]
            .as_str()
            .ok_or_else(|| io::Error::other("campaign review lacks id"))?
            .to_string();
        let gen0 = result["generation"]
            .as_u64()
            .ok_or_else(|| io::Error::other("campaign review lacks generation"))?;
        let names = result["required_confirm"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let engine = std::env::var_os("MANDALA_DEPLOY_ENGINE_BIN")
            .map(PathBuf::from)
            .or_else(|| std::env::current_exe().ok())
            .unwrap_or_else(|| PathBuf::from("mandala"));
        let mut argv = vec![
            engine.display().to_string(),
            "--flake".into(),
            flake.into(),
            "campaign".into(),
            if operation == "campaign_reboot" {
                "reboot".into()
            } else {
                "retry".into()
            },
            id.clone(),
            "--generation".into(),
            gen0.to_string(),
            "--confirm".into(),
            names.clone(),
        ];
        if let Some(selector) = args["selector"].as_str()
            && operation != "campaign_create"
        {
            argv.extend(["--limit".into(), selector.into()]);
        }
        if operation == "campaign_reboot" {
            argv.extend([
                "--serial".into(),
                args["serial"].as_str().unwrap_or("1").into(),
            ]);
            if args["drain"].as_bool().unwrap_or(true) {
                argv.push("--drain".into());
            } else {
                argv.push("--no-drain".into());
            }
        } else {
            if operation == "campaign_retry" && args["dry_activate"].as_bool().unwrap_or(true) {
                argv.push("--dry-activate".into());
            }
            argv.extend([
                "--throttle".into(),
                args["throttle"].as_i64().unwrap_or(4).to_string(),
            ]);
            if args["halt_on_build_failure"].as_bool().unwrap_or(false) {
                argv.push("--halt-on-build-failure".into());
            }
            if let Some(enabled) = args["cache_push"].as_bool() {
                argv.push(if enabled {
                    "--cache-push".into()
                } else {
                    "--no-cache-push".into()
                });
            }
        }
        let mut run = DeployRun::new(names);
        run.program = Some(argv);
        let launch = async {
            run.start().await?;
            loop {
                match run.discover_run() {
                    Ok(true) => break,
                    Ok(false) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
                    Err(error) => return Err(error),
                }
            }
            Ok::<_, io::Error>(())
        }
        .await;
        if let Err(error) = launch {
            return Ok(
                json!({"ok":false,"campaign_id":id,"outcome":"unknown","error":error.to_string(),"guidance":"inspect campaign and reconcile ownership; do not blindly replay"}),
            );
        }
        result["run_id"] = serde_json::to_value(&run.run_id).map_err(io::Error::other)?;
        result["events_dir"] = serde_json::to_value(&run.events_dir).map_err(io::Error::other)?;
        if let Ok(c) = campaign::Store::default().load(&id) {
            result["generation"] = Value::from(c.generation);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_creation_starts_in_the_same_call_but_preview_refusal_and_empty_work_do_not() {
        assert!(should_start(
            "campaign_create",
            &json!({"dry_activate":false}),
            &json!({"ok":true,"ready_to_deploy":true})
        ));
        assert!(!should_start(
            "campaign_create",
            &json!({}),
            &json!({"ok":true,"preview":true,"ready_to_deploy":false})
        ));
        assert!(!should_start(
            "campaign_create",
            &json!({}),
            &json!({"ok":false,"ready_to_deploy":true})
        ));
        assert!(!should_start(
            "campaign_retry",
            &json!({}),
            &json!({"ok":true,"names":[]})
        ));
        assert!(!should_start(
            "campaign_retry",
            &json!({}),
            &json!({"ok":false,"names":["node-a"]})
        ));
        assert!(!should_start(
            "campaign_retry",
            &json!({"reconcile_only":true}),
            &json!({"ok":true,"names":["node-a"]})
        ));
        assert!(should_start(
            "campaign_retry",
            &json!({}),
            &json!({"ok":true,"names":["node-a"]})
        ));
        assert!(!should_start(
            "campaign_reboot",
            &json!({}),
            &json!({"ok":true,"no_op":true})
        ));
        assert!(!should_start(
            "campaign_verify",
            &json!({}),
            &json!({"ok":true,"run_id":"existing-observation"})
        ));
    }
}
