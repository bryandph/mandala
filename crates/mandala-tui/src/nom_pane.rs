//! `nom --json` hosted on a pane-sized pty, vt100-emulated into the pane —
//! the `nom.py` port (spike task 1.3).
//!
//! Shape (load-bearing, from the Python original):
//! - The child's **stdin is a pipe** — we feed it raw `@nix {...}`
//!   internal-json lines. Only stdout/stderr sit on the pty; putting stdin
//!   on the pty would echo the fed json into the emulated screen. This is
//!   why the pane uses raw `openpty` (via the `nix` crate) instead of
//!   portable-pty, whose `spawn_command` hard-wires all three fds to the
//!   pty slave.
//! - A drain thread reads the pty master and feeds a shared
//!   `vt100::Parser`; rendering locks the parser briefly and blits the
//!   screen through tui-term's `PseudoTerminal`.
//! - Spawn is deferred until the first input line, so long evaluation does
//!   not trigger nom's no-input warning. Early lines flush after spawn.
//! - [`NomPane::finish`] closes stdin (EOF) so nom draws its final summary.
//! - nom absent from PATH → a dim notice, never a crash (the summary pane
//!   still tracks the build).
//! - Resize propagates to the emulator, the pty (`TIOCSWINSZ`), and the
//!   child (`SIGWINCH`).

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use nix::pty::{Winsize, openpty};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use tui_term::widget::{Cursor, PseudoTerminal};

nix::ioctl_write_ptr_bad!(tiocswinsz, nix::libc::TIOCSWINSZ, Winsize);
nix::ioctl_read_bad!(tiocgwinsz, nix::libc::TIOCGWINSZ, Winsize);

/// Floor dimensions, mirroring `nom.py`'s `_dims` clamp: nom on a sliver
/// of a pane draws garbage, so never hand it less than this.
const MIN_ROWS: u16 = 10;
const MIN_COLS: u16 = 40;

