//! **`/clear`: the conversation starts over and the session does not.**
//!
//! The operator, verbatim: *"I need to be able to clear context - fresh reseat without loosing
//! session todos and jobs"*, *"and subagents"*, *"this will allow me to fix model degradation"*.
//!
//! What is proved here is the whole list, on one session, through the production doors:
//!
//! 1. **The transcript is new and short** — message zero plus one orientation item, spoken as
//!    the harness (`Speaker::Agent`), naming the old transcript, the board, the job and the
//!    child, and the operator's sentence.
//! 2. **The board is identical** — both halves, statuses and authors.
//! 3. **A job that settles AFTER the clear is delivered into the NEW conversation**, by the next
//!    wake, as it would have been into the old one.
//! 4. **A child that answers AFTER the clear reaches its parent's new conversation** — its
//!    settlement rides the same watchers, which are the session's and not the transcript's.
//! 5. **The old transcript is still in the store, whole.**
//! 6. **The context count is about the new transcript**: unmeasured, and the size of the new
//!    base — no anchor left over from the conversation that was cleared.
//!
//! And a subagent's own `/clear` is answered by the thread that owns it.
//!
//! # What this needs
//!
//! The vocabulary GGUF (the harness renders its prefix) and a process-lifetime tree for the
//! background job — each gate says so and skips. **No model**: every turn is answered by a stub
//! on the shared `canned` frames, held on a gate so the child is mid-turn at the clear.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::harness::JobFate;
use letibot_harnessd::jobwatch::JobWatchSink;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_tokencore::Vocab;
use letibot_tokencore::store::{Store, TodoBy, TodoItem, TodoStatus};
use letibot_tools::NullToolSink;
use letibot_transcript::{Speaker, ToolCall, TranscriptItem, UserPart};
use letibot_turn::Endpoint;

#[path = "../../turn/tests/support/canned.rs"]
mod canned;

use canned::Frame;

const IM_END: u32 = 248046;
const THINK_OPEN: u32 = 248068;
const THINK_CLOSE: u32 = 248069;

const PATIENCE: Duration = Duration::from_secs(30);

/// The child's brief — what the orientation must say it was asked.
const BRIEF: &str = "audit the parser's error paths";
/// The operator's sentence, given with the clear.
const NEXT: &str = "finish the parser, then run the gate";

fn config(session_id: &str, endpoint: Endpoint, store: &Path) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    // A seat that can run commands and start children: unconfined (`leticode`) with `bash`
    // seated — `operator_shell.rs`'s fixture, because the job here is a real process.
    cfg.seat = Seat::Leticode;
    cfg.allow_bash = true;
    cfg.endpoint = endpoint;
    cfg.http_retries = 0;
    // A child is planned against a window; see `message_between_turns.rs`.
    cfg.context_window = Some(32_768);
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg
}

fn parts_for(cfg: &Config) -> Parts {
    let p = Parts::load(cfg).expect("the vocabulary must load");
    *p.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    p
}

fn wiring(cfg: &Config) -> SessionWiring {
    SessionWiring {
        model: cfg.model.clone(),
        dialect: "qwen".into(),
        endpoint: "http://127.0.0.1:1".into(),
        workspace: cfg.workspace.display().to_string(),
    }
}

/// **A parent session with the REAL runner**, its hub adopted into a registry the way a daemon's
/// is, and a teardown that drains the tree (see `message_between_turns.rs`'s `Parent` for why
/// the drain is a crash fix rather than tidiness).
struct Parent {
    harness: Harness,
    registry: Arc<Registry>,
    gate: Arc<Stub>,
}

fn a_parent(session_id: &str, parts: &Parts, cfg: &Config, gate: Arc<Stub>) -> Parent {
    let registry = Registry::new();
    let hub = registry.new_hub(session_id.to_string());
    registry
        .adopt(hub.clone(), session_id.to_string(), wiring(cfg), None)
        .expect("the parent is in the registry");
    let harness =
        Harness::open_with_registry(parts, cfg.clone(), hub, None, None, registry.clone())
            .expect("the parent opens");
    Parent {
        harness,
        registry,
        gate,
    }
}

