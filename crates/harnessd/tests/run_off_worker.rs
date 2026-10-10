//! **The operator's `!` run does not hold the daemon's worker** — the proof this
//! branch exists for, plus the two behaviours the handoff has to keep.
//!
//! # The defect, and what fails without the change
//!
//! `CommandKind::OperatorShell` used to run on the daemon's single worker, and the
//! wait inside it held that worker for as long as the command did — the operator's
//! own words, after it bit them twice in one day: *"while a run of theirs is in
//! flight, nothing else runs. another `!` line only queues."* The first test below
//! is written so that it **fails on the old shape**: it starts a `! sleep`, and while
//! that sleep is still running it submits a second command from the same head and
//! requires the daemon to have SERVED it — the `TodosUpdated` event published, and
//! the snapshot taken at that moment carrying no bang row yet. On the old shape the
//! probe is only dispatched after the run returns, so it arrives after the rows and
//! its snapshot already holds them; both assertions fail. Nothing here references a
//! symbol the base branch lacks, so the file compiles there and fails on behaviour
//! rather than on a missing name.
//!
//! # What these need, and what they do not
//!
//! A real exec host (a delegated cgroup v2 subtree) and the vocabulary GGUF, because
//! a `Harness` renders its stable prefix at open. **Not a model**: the endpoint is a
//! local stub that answers every request with HTTP 400 — the settle turn each run
//! starts fails on it in milliseconds, which is the point: a refused connection is
//! classed `Io` and RETRIED with doubling waits (the rule that takes a model server
//! restart in its stride), while a 4xx is *the request, not the weather* and fails
//! the turn at once. The rows and the note must land whether or not the turn they
//! are for can run.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::event::{SessionEvent, TodoBy, TodoEntry, TodoStatus};
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::Registry;
use letibot_transcript::{CallOrigin, Speaker, TranscriptItem, UserPart};
use letibot_turn::Endpoint;

/// How long the in-flight run sleeps. Long enough that a probe submitted about a
/// second in is still mid-flight on any box; short enough that the old shape's
/// failure path (the probe served only after the run) stays inside the driver's
/// deadline with room for a slow open.
const RUN_SLEEP: u64 = 6;

/// The probe row's content — how a driver tells ITS `TodosUpdated` from any other.
const PROBE: &str = "bang-worker-probe-row";

fn repo() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the workspace root is two levels above this crate")
        .to_path_buf()
}

fn config(session: &str, socket: &std::path::Path, model_port: u16) -> Config {
    let mut cfg = Config::for_this_box(repo());
    // **Not this box's configuration** (`wired.rs`'s rule), and **not this box's
    // model**: the settle arm starts a turn for every run, and this file measures the
    // rows and the worker, not the generation — so the endpoint is the local stub
    // that refuses in the one way the retry rule respects (see `refusing_model_stub`).
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.dialect = Dialect::Qwen;
    // **The `present_gguf` gate at each test says this box HAS a vocabulary; this is the
    // one it was talking about.** `Config::for_this_box` carries no default any more — a
    // daemon on the byte vocabulary needs none, which is main's change — so a harness
    // built here must be handed one. `LETIBOT_VOCAB_GGUF` wins, as everywhere else; the
    // box's own GGUF is the same path the test's gate consulted.
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg.session_id = session.into();
    cfg.socket = socket.to_path_buf();
    cfg.endpoint = Endpoint::new("127.0.0.1", model_port);
    // A seat that can run commands: unconfined (`leticode`), `bash` seated, and a
    // mode that needs no oracle — the point here is the exec path, not the mode.
    cfg.seat = Seat::Leticode;
    cfg.allow_bash = true;
    // `Config::for_this_box` carries no vocabulary default any more (a daemon on
    // the byte vocabulary needs none), so a harness built here is handed one: the
    // operator's `LETIBOT_VOCAB_GGUF` if it is set, else the box's own GGUF — the
    // same path this file's `present_gguf` gate consults.
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg
}

fn parts(cfg: &Config) -> Parts {
    let p = Parts::load(cfg).expect(
        "the vocabulary must load; set LETIBOT_VOCAB_GGUF if this box is not the one \
         this repository is developed on",
    );
    // The operator's per-project mode is not this test's business (`wired.rs`'s rule).
    *p.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    p
}

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "letibot-bang-worker-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

