//! Host-local persistent election rendezvous. No discovery authority lives here.
//!
//! POSIX F_SETLK locks belong to a process (not an open file description),
//! are not inherited by fork children, and are released by ANY close of the
//! inode in that process. Therefore the keyed async mutex is taken BEFORE
//! opening the inode and the sole descriptor stays private to the lease.
use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use nix::libc;
/// Coordination failures never authorize unlocked leadership.
#[derive(Debug)]
pub enum CoordinationError {
    /// Unsafe ownership/type/permissions, path alias or replacement.
    Unsafe(String),
    /// Account home cannot be resolved consistently.
    Home(io::Error),
    /// Secure creation/open/validation failed; no fallback is attempted.
    Unavailable(io::Error),
    /// The filesystem/kernel does not implement process record locks.
    Unsupported(io::Error),
}

impl std::fmt::Display for CoordinationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsafe(s) => write!(f, "unsafe context coordination path: {s}"),
            Self::Home(e) => write!(f, "cannot resolve stable account home: {e}"),
            Self::Unavailable(e) => write!(f, "context coordination unavailable: {e}"),
            Self::Unsupported(e) => write!(f, "host-local POSIX record locking unsupported: {e}"),
        }
    }
}
impl std::error::Error for CoordinationError {}
impl From<io::Error> for CoordinationError {
    fn from(e: io::Error) -> Self { Self::Unavailable(e) }
}

fn unsafe_path(message: impl Into<String>) -> CoordinationError {
    CoordinationError::Unsafe(message.into())
}
fn c_name(name: &OsStr) -> Result<CString, CoordinationError> {
    CString::new(name.as_bytes()).map_err(|_| unsafe_path("NUL in path"))
}

/// Uses the account database, deliberately ignoring HOME and all XDG/client
/// state overrides. getpwuid_r is reentrant during concurrent acquisitions.
pub fn account_home() -> Result<PathBuf, CoordinationError> {
    let mut size = 4096;
    loop {
        let mut buffer = vec![0_u8; size];
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        // SAFETY: all outputs point at live, appropriately sized storage.
        let rc = unsafe { libc::getpwuid_r(libc::geteuid(), entry.as_mut_ptr(), buffer.as_mut_ptr().cast(), buffer.len(), &mut result) };
        if rc == libc::ERANGE && size < 1024 * 1024 { size *= 2; continue; }
        if rc != 0 { return Err(CoordinationError::Home(io::Error::from_raw_os_error(rc))); }
        if result.is_null() { return Err(CoordinationError::Home(io::Error::new(io::ErrorKind::NotFound, "account has no home"))); }
        // SAFETY: successful getpwuid_r initialized entry and its NUL-terminated
        // pw_dir lives in buffer until after the owned PathBuf is constructed.
        let entry = unsafe { entry.assume_init() };
        if entry.pw_dir.is_null() { return Err(unsafe_path("account home is null")); }
        let home = PathBuf::from(OsStr::from_bytes(unsafe { CStr::from_ptr(entry.pw_dir) }.to_bytes()));
        if !home.is_absolute() { return Err(unsafe_path("account home is not absolute")); }
        return Ok(home);
    }
}

/// Stable layout, not a runtime-dir/state-dir-dependent namespace.
pub fn default_root() -> Result<PathBuf, CoordinationError> {
    Ok(account_home()?.join(".local/state/mandala/context-election"))
}

fn open_at(dir: RawFd, name: &CStr, flags: libc::c_int, mode: libc::mode_t) -> io::Result<File> {
    // SAFETY: name is NUL terminated; ownership of a successful fresh fd is
    // transferred exactly once to File. O_CLOEXEC is atomic at open.
    let fd = unsafe { libc::openat(dir, name.as_ptr(), flags | libc::O_CLOEXEC | libc::O_NOFOLLOW, libc::c_uint::from(mode)) };
    if fd < 0 { Err(io::Error::last_os_error()) } else { Ok(unsafe { File::from_raw_fd(fd) }) }
}

fn stat_at(dir: &File, name: &CStr) -> io::Result<libc::stat> {
    let mut st = std::mem::MaybeUninit::uninit();
    // SAFETY: valid descriptor, C string and output storage. Never follow links.
    if unsafe { libc::fstatat(dir.as_raw_fd(), name.as_ptr(), st.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { st.assume_init() })
}

fn same_entry(parent: &File, name: &CStr, file: &File) -> Result<(), CoordinationError> {
    let path = stat_at(parent, name)?;
    let opened = file.metadata()?;
    if path.st_dev as u64 != opened.dev() || path.st_ino as u64 != opened.ino() {
        return Err(unsafe_path("path replaced during open/validation"));
    }
    Ok(())
}

struct Root {
    path: PathBuf,
    dir: File,
    /// Every parent edge stays open for post-open identity validation.
    chain: Vec<(File, CString, File)>,
}
impl Root {
    fn verify(&self) -> Result<(), CoordinationError> {
        for (parent, name, child) in &self.chain { same_entry(parent, name, child)?; }
        Ok(())
    }
}

/// Walk descriptor-relative from /, rejecting every symlink. Only the
/// default suffix beneath account home may be created. Existing shared OS
/// ancestors can be root-owned; beneath account home all entries must be
/// user-owned and not writable by other users. A sticky system temp ancestor
/// is allowed for injected isolated fixtures, never as a fallback root.
fn root_at(path: &Path, create_below: Option<&Path>) -> Result<Root, CoordinationError> {
    if !path.is_absolute() { return Err(unsafe_path("root must be absolute")); }
    let mut current = open_at(libc::AT_FDCWD, c"/", libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
    let mut walked = PathBuf::from("/");
    let mut chain = Vec::new();
    let uid = unsafe { libc::geteuid() };
    for component in path.components() {
        let Component::Normal(name) = component else {
            if component == Component::RootDir { continue; }
            return Err(unsafe_path("dot or parent components are not permitted"));
        };
        walked.push(name);
        let name = c_name(name)?;
        let may_create = create_below.is_some_and(|home| walked.starts_with(home) && walked != home);
        let child = match open_at(current.as_raw_fd(), &name, libc::O_RDONLY | libc::O_DIRECTORY, 0) {
            Ok(child) => child,
            Err(e) if e.kind() == io::ErrorKind::NotFound && may_create => {
                // SAFETY: parent is securely opened and name is one component.
                if unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) } == -1 {
                    let e = io::Error::last_os_error();
                    if e.kind() != io::ErrorKind::AlreadyExists { return Err(e.into()); }
                }
                open_at(current.as_raw_fd(), &name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?
            }
            Err(e) => return Err(e.into()),
        };
        let meta = child.metadata()?;
        let is_root = walked == path;
        let private = create_below.is_some_and(|home| walked.starts_with(home));
        let owner_ok = meta.uid() == uid || (!private && !is_root && meta.uid() == 0);
        let sticky_system = !private && !is_root && meta.uid() == 0 && meta.mode() & 0o1000 != 0;
        if !meta.is_dir() || !owner_ok || (meta.mode() & 0o022 != 0 && !sticky_system)
            || (is_root && meta.mode() & 0o7777 != 0o700) {
            return Err(unsafe_path(format!("directory ownership/mode/type at {}", walked.display())));
        }
        same_entry(&current, &name, &child)?;
        // Duplicating directory descriptors is safe: never a lock-file fd.
        chain.push((current, name, child.try_clone()?));
        current = child;
    }
    let root = Root { path: path.to_path_buf(), dir: current, chain };
    root.verify()?;
    Ok(root)
}

