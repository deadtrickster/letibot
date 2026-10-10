//! **The standing-notes pane** — one row per note the harness reads into the prompt, with the
//! form the budget gave each, and Enter to read one.
//!
//! The rows come from the daemon and the disk facts are read here at draw time, so the
//! fixtures are two halves and this file asserts both: a `StandingNotes` frame built by hand
//! (what the daemon decides) and a real directory of real files (what the disk answers).

use super::*;
use letibot_sessionlog::protocol::{NoteEntry, NoteForm};

/// A directory to put notes in, emptied first. Its own name per test, so two of these cannot
/// collide on a parallel run.
fn corpus(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lb-standing-{}-{name}", std::process::id()));
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
    // And Enter on it says so too, rather than opening an empty note.
    a.key(Key::Down);
    a.key(Key::Enter);
    let screen = a.screen(120, 24).join("\n");
    assert!(
        screen.contains("could not be read") && screen.contains("moved or deleted"),
        "{screen}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// **Enter reads the note; Esc goes back to the list; Esc again closes the pane.**
///
/// The overlay's own Esc must not close the pane under it — the jobs pane's rule, and the
/// reason `key_note_open` sits above the block that closes every pane.
#[test]
fn enter_reads_the_note_and_esc_comes_back_to_the_list() {
    let d = corpus("read");
    let p = d.join("a-note.md");
    std::fs::write(&p, "# Title\n\nthe note's own text\n").unwrap();

    let mut a = app();
    a.session_id = "s1".into();
    a.command("standing");
    a.apply(frame(vec![note(
        &p,
        Some("the note's own text"),
        false,
        NoteForm::Verbatim,
    )]));
    assert_eq!(a.key(Key::Enter), None);
    assert!(a.note_open.is_some(), "enter opens the note");
    let screen = a.screen(120, 24).join("\n");
    assert!(screen.contains("standing note — "), "{screen}");
    assert!(screen.contains("# Title"), "{screen}");
    assert!(screen.contains("the note's own text"), "{screen}");

    a.key(Key::Esc);
    assert!(a.note_open.is_none(), "esc leaves the note");
    assert!(
        a.standing_pane,
        "esc goes back to the list, not out of everything"
    );
    let screen = a.screen(120, 24).join("\n");
    assert!(!screen.contains("standing note — "), "{screen}");
    assert!(screen.contains("a-note.md"), "{screen}");

    a.key(Key::Esc);
    assert!(!a.standing_pane, "and the second esc closes the pane");
    assert!(!a.screen(120, 24).join("\n").contains("standing notes"));
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
