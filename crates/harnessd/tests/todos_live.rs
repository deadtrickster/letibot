//! The todo path, end to end, against the live server: the model writes the
//! plan, the store holds it, the log announces it.
//!
//! The offline files prove every link separately — the tool's behaviour
//! (`letibot-tools`), the store round-trip (`letibot-tokencore`), the restore at
//! open (`todos.rs`), the event's wire shape (`letibot-sessionlog`). What none of
//! them can prove is the chain: a real model, told to plan, actually calling
//! `todo_write` through the gated runtime, the harness flushing at the round
//! boundary, and a head attaching afterwards finding both the store row and the
//! event in its backlog.
//!
//! # The one rule on this box
//!
//! The server on `127.0.0.1:8080` is a singleton and the GLM one is already
//! serving — this session runs on it. **Never start a second model server to
//! make a test pass**: the services evict each other, and the eviction takes down
//! whatever else is running on the same endpoint. This test uses the GLM dialect
//! and the GLM vocabulary, which is what the server is already holding, so it
//! starts nothing and evicts nothing. The serving preflight is the gate, and when
//! nothing answers at all the test SKIPS rather than failing — see
//! `letibot_turn::serving::skip_live_test`, and `LETIBOT_REQUIRE_MODEL` to refuse
//! the skip. Five minutes of `SalvageExhausted` against a closed port is what that
//! guard replaced.

use std::sync::OnceLock;

use letibot_harnessd::config::Config;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::Caps;
use letibot_tokencore::store::{Store, TodoStatus};

const GLM_GGUF: &str =
    "/home/dead/models/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf";

#[test]
fn a_live_todo_write_reaches_the_store_and_the_log() {
    let dir = TempDir::new("harnessd-todos-live");
    let path = dir.path().join("sessions.db");
    let session_id = "todos-live";
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Glm;
    cfg.model = "glm-5.3-flash".into();
    cfg.vocab_gguf = Some(
        std::env::var("LETIBOT_VOCAB_GGUF")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| GLM_GGUF.into()),
    );
    cfg.store = Some(path.clone());
    cfg.session_id = session_id.into();
    // The same escape hatch the other live files honour: with this set, the guard
    // below must check the endpoint the test will actually use, not the default.
    if let Ok(url) = std::env::var("LETIBOT_COMPLETION_URL") {
        let (h, p) = url.rsplit_once(':').expect("HOST:PORT");
        cfg.endpoint = letibot_turn::Endpoint::new(h, p.parse().expect("port"));
    }

    // **NO SERVER, NO TEST — and the guard goes BEFORE the vocabulary assertion.**
    //
    // MEASURED with the endpoint on a dead port: this is the one target in the whole
    // workspace that still failed, `FAILED. 0 passed; 1 failed` in 14.73 s, panicking
    // at `the turn: Turn(SalvageExhausted { streak: 4 })` — four retries against
    // nothing. Every other live file either uses a canned server or needs no endpoint,
    // which `cargo test --workspace --no-fail-fast` established rather than guessed.
    //
    // It goes FIRST because on a machine that is not this box the file fails for a
    // second, louder reason: `GLM_GGUF` is an absolute path under `/home/dead`, so the
    // `<GLM_GGUF>` assertion would fire before anything got as far as the endpoint.
    // A skip has to be reached before the assertions it is skipping past.
    if letibot_turn::serving::skip_live_test(&cfg.endpoint, "This test") {
        return;
    }

    // **The vocabulary is apparatus whether or not a server answered**, so it is
    // the same skip rather than an assert. This used to `assert!` — with a message
    // telling the reader how to make it SKIP, which is a sentence that should have
    // been the code. MEASURED: with a server up and the override pointing at
    // nothing, the assert fired and reported absent apparatus as a failed
    // assertion; on a runner there is no server, so the earlier guard caught it and
    // the inconsistency stayed hidden.
    //
    // **It checks the file the CONFIG resolves, not the constant.** The first version
    // of this guarded `GLM_GGUF` while `cfg.vocab_gguf` above prefers
    // `LETIBOT_VOCAB_GGUF` — so with the override pointing at nothing the guard
    // PASSED and `Parts::load` then failed with `no vocabulary GGUF at
    // /nonexistent/…`. A guard that resolves a different path from the thing it
    // guards can be true while the test still cannot run, which is the whole defect
    // it is there to prevent. MEASURED, and `compact_live.rs` had it too.
    let gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| GLM_GGUF.into());
    let Some(_) = letibot_tokencore::apparatus::present(
        &format!("a GLM vocabulary GGUF ({})", gguf.display()),
        gguf.is_file(),
    ) else {
        return;
    };
    // The server, before the work: a vocabulary on disk is not a model on the
    // endpoint, and only the second one can answer this. `compact_live.rs` has had
    // this guard for a while; this file asserted the same precondition in prose.
    if !glm_is_served() {
        return;
    }
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let hub = Hub::new(session_id);
    let mut h = Harness::open(&parts, cfg.clone(), hub.clone()).expect("the session must open");
    // The log's head now that the session has announced itself: a resume from
    // here is a gap replay of everything the turn appends. (`attach` with
    // since_seq 0 is a snapshot attach — empty backlog by design — so the mark
    // must be taken after open, not before.)
    // A first head, so the log has a nonzero mark for the late joiner to resume
    // from: `attach` itself appends HeadAttached, and `Harness::open` appends
    // nothing — a `since` of 0 would mean a snapshot attach, whose backlog is
    // empty by design, and the assertion below would be testing nothing.
    let _pre = hub.attach("pre", "test", Caps::default(), 0);
    let since = hub.head_seq();
    assert!(since > 0, "the pre-attach must have moved the log's head");

    let r = h
        .submit(
            "Call todo_write now with exactly one entry: content \"write the report\", \
             status \"pending\". Then reply with just: done",
        )
        .expect("the turn");
    eprintln!("  model -> {:?}", r.text.trim());
    eprintln!("  rounds {}, tool calls {}", r.rounds, r.tool_calls);

    // The store holds what the model wrote — the resume guarantee, and the thing
    // a pane's bootstrap read is served from.
    let store = Store::open(&path).expect("reopening");
    let todos = store.todos(session_id).expect("reading the todo row");
    eprintln!("  store -> {todos:?}");
    assert_eq!(todos.len(), 1, "the whole list, as written: {todos:?}");
    assert_eq!(todos[0].content, "write the report");
    assert_eq!(todos[0].status, TodoStatus::Pending);
    // And the harness's board agrees with the store — the flush did both halves.
    assert_eq!(h.todo_list().len(), 1);

    // A head attaching now is told, from the log: the announcement is durable,
    // not a live-only fan-out that a late joiner never sees.
    let att = hub.attach("tui", "test", Caps::default(), since);
    eprintln!(
        "  since {} -> head now {}; resume {:?}, backlog {}",
        since,
        hub.head_seq(),
        att.resumed_from,
        att.backlog.len()
    );
    let announced = att
        .backlog
        .iter()
        .any(|e| matches!(e.event, SessionEvent::TodosUpdated { .. }));
    assert!(
        announced,
        "no TodosUpdated in the backlog of {} envelope(s); a head attaching after \
         the write would open the pane empty until the next write",
        att.backlog.len()
    );
}

