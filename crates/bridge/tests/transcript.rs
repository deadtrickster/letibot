//! **`leticode_transcript` against a real daemon** — a turn's rows, a cursor, and the cap.
//!
//! # What this file is about
//!
//! The operator's Claude Code pane follows a package's session read-only and tailed, and until
//! B8 it did that through `leticode_events` — the bridge's progress ring, whose texts are cut to
//! one line. Reading the session store directly is their ruled-out answer (*"not a good idea"*),
//! so `leticode_transcript` asks the daemon for the rows a head would be handed and renders them
//! with the item in them.
//!
//! Three facts, and each one is a way the tool could quietly lie:
//!
//! | fact | how it is measured |
//! |---|---|
//! | a turn's rows come back IN ORDER, with their content | the user row of the brief is before the assistant row that answered it, in a real turn's snapshot |
//! | `since` returns only newer rows | the cursor from before a second turn, then the second turn's two rows and nothing else |
//! | long text is complete up to the cap | a 9 000-character answer comes back at exactly `cap`, with `cut: true`, and the cap is in the answer |
//! | a WATCHED session answers it too | a second `Bridge` attaches `watch_only` and reads the same rows |
//!
//! # Why a daemon and not a stub
//!
//! The rows are the daemon's: a fixture that handed back a `Snapshot` it built itself would be
//! asserting what this file wrote. So the daemon here is harnessd's own `Daemon::serve` over a
//! real `Registry` and a real `Sessions::open_first`, exactly as `end_to_end.rs` and harnessd's
//! integration tests build one. What is replaced is **the model** — a loopback HTTP server
//! answering the OpenAI frames the provider parses — because a test cannot have one and must not
//! reach for one. Nothing here talks to a provider, to 127.0.0.1:8080, or to a GGUF: the session
//! opens on the byte vocabulary.
//!
//! **One test, because the environment is process-wide.** `$LETIBOT_SOCKET`, `$LETIBOT_STORE` and
//! the config directories are what the bridge and the daemon read, and no argument stands in for
//! them; a second `#[test]` in this binary racing on them would be the defect rather than the
//! check. This file is a test binary of its own for that reason.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_bridge::rows;
use letibot_bridge::tools::Bridge;
use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::{Daemon, Dialect, Parts, Sessions};
use letibot_sessionlog::registry::Registry;
use serde_json::{Value, json};

/// The brief, and the marker its answer carries — so a row can be traced to the turn that
/// produced it rather than to *an* answer.
const BRIEF: &str = "brief-marker: write the long thing";
const LONG_MARK: &str = "long-answer-marker";
const SECOND_LINE: &str = "second-marker: and now the short thing";
const SECOND_ANSWER: &str = "second-answer-marker: done";

/// **The answer that is longer than the cap**, so the cap is what cuts it and nothing else. It
/// carries its marker at the FRONT, because the row a reader gets is the first `cap` characters
/// of it and a marker at the back would be evidence of nothing.
fn long_answer() -> String {
    format!("{LONG_MARK}: {}", "y".repeat(9_000))
}

/// Generous on purpose: every wait here is for something a correct build produces in
/// milliseconds, and the assertion is what happened, not how fast.
const PATIENCE: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// A scratch directory, and the fake model
// ---------------------------------------------------------------------------

/// A directory that removes itself — a test that leaks a store per run fills the disk, and the
/// run that finds out is not this one.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = letibot_sessionlog::testing::scratch_root()
            .join(format!("letibot-mcp-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir(path)
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

/// A fake provider on a loopback port: the n-th request is answered with the n-th entry of the
/// script, and the last entry answers every request after it.
struct Fake {
    url: String,
}

impl Fake {
    fn serve(script: Vec<String>) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut turn = 0usize;
            loop {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let _ = read_request(stream.try_clone().expect("clone the socket"));
                let answer = script
                    .get(turn)
                    .or_else(|| script.last())
                    .cloned()
                    .unwrap_or_else(|| "done".to_string());
                turn += 1;
                let _ = write_sse(stream, &answer);
            }
        });
        Fake {
            url: format!("http://127.0.0.1:{port}/v1/chat/completions"),
        }
    }
}

