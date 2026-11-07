//! Two daemons on one host, talking through the room: A posts a need to a tagged queue, B claims it and answers, and A is woken by the answer with nobody typing.
//!
//! Earlier room tests drive one session and a raw `Client`; only a second daemon can be wrong about two sessions finding each other. The shape is the shipped one: `Coordinator` (`harnessd --room`), a real `Daemon` per session with its own store, socket and workspace, and a canned model on loopback behind each.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::room::coordinator::Coordinator;
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_room::client::Client;
use letibot_room::{Request, Response, Speaker};
use letibot_sessionlog::hub::{Attached, CommandKind, Hub};
use letibot_sessionlog::protocol::Caps;
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::Vocab;
use letibot_transcript::{Speaker as Row, ToolOutcome, TranscriptItem, UserPart};
use letibot_turn::Endpoint;

mod support;

use support::canned::Frame;
use support::qwen::{THINK_CLOSE, a_plain_answer_turn, ids, spoken};
use support::stub::{read_request, write_frames, write_props};

/// Qwen's structural ids, the values `child_completion_wakes.rs` and the turn crate pin.
const THINK_OPEN: u32 = 248068;
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

/// The tag the two sessions meet on. A tag is a LOCK, so this is one resource with one head.
const TAG: &str = "answer-wanted";

/// A's need, as the model writes it into the item's body.
const ASK: &str = "which port does the suite want? answer on this tag, in a post";

/// B's answer, and a marker nothing else in this run can produce: the assertion that A received
/// the ANSWER is a search for this string in A's own transcript.
const ANSWER: &str = "zz-answer: the suite wants 4319";

/// The two prompts a person types, one into each session. Markers, so a stub can tell the turn it
/// was asked for from the turns the room starts.
const ASK_PROMPT: &str = "zz-ask: put the port question in the room";
const CLAIM_PROMPT: &str = "zz-claim: take the head of the queue and answer it";

/// Generous on purpose: nothing here is a performance claim, so this is only a bound on a hang.
const PATIENCE: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// The fixtures
// ---------------------------------------------------------------------------

/// A directory of this test's own, which removes itself - a leaked store per run fills a disk and
/// the run that finds out is never the one that leaked it.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch(tag);
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One daemon: its own registry, socket and store, its own session, and the worker thread that
/// serves it exactly as `cli::run` does.
struct One {
    id: String,
    registry: Arc<Registry>,
    /// `None` once the worker has been joined - the `Daemon` itself is moved onto that thread.
    worker: Option<std::thread::JoinHandle<()>>,
    /// The model behind this session. Held so the run owns it: the listener thread outlives this
    /// struct either way, but a reader should not have to work out who keeps the endpoint up.
    stub: Stub,
}

impl One {
    /// The session's own config: its own store and workspace, no provider (the turn talks to this test's endpoint), the `leticode` seat (unconfined, names `room`), and the room link asked for explicitly so a constructor that stopped setting it cannot make this file measure nothing.
    fn config(dir: &Path, id: &str) -> Config {
        // One project folder for both daemons: queues are per project room, and a folder is its
        // own project, so two sessions share a queue only by sharing a folder.
        let workspace = dir.parent().unwrap_or(dir).join("project");
        std::fs::create_dir_all(&workspace).expect("a workspace");
        let mut cfg = Config::for_this_box(&workspace);
        cfg.dialect = Dialect::Qwen;
        cfg.seat = Seat::Leticode;
        cfg.http_retries = 0;
        cfg.context_window = Some(32_768);
        cfg.store = Some(dir.join("sessions.db"));
        cfg.session_id = id.to_string();
        cfg.web_search = None;
        cfg.permission = Vec::new();
        cfg.provider = None;
        cfg.vocab_gguf = support::vocab::gguf();
        cfg.room_link = true;
        cfg
    }

