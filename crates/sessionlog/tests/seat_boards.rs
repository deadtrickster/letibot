//! A Claude Code seat's board reaches the head on the daemon's `Todos` read (R144,
//! `docs/seat-todo.md`). A seat is not a session - the store keeps `seat_todo` apart for that
//! reason - so the frame carries the boards beside the session's own, and a head draws both.

use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::event::{SeatBoard, TodoEntry, TodoNeed, TodoStatus};
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionSource, SessionWiring, StoredBrief};
use letibot_sessionlog::server::{ServerHandle, serve_registry};

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    letibot_sessionlog::testing::scratch_root().join(format!(
        "lb-seat-{tag}-{}-{:x}.sock",
        std::process::id(),
        n as u32
    ))
}

fn wiring() -> SessionWiring {
    SessionWiring {
        model: "qwen-3.8-flash-next".into(),
        dialect: "qwen3.8".into(),
        endpoint: "127.0.0.1:8080".into(),
        workspace: "/home/dead/Projects/letibot".into(),
    }
}

/// A source with no sessions on disk and one seat's board: the store's two reads, as the daemon
/// asks them. What a session's rows are is `todos`' business; this is only about the seat.
struct OneSeat {
    boards: Vec<SeatBoard>,
}

impl SessionSource for OneSeat {
    fn list(&self) -> Vec<StoredBrief> {
        Vec::new()
    }

    fn seat_boards(&self) -> Vec<SeatBoard> {
        self.boards.clone()
    }
}

fn start(tag: &str, boards: Vec<SeatBoard>) -> (Arc<Registry>, ServerHandle) {
    let r = Registry::new();
    r.create("a", "the cache question", wiring()).unwrap();
    r.set_source(Arc::new(OneSeat { boards }));
    let h = serve_registry(r.clone(), socket_path(tag)).expect("bind");
    (r, h)
}

/// Read frames until one of them says stop, or time out with how many were seen.
fn until(
    rx: &std::sync::mpsc::Receiver<Inbound>,
    mut f: impl FnMut(&ServerFrame) -> bool,
) -> Vec<ServerFrame> {
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(frame) => {
                let frame = frame.frame();
                let done = f(&frame);
                seen.push(frame);
                if done {
                    return seen;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    panic!("the `Todos` frame never arrived; saw {} frames", seen.len());
}

fn row(content: &str, status: TodoStatus, needs: Vec<TodoNeed>) -> TodoEntry {
    TodoEntry {
        content: content.into(),
        status,
        by: letibot_sessionlog::event::TodoBy::Model,
        when: None,
        needs,
    }
}

/// **The daemon answers the pane's one read with the seats' boards**, and a row's wait on a
/// package is the edge it was written with: the pane draws a seat's row as a session's, so the
/// frame has to carry the same `TodoEntry` the session's half does.
#[test]
fn the_todos_read_carries_the_seats_boards_with_their_edges() {
    let (_reg, server) = start(
        "carries",
        vec![SeatBoard {
            seat: "cc@proj".into(),
            todos: vec![
                row(
                    "collect the package",
                    TodoStatus::InProgress,
                    vec![TodoNeed::Child {
                        id: "s-1789462738453908838".into(),
                    }],
                ),
                row("land the port", TodoStatus::Completed, Vec::new()),
            ],
        }],
    );
    let (mut client, _hello, reader) =
        HeadClient::attach(server.path(), "a", 0, "tui", "dead", Caps::default()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let pumping = std::thread::spawn(move || pump(reader, tx));
    client.list_todos().unwrap();
    let seen = until(&rx, |f| matches!(f, ServerFrame::Todos { .. }));
    let ServerFrame::Todos {
        session_id,
        todos,
        seats,
    } = seen.last().cloned().expect("the frame")
    else {
        unreachable!()
    };
    assert_eq!(session_id, "a");
    assert!(todos.is_empty(), "this source answers no session's board");
    assert_eq!(seats.len(), 1, "one seat, one board: {seats:?}");
    assert_eq!(seats[0].seat, "cc@proj");
    assert_eq!(seats[0].todos.len(), 2);
    assert_eq!(
        seats[0].todos[0].needs,
        vec![TodoNeed::Child {
            id: "s-1789462738453908838".into()
        }],
        "the wait on the package crossed the wire"
    );
    assert_eq!(seats[0].todos[1].status, TodoStatus::Completed);
    // The pump is left to the process: `ServerHandle` drops its socket and not its connections,
    // so a reader waiting on one is ended by the test binary exiting and not by this thread.
    drop(client);
    drop(server);
    drop(pumping);
}

/// **A daemon with no seat boards says so by saying nothing.** The frame is the one it always
/// sent, with an empty list: a folder whose seat never wrote a plan is not a pane that has to
/// know about seats at all.
#[test]
fn a_daemon_with_no_seats_answers_an_empty_list() {
    let (_reg, server) = start("none", Vec::new());
    let (mut client, _hello, reader) =
        HeadClient::attach(server.path(), "a", 0, "tui", "dead", Caps::default()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let pumping = std::thread::spawn(move || pump(reader, tx));
    client.list_todos().unwrap();
    let seen = until(&rx, |f| matches!(f, ServerFrame::Todos { .. }));
    let ServerFrame::Todos { seats, .. } = seen.last().cloned().expect("the frame") else {
        unreachable!()
    };
    assert!(seats.is_empty(), "{seats:?}");
    drop(client);
    drop(server);
    drop(pumping);
}

/// The source is the `SessionSource`'s: a registry with none answers no boards rather than
/// failing, which is what a daemon that has not opened its store yet hands the server.
#[test]
fn a_registry_with_no_source_has_no_seat_boards() {
    assert!(Registry::new().seat_boards().is_empty());
}
