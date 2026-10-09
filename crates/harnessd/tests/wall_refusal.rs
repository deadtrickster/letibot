//! **The wall as the provider speaks it**: a context-length refusal is the
//! wall, and every door that starts a turn recovers from it.
//!
//! The wedge this file is the regression guard for was measured 2026-10-09 on
//! `s-1789919514688401228` (pg-noop) on `deepseek/deepseek-flash`:
//!
//! * the daemon said `context 1000000 tokens; compacts automatically with
//!   62500 left`, so the operator believed the wall was managed;
//! * ONE `compacting:` line in the whole log (`grep -c` → 1), after which the
//!   no-progress guard switched automatic compaction off in memory;
//! * the next thirty requests went out at 1,048,624 / 1,049,070 / 1,049,191
//!   tokens against the provider's 1,048,576 and came back
//!   `http 400: This model's maximum context length is …` — the FIRST
//!   attributed to `monitor` (an automatic wake), the LAST to `todo check`
//!   (the plan nag), and none of the thirty recovered, because the refusal
//!   landed as a backend error and every recovery was gated on
//!   `HarnessError::ContextWall`.
//!
//! Every fixture here replays the two measured refusal bodies VERBATIM (the
//! plain-text OpenAI shape deepseek answered with, and llama.cpp's
//! `exceed_context_size_error` JSON), because a matcher is only as narrow as
//! its fixtures are real. It needs the vocabulary GGUF and not a model: the
//! refusal is bytes on the wire, and the compaction's summary turns are
//! answered by the canned server the turn crate's tests replay.

use letibot_harnessd::Sessions;
use letibot_harnessd::config::Config;
use letibot_harnessd::harness::{Harness, HarnessError};
use letibot_harnessd::{Dialect, Parts};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{SessionRecord, Store, TodoBy, TodoCondition, TodoItem, TodoStatus};

/// The canned server the turn crate's tests replay, shared by path rather than
/// copied — a second copy of the wire shape would drift from the first.
#[path = "../../turn/tests/support/canned.rs"]
mod canned;

use canned::Reply;

const WANTED: Dialect = Dialect::Qwen;

/// **The measured deepseek body, verbatim** — an OpenAI-shaped provider, plain
/// text, the numbers in the sentence and a request id trailing. The request id
/// is kept because a matcher that stopped at `Please reduce the length` would
/// be matching a sentence, and this body is the sentence the operator read.
const OPENAI_REFUSAL: &str = "This model's maximum context length is 1048576 tokens. However, you requested 1463497 tokens (1463497 in the messages, 0 in the completion). Please reduce the length of the messages or completion. (request_id: 8a448ce7-fabf-4ffa-b350-16e6ddeed6ac)";

/// **The measured llama.cpp body, verbatim** — JSON with a type field, the
/// numbers as fields. This is the structured half of the matcher's diet.
const LLAMACPP_REFUSAL: &str = r#"{"error":{"code":400,"message":"request (1504198 tokens) exceeds the available context size (262144 tokens), try increasing it","type":"exceed_context_size_error","n_prompt_tokens":1504198,"n_ctx":262144}}"#;

/// A 400 that is NOT about context length — also measured, in the same log —
/// and the shape the matcher must refuse to read as the wall.
const INVALID_TOKENS: &str = r#"{"error":{"code":400,"message":"Prompt contains invalid tokens","type":"invalid_request_error"}}"#;

fn config(store: &std::path::Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = WANTED;
    // No retry ladder: a refusal is deterministic in the bytes, and waiting
    // 1+2+4+8+16+32 seconds to be told so again is the suite's time, not the
    // test's question.
    cfg.http_retries = 0;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg
}

/// A directory that removes itself, because a test that leaks a store per run
/// eventually fills the disk and the run that finds out is not this one.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
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
const IM_END: u32 = 248046;
const THINK_OPEN: u32 = 248068;
const THINK_CLOSE: u32 = 248069;
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

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
/// block, answers, ends. No tool calls, so the round loop ends after one round.
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

/// **The wedge's own first half**: one round that answers the prompt with a
/// BATCH of ten `read` calls, exactly the shape that pushes a session past its
/// wall by ACCUMULATION (the operator's child crossed it on its 207th call;
/// this fixture crosses it on the first round so the test does not spend ten
/// turns getting there). The calls end the turn pending, which is what makes
/// the harness run them and append ~40k tokens of results between rounds.
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

