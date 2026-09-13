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
//! starts nothing and evicts nothing. The serving preflight is the gate.

use letibot_harnessd::config::Config;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::Caps;
use letibot_sessionlog::SessionEvent;
use letibot_tokencore::store::{Store, TodoStatus};

const GLM_GGUF: &str =
    "/home/dead/models/glm-5.3-flash/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf";

#[test]
fn a_live_todo_write_reaches_the_store_and_the_log() {
    assert!(
        std::path::Path::new(GLM_GGUF).is_file(),
        "no GLM vocabulary GGUF at {GLM_GGUF}"
    );
    let dir = TempDir::new("harnessd-todos-live");
    let path = dir.path().join("sessions.db");
    let session_id = "todos-live";
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Glm;
    cfg.model = "glm-5.3-flash".into();
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| GLM_GGUF.into());
    cfg.store = Some(path.clone());
    cfg.session_id = session_id.into();
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
