//! The half a head was missing: a way to answer a gate, and a way to answer a question.

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
// The question vocabulary, which is not the adjudication vocabulary: a permission is
// answered by an option id and a question by a person's words, and neither may settle the
// other. `Hub::submit` refuses that crossing at the door, and these types are why it can.
use letibot_tools::builtins::intent::{AskError, Headless, Question, QuestionAnswer, Questioner};

/// How long a person gets to answer before the gate fails closed. Not forever, because a
/// wait inside a tool call holds the daemon's only worker: a session whose operator walked
/// away would stop every other session on the box.
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

/// The rendezvous: one slot per open request, and the condvar the tool thread sleeps on.
/// Owned by the session and shared with the hub as its [`AnswerSink`]. It has no `&mut
/// self` method on purpose
pub struct Answers {
    slots: Mutex<HashMap<String, Slot>>,
    cv: Condvar,
    /// `(req_id, glob)` for the last answer that carried one, so
    /// [`HeadAdjudicator::last_pattern`] can pick it up after `ask` returns. Keyed by request
    /// so a pattern can only be applied to the call it was typed against.
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

    /// The glob that came with the answer to `req_id`, taken so it cannot be read twice. `None`
    /// for any other request: a pattern typed against one call must never reach the rule
    /// written for another.
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

