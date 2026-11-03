//! A model that stops because "my context is full" when it is not gets told so, through the
//! harness's own round loop. The canned server plays the model: one round ends the turn
//! with the model's sentence, verbatim, and no tool call - and one round says the same sentence
//! and calls a tool anyway, which is the branch where the correction waits for the step boundary.

use letibot_harnessd::Parts;
use letibot_harnessd::config::Config;
use letibot_harnessd::context_claim::{LOG, REPEATED};
use letibot_harnessd::harness::Harness;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_transcript::{Speaker, TranscriptItem, UserPart};

mod support;

use support::canned;
use support::canned::Reply;
use support::qwen::{IM_END, THINK_CLOSE, spoken};

/// The model's sentence, verbatim.
const EXCUSE: &str = "My context is at the wall, so I'll stop here rather than start a piece I can't finish: what's left of the port is scroll regions.";

/// The tokens Qwen spells a tool call with: a round that calls anything has a step boundary, which
/// is where a correction waits for a round that went on working.
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

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
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn config(dir: &TempDir, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box(&dir.path);
    cfg.dialect = letibot_harnessd::Dialect::Qwen;
    cfg.http_retries = 0;
    cfg.store = Some(dir.path.join("sessions.db"));
    cfg.session_id = session_id.to_string();
    cfg.vocab_gguf = support::vocab::gguf();
    cfg
}

/// A round that answers in plain text and makes no tool call - the shape that ends a turn. `n_prompt`
/// is what the provider reports the prompt cost, the other half of the ratio the wall is judged in.
fn a_plain_answer(vocab: &letibot_tokencore::Vocab, answer: &str, n_prompt: u64) -> Reply {
    a_thoughtful_answer(vocab, "thinking", answer, n_prompt)
}

/// The same, with the round's THINKING spelled by the caller - the shape a claim about the model's
/// own context comes in, since that is where a model cuts its work to a budget.
fn a_thoughtful_answer(
    vocab: &letibot_tokencore::Vocab,
    thought: &str,
    answer: &str,
    n_prompt: u64,
) -> Reply {
    let thought = vocab.tokenize_text(thought).expect("tokenizes");
    let answer_ids = vocab.tokenize_text(answer).expect("tokenizes");
    let mut frames = vec![canned::Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.extend(spoken(vocab, &thought));
    frames.extend(spoken(vocab, &[THINK_CLOSE]));
    frames.extend(spoken(vocab, &answer_ids));
    frames.extend(spoken(vocab, &[IM_END]));
    frames.push(canned::Frame::Final {
        stop_type: "eos",
        n_decoded: (thought.len() + answer_ids.len() + 2) as u64,
        n_prompt,
        cache_n: 0,
    });
    Reply::Frames(frames)
}

/// A round that says `excuse` and then calls a tool: the turn goes on, so the correction cannot be
/// the turn's own next round - it has to wait for the step boundary the call opens.
fn an_excuse_that_calls_a_tool(
    vocab: &letibot_tokencore::Vocab,
    excuse: &str,
    n_prompt: u64,
) -> Reply {
    let thought = vocab.tokenize_text("thinking").expect("tokenizes");
    let text = vocab.tokenize_text(excuse).expect("tokenizes");
    let call = vocab
        .tokenize_text(
            "\n<function=no_such_tool>\n<parameter=why>\nno reason\n</parameter>\n</function>\n",
        )
        .expect("tokenizes");
    let mut frames = vec![canned::Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.extend(spoken(vocab, &thought));
    frames.extend(spoken(vocab, &[THINK_CLOSE]));
    frames.extend(spoken(vocab, &text));
    frames.push(canned::Frame::Token {
        id: TOOL_CALL_OPEN,
        text: "<tool_call>",
    });
    frames.extend(spoken(vocab, &call));
    frames.push(canned::Frame::Token {
        id: TOOL_CALL_CLOSE,
        text: "</tool_call>",
    });
    frames.push(canned::Frame::Final {
        stop_type: "eos",
        // Every generated id: the thought, the fence that closes it, the sentence, the two
        // tool-call fences and the call itself.
        n_decoded: (thought.len() + text.len() + call.len() + 3) as u64,
        n_prompt,
        cache_n: 0,
    });
    Reply::Frames(frames)
}

/// The harness's context-check lines in the transcript, as the model will read them.
fn context_checks(h: &Harness) -> Vec<(letibot_transcript::Speaker, String)> {
    h.items()
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::User { speaker, parts } => parts.iter().find_map(|p| match p {
                UserPart::Text { text } if text.starts_with("[context check]") => {
                    Some((*speaker, text.clone()))
                }
                _ => None,
            }),
            _ => None,
        })
        .collect()
}

