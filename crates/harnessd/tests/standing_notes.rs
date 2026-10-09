//! Standing notes against a real harness: the moments the head of the prompt
//! may move — session open, and every base rebuild. Nothing delivers a note
//! mid-conversation; that path was removed on the operator's ruling, so a
//! note edited between rebuilds lands at the next one, not on the next turn.
//!
//! The assembly itself — the sources, the order, the budget, the digest — is
//! asserted as unit tests on `standing_notes`, which needs no harness. What
//! needs one is the part the operator actually asked for:
//!
//! 1. **The re-read at a base rebuild.** `reseat_target` re-reads the files,
//!    so an `AGENTS.md` edited between two compactions lands in the new
//!    base's message 0 — and only the notes move, with everything the
//!    composed prompt carries beside them carried through byte for byte,
//!    including a fabric block appended after the section the way
//!    `Sessions::with_fabric` leaves it.
//!
//! It needs the vocabulary GGUF and not the model server: nothing here runs a
//! turn. Same gate as `compact.rs`, for the same reason.

use letibot_harnessd::config::Config;
use letibot_harnessd::standing_notes;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;

/// A directory that removes itself, so the fixtures here never compete for a
/// name under `/tmp` (the same shape `compact.rs` rolls for itself).
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let p =
            std::env::temp_dir().join(format!("harnessd-standing-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("temp dir");
        TempDir(p)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A session over a workspace with a nonce in its `AGENTS.md`, composed the
/// way `Sessions` composes one: the prompt is built before the harness opens,
/// notes included.
fn opened(workspace: &std::path::Path, nonce: &str) -> Harness {
    let dir = TempDir::new("store");
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = Dialect::Qwen;
    cfg.http_retries = 0;
    cfg.store = Some(dir.path().join("sessions.db"));
    cfg.session_id = format!("standing-{nonce}");
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    std::fs::write(workspace.join("AGENTS.md"), format!("note-{nonce}-one\n"))
        .expect("fixture AGENTS.md");
    cfg.compose_system_with_notes(&parts.vocab);
    let hub = Hub::new(&cfg.session_id);
    Harness::open(&parts, cfg, hub).expect("the session must open")
}

/// **An `AGENTS.md` edited between the compose and the rebuild shows up in the
/// new base.**
///
/// This is the operator's ask verbatim — *"make sure AGENTS.md reread after
/// each compaction"* — driven at the seam every fork computes its next prefix
/// through, without a model server: `reseat_target` is the offline-drivable
/// half of a compaction by its own doc.
#[test]
fn an_edited_agents_md_lands_in_the_next_base() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let ws = TempDir::new("ws-rebuild");
    let mut h = opened(ws.path(), "rebuild");

    // Nothing changed on disk and the seated tools are the same, so the next
    // base is the prompt being spoken: no fork, no new prefix row.
    assert!(
        h.reseat_target().expect("unchanged").is_none(),
        "an unchanged file must not fork the conversation"
    );
    let before = h.config().system.clone();

    // The edit. A nonce, so the two generations of the file cannot be confused
    // by shared words.
    std::fs::write(ws.path().join("AGENTS.md"), "note-rebuild-two\n").expect("edit");

    let Some((next, _id)) = h.reseat_target().expect("changed") else {
        panic!("the edited AGENTS.md did not reach the next base");
    };
    assert!(
        next.system.contains("note-rebuild-two"),
        "the new generation is in message 0"
    );
    assert!(
        !next.system.contains("note-rebuild-one"),
        "the old generation is gone"
    );
    // **And nothing else moved.** Strip the section from both and the prompts
    // are byte-identical: the sections, `system_extra`, the budget arithmetic —
    // the whole frozen-input rule `compose_system` set keeps its side of the
    // bargain and only the notes moved.
    assert_eq!(
        standing_notes::replace(&next.system, None),
        standing_notes::replace(&before, None),
        "everything outside the notes section is carried through unchanged"
    );

    // **A fabric block appended after the section survives the swap** — the
    // shape `Sessions::with_fabric` leaves the prompt in, and the reason the
    // swap is by marker rather than a recompose.
    h.config_mut().system.push_str("\n\nfabric block goes last");
    std::fs::write(ws.path().join("AGENTS.md"), "note-rebuild-three\n").expect("edit");
    let Some((next, _)) = h.reseat_target().expect("changed again") else {
        panic!("the second edit did not reach the next base");
    };
    assert!(
        next.system.contains("note-rebuild-three"),
        "{}",
        next.system
    );
    assert!(
        next.system.ends_with("fabric block goes last"),
        "the fabric block, appended after the section, is untouched: {}",
        next.system
    );
}

/// **A note the `notes` tool writes is one the reader injects — round-tripped,
/// not assumed.**
///
/// The tool lives in `letibot-tools` and the reader here, joined only by a
/// trait (`NotesScope`) and a convention about directories. A note the tool
/// writes that the reader does not read is the bug this feature exists to
/// prevent: the model would be told it kept something, and the next session
/// would never see it. So this drives the REAL tool over a real workspace and
/// reads the result back through [`standing_notes::section`] — the same
/// function `compose_system_with_notes` and `reseat_target` call.
///
/// No vocabulary GGUF needed: the byte vocabulary counts exactly, and the
/// fixture is small.
#[test]
fn a_note_the_tool_writes_is_one_the_reader_injects() {
    struct Scope(std::path::PathBuf, std::path::PathBuf);
    impl letibot_tools::builtins::notes::NotesScope for Scope {
        fn workspace(&self) -> std::path::PathBuf {
            self.0.clone()
        }
        fn global_dir(&self) -> std::path::PathBuf {
            self.1.clone()
        }
    }
    let ws = TempDir::new("roundtrip");
    let global = TempDir::new("roundtrip-global");
    let scope = std::sync::Arc::new(Scope(ws.path().to_path_buf(), global.path().to_path_buf()));
    // The runtime the daemon builds for the tool: the tool itself, an
    // admitting gate (these tests are about the tool, not the gate), and a
    // writable host backend over the workspace.
    let mut reg = letibot_tools::runtime::Registry::new();
    reg.register(Box::new(letibot_tools::builtins::notes::NotesTool::new(
        scope,
    )))
    .expect("the notes tool registers");
    let backend =
        letibot_tools::backend::HostBackend::writable(ws.path()).expect("a writable fixture root");
    let rt = letibot_tools::runtime::ToolRuntime::new(reg, Box::new(backend))
        .with_gate(letibot_tools::testing::allow_all());
    let call = |args: &str| {
        let call = letibot_transcript::ToolCall {
            id: "call_roundtrip".into(),
            name: "notes".into(),
            arguments: args.into(),
        };
        let mut sink = letibot_tools::RecordingToolSink::new();
        rt.invoke("turn_1", &call, &mut sink)
    };
    let r = call(
        r#"{"action":"add","name":"qwen-tail","text":"clamping the compaction tail beats truncating it — measured on this box, 2026-10-09"}"#,
    );
    assert!(r.is_grounded(), "the add lands: {}", r.render());

    // And the reader — the same `section` the prompt is composed with — picks
    // it up, verbatim, under its path.
    let vocab = letibot_tokencore::Vocab::bytes([], []);
    let s = standing_notes::section(ws.path(), global.path(), &vocab)
        .expect("the reader sees the note");
    assert!(
        s.contains(".letibot/notes/qwen-tail.md"),
        "under the path the tool said it wrote: {s}"
    );
    assert!(
        s.contains("beats truncating it"),
        "the text, verbatim, inside the injected block: {s}"
    );
    assert!(
        s.contains("may be outdated"),
        "the injected block carries the historical caveat too: {s}"
    );

    // And an append reaches the reader as well — the loop is round in both
    // directions of the verb set, not just on the happy path.
    let r = call(r#"{"action":"append","name":"qwen-tail","text":"re-measured: still true"}"#);
    assert!(r.is_grounded(), "{}", r.render());
    let s = standing_notes::section(ws.path(), global.path(), &vocab).unwrap();
    assert!(
        s.contains("re-measured: still true"),
        "the appended text is what the reader reads: {s}"
    );
}
