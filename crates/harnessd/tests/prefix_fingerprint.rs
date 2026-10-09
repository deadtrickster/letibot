//! **A session's message 0 is frozen, and a daemon that would now compose a different
//! one has something to say about it.**
//!
//! Protocol §14: *"the tool schemas live in the stable prefix and a session's prefix is
//! fixed when it is created."* So a build that has since changed the system instructions,
//! the tool schemas or the seat hands a session a prompt it will never speak — and until
//! this existed nothing said so on any screen. The operator paid for that four ways in one
//! night; the case that started it was `todo_write`'s grown `target`, invisible to every
//! session seated before the schema grew.
//!
//! This file asserts the daemon's half — the comparison, and the sentence it leaves behind
//! — and three things the feature is worthless without:
//!
//! 1. **A session whose prefix is unchanged says NOTHING.** The notice has to be silent on
//!    the ordinary path — every daemon restart, every `--continue` — or it is a line people
//!    learn to skip, which is worse than no line at all.
//! 2. **A session whose prefix has changed is given a sentence**, in the words of the
//!    remedy: the session, when it was seated, what moved, and `/reseat` by name.
//! 3. **The standing notes are NOT in the fingerprint.** They are re-read at every base
//!    rebuild and every provider turn, so a fingerprint that covered them would report
//!    every session stale the moment a note was written. A note edited between two opens
//!    must produce no sentence at all — while the two composed prompts really do differ,
//!    which is what makes the assertion mean something.
//!
//! # Where the sentence GOES, and why that is a separate test
//!
//! Into the registry ([`Registry::set_stale_prefix`]), not onto the log, and the reason is
//! a property of the head: a warning a head finds in its snapshot is filed
//! `Placed::Before` — listed by `/notes`, counted by `/status`, and **not drawn**. Every
//! ordinary path opens the session before any head is on it, so a warning published here
//! reaches a head that will never draw it. The SENTENCE is therefore published by
//! `server::seat_in`, at attach, while the head is watching — asserted over a real socket
//! in `letibot-sessionlog`'s `stale_prefix.rs`, which is where the attach path lives.
//!
//! # What it needs
//!
//! The vocabulary GGUF and **not** the model server: opening a session and forking it are
//! store and ledger work, and a session is seeded here by `fork_to_summary` — the offline
//! half of a compaction — rather than by running a turn. `LETIBOT_VOCAB_GGUF` overrides
//! the path.

use std::sync::Arc;

use letibot_harnessd::config::Config;
use letibot_harnessd::config::Seat;
use letibot_harnessd::harness::ForkTail;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::Registry;
use letibot_turn::CompactionOutcome;

/// A directory that removes itself, so the fixtures here never compete for a name under
/// `/tmp` (the same shape `standing_notes.rs` rolls for itself).
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let p = std::env::temp_dir().join(format!(
            "harnessd-prefix-{name}-{}-{}",
            std::process::id(),
            letibot_harnessd::config::now_ns()
        ));
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

fn config(store: &std::path::Path, session_id: &str, workspace: &std::path::Path) -> Config {
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = Dialect::Qwen;
    // **These fixtures supply what they assert about, and nothing the laptop happens to
    // be configured for.** `Config::for_this_box` reads the operator's own
    // `~/.config/letibot` — `providers.toml` for a search key, `permission.json` for the
    // preapproved list — and a Brave key would seat `web_search` in one run and not the
    // next. Same discipline (and the same two fields) as `wired.rs`.
    cfg.web_search = None;
    cfg.permission = Vec::new();
    // No model server is reached by anything here; an endpoint that does not answer is
    // the expected state and not one worth a retry ladder.
    cfg.http_retries = 0;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg
}

/// **The operator's project modes are not this test's business.** A resumed session
/// applies the row for its workspace, and on this box that is whatever the laptop happens
/// to be configured for — the same discipline `resume.rs` states at its own `empty_modes`.
fn empty_modes(parts: Parts) -> Parts {
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    parts
}

/// Open one session **in a registry that holds it**, which is what a daemon does and what
/// the sentence needs: the harness leaves it in the entry, and an entry is only there for a
/// session the registry knows. `Harness::open` builds a throwaway registry, so a test that
/// wants to read what the harness decided has to pass its own — `open_with_registry` is
/// public for exactly this.
fn opened(cfg: &Config, parts: &Parts) -> (Harness, Arc<Registry>) {
    let hub = Hub::new(&cfg.session_id);
    let registry = Registry::of(hub.clone());
    let h = Harness::open_with_registry(parts, cfg.clone(), hub, None, None, registry.clone())
        .expect("the session must open");
    (h, registry)
}