fn warnings_of(hub: &Hub, code: &str) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning {
                code: c, detail, ..
            } if c == code => Some(detail.clone()),
            _ => None,
        })
        .collect()
}

/// Every round's ending the log kept, oldest first, as its `ends_turn`.
fn round_endings(hub: &Hub) -> Vec<bool> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::TurnFinished { ends_turn, .. } => Some(*ends_turn),
            _ => None,
        })
        .collect()
}

/// A round's end is not a turn's end: the excuse is a round with no call, the harness nudges it
/// into a second one, and the first round's ending must say the turn goes on. A head told otherwise
/// drew `Responded` and then `Responding` again on every round boundary.
#[test]
fn the_round_a_nudge_continues_does_not_end_the_turn_on_the_wire() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-round-end");
    let mut cfg = config(&dir, "round-end");
    cfg.context_window = Some(1_000_000);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;
    let serv = canned::Canned::serve_replies(
        vec![
            a_plain_answer(vocab, EXCUSE, 30),
            a_plain_answer(vocab, "Continuing with the scroll regions.", 30),
        ],
        2,
    );
    cfg.endpoint = serv.endpoint.clone();

    let hub = Hub::new("round-end");
    let mut h = Harness::open(&parts, cfg, hub.clone()).expect("the session opens");
    let reply = h
        .submit("port the rest of the emulator")
        .expect("the turn runs");
    assert_eq!(
        reply.rounds, 2,
        "the premise: the line ran a second round: {reply:?}"
    );
    assert_eq!(
        round_endings(&hub),
        vec![false, true],
        "the nudged round is not the turn's end; the round that answered is"
    );
}

/// The leticl turn at a fill far from the wall: told, with the numbers, and the work continues.
#[test]
fn an_excuse_at_low_fill_is_answered_with_the_measured_fill_and_the_turn_continues() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-context-claim-low");
    let mut cfg = config(&dir, "context-claim-low");
    cfg.context_window = Some(1_000_000);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;
    let serv = canned::Canned::serve_replies(
        vec![
            a_plain_answer(vocab, EXCUSE, 30),
            a_plain_answer(vocab, "Continuing with the scroll regions.", 30),
        ],
        2,
    );
    cfg.endpoint = serv.endpoint.clone();

    let hub = Hub::new("context-claim-low");
    let mut h = Harness::open(&parts, cfg, hub.clone()).expect("the session opens");
    let reply = h
        .submit("port the rest of the emulator")
        .expect("the turn runs");

    assert_eq!(reply.rounds, 2, "the line continued the turn: {reply:?}");
    assert!(
        reply.text.contains("Continuing with the scroll regions."),
        "the second round is the reply: {:?}",
        reply.text
    );
    let checks = context_checks(&h);
    assert_eq!(checks.len(), 1, "one line, once: {checks:#?}");
    let (speaker, line) = &checks[0];
    assert_eq!(
        *speaker,
        Speaker::Agent,
        "the harness speaking, never the operator"
    );
    // The operator's ruling: stop giving the real numbers, and tell it to stop sizing and do the
    // work. No quote of the model's own sentence either.
    assert_eq!(
        line,
        "[context check] Don't size the work by context; the harness handles it. If context was \
         the only reason you stopped, continue the task."
    );
    assert!(warnings_of(&hub, REPEATED).is_empty());

    let log =
        std::fs::read_to_string(dir.path.join(".letibot").join(LOG)).expect("the claim is logged");
    let row: serde_json::Value =
        serde_json::from_str(log.lines().next().expect("one line")).expect("json");
    assert_eq!(row["slapped"], true, "{row}");
    assert_eq!(row["at_wall"], false, "{row}");
    // The window as configured: it used to be converted through the ratio the round measured, and
    // landed a token off.
    let window = row["window"].as_u64().expect("a measured window");
    assert_eq!(window, 1_000_000, "{row}");
}

