//! The turn an operator's `!` line starts - the end-to-end test of
//! `Harness::run_after_operator_shell`, one of the two call sites that run a turn.
//!
//! The two call sites differ in what they put in front of the model, and that difference is this
//! file's assertion: `Harness::submit_item` appends the row its turn is about, and
//! `Harness::run_after_operator_shell` appends nothing - the rows are already on the log, so all
//! it adds is the turn, and it opens the turn itself. So the `!` line is on the log exactly once
//! and the second prompt carries a head the first never had.
//!
//! It drives the daemon's own arm rather than a unit on the harness: a real `Sessions`, a
//! `CommandKind::OperatorShellResult` submitted to the session's hub exactly as the worker submits
//! it, and `Sessions::dispatch`. The run's half is fabricated (a `ToolOutcome` and a payload)
//! because what arrives at this arm is the run's pieces off the wire, already rendered and capped
//! the turn's half is real - the real round loop, a real transcript, a real provider request - and
//! the model is a recording stub, because *did the turn hand the model the line and the output*
//! is a question about the request and not about the transcript.
//!
//! It needs the vocabulary GGUF and not a process tree: the seat has no `bash`, so no delegated
//! cgroup is needed and the test runs everywhere rather than in a skip branch.

use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::sessions::{Outcome, Sessions};
use letibot_harnessd::{Dialect, Parts};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::{CommandKind, DAEMON_SUBMITTER, Hub, QueuedCommand};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_tokencore::Vocab;
use letibot_transcript::{CallOrigin, Speaker, ToolOutcome, TranscriptItem, UserPart};
use letibot_turn::Endpoint;

mod support;

use support::canned::Frame;
use support::qwen::a_plain_answer_turn;
use support::stub::{read_request, write_frames, write_props};

/// The prompt the first turn is about, the operator's `!` line, and the bytes their command
/// produced - three markers, so a row and a prompt can each be traced to the turn that made it.
const THE_PROMPT: &str = "prompt-marker-what-is-in-the-tree";
const THE_LINE: &str = "! cat bang-marker-output";
const THE_OUTPUT: &str = "bang-marker-output-the-command-really-ran";

/// What the model answers each turn - two different sentences, so the test can say WHICH turn an
/// answer came from.
const TO_THE_PROMPT: &str = "answering the prompt";
const TO_THE_BANG: &str = "answering the command";

fn config(session_id: &str, endpoint: Endpoint, store: &std::path::Path) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    // `leticode`, and no `bash`. This file's turn is answered, not run: the seat is one with no
    // shell so the fixture needs no exec host, and the run's half arrives as the pieces the worker
    // dispatches.
    //
    // `leticode` and not `coder` - the same tools-minus-bash, and the one of the two that needs no
    // boundary: `coder` is a confined seat, so on a Mac it refuses to open and this file would
    // measure that refusal instead of the turn after an operator shell.
    cfg.seat = Seat::Leticode;
    cfg.endpoint = endpoint;
    // No retry ladder: nothing here should fail, and a failure that retried would spend the
    // suite's time hiding behind doubling waits.
    cfg.http_retries = 0;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    // The operator's own `providers.toml` is not this test's business: without these, a search key
    // on the developer's laptop decides what is seated.
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.vocab_gguf = support::vocab::gguf();
    cfg
}

