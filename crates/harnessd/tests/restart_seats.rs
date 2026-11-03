//! A restart restores a session to a WORKING state, not to a waking one. The acceptance
//! case: a todo conditioned on a job's end, then a restart of the head and the daemon -
//! once the server is back it should fire, because the job is gone.

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

/// The handle whose job ended before this daemon came back. The case in hand: the daemon that ran
/// it is gone, so the row's condition is met by ABSENCE.
const HANDLE: &str = "j-the-job-that-ended-before-the-restart";

/// The row's own words, so the assertion names the row the operator wrote rather than a count.
const ROW: &str = "read the job's result and decide what it means";

/// How long the driver waits for the firing. Generous on purpose: the assertion is what happened
/// not how fast.
const PATIENCE: Duration = Duration::from_secs(20);

/// A directory that removes itself, because a test that leaks a store per run eventually fills the
/// disk.
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

/// The local door: the GGUF this box has, an endpoint on a closed port, and no retry ladder - and
/// no provider, because a configured provider's URL is the one the engine takes.
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
    // business.
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

/// Write `items` onto `transcript` as the ledger's own rows - the fixture `resume.rs` and
/// `idle_clock.rs` also use, kept identical so the files cannot drift on the chain a resume
/// recomputes.
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

/// The store a restart finds: a conversation, a board with one conditioned row on it, and
/// the job row that says what became of the handle.
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

    // The job's own ending, written down before the daemon died. Its state word is the job's own
    // (`exited 0`), which is what the firing's sentence quotes back to the model.
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

    // The row the operator attached a condition to - theirs, not the model's, which is what makes
    // the firing a fact about the world rather than about a plan.
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

/// The firing, as the session's own log holds it - the door a head draws and the door the model
/// reads. `Harness::wake` submits it as a user item before it calls the model, so this is the
/// SEATING and not the answer.
fn the_firing(hub: &Hub) -> Option<(String, Duration)> {
    hub.retained().iter().find_map(|e| match &e.event {
        SessionEvent::TranscriptContent { item, .. }
        | SessionEvent::TranscriptAppended {
            item: Some(item), ..
        } => match &**item {
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

/// Wait for the firing, or give up and say so - a bounded wait, so a failure is an
/// assertion rather than a hung test. Returns what was seated and how long after the
/// daemon's own first pass it took.
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

/// The daemon after a restart seats the firing by itself - no head, no command, and nothing
/// ringing. The session is the store's: the board is on disk, the job's ending is on disk
/// and the daemon that comes up against them has never seen either.
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
    let socket = letibot_sessionlog::testing::socket_path(dir.path(), "harnessd.sock");
    let daemon = Daemon::serve(registry.clone(), &socket).expect("the socket binds");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the session opens");

    // WHERE THE MINUTE GOES, read off the daemon's own clock.
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
    // `Daemon::run` - so a failure here is an assertion rather than a hung test.
    let driver = std::thread::spawn(move || {
        let fired = wait_for_the_firing(&hub);
        registry.close();
        fired
    });

    daemon.run(&mut sessions, |_, _| {});
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

/// The control: a ring is what seats it, and it seats it at once. This is the door a daemon
/// that did NOT restart has - `jobwatch`'s watcher thread calls `Bell::ring_wake` the
/// instant a job settles, and the merge queue's door rings the same way.
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
    let socket = letibot_sessionlog::testing::socket_path(dir.path(), "harnessd.sock");
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

    daemon.run(&mut sessions, |_, _| {});
    let fired = driver.join().expect("the driver thread");
    let (said, took) = fired.expect(
        "a ring reached the worker and the stored fact was not seated: the ring is the only door \
         a between-turns fact has, and this is it failing",
    );
    assert!(said.contains("exited 0"), "{said}");
    eprintln!("MEASURED: the ring seated the same firing {took:?} after it was written");
}
