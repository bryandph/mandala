use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

/// Publish a complete executable without ever opening it for writing in libtest.
///
/// Closing a Rust writer before spawn is insufficient: a concurrent test can
/// fork while it is open and retain the writable fd until exec (even with
/// O_CLOEXEC), causing ETXTBSY. Write in a separate child, which cannot inherit
/// its fd back into libtest or sibling children, and reap it before publishing.
/// A fresh sibling inode plus rename also makes replacement safe: never truncate
/// an executable in place. No fsync is needed for these ephemeral fixtures.
pub fn write_program(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let temporary = path.with_file_name(format!(
        ".mandala-program-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut child = Command::new("sh")
            // noclobber prevents accidentally truncating a preexisting inode.
            .args(["-c", "set -C; cat > \"$1\"", "sh"])
            .arg(&temporary)
            .stdin(Stdio::piped())
            .spawn()?;
        // Drop stdin before waiting, including on write failure, and always
        // reap the writer so no child or writable fd survives an error.
        let written = child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(contents.as_ref());
        let status = child.wait()?;
        written?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "writing fixture {}: {status}",
                path.display()
            )));
        }
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
