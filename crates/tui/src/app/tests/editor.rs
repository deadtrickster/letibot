//! The editor pane: rano inside the head - getting there by click and by chord, the keyboard
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

/// A `.txt`, on purpose: rano starts a language server for the languages it knows, and a
/// test has no business starting `rust-analyzer`.
const FILE: &str = "notes.txt";

/// One directory per CALL, not per tag. Two tests both asked for `workspace("frame")`, and
/// running in parallel one's `Drop` removed the other's file mid-test - the editor then opened
/// nothing, now and then. A counter makes a repeated tag harmless.
fn workspace(tag: &str) -> Workspace {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-editor-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let text: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    std::fs::write(dir.join(FILE), text).unwrap();
    Workspace(dir)
}

/// The edit the conversation made: line 11 of `notes.txt`, with two lines of context above
/// and one below - so the first CHANGED line is 11, not the excerpt's first line, 9.
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
            edits: edit.into_iter().collect(),
            origin: None,
            media: None,
        },
    )
}

/// A head attached to a session whose transcript is the operator's question, a `read` and the
/// `edit` of [`change`] - with the session's workspace being `ws`.
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

/// rano's ticks until the file it opened has arrived - the loop's job, done here by hand.
fn settle(a: &mut App) {
    // Up to ten seconds: rano loads on a thread of its own, and under the whole workspace's tests
    // at once a second was not always enough - the frame test then read an empty buffer. It
    // returns the moment the load is done, so a quiet run pays nothing for the bound.
    for _ in 0..10_000 {
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

/// The pane open on the file itself: no change, no view, no prompt, so rano has nothing of its own
/// for Esc to cancel and the key is the host's.
fn idle_pane(tag: &str) -> (Workspace, App) {
    let ws = workspace(tag);
    let mut a = edited(&ws);
    a.open_in_editor(&FileRef::file(FILE));
    settle(&mut a);
    a.screen(100, 30);
    (ws, a)
}

/// Esc in a pane with nothing of rano's left to cancel leaves it, and the press that leaves is the
/// composer's first of the pair - the same two-tap interrupt, one press of it typed inside rano.
#[test]
fn esc_in_an_idle_pane_gives_the_composer_the_keys_and_arms_the_interrupt() {
    let (_ws, mut a) = idle_pane("esc");
    a.clock(1_000);
    assert!(a.editor_focused(), "the pane has the keys");

    let (keys, _) = a.route(vec![key(KeyCode::Esc, Mods::NONE)]);
    assert!(!a.editor_focused(), "esc left the pane");
    assert_eq!(keys, vec![Key::Esc], "and the key is the head's");
    for k in keys {
        a.key(k);
    }
    let bar = a.hint_bar(200);
    assert!(bar.contains("esc again to interrupt"), "{bar}");

    // The pair fires on a running turn although only the second press was typed in the composer.
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    assert!(matches!(a.key(Key::Esc), Some(Action::Interrupt(_))));
}

/// A head chord pressed in the pane names where it works and opens nothing: a refusal that says
/// nothing reads as a key that does nothing, and half-working chords are the foreign feeling the
/// one table exists to end.
#[test]
fn a_head_chord_in_the_pane_names_its_pane_and_opens_nothing() {
    let (_ws, mut a) = idle_pane("chord");
    assert!(a.editor_focused());

    let (keys, _) = a.route(vec![ctrl('s')]);
    assert!(keys.is_empty(), "the chord did not reach the head");
    assert_eq!(a.top_surface(), None, "no session picker");
    assert!(a.editor_focused(), "and the pane keeps the keys");
    assert_eq!(a.notice.as_deref(), Some("ctrl-s is sessions — esc first"));
}

/// Which side has the keyboard is on the frame: the side with the keys draws its edges `Strong` and
/// the other `Faint`, so the crossing is visible without reading the hint row.
#[test]
fn the_frames_edges_say_which_side_has_the_keys() {
    let (_ws, mut a) = idle_pane("focus");
    a.cfg.color = true;
    // The pane's top edge, found by the file it names - the composer's edges are the other frame
    // on this screen and they carry the other half of the marker.
    let pane_edge = |frame: &[String]| -> String {
        frame
            .iter()
            .find(|r| r.contains('╭') && r.contains(FILE))
            .cloned()
            .unwrap_or_else(|| panic!("no pane edge in {frame:#?}"))
    };
    let frame = a.screen(100, 30);
    assert!(
        pane_edge(&frame).trim_start().starts_with("\x1b[1m╭"),
        "the pane's frame is not the strong one: {frame:#?}"
    );
    let (top, _) = a.box_top(96);
    assert!(
        top.starts_with("\x1b[2m"),
        "the composer's edge is the faint one while the pane has the keys: {top:?}"
    );

    // One press below the pane: the composer has the keys, and the marker swaps sides.
    let area = a.edit_pane.as_ref().expect("just opened").area;
    a.route(vec![press(10, area.y + area.h + 2)]);
    let frame = a.screen(100, 30);
    assert!(
        pane_edge(&frame).trim_start().starts_with("\x1b[2m╭"),
        "the pane's frame is still the strong one: {frame:#?}"
    );
    let (top, _) = a.box_top(96);
    assert!(
        top.starts_with("\x1b[1m"),
        "the composer's edge is not the strong one: {top:?}"
    );
}

/// The pane spells its keys the way the rest of the frame does: rano's third notation, `words`, is
/// set on the embedded editor, so its own key edge cannot come back as `C-o` beside a `ctrl-s`.
#[test]
fn the_pane_spells_its_keys_in_words() {
    let (_ws, a) = idle_pane("words");
    let hints = a.edit_pane.as_ref().expect("just opened").ed.key_hints();
    assert!(!hints.is_empty(), "the pane draws no keys at all");
    for (k, _) in &hints {
        assert!(
            !k.contains("C-") && !k.contains("M-") && !k.contains('^'),
            "{k} is not the words spelling"
        );
    }
}

/// Every key the two programs promise is a row of `/help`: the table is the promise, and a page
/// that leaves one out is exactly the drift the table exists to prevent.
#[test]
fn the_help_lists_every_key_the_two_programs_share() {
    let a = app();
    let help = help_lines(&a.cfg, 400, &a.key_overrides);
    for row in rano::agent::keys::SHARED {
        let chord = crate::app::shared_keys::chord(row);
        // The help's key column, and the table's own sentence after it: a row drawn from the table
        // rather than a hand-written line that happens to name the same key.
        let want = format!("  {chord:<16}");
        assert!(
            help.iter()
                .any(|l| l.starts_with(&want) && l.contains(row.hint)),
            "{chord} is not a row of /help:\n{}",
            help.join("\n")
        );
    }
}

/// The map: every screen row of the edit's row - its header and its diff - is a place in
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

/// A click on an edit row opens the change in a popup, not the editor: the whole file with the
/// change inline and the cursor at the first diff. Esc closes it, and the conversation underneath is
/// untouched.
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

/// The whole file is there, and it opens on the first change: on a screen too short for
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

/// Esc closes the popup, and only the popup.
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

/// A permission card that arrives while the diff popup is open stays answerable. With a decision
/// open the card has its keys, the popup keeps the wheel, the page keys and Esc, and the bar names
/// both - a popup that took every key ahead of the decision ladder answered nothing.
#[test]
fn a_decision_under_the_diff_popup_takes_its_keys() {
    use letibot_sessionlog::event::OptionKind;
    let ws = workspace("popdecide");
    let mut a = edited(&ws);
    a.screen(100, 40);
    let (y, _) = a.file_rows[0].clone();
    a.key(Key::Click { x: 10, y: y as u16 });
    assert!(a.diff_popup.is_some());
    // A popup on its own names its keys and not the conversation's chords, dead under it.
    let bar = a.hint_bar(200);
    assert!(bar.contains("ctrl-] opens it in the editor"), "{bar}");
    assert!(!bar.contains("ctrl-s sessions"), "{bar}");

    a.open.push(decision_with(&[
        OptionKind::AllowOnce,
        OptionKind::RejectOnce,
    ]));
    let bar = a.hint_bar(200);
    assert!(bar.contains("a row number marks"), "{bar}");
    assert!(bar.contains("esc closes it"), "{bar}");
    // ↓ moves the ladder, not the diff.
    let scrolled = a.diff_popup.as_ref().unwrap().scroll;
    a.key(Key::Down);
    assert_eq!(a.sel, 1, "↓ moved the card's mark");
    assert_eq!(a.diff_popup.as_ref().unwrap().scroll, scrolled);
    // A row number marks the row, and Enter answers it.
    assert_eq!(a.key(Key::Char('1')), None);
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Answer {
            req_id: "d1".into(),
            option_id: "allow_once".into(),
            pattern: None,
            note: None,
        })
    );
    assert!(a.diff_popup.is_some(), "answering leaves the popup up");
}

