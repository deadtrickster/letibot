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

/// The canned server the turn crate's tests replay, shared by path rather than
/// copied: a second copy of the wire shape would drift from the first, and the
/// frames here are the same bytes those tests replay. It serves an ephemeral
/// port, so nothing here can meet the model server.
#[path = "../../turn/tests/support/canned.rs"]
mod canned;

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

// Qwen's own ids, the same values the turn crate's canned tests pin.
const IM_END: u32 = 248046;
const THINK_OPEN: u32 = 248068;
const THINK_CLOSE: u32 = 248069;

fn ids(vocab: &letibot_tokencore::Vocab, text: &str) -> Vec<u32> {
    vocab.tokenize_text(text).expect("text tokenizes")
}

fn spoken(vocab: &letibot_tokencore::Vocab, ids: &[u32]) -> Vec<canned::Frame> {
    ids.iter()
        .map(|id| canned::Frame::Token {
            id: *id,
            text: vocab
                .detokenize(&[*id], true)
                .expect("one token decodes")
                .leak(),
        })
        .collect()
}

/// A complete turn that answers in plain text: the model thinks, closes the
/// block, answers, ends. No tool calls, so the harness's round loop ends after
/// one round.
fn a_plain_answer_turn(
    vocab: &letibot_tokencore::Vocab,
    thought: &str,
    answer: &str,
    n_prompt: u64,
) -> Vec<canned::Frame> {
    let thought_ids = ids(vocab, thought);
    let answer_ids = ids(vocab, answer);
    let mut frames = vec![canned::Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.extend(spoken(vocab, &thought_ids));
    frames.extend(spoken(vocab, &[THINK_CLOSE]));
    frames.extend(spoken(vocab, &answer_ids));
    frames.extend(spoken(vocab, &[IM_END]));
    frames.push(canned::Frame::Final {
        stop_type: "eos",
        n_decoded: (thought_ids.len() + answer_ids.len() + 2) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

/// The live finding of 2026-09-15, replayed: the stream stops with a normal
/// `eos` while the reasoning block is still open and nothing but reasoning was
/// produced — the shape that failed the operator's automatic compaction.
fn an_unfinished_reasoning_turn(
    vocab: &letibot_tokencore::Vocab,
    thought: &str,
    n_prompt: u64,
) -> Vec<canned::Frame> {
    let thought_ids = ids(vocab, thought);
    let mut frames = vec![canned::Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.push(canned::Frame::Token {
        id: THINK_OPEN,
        text: "",
    });
    frames.extend(spoken(vocab, &thought_ids));
    frames.push(canned::Frame::Final {
        stop_type: "eos",
        n_decoded: (1 + thought_ids.len()) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

/// **The salvage spending itself is still a failure the log announces.**
///
/// The compaction path now salvages a say-nothing summary turn the way the tool
/// loop does, bounded by the engine's budget. What must hold when the budget is
/// spent — a model that ends every retry inside its own reasoning block — is
/// that the failure still COMES OUT: `Sessions::compact` errs, and the
/// automatic path publishes `auto_compact_failed` exactly as it did before the
/// salvage existed. A compaction that did not run must say so; the event
/// disappearing behind retries would be the dishonest version of this fix.
///
/// The window is 512 so the wall sits at 384 tokens (`should_compact` is
/// `resident + headroom >= window`, and below 8192 the headroom is a quarter of
/// the window): the stable prefix alone is past it, so one answered prompt turn
/// is enough for the automatic compaction to fire. The scripts: request one is
/// the prompt turn and answers; every request after is the compaction turn
/// ending unfinished, three of which are salvaged before the fourth comes back
/// `SalvageExhausted`.
#[test]
fn a_compaction_that_exhausts_the_salvage_still_publishes_auto_compact_failed() {
    use letibot_harnessd::Sessions;
    use letibot_sessionlog::event::SessionEvent;
    use letibot_sessionlog::registry::Registry;

    let dir = TempDir::new("harnessd-compact-salvage");
    let path = dir.path().join("sessions.db");
    let session_id = "compact-salvage-test";
    let mut cfg = config(&path, session_id);
    cfg.context_window = Some(512);
    let parts = load_parts(&cfg);
    let vocab = &parts.vocab;

    // One answered prompt turn, then unfinished compaction turns for as long as
    // anybody asks: three salvages, the spent turn, and one spare in case the
    // count is ever off by one — an undersupplied server would turn the spent
    // budget into a socket error and the test would pass for the wrong reason.
    let scripts = vec![
        a_plain_answer_turn(vocab, "counting", "the answer was forty-two", 20),
        an_unfinished_reasoning_turn(vocab, "mid-summary, never closed", 30),
        an_unfinished_reasoning_turn(vocab, "mid-summary, never closed", 30),
        an_unfinished_reasoning_turn(vocab, "mid-summary, never closed", 30),
        an_unfinished_reasoning_turn(vocab, "mid-summary, never closed", 30),
        an_unfinished_reasoning_turn(vocab, "mid-summary, never closed", 30),
    ];
    let serv = canned::Canned::serve_each(scripts, 6);
    cfg.endpoint = serv.endpoint.clone();

    let registry = Registry::new();
    registry
        .create(session_id, "", Sessions::wiring(&cfg))
        .expect("the session is in the registry");
    let hub = registry.get(session_id).expect("the hub is the registry's");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the session opens");

    // The prompt turn answers, and the automatic compaction fires after it —
    // and fails, loudly, after spending the budget.
    let out = sessions.submit(session_id, "count the things");
    assert!(out.is_ok(), "the prompt turn answered: {out:?}");

    let warnings: Vec<(String, String)> = hub
        .retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning { code, detail } => Some((code.clone(), detail.clone())),
            _ => None,
        })
        .collect();

    // The attempt was announced in the vocabulary that was already there, and
    // the failure followed it in the same vocabulary — no parallel event, no
    // silence.
    let attempt_at = warnings
        .iter()
        .position(|(c, _)| c == "auto_compact")
        .expect("the automatic compaction was announced: {warnings:?}");
    let failed_at = warnings
        .iter()
        .position(|(c, _)| c == "auto_compact_failed")
        .expect("the failed compaction was announced: {warnings:?}");
    assert!(
        attempt_at < failed_at,
        "the attempt precedes the failure: {warnings:?}"
    );
    let (_, detail) = &warnings[failed_at];
    assert!(
        detail.contains("the automatic compaction did not run"),
        "the failure says the compaction did not run: {detail}"
    );
    assert!(
        detail.contains("consecutive length salvages; the cap is spent"),
        "the failure names the spent salvage budget, not a socket error: {detail}"
    );

    // And nothing was reduced: no fork, the transcript is the prompt turn plus
    // the instruction and the three notices, and the next `/compact` retries
    // over exactly that.
    let h = sessions
        .harness_of(session_id)
        .expect("the session is still open");
    assert_eq!(
        h.transcript_id(),
        format!("{session_id}#t0"),
        "a failed compaction forks nothing"
    );
    let items = h.items();
    let notices = items
        .iter()
        .filter(|i| {
            matches!(i, letibot_transcript::TranscriptItem::User { parts }
                if matches!(&parts[..],
                    [letibot_transcript::UserPart::Text { text }]
                    if text.starts_with("Your previous turn ended inside a reasoning block")))
        })
        .count();
    assert_eq!(notices, 3, "one notice per salvaged turn: {items:?}");
}