/// The file the ten reads read: 260 numbered lines, ~3950 tokens as `read`
/// renders them. Ten of those past a 32768-token window is the `Cut` state —
/// a history that does not fit with any room for a tail, which is exactly the
/// state `plan_overrun` exists for and the one the wedge never reached.
fn write_the_big_file(dir: &TempDir) -> String {
    let big = dir.path().join("big.txt");
    let mut body = String::new();
    for i in 0..260 {
        body.push_str(&format!(
            "line {i}: the quick brown fox jumps over the lazy dog\n"
        ));
    }
    std::fs::write(&big, body).expect("the file to read is written");
    big.strip_prefix("/tmp")
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| big.display().to_string())
}

/// The session row a resumed daemon needs, carrying the guard's stood-down
/// pair — the state the restart used to lose.
fn seed_a_stood_down_session(path: &std::path::Path, session_id: &str) {
    let s = Store::open(path).expect("seeding");
    s.put_session(&SessionRecord {
        id: session_id.into(),
        title: Some("the wedged session".into()),
        model_id: "m".into(),
        dialect_sha: "sha".into(),
        workspace_root: "/tmp".into(),
        owner: "dead".into(),
        role: None,
        approvers: vec![],
        parent_session_id: None,
    })
    .expect("the session row");
    // The pair the guard wrote when its one compaction gained no room. The
    // numbers are the fixture's own scale, not the log's — the QUESTION is
    // whether the pair survives, not whether this conversation is 1.8M tokens.
    s.set_auto_compact_stood_down(session_id, Some((43_000, 42_500)))
        .expect("the stood-down pair");
}

/// The warnings a session's log holds, code and detail, in order — the order
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

/// **THE CHAIN, through the daemon's own door, with the guard already fired.**
///
/// This is the assertion the whole change exists for: `auto_compact` OFF (the
/// no-progress guard's state, restored from the store the way a restarted
/// daemon now restores it), a turn refused for context length STILL compacts —
/// through the forced door, onto the `Cut` fold that deals with a history too
/// big for a summary in place — and the prompt finishes on the summary rather
/// than dying at the wall. Before the split, every letter of that sentence was
/// false: the recovery read the flag, the flag was off, and the session met
/// the same refusal on every turn for the rest of the daemon's life.
#[test]
fn a_refused_prompt_compacts_under_duress_with_auto_compact_off_and_finishes() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-refused-daemon");
    let path = dir.path().join("sessions.db");
    let session_id = "refused-daemon-test";
    let rel = write_the_big_file(&dir);

    // The guard's finding is on the row BEFORE the daemon opens, exactly as a
    // restart would find it — which also proves the restore: without
    // `restore_auto_compact` the pre-flight below would find the flag ON and
    // compact before the send, and the refusal the test exists for would never
    // be sent.
    seed_a_stood_down_session(&path, session_id);

    let mut cfg = config(&path, session_id);
    // The window that makes ten reads an overrun, at the scale the real ones
    // are: headroom 2048, wall 30720, prefix ~3634 + ten ~3950-token results
    // ≈ 43000 — past the wall and past the window, which is the `Cut` state.
    cfg.context_window = Some(32768);

    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;

    // Request one: the prompt turn, answered with the batch of reads. Request
    // two: round one, REFUSED in deepseek's own words, verbatim. Then the two
    // half-summaries a local `Cut` fold runs, the continuation's answer, and
    // spares — an undersupplied server turns a scripted answer into a socket
    // error and the test would fail for a reason that is not the one under
    // test.
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
            body: OPENAI_REFUSAL.into(),
        },
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

    // The guard's finding came back with the session: the pre-emptive door is
    // stood down, or none of what follows means what it must.
    assert!(
        sessions
            .harness_of(session_id)
            .is_some_and(|h| !h.config().auto_compact),
        "the stood-down pair on the row restores as auto_compact = false"
    );

    let out = sessions.submit(session_id, "read big.txt and finish the task");
    assert!(
        out.is_ok(),
        "the prompt finishes on the summary, past a refused round: {out:?}"
    );

    let warnings = warnings_of(&hub);

    // (1) The refusal was classified AS THE WALL — the warning says the
    // provider refused, and names the provider's own two numbers beside this
    // box's ledger count, so the operator can compare without guessing units.
    let refused = at(&warnings, "context_wall", "the PROVIDER refused the prompt");
    let detail = &warnings[refused].1;
    assert!(
        detail.contains("1463497") && detail.contains("1048576"),
        "the provider's numbers are named, not paraphrased: {detail}"
    );

    // (2) The compaction that answered it was FORCED — announced as under
    // duress, because the automatic one is off and an operator watching a
    // session that "compacts automatically" deserves to know why this ran.
    let duress = at(&warnings, "auto_compact", "under duress");
    assert!(
        refused < duress,
        "the wall is announced before the compaction reacts to it"
    );

    // (3) **It reached the overrun branch.** The local `Cut` fold announces
    // itself in its own words — two halves that overlap — and that arm is the
    // strategy for exactly the refused state: a history with no room for a
    // summary in place. The ordinary fold never runs on a session at its wall
    // (it needs resident + headroom ≤ window), so the sentence is evidence of
    // WHICH fold ran, not decoration.
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
}