/// **A model server that refuses in the one way the retry rule respects.**
///
/// Every settle starts a turn, and this file does not want that turn to work — but a
/// port that merely REFUSES is classed `HttpError::Io`, and `http_retry_after` takes
/// an `Io` round again with the waits doubling: measured here as the worker held for
/// the whole retry budget, which is the very defect this file exists to disprove. A
/// 400 is different — *the request, not the weather* — and fails the turn on the
/// first attempt. So: a listener that drains each request (headers plus the body its
/// `Content-Length` names, so the client's write completes) and answers 400, forever.
fn refusing_model_stub() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            // Read until the headers end and the body the headers name has arrived,
            // so the client's write side is finished before the answer arrives.
            let mut acc: Vec<u8> = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => acc.extend_from_slice(&buf[..n]),
                }
                if let Some(head_end) = acc.windows(4).position(|w| w == b"\r\n\r\n") {
                    let body_have = acc.len() - (head_end + 4);
                    let body_want = acc[..head_end]
                        .split(|b| *b == b'\n')
                        .find_map(|line| {
                            let line = String::from_utf8_lossy(line);
                            let (name, value) = line.split_once(':')?;
                            name.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if body_have >= body_want {
                        break;
                    }
                }
                if acc.len() > (1 << 20) {
                    break;
                }
            }
            let _ = s.write_all(
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    port
}

/// **A daemon, a seated head, and the handles a driver needs.**
///
/// The arrangement is `loop_closes.rs`'s with the threads in their real places: the
/// worker loop runs on the test's own thread (`daemon.run`, exactly what `harnessd`
/// does), the head submits and watches from another, and closing the registry is
/// what ends the loop — which is exactly what a SIGINT does in the real daemon. The
/// head attaches here, BEFORE the session opens and before the worker loop starts,
/// so the driver is watching before the first command can possibly be dispatched:
/// it gets an empty snapshot and every row after it as a live event, which is the
/// path a head that joins a starting daemon takes.
struct Setup {
    daemon: Daemon,
    client: HeadClient,
    rx: Receiver<Inbound>,
    hub: Arc<letibot_sessionlog::hub::Hub>,
    registry: Arc<Registry>,
}

fn setup(tag: &str, session: &str) -> (Setup, Config) {
    let model_port = refusing_model_stub();
    let socket = socket_path(tag);
    let cfg = config(session, &socket, model_port);
    let registry = Registry::new();
    registry
        .create(session.to_string(), "", Sessions::wiring(&cfg))
        .expect("a fresh registry has no session by that name");
    let daemon = Daemon::serve(registry.clone(), &socket).expect("the socket binds");
    let (client, _hello, reader) =
        HeadClient::attach(&socket, session, 0, "tui", "worker-test", Caps::default())
            .expect("a head attaches");
    let (tx, rx) = channel();
    std::thread::spawn(move || pump(reader, tx));
    let hub = registry
        .get(session)
        .expect("the session is in the registry");
    (
        Setup {
            daemon,
            client,
            rx,
            hub,
            registry,
        },
        cfg,
    )
}

fn probe_todo() -> TodoEntry {
    TodoEntry {
        content: PROBE.to_string(),
        status: TodoStatus::Pending,
        by: TodoBy::Operator,
        when: None,
        needs: Vec::new(),
    }
}

/// What one driver has seen, in the order the head saw it.
#[derive(Default)]
struct Record {
    /// The probe's `TodosUpdated` — the fact that the worker was free to serve it.
    todos_seq: Option<u64>,
    /// The run's size note (`operator_shell_ran`) — the settle's first publication.
    note_seq: Option<u64>,
    /// Every `tool_result` row announcement, in seq order.
    tool_rows: Vec<u64>,
    /// How many `tool_result` rows the snapshot held at the moment the probe was
    /// served. `None` until then; `0` is the claim under test.
    tool_rows_at_probe: Option<usize>,
}

/// One event off the head's pump, flattened to what the drivers match on.
enum Seen {
    Todos(u64),
    Note(u64),
    ToolRow(u64),
    Rejected(String),
    Other,
}

fn classify(frame: ServerFrame) -> Seen {
    match frame {
        ServerFrame::Event(env) => match &env.event {
            SessionEvent::TodosUpdated { todos } if todos.iter().any(|t| t.content == PROBE) => {
                Seen::Todos(env.seq)
            }
            SessionEvent::Warning { code, .. } if code == "operator_shell_ran" => {
                Seen::Note(env.seq)
            }
            SessionEvent::TranscriptAppended { kind, .. } if kind == "tool_result" => {
                Seen::ToolRow(env.seq)
            }
            _ => Seen::Other,
        },
        ServerFrame::Rejected { reason, .. } => Seen::Rejected(reason),
        _ => Seen::Other,
    }
}

/// Take one frame off the pump and fold it into the record. A refused frame panics:
/// every frame a driver sends is one the test's premise depends on, and a rejection
/// silently awaited would read as a timeout rather than as what it is.
fn fold(hub: &letibot_sessionlog::hub::Hub, rec: &mut Record, inb: Inbound) {
    match classify(Inbound::frame(inb)) {
        Seen::Todos(seq) => {
            if rec.todos_seq.is_none() {
                // **The snapshot at the moment the probe was served** — the state the
                // worker had produced by then. Taken now, once, because later reads
                // would describe a later world.
                rec.tool_rows_at_probe = Some(
                    hub.snapshot()
                        .items
                        .iter()
                        .filter(|i| i.kind == "tool_result")
                        .count(),
                );
                rec.todos_seq = Some(seq);
            }
        }
        Seen::Note(seq) => {
            rec.note_seq.get_or_insert(seq);
        }
        Seen::ToolRow(seq) => rec.tool_rows.push(seq),
        Seen::Rejected(reason) => {
            panic!("the daemon refused a frame this test depends on: {reason}")
        }
        Seen::Other => {}
    }
}

/// Pump until `done` or the deadline. The deadline is generous — it exists so a
/// regression reads as its missing fact rather than as a hung test — and every
/// driver asserts on what it actually got.
fn watch(
    rx: &Receiver<Inbound>,
    hub: &letibot_sessionlog::hub::Hub,
    rec: &mut Record,
    done: impl Fn(&Record) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(45);
    while Instant::now() < deadline && !done(rec) {
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(inb) => fold(hub, rec, inb),
            Err(RecvTimeoutError::Timeout) => {}
            Err(e) => panic!("the head's pump died: {e}"),
        }
    }
}

