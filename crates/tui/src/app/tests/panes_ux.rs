//! **What the todos, jobs, notes and picker panes say to somebody who has never seen them**: one
//! realistic fixture each, drawn at eighty and a hundred and twenty columns, and the words a
//! newcomer needs asserted.
//!
//! The queue pane set the pattern (`queue_ux`): plain status words, one row per item, aligned
//! columns, detail folded away, and a legend for the verbs. These four are held to it here, so a
//! pane that changes its words changes a picture in this file.

use super::*;

/// The pictures this file writes and reads, beside the queue's.
fn dump(dir: &str, name: &str, text: &str) {
    let dir = std::path::Path::new("scratch").join(dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), text).unwrap();
}

/// Every pane, at both widths, in one file - the dump a change is read against.
#[test]
fn dump_every_pane() {
    let mut out = String::new();
    for w in [80usize, 120] {
        for (name, rows) in [
            ("todos empty", todos_pane(&[], None, w)),
            ("todos one", todos_pane(&[("you", "write the tests", "pending")], None, w)),
            ("todos many", todos_pane(&MANY_TODOS, Some(&REPO), w)),
            ("jobs empty", jobs_pane(&[], w)),
            ("jobs one", jobs_pane(&[("j65", "cargo build", true)], w)),
            ("jobs many", jobs_pane(&MANY_JOBS, w)),
            ("notes empty", notes_pane(&[], w)),
            ("notes one", notes_pane(&[("compacting", "compacting: 958397 of 1000005 tokens")], w)),
            ("notes many", notes_pane(&MANY_NOTES, w)),
            ("picker one", picker_pane(&[], w)),
            ("picker many", picker_pane(&MANY_SESSIONS, w)),
        ] {
            out.push_str(&format!("\n===== {name}, {w} cols =====\n"));
            out.push_str(&rows.join("\n"));
        }
    }
    dump("panes-ux", "panes.txt", &out);
}

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
    ("compacting", "compacting: 958397 of 1000005 tokens, on transcript s#t25"),
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
            .map(|(id, command, running)| letibot_sessionlog::protocol::JobEntry {
                command: command.to_string(),
                state: if *running {
                    "running".into()
                } else {
                    "exited 0".into()
                },
                produced: 4_096,
                elapsed_ms: 14_600,
                ..daemon_job(id, command, *running)
            })
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
