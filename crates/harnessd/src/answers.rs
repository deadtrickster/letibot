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
    OnTimeout as WireOnTimeout, OptionKind, SessionEvent,
};
use letibot_sessionlog::hub::{AnswerSink, Hub, Reply};
use letibot_tools::adjudicate::{
    AdjudicationDecision, AdjudicationRequest, Adjudicator, DecisionOutcome, ModelAdvice, OnTimeout,
    OptionKind as ToolOptionKind, RequestKind,
};

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
        g.insert(req_id.to_string(), Slot::Waiting);
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
    pub fn ask(
        &self,
        hub: &Arc<Hub>,
        req: &AdjudicationRequest,
        budget: Duration,
    ) -> AdjudicationDecision {
        let started = Instant::now();
        let who = hub.deciding_heads();
        if who.is_empty() {
            return AdjudicationDecision::unavailable(
                req,
                "gate:no-head",
                "no head that can answer is attached to this session, so nothing was \
                 asked and nobody decided. The gate fails closed: nothing ran and \
                 nothing changed. Attach a head, or run this seat in a foreground \
                 terminal where the console adjudicator can reach a person.",
            );
        }

        let deadline_ms = unix_millis() + budget.as_millis() as u64;
        // The slot exists before the question does. See `wait`.
        let waited = {
            let mut g = self.lock();
            g.insert(req.id.clone(), Slot::Waiting);
            drop(g);
            hub.publish(pose(req, deadline_ms));
            // **The yellow card says why it is yellow.** The call is still
            // `Proposed` — `ToolStarted` fires only after the gate admits — so
            // this note is the only thing on the card that explains the wait,
            // and the operator's report was *"some tool calls stay yellow, no
            // idea what that means"*. The ask renders in the chrome; the note
            // renders on the call, where the eye already is.
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
                if let Reply::Permission { pattern: Some(p), .. } = &reply {
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
fn settle(req: &AdjudicationRequest, reply: &Reply, by: &str, latency_ms: u64) -> AdjudicationDecision {
    match reply {
        // The glob is not read here. `settle` builds the decision, and a pattern is
        // not part of one — it is what the answer said to do with the RULE, and only
        // `AllowAlways` writes one. `Answers::take_pattern` is where it is picked up.
        Reply::Permission { option_id, .. } => AdjudicationDecision {
            request_id: req.id.clone(),
            outcome: DecisionOutcome::Selected {
                option_id: option_id.clone(),
            },
            by: format!("human:{by}"),
            basis: format!("{by} chose `{option_id}` at the head"),
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

/// The `DecisionRequested` a head renders.
fn pose(req: &AdjudicationRequest, deadline_ms: u64) -> SessionEvent {
    SessionEvent::DecisionRequested {
        req_id: req.id.clone(),
        kind: match req.kind {
            RequestKind::Permission => "permission".into(),
            RequestKind::Question => "question".into(),
        },
        call_id: Some(req.call_id.clone()),
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
        options: req.options.iter().map(wire_option).collect(),
        // A permission has no plain-text choices. `ask_user_question` fills these,
        // and it goes through the same seam.
        choices: Vec::new(),
        because: req
            .boundary_facts
            .first()
            .cloned()
            .unwrap_or_default(),
        // Verbatim from the request the model was asked about, not re-derived. A
        // head that reconstructed the verdict would render a guess about what the
        // oracle said — `ModelBrief`'s rule, one layer out.
        advice: req.advice.as_ref().map(|a| WireAdvice {
            would: a.would.to_string(),
            by: a.by.clone(),
            basis: a.basis.clone(),
            cites: a.cites.clone(),
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
}

impl Adjudicator for HeadAdjudicator {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        *self.shown.lock().unwrap_or_else(|e| e.into_inner()) = Some(req.brief());
        // **Cleared before the ask, not after.** A pattern left over from the
        // previous answer would be applied to this one, which is a rule the operator
        // did not write for a call they were not looking at when they typed it.
        *self.pattern.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let d = self.answers.ask(&self.hub, req, self.budget);
        *self.pattern.lock().unwrap_or_else(|e| e.into_inner()) = self.answers.take_pattern(&req.id);
        d
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
        self.shown
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
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
        self.human.last_pattern().or_else(|| self.inner.last_pattern())
    }

    fn last_brief(&self) -> Option<String> {
        self.human.last_brief().or_else(|| self.inner.last_brief())
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

    fn request() -> AdjudicationRequest {
        use letibot_tools::adjudicate::{ActionClass, Tier};
        use letibot_tools::schema::Access;
        AdjudicationRequest {
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
            options: letibot_tools::adjudicate::permission_options("src/main.rs"),
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
                    hub2.submit(
                        &head_id,
                        "c1",
                        0,
                        letibot_sessionlog::hub::CommandKind::Answer {
                            req_id: d.req_id.clone(),
                            reply: Reply::Permission {
                                option_id: "allow_once".into(),
                                pattern: None,
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
        let adj =
            HeadAdjudicator::new(hub.clone(), answers).with_budget(Duration::from_millis(30));
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
        let adj =
            HeadAdjudicator::new(hub.clone(), answers).with_budget(Duration::from_millis(30));
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
            Arc::new(Stub { kind, name, asked: std::sync::Mutex::new(0) })
        }
        fn asks(&self) -> usize {
            *self.asked.lock().unwrap()
        }
    }

    impl Adjudicator for Stub {
        fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
            *self.asked.lock().unwrap() += 1;
            let (outcome, basis) = match self.kind {
                StubKind::TimesOut => (DecisionOutcome::Timeout, "the guard did not answer in time"),
                StubKind::Unavailable => (DecisionOutcome::Unavailable, "the guard is not reachable"),
                StubKind::Cancels => (DecisionOutcome::Cancelled, "the turn was stopped"),
                StubKind::Allows => (
                    DecisionOutcome::Selected { option_id: "allow_once".into() },
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
            DecisionOutcome::Selected { option_id: "allow_once".into() },
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
        assert!(matches!(d.outcome, DecisionOutcome::Selected { .. }), "{d:?}");
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
        assert!(started.elapsed() < Duration::from_secs(30), "it waited out the budget");
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
                pattern: None
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
        let adj =
            HeadAdjudicator::new(hub.clone(), answers).with_budget(Duration::from_millis(20));
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
}
