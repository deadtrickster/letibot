//! What the todos, jobs, notes and picker panes say to somebody who has never seen them: one realistic fixture each,
//! drawn at eighty and a hundred and twenty columns, with the words a newcomer needs asserted. Held to the
//! pattern of `queue_ux`: plain status words, one row per item, aligned columns, a legend for the verbs.

use super::*;

/// The operator's board with every state on it.
const MANY_TODOS: [(&str, &str, &str); 6] = [
    ("you", "fix the flaky test", "pending"),
    ("model", "read the merge queue's serve loop", "inprogress"),
    ("model", "run the gate on the branch", "completed"),
    ("you", "push once CI lands", "postponed"),
    (
        "model",
        "a row whose title is long enough to reach past the right edge of an eighty column \
         terminal and keep going for a while yet",
        "pending",
    ),
    ("you", "a row that was set aside for good", "cancelled"),
];

/// The repository's own file, which the pane draws below the session's plan.
const REPO: [(&str, &str); 3] = [
    ("the plan's own section", "heading"),
    ("R75 the pane question", "item"),
    ("a section nobody has read", "heading"),
];

/// The daemon's job table with every state on it.
const MANY_JOBS: [(&str, &str, bool); 6] = [
    ("j65", "cargo test --workspace --no-fail-fast", true),
    ("j66", "cargo build --release -p letibot-harnessd", true),
    (
        "j67",
        "rsync -a --exclude target ./ crates/ ../../somewhere/a/long/path/that/keeps/going",
        false,
    ),
    ("j68", "sleep 30", false),
    ("j69", "cargo clippy --all-targets -- -D warnings", false),
    ("j70", "git push", false),
];

/// The notices this head has shown.
const MANY_NOTES: [(&str, &str); 4] = [
    (
        "compacting",
        "compacting: 958397 of 1000005 tokens, on transcript s#t25",
    ),
    (
        "queue_needs_human",
        "`agent/billing-retry` needs a person: the reviewer could not decide",
    ),
    (
        "ledger_chain_mismatch",
        "the ledger's chain does not verify at row s#t25",
    ),
    ("reseated", "the session was reopened on a new daemon"),
];

/// The daemon's sessions.
const MANY_SESSIONS: [(&str, &str); 5] = [
    ("s-habr", "the habr article"),
    ("s-rust", "the rust rewrite"),
    (
        "s-long",
        "a session whose title is long enough to reach past the right edge of the terminal",
    ),
    ("s-review", "review agent/x"),
    ("s-quiet", "a conversation with a quiet board"),
];

fn todos_pane(rows: &[(&str, &str, &str)], repo: Option<&[(&str, &str)]>, w: usize) -> Vec<String> {
    use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoStatus};
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Todos {
        session_id: "s".into(),
        todos: rows
            .iter()
            .map(|(by, content, status)| TodoEntry {
                by: if *by == "you" {
                    TodoBy::Operator
                } else {
                    TodoBy::Model
                },
                content: content.to_string(),
                status: match *status {
                    "inprogress" => TodoStatus::InProgress,
                    "completed" => TodoStatus::Completed,
                    "postponed" => TodoStatus::Postponed,
                    "cancelled" => TodoStatus::Cancelled,
                    _ => TodoStatus::Pending,
                },
                when: None,
                needs: Vec::new(),
            })
            .collect(),
    });
    a.repo_todos = repo.map(|rows| {
        rows.iter()
            .map(|(text, kind)| TodoRow {
                indent: 0,
                mark: (*kind == "item").then_some(TodoMark::Open),
                text: text.to_string(),
                body: Vec::new(),
                item: *kind == "item",
            })
            .collect()
    });
    if !rows.is_empty() || repo.is_some() {
        a.todos_fold_done = false;
    }
    a.push_surface(Surface::Todos);
    a.screen(w, 40)
}

fn jobs_pane(rows: &[(&str, &str, bool)], w: usize) -> Vec<String> {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(jobs_frame(
        "s",
        rows.iter()
            .map(
                |(id, command, running)| letibot_sessionlog::protocol::JobEntry {
                    command: command.to_string(),
                    state: if *running {
                        "running".into()
                    } else {
                        "exited 0".into()
                    },
                    produced: 4_096,
                    elapsed_ms: 14_600,
                    ..daemon_job(id, command, *running)
                },
            )
            .collect(),
    ));
    a.jobs_finished_open = true;
    a.push_surface(Surface::Jobs);
    a.screen(w, 40)
}