    /// Open the daemon and hand it a worker thread. `script` is the model: what this session's
    /// model answers, decided by the prompt it is handed.
    fn start(dir: PathBuf, id: &str, script: fn(&Vocab, &str) -> Vec<Frame>) -> One {
        let mut cfg = One::config(&dir, id);
        let parts: &'static Parts = Box::leak(Box::new(
            Parts::load(&cfg).expect("the vocabulary must load"),
        ));
        // The parts' own `mode_store` is the operator's project mode; this is not its subject.
        *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
        // The model after the parts, because the script is tokenized with the vocabulary this
        // session was opened with: two vocabularies would make the ids mean nothing.
        let stub = Stub::serve(Arc::clone(&parts.vocab), script);
        cfg.endpoint = stub.endpoint.clone();

        let socket = letibot_sessionlog::testing::socket_path(&dir, "harnessd.sock");
        cfg.socket = socket.clone();
        let registry = Registry::new();
        registry
            .create(id.to_string(), "", Sessions::wiring(&cfg))
            .expect("the daemon's own first session");
        let daemon = Daemon::serve(registry.clone(), &socket).expect("the daemon's socket binds");

        let worker = {
            let registry = registry.clone();
            std::thread::Builder::new()
                .name(format!("daemon-{id}"))
                .spawn(move || {
                    let mut sessions =
                        Sessions::open_first(parts, cfg, registry).expect("the session opens");
                    daemon.run(&mut sessions, |_, _| {});
                    daemon.shutdown();
                    let _ = sessions.drain_subagents(Duration::from_secs(2));
                })
                .expect("the daemon's thread")
        };
        One {
            id: id.to_string(),
            registry,
            worker: Some(worker),
            stub,
        }
    }

    fn hub(&self) -> Arc<Hub> {
        self.registry
            .get(&self.id)
            .expect("the session is in its registry")
    }

    /// Say one thing to this session, the way a head does.
    fn prompt(&self, request: &str, text: &str) -> Attached {
        let hub = self.hub();
        let head = hub.attach("tui", "tester", Caps::default(), 0);
        let frame = hub.submit(
            &head.head_id,
            request,
            0,
            CommandKind::Prompt {
                text: text.into(),
                attachments: Vec::new(),
            },
        );
        assert!(
            !matches!(frame, letibot_sessionlog::ServerFrame::Rejected { .. }),
            "{request} was not accepted by the hub: {frame:?}"
        );
        head
    }

    /// Everything the session's transcript holds that is a tool result of `name`.
    fn tool_results(&self, name: &str) -> Vec<(ToolOutcome, String)> {
        rows(&self.hub())
            .into_iter()
            .filter_map(|item| match item {
                TranscriptItem::ToolResult {
                    name: n,
                    outcome,
                    payload,
                    ..
                } if n == name => Some((outcome, payload)),
                _ => None,
            })
            .collect()
    }

    /// The user rows: the operator's own words, and the harness's notices in its own voice.
    fn user_rows(&self) -> Vec<(Row, String)> {
        user_rows(&self.hub())
    }

    /// How many turns the model behind this session answered - the difference between a given
    /// session and a session the room woke.
    fn turns(&self) -> usize {
        self.stub.turns()
    }

    /// The transcript in the words a failing assertion needs: who said what, what each call
    /// answered, and what the model replied - enough to tell a turn that never ran from one that
    /// ran and read nothing.
    fn said(&self) -> String {
        let mut out = String::new();
        for item in rows(&self.hub()) {
            match item {
                TranscriptItem::User { speaker, parts } => {
                    let text = parts
                        .iter()
                        .filter_map(|p| match p {
                            UserPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    out.push_str(&format!("  [{speaker:?}] {text}\n"));
                }
                TranscriptItem::Assistant { text, .. } => {
                    out.push_str(&format!("  [assistant] {text}\n"));
                }
                TranscriptItem::ToolResult {
                    name,
                    outcome,
                    payload,
                    ..
                } => {
                    let first = payload.lines().next().unwrap_or("");
                    out.push_str(&format!("  [{name} {outcome:?}] {first}\n"));
                }
                other => out.push_str(&format!("  [{other:?}]\n")),
            }
        }
        out
    }
}

impl Drop for One {
    fn drop(&mut self) {
        // Closing the registry is what ends `Daemon::run`, so the worker thread is the thing this
        // joins - a daemon left running would hold a socket and a store past the test.
        self.registry.close();
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

fn rows(hub: &Arc<Hub>) -> Vec<TranscriptItem> {
    hub.snapshot()
        .items
        .into_iter()
        .filter_map(|r| r.item)
        .collect()
}

fn user_rows(hub: &Arc<Hub>) -> Vec<(Row, String)> {
    rows(hub)
        .into_iter()
        .filter_map(|item| match item {
            TranscriptItem::User { speaker, parts } => Some((
                speaker,
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
        })
        .collect()
}

/// Wait for `what`, or fail saying what never happened AND what was there instead: a room test's
/// failure is otherwise a timeout that reads like a slow box.
fn wait_until(what: &str, saw: impl Fn() -> String, cond: impl Fn() -> bool) {
    let began = Instant::now();
    while began.elapsed() < PATIENCE {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "waited {PATIENCE:?} for {what}. What the run had instead:\n{}",
        saw()
    );
}

// ---------------------------------------------------------------------------
// The models
// ---------------------------------------------------------------------------

/// One session's model on loopback: the frames are chosen from the PROMPT, not from a count,
/// because a room this session is a member of will ring it for events it did not ask for and a
/// counted script would answer those with the wrong turn.
struct Stub {
    endpoint: Endpoint,
    requests: Arc<AtomicUsize>,
}

impl Stub {
    fn serve(vocab: Arc<Vocab>, script: fn(&Vocab, &str) -> Vec<Frame>) -> Stub {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let mine = Arc::clone(&requests);
        std::thread::spawn(move || {
            loop {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                // One thread per connection: two sessions' turns are in flight at once here.
                let vocab = Arc::clone(&vocab);
                let seen = Arc::clone(&mine);
                std::thread::spawn(move || {
                    let (line, body) = read_request(stream.try_clone().expect("clone the socket"));
                    // A window probe is not a turn: a spawn asks the endpoint for `/props`.
                    if line.starts_with("GET ") {
                        let _ = write_props(stream);
                        return;
                    }
                    seen.fetch_add(1, Ordering::SeqCst);
                    let _ = write_frames(stream, &script(&vocab, &prompt_of(&vocab, &body)));
                });
            }
        });
        Stub {
            endpoint: Endpoint::new(addr.ip().to_string(), addr.port()),
            requests,
        }
    }

    fn turns(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

/// What the model was handed for one request - the token ids, detokenized with the vocabulary the
/// session tokenized with. A prompt is the one thing that says which turn this is.
fn prompt_of(vocab: &Vocab, body: &str) -> String {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return String::new();
    };
    let Some(ids) = json["prompt"].as_array() else {
        return String::new();
    };
    let ids: Vec<u32> = ids
        .iter()
        .filter_map(|v| v.as_u64().map(|n| n as u32))
        .collect();
    vocab.detokenize(&ids, true).unwrap_or_default()
}

/// A turn that calls `room` with these arguments and ends - the shape a ROUND takes, so the tool
/// result lands in the transcript and the round loop asks the model again.
fn a_room_call_turn(vocab: &Vocab, call: &[(&str, &str)], n_prompt: u64) -> Vec<Frame> {
    let mut said = String::from("\n<function=room>\n");
    for (name, value) in call {
        said.push_str(&format!("<parameter={name}>\n{value}\n</parameter>\n"));
    }
    said.push_str("</function>\n");
    let call_ids = ids(vocab, &said);
    let mut frames = vec![Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.push(Frame::Token {
        id: THINK_OPEN,
        text: "",
    });
    frames.push(Frame::Token {
        id: THINK_CLOSE,
        text: "",
    });
    frames.push(Frame::Token {
        id: TOOL_CALL_OPEN,
        text: "<tool_call>",
    });
    frames.extend(spoken(vocab, &call_ids));
    frames.push(Frame::Token {
        id: TOOL_CALL_CLOSE,
        text: "</tool_call>",
    });
    frames.push(Frame::Final {
        stop_type: "eos",
        // The two think fences and the two tool-call fences: the accumulator counts what was
        // streamed, so this has to agree with it exactly.
        n_decoded: (call_ids.len() + 4) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

/// Where `marker` last appears in the prompt, or `None`. A prompt is the WHOLE conversation, so
/// every question a script asks of it - *has this turn read the queue yet?* - is about which of two
/// things came last, never about whether a string is present.
fn last_at(asked: &str, marker: &str) -> Option<usize> {
    asked.rfind(marker)
}

/// A's model. It asks the question when the operator says to; after that every turn is one the
/// ROOM started, and it reads the queue rather than remembering what it posted.
fn a_script(vocab: &Vocab, asked: &str) -> Vec<Frame> {
    let heard = last_at(asked, "[room]");
    let read = last_at(asked, "first come first served");
    // The room spoke after the last queue read: this turn has not looked yet.
    if let Some(at) = heard
        && read.is_none_or(|r| r < at)
    {
        return a_room_call_turn(vocab, &[("verb", "queue"), ("tag", TAG)], 40);
    }
    if heard.is_some() {
        return a_plain_answer_turn(vocab, "read the queue", "the answer is on the tag", 40);
    }
    // The post's own result: this round has the tool's answer in it.
    if asked.contains("posted #") {
        return a_plain_answer_turn(vocab, "asked", "the question is in the room", 40);
    }
    a_room_call_turn(vocab, &[("verb", "post"), ("tag", TAG), ("body", ASK)], 40)
}

/// B's model: claim the head, put the answer on the same tag, finish the item. Each step keys on the
/// LAST of the sentences its own tool results produced, for [`last_at`]'s reason.
fn b_script(vocab: &Vocab, asked: &str) -> Vec<Frame> {
    let finished = last_at(asked, "is done and `answer-wanted` is free");
    let answered = last_at(asked, "posted #2");
    let holding = last_at(asked, "you hold #");
    let asked_to = last_at(asked, "zz-claim");
    let latest = [(finished, 0u8), (answered, 1), (holding, 2), (asked_to, 3)]
        .into_iter()
        .filter_map(|(at, step)| at.map(|at| (at, step)))
        .max();
    match latest.map(|(_, step)| step) {
        Some(0) => a_plain_answer_turn(vocab, "finished", "answered and closed", 40),
        Some(1) => a_room_call_turn(vocab, &[("verb", "done"), ("item", "1")], 40),
        Some(2) => a_room_call_turn(
            vocab,
            &[("verb", "post"), ("tag", TAG), ("body", ANSWER)],
            40,
        ),
        Some(_) => a_room_call_turn(vocab, &[("verb", "claim"), ("tag", TAG)], 40),
        None => a_plain_answer_turn(vocab, "nothing to do", "noted", 40),
    }
}

// ---------------------------------------------------------------------------
// Two daemons, one coordinator, one answer
// ---------------------------------------------------------------------------

#[test]
fn two_daemons_one_host_and_the_answer_arrives_with_nobody_asking() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("room-two-sessions");

    // Nothing of the operator's may reach these sessions: a provider key in their `providers.toml`
    // would take the turn to a real model, and a permission rule would decide what a tool may do.
    std::fs::create_dir_all(dir.path().join("config/letibot")).expect("a config dir");
    // SAFETY: this binary holds ONE test, and these are set before any daemon, thread or harness
    // reads the config they steer.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", dir.path().join("config"));
        std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("GLM_API_KEY");
        std::env::remove_var("LETIBOT_MODE");
        // The room's socket, on the door a fixture has - so no coordinator of the operator's, and
        // no socket of theirs, is in this test's path at all.
        std::env::set_var(
            letibot_harnessd::room::SOCKET_ENV,
            dir.path().join("room.sock"),
        );
    }

    // The coordinator, the shipped one: `harnessd --room` is this call plus a lease. Its bell rings
    // the arbiter's session for a question, and nothing in this run asks one - the tie-breaker has
    // no session to wake here, so the closure is empty rather than a second subject.
    let arbiter = letibot_harnessd::room::session_id();
    let coord = Coordinator::start(&arbiter, Arc::new(|_: &str| {})).expect("the box's room");
    let socket = coord.socket().to_path_buf();

    // The two daemons. Each has its own store, socket and workspace; both sit in one project
    // folder, so they share one project room and its queues.
    let a = One::start(dir.path().join("a"), "s-two-a", a_script);
    let b = One::start(dir.path().join("b"), "s-two-b", b_script);

    // ---- 1. A posts to a tagged queue, through the tool and a real turn --------------------
    let _head = a.prompt("req-ask", ASK_PROMPT);
    wait_until(
        "A's post to reach the room",
        || a.said(),
        || {
            a.tool_results("room").iter().any(|(outcome, payload)| {
                *outcome == ToolOutcome::Ok && payload.contains("posted #1")
            })
        },
    );

    // ---- 2. B claims the head and answers it ----------------------------------------------
    let _head = b.prompt("req-claim", CLAIM_PROMPT);
    wait_until(
        "B to claim #1",
        || b.said(),
        || {
            b.tool_results("room")
                .iter()
                .any(|(_, payload)| payload.contains("you hold #1"))
        },
    );
    wait_until(
        "B's answer to reach the room",
        || b.said(),
        || {
            b.tool_results("room")
                .iter()
                .any(|(_, payload)| payload.contains("posted #2"))
        },
    );
    // B closes the item behind the answer, which is what frees the tag - the state the last
    // section asserts, waited for here rather than assumed to have happened already.
    wait_until(
        "B to finish #1",
        || b.said(),
        || {
            b.tool_results("room")
                .iter()
                .any(|(_, payload)| payload.contains("#1 is done and `answer-wanted` is free"))
        },
    );

    // ---- 3. A is woken by it, and reads the answer off the room ---------------------------
    wait_until(
        "A to hear B's post",
        || a.said(),
        || {
            a.user_rows()
                .iter()
                .any(|(_, said)| said.contains("[room] `s-two-b` posted #2"))
        },
    );
    wait_until(
        "A to read the answer back off the room",
        || a.said(),
        || {
            a.tool_results("room")
                .iter()
                .any(|(outcome, payload)| *outcome == ToolOutcome::Ok && payload.contains(ANSWER))
        },
    );

    // The turn the answer arrived in was the HARNESS's, not a person's: A's transcript holds one
    // operator row - the prompt this test typed - and the room's sentence beside the harness's own
    // voice. That is the half a mailbox cannot do, and the reason the room is a bell.
    let heard = a.user_rows();
    let from_the_room: Vec<&(Row, String)> = heard
        .iter()
        .filter(|(_, said)| said.contains("[room] `s-two-b` posted #2"))
        .collect();
    assert_eq!(
        from_the_room.len(),
        1,
        "A must be told, once, that B posted under its tag: {heard:#?}"
    );
    assert_eq!(
        from_the_room[0].0,
        Row::Agent,
        "and in the harness's own voice, so a head does not draw it as the operator's words"
    );
    let operators = heard
        .iter()
        .filter(|(who, _)| *who == Row::Operator)
        .count();
    assert_eq!(
        operators, 1,
        "nobody typed into A after this test's one prompt — the second turn is the room's: {heard:#?}"
    );

    // And the turn RAN: the reply to the round that read the answer is in the transcript, after
    // the read. Ordered, not merely present - a previous turn's answer sits above it.
    let answered_after_reading = || {
        let items = rows(&a.hub());
        let Some(read_at) = items.iter().position(|i| {
            matches!(i, TranscriptItem::ToolResult { name, payload, .. }
                if name == "room" && payload.contains(ANSWER))
        }) else {
            return false;
        };
        items[read_at + 1..]
            .iter()
            .any(|i| matches!(i, TranscriptItem::Assistant { text, .. } if !text.trim().is_empty()))
    };
    wait_until(
        "A's model to answer the turn in which it read the answer",
        || a.said(),
        answered_after_reading,
    );
    assert!(
        a.turns() >= 2,
        "A's model answered more than the one prompt this test typed, so a turn nobody asked for \
         ran: {} request(s)",
        a.turns()
    );

    // ---- 4. the room's own state, read by a client that is neither session ----------------
    // In the sessions' project room: the queues it reads are that room's.
    let project = letibot_room::project_key(&dir.path().join("project").display().to_string());
    let observer = Client::connect_named(
        &socket,
        Speaker::top_level("s-two-observer"),
        letibot_room::room_name(&coord.server().name(), &project),
        Arc::new(|_: &letibot_room::Event| {}),
    )
    .expect("a third client reads the room");
    // The pane's read, because a queue read drops what is finished and this asserts the whole
    // history: A's need, and what became of it.
    let whole = observer.request(&Request::Room).expect("the room reads");
    let Response::Room { items, .. } = whole else {
        panic!("a room read answers the room: {whole:?}");
    };
    let first = items.iter().find(|r| r.id == 1).expect("A's item is here");
    assert_eq!(first.body, ASK, "item #1 is A's need, in A's words");
    assert_eq!(first.posted_by, "s-two-a", "posted by A");
    assert!(
        matches!(&first.state, letibot_room::State::Done { by, .. } if by == "s-two-b"),
        "B finished it, and the room says who did: {:?}",
        first.state
    );
    let second = items
        .iter()
        .find(|r| r.id == 2)
        .expect("B's answer is here");
    assert_eq!(second.posted_by, "s-two-b");
    assert_eq!(second.body, ANSWER, "the answer's words are on the tag");

    // And the tag is free: a finished item leaves the queue, so what is left under it is B's
    // answer and nothing else.
    let queue = observer
        .request(&Request::Queue {
            tag: TAG.to_string(),
        })
        .expect("the queue reads");
    let Response::Queue { rows, .. } = queue else {
        panic!("a queue read answers rows: {queue:?}");
    };
    assert!(
        rows.iter().all(|r| r.id != 1) && rows.iter().any(|r| r.id == 2),
        "`{TAG}` no longer holds the finished #1: {rows:?}"
    );
    drop(observer);
    drop(coord);
}
