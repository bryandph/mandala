//! Native owner-local adapter/store effects. No flake evaluation or build.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::{AdapterReply, AdapterRequest, Effects, MAX_MESSAGE, VERSION, invalid};
use crate::vm::manifest::store_reference;

#[derive(Default)]
pub struct NativeEffects {
    /// Test injection only; the helper binary always uses `nix-store` on PATH.
    pub nix_store: Option<PathBuf>,
}
impl NativeEffects {
    async fn store(&self, args: &[&str]) -> io::Result<Vec<u8>> {
        let output = mandala_deploy::process::output(
            Command::new(
                self.nix_store
                    .as_deref()
                    .unwrap_or_else(|| Path::new("nix-store")),
            )
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
        )
        .await?;
        if !output.status.success() {
            return Err(invalid(format!(
                "owner store validation/rooting failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(output.stdout)
    }
}
impl Effects for NativeEffects {
    async fn adapter(&self, request: AdapterRequest) -> io::Result<AdapterReply> {
        if !store_reference(&request.manifest.adapter) {
            return Err(invalid("adapter is not an immutable store executable"));
        }
        let mut command = Command::new(&request.manifest.adapter);
        command
            .arg(format!("--mandala-vm-protocol={VERSION}"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (mut child, group) = mandala_deploy::process::spawn(&mut command)?;
        let mut stdin = child.stdin.take().expect("piped adapter input");
        let mut bytes = serde_json::to_vec(&request).map_err(io::Error::other)?;
        bytes.push(b'\n');
        stdin.write_all(&bytes).await?;
        drop(stdin);
        let output = mandala_deploy::process::wait_with_output(child, group).await?;
        if !output.status.success() {
            return Err(invalid(format!(
                "owner adapter {:?} failed: {}",
                request.operation,
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        if output.stdout.len() as u64 > MAX_MESSAGE {
            return Err(invalid("oversized owner adapter evidence"));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|e| invalid(format!("missing/invalid typed adapter evidence: {e}")))
    }
    async fn closure(
        &self,
        profile: &str,
        expected: &crate::vm::manifest::Manifest,
    ) -> io::Result<BTreeSet<String>> {
        if !store_reference(profile) {
            return Err(invalid("owner profile is not a store path"));
        }
        self.store(&["--check-validity", profile]).await?;
        let actual = crate::vm::manifest::Manifest::parse(&std::fs::read(
            Path::new(profile).join(crate::vm::manifest::RELATIVE_PATH),
        )?)
        .map_err(io::Error::other)?;
        if actual != *expected {
            return Err(invalid(
                "copied owner artifact manifest differs from the approved plan",
            ));
        }
        let bytes = self.store(&["--query", "--requisites", profile]).await?;
        let paths = String::from_utf8(bytes).map_err(io::Error::other)?;
        Ok(paths.lines().map(str::to_string).collect())
    }
    async fn root(&self, path: &str, link: &Path) -> io::Result<()> {
        if !store_reference(path) || path.ends_with(".drv") {
            return Err(invalid("recovery rooting requires a prebuilt store path"));
        }
        // Check before --realise: never turn recovery retention into a build.
        self.store(&["--check-validity", path]).await?;
        self.store(&[
            "--add-root",
            &link.display().to_string(),
            "--indirect",
            "--realise",
            path,
        ])
        .await?;
        std::fs::File::open(link.parent().ok_or_else(|| invalid("root has no parent"))?)?.sync_all()
    }
}
