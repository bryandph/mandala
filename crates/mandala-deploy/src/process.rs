//! Owned process groups for deploy effects. The engine and supervisor install
//! signal forwarding explicitly; library users never acquire signal handlers.
use std::collections::BTreeSet;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::{Handle, Signals};

#[derive(Default)]
struct Children {
    groups: BTreeSet<i32>,
    stopping: bool,
}

fn children() -> &'static Mutex<Children> {
    static CHILDREN: OnceLock<Mutex<Children>> = OnceLock::new();
    CHILDREN.get_or_init(Default::default)
}

/// Kills remaining group members on cancellation or early return, but never
/// after a successful wait reaped the leader (its PID/PGID may then be reused).
/// Spawn and registration share the signal-forwarder's lock.
#[derive(Debug)]
pub struct ChildGroup {
    pgid: i32,
    reaped: bool,
}

impl ChildGroup {
    /// Disarm immediately after a successful wait. Descendants of a normally
    /// completed leader are not signalled: the old PGID is no longer ours.
    fn leader_reaped(&mut self, children: &mut Children) {
        self.reaped = true;
        children.groups.remove(&self.pgid);
    }
}

fn signal_group(pgid: i32, signal: Signal) {
    #[cfg(test)]
    if SIGNAL_CALLS.with(|calls| {
        if let Some(count) = calls.get() {
            calls.set(Some(count + 1));
            true
        } else {
            false
        }
    }) {
        return;
    }
    let _ = killpg(Pid::from_raw(pgid), signal);
}

#[cfg(test)]
thread_local! {
    static SIGNAL_CALLS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

impl Drop for ChildGroup {
    fn drop(&mut self) {
        let mut children = children().lock().unwrap_or_else(|e| e.into_inner());
        if !self.reaped {
            signal_group(self.pgid, Signal::SIGKILL);
        }
        children.groups.remove(&self.pgid);
    }
}

pub fn spawn_std(command: &mut Command) -> io::Result<(Child, ChildGroup)> {
    let mut children = children()
        .lock()
        .map_err(|_| io::Error::other("child registry lock poisoned"))?;
    if children.stopping {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "deploy is terminating",
        ));
    }
    let child = command.process_group(0).spawn()?;
    let pid = child.id() as i32;
    children.groups.insert(pid);
    Ok((
        child,
        ChildGroup {
            pgid: pid,
            reaped: false,
        },
    ))
}

/// Like Command::output, for commands whose stdout/stderr should be captured.
pub fn output_std(command: &mut Command) -> io::Result<Output> {
    let (child, group) = spawn_std(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )?;
    wait_std_with_output(child, group)
}

