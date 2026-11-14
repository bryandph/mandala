//! One owned SSH session; dropping it kills/reaps local effects rather than
//! detaching a transaction or replaying a command after an unknown response.

use std::io;
use std::time::Duration;

use mandala_deploy::{DeployData, DeployDefs};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

use super::{Journal, MAX_MESSAGE, Reply, Request, VERSION, invalid, line};

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;

pub struct OwnerClient {
    child: Option<Child>,
    group: Option<mandala_deploy::process::ChildGroup>,
    poisoned: bool,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    stderr: tokio::task::JoinHandle<io::Result<()>>,
}
impl Drop for OwnerClient {
    fn drop(&mut self) {
        self.stderr.abort();
    }
}
impl OwnerClient {
    /// Start through owner-only deploy settings and negotiate before effects.
    ///
    /// # Errors
    /// Spawn/transport failure, missing helper, deadline or protocol mismatch.
    pub async fn connect(deploy: &DeployData<'_>, defs: &DeployDefs) -> io::Result<Self> {
        let command = mandala_deploy::deploy::owner_session_command(deploy, defs);
        Self::spawn(command).await
    }
    async fn spawn(mut command: tokio::process::Command) -> io::Result<Self> {
        let (mut child, group) = mandala_deploy::process::spawn(&mut command)?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| invalid("helper input pipe unavailable"))?;
        let output = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| invalid("helper output pipe unavailable"))?,
        );
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| invalid("helper diagnostic pipe unavailable"))?;
        // Drain without unbounded buffering. Diagnostics are carried in typed
        // replies; an SSH failure is identified as a lost/unknown session.
        let stderr = tokio::spawn(async move {
            let mut buffer = [0_u8; 4096];
            while stderr.read(&mut buffer).await? != 0 {}
            Ok(())
        });
        let mut client = Self {
            child: Some(child),
            group: Some(group),
            poisoned: false,
            input: Some(input),
            output,
            stderr,
        };
        client
            .exchange(Request::Hello { version: VERSION }, Duration::from_secs(30))
            .await?
            .check()?;
        Ok(client)
    }
    /// Return even failed typed replies so their observed/recovery evidence can
    /// be atomically persisted by the controller before deciding the run result.
    /// Never retry a failed/lost response as a fresh runtime operation.
    ///
    /// # Errors
    /// Missing/oversized evidence, transport loss, deadline or version mismatch.
    pub async fn exchange(&mut self, request: Request, bound: Duration) -> io::Result<Reply> {
        if self.poisoned {
            return Err(invalid(
                "owner session outcome unknown; reconnect for explicit reconciliation",
            ));
        }
        let result = tokio::time::timeout(bound, async {
            let mut bytes = serde_json::to_vec(&request).map_err(io::Error::other)?;
            bytes.push(b'\n');
            if bytes.len() as u64 > MAX_MESSAGE {
                return Err(invalid("oversized owner request"));
            }
            let input = self
                .input
                .as_mut()
                .ok_or_else(|| invalid("owner session already closed"))?;
            input.write_all(&bytes).await?;
            input.flush().await?;
            let line = line(&mut self.output).await?.ok_or_else(|| {
                invalid("owner helper session lost; runtime unknown/reconciliation required")
            })?;
            let reply: Reply = serde_json::from_str(&line)
                .map_err(|e| invalid(format!("invalid owner helper evidence: {e}")))?;
            if reply.version != VERSION {
                return Err(invalid(format!(
                    "VM owner protocol mismatch: controller {VERSION}, helper {}",
                    reply.version
                )));
            }
            Ok(reply)
        })
        .await
        .map_err(|_| {
            invalid("owner helper deadline expired; runtime unknown/reconciliation required")
        })
        .and_then(std::convert::identity);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    /// A bounded acknowledged close, retaining the OS lock until settlement.
    ///
    /// # Errors
    /// Unsettled runtime, failed close or owned helper process failure.
    pub async fn finish(&mut self) -> io::Result<Option<Journal>> {
        let journal = self
            .exchange(Request::Finish, Duration::from_secs(30))
            .await?
            .check()?;
        self.input.take();
        let child = self
            .child
            .take()
            .ok_or_else(|| invalid("owner session already reaped"))?;
        let group = self
            .group
            .take()
            .ok_or_else(|| invalid("owner process group already reaped"))?;
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            mandala_deploy::process::wait_with_output(child, group),
        )
        .await
        .map_err(|_| invalid("owner helper close deadline expired"))??;
        if !result.status.success() {
            return Err(invalid("owner helper process failed after settlement"));
        }
        Ok(journal)
    }
}
