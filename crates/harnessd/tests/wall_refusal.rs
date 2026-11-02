//! The wall as the provider speaks it: a context-length refusal is the wall, and every door
//! that starts a turn recovers from it. The wedge this file is the regression guard for

use letibot_harnessd::Sessions;
use letibot_harnessd::config::Config;
use letibot_harnessd::harness::{Harness, HarnessError};
use letibot_harnessd::{Dialect, Parts};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{SessionRecord, Store, TodoBy, TodoCondition, TodoItem, TodoStatus};

/// The shared fixtures: the canned turn these tests replay, and the vocabulary a harness
/// opens with.
mod support;

use support::canned;
use support::canned::Reply;
use support::qwen::{a_plain_answer_turn, ids, spoken};

const WANTED: Dialect = Dialect::Qwen;

/// The measured deepseek body, verbatim - an OpenAI-shaped provider, plain text, the numbers in
/// the sentence and a request id trailing. A matcher that stopped at `Please reduce the length` would
/// be matching a sentence.
const OPENAI_REFUSAL: &str = "This model's maximum context length is 1048576 tokens. However, you requested 1463497 tokens (1463497 in the messages, 0 in the completion). Please reduce the length of the messages or completion. (request_id: 8a448ce7-fabf-4ffa-b350-16e6ddeed6ac)";

/// The measured llama.cpp body, verbatim - JSON with a type field, the
/// numbers as fields. This is the structured half of the matcher's diet.
const LLAMACPP_REFUSAL: &str = r#"{"error":{"code":400,"message":"request (1504198 tokens) exceeds the available context size (262144 tokens), try increasing it","type":"exceed_context_size_error","n_prompt_tokens":1504198,"n_ctx":262144}}"#;

/// A 400 that is NOT about context length - also measured, in the same log -
/// and the shape the matcher must refuse to read as the wall.
const INVALID_TOKENS: &str = r#"{"error":{"code":400,"message":"Prompt contains invalid tokens","type":"invalid_request_error"}}"#;

fn config(store: &std::path::Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = WANTED;
    // No retry ladder: a refusal is deterministic in the bytes, and waiting 1+2+4+8+16+32 seconds to
    // be told so again is the suite's time, not the test's question.
    cfg.http_retries = 0;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.vocab_gguf = support::vocab::gguf();
    cfg
}

/// A directory that removes itself, because a test that leaks a store per run
/// eventually fills the disk and the run that finds out is not this one.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos(),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
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
const THINK_OPEN: u32 = 248068;
const THINK_CLOSE: u32 = 248069;
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

/// The wedge's own first half: one round that answers the prompt with a BATCH of ten `read`
/// calls, which is the shape that pushes a session past its wall by ACCUMULATION.
fn a_turn_of_ten_reads(vocab: &letibot_tokencore::Vocab, rel: &str) -> Vec<canned::Frame> {
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
        // The accumulator counts what was streamed; this must agree exactly or
        // the turn dies in `CountMismatch` before the wall is ever reached.
        n_decoded: (spoken_ids as u64) + control,
        n_prompt: 30,
        cache_n: 0,
    });
    round0
}

/// The file the ten reads read: 260 numbered lines, ~3950 tokens as `read` renders them
/// and [`ten_reads_of`] measures that rather than trusting this number.
fn write_the_big_file(dir: &TempDir, cfg: &mut Config) -> String {
    let big = dir.path().join("big.txt");
    let mut body = String::new();
    for i in 0..260 {
        body.push_str(&format!(
            "line {i}: the quick brown fox jumps over the lazy dog\n"
        ));
    }
    std::fs::write(&big, body).expect("the file to read is written");
    cfg.workspace = dir.path().to_path_buf();
    "big.txt".to_string()
}

/// The margin `plan_cut` adds beside `WRITE_ROOM`. It is private in `letibot_turn`, so it is
/// a number here - and the assertion in `a_refusal_on_a_stuck_session_still_folds` is what
/// fails, with the numbers, if it ever grows.
const CUT_MARGIN: u64 = 8_192;

/// The room a two-half fold needs beyond the prompt itself - `WRITE_ROOM` and `CUT_MARGIN`.
const CUT_ROOM: u64 = letibot_turn::compaction::WRITE_ROOM + CUT_MARGIN;