/// From the popup, `ctrl-]` is the editor - on the same change, for when reading turns into
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

/// A click anywhere else does what it did before - nothing, on a row of the conversation.
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

/// `ctrl-]` opens the newest change, the way the click does, from the composer.
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

/// With nothing changed, the chord says so rather than doing nothing a reader can see.
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

/// `ctrl-]` crosses both ways, and the keys follow the keyboard: typed into rano while the
/// pane has it, into the composer once it has crossed - and in one read, the keys after the
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

/// rano's exit closes the pane - and never the head. `^X` on an unmodified file, and
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

/// An exit with unsaved edits is rano's question first: the pane stays until it is answered.
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

/// A send lands in the composer as a CHIP, and as no text at all: the words are the operator's and
/// the block the model reads is the daemon's to write. Nothing is sent until Enter.
#[test]
fn alt_s_lands_as_a_chip_and_inserts_no_text() {
    let ws = workspace("send");
    let mut a = edited(&ws);
    a.key(Key::CtrlBracket);
    settle(&mut a);
    // Whatever the attach queued is not this test's.
    a.take_actions();
    let (keys, _) = a.route(vec![key(KeyCode::Char('s'), Mods::ALT)]);
    assert!(keys.is_empty());
    assert_eq!(a.editor.text(), "", "the chip is not text in the line");
    assert_eq!(a.contexts.len(), 1, "the place is attached");
    assert!(
        a.contexts[0].label.contains("notes.txt:11"),
        "{}",
        a.contexts[0].label
    );
    assert!(!a.editor_focused(), "the keyboard went to the composer");
    assert!(a.editor_drawn(), "and the pane stays, to come back to");
    assert_eq!(a.take_actions(), Vec::new(), "nothing was submitted");
    // The operator sees it: the composer's own bottom edge carries the chip, so no transcript row
    // was spent and nothing under the caret moved.
    let screen = a.screen(100, 30).join("\n");
    assert!(screen.contains("[context 1"), "{screen}");
    assert!(screen.contains("notes.txt:11"), "{screen}");
    // The operator's words follow, and what the next message will carry is the place, not the words.
    for k in route_text(&mut a, "why?") {
        a.key(k);
    }
    assert_eq!(a.editor.text(), "why?");
    let context = a
        .contexts_to_send()
        .expect("the chip rides with the next message");
    assert_eq!(context.origin, "rano");
    assert_eq!(context.refs.len(), 1);
    assert_eq!(
        context.refs[0].file,
        ws.0.join(FILE).display().to_string(),
        "the wire carries the absolute path the daemon must read"
    );
    assert!(
        context.refs[0].text.is_empty(),
        "a bare place is no selection"
    );
    assert!(
        context
            .open
            .iter()
            .any(|b| b.file == ws.0.join(FILE).display().to_string()),
        "the file open in the pane is named: {context:?}"
    );
}

/// A selection is sent as its range and its text: the chip names the range, and the ref carries the
/// buffer's own lines - which for an unsaved buffer is the only copy of them.
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
    assert_eq!(a.editor.text(), "");
    assert!(
        a.contexts[0].label.contains("notes.txt:11-12"),
        "{}",
        a.contexts[0].label
    );
    let r = &a.contexts[0].r;
    // rano hands the selection's lines without a trailing newline: the reader's own text, which
    // the daemon numbers from the range's first line.
    assert_eq!(r.text, "line 11\nline 12");
    let range = r.range.expect("a selection is a range");
    assert_eq!((range.start.line, range.start.column), (11, 1));
    assert_eq!(r.lang, "txt");
    assert!(!r.unsaved, "nothing was typed into the buffer");
}

/// The pane takes the conversation's rectangle, and the chrome keeps its rows: the
/// composer's box and the hint bar are still there, the bar naming the pane's keys - and the
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
    assert!(text.contains("ctrl-] cross focus"), "{text}");
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
    assert!(text.contains("ctrl-] cross focus"), "{text}");
    assert!(text.contains("enter sends"), "{text}");
    let (row, _) = a.cursor().unwrap();
    assert!(row > 20, "the composer's row, at the bottom");
}

/// A press inside the pane takes the keyboard, and one outside gives it back - the mouse
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