fn notes_pane(notes: &[(&str, &str)], w: usize) -> Vec<String> {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    for (i, (code, detail)) in notes.iter().enumerate() {
        a.apply(ServerFrame::Event(env(
            i as u64 + 1,
            SessionEvent::Warning {
                code: code.to_string(),
                detail: detail.to_string(),
                compaction: None,
            },
        )));
    }
    a.command("notes");
    a.screen(w, 40)
}

fn picker_pane(rows: &[(&str, &str)], w: usize) -> Vec<String> {
    let mut a = app();
    a.home = "/home/dead".into();
    let sessions = if rows.is_empty() {
        vec![brief("s", "one", false)]
    } else {
        rows.iter()
            .map(|(id, title)| brief(id, title, false))
            .collect()
    };
    a.apply(hello("s", sessions, Hub::new("s").snapshot()));
    a.push_surface(Surface::Picker);
    a.screen(w, 40)
}

/// Every drawn row fits the screen: a row that wraps is a row the reader loses their place in.
fn fits(name: &str, rows: &[String], w: usize) {
    for r in rows {
        assert!(
            r.chars().count() <= w,
            "{name} at {w}: row is wider than the screen: {r:?}"
        );
    }
}

fn text(rows: &[String]) -> String {
    rows.join("\n")
}

const WIDTHS: [usize; 2] = [80, 120];

#[test]
fn todos_say_what_each_row_is_and_cut_the_long_title() {
    for w in WIDTHS {
        let rows = todos_pane(&MANY_TODOS, Some(&REPO), w);
        fits("todos", &rows, w);
        let t = text(&rows);
        assert!(
            t.contains("todos \u{2014} 3 open \u{b7} 1 postponed"),
            "{t}"
        );
        assert!(t.contains("done (1)"), "{t}");
        // The cancelled row must not read as a postponed one.
        let line = rows
            .iter()
            .find(|r| r.contains("set aside for good"))
            .expect("row");
        assert!(line.contains("cancelled"), "{line:?}");
        let long = rows
            .iter()
            .position(|r| r.contains("a row whose title"))
            .expect("long");
        assert!(rows[long].contains('\u{2026}'), "{:?}", rows[long]);
        assert!(
            rows[long + 1]
                .trim_start()
                .starts_with(|c: char| c.is_ascii_digit() || c == '['),
            "{:?}",
            rows[long + 1]
        );
    }
}

#[test]
fn todos_empty_and_one_item_are_readable() {
    for w in WIDTHS {
        let empty = todos_pane(&[], None, w);
        fits("todos empty", &empty, w);
        assert!(text(&empty).contains("none yet"), "{}", text(&empty));
        let one = todos_pane(&[("you", "write the tests", "pending")], None, w);
        fits("todos one", &one, w);
        assert!(text(&one).contains("write the tests"), "{}", text(&one));
    }
}

#[test]
fn jobs_say_running_or_exited_one_row_each() {
    for w in WIDTHS {
        let none = jobs_pane(&[], w);
        fits("jobs empty", &none, w);
        assert!(text(&none).contains("none."), "{}", text(&none));
        let one = jobs_pane(&[("j65", "cargo build", true)], w);
        fits("jobs one", &one, w);
        assert!(text(&one).contains("j65 cargo build"), "{}", text(&one));
        assert!(text(&one).contains("running"), "{}", text(&one));
        let many = jobs_pane(&MANY_JOBS, w);
        fits("jobs many", &many, w);
        let t = text(&many);
        assert!(t.contains("finished (4)") && t.contains("exited 0"), "{t}");
        let long = many.iter().find(|r| r.contains("j67")).expect("j67");
        assert!(long.contains('\u{2026}') || w >= 120, "{long:?}");
    }
}

#[test]
fn notes_list_one_note_each_with_its_code() {
    for w in WIDTHS {
        let none = notes_pane(&[], w);
        fits("notes empty", &none, w);
        assert!(text(&none).contains("holds no notes"), "{}", text(&none));
        let many = notes_pane(&MANY_NOTES, w);
        fits("notes many", &many, w);
        let t = text(&many);
        for code in ["queue_needs_human", "ledger_chain_mismatch", "reseated"] {
            assert!(t.contains(code), "{t}");
        }
        assert!(t.contains("/notes dismiss"), "{t}");
    }
}

#[test]
fn picker_names_the_daemon_and_the_workspace() {
    for w in WIDTHS {
        let one = picker_pane(&[], w);
        fits("picker one", &one, w);
        let t = text(&one);
        assert!(
            t.contains("sessions in this daemon") && t.contains("letibot (1)"),
            "{t}"
        );
        let many = picker_pane(&MANY_SESSIONS, w);
        fits("picker many", &many, w);
        assert!(text(&many).contains("letibot (5)"), "{}", text(&many));
    }
}
