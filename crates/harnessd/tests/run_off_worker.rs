//! The operator's `!` run does not hold the daemon's worker - the proof, plus the two
//! behaviours the handoff has to keep.

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

mod support;

/// How long the in-flight run sleeps. Long enough that a probe submitted about a second in is still
/// mid-flight on any box; short enough that the old shape's failure path - the probe served only
/// after the run - stays inside the driver's deadline with room for a slow open.
const RUN_SLEEP: u64 = 6;

/// The probe row's content - how a driver tells ITS `TodosUpdated` from any other.
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
    // Not this box's configuration, and not this box's model
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.dialect = Dialect::Qwen;
    // The `vocab_gguf` gate at each test says this box HAS a vocabulary; this is the one it
    // was talking about. `Config::for_this_box` carries no default - a daemon on the byte
    // vocabulary needs none - so a harness built here must be handed one.
    cfg.vocab_gguf = support::vocab::gguf();
    cfg.session_id = session.into();
    cfg.socket = socket.to_path_buf();
    cfg.endpoint = Endpoint::new("127.0.0.1", model_port);
    // A seat that can run commands: unconfined (`leticode`), `bash` seated, and a mode that needs
    // no oracle - the point here is the exec path, not the mode.
    cfg.seat = Seat::Leticode;
    cfg.allow_bash = true;
    cfg
}

fn parts(cfg: &Config) -> Parts {
    let p = Parts::load(cfg).expect(
        "the vocabulary must load; set LETIBOT_VOCAB_GGUF if this box is not the one \
         this repository is developed on",
    );
    // The operator's per-project mode is not this test's business.
    *p.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    p
}

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-bang-worker-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

/// A model server that refuses in the one way the retry rule respects.
fn refusing_model_stub() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            // Read until the headers end and the body the headers name has arrived, so the client's
            // write side is finished before the answer arrives.
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

/// A daemon, a seated head, and the handles a driver needs. The threads are in their real
/// places
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
    /// The probe's `TodosUpdated` - the fact that the worker was free to serve it.
    todos_seq: Option<u64>,
    /// The run's size note (`operator_shell_ran`) - the settle's first publication.
    note_seq: Option<u64>,
    /// Every `tool_result` row announcement, in seq order.
    tool_rows: Vec<u64>,
    /// How many `tool_result` rows the snapshot held at the moment the probe was served. `None`
    /// until then; `0` is the claim under test.
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

/// Take one frame off the pump and fold it into the record. A refused frame panics: every frame a
/// driver sends is one the test's premise depends on, and a rejection silently awaited would read as
/// a timeout rather than as what it is.
fn fold(hub: &letibot_sessionlog::hub::Hub, rec: &mut Record, inb: Inbound) {
    match classify(Inbound::frame(inb)) {
        Seen::Todos(seq) => {
            if rec.todos_seq.is_none() {
                // The snapshot at the moment the probe was served - the state the worker had
                // produced by then. Taken now, once, because later reads would describe a later
                // world.
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

/// Pump until `done` or the deadline. The deadline is generous - it exists so a regression reads as
/// its missing fact rather than as a hung test - and every driver asserts on what it actually got.
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

/// While a `!` run is in flight, the worker serves everything else. A `! sleep` is
/// submitted; a second later - while that sleep MUST still be running - the same head
/// submits its todo board, and the test requires 1.
#[test]
fn a_bang_run_in_flight_leaves_the_worker_free() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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
            // Not a synchronisation - the run is dispatched within milliseconds, and this only
            // makes sure the probe goes in while it is in flight rather than racing the submit
            // itself.
            std::thread::sleep(Duration::from_millis(900));
            client
                .set_operator_todos(0, vec![probe_todo()], Vec::new())
                .expect("the probe is accepted");
            let mut rec = Record::default();
            watch(&rx, &hub, &mut rec, |rec| {
                rec.todos_seq.is_some() && rec.note_seq.is_some() && !rec.tool_rows.is_empty()
            });
            registry.close();
            rec
        })
    };

    daemon.run(&mut sessions, |_, _| {});
    let rec = driver.join().expect("the driver thread");
    daemon.shutdown();

    // 1. The probe was served first. On the old shape both of these fail: the worker came back
    // from the run with the note and the rows already appended, and only then reached the probe.
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

    // 2. And the run had not settled when it was. This is the assertion that names the defect in
    // its own words: nothing else runs WHILE the run is in flight, not merely before it starts.
    assert_eq!(
        rec.tool_rows_at_probe,
        Some(0),
        "a bang row already existed when the probe was served — the run had settled \
         first, which is the defect this file exists against"
    );

    // 3. The run happened anyway, once, in the feature's own shape.
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

    // And the size note names what it put in the conversation - the disclosure is computed from the
    // payload this row carries, so the two must agree.
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

/// A second `!` line typed while one is in flight waits, then runs once. The serialization
/// the one-input-handle rule needs (`Prompts` holds ONE input handle per session so two
/// concurrent runs would steal each other's card and `!send`).
#[test]
fn a_second_bang_line_waits_for_the_run_in_flight_and_runs_once() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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
    // The probe is the file check, not a timestamp - mtime granularity would make this a race
    // existence at the moment the second run starts is not.
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
            // Submitted at once, while the first MUST be in flight or still queued - either way ahead
            // of it in the order the operator typed.
            client
                .operator_shell(0, &line2)
                .expect("the second `!` line is accepted");
            let mut rec = Record::default();
            watch(&rx, &hub, &mut rec, |rec| rec.tool_rows.len() >= 2);
            registry.close();
            rec
        })
    };

    daemon.run(&mut sessions, |_, _| {});
    let rec = driver.join().expect("the driver thread");
    daemon.shutdown();

    assert_eq!(
        rec.tool_rows.len(),
        2,
        "both runs' results must land; got {}",
        rec.tool_rows.len()
    );
    // The serialization itself.
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

    // And each ran exactly once, in the order typed.
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
