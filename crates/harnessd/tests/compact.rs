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
use letibot_harnessd::harness::ForkTail;
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
    // **This file needs the vocabulary and not the server** — its own module note
    // says so — so an endpoint that does not answer is the expected state and not
    // a server worth waiting for. Left at the default, the one test that does
    // reach for the endpoint walked the whole retry ladder: 1+2+4+8+16+32 seconds,
    // which is the entire 63.7s this file used to take for 3.5s of CPU.
    cfg.http_retries = 0;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    if let Ok(g) = std::env::var("LETIBOT_VOCAB_GGUF") {
        cfg.vocab_gguf = Some(g.into());
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
        truncated: false,
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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
        .fork_to_summary(&outcome(summary), None, None, ForkTail::NONE)
        .expect("the fork must land");

    // (1) The store now holds two transcripts for the session, and the second is
    // the fork: same prefix, parent named, fork point recorded.
    let store = Store::open(&path).expect("reopening");
    let list = store.list_sessions().expect("listing");
    assert_eq!(list.len(), 1, "one session");
    let s = &list[0];
    assert_eq!(
        s.transcript_id.as_deref(),
        Some(report.transcript_id.as_str())
    );
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
        panic!(
            "the forked base's item is a system update, got {:?}",
            items[0].0
        );
    };
    assert!(text.contains(summary), "the summary rides verbatim: {text}");
    assert_eq!(
        report.was_tokens, was,
        "the old base was measured before the drop"
    );
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-compact-twice");
    let path = dir.path().join("sessions.db");
    let session_id = "compact-twice-test";
    let cfg = config(&path, session_id);
    let parts = load_parts(&cfg);
    let mut h = opened(&cfg, &parts);

    let first = h
        .fork_to_summary(&outcome("first summary"), None, None, ForkTail::NONE)
        .expect("the first fork");
    let second = h
        .fork_to_summary(&outcome("second summary"), None, None, ForkTail::NONE)
        .expect("the second fork");
    assert_eq!(first.transcript_id, format!("{session_id}#t1"));
    assert_eq!(second.transcript_id, format!("{session_id}#t2"));
    assert_eq!(
        second.parent_id, first.transcript_id,
        "the chain, not a retry"
    );

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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = WANTED;
    cfg.store = None;
    if let Ok(g) = std::env::var("LETIBOT_VOCAB_GGUF") {
        cfg.vocab_gguf = Some(g.into());
    }
    cfg.session_id = "compact-no-store".into();
    let parts = load_parts(&cfg);
    let mut h = opened(&cfg, &parts);
    let e = h
        .fork_to_summary(&outcome("anything"), None, None, ForkTail::NONE)
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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
    let letibot_transcript::TranscriptItem::User {
        parts: user_parts, ..
    } = last
    else {
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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
            SessionEvent::Warning { code, detail, .. } => Some((code.clone(), detail.clone())),
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
    // **What it names has been both things, so read this before changing it.**
    //
    // This fixture is a 2719-token PROMPT in a 512-token window: hopeless, and
    // hopeless before any turn runs. It used to discover that by attempting a
    // summary and spending the salvage budget, and the assertion here named the
    // spent budget. Compaction now does the arithmetic first, so the same fixture
    // is refused up front and names the actual cause.
    //
    // The test is not about either message. It is about the failure COMING OUT --
    // `Sessions::compact` errs and the automatic path publishes
    // `auto_compact_failed` -- and that is why it survives the behaviour changing
    // underneath it twice.
    assert!(
        detail.contains("PROMPT is") && detail.contains("token window"),
        "the failure names the prompt against the window, which is the cause here: {detail}"
    );
    assert!(
        detail.contains("--context-window") || detail.contains("--system"),
        "and says what would help, since shortening the conversation would not: {detail}"
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
            matches!(i, letibot_transcript::TranscriptItem::User { parts, .. }
                if matches!(&parts[..],
                    [letibot_transcript::UserPart::Text { text }]
                    // Against the constant, not a copy of its words: this test
                    // failed for a rewording of the notice it is not about.
                    if text.as_str() == letibot_turn::UNFINISHED_REASONING_NOTICE))
        })
        .count();
    assert_eq!(
        notices, 0,
        "a compaction refused before it was attempted appends nothing: {items:?}"
    );
}