/// The prompt this box composes for this seat, in tokens - measured, through the door the
/// daemon uses.
fn the_prompt_this_box_composes(tag: &str, parts: &Parts, cfg: &Config) -> u64 {
    let dir = TempDir::new(tag);
    let id = "the-prompt-this-box-composes";
    let mut probe = cfg.clone();
    probe.store = Some(dir.path().join("sessions.db"));
    probe.session_id = id.to_string();
    let registry = Registry::new();
    registry
        .create(id, "", Sessions::wiring(&probe))
        .expect("the probe is in the registry");
    let sessions = Sessions::open_first(parts, probe, registry).expect("the probe opens");
    sessions
        .harness_of(id)
        .expect("the probe is open")
        .prefix_tokens()
        .len() as u64
}

/// A window the two-half fold fits in, from the prompt that was measured - the prompt
/// the room `plan_cut` needs beyond it, and the reserve a turn needs
/// ([`Config::headroom`], which is why the sum is not the answer on its own).
fn a_window_the_fold_fits_in(cfg: &Config, prompt: u64) -> u64 {
    let mut sizing = cfg.clone();
    sizing.context_window = Some(prompt + CUT_ROOM);
    prompt + CUT_ROOM + sizing.headroom()
}

/// What the ten `read`s of [`write_the_big_file`]'s file append, in this vocabulary's tokens -
/// the file's own text, counted with the session's own counter. `read` numbers its lines as well, so
/// this under-counts, which is the side a wall wants to be on.
fn ten_reads_of(vocab: &letibot_tokencore::Vocab, dir: &TempDir, rel: &str) -> u64 {
    let body = std::fs::read_to_string(dir.path().join(rel)).expect("the file the reads read");
    ids(vocab, &body).len() as u64 * 10
}

/// The session row a resumed daemon needs, carrying the guard's stood-down pair - the state the
/// restart used to lose. A `measured` over the window is not the provider's count and is refused
/// (`restore_auto_compact`).
fn seed_a_stood_down_session(path: &std::path::Path, session_id: &str, before: u64, measured: u64) {
    let s = Store::open(path).expect("seeding");
    s.put_session(&SessionRecord {
        id: session_id.into(),
        title: Some("the wedged session".into()),
        model_id: "m".into(),
        dialect_sha: "sha".into(),
        // The scratch dir, where `write_the_big_file` puts the file a resumed
        // session reads: a resume seats the session at its ROW's root, not the config's.
        workspace_root: path
            .parent()
            .expect("the store is in a dir")
            .display()
            .to_string(),
        owner: "dead".into(),
        role: None,
        approvers: vec![],
        parent_session_id: None,
    })
    .expect("the session row");
    // The pair the controller wrote when its ladder ran out: what was folded, and the MEASURED count
    // after it - inside this fixture's window, because a provider's count cannot exceed its window.
    s.set_auto_compact_stood_down(session_id, Some((before, measured)))
        .expect("the stood-down pair");
}

/// The warnings a session's log holds, code and detail, in order - the order
/// is half of every assertion below (announced before attempted before done).
fn warnings_of(hub: &Hub) -> Vec<(String, String)> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning { code, detail, .. } => Some((code.clone(), detail.clone())),
            _ => None,
        })
        .collect()
}

fn at(warnings: &[(String, String)], code: &str, text: &str) -> usize {
    warnings
        .iter()
        .position(|(c, d)| c == code && d.contains(text))
        .unwrap_or_else(|| panic!("no {code} warning saying {text:?} — the log:\n{warnings:#?}"))
}

