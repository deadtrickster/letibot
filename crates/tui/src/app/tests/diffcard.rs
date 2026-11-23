//! The permission card's diff: an edit or a write shows what it would change, drawn by the tool
//! row's renderer, folded past `CARD_DIFF_ROWS` with a count, and opened whole by alt-d.

use super::*;
use crate::ui::cards::decision::CARD_DIFF_ROWS;
use letibot_transcript::ToolEditExcerpt;

fn excerpt(path: &str, before: &str, after: &str, start: usize, created: bool) -> ToolEditExcerpt {
    ToolEditExcerpt {
        path: path.into(),
        created,
        before_start: start,
        after_start: start,
        before_lines: before.lines().count(),
        after_lines: after.lines().count(),
        truncated: false,
        before: before.into(),
        after: after.into(),
        note: String::new(),
    }
}

/// A permission card for `tool` on `path`, arrived as the daemon sends it, carrying `edit`.
fn card_with(tool: &str, path: &str, edit: ToolEditExcerpt) -> App {
    use letibot_sessionlog::event::OptionKind;
    let mut a = app();
    let d = decision_with(&[OptionKind::AllowOnce, OptionKind::RejectOnce]);
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::DecisionRequested {
            write_targets: Vec::new(),
            edits: vec![edit],
            req_id: d.req_id.clone(),
            kind: d.kind.clone(),
            call_id: None,
            access: "write".into(),
            summary: format!("`{tool}` wants write access to `{path}`"),
            target: path.into(),
            detail: String::new(),
            options: d.options.clone(),
            options_note: String::new(),
            choices: Vec::new(),
            because: String::new(),
            advice: None,
            subagent: None,
            shadow: None,
            deadline: None,
            on_timeout: d.on_timeout,
        },
    )));
    a
}

#[test]
fn an_edit_card_shows_the_lines_it_removes_and_adds() {
    let mut a = card_with(
        "edit",
        "src/lib.rs",
        excerpt(
            "src/lib.rs",
            "fn a() {\n    old_call();\n}",
            "fn a() {\n    new_call();\n}",
            10,
            false,
        ),
    );
    let screen = a.screen(120, 40).join("\n");
    assert!(screen.contains("old_call();"), "no removed line:\n{screen}");
    assert!(screen.contains("new_call();"), "no added line:\n{screen}");
    assert!(
        screen.contains("11"),
        "the gutter is not the file's numbering:\n{screen}"
    );
    // Evidence above the answer: the diff is in the content half, the ladder under it.
    let diff_at = screen.find("new_call();").unwrap();
    let ladder_at = screen.find("Allow once").expect("the ladder");
    assert!(
        diff_at < ladder_at,
        "the diff is under the ladder:\n{screen}"
    );
    assert!(
        !screen.contains("diff rows"),
        "a small diff was folded:\n{screen}"
    );
}

#[test]
fn a_write_of_a_new_file_shows_its_lines() {
    let body = "alpha\nbeta\ngamma\ndelta\nepsilon";
    let mut a = card_with(
        "write",
        "notes/new.md",
        excerpt("notes/new.md", "", body, 1, true),
    );
    let screen = a.screen(120, 40).join("\n");
    for word in ["alpha", "gamma", "epsilon"] {
        assert!(
            screen.contains(word),
            "`{word}` is not on the card:\n{screen}"
        );
    }
    assert!(
        !screen.contains("diff rows"),
        "five lines were folded:\n{screen}"
    );
}

