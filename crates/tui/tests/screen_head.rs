//! The head, run in a `screen` the way a model looks at a UI change: started on a pty at a stated
//! size, then read back as rows.

use letibot_tools::builtins::screen::ScreenTool;
use letibot_tools::testing::runner_harness;

#[test]
fn a_head_in_a_screen_shows_its_status_line() {
    let Ok(mut h) = runner_harness() else {
        letibot_tokencore::apparatus::absent("a process host this user may use");
        return;
    };
    h.rt.registry.register(Box::new(ScreenTool::new())).unwrap();
    let cmd = format!("{} --demo", env!("CARGO_BIN_EXE_letibot-tui"));
    let call = |h: &mut letibot_tools::testing::Harness, v: serde_json::Value| {
        h.call("screen", &v.to_string())
    };
    let start = call(
        &mut h,
        serde_json::json!({"action": "start", "cmd": cmd, "cols": 100, "rows": 30}),
    );
    let id: u64 = start
        .payload
        .split_whitespace()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .expect("an id");
    // The demo replays paced on a host of any load: read until every marker shows or a deadline.
    let wanted = ["ctrl-s sessions", "Why the cache missed", "faint"];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let read = loop {
        let read = call(
            &mut h,
            serde_json::json!({"action": "read", "id": id, "idle_ms": 1500, "max_ms": 10000}),
        );
        if wanted.iter().all(|w| read.payload.contains(w)) || std::time::Instant::now() > deadline
        {
            break read;
        }
    };
    for w in wanted {
        assert!(read.payload.contains(w), "{w}: {}", read.payload);
    }
    let shot = call(&mut h, serde_json::json!({"action": "shot", "id": id}));
    assert!(shot.media.is_some(), "{}", shot.payload);
    call(&mut h, serde_json::json!({"action": "stop", "id": id}));
}
