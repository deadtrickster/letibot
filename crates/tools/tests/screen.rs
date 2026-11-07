//! The `screen` tool through a seated runtime: a program on a pty, keys in, rows and styles out,
//! a picture of the right size, and a stop that ends the process.

use letibot_tools::builtins::screen::{CELL_H, CELL_W, ScreenTool};
use letibot_tools::testing::{Harness, runner_harness};
use letibot_transcript::ToolOutcome;

fn seat() -> Option<Harness> {
    let mut h = match runner_harness() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("screen: no process host here, nothing measured: {e}");
            letibot_tokencore::apparatus::absent("a process host this user may use");
            return None;
        }
    };
    h.rt.registry.register(Box::new(ScreenTool::new())).unwrap();
    Some(h)
}

fn call(h: &mut Harness, args: serde_json::Value) -> letibot_tools::ToolResult {
    h.call("screen", &args.to_string())
}

fn id_of(text: &str) -> u64 {
    text.split_whitespace()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("an id in {text}"))
}

#[test]
fn a_program_is_read_back_as_rows_and_a_style_map() {
    let Some(mut h) = seat() else { return };
    let start = call(
        &mut h,
        serde_json::json!({"action": "start", "cols": 40, "rows": 8,
            "cmd": "printf '\\033[1;31mhello\\033[0m world\\n'; sleep 20"}),
    );
    assert_eq!(start.outcome, ToolOutcome::Ok, "{}", start.payload);
    let id = id_of(&start.payload);
    let read = call(&mut h, serde_json::json!({"action": "read", "id": id}));
    assert!(read.payload.contains("hello world"), "{}", read.payload);
    assert!(read.payload.contains("40x8"), "{}", read.payload);
    assert!(
        read.payload.contains("r0 c0-4 bold fg=red"),
        "{}",
        read.payload
    );
    call(&mut h, serde_json::json!({"action": "stop", "id": id}));
}

#[test]
fn keys_reach_the_program() {
    let Some(mut h) = seat() else { return };
    let start = call(
        &mut h,
        serde_json::json!({"action": "start", "cols": 40, "rows": 6,
            "cmd": "read x; echo got:$x; sleep 20"}),
    );
    let id = id_of(&start.payload);
    let sent = call(
        &mut h,
        serde_json::json!({"action": "keys", "id": id, "keys": "abc<Enter>"}),
    );
    assert_eq!(sent.outcome, ToolOutcome::Ok, "{}", sent.payload);
    let read = call(&mut h, serde_json::json!({"action": "read", "id": id}));
    assert!(read.payload.contains("got:abc"), "{}", read.payload);
    let bad = call(
        &mut h,
        serde_json::json!({"action": "keys", "id": id, "keys": "<Nope>"}),
    );
    assert!(matches!(bad.outcome, ToolOutcome::Failed { .. }));
    let click = call(
        &mut h,
        serde_json::json!({"action": "mouse", "id": id, "x": 1, "y": 1}),
    );
    assert!(
        click.payload.contains("mouse reporting"),
        "{}",
        click.payload
    );
    call(&mut h, serde_json::json!({"action": "stop", "id": id}));
}

#[test]
fn a_shot_is_a_png_of_the_screen_size() {
    let Some(mut h) = seat() else { return };
    let start = call(
        &mut h,
        serde_json::json!({"action": "start", "cols": 30, "rows": 5,
            "cmd": "printf 'pixels'; sleep 20"}),
    );
    let id = id_of(&start.payload);
    let shot = call(&mut h, serde_json::json!({"action": "shot", "id": id}));
    let media = shot.media.expect("a picture");
    assert_eq!(media.mime, "image/png");
    assert_eq!(
        (media.width, media.height),
        (Some((30 * CELL_W) as u32), Some((5 * CELL_H) as u32))
    );
    assert!(shot.payload.contains("`read`"), "{}", shot.payload);
    call(&mut h, serde_json::json!({"action": "stop", "id": id}));
}

#[test]
fn stop_ends_the_process() {
    let Some(mut h) = seat() else { return };
    let pidfile = h.root().join("screen.pid");
    let cmd = format!("echo $$ > {}; exec sleep 60", pidfile.display());
    let start = call(&mut h, serde_json::json!({"action": "start", "cmd": cmd}));
    let id = id_of(&start.payload);
    let mut pid = 0i32;
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&pidfile)
            && let Ok(p) = s.trim().parse()
        {
            pid = p;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(pid > 0, "the program never wrote its pid");
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0, "it runs before stop");
    let stop = call(&mut h, serde_json::json!({"action": "stop", "id": id}));
    assert_eq!(stop.outcome, ToolOutcome::Ok, "{}", stop.payload);
    let mut gone = false;
    for _ in 0..50 {
        if unsafe { libc::kill(pid, 0) } != 0 {
            gone = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(gone, "pid {pid} survived stop");
}

#[test]
fn the_description_is_inside_the_budget_and_lints_clean() {
    let schema = ScreenTool::new();
    let s = letibot_tools::runtime::Tool::schema(&schema);
    assert!(s.description.len() < 800, "{} bytes", s.description.len());
    assert_eq!(
        letibot_tools::schema::lint_description(&s.description),
        vec![]
    );
}

#[test]
fn the_skill_is_built_in_and_offered_to_every_seat() {
    let skills = letibot_tools::builtins::skill::SkillRegistry::built_in();
    let skill = skills
        .iter()
        .find(|s| s.name == "screen")
        .expect("built in");
    let seat = std::collections::BTreeSet::new();
    assert!(skill.unmet(&seat).is_none());
}