/// THE CHAIN, through the daemon's own door, with the guard already fired - and released by
/// the first measured count. The no-progress finding is restored from the store the way a
/// restarted daemon restores it, and `Stuck` is not for good
#[test]
fn a_stuck_session_is_released_by_a_measured_count_and_then_compacts_at_the_wall() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-refused-daemon");
    let path = dir.path().join("sessions.db");
    let session_id = "refused-daemon-test";

    let mut cfg = config(&path, session_id);
    let rel = write_the_big_file(&dir, &mut cfg);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;
    let prompt = the_prompt_this_box_composes("harnessd-refused-daemon-prompt", &parts, &cfg);
    let history = ten_reads_of(vocab, &dir, &rel);
    // The middle of the band: far enough above the fold's floor that the fold is not
    // `Hopeless`, far enough below the reads that they overrun it.
    let window = prompt + CUT_ROOM + history.saturating_sub(CUT_ROOM) / 2;
    let reserve = {
        let mut sizing = cfg.clone();
        sizing.context_window = Some(window);
        sizing.headroom()
    };

    // The guard's finding is on the row BEFORE the daemon opens, exactly as a
    // restart would find it - which also proves the restore (asserted below).
    seed_a_stood_down_session(&path, session_id, window + window / 4, window - reserve);
    cfg.context_window = Some(window);

    // Request one: the prompt turn, answered with the batch of reads. Request two: round one, REFUSED
    // in deepseek's own words, verbatim. Then the two half-summaries a local `Cut` fold runs, the
    // continuation's answer, and spares.
    let half = a_plain_answer_turn(
        vocab,
        "summarising",
        "the operator asked for a file; it was read ten times; the work continues",
        30,
    );
    let half2 = a_plain_answer_turn(
        vocab,
        "summarising",
        "the earlier history: the same file, read again; nothing else happened",
        30,
    );
    let continued = a_plain_answer_turn(vocab, "resuming", "the task is finished", 30);
    let spare = a_plain_answer_turn(vocab, "resuming", "the task is finished", 30);
    let spare2 = a_plain_answer_turn(vocab, "resuming", "the task is finished", 30);
    let scripts = vec![
        Reply::Frames(a_turn_of_ten_reads(vocab, &rel)),
        Reply::Frames(half),
        Reply::Frames(half2),
        Reply::Frames(continued),
        Reply::Frames(spare),
        Reply::Frames(spare2),
    ];
    let serv = canned::Canned::serve_replies(scripts, 8);
    cfg.endpoint = serv.endpoint.clone();

    let registry = Registry::new();
    registry
        .create(session_id, "", Sessions::wiring(&cfg))
        .expect("the session is in the registry");
    let hub = registry.get(session_id).expect("the hub is the registry's");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the session opens");

    // The guard's finding came back with the session: the pre-emptive door is stood down, or none of
    // what follows means what it must. A refusal is folded from `Stuck` too, which is this test's
    // chain.
    assert!(
        sessions
            .harness_of(session_id)
            .is_some_and(|h| h.wall().is_stuck() && h.config().auto_compact),
        "the stood-down pair on the row restores as the controller's Stuck"
    );

    let out = sessions.submit(session_id, "read big.txt and finish the task");
    assert!(
        out.is_ok(),
        "the prompt finishes on the summary, past the wall: {out:?}"
    );

    let warnings = warnings_of(&hub);

    // (1) The first round's measured count released the restored Stuck, and the
    // round loop's wall stop - stood down while stuck - stopped round one.
    let released = at(&warnings, "auto_compact", "re-armed: measured");
    let refused = at(&warnings, "context_wall", "stopping this turn");
    assert!(
        released < refused,
        "re-armed by a measurement, then the wall: {released} < {refused}"
    );

    // (2) The compaction that answered it was FORCED - announced as under
    // duress, because the automatic one is off and an operator watching a
    // session that "compacts automatically" deserves to know why this ran.
    let duress = at(&warnings, "auto_compact", "under duress");
    assert!(
        refused < duress,
        "the wall is announced before the compaction reacts to it"
    );

    // (3) It reached the overrun branch. The local `Cut` fold announces itself in its own words -
    // two halves that overlap - and the ordinary fold never runs on a session at its wall, so the
    // sentence is evidence of WHICH fold ran.
    let cut = at(&warnings, "auto_compact", "two halves that overlap");
    assert!(
        duress < cut,
        "the forced door announces before the fold it chose: {duress} < {cut}"
    );

    // (4) The fold landed: a `compacted` report, a forked transcript, and the
    // continuation AFTER the fork.
    let compacted = at(&warnings, "compacted", "tokens");
    assert!(
        cut < compacted,
        "the fold is chosen before it is reported: {cut} < {compacted}"
    );
    assert_eq!(
        sessions
            .harness_of(session_id)
            .expect("the session is still open")
            .transcript_id(),
        format!("{session_id}#t1"),
        "the wedged session forked onto the compaction's base"
    );

    // (5) And the guard did not re-fire: this fold made room, which is the
    // difference between a session that recovers and one that is stood down
    // for good.
    assert!(
        !warnings
            .iter()
            .any(|(c, _)| c == "auto_compact_no_progress"),
        "a fold that made room does not trip the no-progress guard: {warnings:#?}"
    );
    // (6) And a fold that made room, measured (a local base is measured as it lands)
    // re-arms the stuck session: the finding is no longer for good.
    assert!(
        sessions
            .harness_of(session_id)
            .is_some_and(|h| !h.wall().is_stuck()),
        "a measured fold below the wall releases Stuck"
    );
}

