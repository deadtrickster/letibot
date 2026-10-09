//! **An answered question leaves the operator's answer in the conversation as THEIRS.**
//!
//! The operator's report, verbatim: *"by the way i dont see my answer to the selector"*.
//! What was measured: a question rides the permission-card machinery — `Answers::ask_question`
//! publishes `SessionEvent::DecisionRequested { kind: "question", … }` and the answer closes it
//! with `DecisionAnswered` — so the selector is a **live overlay**. The answer travelled back
//! through `ClientFrame::AnswerQuestion` into the `ask_user_question` **tool result**, and that
//! payload was the only durable trace. Under `read-edits` — the operator's own profile — the
//! tool rows are not drawn, so once the card closed their screen held no record of the
//! exchange at all. And the half that matters more than visibility: the answer was not a `User`
//! row, so it was not in the authorisation trail either — a *yes* given through that card could
//! not be cited as authorisation, while a *yes* typed in the composer could.
//!
//! # What is asserted, and through what
//!
//! The whole chain and nothing stubbed between the call and the row: a **canned model**
//! (the shared `canned` frames, included by path rather than copied) that emits an
//! `ask_user_question` call in the Qwen dialect's own shape; the session's own tool runtime
//! running it through its own gate; a head attached to the session's own hub answering the
//! card the way `letibot-tui` does; and the assertion on **the session's transcript** and on
//! **the authorisation trail**, which are the two places the operator's words have to be.
//!
//! Three things, and each is a different kind of claim:
//!
//! 1. **The answer row is theirs, byte for byte.** `TranscriptItem::User { speaker: Operator }`
//!    whose text is exactly what they chose — the option's own text and their note, with
//!    nothing of the harness's prepended. A citable utterance with a harness sentence glued to
//!    its front is a citable utterance of the wrong thing.
//! 2. **The question row is not.** It is `Speaker::Agent`, because the trail reads every
//!    `User`-with-`speaker: Operator` row as words an action may cite — and a question put in
//!    their mouth would be a question the harness could quote back as authorisation for the
//!    answer to it.
//! 3. **The trail can cite the answer.** `Speaker::Operator`, through the same door the
//!    composer's own speech is recorded by. This is the half the operator's screen never
//!    showed: before this the answer existed only as a tool payload.
//!
//! # The order, and why it is not the brief's
//!
//! The `!` line's recipe is *the person's row, then the result*, and it is right there because
//! nothing above the result proposed the call. Here something did — the model's
//! `Assistant { tool_calls }` row — and `letibot_provider::messages::pair_tool_calls` closes
//! its answering window on any non-`tool` row. A `User` row between the proposal and its
//! result makes the model read *"no result was recorded for this call"* beside the real one.
//! So the result comes first and the person's half follows it; the assertion below pins the
//! order rather than leaving it to a reader.
//!
//! # What this needs, and what it does not
//!
//! The vocabulary GGUF (`Harness::open` renders its stable prefix) and not the model server:
//! the turns are replayed from the shared canned frames. No exec host — nothing here runs a
//! command — so the gate is the harness's own `Head` arm, which is also where the question
//! path is installed (`letibot-harnessd`'s `answers` module says why those are one act).

use std::sync::Arc;

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::{CommandKind, Hub, Reply};
use letibot_tokencore::Vocab;
use letibot_tools::authorise::{AuthorisationTrail, Speaker};
use letibot_transcript::{TranscriptItem, UserPart};

/// The row's own speaker vocabulary, named apart from the trail's: the two enums are two
/// values with one meaning, and a test that let them share a name would not notice if the
/// session wrote the wrong one.
use letibot_transcript::Speaker as RowSpeaker;

/// The canned server the turn crate's tests replay, shared by path rather than copied — a
/// second copy of the wire shape would drift from the first.
#[path = "../../turn/tests/support/canned.rs"]
mod canned;

use canned::Frame;

/// Qwen's own ids, the same values the turn crate's canned tests pin.
const IM_END: u32 = 248046;
const THINK_OPEN: u32 = 248068;
const THINK_CLOSE: u32 = 248069;
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

/// The question the model asks, and the two choices it offers.
const QUESTION: &str = "which database should the migration target?";
const OPTIONS: [&str; 2] = ["postgres", "sqlite"];
/// The note the operator puts on their choice — the second half of what they said.
const NOTE: &str = "only for the CUDA box";