fn parts_for(cfg: &Config) -> Parts {
    let p = Parts::load(cfg).expect("the vocabulary must load");
    // And the operator's per-project mode is not this test's business either.
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

// ---------------------------------------------------------------------------
// The stub
// ---------------------------------------------------------------------------

/// A model that answers the turn, and keeps the request it was asked.
///
/// One frame list per turn, answered as soon as the request has been read - and the body recorded
/// first, because the prompt is what this file has to read.
struct Stub {
    endpoint: Endpoint,
    seen: Arc<Mutex<Vec<String>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Stub {
    fn answering(scripts: Vec<Vec<Frame>>) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mine = seen.clone();
        let handle = std::thread::spawn(move || {
            let mut turns = 0usize;
            while turns < scripts.len() {
                // A listener that cannot accept is a listener nobody will be answered by: there
                // is nothing left to say, and the client's own error is the report.
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let (line, body) = read_request(stream.try_clone().expect("clone the socket"));
                // `/props` is not a turn. A session asking its endpoint for the window sends a
                // `GET`, and counting it as a turn would shift every script by one.
                if line.starts_with("GET ") {
                    let _ = write_props(stream);
                    continue;
                }
                mine.lock().unwrap().push(body);
                let _ = write_frames(stream, &scripts[turns]);
                turns += 1;
            }
        });
        Stub {
            endpoint: Endpoint::new(addr.ip().to_string(), addr.port()),
            seen,
            handle: Some(handle),
        }
    }

    /// How many turns have been asked for. The two doors this file drives are both synchronous -
    /// each returns only once its turn is answered - so this needs no waiting; it is here so a
    /// failure says which turn never arrived.
    fn turns(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    /// What the model was handed for turn `n` - the request's token ids, detokenized with the
    /// vocabulary the session tokenized with.
    fn prompt_of(&self, n: usize, vocab: &Vocab) -> String {
        let body = self
            .seen
            .lock()
            .unwrap()
            .get(n)
            .cloned()
            .unwrap_or_else(|| panic!("turn {n} was never asked for (turns: {})", self.turns()));
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
        // Detach rather than join: a test that failed early may have left a turn unanswered, and
        // blocking here would hang the suite on the case it was written to check.
        if let Some(h) = self.handle.take() {
            drop(h);
        }
    }
}

/// Every transcript row on the session's own log, in order.
fn rows(hub: &Hub) -> Vec<TranscriptItem> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::TranscriptContent { item, .. } => Some((**item).clone()),
            _ => None,
        })
        .collect()
}

