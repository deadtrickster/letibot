//! **A stored session a head asks a running daemon for, end to end** — the warm half of
//! resume, which is the half no test covered.
//!
//! MEASURED 2026-10-26 ~14:10 on macOS: the operator's daemon for `~/Projects/stroppy-io`
//! was up and seated on one session; they ran `leticode --session s-1793…`, a session the
//! store held with 166 rows. The daemon's own log says the restore happened
//! (`s-1793017817134441000 · resumed 166 row(s), 537946 tokens`) and the prompt the
//! operator then typed was appended as seq 166 of that conversation, with the model
//! answering about the pptx they had been talking about an hour earlier. **What the head
//! drew was an empty transcript with the right title**, and the echo of the prompt said
//! `queued` — so between the daemon's restore and the head's screen something was missing.
//!
//! # The path, and where each half is measured
//!
//! `leticode --session ID` with a daemon up runs `letibot-tui --resume ID`
//! (`scripts/letibot`), and the head then takes five frames: `ResumeSession`, the
//! `Sessions { created }` answer, `Switch`, the `Hello` with a snapshot, and — published
//! by the worker when it reaches the `Work::Open` the creation rung — the restored rows as
//! `TranscriptAppended` + `TranscriptContent`.
//!
//! This file drives the first four from a real `HeadClient` against a real daemon socket
//! and the fifth from the real `Sessions::open`, with a real store underneath. What it
//! asserts is the thing the operator's screen needed: **every stored row reaches the head**,
//! whether it arrives in the switch's snapshot or as a live event afterwards, and the
//! daemon knows it resumed (`resume_report`, which is the `RESUMED … row(s)` line).
//!
//! The head's own half — that an `App` holding those frames draws them — is
//! `letibot-tui`'s `tests/warm_session.rs`, because this crate is above the head's and
//! cannot build one.
//!
//! # One test, and why
//!
//! The turns here go to a fake provider on loopback, which is reached through
//! `providers.toml` under a scratch `XDG_CONFIG_HOME` — and `set_var` beside a live thread
//! is undefined behaviour this edition makes `unsafe`. So the environment is pinned once,
//! before the daemon or any thread exists, and the phases are phases of one test. Same
//! discipline as `activity_two_sessions.rs`, for the same reason.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::sessions::StoreSessions;
use letibot_harnessd::{Dialect, Parts, Sessions};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::hub::CommandKind;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::registry::{Registry, Work};
use letibot_sessionlog::server::serve_registry;
use letibot_tokencore::ledger::{LedgerRow, chain, hash_tokens};
use letibot_tokencore::store::{SessionRecord, StablePrefixRecord, Store};
use letibot_transcript::{TranscriptItem, UserPart};

/// The daemon's own first session: what a bare attach lands on.
const SEATED: &str = "s-seated";
/// The session in the store that the head asks for — the operator's case.
const EARLIER: &str = "s-earlier";
/// A second stored session: the one a prompt is queued against before it is opened.
const PROMPTED: &str = "s-prompted";
/// The rows each stored session holds: three pairs of a conversation about a pptx.
const ROWS: u32 = 6;

const PATIENCE: Duration = Duration::from_secs(30);

