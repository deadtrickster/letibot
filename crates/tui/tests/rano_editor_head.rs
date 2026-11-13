//! The editor head, driven by rano's own client against a real daemon: `docs/socket.md`'s subset.
//!
//! rano cannot link this crate, so its frames are a SECOND spelling of these types and a renamed
//! field reads as empty in its panel rather than as an error — which is why every assertion below
//! is a field's VALUE. It lives here because a head already links both, so no manifest moved.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::{self, AnswerSink, CommandKind, Hub};
use letibot_sessionlog::protocol::PROTOCOL_VERSION;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::{ServerHandle, serve_registry};
use letibot_sessionlog::testing;
use letibot_transcript::ToolOutcome;
use rano::letibot::{Client, Event, Frame, Reply, Seat};

/// The session the panel is opened on. One per test, because an id is a fact the daemon keeps.
const SESSION: &str = "s-editor";

/// A bound on a hang, not a performance claim: nothing here is measured except the wire.
const PATIENCE: Duration = Duration::from_secs(30);

const WORDS: &str = "why did the cache miss on tuesday?";
const THINKING: &str = "the key holds the dialect sha, so a template edit misses";
const PROSE: &str = "the cache key changed";
const REASON: &str = "the operator stopped it";
const SUMMARY: &str = "run `cargo test` in the workspace";
const TARGET: &str = "cargo test";

// ---------------------------------------------------------------------------
// The fixtures
// ---------------------------------------------------------------------------