// ---------------------------------------------------------------------------
// The fixtures
// ---------------------------------------------------------------------------

fn config(session_id: &str, store: &std::path::Path) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    // **`leticode` because that is the seat `ask_user_question` lives on** — it takes its own
    // seat there and nowhere else (`letibot-tools`' seat table). A test against a seat that
    // does not carry the tool would be measuring the seat table.
    cfg.seat = Seat::Leticode;
    // The operator's own `providers.toml` and `permission.json` are not this test's business
    // (`wired.rs`'s rule): without these, a key on the developer's laptop decides what is
    // seated.
    cfg.web_search = None;
    cfg.permission = Vec::new();
    // Nothing here should fail, and a failure that retried would spend the suite's time
    // hiding behind doubling waits.
    cfg.http_retries = 0;
    cfg.context_window = Some(32_768);
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg
}

fn parts_for(cfg: &Config) -> Parts {
    let p = Parts::load(cfg).expect(
        "the vocabulary must load; set LETIBOT_VOCAB_GGUF if this box is not the one \
         this repository is developed on",
    );
    // The operator's per-project mode is not this test's business: `Harness::open` applies the
    // project store's row over `cfg.mode`, and that row is whatever they last set with `/mode`.
    *p.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    p
}

fn ids(vocab: &Vocab, text: &str) -> Vec<u32> {
    vocab.tokenize_text(text).expect("text tokenizes")
}

fn spoken(vocab: &Vocab, ids: &[u32]) -> Vec<Frame> {
    ids.iter()
        .map(|id| Frame::Token {
            id: *id,
            text: vocab
                .detokenize(&[*id], true)
                .expect("one token decodes")
                .leak(),
        })
        .collect()
}

