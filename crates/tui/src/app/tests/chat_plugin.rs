//! The chat as a layer (`docs/plugin-extension-points.md` §5 P5): the `chat` feed, the memo that
//! asks it once per generation, the input's two hooks, the acts an answer performs, and the native
//! adapter a broken layer falls to in the same frame.
//!
//! The layer here is scripted - a `Module` that answers what a test says - because what is being
//! proved is the HOST's half: a wasm guest that draws a chat is P6's.

use super::rooms::{history, msg as wire_msg, opened, reply};
use super::*;
use letibot_plugins::{Capability, Loader, Module, Stack};
use rano::style::Palette;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// The fuel a full 200-message window costs at the wasm boundary, measured on lubuntu1 2026-11-11
/// against this binary's own `sessions` layer - the cheapest pane layer the tree has, whose input
/// parse IS the feed's JSON. It is recorded in `docs/plugin-extension-points.md` §5 P5; the ceiling
/// below is a little over twice it, so a doubling of the engine's own work is a failure and the
/// engine's own 50M per-call cap is never near.
const FUEL_200: u64 = 3_456_254;
const FUEL_CEILING: u64 = 8_000_000;

/// What one layer was asked, per hook - the answer alone cannot say whether it was asked twice.
#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<(String, Value)>>>);

impl Seen {
    fn record(&self, hook: &str, input: &str) {
        self.0.lock().expect("the recorder").push((
            hook.to_string(),
            serde_json::from_str(input).unwrap_or(Value::Null),
        ));
    }

    fn of(&self, hook: &str) -> Vec<Value> {
        self.0
            .lock()
            .expect("the recorder")
            .iter()
            .filter(|(h, _)| h == hook)
            .map(|(_, v)| v.clone())
            .collect()
    }

    fn count(&self, hook: &str) -> usize {
        self.of(hook).len()
    }

    fn last(&self, hook: &str) -> Value {
        self.of(hook).pop().unwrap_or(Value::Null)
    }
}

/// A layer that answers one JSON per hook, or traps.
struct Scripted {
    seen: Seen,
    answers: std::collections::BTreeMap<String, String>,
    trap: Option<String>,
}

impl Module for Scripted {
    /// Every hook the manifest declares, always: a layer whose exports and manifest disagree is
    /// refused at load, and a hook with no answer in this test is one that answers nothing.
    fn hooks(&self) -> Vec<String> {
        vec![
            "pane_chat".to_string(),
            "command_chat".to_string(),
            "complete_chat".to_string(),
        ]
    }

    fn call(&self, hook: &str, input: &str) -> Result<Option<String>, String> {
        self.seen.record(hook, input);
        if let Some(why) = &self.trap {
            return Err(why.clone());
        }
        Ok(self.answers.get(hook).cloned())
    }

    fn fuel(&self) -> u64 {
        7
    }
}

struct ScriptedLoader {
    seen: Seen,
    answers: std::collections::BTreeMap<String, String>,
    trap: Option<String>,
}

impl Loader for ScriptedLoader {
    fn load(&self, _wasm: &Path, _caps: &[Capability]) -> Result<Box<dyn Module>, String> {
        Err("this test's layers are built in, not placed".to_string())
    }

    fn load_bytes(
        &self,
        _name: &str,
        _bytes: &[u8],
        _caps: &[Capability],
    ) -> Result<Box<dyn Module>, String> {
        Ok(Box::new(Scripted {
            seen: self.seen.clone(),
            answers: self.answers.clone(),
            trap: self.trap.clone(),
        }))
    }
}

/// The chat layer's manifest, as H0's `manifest.rs` spells it: the three hooks, the one feed, the
/// acts. `actions` is a parameter because the allowlist is what one test is about.
fn manifest(actions: &str) -> String {
    format!(
        "name = \"chat\"\n\
         purpose = \"the room's messages, drawn the way this project reads them\"\n\
         hooks = [\"pane_chat\", \"command_chat\", \"complete_chat\"]\n\
         capabilities = []\n\
         feeds = [\"chat\"]\n\
         actions = [{actions}]\n\
         contract = 1\n"
    )
}

/// One view with a stream of the entries `entries` spells, and the input surface the layer owns.
fn a_view(entries: Value, input: bool) -> String {
    let mut view = json!({
        "title": "the room, as this project reads it",
        "panels": [{"id": "chat", "key": "c", "title": "chat",
                    "summary": [{"stream": {"entries": entries}}]}],
    });
    if input {
        view["input"] = json!({"key": "host", "placeholder": "say it", "footer": "enter sends"});
    }
    view.to_string()
}