/// **The sentence an attaching head would be told**, or `None` for silence. This is the
/// same read `server::seat_in` makes before it publishes.
fn stale(registry: &Registry, session_id: &str) -> Option<String> {
    registry.stale_prefix(session_id)
}

/// **One row, so the next open RESUMES.** `resumed` requires a session with rows behind
/// it, and a fork is how a session gets one without a model: `fork_to_summary` writes a
/// new transcript under the prefix the conversation is already speaking.
fn seed(h: &mut Harness) {
    let outcome = CompactionOutcome {
        turn_id: format!("{}#seed", h.transcript_id()),
        summary: "the work so far, written down so this session has a row to resume".into(),
        tool_calls: 0,
        truncated: false,
        cached_tokens: 0,
        reusable: 0,
        generated_tokens: 0,
    };
    h.fork_to_summary(&outcome, None, None, ForkTail::NONE)
        .expect("the seed fork must land");
}

/// **The unchanged case says nothing, and the changed one names the remedy.**
///
/// Both halves in one test on purpose: the false positive and the false negative are the
/// two ways this feature fails, and a test that asserted only one of them would pass on a
/// daemon that reported every session stale.
#[test]
fn an_unchanged_prompt_is_silent_and_a_changed_one_names_reseat() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("instructions");
    let ws = TempDir::new("ws-instructions");
    let store = dir.path().join("sessions.db");
    let session_id = "prefix-instructions";
    let cfg = config(&store, session_id, ws.path());
    let parts = empty_modes(Parts::load(&cfg).expect("the vocabulary must load"));

    {
        let (mut h, _registry) = opened(&cfg, &parts);
        seed(&mut h);
    }

    // (1) The same composition, opened again: a resume, and nothing to say about it.
    let (h, registry) = opened(&cfg, &parts);
    assert!(
        h.resumed().is_some(),
        "the seeded session must resume, or the comparison never runs"
    );
    assert_eq!(
        stale(&registry, session_id),
        None,
        "an unchanged prefix must be silent — a notice here is the nuisance that gets \
         the real one skipped"
    );

    // (2) The instructions change — the `--system` case, and the one the old token-count
    // comparison used to report with *"start a new session"* as its only remedy.
    let mut changed = cfg.clone();
    changed
        .system
        .push_str("\n\nA new instruction that arrived after this session was seated.");
    let (mut h, registry) = opened(&changed, &parts);
    assert!(
        h.resumed().is_some(),
        "a changed prompt is a reason to SAY something, never to refuse the resume"
    );
    let said = stale(&registry, session_id).expect("a changed prompt must be said");
    assert!(said.contains(session_id), "it names the session: {said}");
    assert!(
        said.contains("was seated"),
        "and when it was seated, the launcher's own register: {said}"
    );
    assert!(
        said.contains("the system instructions"),
        "and what moved: {said}"
    );
    assert!(
        said.contains("`/reseat`"),
        "and the remedy BY NAME, which is the whole point of the sentence: {said}"
    );
    // **The session kept the prefix it was created with**, and the notice is a fact about
    // it rather than a claim that the daemon moved it: the resume came back on the fork
    // the seed wrote, under the prefix that fork opened with. A daemon that quietly
    // re-seated the conversation would land on a NEW transcript and say nothing about
    // what that cost.
    assert_eq!(
        h.transcript_id(),
        format!("{session_id}#t1"),
        "the resume must replay the stored prefix, not fork onto the new one"
    );

    // (3) **A re-seat takes the sentence away**, because it stops being true: `/reseat`
    // forks the conversation onto the prompt this daemon composes now, so a head attaching
    // afterwards must not be told the session is speaking an older one. Driven through
    // `fork_to_summary` — the half of a re-seat that needs no model — with the prefix
    // `reseat_target` composes, which is what the real path hands it.
    let outcome = CompactionOutcome {
        turn_id: format!("{}#reseat", h.transcript_id()),
        summary: String::new(),
        tool_calls: 0,
        truncated: false,
        cached_tokens: 0,
        reusable: 0,
        generated_tokens: 0,
    };
    let (next, next_id) = h
        .reseat_target()
        .expect("composing the seated prompt")
        .expect("this session is not speaking the seated prompt, so there is one to move to");
    assert!(
        next.system.contains("A new instruction"),
        "the prompt this daemon seats now is the one the re-seat lands on"
    );
    h.fork_to_summary(&outcome, Some(&next), Some(&next_id), ForkTail::NONE)
        .expect("the re-seat must land");
    assert_eq!(
        stale(&registry, session_id),
        None,
        "a re-seated session speaks the seated prompt, so the notice has stopped being true"
    );
}

