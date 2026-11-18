//! A row that waits on ANOTHER board's row - an external seat's, or another session's of this
//! project: quiet while that row is open, and freed with one note the moment it is finished there.

use super::*;
use letibot_tokencore::store::{Store, TodoNeed, TodoStatus};

/// A directory of its own per test, removed when it ends.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            letibot_sessionlog::registry::now_ms()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }

    /// The handle a bridge in this folder would carry: `cc@<folder>`, which is one of the words the
    /// project is known by - the other half of the same-project rule.
    fn seat(&self) -> String {
        format!(
            "cc@{}",
            self.0
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        )
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A session on the byte vocabulary, with a store of its own in `dir`.
fn a_harness(dir: &Scratch, id: &str) -> Harness {
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(dir.0.join("sessions.db"));
    cfg.session_id = id.into();
    cfg.vocab_gguf = None;
    at_always_ask(&mut cfg);
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    Harness::open(&parts, cfg, Hub::new(id)).expect("the session opens")
}

/// The seat's board, written the way a bridge writes it: into the same store, under its handle.
fn seat_board(dir: &Scratch, seat: &str, rows: &[(&str, TodoStatus)]) {
    let store = Store::open(&dir.0.join("sessions.db")).expect("the store");
    let rows: Vec<letibot_tokencore::store::TodoItem> = rows
        .iter()
        .map(|(content, status)| letibot_tokencore::store::TodoItem {
            content: (*content).into(),
            status: *status,
            by: letibot_tokencore::store::TodoBy::Model,
            when: None,
            needs: Vec::new(),
        })
        .collect();
    store.put_seat_todos(seat, &rows).expect("the seat's board");
}

/// A row on the session's own board waiting on `content` of `seat`.
fn wait_on(h: &mut Harness, row: &str, seat: &str, content: &str) {
    h.todos.upsert_model(&[(
        row.to_string(),
        TodoStatus::InProgress,
        vec![TodoNeed::Todo {
            seat: seat.to_string(),
            content: content.to_string(),
        }],
    )]);
}

const ROW: &str = "collect the package";
const TARGET: &str = "review the parity row";

/// The plan check says nothing while the other board's row is open - and the sweep, which runs at
/// open and at every nag tick, frees the edge with one note naming the seat and the row when that
/// board finishes it, and wakes the session because the row can start now.
#[test]
fn a_row_waiting_on_a_seats_row_is_held_and_then_freed_by_its_finishing() {
    let dir = Scratch::new("seat-wait-freed");
    let seat = dir.seat();
    seat_board(&dir, &seat, &[(TARGET, TodoStatus::InProgress)]);
    let mut h = a_harness(&dir, "s-r65-held");
    wait_on(&mut h, ROW, &seat, TARGET);

    assert!(
        h.nag_notice().is_none(),
        "the check has nothing to say while another board's row is open"
    );
    let freed = h.resolve_ended_waits();
    assert_eq!(freed.waits, 0, "nothing has ended there");
    assert_eq!(
        h.todos.snapshot()[0].needs,
        vec![TodoNeed::Todo {
            seat: seat.clone(),
            content: TARGET.into(),
        }],
        "the edge stands while that row is open"
    );

    // The seat marks its row done - what its own `todo_write` writes to the store.
    seat_board(&dir, &seat, &[(TARGET, TodoStatus::Completed)]);
    let freed = h.resolve_ended_waits();
    assert_eq!(freed.waits, 1, "one wait was freed");
    assert!(
        freed.wake,
        "the row can start now, so the note rides a turn rather than a silent line"
    );
    let said = h.board_notices.join("\n");
    assert!(
        said.contains(&format!("{seat} finished \"{TARGET}\"")) && said.contains(ROW),
        "the note names the seat, its row and yours: {said}"
    );
    assert!(
        h.todos.snapshot()[0].needs.is_empty(),
        "the edge is off the row"
    );
    // And the row is work the check may speak about again.
    let board = h.todos.snapshot();
    let children = h.child_sessions();
    let jobs = h.running_jobs(&board);
    assert!(
        letibot_tools::builtins::todo::is_ready(&board, &board[0], &children, &jobs),
        "nothing is in its way now"
    );
}

/// A row the seat DROPS, or one the operator struck off there, frees the wait too and says which:
/// a board is replaced whole, so a row that is gone is a row somebody dropped.
#[test]
fn a_row_that_is_dropped_or_struck_off_frees_the_wait_and_says_so() {
    let dir = Scratch::new("seat-wait-gone");
    let seat = dir.seat();
    seat_board(
        &dir,
        &seat,
        &[
            (TARGET, TodoStatus::InProgress),
            ("a row it keeps", TodoStatus::InProgress),
        ],
    );
    let mut h = a_harness(&dir, "s-r65-gone");
    wait_on(&mut h, ROW, &seat, TARGET);
    assert_eq!(h.resolve_ended_waits().waits, 0);

    // Dropped: the board is written without it.
    seat_board(&dir, &seat, &[("a row it keeps", TodoStatus::InProgress)]);
    let freed = h.resolve_ended_waits();
    assert_eq!(freed.waits, 1);
    let said = h.board_notices.join("\n");
    assert!(
        said.contains(&format!("`{TARGET}` is no longer on {seat}'s board")),
        "{said}"
    );

    // And struck off: the row is still there, marked cancelled.
    let dir = Scratch::new("seat-wait-struck");
    let seat = dir.seat();
    seat_board(&dir, &seat, &[(TARGET, TodoStatus::InProgress)]);
    let mut h = a_harness(&dir, "s-r65-struck");
    wait_on(&mut h, ROW, &seat, TARGET);
    assert_eq!(h.resolve_ended_waits().waits, 0);
    seat_board(&dir, &seat, &[(TARGET, TodoStatus::Cancelled)]);
    assert_eq!(h.resolve_ended_waits().waits, 1);
    let said = h.board_notices.join("\n");
    assert!(
        said.contains(&format!("{seat} struck \"{TARGET}\" off")),
        "{said}"
    );
}

/// The daemon's own `todo_write`, seated at open: a wait on a seat's row is accepted when that
/// board is there, and refused by name when it is not - the store is read in the call that writes
/// it, so nothing is written on a refusal.
#[test]
fn the_seated_tool_writes_the_wait_or_refuses_the_seat_by_name() {
    let dir = Scratch::new("seat-wait-tool");
    let seat = dir.seat();
    seat_board(&dir, &seat, &[(TARGET, TodoStatus::InProgress)]);
    let h = a_harness(&dir, "s-r65-tool");
    let mut sink = letibot_tools::events::NullToolSink;
    let call = |args: &str| ToolCall {
        id: "c1".into(),
        name: "todo_write".into(),
        arguments: args.into(),
    };

    let unknown = h.runtime.invoke(
        "t1",
        &call(
            r#"{"todos": [{"content": "wait for it", "status": "in_progress",
                "needs": [{"kind": "todo", "seat": "cc@no-such-seat", "content": "x"}]}]}"#,
        ),
        &mut sink,
    );
    assert!(!unknown.is_grounded(), "{}", unknown.payload);
    assert!(
        format!("{:?}", unknown.outcome).contains("cc@no-such-seat"),
        "the refusal is by name: {:?}",
        unknown.outcome
    );
    assert!(h.todos.snapshot().is_empty(), "nothing was written");

    let wrote = h.runtime.invoke(
        "t2",
        &call(&format!(
            r#"{{"todos": [{{"content": "{ROW}", "status": "in_progress",
                "needs": [{{"kind": "todo", "seat": "{seat}", "content": "{TARGET}"}}]}}]}}"#
        )),
        &mut sink,
    );
    assert!(wrote.is_grounded(), "{}", wrote.payload);
    assert_eq!(
        h.todos.snapshot()[0].needs,
        vec![TodoNeed::Todo {
            seat: seat.clone(),
            content: TARGET.into(),
        }],
        "the edge the daemon wrote is the one its sweep will free"
    );
    assert!(
        h.nag_notice().is_none(),
        "and the row is quiet while that board's row is open"
    );
}

