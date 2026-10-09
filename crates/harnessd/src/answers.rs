//! **The half a head was missing: a way to answer.**
//!
//! `TODO.md` D25. The console adjudicator reads the daemon's own stdin, which works
//! for `harnessd --prompt …` and for a daemon in a foreground terminal and does not
//! work for the shape people run — a daemon in the background with `letibot-tui`
//! attached. The head already *renders* the question; there was no path back, so
//! `write` and `exec` were a foreground thing and the operator was told to *"restart
//! with `--role coder`"*, which is the least liftable form of a refusal there is.
//!
//! `docs/boundary-and-adjudication.md` §4b makes liftability a requirement: *"the
//! grant path is reachable **at the moment of denial**, not after the task has
//! died"*. A path that only goes one way is not a path.
//!
//! # The deadlock this is shaped around
//!
//! §13.2 has **one worker** per daemon. It takes a command, runs the turn, and a
//! gated tool call happens *inside* that turn — so when the adjudicator blocks, the
//! worker is inside `Sessions::dispatch`. An answer that arrives on the command queue
//! is drained by the thread that is waiting for it, which is never.
//!
//! So the answer comes in through [`letibot_sessionlog::hub::AnswerSink`], delivered
//! on the socket reader's thread, and this module is what is on the other end of it:
//! a slot per open request, a condvar, and a deadline.
//!
//! # Every way this can end, and all but one of them refuse
//!
//! | | what the gate gets |
//! |---|---|
//! | a head answered | the option it chose |
//! | **no head can answer** | `Unavailable`, *before* any wait — see [`Answers::ask`] |
//! | the deadline passed | `Timeout` → the request's `on_timeout`, which is `Deny` |
//! | the operator interrupted | `Cancelled` |
//! | the session closed | `Cancelled` |
//!
//! Only the first opens the gate, and it opens it by naming an option the request
//! offered — [`letibot_tools::AskAdjudicator`]'s rule, one layer up. The rest are all
//! `NotRun`: *nobody decided*, which is a different sentence from *denied* and is the
//! distinction §4b says stops a model inferring that its approach was wrong.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use letibot_sessionlog::event::{
    Decider, DecisionOption, DecisionOutcome as WireOutcome, ModelAdvice as WireAdvice,
    OnTimeout as WireOnTimeout, OptionKind, SessionEvent, SubagentAsk,
};
use letibot_sessionlog::hub::{AnswerSink, Hub, Reply};
use letibot_tools::adjudicate::{
    AdjudicationDecision, AdjudicationRequest, Adjudicator, DecisionOutcome, ModelAdvice,
    OnTimeout, OptionKind as ToolOptionKind, RequestKind,
};
// **The question vocabulary, which is not the adjudication vocabulary.** §11.6's split,
// one layer out: a permission is answered by an option id and a question by a person's
// words, and the two must not be able to settle each other — `Hub::submit` refuses that
// crossing at the door, and these types are why it can.
use letibot_tools::builtins::intent::{AskError, Headless, Question, QuestionAnswer, Questioner};

/// How long a person gets to answer before the gate fails closed.
///
/// A number rather than §11.5's *"wait forever"*, and the reason is not impatience.
/// A forever wait inside a tool call holds the daemon's only worker, so a session
/// whose operator walked away stops every other session on the box — and nothing in
/// this daemon polls, so there is no other thread that would notice.
///
/// Five minutes is long for a person who is looking at the screen and short for a
/// daemon nobody is watching. An operator who is not there gets `not_run` and a
/// refusal that says the deadline passed, which is recoverable by asking again; the
/// alternative is a hung box, which is not. The deadline travels on the wire so the
/// head can show what is left of it rather than the person guessing.
pub const ANSWER_BUDGET: Duration = Duration::from_secs(300);

/// What became of one open request.
enum Slot {
    /// Posted, nobody has said anything.
    Waiting,
    /// A head answered. `by` is read from the connection the answer arrived on and
    /// never from the answer itself.
    Answered { reply: Reply, by: String },
    /// An interrupt or a close arrived while this was open.
    Cancelled { why: String },
}

/// The rendezvous: one slot per open request, and the condvar the tool thread sleeps
/// on.
///
/// Owned by the session, shared with the hub as its [`AnswerSink`]. It is `Sync` and
/// deliberately has no `&mut self` method: the thread that posts and the thread that
/// answers are different threads by construction, which is the entire point.
pub struct Answers {
    slots: Mutex<HashMap<String, Slot>>,
    cv: Condvar,
    /// `(req_id, glob)` for the last answer that carried one, so
    /// [`HeadAdjudicator::last_pattern`] can pick it up after `ask` returns.
    ///
    /// Keyed by request rather than kept as a bare `Option`, so a pattern can only
    /// ever be applied to the call it was typed against. One ask is in flight per
    /// daemon today — §13.2's single worker — and a value that is only correct
    /// because of a property elsewhere is the kind that stops being correct quietly.
    pattern: Mutex<Option<(String, String)>>,
}

impl Default for Answers {
    fn default() -> Self {
        Answers::new()
    }
}

impl Answers {
    pub fn new() -> Answers {
        Answers {
            slots: Mutex::new(HashMap::new()),
            cv: Condvar::new(),
            pattern: Mutex::new(None),
        }
    }