/// **While a `!` run is in flight, the worker serves everything else.**
///
/// The proof the branch exists for. A `! sleep` is submitted; a second later — while
/// that sleep MUST still be running — the same head submits its todo board, and the
/// test requires:
///
/// 1. the probe's `TodosUpdated` to arrive **before** the run's size note and its
///    result row (on the old shape the worker was inside the run, so the probe
///    waited for it — this ordering is exactly what was impossible);
/// 2. the snapshot taken when the probe was served to hold **no bang row yet** —
///    the run had not settled, which is the whole claim;
/// 3. the run itself to have happened anyway, once: the marker written, the typed
///    line and its result landed as the feature spells them, and the note's byte
///    count naming the payload it went out with.
#[test]
fn a_bang_run_in_flight_leaves_the_worker_free() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let (setup, cfg) = setup("free", "bang-worker-free");
    let p = parts(&cfg);
    let Setup {
        daemon,
        client,
        rx,
        hub,
        registry,
    } = setup;
    let marker = format!("bang-free-marker-{}", std::process::id());
    let line = format!("! sleep {RUN_SLEEP} && printf bang-ran > {marker} && cat {marker}");
    let mut sessions = Sessions::open_first(&p, cfg, registry.clone()).expect("the session opens");

    let driver = {
        let registry = registry.clone();
        let hub = hub.clone();
        let line = line.clone();
        std::thread::spawn(move || {
            let mut client = client;
            client
                .operator_shell(0, &line)
                .expect("the `!` line is accepted");
            // **Not a synchronisation** — the run is dispatched within milliseconds,
            // and this only makes sure the probe goes in while it is in flight
            // rather than racing the submit itself.
            std::thread::sleep(Duration::from_millis(900));
            client
                .set_operator_todos(0, vec![probe_todo()])
                .expect("the probe is accepted");
            let mut rec = Record::default();
            watch(&rx, &hub, &mut rec, |rec| {
                rec.todos_seq.is_some() && rec.note_seq.is_some() && !rec.tool_rows.is_empty()
            });
            registry.close();
            rec
        })
    };

    daemon.run(&mut sessions, |_, _, _| {});
    let rec = driver.join().expect("the driver thread");
    daemon.shutdown();

    // **1. The probe was served first.** On the old shape both of these fail: the
    // worker came back from the run with the note and the rows already appended,
    // and only then reached the probe.
    let todos_seq = rec
        .todos_seq
        .expect("the probe's TodosUpdated never arrived");
    let note_seq = rec.note_seq.expect("the run's size note never landed");
    assert!(
        !rec.tool_rows.is_empty(),
        "the run's result row never landed"
    );
    assert!(
        todos_seq < note_seq,
        "the probe was served only after the run's size note — the worker was held \
         by the run: todos at seq {todos_seq}, note at seq {note_seq}"
    );
    assert!(
        todos_seq < rec.tool_rows[0],
        "the probe was served only after the run's result row — the worker was held \
         by the run: todos at seq {todos_seq}, row at seq {}",
        rec.tool_rows[0]
    );

    // **2. And the run had not settled when it was.** This is the assertion that
    // names the defect in its own words: nothing else runs WHILE the run is in
    // flight, not merely before it starts.
    assert_eq!(
        rec.tool_rows_at_probe,
        Some(0),
        "a bang row already existed when the probe was served — the run had settled \
         first, which is the defect this file exists against"
    );

    // **3. The run happened anyway, once, in the feature's own shape.**
    let snap = hub.snapshot();
    let items: Vec<&TranscriptItem> = snap.items.iter().filter_map(|i| i.item.as_ref()).collect();
    let typed = items
        .iter()
        .filter(|i| match i {
            TranscriptItem::User {
                speaker: Speaker::Operator,
                parts,
            } => parts
                .iter()
                .any(|p| matches!(p, UserPart::Text { text } if text == &line)),
            _ => false,
        })
        .count();
    assert_eq!(
        typed, 1,
        "the typed line must land once, as the operator's own row: {items:?}"
    );
    let results: Vec<&&TranscriptItem> = items
        .iter()
        .filter(|i| matches!(i, TranscriptItem::ToolResult { name, .. } if name == "bash"))
        .collect();
    assert_eq!(
        results.len(),
        1,
        "the run's result must land exactly once: {items:?}"
    );
    let payload_len = match results[0] {
        TranscriptItem::ToolResult {
            call_id,
            outcome,
            payload,
            origin: Some(CallOrigin::Operator { who }),
            ..
        } => {
            assert_eq!(call_id, "bang-1", "the id the worker minted");
            assert_eq!(who, "worker-test", "the head's identity, as the actor");
            assert!(
                matches!(outcome, letibot_transcript::ToolOutcome::Ok),
                "the command ran: {payload}"
            );
            assert!(
                payload.contains("bang-ran"),
                "the row carries the command's output: {payload}"
            );
            payload.len()
        }
        other => panic!("the row after the line is not the command's result: {other:?}"),
    };
    let wrote = std::fs::read_to_string(repo().join(&marker)).unwrap_or_default();
    assert_eq!(
        wrote, "bang-ran",
        "the command ran in the session's workspace"
    );
    let _ = std::fs::remove_file(repo().join(&marker));

    // And the size note names what it put in the conversation — the disclosure is
    // computed from the payload this row carries, so the two must agree.
    let note = hub
        .retained()
        .iter()
        .find_map(|env| match &env.event {
            SessionEvent::Warning { code, detail, .. } if code == "operator_shell_ran" => {
                Some(detail.clone())
            }
            _ => None,
        })
        .expect("the size note is on the log");
    assert!(
        note.contains(&format!("{payload_len} byte(s) of context")),
        "the note must name the payload's size ({payload_len}): {note}"
    );
    assert!(
        note.contains(&format!("sleep {RUN_SLEEP}")),
        "the note names the command as typed minus the bang: {note}"
    );
}

