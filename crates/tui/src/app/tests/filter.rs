//! A list pane's `/` filter: the line narrows the rows as it is typed, the filter's own row says how
//! many it keeps - or why a `re:` is not a pattern - and Esc gives the whole list back without
//! closing the pane. The words are `app::query`'s, so a `"phrase"` and a `re:` mean here what they
//! mean in the session list.

use super::*;
use letibot_sessionlog::protocol::{JobEntry, NoteEntry, NoteForm};

/// A head in `s1` with the standing pane open over three notes on disk.
fn standing_head(name: &str) -> (App, std::path::PathBuf) {
    let d = letibot_sessionlog::testing::scratch_root()
        .join(format!("lb-filter-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let note = |file: &str, abstract_: &str| NoteEntry {
        path: d.join(file).display().to_string(),
        abstract_line: Some(abstract_.to_string()),
        abstract_written: true,
        form: NoteForm::Verbatim,
    };
    for file in ["alpha.md", "beta.md", "gamma.md"] {
        std::fs::write(d.join(file), format!("the {file} body\n")).unwrap();
    }
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(hello(
        "s1",
        vec![brief("s1", "one", false)],
        Hub::new("s1").snapshot(),
    ));
    let _ = a.command("standing");
    a.apply(ServerFrame::StandingNotes {
        session_id: "s1".into(),
        notes: vec![
            note("alpha.md", "the first note"),
            note("beta.md", "the second note"),
            note("gamma.md", "the third note"),
        ],
        dir: Some(d.display().to_string()),
    });
    (a, d)
}

/// The notes a frame names, which is what the pane drew.
fn notes_on_screen(a: &mut App) -> Vec<&'static str> {
    let screen = a.screen(120, 24).join("\n");
    ["alpha.md", "beta.md", "gamma.md"]
        .into_iter()
        .filter(|f| screen.contains(f))
        .collect()
}

/// A head in `s1` with the jobs pane open over three jobs.
fn jobs_head() -> App {
    let job = |id: &str, command: &str, state: &str, running: bool| JobEntry {
        id: id.into(),
        command: command.into(),
        slug: String::new(),
        how: "asked".into(),
        state: state.into(),
        running,
        never_ran: false,
        redirect: None,
        produced: 0,
        cgroup: String::new(),
        elapsed_ms: 10,
    };
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(hello(
        "s1",
        vec![brief("s1", "one", false)],
        Hub::new("s1").snapshot(),
    ));
    let jobs = vec![
        job("j1", "cargo test --workspace", "running", true),
        job("j2", "cargo build --release", "running", true),
        job("j3", "rg TODO src", "running", true),
    ];
    a.apply(jobs_frame("s1", jobs));
    a.key(Key::CtrlQ);
    a
}

fn jobs_on_screen(a: &mut App) -> Vec<&'static str> {
    let screen = a.screen(120, 24).join("\n");
    [
        "cargo test --workspace",
        "cargo build --release",
        "rg TODO src",
    ]
    .into_iter()
    .filter(|c| screen.contains(c))
    .collect()
}

/// `/` on a list is the filter: what is typed next narrows the rows rather than being a message, the
/// filter's own row says how many are left, and Esc is the filter's before it is the pane's.
#[test]
fn a_filter_narrows_the_standing_notes_as_it_is_typed_and_esc_gives_them_back() {
    let (mut a, d) = standing_head("standing");
    assert_eq!(
        notes_on_screen(&mut a),
        vec!["alpha.md", "beta.md", "gamma.md"],
        "the premise: every note is on the pane"
    );

    assert_eq!(a.key(Key::Char('/')), None, "`/` sends nothing");
    assert!(a.filtering(), "`/` did not start a filter");
    assert_eq!(a.input(), "", "the filter line starts empty");

    typed(&mut a, "bet");
    assert_eq!(
        notes_on_screen(&mut a),
        vec!["beta.md"],
        "the list did not narrow"
    );
    let screen = a.screen(120, 24).join("\n");
    assert!(
        screen.contains("1 row"),
        "the filter's own row counts what it keeps: {screen}"
    );

    // Esc is the filter's: the rows come back whole and the pane stays open.
    assert_eq!(a.key(Key::Esc), None);
    assert!(!a.filtering());
    assert!(
        a.is_open(Surface::Standing),
        "esc took the pane down with the filter"
    );
    assert_eq!(
        notes_on_screen(&mut a),
        vec!["alpha.md", "beta.md", "gamma.md"],
        "the whole list did not come back"
    );
    // And the second Esc is the pane's, as it always was.
    a.key(Key::Esc);
    assert!(!a.is_open(Surface::Standing));
    let _ = std::fs::remove_dir_all(&d);
}