/// **The same chain through a child's own seam**, on the other measured body.
///
/// `submit_as_a_normal_session` is the tail a subagent's thread gives its
/// prompts — no `Sessions`, no `after_turn`, nothing but the harness — and it
/// is where the 2026-10-06 child wedge lived. The refusal here is llama.cpp's
/// JSON shape, so both measured bodies are proven through a real door and not
/// only through the matcher's unit tests.
#[test]
fn a_refused_prompt_through_a_childs_seam_compacts_and_finishes() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-refused-child");
    let path = dir.path().join("sessions.db");
    let session_id = "refused-child-test";
    let rel = write_the_big_file(&dir);

    let mut cfg = config(&path, session_id);
    cfg.context_window = Some(32768);
    // The wedge's own state: the guard has fired, the flag is off. The child
    // has no store row here because a spawned child is a NEW session — the
    // flag is the fact, and this test sets it the way the guard does.
    cfg.auto_compact = false;

    let parts = Parts::load(&cfg).expect("the vocabulary must load");
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
    // Under duress, onto the two-half fold, forked, finished — the same chain,
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

/// **A 400 that is not about context length stays a backend error.**
///
/// The matcher's narrowness is the whole of its safety: a quota refusal, a
/// bad token, a wrong model name — each is a 400 with a body, and any of them
/// misread as the wall would put a session through a compaction it did not
/// need on top of a failure it did not deserve. This is the measured
/// invalid-tokens body from the same log.
#[test]
fn a_non_context_400_stays_a_backend_error() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-refused-other");
    let path = dir.path().join("sessions.db");
    let session_id = "refused-other-test";
    let mut cfg = config(&path, session_id);
    cfg.context_window = Some(32768);

    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;

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

/// **The two automatic doors take the pre-turn check** — Repair 3.
///
/// `nag_turn` (the plan check — the LAST of the thirty refusals was attributed
/// to `todo check`) and `wake` (a monitor firing — the FIRST was attributed to
/// `monitor`) both start a turn, and both used to go straight to
/// `submit_item`. The pre-flight is insurance rather than the load-bearing fix
/// — a refused nag or wake is classified as the wall and recovered by
/// `after_turn` like any other turn — but it spares the round-trip the
/// daemon's own numbers can predict, and this is the assertion that it runs:
/// at the wall with `auto_compact` on, the door compacts BEFORE the turn it
/// was going to send.
///
/// The fixture both door tests stand on: a session whose store carries the
/// todo rows the door under test will speak about (seeded BEFORE the harness
/// opens, because the board is restored at open — the resume's own rule), a
/// window of 32768, and the parts. The test then points `cfg.endpoint` at its
/// own canned server, opens the harness, and wedges it with one submit whose
/// ten reads cross the wall — a bare harness returns the wall with nothing
/// tidied, which is the state every door then meets.
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
            workspace_root: "/tmp".into(),
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
    cfg.context_window = Some(32768);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    (dir, cfg, parts)
}

#[test]
fn the_nag_door_compacts_before_sending_when_the_next_turn_would_not_fit() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let (dir, mut cfg, parts) = a_session_at_the_wall(
        "harnessd-nag-wall",
        &[TodoItem {
            content: "finish the migration".into(),
            status: TodoStatus::InProgress,
            by: TodoBy::Model,
            when: None,
        }],
    );
    let rel = write_the_big_file(&dir);

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
    // harness has no tail) — the state the door then meets.
    let out = h.submit("read big.txt and fill the context");
    assert!(matches!(out, Err(HarnessError::ContextWall { .. })));

    // The door: the plan is unfinished, so the nag has something to say — and
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    // A todo row conditioned on a job handle nothing knows: **absence is the
    // condition** (`TodoCondition::Job`'s own rule), so the row is due the
    // moment the wake looks — the cheapest honest way to give a wake something
    // to say.
    let (dir, mut cfg, parts) = a_session_at_the_wall(
        "harnessd-wake-wall",
        &[TodoItem {
            content: "ship the pane".into(),
            status: TodoStatus::InProgress,
            by: TodoBy::Model,
            when: Some(TodoCondition::Job {
                handle: "j-nothing".into(),
            }),
        }],
    );
    let rel = write_the_big_file(&dir);

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

    // The wedge, then the door — same shape as the nag's.

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
