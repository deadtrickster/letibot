//! The waits a test is allowed to have.

use std::time::{Duration, Instant};

/// How long a test may wait for something that should never take this long. A loaded box
/// stretches a wake-up by seconds, so a bound that fails here is a fact about the box.
pub(crate) const PATIENCE: Duration = Duration::from_secs(60);

/// Poll `cond` every few milliseconds until it holds or `deadline` passes; report whether it held.
/// This is what a test waits with instead of sleeping a number somebody guessed.
pub(crate) fn wait_until(deadline: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + deadline;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= until {
            return cond();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
