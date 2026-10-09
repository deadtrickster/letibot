//! **Ctrl+O during a run in flight must move that run to the background** — the
//! frame the head sends, through the daemon, to the run the operator is looking at.
//!
//! # The defect this file is the proof of
//!
//! The operator: *"also ctrl-o printed \"background requests\" and didnt background
//! it"*. The run they were looking at was in flight, and the request was announced —
//! `CommandKind::Promote` renders as *"background requested"* on the log — so the
//! frame arrived. What never happened is the promotion.
//!
//! The mechanism, and why the wait loop never saw the request: the request travels
//! as a **flag on the hub** (shared with the exec backend, so the `bash` wait loop
//! can take it without the worker delivering anything) **and** as a queued command
//! that announces it. For a run the worker is itself holding, the queued command
//! cannot be dispatched until the run ends, so the flag is the request, and the wait
//! loop takes it. But an **operator `!` run does not hold the worker** — it runs on
//! a thread of its own (`crate::bangrun`), exactly so everything else keeps working
//! (`run_off_worker.rs` is that proof). The worker is idle, so it dispatches the
//! queued `CommandKind::Promote` at once, and the between-turns arm takes the flag
//! and warns *"a background request arrived between turns; nothing was running to
//! move"* — which is false: a run was in flight, on the bang thread, whose wait loop
//! is the flag's designed consumer and now finds it gone.
//!
//! The first test below fails on that shape in both of its assertions: the run's
//! row settles only when the command's own clock ends it (the full sleep, not the
//! sub-second a promotion takes), and it settles as the command's own outcome
//! rather than `Backgrounded` by the operator.
//!
//! # What these need, and what they do not
//!
//! The same apparatus `run_off_worker.rs` needs: a delegated cgroup v2 subtree (a
//! run has to start and be movable between scopes) and the vocabulary GGUF (a
//! `Harness` renders its stable prefix at open). **Not a model**: the endpoint is a
//! local stub that answers every request with HTTP 400, so the settle turn each run
//! starts fails in milliseconds and the rows land anyway.

use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::Registry;
use letibot_transcript::{Backgrounding, ToolOutcome, TranscriptItem};

/// How long the run sleeps if nothing intervenes. Long enough that a promote sent
/// about a second in is unambiguously mid-flight; short enough that the broken
/// shape's failure path (the row only when the sleep ends) still lands inside the
/// driver's watch, so the test fails on its assertions rather than on a timeout.
const RUN_SLEEP: u64 = 20;

/// **How long after Ctrl+O the run's row may take to settle.** A promotion ends the
/// wait the moment the wait loop polls the flag (half a second), so anything near
/// this bound means nobody took the request; the bound is set for scheduler jitter
/// and a slow settle, not because the promotion itself is slow.
const PROMOTE_BUDGET: Duration = Duration::from_secs(8);

fn repo() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the workspace root is two levels above this crate")
        .to_path_buf()
}

fn config(session: &str, socket: &std::path::Path, model_port: u16) -> Config {
    let mut cfg = Config::for_this_box(repo());
    // Not this box's configuration (`wired.rs`'s rule), and not this box's model:
    // the settle arm starts a turn per run and this file measures the promote, not
    // the generation. A 400 fails the turn at once (see `refusing_model_stub`).
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.dialect = Dialect::Qwen;
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg.session_id = session.into();
    cfg.socket = socket.to_path_buf();
    cfg.endpoint = letibot_turn::Endpoint::new("127.0.0.1", model_port);
    cfg.seat = Seat::Leticode;
    cfg.allow_bash = true;
    cfg
}

fn parts(cfg: &Config) -> Parts {
    let p = Parts::load(cfg).expect(
        "the vocabulary must load; set LETIBOT_VOCAB_GGUF if this box is not the one \
         this repository is developed on",
    );
    *p.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    p
}

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "letibot-promote-midflight-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

/// A model server that refuses in the one way the retry rule respects — a 400 is
/// *the request, not the weather*, so the settle turn fails on the first attempt
/// instead of holding anything for the retry budget. `run_off_worker.rs`'s stub,
/// verbatim in purpose.
fn refusing_model_stub() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
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

/// What one driver saw. The promote's own announcement is recorded so a failure can
/// name which half of the two-halves design arrived; the idle warning is recorded
/// because on the defect it is the *answer* the request got, and a failure message
/// that quotes it says the mechanism in the daemon's own words.
#[derive(Default)]
struct Record {
    /// The `!` run's result row, and when it settled relative to the promote.
    row_at: Option<Duration>,
    /// The promote's queued-command announcement ("background requested").
    announced: bool,
    /// The between-turns warning that ate the request, if the daemon published it.
    idle_warned: bool,
}