/// At 160 columns the editor is a column on the LEFT and the conversation keeps the rest: a
/// click in either lands in it, and the caret is where the keyboard is. At 100 columns the two
/// cannot share, so the frame is the one there was - the pane over the whole rectangle.
#[test]
fn the_editor_sits_beside_the_conversation_and_zooms_when_the_columns_will_not_fit() {
    let ws = workspace("split");
    let mut a = edited(&ws);
    a.screen(160, 30);
    a.key(Key::CtrlBracket);
    // Off the review view and onto the text, where rano shows a caret.
    a.route(vec![key(KeyCode::Esc, Mods::NONE)]);
    settle(&mut a);
    let rows = a.screen(160, 30);
    let edw = a.editor_col;
    assert!(edw >= 40, "the editor took {edw} columns");
    // The pane's frame, the file in its top edge, in the left of the row it was drawn on.
    let top = rows
        .iter()
        .find(|l| l.contains('╭') && l.contains(FILE))
        .unwrap_or_else(|| panic!("no editor column: {rows:#?}"));
    let at = crate::app::keyhints::cells(top)
        .windows(FILE.chars().count())
        .position(|w| w.iter().collect::<String>() == FILE)
        .expect("the file's name in the edge");
    assert!(at < edw, "the name is in the editor's columns: {at}");
    // …and the conversation is to its right, on the same rows.
    let q = rows
        .iter()
        .position(|l| l.contains("please fix line eleven"))
        .expect("the conversation beside it");
    let qx = crate::app::keyhints::cells(&rows[q])
        .windows(6)
        .position(|w| w.iter().collect::<String>() == "please")
        .expect("the question's column");
    assert!(qx > edw, "the conversation is right of the border: {qx}");
    assert!(a.editor_focused(), "the pane has the keys");
    // The caret is rano's, in the editor's own columns.
    let (_, cx) = a.cursor().expect("the caret");
    assert!(cx < edw, "the caret is in the pane: {cx}");

    // A click on the conversation takes the keyboard out of the pane; one back in the pane's
    // text takes it back - the column's x is the terminal's, and neither is off by a column.
    let (keys, took) = a.route(vec![press((qx + 1) as u16, q as u16)]);
    assert!(!took && keys.len() == 1, "the conversation's click");
    assert!(!a.editor_focused());
    a.screen(160, 30);
    let (_, cx) = a.cursor().expect("the caret");
    assert!(cx > edw, "the composer's caret, right of the border: {cx}");
    let (keys, took) = a.route(vec![press((edw / 2) as u16, 3)]);
    assert!(took && keys.is_empty(), "the pane's click");
    assert!(a.editor_focused());

    // The overlay: the same pane over the whole rectangle, as it was before the split - its
    // text inset two columns from the frame, which sits at the gutter.
    a.screen(100, 30);
    assert_eq!(a.editor_col, 0, "too narrow to share");
    let rows = a.screen(100, 30);
    let qx = rows
        .iter()
        .position(|l| l.contains("please fix line eleven"));
    assert!(
        qx.is_none(),
        "the pane takes the rectangle: the conversation is not drawn beside it"
    );
    let area = a.edit_pane.as_ref().unwrap().area;
    let gutter = u16::try_from(App::gutter(100)).unwrap();
    assert_eq!(
        (area.x, area.w),
        (gutter + 2, 100 - 2 * gutter - 4),
        "the rectangle it always had: the gutter, then the frame's two columns"
    );
}