/// A chat layer as a BUILT-IN: the one scope that may read the `chat` feed unpinned
/// (`Manifest::admit_for`), and the scope the built-in engine installs.
fn a_head(seen: &Seen, actions: &str, answers: &[(&str, &str)], trap: Option<&str>) -> App {
    let mut answers_map = std::collections::BTreeMap::new();
    for (hook, answer) in answers {
        answers_map.insert((*hook).to_string(), (*answer).to_string());
    }
    let mut stack = Stack::discover(
        None,
        None,
        None,
        Arc::new(ScriptedLoader {
            seen: seen.clone(),
            answers: answers_map,
            trap: trap.map(str::to_string),
        }),
    );
    stack
        .register_builtin_bytes("chat", &manifest(actions), b"\0asm")
        .expect("the chat layer installs");

    let mut a = opened(1_000);
    let workspace = a.workspace_path();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::with_stack(
        stack, workspace,
    ));
    a
}

/// The room with two messages, so the native adapter has something to draw if it has to.
fn a_room(a: &mut App) {
    reply(a, super::rooms::rooms_reply(&[("host", 2, 2, 0)]));
    reply(
        a,
        history(
            "host",
            vec![
                wire_msg(1, "host", "s-a", "session", "the native line one"),
                wire_msg(2, "host", "s-b", "session", "the native line two"),
            ],
        ),
    );
}

/// One entry, as a layer writes a message.
fn an_entry(id: &str, body: &str) -> Value {
    json!({"id": id, "head": [{"text": "fix-room-push", "role": "hue:3"}], "time": "14:20",
           "body": body})
}

fn acts_answer(name: &str, args: Value) -> String {
    json!({"acts": [{"name": name, "args": args}]}).to_string()
}

// ─────────────────────────── the feed and its memo ───────────────────────────

/// The feed's generation is the ask's clock: a second frame over the same feed costs no wasm call,
/// which is the whole difference between a pane that draws and a pane that recompiles.
#[test]
fn a_head_pane_is_asked_once_per_feed_generation_not_per_draw() {
    let seen = Seen::default();
    let view = a_view(json!([an_entry("1", "the layer's line")]), true);
    let mut a = a_head(&seen, "\"room_say\"", &[("pane_chat", &view)], None);
    a_room(&mut a);

    let first = a.screen(120, 30).join("\n");
    assert!(first.contains("the layer's line"), "{first}");
    assert_eq!(seen.count("pane_chat"), 1, "asked on the first frame");
    // Three more frames, the same feed: not one call more.
    for _ in 0..3 {
        a.screen(120, 30);
    }
    assert_eq!(
        seen.count("pane_chat"),
        1,
        "a draw is not an ask: the feed did not move"
    );
}

/// A pushed message moves the feed, so the layer is asked again - once, and on the next frame.
#[test]
fn a_pushed_message_re_asks_the_chat_pane() {
    let seen = Seen::default();
    let view = a_view(json!([an_entry("1", "one")]), true);
    let mut a = a_head(&seen, "\"room_say\"", &[("pane_chat", &view)], None);
    a_room(&mut a);
    a.screen(120, 30);
    assert_eq!(seen.count("pane_chat"), 1);

    a.on_room_pushed(
        json!({"seq": 3, "ts": 1_793_735_974_000u64, "room": "host", "author": "s-a",
               "kind": "session", "text": "pushed"}),
        false,
    );
    let after = a.screen(120, 30).join("\n");
    assert!(after.contains("one"), "{after}");
    assert_eq!(
        seen.count("pane_chat"),
        2,
        "the push is a new generation, and one ask"
    );
    // And the feed it was asked with carries the pushed message.
    let asked = seen.last("pane_chat");
    let window = asked["feeds"]["chat"]["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        window.iter().any(|m| m["text"] == "pushed"),
        "the window has the new message: {asked}"
    );
}

/// The clock and the zone are the host's: a guest has neither, and a chat that prints `today`
/// without them is printing yesterday.
#[test]
fn the_chat_feed_carries_now_ms_and_the_local_offset() {
    let seen = Seen::default();
    let view = a_view(json!([an_entry("1", "one")]), true);
    let mut a = a_head(&seen, "\"room_say\"", &[("pane_chat", &view)], None);
    a.clock(7_000);
    a_room(&mut a);
    a.screen(120, 30);

    let asked = seen.last("pane_chat");
    assert_eq!(asked["now_ms"], json!(a.now_ms), "{asked}");
    let offset = asked["utc_offset_s"].as_i64().expect("a zone");
    assert!(
        (-12 * 3600..=14 * 3600).contains(&offset),
        "a real offset, not 1970: {offset}"
    );
    // The feed itself carries the room, the selected channel and the window.
    let chat = &asked["feeds"]["chat"];
    assert_eq!(chat["selected"], json!("host"), "{chat}");
    assert_eq!(chat["channels"][0]["room"], json!("host"), "{chat}");
    assert_eq!(chat["messages"].as_array().map(Vec::len), Some(2), "{chat}");
}

