//! Job waits that end: taken off the board with a note, never left reading as pending.

use super::*;

fn two_jobs(row: &str, a: &str, b: &str) -> TodoItem {
    TodoItem {
        needs: vec![
            TodoNeed::Job { handle: a.into() },
            TodoNeed::Job { handle: b.into() },
        ],
        ..item(row, TodoStatus::Pending)
    }
}

#[test]
fn an_ended_job_comes_off_every_row_that_waited_on_it() {
    let board = TodoBoard::new(vec![
        two_jobs("K4", "j1", "j2"),
        two_jobs("K5", "j1", "j2"),
        waiting_on_a_job("K6", "j3"),
    ]);
    let before = board.version();
    let mut asked = Vec::new();
    let resolved = board.resolve_job_waits(|h| {
        asked.push(h.to_string());
        (h == "j1").then(|| job_ended_words(h, Some("exited 0"), Some("11-15 22:13")))
    });
    assert_eq!(asked, vec!["j1", "j2", "j3"], "each handle is asked once");
    assert_eq!(
        resolved,
        vec![ResolvedWait {
            handle: "j1".into(),
            said: "j1 ended (exit 0) at 11-15 22:13".into(),
            rows: vec!["K4".into(), "K5".into()],
        }]
    );
    let snap = board.snapshot();
    assert_eq!(
        snap[0].needs,
        vec![TodoNeed::Job {
            handle: "j2".into()
        }]
    );
    assert_eq!(
        snap[1].needs,
        vec![TodoNeed::Job {
            handle: "j2".into()
        }]
    );
    assert_eq!(
        snap[2].needs,
        vec![TodoNeed::Job {
            handle: "j3".into()
        }]
    );
    assert!(board.version() > before, "a resolution is persisted");
    let text = board_text(&snap);
    assert!(!text.contains("j1"), "the ended job is not shown: {text}");
    assert_eq!(
        resolved_waits_note(&resolved),
        "[todo] j1 ended (exit 0) at 11-15 22:13 - no longer a wait of `K4`, `K5`."
    );
}

#[test]
fn a_row_whose_only_job_ended_is_ready_and_listed() {
    let board = TodoBoard::new(vec![waiting_on_a_job("deploy", "j9")]);
    board.resolve_job_waits(|h| Some(job_ended_words(h, Some("exited 0"), None)));
    let snap = board.snapshot();
    assert!(snap[0].needs.is_empty());
    assert!(
        !waits_on_a_running_job(&snap[0], &no_jobs()),
        "the row is free once its job ended"
    );
    assert_eq!(render_answer(&snap, &[]).matches("deploy").count(), 1);
}

#[test]
fn nothing_running_moves_and_done_rows_are_left_alone() {
    let mut done = waiting_on_a_job("old gate", "j1");
    done.status = TodoStatus::Completed;
    let board = TodoBoard::new(vec![done, waiting_on_a_job("next", "j2")]);
    let before = board.version();
    let resolved = board.resolve_job_waits(|h| (h == "j1").then(|| "ended".to_string()));
    assert!(
        resolved.is_empty(),
        "j1 is only on a done row, j2 still runs"
    );
    assert_eq!(board.version(), before);
    assert_eq!(board.snapshot()[0].needs.len(), 1);
}

#[test]
fn the_operators_half_is_resolved_too() {
    let mut theirs = waiting_on_a_job("theirs", "j1");
    theirs.by = TodoBy::Operator;
    let board = TodoBoard::new(vec![theirs]);
    let resolved = board.resolve_job_waits(|_| Some("j1 ended (exit 0)".into()));
    assert_eq!(resolved[0].rows, vec!["theirs".to_string()]);
    assert!(board.snapshot()[0].needs.is_empty());
}

#[test]
fn a_failed_or_missing_job_says_so() {
    assert_eq!(
        job_ended_words("j7", Some("exited 101"), Some("11-15 22:37")),
        "j7 ended (exit 101, failed) at 11-15 22:37"
    );
    assert_eq!(
        job_ended_words("j7", Some("killed by job_kill"), None),
        "j7 ended (killed by job_kill)"
    );
    assert_eq!(
        job_ended_words("j7", None, None),
        "j7 is not in this session's job table (lost in a restart, or a wrong handle), so its \
         result is unavailable"
    );
}

#[test]
fn a_child_pid_or_monitor_that_ended_comes_off_and_row_edges_stay() {
    let row = TodoItem {
        needs: vec![
            TodoNeed::Child { id: "s-c1".into() },
            TodoNeed::Pid { pid: 7, start: 9 },
            TodoNeed::Monitor { name: "m".into() },
            TodoNeed::Row {
                content: "prep".into(),
            },
        ],
        ..item("after", TodoStatus::Pending)
    };
    let board = TodoBoard::new(vec![item("prep", TodoStatus::Pending), row]);
    let resolved = board.resolve_waits(|need| match need {
        TodoNeed::Child { id } => Some(format!("child {id} ended (done)")),
        TodoNeed::Monitor { .. } => None,
        _ => Some("gone".into()),
    });
    let handles: Vec<&str> = resolved.iter().map(|r| r.handle.as_str()).collect();
    assert_eq!(handles, vec!["s-c1", "pid 7:9"]);
    assert_eq!(
        board.snapshot()[1].needs,
        vec![
            TodoNeed::Monitor { name: "m".into() },
            TodoNeed::Row {
                content: "prep".into()
            },
        ]
    );
}
