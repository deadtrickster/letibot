//! Standing notes against a real harness: the moments the head of the prompt may move -
//! session open, and every base rebuild. Nothing delivers a note mid-conversation

use letibot_harnessd::config::Config;
use letibot_harnessd::standing_notes;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;

mod support;

/// A directory that removes itself, so the fixtures here never compete for a
/// name under `/tmp` (the same shape `compact.rs` rolls for itself).
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let p = letibot_sessionlog::testing::scratch_root()
            .join(format!("harnessd-standing-{name}-{}", std::process::id()));
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

/// A box with no instruction file of its own.
fn no_box(scratch: &std::path::Path) -> letibot_harnessd::leticode_config::BoxDirs {
    letibot_harnessd::leticode_config::BoxDirs {
        config: scratch.join("box-config"),
        home: scratch.join("box-home"),
    }
}

/// A session over a workspace with a nonce in its `AGENTS.md`, composed the way `Sessions`
/// composes one: `seat_overlay` set, so the harness composes the prompt at open for its
/// seated tools, notes included. The store's directory comes back with the harness
fn opened(workspace: &std::path::Path, nonce: &str) -> (Harness, TempDir) {
    opened_over(
        workspace,
        nonce,
        letibot_harnessd::leticode_config::BoxDirs::default(),
        letibot_tools::builtins::skill::SkillDirs::default(),
    )
}

/// [`opened`] over a box the caller names: the instruction directories and the skill roots
/// are the config's, so a fixture that is ABOUT them hands its own and one that is not gets
/// nothing.
fn opened_over(
    workspace: &std::path::Path,
    nonce: &str,
    box_dirs: letibot_harnessd::leticode_config::BoxDirs,
    box_skills: letibot_tools::builtins::skill::SkillDirs,
) -> (Harness, TempDir) {
    let dir = TempDir::new("store");
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = Dialect::Qwen;
    cfg.http_retries = 0;
    cfg.store = Some(dir.path().join("sessions.db"));
    cfg.session_id = format!("standing-{nonce}");
    cfg.vocab_gguf = support::vocab::gguf();
    cfg.box_dirs = box_dirs;
    cfg.box_skills = box_skills;
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    std::fs::write(workspace.join("AGENTS.md"), format!("note-{nonce}-one\n"))
        .expect("fixture AGENTS.md");
    cfg.seat_overlay = Some(letibot_harnessd::config::SeatOverlay::default());
    let hub = Hub::new(&cfg.session_id);
    (
        Harness::open(&parts, cfg, hub).expect("the session must open"),
        dir,
    )
}

/// The box a session reads is the one its config names, and an unconfigured one reads
/// nothing of the machine it runs on. A fixture's prompt has to be a function of its own
/// files
#[test]
fn the_box_a_session_reads_is_the_one_its_config_names() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let ws = TempDir::new("ws-box");
    let bx = TempDir::new("box");
    let home = bx.path().join("home");
    let skills = home.join(".claude/skills");
    std::fs::create_dir_all(skills.join("box-skill")).expect("the box's skill dir");
    std::fs::write(
        home.join(".claude")
            .join(letibot_harnessd::leticode_config::CANONICAL_INSTRUCTION_FILE),
        "a rule only this box has: box-nonce-one\n",
    )
    .expect("the box's instruction file");
    std::fs::write(
        skills.join("box-skill/SKILL.md"),
        "---\nname: box-skill\ndescription: a skill only this box has\n---\n\nthe body\n",
    )
    .expect("the box's skill");

    // The built-in skills are in every count; the box adds exactly one.
    let (nobody, _store) = opened(ws.path(), "nobody");
    let bare = nobody.config().system.clone();
    let built_in = bare
        .split("pinned of ")
        .nth(1)
        .and_then(|t| t.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(0);

    let (mine, _store) = opened_over(
        ws.path(),
        "mine",
        letibot_harnessd::leticode_config::BoxDirs {
            config: bx.path().join("config"),
            home,
        },
        letibot_tools::builtins::skill::SkillDirs {
            installed: vec![skills],
            ..Default::default()
        },
    );
    let system = &mine.config().system;
    assert!(
        system.contains("box-nonce-one"),
        "the instruction file the config points at is in message zero: {system}"
    );
    assert!(
        system.contains(&format!("pinned of {}", built_in + 1)),
        "and the skills the config's roots hold are counted in the prompt: {system}"
    );

    let system = &bare;
    assert!(
        !system.contains("box-nonce-one")
            && !system.contains(&format!("pinned of {}", built_in + 1)),
        "a session whose config names no box reads none of one: {system}"
    );
}