    /// Open a slot and wait on it, up to `budget`. The slot is inserted before the caller
    /// publishes the question, so an answer that arrives while the publish is still fanning out
    /// has somewhere to land.
    fn wait(&self, req_id: &str, budget: Duration) -> Waited {
        let deadline = Instant::now() + budget;
        let mut g = self.lock();
        // `or_insert`, never `insert`: `ask` creates the slot before it publishes, so a head
        // that answers inside that window has already set `Answered`, and an unconditional
        // insert here would put `Waiting` back over it and drop the answer.
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
            // `Waiting` at the deadline, or gone entirely. Both are *nobody answered*.
            _ => Waited::TimedOut,
        }
    }

    /// Post a request to the heads and wait for one of them to settle it.
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
            // The yellow card says why it is yellow: the call is still `Proposed`, so this
            // note is the only thing on it that explains the wait. A subagent's note lands on
            // the root's hub where its card did, and the root has no such call to hang it on.
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
            // Nobody decided, so `by` is the gate and not the heads that were asked - the
            // heads are named in the basis. A `human:` here would file timeouts in the corpus
            // as a person's refusals.
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
        // The open decision is closed on every path, including the ones nobody answered
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
    /// Post a question to the heads and wait for one of them to answer it. The rendezvous is
    /// [`Answers::ask`]'s, because the deadlock is the same one. What differs is the payload
    /// each way: the card is posed with `kind
    pub fn ask_question(
        &self,
        hub: &Arc<Hub>,
        q: &Question,
        budget: Duration,
    ) -> Result<(QuestionAnswer, String), AskError> {
        // Read, not assumed, and asked before anything is posted - the same check
        // `Answers::ask` makes, for the same reason.
        let who = hub.deciding_heads();
        if who.is_empty() {
            return Err(AskError::NoHead(no_head_for_a_question()));
        }

        let req_id = question_id(&hub.session_id());
        let deadline_ms = unix_millis() + budget.as_millis() as u64;
        // The slot exists before the question does. See `Answers::wait`.
        let waited = {
            let mut g = self.lock();
            g.insert(req_id.clone(), Slot::Waiting);
            drop(g);
            hub.publish(pose_question(&req_id, q, deadline_ms));
            self.wait(&req_id, budget)
        };

        // The open card is closed on every path, including the ones nobody answered: an
        // unclosed `DecisionRequested` stays in the head's open set for the life of the
        // session, and the hub then accepts an answer to a question that is over.
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
            // The other vocabulary's reply. `Hub::submit` refuses it at the door, so it is
            // unreachable through the daemon; calling it an answer would be the defect the
            // two types exist to prevent.
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
                Err(AskError::Expired(
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

/// The wire's answer, as the tool's own shape. A field-by-field copy and not a `From`
/// neither crate may learn about the other, and the daemon is the place they meet.
fn to_tool_answer(a: &letibot_sessionlog::question::QuestionAnswer) -> QuestionAnswer {
    QuestionAnswer {
        option: a.option,
        note: a.note.clone(),
        free: a.free.clone(),
        abstain: a.abstain,
    }
}

/// The `DecisionRequested` a question is posed as. `choices` and not `options`
fn pose_question(req_id: &str, q: &Question, deadline_ms: u64) -> SessionEvent {
    SessionEvent::DecisionRequested {
        write_targets: Vec::new(),
        req_id: req_id.into(),
        kind: "question".into(),
        call_id: None,
        // A question is not a gate: nothing about a declaration asks for it, and a card that
        // drew an access class here would be claiming one.
        access: String::new(),
        summary: q.text.clone(),
        target: String::new(),
        detail: String::new(),
        options: Vec::new(),
        // A question has no adjudication ladder, so there is no rung to have left off.
        options_note: String::new(),
        // The model's default is on the card, so a person who comes back to it knows what
        // happens at the deadline: that choice is taken, not nothing.
        choices: q
            .options
            .iter()
            .enumerate()
            .map(|(i, o)| {
                if q.default == Some(i) {
                    format!("{o} (default)")
                } else {
                    o.clone()
                }
            })
            .collect(),
        because: match q.default {
            Some(_) if q.because.is_empty() => {
                "if nobody answers by the deadline, the default is taken".into()
            }
            Some(_) => format!(
                "{} · if nobody answers by the deadline, the default is taken",
                q.because
            ),
            None => q.because.clone(),
        },
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

/// Why nobody could be asked, in the words a question needs rather than a gate's.
/// [`no_head`] says the gate fails closed - nothing ran and nothing changed. A question
/// never had anything to run, so what a model needs told is the other half
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

/// One question and the answer it got, as the daemon's own questioner saw them.
#[derive(Debug, Clone)]
pub struct Asked {
    /// The question as it was posed, options and all - where a chosen option's text is read.
    pub question: Question,
    /// What the person said.
    pub answer: QuestionAnswer,
    /// Who said it: the same identity the gate records as `human:<who>`.
    pub by: String,
}

/// Where a questioner records the exchange it just carried. `None` is an empty slot. One
/// slot and not a queue: a session runs one tool call at a time on its worker, so a second
/// exchange cannot start before the first is written down.
pub type AskLog = Arc<Mutex<Option<Asked>>>;

/// The questioner a session with a head is given. It posts the question to the head and
/// waits on the answer frame the head already had, and holds nothing of its own
pub struct HeadQuestioner {
    hub: Arc<Hub>,
    answers: Arc<Answers>,
    budget: Duration,
    asked: Option<AskLog>,
    /// Where a taken default is written down: `AUTO-DECISIONS.md` at the top of the
    /// repository this is in, when it is in one. See [`auto_decision`].
    workspace: Option<std::path::PathBuf>,
}

impl HeadQuestioner {
    pub fn new(hub: Arc<Hub>, answers: Arc<Answers>) -> HeadQuestioner {
        HeadQuestioner {
            hub,
            answers,
            budget: ANSWER_BUDGET,
            asked: None,
            workspace: None,
        }
    }

    /// The session's workspace, where a taken default is logged.
    pub fn with_workspace(mut self, workspace: impl Into<std::path::PathBuf>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// Where the exchange is recorded for the session to write down. `None` - the default -
    /// is a questioner asked by a test that only wants the question asked.
    pub fn with_ask_log(mut self, asked: AskLog) -> Self {
        self.asked = Some(asked);
        self
    }
}

impl Questioner for HeadQuestioner {
    fn ask(&self, q: &Question) -> Result<(QuestionAnswer, String), AskError> {
        let got = self
            .answers
            .ask_question(&self.hub, q, q.timeout.unwrap_or(self.budget))?;
        // Recorded on the way past, and only an answer. Every `Err` arm of `ask_question` is
        // *nobody answered*, and a refusal is not a person's utterance. Whether the tool then
        // accepts the answer is the tool's own call, and the round loop asks the result.
        if let Some(log) = &self.asked {
            *log.lock().unwrap_or_else(|e| e.into_inner()) = Some(Asked {
                question: q.clone(),
                answer: got.0.clone(),
                by: got.1.clone(),
            });
        }
        Ok(got)
    }

    /// The same shape [`HeadAdjudicator::describe`] gives, and read for the same reason
    /// *reachable* is a fact about right now, and a line claiming a head for a session with none
    /// would be the disclosure claiming a safety property the session has not got.
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

    fn defaulted(&self, q: &Question, waited: Duration) -> Option<String> {
        let ws = self.workspace.as_deref()?;
        let said = auto_decision(ws, &self.hub.session_id(), q, waited, unix_millis());
        self.hub.publish(SessionEvent::Warning {
            code: "auto_decision".into(),
            detail: said.clone(),
            compaction: None,
        });
        Some(said)
    }
}

/// The log is never committed: a line in the repository's own `.gitignore`, added once, so
/// every clone and worktree of it ignores the file.
fn ignore_locally(top: &std::path::Path) {
    let ignore = top.join(".gitignore");
    let had = std::fs::read_to_string(&ignore).unwrap_or_default();
    if had
        .lines()
        .any(|l| matches!(l.trim(), "AUTO-DECISIONS.md" | "/AUTO-DECISIONS.md"))
    {
        return;
    }
    let sep = if had.is_empty() || had.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let _ = std::fs::write(&ignore, format!("{had}{sep}/AUTO-DECISIONS.md\n"));
}

/// A default taken, written down. In a repository: an entry appended to `AUTO-DECISIONS.md`
/// at its top, naming the commit, the question, every option and the one taken.
pub fn auto_decision(
    workspace: &std::path::Path,
    session: &str,
    q: &Question,
    waited: Duration,
    at_ms: u64,
) -> String {
    let Some(d) = q.default.and_then(|d| q.options.get(d)) else {
        return String::new();
    };
    let options = q
        .options
        .iter()
        .map(|o| format!("\"{o}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let summary = format!(
        "nobody answered \"{}\" within {}s, so the model's default \"{d}\" was taken (options: \
         {options})",
        q.text,
        waited.as_secs()
    );
    let git = crate::mergequeue::git::git;
    let Ok(top) = crate::mergequeue::git::repo_root(workspace) else {
        return format!("{summary} — not a repository here, so this note is the only record");
    };
    let commit = git(&top, &["rev-parse", "--short", "HEAD"])
        .map(|c| c.trim().to_string())
        .unwrap_or_else(|_| "no commit".into());
    let branch = git(&top, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|b| b.trim().to_string())
        .unwrap_or_default();
    let repo = top
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| top.display().to_string());
    let file = top.join("AUTO-DECISIONS.md");
    ignore_locally(&top);
    let mut entry = String::new();
    if !file.exists() {
        entry.push_str(
            "# Auto-decisions\n\nQuestions nobody answered in time, and the default the model \
             took. Review them.\n",
        );
    }
    entry.push_str(&format!(
        "\n## {} · {session}\n\n- repo: {repo} @ {commit} ({branch})\n- question: {}\n",
        crate::subagent_record::utc(at_ms),
        q.text
    ));
    if !q.because.is_empty() {
        entry.push_str(&format!("- because: {}\n", q.because));
    }
    entry.push_str("- options:\n");
    for (i, o) in q.options.iter().enumerate() {
        let mark = if q.default == Some(i) {
            " ← taken"
        } else {
            ""
        };
        entry.push_str(&format!("  - {o}{mark}\n"));
    }
    entry.push_str(&format!(
        "- default taken: {d} — nobody answered within {}s\n",
        waited.as_secs()
    ));
    use std::io::Write as _;
    let wrote = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .and_then(|mut f| f.write_all(entry.as_bytes()));
    match wrote {
        Ok(()) => format!(
            "{summary} — logged in {} at {repo} @ {commit}",
            file.display()
        ),
        Err(e) => format!(
            "{summary} — at {repo} @ {commit}; {} could not be written ({e})",
            file.display()
        ),
    }
}

/// The questioner the intent wiring is given, whose head arrives with the gate. The order
/// is forced
pub struct SlotQuestioner(pub Arc<Mutex<Option<Arc<dyn Questioner>>>>);

impl Questioner for SlotQuestioner {
    fn ask(&self, q: &Question) -> Result<(QuestionAnswer, String), AskError> {
        let head = self.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match head {
            Some(h) => h.ask(q),
            None => Headless.ask(q),
        }
    }

    fn defaulted(&self, q: &Question, waited: Duration) -> Option<String> {
        let head = self.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        head.and_then(|h| h.defaulted(q, waited))
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
            // Nothing is waiting. Dropped rather than held: a reply to a question asked ten
            // minutes ago is input arriving from nowhere, and applying a late allow to a call
            // already reported as `not_run` is the worst outcome available here.
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

/// Turn a head's reply into the decision the gate reads. A permission names an option the
/// request offered; anything else is `Unavailable`, which [`letibot_tools::AskAdjudicator`]
/// checks again.
fn settle(
    req: &AdjudicationRequest,
    reply: &Reply,
    by: &str,
    latency_ms: u64,
) -> AdjudicationDecision {
    match reply {
        // The glob is not read here: a pattern is not part of a decision, only `AllowAlways`
        // writes one, and `Answers::take_pattern` is where it is picked up.
        Reply::Permission {
            option_id, note, ..
        } => AdjudicationDecision {
            request_id: req.id.clone(),
            outcome: DecisionOutcome::Selected {
                option_id: option_id.clone(),
            },
            by: format!("human:{by}"),
            // The operator's own words reach the model. `deny_and_tell` is labelled *"Deny and tell
            // the model why"*, and the why went nowhere: the basis said only which button was pressed.
            basis: match note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                Some(why) => format!("{by} chose `{option_id}` at the head: \"{why}\""),
                // A `deny_and_tell` with nothing typed is a denial with no reason given, and
                // says so rather than implying one was.
                None if option_id == "deny_and_tell" => format!(
                    "{by} chose `{option_id}` at the head, without saying why — the \
                     reason is typed after the option id, as `deny_and_tell <why>`"
                ),
                None => format!("{by} chose `{option_id}` at the head"),
            },
            latency_ms,
        },
        // Not the person's decision: nobody chose an option. `by` says who decided, and here
        // nothing did.
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

/// The sentence a gate gets when nobody at the head can answer. A fact, not a policy - and
/// for a subagent a different fact, told differently
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

/// The `DecisionRequested` a head renders. `subagent` is carried straight onto the card: a
/// child's gate posts its card to the tree's root, and a card that arrived there unlabelled
/// would be answered for the wrong thing.
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
        // The tool's declared access, so the card can say why it is asking: the headline names
        // the declaration and `detail` is layer A's reading of the action.
        access: req.class.access.as_str().to_string(),
        // The question, and nothing else, on the line the operator reads first. These three
        // used to be one string with layer A's reading joined to it by an em dash, and the
        // command ended up missing from the middle of four registers.
        summary: req.summary.clone(),
        target: req.target.clone(),
        detail: req.baseline.clone(),
        // What the call writes, taken off the request's own `reading` so the card and layer A's
        // line cannot disagree. The model is not told - `brief` is what it sees, and it wrote
        // the script - so this is the operator's, which is the cheaper claim besides.
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
        // What the ladder left off, and why, verbatim from the gate: the rung and the sentence
        // explaining its absence are one fact, and a head that composed the sentence would be
        // re-deriving which refusal applied.
        options_note: req.options_note.clone().unwrap_or_default(),
        // A permission has no plain-text choices. `ask_user_question` fills these
        // and it goes through the same seam.
        choices: Vec::new(),
        because: req.boundary_facts.first().cloned().unwrap_or_default(),
        // Verbatim from the request the model was asked about, not re-derived: a head that
        // reconstructed the verdict would render a guess about what the oracle said.
        advice: req.advice.as_ref().map(|a| WireAdvice {
            consulted: a.consulted,
            would: a.would.to_string(),
            by: a.by.clone(),
            basis: a.basis.clone(),
            cites: a.cites.clone(),
            // `as_str` and not the debug name, because this is the same string the corpus
            // column `oracle_reading` stores, and one vocabulary with two destinations drifts.
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
        // `Escalate` and `Unavailable` are not outcomes the wire has, and `Cancelled` is the
        // honest mapping: the request is off the screen and nobody selected anything. The
        // `basis` carries which it was.
        _ => WireOutcome::Cancelled,
    };
    SessionEvent::DecisionAnswered {
        req_id: req.id.clone(),
        outcome,
        // The decider says who it is and this does not guess: `d.by` is already `kind:identity`
        // so it is split rather than re-derived.
        by: split_decider(&d.by),
        basis: d.basis.clone(),
        late: false,
    }
}

/// `kind:identity` into its two halves. A `by` with no colon is all kind and no identity, and
/// an empty identity renders as nothing rather than as an empty quoted name.
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

/// The adjudicator that asks whoever is looking at the session. It holds the call the way
/// `ConsoleAdjudicator` does - a pending decision is a visible row not a held connection
pub struct HeadAdjudicator {
    hub: Arc<Hub>,
    answers: Arc<Answers>,
    budget: Duration,
    /// What the last request rendered to, for [`Adjudicator::last_brief`] and so for the corpus
    /// row's `shown`: a person is shown the brief, and recording it keeps the trail honest for
    /// a human adjudicator as well as a model one.
    shown: Mutex<Option<String>>,
    /// The glob the operator typed with the last answer, for [`Adjudicator::last_pattern`].
    /// `None` when they typed none, which means *use the pattern derived from the call*.
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

    /// The ask itself, with the caller's own attribution. `None` is this session's own call
    /// `Some` is a subagent's, posted to the session whose head this is.
    pub fn decide_as(
        &self,
        req: &AdjudicationRequest,
        subagent: Option<&SubagentAsk>,
    ) -> AdjudicationDecision {
        *self.shown.lock().unwrap_or_else(|e| e.into_inner()) = Some(req.brief());
        // Cleared before the ask, not after: a pattern left over from the previous answer
        // would be applied to this one, which is a rule the operator did not write for a call
        // they were not looking at when they typed it.
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
            // Read, not assumed: a banner that said "a head decides" for a session with no head
            // would be the disclosure claiming a safety property the session has not got.
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

/// A subagent's gate, asking the head at the root of its tree.
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

    /// No head-wired adjudicator in this tree, said as the fact it is. `by` is `gate:no-head`
    /// the word the root's own no-head refusal uses, because it is the same fact one level
    /// over.
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

    /// Never starts with `none`: that prefix is the harness's own signal for *nobody reachable
    /// refuse to open a gated session*, and a subagent does have a decider - the ruleset it
    /// inherited. What reaches this adjudicator is only what that ruleset left as `ask`.
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

/// The model decides; when it cannot, the person does. This forwards to the model and, when
/// the answer means nobody decided - `Timeout` or `Unavailable` - asks the person instead.
/// A `Cancelled` decision is not forwarded
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
                // The record keeps both facts: the person decided, and they were asked because
                // the guard did not answer. Only the first would read as a person overruling a
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
        // Whichever of the two answered: a pattern typed with a person's answer belongs to the
        // person, and a model that minted one reported its own.
        self.human
            .last_pattern()
            .or_else(|| self.inner.last_pattern())
    }

    fn last_brief(&self) -> Option<String> {
        self.human.last_brief().or_else(|| self.inner.last_brief())
    }

    /// The model's bytes, not the person's: the corpus column is the input an ORACLE was given
    /// and the human's half has no such bytes. Where both answered, the model's is the exchange
    /// a reader came to check.
    fn last_reply(&self) -> Option<String> {
        self.inner.last_reply().or_else(|| self.human.last_reply())
    }

    fn last_advice(&self) -> Option<ModelAdvice> {
        // The model's verdict travels to the corpus row even when it lost the
        // decision to the timeout - that row is the disagreement signal.
        self.inner.last_advice()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tell_tests {
    use super::*;

    /// The operator's words reach the model: `deny_and_tell` is labelled *"Deny, and tell the
    /// model why"* and the why went nowhere, so a refusal with a reason and one without were
    /// the same sentence.
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
        // Quoted and attributed: a sentence the model reads as the harness's own reasoning is
        // one it will argue with; one it reads as the operator's is an instruction. Trimmed
        // because the words were typed at a prompt.
        assert!(
            told.basis.contains("\"use the scratch dir, not /tmp\""),
            "{}",
            told.basis
        );
        assert!(told.basis.contains("deadtrickster"), "{}", told.basis);

        // Nothing typed is a denial with no reason, and says so rather than implying one was
        // given - and names how to give one.
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
