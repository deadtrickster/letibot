//! **A wake arms the clock that closes out a call nothing answered.**
//!
//! The worker's wait is `next_work_until(deadline)`, and the deadline is the earliest of the
//! per-session idle plan-check and the abandoned-call sweep (`Sessions::next_nag_at`). Two arms on
//! the worker's own thread run a turn, and a turn dispatches tool calls — so both of them must arm
//! the sweep, which is the only thing that looks at a call whose executor vanished
//! (`Sessions::sweep_abandoned_calls`, run from the idle arm).
//!
//! The **command** arm has always armed it (`daemon.rs`'s `Work::Command`, with the measurement in
//! its comment: eight minutes of a spinner, `esc esc` dead). The **wake** arm did not, and a wake is
//! a turn: a child's settlement, a background job's completion, a merge-queue ring that finds an
//! entry waiting for a gate. That is the arm this file is about, and it is the arm the tree's own
//! history fixed once before — `8bd6fb8`'s *"the merge arm that ran a turn without touching it is
//! armed now"*, one door along, in `serve_child`'s between-turns message.
//!
//! # Why the plan-check is not the deadline a wake leaves behind
//!
//! `Sessions::wake` does call `after_turn`, and `after_turn` does re-arm the plan-check. It answers
//! *no* for an unchanged plan, and that is by design rather than a defect — `nag_should_arm`: the
//! plan has not moved, so what a wake-driven turn leaves is the REPEAT, minutes away, and nothing
//! sooner. So the sweep is the only clock that can look at the transcript a wake-driven turn just
//! wrote, and without it the worker goes back to a wait with no deadline in it at all.
//!
//! MEASURED 2026-10-10 (`/home/dead/logs/harnessd.log`): the last three turns before a nine-hour
//! silence were `monitor -> …` — wake-driven, every one — and the daemon was woken only by the
//! operator's command the next morning. A call stranded by one of those turns would have sat in the
//! head for the whole nine hours.
//!
//! # What is driven, and what is not
//!
//! The whole daemon, its real worker and its real idle arm, on a fixture whose call nothing answered
//! is written into the store rather than produced by a turn — the executor that vanishes cannot be
//! summoned, and what the sweep reads is the transcript. The wake arrives as `Ring::Woken`, which is
//! the door a settlement and the queue's ring both come through. **No model is asked anything**: the
//! wake here has nothing to say, so it runs no turn, and the only actor is the daemon's own clock.
//! That is deliberate — the property under test is the ARMING, and a turn in the middle would put a
//! model between the assertion and the arm.
//!
//! No GGUF either: the byte vocabulary, the door `resume.rs` and `no_vocab.rs` take, so this runs on
//! a machine with no model file.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, ProviderConfig};
use letibot_harnessd::daemon::Daemon;
use letibot_harnessd::{Dialect, Parts, Sessions};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::ledger::{LedgerRow, chain, hash_tokens};
use letibot_tokencore::store::{SessionRecord, StablePrefixRecord, Store};
use letibot_transcript::{ToolCall, ToolOutcome, TranscriptItem, UserPart};

/// The dialect this file configures, named once so the fixture and the config cannot drift.
const WANTED: Dialect = Dialect::Qwen;

/// How long the driver waits for the sweep's row. The grace the arm sets is two seconds; this is the
/// patience of a loaded runner, and what is asserted is that the call is closed, not how fast.
const PATIENCE: Duration = Duration::from_secs(30);

/// A directory that removes itself, because a test that leaks a store per run eventually fills the
/// disk and the run that finds out is not this one.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
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

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A provider that is never called and no GGUF: the byte vocabulary, so a resume is a store read and
/// a re-hash and nothing here needs a model file.
fn config(store: &Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = WANTED;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: None,
        api_key: Some("sk-not-used".into()),
        thinking: false,
    });
    cfg
}