/// The second pane, and the same query: `"a phrase"` narrows by the words as typed and takes the
/// letters-in-order fallback away, which is the whole point of quoting one.
#[test]
fn a_quoted_phrase_narrows_the_jobs_pane_and_the_letters_in_order_do_not_answer_it() {
    let mut a = jobs_head();
    assert_eq!(jobs_on_screen(&mut a).len(), 3, "the premise: three jobs");

    a.key(Key::Char('/'));
    // Unquoted, `ctw` reaches `cargo test --workspace` only by its letters in order.
    typed(&mut a, "ctw");
    assert_eq!(
        jobs_on_screen(&mut a),
        vec!["cargo test --workspace"],
        "the ladder's fallback did not reach it"
    );
    // Quoted, the same letters are a phrase, and no job contains them.
    a.key(Key::Esc);
    a.key(Key::Char('/'));
    typed(&mut a, "\"ctw\"");
    assert_eq!(
        jobs_on_screen(&mut a),
        Vec::<&str>::new(),
        "a phrase matched the letters in order"
    );
    // And a phrase that IS in a row narrows to it.
    a.key(Key::Esc);
    a.key(Key::Char('/'));
    typed(&mut a, "\"cargo b\"");
    assert_eq!(jobs_on_screen(&mut a), vec!["cargo build --release"]);
}

/// A `re:` is a pattern, and one that does not compile matches nothing and says why in the filter's
/// own row - a head that panicked, or that quietly matched everything, would be two worse answers.
#[test]
fn a_bad_regex_says_why_in_the_filter_line_and_matches_nothing() {
    let mut a = jobs_head();
    a.key(Key::Char('/'));
    typed(&mut a, "re:^cargo (test|build)");
    assert_eq!(
        jobs_on_screen(&mut a),
        vec!["cargo test --workspace", "cargo build --release"],
        "the pattern did not narrow by the expression"
    );

    // A pattern that does not compile: nothing on the list, and the reason is in the filter row
    // rather than in a panic (the frame below is drawn by the head that took the line).
    a.key(Key::Esc);
    a.key(Key::Char('/'));
    typed(&mut a, "re:cargo (");
    assert_eq!(
        jobs_on_screen(&mut a),
        Vec::<&str>::new(),
        "a bad pattern matched"
    );
    let screen = a.screen(140, 24).join("\n");
    assert!(
        screen.contains("unclosed group") || screen.contains("regex"),
        "the filter row does not say why nothing matched: {screen}"
    );
}

/// Enter over a filtered list is the ROW's, not the composer's: what is typed is a question about
/// the list, and a filter must never be sent as a message.
#[test]
fn enter_over_a_filter_takes_the_row_and_does_not_send_the_line() {
    let (mut a, d) = standing_head("enter");
    a.key(Key::Char('/'));
    typed(&mut a, "beta");
    // Enter opens the note under the cursor and sends nothing: the standing pane's Enter is a file.
    assert_eq!(a.key(Key::Enter), None);
    let opened = a
        .edit_pane
        .as_ref()
        .expect("the pane is open")
        .ed
        .send_event()
        .path
        .map(|p| p.display().to_string());
    assert_eq!(
        opened,
        Some(d.join("beta.md").display().to_string()),
        "enter took the wrong row, or none"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// A pane that takes no filter keeps the letter: only the panes whose rows are narrowed bind `/`,
/// and a pane with one says so on the bar.
#[test]
fn only_a_pane_that_narrows_its_rows_binds_the_filter_key() {
    use crate::app::keys::bindings::{Act, On};
    for s in Surface::ALL.into_iter().filter(|s| s.is_list()) {
        let binds = s
            .bindings()
            .iter()
            .any(|b| b.on.contains(&On::Char('/')) && b.act == Act::Filter);
        if !binds {
            continue;
        }
        assert!(
            a_head_with(s).surface_hint(s).contains("/ filters"),
            "{s:?} binds `/` and its bar does not say so"
        );
    }
    // The panes this change narrows are the ones that bind it; a pane whose Enter is a typed line
    // (the session list) keeps `/` for its commands.
    for s in [
        Surface::Standing,
        Surface::Skills,
        Surface::Jobs,
        Surface::Monitors,
        Surface::Config,
    ] {
        assert!(
            s.bindings()
                .iter()
                .any(|b| b.on.contains(&On::Char('/')) && b.act == Act::Filter),
            "{s:?} does not bind the filter key"
        );
    }
    assert!(
        !Surface::Picker
            .bindings()
            .iter()
            .any(|b| b.on.contains(&On::Char('/'))),
        "the session list's typed line is already its filter and `/` there is `/new`"
    );
}

fn a_head_with(s: Surface) -> App {
    let mut a = app();
    a.session_id = "s1".into();
    a.push_surface(s);
    a
}