/// Reap and unregister under the forwarder's lock. Poll rather than holding
/// that lock across a blocking wait (which would prevent termination).
pub fn wait_std(child: &mut Child, group: &mut ChildGroup) -> io::Result<std::process::ExitStatus> {
    loop {
        {
            let mut children = children().lock().unwrap_or_else(|e| e.into_inner());
            if let Some(status) = child.try_wait()? {
                group.leader_reaped(&mut children);
                return Ok(status);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn wait_std_with_output(mut child: Child, mut group: ChildGroup) -> io::Result<Output> {
    fn read(mut pipe: impl io::Read) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    let stdout = child
        .stdout
        .take()
        .map(|pipe| std::thread::spawn(move || read(pipe)));
    let stderr = child
        .stderr
        .take()
        .map(|pipe| std::thread::spawn(move || read(pipe)));
    let status = wait_std(&mut child, &mut group)?;
    let join = |task: Option<JoinHandle<io::Result<Vec<u8>>>>| {
        task.map_or(Ok(Vec::new()), |task| {
            task.join()
                .map_err(|_| io::Error::other("output reader panicked"))?
        })
    };
    Ok(Output {
        status,
        stdout: join(stdout)?,
        stderr: join(stderr)?,
    })
}

pub fn spawn(
    command: &mut tokio::process::Command,
) -> io::Result<(tokio::process::Child, ChildGroup)> {
    let mut children = children()
        .lock()
        .map_err(|_| io::Error::other("child registry lock poisoned"))?;
    if children.stopping {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "deploy is terminating",
        ));
    }
    let child = command.process_group(0).kill_on_drop(true).spawn()?;
    let pid = child.id().expect("new child has a PID") as i32;
    children.groups.insert(pid);
    Ok((
        child,
        ChildGroup {
            pgid: pid,
            reaped: false,
        },
    ))
}

pub async fn wait_with_output(
    mut child: tokio::process::Child,
    mut group: ChildGroup,
) -> io::Result<Output> {
    use tokio::io::AsyncReadExt;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let read_stdout = async {
        let mut bytes = Vec::new();
        if let Some(pipe) = stdout.as_mut() {
            pipe.read_to_end(&mut bytes).await?;
        }
        io::Result::Ok(bytes)
    };
    let read_stderr = async {
        let mut bytes = Vec::new();
        if let Some(pipe) = stderr.as_mut() {
            pipe.read_to_end(&mut bytes).await?;
        }
        io::Result::Ok(bytes)
    };
    let mut child_wait = std::pin::pin!(child.wait());
    let wait = std::future::poll_fn(|cx| {
        use std::future::Future;
        let mut children = children().lock().unwrap_or_else(|e| e.into_inner());
        let result = child_wait.as_mut().poll(cx);
        if matches!(&result, std::task::Poll::Ready(Ok(_))) {
            // Polling wait can reap: remove its PGID before the forwarder can
            // acquire the lock, even while descendants hold capture pipes.
            group.leader_reaped(&mut children);
        }
        result
    });
    let (status, stdout, stderr) = tokio::try_join!(wait, read_stdout, read_stderr)?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

pub async fn output(command: &mut tokio::process::Command) -> io::Result<Output> {
    let (child, group) = spawn(command)?;
    wait_with_output(child, group).await
}

/// Forward SIGINT/SIGTERM to every owned group. Engines escalate after two
/// seconds and exit; supervisors instead stay alive to reap the engine. A
/// stopping flag prevents recovery or fan-out from launching new effects.
pub struct SignalForwarder {
    handle: Handle,
    thread: Option<JoinHandle<()>>,
}

impl SignalForwarder {
    pub fn install(engine: bool) -> io::Result<Self> {
        let mut signals = Signals::new([SIGINT, SIGTERM])?;
        let handle = signals.handle();
        let thread = std::thread::Builder::new()
            .name("deploy-signals".into())
            .spawn(move || {
                for signal in signals.forever() {
                    let signal_kind = Signal::try_from(signal).expect("registered signal");
                    {
                        let mut children = children().lock().unwrap_or_else(|e| e.into_inner());
                        children.stopping = true;
                        for &group in &children.groups {
                            signal_group(group, signal_kind);
                        }
                    }
                    if engine {
                        std::thread::sleep(Duration::from_secs(2));
                        let children = children().lock().unwrap_or_else(|e| e.into_inner());
                        for &group in &children.groups {
                            signal_group(group, Signal::SIGKILL);
                        }
                        std::process::exit(128 + signal);
                    }
                }
            })?;
        Ok(Self {
            handle,
            thread: Some(thread),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SignalRecorder;
    impl SignalRecorder {
        fn new() -> Self {
            SIGNAL_CALLS.with(|calls| calls.set(Some(0)));
            Self
        }
        fn count(&self) -> usize {
            SIGNAL_CALLS.with(|calls| calls.get().unwrap())
        }
    }
    impl Drop for SignalRecorder {
        fn drop(&mut self) {
            SIGNAL_CALLS.with(|calls| calls.set(None));
        }
    }

    #[test]
    fn completed_std_child_group_drop_sends_no_signal() {
        let recorder = SignalRecorder::new();
        let (mut child, mut group) = spawn_std(Command::new("sh").args(["-c", "exit 0"])).unwrap();
        assert!(wait_std(&mut child, &mut group).unwrap().success());
        drop(group);
        assert_eq!(recorder.count(), 0);
        let result = output_std(Command::new("sh").args(["-c", "printf complete"])).unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, b"complete");
        assert_eq!(recorder.count(), 0);
    }

    #[tokio::test]
    async fn completed_async_output_group_drop_sends_no_signal() {
        let recorder = SignalRecorder::new();
        let result = output(
            tokio::process::Command::new("sh")
                .args(["-c", "printf complete"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .await
        .unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, b"complete");
        assert_eq!(recorder.count(), 0);
    }

    #[tokio::test]
    async fn reaped_leader_capture_cancellation_sends_no_signal() {
        use std::future::Future;
        use std::task::Poll;
        let recorder = SignalRecorder::new();
        let (child, group) = spawn(
            tokio::process::Command::new("sh")
                .args(["-c", "sleep 0.5 & exit 0"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        let pgid = group.pgid;
        let mut capture = Box::pin(wait_with_output(child, group));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        std::future::poll_fn(|cx| {
            let result = capture.as_mut().poll(cx);
            if result.is_ready() || !children().lock().unwrap().groups.contains(&pgid) {
                return Poll::Ready(());
            }
            assert!(std::time::Instant::now() < deadline, "leader not reaped");
            cx.waker().wake_by_ref();
            Poll::Pending
        })
        .await;
        drop(capture);
        assert_eq!(recorder.count(), 0);
    }

    #[test]
    fn unreaped_group_drop_still_signals() {
        let recorder = SignalRecorder::new();
        // The hook intercepts killpg: no signal is sent to this synthetic PID.
        drop(ChildGroup {
            pgid: i32::MAX,
            reaped: false,
        });
        assert_eq!(recorder.count(), 1);
    }
}

impl Drop for SignalForwarder {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        children()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopping = false;
    }
}
