//! **The standing-notes pane** — one row per note the harness reads into the prompt, with the
//! form the budget gave each, and Enter to open one in the editor pane.
//!
//! The rows come from the daemon and the disk facts are read here at draw time, so the
//! fixtures are two halves and this file asserts both: a `StandingNotes` frame built by hand
//! (what the daemon decides) and a real directory of real files (what the disk answers).

use super::*;
use letibot_sessionlog::protocol::{NoteEntry, NoteForm};
use rano::term::{Event, KeyCode, KeyEvent, Mods};
use std::time::Instant;

/// A directory to put notes in, emptied first. Its own name per test, so two of these cannot
/// collide on a parallel run.
fn corpus(name: &str) -> std::path::PathBuf {
    let d = letibot_sessionlog::testing::scratch_root()
        .join(format!("lb-standing-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// One note, as the daemon's `ListNotes` carries it.
fn note(
    path: &std::path::Path,
    abstract_: Option<&str>,
    written: bool,
    form: NoteForm,
) -> NoteEntry {
    NoteEntry {
        path: path.display().to_string(),
        abstract_line: abstract_.map(str::to_string),
        abstract_written: written,
        form,
    }
}

fn frame(notes: Vec<NoteEntry>) -> ServerFrame {
    ServerFrame::StandingNotes {
        session_id: "s1".into(),
        notes,
    }
}

/// rano's ticks until the file it opened has arrived — the loop's job, done here by hand.
///
/// The editor pane reads a file **asynchronously** (`App::editor_tick` is where the rows land),
/// so a test that looked at the pane straight after the key would be asking it about a buffer
/// that is still coming.
fn settle(a: &mut App) {
    for _ in 0..1000 {
        a.editor_tick(Instant::now());
        if !a.edit_pane.as_ref().is_some_and(|p| p.ed.loading()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    a.editor_tick(Instant::now());
}

/// Where rano is: the file it is holding and the 1-based line — read out of the pane's own
/// answer, never out of the row a test handed in, so a pane that opened the wrong note fails.
fn place(a: &App) -> (String, usize) {
    let e = a
        .edit_pane
        .as_ref()
        .expect("the pane is open")
        .ed
        .send_event();
    (
        e.path.expect("a named file").display().to_string(),
        e.cursor.line,
    )
}

/// The screen row that names `needle`, which is where a mark on a note's row would be.
fn row_naming<'a>(screen: &'a str, needle: &str) -> &'a str {
    screen
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no row names {needle:?}: {screen}"))
}

/// **`/standing` asks the daemon and draws what comes back** — the index's abstract, the
/// file's own size and age, and the form the budget gave it.
///
/// The size is asserted as the number the file really is (`bytes_human` of its own length),
/// not as a string copied from the renderer: a row that showed a remembered size would pass a
/// copied string and fail this.
#[test]
fn the_pane_draws_the_indexes_abstract_and_the_files_own_size_and_age() {
    let d = corpus("rows");
    let small = d.join("small.md");
    std::fs::write(&small, "the small note's prose\n").unwrap();
    let big = d.join("big.md");
    std::fs::write(&big, "# Big\n\nits body\n").unwrap();
    let size = rano::agent::text::bytes_human(std::fs::metadata(&small).unwrap().len());

    let mut a = app();
    a.session_id = "s1".into();
    assert_eq!(a.command("standing"), Some(Action::ListNotes));
    assert!(a.standing_pane);
    // Nothing has arrived: the pane says so rather than drawing a corpus nobody sent.
    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("standing notes"), "{screen}");
    assert!(screen.contains("none."), "{screen}");

    a.apply(frame(vec![
        note(
            &small,
            Some("the author's own line"),
            true,
            NoteForm::Verbatim,
        ),
        note(&big, Some("its body"), false, NoteForm::Indexed),
    ]));
    let screen = a.screen(120, 24).join("\n");
    assert!(screen.contains("small.md"), "{screen}");
    assert!(screen.contains("the author's own line"), "{screen}");
    assert!(screen.contains(&size), "the file's own size: {screen}");
    assert!(
        screen.contains("under a minute ago"),
        "the file's own mtime, as an age: {screen}"
    );
    // **The two forms, and the legend that says what the second one means.**
    assert!(screen.contains("verbatim"), "{screen}");
    assert!(screen.contains("indexed"), "{screen}");
    assert!(
        screen.contains("1 of 2 indexed"),
        "the budget's effect is the field this pane exists for: {screen}"
    );
    // **A derived abstract says so, and an author's own line does not** — once each, so the
    // marker cannot be decorating both halves.
    assert_eq!(
        screen.matches("(derived)").count(),
        1,
        "exactly the derived one is marked: {screen}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// **A note whose file has gone says that, rather than a row that looks fine.**
///
/// The index still names it — the prompt is carrying its entry — and the disk does not, and
/// the row is the only place the two can be seen disagreeing.
#[test]
fn a_note_whose_file_is_gone_says_so() {
    let d = corpus("gone");
    let here = d.join("here.md");
    std::fs::write(&here, "still here\n").unwrap();
    let gone = d.join("gone.md");
    std::fs::write(&gone, "about to go\n").unwrap();
    std::fs::remove_file(&gone).unwrap();

    let mut a = app();
    a.session_id = "s1".into();
    a.command("standing");
    a.apply(frame(vec![
        note(&here, Some("still here"), true, NoteForm::Verbatim),
        note(&gone, Some("about to go"), true, NoteForm::Indexed),
    ]));
    let screen = a.screen(120, 24).join("\n");
    assert!(screen.contains("here.md"), "{screen}");
    assert!(screen.contains("gone.md"), "{screen}");
    assert!(
        screen.contains("gone — the file is not there"),
        "the row says what happened: {screen}"
    );
    // And Enter on it says so too, rather than opening an empty pane.
    a.key(Key::Down);
    a.key(Key::Enter);
    assert!(a.edit_pane.is_none(), "nothing was opened");
    assert!(!a.editor_drawn());
    let notice = a.notice.clone().unwrap_or_default();
    assert!(
        notice.contains("gone.md") && notice.contains("cannot open"),
        "the key says why, and names the file: {notice:?}"
    );
    // **And the list stays where it was**, on the row that said it: a key that failed has not
    // earned taking the reader's screen away.
    let screen = a.screen(120, 24).join("\n");
    assert!(screen.contains("gone — the file is not there"), "{screen}");
    assert!(screen.contains("standing notes"), "{screen}");
    let _ = std::fs::remove_dir_all(&d);
}

/// **Enter opens the selected note's FILE in the editor pane** — rano, the same pane a click on
/// an edit row and `ctrl-]` open — and it opens the row the cursor is on, not its neighbour.
///
/// The path is read back from rano rather than compared with the string this test passed in, and
/// the line is the note's own first: a note is opened to be read from the top, because there is
/// no change to place a cursor on (see `FileRef::file`).
#[test]
fn enter_opens_the_selected_note_in_the_editor_pane() {
    let d = corpus("open");
    let first = d.join("first.md");
    std::fs::write(&first, "# First\n\nthe first note\n").unwrap();
    let second = d.join("second.md");
    std::fs::write(&second, "# Second\n\nthe second note\n").unwrap();

    let mut a = app();
    a.session_id = "s1".into();
    a.wiring.workspace = d.display().to_string();
    a.command("standing");
    a.apply(frame(vec![
        note(&first, Some("the first"), true, NoteForm::Verbatim),
        note(&second, Some("the second"), true, NoteForm::Indexed),
    ]));
    // A frame first, so the pane has a rectangle to be opened into.
    a.screen(120, 24);
    a.key(Key::Down);
    assert_eq!(a.key(Key::Enter), None);
    assert!(a.editor_focused(), "enter hands the keyboard to the editor");
    settle(&mut a);

    let (path, line) = place(&a);
    assert!(
        path.ends_with("second.md"),
        "the row under the cursor, not the one above it: {path}"
    );
    assert_eq!(line, 1, "a note is read from its first line");
    // The whole file is what is on the screen — for an `indexed` note this is the point: the
    // prompt carries its headings and line ranges, and the text is only here.
    let screen = a.screen(120, 24).join("\n");
    assert!(screen.contains("the second note"), "{screen}");
    assert!(
        !screen.contains("the first note"),
        "the neighbour's text is on the screen: {screen}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// **Leaving rano lands back on the list**, with the cursor where it was — the list stays open
/// under the pane, so a note is not a one-way trip out of the pane it was opened from.
///
/// The exit goes through the router as `ctrl-q` does, because that is rano's own key while it
/// has the keyboard: from the composer the same byte is this head's jobs pane.
#[test]
fn leaving_rano_lands_back_on_the_notes_list() {
    let d = corpus("leave");
    let first = d.join("first.md");
    std::fs::write(&first, "the first note\n").unwrap();
    let second = d.join("second.md");
    std::fs::write(&second, "the second note\n").unwrap();

    let mut a = app();
    a.session_id = "s1".into();
    a.wiring.workspace = d.display().to_string();
    a.command("standing");
    a.apply(frame(vec![
        note(&first, Some("the first"), true, NoteForm::Verbatim),
        note(&second, Some("the second"), true, NoteForm::Verbatim),
    ]));
    a.screen(120, 24);
    assert!(
        a.hint_bar(160)
            .contains("enter opens the note in the editor"),
        "the bar names where the key goes: {:?}",
        a.hint_bar(160)
    );
    a.key(Key::Down);
    a.key(Key::Enter);
    settle(&mut a);
    assert!(a.editor_drawn(), "the note took the screen");
    assert!(
        !a.screen(120, 24).join("\n").contains("standing notes"),
        "and the list is under it, not beside it"
    );
    // **And the list's keys are off while rano is over it**: a bar naming keys that do nothing
    // would be advertising a pane the reader cannot see.
    assert!(
        !a.hint_bar(160).contains("↑↓ moves"),
        "the list's keys are not advertised under the editor: {:?}",
        a.hint_bar(160)
    );

    a.route(vec![Event::Key(KeyEvent::new(
        KeyCode::Char('q'),
        Mods::CTRL,
    ))]);
    a.editor_tick(Instant::now());
    assert!(a.edit_pane.is_none(), "rano's exit closed the pane");
    assert!(a.standing_pane, "and the list is what was under it");
    let screen = a.screen(120, 24).join("\n");
    assert!(screen.contains("standing notes"), "{screen}");
    assert_eq!(a.standing_sel, 1, "the cursor is where the reader left it");
    assert!(
        row_naming(&screen, "second.md").contains('▸'),
        "and the mark is on that row: {screen}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// **The arrows move the selection and touch nothing else** — the pane owns Up and Down while
/// it is open (the rule every list pane keeps), and the composer's half-typed line is not
/// theirs. The mark the frame draws is asserted with the state, so a key that moved a cursor
/// nobody could see would fail here.
#[test]
fn the_arrows_move_the_selection_without_touching_the_composer_draft() {
    let d = corpus("keys");
    let first = d.join("a.md");
    std::fs::write(&first, "a note\n").unwrap();
    let second = d.join("b.md");
    std::fs::write(&second, "another note\n").unwrap();

    let mut a = app();
    a.session_id = "s1".into();
    a.command("standing");
    a.apply(frame(vec![
        note(&first, Some("a note"), true, NoteForm::Verbatim),
        note(&second, Some("another note"), true, NoteForm::Verbatim),
    ]));
    typed(&mut a, "half a thought");
    assert_eq!(a.standing_sel, 0);

    assert_eq!(a.key(Key::Down), None);
    assert_eq!(a.standing_sel, 1, "down moved the selection");
    assert_eq!(a.editor.text(), "half a thought", "and not the draft");
    let screen = a.screen(120, 24).join("\n");
    assert!(
        row_naming(&screen, "b.md").contains('▸'),
        "the mark follows the cursor: {screen}"
    );
    assert!(
        !row_naming(&screen, "a.md").contains('▸'),
        "and only one row is marked: {screen}"
    );

    assert_eq!(a.key(Key::Up), None);
    assert_eq!(a.standing_sel, 0, "and up comes back");
    assert_eq!(a.editor.text(), "half a thought");
    let _ = std::fs::remove_dir_all(&d);
}

/// **The rows are what `ListNotes` said, not a listing of the notes directory.**
///
/// The paths, the abstracts and the forms are the daemon's — decided where the section is
/// assembled, with the session's token counter — so a `.md` file sitting beside the ones the
/// daemon sent is not a row, and this head cannot invent one by reading the directory its own
/// rows happen to come from.
#[test]
fn the_rows_are_what_list_notes_said() {
    let d = corpus("from-daemon");
    let sent = d.join("sent.md");
    std::fs::write(&sent, "the sent one\n").unwrap();
    let unsent = d.join("unsent.md");
    std::fs::write(&unsent, "the daemon never sent this\n").unwrap();

    let mut a = app();
    a.session_id = "s1".into();
    a.wiring.workspace = d.display().to_string();
    a.command("standing");
    let entries = vec![note(&sent, Some("the sent one"), true, NoteForm::Verbatim)];
    a.apply(frame(entries.clone()));
    assert_eq!(a.standing, entries, "the daemon's rows, unchanged");

    let screen = a.screen(120, 24).join("\n");
    assert!(screen.contains("1 note(s)"), "{screen}");
    assert!(screen.contains("sent.md"), "{screen}");
    assert!(screen.contains("the sent one"), "{screen}");
    assert!(
        !screen.contains("unsent.md"),
        "a file the daemon did not name is not a row: {screen}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// **An empty corpus says so rather than drawing an empty pane** — and Enter in it does
/// nothing rather than falling through to the composer.
#[test]
fn an_empty_corpus_says_so() {
    let mut a = app();
    a.session_id = "s1".into();
    a.command("standing");
    a.apply(frame(Vec::new()));
    let screen = a.screen(120, 24).join("\n");
    assert!(screen.contains("standing notes — 0 note(s)"), "{screen}");
    assert!(screen.contains("none."), "{screen}");
    assert!(!screen.contains("▸"), "{screen}");
    assert_eq!(
        a.key(Key::Enter),
        None,
        "the pane owns enter even when empty"
    );
}

/// **The form is as of NOW, not as of when the pane opened** — the field the whole pane
/// exists for, and the reason it is not simply drawn from what `ListNotes` last said.
///
/// The form is the daemon's answer (decided with the session's token counter, which this head
/// does not have) while the size beside it is read here at draw time — so a note edited with
/// the pane open would otherwise draw a NEW size next to the form it had BEFORE the edit, and
/// the row would be contradicting itself about the one thing it is for. The head cannot decide
/// the form; what it can do is notice the corpus moved and ask again, which is what this pins.
#[test]
fn a_note_edited_while_the_pane_is_open_makes_the_pane_ask_again() {
    let d = corpus("watch");
    let p = d.join("a-note.md");
    std::fs::write(&p, "short to begin with\n").unwrap();

    let mut a = app();
    a.session_id = "s1".into();
    // The workspace, so the pane watches the directory a note would be written in even when
    // it has no row for it.
    a.wiring.workspace = d.display().to_string();
    a.command("standing");
    a.apply(frame(vec![note(
        &p,
        Some("short to begin with"),
        false,
        NoteForm::Verbatim,
    )]));
    a.take_actions();
    // The first draw is the BASELINE and asks for nothing: the rows landed a moment ago.
    a.screen(120, 24);
    assert!(
        !a.take_actions().contains(&Action::ListNotes),
        "the draw that seeds the watch is not a change"
    );
    // And a second draw with nothing touched is still not a change.
    a.screen(120, 24);
    assert!(!a.take_actions().contains(&Action::ListNotes));

    // **The note grows, with the pane open.** The size on the row moves at once — it is read
    // here — so the form must be asked for again rather than left saying `verbatim` beside a
    // size that no longer fits.
    std::fs::write(&p, "x".repeat(4096)).unwrap();
    a.screen(120, 24);
    assert!(
        a.take_actions().contains(&Action::ListNotes),
        "the corpus moved, so the pane asks the daemon again"
    );
    // Once per change and not once per frame: the next draw sees the same corpus.
    a.screen(120, 24);
    assert!(!a.take_actions().contains(&Action::ListNotes));

    // **And a note that did not exist before is a change too**, in the workspace's own notes
    // directory — watched whether or not it has anything in it, so an empty pane does not sit
    // there saying there are none while one is being written.
    let mut b = app();
    b.session_id = "s1".into();
    b.wiring.workspace = d.display().to_string();
    b.command("standing");
    b.apply(frame(Vec::new()));
    b.take_actions();
    b.screen(120, 24);
    std::fs::create_dir_all(d.join(".letibot/notes")).unwrap();
    std::fs::write(d.join(".letibot/notes/fresh.md"), "brand new\n").unwrap();
    b.screen(120, 24);
    assert!(
        b.take_actions().contains(&Action::ListNotes),
        "a note written into the workspace's notes dir is a change"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// **The path on a note's row is a link the terminal opens** (OSC 8) — the operator's own
/// report, about this pane's fact: *"yeah you gave md name but it is not clickable"*.
///
/// The row names the path the prompt's section carries, and `rano::term::links::file_url` is
/// what turns it into the URL — absolute against the session's workspace, with this machine's
/// name in it. A terminal that does not speak links gets the row it always got, byte for byte:
/// that half is asserted here too, because the gate is the whole reason the feature is safe.
#[test]
fn a_notes_path_on_the_row_is_a_link_the_terminal_can_open() {
    let d = corpus("link");
    let p = d.join("clickable.md");
    std::fs::write(&p, "the note's prose\n").unwrap();

    let mut a = app();
    a.session_id = "s1".into();
    a.wiring.workspace = d.display().to_string();
    a.set_features(rano::term::Features::ALL);
    a.command("standing");
    a.apply(frame(vec![note(
        &p,
        Some("a line"),
        true,
        NoteForm::Verbatim,
    )]));
    let screen = a.screen(140, 24).join("\n");
    assert!(screen.contains("clickable.md"), "{screen}");
    // The URL the row carries, read off the row: everything between the opener and its `ST`.
    let opener = screen
        .find("\u{1b}]8;;")
        .map(|at| &screen[at + "\u{1b}]8;;".len()..])
        .and_then(|rest| rest.split_once("\u{1b}\\"))
        .map(|(url, _)| url.to_string());
    let url = opener.expect("the row's path is not a link");
    assert!(
        url.starts_with("file://") && url.ends_with(&p.display().to_string()),
        "the link is not the file's URL: {url:?}"
    );

    // The same frame on a terminal that does not speak OSC 8 carries no sequence at all.
    let mut b = app();
    b.session_id = "s1".into();
    b.wiring.workspace = d.display().to_string();
    b.command("standing");
    b.apply(frame(vec![note(
        &p,
        Some("a line"),
        true,
        NoteForm::Verbatim,
    )]));
    let plain = b.screen(140, 24).join("\n");
    assert!(plain.contains("clickable.md"), "{plain}");
    assert!(!plain.contains("\u{1b}]8;;"), "{plain:?}");
    let _ = std::fs::remove_dir_all(&d);
}