#[test]
fn a_huge_diff_folds_with_a_count_and_alt_d_opens_it() {
    let body: String = (1..=300)
        .map(|i| format!("row number {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut a = card_with("write", "big.txt", excerpt("big.txt", "", &body, 1, true));

    let folded = a.screen(120, 40).join("\n");
    assert!(folded.contains("row number 1"), "{folded}");
    assert!(
        !folded.contains("row number 300"),
        "nothing was folded:\n{folded}"
    );
    let fold = folded
        .lines()
        .find(|l| l.contains("diff rows · alt-d shows the whole diff"))
        .unwrap_or_else(|| panic!("no fold line:\n{folded}"));
    let hidden: usize = fold
        .split("+")
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no count on `{fold}`"));
    let d = a.open[0].clone();
    a.key(Key::AltD);
    // Every diff row, less the `alt-d folds the diff` line under them.
    let all = a.card_diff_rows(&d, 120).len() - 1;
    a.key(Key::AltD);
    assert_eq!(
        hidden + CARD_DIFF_ROWS,
        all,
        "the count is not the rows left out"
    );
    // The ladder stays on the screen either way.
    assert!(folded.contains("Allow once"), "{folded}");

    // alt-d opens it whole, pgdn pages to its end, and alt-d folds it again.
    a.key(Key::AltD);
    let whole = a.decision_lines(&d, 120).join("\n");
    assert!(
        whole.contains("row number 300"),
        "alt-d did not open it:\n{whole}"
    );
    assert!(whole.contains("alt-d folds the diff"), "{whole}");
    let opened = a.screen(120, 40).join("\n");
    assert!(
        opened.contains("Allow once"),
        "the open diff pushed the ladder off:\n{opened}"
    );
    assert!(
        opened.contains("pgdn scrolls"),
        "no seam over the open diff:\n{opened}"
    );
    for _ in 0..40 {
        a.key(Key::PageDown);
    }
    let end = a.screen(120, 40).join("\n");
    assert!(
        end.contains("row number 300"),
        "pgdn did not reach the end:\n{end}"
    );
    a.key(Key::AltD);
    assert!(
        !a.decision_lines(&d, 120)
            .join("\n")
            .contains("row number 300")
    );
}

#[test]
fn alt_d_with_no_diff_on_screen_says_so() {
    let mut a = app();
    a.key(Key::AltD);
    let said = a.notice.clone().unwrap_or_default();
    assert!(said.contains("no card with one is up"), "{said}");
}

/// An `edit` whose text is not in the file changes nothing, so there is no diff of the file to draw -
/// and the card says why in the tool's own words, over the text the call asked to write. Neither half
/// is optional: a card with no diff and no reason is a card the person cannot answer.
#[test]
fn an_edit_that_would_not_apply_says_why_and_shows_what_it_asked_to_write() {
    let proposed = "fn main() {\n    println!(\"new\");\n}";
    let edit = ToolEditExcerpt {
        note: "`old_string` occurs 3 times in `src/main.rs`".into(),
        before: String::new(),
        after: proposed.into(),
        ..excerpt("src/main.rs", "", proposed, 1, false)
    };
    let mut a = card_with("edit", "src/main.rs", edit);
    let screen = a.screen(120, 40).join("\n");
    let why = screen
        .lines()
        .position(|l| l.contains("`old_string` occurs 3 times in `src/main.rs`"))
        .unwrap_or_else(|| panic!("the card does not say why there is no diff:\n{screen}"));
    assert!(
        screen.contains("nothing would be written"),
        "the card does not say what allowing it does:\n{screen}"
    );
    let shown = screen
        .lines()
        .position(|l| l.contains("println!"))
        .unwrap_or_else(|| panic!("the proposal is not drawn:\n{screen}"));
    assert!(
        why < shown,
        "the reason is under the text it explains:\n{screen}"
    );
    let ladder = screen.find("Allow once").expect("the ladder");
    assert!(
        screen.find("println!").unwrap() < ladder,
        "the proposal is drawn in the answer half:\n{screen}"
    );
}

/// And a call that would not apply and carries nothing to write either: the reason is the whole of
/// what there is, and the card must not promise lines under it.
#[test]
fn an_unresolved_edit_with_no_proposal_promises_no_lines() {
    let edit = ToolEditExcerpt {
        note: "step 2 of 2 found no match".into(),
        before: String::new(),
        after: String::new(),
        ..excerpt("a.md", "", "", 1, false)
    };
    let mut a = card_with("edit", "a.md", edit);
    let screen = a.screen(120, 40).join("\n");
    assert!(screen.contains("step 2 of 2 found no match"), "{screen}");
    assert!(
        screen.contains("so this call would write nothing"),
        "{screen}"
    );
    assert!(
        !screen.contains("below is what this call asked to write"),
        "the card promises lines it does not draw:\n{screen}"
    );
}

/// A `notes` write is a file write like any other, and the card shows it: the note as it is against
/// the note the call would leave. It used to get none, because the file is outside the session's
/// tree and nothing but the `notes` tool knows where it is.
#[test]
fn a_notes_write_shows_the_note_it_would_change() {
    let edit = excerpt(
        "notes/rows-tui.md",
        "# rows-tui\n\nthe card draws nothing for a note",
        "# rows-tui\n\nthe card draws the note's diff",
        1,
        false,
    );
    let mut a = card_with("notes", "notes/rows-tui.md", edit);
    let screen = a.screen(120, 40).join("\n");
    assert!(
        screen.contains("the card draws nothing for a note"),
        "the note as it is is not on the card:\n{screen}"
    );
    assert!(
        screen.contains("the card draws the note's diff"),
        "the note it would leave is not on the card:\n{screen}"
    );
}