/// A stored "measured" past the window is not a provider's count, and is not restored. Such a row
/// is cleared, and the session opens armed: the next measured round decides.
#[test]
fn a_stored_pair_past_the_window_is_not_restored_and_is_cleared() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-implausible-stuck");
    let path = dir.path().join("sessions.db");
    let session_id = "implausible-stuck";
    // A pair that IS the window's - the next lines overwrite it with the implausible one
    // which is this test's subject.
    seed_a_stood_down_session(&path, session_id, 1_000_000, 900_000);
    Store::open(&path)
        .unwrap()
        .set_auto_compact_stood_down(session_id, Some((3_213_409, 1_687_673)))
        .unwrap();
    let mut cfg = config(&path, session_id);
    cfg.context_window = Some(1_000_000);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let hub = Hub::new(session_id);
    let mut h = Harness::open(&parts, cfg, hub).expect("the session opens");
    assert_eq!(
        h.restore_auto_compact(),
        None,
        "an implausible pair is not restored"
    );
    let row = Store::open(&path)
        .unwrap()
        .session(session_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        row.auto_compact_stood_down, None,
        "and it is cleared from the row"
    );
}

/// A refusal on a STUCK session is still folded - the wedge itself, against the controller
/// that replaced the latches.
#[test]
fn a_refusal_on_a_stuck_session_still_folds() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-refused-stuck");
    let path = dir.path().join("sessions.db");
    let session_id = "refused-stuck-test";

    let mut cfg = config(&path, session_id);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;

    // The window: this box's prompt, the room a two-half fold needs beyond it, and the
    // reserve a turn needs. Nothing here is a number that fits one home's instruction file.
    let prompt = the_prompt_this_box_composes("harnessd-refused-stuck-prompt", &parts, &cfg);
    let window = a_window_the_fold_fits_in(&cfg, prompt);
    let reserve = {
        let mut sizing = cfg.clone();
        sizing.context_window = Some(window);
        sizing.headroom()
    };

    // The finding the restart used to lose, in this window's own numbers: a fold of
    // `before` measured `measured`, still at the wall.
    seed_a_stood_down_session(&path, session_id, window + window / 4, window - reserve);
    cfg.context_window = Some(window);

    let half = a_plain_answer_turn(vocab, "summarising", "the operator asked for a task", 30);
    let half2 = a_plain_answer_turn(vocab, "summarising", "nothing else happened", 30);
    let continued = a_plain_answer_turn(vocab, "resuming", "the task is finished", 30);
    let spare = a_plain_answer_turn(vocab, "resuming", "the task is finished", 30);
    let scripts = vec![
        Reply::Status {
            code: 400,
            body: OPENAI_REFUSAL.into(),
        },
        Reply::Frames(half),
        Reply::Frames(half2),
        Reply::Frames(continued),
        Reply::Frames(spare),
    ];
    let serv = canned::Canned::serve_replies(scripts, 8);
    cfg.endpoint = serv.endpoint.clone();

    let registry = Registry::new();
    registry
        .create(session_id, "", Sessions::wiring(&cfg))
        .expect("the session is in the registry");
    let hub = registry.get(session_id).expect("the hub is the registry's");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the session opens");
    {
        let h = sessions
            .harness_of(session_id)
            .expect("the session is open");
        // The room the fold needs, asserted rather than assumed - the window was built from a
        // probe open's prompt and this is the seated one; a prompt that outgrew the window would make
        // the fold `Hopeless`, and this line is where that is said with the numbers.
        let seated = h.prefix_tokens().len() as u64;
        assert!(
            seated + CUT_ROOM < window,
            "a two-half fold needs {CUT_ROOM} tokens of room over the prompt: this session \
             was seated with {seated}, and {} of a {window}-token window is left",
            window - seated
        );
        assert!(h.wall().is_stuck(), "restored stuck");
    }

    let out = sessions.submit(session_id, "finish the task");
    assert!(
        out.is_ok(),
        "a refusal on a stuck session is folded and the prompt finishes: {out:?}"
    );
    let warnings = warnings_of(&hub);
    let refused = at(&warnings, "context_wall", "the PROVIDER refused the prompt");
    let duress = at(&warnings, "auto_compact", "under duress");
    let sliced = at(&warnings, "auto_compact", "this one is the sliced fold");
    let cut = at(&warnings, "auto_compact", "two halves that overlap");
    assert!(
        refused < duress && duress <= sliced && sliced < cut,
        "refused, then folded under duress with the hardest press: {warnings:#?}"
    );
    assert_eq!(
        sessions
            .harness_of(session_id)
            .expect("the session is still open")
            .transcript_id(),
        format!("{session_id}#t1"),
        "the stuck session forked onto the fold's base"
    );
}