/// **A compaction re-seats, so `/compact` then `/reseat` is no longer two
/// summaries.**
///
/// Measured in the operator's own session before this landed. Their `…#t8` was
/// five rows: row 0 the `/compact` summary, row 1 `/reseat` asking for a summary
/// of a transcript that was already nothing but a summary, row 2 the model —
/// with nothing to summarise — running `git log` instead, which tripped the "a
/// summary is a record, not an action" guard and refused the re-seat, and rows 3
/// and 4 the retry. Three summary turns and two cold prefills to pick up a tool
/// list. Their reading: *"let compaction automatically reseat so new tools picked
/// up"*.
///
/// This drives the half that needs no model server: the daemon comes back with a
/// different prompt, the fork lands on THAT one, and a `/reseat` afterwards has
/// nothing left to do.
#[test]
fn a_compaction_forks_onto_the_prompt_the_daemon_seats_now() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-compact-reseat");
    let path = dir.path().join("sessions.db");
    let session_id = "compact-reseat-test";

    // A conversation opened under one prompt, and compacted under it.
    let cfg = config(&path, session_id);
    let parts = load_parts(&cfg);
    let mut h = opened(&cfg, &parts);
    // Nothing has changed since it opened, so there is nothing to re-seat and a
    // compaction keeps the prompt it is speaking. This is the common case and it
    // must stay free.
    assert!(
        h.reseat_target().expect("probing").is_none(),
        "an unchanged daemon re-seats nothing"
    );
    let first = h
        .fork_to_summary(&outcome("the first summary"), None, None, ForkTail::NONE)
        .expect("the first fork lands");
    drop(h);

    // The daemon comes back with a different system prompt — which is what a new
    // tool does to the prefix, without needing a registry this test cannot build.
    let mut cfg2 = config(&path, session_id);
    cfg2.system = format!("{}\n\nAnd one more standing instruction.", cfg2.system);
    let parts2 = load_parts(&cfg2);
    let mut h2 = opened(&cfg2, &parts2);
    // It resumed onto the STORED prompt, because that is what the tokens were
    // produced under. The conversation is still speaking the old one.
    assert_eq!(h2.transcript_id(), first.transcript_id);

    let target = h2
        .reseat_target()
        .expect("probing")
        .expect("the seated prompt differs, so there is something to re-seat onto");
    let (next_prefix, next_id) = target.clone();
    assert_ne!(
        next_id, first.transcript_id,
        "a new prefix row, not the old one"
    );

    // The compaction forks onto it — one summary, one fork, one cold prefill.
    let second = h2
        .fork_to_summary(
            &outcome("the second summary"),
            Some(&next_prefix),
            Some(&next_id),
            ForkTail::NONE,
        )
        .expect("the re-seating fork lands");
    h2.adopt_reseat(Some(target));

    // The store agrees: the fork's own prefix row is the new one.
    let store = Store::open(&path).expect("reopening");
    let fork = store
        .load_transcript(&second.transcript_id)
        .expect("reading the fork");
    assert_eq!(
        fork.parent_transcript_id.as_deref(),
        Some(first.transcript_id.as_str())
    );
    assert_eq!(fork.stable_prefix_id, next_id);

    // **And this is the whole point**: a `/reseat` now has nothing to do, so it
    // refuses before running a turn instead of asking for a summary of a summary.
    let said = match h2.reseat() {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a re-seat after a re-seating compaction must find nothing to do"),
    };
    assert!(said.contains("nothing to re-seat"), "{said}");
    assert!(
        said.contains("every compaction now forks onto the seated prompt"),
        "the refusal says why there is nothing to do: {said}"
    );

    // A resume lands on the re-seated fork and speaks the new prompt.
    drop(h2);
    let cfg3 = config(&path, session_id);
    let parts3 = load_parts(&cfg3);
    let h3 = opened(&cfg3, &parts3);
    let r = h3.resumed().expect("a session with forks must resume");
    assert_eq!(r.transcript_id, second.transcript_id);
}

/// **`/tools` names the gap between seated and announced.**
///
/// The registry is what this daemon seated; the prompt is what message zero
/// announces, and it is fixed when a transcript starts. A tool seated after the
/// conversation began is one the model has never been told about and cannot call
/// — and the banner is no help, because the banner is computed from the registry.
/// That gap is the operator's lost hour: *"i did letibot --stop and leticode
/// --continue but still no exec"*.
#[test]
fn the_tools_listing_marks_what_the_prompt_has_never_heard_of() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-tools-listing");
    let path = dir.path().join("sessions.db");
    let session_id = "tools-listing-test";
    let cfg = config(&path, session_id);
    let parts = load_parts(&cfg);
    let h = opened(&cfg, &parts);

    // A fresh session: the prompt was built from this registry a moment ago, so
    // everything seated is announced and the listing says so plainly.
    let said = h.tools_lines().join("\n");
    assert!(said.contains("tool(s) seated in this session"), "{said}");
    assert!(
        said.contains("everything seated is callable"),
        "a fresh session has no gap: {said}"
    );
    assert!(
        !said.contains("NOT ANNOUNCED"),
        "and nothing is flagged: {said}"
    );
    // Every seated tool is on its own line with its access class.
    assert!(said.contains("(read)"), "{said}");
}