/// **Is the box actually serving the model this test renders for?**
///
/// It was not, and this file had no way to notice. Its header says *"The serving
/// preflight is the gate"* — and the gate was a vocabulary FILE existing, which is a
/// different question from a server being up. MEASURED on this box: serving
/// `qwen-3.8-27b` while this test rendered GLM token ids, the turn failed with
/// `Turn(SalvageExhausted { streak: 4 })` in 21.67 s — a symptom that names nothing
/// about the cause, because the ids were out of range for the other vocabulary
/// rather than because the salvage logic is wrong.
///
/// **`compact_live.rs` documents exactly this incident at length** (a GLM-vocabulary
/// runner against a qwen server, `262194 tokens exceeds the available context size
/// 262144`, *51 minutes* to discover) and guards against it. This file asserts the
/// same precondition in prose and not in code — the same gap as the `GLM_GGUF` guard
/// that resolved a different path from the config, in the same file.
///
/// So: refuse by name, in seconds, saying `THIS IS NOT A PASS`.
fn glm_is_served() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        let want = std::env::var("LETIBOT_MODEL_ALIAS").unwrap_or_else(|_| "glm-5.3-flash".into());
        match letibot_turn::serving::served_model(&cfg_endpoint()) {
            Ok(served) if letibot_turn::serving::matches(&served, &want) => true,
            Ok(served) => {
                eprintln!(
                    "SKIPPED: {} is serving `{served}`, and this test renders the GLM dialect for \
                     `{want}` — the control tokens would not resolve, so nothing was run and THIS \
                     IS NOT A PASS. Start {want}, or set LETIBOT_MODEL_ALIAS.",
                    cfg_endpoint().authority()
                );
                false
            }
            Err(e) => {
                eprintln!(
                    "SKIPPED: could not ask {}/props ({e}), so nothing was run and THIS IS NOT A \
                     PASS.",
                    cfg_endpoint().authority()
                );
                false
            }
        }
    })
}

/// The endpoint this test would use, without building a whole config.
fn cfg_endpoint() -> letibot_turn::Endpoint {
    match std::env::var("LETIBOT_COMPLETION_URL") {
        Ok(url) => {
            let (h, p) = url.rsplit_once(':').expect("HOST:PORT");
            letibot_turn::Endpoint::new(h, p.parse().expect("port"))
        }
        Err(_) => letibot_turn::Endpoint::new("127.0.0.1", 8080),
    }
}

/// A directory that removes itself, because a test that leaks a store per run
/// eventually fills the disk and the run that finds out is not this one.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
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

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