/// Drain one HTTP request.
fn read_request(mut s: TcpStream) -> String {
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
    match head_end {
        Some(at) => String::from_utf8_lossy(&acc[at + 4..]).into_owned(),
        None => String::from_utf8_lossy(&acc).into_owned(),
    }
}

/// One answer, in the provider's wire shape: a delta, a finish reason, the usage block and
/// `[DONE]`.
fn write_sse(mut stream: TcpStream, text: &str) -> std::io::Result<()> {
    let mut body = String::new();
    body.push_str(&format!(
        "data: {}\n\n",
        json!({"choices":[{"delta":{"content": text}}]})
    ));
    body.push_str(&format!(
        "data: {}\n\n",
        json!({"choices":[{"delta":{},"finish_reason":"stop"}]})
    ));
    body.push_str(&format!(
        "data: {}\n\n",
        json!({"choices":[],"usage":{
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

// ---------------------------------------------------------------------------
// The daemon
// ---------------------------------------------------------------------------

/// The daemon, the fake model and the config they share.
struct World {
    dir: TempDir,
    worktree: PathBuf,
    socket: PathBuf,
    registry: Arc<Registry>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl World {
    fn start() -> World {
        let dir = TempDir::new("transcript");
        let root = dir.path();
        // **Nothing of the operator's may reach this session**: a key in their
        // `providers.toml` or a mode row in their store would decide what answers.
        std::fs::create_dir_all(root.join("config/letibot")).expect("config dir");
        // SAFETY: this test binary runs one test, and the variables are set before the
        // daemon, any thread that reads config, or the bridge exists.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
            std::env::set_var("XDG_DATA_HOME", root.join("data"));
            std::env::set_var("LETIBOT_STORE", root.join("sessions.db"));
            std::env::remove_var("DEEPSEEK_API_KEY");
            std::env::remove_var("GLM_API_KEY");
            std::env::remove_var("LETIBOT_MODE");
            std::env::remove_var("LETIBOT_SOCKET");
        }
        let worktree = root.join("worktree");
        std::fs::create_dir_all(&worktree).expect("worktree");

        let fake = Fake::serve(vec![long_answer(), SECOND_ANSWER.to_string()]);
        std::fs::write(
            root.join("config/letibot/providers.toml"),
            format!(
                "# Written by crates/bridge/tests/transcript.rs — a fake on loopback.\n\
                 [deepseek]\nkey = \"sk-test\"\nurl = \"{}\"\n",
                fake.url
            ),
        )
        .expect("providers.toml");

        let socket = root.join("daemon.sock");
        // The bridge finds a folder's daemon by the workspace's hash, so `$LETIBOT_SOCKET`
        // names this one — the same override the shell honours.
        unsafe { std::env::set_var("LETIBOT_SOCKET", &socket) };
        // The operator's hosts file, with nothing in it: this file is about rows, and a host
        // this box does not have would only make `leticode_start` refuse.
        std::fs::write(
            root.join("config/letibot/hosts.toml"),
            "# Written by crates/bridge/tests/transcript.rs — no hosts.\n",
        )
        .expect("hosts.toml");
        unsafe { std::env::set_var("LETIBOT_HOSTS", root.join("config/letibot/hosts.toml")) };

        let mut cfg = Config::for_this_box(&worktree);
        cfg.dialect = Dialect::Qwen;
        cfg.seat = Seat::Leticode;
        // **No exec backend**: a delegated cgroup is apparatus this box may not have, and
        // nothing in this file runs a command. The writable backend is enough for a turn that
        // only talks and for a session that only needs a point.
        cfg.allow_bash = false;
        // The byte vocabulary: a session whose turns go to a provider needs no GGUF.
        cfg.vocab_gguf = None;
        cfg.provider = Some(ProviderConfig {
            name: "deepseek".into(),
            model: Some("deepseek-chat".into()),
            api_key: None,
            thinking: false,
        });
        cfg.web_search = None;
        cfg.permission = Vec::new();
        cfg.http_retries = 0;
        // **A window the prompt fits inside.** The byte vocabulary counts every byte of the
        // system prompt and the tool schemas as a token, so a session's message zero is ~54k by
        // that measure; a 32k window made the harness try to compact before every turn, which is
        // a failed compaction per turn and nothing this file is about.
        cfg.context_window = Some(200_000);
        cfg.store = Some(root.join("sessions.db"));
        cfg.socket = socket.clone();
        cfg.session_id = "transcript-daemon-session".into();

        let parts: &'static Parts = Box::leak(Box::new(
            Parts::load(&cfg).expect("a provider session opens on the byte vocabulary"),
        ));
        *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();

        let session = cfg.session_id.clone();
        let registry = Registry::new();
        registry
            .create(session, "", Sessions::wiring(&cfg))
            .expect("a fresh registry takes the session");
        // Bound HERE rather than on the worker thread: the bridge decides whether to start a
        // daemon by connecting, and a socket that appears a moment later would be a race.
        let daemon = Daemon::serve(registry.clone(), &socket).expect("the socket binds");
        // Where this daemon's tools run, written where harnessd writes it: `host` is this
        // box, and the record is what `leticode_start` compares its own request against.
        letibot_sessionlog::placement::write(&socket, "host").expect("the record");
        let worker_registry = registry.clone();
        let worker = std::thread::spawn(move || {
            let mut sessions = Sessions::open_first(parts, cfg, worker_registry)
                .expect("the daemon opens its first session");
            daemon.run(&mut sessions, |_, _| {});
            daemon.shutdown();
        });

        World {
            dir,
            worktree,
            socket,
            registry,
            worker: Some(worker),
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // The registry's close is what ends `Daemon::run`, so this is the orderly stop.
        self.registry.close();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        let _ = &self.dir;
    }
}

/// Poll `status` until it says what we are waiting for, or the patience runs out.
fn until<F>(bridge: &Bridge, session: &str, what: &str, mut ready: F) -> Value
where
    F: FnMut(&Value) -> bool,
{
    let deadline = Instant::now() + PATIENCE;
    let mut last;
    loop {
        last = match bridge.status(session) {
            Ok(v) => v,
            Err(e) => panic!("status failed while waiting for {what}: {e}"),
        };
        if ready(&last) {
            return last;
        }
        assert!(
            Instant::now() < deadline,
            "waited {PATIENCE:?} for {what}; the session says {last}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The row whose `text` contains `marker`, and where it is in the answer — the assertions are
/// about order and content, so they are made against the rows rather than against indexes this
/// file would be guessing at.
fn row_with(rows: &[Value], marker: &str) -> (usize, Value) {
    rows.iter()
        .enumerate()
        .find(|(_, r)| {
            r["text"].as_str().is_some_and(|t| t.contains(marker))
                || r["payload"].as_str().is_some_and(|t| t.contains(marker))
        })
        .map(|(at, r)| (at, r.clone()))
        .unwrap_or_else(|| panic!("no row carries `{marker}`: {rows:#?}"))
}

#[test]
fn a_sessions_conversation_comes_back_as_rows_from_a_cursor_with_the_long_text_whole() {
    let world = World::start();
    assert!(
        letibot_sessionlog::client::HeadClient::attach(
            &world.socket,
            &world.registry.list()[0].session_id,
            0,
            "remote",
            "probe",
            Default::default(),
        )
        .is_ok(),
        "the daemon must be serving before the bridge is asked to start anything"
    );
    let bridge = Bridge::new();

    // --- a turn runs, and its rows are there afterwards -----------------------
    let started = bridge
        .start(
            &world.worktree.display().to_string(),
            BRIEF,
            Some("deepseek/deepseek-chat"),
            "writes-allowed",
            None,
        )
        .expect("the bridge starts a session");
    let session = started["session"]
        .as_str()
        .expect("a session id")
        .to_string();
    until(&bridge, &session, "the brief's turn to end", |v| {
        v["state"].as_str() == Some("idle")
            && v["last_message"]
                .as_str()
                .unwrap_or_default()
                .contains(LONG_MARK)
    });

    let first = bridge
        .transcript(&session, None, None, false)
        .expect("the session's rows");
    assert_eq!(first["session"], json!(session));
    assert_eq!(first["cap"], json!(rows::CAP), "the cap is stated: {first}");
    assert_eq!(first["reasoning"], json!(false));
    let list = first["rows"].as_array().expect("rows").clone();
    assert!(!list.is_empty(), "a turn ran and left no rows: {first}");

    // **The user row first, then the answer** — the order the conversation is in, and the one
    // thing a tailing reader depends on.
    let (at_brief, brief_row) = row_with(&list, "brief-marker");
    let (at_answer, answer_row) = row_with(&list, LONG_MARK);
    assert!(
        at_brief < at_answer,
        "the brief is before its answer: {list:#?}"
    );
    assert_eq!(brief_row["kind"], json!("user"), "{brief_row}");
    assert_eq!(brief_row["speaker"], json!("operator"), "{brief_row}");
    assert!(
        brief_row["text"]
            .as_str()
            .unwrap_or_default()
            .contains("brief-marker: write the long thing"),
        "the prompt comes back whole: {brief_row}"
    );
    assert_eq!(answer_row["kind"], json!("assistant"), "{answer_row}");

    // --- the long answer is whole up to the cap -------------------------------
    let text = answer_row["text"].as_str().expect("the text");
    assert_eq!(
        text.chars().count(),
        rows::CAP,
        "the answer is cut at the cap and at nothing shorter"
    );
    assert!(
        text.starts_with(LONG_MARK),
        "and from the front: {text:.40}"
    );
    assert_eq!(
        answer_row["cut"],
        json!(true),
        "a row that was cut says so: {answer_row}"
    );

    // --- the cursor reads on, and returns only what is new ---------------------
    let next = first["next"].as_u64().expect("a cursor");
    assert_eq!(next, first["rows_total"].as_u64().unwrap_or(0));
    let nothing = bridge
        .transcript(&session, Some(next), None, false)
        .expect("a read from the cursor");
    assert_eq!(
        nothing["rows"].as_array().map(Vec::len),
        Some(0),
        "nothing has happened since: {nothing}"
    );
    assert_eq!(nothing["next"], json!(next), "the cursor holds still");

    bridge.send(&session, SECOND_LINE).expect("a line in");
    until(&bridge, &session, "the second turn to end", |v| {
        v["state"].as_str() == Some("idle")
            && v["last_message"]
                .as_str()
                .unwrap_or_default()
                .contains(SECOND_ANSWER)
    });

    let after = bridge
        .transcript(&session, Some(next), None, false)
        .expect("the rows since the cursor");
    let rows = after["rows"].as_array().expect("rows").clone();
    assert!(
        rows.len() >= 2,
        "the second turn's prompt and answer, and nothing older: {after}"
    );
    for row in &rows {
        assert!(
            row["i"].as_u64().unwrap_or(0) >= next,
            "a row the cursor had already read came back: {row} (cursor {next})"
        );
    }
    let (_, said) = row_with(&rows, "second-marker");
    assert_eq!(said["kind"], json!("user"), "{said}");
    let (_, answered) = row_with(&rows, SECOND_ANSWER);
    assert_eq!(answered["kind"], json!("assistant"), "{answered}");
    assert!(
        !rows
            .iter()
            .any(|r| r["text"].as_str().is_some_and(|t| t.contains(LONG_MARK))),
        "the first turn's rows are behind the cursor: {rows:#?}"
    );

    // --- and a WATCHED session answers the same read --------------------------
    // The operator's pane follows a package it did not start as often as one it did, and a read
    // is not an act on somebody's session: a `watch_only` head takes the same rows.
    let watcher = Bridge::new();
    watcher
        .attach(&world.worktree.display().to_string(), &session, true)
        .expect("a watching head attaches");
    let watched = watcher
        .transcript(&session, None, None, false)
        .expect("a watched session's rows");
    assert_eq!(
        watched["rows_total"], after["rows_total"],
        "the watcher reads the whole conversation, both turns: {watched}"
    );
    assert_eq!(
        watched["rows"][0]["i"],
        json!(0),
        "from the session's first row: {watched}"
    );
    // And it still may not act on the session — the cap is on the three verbs, not on a read.
    let refused = watcher
        .set_mode(&session, "read-only")
        .expect_err("a watcher does not change a session's point");
    assert!(refused.contains("watched, not held"), "{refused}");
}
