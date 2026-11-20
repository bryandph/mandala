//! Host-local persistent election rendezvous. No discovery authority lives here.
//!
//! POSIX F_SETLK locks belong to a process (not an open file description),
//! are not inherited by fork children, and are released by ANY close of the
//! inode in that process. Therefore the keyed async mutex is taken BEFORE
//! opening the inode and the sole descriptor stays private to the lease.
use crate::{AcquireOptions, ContextIdentity};
use std::collections::HashMap;
use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

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
    /// Known remote/FUSE filesystem or a non-local Darwin mount.
    UnqualifiedFilesystem(String),
}

impl std::fmt::Display for CoordinationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsafe(s) => write!(f, "unsafe context coordination path: {s}"),
            Self::UnqualifiedFilesystem(name) => {
                write!(f, "unqualified filesystem for context coordination: {name}")
            }
            Self::Home(e) => write!(f, "cannot resolve stable account home: {e}"),
            Self::Unavailable(e) => write!(f, "context coordination unavailable: {e}"),
            Self::Unsupported(e) => write!(f, "host-local POSIX record locking unsupported: {e}"),
        }
    }
}
impl std::error::Error for CoordinationError {}
impl From<io::Error> for CoordinationError {
    fn from(e: io::Error) -> Self {
        Self::Unavailable(e)
    }
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
        let rc = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if rc == libc::ERANGE && size < 1024 * 1024 {
            size *= 2;
            continue;
        }
        if rc != 0 {
            return Err(CoordinationError::Home(io::Error::from_raw_os_error(rc)));
        }
        if result.is_null() {
            return Err(CoordinationError::Home(io::Error::new(
                io::ErrorKind::NotFound,
                "account has no home",
            )));
        }
        // SAFETY: successful getpwuid_r initialized entry and its NUL-terminated
        // pw_dir lives in buffer until after the owned PathBuf is constructed.
        let entry = unsafe { entry.assume_init() };
        if entry.pw_dir.is_null() {
            return Err(unsafe_path("account home is null"));
        }
        let home = PathBuf::from(OsStr::from_bytes(
            unsafe { CStr::from_ptr(entry.pw_dir) }.to_bytes(),
        ));
        if !home.is_absolute() {
            return Err(unsafe_path("account home is not absolute"));
        }
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
    let fd = unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            libc::c_uint::from(mode),
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn stat_at(dir: &File, name: &CStr) -> io::Result<libc::stat> {
    let mut st = std::mem::MaybeUninit::uninit();
    // SAFETY: valid descriptor, C string and output storage. Never follow links.
    if unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            st.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { st.assume_init() })
}

fn same_entry(parent: &File, name: &CStr, file: &File) -> Result<(), CoordinationError> {
    let path = stat_at(parent, name)?;
    let opened = file.metadata()?;
    if stat_inode(&path) != (opened.dev(), opened.ino()) {
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
        for (parent, name, child) in &self.chain {
            same_entry(parent, name, child)?;
        }
        Ok(())
    }
}