fn user(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

fn assistant(text: &str) -> TranscriptItem {
    TranscriptItem::Assistant {
        text: text.into(),
        tool_calls: Vec::new(),
        truncated: false,
    }
}

/// The conversation both stored sessions hold — the text the restored prompt must carry.
fn conversation() -> Vec<TranscriptItem> {
    vec![
        user("look at ~/Documents/stroppy_final_final.pptx"),
        assistant("it is a deck of twelve slides about the conformance suite"),
        user("write a draft of the notes beside it"),
        assistant("I wrote draft.md beside the deck"),
        user("keep it short"),
        assistant("short it is — three bullets and a link to the deck"),
    ]
}

/// The hash this build renders `Dialect::Qwen` with, spelled the way the store holds it.
fn renders_template(parts: &Parts) -> String {
    parts
        .wiring
        .spec()
        .template_sha
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// **Write `items` onto `transcript` as the ledger's own rows** — `tests/resume.rs`'s
/// helper, which is the one fixture in this tree that writes a conversation the way the
/// ledger's own restore reads it back.
fn write_rows(
    store: &Store,
    transcript: &str,
    prefix_tokens: &[u32],
    items: &[TranscriptItem],
) -> Vec<String> {
    let mut h = hash_tokens(prefix_tokens);
    let mut offset = prefix_tokens.len() as u32;
    let mut ids = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let tokens: Vec<u32> = vec![100 + i as u32, 200 + i as u32];
        h = chain(&h, &tokens);
        let item_id = format!("{transcript}.{i}");
        let row = LedgerRow {
            item_id: item_id.clone(),
            tok_offset: offset,
            tok_len: tokens.len() as u32,
            h_k: h,
        };
        offset += tokens.len() as u32;
        store
            .append_item(transcript, i as u32, item, &row, &tokens)
            .expect("appending a row");
        ids.push(item_id);
    }
    ids
}

/// **A session in the store with a conversation in it**, written here so the store is the
/// test's and never the operator's.
fn stored_session(store: &Store, parts: &Parts, session_id: &str, workspace: &Path) -> Vec<String> {
    let template = renders_template(parts);
    store
        .put_session(&SessionRecord {
            id: session_id.to_string(),
            title: Some("the pptx conversation".into()),
            model_id: Dialect::Qwen.name().into(),
            dialect_sha: template.clone(),
            workspace_root: workspace.display().to_string(),
            owner: "dead".into(),
            approvers: vec![],
            role: None,
            parent_session_id: None,
        })
        .expect("putting the session");
    let prefix_tokens: Vec<u32> = vec![7, 8, 9];
    let prefix_id = store
        .put_stable_prefix(&StablePrefixRecord {
            dialect_sha: template,
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
    write_rows(store, &transcript, &prefix_tokens, &conversation())
}

#[test]
fn a_stored_session_asked_for_by_a_head_is_restored_before_any_turn_runs() {
    let dir = TempDir::new("warm-open");
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("config/letibot")).expect("config dir");
    // SAFETY: this binary has ONE test, and these are set before the daemon, any thread that
    // reads config, or the harness exists.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("GLM_API_KEY");
        std::env::remove_var("LETIBOT_MODE");
    }
    let fake = Fake::serve();
    std::fs::write(
        root.join("config/letibot/providers.toml"),
        format!(
            "# Written by crates/harnessd/tests/warm_open.rs — a fake on loopback.\n\
             [deepseek]\nkey = \"sk-not-used\"\nurl = \"{}\"\n",
            fake.url
        ),
    )
    .expect("providers.toml");

    let workspace = root.join("ws");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let mut cfg = Config::for_this_box(&workspace);
    cfg.dialect = Dialect::Qwen;
    // **`leticode`, and not `coder`** — a confined seat cannot be built on a Mac.
    cfg.seat = Seat::Leticode;
    cfg.http_retries = 0;
    cfg.context_window = Some(32_768);
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.session_id = SEATED.into();
    cfg.store = Some(root.join("sessions.db"));
    // The byte vocabulary and a provider: no GGUF, so this runs anywhere.
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: None,
        thinking: false,
    });
    let parts: &'static Parts = Box::leak(Box::new(
        Parts::load(&cfg).expect("the byte vocabulary needs no model file"),
    ));
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

    // --- the store: two conversations no daemon has ever opened ----------------
    let store = Store::open(cfg.store.as_ref().expect("a store path")).expect("the store opens");
    let earlier_ids = stored_session(&store, parts, EARLIER, &workspace);
    let prompted_ids = stored_session(&store, parts, PROMPTED, &workspace);
    assert_eq!(earlier_ids.len(), ROWS as usize);
    drop(store);

    // --- the daemon: seated on ONE session, holding a socket ----------------
    let registry = Registry::new();
    registry
        .create(SEATED.to_string(), "", Sessions::wiring(&cfg))
        .expect("the daemon's own first session");
    registry.set_source(StoreSessions::open(&cfg).expect("the store is the source"));
    let socket = root.join("warm.sock");
    let server = serve_registry(registry.clone(), &socket).expect("the socket binds");
    let mut sessions = Sessions::open_first(parts, cfg.clone(), registry.clone())
        .expect("the first session opens");
    // **The rings this test does not measure are drained here**: creating the daemon's first
    // session rang its own `Open`, and `open_first` rings a wake. What is measured below is
    // the open the HEAD's ask causes, and a queue with yesterday's work in it would make
    // `next_work` a queue position rather than a fact.
    drain_work(&registry);
    assert!(
        registry.get(EARLIER).is_none(),
        "the daemon does not hold the stored session yet — that is the state the head asks from"
    );

    // --- the head: attach to the seated one, then ask for the stored one ------
    let (mut client, hello, reader) =
        HeadClient::attach(&socket, SEATED, 0, "tui", "dead", Caps::default())
            .expect("the head attaches");
    assert!(
        matches!(hello, ServerFrame::Hello { .. }),
        "a bare attach is answered with a Hello"
    );
    let (tx, rx) = std::sync::mpsc::channel();
    let pumping = std::thread::spawn(move || pump(reader, tx));

    client.resume_session(EARLIER).expect("the ask goes out");
    let f = until(&rx, PATIENCE, |f| matches!(f, ServerFrame::Sessions { .. }));
    let ServerFrame::Sessions { created, .. } = f else {
        unreachable!()
    };
    assert_eq!(
        created.as_deref(),
        Some(EARLIER),
        "answered like a creation, which is what the head switches on"
    );
    let hub = registry
        .get(EARLIER)
        .expect("ResumeSession registered a hub");
    assert_eq!(
        registry.parent_of(EARLIER),
        None,
        "a resumed root is a root"
    );
    // 1. **The worker is rung to open it, and that is the restore's only door.**
    assert!(
        matches!(registry.next_work(), Some(Work::Open(id)) if id == EARLIER),
        "registering the session must wake the worker to open it — the resume's chain \
         check, its dialect refusal and its republished transcript all live there"
    );

    client.switch(EARLIER, 0).expect("the head goes there");
    let f = until(&rx, PATIENCE, |f| matches!(f, ServerFrame::Hello { .. }));
    let ServerFrame::Hello {
        session_id,
        snapshot,
        ..
    } = f
    else {
        unreachable!()
    };
    assert_eq!(
        session_id, EARLIER,
        "the head is in the session it asked for"
    );
    // The frame the operator saw: a hub nothing has written to yet, so an empty snapshot —
    // and the ONE thing that must come with it, the restore announced from the store's own
    // count. Without it a head is left to draw a conversation of `ROWS` rows as a session
    // that has said nothing (MEASURED 2026-10-26), and the prompt it types meanwhile says a
    // bare `queued` with nothing behind it.
    let in_snapshot: Vec<letibot_sessionlog::view::SnapshotItem> = snapshot
        .as_ref()
        .map(|s| s.items.clone())
        .unwrap_or_default();
    let in_snapshot_len = in_snapshot.len();
    let announced = until_event(
        &rx,
        PATIENCE,
        |e| matches!(e, SessionEvent::Filling { what, .. } if what == letibot_sessionlog::RESTORING_ROWS),
    );
    let SessionEvent::Filling {
        what,
        unit,
        done,
        total,
    } = announced
    else {
        unreachable!()
    };
    assert_eq!(what, letibot_sessionlog::RESTORING_ROWS);
    assert_eq!(unit, "rows", "the unit the walk counts in");
    assert_eq!((done, total), (0, ROWS as u64));
    drop(hub);
    let hub = registry.get(EARLIER).expect("its hub");

    // --- the worker reaches Work::Open, and the restore happens ---------------
    assert!(
        sessions.open(EARLIER).expect("the session must resume"),
        "it was not open before, so this is the open"
    );
    let report = sessions
        .resume_report(EARLIER)
        .unwrap_or_else(|| panic!("{EARLIER} was not resumed"));
    assert_eq!(
        report.rows as u32, ROWS,
        "every stored row came back — this is the `RESUMED … row(s)` line the daemon prints"
    );

    // 2. **Every row reaches the head**, whether it was in the switch's snapshot or
    // published live after it. This is the assertion the operator's empty screen failed.
    let seen = drain_rows(&rx, &earlier_ids, &in_snapshot, PATIENCE);
    assert_eq!(
        seen.announced, ROWS as usize,
        "{} row(s) announced to the head (snapshot {in_snapshot_len} + {})",
        seen.announced, seen.live
    );
    assert_eq!(
        seen.bodies, ROWS as usize,
        "{} row(s) arrived with a body — an announced row with none draws as \
         `content not loaded` for ever",
        seen.bodies
    );
    // And the hub a later head would attach to holds the conversation too.
    assert_eq!(
        hub.snapshot().items.len() as u32,
        ROWS,
        "the session's log is the conversation"
    );

    // --- a prompt queued BEFORE the restore finishes -------------------------
    //
    // This is the operator's `hmm`: typed into a session the daemon had registered and
    // not opened. The bell has two lanes, and the open is served first.
    registry
        .create_under(
            PROMPTED,
            "the pptx conversation",
            registry.wiring(PROMPTED),
            None,
        )
        .expect("what ResumeSession does");
    let prompting = registry.get(PROMPTED).expect("its hub");
    let seat = prompting.attach("tui", "dead", Caps::default(), 0);
    let reply = prompting.submit(
        &seat.head_id,
        "c-hmm",
        0,
        CommandKind::Prompt {
            text: "hmm, and what did you call the draft?".into(),
            attachments: Vec::new(),
        },
    );
    assert!(
        matches!(reply, ServerFrame::Accepted { .. }),
        "the prompt is taken: {reply:?}"
    );
    assert!(
        matches!(registry.next_work(), Some(Work::Open(id)) if id == PROMPTED),
        "the open of a session a prompt arrives in comes first"
    );
    let Work::Command(id, cmd) = registry
        .next_work()
        .expect("the prompt is queued behind the open")
    else {
        panic!("the prompt must be served after the open, and as a command");
    };
    assert_eq!(id, PROMPTED);

    assert!(
        sessions.open(PROMPTED).expect("the second session resumes"),
        "the restore runs before the prompt does"
    );
    assert_eq!(
        sessions
            .resume_report(PROMPTED)
            .expect("and it was a resume")
            .rows as u32,
        ROWS
    );
    sessions.dispatch(PROMPTED, &cmd);

    // 3. **The turn ran on the restored conversation, not on an empty one.**
    let asked = fake.last_request();
    assert!(
        asked.contains("stroppy_final_final.pptx"),
        "the model was sent the conversation the store held; it got:\n{asked}"
    );
    assert!(
        asked.contains("hmm, and what did you call the draft?"),
        "and the prompt itself: {asked}"
    );
    // The row the prompt appended continues the stored chain — seq ROWS, not seq 0 of a
    // new transcript, which is the same fact stated where the store keeps it.
    let store = Store::open(cfg.store.as_ref().expect("a store path")).expect("reopening");
    let transcript = format!("{PROMPTED}#t0");
    assert_eq!(
        store.item_count(&transcript).expect("counting"),
        ROWS + 2,
        "the prompt and its answer are rows {ROWS} and {} of the SAME transcript",
        ROWS + 1
    );
    for id in &prompted_ids {
        assert!(
            id.starts_with(&format!("{transcript}.")),
            "the stored rows are the transcript's own: {id}"
        );
    }

    let _ = client.detach();
    drop(client);
    let _ = pumping.join();
    server.shutdown();
    registry.close();
}