/// The same chain through a child's own seam, on the other measured body.
/// `submit_as_a_normal_session` is the tail a subagent's thread gives its prompts - no
/// `Sessions`, no `after_turn`, nothing but the harness.
#[test]
fn a_refused_prompt_through_a_childs_seam_compacts_and_finishes() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-refused-child");
    let path = dir.path().join("sessions.db");
    let session_id = "refused-child-test";

    let mut cfg = config(&path, session_id);
    let rel = write_the_big_file(&dir, &mut cfg);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;
    let prompt = the_prompt_this_box_composes("harnessd-refused-child-prompt", &parts, &cfg);
    let history = ten_reads_of(vocab, &dir, &rel);
    cfg.context_window = Some(prompt + CUT_ROOM + history.saturating_sub(CUT_ROOM) / 2);
    // The wedge's own state: the guard has fired, the flag is off. The child
    // has no store row here because a spawned child is a NEW session - the
    // flag is the fact, and this test sets it the way the guard does.
    cfg.auto_compact = false;

    let half = a_plain_answer_turn(
        vocab,
        "summarising",
        "the operator asked for a file; it was read ten times; the work continues",
        30,
    );
    let half2 = a_plain_answer_turn(
        vocab,
        "summarising",
        "the earlier history: the same file, read again; nothing else happened",
        30,
    );
    let continued = a_plain_answer_turn(vocab, "resuming", "the task is finished", 30);
    let spare = a_plain_answer_turn(vocab, "resuming", "the task is finished", 30);
    let spare2 = a_plain_answer_turn(vocab, "resuming", "the task is finished", 30);
    let scripts = vec![
        Reply::Frames(a_turn_of_ten_reads(vocab, &rel)),
        Reply::Status {
            code: 400,
            body: LLAMACPP_REFUSAL.into(),
        },
        Reply::Frames(half),
        Reply::Frames(half2),
        Reply::Frames(continued),
        Reply::Frames(spare),
        Reply::Frames(spare2),
    ];
    let serv = canned::Canned::serve_replies(scripts, 8);
    cfg.endpoint = serv.endpoint.clone();

    // A CHILD's harness: opened bare, held directly, the way the runner drives
    // one. Everything the daemon's own sessions get around a turn, this
    // harness must supply itself.
    let hub = Hub::new(session_id);
    let mut h = Harness::open(&parts, cfg.clone(), hub.clone()).expect("the child opens");

    let out = h.submit_as_a_normal_session("read big.txt and finish the task");
    assert!(
        out.is_ok(),
        "the child's task finishes past a refused round: {out:?}"
    );

    let warnings = warnings_of(&hub);
    // The refusal was classified, with llama.cpp's own numbers named: this
    // body carries them as FIELDS, and the warning still says whose they are.
    let refused = at(&warnings, "context_wall", "the PROVIDER refused the prompt");
    assert!(
        warnings[refused].1.contains("1504198") && warnings[refused].1.contains("262144"),
        "the JSON body's fields are the numbers named: {}",
        warnings[refused].1
    );
    // Under duress, onto the two-half fold, forked, finished - the same chain
    // the other seam.
    let duress = at(&warnings, "auto_compact", "under duress");
    let cut = at(&warnings, "auto_compact", "two halves that overlap");
    let compacted = at(&warnings, "compacted", "tokens");
    assert!(refused < duress && duress < cut && cut < compacted);
    assert_eq!(
        h.transcript_id(),
        format!("{session_id}#t1"),
        "the child's transcript forked onto the compaction's base"
    );
}

