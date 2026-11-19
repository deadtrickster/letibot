//! The waits a test in this crate uses instead of a sleep: a fixture is asked whether it has
//! happened yet, so the wait ends when it does rather than after a guess about how long it takes.

/// How long a test waits for a thing to happen before it calls it never-happened. Generous on
/// purpose: nothing here asserts how FAST anything is, and a loaded box is slow.
pub const PATIENCE: std::time::Duration = std::time::Duration::from_secs(60);

/// The gap between two looks inside a busy wait: short enough that a poller's own 30 ms period is
/// still what decides when a request is sent, long enough that the wait costs the box nothing.
const TICK: std::time::Duration = std::time::Duration::from_millis(2);

/// Poll `cond` until it holds, and panic naming `what` if it has not within [`PATIENCE`]. A busy
/// wait, not a sleep: a request that lands in 5 ms is not waited out for a second.
pub fn wait_until(what: &str, cond: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + PATIENCE;
    let mut cond = cond;
    while !cond() {
        assert!(
            std::time::Instant::now() < deadline,
            "waited {PATIENCE:?} for {what}, which never happened"
        );
        std::thread::sleep(TICK);
    }
}