/// Walk descriptor-relative from /, rejecting every symlink. Only the
/// default suffix beneath account home may be created. Existing shared OS
/// ancestors can be root-owned; beneath account home all entries must be
/// user-owned and not writable by other users. A sticky system temp ancestor
/// is allowed for injected isolated fixtures, never as a fallback root.
fn root_at(path: &Path, create_below: Option<&Path>) -> Result<Root, CoordinationError> {
    if !path.is_absolute() {
        return Err(unsafe_path("root must be absolute"));
    }
    let mut current = open_at(libc::AT_FDCWD, c"/", libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
    let mut walked = PathBuf::from("/");
    let mut chain = Vec::new();
    let uid = unsafe { libc::geteuid() };
    // A private user namespace can report the host's root owner as the
    // overflow uid. Anchor trusted OS ancestors to /'s observed owner,
    // while the coordination root itself must always be current-user owned.
    let system_uid = current.metadata()?.uid();
    for component in path.components() {
        let Component::Normal(name) = component else {
            if component == Component::RootDir {
                continue;
            }
            return Err(unsafe_path("dot or parent components are not permitted"));
        };
        walked.push(name);
        let name = c_name(name)?;
        let may_create =
            create_below.is_some_and(|home| walked.starts_with(home) && walked != home);
        let mut created = false;
        let child = match open_at(
            current.as_raw_fd(),
            &name,
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        ) {
            Ok(child) => child,
            Err(e) if e.kind() == io::ErrorKind::NotFound && may_create => {
                // Fail closed even in user namespaces/capability-bearing test
                // processes that could otherwise bypass missing write bits.
                if current.metadata()?.mode() & 0o200 == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "default coordination parent is not owner-writable",
                    )
                    .into());
                }
                // SAFETY: parent is securely opened and name is one component.
                if unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) } == -1 {
                    let e = io::Error::last_os_error();
                    if e.kind() != io::ErrorKind::AlreadyExists {
                        return Err(e.into());
                    }
                } else {
                    created = true;
                }
                open_at(
                    current.as_raw_fd(),
                    &name,
                    libc::O_RDONLY | libc::O_DIRECTORY,
                    0,
                )?
            }
            Err(e) => return Err(e.into()),
        };
        let meta = child.metadata()?;
        let is_root = walked == path;
        let private = create_below.is_some_and(|home| walked.starts_with(home));
        let owner_ok = directory_owner_safe(meta.uid(), uid, system_uid, private, is_root);
        let sticky_system =
            !private && !is_root && meta.uid() == system_uid && meta.mode() & 0o1000 != 0;
        if !meta.is_dir()
            || !owner_ok
            || (meta.mode() & 0o022 != 0 && !sticky_system)
            || ((is_root || created) && meta.mode() & 0o7777 != 0o700)
        {
            return Err(unsafe_path(format!(
                "directory ownership/mode/type at {}",
                walked.display()
            )));
        }
        same_entry(&current, &name, &child)?;
        // Duplicating directory descriptors is safe: never a lock-file fd.
        chain.push((current, name, child.try_clone()?));
        current = child;
    }
    let root = Root {
        path: path.to_path_buf(),
        dir: current,
        chain,
    };
    root.verify()?;
    verify_filesystem(&root.dir)?;
    Ok(root)
}

fn directory_owner_safe(owner: u32, current: u32, system: u32, private: bool, root: bool) -> bool {
    owner == current || (!private && !root && owner == system)
}

// Values from Linux include/uapi/linux/magic.h. Prefer libc's exported
// constants on Linux; fallback values let both native test suites exercise
// the complete classifier, without making Linux's local-FS set an allowlist.
macro_rules! filesystem_magic {
    ($name:ident, $libc_name:ident, $value:expr) => {
        #[cfg(target_os = "linux")]
        const $name: u64 = libc::$libc_name as u64;
        #[cfg(not(target_os = "linux"))]
        const $name: u64 = $value;
    };
}
filesystem_magic!(NFS_MAGIC, NFS_SUPER_MAGIC, 0x6969);
filesystem_magic!(SMB_MAGIC, SMB_SUPER_MAGIC, 0x517b);
filesystem_magic!(FUSE_MAGIC, FUSE_SUPER_MAGIC, 0x65735546);
filesystem_magic!(AFS_MAGIC, AFS_SUPER_MAGIC, 0x5346414f);
filesystem_magic!(CODA_MAGIC, CODA_SUPER_MAGIC, 0x73757245);
filesystem_magic!(NCP_MAGIC, NCP_SUPER_MAGIC, 0x564c);
// These magic.h constants are not exported by our libc version.
const CIFS_MAGIC: u64 = 0xff534d42; // CIFS_MAGIC_NUMBER
const SMB2_MAGIC: u64 = 0xfe534d42; // SMB2_MAGIC_NUMBER
const V9FS_MAGIC: u64 = 0x01021997; // V9FS_MAGIC (9p / WSL drvfs)
const CEPH_MAGIC: u64 = 0x00c36400; // CEPH_SUPER_MAGIC
const KERNEL_AFS_MAGIC: u64 = 0x6b414653; // AFS_FS_MAGIC (in-kernel AFS)
const REMOTE_FILESYSTEMS: [u64; 11] = [
    NFS_MAGIC,
    SMB_MAGIC,
    CIFS_MAGIC,
    SMB2_MAGIC,
    V9FS_MAGIC,
    FUSE_MAGIC,
    CEPH_MAGIC,
    AFS_MAGIC,
    KERNEL_AFS_MAGIC,
    CODA_MAGIC,
    NCP_MAGIC,
];

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
fn linux_filesystem_is_local(kind: u64) -> bool {
    !REMOTE_FILESYSTEMS.contains(&kind)
}
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn mac_filesystem_is_local(flags: u64) -> bool {
    #[cfg(target_os = "macos")]
    let local = libc::MNT_LOCAL as u64;
    #[cfg(not(target_os = "macos"))]
    let local = 0x1000; // Darwin MNT_LOCAL
    flags & local != 0
}

