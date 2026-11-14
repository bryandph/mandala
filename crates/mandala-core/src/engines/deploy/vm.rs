//! Owner-only seam around existing copy/preflight/deploy-rs effects. Keeping
//! this separate leaves every non-VM byte/effect path unchanged.

use super::*;
use crate::vm::manifest::{Manifest, RELATIVE_PATH};
use crate::vm::owner::{Journal, Mode, OwnerClient, Request, State};
use std::collections::BTreeSet;
use std::io::Write;
use std::time::Duration;

#[cfg(test)]
#[path = "vm_tests.rs"]
mod tests;

pub(super) struct Transaction {
    client: OwnerClient,
    unit: crate::vm::OwnerUnit,
    manifest: Manifest,
    profile: String,
    run: PathBuf,
    writer: Arc<EventWriter>,
    bound: Duration,
    pub last: Option<Journal>,
}
impl Transaction {
    pub async fn connect(
        run: &RegisteredDeployRun,
        host: &str,
        profile: &Path,
        deploy: &mandala_deploy::DeployData<'_>,
        defs: &mandala_deploy::DeployDefs,
        writer: Arc<EventWriter>,
    ) -> io::Result<Self> {
        let unit = run.plan.vm.units[host].clone();
        let manifest: Manifest =
            serde_json::from_slice(&std::fs::read(profile.join(RELATIVE_PATH))?)
                .map_err(io::Error::other)?;
        let output = mandala_deploy::process::output(
            tokio::process::Command::new(
                deploy
                    .cmd_overrides
                    .nix_program
                    .as_deref()
                    .unwrap_or_else(|| Path::new("nix")),
            )
            .args([
                "path-info",
                "--recursive",
                "--json",
                &profile.display().to_string(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
        )
        .await?;
        if !output.status.success() {
            return Err(io::Error::other(
                "owner artifact closure could not be verified",
            ));
        }
        let paths: Value = serde_json::from_slice(&output.stdout).map_err(io::Error::other)?;
        let closure: BTreeSet<_> = if let Some(object) = paths.as_object() {
            object.keys().cloned().collect()
        } else if let Some(array) = paths.as_array() {
            array
                .iter()
                .filter_map(|entry| entry["path"].as_str().map(str::to_string))
                .collect()
        } else {
            return Err(io::Error::other("invalid owner closure evidence"));
        };
        manifest
            .validate(&unit, &closure)
            .map_err(io::Error::other)?;
        let bound = Duration::from_secs(
            manifest
                .guests
                .values()
                .map(|g| g.readiness.deadline_seconds)
                .max()
                .unwrap_or(1)
                * 2
                + 30,
        );
        let client = OwnerClient::connect(deploy, defs).await?;
        let mut transaction = Self {
            client,
            unit,
            manifest,
            profile: profile.display().to_string(),
            run: run.path.clone(),
            writer,
            bound,
            last: None,
        };
        transaction
            .request(Request::Lock {
                owner: host.into(),
                transaction: run.run_id.clone(),
                dry: run.plan.dry_activate,
                reconcile: false,
            })
            .await?;
        Ok(transaction)
    }
    async fn request(&mut self, request: Request) -> io::Result<()> {
        let reply = match self.client.exchange(request, self.bound).await {
            Ok(reply) => reply,
            Err(error) => {
                self.mark_unknown(&error.to_string())?;
                return Err(error);
            }
        };
        if let Some(journal) = &reply.journal {
            self.record(journal)?;
            self.last = Some(journal.clone());
        }
        reply.check()?;
        Ok(())
    }
    fn record(&self, journal: &Journal) -> io::Result<()> {
        let directory = self.run.join("vm");
        std::fs::create_dir_all(&directory)?;
        let tmp = directory.join(format!("{}.tmp", self.unit.owner));
        let path = directory.join(format!("{}.json", self.unit.owner));
        let mut file = std::fs::File::create(&tmp)?;
        serde_json::to_writer(&mut file, journal).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(tmp, path)?;
        std::fs::File::open(&directory)?.sync_all()?;
        for (guest, outcome) in &journal.guests {
            self.writer.emit(
                "vm",
                event_fields([
                    ("owner", Value::from(self.unit.owner.clone())),
                    ("guest", Value::from(guest.clone())),
                    ("backend", Value::from("nixos-module")),
                    (
                        "manifest_version",
                        Value::from(self.manifest.manifest_version),
                    ),
                    (
                        "outcome",
                        serde_json::to_value(outcome).map_err(io::Error::other)?,
                    ),
                ]),
            )?;
        }
        Ok(())
    }
    fn mark_unknown(&mut self, error: &str) -> io::Result<()> {
        if let Some(mut journal) = self.last.clone()
            && !matches!(
                journal.state,
                State::Confirmed | State::Pending | State::Restored | State::NotApplied
            )
        {
            journal.state = State::Unknown;
            journal.primary_failure.get_or_insert_with(|| error.into());
            journal
                .guests
                .values_mut()
                .for_each(|s| *s = crate::vm::owner::GuestOutcome::Unknown);
            self.record(&journal)?;
            self.last = Some(journal);
        }
        Ok(())
    }
    pub async fn prepare(&mut self) -> io::Result<()> {
        self.request(Request::Prepare {
            unit: Box::new(self.unit.clone()),
            manifest: Box::new(self.manifest.clone()),
            profile: self.profile.clone(),
        })
        .await
    }
    pub async fn start(&mut self) -> io::Result<()> {
        self.request(Request::HostStart).await
    }
    pub async fn abort_unapplied(&mut self, error: &str) {
        if mandala_deploy::process::is_stopping() {
            return;
        }
        if self
            .request(Request::AbortUnapplied {
                diagnostic: error.into(),
            })
            .await
            .is_ok()
        {
            let _ = self.finish().await;
        }
    }
    pub async fn apply(&mut self, boot: bool) -> io::Result<()> {
        self.request(Request::Apply {
            mode: if boot { Mode::Boot } else { Mode::Live },
        })
        .await
    }
    pub async fn recover(&mut self, auto_rollback: bool, primary: &str) -> io::Result<()> {
        if mandala_deploy::process::is_stopping() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "owner runtime unknown; recovery prohibited during termination",
            ));
        }
        self.request(Request::Recover {
            auto_rollback,
            primary: primary.into(),
        })
        .await
    }
    pub async fn finish(&mut self) -> io::Result<()> {
        let result = self.client.finish().await;
        if let Err(error) = &result {
            self.mark_unknown(&error.to_string())?;
        }
        if let Some(journal) = result? {
            self.record(&journal)?;
            self.last = Some(journal);
        }
        Ok(())
    }
    pub fn state(&self) -> HostState {
        match self.last.as_ref().map(|j| j.state) {
            Some(State::Confirmed) => HostState::Confirmed,
            Some(State::Pending) => HostState::RebootPending,
            Some(State::Restored) => HostState::RolledBack,
            _ => HostState::Failed,
        }
    }
}