/// **R93.A6: a claim found in the reasoning is not left lying at the head of the next turn.** The
/// reasoning's line does not continue the turn, so on a turn that ends cleanly there is nothing for
/// it to correct - said there it is the last item in the transcript, read at the start of an
/// unrelated later turn. The claim is still measured and logged.
#[test]
fn a_reasoning_claim_on_a_turn_that_ends_is_not_said() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-reasoning-claim-end");
    let mut cfg = config(&dir, "reasoning-claim-end");
    cfg.context_window = Some(1_000_000);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;
    let serv = canned::Canned::serve_replies(
        vec![a_thoughtful_answer(
            vocab,
            "My context is at 49%, so one call.",
            "Landed the port; the tests are green.",
            30,
        )],
        1,
    );
    cfg.endpoint = serv.endpoint.clone();

    let hub = Hub::new("reasoning-claim-end");
    let mut h = Harness::open(&parts, cfg, hub.clone()).expect("the session opens");
    let reply = h
        .submit("port the rest of the emulator")
        .expect("the turn runs");

    assert_eq!(reply.rounds, 1, "nothing continued the turn: {reply:?}");
    let checks = context_checks(&h);
    assert!(
        checks.is_empty(),
        "a turn that ended is told nothing it cannot act on: {checks:#?}"
    );
    // Measured all the same: the floor is tuned from what a turn's reasoning said, told or not.
    let log =
        std::fs::read_to_string(dir.path.join(".letibot").join(LOG)).expect("the claim is logged");
    let row: serde_json::Value =
        serde_json::from_str(log.lines().next().expect("one line")).expect("json");
    assert_eq!(row["source"], "reasoning", "{row}");
}

/// **The same excuse in a round that calls a tool.** The model has not stopped, so the correction is
/// not the turn's own next round: it rides the step boundary the call opens, and the round after it
/// is where the model reads it. Before this there was no harness-level test of that branch at all -
/// every round the fixtures played answered in plain text.
#[test]
fn an_excuse_in_a_round_that_calls_a_tool_rides_the_step_boundary() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-context-claim-tools");
    let mut cfg = config(&dir, "context-claim-tools");
    cfg.context_window = Some(1_000_000);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;
    let serv = canned::Canned::serve_replies(
        vec![
            an_excuse_that_calls_a_tool(vocab, EXCUSE, 30),
            a_plain_answer(vocab, "Continuing with the scroll regions.", 60),
        ],
        2,
    );
    cfg.endpoint = serv.endpoint.clone();

    let hub = Hub::new("context-claim-tools");
    let mut h = Harness::open(&parts, cfg, hub.clone()).expect("the session opens");
    let reply = h
        .submit("port the rest of the emulator")
        .expect("the turn runs");

    assert_eq!(
        reply.rounds, 2,
        "the call gave the turn a boundary: {reply:?}"
    );
    assert!(
        reply.tool_calls >= 1,
        "the premise: a call was made: {reply:?}"
    );
    assert!(
        h.items()
            .iter()
            .any(|i| matches!(i, TranscriptItem::ToolResult { .. })),
        "no result was appended for the call, so there was no boundary to ride"
    );
    let checks = context_checks(&h);
    assert_eq!(checks.len(), 1, "one line, once: {checks:#?}");
    let (speaker, line) = &checks[0];
    assert_eq!(
        *speaker,
        Speaker::Agent,
        "the harness speaking, never the operator"
    );
    assert_eq!(
        line,
        "[context check] Don't size the work by context; the harness handles it. If context was \
         the only reason you stopped, continue the task."
    );
    assert!(
        reply.text.contains("Continuing with the scroll regions."),
        "the round after the boundary is the reply: {:?}",
        reply.text
    );

    let log =
        std::fs::read_to_string(dir.path.join(".letibot").join(LOG)).expect("the claim is logged");
    let row: serde_json::Value =
        serde_json::from_str(log.lines().next().expect("one line")).expect("json");
    assert_eq!(row["slapped"], true, "{row}");
    assert_eq!(row["at_wall"], false, "{row}");
}