    /// The glob that came with the answer to `req_id`, taken so it cannot be read
    /// twice.
    ///
    /// Returns `None` for any other request, which is the property this is keyed
    /// for: a pattern typed against one call must never reach the rule written for
    /// another.
    pub fn take_pattern(&self, req_id: &str) -> Option<String> {
        let mut g = self.pattern.lock().unwrap_or_else(|e| e.into_inner());
        match g.as_ref() {
            Some((id, p)) if id == req_id => {
                let p = p.clone();
                *g = None;
                Some(p)
            }
            _ => None,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Slot>> {
        self.slots.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Open a slot and wait on it, up to `budget`.
    ///
    /// The slot is inserted **before** the caller publishes the question, so an
    /// answer that arrives while the publish is still fanning out has somewhere to
    /// land. Doing it the other way round is a race with a window of exactly the
    /// length of a fan-out, which is the kind nobody reproduces and everybody
    /// eventually hits.
    fn wait(&self, req_id: &str, budget: Duration) -> Waited {
        let deadline = Instant::now() + budget;
        let mut g = self.lock();
        // **`or_insert`, never `insert`.** `ask` creates the slot BEFORE it
        // publishes the question — that ordering is the whole rendezvous, and its
        // comment says so — which leaves a window between the publish and this
        // call. A head that answers inside that window sets `Slot::Answered`, and
        // an unconditional insert here put `Waiting` back over it: the answer was
        // dropped, this waited out the whole budget, and the call came back
        // `Timeout` / *"nobody answered"* with the operator's own answer thrown
        // away. The faster the answerer the likelier it was, so a guard model
        // replying in 40ms lost more often than a person.
        //
        // Caught as a 2-in-8 flake in `a_head_answers_a_decision_the_daemons_stdin_could_not`,
        // whose answering thread polls every 2ms and therefore lands in the window
        // regularly. The test was right and the code was wrong.
        g.entry(req_id.to_string()).or_insert(Slot::Waiting);
        loop {
            match g.get(req_id) {
                Some(Slot::Answered { .. }) | Some(Slot::Cancelled { .. }) | None => break,
                Some(Slot::Waiting) => {}
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let (next, _) = self
                .cv
                .wait_timeout(g, left)
                .unwrap_or_else(|e| e.into_inner());
            g = next;
        }
        match g.remove(req_id) {
            Some(Slot::Answered { reply, by }) => Waited::Answered { reply, by },
            Some(Slot::Cancelled { why }) => Waited::Cancelled { why },
            // `Waiting` at the deadline, or gone entirely. Both are *nobody
            // answered*, and neither is a decision.
            _ => Waited::TimedOut,
        }
    }

    /// **Post a request to the heads and wait for one of them to settle it.**
    ///
    /// `describe_heads` is asked first and the answer decides whether anything is
    /// posted at all: a session whose only head is a read-only connector cannot
    /// answer, and posting to it and then sitting out the deadline would report a
    /// timeout about a question nobody was ever asked.
    ///
    /// `subagent` is who is asking, when it is not this session's own call: a child's
    /// gate with no head of its own, posting its card to the tree's ROOT — the session
    /// whose head exists. See [`SubagentAsk`] for the ruling. `None` is every ask this
    /// daemon made before that existed, and the card then says nothing about attribution.
    pub fn ask(
        &self,
        hub: &Arc<Hub>,
        req: &AdjudicationRequest,
        budget: Duration,
        subagent: Option<&SubagentAsk>,
    ) -> AdjudicationDecision {
        let started = Instant::now();
        let who = hub.deciding_heads();
        if who.is_empty() {
            return AdjudicationDecision::unavailable(req, "gate:no-head", &no_head(subagent));
        }

        let deadline_ms = unix_millis() + budget.as_millis() as u64;
        // The slot exists before the question does. See `wait`.
        let waited = {
            let mut g = self.lock();
            g.insert(req.id.clone(), Slot::Waiting);
            drop(g);
            hub.publish(pose(req, deadline_ms, subagent));
            // **The yellow card says why it is yellow.** The call is still
            // `Proposed` — `ToolStarted` fires only after the gate admits — so
            // this note is the only thing on the card that explains the wait,
            // and the operator's report was *"some tool calls stay yellow, no
            // idea what that means"*. The ask renders in the chrome; the note
            // renders on the call, where the eye already is.
            //
            // **A subagent's note lands where its card did**, on the root's hub, and the
            // root has no such call — a head looks the id up, finds nothing and drops the
            // note. That is the honest place for it (it travels with the card, and the
            // child's own card is on a screen no head is watching), and the wait is still
            // visible on the card itself, which carries the deadline.
            hub.publish(SessionEvent::ToolProgress {
                turn_id: req.turn_id.clone(),
                call_id: req.call_id.clone(),
                note: format!(
                    "waiting for you: the ask is open for {}s, then nothing runs",
                    budget.as_secs()
                ),
            });
            self.wait(&req.id, budget)
        };

        let latency_ms = started.elapsed().as_millis() as u64;
        let decision = match waited {
            Waited::Answered { reply, by } => {
                if let Reply::Permission {
                    pattern: Some(p), ..
                } = &reply
                {
                    *self.pattern.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some((req.id.clone(), p.clone()));
                }
                settle(req, &reply, &by, latency_ms)
            }
            // Nobody decided, so `by` is the gate and not the heads that were
            // asked — the heads are named in the basis. A `human:` here put
            // timeouts into the corpus as a person's refusals (2026-09-16).
            Waited::TimedOut => AdjudicationDecision {
                request_id: req.id.clone(),
                outcome: DecisionOutcome::Timeout,
                by: "gate:timeout".into(),
                basis: format!(
                    "nobody answered within {}s (asked {}). This is not a denial — nobody decided.",
                    budget.as_secs(),
                    who.join(",")
                ),
                latency_ms,
            },
            Waited::Cancelled { why } => AdjudicationDecision {
                request_id: req.id.clone(),
                outcome: DecisionOutcome::Cancelled,
                by: "gate:interrupt".into(),
                basis: format!("the decision was cancelled before anybody answered: {why}"),
                latency_ms,
            },
        };
        // **The open decision is closed on every path**, including the ones nobody
        // answered. A `DecisionRequested` with no matching `DecisionAnswered` stays
        // in `SessionView::open` for the life of the session, so the head keeps
        // offering a prompt for a call that was refused minutes ago — and the hub
        // then accepts an answer to it, which is §13.2b's *"input arriving from
        // nowhere"*.
        hub.publish(close(req, &decision));
        decision
    }
}

enum Waited {
    Answered { reply: Reply, by: String },
    TimedOut,
    Cancelled { why: String },
}

// ---------------------------------------------------------------------------
// `ask_user_question`: the other vocabulary, on the same rendezvous.
// ---------------------------------------------------------------------------

impl Answers {
    /// **Post a question to the heads and wait for one of them to answer it.**
    ///
    /// The operator's ruling, 2026-10-09: *"yeah i want you to be able to ask me for a
    /// choice. each choice can have my note, and i can abstain or type my answer"*. The
    /// tool has existed since D10 and had no way to reach a person: `Headless` was what
    /// every session got, so `ask_user_question` was a verb that could only refuse.
    ///
    /// # Why this is not `Answers::ask`
    ///
    /// The rendezvous is the same one — the same slots, the same condvar, the same
    /// deadline, the same answer sink — because the *deadlock* is the same deadlock: the
    /// question is asked from inside a tool call on the daemon's only worker, so the
    /// answer cannot come in through the command queue ([`Answers`] says why). What
    /// differs is the payload in each direction, and that is the whole of it:
    ///
    /// * the card is posed with `kind: "question"` and its plain-text `choices`;
    /// * the reply is [`Reply::Question`], which `Hub::submit` will not accept for a
    ///   permission and will not accept a permission's answer for.
    ///
    /// # Every way this can end
    ///
    /// | | what the tool gets |
    /// |---|---|
    /// | a head answered | the answer, and who gave it |
    /// | **no head can answer** | [`AskError::NoHead`], *before* any wait |
    /// | the deadline passed | [`AskError::Unanswered`] — *nobody answered* |
    /// | the operator interrupted, or the session closed | [`AskError::Unanswered`] |
    ///
    /// **An abstention is not one of those.** It comes back through the first row like
    /// any other answer, and it is the tool (`accept`, in `letibot-tools`) that reports
    /// it as `Abstained` rather than as silence. This function never decides that a person
    /// meant anything: it carries what arrived, and who it arrived from.
    ///
    /// # The settled event, and the word it carries
    ///
    /// A question's ending is published as [`WireOutcome::Cancelled`] with the person's
    /// own answer in the `basis`, which is `close`'s own precedent for an outcome the wire
    /// has not got: the request is off the screen and nobody selected anything *from a
    /// ladder*, because a question has no ladder. The alternative — inventing an option id
    /// so a `Selected` would parse — would put a word in the log that nothing chose, and
    /// the answer itself is in the basis and in the tool's result either way.
    ///
    /// **No `PROTOCOL_VERSION` bump.** Nothing about the wire changed: `kind: "question"`
    /// and `choices` are `PROTOCOL_VERSION` 7, `AnswerQuestion` and [`Reply::Question`] are
    /// 5, and `DecisionOutcome` grew nothing.
    pub fn ask_question(
        &self,
        hub: &Arc<Hub>,
        q: &Question,
        budget: Duration,
    ) -> Result<(QuestionAnswer, String), AskError> {
        // **Read, not assumed**, and asked before anything is posted: a session whose only
        // head is a read-only connector cannot answer, and posting to it and then sitting
        // out the deadline would report a timeout about a question nobody was ever asked.
        // The same check `Answers::ask` makes, for the same reason.
        let who = hub.deciding_heads();
        if who.is_empty() {
            return Err(AskError::NoHead(no_head_for_a_question()));
        }

        let req_id = question_id(&hub.session_id());
        let deadline_ms = unix_millis() + budget.as_millis() as u64;
        // The slot exists before the question does. See `Answers::wait` — the window is
        // the length of a fan-out and an answer that lands inside it must have somewhere
        // to land.
        let waited = {
            let mut g = self.lock();
            g.insert(req_id.clone(), Slot::Waiting);
            drop(g);
            hub.publish(pose_question(&req_id, q, deadline_ms));
            self.wait(&req_id, budget)
        };

        // **The open card is closed on every path**, including the ones nobody answered —
        // the same rule `Answers::ask` states: a `DecisionRequested` with no matching
        // `DecisionAnswered` stays in the head's open set for the life of the session, and
        // the hub then accepts an answer to a question that is over.
        let close = |outcome: WireOutcome, by: Decider, basis: String| {
            hub.publish(close_question(&req_id, outcome, by, basis));
        };

        match waited {
            Waited::Answered {
                reply: Reply::Question(wire),
                by,
            } => {
                let a = to_tool_answer(&wire);
                // The same renderer the model reads, flattened onto one line, so the head
                // and the tool cannot disagree about what the person said.
                let basis = one_line(&a.render(q));
                close(
                    WireOutcome::Cancelled,
                    Decider {
                        kind: "human".into(),
                        identity: by.clone(),
                    },
                    basis,
                );
                Ok((a, by))
            }
            // **The other vocabulary's reply.** `Hub::submit` refuses this at the door, so
            // it is unreachable through the daemon; the arm exists because the slot can
            // only be filled by *something*, and calling it an answer would be the defect
            // the two types exist to prevent.
            Waited::Answered {
                reply: Reply::Permission { .. },
                by,
            } => {
                close(
                    WireOutcome::Cancelled,
                    Decider {
                        kind: "gate".into(),
                        identity: "unavailable".into(),
                    },
                    format!(
                        "a permission's answer arrived from {by} for a question, which settles \
                     nothing; a question is settled by a choice, a note or words"
                    ),
                );
                Err(AskError::NotAnAnswer(
                    "a permission's answer arrived for it".into(),
                ))
            }
            Waited::TimedOut => {
                close(
                    WireOutcome::TimedOut,
                    Decider {
                        kind: "gate".into(),
                        identity: "timeout".into(),
                    },
                    format!(
                        "nobody answered within {}s (asked {}). This is not a decision, and it \
                     is not a declining: the question is still open.",
                        budget.as_secs(),
                        who.join(",")
                    ),
                );
                Err(AskError::Unanswered(
                    "the question was posted to the attached head and nobody answered before the \
                 deadline, so NOBODY has answered it. A deadline passing is not a decision, \
                 not a declining, and not a `chat later`: the question is still open."
                        .into(),
                ))
            }
            Waited::Cancelled { why } => {
                close(
                    WireOutcome::Cancelled,
                    Decider {
                        kind: "gate".into(),
                        identity: "interrupt".into(),
                    },
                    format!("the question was cancelled before anybody answered: {why}"),
                );
                Err(AskError::Unanswered(format!(
                    "the question was cancelled before anybody answered ({why}), so NOBODY has \
                 answered it and it is still open."
                )))
            }
        }
    }
}

/// The wire's answer, as the tool's own shape.
///
/// **A field-by-field copy and not a `From`, deliberately.** The two types exist
/// because neither crate may learn about the other — `crates/sessionlog/src/question.rs`
/// says why, and its `the_wire_shape_and_the_tool_shape_agree` is the pin that keeps
/// them one shape. A conversion that lived in either crate would be that learning; the
/// daemon is the place they meet, which is what `lift_tools` is for the other pair.
///
/// A field this forgets is a field a person sent that the model never sees, which is
/// why it copies all four and why the pin above is worth having.
fn to_tool_answer(a: &letibot_sessionlog::question::QuestionAnswer) -> QuestionAnswer {
    QuestionAnswer {
        option: a.option,
        note: a.note.clone(),
        free: a.free.clone(),
        abstain: a.abstain,
    }
}

/// **The `DecisionRequested` a question is posed as.**
///
/// `choices` and not `options`: `options` carries the adjudication ladder, which is
/// empty for a question, and `choices` is the plain-text list `PROTOCOL_VERSION` 7
/// added so that a question could be posed *with* its choices rather than, in
/// `protocol.rs`'s own words, *"only by discarding the choices"*.
///
/// `call_id: None`: this is not a gated call, so there is no row for the card to
/// ride — a head draws it as the ask it is, which is why a question's settled event
/// becomes a note rather than a line on a call.
///
/// `on_timeout: Deny` is the honest one of the three the wire has. `Ask` draws *"the
/// guard model decides"* — false, nothing is consulted for a question — and `Allow`
/// draws *"it RUNS anyway"* — false, nothing was ever going to run. `Deny` draws *"if
/// nobody answers, nothing runs"*, which is exactly what a question's deadline costs:
/// the tool returns `not_run` and the turn proceeds with no answer.
fn pose_question(req_id: &str, q: &Question, deadline_ms: u64) -> SessionEvent {
    SessionEvent::DecisionRequested {
        write_targets: Vec::new(),
        req_id: req_id.into(),
        kind: "question".into(),
        call_id: None,
        // A question is not a gate: nothing about a declaration asks for it, and a
        // card that drew an access class here would be claiming one.
        access: String::new(),
        summary: q.text.clone(),
        target: String::new(),
        detail: String::new(),
        options: Vec::new(),
        choices: q.options.clone(),
        because: q.because.clone(),
        advice: None,
        subagent: None,
        deadline: Some(deadline_ms),
        on_timeout: WireOnTimeout::Deny,
    }
}

/// The `DecisionAnswered` that takes a question off the head's screen.
fn close_question(req_id: &str, outcome: WireOutcome, by: Decider, basis: String) -> SessionEvent {
    SessionEvent::DecisionAnswered {
        req_id: req_id.into(),
        outcome,
        by,
        basis,
        late: false,
    }
}

/// **Why nobody could be asked, in the words a question needs rather than a gate's.**
///
/// [`no_head`] is the gate's sentence and says the *gate* fails closed — nothing ran
/// and nothing changed. A question never had anything to run, so the same sentence
/// would be describing a call that was never gated. What a model needs told here is
/// the other half: the question is still open, and an assumption is not an answer.
fn no_head_for_a_question() -> String {
    "no head that can answer is attached to this session, so NOBODY was asked and nobody \
     answered. This is not a refusal and it is not permission to proceed on an \
     assumption: the question is still open. State the assumption you would have to make, \
     and stop, or take the path that does not need the answer."
        .to_string()
}

/// A question's request id: unique per daemon, and recognisable as a question's in a
/// log where every other id begins `adj-`.
fn question_id(session_id: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    format!("q-{session_id}-{n:04}")
}

/// Whitespace-collapsed, so a multi-line rendering travels as one `basis` line.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// **One question and the answer it got, as the daemon's own questioner saw them.**
///
/// The tool returns the answer to the model; the transcript is the SESSION's to write
/// ([`crate::harness::Harness`] — *the writer is the daemon, not the tool*). This is how
/// the two halves meet: the questioner records what it carried, and the round loop — the
/// one place a result is appended — reads it back and writes the person's answer down as
/// theirs.
///
/// **A record and not a parse.** The tool's payload is prose the harness itself rendered
/// (`QuestionAnswer::render`), and reading a person's words back out of our own sentences
/// would be the harness deciding what they said. What is kept here is the structured
/// answer that came off the wire, beside the question it answers.
#[derive(Debug, Clone)]
pub struct Asked {
    /// The question as it was posed, options and all — the answer row's context, and
    /// where a chosen option's own text is read from.
    pub question: Question,
    /// What the person said.
    pub answer: QuestionAnswer,
    /// Who said it: the head's identity, the same string the gate records as
    /// `human:<who>`, so the row and its own adjudication name the actor the same way.
    pub by: String,
}

/// Where a questioner records the exchange it just carried. `None` is an empty slot.
///
/// One slot rather than a queue: a session runs one tool call at a time on its worker,
/// so a second exchange cannot start before the first is written down.
pub type AskLog = Arc<Mutex<Option<Asked>>>;

/// **The questioner a session with a head is given.**
///
/// The seam is [`Questioner`] and this is the implementation D10's tool was missing:
/// it posts the question to the head and waits on the answer frame the head already
/// had. It holds nothing of its own — the rendezvous is [`Answers`], shared with the
/// adjudicator — so a question and a permission are one mechanism in the daemon too,
/// differing in `kind`, which is §11.6's sentence made true one layer down.
///
/// The one thing it does hold is the [`AskLog`] a session hands it, which is not state
/// of its own either: it is where the exchange goes so the session that owns the
/// transcript can write the person's half of it down.
pub struct HeadQuestioner {
    hub: Arc<Hub>,
    answers: Arc<Answers>,
    budget: Duration,
    asked: Option<AskLog>,
}

impl HeadQuestioner {
    pub fn new(hub: Arc<Hub>, answers: Arc<Answers>) -> HeadQuestioner {
        HeadQuestioner {
            hub,
            answers,
            budget: ANSWER_BUDGET,
            asked: None,
        }
    }

    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// **Where the exchange is recorded for the session to write down.** `None` — the
    /// default — is a questioner asked by a test that only wants the question asked.
    pub fn with_ask_log(mut self, asked: AskLog) -> Self {
        self.asked = Some(asked);
        self
    }
}

impl Questioner for HeadQuestioner {
    fn ask(&self, q: &Question) -> Result<(QuestionAnswer, String), AskError> {
        let got = self.answers.ask_question(&self.hub, q, self.budget)?;
        // **Recorded on the way past, and only an ANSWER.** Every `Err` arm of
        // `ask_question` is *nobody answered* — no head, a deadline, an interrupt — and a
        // refusal is not a person's utterance, so nothing is written down for one.
        // Whether the tool then ACCEPTS the answer (an abstention is accepted, an option
        // index nobody offered is not) is the tool's own call, and the round loop asks the
        // result before it writes anything.
        if let Some(log) = &self.asked {
            *log.lock().unwrap_or_else(|e| e.into_inner()) = Some(Asked {
                question: q.clone(),
                answer: got.0.clone(),
                by: got.1.clone(),
            });
        }
        Ok(got)
    }

    /// The same shape [`HeadAdjudicator::describe`] gives, and read for the same
    /// reason: *reachable* is a fact about right now, and a line that claimed a head
    /// for a session with none is the disclosure claiming a safety property the
    /// session does not have.
    fn describe(&self) -> String {
        let heads = self.hub.deciding_heads();
        if heads.is_empty() {
            return format!(
                "asked at the head — none attached right now, so every `ask_user_question` \
                 call refuses with not_run (deadline {}s)",
                self.budget.as_secs()
            );
        }
        format!(
            "asked at the head: {} (deadline {}s)",
            heads.join(", "),
            self.budget.as_secs()
        )
    }
}

/// **The questioner the intent wiring is given, whose head arrives with the gate.**
///
/// The order is forced and not a design: `Harness::open` builds the intent wiring —
/// and registers the tools that take it — *before* it builds the gate, and the gate is
/// where [`Answers`] and the hub's answer sink are created, as one act, for the reason
/// `Answers::ask` states. So the wiring gets a slot and this reads it at ask time.
///
/// Reading it late is also the honest reading: what matters is whether a head is
/// attached **now**, and a session whose head attached after it opened can be asked.
/// An empty slot refuses with [`Headless`]'s own sentence rather than a copy of it —
/// there is one place in this tree that says what *no head* means, and this is not it.
pub struct SlotQuestioner(pub Arc<Mutex<Option<Arc<dyn Questioner>>>>);

impl Questioner for SlotQuestioner {
    fn ask(&self, q: &Question) -> Result<(QuestionAnswer, String), AskError> {
        let head = self.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match head {
            Some(h) => h.ask(q),
            None => Headless.ask(q),
        }
    }

    fn describe(&self) -> String {
        let head = self.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match head {
            Some(h) => h.describe(),
            None => Headless.describe(),
        }
    }
}

impl AnswerSink for Answers {
    fn answer(&self, req_id: &str, identity: &str, reply: &Reply) -> bool {
        let mut g = self.lock();
        let Some(slot) = g.get_mut(req_id) else {
            // Nothing is waiting. Dropped rather than held: §13.2b, *"a reply to a
            // question asked ten minutes ago is not an answer, it is input arriving
            // from nowhere"* — and applying a late allow to a call already reported
            // as `not_run` is the worst outcome available here.
            return false;
        };
        *slot = Slot::Answered {
            reply: reply.clone(),
            by: identity.to_string(),
        };
        drop(g);
        self.cv.notify_all();
        true
    }

    fn cancel(&self, why: &str) {
        let mut g = self.lock();
        for slot in g.values_mut() {
            if matches!(slot, Slot::Waiting) {
                *slot = Slot::Cancelled {
                    why: why.to_string(),
                };
            }
        }
        drop(g);
        self.cv.notify_all();
    }

    fn describe(&self) -> String {
        "an attached head, over the session socket".into()
    }
}

/// Turn a head's reply into the decision the gate reads.
///
/// A **permission** names an option the request offered; anything else is
/// `Unavailable`, which is [`letibot_tools::AskAdjudicator`]'s rule and is checked
/// again there. A **question's** answer cannot settle a permission and the hub has
/// already refused it at the door, so this arm is the belt to that pair of braces.
fn settle(
    req: &AdjudicationRequest,
    reply: &Reply,
    by: &str,
    latency_ms: u64,
) -> AdjudicationDecision {
    match reply {
        // The glob is not read here. `settle` builds the decision, and a pattern is
        // not part of one — it is what the answer said to do with the RULE, and only
        // `AllowAlways` writes one. `Answers::take_pattern` is where it is picked up.
        Reply::Permission {
            option_id, note, ..
        } => AdjudicationDecision {
            request_id: req.id.clone(),
            outcome: DecisionOutcome::Selected {
                option_id: option_id.clone(),
            },
            by: format!("human:{by}"),
            // **The operator's own words reach the model.** `deny_and_tell` is
            // labelled *"Deny, and tell the model why"*, and the why went nowhere:
            // the basis said only which button was pressed. It is the basis now,
            // which is what the refusal notice puts in front of the model and what
            // the corpus row keeps.
            //
            // Quoted rather than paraphrased, and attributed — a sentence the
            // model reads as the harness's own reasoning is a sentence it will
            // argue with; one it reads as the operator's is an instruction.
            basis: match note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                Some(why) => format!("{by} chose `{option_id}` at the head: \"{why}\""),
                // A `deny_and_tell` with nothing typed is a denial with no reason
                // given, and says so rather than implying one was.
                None if option_id == "deny_and_tell" => format!(
                    "{by} chose `{option_id}` at the head, without saying why — the \
                     reason is typed after the option id, as `deny_and_tell <why>`"
                ),
                None => format!("{by} chose `{option_id}` at the head"),
            },
            latency_ms,
        },
        // Not the person's decision: nobody chose an option. `by` says who
        // decided, and here nothing did — see the same rule in
        // `AskAdjudicator::decide`.
        Reply::Question(_) => AdjudicationDecision {
            request_id: req.id.clone(),
            outcome: DecisionOutcome::Unavailable,
            by: "gate:unavailable".into(),
            basis: format!(
                "a question's answer arrived from {by} for a permission, which settles \
                 nothing; a permission is answered by option id"
            ),
            latency_ms,
        },
    }
}

/// **The sentence a gate gets when nobody at the head can answer.**
///
/// A fact, not a policy — and for a subagent it is a different fact, told differently: the
/// card is not this session's to draw, so the refusal has to say *which session it belongs
/// to*, that no head is attached there, and what the child should do about it.
///
/// **"Stop and report" rather than "retry", and the reason is the shape of the failure**: a
/// retry reaches the same unattended session and costs another deadline, so a child that
/// retried would burn its whole turn proving the same fact. What it can do instead is tell
/// whoever asked for the work, who is the only party that can attach a head.
fn no_head(subagent: Option<&SubagentAsk>) -> String {
    let Some(s) = subagent else {
        return "no head that can answer is attached to this session, so nothing was \
                asked and nobody decided. The gate fails closed: nothing ran and \
                nothing changed. Attach a head, or run this seat in a foreground \
                terminal where the console adjudicator can reach a person."
            .to_string();
    };
    let task = if s.task.is_empty() {
        String::new()
    } else {
        format!(" — `{}`", s.task)
    };
    format!(
        "this is subagent `{}`'s call{task}, and its card belongs on session `{}`: the root \
         of its tree, which is the only session in a subagent tree with a head to draw it \
         (a child has none of its own). No head that can answer is attached there, so \
         nothing was asked and nobody decided. The gate fails closed: nothing ran and \
         nothing changed. Attaching a head to that session is the operator's move; the \
         subagent's is to STOP AND REPORT this call rather than retry it — a retry reaches \
         the same unattended session.",
        s.handle, s.root
    )
}

/// The `DecisionRequested` a head renders.
///
/// `subagent` is carried straight onto the card: a child's gate posts its card to the tree's
/// ROOT, and a card that arrived there unlabelled would be answered for the wrong thing.
fn pose(
    req: &AdjudicationRequest,
    deadline_ms: u64,
    subagent: Option<&SubagentAsk>,
) -> SessionEvent {
    SessionEvent::DecisionRequested {
        req_id: req.id.clone(),
        subagent: subagent.cloned(),
        kind: match req.kind {
            RequestKind::Permission => "permission".into(),
            RequestKind::Question => "question".into(),
        },
        call_id: Some(req.call_id.clone()),
        // **The tool's declared access, so the card can say why it is asking.** §11.7:
        // the headline names the declaration (`wants exec access`) and `detail` is layer
        // A's reading of the *action*, and nothing joined them. The request has carried
        // this as `ActionClass::access` all along; it had simply never been put on the
        // wire, so a head could only have got it by parsing its own sentence.
        access: req.class.access.as_str().to_string(),
        // The one line a person decides from, and then what layer A read, because a
        // permission the operator cannot evaluate is one they will approve out of
        // fatigue — which is the mechanism behind the 67% in §1.
        // **The question, and nothing else, on the line the operator reads first.**
        //
        // These three used to be one string — the sentence, then layer A's reading,
        // joined with an em dash — and the head then appended `[permission]` to it.
        // What that produced was *"`bash` wants exec access to `<no target
        // argument>` — ask — intents [read_file write_file execute_code] over
        // [host_other] [permission]"*: four registers in one line, the command
        // missing from the middle of it, and the taxonomy taking the space the
        // command needed. Now the head gets the parts and decides where each goes.
        summary: req.summary.clone(),
        target: req.target.clone(),
        detail: req.baseline.clone(),
        // **R35's severed wire, joined.** The classifier found these and they stopped there; the
        // card is where a person reads them. Taken off the request's own `reading` — the same
        // `Baseline` the tier and the intents come from — so the card and layer A's line cannot
        // disagree about what this call writes.
        //
        // **The MODEL is not told.** `brief()` is what a model sees and this does not touch it: the
        // model wrote the script, so the paths are not news to it, and §11.7's exclusion stands. The
        // operator is the reader this is for, which is also the cheaper claim — one head drawing
        // what the daemon already knew, and nothing disclosed to a model that did not ask.
        write_targets: req
            .reading
            .as_ref()
            .map(|b| {
                b.write_targets
                    .iter()
                    .map(|w| letibot_sessionlog::event::WriteTarget {
                        path: w.path().to_string(),
                        unresolved: !w.resolved(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        options: req.options.iter().map(wire_option).collect(),
        // A permission has no plain-text choices. `ask_user_question` fills these,
        // and it goes through the same seam.
        choices: Vec::new(),
        because: req.boundary_facts.first().cloned().unwrap_or_default(),
        // Verbatim from the request the model was asked about, not re-derived. A
        // head that reconstructed the verdict would render a guess about what the
        // oracle said — `ModelBrief`'s rule, one layer out.
        advice: req.advice.as_ref().map(|a| WireAdvice {
            consulted: a.consulted,
            would: a.would.to_string(),
            by: a.by.clone(),
            basis: a.basis.clone(),
            cites: a.cites.clone(),
            // **R12's third outcome, as a token** — see `WireAdvice::unsure`. `as_str()` and
            // not the debug name, because this is the same string the corpus column
            // `oracle_reading` stores and one vocabulary with two destinations does not drift.
            unsure: a.unsure.map(|k| k.as_str().to_string()),
            latency_ms: a.latency_ms,
        }),
        deadline: Some(deadline_ms),
        on_timeout: match req.on_timeout {
            OnTimeout::Deny => WireOnTimeout::Deny,
            OnTimeout::Allow => WireOnTimeout::Allow,
            OnTimeout::AgentDecides => WireOnTimeout::Ask,
        },
    }
}

/// The `DecisionAnswered` that takes it off the head's screen, however it ended.
fn close(req: &AdjudicationRequest, d: &AdjudicationDecision) -> SessionEvent {
    let outcome = match &d.outcome {
        DecisionOutcome::Selected { option_id } => WireOutcome::Selected {
            option_id: option_id.clone(),
        },
        DecisionOutcome::Timeout => WireOutcome::TimedOut,
        // `Escalate` and `Unavailable` are not outcomes the wire has, and the honest
        // mapping is `Cancelled`: the request is off the screen and nobody selected
        // anything. The `basis` carries which it was.
        _ => WireOutcome::Cancelled,
    };
    SessionEvent::DecisionAnswered {
        req_id: req.id.clone(),
        outcome,
        // **The decider says who it is; this does not guess.** `d.by` is already
        // `kind:identity` — `human:dead`, `model:qwen-3.8-27b`, `gate:timeout` —
        // so it is split rather than re-derived.
        //
        // It used to be derived from the OUTCOME, which made every `Selected` a
        // `human` whoever chose it, and put the whole `human:dead` into `identity`
        // besides — so a head drew `by human human:dead`, and a decision an oracle
        // selected drew `by human` over a model's id. Two wrong facts from one
        // guess about a field that was carrying the answer all along.
        by: split_decider(&d.by),
        basis: d.basis.clone(),
        late: false,
    }
}

/// `kind:identity` into its two halves. A `by` with no colon is all kind and no
/// identity — `gate:timeout` has one, a bare `subagent` does not — and an empty
/// identity renders as nothing rather than as an empty quoted name.
fn split_decider(by: &str) -> Decider {
    match by.split_once(':') {
        Some((kind, identity)) => Decider {
            kind: kind.to_string(),
            identity: identity.to_string(),
        },
        None => Decider {
            kind: by.to_string(),
            identity: String::new(),
        },
    }
}

fn wire_option(o: &letibot_tools::adjudicate::DecisionOption) -> DecisionOption {
    DecisionOption {
        option_id: o.id.clone(),
        label: o.label.clone(),
        kind: match o.kind {
            ToolOptionKind::AllowOnce => OptionKind::AllowOnce,
            ToolOptionKind::AllowSession => OptionKind::AllowSession,
            ToolOptionKind::AllowAlways => OptionKind::AllowAlways,
            ToolOptionKind::Deny => OptionKind::RejectOnce,
            ToolOptionKind::DenyAndTell => OptionKind::RejectAlways,
        },
    }
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// **The adjudicator that asks whoever is looking at the session.**
///
/// The seam is [`letibot_tools::Adjudicator`] and this is the implementation D25 says
/// is missing. It holds the call the way `ConsoleAdjudicator` does — §11.5 would
/// rather a pending decision were a visible row than a held connection, and it *is* a
/// visible row: the hold is on the tool thread, and the head sees
/// `DecisionRequested` the instant it is posted.
pub struct HeadAdjudicator {
    hub: Arc<Hub>,
    answers: Arc<Answers>,
    budget: Duration,
    /// What the last request rendered to, for [`Adjudicator::last_brief`] and
    /// therefore for the corpus row's `shown`. A person is shown the brief; recording
    /// it is what keeps §4c's *"the trail as it was actually shown"* honest for a
    /// human adjudicator as well as a model one.
    shown: Mutex<Option<String>>,
    /// The glob the operator typed with the last answer, for
    /// [`Adjudicator::last_pattern`]. `None` when they typed none, which means *use
    /// the pattern derived from the call*.
    pattern: Mutex<Option<String>>,
}

impl HeadAdjudicator {
    pub fn new(hub: Arc<Hub>, answers: Arc<Answers>) -> HeadAdjudicator {
        HeadAdjudicator {
            hub,
            answers,
            budget: ANSWER_BUDGET,
            shown: Mutex::new(None),
            pattern: Mutex::new(None),
        }
    }

    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// **The ask itself, with the caller's own attribution.**
    ///
    /// `None` is this session's own call, which is every card before R58's ask route
    /// existed. `Some` is a subagent's, posted to the session whose head this is — the
    /// only difference between the two, and it is one parameter rather than a second
    /// adjudicator because the bookkeeping below (what the person was shown, the glob
    /// they typed) is the same bookkeeping and must not be written twice.
    pub fn decide_as(
        &self,
        req: &AdjudicationRequest,
        subagent: Option<&SubagentAsk>,
    ) -> AdjudicationDecision {
        *self.shown.lock().unwrap_or_else(|e| e.into_inner()) = Some(req.brief());
        // **Cleared before the ask, not after.** A pattern left over from the
        // previous answer would be applied to this one, which is a rule the operator
        // did not write for a call they were not looking at when they typed it.
        *self.pattern.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let d = self.answers.ask(&self.hub, req, self.budget, subagent);
        *self.pattern.lock().unwrap_or_else(|e| e.into_inner()) =
            self.answers.take_pattern(&req.id);
        d
    }
}

impl Adjudicator for HeadAdjudicator {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        self.decide_as(req, None)
    }

    fn describe(&self) -> String {
        let heads = self.hub.deciding_heads();
        if heads.is_empty() {
            // **Read, not assumed.** A banner that said "a head decides" for a
            // session with no head is the disclosure claiming a safety property the
            // session does not have, which is the defect `startup_disclosure` exists
            // to stop one layer up.
            return format!(
                "a head over the session socket — none attached right now, so every gated \
                 call refuses with not_run until one is (deadline {}s)",
                self.budget.as_secs()
            );
        }
        format!(
            "asked at the head: {} (deadline {}s)",
            heads.join(", "),
            self.budget.as_secs()
        )
    }

    fn last_pattern(&self) -> Option<String> {
        self.pattern
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn last_brief(&self) -> Option<String> {
        self.shown.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// **A subagent's gate, asking the head at the ROOT of its tree.**
///
/// The operator, on who a subagent's ask belongs to: *"who asks subagents permissions? i
/// think they should surface to the parent head all the way to the root obviously"*. A child
/// has no head attached and **cannot be driven** — it is adopted into the session registry and
/// not into `Sessions::open`, so `Sessions::wake` cannot run its turn — so its
/// gate's card is posted to the one session in the tree that does have a head: the root.
///
/// This is deliberately **not a second mechanism**: it is a [`HeadAdjudicator`] over the
/// root's hub and the root's answers, with the child's own name on the card and on the
/// refusal. Everything about the ask — the slot, the rendezvous, the deadline, the close, the
/// recorded brief and glob — is that one implementation, which is what makes a child's answer
/// the same answer the root would have got.
///
/// **What it does not do: make the child drivable.** Nothing here touches `Sessions::open` or
/// the child's watcher set; the card travels and the answer comes back on the asking thread,
/// exactly as a root's does. A child that IS served is served by the thread that owns it —
/// `Sessions::wake` handing the wake to `Hub::wake_its_own_reader` — which is a different
/// door from this one.
///
/// `head` is `None` when the tree has no head-wired adjudicator at all — a root run with
/// `--adjudicator console`, or one whose answers a child cannot reach. There is then nothing
/// to post to, and the refusal says which session it would have been and that the child should
/// stop and report rather than retry.
pub struct SubagentAdjudicator {
    /// Who is asking: the child's handle, its task, and the root it belongs to.
    child: SubagentAsk,
    /// The tree's answer path, when there is one.
    head: Option<HeadAdjudicator>,
}

impl SubagentAdjudicator {
    pub fn new(child: SubagentAsk, head: Option<HeadAdjudicator>) -> SubagentAdjudicator {
        SubagentAdjudicator { child, head }
    }

    /// **No head-wired adjudicator in this tree, said as the fact it is.**
    ///
    /// `by` is `gate:no-head`, the same word the root's own no-head refusal uses, because it
    /// is the same fact one level over: the gate could not ask anybody. The child's handle,
    /// its task and the session the card would belong to are all in the basis, because a
    /// refusal that does not name where it failed is one nobody can act on.
    fn no_path(&self) -> String {
        let task = if self.child.task.is_empty() {
            String::new()
        } else {
            format!(" — `{}`", self.child.task)
        };
        format!(
            "this is subagent `{}`'s call{task}, and there is nobody to ask: its card would be \
             posted to session `{}`, the root of its tree, and that session has no head-wired \
             adjudicator at all — so there is no head to reach at any depth up to the root. \
             The gate fails closed: nothing ran and nothing changed. The child should STOP AND \
             REPORT this call rather than retry it: a retry finds the same empty tree. A root \
             that does have one (`--adjudicator head`, or `model` with no oracle answer) is the \
             configuration this ask needs.",
            self.child.handle, self.child.root
        )
    }
}

impl Adjudicator for SubagentAdjudicator {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        match &self.head {
            Some(head) => head.decide_as(req, Some(&self.child)),
            None => AdjudicationDecision::unavailable(req, "gate:no-head", &self.no_path()),
        }
    }

    /// **Never starts with `none`**: that prefix is the harness's own signal for *nobody
    /// reachable, refuse to open a gated session*, and a subagent does have a decider — the
    /// ruleset it inherited. What reaches this adjudicator is only what that ruleset left as
    /// `ask`, and that now goes to the tree's root.
    fn describe(&self) -> String {
        match &self.head {
            Some(_) => format!(
                "subagent `{}` — its asks surface at the head over session `{}`, the root of \
                 its tree; the inherited permission rules decide the rest",
                self.child.handle, self.child.root
            ),
            None => format!(
                "subagent `{}` — no head-wired adjudicator in its tree, so an ask fails \
                 closed (session `{}` is the root it would have been posted to)",
                self.child.handle, self.child.root
            ),
        }
    }
}

/// **The model decides; when it cannot, the person does.**
///
/// `--adjudicator model` puts the model in front of the gate alone, and an oracle
/// that timed out failed the call closed with a refusal the operator experienced as
/// a card that sat yellow and then said `not run` — their rule, after that: *"if a
/// tool call reaches oracle and timeouts — the timeout must be visible and result
/// in a human ask"*. The timeout already is visible (the card's note names the
/// consult while it waits); what was missing was the ask.
///
/// This wrapper forwards to the model and, when the answer means **nobody decided**
/// — [`DecisionOutcome::Timeout`] or [`DecisionOutcome::Unavailable`] — asks the
/// person instead. A `Cancelled` decision is *not* forwarded: a cancel is somebody
/// stopping the turn, and asking over them would be input arriving from nowhere.
/// And when no head is attached the ask returns `Unavailable` immediately, so an
/// unattended session degrades to exactly the fail-closed refusal it had before —
/// the fallback makes a session stronger when a person is there and changes nothing
/// when one is not, which is the difference between this and the silent fallback
/// `--oracle`'s absence refuses to make.
pub struct EscalateOnTimeout {
    inner: Arc<dyn Adjudicator>,
    human: Arc<dyn Adjudicator>,
}

impl EscalateOnTimeout {
    pub fn new(inner: Arc<dyn Adjudicator>, human: Arc<dyn Adjudicator>) -> Self {
        EscalateOnTimeout { inner, human }
    }
}

impl Adjudicator for EscalateOnTimeout {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        let d = self.inner.decide(req);
        match d.outcome {
            DecisionOutcome::Timeout | DecisionOutcome::Unavailable => {
                let mut h = self.human.decide(req);
                // The record keeps both facts: the person decided, and the reason
                // they were asked is that the guard did not answer. A corpus row
                // that showed only the first would read as a person overruling a
                // model that never spoke.
                h.basis.push_str(&format!(" {}", d.basis));
                h
            }
            _ => d,
        }
    }

    fn describe(&self) -> String {
        format!(
            "{}, and when it cannot answer in time, the person at the head",
            self.inner.describe()
        )
    }

    fn last_pattern(&self) -> Option<String> {
        // Whichever of the two answered: a pattern typed with a person's answer
        // belongs to the person, and a model that minted one reported its own.
        self.human
            .last_pattern()
            .or_else(|| self.inner.last_pattern())
    }

    fn last_brief(&self) -> Option<String> {
        self.human.last_brief().or_else(|| self.inner.last_brief())
    }

    /// **The model's bytes, not the person's** (R11). The corpus column is the input an
    /// ORACLE was given, and the human's half of this pair has no such bytes; where both
    /// answered, the model's is the exchange a reader came to check.
    fn last_reply(&self) -> Option<String> {
        self.inner.last_reply().or_else(|| self.human.last_reply())
    }

    fn last_advice(&self) -> Option<ModelAdvice> {
        // The model's verdict travels to the corpus row even when it lost the
        // decision to the timeout — that row is the disagreement signal.
        self.inner.last_advice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_sessionlog::protocol::Caps;
    use letibot_sessionlog::view::OpenDecision;

    pub(super) fn request() -> AdjudicationRequest {
        use letibot_tools::adjudicate::{ActionClass, Tier};
        use letibot_tools::schema::Access;
        AdjudicationRequest {
            scripts: Vec::new(),
            id: "adj-1".into(),
            session_id: "s".into(),
            turn_id: "t1".into(),
            call_id: "c1".into(),
            agent: "a".into(),
            tool: "write".into(),
            target: "src/main.rs".into(),
            class: ActionClass::host(Access::Write, true, false),
            summary: "`write` wants write access to `src/main.rs`".into(),
            arguments: serde_json::json!({"path": "src/main.rs"}),
            arguments_digest: "fnv1a:0".into(),
            boundary_facts: vec!["workspace: /tmp/p".into()],
            kind: RequestKind::Permission,
            options: letibot_tools::adjudicate::permission_options(Some("src/main.rs")),
            on_timeout: OnTimeout::Deny,
            tier: Tier::MayApprove,
            resolved: true,
            baseline: "writes one file inside the project".into(),
            trail: Default::default(),
            prior: Vec::new(),
            examples: Vec::new(),
            brief_variant: letibot_tools::authorise::BriefVariant::AskedForIt,
            agent_claim: None,
            reading: None,
            shape: None,
            advice: None,
        }
    }

    fn open_of(hub: &Arc<Hub>) -> Vec<OpenDecision> {
        hub.snapshot().open_decisions
    }

    /// **The whole of D25 in one test**: the decision reaches a head, the head sends
    /// an answer through the frame that already existed, and the tool thread — which
    /// is not the thread the answer arrived on — wakes with it.
    #[test]
    fn a_head_answers_a_decision_the_daemons_stdin_could_not() {
        let hub = Hub::new("s");
        let answers = Arc::new(Answers::new());
        hub.set_answer_sink(answers.clone());
        let head = hub.attach("tui", "deadtrickster", Caps::default(), 0);

        // One value for the adjudicator's budget and the answerer's patience.
        let budget = Duration::from_secs(10);
        let adj = HeadAdjudicator::new(hub.clone(), answers.clone()).with_budget(budget);
        let req = request();

        // The head's side, on its own thread: wait for the question to appear, then
        // answer it the way `letibot-tui` does.
        let hub2 = hub.clone();
        let head_id = head.head_id.clone();
        // **No second deadline.** The answerer used to poll 500 × 2ms — one
        // second — while the adjudicator it races waits ten, so on a box running
        // the whole workspace's test binaries at once it gave up first and
        // panicked "the decision never reached the head" about a decision that
        // was on its way. Widening it to the budget only moved the race: with
        // both at ten seconds a loaded box lost it again the same afternoon.
        //
        // A poller racing a deadline it does not own cannot be made reliable by
        // choosing a bigger number. This one stops when the thing it is helping
        // has finished, and the FAILURE is then the assertion below — "the
        // adjudicator did not come back with the answer" — which is a statement
        // about the code rather than about the load on the box.
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = stop.clone();
        let answerer = std::thread::spawn(move || {
            while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
                let open = hub2.snapshot().open_decisions;
                if let Some(d) = open.first() {
                    // **And it is this session's own card**, not a subagent's: the new
                    // attribution field is `None` for every ask that is not a child's, which
                    // is what keeps the clause off every card that never needed it.
                    assert!(
                        d.subagent.is_none(),
                        "this session's own call was labelled a subagent's: {:?}",
                        d.subagent
                    );
                    hub2.submit(
                        &head_id,
                        "c1",
                        0,
                        letibot_sessionlog::hub::CommandKind::Answer {
                            req_id: d.req_id.clone(),
                            reply: Reply::Permission {
                                option_id: "allow_once".into(),
                                pattern: None,
                                note: None,
                            },
                        },
                    );
                    return true;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            // The adjudicator returned before a decision was ever open. Not a
            // panic: whatever it returned is what the assertions below read,
            // and they say more about why than this thread can.
            false
        });

        let d = adj.decide(&req);
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let submitted = answerer.join().unwrap();
        assert!(submitted, "the head never got to answer: {d:?}");

        assert_eq!(
            d.outcome,
            DecisionOutcome::Selected {
                option_id: "allow_once".into()
            },
            "{d:?}"
        );
        assert!(d.by.contains("deadtrickster"), "{}", d.by);
        assert!(
            open_of(&hub).is_empty(),
            "an answered decision is off the head's screen"
        );
    }

    // ---- `ask_user_question`: the other vocabulary, the same rendezvous -----

    fn a_question() -> Question {
        Question {
            text: "which database should the migration target?".into(),
            options: vec!["postgres".into(), "sqlite".into()],
            because: "the two need different migration files".into(),
        }
    }

    /// **The whole of `ask_user_question` reaching a person**, end to end inside the
    /// daemon: the question is posed to the head as a card with its choices, the head
    /// answers it through the frame that already existed, and the tool thread — which
    /// is not the thread the answer arrived on — wakes with the answer **and who gave
    /// it**.
    ///
    /// The attribution is the half that is not a formality: a question's result is a
    /// claim about what a person said, and the identity is read from the connection the
    /// answer arrived on rather than from anything the answer carries.
    #[test]
    fn a_head_answers_a_question_and_the_answer_carries_who_gave_it() {
        let hub = Hub::new("s");
        let answers = Arc::new(Answers::new());
        hub.set_answer_sink(answers.clone());
        let head = hub.attach("tui", "deadtrickster", Caps::default(), 0);

        let budget = Duration::from_secs(10);
        let q = a_question();
        let asker = HeadQuestioner::new(hub.clone(), answers.clone()).with_budget(budget);

        // The head's side, on its own thread: wait for the card, check it is a question
        // with the model's own choices on it, and answer it the way `letibot-tui` does.
        let hub2 = hub.clone();
        let head_id = head.head_id.clone();
        let seen = Arc::new(std::sync::Mutex::new(None));
        let seen2 = seen.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = stop.clone();
        let answerer = std::thread::spawn(move || {
            while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
                let open = hub2.snapshot().open_decisions;
                if let Some(d) = open.first() {
                    *seen2.lock().unwrap() = Some(d.clone());
                    hub2.submit(
                        &head_id,
                        "c1",
                        0,
                        letibot_sessionlog::hub::CommandKind::Answer {
                            req_id: d.req_id.clone(),
                            reply: Reply::Question(
                                letibot_sessionlog::question::QuestionAnswer::choosing(1)
                                    .with_note("only for the CUDA box"),
                            ),
                        },
                    );
                    return true;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            false
        });

        let got = asker.ask(&q);
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let submitted = answerer.join().unwrap();
        assert!(submitted, "the head never got the question: {got:?}");

        let (a, by) = got.expect("the head answered");
        assert_eq!(a.option, Some(1), "the choice came back as an index");
        assert_eq!(a.note.as_deref(), Some("only for the CUDA box"));
        assert_eq!(a.free, None);
        assert!(!a.abstain);
        assert_eq!(by, "deadtrickster", "the answer is attributed to nobody");

        // **The card is the surface, and it is the card the composer answers.** The
        // posed `DecisionRequested` is a `question` whose ladder is its plain-text
        // choices — `options` is the adjudication ladder and is empty for one, which is
        // why the hub validates an answer against `choices`.
        let card = seen.lock().unwrap().clone().expect("the card was seen");
        assert_eq!(card.kind, "question");
        assert_eq!(card.summary, q.text);
        assert_eq!(card.choices, q.options);
        assert_eq!(card.because, q.because);
        assert!(card.options.is_empty(), "a question has no ladder");
        assert!(card.deadline.is_some(), "a question expires like any ask");

        // And it is off the head's screen, with the answer recorded against it.
        assert!(
            open_of(&hub).is_empty(),
            "an answered question is off the head's screen"
        );
        let settled = hub
            .snapshot()
            .settled_decisions
            .into_iter()
            .find(|d| d.req_id == card.req_id)
            .expect("the answer is on the log");
        assert_eq!(settled.by.kind, "human");
        assert_eq!(settled.by.identity, "deadtrickster");
        assert!(
            settled.basis.contains("chose option 1: sqlite")
                && settled.basis.contains("only for the CUDA box"),
            "the basis is not what the person said: {}",
            settled.basis
        );
    }

    /// **The exchange is recorded where the session can write it down, and only an
    /// answer is.**
    ///
    /// The two halves of this are one test because they are one rule: what a session
    /// turns into the operator's own row is what a person SAID, and a question nobody
    /// answered is not something they said. `Answers::ask_question`'s `Err` arms are
    /// every way a question ends without an answer, and none of them may leave a record
    /// behind that the transcript would then attribute to somebody.
    #[test]
    fn a_questioner_records_the_exchange_it_carried_and_nothing_for_a_refusal() {
        let hub = Hub::new("s");
        let answers = Arc::new(Answers::new());
        hub.set_answer_sink(answers.clone());
        let head = hub.attach("tui", "deadtrickster", Caps::default(), 0);
        let q = a_question();
        let log: AskLog = Default::default();
        let asker = HeadQuestioner::new(hub.clone(), answers.clone())
            .with_budget(Duration::from_secs(10))
            .with_ask_log(log.clone());

        let hub2 = hub.clone();
        let head_id = head.head_id.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = stop.clone();
        let answerer = std::thread::spawn(move || {
            while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
                if let Some(d) = hub2.snapshot().open_decisions.first() {
                    hub2.submit(
                        &head_id,
                        "c1",
                        0,
                        letibot_sessionlog::hub::CommandKind::Answer {
                            req_id: d.req_id.clone(),
                            reply: Reply::Question(
                                letibot_sessionlog::question::QuestionAnswer::choosing(0),
                            ),
                        },
                    );
                    return true;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            false
        });
        let got = asker.ask(&q);
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(answerer.join().unwrap(), "the head never got the question");
        assert!(got.is_ok(), "{got:?}");

        let held = log.lock().unwrap().clone().expect("the exchange is recorded");
        assert_eq!(held.question.text, q.text, "the question it answered");
        assert_eq!(held.question.options, q.options);
        assert_eq!(held.answer.option, Some(0));
        assert_eq!(held.by, "deadtrickster", "the answer is attributed to nobody");

        // **A refusal records nothing.** No head is attached to this hub, so the ask
        // refuses before anybody is posted to — and the slot it left is the empty one.
        let bare = Hub::new("bare");
        let log: AskLog = Default::default();
        let asker = HeadQuestioner::new(bare, Arc::new(Answers::new()))
            .with_budget(Duration::from_secs(1))
            .with_ask_log(log.clone());
        let refused = asker.ask(&q);
        assert!(matches!(refused, Err(AskError::NoHead(_))), "{refused:?}");
        assert!(
            log.lock().unwrap().is_none(),
            "a question nobody answered left a record to attribute to somebody"
        );
    }

    /// **An abstention travels the whole way and is not silence.** The head sends the
    /// fourth shape; the hub accepts it (it is not an empty answer and it conforms);
    /// and what comes back is the abstention itself rather than an error, which is what
    /// lets the tool report `Abstained` rather than `not_run`.
    #[test]
    fn an_abstention_is_an_answer_that_reaches_the_tool_as_one() {
        let hub = Hub::new("s");
        let answers = Arc::new(Answers::new());
        hub.set_answer_sink(answers.clone());
        let head = hub.attach("tui", "deadtrickster", Caps::default(), 0);
        let budget = Duration::from_secs(10);
        let q = a_question();
        let asker = HeadQuestioner::new(hub.clone(), answers.clone()).with_budget(budget);

        let hub2 = hub.clone();
        let head_id = head.head_id.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = stop.clone();
        let answerer = std::thread::spawn(move || {
            while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
                let open = hub2.snapshot().open_decisions;
                if let Some(d) = open.first() {
                    hub2.submit(
                        &head_id,
                        "c1",
                        0,
                        letibot_sessionlog::hub::CommandKind::Answer {
                            req_id: d.req_id.clone(),
                            reply: Reply::Question(
                                letibot_sessionlog::question::QuestionAnswer::abstaining(),
                            ),
                        },
                    );
                    return true;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            false
        });

        let got = asker.ask(&q);
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(answerer.join().unwrap(), "the head never got the question");

        let (a, by) = got.expect("an abstention is an answer, not a failure to answer");
        assert!(a.abstain, "{a:?}");
        assert_eq!(by, "deadtrickster");
        // And the log says what they did rather than leaving it as a bare id.
        let settled = hub
            .snapshot()
            .settled_decisions
            .into_iter()
            .next()
            .expect("the abstention is on the log");
        assert!(
            settled.basis.contains("abstained"),
            "the head cannot tell this from an empty answer: {}",
            settled.basis
        );
    }

    /// **No head, and it refuses by name rather than waiting out a deadline.** This is
    /// the behaviour `Headless` had and it is kept: the ask must not hang, and it must
    /// not invent an answer. Measured rather than asserted in prose — a refusal that
    /// took the budget would pass a test that only looked at the error.
    #[test]
    fn a_question_with_no_head_refuses_by_name_and_does_not_wait() {
        let hub = Hub::new("s");
        let answers = Arc::new(Answers::new());
        // No `attach`, so nobody can decide: the hub's own answer.
        let budget = Duration::from_secs(30);
        let asker = HeadQuestioner::new(hub.clone(), answers).with_budget(budget);
        let started = std::time::Instant::now();
        let refused = asker.ask(&a_question());
        let took = started.elapsed();
        let err = refused.expect_err("nobody was there to answer");
        assert!(matches!(err, AskError::NoHead(_)), "{err:?}");
        let said = err.to_string();
        assert!(said.contains("NOBODY was asked"), "{said}");
        assert!(said.contains("still open"), "{said}");
        assert!(
            !said.to_lowercase().contains("best judg"),
            "the survey's failure, reproduced: {said}"
        );
        assert!(
            took < budget / 2,
            "a session with nobody to ask sat out the deadline: {took:?}"
        );
        // Nothing was posed, so nothing is left open on a screen nobody has.
        assert!(open_of(&hub).is_empty());
    }

    /// A head that is attached and cannot decide is the same fact as no head, and it is
    /// found out the same way — before the wait, not after it.
    #[test]
    fn a_question_to_a_head_that_cannot_decide_refuses_at_once() {
        let hub = Hub::new("s");
        let answers = Arc::new(Answers::new());
        hub.set_answer_sink(answers.clone());
        // A read-only connector: attached, and not somebody who may answer.
        hub.attach(
            "acp",
            "stranger",
            Caps {
                can_decide: false,
                ..Caps::default()
            },
            0,
        );
        let budget = Duration::from_secs(30);
        let asker = HeadQuestioner::new(hub, answers).with_budget(budget);
        let started = std::time::Instant::now();
        let err = asker.ask(&a_question()).expect_err("nobody may answer");
        assert!(matches!(err, AskError::NoHead(_)), "{err:?}");
        assert!(
            started.elapsed() < budget / 2,
            "the ask sat out a deadline for an answer nobody was allowed to give"
        );
    }

    /// **A question nobody answers is `not_run`, and it is not a declining.** The same
    /// rule the gate keeps, for the same reason: a deadline is not a decision.
    #[test]
    fn a_question_nobody_answers_is_unanswered_and_the_card_comes_down() {
        let hub = Hub::new("s");
        let answers = Arc::new(Answers::new());
        hub.set_answer_sink(answers.clone());
        let _head = hub.attach("tui", "deadtrickster", Caps::default(), 0);
        // Short, because the point is the ending rather than the patience.
        let asker =
            HeadQuestioner::new(hub.clone(), answers).with_budget(Duration::from_millis(60));
        let err = asker.ask(&a_question()).expect_err("nobody answered");
        assert!(matches!(err, AskError::Unanswered(_)), "{err:?}");
        assert!(err.to_string().contains("still open"), "{err}");
        assert!(
            open_of(&hub).is_empty(),
            "an unanswered question was left on the head's screen"
        );
    }

    /// A read-only head is not somebody who can answer, and finding that out must
    /// cost microseconds rather than a deadline. Before the `deciding_heads` check
    /// this was a five-minute wait reported as a timeout — *"a real answer given for
    /// a fake reason"*.
    #[test]
    fn a_session_with_nobody_who_can_answer_refuses_at_once() {
        let hub = Hub::new("s");
        let answers = Arc::new(Answers::new());
        hub.set_answer_sink(answers.clone());
        hub.attach(
            "flowy",
            "room:general",
            Caps {
                can_decide: false,
                ..Caps::default()
            },
            0,
        );
        let adj = HeadAdjudicator::new(hub.clone(), answers).with_budget(Duration::from_secs(30));
        let started = Instant::now();
        let d = adj.decide(&request());
        assert!(started.elapsed() < Duration::from_secs(5), "it waited");
        assert_eq!(d.outcome, DecisionOutcome::Unavailable);
        assert!(d.basis.contains("nothing was asked"), "{}", d.basis);
        assert!(
            open_of(&hub).is_empty(),
            "nothing was posted, so nothing is open"
        );
    }

    /// Nobody answers: `Timeout`, which the gate routes to `on_timeout` — `Deny` for
    /// a permission — and which becomes `not_run`, never `denied`. And the decision
    /// is taken off the screen, or the head keeps offering a prompt for a call that
    /// was refused minutes ago.
    #[test]
    fn a_deadline_that_passes_is_nobody_deciding_and_closes_the_row() {
        let hub = Hub::new("s");
        let answers = Arc::new(Answers::new());
        hub.set_answer_sink(answers.clone());
        hub.attach("tui", "alice", Caps::default(), 0);
        let adj = HeadAdjudicator::new(hub.clone(), answers).with_budget(Duration::from_millis(30));
        let d = adj.decide(&request());
        assert_eq!(d.outcome, DecisionOutcome::Timeout, "{d:?}");
        assert!(d.basis.contains("not a denial"), "{}", d.basis);
        assert!(open_of(&hub).is_empty(), "the row is closed either way");
    }

    /// **The yellow card says why it is yellow.** The ask renders in the chrome,
    /// but the call is where the eye already is — and the operator's report was
    /// *"some tool calls stay yellow, no idea what that means"*. So posing an ask
    /// also publishes a progress note on the call itself, naming the wait and
    /// the budget. The call is still `Proposed` at this point — `ToolStarted`
    /// fires only after the gate admits — which is exactly why the note has to
    /// travel on `ToolProgress`: it is the one event the head applies to an open
    /// call of either state.
    #[test]
    fn an_open_ask_tells_the_card_why_it_waits() {
        let hub = Hub::new("s");
        let answers = Arc::new(Answers::new());
        hub.set_answer_sink(answers.clone());
        hub.attach("tui", "alice", Caps::default(), 0);
        let adj = HeadAdjudicator::new(hub.clone(), answers).with_budget(Duration::from_millis(30));
        let _ = adj.decide(&request());
        let note = hub
            .retained()
            .iter()
            .find_map(|e| match &e.event {
                SessionEvent::ToolProgress { note, .. } => Some(note.clone()),
                _ => None,
            })
            .expect("posing an ask published a note on the call");
        assert!(note.contains("waiting for you"), "{note}");
        assert!(note.contains("nothing runs"), "{note}");
    }

    // ------------------------------------------------- a subagent's card (R58's ask route)

    /// The child every one of these tests is about: a real handle, a real task, and the root
    /// whose head its card belongs on.
    fn child_ask() -> SubagentAsk {
        SubagentAsk {
            handle: "s-sub-3".into(),
            task: "count the rows the store never reads".into(),
            root: "root".into(),
        }
    }

    /// **A child's ask is the ROOT's card, named, and the answer comes back to the child.**
    ///
    /// The operator's ruling, verbatim: *"who asks subagents permissions? i think they should
    /// surface to the parent head all the way to the root obviously"*. A child has no head
    /// attached and cannot be driven, so its gate's ask is posted to the session that does
    /// have one — and the operator has to be able to tell it from that session's own call,
    /// or the card is answered for the wrong thing.
    #[test]
    fn a_subagents_ask_lands_on_the_roots_card_and_the_answer_comes_back() {
        let root = Hub::new("root");
        let answers = Arc::new(Answers::new());
        root.set_answer_sink(answers.clone());
        let head = root.attach("tui", "deadtrickster", Caps::default(), 0);
        // The child's own session, with NOTHING installed on it: a head could attach there,
        // and no card belongs there.
        let child = Hub::new("s-sub-3");

        let budget = Duration::from_secs(10);
        let adj = SubagentAdjudicator::new(
            child_ask(),
            Some(HeadAdjudicator::new(root.clone(), answers.clone()).with_budget(budget)),
        );
        let mut req = request();
        req.id = "adj-s-sub-3-0001".into();
        req.session_id = "s-sub-3".into();

        // **The head's side, on its own thread** — the same thread the root's own ask is
        // answered on, reading the card off the ROOT and answering through the ROOT's head.
        let root2 = root.clone();
        let child2 = child.clone();
        let head_id = head.head_id.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = stop.clone();
        let answerer = std::thread::spawn(move || {
            while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
                let open = root2.snapshot().open_decisions;
                if let Some(d) = open.first() {
                    // **Nothing was posted to the child.** The card belongs to the session
                    // whose head exists; a card on a session nothing drives would sit open
                    // for the life of the tree.
                    assert!(
                        child2.snapshot().open_decisions.is_empty(),
                        "a subagent's ask was posted to the subagent's own session"
                    );
                    let seen = d.subagent.clone();
                    root2.submit(
                        &head_id,
                        "c1",
                        0,
                        letibot_sessionlog::hub::CommandKind::Answer {
                            req_id: d.req_id.clone(),
                            reply: Reply::Permission {
                                option_id: "allow_once".into(),
                                pattern: None,
                                note: None,
                            },
                        },
                    );
                    return seen;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            None
        });

        let d = adj.decide(&req);
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let seen = answerer
            .join()
            .unwrap()
            .expect("the head never got a card to answer: {d:?}");

        // **The card named the CHILD, its task and the session it belongs to** — the three
        // facts that stop it reading as the root's own call.
        assert_eq!(seen, child_ask());
        // **And the child's gate got the operator's own answer**, acting on it exactly as the
        // root would have.
        assert_eq!(
            d.outcome,
            DecisionOutcome::Selected {
                option_id: "allow_once".into()
            },
            "{d:?}"
        );
        assert!(d.by.contains("deadtrickster"), "{}", d.by);
        // Answered on the root's queue, so the row is off the root's screen either way.
        assert!(open_of(&root).is_empty());
        assert!(open_of(&child).is_empty());
    }

    /// **Nobody at the root to answer: the refusal is a fact, by name.**
    ///
    /// The old sentence was a policy about subagents — *"a subagent has no operator to ask; an
    /// ask is denied"* — which said nothing about where the ask went or what the child should
    /// do. This one names the child, the session its card belongs to, that no head is attached
    /// there, and that the child should stop and report rather than retry.
    #[test]
    fn a_subagents_ask_with_nobody_at_the_root_refuses_by_name() {
        let root = Hub::new("root");
        let answers = Arc::new(Answers::new());
        root.set_answer_sink(answers.clone());
        // A read-only head is not somebody who can answer — the sibling test above's premise,
        // and here it is the ROOT's state, which is what the child has to be told about.
        root.attach(
            "flowy",
            "room:general",
            Caps {
                can_decide: false,
                ..Caps::default()
            },
            0,
        );
        let child = Hub::new("s-sub-3");
        let adj = SubagentAdjudicator::new(
            child_ask(),
            Some(HeadAdjudicator::new(root.clone(), answers).with_budget(Duration::from_secs(30))),
        );
        let started = Instant::now();
        let d = adj.decide(&request());
        assert!(started.elapsed() < Duration::from_secs(5), "it waited");
        assert_eq!(d.outcome, DecisionOutcome::Unavailable);
        assert!(d.by.starts_with("gate:no-head"), "{}", d.by);
        // Which child, which session the card belongs to, that nothing was asked, and what to
        // do about it.
        for want in [
            "s-sub-3",
            "root",
            "nothing was asked",
            "STOP AND REPORT",
            "rather than retry",
        ] {
            assert!(d.basis.contains(want), "{want:?} is not in {:?}", d.basis);
        }
        // **Refused before the post**, so no card is left open on a session nothing will
        // answer — on either queue.
        assert!(open_of(&root).is_empty());
        assert!(open_of(&child).is_empty());
    }

    /// **A tree with no head-wired adjudicator at all**, which is a different fact from a root
    /// whose head is not attached right now — and it is the one the child can do nothing about.
    #[test]
    fn a_subagent_in_a_tree_with_no_head_adjudicator_refuses_by_name() {
        let adj = SubagentAdjudicator::new(child_ask(), None);
        // **Never `none`.** That prefix is the harness's own signal for *nobody reachable,
        // refuse to open a gated session*, and this adjudicator does have a decider: the
        // ruleset the child inherited. It simply has nobody to ask.
        assert!(!adj.describe().starts_with("none"), "{}", adj.describe());
        assert!(adj.describe().contains("s-sub-3"), "{}", adj.describe());
        assert!(adj.describe().contains("root"), "{}", adj.describe());

        let d = adj.decide(&request());
        assert_eq!(d.outcome, DecisionOutcome::Unavailable);
        assert!(d.by.starts_with("gate:no-head"), "{}", d.by);
        for want in [
            "s-sub-3",
            "root",
            "no head-wired adjudicator",
            "STOP AND REPORT",
            "rather than retry it",
        ] {
            assert!(d.basis.contains(want), "{want:?} is not in {:?}", d.basis);
        }
    }

    // ------------------------------------------------------------- escalation

    /// A stand-in adjudicator that answers one of four ways, and counts how many
    /// times it was asked — the counter is what distinguishes *decided* from
    /// *consulted on the way past*. The name is what the decision's `by`
    /// carries, so a test can tell which side answered.
    struct Stub {
        kind: StubKind,
        name: &'static str,
        asked: std::sync::Mutex<usize>,
    }

    enum StubKind {
        /// The oracle's timeout: nobody decided.
        TimesOut,
        /// The oracle is down: also nobody decided.
        Unavailable,
        /// Somebody stopped the turn.
        Cancels,
        /// A verdict: the first option, allowed.
        Allows,
    }

    impl Stub {
        fn new(kind: StubKind, name: &'static str) -> Arc<Stub> {
            Arc::new(Stub {
                kind,
                name,
                asked: std::sync::Mutex::new(0),
            })
        }
        fn asks(&self) -> usize {
            *self.asked.lock().unwrap()
        }
    }

    impl Adjudicator for Stub {
        fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
            *self.asked.lock().unwrap() += 1;
            let (outcome, basis) = match self.kind {
                StubKind::TimesOut => {
                    (DecisionOutcome::Timeout, "the guard did not answer in time")
                }
                StubKind::Unavailable => {
                    (DecisionOutcome::Unavailable, "the guard is not reachable")
                }
                StubKind::Cancels => (DecisionOutcome::Cancelled, "the turn was stopped"),
                StubKind::Allows => (
                    DecisionOutcome::Selected {
                        option_id: "allow_once".into(),
                    },
                    "the person allowed it",
                ),
            };
            AdjudicationDecision {
                request_id: req.id.clone(),
                outcome,
                by: self.name.into(),
                basis: basis.into(),
                latency_ms: 1,
            }
        }
        fn describe(&self) -> String {
            self.name.into()
        }
    }

    /// The operator's rule: **an oracle timeout is a hand-off, not a refusal.**
    /// The model cannot answer, so the person is asked — and the record keeps
    /// both facts: the person's decision, and the reason they were asked. A row
    /// that showed only the first would read as a person overruling a model
    /// that never spoke.
    #[test]
    fn an_oracle_timeout_asks_the_person_instead_of_refusing() {
        let model = Stub::new(StubKind::TimesOut, "stub:model");
        let person = Stub::new(StubKind::Allows, "stub:person");
        let esc = EscalateOnTimeout::new(model.clone(), person.clone());
        let d = esc.decide(&request());
        assert_eq!(model.asks(), 1);
        assert_eq!(person.asks(), 1, "the person was asked");
        assert_eq!(
            d.outcome,
            DecisionOutcome::Selected {
                option_id: "allow_once".into()
            },
            "{d:?}"
        );
        assert!(d.by.contains("person"), "{}", d.by);
        assert!(d.basis.contains("the person allowed it"), "{}", d.basis);
        assert!(
            d.basis.contains("the guard did not answer in time"),
            "the record keeps why the person was asked: {}",
            d.basis
        );
    }

    /// An oracle that is *down* is the same nobody-decided as one that is slow,
    /// and escalates the same way.
    #[test]
    fn an_oracle_that_cannot_answer_asks_too() {
        let model = Stub::new(StubKind::Unavailable, "stub:model");
        let person = Stub::new(StubKind::Allows, "stub:person");
        let esc = EscalateOnTimeout::new(model.clone(), person.clone());
        let d = esc.decide(&request());
        assert_eq!(person.asks(), 1, "the person was asked");
        assert!(
            matches!(d.outcome, DecisionOutcome::Selected { .. }),
            "{d:?}"
        );
    }

    /// A model that answers decides, and nobody else is asked.
    #[test]
    fn a_model_that_answers_decides_and_nobody_else_is_asked() {
        let model = Stub::new(StubKind::Allows, "stub:model");
        let person = Stub::new(StubKind::Allows, "stub:person");
        let esc = EscalateOnTimeout::new(model.clone(), person.clone());
        let d = esc.decide(&request());
        assert_eq!(model.asks(), 1);
        assert_eq!(person.asks(), 0, "the person was never asked");
        assert!(d.by.contains("model"), "{}", d.by);
    }

    /// A cancellation is somebody stopping the turn, and asking over them would
    /// be input arriving from nowhere — the same rule the sink itself keeps.
    #[test]
    fn a_cancelled_decision_is_not_overridden_by_an_ask() {
        let model = Stub::new(StubKind::Cancels, "stub:model");
        let person = Stub::new(StubKind::Allows, "stub:person");
        let esc = EscalateOnTimeout::new(model.clone(), person.clone());
        let d = esc.decide(&request());
        assert_eq!(person.asks(), 0, "the person was never asked");
        assert_eq!(d.outcome, DecisionOutcome::Cancelled, "{d:?}");
    }

    /// Esc-Esc while a decision is open. The interrupt does not reach the worker —
    /// the worker is the thread waiting — so it reaches the sink, and the wait ends
    /// as a cancellation rather than sitting out five minutes.
    #[test]
    fn an_interrupt_ends_a_wait_the_worker_could_not_have_drained() {
        let hub = Hub::new("s");
        let answers = Arc::new(Answers::new());
        hub.set_answer_sink(answers.clone());
        let head = hub.attach("tui", "alice", Caps::default(), 0);
        let budget = Duration::from_secs(60);
        let adj = HeadAdjudicator::new(hub.clone(), answers).with_budget(budget);

        let hub2 = hub.clone();
        let head_id = head.head_id.clone();
        // The same rule as the answerer above: no deadline of its own. It stops
        // when the adjudicator it is racing has returned, and the assertions
        // below are what fail if it never got its chance.
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = stop.clone();
        let interrupter = std::thread::spawn(move || {
            while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
                if !hub2.snapshot().open_decisions.is_empty() {
                    hub2.submit(
                        &head_id,
                        "c1",
                        0,
                        letibot_sessionlog::hub::CommandKind::Interrupt {
                            reason: "operator pressed esc twice".into(),
                        },
                    );
                    return true;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            false
        });

        let started = Instant::now();
        let d = adj.decide(&request());
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let interrupted = interrupter.join().unwrap();
        assert!(interrupted, "nothing was ever open to interrupt: {d:?}");
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "it waited out the budget"
        );
        assert_eq!(d.outcome, DecisionOutcome::Cancelled, "{d:?}");
        assert!(d.basis.contains("esc twice"), "{}", d.basis);
    }

    /// An answer for a request nobody is waiting on is dropped, not held. A late
    /// allow applied to a call the gate has already reported as `not_run` would be
    /// the worst outcome available.
    #[test]
    fn an_answer_with_nothing_waiting_settles_nothing() {
        let answers = Answers::new();
        assert!(!answers.answer(
            "adj-nope",
            "alice",
            &Reply::Permission {
                option_id: "allow_once".into(),
                pattern: None,
                note: None
            }
        ));
    }

    /// The corpus wants the bytes a decider was **shown**, verbatim, not a later
    /// reconstruction — §4c. That rule is about the model and it is not only about
    /// the model: an operator's override is a labelled example of the same shape.
    #[test]
    fn what_the_person_was_shown_is_recorded_verbatim() {
        let hub = Hub::new("s");
        let answers = Arc::new(Answers::new());
        hub.set_answer_sink(answers.clone());
        hub.attach("tui", "alice", Caps::default(), 0);
        let adj = HeadAdjudicator::new(hub.clone(), answers).with_budget(Duration::from_millis(20));
        assert!(adj.last_brief().is_none(), "nothing has been shown yet");
        let req = request();
        adj.decide(&req);
        assert_eq!(adj.last_brief().as_deref(), Some(req.brief().as_str()));
    }

    /// **The decider is read, not guessed.** `close` used to derive the kind from
    /// the OUTCOME — every `Selected` was `human` whoever chose it — and put the
    /// whole `human:dead` into `identity` besides, so a head drew `by human
    /// human:dead` and an oracle's own admission drew `by human` over a model id.
    #[test]
    fn a_decider_is_split_into_its_kind_and_its_identity() {
        for (by, kind, identity) in [
            ("human:dead", "human", "dead"),
            ("model:qwen-3.8-27b", "model", "qwen-3.8-27b"),
            ("boundary:flow", "boundary", "flow"),
            ("gate:timeout", "gate", "timeout"),
        ] {
            let d = split_decider(by);
            assert_eq!(d.kind, kind, "{by}");
            assert_eq!(d.identity, identity, "{by}");
        }
        // No colon: all kind, no identity — and the head renders the kind alone
        // rather than a name that is not there.
        let d = split_decider("subagent");
        assert_eq!(d.kind, "subagent");
        assert!(d.identity.is_empty());
    }

    /// An oracle that decided alone is reported as a model, not as a person. The
    /// outcome is `Selected` either way, which is exactly why the outcome cannot
    /// be what names the decider.
    #[test]
    fn an_oracle_that_selected_is_not_reported_as_a_human() {
        let req = request();
        let d = AdjudicationDecision {
            request_id: req.id.clone(),
            outcome: DecisionOutcome::Selected {
                option_id: "allow_once".into(),
            },
            by: "model:glm-5.3-flash".into(),
            basis: "the operator asked for this in the same turn".into(),
            latency_ms: 40,
        };
        match close(&req, &d) {
            SessionEvent::DecisionAnswered { by, .. } => {
                assert_eq!(by.kind, "model");
                assert_eq!(by.identity, "glm-5.3-flash");
            }
            other => panic!("{other:?}"),
        }
    }

    /// **An answer that arrives before the wait starts is not lost.**
    ///
    /// `ask` creates the slot, publishes the question, and only then calls
    /// `wait`. A head answering inside that window sets `Slot::Answered`, and
    /// `wait` used to `insert` `Waiting` straight back over it — so the answer
    /// vanished and the call sat out its whole budget before reporting that
    /// nobody had decided. This is that window, made deterministic.
    #[test]
    fn an_answer_that_lands_before_the_wait_begins_is_still_the_answer() {
        let answers = Answers::new();
        // What `ask` does before it publishes.
        answers.lock().insert("adj-race".to_string(), Slot::Waiting);
        // The head, faster than the thread that is about to wait.
        assert!(answers.answer(
            "adj-race",
            "deadtrickster",
            &Reply::Permission {
                option_id: "allow_once".into(),
                pattern: None,
                note: None
            },
        ));
        // And now the wait begins. It must find the answer, not overwrite it —
        // and must not spend the budget doing so.
        let started = Instant::now();
        match answers.wait("adj-race", Duration::from_secs(10)) {
            Waited::Answered { reply, by } => {
                assert_eq!(by, "deadtrickster");
                assert!(matches!(
                    reply,
                    Reply::Permission { ref option_id, .. } if option_id == "allow_once"
                ));
            }
            Waited::TimedOut => panic!("the answer was overwritten and the wait timed out"),
            Waited::Cancelled { why } => panic!("cancelled instead of answered: {why}"),
        }
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "it returned immediately rather than waiting out the budget"
        );
    }
}

#[cfg(test)]
mod tell_tests {
    use super::*;

    /// **The operator's words reach the model.** `deny_and_tell` is labelled
    /// *"Deny, and tell the model why"* and the why went nowhere: the basis said
    /// only which button was pressed, so a refusal with a reason and one without
    /// were the same sentence.
    #[test]
    fn a_told_denial_carries_the_reason_and_an_untold_one_says_it_has_none() {
        let r = tests::request();
        let told = settle(
            &r,
            &Reply::Permission {
                option_id: "deny_and_tell".into(),
                pattern: None,
                note: Some("  use the scratch dir, not /tmp  ".into()),
            },
            "deadtrickster",
            10,
        );
        // Quoted and attributed: a sentence the model reads as the harness's own
        // reasoning is one it will argue with; one it reads as the operator's is
        // an instruction. Trimmed, because the words were typed at a prompt.
        assert!(
            told.basis.contains("\"use the scratch dir, not /tmp\""),
            "{}",
            told.basis
        );
        assert!(told.basis.contains("deadtrickster"), "{}", told.basis);

        // Nothing typed is a denial with no reason, and says so rather than
        // implying one was given — and names how to give one.
        for empty in [None, Some("   ".to_string())] {
            let untold = settle(
                &r,
                &Reply::Permission {
                    option_id: "deny_and_tell".into(),
                    pattern: None,
                    note: empty,
                },
                "deadtrickster",
                10,
            );
            assert!(
                untold.basis.contains("without saying why"),
                "{}",
                untold.basis
            );
            assert!(
                untold.basis.contains("deny_and_tell <why>"),
                "{}",
                untold.basis
            );
        }

        // Every other option is unchanged: this is about the one that asked.
        let plain = settle(
            &r,
            &Reply::Permission {
                option_id: "allow_once".into(),
                pattern: None,
                note: None,
            },
            "deadtrickster",
            10,
        );
        assert_eq!(plain.basis, "deadtrickster chose `allow_once` at the head");
    }
}
