//! Test-only loopback environment check; production busy rules are untouched.
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::Once;
use std::time::Duration;

/// Require three actual refused connections within a fixed sample budget.
/// A successful connect is not a refusal: that candidate port is occupied,
/// so choose another. The preflight never creates a listening socket itself.
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
    Err("could not obtain three refusal samples in 12 attempts: test-probe ports were occupied; no successful connection counts as a passing sample".into())
}

pub fn refusal_preflight() {
    static CHECK: Once = Once::new();
    CHECK.call_once(|| {
        // Dedicated probe range: outside production/context fixture ports and
        // below Linux/macOS ephemeral allocations. Do not briefly LISTEN to
        // select a port: children can inherit that listener, and WSL can treat
        // recently-listening/ephemeral ports differently from unused test ports.
        let mut offset = u16::try_from(std::process::id() % 1024).expect("probe offset fits");
        let result = collect_refusals(|| {
            let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 30000 + offset));
            offset = (offset + 1) % 1024;
            TcpStream::connect_timeout(&addr, Duration::from_millis(500))
                .map(drop)
                .map_err(|e| io::Error::new(e.kind(), format!("{addr}: {e}")))
        });
        assert!(result.is_ok(), "LAB-206 loopback-refusal preflight failed: {result:?}. On WSL run an immutable prebuilt test binary in `unshare --user --map-root-user --net` with `ip link set lo up`, isolated HOME/state/runtime and injected coordination roots. Production busy-leader handling is unchanged.");
    });
}