/// **The note in the new base may not claim a region was replaced while showing
/// it.** R27 ruled that a remote compaction carries the newest exchanges
/// verbatim, and that is exactly the change that made the old note false: it said
/// *"everything said before this point is replaced by the summary below"* and
/// then appended the tail, so the reader is told a thing is gone while reading it.
///
/// The two sentences the tail adds are the disclosure a reader needs and could
/// not otherwise get: how much was carried, and — when the tail had to start
/// inside an exchange — that its first message answers something no longer here.
/// That second one is `docs/compaction.md` §3's *something must stand where the
/// evicted span was* applied inside the boundary rather than before it.
#[test]
fn the_fork_says_what_it_carried_verbatim_and_never_claims_it_was_replaced() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-compact-tail");
    let path = dir.path().join("sessions.db");
    let session_id = "compact-tail-test";
    let cfg = config(&path, session_id);
    let parts = load_parts(&cfg);
    let mut h = opened(&cfg, &parts);

    let tail = vec![
        letibot_transcript::TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![letibot_transcript::UserPart::Text {
                text: "the turn in progress".into(),
            }],
        },
        letibot_transcript::TranscriptItem::Assistant {
            text: "working on it".into(),
            tool_calls: vec![],
            truncated: false,
        },
        letibot_transcript::TranscriptItem::Assistant {
            text: "and its second half".into(),
            tool_calls: vec![],
            truncated: false,
        },
    ];
    let report = h
        .fork_to_summary(
            &outcome("## Objective\n- carry the recent past"),
            None,
            None,
            ForkTail {
                items: &tail,
                split: Some(letibot_turn::TailSplit { dropped: 2 }),
                because: "budget",
            },
        )
        .expect("the fork lands");

    assert_eq!(report.tail_items, 3, "three items went in verbatim");
    assert_eq!(report.tail_dropped, Some(2), "and two of them are missing");
    // **The tail on the wire is the words, in order, with a role each** — so a head
    // draws the recent past instead of parsing a count out of the note. Two agent
    // turns and one operator turn, and an empty assistant row would be no turn at all.
    assert_eq!(
        report.tail_turns,
        vec![
            letibot_sessionlog::event::CompactionTurn {
                role: "operator".into(),
                text: "the turn in progress".into()
            },
            letibot_sessionlog::event::CompactionTurn {
                role: "agent".into(),
                text: "working on it".into()
            },
            letibot_sessionlog::event::CompactionTurn {
                role: "agent".into(),
                text: "and its second half".into()
            },
        ]
    );
    assert_eq!(report.tail_because, "budget", "why this much and no more");

    let store = Store::open(&path).expect("reopening");
    let words = store
        .load_transcript(&report.transcript_id)
        .expect("reading the fork")
        .items;
    assert_eq!(words.len(), 4, "the note and the three carried items");
    let letibot_transcript::TranscriptItem::System { text, .. } = &words[0].0 else {
        panic!("the base's first item is the note");
    };
    assert!(
        text.contains("The last 3 item(s) of it follow this note VERBATIM"),
        "the count is stated: {text}"
    );
    assert!(
        text.contains("MIDDLE of an exchange: 2 item(s)"),
        "a mid-exchange start is disclosed: {text}"
    );
    assert!(
        !text.contains("everything said before this point is replaced"),
        "the note must not claim a region was replaced while the tail shows it: {text}"
    );
    assert!(
        text.contains("is replaced by the summary below"),
        "and it still says what the summary is: {text}"
    );
    // The carried items really are the tail, in order, and not a description.
    let letibot_transcript::TranscriptItem::Assistant { text, .. } = &words[3].0 else {
        panic!("the last carried item is the assistant's");
    };
    assert_eq!(text, "and its second half");
}

/// A fork with no tail says nothing about one, and keeps the replacement
/// sentence — the local path, and every re-seat and re-ingest.
#[test]
fn a_fork_with_no_tail_claims_nothing_about_a_tail() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-compact-notail");
    let path = dir.path().join("sessions.db");
    let session_id = "compact-no-tail-test";
    let cfg = config(&path, session_id);
    let parts = load_parts(&cfg);
    let mut h = opened(&cfg, &parts);

    let report = h
        .fork_to_summary(&outcome("a summary"), None, None, ForkTail::NONE)
        .expect("the fork lands");
    assert_eq!(report.tail_items, 0);
    assert_eq!(report.tail_dropped, None);

    let store = Store::open(&path).expect("reopening");
    let words = store
        .load_transcript(&report.transcript_id)
        .expect("reading the fork")
        .items;
    assert_eq!(words.len(), 1, "the note and nothing else");
    let letibot_transcript::TranscriptItem::System { text, .. } = &words[0].0 else {
        panic!("the base's first item is the note");
    };
    assert!(
        text.contains("is replaced by the summary below"),
        "the ordinary local compaction replaces the history: {text}"
    );
    assert!(
        !text.contains("VERBATIM") && !text.contains("MIDDLE of an exchange"),
        "and with no tail there is nothing to disclose: {text}"
    );
}