/// An `AGENTS.md` edited between the compose and the rebuild shows up in the new base.
/// Driven at the seam every fork computes its next prefix through, without a model server
/// `reseat_target` is the offline-drivable half of a compaction by its own doc.
#[test]
fn an_edited_agents_md_lands_in_the_next_base() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let ws = TempDir::new("ws-rebuild");
    let (mut h, _store) = opened(ws.path(), "rebuild");

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
    // And nothing else moved. Strip the section from both and the prompts are byte-identical
    // the sections, `system_extra`, the budget arithmetic - the whole frozen-input rule
    // `compose_for_seat` set keeps its side of the bargain and only the notes moved.
    assert_eq!(
        standing_notes::replace(&next.system, None),
        standing_notes::replace(&before, None),
        "everything outside the notes section is carried through unchanged"
    );

    // A fabric block appended after the section survives the swap - the
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

/// A note the `notes` tool writes is one the reader injects - round-tripped, not assumed.
/// The tool lives in `letibot-tools` and the reader here, joined only by a trait
/// (`NotesScope`) naming the notes checkout, which is outside the workspace.
#[test]
fn a_note_the_tool_writes_is_one_the_reader_injects() {
    struct Scope(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf);
    impl letibot_tools::builtins::notes::NotesScope for Scope {
        fn workspace(&self) -> std::path::PathBuf {
            self.0.clone()
        }
        fn global_dir(&self) -> std::path::PathBuf {
            self.1.clone()
        }
        fn project_dir(&self) -> std::path::PathBuf {
            self.2.join("proj")
        }
        fn repo(&self) -> Option<std::path::PathBuf> {
            Some(self.2.clone())
        }
    }
    let ws = TempDir::new("roundtrip");
    let global = TempDir::new("roundtrip-global");
    let repo = TempDir::new("roundtrip-notes");
    let scope = std::sync::Arc::new(Scope(
        ws.path().to_path_buf(),
        global.path().to_path_buf(),
        repo.path().to_path_buf(),
    ));
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

    // And the reader - the same `section` the prompt is composed with - picks
    // it up, verbatim, under its path.
    let vocab = letibot_tokencore::Vocab::bytes([], []);
    let files =
        standing_notes::Reader::new(&*rt.backend, ws.path()).with_notes(repo.path().join("proj"));
    let s = standing_notes::section(&files, global.path(), &no_box(global.path()), &vocab)
        .expect("the reader sees the note");
    assert!(
        s.contains("proj/qwen-tail.md"),
        "under the path the tool said it wrote: {s}"
    );
    assert!(
        !ws.path().join(".letibot").exists(),
        "the project tree stays clean"
    );
    assert!(
        s.contains("beats truncating it"),
        "the text, verbatim, inside the injected block: {s}"
    );
    assert!(
        s.contains("may be outdated"),
        "the injected block carries the historical caveat too: {s}"
    );

    // And an append reaches the reader as well - the loop is round in both
    // directions of the verb set, not just on the happy path.
    let r = call(r#"{"action":"append","name":"qwen-tail","text":"re-measured: still true"}"#);
    assert!(r.is_grounded(), "{}", r.render());
    let s = standing_notes::section(&files, global.path(), &no_box(global.path()), &vocab).unwrap();
    assert!(
        s.contains("re-measured: still true"),
        "the appended text is what the reader reads: {s}"
    );
}