/// A complete turn that answers in plain text: the model thinks, closes the block, answers,
/// ends. No tool calls, so the harness's round loop ends after one round.
fn a_plain_answer_turn(vocab: &Vocab, thought: &str, answer: &str, n_prompt: u64) -> Vec<Frame> {
    let thought_ids = ids(vocab, thought);
    let answer_ids = ids(vocab, answer);
    let mut frames = vec![Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.extend(spoken(vocab, &thought_ids));
    frames.extend(spoken(vocab, &[THINK_CLOSE]));
    frames.extend(spoken(vocab, &answer_ids));
    frames.extend(spoken(vocab, &[IM_END]));
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: (thought_ids.len() + answer_ids.len() + 2) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

/// **The turn that asks the question** — one tool call and no text, which is the round
/// boundary the tool runs at.
fn an_ask_turn(vocab: &Vocab, n_prompt: u64) -> Vec<Frame> {
    let call = format!(
        "\n<function=ask_user_question>\n<parameter=question>\n{QUESTION}\n</parameter>\n\
         <parameter=options>\n[\"{}\", \"{}\"]\n</parameter>\n</function>\n",
        OPTIONS[0], OPTIONS[1]
    );
    let call_ids = ids(vocab, &call);
    let mut frames = vec![Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.push(Frame::Token {
        id: THINK_OPEN,
        text: "",
    });
    frames.push(Frame::Token {
        id: THINK_CLOSE,
        text: "",
    });
    frames.push(Frame::Token {
        id: TOOL_CALL_OPEN,
        text: "<tool_call>",
    });
    frames.extend(spoken(vocab, &call_ids));
    frames.push(Frame::Token {
        id: TOOL_CALL_CLOSE,
        text: "</tool_call>",
    });
    frames.push(Frame::Final {
        stop_type: "eos",
        // The think fences and the two tool-call fences: the accumulator counts what was
        // streamed, so this has to agree with it exactly.
        n_decoded: (call_ids.len() + 4) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

/// A directory that removes itself, because a test that leaks a store per run eventually
/// fills the disk and the run that finds out is not this one.
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

// ---------------------------------------------------------------------------
// The exchange
// ---------------------------------------------------------------------------

/// **One whole exchange**, from the model's call to the rows the session wrote afterwards.
///
/// Returns the session's transcript and its authorisation trail, which are the two places a
/// person's words have to end up.
fn one_exchange(tag: &str, reply: Reply) -> (Vec<TranscriptItem>, AuthorisationTrail) {
    let dir = TempDir::new(tag);
    let path = dir.path().join("sessions.db");
    let mut cfg = config(tag, &path);
    let parts = parts_for(&cfg);

    // The turns: ask, then answer from what came back.
    let serv = canned::Canned::serve_each(
        vec![
            an_ask_turn(&parts.vocab, 30),
            a_plain_answer_turn(&parts.vocab, "reading the answer", "sqlite it is", 40),
        ],
        2,
    );
    cfg.endpoint = serv.endpoint.clone();

    let hub = Hub::new(tag);
    // **`None` for the adjudicator, and that is the point of the fixture.** The question path
    // is installed in the arm that builds the head adjudicator from `cfg.adjudicator` — one
    // `Answers`, one sink, one slot, as one act — so a test that handed in its own adjudicator
    // would be testing a session where no question can reach anybody.
    let mut h = Harness::open_with(&parts, cfg, hub.clone(), None, None)
        .expect("a leticode session opens with the head adjudicator");

    // A head that can answer, playing itself on its own thread — the same shape `wired.rs`'s
    // end-to-end ask uses, so the two cannot drift on what the card looks like.
    let head = hub.attach("tui", "deadtrickster", Default::default(), 0);
    let hub2 = hub.clone();
    let head_id = head.head_id.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let answerer = std::thread::spawn(move || {
        while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
            let open = hub2.snapshot().open_decisions;
            if let Some(d) = open.first() {
                assert_eq!(d.kind, "question", "the head was shown a permission");
                assert_eq!(
                    d.choices,
                    vec![OPTIONS[0].to_string(), OPTIONS[1].to_string()],
                    "the head was not shown the model's own choices"
                );
                hub2.submit(
                    &head_id,
                    "c1",
                    0,
                    CommandKind::Answer {
                        req_id: d.req_id.clone(),
                        reply,
                    },
                );
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        false
    });

    let out = h.submit("ask me which database");
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        answerer.join().unwrap(),
        "the question never reached the head: {out:?}"
    );
    assert!(out.is_ok(), "the turn must run through: {out:?}");

    (h.items().to_vec(), h.trail())
}

/// The speaker and text of a `User` row, or `None` when the row is not one.
fn user_row(item: &TranscriptItem) -> Option<(RowSpeaker, String)> {
    let TranscriptItem::User { speaker, parts } = item else {
        return None;
    };
    let text = parts
        .iter()
        .map(|p| match p {
            UserPart::Text { text } => text.clone(),
            other => format!("{other:?}"),
        })
        .collect::<Vec<_>>()
        .join("");
    Some((*speaker, text))
}

/// **A choice, and the person's own note on it, land as their words — and the trail can cite
/// them.**
///
/// The operator's requirement for the row is the one the `!` line's own row keeps: their
/// words, verbatim, with nothing of the harness's prepended. So the option's text is read off
/// the QUESTION they were shown — the label, not an index, because an index is not something
/// anybody said — and the note follows it on its own line.
#[test]
fn the_answer_is_the_operators_own_row_and_the_trail_can_cite_it() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let said = format!("sqlite\n{NOTE}");
    let (items, trail) = one_exchange(
        "question-row-choice",
        Reply::Question(letibot_sessionlog::question::QuestionAnswer::choosing(1).with_note(NOTE)),
    );

    // **The answer row: theirs, byte for byte.** `sqlite` is the option's own text — the label
    // the person was shown and picked — and the note is what they said about it.
    let answer = items
        .iter()
        .position(|i| matches!(user_row(i), Some((RowSpeaker::Operator, t)) if t == said))
        .unwrap_or_else(|| panic!("no row carries the operator's answer: {items:#?}"));

    // **The question row: the session's, not theirs.** The trail reads every
    // `User`-with-`speaker: Operator` row as citable words of theirs, so the question must not
    // be one — and a head must not draw it as them either.
    let question = items
        .iter()
        .position(|i| matches!(user_row(i), Some((RowSpeaker::Agent, t)) if t.contains(QUESTION)))
        .unwrap_or_else(|| panic!("the question is not on the log: {items:#?}"));
    let (_, asked_text) = user_row(&items[question]).expect("the question is a user row");
    assert!(
        asked_text.contains(OPTIONS[0]) && asked_text.contains(OPTIONS[1]),
        "the question row must name the options it offered: {asked_text}"
    );
    assert!(
        !items.iter().any(|i| {
            matches!(user_row(i), Some((RowSpeaker::Operator, t)) if t.contains(QUESTION))
        }),
        "the question was attributed to the operator: {items:#?}"
    );

    // **The result is the person's act**, which is what every head draws at every verbosity —
    // and it still carries the payload the model reads.
    let result = items
        .iter()
        .position(|i| {
            matches!(
                i,
                TranscriptItem::ToolResult {
                    name,
                    origin: Some(letibot_transcript::CallOrigin::Operator { who }),
                    ..
                } if name == "ask_user_question" && who == "deadtrickster"
            )
        })
        .unwrap_or_else(|| panic!("the result does not say the person answered: {items:#?}"));
    let TranscriptItem::ToolResult { payload, .. } = &items[result] else {
        unreachable!("the position was found by matching a tool result");
    };
    assert!(
        payload.contains("chose option 1: sqlite"),
        "the payload the model reads is unchanged: {payload}"
    );

    // **And the order.** The result answers the model's proposal, so it comes first; the
    // person's half follows it. See this file's header for the measurement behind it.
    assert!(
        result < question && question < answer,
        "result {result}, question {question}, answer {answer}: the exchange is out of order"
    );

    // **The trail can cite it** — `Speaker::Operator`, through the same door the composer's own
    // speech is recorded by. This is the half the operator's screen never showed.
    let cited = trail
        .utterances
        .iter()
        .find(|u| u.text == said)
        .unwrap_or_else(|| panic!("the answer is not in the trail: {:#?}", trail.utterances));
    assert_eq!(
        cited.speaker,
        Speaker::Operator,
        "an answer given through a card must authorise exactly as one typed in the composer"
    );
    // And the question is not theirs there either: the trail's two speakers are the whole of
    // the distinction an `ALLOW <n>` rests on.
    let question_in_trail = trail
        .utterances
        .iter()
        .find(|u| u.text.contains(QUESTION))
        .expect("the question is in the trail as context");
    assert_eq!(
        question_in_trail.speaker,
        Speaker::Agent,
        "the question must never read back as the operator's words"
    );
}

/// **A typed answer is a first-class answer, and it lands as the person's row.**
///
/// The fourth shape the tool takes (`QuestionAnswer::free`): no option, no note, just what they
/// typed. It is the case the composer's own words are the whole of, and the case the row has to
/// carry without a label, an index or a quotation around it.
#[test]
fn a_typed_answer_lands_as_the_operators_own_words() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let said = "neither — split it into two migrations";
    let (items, trail) = one_exchange(
        "question-row-free",
        Reply::Question(letibot_sessionlog::question::QuestionAnswer::free(said)),
    );

    assert!(
        items
            .iter()
            .any(|i| matches!(user_row(i), Some((RowSpeaker::Operator, t)) if t == said)),
        "the typed answer is not on the log as theirs: {items:#?}"
    );
    let cited = trail
        .utterances
        .iter()
        .find(|u| u.text == said)
        .unwrap_or_else(|| panic!("the typed answer is not in the trail: {:#?}", trail.utterances));
    assert_eq!(cited.speaker, Speaker::Operator);
}

/// **An abstention is an ANSWER, and it lands as one.**
///
/// The tool reports it as `Abstained` rather than as `not_run` — *"I am not answering this
/// one"* is a decision the person made — so the row exists and the trail can cite it. It is
/// also the one outcome where the answer row is neither a choice nor a sentence, and the word
/// the wire carries for the act is the word the row carries.
#[test]
fn an_abstention_lands_as_the_operators_own_row() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let (items, trail) = one_exchange(
        "question-row-abstain",
        Reply::Question(letibot_sessionlog::question::QuestionAnswer::abstaining()),
    );

    assert!(
        items
            .iter()
            .any(|i| matches!(user_row(i), Some((RowSpeaker::Operator, t)) if t == "abstain")),
        "the abstention is not on the log as theirs: {items:#?}"
    );
    let cited = trail
        .utterances
        .iter()
        .find(|u| u.text == "abstain")
        .unwrap_or_else(|| panic!("the abstention is not in the trail: {:#?}", trail.utterances));
    assert_eq!(cited.speaker, Speaker::Operator);
    // And the tool said the same thing, which is the pair that keeps the row and the result
    // from disagreeing about what happened.
    assert!(
        items.iter().any(|i| matches!(
            i,
            TranscriptItem::ToolResult {
                outcome: letibot_transcript::ToolOutcome::Abstained { .. },
                ..
            }
        )),
        "the result does not report an abstention: {items:#?}"
    );
}
