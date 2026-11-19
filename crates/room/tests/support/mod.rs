//! The one busy wait every room test uses: a test waits for a fact, never for a fixed time.

#![allow(dead_code)]

use std::path::Path;
use std::time::{Duration, Instant};

/// The "never happened" bound. A loaded box must not trip it; a hang still ends.
pub const PATIENCE: Duration = Duration::from_secs(60);

/// How often a wait looks again.
pub const TICK: Duration = Duration::from_millis(10);

/// Poll `cond` until it holds, and fail naming `what` if `PATIENCE` passes first.
pub fn wait_until(what: &str, cond: impl FnMut() -> bool) {
    wait_until_by(what, Instant::now() + PATIENCE, cond)
}

/// `wait_until` with a bound of the caller's own, for a wait whose fact has its own clock.
pub fn wait_until_by(what: &str, deadline: Instant, mut cond: impl FnMut() -> bool) {
    while !cond() {
        assert!(Instant::now() < deadline, "never happened: {what}");
        std::thread::sleep(TICK);
    }
}

/// The coordinator's own socket is gone, so the same path can be served again. Not for an
/// inherited listener: a manager's socket outlives every coordinator stopped on it.
pub fn wait_socket_gone(sock: &Path) {
    wait_until(&format!("{} to go", sock.display()), || !sock.exists());
}
