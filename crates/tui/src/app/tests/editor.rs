//! The editor pane: rano inside the head — getting there by click and by chord, the keyboard
//! crossing between it and the composer, leaving it, and a send landing in the composer.

use super::*;
use rano::term::{Event, KeyCode, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A workspace on disk with the file the conversation edited, removed when the test ends.
struct Workspace(PathBuf);

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **A `.txt`, on purpose**: rano starts a language server for the languages it knows, and a
/// test has no business starting `rust-analyzer`.
const FILE: &str = "notes.txt";

fn workspace(tag: &str) -> Workspace {
    let dir = std::env::temp_dir().join(format!("letibot-editor-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let text: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    std::fs::write(dir.join(FILE), text).unwrap();
    Workspace(dir)
}

/// The edit the conversation made: line 11 of `notes.txt`, with two lines of context above
/// and one below — so the first CHANGED line is 11, not the excerpt's first line, 9.
fn change() -> letibot_sessionlog::event::ToolEdit {
    letibot_sessionlog::event::ToolEdit {
        path: FILE.into(),
        created: false,
        before_start: 9,
        after_start: 9,
        before_lines: 30,
        after_lines: 30,
        truncated: false,
        before: "line 9\nline 10\nold 11\nline 12".into(),
        after: "line 9\nline 10\nline 11\nline 12".into(),
    }
}

fn row(
    id: &str,
    call: &str,
    name: &str,
    edit: Option<letibot_sessionlog::event::ToolEdit>,
) -> (String, TranscriptItem) {
    (
        id.into(),
        TranscriptItem::ToolResult {
            call_id: call.into(),
            name: name.into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "done".into(),
            edit,
            origin: None,
            media: None,
        },
    )
}

/// A head attached to a session whose transcript is the operator's question, a `read` and the
/// `edit` of [`change`] — with the session's workspace being `ws`.
fn edited(ws: &Workspace) -> App {
    let hub = Hub::new("s");
    hub.publish(testing::appended("s.1", "user"));
    hub.record_item(
        "s.1",
        TranscriptItem::User {
            parts: vec![UserPart::Text {
                text: "please fix line eleven".into(),
            }],
            speaker: letibot_transcript::Speaker::Operator,
        },
    );
    for (id, item) in [
        row("s.2", "c1", "read", None),
        row("s.3", "c2", "edit", Some(change())),
    ] {
        hub.publish(testing::appended(&id, "tool_result"));
        hub.record_item(&id, item);
    }
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    a.tools = Fold::Open;
    a.wiring.workspace = ws.0.display().to_string();
    a
}

/// rano's ticks until the file it opened has arrived — the loop's job, done here by hand.
fn settle(a: &mut App) {
    for _ in 0..1000 {
        a.editor_tick(Instant::now());
        if !a.edit_pane.as_ref().is_some_and(|p| p.ed.loading()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    a.editor_tick(Instant::now());
}

/// Where rano is: the file and the 1-based line, as its own send would say them.
fn place(a: &App) -> (PathBuf, usize) {
    let e = a
        .edit_pane
        .as_ref()
        .expect("the pane is open")
        .ed
        .send_event();
    (e.path.expect("a named file"), e.cursor.line)
}

fn key(code: KeyCode, mods: Mods) -> Event {
    Event::Key(KeyEvent::new(code, mods))
}

fn ctrl(c: char) -> Event {
    key(KeyCode::Char(c), Mods::CTRL)
}

fn press(x: u16, y: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseKind::Press(MouseButton::Left),
        x,
        y,
        mods: Mods::NONE,
    })
}

/// Type `text` through the router, as the terminal would deliver it.
fn route_text(a: &mut App, text: &str) -> Vec<Key> {
    a.route(
        text.chars()
            .map(|c| key(KeyCode::Char(c), Mods::NONE))
            .collect(),
    )
    .0
}

fn is_the_file(p: &Path) -> bool {
    p.file_name().is_some_and(|n| n == FILE)
}

/// **The map**: every screen row of the edit's row — its header and its diff — is a place in
/// `notes.txt` at the first changed line, and no other row is.
#[test]
fn the_rows_of_an_edit_are_mapped_to_its_file_and_its_first_changed_line() {
    let ws = workspace("map");
    let mut a = edited(&ws);
    let screen = a.screen(100, 40);
    assert!(!a.file_rows.is_empty(), "{screen:#?}");
    for (y, f) in &a.file_rows {
        assert_eq!(f.path, FILE);
        assert_eq!(f.line, 11, "the first CHANGED line, past the context");
        assert!(*y >= 1, "the header is not a row of the conversation");
    }
    // The row that shows the changed line is in the map; the question and the read are not.
    let at = |needle: &str| {
        screen
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not on screen: {screen:#?}"))
    };
    let mapped = |y: usize| a.file_rows.iter().any(|(r, _)| *r == y);
    assert!(mapped(at("old 11")), "the diff is part of the row");
    assert!(!mapped(at("please fix line eleven")));
    // Contiguous: one row, one run of screen rows.
    let ys: Vec<usize> = a.file_rows.iter().map(|(y, _)| *y).collect();
    assert!(ys.windows(2).all(|w| w[1] == w[0] + 1), "{ys:?}");
}

/// **A click on an edit row opens the change in a popup, not the editor** — the operator,
/// 2026-10-08: *"I dont need the full rano chrome when I look on the full edit, but i do need
/// the context - so full file with diff and cursor placement at the beginning of the first diff.
/// preferably in a popup and of course Esc must just close it"*.
#[test]
fn a_click_on_an_edit_row_opens_the_change_in_a_popup_not_the_editor() {
    let ws = workspace("click");
    let mut a = edited(&ws);
    a.screen(100, 40);
    let (y, _) = a.file_rows[0].clone();
    assert_eq!(a.key(Key::Click { x: 10, y: y as u16 }), None);
    assert!(a.diff_popup.is_some(), "a click opens the popup");
    assert!(a.edit_pane.is_none(), "and not rano's editor");
    let screen = a.screen(100, 40).join("\n");
    // The whole file, the change inline: the unchanged lines around it, and the change itself.
    assert!(
        screen.contains("line 1 ")
            || screen.contains("line 1\n")
            || screen.contains("line 1 │")
            || screen.contains(" line 1"),
        "{screen}"
    );
    assert!(
        screen.contains("old 11"),
        "the removed line is shown: {screen}"
    );
    assert!(
        screen.contains("line 11"),
        "the added line is shown: {screen}"
    );
    // No editor chrome: no title bar, no function bar, no status line.
    assert!(!screen.contains("rano 0."), "{screen}");
    assert!(!screen.contains("C-q Exit"), "{screen}");
    assert!(!screen.contains("Review notes.txt"), "{screen}");
    // The popup names the file and its own keys.
    assert!(screen.contains("notes.txt"), "{screen}");
    assert!(screen.contains("esc closes"), "{screen}");
}

/// **The whole file is there, and it opens on the first change**: on a screen too short for
/// the file, the change is in view at once, and the file's first and last lines are a key away.
#[test]
fn the_popup_holds_the_whole_file_and_opens_on_the_first_change() {
    let ws = workspace("whole");
    let mut a = edited(&ws);
    a.screen(100, 14);
    let (y, _) = a.file_rows[0].clone();
    a.key(Key::Click { x: 10, y: y as u16 });
    let first = a.screen(100, 14).join("\n");
    assert!(
        first.contains("old 11"),
        "the first change is in view: {first}"
    );
    assert!(
        !first.contains("line 30"),
        "a short screen cannot hold the whole file: {first}"
    );
    a.key(Key::End);
    let end = a.screen(100, 14).join("\n");
    assert!(
        end.contains("line 30"),
        "the end of the file is a key away: {end}"
    );
    a.key(Key::Home);
    let home = a.screen(100, 14).join("\n");
    assert!(home.contains("line 1"), "and so is its start: {home}");
}

/// **Esc closes the popup, and only the popup.**
#[test]
fn esc_closes_the_diff_popup_onto_the_conversation() {
    let ws = workspace("popesc");
    let mut a = edited(&ws);
    let before = a.screen(100, 40);
    let (y, _) = a.file_rows[0].clone();
    a.key(Key::Click { x: 10, y: y as u16 });
    assert!(a.diff_popup.is_some());
    assert_eq!(a.key(Key::Esc), None);
    assert!(a.diff_popup.is_none(), "esc closed the popup");
    assert_eq!(
        a.screen(100, 40),
        before,
        "and the conversation is as it was"
    );
}

/// **From the popup, `ctrl-]` is the editor** — on the same change, for when reading turns into
/// changing.
#[test]
fn ctrl_bracket_in_the_popup_opens_the_editor_on_the_change() {
    let ws = workspace("popedit");
    let mut a = edited(&ws);
    a.screen(100, 40);
    let (y, _) = a.file_rows[0].clone();
    a.key(Key::Click { x: 10, y: y as u16 });
    a.key(Key::CtrlBracket);
    assert!(a.diff_popup.is_none(), "the popup gives way to the editor");
    assert!(a.editor_focused(), "the editor has the keyboard");
    settle(&mut a);
    let (path, line) = place(&a);
    assert!(is_the_file(&path), "{path:?}");
    assert_eq!(line, 11);
}

/// **A click anywhere else does what it did before** — nothing, on a row of the conversation.
#[test]
fn a_click_on_a_row_that_is_not_a_file_opens_nothing() {
    let ws = workspace("noclick");
    let mut a = edited(&ws);
    let before = a.screen(100, 40);
    let y = before
        .iter()
        .position(|l| l.contains("please fix line eleven"))
        .unwrap();
    assert_eq!(a.key(Key::Click { x: 10, y: y as u16 }), None);
    assert!(a.edit_pane.is_none());
    assert_eq!(a.screen(100, 40), before, "and the screen is as it was");
}

/// **`ctrl-]` opens the newest change**, the way the click does, from the composer.
#[test]
fn the_chord_opens_the_newest_change() {
    let ws = workspace("chord");
    let mut a = edited(&ws);
    a.screen(100, 40);
    a.key(Key::CtrlBracket);
    settle(&mut a);
    let (path, line) = place(&a);
    assert!(is_the_file(&path), "{path:?}");
    assert_eq!(line, 11);
    assert!(a.editor_focused());
}

/// **With nothing changed, the chord says so** rather than doing nothing a reader can see.
#[test]
fn the_chord_with_no_change_in_the_conversation_says_so() {
    let mut a = app();
    a.key(Key::CtrlBracket);
    assert!(a.edit_pane.is_none());
    assert!(
        a.notice
            .as_deref()
            .is_some_and(|n| n.contains("no edit or write")),
        "{:?}",
        a.notice
    );
}

/// **`ctrl-]` crosses both ways, and the keys follow the keyboard**: typed into rano while the
/// pane has it, into the composer once it has crossed — and in one read, the keys after the
/// chord go where it sent them.
#[test]
fn the_crossing_chord_moves_the_keyboard_and_the_keys_follow_it() {
    let ws = workspace("cross");
    let mut a = edited(&ws);
    a.screen(100, 40);
    a.key(Key::CtrlBracket);
    settle(&mut a);
    // Off the review and into the text, then type into the file.
    a.route(vec![key(KeyCode::Esc, Mods::NONE)]);
    assert!(route_text(&mut a, "zz").is_empty(), "rano took them");
    assert!(a.editor.text().is_empty());
    assert!(a.edit_pane.as_ref().unwrap().ed.send_event().modified);

    // Across, and the rest of the same read is the composer's.
    let mut read = vec![ctrl(']')];
    read.extend("hi".chars().map(|c| key(KeyCode::Char(c), Mods::NONE)));
    let (keys, _) = a.route(read);
    assert!(!a.editor_focused(), "the composer has the keyboard");
    assert!(a.editor_drawn(), "and the pane is still drawn");
    for k in keys {
        a.key(k);
    }
    assert_eq!(a.editor.text(), "hi");

    // And back, in one read too.
    let mut read = vec![ctrl(']')];
    read.push(key(KeyCode::Char('q'), Mods::NONE));
    let (keys, took) = a.route(read);
    assert!(a.editor_focused());
    assert!(keys.is_empty() && took, "the `q` was rano's");
    assert_eq!(a.editor.text(), "hi");
}

/// **rano's exit closes the pane — and never the head.** `^X` on an unmodified file, and
/// `ctrl-q`, which the head binds to the same command.
#[test]
fn rano_exit_closes_the_pane_and_never_the_head() {
    let ws = workspace("exit");
    for chord in ['x', 'q'] {
        let mut a = edited(&ws);
        a.screen(100, 40);
        a.key(Key::CtrlBracket);
        settle(&mut a);
        a.route(vec![key(KeyCode::Esc, Mods::NONE), ctrl(chord)]);
        a.editor_tick(Instant::now());
        assert!(a.edit_pane.is_none(), "ctrl-{chord} closed the pane");
        assert!(!a.should_quit(), "and the head is still here");
        // The conversation has its rectangle back, and the click map with it.
        a.screen(100, 40);
        assert!(!a.file_rows.is_empty());
    }
}

/// **An exit with unsaved edits is rano's question first**: the pane stays until it is answered.
#[test]
fn an_exit_over_unsaved_edits_asks_before_the_pane_goes() {
    let ws = workspace("unsaved");
    let mut a = edited(&ws);
    a.key(Key::CtrlBracket);
    settle(&mut a);
    a.route(vec![key(KeyCode::Esc, Mods::NONE)]);
    route_text(&mut a, "z");
    a.route(vec![ctrl('q')]);
    assert!(a.edit_pane.is_some(), "rano is asking whether to save");
    // No: the edit is dropped and the pane closes.
    route_text(&mut a, "n");
    assert!(a.edit_pane.is_none());
    let disk = std::fs::read_to_string(ws.0.join(FILE)).unwrap();
    assert!(!disk.contains('z'), "nothing was saved");
}

/// **A send lands in the composer as a reference, and is not sent**: `path:line` and a space
/// for the operator's words, with the keyboard moved there to write them.
#[test]
fn a_send_lands_in_the_composer_as_a_reference_and_waits_for_enter() {
    let ws = workspace("send");
    let mut a = edited(&ws);
    a.key(Key::CtrlBracket);
    settle(&mut a);
    // Whatever the attach queued is not this test's.
    a.take_actions();
    let (keys, _) = a.route(vec![key(KeyCode::Char('s'), Mods::ALT)]);
    assert!(keys.is_empty());
    assert_eq!(a.editor.text(), "notes.txt:11 ");
    assert!(!a.editor_focused(), "the keyboard went to the composer");
    assert!(a.editor_drawn(), "and the pane stays, to come back to");
    assert_eq!(a.take_actions(), Vec::new(), "nothing was submitted");
    // The operator's words follow, and Enter sends the whole line as an ordinary prompt.
    for k in route_text(&mut a, "why?") {
        a.key(k);
    }
    assert_eq!(a.editor.text(), "notes.txt:11 why?");
}

/// **A selection is sent as its range and its text, fenced**, and a range that ends at the
/// start of a line does not claim that line.
#[test]
fn a_send_with_a_selection_carries_the_range_and_the_text() {
    let ws = workspace("select");
    let mut a = edited(&ws);
    a.key(Key::CtrlBracket);
    settle(&mut a);
    a.route(vec![
        key(KeyCode::Esc, Mods::NONE),
        ctrl('a'),
        key(KeyCode::Down, Mods::NONE),
        key(KeyCode::Down, Mods::NONE),
        key(KeyCode::Char('s'), Mods::ALT),
    ]);
    assert_eq!(
        a.editor.text(),
        "notes.txt:11-12\n```txt\nline 11\nline 12\n```\n"
    );
}

/// **The pane takes the conversation's rectangle, and the chrome keeps its rows**: the
/// composer's box and the hint bar are still there, the bar naming the pane's keys — and the
/// caret is rano's while rano has the keyboard, hidden over the review, back on the text after.
#[test]
fn the_pane_takes_the_conversations_rectangle_and_the_composer_stays() {
    let ws = workspace("frame");
    let mut a = edited(&ws);
    let closed = a.screen(100, 30);
    a.key(Key::CtrlBracket);
    settle(&mut a);
    let open = a.screen(100, 30);
    assert_eq!(open.len(), closed.len());
    let text = open.join("\n");
    assert!(
        !text.contains("please fix line eleven"),
        "the conversation is covered"
    );
    assert!(
        text.contains('╭') && text.contains('╰'),
        "the composer's box: {text}"
    );
    assert!(text.contains("ctrl-] to the composer"), "{text}");
    assert_eq!(a.cursor(), None, "no caret over the review");

    a.route(vec![key(KeyCode::Esc, Mods::NONE)]);
    a.editor_tick(Instant::now());
    let text = a.screen(100, 30);
    let (row, _) = a.cursor().expect("rano's caret on the text");
    assert!(
        text[row].contains("line 11"),
        "on the changed line: {:?}",
        text[row]
    );

    // Crossed to the composer: the caret is the composer's and the bar says how to go back.
    a.route(vec![ctrl(']')]);
    let text = a.screen(100, 30).join("\n");
    assert!(text.contains("ctrl-] back to the editor"), "{text}");
    let (row, _) = a.cursor().unwrap();
    assert!(row > 20, "the composer's row, at the bottom");
}

/// **A press inside the pane takes the keyboard, and one outside gives it back** — the mouse
/// crosses the way the chord does.
#[test]
fn a_press_in_the_pane_takes_the_keyboard_and_one_outside_gives_it_back() {
    let ws = workspace("mouse");
    let mut a = edited(&ws);
    a.key(Key::CtrlBracket);
    settle(&mut a);
    a.screen(100, 30);
    let area = a.edit_pane.as_ref().unwrap().area;
    // Below the pane: the composer's rows.
    let (keys, _) = a.route(vec![press(10, area.y + area.h + 2)]);
    assert!(!a.editor_focused());
    assert_eq!(keys.len(), 1, "and it is still the head's click");
    let (keys, took) = a.route(vec![press(area.x + 5, area.y + 3)]);
    assert!(a.editor_focused());
    assert!(keys.is_empty() && took);
}

/// **The chord is advertised where the head's chords are**: `/help` names it, with what it does
/// on each side of the crossing.
#[test]
fn the_crossing_chord_is_in_help() {
    let a = app();
    let help = help_lines(&a.cfg, 140).join("\n");
    assert!(help.contains("ctrl-]"), "{help}");
    assert!(help.contains("alt-s"), "{help}");
}

/// **The acceptance suite's session, as the daemon would log it** — the operator's question,
/// the `edit` of [`change`] with the assistant row that made it, and a closing sentence — one
/// `Envelope` per line, the shape `letibot-tui --replay` reads.
fn acceptance_session() -> String {
    use letibot_sessionlog::event::{Envelope, SessionEvent};
    let t = "s#1";
    let item = |id: &str, item: TranscriptItem| SessionEvent::TranscriptContent {
        item_id: id.into(),
        item: Box::new(item),
    };
    let events = vec![
        testing::appended("s.1", "user"),
        item(
            "s.1",
            TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Operator,
                parts: vec![UserPart::Text {
                    text: "please fix line eleven".into(),
                }],
            },
        ),
        testing::turn_started(t),
        testing::progress(t),
        testing::proposed_on(t, "c1", "edit", FILE),
        SessionEvent::ToolStarted {
            turn_id: t.into(),
            call_id: "c1".into(),
            name: "edit".into(),
            access: "write".into(),
        },
        // The row that made the call, before the call's result — the daemon's order.
        testing::appended("s.2", "assistant"),
        item(
            "s.2",
            TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![letibot_transcript::ToolCall {
                    id: "c1".into(),
                    name: "edit".into(),
                    arguments: format!(r#"{{"path":"{FILE}"}}"#),
                }],
                truncated: false,
            },
        ),
        SessionEvent::ToolFinished {
            turn_id: t.into(),
            call_id: "c1".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload_digest: "fnv1a:0000000000000000".into(),
            inline_bytes: 4,
            full_bytes: 4,
            spill: None,
            repairs: 0,
            edit: Some(change()),
        },
        testing::appended("s.3", "tool_result"),
        item("s.3", row("s.3", "c1", "edit", Some(change())).1),
        testing::delta(t, "Line eleven is fixed."),
        testing::appended("s.4", "assistant"),
        item(
            "s.4",
            TranscriptItem::Assistant {
                text: "Line eleven is fixed.".into(),
                tool_calls: Vec::new(),
                truncated: false,
            },
        ),
        testing::turn_finished(t),
    ];
    events
        .into_iter()
        .enumerate()
        .map(|(i, event)| {
            let env = Envelope {
                session_id: "acceptance".into(),
                seq: i as u64 + 1,
                ts: 0,
                event,
            };
            serde_json::to_string(&env).unwrap() + "\n"
        })
        .collect()
}

/// **The acceptance suite replays this file, and it is generated, not hand-written**, so a
/// change to the protocol shows up here as a failing test rather than as a fixture the head
/// silently stops reading. `LETIBOT_BLESS=1` writes it again.
#[test]
fn the_acceptance_fixture_is_this_session() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/acceptance/fixtures/edit.jsonl");
    let want = acceptance_session();
    if std::env::var_os("LETIBOT_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &want).unwrap();
    }
    let got = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        got == want,
        "{} is stale: run `LETIBOT_BLESS=1 cargo test -p letibot-tui the_acceptance_fixture`",
        path.display()
    );
}

/// **A file changed since the edit falls back to the edit itself, and says so** — a whole-file
/// view of a file somebody has edited since would be a diff of their change, not this one.
#[test]
fn a_file_changed_since_the_edit_shows_the_edit_alone_and_says_why() {
    let ws = workspace("popstale");
    let mut a = edited(&ws);
    a.screen(100, 40);
    // Somebody edited line 11 again after the conversation's edit.
    let text: String = (1..=30)
        .map(|i| {
            if i == 11 {
                "line eleven, again\n".to_string()
            } else {
                format!("line {i}\n")
            }
        })
        .collect();
    std::fs::write(ws.0.join(FILE), text).unwrap();
    let (y, _) = a.file_rows[0].clone();
    a.key(Key::Click { x: 10, y: y as u16 });
    let screen = a.screen(100, 40).join("\n");
    assert!(
        screen.contains("the file has changed since this edit"),
        "{screen}"
    );
    assert!(
        screen.contains("old 11"),
        "the edit's own change is shown: {screen}"
    );
    assert!(
        !screen.contains("line 30"),
        "and not a whole file that is no longer its: {screen}"
    );
}

/// The whole file before the edit is the file now with the edit's `after` put back to `before`.
#[test]
fn the_file_before_the_edit_is_the_file_with_the_edit_undone() {
    let now: String = (1..=5).map(|i| format!("line {i}\n")).collect();
    let c = rano::review::Change {
        before: "line 2\nold 3\nline 4".into(),
        after: "line 2\nline 3\nline 4".into(),
        before_start: 2,
        after_start: 2,
    };
    assert_eq!(
        crate::app::diff_popup::whole_file_before(&now, &c).as_deref(),
        Some("line 1\nline 2\nold 3\nline 4\nline 5\n")
    );
    let other = now.replace("line 3", "line three");
    assert_eq!(crate::app::diff_popup::whole_file_before(&other, &c), None);
}