/// `alt-i` crosses without the chord and `alt-m` puts the pane back over the whole rectangle,
/// and the width the operator keyed is one the next start finds in `head.toml`.
#[test]
fn alt_i_crosses_alt_m_zooms_and_the_width_is_kept() {
    let ws = workspace("splitkeys");
    let dir = std::env::temp_dir().join(format!("letibot-editor-w-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("head.toml");
    let mut a = edited(&ws);
    a.prefs_path = Some(path.clone());
    a.screen(160, 30);
    a.key(Key::CtrlBracket);
    a.route(vec![key(KeyCode::Esc, Mods::NONE)]);
    settle(&mut a);
    a.screen(160, 30);
    assert!(a.editor_focused());
    let half = a.editor_col;
    assert_eq!(half, 80, "half the terminal");

    // alt-i inside the pane: the composer's keys, and the pane still beside it.
    let (keys, took) = a.route(vec![key(KeyCode::Char('i'), Mods::ALT)]);
    assert!(
        keys.is_empty() && took,
        "the chord is the head's, not rano's"
    );
    assert!(!a.editor_focused());
    a.screen(160, 30);
    assert_eq!(a.editor_col, half, "the pane stays drawn");
    // …and alt-i from the composer comes back.
    a.key(Key::AltI);
    assert!(a.editor_focused());

    // alt-m: the whole rectangle, exactly as it was before the split.
    a.route(vec![key(KeyCode::Char('m'), Mods::ALT)]);
    a.screen(160, 30);
    assert_eq!(a.editor_col, 0, "zoomed");
    assert!(a.editor_drawn() && a.editor_focused());
    a.key(Key::AltM);
    a.screen(160, 30);
    assert_eq!(a.editor_col, half, "and back beside the conversation");

    // The key resize, and the file it is kept in.
    a.key(Key::AltNarrow);
    assert_eq!(a.editor_width, Some(half - 4));
    a.screen(160, 30);
    assert_eq!(a.editor_col, half - 4);
    let mut b = app();
    b.prefs_path = Some(path);
    b.load_prefs();
    assert_eq!(b.editor_width, Some(half - 4), "the next start");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The chord is advertised where the head's chords are: `/help` names it, with what it does
/// on each side of the crossing.
#[test]
fn the_crossing_chord_is_in_help() {
    let a = app();
    let help = help_lines(&a.cfg, 140, &a.key_overrides).join("\n");
    assert!(help.contains("ctrl-]"), "{help}");
    assert!(help.contains("alt-s"), "{help}");
}

/// The acceptance suite's session, as the daemon would log it - the operator's question,
/// the `edit` of [`change`] with the assistant row that made it, and a closing sentence - one
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
        // The row that made the call, before the call's result - the daemon's order.
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
            edits: vec![change()],
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

/// The acceptance suite replays this file, and it is generated, not hand-written, so a
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

/// A file changed since the edit falls back to the edit itself, and says so - a whole-file
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

/// Scrolling the popup moves the popup and nothing else, and there is no caret: the keys are the
/// popup's while it is open, so a caret parked in the composer is an affordance for a field nobody
/// is typing in.
#[test]
fn scrolling_the_popup_repaints_only_the_popup_and_shows_no_caret() {
    let ws = workspace("popscroll");
    let mut a = edited(&ws);
    a.screen(100, 14);
    let (y, _) = a.file_rows[0].clone();
    a.key(Key::Click { x: 10, y: y as u16 });
    let first = a.screen(100, 14);
    assert_eq!(a.cursor(), None, "a caret over the popup: {first:#?}");
    let mut frames = vec![first.clone()];
    for k in [
        Key::WheelDown,
        Key::WheelDown,
        Key::WheelUp,
        Key::WheelDown,
        Key::WheelUp,
    ] {
        let said = format!("{k:?}");
        a.take_repaint();
        a.key(k);
        assert!(
            !a.take_repaint(),
            "{said} asked for a full repaint: the whole screen erased for a scroll"
        );
        frames.push(a.screen(100, 14));
        assert_eq!(a.cursor(), None, "a caret over the popup after {said}");
    }
    // The popup's own rows move; the header and everything under the popup do not.
    let moved: Vec<usize> = (0..first.len())
        .filter(|&i| frames.iter().any(|f| f[i] != first[i]))
        .collect();
    assert!(!moved.is_empty(), "the wheel moved nothing");
    let popup_end = first
        .iter()
        .position(|l| l.contains("esc closes"))
        .expect("the popup's footer");
    assert!(
        moved.iter().all(|&i| i >= 1 && i < popup_end),
        "rows outside the popup changed while it scrolled: {moved:?} (popup ends at {popup_end})\n{}",
        frames
            .iter()
            .map(|f| f.join("\n"))
            .collect::<Vec<_>>()
            .join("\n-----\n")
    );
}

/// `ctrl-e` on an empty prompt is the editor, with nothing changed to open it on: rano opens with its
/// own `Open:` prompt up so the next thing typed is a path, and `ctrl-]` comes back as from any pane.
#[test]
fn ctrl_e_on_an_empty_prompt_opens_the_editor_asking_for_a_file() {
    let ws = workspace("ctrle");
    let mut a = app();
    a.session_id = "s".into();
    a.screen(100, 30);
    assert_eq!(a.key(Key::CtrlE), None);
    let pane = a.edit_pane.as_ref().expect("ctrl-e opened the editor");
    assert!(pane.focused, "and gave it the keyboard");
    let screen = a.screen(100, 30).join("\n");
    assert!(screen.contains("Open:"), "rano asks for a file: {screen}");
    // The path typed goes to rano's prompt, and Enter opens it.
    let path = ws.0.join(FILE).display().to_string();
    assert!(route_text(&mut a, &path).is_empty(), "rano took the path");
    a.route(vec![key(KeyCode::Enter, Mods::NONE)]);
    settle(&mut a);
    let screen = a.screen(100, 30).join("\n");
    assert!(screen.contains("line 11"), "the file is open: {screen}");
    assert!(
        a.editor.text().is_empty(),
        "nothing leaked into the composer"
    );
    // And back to the composer, with the pane kept.
    a.route(vec![ctrl(']')]);
    assert!(!a.editor_focused());
    assert!(a.edit_pane.is_some());
    // ctrl-e on the empty prompt now puts the pane away: the conversation is back, the pane kept.
    a.key(Key::CtrlE);
    assert!(a.edit_pane.is_some(), "put away, not closed");
    assert!(!a.editor_drawn() && !a.editor_focused());
    let screen = a.screen(100, 30).join("\n");
    assert!(
        !screen.contains("line 11"),
        "the conversation has the screen: {screen}"
    );
    // And again brings it back with the keyboard, the same pane on the same file.
    a.key(Key::CtrlE);
    assert!(a.editor_focused());
    let screen = a.screen(100, 30).join("\n");
    assert!(
        screen.contains("line 11"),
        "the same pane, the same file: {screen}"
    );
}

/// The editor stays on the screen while the prompt is typed in - *"editor is just a
/// pane"*. `ctrl-]` crosses to the composer and back; only `ctrl-e` puts it away, and `ctrl-]`
/// (or a click on a change) brings a put-away pane back too.
#[test]
fn typing_in_the_prompt_leaves_the_editor_up_and_ctrl_bracket_brings_back_a_hidden_one() {
    let ws = workspace("hide");
    let mut a = edited(&ws);
    a.screen(100, 40);
    a.key(Key::CtrlBracket);
    settle(&mut a);
    a.route(vec![ctrl(']')]);
    typed(&mut a, "why is line 11 like this");
    assert!(a.editor_drawn(), "typing hid the editor");
    // A typed prompt keeps ctrl-e as end-of-line, so the pane stays.
    a.key(Key::CtrlE);
    assert!(a.editor_drawn());
    // Empty prompt: away, and ctrl-] brings it back with the keyboard.
    while !a.editor.text().is_empty() {
        a.key(Key::Backspace);
    }
    a.key(Key::CtrlE);
    assert!(!a.editor_drawn());
    a.key(Key::CtrlBracket);
    assert!(
        a.editor_focused(),
        "ctrl-] did not bring back the put-away pane"
    );
}

/// With text in the prompt, `ctrl-e` is end-of-line, as it always was. And inside a pane
/// that reads `End` - the result window - it is still `End`.
#[test]
fn ctrl_e_with_text_in_the_prompt_is_end_of_line() {
    let mut a = app();
    a.session_id = "s".into();
    typed(&mut a, "hello world");
    a.key(Key::Home);
    a.key(Key::CtrlE);
    assert!(
        a.edit_pane.is_none(),
        "ctrl-e opened the editor over a typed prompt"
    );
    a.key(Key::Char('!'));
    assert_eq!(
        a.editor.text(),
        "hello world!",
        "ctrl-e did not go to the end"
    );
}

/// `ctrl-]` with nothing changed says where the editor is, rather than dead-ending.
#[test]
fn ctrl_bracket_with_nothing_changed_points_at_ctrl_e() {
    let mut a = app();
    a.session_id = "s".into();
    a.key(Key::CtrlBracket);
    assert!(
        a.notice.as_deref().is_some_and(|n| n.contains("ctrl-e")),
        "{:?}",
        a.notice
    );
}

/// The pane is framed in the head's own look, not nano's: no `rano 0.x` title row and no two-row
/// function bar. The file sits in the top edge, rano's keys faint in the bottom one, and a change
/// says `modified` there.
#[test]
fn the_editor_pane_wears_the_heads_frame_not_nanos() {
    let ws = workspace("frame");
    let mut a = edited(&ws);
    a.screen(100, 40);
    a.key(Key::CtrlBracket);
    settle(&mut a);
    a.route(vec![key(KeyCode::Esc, Mods::NONE)]);
    settle(&mut a);
    let screen = a.screen(100, 40);
    let flat = screen.join("\n");
    assert!(!flat.contains("rano 0."), "rano's title row: {flat}");
    assert!(!flat.contains("C-q Exit"), "nano's function bar: {flat}");
    let top = screen
        .iter()
        .find(|l| l.contains('╭') && l.contains("notes.txt"))
        .unwrap_or_else(|| panic!("no top edge naming the file: {flat}"));
    assert!(!top.contains("modified"), "{top}");
    assert!(
        screen
            .iter()
            .any(|l| l.contains('╰') && l.contains("write out")),
        "rano's keys in the bottom edge: {flat}"
    );
    // The close chord is the one the edge names, and the older one still closes the pane without
    // being advertised anywhere (`rano_exit_closes_the_pane_and_never_the_head` has both).
    let edge = screen
        .iter()
        .find(|l| l.contains('╰') && l.contains("write out"))
        .expect("the bottom edge");
    assert!(
        edge.contains("ctrl-x exit"),
        "the close chord on the edge: {edge:?}"
    );
    assert!(
        !edge.contains("ctrl-q") && !edge.contains("C-q"),
        "the old chord is not advertised: {edge:?}"
    );
    // The text is inside the frame, and the cursor's place is still said.
    assert!(flat.contains("line 11"), "{flat}");
    assert!(flat.contains("Ln 11"), "{flat}");
    // Typing into the file says so in the edge.
    route_text(&mut a, "z");
    settle(&mut a);
    let screen = a.screen(100, 40);
    assert!(
        screen
            .iter()
            .any(|l| l.contains('╭') && l.contains("notes.txt") && l.contains("modified")),
        "{}",
        screen.join("\n")
    );
}

/// rano stays inside the frame: two columns in from each side, like the diff popup. Given the frame's
/// width less the LEFT indent only, a long line ran to the corner and the `Ln/Col` readout sat on it.
#[test]
fn the_editor_keeps_two_columns_clear_on_the_right_of_its_frame() {
    let ws = workspace("margin");
    std::fs::write(ws.0.join(FILE), format!("{}\nshort\n", "x".repeat(300))).unwrap();
    let mut a = app();
    a.session_id = "s".into();
    a.screen(100, 30);
    a.key(Key::CtrlE);
    route_text(&mut a, &ws.0.join(FILE).display().to_string());
    a.route(vec![key(KeyCode::Enter, Mods::NONE)]);
    settle(&mut a);
    let screen = a.screen(100, 30);
    let plain: Vec<String> = screen.clone();
    let top = plain
        .iter()
        .position(|l| l.contains('╭'))
        .expect("the frame's top edge");
    let bottom = plain
        .iter()
        .position(|l| l.contains('╰'))
        .expect("the frame's bottom edge");
    let right = plain[top].chars().count() - 1;
    assert!(plain[top].ends_with('╮'), "{:?}", plain[top]);
    assert!(
        plain[top + 1..bottom].iter().any(|l| l.contains("xxxx")),
        "the long line is drawn: {plain:#?}"
    );
    assert!(
        plain[top + 1..bottom].iter().any(|l| l.contains("Ln 1")),
        "{plain:#?}"
    );
    for l in &plain[top + 1..bottom] {
        let cells: Vec<char> = l.chars().collect();
        for col in right - 1..=right {
            assert!(
                cells.get(col).is_none_or(|c| *c == ' '),
                "column {col} of the frame's margin is drawn on: {l:?}"
            );
        }
    }
}

/// The popup names its file once, in its top edge - the operator: the path was
/// in the edge and again on the first row, because the diff renderer heads every edit view with
/// the file's name (right for a card in the conversation, a repeat inside a frame that says it).
#[test]
fn the_diff_popup_names_its_file_once() {
    let ws = workspace("popname");
    let mut a = edited(&ws);
    a.screen(100, 40);
    let (y, _) = a.file_rows[0].clone();
    a.key(Key::Click { x: 10, y: y as u16 });
    let screen = a.screen(100, 40);
    let top = screen
        .iter()
        .position(|l| l.contains('╭'))
        .expect("the popup's top edge");
    assert!(
        screen[top].contains(FILE),
        "the edge names it: {:?}",
        screen[top]
    );
    let named = screen[top + 1..]
        .iter()
        .take_while(|l| !l.contains('╰'))
        .filter(|l| l.trim().ends_with(FILE))
        .count();
    assert_eq!(
        named,
        0,
        "the file is named again inside the popup:\n{}",
        screen.join("\n")
    );
}

/// The diff's style is discoverable where the diff is drawn. `/diff` switches split and
/// unified, and the popup's own edge is the only thing a reader looking at a change sees; nothing
/// there named it, which is the operator asking *"how to switch diff style?"*.
#[test]
fn the_diff_popup_names_the_switch_that_draws_it_two_ways() {
    let ws = workspace("popstyle");
    let mut a = edited(&ws);
    a.screen(100, 40);
    let (y, _) = a.file_rows[0].clone();
    a.key(Key::Click { x: 10, y: y as u16 });
    let screen = a.screen(120, 40);
    let edge = screen
        .iter()
        .find(|l| l.contains('╰'))
        .expect("the popup's bottom edge");
    // The whole edge, as the reader sees it: every key it takes, and then the switch.
    assert!(
        edge.contains("esc closes · ↑↓ pgup/pgdn home/end · ctrl-] edits it · /diff split|unified"),
        "{edge:?}"
    );

    // And the switch is a button, like every other hint: a click on it asks for that picker.
    let col = crate::app::keyhints::cells(edge)
        .windows("/diff".chars().count())
        .position(|w| w.iter().collect::<String>() == "/diff")
        .expect("the hint's column");
    let y = screen
        .iter()
        .position(|l| l.contains('╰'))
        .expect("the edge's row");
    a.key(Key::Click {
        x: col as u16,
        y: y as u16,
    });
    assert_eq!(a.take_injected(), b"/diff\r", "the hint types the verb");
}

/// The popup closes by a click on its own `esc closes` - the operator: *"i open
/// diff popup by clicking - i cant close it by clicking … all keymap items anywhere must be
/// clickable"*. The hint is a button that types its key, by the route a keypress takes.
#[test]
fn a_click_on_a_key_hint_is_that_key() {
    fn click_text(a: &mut App, screen: &[String], needle: &str) {
        let (y, row) = screen
            .iter()
            .enumerate()
            .find(|(_, l)| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} is not on the screen: {screen:#?}"));
        let col = crate::app::keyhints::cells(row)
            .windows(needle.chars().count())
            .position(|w| w.iter().collect::<String>() == needle)
            .expect("the hint's column");
        a.key(Key::Click {
            x: col as u16,
            y: y as u16,
        });
        let typed = a.take_injected();
        assert!(!typed.is_empty(), "the click on {needle:?} typed nothing");
        for k in a.route(rano::term::decode(&typed)).0 {
            a.key(k);
        }
    }
    let ws = workspace("hintclick");
    let mut a = edited(&ws);
    a.screen(100, 40);
    let (y, _) = a.file_rows[0].clone();
    a.key(Key::Click { x: 10, y: y as u16 });
    assert!(a.diff_popup.is_some());
    let screen = a.screen(100, 40);
    click_text(&mut a, &screen, "esc closes");
    assert!(
        a.diff_popup.is_none(),
        "the popup's `esc closes` did not close it"
    );

    // The hint bar's `ctrl-g subagents` opens the subagents pane.
    let screen = a.screen(160, 40);
    click_text(&mut a, &screen, "ctrl-g subagents");
    let screen = a.screen(160, 40).join("\n");
    assert!(screen.contains("subagents"), "{screen}");
    assert!(
        a.focused_surface() == Some(crate::app::Surface::Subagents),
        "{screen}"
    );
}

/// **The pointer: hover marks what a click would do, a still press is a click, and a drag
/// selects and copies** - the operator: *"simple mouse selection doesnt work in
/// letibot - only with shift"*, and *"i want clickable things to be somehow hinted"*.
#[test]
fn hover_marks_a_button_a_still_press_clicks_and_a_drag_copies() {
    let ws = workspace("pointer");
    let mut a = edited(&ws);
    let features = rano::term::Features {
        clipboard: true,
        ..Default::default()
    };
    a.set_features(features);
    let _screen = a.screen(100, 40);
    let (y, _) = a.file_rows[0].clone();

    // A press and a release with no motion between them: the edit row's click, the popup.
    a.key(Key::Press { x: 10, y: y as u16 });
    a.key(Key::Release { x: 10, y: y as u16 });
    assert!(
        a.diff_popup.is_some(),
        "a still press-and-release was not a click"
    );

    // Hover over the popup's `esc closes`: a tad brighter, and the hand asked for.
    let screen2 = a.screen(100, 40);
    let (hy, row) = screen2
        .iter()
        .enumerate()
        .find(|(_, l)| l.contains("esc closes"))
        .expect("the popup's footer");
    let hx = crate::app::keyhints::cells(row)
        .windows(3)
        .position(|w| w.iter().collect::<String>() == "esc")
        .unwrap();
    a.key(Key::Hover {
        x: hx as u16,
        y: hy as u16,
    });
    let marked = a.screen(100, 40);
    assert!(
        marked[hy].contains("\x1b[22m") && !marked[hy].contains("\x1b[4m"),
        "not brightened (or underlined): {:?}",
        marked[hy]
    );
    assert_eq!(a.take_pointer(), Some(true), "the hand was not asked for");
    a.key(Key::Hover { x: 0, y: 0 });
    assert_eq!(a.take_pointer(), Some(false));
    a.key(Key::Esc);

    // A drag across the question's row selects it; the release copies it, gutter off.
    let screen = a.screen(100, 40);
    let qy = screen
        .iter()
        .position(|l| l.contains("please fix line eleven"))
        .expect("the question");
    let qx = crate::app::keyhints::cells(&screen[qy])
        .windows(6)
        .position(|w| w.iter().collect::<String>() == "please")
        .unwrap();
    a.key(Key::Press {
        x: qx as u16,
        y: qy as u16,
    });
    a.key(Key::Drag {
        x: (qx + 10) as u16,
        y: qy as u16,
    });
    let during = a.screen(100, 40);
    assert!(
        during[qy].contains("\x1b[7m"),
        "the selection is not drawn: {:?}",
        during[qy]
    );
    a.key(Key::Release {
        x: (qx + 10) as u16,
        y: qy as u16,
    });
    assert_eq!(a.take_clipboard().as_deref(), Some("please fix"));
    assert!(a.diff_popup.is_none(), "a drag was taken for a click");
}

/// A selection holds the VIEW still, not the keyboard: a drag copies and then holds the frame while
/// the operator keeps typing into a prompt under it, so a head that went deaf there would trade a
/// corrupted prompt for one that cannot be written at all.
#[test]
fn a_selection_does_not_swallow_what_is_typed() {
    let ws = workspace("pointer-typing");
    let mut a = edited(&ws);
    let screen = a.screen(100, 40);
    let qy = screen
        .iter()
        .position(|l| l.contains("please fix line eleven"))
        .expect("the operator's row");
    let qx = crate::app::keyhints::cells(&screen[qy])
        .windows(6)
        .position(|w| w.iter().collect::<String>() == "please")
        .unwrap();
    a.key(Key::Press {
        x: qx as u16,
        y: qy as u16,
    });
    a.key(Key::Drag {
        x: (qx + 10) as u16,
        y: qy as u16,
    });
    a.key(Key::Release {
        x: (qx + 10) as u16,
        y: qy as u16,
    });
    assert!(a.selection.is_some(), "the premise: a selection is up");
    assert!(a.selection_held, "and the frame is held under it");
    typed(&mut a, "c");
    assert_eq!(
        a.editor.text(),
        "c",
        "a selection is no reason to swallow a key — the ⌘C fix is upstream, not here"
    );
}

/// A code block copies as code - the operator: *"can i copy code block without
/// vertical bars?"*. The block's frame is drawn as text (`┌─ lang`, a `│ ` rail, `└─`), and a
/// selection across it copies the code alone, its own indentation kept.
#[test]
fn a_selected_code_block_copies_without_its_frame() {
    let hub = Hub::new("s");
    hub.publish(testing::appended("s.1", "assistant"));
    hub.record_item(
        "s.1",
        TranscriptItem::Assistant {
            text: "Here it is:\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n\nDone."
                .into(),
            tool_calls: vec![],
            truncated: false,
        },
    );
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    let features = rano::term::Features {
        clipboard: true,
        ..Default::default()
    };
    a.set_features(features);
    let screen = a.screen(100, 30);
    let top = screen
        .iter()
        .position(|l| l.contains("┌─"))
        .unwrap_or_else(|| panic!("no code block drawn: {screen:#?}"));
    let bottom = screen
        .iter()
        .position(|l| l.contains("└─"))
        .expect("its foot");
    a.key(Key::Press {
        x: 0,
        y: top as u16,
    });
    a.key(Key::Drag {
        x: 99,
        y: bottom as u16,
    });
    // The frame is not selectable: mid-drag, a code line is marked from after its rail, and
    // the `┌─` row is not marked at all - what is highlighted is what will be copied.
    let during = a.screen(100, 30);
    let line = during
        .iter()
        .find(|l| l.contains("fn main"))
        .expect("the code line");
    let before_mark = &line[..line.find("\x1b[7m").expect("the code line is marked")];
    let shown: String = crate::app::keyhints::cells(before_mark)
        .into_iter()
        .collect();
    assert!(
        shown.ends_with("│ "),
        "the mark starts before the rail ends: {shown:?}"
    );
    assert!(
        !during[top].contains("\x1b[7m"),
        "the frame row is marked: {:?}",
        during[top]
    );
    a.key(Key::Release {
        x: 99,
        y: bottom as u16,
    });
    assert_eq!(
        a.take_clipboard().as_deref(),
        Some("fn main() {\n    println!(\"hi\");\n}"),
        "{}",
        screen.join("\n")
    );
    // From the sentence before it to the one after: the prose stays, only the frame goes.
    let here = screen
        .iter()
        .position(|l| l.contains("Here it is:"))
        .unwrap();
    let done = screen.iter().position(|l| l.contains("Done.")).unwrap();
    a.key(Key::Press {
        x: 0,
        y: here as u16,
    });
    a.key(Key::Drag {
        x: 99,
        y: done as u16,
    });
    a.key(Key::Release {
        x: 99,
        y: done as u16,
    });
    let copied = a.take_clipboard().unwrap_or_default();
    assert!(copied.starts_with("Here it is:"), "{copied:?}");
    assert!(
        copied.contains("\nfn main() {\n    println!(\"hi\");\n}\n"),
        "{copied:?}"
    );
    assert!(copied.trim_end().ends_with("Done."), "{copied:?}");
    assert!(
        !copied.contains('│') && !copied.contains("┌") && !copied.contains("└"),
        "{copied:?}"
    );
}

/// A drag holds the view, and Esc drops the selection and lets it follow - the operator,
/// *"can we auto trigger ctrl-p once selection starts?"* and *"cancel selection on
/// esc"*. A hold the operator took with ctrl-p is theirs: dropping a selection leaves it.
#[test]
fn a_drag_holds_the_view_and_esc_lets_it_follow() {
    let ws = workspace("dragheld");
    let mut a = edited(&ws);
    let screen = a.screen(100, 40);
    let qy = screen
        .iter()
        .position(|l| l.contains("please fix line eleven"))
        .expect("the question");
    a.key(Key::Press { x: 4, y: qy as u16 });
    a.key(Key::Drag { x: 8, y: qy as u16 });
    assert!(a.hold, "a drag did not hold the view");
    // Still drawn as it moves, on the held frame.
    let one = a.screen(100, 40);
    a.key(Key::Drag {
        x: 14,
        y: qy as u16,
    });
    let two = a.screen(100, 40);
    assert!(
        two[qy].contains("\x1b[7m"),
        "the held frame lost the selection"
    );
    assert_ne!(
        one[qy], two[qy],
        "the selection did not follow the drag on a held view"
    );
    a.key(Key::Release {
        x: 14,
        y: qy as u16,
    });
    assert!(a.hold, "the copy let go of the hold");
    // Esc: the selection goes, the view follows, and nothing else took the key.
    a.key(Key::Esc);
    assert!(
        a.selection.is_none() && !a.hold,
        "esc left the selection or the hold"
    );

    // The operator's own hold is theirs.
    let _ = a.toggle_hold();
    assert!(a.hold);
    a.key(Key::Press { x: 4, y: qy as u16 });
    a.key(Key::Drag { x: 9, y: qy as u16 });
    a.key(Key::Release { x: 9, y: qy as u16 });
    a.key(Key::Esc);
    assert!(a.selection.is_none(), "esc left the selection");
    assert!(
        a.hold,
        "dropping a selection released the operator's own ctrl-p hold"
    );
}

/// The whole clickable block lights up - the operator: *"can you light up the
/// whole clickable block?"*. An edit card is several rows and any of them opens it, so the pointer
/// over one of them brightens every one.
#[test]
fn hovering_an_edit_card_brightens_all_of_its_rows() {
    let ws = workspace("cardblock");
    let mut a = edited(&ws);
    a.screen(100, 40);
    let rows: Vec<usize> = a.file_rows.iter().map(|(r, _)| *r).collect();
    assert!(
        rows.len() > 1,
        "the premise: the card is several rows: {rows:?}"
    );
    let top = a
        .hits
        .iter()
        .find(|h| h.target == crate::app::Target::Conversation)
        .unwrap()
        .rect
        .y;
    a.key(Key::Hover {
        x: 10,
        y: (top + rows[rows.len() - 1]) as u16,
    });
    let screen = a.screen(100, 40);
    // Every row of the card with something on it - a blank spacer has nothing to brighten.
    for r in rows.iter().filter(|r| !screen[top + **r].trim().is_empty()) {
        assert!(
            screen[top + r].contains("\x1b[22m"),
            "row {r} of the card is not lit: {:?}",
            screen[top + r]
        );
    }
}

/// A note the `notes` tool rewrote is an edit card: one header naming the file once, the diff, no
/// prose, and a click opens it - the tool made the change, so it must behave like a regular diff
/// card.
#[test]
fn a_note_the_notes_tool_rewrote_is_drawn_and_clicked_as_an_edit() {
    let ws = workspace("notes");
    let hub = Hub::new("s");
    let (id, mut item) = row("s.1", "c1", "notes", Some(change()));
    if let TranscriptItem::ToolResult { payload, .. } = &mut item {
        *payload = format!("replaced `{FILE}` — was 2.0 KB, 30 line(s)\nsecond\nthird\nfourth");
    }
    hub.publish(testing::appended(&id, "tool_result"));
    hub.record_item(&id, item);
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    a.wiring.workspace = ws.0.display().to_string();
    let screen = a.screen(100, 40);
    let all = screen.join("\n");
    assert!(all.contains("old 11"), "the diff is drawn: {all}");
    assert!(!all.contains("replaced `"), "no tool prose: {all}");
    assert!(!all.contains("ctrl-v opens it"), "no payload fold: {all}");
    assert!(
        !all.contains("detected afterwards"),
        "the tool made it: {all}"
    );
    assert_eq!(
        screen.iter().filter(|l| l.contains(FILE)).count(),
        1,
        "the file is named once: {all}"
    );
    assert!(!a.file_rows.is_empty(), "the card is a place: {all}");
    let (y, _) = a.file_rows[0].clone();
    assert_eq!(a.key(Key::Click { x: 10, y: y as u16 }), None);
    assert!(a.diff_popup.is_some(), "a click opens the popup");
}

/// A decision arriving while the editor has the keyboard takes it back, and the bar
/// describes the card - `1` or Enter meant for the card must not edit the file.
#[test]
fn a_decision_arriving_takes_the_keyboard_back_from_the_editor() {
    let ws = workspace("decision");
    let mut a = edited(&ws);
    a.screen(100, 40);
    a.key(Key::CtrlBracket);
    settle(&mut a);
    assert!(a.editor_focused());
    a.apply(ServerFrame::Event(env(9, testing::requested("r1", "rm"))));
    assert!(!a.editor_focused(), "the keyboard is back on the composer");
    assert!(a.editor_drawn(), "the file stays on the screen");
    let bar = a.hint_bar(200);
    assert!(
        bar.contains("a row number marks"),
        "the bar is the card's: {bar}"
    );
    let keys = route_text(&mut a, "2");
    for k in keys {
        a.key(k);
    }
    assert_eq!(a.sel, 1, "the digit reached the card");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Answer {
            req_id: "r1".into(),
            option_id: "deny".into(),
            pattern: None,
            note: None
        })
    );
}

/// A sudo secret asked while the editor has the keyboard takes it back: the password goes to the
/// masked card, never into the open file.
#[test]
fn a_secret_ask_takes_the_keyboard_back_from_the_editor() {
    let ws = workspace("secret");
    let mut a = edited(&ws);
    a.screen(100, 40);
    a.key(Key::CtrlBracket);
    settle(&mut a);
    assert!(a.editor_focused());
    a.apply(ServerFrame::Event(env(
        9,
        SessionEvent::SecretRequested {
            req_id: "secret-s-9".into(),
            prompt: "[sudo] password for dead:".into(),
            command: "sudo true".into(),
            deadline: 1_000_000,
        },
    )));
    a.screen(100, 40);
    assert!(!a.editor_focused(), "the keyboard left the editor");
    for k in route_text(&mut a, "hunter2") {
        a.key(k);
    }
    assert!(
        !a.edit_pane.as_ref().unwrap().ed.send_event().modified,
        "the password was typed into the buffer"
    );
    let buffer = a.screen(100, 40).join("\n");
    assert!(
        !buffer.contains("hunter2"),
        "the password is on the screen:\n{buffer}"
    );
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Secret {
            req_id: "secret-s-9".into(),
            secret: Some("hunter2".into()),
        })
    );
}