fn winsize(rows: u16, cols: u16) -> Winsize {
    Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

/// The pane. Feed internal-json lines via [`feed`](Self::feed); everything
/// else is lifecycle.
pub struct NomPane {
    parser: Option<Arc<Mutex<vt100::Parser>>>,
    /// Set by the drain thread whenever pty output changed the screen; the
    /// loop's dirty-flag render path polls it via [`take_dirty`](Self::take_dirty).
    dirty: Arc<AtomicBool>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    master: Option<OwnedFd>,
    failed: Option<String>,
    /// `Some` until spawn: lines fed early buffer here (nom.py parity).
    pending: Option<Vec<String>>,
    /// Requested renderer and latest dimensions while waiting for real input.
    launch: Option<(String, Vec<String>, u16, u16)>,
    finished: bool,
}

impl Default for NomPane {
    fn default() -> Self {
        Self::new()
    }
}

impl NomPane {
    pub fn new() -> Self {
        Self {
            parser: None,
            dirty: Arc::new(AtomicBool::new(false)),
            child: None,
            stdin: None,
            master: None,
            failed: None,
            pending: Some(Vec::new()),
            launch: None,
            finished: false,
        }
    }

    /// Prepare `nom --json`; create its PTY when the first Nix record arrives.
    pub fn spawn(&mut self, rows: u16, cols: u16) {
        self.spawn_cmd("nom", &["--json"], rows, cols);
    }

    /// Spawn an arbitrary command on the pty — the test seam (a stand-in
    /// script proves the pty+emulator path where nom isn't installed).
    pub fn spawn_cmd(&mut self, cmd: &str, args: &[&str], rows: u16, cols: u16) {
        if self.finished || self.failed.is_some() || self.child.is_some() {
            return;
        }
        self.launch = Some((
            cmd.to_string(),
            args.iter().map(|arg| (*arg).to_string()).collect(),
            rows,
            cols,
        ));
        self.dirty.store(true, Ordering::Release);
        self.start_if_ready();
    }

    fn start_if_ready(&mut self) {
        if self.pending.as_ref().is_none_or(Vec::is_empty) {
            return;
        }
        if let Some((cmd, args, rows, cols)) = self.launch.take() {
            self.spawn_now(&cmd, &args, rows, cols);
        }
    }

    fn spawn_now(&mut self, cmd: &str, args: &[String], rows: u16, cols: u16) {
        let rows = rows.max(MIN_ROWS);
        let cols = cols.max(MIN_COLS);
        let ws = winsize(rows, cols);
        let pty = match openpty(Some(&ws), None) {
            Ok(pty) => pty,
            Err(e) => {
                self.fail(format!(
                    "pty unavailable ({e}) — the summary pane still tracks the build"
                ));
                return;
            }
        };
        let slave_clone = match pty.slave.try_clone() {
            Ok(fd) => fd,
            Err(e) => {
                self.fail(format!(
                    "pty unavailable ({e}) — the summary pane still tracks the build"
                ));
                return;
            }
        };
        let child = Command::new(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(slave_clone))
            .stderr(Stdio::from(pty.slave))
            .env("TERM", "xterm-256color")
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(e) => {
                // The load-bearing fallback (nom.py parity): a notice, not
                // a crash — the summary pane still tracks the build.
                self.fail(format!(
                    "{cmd} unavailable ({e}) — the summary pane still tracks the build"
                ));
                return;
            }
        };
        // Parent side: both slave fds were moved into the child's stdio and
        // are closed here; we keep only the master.
        self.stdin = child.stdin.take();
        self.child = Some(child);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        self.parser = Some(parser.clone());

        match pty.master.try_clone() {
            Ok(read_fd) => {
                let dirty = self.dirty.clone();
                std::thread::spawn(move || drain_pty(read_fd, parser, dirty));
            }
            Err(e) => {
                self.fail(format!(
                    "pty unavailable ({e}) — the summary pane still tracks the build"
                ));
                return;
            }
        }
        self.master = Some(pty.master);

        // Flush the pre-spawn backlog in order.
        if let Some(pending) = self.pending.take() {
            for line in pending {
                self.write_line(&line);
            }
        }
    }

    fn fail(&mut self, notice: String) {
        self.dirty.store(true, Ordering::Release);
        self.launch = None;
        self.failed = Some(notice);
        self.pending = None;
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.master = None;
        self.parser = None;
    }

    /// One raw `@nix {...}` line into nom's stdin (buffered until spawn).
    pub fn feed(&mut self, line: &str) {
        if self.finished {
            return;
        }
        if let Some(pending) = self.pending.as_mut() {
            pending.push(line.to_string());
            self.start_if_ready();
            return;
        }
        self.write_line(line);
    }

    fn write_line(&mut self, line: &str) {
        if let Some(stdin) = self.stdin.as_mut() {
            // Broken pipe = nom died; the drain thread already stopped and
            // the last emulated screen stays up. Never propagate.
            let _ = stdin
                .write_all(line.as_bytes())
                .and_then(|()| stdin.write_all(b"\n"))
                .and_then(|()| stdin.flush());
        }
    }

    /// EOF nom's stdin so it draws its final summary and exits.
    pub fn finish(&mut self) {
        self.finished = true;
        self.launch = None;
        self.pending = None;
        self.stdin = None;
        self.dirty.store(true, Ordering::Release);
    }

    /// Propagate a pane resize: emulator screen, pty winsize, SIGWINCH.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        if let Some((_, _, pending_rows, pending_cols)) = self.launch.as_mut() {
            *pending_rows = rows;
            *pending_cols = cols;
        }
        let rows = rows.max(MIN_ROWS);
        let cols = cols.max(MIN_COLS);
        if let Some(parser) = self.parser.as_ref() {
            parser
                .lock()
                .expect("nom pane emulator lock poisoned")
                .screen_mut()
                .set_size(rows, cols);
        }
        if let Some(master) = self.master.as_ref() {
            let ws = winsize(rows, cols);
            let _ = unsafe { tiocswinsz(master.as_raw_fd(), &ws) };
        }
        if let Some(child) = self.child.as_mut()
            && matches!(child.try_wait(), Ok(None))
        {
            let _ = kill(Pid::from_raw(child.id() as i32), Signal::SIGWINCH);
        }
    }

    /// Whether pty output changed the screen since the last call — the
    /// loop's dirty-flag hook.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    /// The emulated screen's plain-text contents (test/observability seam).
    pub fn screen_contents(&self) -> Option<String> {
        self.parser.as_ref().map(|p| {
            p.lock()
                .expect("nom pane emulator lock poisoned")
                .screen()
                .contents()
        })
    }

    /// The emulator screen's current size, `(rows, cols)` (test seam).
    pub fn emulator_size(&self) -> Option<(u16, u16)> {
        self.parser.as_ref().map(|p| {
            p.lock()
                .expect("nom pane emulator lock poisoned")
                .screen()
                .size()
        })
    }

    /// The pty's actual winsize as the kernel reports it (test seam).
    pub fn pty_size(&self) -> Option<(u16, u16)> {
        let master = self.master.as_ref()?;
        let mut ws = winsize(0, 0);
        unsafe { tiocgwinsz(master.as_raw_fd(), &mut ws) }.ok()?;
        Some((ws.ws_row, ws.ws_col))
    }

    /// The fallback notice, when spawning failed.
    pub fn failure(&self) -> Option<&str> {
        self.failed.as_deref()
    }
}

