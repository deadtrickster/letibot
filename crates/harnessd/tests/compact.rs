//! Compaction's fork: the daemon's half of turning a summary into the new base.
//!
//! `letibot-turn`'s `compaction.rs` asserts the summary turn itself — the cached
//! prefix, the instruction, the outcome's numbers. This file asserts what the
//! *harness* does with that outcome, none of which the turn crate can see:
//!
//! 1. **The fork is a store row, not a truncation.** A new transcript row with the
//!    session's own stable prefix, linked back to the parent with the session
//!    log's seq — the old transcript kept whole, because compaction never deletes
//!    anything, it stops carrying it.
//! 2. **The new base is the prefix plus one item**, and that item says what it is
//!    and carries the summary verbatim. A base that paraphrased the summary would
//!    be a base nobody can check against the turn that produced it.
//! 3. **A resume lands on the fork.** `list_sessions` picks a session's newest
//!    transcript, so reopening the session after a compaction must resume the
//!    fork — a resume that came back on the parent would put the full history
//!    behind the next prompt, which is the thing compaction exists to prevent.
//! 4. **Twice is a chain.** A second compaction forks off the first fork, and the
//!    ids say so.
//!
//! It needs the vocabulary GGUF and **not** the model server: the fork needs only
//! the store, the vocabulary and the harness. That is why [`Harness::compact`]
//! (which runs the turn) and [`Harness::fork_to_summary`] (which does not) are two
//! methods. `LETIBOT_VOCAB_GGUF` overrides the path.

use letibot_harnessd::config::Config;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_tokencore::store::Store;
use letibot_turn::CompactionOutcome;

const WANTED: Dialect = Dialect::Qwen;

fn config(store: &std::path::Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = WANTED;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    if let Ok(g) = std::env::var("LETIBOT_VOCAB_GGUF") {
        cfg.vocab_gguf = g.into();
    }
    cfg
}

/// A summary turn's outcome, as the turn crate would hand it over. The fork does
/// not know how the summary was produced and must not care.
fn outcome(summary: &str) -> CompactionOutcome {
    CompactionOutcome {
        turn_id: "test-turn".into(),
        summary: summary.into(),
        tool_calls: 0,
        cached_tokens: 0,
        reusable: 0,
        generated_tokens: 0,
    }
}

fn load_parts(cfg: &Config) -> Parts {
    Parts::load(cfg).expect("the vocabulary must load")
}

fn opened<'a>(cfg: &Config, parts: &'a Parts) -> Harness<'a> {
    let hub = Hub::new(&cfg.session_id);
    Harness::open(parts, cfg.clone(), hub).expect("the session must open")
}

#[test]
fn a_compaction_fork_is_a_store_row_and_a_resume_lands_on_it() {
    let dir = TempDir::new("harnessd-compact");
    let path = dir.path().join("sessions.db");
    let session_id = "compact-fork-test";
    let cfg = config(&path, session_id);
    let parts = load_parts(&cfg);
    let mut h = opened(&cfg, &parts);
    assert_eq!(h.transcript_id(), format!("{session_id}#t0"));

    // Nothing has run yet, so the base being replaced is the prefix alone.
    let was = h.ledger_len();
    let summary = "decided A because B; changed crates/x/src/lib.rs; `cargo test` green; \
                   the open question is whether Y holds";
    let report = h
        .fork_to_summary(&outcome(summary), None, None)
        .expect("the fork must land");

    // (1) The store now holds two transcripts for the session, and the second is
    // the fork: same prefix, parent named, fork point recorded.
    let store = Store::open(&path).expect("reopening");
    let list = store.list_sessions().expect("listing");
    assert_eq!(list.len(), 1, "one session");
    let s = &list[0];
    assert_eq!(s.transcript_id.as_deref(), Some(report.transcript_id.as_str()));
    let fork = store
        .load_transcript(&report.transcript_id)
        .expect("reading the fork");
    assert_eq!(
        fork.parent_transcript_id.as_deref(),
        Some(format!("{session_id}#t0").as_str())
    );
    assert_eq!(fork.forked_at_seq, Some(report.forked_at as u32));

    // (2) The new base is the prefix plus one item, and the item carries the
    // summary verbatim inside a note that says what happened.
    let items = fork.items;
    assert_eq!(items.len(), 1, "the forked base is one item");
    let letibot_transcript::TranscriptItem::System { text, .. } = &items[0].0 else {
        panic!("the forked base's item is a system update, got {:?}", items[0].0);
    };
    assert!(
        text.contains(summary),
        "the summary rides verbatim: {text}"
    );
    assert_eq!(report.was_tokens, was, "the old base was measured before the drop");
    assert!(
        report.base_tokens > report.was_tokens,
        "prefix + summary item is bigger than the empty body it replaced"
    );
    assert_eq!(h.transcript_id(), report.transcript_id);

    // (3) A resume lands on the fork, not on the parent.
    drop(h);
    let cfg2 = config(&path, session_id);
    let parts2 = load_parts(&cfg2);
    let h2 = opened(&cfg2, &parts2);
    let r = h2
        .resumed()
        .unwrap_or_else(|| panic!("a session with a fork must resume"));
    assert_eq!(r.transcript_id, report.transcript_id);
    assert_eq!(r.rows, 1, "the resumed base is the summary item alone");
    assert_eq!(h2.items().len(), 1);
    assert_eq!(h2.ledger_len(), report.base_tokens);
}