fn verify_filesystem(root: &File) -> Result<(), CoordinationError> {
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: live opened directory descriptor and valid output storage.
    if unsafe { libc::fstatfs(root.as_raw_fd(), stat.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error().into());
    }
    let stat = unsafe { stat.assume_init() };
    #[cfg(target_os = "macos")]
    let (qualified, description) = (
        mac_filesystem_is_local(u64::from(stat.f_flags)),
        format!("Darwin mount flags {:#x} lack MNT_LOCAL", stat.f_flags),
    );
    #[cfg(target_os = "linux")]
    let (qualified, description) = (
        linux_filesystem_is_local(stat.f_type as u64),
        format!("known network/FUSE filesystem type {:#x}", stat.f_type),
    );
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let (qualified, description) = (
        false,
        "platform has no qualified filesystem policy".to_string(),
    );
    if qualified {
        Ok(())
    } else {
        Err(CoordinationError::UnqualifiedFilesystem(description))
    }
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct Key {
    device: u64,
    inode: u64,
    identity: String,
}
type Inode = (u64, u64);
// libc's dev_t/ino_t widths and signedness differ between Linux and Darwin.
#[allow(clippy::unnecessary_cast)]
fn stat_inode(meta: &libc::stat) -> Inode {
    (meta.st_dev as u64, meta.st_ino as u64)
}
#[derive(Default)]
struct Registry {
    mutexes: HashMap<Key, Arc<AsyncMutex<()>>>,
    // Each entry belongs to one ElectionLease. Unexpected alias descriptors
    // are transferred to that lease, never closed while its lock is held.
    owned: HashMap<Inode, Vec<File>>,
}
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}
fn mutex_for(key: Key) -> Arc<AsyncMutex<()>> {
    let mut registry = registry().lock().expect("election registry poisoned");
    // Retain entries for process lifetime; removing a live weak entry is not
    // necessary and must never split one coordination identity.
    Arc::clone(
        registry
            .mutexes
            .entry(key)
            .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
    )
}

/// Sole owner of the lock descriptor, then the keyed mutex. Field order is
/// deliberate: close the process-lock fd before releasing same-process access.
/// Never clone, expose, diagnostically open, unlink or replace this inode.
pub struct ElectionLease {
    _file: Option<File>,
    _mutex: OwnedMutexGuard<()>,
    inode: Inode,
}
impl Drop for ElectionLease {
    fn drop(&mut self) {
        let mut registry = registry().lock().expect("election registry poisoned");
        let deferred = registry
            .owned
            .remove(&self.inode)
            .expect("registered lease inode");
        // Serialize release with every helper open. First release this owner's
        // record lock, then close quarantined alias fds, then the keyed mutex.
        drop(self._file.take());
        drop(deferred);
    }
}

