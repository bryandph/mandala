//! Isolated context-test environment; never relax production busy classification.
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Once;
use std::time::Duration;

/// LAB-206: a dead endpoint must refuse, not silently blackhole connects.
/// Run once per integration-test process, before any election assertion.
pub fn refusal_preflight() {
    static CHECK: Once = Once::new();
    CHECK.call_once(|| {
        for _ in 0..3 {
            let reserved = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve test port");
            let addr = reserved.local_addr().unwrap();
            drop(reserved);
            let result = TcpStream::connect_timeout(&addr, Duration::from_millis(500));
            assert!(
                matches!(&result, Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused),
                "LAB-206 loopback-refusal preflight: unused {addr} must refuse, not timeout: {result:?}. On WSL run an immutable prebuilt test binary in `unshare --user --map-root-user --net` with `ip link set lo up`; use isolated HOME/state/runtime and injected coordination roots. Production busy-leader handling is unchanged."
            );
        }
    });
}