#[test]
fn a_second_compaction_forks_off_the_first_fork() {
    let dir = TempDir::new("harnessd-compact-twice");
    let path = dir.path().join("sessions.db");
    let session_id = "compact-twice-test";
    let cfg = config(&path, session_id);
    let parts = load_parts(&cfg);
    let mut h = opened(&cfg, &parts);

    let first = h
        .fork_to_summary(&outcome("first summary"), None, None)
        .expect("the first fork");
    let second = h
        .fork_to_summary(&outcome("second summary"), None, None)
        .expect("the second fork");
    assert_eq!(first.transcript_id, format!("{session_id}#t1"));
    assert_eq!(second.transcript_id, format!("{session_id}#t2"));
    assert_eq!(second.parent_id, first.transcript_id, "the chain, not a retry");

    let store = Store::open(&path).expect("reopening");
    let items = store
        .load_transcript(&second.transcript_id)
        .expect("loading")
        .items;
    assert_eq!(items.len(), 1);
    let letibot_transcript::TranscriptItem::System { text, .. } = &items[0].0 else {
        panic!("a forked base's item is a system update");
    };
    assert!(
        text.contains("second summary") && !text.contains("first summary"),
        "each fork carries its own summary, not the previous one: {text}"
    );
}

#[test]
fn a_session_without_a_store_refuses_to_fork_by_name() {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = WANTED;
    cfg.store = None;
    if let Ok(g) = std::env::var("LETIBOT_VOCAB_GGUF") {
        cfg.vocab_gguf = g.into();
    }
    cfg.session_id = "compact-no-store".into();
    let parts = load_parts(&cfg);
    let mut h = opened(&cfg, &parts);
    let e = h
        .fork_to_summary(&outcome("anything"), None, None)
        .expect_err("no store, no fork");
    assert!(
        e.to_string().contains("store"),
        "the refusal names the missing store: {e}"
    );
}

/// **The turn after the wall is the harness's own words, not the operator's.**
///
/// `continue_after_wall` is what `Sessions::after_turn` calls instead of leaving
/// the operator to type "continue" after every compaction (*"again have to write
/// it after compaction"*). What must hold even where no model server answers:
/// the item lands as a User row, because that is what the model reads as a
/// prompt; the trail records it as `Speaker::Agent`, because the harness talking
/// to itself must never be readable as the operator authorising the work it
/// resumes; and the turn it then attempts is allowed to fail — the append
/// happens first, which is the ordering this test can see without a server and
/// the part that would be lost if the item were never written. The endpoint is
/// a dead port, so the attempt fails fast and touches no server.
#[test]
fn the_continuation_after_a_wall_is_the_harnesss_own_words() {
    use letibot_tools::authorise::Speaker;

    let dir = TempDir::new("harnessd-continue");
    let path = dir.path().join("sessions.db");
    let mut cfg = config(&path, "continue-test");
    cfg.endpoint = letibot_turn::Endpoint::parse("127.0.0.1:1").expect("a literal endpoint");
    let parts = load_parts(&cfg);
    let mut h = opened(&cfg, &parts);

    let out = h.continue_after_wall();
    assert!(out.is_err(), "a dead endpoint cannot answer: {out:?}");

    // The item is there, and it is a prompt: a User row that says what happened
    // and asks for the work back.
    let items = h.items();
    let last = items.last().expect("the continuation was appended");
    let letibot_transcript::TranscriptItem::User { parts: user_parts } = last else {
        panic!("the continuation is a user item, got {last:?}");
    };
    let letibot_transcript::UserPart::Text { text } = &user_parts[0] else {
        panic!("the continuation is text, got {:?}", user_parts[0]);
    };
    assert!(
        text.contains("context wall") && text.contains("compacted"),
        "the continuation says what happened: {text}"
    );
    assert!(
        text.contains("Continue the work"),
        "the continuation asks for the work back: {text}"
    );

    // And the trail says who spoke: the harness, never the operator.
    let t = h.trail();
    let said = t
        .utterances
        .iter()
        .find(|u| u.text.contains("context wall"))
        .expect("the continuation is in the trail");
    assert_eq!(
        said.speaker,
        Speaker::Agent,
        "the continuation must never read as the operator's words"
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
