//! A mode name this build does not know refuses the start, and no brief reaches the provider.
//! The old path kept the session and ran the brief at the daemon's own point.

#[path = "support/world.rs"]
mod world;

use std::time::Duration;

use letibot_bridge::tools::Bridge;
use world::World;

#[test]
fn start_with_unknown_mode_refuses_and_sends_no_brief() {
    let world = World::start("unknown-mode");
    let bridge = Bridge::new();
    let refused = bridge
        .start(
            &world.worktree(),
            "brief-marker: say the one line",
            None,
            "read-edits",
            None,
            false,
            None,
        )
        .expect_err("a mode this build does not have is refused");
    assert!(
        refused.contains("read-edits") && refused.contains("writes allowed"),
        "the refusal names the mode and lists the valid ones: {refused}"
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        world.fake.requests().is_empty(),
        "no turn ran on a refused mode: {:?}",
        world.fake.requests()
    );
}