/// **A tool schema that changed is named as a schema**, and a tool that merely ARRIVED is
/// named as added.
///
/// `--bash` is the lever because seating it is a flag and nothing else, and it is the same
/// lever `wired.rs` uses for the same reason: `m2_coder` names `bash` and the daemon strips
/// it back off unless the flag is passed. So the two opens are one session, one
/// conversation and a different tool list in the prompt it was created under — which is
/// what a build that grew a tool's schema looks like from here.
///
/// The seat is `coder` because `bash` is not in the orchestrator's role at all: the flag
/// toggles a name that is already there, and on the default seat it would toggle nothing.
/// `wired.rs` skips this shape when the box cannot build a boundary, and so does this.
#[test]
fn a_tool_that_arrived_is_named_in_the_sentence() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("tools");
    let ws = TempDir::new("ws-tools");
    let store = dir.path().join("sessions.db");
    let session_id = "prefix-tools";
    let mut bare = config(&store, session_id, ws.path());
    bare.seat = Seat::Coder;
    let parts = empty_modes(Parts::load(&bare).expect("the vocabulary must load"));

    {
        let hub = Hub::new(session_id);
        let registry = Registry::of(hub.clone());
        let mut h = match Harness::open_with_registry(
            &parts,
            bare.clone(),
            hub,
            None,
            None,
            registry.clone(),
        ) {
            Ok(h) => h,
            Err(e) => {
                let e = e.to_string();
                assert!(
                    e.contains("cgroup") || e.contains("boundary") || e.contains("confin"),
                    "a confined backend failed for a reason it did not name: {e}"
                );
                return letibot_tokencore::apparatus::absent(
                    "a boundary this box can build (cgroup v2; process groups on macOS) — the \
                     coder seat will not open without one",
                );
            }
        };
        seed(&mut h);
    }

    // The same daemon, now seated with a shell. Nothing about the conversation changed.
    let mut with_bash = bare.clone();
    with_bash.allow_bash = true;
    let (h, registry) = opened(&with_bash, &parts);
    assert!(h.resumed().is_some(), "the seeded session resumes");
    assert!(
        h.wiring().seated.iter().any(|s| s == "bash"),
        "the fixture must really have seated the shell, or nothing changed: {:?}",
        h.wiring().seated
    );
    let said = stale(&registry, session_id).expect("a changed tool list must be said");
    assert!(
        said.contains("the tool schemas"),
        "the tool list is what moved: {said}"
    );
    assert!(
        said.contains("bash"),
        "and the tool is named, which is the half a reader acts on: {said}"
    );
    assert!(said.contains("`/reseat`"), "{said}");
    // **And the instructions are NOT claimed to have moved.** One sentence that named
    // both would send a reader to `prompts.toml` for a change that is not there.
    assert!(
        !said.contains("the system instructions"),
        "the instructions did not move and must not be blamed: {said}"
    );
}

/// **A note written between two opens is not a stale prefix.**
///
/// The frozen/live split, asserted at the seam that makes it necessary: the standing notes
/// are re-read at session open, at every base rebuild and on every provider turn, so a
/// fingerprint covering them would cry stale every time a note was written. Both opens
/// compose the notes — and the two composed prompts must really DIFFER, or this test would
/// pass on a fixture that never wrote a note at all.
#[test]
fn a_note_written_between_two_opens_is_not_a_stale_prefix() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("notes");
    let ws = TempDir::new("ws-notes");
    let store = dir.path().join("sessions.db");
    let session_id = "prefix-notes";
    let base = config(&store, session_id, ws.path());
    let parts = empty_modes(Parts::load(&base).expect("the vocabulary must load"));

    std::fs::write(
        ws.path().join("AGENTS.md"),
        "note-one: the first generation\n",
    )
    .expect("fixture AGENTS.md");
    let mut first = base.clone();
    first.compose_system_with_notes(&parts.vocab);
    let was_system = first.system.clone();
    {
        let (mut h, _registry) = opened(&first, &parts);
        assert!(
            h.config().system.contains("note-one"),
            "the fixture must actually carry the note it wrote"
        );
        seed(&mut h);
    }

    // The edit. A nonce, so the two generations cannot be confused by shared words.
    std::fs::write(
        ws.path().join("AGENTS.md"),
        "note-two: a later generation\n",
    )
    .expect("edited AGENTS.md");
    let mut second = base.clone();
    second.compose_system_with_notes(&parts.vocab);
    assert_ne!(
        was_system, second.system,
        "the two opens must compose DIFFERENT prompts, or the silence below proves nothing"
    );

    let (h, registry) = opened(&second, &parts);
    assert!(h.resumed().is_some(), "the second open resumes");
    assert!(
        h.config().system.contains("note-two"),
        "the live half really did move: {}",
        h.config().system
    );
    assert_eq!(
        stale(&registry, session_id),
        None,
        "the notes are re-read every rebuild; a fingerprint that covered them would \
         report this session stale on every note the operator ever writes"
    );
}