fn classify(frame: ServerFrame, sent: &Instant, rec: &mut Record) {
    match frame {
        ServerFrame::Accepted { note, .. } if note.contains("background requested") => {
            // The daemon's own sentence for the frame arriving — what the operator
            // saw printed. Recorded so a failure can say the request DID land.
            rec.announced = true;
        }
        ServerFrame::Event(env) => match &env.event {
            SessionEvent::TranscriptAppended { kind, .. } if kind == "tool_result" => {
                // The run's row is the first tool result this session produces.
                if rec.row_at.is_none() {
                    rec.row_at = Some(sent.elapsed());
                }
            }
            SessionEvent::Warning { code, .. } if code == "promote_idle" => {
                rec.idle_warned = true;
            }
            _ => {}
        },
        ServerFrame::Rejected { reason, .. } => {
            panic!("the daemon refused a frame this test depends on: {reason}")
        }
        _ => {}
    }
}

fn watch(rx: &Receiver<Inbound>, sent: &Instant, rec: &mut Record) {
    // Long enough that the broken shape's row (at the sleep's end) still arrives and
    // the test fails on its assertions with the real number in the message.
    let deadline = Instant::now() + Duration::from_secs(40);
    while Instant::now() < deadline && rec.row_at.is_none() {
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(inb) => classify(Inbound::frame(inb), sent, rec),
            Err(RecvTimeoutError::Timeout) => {}
            Err(e) => panic!("the head's pump died: {e}"),
        }
    }
}

/// **Ctrl+O's frame, mid-run, moves the run the operator is looking at.**
///
/// A `! sleep` runs on its own thread; a second later — while it MUST still be in
/// flight — the head sends exactly what Ctrl+O sends (`HeadClient::promote`, the
/// driver's own call for `Action::Promote`). The run must then settle as
/// `Backgrounded` by the operator, promptly: a promotion ends the wait, so a row
/// that only arrives when the sleep has run its course is a request nobody honoured.
#[test]
fn a_ctrl_o_promotes_the_operators_run_in_flight() {
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let session = "promote-midflight";
    let model_port = refusing_model_stub();
    let socket = socket_path("bang");
    let cfg = config(session, &socket, model_port);
    let registry = Registry::new();
    registry
        .create(session.to_string(), "", Sessions::wiring(&cfg))
        .expect("a fresh registry has no session by that name");
    let daemon = Daemon::serve(registry.clone(), &socket).expect("the socket binds");
    let (mut client, _hello, reader) =
        HeadClient::attach(&socket, session, 0, "tui", "promote-test", Caps::default())
            .expect("a head attaches");
    let (tx, rx) = channel();
    std::thread::spawn(move || pump(reader, tx));
    let hub = registry
        .get(session)
        .expect("the session is in the registry");

    let line = format!("! sleep {RUN_SLEEP}");
    let p = parts(&cfg);
    let mut sessions = Sessions::open_first(&p, cfg, registry.clone()).expect("the session opens");

    let driver = {
        let registry = registry.clone();
        let line = line.clone();
        std::thread::spawn(move || {
            client
                .operator_shell(0, &line)
                .expect("the `!` line is accepted");
            // Not a synchronisation — the run is dispatched within milliseconds, and
            // this only makes sure the promote goes in while the run is in flight
            // rather than racing the submit itself.
            std::thread::sleep(Duration::from_millis(1500));
            let sent = Instant::now();
            client.promote(0).expect("ctrl-o's frame is accepted");
            let mut rec = Record::default();
            watch(&rx, &sent, &mut rec);
            registry.close();
            rec
        })
    };

    daemon.run(&mut sessions, |_, _, _| {});
    let rec = driver.join().expect("the driver thread");
    daemon.shutdown();

    // **The run settled as a backgrounding, attributed to the operator.** On the
    // defect this is the load-bearing assertion: the run runs to its own end and
    // settles as its own outcome, because nothing took the request.
    let snap = hub.snapshot();
    let backgrounded = snap
        .items
        .iter()
        .filter_map(|i| i.item.as_ref())
        .find_map(|i| match i {
            TranscriptItem::ToolResult {
                outcome: out @ ToolOutcome::Backgrounded { .. },
                ..
            } => Some(out.clone()),
            _ => None,
        });
    let how = match backgrounded {
        Some(ToolOutcome::Backgrounded { how, .. }) => how,
        Some(_) => unreachable!("the find_map only yields Backgrounded"),
        None => panic!(
            "no row settled as `Backgrounded`: the promote was announced \
             ({}) and the daemon's between-turns answer was the idle warning ({}) \
             while the operator's run was in flight on its own thread",
            rec.announced, rec.idle_warned
        ),
    };
    assert_eq!(
        how,
        Backgrounding::Operator {
            identity: "promote-test".into()
        },
        "the head that pressed Ctrl+O is who the backgrounding is attributed to"
    );

    // **And promptly.** A promotion ends the wait at the loop's next poll of the
    // flag; a row that arrives only at the sleep's end is the request dropped.
    let at = rec.row_at.expect("a backgrounded run settles at once");
    assert!(
        at < PROMOTE_BUDGET,
        "the run settled {at:?} after Ctrl+O — the request was not honoured while \
         the run was in flight (the announcement landed: {}, the idle warning: {})",
        rec.announced,
        rec.idle_warned
    );
}