/// A board of this project that is not there is SAID to be gone; a board of another project is
/// never read at all - the wait stands rather than being freed by something this project cannot
/// see, which is the same rule the write refuses on.
#[test]
fn a_gone_board_is_said_to_be_gone_and_another_projects_is_not_read() {
    let dir = Scratch::new("seat-wait-unread");
    let seat = dir.seat();
    seat_board(&dir, &seat, &[(TARGET, TodoStatus::InProgress)]);
    let mut h = a_harness(&dir, "s-r65-unread");
    // A seat of THIS project whose board is no longer there: the row is named, and the board has
    // gone - which is an ending the model is owed.
    wait_on(&mut h, ROW, &format!("{seat}:twin"), TARGET);
    assert_eq!(h.resolve_ended_waits().waits, 1);
    assert!(
        h.board_notices
            .join("\n")
            .contains(&format!("{seat}:twin's board is gone")),
        "{:?}",
        h.board_notices
    );

    // And a seat of ANOTHER project: nothing here reads it, so nothing here frees the wait.
    let mut h = a_harness(&dir, "s-r65-elsewhere");
    wait_on(&mut h, ROW, "cc@another-project", TARGET);
    assert_eq!(h.resolve_ended_waits().waits, 0);
    assert_eq!(h.todos.snapshot()[0].needs.len(), 1, "the edge stands");
}
