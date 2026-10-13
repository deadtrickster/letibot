//! **A `/reseat` that carries a long conversation is a wait the head can SEE, and it ends.**
//!
//! The rano session, 2026-10-12 01:17: the operator ran a bare `/reseat` on a 971-row
//! conversation with the head attached since the daemon's boot window, then typed `pull`. For
//! six and a half minutes the head showed the old conversation and `queued · pull` and nothing
//! else. The store says why: the re-ingest's fork wrote the 971 carried rows one autocommit
//! `INSERT` at a time (their `created_at` runs 01:17:48 → 01:24:07) while a dozen daemons shared
//! the file's single write lock, and the worker was inside that command the whole while. Then
//! the `[reseat]` row landed and `pull` was taken right after it.
//!
//! So this pins, on a long conversation, a head attached BEFORE the harness opens, and a prompt
//! queued while the re-seat runs:
//!
//! * the carried rows are written a chunk per transaction, and the head is told how far the
//!   carry has got (`Filling`), from the start, ending at `total of total`;
//! * the head goes on receiving the session's events after the fork — the turn that follows is
//!   on its queue, and it is never demoted to a resync by the carry;
//! * the `[reseat]` row is spoken and the operator's queued prompt is taken by that turn.

use letibot_harnessd::config::Config;
use letibot_harnessd::harness::{ForkTail, Harness};
use letibot_harnessd::sessions::{Outcome, Sessions};
use letibot_harnessd::{Dialect, Parts};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Delivery;
use letibot_sessionlog::hub::{CommandKind, DAEMON_SUBMITTER, Hub, QueuedCommand};
use letibot_sessionlog::protocol::Caps;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_transcript::{Speaker, TranscriptItem, UserPart};
use letibot_turn::CompactionOutcome;

#[allow(dead_code)]
#[path = "../../turn/tests/support/canned.rs"]
mod canned;

const IM_END: u32 = 248046;
const THINK_CLOSE: u32 = 248069;

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "harnessd-reseat-carry-{tag}-{}-{}",
            std::process::id(),
            letibot_harnessd::config::now_ns()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("temp dir");
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config(store: &std::path::Path, session_id: &str, workspace: &std::path::Path) -> Config {
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = Dialect::Qwen;
    // The operator's own `providers.toml` and `permission.json` are not this test's business.
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.http_retries = 0;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg
}

fn spoken(vocab: &letibot_tokencore::Vocab, ids: &[u32]) -> Vec<canned::Frame> {
    ids.iter()
        .map(|id| canned::Frame::Token {
            id: *id,
            text: vocab
                .detokenize(&[*id], true)
                .expect("one token decodes")
                .leak(),
        })
        .collect()
}

fn a_plain_answer(vocab: &letibot_tokencore::Vocab, answer: &str) -> canned::Reply {
    let thought = vocab.tokenize_text("thinking").expect("tokenizes");
    let answer_ids = vocab.tokenize_text(answer).expect("tokenizes");
    let mut frames = vec![canned::Frame::Progress {
        total: 30,
        processed: 30,
    }];
    frames.extend(spoken(vocab, &thought));
    frames.extend(spoken(vocab, &[THINK_CLOSE]));
    frames.extend(spoken(vocab, &answer_ids));
    frames.extend(spoken(vocab, &[IM_END]));
    frames.push(canned::Frame::Final {
        stop_type: "eos",
        n_decoded: (thought.len() + answer_ids.len() + 2) as u64,
        n_prompt: 30,
        cache_n: 0,
    });
    canned::Reply::Frames(frames)
}

fn wiring(cfg: &Config) -> SessionWiring {
    SessionWiring {
        model: cfg.model.clone(),
        dialect: "qwen".into(),
        endpoint: "http://127.0.0.1:1".into(),
        workspace: cfg.workspace.display().to_string(),
    }
}

/// A conversation of `n` rows, alternating the operator's words and the model's answers.
fn a_long_conversation(n: usize) -> Vec<TranscriptItem> {
    (0..n)
        .map(|i| {
            if i % 2 == 0 {
                TranscriptItem::User {
                    speaker: Speaker::Operator,
                    parts: vec![UserPart::Text {
                        text: format!("question number {i}: what does the log mode do next?"),
                    }],
                }
            } else {
                TranscriptItem::Assistant {
                    text: format!("answer number {i}: it tails the file and follows appends."),
                    tool_calls: vec![],
                    truncated: false,
                }
            }
        })
        .collect()
}

