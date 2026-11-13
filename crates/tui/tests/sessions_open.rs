//! The picker as it opens, in a real terminal: ONE line saying the session list is being built,
//! and then the list. What a person must not see is a list that is drawn and then replaced by a
//! second drawing of the same list - the swap this pane was, and the reason it had two renderers.
//!
//! `--demo` replays a recorded log with no daemon, so the list the layer answers is the empty one:
//! what is under test is which line is on the screen while the head's stack is being built, and in
//! what order.

use letibot_tools::builtins::screen::ScreenTool;
use letibot_tools::testing::runner_harness;

/// The pane's title, drawn first, so the test knows the picker is what it is reading.
const TITLE: &str = "sessions in this daemon";
/// What the pane says while the head's layers are being loaded.
const BUILDING: &str = "building the session list";
/// What the layer says when it has answered and the daemon holds nothing.
const EMPTY: &str = "none listed yet";

#[test]
fn the_picker_says_it_is_building_and_never_swaps_one_list_for_another() {
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
    // The demo replays paced, so wait for the hint bar that names the key this test types.
    let read = |h: &mut letibot_tools::testing::Harness, idle: u64, max: u64| -> String {
        call(
            h,
            serde_json::json!({"action": "read", "id": id, "idle_ms": idle, "max_ms": max}),
        )
        .payload
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if read(&mut h, 400, 2_000).contains("ctrl-s sessions")
            || std::time::Instant::now() > deadline
        {
            break;
        }
    }
    call(
        &mut h,
        serde_json::json!({"action": "keys", "id": id, "keys": "<C-s>"}),
    );

    // Every frame from the key on, in order. A frame that shows the layer's answer before the
    // building line has been seen is the swap: the pane drew a list it did not have yet.
    let mut building_at: Option<usize> = None;
    let mut answer_at: Option<usize> = None;
    let mut frames: Vec<String> = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        // A fast read: this test is about the frames BEFORE the answer, and a settled one is the
        // answer already. The demo is paced, so a few of these land inside the build.
        let screen = read(&mut h, 20, 40);
        let pane = screen.contains(TITLE);
        if pane {
            if screen.contains(BUILDING) && building_at.is_none() {
                building_at = Some(frames.len());
            }
            if screen.contains(EMPTY) {
                answer_at = Some(frames.len());
                frames.push(screen);
                break;
            }
            frames.push(screen);
        }
    }
    call(&mut h, serde_json::json!({"action": "stop", "id": id}));

    let building = building_at.unwrap_or_else(|| {
        panic!("no frame said `{BUILDING}` while the stack was built: {frames:#?}")
    });
    let answered = answer_at
        .unwrap_or_else(|| panic!("the layer's answer never reached the pane: {frames:#?}"));
    assert!(
        building < answered,
        "the list was drawn before the pane said it was building - {} frame(s) of a swap: \
         {frames:#?}",
        answered - building
    );
    // And the answer, once it is there, is the layer's: the pane's own line never comes back.
    let last = frames.last().expect("the frame with the answer");
    assert!(
        !last.contains(BUILDING),
        "the building line is still on the screen under the list:\n{last}"
    );
}