/// The pointer's light belongs to the screen it was taken on. The popup is drawn over the rows the
/// card lit and is not scrolled to the pointer, so a light left on them marks nothing in particular.
#[test]
fn a_click_that_opens_the_popup_leaves_no_light_on_it() {
    let ws = workspace("stale-hover");
    let mut a = edited(&ws);
    a.screen(100, 40);
    let (y, _) = a.file_rows[0].clone();
    let top = a
        .hits
        .iter()
        .find(|h| h.target == crate::app::Target::Conversation)
        .unwrap()
        .rect
        .y;
    let y = (top + y) as u16;
    a.key(Key::Hover { x: 10, y });
    assert!(
        a.screen(100, 40)[y as usize].contains("\x1b[22m"),
        "the premise: the card is lit"
    );
    assert_eq!(a.key(Key::Click { x: 10, y }), None);
    assert!(a.diff_popup.is_some());
    let popup = a.screen(100, 40);
    for (i, l) in popup.iter().enumerate() {
        assert!(
            !l.contains("\x1b[22m"),
            "row {i} of the popup is lit: {l:?}"
        );
    }
}

/// Opens `name` in the pane at `w` x 40 and returns the app, ready for a frame.
fn open_file_in_pane(ws: &Workspace, name: &str, w: usize) -> App {
    let mut a = app();
    a.session_id = "s".into();
    a.screen(w, 40);
    a.key(Key::CtrlE);
    route_text(&mut a, &ws.0.join(name).display().to_string());
    a.route(vec![key(KeyCode::Enter, Mods::NONE)]);
    settle(&mut a);
    a.screen(w, 40);
    a
}