/// Nonblocking attempt. `Ok(None)` is contention, never permission to lead.
/// Roots are validated on every attempt, but the lock inode is never opened
/// until the keyed mutex is owned.
///
/// # Errors
/// Unsafe paths, missing injected roots, creation/access failures and unsupported locks.
/// Validate or securely create the configured root without opening any lock
/// inode. Followers need no lease, but unsafe explicit options still fail.
///
/// # Errors
/// See [`try_acquire`].
pub fn validate_root(options: &AcquireOptions) -> Result<(), CoordinationError> {
    match &options.coordination_root {
        Some(path) => root_at(path, None).map(|_| ()),
        None => {
            let home = account_home()?;
            root_at(
                &home.join(".local/state/mandala/context-election"),
                Some(&home),
            )
            .map(|_| ())
        }
    }
}

pub fn try_acquire(
    identity: &ContextIdentity,
    options: &AcquireOptions,
) -> Result<Option<ElectionLease>, CoordinationError> {
    let (path, home) = match &options.coordination_root {
        Some(path) => (path.clone(), None),
        None => {
            let home = account_home()?;
            (
                home.join(".local/state/mandala/context-election"),
                Some(home),
            )
        }
    };
    try_at(identity.key(), &path, home.as_deref())
}

fn lock_metadata_safe(regular: bool, owner: u32, current: u32, mode: u32, links: u64) -> bool {
    regular && owner == current && mode & 0o7777 == 0o600 && links == 1
}

fn try_at(
    identity: &str,
    path: &Path,
    home: Option<&Path>,
) -> Result<Option<ElectionLease>, CoordinationError> {
    try_at_observed(identity, path, home, || {})
}