/// The `User` rows of one speaker, as their text - the operator's own words, and the session's.
fn said_by(log: &[TranscriptItem], who: Speaker) -> Vec<String> {
    log.iter()
        .filter_map(|i| match i {
            TranscriptItem::User { speaker, parts } if *speaker == who => Some(
                parts
                    .iter()
                    .filter_map(|p| match p {
                        UserPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .collect()
}

/// The model's own answers, which is how a turn that RAN is told from one that was merely opened.
fn answers(log: &[TranscriptItem]) -> Vec<String> {
    log.iter()
        .filter_map(|i| match i {
            TranscriptItem::Assistant { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// A directory that removes itself, because a test that leaks a store per run eventually fills the
/// disk.
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
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ---------------------------------------------------------------------------
// THE TEST
// ---------------------------------------------------------------------------

/// The `!` line's turn reaches the model with the operator's line and their command's output in
/// it, and the answer lands - and the two call sites differ in exactly one way, which this
/// asserts.
///
/// The daemon arm is the real one: `Sessions::dispatch` taking an `OperatorShellResult` off the
/// session's queue, which is the shape the run's own thread sends back. What is fabricated is the
/// run's *pieces* (an outcome and a payload), because the run itself is `operator_shell.rs`'s test
/// and this is the turn its rows are for.
#[test]
fn the_turn_after_an_operator_shell_reads_the_line_and_the_output() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-bang-turn");
    let store = dir.path().join("sessions.db");
    let id = "s-bang-turn";

    // The vocabulary first - the canned frames must speak the ids the session will send - and one
    // frame list per turn: the prompt's, then the `!` line's.
    let parts = parts_for(&config(id, Endpoint::new("127.0.0.1", 1), &store));
    let first = a_plain_answer_turn(&parts.vocab, "reading the prompt", TO_THE_PROMPT, 30);
    let second = a_plain_answer_turn(&parts.vocab, "reading the command", TO_THE_BANG, 30);
    let stub = Stub::answering(vec![first, second]);

    let cfg = config(id, stub.endpoint.clone(), &store);
    let registry = Registry::new();
    registry
        .create(id, "a session", wiring(&cfg))
        .expect("the session is created");
    let mut sessions =
        Sessions::open_first(&parts, cfg, registry.clone()).expect("the daemon opens");

    // ---- The other call site, for the contrast ---------------------------------
    //
    // A prompt goes through `submit_item`: the row is appended, and the turn that follows reads it
    // as its head.
    sessions.submit(id, THE_PROMPT).expect("the prompt answers");

    // ---- The call site this file exists for ------------------------------------
    //
    // The operator's run comes back as the command the worker dispatches - the same frame
    // `start_operator_run`'s settle path sends, through the same door (`Hub::submit_daemon`, which
    // is how a thread that is not a head hands the worker a command), carrying the run's rendered
    // pieces rather than the run.
    let hub = registry.get(id).expect("the session's hub");
    let queued = hub.submit_daemon(
        "dead",
        "bang-1",
        CommandKind::OperatorShellResult {
            line: THE_LINE.to_string(),
            who: "dead".to_string(),
            call_id: "bang-1".to_string(),
            outcome: ToolOutcome::Ok,
            payload: THE_OUTPUT.to_string(),
            spill: None,
        },
    );
    assert!(queued, "the run's result was not queued");
    let cmd: QueuedCommand = hub
        .try_command()
        .expect("the worker takes the result off the session's queue");
    assert!(
        cmd.head_id == DAEMON_SUBMITTER,
        "and it is the daemon's own submission: {}",
        cmd.head_id
    );
    let outcome = sessions.dispatch(id, &cmd);
    assert!(
        matches!(outcome, Outcome::Replied(_)),
        "the arm answered with the turn's own outcome, not a failure"
    );

    // ---- What the model was handed ---------------------------------------------
    //
    // The prompt is the assertion, and it is why the stub records requests: "the model reads
    // the operator's line and the output" is a claim about the bytes on the wire, and a test that
    // only read the transcript would pass on a build that opened an empty turn.
    assert_eq!(
        stub.turns(),
        2,
        "two turns: the prompt's, and the turn the `!` line started"
    );
    let asked = stub.prompt_of(1, &parts.vocab);
    assert!(
        asked.contains(THE_LINE),
        "the operator's own line is in the prompt the model was handed"
    );
    assert!(
        asked.contains(THE_OUTPUT),
        "and their command's output with it — this is what the turn exists for"
    );
    assert!(
        asked.contains(TO_THE_PROMPT),
        "and it is one conversation — the earlier turn's answer is still in front of it"
    );
    // The difference between the two call sites, stated where it can fail. The first turn's
    // prompt cannot hold the `!` line - the rows did not exist yet - and the second carries it
    // without any row having been appended for it.
    let first_asked = stub.prompt_of(0, &parts.vocab);
    assert!(first_asked.contains(THE_PROMPT), "the prompt's own head");
    assert!(
        !first_asked.contains(THE_LINE),
        "the `!` line is not in a prompt sent before it was run"
    );

    // ---- What landed on the log ------------------------------------------------
    let log = rows(&hub);
    let operator = said_by(&log, Speaker::Operator);
    assert!(
        operator.iter().any(|t| t == THE_PROMPT),
        "the prompt is the operator's own row: {operator:?}"
    );
    assert_eq!(
        operator.iter().filter(|t| *t == THE_LINE).count(),
        1,
        "**the `!` line is on the log exactly once** — `submit_item` appends the row \
         its turn is about and `run_after_operator_shell` appends nothing, because \
         `settle_operator_shell` already wrote this one: {operator:?}"
    );
    let run = log
        .iter()
        .find_map(|i| match i {
            TranscriptItem::ToolResult {
                name,
                payload,
                origin,
                ..
            } if name == "bash" => Some((payload.clone(), origin.clone())),
            _ => None,
        })
        .expect("the run's result is on the log");
    assert!(
        run.0.contains(THE_OUTPUT),
        "the result carries the command's output: {}",
        run.0
    );
    assert!(
        matches!(run.1, Some(CallOrigin::Operator { .. })),
        "and it is the operator's act, not the model's: {:?}",
        run.1
    );
    let said = answers(&log);
    assert!(
        said.iter().any(|a| a.contains(TO_THE_BANG)),
        "the turn the call site opened really ran, and its answer landed: {said:?}"
    );
    assert!(
        said.iter().any(|a| a.contains(TO_THE_PROMPT)),
        "and so did the prompt's: {said:?}"
    );
}