impl Parent {
    /// **One tool call through the session's own runtime, WATCHED** — the sink a turn wraps its
    /// tools in (`JobWatchSink`), so a `Backgrounded` outcome is armed for delivery exactly as a
    /// model's call would arm it.
    fn call(&self, name: &str, args: serde_json::Value) -> letibot_tools::ToolResult {
        let rt = self.harness.runtime_handle();
        let call = ToolCall {
            id: format!("call_{name}"),
            name: name.to_string(),
            arguments: args.to_string(),
        };
        let mut sink = JobWatchSink::new(NullToolSink, self.harness.job_watchers());
        rt.invoke("t1", &call, &mut sink)
    }

    fn backgrounded(&self, name: &str, args: serde_json::Value) -> String {
        let r = self.call(name, args);
        match r.outcome {
            letibot_transcript::ToolOutcome::Backgrounded { handle, .. } => handle,
            other => panic!("`{name}` did not background: {other:?} — {}", r.payload),
        }
    }

    fn hub_of(&self, handle: &str) -> Arc<Hub> {
        let began = Instant::now();
        loop {
            if let Some(hub) = self.registry.get(handle) {
                return hub;
            }
            assert!(
                began.elapsed() < PATIENCE,
                "`{handle}` never entered the registry"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Parent {
    fn drop(&mut self) {
        self.gate.release(usize::MAX);
        self.registry.close();
        if !self.harness.wait_for_children(PATIENCE) && !std::thread::panicking() {
            panic!("the tree still had a subagent thread after {PATIENCE:?}");
        }
    }
}

/// The text of a user item, or `None` for any other kind.
fn user_text(item: &TranscriptItem) -> Option<(Speaker, String)> {
    match item {
        TranscriptItem::User { speaker, parts } => Some((
            *speaker,
            parts
                .iter()
                .filter_map(|p| match p {
                    UserPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )),
        _ => None,
    }
}

/// **Whether the model's answer has come back on this hub** — the reply, and not the prompt that
/// asked for it: a settlement is owed when a child has FINISHED, and a turn that has been asked is
/// not a turn that has answered.
fn answered(hub: &Hub, needle: &str) -> bool {
    hub.retained().iter().any(|e| match &e.event {
        SessionEvent::TranscriptContent { item, .. } => {
            matches!(&**item, TranscriptItem::Assistant { text, .. } if text.contains(needle))
        }
        _ => false,
    })
}

fn warnings(hub: &Hub) -> Vec<(String, String)> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning { code, detail, .. } => Some((code.clone(), detail.clone())),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// THE ONE THIS CHANGE EXISTS FOR
// ---------------------------------------------------------------------------

#[test]
fn a_cleared_session_keeps_its_board_its_job_and_its_child() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let dir = TempDir::new("harnessd-clear");
    let path = dir.path().join("sessions.db");
    let go = dir.path().join("go");
    let session_id = "s-clear-parent";

    let parts = parts_for(&config(session_id, Endpoint::new("127.0.0.1", 1), &path));
    let v = &parts.vocab;
    // Requests, in the only order they can arrive: #0 the parent's own turn (history to clear),
    // #1 the child's first turn — HELD until after the clear, so the child is mid-turn at it —
    // then the wakes that deliver the two settlements, answered plainly.
    let scripts = vec![
        a_plain_answer_turn(
            v,
            "reading",
            "the old conversation's answer, full of drift",
            30,
        ),
        a_plain_answer_turn(v, "auditing", "the child's audit is done", 30),
        a_plain_answer_turn(v, "noting", "noted the settlement", 30),
        a_plain_answer_turn(v, "noting", "noted the settlement", 30),
        a_plain_answer_turn(v, "noting", "noted the settlement", 30),
    ];
    let stub = Arc::new(Stub::held(scripts, 5));
    stub.release(1);

    let cfg = config(session_id, stub.endpoint.clone(), &path);
    let mut parent = a_parent(session_id, &parts, &cfg, stub.clone());

    // **A history worth clearing.**
    parent
        .harness
        .submit("start on the parser")
        .expect("the first turn runs");
    let old_id = parent.harness.transcript_id().to_string();
    let old_items = parent.harness.items().len();
    assert!(old_items >= 2, "a prompt and its answer");

    // **The board: one row of each author.**
    parent.harness.set_operator_todos(
        vec![TodoItem {
            content: "review the parser before it lands".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        }],
        &[],
    );
    let r = parent.call(
        "todo_write",
        serde_json::json!({"todos": [
            {"content": "write the parser", "status": "in_progress"},
            {"content": "run the gate", "status": "pending"}
        ]}),
    );
    assert_eq!(
        r.outcome,
        letibot_transcript::ToolOutcome::Ok,
        "{}",
        r.payload
    );
    let board = parent.harness.todo_list();
    assert_eq!(board.len(), 3, "{board:#?}");

    // **A background job that cannot finish until the test says so** — so its settlement lands
    // after the clear, never before it.
    let job = parent.backgrounded(
        "bash",
        serde_json::json!({
            "command": format!(
                "while [ ! -f {} ]; do sleep 0.05; done; echo JOB-FINISHED",
                go.display()
            ),
            "background": true
        }),
    );
    // **A child, mid-turn**: its first request is held by the stub.
    let child = parent.backgrounded("task", serde_json::json!({"prompt": BRIEF}));
    let child_hub = parent.hub_of(&child);
    assert!(
        stub.wait_for_requests(2),
        "the child's turn reached the model"
    );

    // ---- THE CLEAR ----
    let r = parent
        .harness
        .clear_conversation(Some(NEXT))
        .expect("the clear lands");

    // (1) New and short: message zero plus the orientation, nothing else.
    assert_eq!(parent.harness.transcript_id(), format!("{session_id}#t1"));
    assert_eq!(r.fork.parent_id, old_id);
    let items = parent.harness.items().to_vec();
    assert_eq!(items.len(), 1, "message zero plus ONE item: {items:#?}");
    let (speaker, said) = user_text(&items[0]).expect("the orientation is a user item");
    assert_eq!(
        speaker,
        Speaker::Agent,
        "the harness speaks the orientation, never the operator"
    );
    assert_eq!(said, r.orientation);
    for must in [
        "[cleared]",
        old_id.as_str(),
        NEXT,
        "write the parser",
        "run the gate",
        "review the parser before it lands",
        job.as_str(),
        child.as_str(),
        BRIEF,
    ] {
        assert!(
            said.contains(must),
            "the orientation lacks `{must}`:\n{said}"
        );
    }
    assert!(
        !said.contains("drift"),
        "nothing of the old conversation's prose is carried:\n{said}"
    );
    eprintln!("--- the orientation ---\n{said}\n---");

    // (2) The board is identical.
    assert_eq!(parent.harness.todo_list(), board);

    // (5) The old transcript is still whole in the store.
    let store = Store::open(&path).expect("the store reopens");
    let old = store
        .load_transcript(&old_id)
        .expect("the old transcript loads");
    assert_eq!(old.items.len(), old_items, "kept whole");
    assert!(
        old.items
            .iter()
            .filter_map(|(item, _, _)| user_text(item))
            .any(|(_, t)| t.contains("start on the parser")),
        "with its history in it"
    );

    // (6) The count is the new transcript's, unmeasured.
    let resident = parent.harness.measure_resident();
    assert_eq!(resident.measured, None, "no measurement survives a clear");
    assert_eq!(
        resident.tokens,
        parent.harness.ledger_len() as u64,
        "the count is the new base, not the old conversation"
    );
    let row = store.session(session_id).unwrap().expect("the session row");
    assert_eq!(
        row.context_tokens, None,
        "the stored count was about the old conversation"
    );

    // ---- AFTER THE CLEAR ----
    // The child answers, the job finishes, and every wake after them is answered.
    stub.release(usize::MAX);
    std::fs::write(&go, b"").expect("the job's go file");

    // (3) and (4): both settlements reach the NEW conversation. **Under R79 a routine result is
    // HELD** — nothing on this board waits on the job or the child, so neither may start a turn of
    // its own (`Harness::wake` answers `Ok(None)`) — and it rides in with the next turn the model
    // takes anyway, as one `[meanwhile]` batch. So the loop wakes (which delivers anything that IS
    // immediate) and, once both have SETTLED, takes the one turn a person would: a prompt. What is
    // asserted is unchanged — the settlements land in the cleared conversation.
    //
    // **"Once both have settled" is read off the settlements, not off a stopwatch.** It used to be
    // two seconds after the releases, and that was a guess about the box: on a loaded one the job's
    // own poll and the child's turn both outlast it, so the prompt fired with a settlement still in
    // flight — its `[meanwhile]` then carried only the half that had arrived, and the loop below sat
    // out its whole patience waiting for a turn nothing was going to take. The two facts are the
    // settlement's own: the job is over, and the child's answer is on the child's log. Neither can
    // become true early, so a busy box makes this wait longer and never makes it wrong.
    let began = Instant::now();
    let mut prompted = false;
    loop {
        let _ = parent.harness.wake().expect("the wake runs");
        if !prompted
            && matches!(parent.harness.job_fate(&job), JobFate::Ended { .. })
            && answered(&child_hub, "the child's audit is done")
        {
            parent
                .harness
                .submit("carry on")
                .expect("the prompt's turn runs");
            prompted = true;
        }
        let texts: Vec<String> = parent
            .harness
            .items()
            .iter()
            .filter_map(user_text)
            .map(|(_, t)| t)
            .collect();
        // Past the orientation, which names both handles itself: what is looked for is the
        // SETTLEMENT — the job's output and the child's answer, which nothing before the clear
        // could have known.
        let later = || texts.iter().skip(1);
        // R93.C3: the completion names the job and how it ended; its own command is not echoed.
        let job_in = later().any(|t| t.contains(&format!("`{job}` exited 0")));
        let child_in =
            later().any(|t| t.contains(&child) && t.contains("the child's audit is done"));
        if job_in && child_in {
            eprintln!(
                "--- delivered ---\n{}\n---",
                later().cloned().collect::<Vec<_>>().join("\n---\n")
            );
            break;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "the settlements never reached the new conversation: {texts:#?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        parent.harness.transcript_id(),
        format!("{session_id}#t1"),
        "delivered into the cleared conversation, not a new one"
    );
    // The child is still this session's.
    assert_eq!(
        parent.registry.parent_of(&child).as_deref(),
        Some(session_id),
        "the child's parent is the session, not the transcript"
    );
    drop(child_hub);
}

/// **A subagent's own `/clear` is answered by the thread that owns it** — with its board kept,
/// its transcript forked, and the boundary row on its own log; and the old one in the store.
#[test]
fn a_childs_clear_is_answered_by_its_own_thread() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-clear-child");
    let path = dir.path().join("sessions.db");
    let session_id = "s-clear-child-parent";
    let parts = parts_for(&config(session_id, Endpoint::new("127.0.0.1", 1), &path));
    let v = &parts.vocab;
    let stub = Arc::new(Stub::answering(
        vec![a_plain_answer_turn(v, "auditing", "audited", 30)],
        1,
    ));
    let cfg = config(session_id, stub.endpoint.clone(), &path);
    let parent = a_parent(session_id, &parts, &cfg, stub.clone());
    let child = parent.backgrounded("task", serde_json::json!({"prompt": BRIEF}));
    let hub = parent.hub_of(&child);
    // Between turns: its answer is in.
    let began = Instant::now();
    while parent
        .call("task_result", serde_json::json!({"task": child}))
        .outcome
        != letibot_transcript::ToolOutcome::Ok
    {
        assert!(began.elapsed() < PATIENCE, "the child never answered");
        std::thread::sleep(Duration::from_millis(20));
    }

    let head = hub.attach(
        "tui",
        "dead",
        letibot_sessionlog::protocol::Caps::default(),
        0,
    );
    hub.submit(
        &head.head_id,
        "r-clear",
        0,
        letibot_sessionlog::hub::CommandKind::Slash {
            line: "clear".into(),
        },
    );
    hub.wake_its_own_reader();

    let began = Instant::now();
    let cleared = loop {
        if let Some((_, d)) = warnings(&hub).into_iter().find(|(c, _)| c == "cleared") {
            break d;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "the child never answered its clear: {:#?}",
            warnings(&hub)
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(cleared.contains(&format!("{child}#t1")), "{cleared}");
    assert!(
        !warnings(&hub)
            .iter()
            .any(|(c, _)| c == "child_door_refused"),
        "a clear is the one slash verb a child answers itself"
    );
    let store = Store::open(&path).expect("the store reopens");
    let fork = store
        .load_transcript(&format!("{child}#t1"))
        .expect("the child's new transcript");
    assert_eq!(fork.items.len(), 1, "message zero plus the orientation");
    let old = store
        .load_transcript(&format!("{child}#t0"))
        .expect("the child's old transcript");
    assert!(old.items.len() >= 2, "the brief and the answer, kept");
}

// ---------------------------------------------------------------------------
// The stub (`message_between_turns.rs`'s, which holds a request until released)
// ---------------------------------------------------------------------------

/// **A model that answers on a gate, and keeps every request it was asked.**
///
/// `canned::Canned` answers as fast as it can, which closes the window this file has to
/// reach into: a turn is mid-flight only for as long as its model takes to answer. So each
/// request is read, RECORDED, and held until the test releases it — *"the child is in a
/// turn"* becomes a state the test stands in rather than races. The recorded body is what
/// makes the other half provable: a request carries token ids, and detokenized with the
/// same vocabulary the child tokenized with, they are the prompt the model was handed.
struct Stub {
    endpoint: Endpoint,
    shared: Arc<Gate>,
    handle: Option<std::thread::JoinHandle<()>>,
}

struct Gate {
    inner: Mutex<Inner>,
    cv: Condvar,
}

#[derive(Default)]
struct Inner {
    /// Every request's body, in arrival order.
    bodies: Vec<String>,
    /// How many requests may be answered. A request waits until this passes its index.
    released: usize,
}

impl Stub {
    /// One frame list per request, answered as soon as the request arrives.
    fn answering(scripts: Vec<Vec<Frame>>, requests: usize) -> Stub {
        let stub = Stub::held(scripts, requests);
        stub.release(requests);
        stub
    }

    /// The same, and nothing is answered until [`Stub::release`] says so.
    fn held(scripts: Vec<Vec<Frame>>, requests: usize) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
        let addr = listener.local_addr().unwrap();
        let shared = Arc::new(Gate {
            inner: Mutex::new(Inner::default()),
            cv: Condvar::new(),
        });
        let mine = shared.clone();
        let handle = std::thread::spawn(move || {
            let mut i = 0usize;
            while i < requests {
                // A listener that cannot accept is a listener nobody will be answered by:
                // there is nothing left to say, and the client's own error is the report.
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let (line, body) = read_request(stream.try_clone().expect("clone the socket"));
                // **`/props` is not a turn and does not consume a script.** A spawn asks the
                // parent's own endpoint for its window (`HarnessTaskRunner::start`'s
                // `served_ctx`, one `GET` per child while this session has never left
                // local) — a fact about the spawn path rather than about the model, and
                // counting it as a turn would shift every script by one.
                if line.starts_with("GET ") {
                    let _ = write_props(stream);
                    continue;
                }
                {
                    let mut g = mine.inner.lock().unwrap();
                    g.bodies.push(body);
                    mine.cv.notify_all();
                    while g.released <= i {
                        g = mine.cv.wait(g).unwrap();
                    }
                }
                let _ = write_frames(stream, &scripts[i % scripts.len()]);
                i += 1;
            }
        });
        Stub {
            endpoint: Endpoint::new(addr.ip().to_string(), addr.port()),
            shared,
            handle: Some(handle),
        }
    }

    fn bodies(&self) -> Vec<String> {
        self.shared.inner.lock().unwrap().bodies.clone()
    }

    /// Wait until at least `n` requests have arrived. `false` is the timeout, so a caller
    /// can say what it was waiting for rather than hang.
    fn wait_for_requests(&self, n: usize) -> bool {
        let began = Instant::now();
        let mut g = self.shared.inner.lock().unwrap();
        while g.bodies.len() < n {
            if began.elapsed() > PATIENCE {
                return false;
            }
            let (guard, _) = self
                .shared
                .cv
                .wait_timeout(g, Duration::from_millis(20))
                .unwrap();
            g = guard;
        }
        true
    }

    /// Let the first `n` requests be answered.
    fn release(&self, n: usize) {
        let mut g = self.shared.inner.lock().unwrap();
        g.released = g.released.max(n);
        self.shared.cv.notify_all();
    }

    /// **What the model was handed for request `n`** — the token ids, detokenized with the
    /// vocabulary the child tokenized with.
    fn prompt_of(&self, n: usize, vocab: &Vocab) -> String {
        let body = self.bodies().get(n).cloned().unwrap_or_default();
        let json: serde_json::Value =
            serde_json::from_str(&body).unwrap_or_else(|e| panic!("request {n} is JSON: {e}"));
        let ids: Vec<u32> = json["prompt"]
            .as_array()
            .unwrap_or_else(|| panic!("request {n} carries a prompt: {body}"))
            .iter()
            .map(|v| v.as_u64().expect("a token id") as u32)
            .collect();
        vocab
            .detokenize(&ids, true)
            .unwrap_or_else(|e| panic!("request {n}'s ids decode: {e}"))
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        // Release anything still held, then detach: a test that failed early must not leave
        // a thread parked on the gate for the rest of the suite.
        self.release(usize::MAX);
        if let Some(h) = self.handle.take() {
            drop(h);
        }
    }
}

/// Drain one request — headers plus the body `Content-Length` names — so the client's write
/// side is finished before the answer arrives. `model_switch_at_turn_end.rs`'s reader, and it
/// hands back the request line as well so the caller can tell a `/props` probe from a turn.
fn read_request(mut s: TcpStream) -> (String, String) {
    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    let mut head_end = None;
    let mut want = 0usize;
    loop {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => acc.extend_from_slice(&buf[..n]),
        }
        if head_end.is_none()
            && let Some(at) = acc.windows(4).position(|w| w == b"\r\n\r\n")
        {
            head_end = Some(at);
            want = acc[..at]
                .split(|b| *b == b'\n')
                .find_map(|line| {
                    let line = String::from_utf8_lossy(line);
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
        }
        if let Some(at) = head_end
            && acc.len() - (at + 4) >= want
        {
            break;
        }
        if acc.len() > (1 << 22) {
            break;
        }
    }
    let text = String::from_utf8_lossy(&acc).into_owned();
    match head_end {
        Some(at) => (
            text[..at].lines().next().unwrap_or("").trim().to_string(),
            text[at + 4..].to_string(),
        ),
        None => (String::new(), text),
    }
}

/// **The endpoint's own props**, as `served_ctx` reads them: the one field a spawn asks for
/// is the window, and a server that answers nothing gets `None`, which is no wall at all.
fn write_props(mut stream: TcpStream) -> std::io::Result<()> {
    const BODY: &str = "{\"default_generation_settings\":{\"n_ctx\":32768}}";
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{BODY}",
        BODY.len()
    )?;
    stream.flush()
}

fn write_frames(mut stream: TcpStream, frames: &[Frame]) -> std::io::Result<()> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
          Transfer-Encoding: chunked\r\n\r\n",
    )?;
    stream.write_all(&canned::sse_body(frames))?;
    stream.flush()
}

// ---------------------------------------------------------------------------
// The turns the stub replays
// ---------------------------------------------------------------------------

fn ids(vocab: &Vocab, text: &str) -> Vec<u32> {
    vocab.tokenize_text(text).expect("text tokenizes")
}

fn spoken(vocab: &Vocab, ids: &[u32]) -> Vec<Frame> {
    ids.iter()
        .map(|id| Frame::Token {
            id: *id,
            text: vocab
                .detokenize(&[*id], true)
                .expect("one token decodes")
                .leak(),
        })
        .collect()
}

/// A complete turn that answers in plain text: thinks, closes the block, answers, ends.
fn a_plain_answer_turn(vocab: &Vocab, thought: &str, answer: &str, n_prompt: u64) -> Vec<Frame> {
    let thought_ids = ids(vocab, thought);
    let answer_ids = ids(vocab, answer);
    let mut frames = vec![Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.extend(spoken(vocab, &thought_ids));
    frames.extend(spoken(vocab, &[THINK_CLOSE]));
    frames.extend(spoken(vocab, &answer_ids));
    frames.extend(spoken(vocab, &[IM_END]));
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: (thought_ids.len() + answer_ids.len() + 2) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

/// A directory that removes itself, because a test that leaks a store per run eventually
/// fills the disk and the run that finds out is not this one.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos(),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        std::fs::create_dir_all(&path).expect("creating the temp dir");
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// The path as `read` takes it: relative to the session root, which is `/tmp`.
    fn relative(&self) -> String {
        self.path
            .file_name()
            .expect("a temp dir has a name")
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