/// A daemon of one session on its own socket, and what a head needs to reach it. Dropped whole,
/// because a leaked socket in the temp dir outlives the run that made it.
struct Daemon {
    hub: Arc<Hub>,
    dir: PathBuf,
    socket: PathBuf,
    _registry: Arc<Registry>,
    _server: ServerHandle,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn daemon(tag: &str) -> Daemon {
    let dir = testing::scratch(&format!("rano-editor-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let socket = testing::socket_path(&dir, "head.sock");
    let _ = std::fs::remove_file(&socket);
    let registry = Registry::new();
    let hub = registry
        .create(SESSION, "the article", SessionWiring::default())
        .expect("a session");
    let server = serve_registry(registry.clone(), &socket).expect("the socket binds");
    Daemon {
        hub,
        dir,
        socket,
        _registry: registry,
        _server: server,
    }
}

/// Where a decision would be answered from. A permission is settled by whoever is blocked on it,
/// and this is that thread's place: an answer or a cancellation that reaches the recorder is one a
/// person gave, not one that fell between the frame and the tool call.
#[derive(Default)]
struct Waiter {
    /// `(req_id, the head's identity, the option id it answered)`.
    answers: Mutex<Vec<(String, String, String)>>,
    cancelled: Mutex<Vec<String>>,
}

impl AnswerSink for Waiter {
    fn answer(&self, req_id: &str, identity: &str, reply: &hub::Reply) -> bool {
        let chosen = match reply {
            hub::Reply::Permission { option_id, .. } => option_id.clone(),
            hub::Reply::Question(a) => format!("{a:?}"),
        };
        self.answers
            .lock()
            .unwrap()
            .push((req_id.to_string(), identity.to_string(), chosen));
        true
    }

    fn cancel(&self, why: &str) {
        self.cancelled.lock().unwrap().push(why.to_string());
    }

    fn describe(&self) -> String {
        "the editor head's test".into()
    }
}

// ---------------------------------------------------------------------------
// Reading the wire
// ---------------------------------------------------------------------------

/// Frames until one the caller wants, cut short when the peer goes quiet, and the whole exchange
/// printed on a timeout: a conformance failure has to say which frame the daemon sent instead.
fn until(client: &Client, what: &str, want: impl Fn(&Frame) -> bool) -> Frame {
    let deadline = Instant::now() + PATIENCE;
    let mut seen: Vec<String> = Vec::new();
    while Instant::now() < deadline {
        let Some(frame) = client.frame(Duration::from_millis(200)) else {
            continue;
        };
        if want(&frame) {
            return frame;
        }
        seen.push(short(&frame));
    }
    panic!("no {what} within {PATIENCE:?}; what the daemon sent was: {seen:?}");
}

/// An event frame's payload, or a panic naming what arrived instead.
fn event_of(frame: &Frame) -> &Event {
    let Frame::Event { event, .. } = frame else {
        panic!("an event was expected, and this is: {frame:?}");
    };
    event
}

/// The frames seen are printed on a panic, and a `Hello` with a snapshot in it is tens of
/// kilobytes of rows that say nothing about the frame's tag.
fn short(frame: &Frame) -> String {
    let s = format!("{frame:?}");
    match s.char_indices().nth(120) {
        Some((at, _)) => format!("{}…", &s[..at]),
        None => s,
    }
}

/// Frames read until `done`, folding each into `reply` and handing it to `watch` — the shape a
/// panel's own loop has.
fn stream(
    client: &Client,
    reply: &mut Reply,
    mut watch: impl FnMut(&Frame),
    done: impl Fn(&Reply, &Frame) -> bool,
) {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        let Some(frame) = client.frame(Duration::from_millis(200)) else {
            continue;
        };
        reply.fold(&frame);
        watch(&frame);
        if done(reply, &frame) {
            return;
        }
    }
    panic!("the stream never reached its end; the reply so far is {reply:?}");
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// The row's bar, as one walk: attach, prompt, stream, answer a decision, interrupt. Every step
/// asserts the FIELDS, not only the tags: rano reads a missing field as its empty value, so a
/// renamed one is a blank in the panel and a tag-only assertion would not see it.
#[test]
fn the_editor_head_attaches_prompts_streams_answers_and_interrupts() {
    let daemon = daemon("subset");
    let waiter = Arc::new(Waiter::default());
    daemon.hub.set_answer_sink(waiter.clone());

    let mut head = Client::connect(&daemon.socket).expect("connect to the daemon's socket");
    head.attach(SESSION, 0, &Seat::default()).expect("attach");

    // 1. ATTACH. The panel reads its session, its own seat and the picker off this one frame.
    let Frame::Hello(hello) = until(&head, "Hello", |f| matches!(f, Frame::Hello(_))) else {
        unreachable!("the predicate matched")
    };
    assert_eq!(
        hello.protocol_version, PROTOCOL_VERSION,
        "the daemon states its own number and the panel compares it, so this is not a gate"
    );
    assert_eq!(
        hello.session_id, SESSION,
        "the session this connection is in"
    );
    assert!(
        !hello.head_id.is_empty(),
        "every request id the panel mints is prefixed with this"
    );
    assert!(
        hello.seq.is_some(),
        "a snapshot attach says the seq it is true as of, and the next event is seq + 1"
    );
    let row = hello
        .sessions
        .iter()
        .find(|s| s.id == SESSION)
        .expect("the picker is populated by the attach itself, and lists the session joined");
    assert_eq!(row.title, "the article", "a row is drawn by its title");
    assert!(row.live, "a session this daemon holds is live");
    assert!(row.created_ms > 0, "a picker orders by when it was made");

    // 2. THE READ MARK. A head acks what it has DRAWN, which is why there is no answer frame.
    head.ack(3, 1).expect("ack");
    let deadline = Instant::now() + PATIENCE;
    let mark = loop {
        if let Some(mark) = daemon.hub.mark(&hello.head_id)
            && mark.rendered == 3
        {
            break mark;
        }
        assert!(
            Instant::now() < deadline,
            "the ack never reached the daemon"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        (mark.rendered, mark.filtered),
        (3, 1),
        "how much was drawn and how much was filtered are different facts"
    );

    // 3. A PROMPT. The words must arrive whole, on the queue a turn drains.
    let asked = head.prompt(WORDS).expect("prompt");
    let Frame::Accepted {
        client_request_id,
        seq,
        note,
    } = until(
        &head,
        "Accepted for the prompt",
        |f| matches!(f, Frame::Accepted { client_request_id, .. } if client_request_id == &asked),
    )
    else {
        unreachable!("the predicate matched")
    };
    assert_eq!(client_request_id, asked, "an acceptance names the request");
    assert!(seq > 0, "an acceptance says when its effect is visible");
    assert!(
        note.contains("queued"),
        "the panel shows what happened: {note}"
    );
    let queued = daemon
        .hub
        .try_command()
        .expect("the prompt is on the session's queue");
    assert_eq!(queued.head_id, hello.head_id, "the head that asked");
    assert_eq!(queued.client_request_id, asked);
    let CommandKind::Prompt {
        text,
        attachments,
        context,
    } = &queued.kind
    else {
        panic!("a prompt must queue a prompt: {:?}", queued.kind);
    };
    assert_eq!(text, WORDS, "the operator's words reach the daemon whole");
    assert!(attachments.is_empty(), "no chips were attached");
    assert!(
        context.is_none(),
        "the client pinned today was written against version 70 and sends no editor context"
    );

    // 4. THE STREAM. Deltas fold into prose and thinking apart, and a call is two rows.
    daemon.hub.publish(testing::turn_started("t1"));
    daemon.hub.publish(testing::reasoning("t1", THINKING));
    daemon.hub.publish(testing::delta("t1", "the cache key "));
    daemon.hub.publish(testing::delta("t1", PROSE));
    daemon.hub.publish(SessionEvent::ToolStarted {
        turn_id: "t1".into(),
        call_id: "c1".into(),
        name: "read".into(),
        access: "read".into(),
    });
    daemon.hub.publish(SessionEvent::ToolFinished {
        turn_id: "t1".into(),
        call_id: "c1".into(),
        outcome: ToolOutcome::Failed {
            reason: "no such file".into(),
        },
        payload_digest: "fnv1a:0000000000000000".into(),
        inline_bytes: 0,
        full_bytes: 0,
        spill: None,
        repairs: 0,
        edits: Vec::new(),
    });
    daemon.hub.publish(testing::turn_finished("t1"));

    let mut reply = Reply::default();
    let mut started: Option<String> = None;
    let mut tool: Option<(String, String, String)> = None;
    let mut finished: Option<(String, String, String)> = None;
    let mut ending: Option<(String, bool)> = None;
    stream(
        &head,
        &mut reply,
        |frame| {
            let Frame::Event { event, .. } = frame else {
                return;
            };
            match event {
                Event::TurnStarted { model, .. } => started = Some(model.clone()),
                Event::ToolStarted {
                    call_id,
                    name,
                    access,
                    ..
                } => tool = Some((call_id.clone(), name.clone(), access.clone())),
                Event::ToolFinished {
                    call_id,
                    outcome,
                    why,
                    ..
                } => finished = Some((call_id.clone(), outcome.clone(), why.clone())),
                Event::TurnFinished { turn_id, ends_turn } => {
                    ending = Some((turn_id.clone(), *ends_turn))
                }
                _ => {}
            }
        },
        |_, frame| {
            matches!(
                frame,
                Frame::Event {
                    event: Event::TurnFinished {
                        ends_turn: true,
                        ..
                    },
                    ..
                }
            )
        },
    );
    assert_eq!(
        reply.text,
        format!("the cache key {PROSE}"),
        "deltas are fragments, folded in the order they arrived"
    );
    assert_eq!(
        reply.thinking, THINKING,
        "a thought is its own channel and never the reply's prose"
    );
    assert!(
        started.is_some_and(|model| !model.is_empty()),
        "the turn's start names the model the panel's header shows"
    );
    assert_eq!(
        tool,
        Some(("c1".to_string(), "read".to_string(), "read".to_string())),
        "a call's row is its name and the access its schema declares"
    );
    assert_eq!(
        finished,
        Some((
            "c1".to_string(),
            "failed".to_string(),
            "no such file".to_string()
        )),
        "a finished call carries its word and the reason beside it"
    );
    assert_eq!(
        ending,
        Some(("t1".to_string(), true)),
        "the ending says whether the whole prompt is over, because only the daemon knows"
    );

    // 5. A CARD, AND ITS ANSWER. The ladder a permission carries is what the panel's letters pick.
    let mut card = testing::requested("r1", SUMMARY);
    let SessionEvent::DecisionRequested { target: about, .. } = &mut card else {
        panic!("the fixture is a card");
    };
    *about = TARGET.into();
    daemon.hub.publish(card);
    let drawn = until(&head, "a card", |f| {
        matches!(
            f,
            Frame::Event {
                event: Event::DecisionRequested(_),
                ..
            }
        )
    });
    let Event::DecisionRequested(card) = event_of(&drawn) else {
        unreachable!("the predicate matched")
    };
    assert_eq!(
        card.req_id, "r1",
        "an answer is keyed by this and nothing else"
    );
    assert_eq!(
        card.kind, "permission",
        "a permission and a question differ by this word, not by the frame"
    );
    assert_eq!(card.summary, SUMMARY, "the one line a person reads first");
    assert_eq!(card.target, TARGET, "and the one thing being decided about");
    assert_eq!(
        card.options
            .iter()
            .map(|o| o.option_id.as_str())
            .collect::<Vec<_>>(),
        ["allow", "deny"],
        "the ladder, in the order the option ids are answered by"
    );
    assert_eq!(
        card.options[0].label, "Allow once",
        "a rung is drawn with the words a person reads, not its id"
    );
    assert!(
        card.deadline.is_none(),
        "no deadline is *wait forever*: a zero would read as already expired"
    );

    let answered = head.answer("r1", "allow").expect("answer");
    let Frame::Accepted {
        client_request_id,
        note,
        ..
    } = until(
        &head,
        "Accepted for the answer",
        |f| matches!(f, Frame::Accepted { client_request_id, .. } if client_request_id == &answered),
    )
    else {
        unreachable!("the predicate matched")
    };
    assert_eq!(client_request_id, answered);
    assert_eq!(
        note, "permission answered",
        "the panel says what it settled, not only that something was accepted"
    );
    assert_eq!(
        waiter.answers.lock().unwrap().as_slice(),
        [(
            "r1".to_string(),
            Seat::default().identity,
            "allow".to_string()
        )],
        "the answer reaches whoever is blocked on it, attributed to this head"
    );

    // 6. THE INTERRUPT. It names its reason, and it wakes a waiter as well as queueing.
    let stopped = head.interrupt(REASON).expect("interrupt");
    let Frame::Accepted {
        client_request_id,
        note,
        ..
    } = until(
        &head,
        "Accepted for the interrupt",
        |f| matches!(f, Frame::Accepted { client_request_id, .. } if client_request_id == &stopped),
    )
    else {
        unreachable!("the predicate matched")
    };
    assert_eq!(client_request_id, stopped);
    assert_eq!(note, "interrupt requested");
    let queued = daemon
        .hub
        .try_command()
        .expect("the interrupt is on the session's queue");
    assert_eq!(queued.head_id, hello.head_id);
    assert!(
        matches!(&queued.kind, CommandKind::Interrupt { reason } if reason == REASON),
        "the reason the operator gave is what the turn is stopped with: {:?}",
        queued.kind
    );
    assert_eq!(
        waiter.cancelled.lock().unwrap().as_slice(),
        [REASON.to_string()],
        "a thread blocked on a decision is woken by the interrupt, not left to its deadline"
    );
}

/// A newer daemon may say something this build of rano has never heard of. It is counted and
/// skipped, and the stream carries on: a panel that blanked on it would lose the turn, and one
/// that folded it into the reply would draw a plan as prose.
#[test]
fn an_event_the_editor_does_not_know_is_counted_and_the_stream_carries_on() {
    const AFTER: &str = "still here";

    let daemon = daemon("unknown");
    let mut head = Client::connect(&daemon.socket).expect("connect to the daemon's socket");
    head.attach(SESSION, 0, &Seat::default()).expect("attach");
    until(&head, "Hello", |f| matches!(f, Frame::Hello(_)));

    // `Explain` is a real event of this session's log with no reader in an editor — the plan the
    // daemon shows itself. What a variant added after rano's pin looks like from here.
    daemon.hub.publish(SessionEvent::Explain {
        turn_id: "t1".into(),
        plan: serde_json::json!("three steps, the third of them a tool call"),
    });
    daemon.hub.publish(testing::delta("t1", AFTER));

    let mut reply = Reply::default();
    let mut unknown: Vec<String> = Vec::new();
    stream(
        &head,
        &mut reply,
        |frame| {
            if let Frame::Event {
                event: Event::Unknown { tag },
                ..
            } = frame
            {
                unknown.push(tag.clone());
            }
        },
        |reply, _| reply.text == AFTER,
    );
    assert!(
        unknown.iter().any(|tag| tag == "explain"),
        "the tag it could not read is named and counted, never silently dropped: {unknown:?}"
    );
    assert_eq!(reply.text, AFTER, "the delta after it still arrives");
    assert!(
        reply.thinking.is_empty() && reply.markup.is_empty(),
        "an unknown event is not prose and not markup: {reply:?}"
    );
    assert!(
        head.skipped() >= unknown.len() as u64,
        "the panel says how much of a newer daemon it could not read"
    );
}