/// Pop every piece of work already queued, so the next `next_work` is the one this test's
/// own step caused.
fn drain_work(registry: &Arc<Registry>) {
    while !matches!(
        registry.next_work_until(Some(std::time::Instant::now())),
        letibot_sessionlog::registry::WorkOrIdle::Idle
            | letibot_sessionlog::registry::WorkOrIdle::Closed
    ) {}
}

/// What the head was told about the restored rows, over the snapshot and the live stream.
struct Seen {
    announced: usize,
    bodies: usize,
    live: usize,
}

/// Read frames until every id in `want` has been announced **and** carried a body, counting
/// which came from the switch's `Hello` and which arrived as events after it.
fn drain_rows(
    rx: &std::sync::mpsc::Receiver<Inbound>,
    want: &[String],
    at_hello: &[letibot_sessionlog::view::SnapshotItem],
    within: Duration,
) -> Seen {
    use std::collections::HashSet;
    let mut announced: HashSet<String> = HashSet::new();
    let mut announced_at_hello: HashSet<String> = HashSet::new();
    let mut bodies: HashSet<String> = HashSet::new();
    for item in at_hello {
        let id = item.item_id.clone();
        if item.item.is_some() {
            bodies.insert(id.clone());
        }
        announced_at_hello.insert(id.clone());
        announced.insert(id);
    }
    let deadline = Instant::now() + within;
    while announced.len() < want.len() || bodies.len() < want.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let Ok(inbound) = rx.recv_timeout(left.min(Duration::from_millis(500))) else {
            continue;
        };
        if let ServerFrame::Event(env) = inbound.frame() {
            match env.event {
                SessionEvent::TranscriptAppended { item_id, .. } => {
                    announced.insert(item_id);
                }
                SessionEvent::TranscriptContent { item_id, .. } => {
                    bodies.insert(item_id);
                }
                _ => {}
            }
        }
    }
    let live = announced
        .iter()
        .filter(|id| !announced_at_hello.contains(*id))
        .count();
    Seen {
        announced: want.iter().filter(|id| announced.contains(*id)).count(),
        bodies: want.iter().filter(|id| bodies.contains(*id)).count(),
        live,
    }
}

