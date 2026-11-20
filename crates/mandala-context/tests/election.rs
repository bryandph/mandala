//! Native process-lock, inode and fork-before-exec gates. All roots are injected.
use mandala_context::{AcquireOptions, ContextIdentity, election};
use nix::libc;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

fn fixture(tag: &str) -> (PathBuf, ContextIdentity, AcquireOptions) {
    let path = std::env::temp_dir().join(format!(
        "mandala-election-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .unwrap();
    let path = path.canonicalize().unwrap();
    let flake = path.join("flake");
    std::fs::create_dir(&flake).unwrap();
    let root = path.join("coordination");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    (
        path,
        ContextIdentity::for_flake(flake).unwrap(),
        AcquireOptions {
            coordination_root: Some(root),
            ..AcquireOptions::default()
        },
    )
}
fn lock_path(id: &ContextIdentity, options: &AcquireOptions) -> PathBuf {
    options
        .coordination_root
        .as_ref()
        .unwrap()
        .join(format!("{}.lock", id.key()))
}

#[test]
fn same_process_sessions_aliases_and_independent_flakes() {
    let (path, id, options) = fixture("sessions");
    let lease = election::try_acquire(&id, &options).unwrap().unwrap();
    assert!(election::try_acquire(&id, &options).unwrap().is_none());
    let alias = ContextIdentity::for_flake(path.join("flake/../flake")).unwrap();
    assert_eq!(id.key(), alias.key());
    let mut alias_options = options.clone();
    alias_options.coordination_root = Some(
        options
            .coordination_root
            .as_ref()
            .unwrap()
            .join("../coordination"),
    );
    // Parent traversal is rejected rather than introducing another namespace.
    assert!(election::try_acquire(&alias, &alias_options).is_err());
    assert!(election::try_acquire(&alias, &options).unwrap().is_none());
    let other = path.join("other");
    std::fs::create_dir(&other).unwrap();
    let other = ContextIdentity::for_flake(other).unwrap();
    let independent = election::try_acquire(&other, &options).unwrap().unwrap();
    drop(independent);
    drop(lease);
    assert!(election::try_acquire(&id, &options).unwrap().is_some());
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn inode_is_persistent_and_lock_file_private() {
    let (path, id, options) = fixture("inode");
    let lease = election::try_acquire(&id, &options).unwrap().unwrap();
    let before = std::fs::symlink_metadata(lock_path(&id, &options)).unwrap();
    assert_eq!(before.mode() & 0o7777, 0o600);
    assert!(before.is_file());
    drop(lease);
    let lease = election::try_acquire(&id, &options).unwrap().unwrap();
    let after = std::fs::symlink_metadata(lock_path(&id, &options)).unwrap();
    assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
    drop(lease);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn unsafe_roots_and_lock_files_fail_closed() {
    let (path, id, options) = fixture("security");
    let missing = path.join("missing");
    assert!(
        election::try_acquire(
            &id,
            &AcquireOptions {
                coordination_root: Some(missing.clone()),
                ..AcquireOptions::default()
            }
        )
        .is_err()
    );
    assert!(!missing.exists());
    let root = options.coordination_root.as_ref().unwrap();
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(election::try_acquire(&id, &options).is_err());
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let link = path.join("link");
    std::os::unix::fs::symlink(root, &link).unwrap();
    assert!(
        election::try_acquire(
            &id,
            &AcquireOptions {
                coordination_root: Some(link),
                ..AcquireOptions::default()
            }
        )
        .is_err()
    );
    let lock = lock_path(&id, &options);
    std::os::unix::fs::symlink(path.join("foreign"), &lock).unwrap();
    assert!(election::try_acquire(&id, &options).is_err());
    assert!(!path.join("foreign").exists());
    std::fs::remove_file(&lock).unwrap();
    std::fs::create_dir(&lock).unwrap();
    assert!(election::try_acquire(&id, &options).is_err());
    std::fs::remove_dir(&lock).unwrap();
    std::fs::write(&lock, []).unwrap();
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(election::try_acquire(&id, &options).is_err());
    assert_eq!(
        std::fs::metadata(&lock).unwrap().mode() & 0o777,
        0o644,
        "no repair"
    );
    std::fs::remove_dir_all(path).unwrap();
}

fn spawn_holder(
    id: &ContextIdentity,
    options: &AcquireOptions,
    mode: &str,
) -> (Child, Option<i32>) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "subprocess_holder", "--nocapture"])
        .env("MANDALA_LOCK_TEST_FLAKE", id.flake())
        .env(
            "MANDALA_LOCK_TEST_ROOT",
            options.coordination_root.as_ref().unwrap(),
        )
        .env("MANDALA_LOCK_TEST_MODE", mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let fork = loop {
        let mut line = String::new();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "holder exited before readiness"
        );
        if let Some(pid) = line.strip_prefix("READY ") {
            break pid.trim().parse().ok();
        }
    };
    (child, fork)
}

#[test]
fn distinct_process_exclusion_and_sigkill_release() {
    let (path, id, options) = fixture("process");
    let (mut child, _) = spawn_holder(&id, &options, "plain");
    assert!(election::try_acquire(&id, &options).unwrap().is_none());
    child.kill().unwrap();
    child.wait().unwrap();
    let lease = election::try_acquire(&id, &options).unwrap().unwrap();
    drop(lease);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn fork_child_blocked_before_exec_cannot_retain_dead_parent_lock() {
    for mode in ["fork", "failed-exec"] {
        let (path, id, options) = fixture(mode);
        let (mut parent, fork) = spawn_holder(&id, &options, mode);
        let fork = fork.expect("fork child pid");
        assert!(
            election::try_acquire(&id, &options).unwrap().is_none(),
            "parent still excludes"
        );
        parent.kill().unwrap();
        parent.wait().unwrap();
        // SAFETY: kill(pid,0) observes only our deliberately created child.
        assert_eq!(
            unsafe { libc::kill(fork, 0) },
            0,
            "child pid={fork} mode={mode} still alive before replacement: {}",
            std::io::Error::last_os_error()
        );
        let lease = election::try_acquire(&id, &options).unwrap().unwrap();
        assert_eq!(
            unsafe { libc::kill(fork, 0) },
            0,
            "replacement owns while child pid={fork} mode={mode} remains alive: {}",
            std::io::Error::last_os_error()
        );
        unsafe {
            libc::kill(fork, libc::SIGKILL);
        }
        drop(lease);
        std::fs::remove_dir_all(path).unwrap();
    }
}

/// Separate test process owns its lease. In a post-fork child, only
/// async-signal-safe libc operations run: no allocator, Rust locks or runtime.
#[test]
fn subprocess_holder() {
    let Ok(flake) = std::env::var("MANDALA_LOCK_TEST_FLAKE") else {
        return;
    };
    let root = std::env::var_os("MANDALA_LOCK_TEST_ROOT").unwrap();
    let mode = std::env::var("MANDALA_LOCK_TEST_MODE").unwrap();
    let id = ContextIdentity::for_flake(flake).unwrap();
    let options = AcquireOptions {
        coordination_root: Some(root.into()),
        ..AcquireOptions::default()
    };
    let _lease = election::try_acquire(&id, &options).unwrap().unwrap();
    let mut pid = 0;
    if mode != "plain" {
        // Both variants hold the child alive, deliberately before successful exec.
        // A failed exec must not accidentally retain the parent's process lock.
        let failed = mode == "failed-exec";
        let mut ready = [0; 2];
        assert_eq!(unsafe { libc::pipe(ready.as_mut_ptr()) }, 0);
        pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe {
                libc::close(ready[0]);
                // Keep the deliberately orphaned fixture alive despite a
                // parent's terminal/session hangup on native macOS.
                libc::signal(libc::SIGHUP, libc::SIG_IGN);
                libc::signal(libc::SIGPIPE, libc::SIG_IGN);
                if failed {
                    libc::execl(
                        c"/does-not-exist/mandala-test".as_ptr(),
                        c"mandala-test".as_ptr(),
                        std::ptr::null::<libc::c_char>(),
                    );
                }
                libc::write(ready[1], b"!".as_ptr().cast(), 1);
                libc::close(ready[1]);
                loop {
                    libc::pause();
                }
            }
        }
        unsafe {
            libc::close(ready[1]);
        }
        let mut ack = [0_u8];
        assert_eq!(
            unsafe { libc::read(ready[0], ack.as_mut_ptr().cast(), 1) },
            1,
            "child reached pre-exec/failed-exec barrier"
        );
        unsafe {
            libc::close(ready[0]);
        }
    }
    println!("READY {pid}");
    std::io::stdout().flush().unwrap();
    // Remain alive independently of stdin pipe closure. Child::wait closes
    // stdin; treating that as graceful cleanup can kill the fork fixture
    // before the parent's asynchronous SIGKILL is delivered on macOS.
    loop {
        unsafe {
            libc::pause();
        }
    }
}