/// A 400 that is not about context length stays a backend error. The matcher's narrowness
/// is the whole of its safety
#[test]
fn a_non_context_400_stays_a_backend_error() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-refused-other");
    let path = dir.path().join("sessions.db");
    let session_id = "refused-other-test";
    let mut cfg = config(&path, session_id);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;
    // A window this session fits in with room to spare, so the round-0 wall check cannot be what
    // answers: the 400 must come from the provider and stay a backend error.
    let prompt = the_prompt_this_box_composes("harnessd-refused-other-prompt", &parts, &cfg);
    cfg.context_window = Some(a_window_the_fold_fits_in(&cfg, prompt));

    let scripts = vec![
        Reply::Status {
            code: 400,
            body: INVALID_TOKENS.into(),
        },
        // A spare answer, in case the shape of the failure ever changes enough
        // to spend a second request; an undersupplied server is a socket error
        // and a pass for the wrong reason.
        Reply::Frames(a_plain_answer_turn(vocab, "thinking", "answered", 30)),
    ];
    let serv = canned::Canned::serve_replies(scripts, 2);
    cfg.endpoint = serv.endpoint.clone();

    let hub = Hub::new(session_id);
    let mut h = Harness::open(&parts, cfg.clone(), hub.clone()).expect("the session opens");
    let out = h.submit("anything at all");
    match out {
        Err(HarnessError::ContextWall { .. }) => panic!(
            "an invalid-tokens 400 is a backend error, not the wall — the matcher is too wide"
        ),
        Err(e) => assert!(
            e.to_string().contains("http 400"),
            "the failure is the provider's own status and body: {e}"
        ),
        Ok(_) => panic!("a refused request must not come back Ok"),
    }
    // And nothing compacted behind it: a compaction here would be the
    // misdiagnosis spending a summary turn.
    let warnings = warnings_of(&hub);
    assert!(
        !warnings.iter().any(|(c, _)| c == "compacted"),
        "no compaction ran behind a non-context refusal: {warnings:#?}"
    );
}