/// The same sentence AT the wall is true - no line, and the turn ends where the model ended it. The
/// window is set to what the session already holds, and automatic compaction is off so the round
/// loop does not stop the turn for the wall itself.
#[test]
fn the_same_excuse_at_the_wall_is_left_alone() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-context-claim-wall");
    let mut cfg = config(&dir, "context-claim-wall");
    cfg.auto_compact = false;
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;

    // The prefix this session opens with is what the window is set to, so the session is at the wall
    // before the turn starts. The provider reports the same count, so the ratio is ~1 and the window
    // is not scaled away from it.
    let resident = {
        let probe = Harness::open(&parts, cfg.clone(), Hub::new("probe")).expect("opens");
        probe.ledger_len() as u64
    };
    let serv = canned::Canned::serve_replies(
        vec![
            a_plain_answer(vocab, EXCUSE, resident + 16),
            // A spare, so an unexpected second round is an assertion and not a socket
            // error.
            a_plain_answer(vocab, "Continuing.", resident + 16),
        ],
        2,
    );
    cfg.endpoint = serv.endpoint.clone();
    cfg.session_id = "context-claim-wall-2".into();
    cfg.context_window = Some(resident);

    let hub = Hub::new("context-claim-wall-2");
    let mut h = Harness::open(&parts, cfg, hub.clone()).expect("the session opens");
    let reply = h
        .submit("port the rest of the emulator")
        .expect("the turn runs");

    assert_eq!(reply.rounds, 1, "nothing continued the turn: {reply:?}");
    assert!(
        context_checks(&h).is_empty(),
        "a true claim is not corrected"
    );
    let log =
        std::fs::read_to_string(dir.path.join(".letibot").join(LOG)).expect("the claim is logged");
    let row: serde_json::Value =
        serde_json::from_str(log.lines().next().expect("one line")).expect("json");
    assert_eq!(row["at_wall"], true, "{row}");
    assert_eq!(row["slapped"], false, "{row}");
}

/// With automatic compaction off there is no check: "the harness handles it" would be false -
/// nothing compacts at the wall - so the same excuse at low fill is logged and the model is told
/// nothing.
#[test]
fn no_check_when_auto_compaction_is_off() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-context-claim-off");
    let mut cfg = config(&dir, "context-claim-off");
    cfg.context_window = Some(1_000_000);
    cfg.auto_compact = false;
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;
    let serv = canned::Canned::serve_replies(
        vec![
            a_plain_answer(vocab, EXCUSE, 30),
            a_plain_answer(vocab, "Continuing.", 30),
        ],
        2,
    );
    cfg.endpoint = serv.endpoint.clone();
    let hub = Hub::new("context-claim-off");
    let mut h = Harness::open(&parts, cfg, hub.clone()).expect("the session opens");
    let reply = h
        .submit("port the rest of the emulator")
        .expect("the turn runs");
    assert_eq!(reply.rounds, 1, "nothing continued the turn: {reply:?}");
    assert!(context_checks(&h).is_empty(), "no line to the model");
    let log =
        std::fs::read_to_string(dir.path.join(".letibot").join(LOG)).expect("the claim is logged");
    let row: serde_json::Value =
        serde_json::from_str(log.lines().next().expect("one line")).expect("json");
    assert_eq!(row["slapped"], false, "{row}");
}

/// Said again after being told: the operator hears, the model is not told twice.
#[test]
fn an_excuse_repeated_after_the_line_is_a_warning_for_the_operator() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-context-claim-again");
    let mut cfg = config(&dir, "context-claim-again");
    cfg.context_window = Some(1_000_000);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    let vocab = &parts.vocab;
    let serv = canned::Canned::serve_replies(
        vec![
            a_plain_answer(vocab, EXCUSE, 30),
            a_plain_answer(
                vocab,
                "I am at the end of my context, so this is where I stop.",
                30,
            ),
            a_plain_answer(vocab, "spare", 30),
        ],
        3,
    );
    cfg.endpoint = serv.endpoint.clone();

    let hub = Hub::new("context-claim-again");
    let mut h = Harness::open(&parts, cfg, hub.clone()).expect("the session opens");
    let reply = h
        .submit("port the rest of the emulator")
        .expect("the turn runs");

    assert_eq!(reply.rounds, 2, "told once, then the turn ends: {reply:?}");
    assert_eq!(
        context_checks(&h).len(),
        1,
        "one line to the model, not two"
    );
    let warned = warnings_of(&hub, REPEATED);
    assert_eq!(warned.len(), 1, "{warned:#?}");
    assert!(
        warned[0].contains("I am at the end of my context"),
        "{}",
        warned[0]
    );
}