// ─────────────────────────────── the input ───────────────────────────────

/// Enter on the layer's input calls `command_<pane>` with the line, the key it bound, the reply
/// target and the chips - the whole shape of `X5`, and never a keystroke.
#[test]
fn enter_in_a_plugin_input_calls_its_command_hook_with_the_line() {
    let seen = Seen::default();
    let view = a_view(json!([an_entry("1", "one")]), true);
    let command = acts_answer("room_say", json!({"text": "build is green"}));
    let mut a = a_head(
        &seen,
        "\"room_say\"",
        &[("pane_chat", &view), ("command_chat", &command)],
        None,
    );
    a_room(&mut a);
    a.screen(120, 30);

    a.set_composer("build is green");
    a.key(Key::Enter);

    let asked = seen.last("command_chat");
    assert_eq!(asked["line"], json!("build is green"), "{asked}");
    assert_eq!(asked["key"], json!("host"), "the input the view bound");
    assert!(asked.get("reply_to").is_some(), "{asked}");
    assert_eq!(asked["chips"], json!([]), "{asked}");
    assert_eq!(
        asked["feeds"]["chat"]["selected"],
        json!("host"),
        "the line is asked against the same feed: {asked}"
    );
}

/// The act a chat layer answers with is a room `say`, and the head performs it AS THE OPERATOR:
/// the same request the native view sends, whose author is the seat's connection, not a word a
/// layer can spell.
#[test]
fn a_room_say_act_sends_as_the_operator() {
    let seen = Seen::default();
    let view = a_view(json!([an_entry("1", "one")]), true);
    let command = acts_answer("room_say", json!({"text": "build is green"}));
    let mut a = a_head(
        &seen,
        "\"room_say\"",
        &[("pane_chat", &view), ("command_chat", &command)],
        None,
    );
    a_room(&mut a);
    a.screen(120, 30);

    a.set_composer("build is green");
    let sent = a.key(Key::Enter);
    let Some(Action::RoomChat { op, id }) = sent else {
        panic!("not a chat request: {sent:?}");
    };
    assert_eq!(op["type"], json!("say"), "{op}");
    assert_eq!(op["room"], json!("host"), "the channel the feed selected");
    assert_eq!(op["text"], json!("build is green"), "{op}");
    assert_eq!(op["thread"], Value::Null, "a plain line answers nothing");
    assert!(
        id.as_deref().is_some_and(|i| i.starts_with("op-")),
        "the head's own request id: {id:?}"
    );
    // The operator's identity is the SEAT's: the frame carries no author, and the room writes
    // `operator` because that is whose connection said it.
    assert!(
        op.get("author").is_none(),
        "an act cannot name its author: {op}"
    );
    // The native view's own ledger holds it, so a send that fails is still the operator's line.
    assert_eq!(a.rooms.sends.len(), 1, "the send is recorded");
    assert_eq!(a.rooms.sends[0].text, "build is green");
}

/// An act the manifest did not declare is SAID and not performed: the list in `plugin.toml` is the
/// whole of what a layer can reach.
#[test]
fn an_act_the_manifest_did_not_declare_is_said_and_not_performed() {
    let seen = Seen::default();
    let view = a_view(json!([an_entry("1", "one")]), true);
    // The manifest declares `room_say` alone; the answer asks for a direct message.
    let command = acts_answer("room_dm", json!({"to": "s-b", "text": "psst"}));
    let mut a = a_head(
        &seen,
        "\"room_say\"",
        &[("pane_chat", &view), ("command_chat", &command)],
        None,
    );
    a_room(&mut a);
    a.screen(120, 30);
    a.take_actions();

    a.set_composer("psst");
    let sent = a.key(Key::Enter);
    assert!(
        sent.is_none() && a.take_actions().is_empty(),
        "nothing was sent: {sent:?}"
    );
    assert!(a.rooms.sends.is_empty(), "and nothing was recorded");
    let said = a.notice.clone().unwrap_or_default();
    assert!(said.contains("room_dm"), "the act is named: {said}");
    assert!(said.contains("does not declare"), "{said}");
}

