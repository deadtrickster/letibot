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