/// The two automatic doors take the pre-turn check. `nag_turn` (the plan check) and `wake`
/// (a monitor firing) both start a turn, and both used to go straight to `submit_item`.
fn a_session_at_the_wall(tag: &str, todos: &[TodoItem]) -> (TempDir, Config, Parts) {
    let dir = TempDir::new(tag);
    let path = dir.path().join("sessions.db");
    let session_id = format!("{tag}-test");
    {
        let s = Store::open(&path).expect("seeding");
        s.put_session(&SessionRecord {
            id: session_id.clone(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: dir.path().display().to_string(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .expect("the session row");
        if !todos.is_empty() {
            s.put_todos(&session_id, todos).expect("the todo rows");
        }
    }
    let mut cfg = config(&path, &session_id);
    cfg.workspace = dir.path().to_path_buf();
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let prompt = the_prompt_this_box_composes(&format!("{tag}-prompt"), &parts, &cfg);
    let history = ten_reads_of(&parts.vocab, &dir, &write_the_big_file(&dir, &mut cfg));
    cfg.context_window = Some(prompt + CUT_ROOM + history.saturating_sub(CUT_ROOM) / 2);
    (dir, cfg, parts)
}

#[test]
fn the_nag_door_compacts_before_sending_when_the_next_turn_would_not_fit() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (dir, mut cfg, parts) = a_session_at_the_wall(
        "harnessd-nag-wall",
        &[TodoItem {
            content: "finish the migration".into(),
            status: TodoStatus::InProgress,
            by: TodoBy::Model,
            when: None,
            needs: Vec::new(),
        }],
    );
    let rel = write_the_big_file(&dir, &mut cfg);

    let vocab = &parts.vocab;
    let half = a_plain_answer_turn(
        vocab,
        "summarising",
        "the operator asked for a file; it was read ten times; the work continues",
        30,
    );
    let half2 = a_plain_answer_turn(
        vocab,
        "summarising",
        "the earlier history: the same file, read again; nothing else happened",
        30,
    );
    let nag_answer = a_plain_answer_turn(vocab, "checking", "the plan is on track", 30);
    let spare = a_plain_answer_turn(vocab, "checking", "the plan is on track", 30);
    let scripts = vec![
        Reply::Frames(a_turn_of_ten_reads(vocab, &rel)),
        Reply::Frames(half),
        Reply::Frames(half2),
        Reply::Frames(nag_answer),
        Reply::Frames(spare),
    ];
    let serv = canned::Canned::serve_replies(scripts, 5);
    cfg.endpoint = serv.endpoint.clone();
    let hub = Hub::new(&cfg.session_id);
    let mut h = Harness::open(&parts, cfg.clone(), hub.clone()).expect("the session opens");

    // The wedge: round-loop wall after the ten reads, nothing tidied (a bare
    // harness has no tail) - the state the door then meets.
    let out = h.submit("read big.txt and fill the context");
    assert!(matches!(out, Err(HarnessError::ContextWall { .. })));

    // The door: the plan is unfinished, so the nag has something to say - and
    // THIS is the assertion, that it says it on a compacted base.
    let out = h.nag_turn().expect("the nag turn runs");
    assert!(
        out.is_some(),
        "the plan was unfinished, so a turn was spent"
    );

    let warnings = warnings_of(&hub);
    let compacting = at(&warnings, "auto_compact", "compacting now");
    let compacted = at(&warnings, "compacted", "tokens");
    assert!(compacting < compacted);
    // The nag's own turn STARTED after the fork landed: a TurnStarted later
    // than the report is the door's turn on the summary, not on the wall.
    let events: Vec<SessionEvent> = hub.retained().iter().map(|e| e.event.clone()).collect();
    let last_started = events
        .iter()
        .rposition(|e| matches!(e, SessionEvent::TurnStarted { .. }))
        .expect("the nag ran a turn");
    assert!(
        compacted < last_started,
        "the door compacts BEFORE it sends: compacted at {compacted}, its turn at {last_started}"
    );
    assert_eq!(
        h.transcript_id(),
        format!("{}#t1", cfg.session_id),
        "the nag's turn ran on the compaction's base"
    );
}

#[test]
fn the_wake_door_compacts_before_sending_when_the_next_turn_would_not_fit() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // A todo row conditioned on a job handle nothing knows: absence is the condition
    // (`TodoCondition::Job`'s own rule), so the row is due the moment the wake looks - the
    // cheapest honest way to give a wake something to say.
    let (dir, mut cfg, parts) = a_session_at_the_wall(
        "harnessd-wake-wall",
        &[TodoItem {
            content: "ship the pane".into(),
            status: TodoStatus::InProgress,
            by: TodoBy::Model,
            when: Some(TodoCondition::Job {
                handle: "j-nothing".into(),
            }),
            needs: Vec::new(),
        }],
    );
    let rel = write_the_big_file(&dir, &mut cfg);

    let vocab = &parts.vocab;
    let half = a_plain_answer_turn(
        vocab,
        "summarising",
        "the operator asked for a file; it was read ten times; the work continues",
        30,
    );
    let half2 = a_plain_answer_turn(
        vocab,
        "summarising",
        "the earlier history: the same file, read again; nothing else happened",
        30,
    );
    let wake_answer = a_plain_answer_turn(vocab, "waking", "the condition is met", 30);
    let spare = a_plain_answer_turn(vocab, "waking", "the condition is met", 30);
    let scripts = vec![
        Reply::Frames(a_turn_of_ten_reads(vocab, &rel)),
        Reply::Frames(half),
        Reply::Frames(half2),
        Reply::Frames(wake_answer),
        Reply::Frames(spare),
    ];
    let serv = canned::Canned::serve_replies(scripts, 5);
    cfg.endpoint = serv.endpoint.clone();
    let hub = Hub::new(&cfg.session_id);
    let mut h = Harness::open(&parts, cfg.clone(), hub.clone()).expect("the session opens");

    // The wedge, then the door - same shape as the nag's.

    let out = h.submit("read big.txt and fill the context");
    assert!(matches!(out, Err(HarnessError::ContextWall { .. })));

    let out = h.wake().expect("the wake runs");
    assert!(out.is_some(), "a due row was waiting, so a turn was spent");

    let warnings = warnings_of(&hub);
    let compacting = at(&warnings, "auto_compact", "compacting now");
    let compacted = at(&warnings, "compacted", "tokens");
    assert!(compacting < compacted);
    let events: Vec<SessionEvent> = hub.retained().iter().map(|e| e.event.clone()).collect();
    let last_started = events
        .iter()
        .rposition(|e| matches!(e, SessionEvent::TurnStarted { .. }))
        .expect("the wake ran a turn");
    assert!(
        compacted < last_started,
        "the door compacts BEFORE it sends: compacted at {compacted}, its turn at {last_started}"
    );
    assert_eq!(
        h.transcript_id(),
        format!("{}#t1", cfg.session_id),
        "the wake's turn ran on the compaction's base"
    );
}

/// A first round already past the window is stopped BEFORE it is sent. The doors before a
/// turn judge the conversation before the prompt is appended, with a sixteenth of the
/// window in reserve for it; the round loop judged only BETWEEN rounds.
#[test]
fn a_first_round_already_past_the_window_is_not_sent() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-round-zero-wall");
    let path = dir.path().join("sessions.db");
    let session_id = "round-zero-wall-test";
    let mut cfg = config(&path, session_id);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;
    let window = 2 * the_prompt_this_box_composes("harnessd-round-zero-wall-prompt", &parts, &cfg);
    cfg.context_window = Some(window);
    // An undersupplied server is a socket error, which would fail the turn for the
    // wrong reason; one answer is there so that sending is visibly a different outcome.
    let serv = canned::Canned::serve_replies(
        vec![Reply::Frames(a_plain_answer_turn(
            vocab, "thinking", "answered", 40_000,
        ))],
        1,
    );
    cfg.endpoint = serv.endpoint.clone();
    let hub = Hub::new(session_id);
    let mut h = Harness::open(&parts, cfg.clone(), hub.clone()).expect("the session opens");

    let paste = "the quick brown fox jumps over the lazy dog. ".repeat(4_000);
    match h.submit(&paste) {
        Err(e @ HarnessError::ContextWall { rounds: 0, .. }) => {
            let said = e.to_string();
            assert!(
                said.contains(&format!("of {window} tokens resident")),
                "the window as given, with its unit: {said}"
            );
        }
        Err(e) => panic!("the wall at round 0 was expected, got: {e}"),
        Ok(r) => panic!(
            "a prompt past the window was SENT and answered ({:?}); it must stop first",
            r.text
        ),
    }
}

/// A manual `/compact` reports the count it compacted FROM, as the automatic one does.
#[test]
fn a_manual_compact_reports_the_count_it_compacted_from() {
    use letibot_sessionlog::hub::{CommandKind, QueuedCommand};
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-manual-compact");
    let path = dir.path().join("sessions.db");
    let session_id = "manual-compact-test";
    let mut cfg = config(&path, session_id);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;
    let prompt = the_prompt_this_box_composes("harnessd-manual-compact-prompt", &parts, &cfg);
    let window = a_window_the_fold_fits_in(&cfg, prompt);
    cfg.context_window = Some(window);
    let answer = a_plain_answer_turn(vocab, "thinking", "the long answer, said once", 30);
    let summary = a_plain_answer_turn(
        vocab,
        "summarising",
        "the operator asked one question and it was answered",
        30,
    );
    let spare = a_plain_answer_turn(vocab, "summarising", "spare", 30);
    let serv = canned::Canned::serve_replies(
        vec![
            Reply::Frames(answer),
            Reply::Frames(summary),
            Reply::Frames(spare),
        ],
        3,
    );
    cfg.endpoint = serv.endpoint.clone();
    let registry = Registry::new();
    registry
        .create(session_id, "", Sessions::wiring(&cfg))
        .expect("the session is in the registry");
    let hub = registry.get(session_id).expect("the hub is the registry's");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the session opens");
    let words = "a question with some length to it, so the fork is smaller. ".repeat(200);
    sessions
        .submit(session_id, &words)
        .expect("the prompt turn is answered");

    let cmd = QueuedCommand {
        head_id: "test-head".into(),
        identity: "dead".into(),
        client_request_id: "req-compact".into(),
        at_seq: 0,
        enqueued_ms: 0,
        kind: CommandKind::Compact,
    };
    let _ = sessions.dispatch(session_id, &cmd);

    let report = hub
        .retained()
        .iter()
        .find_map(|e| match &e.event {
            SessionEvent::Warning {
                code, compaction, ..
            } if code == "compacted" => compaction.clone(),
            _ => None,
        })
        .expect("a manual compaction publishes its report");
    assert!(
        report.tokens_after < report.tokens_before,
        "the fixture compacts something: {} → {}",
        report.tokens_before,
        report.tokens_after
    );
    // `tokens_before` is the fork's `was_tokens`, which also holds the summary turn
    // itself (its instruction and its reply are appended before the fork) - so the
    // count the operator compacted from is at most that, and well above the base.
    assert!(
        report.resident > report.tokens_after && report.resident <= report.tokens_before,
        "`resident` is the count compacted FROM ({} → {}), not the new base's: {}",
        report.tokens_before,
        report.tokens_after,
        report.resident
    );
    assert_eq!(
        (report.window, report.headroom),
        (window, cfg.headroom()),
        "the window as given"
    );
}
