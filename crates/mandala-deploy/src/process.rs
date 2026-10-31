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

/// Kills remaining group members on cancellation, early return or normal exit.
/// Spawn and registration share the signal-forwarder's lock, closing the race
/// between starting a process and recording its PID.
#[derive(Debug)]
pub struct ChildGroup(i32);

impl Drop for ChildGroup {
    fn drop(&mut self) {
        let mut children = children().lock().unwrap_or_else(|e| e.into_inner());
        let _ = killpg(Pid::from_raw(self.0), Signal::SIGKILL);
        children.groups.remove(&self.0);
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
    Ok((child, ChildGroup(pid)))
}

/// Like Command::output, for commands whose stdout/stderr should be captured.
pub fn output_std(command: &mut Command) -> io::Result<Output> {
    let (child, _group) = spawn_std(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )?;
    child.wait_with_output()
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
    Ok((child, ChildGroup(pid)))
}

pub async fn wait_with_output(
    child: tokio::process::Child,
    _group: ChildGroup,
) -> io::Result<Output> {
    child.wait_with_output().await
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
                            let _ = killpg(Pid::from_raw(group), signal_kind);
                        }
                    }
                    if engine {
                        std::thread::sleep(Duration::from_secs(2));
                        let children = children().lock().unwrap_or_else(|e| e.into_inner());
                        for &group in &children.groups {
                            let _ = killpg(Pid::from_raw(group), Signal::SIGKILL);
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
