//! Additive headless campaign lifecycle. Existing deploy/run commands do not
//! route through this module and never implicitly allocate a campaign.

use super::*;
use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};
use std::process::ExitCode;

fn flag(name: &'static str) -> Arg {
    Arg::new(name).long(name).action(ArgAction::SetTrue)
}
fn execution_args(command: Command) -> Command {
    command
        .args(crate::engines::cache_publish::flags())
        .arg(
            Arg::new("generation")
                .long("generation")
                .required(true)
                .value_parser(value_parser!(u64)),
        )
        .arg(Arg::new("confirm").long("confirm"))
        .arg(Arg::new("limit").long("limit"))
        .arg(
            Arg::new("throttle")
                .long("throttle")
                .default_value("4")
                .value_parser(value_parser!(i64)),
        )
        .arg(flag("dry-activate"))
        .arg(flag("halt-on-build-failure"))
}
fn id(command: Command) -> Command {
    command.arg(Arg::new("id").required(true))
}

pub fn command() -> Command {
    Command::new("campaign")
        .about("Explicit durable deployment campaigns")
        .subcommand_required(true)
        .arg(flag("json").global(true))
        .subcommand(
            Command::new("create")
                .about("Bind and deploy in one operation; --preview binds without effects")
                .args(crate::engines::cache_publish::flags())
                .arg(Arg::new("limit").long("limit").required(true))
                .arg(Arg::new("confirm").long("confirm"))
                .arg(Arg::new("actor").long("actor").default_value("operator"))
                .arg(
                    Arg::new("throttle")
                        .long("throttle")
                        .default_value("4")
                        .value_parser(value_parser!(i64)),
                )
                .arg(flag("preview"))
                .arg(flag("boot"))
                .arg(flag("halt-on-build-failure")),
        )
        .subcommand(
            Command::new("inspect")
                .arg(Arg::new("id"))
                .arg(
                    Arg::new("offset")
                        .long("offset")
                        .default_value("0")
                        .value_parser(value_parser!(usize)),
                )
                .arg(
                    Arg::new("page-size")
                        .long("page-size")
                        .default_value("50")
                        .value_parser(value_parser!(usize)),
                )
                .arg(flag("check-source")),
        )
        .subcommand(execution_args(id(Command::new("retry"))))
        .subcommand(
            id(Command::new("reboot"))
                .arg(
                    Arg::new("generation")
                        .long("generation")
                        .required(true)
                        .value_parser(value_parser!(u64)),
                )
                .arg(Arg::new("confirm").long("confirm"))
                .arg(Arg::new("limit").long("limit"))
                .arg(Arg::new("serial").long("serial").default_value("1"))
                .arg(flag("drain").conflicts_with("no-drain"))
                .arg(flag("no-drain")),
        )
        .subcommand(id(Command::new("verify")).arg(flag("no-refresh")))
        .subcommand(id(Command::new("reconcile")))
        .subcommand(
            id(Command::new("exclude"))
                .arg(
                    Arg::new("generation")
                        .long("generation")
                        .required(true)
                        .value_parser(value_parser!(u64)),
                )
                .arg(Arg::new("confirm").long("confirm"))
                .arg(Arg::new("actor").long("actor").default_value("operator"))
                .arg(Arg::new("reason").long("reason").required(true))
                .arg(Arg::new("add").long("add"))
                .arg(Arg::new("remove").long("remove")),
        )
        .subcommand(
            id(Command::new("close"))
                .arg(
                    Arg::new("generation")
                        .long("generation")
                        .required(true)
                        .value_parser(value_parser!(u64)),
                )
                .arg(Arg::new("confirm").long("confirm"))
                .arg(Arg::new("actor").long("actor").default_value("operator"))
                .arg(Arg::new("reason").long("reason").required(true)),
        )
}
fn string<'a>(m: &'a ArgMatches, name: &str) -> &'a str {
    m.get_one::<String>(name).map_or("", String::as_str)
}
fn confirm(m: &ArgMatches) -> Option<&str> {
    m.get_one::<String>("confirm").map(String::as_str)
}
fn generation(m: &ArgMatches) -> u64 {
    *m.get_one::<u64>("generation").expect("required generation")
}
fn names(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn run_drift(flake: &str, m: &ArgMatches) -> ExitCode {
    let store = Store::default();
    let id = string(m, "campaign");
    let result = (|| -> io::Result<Value> {
        if m.get_flag("refresh") {
            let c = store.load(id)?;
            let names = c.eligible(AttemptKind::Verify, None)?;
            if !names.is_empty() && api::survey(&store, &c, &names)?.rc != 0 {
                return Err(invalid("campaign drift survey failed"));
            }
        }
        api::drift(&store, id, flake, m.get_flag("eval"))
    })();
    match result {
        Ok(value) => {
            println!("{}", serde_json::to_string_pretty(&value).unwrap());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("mandala drift: {error}");
            ExitCode::FAILURE
        }
    }
}

fn publish(id: &str) -> io::Result<()> {
    let mut out = io::stdout().lock();
    writeln!(out, "{id}")?;
    out.flush()
}

pub fn run(flake: &str, m: &ArgMatches) -> ExitCode {
    let result = (|| -> io::Result<Value> {
        let _signals = mandala_deploy::process::SignalForwarder::install(true)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(dispatch(flake, m))
    })();
    match result {
        Ok(value) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&value).expect("JSON result")
            );
            if value["ok"] == false {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            eprintln!("mandala campaign: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(flake: &str, m: &ArgMatches) -> io::Result<Value> {
    let store = Store::default();
    let (action, m) = m
        .subcommand()
        .ok_or_else(|| invalid("campaign action required"))?;
    match action {
        "create" => {
            let mut result = api::create(
                &store,
                flake,
                api::CreateRequest {
                    selector: string(m, "limit"),
                    actor: string(m, "actor"),
                    boot: m.get_flag("boot"),
                    preview: m.get_flag("preview"),
                    confirm: confirm(m),
                    throttle: *m.get_one::<i64>("throttle").unwrap(),
                },
            )?;
            if result["ready_to_deploy"] == true {
                let id = result["campaign_id"]
                    .as_str()
                    .expect("bound id")
                    .to_string();
                let request = execution::DeployRequest {
                    generation: 0,
                    confirm: confirm(m),
                    narrow: None,
                    dry_activate: false,
                    throttle: *m.get_one::<i64>("throttle").unwrap(),
                    halt_on_build_failure: m.get_flag("halt-on-build-failure"),
                    publish: crate::engines::cache_publish::Config::resolve(
                        crate::engines::cache_publish::choice(m),
                        std::env::var_os("MANDALA_CACHE_URL"),
                        std::env::var_os("MANDALA_CACHE_NETRC_FILE"),
                    )
                    .map_err(|e| invalid(e.to_string()))?,
                };
                if let Some(deployment) =
                    execution::deploy_notified(&store, &id, request, publish).await?
                {
                    result["run_id"] = Value::from(deployment.run_id);
                    result["rc"] = Value::from(deployment.outcome.rc);
                    result["ok"] = Value::Bool(deployment.outcome.rc == 0);
                }
            }
            Ok(result)
        }
        "inspect" => api::inspect(
            &store,
            m.get_one::<String>("id").map(String::as_str),
            *m.get_one::<usize>("offset").unwrap(),
            *m.get_one::<usize>("page-size").unwrap(),
            m.get_flag("check-source"),
        ),
        "retry" => {
            let id = string(m, "id");
            let narrow = api::narrow(&store, id, m.get_one::<String>("limit").map(String::as_str))?;
            let dry = m.get_flag("dry-activate");
            let mut result = api::review(
                &store,
                id,
                generation(m),
                AttemptKind::Deploy,
                narrow.as_ref(),
                dry,
                confirm(m),
            )?;
            if result["ok"] == false {
                return Ok(result);
            }
            let request = execution::DeployRequest {
                generation: generation(m),
                confirm: confirm(m),
                narrow: narrow.as_ref(),
                dry_activate: dry,
                throttle: *m.get_one::<i64>("throttle").unwrap(),
                halt_on_build_failure: m.get_flag("halt-on-build-failure"),
                publish: crate::engines::cache_publish::Config::resolve(
                    crate::engines::cache_publish::choice(m),
                    std::env::var_os("MANDALA_CACHE_URL"),
                    std::env::var_os("MANDALA_CACHE_NETRC_FILE"),
                )
                .map_err(|e| invalid(e.to_string()))?,
            };
            if let Some(deployment) =
                execution::deploy_notified(&store, id, request, publish).await?
            {
                result["run_id"] = Value::from(deployment.run_id);
                result["rc"] = Value::from(deployment.outcome.rc);
                result["ok"] = Value::Bool(deployment.outcome.rc == 0);
            } else {
                result["no_op"] = Value::Bool(true);
            }
            Ok(result)
        }
        "reboot" => {
            let id = string(m, "id");
            let narrow = api::narrow(&store, id, m.get_one::<String>("limit").map(String::as_str))?;
            execution::reboot(
                &store,
                id,
                execution::RebootRequest {
                    generation: generation(m),
                    confirm: confirm(m),
                    narrow: narrow.as_ref(),
                    serial: string(m, "serial"),
                    drain: !m.get_flag("no-drain"),
                },
                publish,
            )
        }
        "verify" => api::verify(&store, string(m, "id"), !m.get_flag("no-refresh")),
        "reconcile" => {
            execution::reconcile(&store, string(m, "id"))?;
            views::inspect(&store, Some(string(m, "id")), 0, 100)
        }
        "exclude" => {
            let reason = string(m, "reason");
            let add = names(string(m, "add"))
                .into_iter()
                .map(|name| (name, reason.into()))
                .collect();
            api::exclude(
                &store,
                string(m, "id"),
                api::PolicyRequest {
                    generation: generation(m),
                    actor: string(m, "actor"),
                    reason,
                    confirm: confirm(m),
                },
                add,
                names(string(m, "remove")),
            )
        }
        "close" => api::close(
            &store,
            string(m, "id"),
            generation(m),
            string(m, "actor"),
            string(m, "reason"),
            confirm(m),
        ),
        _ => Err(invalid("unknown campaign action")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "c-20260102T030405_000001-123";
    #[test]
    fn create_is_real_by_default_and_preview_is_an_explicit_no_effect_path() {
        let m = command()
            .try_get_matches_from([
                "campaign",
                "create",
                "--limit",
                "node-a",
                "--confirm",
                "node-a",
            ])
            .unwrap();
        let (_, m) = m.subcommand().unwrap();
        assert!(!m.get_flag("preview"));
        assert_eq!(confirm(m), Some("node-a"));
        let m = command()
            .try_get_matches_from(["campaign", "create", "--limit", "node-a", "--preview"])
            .unwrap();
        let (_, m) = m.subcommand().unwrap();
        assert!(m.get_flag("preview"));
        assert_eq!(confirm(m), None);
    }
    #[test]
    fn retry_requires_generation_and_preserves_dry_mode_and_exact_confirmation() {
        assert!(
            command()
                .try_get_matches_from(["campaign", "retry", ID])
                .is_err()
        );
        let m = command()
            .try_get_matches_from([
                "campaign",
                "retry",
                ID,
                "--generation",
                "7",
                "--confirm",
                "node-b",
                "--limit",
                "node-b",
                "--dry-activate",
            ])
            .unwrap();
        let (_, m) = m.subcommand().unwrap();
        assert_eq!(generation(m), 7);
        assert_eq!(confirm(m), Some("node-b"));
        assert!(m.get_flag("dry-activate"));
        assert_eq!(string(m, "limit"), "node-b");
    }
    #[test]
    fn reboot_keeps_existing_drain_default_with_explicit_disable_and_policy_requires_reason() {
        let m = command()
            .try_get_matches_from(["campaign", "reboot", ID, "--generation", "7"])
            .unwrap();
        let (_, m) = m.subcommand().unwrap();
        assert!(!m.get_flag("no-drain"));
        assert_eq!(string(m, "serial"), "1");
        let m = command()
            .try_get_matches_from([
                "campaign",
                "reboot",
                ID,
                "--generation",
                "7",
                "--no-drain",
                "--serial",
                "2",
            ])
            .unwrap();
        let (_, m) = m.subcommand().unwrap();
        assert!(m.get_flag("no-drain"));
        assert_eq!(string(m, "serial"), "2");
        assert!(
            command()
                .try_get_matches_from(["campaign", "close", ID, "--generation", "7"])
                .is_err()
        );
        assert!(
            command()
                .try_get_matches_from([
                    "campaign",
                    "exclude",
                    ID,
                    "--generation",
                    "7",
                    "--add",
                    "node-b"
                ])
                .is_err()
        );
    }
}
