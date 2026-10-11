//! **A restart restores a session to a WORKING state, not to a waking one.**
//!
//! The operator's ruling, verbatim, and it is the acceptance case this file is written
//! against: *"a todo conditioned on job end, and then i restart head and harnessd. once server
//! is back it should fire - job is gone"*.
//!
//! The DECISION that makes that true is already here and already tested — `harness::fate_of`
//! reads a handle this daemon never ran as `JobFate::Unknown`, and `JobFate::Unknown` MEETS the
//! condition (`the_evaluator_fires_a_row_whose_handle_is_gone`). What was missing is the
//! DELIVERY, and the shape of the hole is one sentence long: **every door that turns a stored
//! fact into a turn is a bell, and a bell is rung by something that was running before the
//! restart.**
//!
//! * `Harness::wake` is the only reader of `due_todo_rows`, of `queue_notices` and of
//!   `handover_notices` — the three facts that are already true in the STORE and that no event
//!   carries after a restart;
//! * `Sessions::wake` is its only servant on this daemon's worker, and the worker is only ever
//!   woken for a session by a ring — a monitor's firing, a background job's settlement, the merge
//!   queue's ask, or a head's command;
//! * the job watcher that would have rung for this very settlement is a thread that died with
//!   the daemon it belonged to, and the settlement it would have reported happened while that
//!   daemon was not there.
//!
//! So a session the daemon RESUMES is opened, armed and parked — and nothing ever rings it. The
//! row the operator attached a condition to is on the board, the world meets the condition, and
//! the model is never told. That is the operator's sentence read the other way round: they asked
//! for *it should fire*, and what they got was *it will fire when something happens to wake this
//! session*, where "something" was the plan check a minute later — whose text is the plan's
//! unfinished work and **not** the firing, so even that turn did not carry the fate.
//!
//! # What is staged, and what is not
//!
//! A **restart**, and the store is what makes it one: a session, a transcript, a board and a job
//! row are written by hand, and the daemon that comes up against them is a daemon that has never
//! seen any of it — the state `letibot --continue` starts in, with the whole of the previous
//! process gone (its process table, its watchers, its bell). What is NOT staged is the process
//! boundary itself: no `exit()` and no `exec`, so the box's own GGUF cache and the model server's
//! KV cache are not thrown away either. Neither is this file's subject — the subject is which
//! ring reaches the worker — and naming that is the honest half.
//!
//! The control is the door a daemon that did NOT restart reaches and a restarted one had no way
//! to: the world RINGS the session (`Bell::ring_wake`, which is what `jobwatch`'s watcher thread
//! calls the moment a job settles). It is rung here by the test rather than by a real background
//! job, because a real job's watcher cannot be summoned after the daemon it belonged to is gone —
//! which is precisely the defect, and why the fix is the daemon ringing for itself. So the control
//! asserts the MECHANISM: a ring is what seats the firing, and it seats it at once.
//!
//! # What it needs
//!
//! The vocabulary GGUF, because `Harness::open` renders its stable prefix — `resume.rs`'s,
//! `session_channel.rs`'s and `review_pass.rs`'s situation exactly, answered the same way:
//! [`present_gguf`] announces a skip when it is not here rather than failing, and
//! `LETIBOT_REQUIRE_APPARATUS=1` refuses that skip.
//!
//! **And NOT a provider, and not a model.** A `ProviderConfig` is not this file's to name: the
//! engine takes a configured provider's own URL, so a fixture that named one to get the byte
//! vocabulary sent its turn to a real endpoint — MEASURED while writing this file, as a `401`
//! from the provider with the prompt in it, which is egress nobody asked for. The local door is
//! the GGUF with `endpoint` on a CLOSED port and `--http-retries 0`, so the turn the firing
//! starts fails in milliseconds without leaving the box. **The row is asserted, not the
//! answer**: `Harness::wake` appends the `[todo] … is due` item BEFORE it calls the model, so
//! what this file measures is the seating and not the generation.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_harnessd::config::Config;
use letibot_harnessd::daemon::Daemon;
use letibot_harnessd::{Dialect, Parts, Sessions};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::ledger::{LedgerRow, chain, hash_tokens};
use letibot_tokencore::store::{
    JobRecord, SessionRecord, StablePrefixRecord, Store, TodoBy, TodoCondition, TodoItem,
    TodoStatus,
};
use letibot_transcript::{TranscriptItem, UserPart};

/// The dialect this file configures, named once so the fixture and the config cannot drift.
const WANTED: Dialect = Dialect::Qwen;

/// **The handle whose job ended before this daemon came back.** The operator's own case: the
/// daemon that ran it is gone, so the row's condition is met by ABSENCE.
const HANDLE: &str = "j-the-job-that-ended-before-the-restart";