fn try_at_observed(
    identity: &str,
    path: &Path,
    home: Option<&Path>,
    before_open: impl FnOnce(),
) -> Result<Option<ElectionLease>, CoordinationError> {
    let root = root_at(path, home)?;
    let meta = root.dir.metadata()?;
    let mutex = mutex_for(Key {
        device: meta.dev(),
        inode: meta.ino(),
        identity: identity.to_owned(),
    });
    let Ok(guard) = mutex.try_lock_owned() else {
        return Ok(None);
    };
    root.verify()?;
    let name = c_name(OsStr::new(&format!("{identity}.lock")))?;
    let mut registry = registry().lock().expect("election registry poisoned");
    match stat_at(&root.dir, &name) {
        Ok(meta) if registry.owned.contains_key(&stat_inode(&meta)) => {
            return Err(unsafe_path(
                "lock path aliases an inode already owned by this process",
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    before_open();
    // O_NONBLOCK prevents a malicious FIFO from blocking before validation.
    let file = open_at(
        root.dir.as_raw_fd(),
        &name,
        libc::O_RDWR | libc::O_CREAT | libc::O_NONBLOCK,
        0o600,
    )?;
    let meta = file.metadata()?;
    let inode = (meta.dev(), meta.ino());
    if let Some(deferred) = registry.owned.get_mut(&inode) {
        // Same-user replacement raced the pre-open check. Closing this fd
        // here would silently release another live lease's F_SETLK lock.
        deferred.push(file);
        return Err(unsafe_path(
            "opened lock aliases an owned inode after replacement; descriptor retained by existing lease",
        ));
    }
    if !lock_metadata_safe(
        meta.is_file(),
        meta.uid(),
        unsafe { libc::geteuid() },
        meta.mode(),
        meta.nlink(),
    ) {
        return Err(unsafe_path(format!(
            "lock ownership/mode/type/link count at {}",
            root.path.display()
        )));
    }
    same_entry(&root.dir, &name, &file)?;
    root.verify()?;
    // SAFETY: zeroed flock's fields are fully set for a whole-file write lock.
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = libc::F_WRLCK as _;
    lock.l_whence = libc::SEEK_SET as _;
    lock.l_start = 0;
    lock.l_len = 0;
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &lock) } == -1 {
        return classify_lock_error(io::Error::last_os_error());
    }
    same_entry(&root.dir, &name, &file)?;
    root.verify()?;
    registry.owned.insert(inode, Vec::new());
    Ok(Some(ElectionLease {
        _file: Some(file),
        _mutex: guard,
        inode,
    }))
}

fn classify_lock_error(e: io::Error) -> Result<Option<ElectionLease>, CoordinationError> {
    match e.raw_os_error() {
        Some(libc::EACCES | libc::EAGAIN) => Ok(None),
        Some(libc::ENOSYS | libc::ENOTSUP | libc::ENOLCK | libc::EINVAL) => {
            Err(CoordinationError::Unsupported(e))
        }
        _ => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    fn home(tag: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!(
            "election-home-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        home.canonicalize().unwrap()
    }

    #[test]
    fn isolated_default_home_creates_private_parents_and_retains_inode() {
        let home = home("create");
        let root = home.join(".local/state/mandala/context-election");
        let lease = try_at("fixture", &root, Some(&home)).unwrap().unwrap();
        for suffix in [
            ".local",
            ".local/state",
            ".local/state/mandala",
            ".local/state/mandala/context-election",
        ] {
            let meta = std::fs::symlink_metadata(home.join(suffix)).unwrap();
            assert!(meta.is_dir());
            assert_eq!(meta.uid(), unsafe { libc::geteuid() });
            assert_eq!(meta.mode() & 0o7777, 0o700);
        }
        let before = std::fs::metadata(root.join("fixture.lock")).unwrap().ino();
        assert_ne!(
            unsafe { libc::fcntl(lease._file.as_ref().unwrap().as_raw_fd(), libc::F_GETFD) }
                & libc::FD_CLOEXEC,
            0
        );
        drop(lease);
        let lease = try_at("fixture", &root, Some(&home)).unwrap().unwrap();
        assert_eq!(
            before,
            std::fs::metadata(root.join("fixture.lock")).unwrap().ino()
        );
        drop(lease);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn existing_unsafe_parent_and_default_creation_failure_fail_closed() {
        let home = home("parent");
        let local = home.join(".local");
        std::fs::create_dir(&local).unwrap();
        std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o777)).unwrap();
        let root = home.join(".local/state/mandala/context-election");
        assert!(try_at("fixture", &root, Some(&home)).is_err());
        assert!(!root.exists());
        std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert!(try_at("fixture", &root, Some(&home)).is_err());
        assert!(!root.exists());
        std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir(&local).unwrap();
        std::os::unix::fs::symlink(&home, &local).unwrap();
        assert!(try_at("fixture", &root, Some(&home)).is_err());
        std::fs::remove_file(&local).unwrap();
        std::fs::write(&local, []).unwrap();
        assert!(try_at("fixture", &root, Some(&home)).is_err());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn filesystem_policy_refuses_known_remote_types_and_nonlocal_darwin_mounts() {
        for kind in REMOTE_FILESYSTEMS {
            assert!(!linux_filesystem_is_local(kind), "remote magic {kind:#x}");
        }
        // A denylist, not a local-filesystem allowlist: unknown/local types pass.
        for kind in [
            0xef53, 0x9123683e, 0x01021994, 0x794c7630, 0x2fc12fc1, 0x12345678,
        ] {
            assert!(linux_filesystem_is_local(kind));
        }
        assert!(!mac_filesystem_is_local(0));
        assert!(!mac_filesystem_is_local(0x1));
        assert!(mac_filesystem_is_local(0x1000));
        assert!(mac_filesystem_is_local(0x1001));
    }

    #[test]
    fn native_opened_temp_filesystem_passes_admission() {
        let home = home("native-fs");
        let root = root_at(&home, None).unwrap();
        verify_filesystem(&root.dir).unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn process_lock_probe() {
        let Some(path) = std::env::var_os("MANDALA_PROBE_LOCK_PATH") else {
            return;
        };
        let file = File::open(path).unwrap();
        let mut record: libc::flock = unsafe { std::mem::zeroed() };
        record.l_type = libc::F_WRLCK as _;
        record.l_whence = libc::SEEK_SET as _;
        assert_ne!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &mut record) },
            -1
        );
        let held = std::env::var("MANDALA_PROBE_LOCK_HELD").unwrap() == "true";
        assert_eq!(record.l_type != libc::F_UNLCK as libc::c_short, held);
        if held {
            assert!(record.l_pid > 0);
        }
    }

    fn probe_lock(path: &Path, held: bool) {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "election::tests::process_lock_probe",
                "--nocapture",
            ])
            .env("MANDALA_PROBE_LOCK_PATH", path)
            .env("MANDALA_PROBE_LOCK_HELD", held.to_string())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "independent process observes record lock held={held}"
        );
    }

    #[test]
    fn owned_inode_hardlink_and_rename_aliases_never_release_original_lock() {
        for rename in [false, true] {
            let home = home(if rename {
                "rename-alias"
            } else {
                "hardlink-alias"
            });
            let root = home.join("coordination");
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .unwrap();
            let lease = try_at("first", &root, None).unwrap().unwrap();
            let first = root.join("first.lock");
            let second = root.join("second.lock");
            if rename {
                std::fs::rename(&first, &second).unwrap();
            } else {
                std::fs::hard_link(&first, &second).unwrap();
            }
            assert!(matches!(
                try_at("second", &root, None),
                Err(CoordinationError::Unsafe(_))
            ));
            probe_lock(&second, true);
            drop(lease);
            probe_lock(&second, false);
            std::fs::remove_dir_all(home).unwrap();
        }
    }

    #[test]
    fn post_open_alias_race_defers_close_until_original_owner_releases() {
        use std::os::unix::fs::OpenOptionsExt;
        let home = home("deferred-alias");
        let root = home.join("coordination");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let lease = try_at("first", &root, None).unwrap().unwrap();
        let first = root.join("first.lock");
        let second = root.join("second.lock");
        File::options()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&second)
            .unwrap();
        let result = try_at_observed("second", &root, None, || {
            std::fs::rename(&second, root.join("old-second")).unwrap();
            std::fs::rename(&first, &second).unwrap();
        });
        assert!(matches!(result, Err(CoordinationError::Unsafe(_))));
        let deferred = {
            let registry = registry().lock().unwrap();
            let files = registry.owned.get(&lease.inode).unwrap();
            assert_eq!(files.len(), 1);
            files[0].as_raw_fd()
        };
        probe_lock(&second, true);
        drop(lease);
        assert_eq!(
            unsafe { libc::fcntl(deferred, libc::F_GETFD) },
            -1,
            "deferred fd cannot outlive its owner"
        );
        probe_lock(&second, false);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn foreign_metadata_and_unsupported_lock_errors_fail_closed() {
        // Synthetic ownership validation: no privilege or live chown fixture.
        // Native foreign-owner inode coverage requires a privileged fixture.
        assert!(!directory_owner_safe(1001, 1000, 0, false, false));
        assert!(!directory_owner_safe(0, 1000, 0, true, false));
        assert!(!directory_owner_safe(0, 1000, 0, false, true));
        assert!(directory_owner_safe(0, 1000, 0, false, false));
        assert!(directory_owner_safe(1000, 1000, 0, true, true));
        assert!(!lock_metadata_safe(true, 1001, 1000, 0o600, 1));
        assert!(!lock_metadata_safe(true, 1000, 1000, 0o600, 2));
        assert!(lock_metadata_safe(true, 1000, 1000, 0o600, 1));
        for errno in [libc::ENOSYS, libc::ENOTSUP, libc::ENOLCK, libc::EINVAL] {
            assert!(matches!(
                classify_lock_error(io::Error::from_raw_os_error(errno)),
                Err(CoordinationError::Unsupported(_))
            ));
        }
        for errno in [libc::EACCES, libc::EAGAIN] {
            assert!(matches!(
                classify_lock_error(io::Error::from_raw_os_error(errno)),
                Ok(None)
            ));
        }
        assert!(matches!(
            classify_lock_error(io::Error::from_raw_os_error(libc::EIO)),
            Err(CoordinationError::Unavailable(_))
        ));
    }

    #[test]
    fn post_open_root_replacement_is_rejected() {
        let home = home("race");
        let root = home.join("root");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let opened = root_at(&root, None).unwrap();
        std::fs::rename(&root, home.join("old")).unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        assert!(matches!(opened.verify(), Err(CoordinationError::Unsafe(_))));
        std::fs::remove_dir_all(home).unwrap();
    }
}