/// Tab asks `complete_<pane>` with the line and the caret, and the row the composer's edge draws is
/// that answer: one row for a room's completion and a layer's.
#[test]
fn tab_asks_the_complete_hook_and_draws_its_candidates() {
    let seen = Seen::default();
    let view = a_view(json!([an_entry("1", "one")]), true);
    let complete = json!({
        "from": 0, "to": 3,
        "candidates": [
            {"insert": "@fix-room-push ", "label": "fix-room-push", "note": "here", "hue": 3},
            {"insert": "@fix-room-say ", "label": "fix-room-say"}
        ]
    })
    .to_string();
    let mut a = a_head(
        &seen,
        "\"room_say\"",
        &[("pane_chat", &view), ("complete_chat", &complete)],
        None,
    );
    a_room(&mut a);
    a.screen(120, 30);

    a.set_composer("@fi");
    a.key(Key::Tab);
    let asked = seen.last("complete_chat");
    assert_eq!(asked["text"], json!("@fi"), "{asked}");
    assert_eq!(asked["cursor"], json!(3), "{asked}");
    assert_eq!(asked["key"], json!("host"), "{asked}");

    // The first candidate is in the line, and the row draws the names.
    assert_eq!(a.editor.text(), "@fix-room-push ", "{}", a.editor.text());
    let row = a.rooms_completion_row(80).expect("the row");
    let drawn = crate::ui::render::row(&row, Palette::None);
    assert!(drawn.contains("fix-room-push"), "{drawn}");
    assert!(drawn.contains("fix-room-say"), "{drawn}");
    // A second Tab cycles to the next candidate rather than asking again.
    a.key(Key::Tab);
    assert_eq!(a.editor.text(), "@fix-room-say ", "{}", a.editor.text());
    assert_eq!(seen.count("complete_chat"), 1, "the cycle is the host's");
}

// ─────────────────────────────── the fallback ───────────────────────────────

/// A layer that traps falls back to the native adapter IN THE SAME FRAME: the frame that asked is
/// the frame that drew the fallback, and the person is told once why.
#[test]
fn a_trapping_chat_layer_falls_back_to_the_native_renderer() {
    let seen = Seen::default();
    let mut a = a_head(
        &seen,
        "\"room_say\"",
        &[("pane_chat", "{\"title\":\"never\"}")],
        Some("out of fuel"),
    );
    a_room(&mut a);

    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("the native line one") && screen.contains("the native line two"),
        "the adapter drew this frame:\n{screen}"
    );
    assert_eq!(seen.count("pane_chat"), 1, "asked once, and it trapped");
    // The reason is said once, and it names the trap and the pane it was drawn for.
    let told = a.notice.clone().unwrap_or_default();
    assert!(told.contains("out of fuel"), "{told}");
    assert!(told.contains("the chat"), "{told}");
}

// ─────────────────────────────── the window ───────────────────────────────