/// **A second `!` line typed while one is in flight waits, then runs once.**
///
/// The serialization the one-input-handle rule needs (`Prompts` holds ONE input
/// handle per session, so two concurrent runs would steal each other's card and
/// `!send`). The second line here reads the first's marker: if it ran ON TOP of the
/// in-flight run, the file would not exist yet and it would write *overlapped*; if
/// it waited for the run to settle — the rule — it writes *after-first*. It passes
/// on the old shape too (the worker serialized by holding it, which is the defect);
/// it is here because the new path must keep the ordering while dropping the hold,
/// and because a queued line that ran twice or not at all would be a worse defect
/// than the one this branch fixes.
#[test]
fn a_second_bang_line_waits_for_the_run_in_flight_and_runs_once() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let (setup, cfg) = setup("serial", "bang-worker-serial");
    let p = parts(&cfg);
    let Setup {
        daemon,
        client,
        rx,
        hub,
        registry,
    } = setup;
    let first = format!("bang-serial-first-{}", std::process::id());
    let second = format!("bang-serial-second-{}", std::process::id());
    let line1 = format!("! sleep 2 && printf first > {first}");
    // **The probe is the file check, not a timestamp** — mtime granularity would
    // make this a race; existence at the moment the second run starts is not.
    let line2 = format!(
        "! if [ -f {first} ]; then printf after-first > {second}; else printf overlapped > {second}; fi"
    );
    let mut sessions = Sessions::open_first(&p, cfg, registry.clone()).expect("the session opens");

    let driver = {
        let registry = registry.clone();
        let hub = hub.clone();
        let line1 = line1.clone();
        let line2 = line2.clone();
        std::thread::spawn(move || {
            let mut client = client;
            client
                .operator_shell(0, &line1)
                .expect("the first `!` line is accepted");
            // Submitted at once, while the first MUST be in flight or still queued —
            // either way ahead of it in the order the operator typed.
            client
                .operator_shell(0, &line2)
                .expect("the second `!` line is accepted");
            let mut rec = Record::default();
            watch(&rx, &hub, &mut rec, |rec| rec.tool_rows.len() >= 2);
            registry.close();
            rec
        })
    };

    daemon.run(&mut sessions, |_, _, _| {});
    let rec = driver.join().expect("the driver thread");
    daemon.shutdown();

    assert_eq!(
        rec.tool_rows.len(),
        2,
        "both runs' results must land; got {}",
        rec.tool_rows.len()
    );
    // **The serialization itself.**
    let wrote_first = std::fs::read_to_string(repo().join(&first)).unwrap_or_default();
    assert_eq!(wrote_first, "first", "the first run ran");
    let wrote_second = std::fs::read_to_string(repo().join(&second)).unwrap_or_default();
    assert_eq!(
        wrote_second, "after-first",
        "the second line must see the first run's file — it waited for the run in \
         flight rather than overlapping it"
    );
    let _ = std::fs::remove_file(repo().join(&first));
    let _ = std::fs::remove_file(repo().join(&second));

    // **And each ran exactly once, in the order typed.**
    let snap = hub.snapshot();
    let items: Vec<&TranscriptItem> = snap.items.iter().filter_map(|i| i.item.as_ref()).collect();
    for (line, n) in [(&line1, "first"), (&line2, "second")] {
        let typed = items
            .iter()
            .filter(|i| match i {
                TranscriptItem::User { parts, .. } => parts
                    .iter()
                    .any(|p| matches!(p, UserPart::Text { text } if text == line)),
                _ => false,
            })
            .count();
        assert_eq!(
            typed, 1,
            "the {n} line must land once as the operator's own row: {items:?}"
        );
    }
    let bangs: Vec<&str> = items
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::ToolResult { name, call_id, .. } if name == "bash" => {
                Some(call_id.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        bangs,
        ["bang-1", "bang-2"],
        "one result per run, minted in submission order, none repeated: {items:?}"
    );
}