/// The hash this build renders `WANTED` with, spelled the way the store holds it.
fn renders_template(parts: &Parts) -> String {
    parts
        .wiring
        .spec()
        .template_sha
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Write `items` onto `transcript` as the ledger's own rows — `resume.rs`'s fixture, kept identical
/// so the two files cannot drift on the chain a resume recomputes.
fn write_rows(store: &Store, transcript: &str, prefix_tokens: &[u32], items: &[TranscriptItem]) {
    let mut h = hash_tokens(prefix_tokens);
    let mut offset = prefix_tokens.len() as u32;
    for (i, item) in items.iter().enumerate() {
        let tokens: Vec<u32> = vec![100 + i as u32, 200 + i as u32];
        h = chain(&h, &tokens);
        let row = LedgerRow {
            item_id: format!("{transcript}.{i}"),
            tok_offset: offset,
            tok_len: tokens.len() as u32,
            h_k: h,
        };
        offset += tokens.len() as u32;
        store
            .append_item(transcript, i as u32, item, &row, &tokens)
            .expect("appending a row");
    }
}

/// **A session whose transcript ends in a call nothing answered** — the state the sweep exists for.
///
/// No plan: an empty board arms no idle check, which is what makes the wake the only thing that can
/// arm the sweep in this test.
fn seed(path: &Path, parts: &Parts, session_id: &str) {
    let store = Store::open(path).expect("opening the store");
    store
        .put_session(&SessionRecord {
            id: session_id.to_string(),
            title: Some("the idle-clock session".into()),
            model_id: WANTED.name().into(),
            dialect_sha: renders_template(parts),
            workspace_root: std::env::temp_dir().display().to_string(),
            owner: "dead".into(),
            approvers: vec![],
            role: None,
            parent_session_id: None,
        })
        .expect("putting the session");

    let prefix_tokens: Vec<u32> = vec![7, 8, 9];
    let prefix_id = store
        .put_stable_prefix(&StablePrefixRecord {
            dialect_sha: renders_template(parts),
            system: "you are letibot".into(),
            tools_json: vec![],
            tokens: prefix_tokens.clone(),
            h_init: hash_tokens(&prefix_tokens),
            vocab_source: parts.vocab.source().to_string(),
        })
        .expect("putting the prefix");

    let transcript = format!("{session_id}#t0");
    store
        .put_transcript(&transcript, session_id, &prefix_id)
        .expect("putting the transcript");
    let items = vec![
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: "read the thing".into(),
            }],
        },
        // **The request, and no answer after it.** This one row is the whole fixture:
        // `abandoned_calls` reads the transcript and finds a call nobody will ever answer.
        TranscriptItem::Assistant {
            text: "on it".into(),
            tool_calls: vec![ToolCall {
                id: "call-nothing-answered".into(),
                name: "read".into(),
                arguments: r#"{"path":"src/main.rs"}"#.into(),
            }],
            truncated: false,
        },
    ];
    write_rows(&store, &transcript, &prefix_tokens, &items);
}

/// **The swept row on the session's own log** — the door a head draws, and the durable record the
/// sweep's `NotRun` result is: `Harness::reconcile` hands every appended item to `Hub::record_item`,
/// so the hub is where a row the idle arm wrote shows up.
fn the_call_is_closed(hub: &Hub) -> bool {
    hub.retained().iter().any(|e| {
        matches!(
            &e.event,
            SessionEvent::TranscriptContent { item, .. }
                if matches!(
                    &**item,
                    TranscriptItem::ToolResult {
                        outcome: ToolOutcome::NotRun { .. },
                        ..
                    }
                )
        )
    })
}

/// Wait for the sweep's row, or give up and say so — a bounded wait, so a failure is an assertion
/// rather than a hung test.
fn wait_for_the_swept_call(hub: &Hub) -> bool {
    let began = Instant::now();
    while began.elapsed() < PATIENCE {
        if the_call_is_closed(hub) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// **The arm this file exists for**: a wake, and the daemon comes back and closes a call nothing
/// answered. Without the arming the worker's next wait has no deadline at all, the idle arm never
/// runs, and the call stays open for ever.
#[test]
fn a_wake_arms_the_sweep_that_closes_a_call_nothing_answered() {
    let dir = TempDir::new("harnessd-idle-clock");
    let path = dir.path().join("sessions.db");
    let session_id = "s-idle-clock";
    let cfg = config(&path, session_id);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    seed(&path, &parts, session_id);

    let registry = Registry::new();
    registry
        .create(session_id.to_string(), "", Sessions::wiring(&cfg))
        .expect("a fresh registry has no session by that name");
    let socket = dir.path().join("harnessd.sock");
    let daemon = Daemon::serve(registry.clone(), &socket).expect("the socket binds");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the session opens");

    // **Nothing is armed, and that is the premise.** No plan, so no idle check is owed; no command,
    // so nothing has armed the sweep. The worker's next wait would be a condvar wait with no
    // deadline at all — and with no deadline the idle arm never runs, which is the nine hours.
    assert!(
        sessions.next_nag_at().is_none(),
        "the fixture must start with nothing armed, or the wake is not what arms the sweep"
    );

    // The wake, and the wait for what the sweep writes. The driver is the daemon's neighbours — a
    // child settling, a job finishing, the merge queue ringing — every one of which arrives as
    // `Ring::Woken`, the arm under test.
    let hub = registry.get(session_id).expect("the session's hub");
    let driver = std::thread::spawn(move || {
        registry.bell().ring_wake(session_id);
        let closed = wait_for_the_swept_call(&hub);
        // **Always, and before the result is returned**: closing the registry is what ends
        // `Daemon::run`, so a failure here has to be an assertion rather than a hung test.
        registry.close();
        closed
    });

    daemon.run(&mut sessions, |_, _, _| {});
    let closed = driver.join().expect("the driver thread");
    assert!(
        closed,
        "the wake left the worker no deadline: the idle arm never ran and the call nothing \
         answered is still open"
    );
}