/// **The door's call is a DEPOSIT: it lands in the transcript and nothing replies to it.**
///
/// R31's whole reason for the door, and the three facts that make it one rather than a
/// request:
///
/// 1. the row is in `items` — the next turn reads it, and no turn was started to produce it;
/// 2. it carries `origin: Operator { who }`, so every head draws it as the person's act and
///    no head has to infer it (`dd81999`);
/// 3. **the size is said before the row lands**, which is the one thing the operator chose to
///    buy and would otherwise discover at the next compaction.
///
/// Run through `run_operator_call`, which is the same function both paths call — the worker's
/// between-turns arm and the round boundary a running turn polls
/// (`Harness::apply_queued_head_run`) — so what this asserts is the deposit itself and not one
/// caller of it.
#[test]
fn a_door_call_lands_as_a_deposit_with_its_origin_and_its_size() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-door-deposit");
    let path = dir.path().join("sessions.db");
    let cfg = config(&path, "door-deposit-test");
    let parts = load_parts(&cfg);
    let mut h = opened(&cfg, &parts);
    let before = h.items().len();

    h.run_operator_call("h1-1", "read", r#"{"path":"Cargo.toml"}"#, "dead")
        .expect("the call runs and its row lands");

    let after = h.items();
    assert_eq!(after.len(), before + 1, "exactly one row");
    let letibot_transcript::TranscriptItem::ToolResult {
        call_id,
        name,
        origin,
        payload,
        ..
    } = &after[after.len() - 1]
    else {
        panic!("the door's row is a tool result: {:?}", after.last());
    };
    assert_eq!(call_id, "h1-1");
    assert_eq!(name, "read");
    // **The provenance, which only this mechanism gives.** Pasted into the composer the text
    // would be indistinguishable from the operator's own opinion of it; through the door it is
    // evidence supplied, and a model may weigh the two differently.
    assert_eq!(
        origin,
        &Some(letibot_transcript::CallOrigin::Operator { who: "dead".into() }),
        "the row must say the person ran it, and name them"
    );
    assert!(!payload.is_empty(), "the file's text is the payload");

    // **And the size was said while the operator could still act on it.** The note names the
    // bytes that reach the model and the tokens they cost — the figure R31 asks for, and the
    // one thing about this call they chose to buy.
    let said: Vec<String> = h
        .hub()
        .retained()
        .iter()
        .filter_map(|e| match &e.event {
            letibot_sessionlog::SessionEvent::Warning { code, detail, .. }
                if code == "operator_call_ran" =>
            {
                Some(detail.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(said.len(), 1, "one disclosure, one call: {said:#?}");
    assert!(
        said[0].contains("reach the model from its next turn"),
        "the note says what the model will read: {}",
        said[0]
    );
    assert!(
        said[0].contains("No reply is generated"),
        "and that nothing replies to it: {}",
        said[0]
    );
}

/// **The transcript's own note says WHY it has no tail** — and this half is the DURABLE one.
///
/// The `compacted` warning reaches attached heads; the store has no events table. After a
/// restart, the note in the new transcript is all a reader has — so with no tail it said
/// nothing about one, and R27's ruling working was the same row as the budget losing to a
/// single item. That is R41's shape one document over: an absence with three causes and one
/// appearance.
///
/// The reason is CHOSEN here rather than hoped for from a fixture's sizes, which is the whole
/// reason `ForkTail::because` is a parameter.
#[test]
fn the_compaction_note_says_why_it_has_no_tail() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-compact-why");
    let path = dir.path().join("sessions.db");
    for (because, want) in [
        ("local_model", "LOCAL model"),
        ("nothing_fits", "larger than the whole tail budget"),
        ("no_turns", "nothing to carry"),
    ] {
        // **A fresh session per case**, because a fork replaces the transcript and the second
        // fork would be reading the first one's note.
        let session_id = format!("compact-why-{because}");
        let cfg = config(&path, &session_id);
        let parts = load_parts(&cfg);
        let mut h = opened(&cfg, &parts);
        let report = h
            .fork_to_summary(
                &outcome("## Objective\n- nothing carried"),
                None,
                None,
                ForkTail {
                    items: &[],
                    split: None,
                    because,
                },
            )
            .expect("the fork lands");
        assert_eq!(report.tail_items, 0, "this case carries no tail");
        assert_eq!(report.tail_because, because);
        let store = Store::open(&path).expect("reopening");
        let words = store
            .load_transcript(&report.transcript_id)
            .expect("reading the fork")
            .items;
        let letibot_transcript::TranscriptItem::System { text, .. } = &words[0].0 else {
            panic!("the base's first item is the note");
        };
        assert!(
            text.contains(want),
            "a compaction with no tail did not say why ({because}): {text}"
        );
        assert!(
            !text.contains("follow this note VERBATIM"),
            "the note claims a tail it does not have: {text}"
        );
    }
    // **And a fork that is not a compaction gets no clause at all.** A re-seat passes an empty
    // `because`, and a reason for it would be a claim about a decision nobody made.
    let session_id = "compact-why-reseat";
    let cfg = config(&path, session_id);
    let parts = load_parts(&cfg);
    let mut h = opened(&cfg, &parts);
    let report = h
        .fork_to_summary(&outcome(""), None, None, ForkTail::NONE)
        .expect("the re-seat lands");
    let store = Store::open(&path).expect("reopening");
    let words = store
        .load_transcript(&report.transcript_id)
        .expect("reading the fork")
        .items;
    let letibot_transcript::TranscriptItem::System { text, .. } = &words[0].0 else {
        panic!("the base's first item is the note");
    };
    for excuse in [
        "LOCAL model",
        "tail budget",
        "nothing to carry",
        "No verbatim tail",
    ] {
        assert!(
            !text.contains(excuse),
            "a re-seat got a tail's excuse (`{excuse}`): {text}"
        );
    }
}

/// **A switch clears the token ratio, in BOTH directions** — ruled 2026-09-24.
///
/// `ledger_scale` is a measurement: `(this box's ledger tokens, the model's own prompt
/// tokens)` for one prompt on one model. A ratio from one model is not evidence about another,
/// and carrying it forward presents a stale measurement as current. **Cleared rather than
/// re-derived**, because there is nothing to derive it from at the moment of the switch — the
/// first round on the new model is what produces it — and inventing one would be a guess
/// wearing a number's clothes.
///
/// Both directions, and the second had a live defect behind it: the per-round setter was
/// guarded on `self.provider.is_some()`, so a LOCAL round never refreshed the ratio and a
/// provider → local switch kept the provider's scale for the rest of the session.
#[test]
fn a_switch_clears_the_token_ratio_and_a_local_round_can_set_it() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-switch-scale");
    let path = dir.path().join("sessions.db");
    let cfg = config(&path, "switch-scale-test");
    let parts = load_parts(&cfg);
    let mut h = opened(&cfg, &parts);

    // A measurement exists — as it would after any metered round.
    h.config_mut().ledger_scale = Some((1_000_699, 671_280));
    assert!(h.config().tokens_are_converted(), "the fixture is scaled");

    // **A switch that CANNOT be built changes nothing**, which is this method's own promise:
    // a provider whose key is missing is refused, and a scale dropped for a refusal would be
    // a change made by a thing that did not happen.
    let before = h.config().ledger_scale;
    let refused = h.set_provider(Some(letibot_harnessd::config::ProviderConfig {
        name: "deepseek".into(),
        model: None,
        // An empty key is not a key: `keys::resolve` falls through to the environment and the
        // file, and neither has one for this name in a test.
        api_key: Some(String::new()),
        thinking: false,
    }));
    if refused.is_err() {
        assert_eq!(
            h.config().ledger_scale,
            before,
            "a refused switch dropped the measurement"
        );
    }

    // **A switch that CAN be built clears it.** An explicit key resolves without any network,
    // so this is the success path and not a mock of it.
    let line = h
        .set_provider(Some(letibot_harnessd::config::ProviderConfig {
            name: "deepseek".into(),
            model: None,
            api_key: Some("test-key-not-used-on-the-wire".into()),
            thinking: false,
        }))
        .expect("a provider with a key on the flag builds");
    assert!(!line.is_empty(), "the switch says what answers now");
    assert_eq!(
        h.config().ledger_scale,
        None,
        "the switch carried a measurement made on another model"
    );
    // **And the fact is SAYABLE**, which is the ruling's other half: a reader of an unscaled
    // number must be able to tell it from a calibrated one.
    assert!(
        h.config().tokens_are_unscaled(),
        "a metered model with no measurement must say so"
    );
    assert!(
        !h.config().tokens_are_converted(),
        "and it is not converted"
    );

    // **Back to local clears it too** — the direction that had the live defect.
    h.set_provider(None).expect("local is not refused");
    assert_eq!(h.config().ledger_scale, None);
    // And a local session is not "unscaled" in the sense that needs saying: its two counts
    // agree by construction, so an unconverted number there is the whole truth.
    assert!(
        !h.config().tokens_are_unscaled(),
        "a local session's counts need no measurement and are not a warning"
    );
    // **And a LOCAL round may set the ratio.** The setter's guard used to be
    // `self.provider.is_some()`, so this assignment was impossible and a session that had ever
    // been switched ran unscaled for ever.
    h.config_mut().ledger_scale = Some((1_000_699, 1_000_699));
    assert!(
        !h.config().tokens_are_unscaled(),
        "a local round's own measurement must clear the unscaled state"
    );
    assert!(
        !h.config().tokens_are_converted(),
        "a ratio of one is not a conversion: the two counts are the same number"
    );
}

/// **A resumed session is checked for the wall BEFORE its first turn is sent.**
///
/// `compact_if_at_the_wall`'s own contract is to compact *"when the NEXT turn
/// would not fit"*, and its two older call sites are both AFTER a turn — so the
/// one prompt nothing checked was the first prompt after a resume, which is
/// exactly the prompt a reopened session gets. Measured 2026-10-02: a reopened
/// leticl session sent 1,463,497 tokens against a 1,048,576 window on every
/// attempt, and the provider's refusal came back as a BACKEND error rather than
/// `ContextWall` — so `after_turn`'s `out.is_ok() || wall` was false, the
/// compaction was never reached, and no amount of retyping could dig that session
/// out. A dead head, not a slow one.
///
/// The fixture is the salvage test's, for the same reason it uses it: a prefix in
/// a 512-token window is over the wall before any turn runs, so nothing about the
/// conversation has to build up first. What is asserted is the ORDER — the wall
/// is announced before the turn starts — because that ordering *is* the property
/// under test, and it is the one thing an assertion on `out.is_ok()` cannot see.
///
/// The compaction then FAILS here, because this fixture's prompt is larger than
/// its window; that is the fixture's point and not this test's. What matters is
/// that it was CONSIDERED before anything was sent, which is what a resumed
/// session needs and what it did not have. The failure itself is the salvage
/// test's assertion, not this one's.
#[test]
fn the_wall_is_checked_before_the_first_turn_after_a_resume() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    use letibot_harnessd::Sessions;
    use letibot_sessionlog::event::SessionEvent;
    use letibot_sessionlog::registry::Registry;

    let dir = TempDir::new("harnessd-wall-before-turn");
    let path = dir.path().join("sessions.db");
    let session_id = "wall-before-turn-test";
    let mut cfg = config(&path, session_id);
    cfg.context_window = Some(512);
    let parts = load_parts(&cfg);

    // A plain answer, because the prompt turn is the only request that fits here.
    let scripts = vec![a_plain_answer_turn(
        &parts.vocab,
        "counting",
        "forty-two",
        30,
    )];
    let serv = canned::Canned::serve_each(scripts, 4);
    cfg.endpoint = serv.endpoint.clone();

    let registry = Registry::new();
    registry
        .create(session_id, "", Sessions::wiring(&cfg))
        .expect("the session is in the registry");
    let hub = registry.get(session_id).expect("the hub is the registry's");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the session opens");

    // **Nothing has run yet**: the session has just been opened, which is the state
    // a resume leaves it in, and the prompt below is its first turn.
    let out = sessions.submit(session_id, "count the things");
    assert!(out.is_ok(), "the prompt turn still answers: {out:?}");

    let events: Vec<SessionEvent> = hub.retained().iter().map(|e| e.event.clone()).collect();
    let announced = events
        .iter()
        .position(|e| matches!(e, SessionEvent::Warning { code, .. } if code == "auto_compact"))
        .expect("the wall was announced at all");
    let started = events
        .iter()
        .position(|e| matches!(e, SessionEvent::TurnStarted { .. }))
        .expect("the turn started at all");
    assert!(
        announced < started,
        "the wall must be considered before the first turn is sent, not after it: \
         auto_compact at {announced}, TurnStarted at {started}"
    );
}

/// `<tool_call>` and `</tool_call>` — Qwen's own control tokens, the same ids the
/// turn crate's engine tests pin. Only this file's tool-call fixture needs them.
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

/// **A child's session compacts when its TOOL RESULT overruns, and finishes the task.**
///
/// The operator's requirement, in their words: *"subagent is a normal session. it must
/// be able to compact on tool call overrun"*, and their question, *"we have overrun
/// protection exactly for this case, why this resurfaces again and again"*. The
/// measured answer this test is the regression guard for: a child of
/// `s-1789462738453908838` stopped after 207 rounds at 1,131,717 of 1,146,137 tokens
/// with a wall notice promising *"a compaction was ATTEMPTED"*, and the store showed
/// ONE transcript for it — no fork, no compaction item — while its parent compacted
/// 57 times in the same window. The seam that reacts to the wall lived only in
/// `Sessions::after_turn`, and a child never passes through `Sessions`.
///
/// The fixture is a CHILD's shape on purpose: a bare [`Harness`] driven directly, the
/// way `HarnessTaskRunner::run_to_completion` drives one — no `Sessions`, no daemon
/// worker, nothing between the turn and the wall. The overrun is the operator's own
/// case: round zero calls `read`, the result is appended between rounds, and the
/// ledger crosses the wall BEFORE round one is sent — so what is asserted is exactly
/// *"compacts before the next round rather than after the wall"*:
///
/// 1. the prompt still ANSWERS (the child's task finishes rather than dying with the
///    wall notice);
/// 2. the wall fired mid-turn, from the tool result — `context_wall` is in the log
///    after the round that ran the call, and before the compaction;
/// 3. the compaction ran — `compacting now` precedes a `compacted` warning, and the
///    transcript FORKED (`#t1`), which is the store row the dead child never had.
#[test]
fn a_child_session_compacts_on_a_tool_result_overrun_and_finishes() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    use letibot_sessionlog::event::SessionEvent;

    let dir = TempDir::new("harnessd-child-wall");
    let path = dir.path().join("sessions.db");
    let session_id = "child-wall-test";
    let mut cfg = config(&path, session_id);
    // The window that makes the tool results an overrun, at the scale the real
    // ones are. The stable prefix of a seated session is ~3634 tokens of this
    // vocabulary; `read` renders ~15 tokens per line (gutter and all) and is
    // byte-capped per call, so the fixture reaches the wall the way the dead
    // child did — by ACCUMULATION across a batch of calls, not one giant result.
    // At 32768 the headroom is 2048 (`max(w/16, 2048).min(w/4)`), the wall at
    // 30720, and prefix plus ten reads of a 260-line file (~3950 tokens each)
    // lands at ~43000 — past the wall and past the window, which is the state
    // `plan_overrun` calls `Cut` and answers with the two-half summary a local
    // server takes. That is the arm a wall-triggered compaction really runs on
    // big sessions; the ordinary arm needs resident + 8192 ≤ window, which no
    // session at its wall ever satisfies.
    cfg.context_window = Some(32768);

    // The file the child reads: 260 numbered lines, ~3950 tokens as `read`
    // renders them, under both the 200-line default cap's explicit-limit door and
    // the 32 KiB byte cap. Written before the harness opens, because the tool
    // that reads it is seated against this workspace at open.
    let big = dir.path().join("big.txt");
    let mut body = String::new();
    for i in 0..260 {
        body.push_str(&format!(
            "line {i}: the quick brown fox jumps over the lazy dog\n"
        ));
    }
    std::fs::write(&big, body).expect("the file to read is written");
    // `read` takes a path relative to the session root, and the root is `/tmp`
    // (`Config::for_this_box("/tmp")` in `config` above) — which is where this
    // temp dir already is.
    let rel = big
        .strip_prefix("/tmp")
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| big.display().to_string());

    let parts = load_parts(&cfg);
    let vocab = &parts.vocab;

    // Round zero: a BATCH of ten `read` calls, end — the turn hands the harness
    // ten calls to run in order, which is how one round's results push a session
    // past the wall (the operator's child crossed it on its 207th). Each body is
    // the QWEN dialect's own tool-call shape — `<function=read>` with
    // `<parameter>` entries, the format `parse.rs`'s round-trip test pins — and
    // the fences carry their literal text, because that is what a boundary looks
    // like on the wire. A final `eos` ends the turn with the calls pending, which
    // is what makes the harness run them.
    let mut round0 = vec![canned::Frame::Progress {
        total: 30,
        processed: 30,
    }];
    round0.push(canned::Frame::Token {
        id: THINK_OPEN,
        text: "",
    });
    round0.push(canned::Frame::Token {
        id: THINK_CLOSE,
        text: "",
    });
    let mut control = 2u64;
    let mut spoken_ids = 0usize;
    for offset in (0..10).map(|n| 1 + n * 260) {
        let call = format!(
            "\n<function=read>\n<parameter=path>\n{rel}\n</parameter>\n<parameter=offset>\n{offset}\n</parameter>\n<parameter=limit>\n260\n</parameter>\n</function>\n"
        );
        let call_ids = ids(vocab, &call);
        spoken_ids += call_ids.len();
        control += 2;
        round0.push(canned::Frame::Token {
            id: TOOL_CALL_OPEN,
            text: "<tool_call>",
        });
        round0.extend(spoken(vocab, &call_ids));
        round0.push(canned::Frame::Token {
            id: TOOL_CALL_CLOSE,
            text: "</tool_call>",
        });
    }
    round0.push(canned::Frame::Final {
        stop_type: "eos",
        // Every control token besides the bodies: the think fences and, per call,
        // the two tool-call fences. The accumulator counts what was streamed, so
        // this has to agree with it exactly or the turn dies in `CountMismatch`
        // before the wall is ever reached.
        n_decoded: (spoken_ids as u64) + control,
        n_prompt: 30,
        cache_n: 0,
    });

    // The compaction's two half-summaries (a local overrun arm summarises BOTH
    // halves, over a scratch transcript), then the continuation's answer: plain
    // turns all, because a summary that proposed a tool call would refuse the
    // fork and a continuation that answers has finished the task.
    let half = a_plain_answer_turn(
        vocab,
        "summarising",
        "the operator asked for a file; it was read ten times; the work continues",
        30,
    );
    let half2 = a_plain_answer_turn(
        vocab,
        "summarising",
        "the operator asked for a file; it was read ten times; the work continues",
        30,
    );
    let continued = a_plain_answer_turn(vocab, "resuming", "the task is finished", 30);
    // Spares, in case a path through the fix asks one turn more than the three
    // above: an undersupplied server turns a scripted answer into a socket error
    // and the test would fail for a reason that is not the one under test.
    // (`Frame` is not `Clone` — the turn crate's support keeps it minimal — so
    // the spares are built, not copied.)
    let spare = a_plain_answer_turn(vocab, "resuming", "the task is finished", 30);
    let spare2 = a_plain_answer_turn(vocab, "resuming", "the task is finished", 30);
    let scripts = vec![round0, half, half2, continued, spare, spare2];
    let serv = canned::Canned::serve_each(scripts, 8);
    cfg.endpoint = serv.endpoint.clone();

    // A CHILD's harness: opened bare, held directly, no `Sessions` anywhere near
    // it. That is the whole of the defect's setting — everything the daemon's own
    // sessions get around a turn, this harness must supply itself.
    let hub = Hub::new(session_id);
    let mut h = Harness::open(&parts, cfg.clone(), hub.clone()).expect("the child opens");

    let out = h.submit_as_a_normal_session("read big.txt and finish the task");
    assert!(
        out.is_ok(),
        "the child's task finishes past the wall: {out:?}"
    );
    assert!(
        out.as_ref().is_ok_and(|r| !r.text.is_empty()),
        "the answer is the continuation's, not a silent empty: {out:?}"
    );

    let events: Vec<SessionEvent> = hub.retained().iter().map(|e| e.event.clone()).collect();
    let at = |code: &str, text: &str| {
        events
            .iter()
            .position(|e| matches!(e, SessionEvent::Warning { code: c, detail, .. } if c == code && detail.contains(text)))
            .unwrap_or_else(|| {
                panic!("no {code} warning saying {text:?} — the child's own log:\n{events:#?}")
            })
    };
    // The overrun came from the TOOL RESULT: the wall was announced after the
    // round that ran the call, and the compaction before the next turn was sent.
    let wall = at("context_wall", "stopping this turn");
    let compacting = at("auto_compact", "compacting now");
    let compacted = at("compacted", "tokens");
    assert!(
        wall < compacting,
        "the wall is announced before the compaction reacts to it: {wall} < {compacting}"
    );
    assert!(
        compacting < compacted,
        "the compaction is attempted before it is reported: {compacting} < {compacted}"
    );
    // ...and the turn that finished the task ran AFTER the compaction, on the
    // summary — a TurnStarted later than the report is the continuation.
    let last_started = events
        .iter()
        .rposition(|e| matches!(e, SessionEvent::TurnStarted { .. }))
        .expect("a turn ran after the compaction");
    assert!(
        compacted < last_started,
        "the continuation runs on the summary, after the fork: compacted at {compacted}, \
         last TurnStarted at {last_started}"
    );

    // **The store row the dead child never had.** The fork is what a compaction
    // leaves behind — one transcript became two — and `transcript_id` is the
    // store's own id for the new base.
    assert_eq!(
        h.transcript_id(),
        format!("{session_id}#t1"),
        "the child's transcript forked onto the compaction's base"
    );
}