/// The row's own words, so the assertion names the row the operator wrote rather than a count.
const ROW: &str = "read the job's result and decide what it means";

/// How long the driver waits for the firing. Generous on purpose: the assertion is what
/// happened, not how fast — see the measured numbers in the two tests.
const PATIENCE: Duration = Duration::from_secs(20);

/// A directory that removes itself, because a test that leaks a store per run eventually fills
/// the disk and the run that finds out is not this one.
struct TempDir {
    path: PathBuf,
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

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The local door: the GGUF this box has, an endpoint on a closed port, and no retry ladder —
/// `session_channel.rs`'s config with `resume.rs`'s vocabulary, and **no provider**, because a
/// configured provider's URL is the one the engine takes.
fn config(store: &Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = WANTED;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.vocab_gguf = letibot_harnessd::vocab_gguf();
    cfg.provider = None;
    cfg.endpoint = letibot_turn::Endpoint::new("127.0.0.1", 1);
    cfg.http_retries = 0;
    // The operator's own `providers.toml`, permission list and search key are not this test's
    // business — `wired.rs`'s rule, and `session_channel.rs`'s two lines.
    cfg.web_search = None;
    cfg.permission = Vec::new();
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

/// Write `items` onto `transcript` as the ledger's own rows — `resume.rs`'s and `idle_clock.rs`'s
/// fixture, kept identical so the three files cannot drift on the chain a resume recomputes.
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

/// **The store a restart finds**: a conversation, a board with one conditioned row on it, and the
/// job row that says what became of the handle.
///
/// The job row is the durable half `JobRecord` exists for — *"a handle the pane listed a moment
/// ago answered `no job … here` to the next daemon — about a job whose ending was written down"*.
/// It is written with the daemon that ran the job, and the daemon that comes back reads it.
fn seed(path: &Path, parts: &Parts, session_id: &str) {
    let store = Store::open(path).expect("opening the store");
    store
        .put_session(&SessionRecord {
            id: session_id.to_string(),
            title: Some("the session a restart found".into()),
            model_id: WANTED.name().into(),
            dialect_sha: renders_template(parts),
            workspace_root: letibot_sessionlog::testing::scratch_root()
                .display()
                .to_string(),
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
    write_rows(
        &store,
        &transcript,
        &prefix_tokens,
        &[
            TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![UserPart::Text {
                    text: "background that build and tell me when it is done".into(),
                }],
            },
            TranscriptItem::Assistant {
                text: "backgrounded it; the handle is in your board".into(),
                tool_calls: vec![],
                truncated: false,
            },
        ],
    );

    // **The job's own ending, written down before the daemon died.** Its state word is the job's
    // own (`exited 0`), which is what the firing's sentence quotes back to the model.
    store
        .put_job(
            session_id,
            &JobRecord {
                handle: HANDLE.into(),
                command: "cargo test --workspace".into(),
                how: "asked".into(),
                state: "exited 0".into(),
                produced: 4096,
                elapsed_ms: 91_000,
                redirect: None,
            },
        )
        .expect("putting the job row");

    // **The row the operator attached a condition to** — theirs, not the model's, which is what
    // makes the firing a fact about the world rather than about a plan.
    store
        .put_todos(
            session_id,
            &[TodoItem {
                content: ROW.into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: Some(TodoCondition::Job {
                    handle: HANDLE.into(),
                }),
                needs: Vec::new(),
            }],
        )
        .expect("putting the board");
}

/// **The firing, as the session's own log holds it** — the door a head draws and the door the
/// model reads. `Harness::wake` submits it as a user item before it calls the model, so this is
/// the SEATING and not the answer.
fn the_firing(hub: &Hub) -> Option<(String, Duration)> {
    hub.retained().iter().find_map(|e| match &e.event {
        SessionEvent::TranscriptContent { item, .. } => match &**item {
            TranscriptItem::User { parts, .. } => {
                let text: String = parts
                    .iter()
                    .filter_map(|p| match p {
                        UserPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                (text.contains("[todo]") && text.contains(HANDLE)).then_some((
                    text,
                    Duration::from_millis(letibot_sessionlog::event::now_ms().saturating_sub(e.ts)),
                ))
            }
            _ => None,
        },
        _ => None,
    })
}

/// Wait for the firing, or give up and say so — a bounded wait, so a failure is an assertion
/// rather than a hung test. Returns what was seated and how long after the daemon's own first
/// pass it took, which is the number this file exists to report.
///
/// **The poll is finer than the thing it measures, so the number is a reading rather than the
/// poll's own granularity**: the seating is one worker pass, and a 50 ms sleep would report
/// every answer as `50ms`. Ten is still two orders of magnitude under the clock this file is
/// about.
fn wait_for_the_firing(hub: &Hub) -> Option<(String, Duration)> {
    let began = Instant::now();
    while began.elapsed() < PATIENCE {
        if let Some(found) = the_firing(hub) {
            return Some(found);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// **The daemon after a restart seats the firing by itself** — no head, no command, and nothing
/// ringing.
///
/// The session is the store's: the board is on disk, the job's ending is on disk, and the daemon
/// that comes up against them has never seen either. What it must do is put the restored session
/// to work — the door every other between-turns fact arrives through (`Sessions::wake` →
/// `Harness::wake`), reached by the daemon's own ring rather than by a watcher that no longer
/// exists.
#[test]
fn a_restart_fires_the_row_whose_job_is_gone() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-restart-seats");
    let path = dir.path().join("sessions.db");
    let session_id = "s-restart-seats";
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

    // **WHERE THE MINUTE GOES, read off the daemon's own clock rather than remembered.** The only
    // deadline a restored session has is the idle plan-check, and it is armed a minute out — that
    // is the turn the operator sees after a restart, and what it carries is the plan's unfinished
    // work (`unfinished_plan`), NOT the fate of the handle the row waits on. So the minute is the
    // plan check's, and the firing is not on it: the two facts are read here together, before the
    // worker takes the thread.
    let clock = sessions
        .next_nag_at()
        .expect("the restored session's plan check is armed — the row is Pending");
    let left = clock.saturating_duration_since(Instant::now());
    assert!(
        left > Duration::from_secs(55) && left <= Duration::from_secs(60),
        "the plan check is the only clock this session has, and it is TODO_NAG_AFTER away: {left:?}"
    );
    eprintln!(
        "MEASURED: the restored session's only deadline is the idle plan check, {left:?} out — \
         and it does not carry the firing"
    );

    let hub = registry.get(session_id).expect("the session's hub");
    // The driver waits for the firing and then closes the registry, which is what ends
    // `Daemon::run` — so a failure here is an assertion rather than a hung test.
    let driver = std::thread::spawn(move || {
        let fired = wait_for_the_firing(&hub);
        registry.close();
        fired
    });

    daemon.run(&mut sessions, |_, _, _| {});
    let fired = driver.join().expect("the driver thread");

    let (said, took) = fired.unwrap_or_else(|| {
        panic!(
            "the daemon came back to a board whose row is due and never told the model: nothing \
             rang this session, so the firing waited for whatever arms it next — the idle plan \
             check a minute later, whose text is the plan and not the fate. The row is `{ROW}` \
             and the handle `{HANDLE}`."
        )
    });
    assert!(
        said.contains("exited 0"),
        "the firing carries what the world says about the handle — the job row's own word — \
         rather than only the row's text: {said}"
    );
    assert!(
        took < PATIENCE,
        "the firing is seated at once, not on the daemon's own clock: {took:?}"
    );
    eprintln!(
        "MEASURED: the firing was seated {took:?} after the row was written, with nobody typing \
         and nothing ringing"
    );
}

/// **The control: a ring is what seats it, and it seats it at once.**
///
/// This is the door a daemon that did NOT restart has — `jobwatch`'s watcher thread calls
/// `Bell::ring_wake` the instant a job settles, and the merge queue's door rings the same way.
/// It is rung here by the test rather than by a real job, because the watcher that would ring it
/// belonged to the process that died: **that is the defect**, and a fixture that could summon it
/// back would be testing something else. What this pins is the mechanism the first test turns
/// on — a ring reaches the worker, and the worker seats the stored fact.
#[test]
fn a_ring_seats_the_firing_at_once() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-restart-seats-ring");
    let path = dir.path().join("sessions.db");
    let session_id = "s-restart-seats-ring";
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

    let hub = registry.get(session_id).expect("the session's hub");
    let ringing = registry.clone();
    let driver = std::thread::spawn(move || {
        // The settlement's own ring, as `jobwatch` makes it.
        ringing.bell().ring_wake(session_id);
        let fired = wait_for_the_firing(&hub);
        ringing.close();
        fired
    });

    daemon.run(&mut sessions, |_, _, _| {});
    let fired = driver.join().expect("the driver thread");
    let (said, took) = fired.expect(
        "a ring reached the worker and the stored fact was not seated: the ring is the only door \
         a between-turns fact has, and this is it failing",
    );
    assert!(said.contains("exited 0"), "{said}");
    eprintln!("MEASURED: the ring seated the same firing {took:?} after it was written");
}
