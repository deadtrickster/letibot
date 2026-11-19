//! Waits the crate's tests use instead of a sleep: a fixture is asked whether the thing has
//! happened yet, so the wait ends when it does rather than after a guess about how long it takes.

/// How long a test waits for a thing to happen before it calls it never-happened. Generous on
/// purpose: nothing here asserts how FAST anything is, and a loaded box is slow.
pub const PATIENCE: std::time::Duration = std::time::Duration::from_secs(60);

/// The gap between two looks inside a busy wait: short against the millisecond a lock is held or
/// a clock ticks, long enough that the wait costs the box nothing.
const TICK: std::time::Duration = std::time::Duration::from_millis(2);

/// Poll `cond` until it holds, and panic naming `what` if it has not within [`PATIENCE`]. A busy
/// wait, not a sleep: a lock released in 5 ms is not waited out for a third of a second.
pub fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + PATIENCE;
    while !cond() {
        assert!(
            std::time::Instant::now() < deadline,
            "waited {PATIENCE:?} for {what}, which never happened"
        );
        std::thread::sleep(TICK);
    }
}