/// Unmount semantics (nom.py parity): EOF stdin, terminate the child,
/// close the master.
impl Drop for NomPane {
    fn drop(&mut self) {
        self.finish();
        if let Some(mut child) = self.child.take() {
            if matches!(child.try_wait(), Ok(None)) {
                let _ = kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM);
                // Give it a beat to exit on SIGTERM, then make sure it is
                // reaped — a spike-scoped bounded wait, not a supervisor.
                for _ in 0..20 {
                    if !matches!(child.try_wait(), Ok(None)) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                if matches!(child.try_wait(), Ok(None)) {
                    let _ = child.kill();
                }
            }
            let _ = child.wait();
        }
        // Dropping the master closes our side; the drain thread's clone
        // unblocks when the child side goes away.
        self.master = None;
    }
}

impl Widget for &NomPane {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if let Some(notice) = self.failed.as_deref() {
            Paragraph::new(notice)
                .style(Style::new().add_modifier(Modifier::DIM))
                .wrap(Wrap { trim: false })
                .render(area, buf);
            return;
        }
        let Some(parser) = self.parser.as_ref() else {
            let message = if self.finished {
                "No Nix build output received. See the summary and run log."
            } else {
                "Waiting for Nix output — evaluation may take several minutes."
            };
            Paragraph::new(message)
                .style(Style::new().add_modifier(Modifier::DIM))
                .wrap(Wrap { trim: false })
                .render(area, buf);
            return;
        };
        let parser = parser.lock().expect("nom pane emulator lock poisoned");
        let mut cursor = Cursor::default();
        cursor.hide();
        PseudoTerminal::new(parser.screen())
            .cursor(cursor)
            .render(area, buf);
    }
}

/// Drain thread: pty master → emulator. Exits on EOF/EIO (child gone).
fn drain_pty(fd: OwnedFd, parser: Arc<Mutex<vt100::Parser>>, dirty: Arc<AtomicBool>) {
    let mut file = File::from(fd);
    let mut buf = [0u8; 8192];
    loop {
        match file.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                parser
                    .lock()
                    .expect("nom pane emulator lock poisoned")
                    .process(&buf[..n]);
                dirty.store(true, Ordering::Release);
            }
        }
    }
}