/// Wait for the next session event matching `f`, or fail.
fn until_event(
    rx: &std::sync::mpsc::Receiver<Inbound>,
    within: Duration,
    mut f: impl FnMut(&SessionEvent) -> bool,
) -> SessionEvent {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(frame) => {
                if let ServerFrame::Event(env) = frame.frame()
                    && f(&env.event)
                {
                    return env.event;
                }
            }
            Err(_) => break,
        }
    }
    panic!("the event never arrived");
}

/// Wait for a frame matching `f`, or fail. The head's channel is the only reader of the
/// socket, so a frame nobody matches is a frame nobody sees.
fn until(
    rx: &std::sync::mpsc::Receiver<Inbound>,
    within: Duration,
    mut f: impl FnMut(&ServerFrame) -> bool,
) -> ServerFrame {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(frame) => {
                let frame = frame.frame();
                if f(&frame) {
                    return frame;
                }
            }
            Err(_) => break,
        }
    }
    panic!("the frame never arrived");
}

/// A directory that removes itself, because a test that leaks a store per run eventually
/// fills the disk and the run that finds out is not this one.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos()
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

/// **A fake provider on a loopback port**, answering every round with the same sentence and
/// remembering the last request it was sent — so *"the turn ran on the restored
/// conversation"* is a fact about the bytes rather than an inference from prose.
struct Fake {
    url: String,
    last: Arc<Mutex<String>>,
}