/// A markdown preview in a wide pane keeps rano's centred 100-column prose.
#[test]
fn a_markdown_preview_is_centred_in_a_wide_pane() {
    let ws = workspace("centre");
    let words = "alpha beta gamma delta ".repeat(30);
    std::fs::write(ws.0.join("doc.md"), format!("# Title\n\n{words}\n")).unwrap();
    let mut a = open_file_in_pane(&ws, "doc.md", 220);
    a.route(vec![Event::Key(KeyEvent::new(
        KeyCode::Char('p'),
        Mods::ALT,
    ))]);
    settle(&mut a);
    let screen = a.screen(220, 40);
    let line = screen
        .iter()
        .find(|l| l.contains("alpha beta"))
        .unwrap_or_else(|| panic!("the prose is drawn: {screen:#?}"));
    // A terminal this wide shares its columns with the conversation, so rano's centring is
    // measured inside the editor's own - which is what the frame must not take away from it.
    let cols = if a.editor_col > 0 { a.editor_col } else { 220 };
    let mine: String = crate::app::keyhints::cells(line)
        .into_iter()
        .take(cols)
        .collect();
    let lead = mine.chars().take_while(|c| *c == ' ').count();
    assert!(
        lead >= 4,
        "prose is centred, not at the left: {lead} of {cols} {mine:?}"
    );
}