/// A window of 200 messages is a real feed: it is under `WINDOW_BYTES`, it is asked through a wasm
/// engine, and the fuel it costs is recorded and asserted under a ceiling. The layer is this
/// binary's own `sessions` pane - the cheapest one there is - asked with the chat window, so the
/// number is the boundary's price and not a guess about P6's policy.
#[test]
fn a_window_of_200_messages_answers_under_the_fuel_cap() {
    let mut a = opened(1_000);
    a.clock(1_793_735_794_000);
    let rows: Vec<Value> = (1..=200)
        .map(|seq| {
            wire_msg(
                seq,
                "host",
                "s-a",
                "session",
                &format!("message {seq} with a line of words in it, long enough to be a message"),
            )
        })
        .collect();
    reply(&mut a, super::rooms::rooms_reply(&[("host", 2, 200, 0)]));
    reply(&mut a, history("host", rows));

    // The feed the host would hand a chat layer: the real builder, over 200 messages.
    let feed = a.chat_feed();
    assert_eq!(feed.messages.len(), 200, "the whole window");
    let bytes = serde_json::to_string(&feed)
        .expect("the feed serialises")
        .len();
    println!("window bytes: {bytes}");
    assert!(
        bytes <= crate::app::chat_plugin::WINDOW_BYTES,
        "the window is {bytes} bytes"
    );

    // The same JSON through a real engine, on a layer that has to parse the whole input.
    let tmp = std::env::temp_dir().join(format!(
        "letibot-chat-fuel-{}-{}",
        std::process::id(),
        a.now_ms
    ));
    std::fs::create_dir_all(&tmp).expect("a temp dir");
    let host = crate::app::sessions_pane::Host::build("s-none", &tmp);
    let input = json!({
        "v": letibot_plugin::pane::V,
        "size": {"w": 120, "h": 40},
        // The sessions layer answers from its own feed, and parses the whole `feeds` map on the
        // way - which is the window's price at the boundary.
        "feeds": {
            "sessions": {"sessions": [{"id": "s-1", "title": "one"}], "here": "s-1",
                         "home": "/home/x"},
            "chat": feed,
        },
        "now_ms": a.now_ms,
    });
    let answered = host.call("pane_sessions", "the chat's window", &input);
    let fuel = answered.fuel.expect("the engine measured the call");
    println!("fuel for 200 messages: {fuel}");
    assert!(
        fuel > 0 && fuel <= FUEL_CEILING,
        "the window cost {fuel} fuel, and the ceiling is {FUEL_CEILING} (measured {FUEL_200})"
    );
    assert!(
        answered.view.is_some(),
        "a real layer answered: {:?}",
        answered.view.map(|v| v.title)
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

// ─────────────────────────── the pane's own renderer ───────────────────────────

/// A pane's `stream` block is drawn by P2's renderer: the entry above, the body under it, the
/// divider where the layer put one - and the entry is what the cursor rests on, not a screen row.
#[test]
fn a_plugin_panes_stream_block_draws_through_the_chat_renderer() {
    let view: letibot_sessionlog::pane::View = serde_json::from_value(json!({
        "title": "a chat pane",
        "panels": [{"id": "chat", "key": "c", "title": "chat", "summary": [{"stream": {
            "entries": [
                {"divider": "today", "text": "today"},
                {"id": "1", "head": [{"text": "fix-room-push", "role": "hue:3"}],
                 "time": "14:20", "body": "the body it **wrote**"},
                {"id": "2", "body": "a second message"}
            ]
        }}]}],
    }))
    .expect("the view parses");

    let drawn =
        crate::ui::panes::plugin::content(&view, crate::ui::panes::plugin::Screen::Overview, 0, 80);
    let text: Vec<String> = drawn
        .lines
        .lines
        .iter()
        .map(|l| crate::ui::render::row(l, Palette::None))
        .collect();
    let all = text.join("\n");
    assert!(all.contains("today"), "the divider is drawn: {all}");
    assert!(all.contains("fix-room-push"), "the head is drawn: {all}");
    assert!(all.contains("the body it"), "the body is drawn: {all}");
    assert!(
        !all.contains("**wrote**"),
        "and as markdown, not as its punctuation: {all}"
    );
    assert!(all.contains("a second message"), "{all}");
    // Two messages are two stops; the divider is none.
    assert_eq!(
        crate::ui::panes::plugin::stops(&view, crate::ui::panes::plugin::Screen::Overview).len(),
        2
    );
    // The first message wears the cursor's mark.
    assert!(
        text.iter().any(|l| l.contains("▸") || l.contains('>')),
        "a stop is marked: {all}"
    );
}

// ─────────────────────────────── the keys ───────────────────────────────

/// A pane that owns the composer must not eat the letters, so its own keys are live only once Tab
/// put the reader into the view (`X8`): Esc hands them back.
#[test]
fn a_views_own_keys_are_live_only_in_cursor_mode() {
    let mut a = app();
    a.on_plugin_event(letibot_sessionlog::event::SessionEvent::PluginPane {
        pane: "chat/chat".to_string(),
        view: json!({
            "title": "a chat pane",
            "input": {"key": "host"},
            "keys": [{"key": "Y", "label": "copy", "event": "copy"}],
            "panels": [{"id": "chat", "key": "c", "summary": [{"stream": {
                "entries": [{"id": "7", "body": "a message"}]
            }}]}],
        }),
    });
    a.open_plugin("chat/chat");
    a.take_actions();
    a.screen(120, 30);
    assert!(
        !a.plugin_cursor(),
        "a pane with an input starts in the composer"
    );

    // A letter is the composer's until Tab: `Y` types, and nothing is sent to the pane.
    assert_eq!(a.key(Key::Char('Y')), None);
    assert_eq!(a.editor.text(), "Y");
    a.set_composer("");

    // Tab on the empty input enters the mode; `Y` is now the view's, on the entry under the cursor.
    a.key(Key::Tab);
    assert!(a.plugin_cursor(), "tab entered the view");
    let sent = a.key(Key::Char('Y'));
    assert_eq!(
        sent,
        Some(Action::PaneAction {
            pane: "chat/chat".to_string(),
            item: "7".to_string(),
            action: "copy".to_string(),
        }),
        "the entry under the cursor, and the view's own word"
    );

    // Esc leaves the mode, and only the second Esc leaves the view.
    a.key(Key::Esc);
    assert!(!a.plugin_cursor(), "esc left the mode");
    assert_eq!(a.focused_surface(), Some(Surface::PluginPane));
}