/// Everything a live head is sent, collected on a thread of its own the way the server's pump
/// drains it — so the head's queue is drained as it would be, and a demotion shows up as the
/// `Resync` it is rather than as a test that never read its queue.
fn follow(
    hub: std::sync::Arc<Hub>,
    head_id: String,
) -> std::sync::Arc<std::sync::Mutex<Vec<Seen>>> {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let out = seen.clone();
    std::thread::spawn(move || {
        loop {
            match hub.next_batch(&head_id, 4096) {
                Delivery::Events(b) => {
                    let mut g = out.lock().unwrap();
                    for e in b.events() {
                        g.push(Seen::Event(e.event.clone()));
                    }
                }
                Delivery::Resync { reason, .. } => out
                    .lock()
                    .unwrap()
                    .push(Seen::Resync(format!("{reason:?}"))),
                Delivery::Frames(_) => {}
                _ => return,
            }
        }
    });
    seen
}

#[derive(Debug, Clone)]
enum Seen {
    Event(SessionEvent),
    Resync(String),
}

#[test]
fn a_long_reseat_is_watched_by_a_boot_window_head_and_its_queued_prompt_is_taken() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    const ROWS: usize = 600;
    let dir = TempDir::new("store");
    let ws = TempDir::new("ws");
    let store = dir.0.join("sessions.db");
    let id = "s-reseat-carry";
    let seated = config(&store, id, &ws.0);
    let parts = Parts::load(&seated).expect("the vocabulary must load");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

    // Seated under an older prompt, with a long conversation behind it.
    let history = a_long_conversation(ROWS);
    {
        let mut older = seated.clone();
        older.system = "the prompt this session was seated with, several builds ago".into();
        let hub = Hub::new(id);
        let registry = Registry::of(hub.clone());
        let mut h = Harness::open_with_registry(&parts, older, hub, None, None, registry)
            .expect("the session opens");
        let outcome = CompactionOutcome {
            turn_id: format!("{}#seed", h.transcript_id()),
            summary: "the work so far".into(),
            tool_calls: 0,
            truncated: false,
            cached_tokens: 0,
            reusable: 0,
            generated_tokens: 0,
        };
        h.fork_to_summary(
            &outcome,
            None,
            None,
            ForkTail {
                items: &history,
                split: None,
                because: "",
            },
        )
        .expect("the seed fork lands");
    }

    // The restart, and the head that reconnected across it: registered and attached BEFORE
    // anything is opened — the boot window `b0608c1` found.
    let serv =
        canned::Canned::serve_replies(vec![a_plain_answer(&parts.vocab, "re-seat noted")], 1);
    let mut now = seated.clone();
    now.endpoint = serv.endpoint.clone();
    let registry = Registry::new();
    registry
        .create(id, "a session", wiring(&now))
        .expect("the session is created");
    let hub = registry.get(id).expect("the hub");
    let head = hub.attach("tui", "dead", Caps::default(), 0);
    let seen = follow(hub.clone(), head.head_id.clone());
    let mut sessions =
        Sessions::open_first(&parts, now, registry.clone()).expect("the daemon opens");
    assert!(
        registry.stale_prefix(id).is_some(),
        "the fixture must be a session speaking a stale prompt, or the re-seat proves nothing"
    );
    let before = sessions.harness_of(id).expect("open").items().len();
    assert!(
        before > ROWS,
        "the long conversation resumed: {before} rows"
    );

    // The operator's `pull`, typed while the re-seat holds the worker. It is on the hub's
    // queue, which is where a head's prompt waits for the worker or a step boundary.
    hub.submit(
        &head.head_id,
        "pull-1",
        0,
        CommandKind::Prompt {
            text: "pull".into(),
        },
    );

    let began = std::time::Instant::now();
    let out = sessions.dispatch(
        id,
        &QueuedCommand {
            head_id: DAEMON_SUBMITTER.into(),
            identity: "dead".into(),
            client_request_id: "reseat-1".into(),
            at_seq: 0,
            enqueued_ms: 0,
            kind: CommandKind::Reseat { summarise: false },
        },
    );
    let took = began.elapsed();
    assert!(matches!(out, Outcome::Compacted(_)), "the re-seat lands");
    eprintln!("re-seat of {before} rows, with its turn: {took:?}");

    // The store holds the whole carry under the new transcript.
    let h = sessions.harness_of(id).expect("open");
    let items = h.items().to_vec();
    let tid = h.transcript_id().to_string();
    let st = letibot_tokencore::store::Store::open(&store).expect("the store opens");
    let stored = st
        .load_transcript(&tid)
        .expect("the new transcript is stored");
    assert_eq!(
        stored.items.len(),
        items.len(),
        "every row the harness holds is in the store"
    );

    // The model was told, and the operator's queued prompt was taken by that turn.
    let told = items
        .iter()
        .position(|i| matches!(i, TranscriptItem::User { speaker: Speaker::Agent, parts }
            if parts.iter().any(|p| matches!(p, UserPart::Text { text } if text.starts_with("[reseat] ")))))
        .expect("the [reseat] row is spoken after a bare /reseat");
    let pulled = items
        .iter()
        .rposition(|i| {
            matches!(i, TranscriptItem::User { speaker: Speaker::Operator, parts }
            if parts.iter().any(|p| matches!(p, UserPart::Text { text } if text == "pull")))
        })
        .expect("the prompt queued during the re-seat is taken, not left `queued`");
    assert!(pulled > told, "the queued prompt follows the [reseat] row");
    assert!(
        items[told..]
            .iter()
            .any(|i| matches!(i, TranscriptItem::Assistant { text, .. } if text.contains("re-seat noted"))),
        "the turn ran"
    );

    // What the head saw. Wait for the turn's end to have reached it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let seen = loop {
        let s = seen.lock().unwrap().clone();
        if s.iter()
            .any(|e| matches!(e, Seen::Event(SessionEvent::TurnFinished { .. })))
            || std::time::Instant::now() > deadline
        {
            break s;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    if let Some(Seen::Resync(reason)) = seen.iter().find(|e| matches!(e, Seen::Resync(_))) {
        panic!("the carry must not demote the head it is reporting to: {reason}");
    }
    let forked = seen
        .iter()
        .position(|e| matches!(e, Seen::Event(SessionEvent::TranscriptForked { .. })))
        .expect("the head attached in the boot window is told of the fork");
    let ticks: Vec<(String, u64, u64)> = seen
        .iter()
        .filter_map(|e| match e {
            Seen::Event(SessionEvent::Filling {
                what, done, total, ..
            }) if what.starts_with("re-seating") => Some((what.clone(), *done, *total)),
            _ => None,
        })
        .collect();
    assert!(
        !ticks.is_empty(),
        "a carry of {before} rows must say so while it runs — a bare `queued` was all the \
         operator had for 6.5 minutes"
    );
    let total = ticks[0].2;
    assert_eq!(ticks[0].1, 0, "said from the first moment: {ticks:?}");
    assert_eq!(
        ticks.last().unwrap().1,
        total,
        "and ended by fact, `total of total`: {ticks:?}"
    );
    assert!(
        ticks.len() <= 2 * (total as usize / 64 + 3),
        "a tick per chunk in each of the two phases, not per row — a flood demotes the head: \
         {} ticks",
        ticks.len()
    );
    // **The render ticks too** — the long half of the carry, which used to say `0 of N` and
    // nothing more until the write raced through: the operator, *"progress bars do not work …
    // always at zero"*. Between its start and its end it says how far it has got, and it ends
    // before the write's bar begins.
    let rendering: Vec<u64> = ticks
        .iter()
        .filter(|(w, _, _)| w.contains("rendering"))
        .map(|(_, d, _)| *d)
        .collect();
    assert!(
        rendering.iter().any(|d| *d > 0 && *d < total),
        "the render moves between 0 and {total}: {ticks:?}"
    );
    let last_render = ticks
        .iter()
        .rposition(|(w, _, _)| w.contains("rendering"))
        .unwrap();
    let first_write = ticks
        .iter()
        .position(|(w, _, _)| w.contains("writing"))
        .expect("the write has a bar of its own");
    assert!(
        last_render < first_write,
        "the render ends before the write begins: {ticks:?}"
    );
    assert!(
        seen[forked..]
            .iter()
            .any(|e| matches!(e, Seen::Event(SessionEvent::TurnStarted { .. }))),
        "the turn after the re-seat reaches the head that watched it"
    );
    assert!(
        seen[forked..].iter().any(|e| matches!(
            e,
            Seen::Event(SessionEvent::Delta { .. })
                | Seen::Event(SessionEvent::TranscriptAppended { .. })
        )),
        "and so do its rows"
    );
}