const PNG_1X1: &str = "89504e470d0a1a0a0000000d4948445200000001000000010806000000\
1f15c4890000000d49444154789c63000100000500010d0a2db40000000049454e44ae426082";

/// A workspace whose `doc.md` shows `pic.png`, previewed in a 200-column pane.
fn previewed(proto: rano::term::Proto) -> (Workspace, App) {
    let ws = workspace("pics");
    let png: Vec<u8> = (0..PNG_1X1.len() / 2)
        .map(|i| u8::from_str_radix(&PNG_1X1[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    std::fs::write(ws.0.join("pic.png"), png).unwrap();
    std::fs::write(ws.0.join("doc.md"), "# T\n\n![pic](pic.png)\n\nafter\n").unwrap();
    let mut a = app();
    a.set_proto(proto);
    a.session_id = "s".into();
    a.screen(200, 40);
    a.key(Key::CtrlE);
    route_text(&mut a, &ws.0.join("doc.md").display().to_string());
    a.route(vec![key(KeyCode::Enter, Mods::NONE)]);
    settle(&mut a);
    a.route(vec![Event::Key(KeyEvent::new(
        KeyCode::Char('p'),
        Mods::ALT,
    ))]);
    settle(&mut a);
    a.screen(200, 40);
    (ws, a)
}

fn text_of(bytes: &[Vec<u8>]) -> String {
    bytes
        .iter()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .collect()
}

/// A preview's pictures reach the head's uploads, as virtual placements: no cursor motion, so
/// the pane's offset in the screen changes nothing.
#[test]
fn a_preview_hands_its_pictures_to_the_terminal_writes() {
    let (_ws, mut a) = previewed(rano::term::Proto::Kitty);
    let sent = text_of(&a.take_image_uploads());
    assert!(sent.contains("a=t,f=100"), "uploaded: {sent:?}");
    assert!(sent.contains("a=p,U=1"), "placed virtually: {sent:?}");
    assert!(!sent.contains("\x1b["), "no absolute position: {sent:?}");
    let screen = a.screen(200, 40).join("\n");
    assert!(screen.contains('\u{10EEEE}'), "placeholders in the rows");
}

/// Closing the pane frees what it uploaded.
#[test]
fn closing_the_pane_deletes_its_pictures() {
    let (_ws, mut a) = previewed(rano::term::Proto::Kitty);
    let _ = a.take_image_uploads();
    a.close_editor();
    let sent = text_of(&a.take_image_uploads());
    assert!(sent.contains("a=d,d=I"), "deleted: {sent:?}");
}

/// A terminal with no picture protocol is sent nothing.
#[test]
fn a_terminal_without_pictures_gets_no_graphics() {
    let (_ws, mut a) = previewed(rano::term::Proto::None);
    a.close_editor();
    assert!(a.take_image_uploads().is_empty());
}

/// A pane put away with `ctrl-e` holding unsaved edits: the quit card names the files, and the
/// first Enter only asks - the edits are not dropped by a card the operator never read.
#[test]
fn the_quit_card_names_a_dirty_hidden_pane_and_waits_for_the_second_enter() {
    let ws = workspace("quitdirty");
    let mut a = open_file_in_pane(&ws, FILE, 100);
    a.wiring.workspace = ws.0.display().to_string();
    assert!(route_text(&mut a, "zz").is_empty(), "rano took the keys");
    assert!(
        a.edit_pane.as_ref().unwrap().ed.send_event().modified,
        "the buffer is dirty"
    );
    // Away to the composer, and the pane put away: the edits are on a screen nobody is looking at.
    a.route(vec![ctrl(']')]);
    a.key(Key::CtrlE);
    assert!(!a.editor_drawn(), "the pane is put away, not closed");
    assert_eq!(a.modified_buffers(), vec![FILE.to_string()]);

    a.key(Key::CtrlC);
    a.key(Key::CtrlC);
    assert!(a.quit_card, "the second ctrl-c asks");
    let card = a.quit_card_lines(120).join(" ");
    assert!(card.contains("unsaved edits to notes.txt"), "{card}");
    assert!(card.contains("press e to show"), "{card}");
    assert!(
        a.screen(100, 30)
            .join("\n")
            .contains("unsaved edits to notes.txt"),
        "the card is on the screen"
    );

    assert_eq!(a.key(Key::Enter), None, "the first enter only asks");
    assert!(!a.quit, "no quit action while the edits are unconfirmed");
    assert!(a.quit_card, "and the card stays up");
    assert!(
        a.notice
            .as_deref()
            .is_some_and(|n| n.contains("enter again leaves without saving")),
        "{:?}",
        a.notice
    );
    // The second enter is the confirmation, and it takes the row the mark is on.
    assert_eq!(a.key(Key::Enter), Some(Action::Quit));
    assert!(a.quit && !a.quit_card);
}

/// The card's `e` shows what it counts: the pane comes back on the file with the unsaved edits,
/// still modified, so leaving is a decision about a file the operator has seen.
#[test]
fn the_quit_card_shows_the_dirty_buffers_before_anything_is_left() {
    let ws = workspace("quitshow");
    let mut a = open_file_in_pane(&ws, FILE, 100);
    a.wiring.workspace = ws.0.display().to_string();
    route_text(&mut a, "zz");
    a.route(vec![ctrl(']')]);
    a.key(Key::CtrlE);
    a.key(Key::CtrlC);
    a.key(Key::CtrlC);
    assert!(a.quit_card);

    assert_eq!(a.key(Key::Char('e')), None);
    assert!(!a.quit_card && !a.quit, "showing them is not leaving");
    assert!(a.editor_drawn() && a.editor_focused(), "the pane is back");
    assert!(is_the_file(&place(&a).0), "on the file that would be lost");
    assert!(
        a.edit_pane.as_ref().unwrap().ed.send_event().modified,
        "the edits are still there"
    );
    assert!(
        a.notice.as_deref().is_some_and(|n| n.contains("notes.txt")),
        "{:?}",
        a.notice
    );
}

/// With nothing modified the card is what it was: no file named, and one Enter leaves.
#[test]
fn a_clean_quit_card_names_no_file_and_leaves_on_the_first_enter() {
    let ws = workspace("quitclean");
    let mut a = open_file_in_pane(&ws, FILE, 100);
    a.wiring.workspace = ws.0.display().to_string();
    a.route(vec![ctrl(']')]);
    a.key(Key::CtrlE);
    a.key(Key::CtrlC);
    a.key(Key::CtrlC);
    assert!(a.quit_card);
    assert!(a.modified_buffers().is_empty());
    let card = a.quit_card_lines(120).join(" ");
    assert!(!card.contains("unsaved edits"), "{card}");
    assert!(!card.contains(FILE), "{card}");
    assert_eq!(a.key(Key::Enter), Some(Action::Quit));
}
