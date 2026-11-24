//! A session `leticode_start` opened lives as long as its caller. The MCP client going away (its
//! input ends) closes it at the daemon, so a stopped orchestration leaves no daemon working.

#[path = "support/world.rs"]
mod world;

use std::time::{Duration, Instant};

use letibot_bridge::mcp;
use letibot_bridge::tools::Bridge;
use world::World;

#[test]
fn stopping_the_caller_closes_its_sessions() {
    let world = World::start("caller-lease");
    let bridge = Bridge::new();
    let started = bridge
        .start(
            &world.worktree(),
            "brief-marker: say the one line",
            None,
            "writes-allowed",
            None,
            Some(false),
            None,
        )
        .expect("the session starts");
    let session = started["session"].as_str().expect("an id").to_string();
    assert!(
        world.registry.get(&session).is_some(),
        "open while its caller is here"
    );

    // The client leaves: its stdin ends and the server returns.
    let mut out: Vec<u8> = Vec::new();
    mcp::serve(std::io::Cursor::new(Vec::<u8>::new()), &mut out, &bridge)
        .expect("an ended input is a clean end");

    let deadline = Instant::now() + Duration::from_secs(30);
    while world.registry.get(&session).is_some() {
        assert!(
            Instant::now() < deadline,
            "`{session}` is still open at the daemon after its caller went away"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        world.registry.get(&world.session).is_some(),
        "a session this caller did not start is not its to close"
    );
}
