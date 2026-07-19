use std::collections::HashMap;
use std::fs;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use futures_util::FutureExt;
use mandala_deploy::data::{GenericSettings, Node, NodeSettings, Profile, ProfileSettings};
use mandala_deploy::deploy::deploy_profile;
use mandala_deploy::push::push_profile;
use mandala_deploy::{CmdOverrides, EventSink, JsonlSink, Level, make_deploy_data};
use serde::Deserialize;
use tokio::task::JoinSet;

#[derive(Deserialize)]
struct Config {
    hosts: Vec<HostConfig>,
}

#[derive(Clone, Deserialize)]
struct HostConfig {
    name: String,
    hostname: String,
    ssh_user: String,
    #[serde(default)]
    ssh_opts: Vec<String>,
    profile_path: String,
    #[serde(default)]
    panic: bool,
}

async fn run_host(host: HostConfig, sink: &dyn EventSink) -> Result<(), String> {
    if host.panic {
        panic!("deliberate spike panic for {}", host.name);
    }
    let profile = Profile {
        profile_settings: ProfileSettings {
            path: host.profile_path,
            profile_path: None,
        },
        generic_settings: GenericSettings::default(),
    };
    let mut profiles = HashMap::new();
    profiles.insert("system".to_owned(), profile.clone());
    let node = Node {
        generic_settings: GenericSettings {
            ssh_user: Some(host.ssh_user),
            ssh_opts: host.ssh_opts,
            ..GenericSettings::default()
        },
        node_settings: NodeSettings {
            hostname: host.hostname,
            profiles,
            profiles_order: vec![],
        },
    };
    let overrides = CmdOverrides::default();
    let deploy = make_deploy_data(
        &GenericSettings::default(),
        &node,
        &host.name,
        &profile,
        "system",
        &overrides,
        sink,
    );
    let defs = deploy.defs().map_err(|error| error.to_string())?;
    sink.emit(Level::Info, "milestone=copy");
    push_profile(&deploy, &defs, false)
        .await
        .map_err(|error| error.to_string())?;
    sink.emit(Level::Info, "milestone=activate");
    deploy_profile(&deploy, &defs, true, false)
        .await
        .map_err(|error| error.to_string())?;
    sink.emit(Level::Info, "milestone=complete");
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    // A caught panic is a per-host result, never terminal noise.
    std::panic::set_hook(Box::new(|_| {}));
    let mut args = std::env::args_os().skip(1);
    let Some(config_path) = args.next() else {
        return ExitCode::from(2);
    };
    let Some(events_dir) = args.next() else {
        return ExitCode::from(2);
    };
    if args.next().is_some() {
        return ExitCode::from(2);
    }
    let Ok(config) = fs::read(&config_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Config>(&bytes).ok())
        .ok_or(())
    else {
        return ExitCode::from(2);
    };
    let events_dir = PathBuf::from(events_dir);
    if fs::create_dir_all(&events_dir).is_err() {
        return ExitCode::FAILURE;
    }

    let mut tasks = JoinSet::new();
    for host in config.hosts {
        let path = events_dir.join(format!("{}.jsonl", host.name));
        tasks.spawn(async move {
            let name = host.name.clone();
            let sink = match JsonlSink::new(&name, &path) {
                Ok(sink) => Arc::new(sink),
                Err(error) => return (name, None, Err(error.to_string())),
            };
            let result = AssertUnwindSafe(run_host(host, sink.as_ref()))
                .catch_unwind()
                .await
                .map_err(|_| "host task panicked".to_owned())
                .and_then(|result| result);
            (name, Some(sink), result)
        });
    }

    let mut failed = false;
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((_name, Some(sink), Ok(()))) => sink.emit(Level::Info, "host-result=ok"),
            Ok((_name, Some(sink), Err(error))) => {
                failed = true;
                sink.emit(Level::Error, &format!("host-result=failed error={error}"));
            }
            Ok((_name, None, Err(_))) => failed = true,
            Err(_) => failed = true,
            Ok((_name, None, Ok(()))) => unreachable!(),
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[allow(dead_code)]
fn _path_is_scoped(path: &Path) -> bool {
    path.is_relative()
}
