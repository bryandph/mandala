//! Test-only loopback environment check; production busy rules are untouched.
use std::io;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Once;
use std::time::Duration;

/// Require three actual refused connections within a fixed sample budget.
/// A successful connect is not a refusal: that released port became occupied
/// (or a pre-exec child retained its temporary listener), so choose another.
/// A timeout or any other error fails immediately; it is never skipped.
pub(crate) fn collect_refusals(mut probe: impl FnMut() -> io::Result<()>) -> Result<(), String> {
    let mut refused = 0;
    for _ in 0..12 {
        match probe() {
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                refused += 1;
                if refused == 3 {
                    return Ok(());
                }
            }
            Ok(()) => std::thread::sleep(Duration::from_millis(1)),
            Err(e) => {
                return Err(format!(
                    "unused loopback ports must refuse, not timeout or fail otherwise: {e}"
                ));
            }
        }
    }
    Err("could not obtain three refusal samples in 12 attempts: released test ports became occupied; no successful connection counts as a passing sample".into())
}

pub fn refusal_preflight() {
    static CHECK: Once = Once::new();
    CHECK.call_once(|| {
        let result = collect_refusals(|| {
            let reserved = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
            let addr = reserved.local_addr()?;
            drop(reserved);
            TcpStream::connect_timeout(&addr, Duration::from_millis(500))
                .map(drop)
                .map_err(|e| io::Error::new(e.kind(), format!("{addr}: {e}")))
        });
        assert!(result.is_ok(), "LAB-206 loopback-refusal preflight failed: {result:?}. On WSL run an immutable prebuilt test binary in `unshare --user --map-root-user --net` with `ip link set lo up`, isolated HOME/state/runtime and injected coordination roots. Production busy-leader handling is unchanged.");
    });
}