impl Fake {
    fn serve() -> Fake {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let url = format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            listener.local_addr().unwrap().port()
        );
        let last = Arc::new(Mutex::new(String::new()));
        let kept = Arc::clone(&last);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let body = read_request(&conn);
                *kept.lock().unwrap_or_else(|e| e.into_inner()) = body;
                let _ = write_sse(conn, "you called it draft.md");
            }
        });
        Fake { url, last }
    }

    fn last_request(&self) -> String {
        self.last.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Drain one HTTP request off the socket: headers, then the body `content-length` names.
fn read_request(stream: &TcpStream) -> String {
    use std::io::BufRead;
    use std::io::BufReader;
    let mut reader = BufReader::new(stream.try_clone().expect("clone the socket"));
    let mut line = String::new();
    let _ = reader.read_line(&mut line);
    let mut len = 0usize;
    loop {
        let mut head = String::new();
        if reader.read_line(&mut head).is_err() || head.trim_end().is_empty() {
            break;
        }
        if let Some(v) = head.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; len];
    let _ = reader.read_exact(&mut body);
    String::from_utf8_lossy(&body).into_owned()
}

/// One round as SSE — the frames `letibot-provider` parses, in the shapes
/// `activity_two_sessions.rs` serves, so the two fakes cannot drift on the wire bytes.
fn write_sse(mut stream: TcpStream, text: &str) -> std::io::Result<()> {
    let mut body = String::new();
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"delta":{"content": text}}]})
    ));
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}]})
    ));
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[],"usage":{
            "prompt_tokens": 50,
            "completion_tokens": 9,
            "prompt_cache_hit_tokens": 20,
        }})
    ));
    body.push_str("data: [DONE]\n\n");
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}
