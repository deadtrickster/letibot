//! The intent ledger: what the turn *said* it would do, what actually *ran*, and
//! the difference between them.
//!
//! This is `docs/closed-loop.md` §2's **encoder** and **error signal**, which are
//! the two parts the four surveyed harnesses with a todo tool do not have. Their
//! todo list is a note the model writes to itself; nothing ever reads it back
//! against the tool log. T21.3 is exactly the failure that leaves:
//!
//! > when model says ill start that and by end of the turn forgets and does not
//! > start anything
//!
//! Intent was commanded, effect was never measured, and the drift is silent.
//!
//! # The three quantities, which are not one thing
//!
//! | | what it is | where it comes from |
//! |---|---|---|
//! | **intent** | an item the model declared | [`IntentLedger::declare`], via the `todo` tool or a plan |
//! | **effect** | a tool call that ran and returned `Ok` | [`IntentSink`], decorating the runtime's own event sink |
//! | **diff** | intent this turn with no effect this turn | [`IntentLedger::reconcile`] |
//!
//! An **attempt** is a fourth: a tool call that ran and did not return `Ok`. It is
//! counted separately, because "the model tried and the tool abstained" and "the
//! model did nothing" are different facts and reporting them identically is the
//! bug one layer down.
//!
//! # The encoder can be missing, and that is its own state
//!
//! §2.5 of `docs/tool-design-brief.md` says a gate that answers "allowed" because
//! nothing is wired is worse than no gate. The mirror holds here and is easier to
//! ship by accident: a ledger with no [`IntentSink`] attached sees zero effects,
//! so **every** completion looks unverified and the diff screams drift at a
//! session that was working perfectly.
//!
//! So the ledger knows whether its encoder is attached ([`IntentLedger::encoder`]),
//! and an unverifiable completion is [`Verification::NoEncoder`] — a distinct
//! state from [`Verification::NoEffect`], reported as a defect in the *harness*
//! rather than as a finding about the model. A dead encoder never travels as a
//! measurement.

use std::collections::BTreeMap;
use std::sync::Mutex;

use letibot_transcript::ToolOutcome;

use crate::events::{ToolEvent, ToolEventSink};

/// The tools that record intent rather than produce effect.
///
/// A `todo` call is not evidence that the todo was done, and letting it count
/// would make the whole mechanism self-satisfying: the model could close every
/// item by announcing that it had. This list is the reason the diff means
/// anything.
pub const INTENT_TOOLS: &[&str] = &[
    "todo",
    "goal",
    "enter_plan_mode",
    "exit_plan_mode",
    "ask_user_question",
];

/// How a completion was checked. **Not a boolean**, because the third case is the
/// harness's fault and must not be reported as the model's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verification {
    /// At least one tool call returned `Ok` while this item was in progress, and
    /// these are their call ids.
    ByEffect { calls: Vec<String> },
    /// The item was marked complete and nothing ran. **This is not complete.**
    NoEffect,
    /// No effect log is attached to this session, so completion could not be
    /// checked either way. Also not complete — and the reason is ours.
    NoEncoder,
}

impl Verification {
    /// Only the first one. F5: a component's "I did not do this" is never reported
    /// upward as success, and neither is "nobody looked".
    pub fn is_complete(&self) -> bool {
        matches!(self, Verification::ByEffect { .. })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Verification::ByEffect { .. } => "done",
            Verification::NoEffect => "claimed (nothing ran)",
            Verification::NoEncoder => "unverifiable (no effect log attached)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Pending,
    InProgress,
    /// Settled, with how that was checked. Only [`Verification::ByEffect`] counts
    /// as complete anywhere in this module.
    Settled(Verification),
    Blocked { why: String },
}

impl Status {
    pub fn word(&self) -> String {
        match self {
            Status::Pending => "pending".into(),
            Status::InProgress => "in progress".into(),
            Status::Settled(v) => v.as_str().into(),
            Status::Blocked { why } => format!("blocked: {why}"),
        }
    }
}

/// Where an intent came from. A plan step and a hand-written todo are both
/// intents, and a prose commitment is a *candidate* — see [`commitments`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Todo,
    Plan,
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Todo => "todo",
            Source::Plan => "plan",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    pub id: u32,
    pub text: String,
    pub status: Status,
    pub source: Source,
    pub declared_turn: String,
    pub started_turn: Option<String>,
    pub settled_turn: Option<String>,
    /// The row id on the shared board, when one is mounted.
    ///
    /// The ledger is not a *second list*: when a board is mounted the row is the
    /// record and this is a handle to it. What stays here either way is the part
    /// the board cannot know — which of **this session's** tool calls ran against
    /// the item, which is what [`Verification`] is computed from.
    pub row: Option<String>,
}

/// One tool call, as the encoder saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effect {
    pub turn_id: String,
    pub call_id: String,
    pub name: String,
    /// The call returned `Ok`. An abstention, a failure and a `not_run` are all
    /// `false` here and are counted as *attempts*, which is a different number.
    pub ok: bool,
}

/// The goal, and the sentence that says how anybody would know it was met.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Goal {
    pub text: String,
    /// **Required.** A goal with no acceptance criterion cannot be checked, and a
    /// goal that cannot be checked is a wish. [`IntentLedger::set_goal`] refuses
    /// without one.
    pub acceptance: String,
    pub set_turn: String,
    pub met: Option<Verification>,
}

/// The board, with its denominators.
///
/// *"3 todos" is not a status.* Every field here is printed every time, including
/// the zeroes, because a category that vanishes when empty is a category the
/// reader stops looking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Board {
    pub total: usize,
    pub done: usize,
    pub claimed: usize,
    pub unverifiable: usize,
    pub in_progress: usize,
    pub pending: usize,
    pub blocked: usize,
}

impl std::fmt::Display for Board {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.total == 0 {
            return write!(f, "0 items — nothing has been declared in this session");
        }
        write!(
            f,
            "{} of {} complete",
            self.done, self.total
        )?;
        write!(
            f,
            " — {} in progress, {} pending, {} blocked",
            self.in_progress, self.pending, self.blocked
        )?;
        if self.claimed > 0 {
            write!(
                f,
                ", {} marked complete with NO tool call behind them (not counted as complete)",
                self.claimed
            )?;
        }
        if self.unverifiable > 0 {
            write!(
                f,
                ", {} unverifiable because no effect log is attached (a harness defect, not a model one)",
                self.unverifiable
            )?;
        }
        Ok(())
    }
}

/// One thing the diff found. Never a failure — T21.3 asks for steering, and a
/// wrong steer costs a few tokens where a wrong failure costs the turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// Declared or started in this turn, and the turn ran no tool at all. This is
    /// T21.3 verbatim.
    AnnouncedNotStarted { id: u32, text: String },
    /// Marked complete with nothing measured behind it.
    CompletedWithoutEffect { id: u32, text: String },
    /// In progress for more than one turn with no effect in any of them.
    Stalled {
        id: u32,
        text: String,
        since_turn: String,
    },
    /// A future-tense first-person sentence in the assistant's own text with no
    /// matching tool call. **A heuristic**, and labelled as one wherever it is
    /// printed — see [`commitments`].
    ProseCommitment { sentence: String },
    /// The encoder is not wired. A finding about the harness, emitted first so it
    /// is never mistaken for a finding about the model.
    NoEncoder,
}

impl Finding {
    pub fn line(&self) -> String {
        match self {
            Finding::AnnouncedNotStarted { id, text } => format!(
                "#{id} \"{text}\" was declared this turn and no tool ran in this turn — \
                 start it, or say why not"
            ),
            Finding::CompletedWithoutEffect { id, text } => format!(
                "#{id} \"{text}\" was marked complete and no tool call succeeded while it \
                 was in progress; it is recorded as claimed, not complete"
            ),
            Finding::Stalled {
                id,
                text,
                since_turn,
            } => format!(
                "#{id} \"{text}\" has been in progress since turn {since_turn} with nothing \
                 measured against it"
            ),
            Finding::ProseCommitment { sentence } => format!(
                "you said \"{sentence}\" and no tool ran in this turn — do it, or say why not \
                 (this line is a text heuristic and can be wrong)"
            ),
            Finding::NoEncoder => "no effect log is attached to this session, so nothing in \
                 this diff was measured against what actually ran; completion cannot be \
                 checked until an `IntentSink` is wired"
                .into(),
        }
    }
}

/// What one turn's reconciliation found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    pub turn_id: String,
    /// Intents declared in this turn.
    pub declared: usize,
    /// Tool calls in this turn that returned `Ok`.
    pub effects: usize,
    /// Tool calls in this turn that ran and did not return `Ok`.
    pub attempts: usize,
    pub board: Board,
    pub findings: Vec<Finding>,
}

/// The most findings one steering message carries.
///
/// `docs/tool-design-brief.md` §5: a miss must not produce an unbounded reply. A
/// turn that declared forty items and did none of them needs to be told once, not
/// forty times.
const MAX_FINDINGS: usize = 6;

impl Reconciliation {
    /// The steering text to inject at the step boundary, or `None` when there is
    /// nothing to say.
    ///
    /// `None` is the common case and matters: a mechanism that speaks every turn
    /// is a mechanism that gets ignored by the fourth turn.
    pub fn steering(&self) -> Option<String> {
        if self.findings.is_empty() {
            return None;
        }
        let mut out = String::from("[intent check] ");
        out.push_str(&format!(
            "this turn declared {} item(s); {} tool call(s) succeeded and {} ran without \
             producing a result.\n",
            self.declared, self.effects, self.attempts
        ));
        for f in self.findings.iter().take(MAX_FINDINGS) {
            out.push_str("  - ");
            out.push_str(&f.line());
            out.push('\n');
        }
        if self.findings.len() > MAX_FINDINGS {
            out.push_str(&format!(
                "  … and {} more; call `todo` with `op: \"list\"` for the rest\n",
                self.findings.len() - MAX_FINDINGS
            ));
        }
        out.push_str(&format!("board: {}\n", self.board));
        Some(out)
    }
}

#[derive(Default)]
struct Inner {
    items: Vec<Intent>,
    effects: Vec<Effect>,
    next_id: u32,
    goal: Option<Goal>,
    encoder: bool,
    /// Turns already reconciled, so a second call is idempotent rather than
    /// double-counting.
    reconciled: BTreeMap<String, ()>,
}

/// The session's intent state. Shared by `Arc` between the tools that write it and
/// whatever runs at the turn boundary and reads it.
#[derive(Default)]
pub struct IntentLedger {
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for IntentLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntentLedger")
            .field("board", &self.board())
            .field("encoder", &self.encoder())
            .finish()
    }
}

impl IntentLedger {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned lock here means a tool panicked mid-update. The ledger is
        // session state and not a safety boundary, so recovering the guard is
        // right: losing the whole board because one call panicked would turn a
        // tool bug into a lost session.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether an effect log is attached. See the module docs: this is the
    /// difference between "nothing ran" and "nobody looked".
    pub fn encoder(&self) -> bool {
        self.lock().encoder
    }

    /// Called by [`IntentSink::new`]. Not public API for anybody else: an encoder
    /// that can be declared without being wired is the failure this flag exists to
    /// make visible.
    pub(crate) fn attach_encoder(&self) {
        self.lock().encoder = true;
    }

    // -- intent -----------------------------------------------------------

    /// Record an intent. Returns its id.
    pub fn declare(&self, turn_id: &str, text: &str, source: Source) -> u32 {
        self.declare_row(turn_id, text, source, None)
    }

    /// As [`IntentLedger::declare`], carrying the shared board's row id when one
    /// is mounted.
    pub fn declare_row(
        &self,
        turn_id: &str,
        text: &str,
        source: Source,
        row: Option<String>,
    ) -> u32 {
        let mut g = self.lock();
        g.next_id += 1;
        let id = g.next_id;
        g.items.push(Intent {
            id,
            text: text.to_string(),
            status: Status::Pending,
            source,
            declared_turn: turn_id.to_string(),
            started_turn: None,
            settled_turn: None,
            row,
        });
        id
    }

    pub fn get(&self, id: u32) -> Option<Intent> {
        self.lock().items.iter().find(|i| i.id == id).cloned()
    }

    pub fn items(&self) -> Vec<Intent> {
        self.lock().items.clone()
    }

    /// The ledger entry for a row on the shared board, if this session has one.
    pub fn by_row(&self, row: &str) -> Option<Intent> {
        self.lock()
            .items
            .iter()
            .find(|i| i.row.as_deref() == Some(row))
            .cloned()
    }

    /// Make sure a board row has a ledger entry, so the intent/effect diff covers
    /// it too. Returns the local id.
    ///
    /// This is what makes a row **another seat filed** subject to the same check
    /// as one this session wrote: a row claimed and not worked is visible to the
    /// whole fleet, and that is the case where the diff is worth the most.
    pub fn adopt(&self, turn_id: &str, row: &str, title: &str) -> u32 {
        if let Some(i) = self.by_row(row) {
            return i.id;
        }
        self.declare_row(turn_id, title, Source::Todo, Some(row.to_string()))
    }

    /// Resolve what the model typed into a local id: a bare number is a local id,
    /// anything else is a row id on the shared board.
    pub fn resolve(&self, arg: &str) -> Option<u32> {
        let g = self.lock();
        if let Ok(n) = arg.trim().trim_start_matches('#').parse::<u32>()
            && g.items.iter().any(|i| i.id == n)
        {
            return Some(n);
        }
        g.items
            .iter()
            .find(|i| i.row.as_deref() == Some(arg.trim()))
            .map(|i| i.id)
    }

    /// The ids a caller may refer to, for a miss report.
    pub fn known(&self) -> Vec<String> {
        self.lock()
            .items
            .iter()
            .map(|i| match &i.row {
                Some(r) => format!("#{} (row {r})", i.id),
                None => format!("#{}", i.id),
            })
            .collect()
    }

    pub fn start(&self, turn_id: &str, id: u32) -> Result<Intent, LedgerError> {
        let mut g = self.lock();
        let Some(i) = g.items.iter_mut().find(|i| i.id == id) else {
            let known: Vec<u32> = g.items.iter().map(|i| i.id).collect();
            return Err(LedgerError::NoSuchItem { id, known });
        };
        i.status = Status::InProgress;
        i.started_turn = Some(turn_id.to_string());
        i.settled_turn = None;
        Ok(i.clone())
    }

    /// Settle an item as complete — **as checked, not as claimed.**
    ///
    /// The check is the whole point and it is done here rather than trusted from
    /// the argument: at least one tool call must have returned `Ok` since the item
    /// was started (or, if it was never started, in the turn that is settling it).
    /// A `todo` call cannot be its own evidence — see [`INTENT_TOOLS`].
    pub fn complete(&self, turn_id: &str, id: u32) -> Result<Intent, LedgerError> {
        let mut g = self.lock();
        if !g.items.iter().any(|i| i.id == id) {
            let known: Vec<u32> = g.items.iter().map(|i| i.id).collect();
            return Err(LedgerError::NoSuchItem { id, known });
        }
        let verification = if !g.encoder {
            Verification::NoEncoder
        } else {
            let since = g
                .items
                .iter()
                .find(|i| i.id == id)
                .and_then(|i| i.started_turn.clone())
                .unwrap_or_else(|| turn_id.to_string());
            let calls = evidence_since(&g.effects, &since);
            if calls.is_empty() {
                Verification::NoEffect
            } else {
                Verification::ByEffect { calls }
            }
        };
        let i = g
            .items
            .iter_mut()
            .find(|i| i.id == id)
            .expect("presence checked above");
        i.status = Status::Settled(verification);
        i.settled_turn = Some(turn_id.to_string());
        Ok(i.clone())
    }

    pub fn block(&self, turn_id: &str, id: u32, why: &str) -> Result<Intent, LedgerError> {
        let mut g = self.lock();
        let Some(i) = g.items.iter_mut().find(|i| i.id == id) else {
            let known: Vec<u32> = g.items.iter().map(|i| i.id).collect();
            return Err(LedgerError::NoSuchItem { id, known });
        };
        i.status = Status::Blocked {
            why: why.to_string(),
        };
        i.settled_turn = Some(turn_id.to_string());
        Ok(i.clone())
    }

    // -- goal -------------------------------------------------------------

    pub fn goal(&self) -> Option<Goal> {
        self.lock().goal.clone()
    }

    pub fn set_goal(&self, turn_id: &str, text: &str, acceptance: &str) -> Goal {
        let g = Goal {
            text: text.to_string(),
            acceptance: acceptance.to_string(),
            set_turn: turn_id.to_string(),
            met: None,
        };
        self.lock().goal = Some(g.clone());
        g
    }

    /// Mark the goal met, checked the same way an item is.
    pub fn meet_goal(&self, turn_id: &str) -> Result<Goal, LedgerError> {
        let mut g = self.lock();
        if g.goal.is_none() {
            return Err(LedgerError::NoGoal);
        }
        let verification = if !g.encoder {
            Verification::NoEncoder
        } else {
            let since = g
                .goal
                .as_ref()
                .map(|x| x.set_turn.clone())
                .unwrap_or_else(|| turn_id.to_string());
            let calls = evidence_since(&g.effects, &since);
            if calls.is_empty() {
                Verification::NoEffect
            } else {
                Verification::ByEffect { calls }
            }
        };
        let goal = g.goal.as_mut().expect("presence checked above");
        goal.met = Some(verification);
        Ok(goal.clone())
    }

    // -- effect -----------------------------------------------------------

    /// Record what actually ran. Called by [`IntentSink`], never by a tool.
    pub fn record_effect(&self, turn_id: &str, call_id: &str, name: &str, outcome: &ToolOutcome) {
        if INTENT_TOOLS.contains(&name) {
            return;
        }
        let mut g = self.lock();
        g.effects.push(Effect {
            turn_id: turn_id.to_string(),
            call_id: call_id.to_string(),
            name: name.to_string(),
            ok: matches!(outcome, ToolOutcome::Ok),
        });
    }

    pub fn effects(&self) -> Vec<Effect> {
        self.lock().effects.clone()
    }

    // -- the diff ---------------------------------------------------------

    pub fn board(&self) -> Board {
        board_of(&self.lock().items)
    }

    /// The error signal. Diff what this turn declared against what this turn ran.
    ///
    /// `assistant_text` is the turn's own prose, for the [`commitments`]
    /// heuristic; pass `""` to use only the tool-declared half, which is the
    /// deterministic one.
    ///
    /// Idempotent per turn: calling it twice returns the same findings and does
    /// not double-count, because a turn boundary that fires twice is a thing that
    /// happens and a diff that grows each time it is read is not a measurement.
    pub fn reconcile(&self, turn_id: &str, assistant_text: &str) -> Reconciliation {
        let mut g = self.lock();
        g.reconciled.insert(turn_id.to_string(), ());

        let effects: usize = g
            .effects
            .iter()
            .filter(|e| e.turn_id == turn_id && e.ok)
            .count();
        let attempts: usize = g
            .effects
            .iter()
            .filter(|e| e.turn_id == turn_id && !e.ok)
            .count();
        let ran_anything = effects + attempts > 0;

        let mut findings = Vec::new();
        if !g.encoder {
            findings.push(Finding::NoEncoder);
        }

        let declared = g
            .items
            .iter()
            .filter(|i| i.declared_turn == turn_id)
            .count();

        for i in &g.items {
            // T21.3: declared or started here, and the turn ran nothing at all.
            let touched_here =
                i.declared_turn == turn_id || i.started_turn.as_deref() == Some(turn_id);
            if touched_here
                && !ran_anything
                && !matches!(i.status, Status::Settled(_) | Status::Blocked { .. })
            {
                findings.push(Finding::AnnouncedNotStarted {
                    id: i.id,
                    text: i.text.clone(),
                });
                continue;
            }
            if i.settled_turn.as_deref() == Some(turn_id)
                && matches!(i.status, Status::Settled(Verification::NoEffect))
            {
                findings.push(Finding::CompletedWithoutEffect {
                    id: i.id,
                    text: i.text.clone(),
                });
                continue;
            }
            if matches!(i.status, Status::InProgress)
                && let Some(since) = &i.started_turn
                && since != turn_id
                && !g.effects.iter().any(|e| e.ok && &e.turn_id >= since)
            {
                findings.push(Finding::Stalled {
                    id: i.id,
                    text: i.text.clone(),
                    since_turn: since.clone(),
                });
            }
        }

        // The prose half. Only when the turn ran nothing: a turn that made a
        // commitment and then ran six tools is not the failure this catches, and
        // firing there is how the heuristic's false positives start costing more
        // than the true ones save.
        if !ran_anything {
            for s in commitments(assistant_text) {
                findings.push(Finding::ProseCommitment { sentence: s });
            }
        }

        Reconciliation {
            turn_id: turn_id.to_string(),
            declared,
            effects,
            attempts,
            board: board_of(&g.items),
            findings,
        }
    }
}

fn board_of(items: &[Intent]) -> Board {
    let mut b = Board {
        total: items.len(),
        ..Board::default()
    };
    for i in items {
        match &i.status {
            Status::Pending => b.pending += 1,
            Status::InProgress => b.in_progress += 1,
            Status::Blocked { .. } => b.blocked += 1,
            Status::Settled(Verification::ByEffect { .. }) => b.done += 1,
            Status::Settled(Verification::NoEffect) => b.claimed += 1,
            Status::Settled(Verification::NoEncoder) => b.unverifiable += 1,
        }
    }
    b
}

/// Call ids of successful calls in `since` or any later turn.
///
/// Turn ids sort lexically in creation order in this harness (`turn_1`,
/// `turn_2`, …); where they do not, this degrades to "calls in the same turn",
/// which is the conservative direction — it can only *withhold* evidence, never
/// invent it.
fn evidence_since(effects: &[Effect], since: &str) -> Vec<String> {
    effects
        .iter()
        .filter(|e| e.ok && e.turn_id.as_str() >= since)
        .map(|e| e.call_id.clone())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerError {
    NoSuchItem { id: u32, known: Vec<u32> },
    NoGoal,
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::NoSuchItem { id, known } => {
                if known.is_empty() {
                    write!(
                        f,
                        "there is no item #{id}, and this session has no items at all — \
                         add one with `op: \"add\"` before referring to it"
                    )
                } else {
                    write!(
                        f,
                        "there is no item #{id}; this session has {}: {}",
                        known.len(),
                        known
                            .iter()
                            .map(|i| format!("#{i}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            LedgerError::NoGoal => write!(
                f,
                "no goal has been set in this session, so there is nothing to mark met; \
                 set one with `op: \"set\"`, `goal` and `acceptance`"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// The encoder.
// ---------------------------------------------------------------------------

/// A [`ToolEventSink`] that feeds the ledger on its way to the real sink.
///
/// This is the encoder, and it is a decorator rather than a hook so that wiring it
/// is one line at the call site and forgetting it is *visible* rather than silent
/// ([`IntentLedger::encoder`]).
///
/// `ToolEvent::Finished` does not carry the tool's name — only `Started` does — so
/// this keeps the correlation itself rather than asking for the event shape to
/// change.
pub struct IntentSink<S> {
    ledger: std::sync::Arc<IntentLedger>,
    pub inner: S,
    names: BTreeMap<String, String>,
}

impl<S: ToolEventSink> IntentSink<S> {
    pub fn new(ledger: std::sync::Arc<IntentLedger>, inner: S) -> Self {
        ledger.attach_encoder();
        IntentSink {
            ledger,
            inner,
            names: BTreeMap::new(),
        }
    }
}

impl<S: ToolEventSink> ToolEventSink for IntentSink<S> {
    fn emit(&mut self, event: ToolEvent) {
        match &event {
            ToolEvent::Started { call_id, name, .. } => {
                self.names.insert(call_id.clone(), name.clone());
            }
            ToolEvent::Finished {
                turn_id,
                call_id,
                outcome,
                ..
            } => {
                // A call that never emitted `Started` never reached a tool — an
                // unknown name, a salvage failure, a gate refusal. That is not an
                // effect, and it is also not nothing: it is recorded under the
                // name the runtime refused, so `attempts` counts it.
                let name = self
                    .names
                    .remove(call_id)
                    .unwrap_or_else(|| "<refused>".to_string());
                self.ledger
                    .record_effect(turn_id, call_id, &name, outcome);
            }
            ToolEvent::Progress { .. } => {}
        }
        self.inner.emit(event);
    }
}

// ---------------------------------------------------------------------------
// The prose half, which is a heuristic and says so.
// ---------------------------------------------------------------------------

/// The most prose commitments one reconciliation reports.
const MAX_COMMITMENTS: usize = 3;

/// The longest a quoted sentence gets before it is cut.
const MAX_SENTENCE_BYTES: usize = 160;

/// First-person future-tense openings. Deliberately short: T21 says detecting
/// intent is a judgement rather than a parse, and a list that fires on "this will
/// need" produces noise that teaches the reader to skip the whole block.
const OPENERS: &[&str] = &[
    "i'll ",
    "i will ",
    "i am going to ",
    "i'm going to ",
    "let me ",
    "next i'll ",
    "next, i'll ",
    "now i'll ",
];

/// Future-tense first-person sentences in the assistant's own text.
///
/// **This is a heuristic and every caller labels it as one.** T21 states the trade
/// explicitly: the harness does not need certainty, it needs the gap visible
/// rather than silent, and a wrong steer costs a few tokens.
///
/// Three things keep the false-positive rate survivable: fenced code is skipped
/// (a `let me` inside a code block is not a commitment), the openers are
/// first-person only, and the result is capped.
pub fn commitments(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        for sentence in split_sentences(line) {
            let lower = sentence.to_lowercase();
            let hit = OPENERS.iter().any(|o| {
                lower.starts_with(o) || lower.contains(&format!(". {o}")) || {
                    // Mid-sentence after a conjunction, which is where the
                    // measured form actually appears: "…, then I'll start it".
                    lower.contains(&format!(", {o}")) || lower.contains(&format!("then {o}"))
                }
            });
            if !hit {
                continue;
            }
            out.push(clip(sentence.trim()));
            if out.len() >= MAX_COMMITMENTS {
                return out;
            }
        }
    }
    out
}

fn split_sentences(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, c) in line.char_indices() {
        if c == '.' || c == '!' || c == '?' {
            let end = i + c.len_utf8();
            if !line[start..end].trim().is_empty() {
                out.push(line[start..end].trim());
            }
            start = end;
        }
    }
    if start < line.len() && !line[start..].trim().is_empty() {
        out.push(line[start..].trim());
    }
    out
}

fn clip(s: &str) -> String {
    if s.len() <= MAX_SENTENCE_BYTES {
        return s.to_string();
    }
    let mut end = MAX_SENTENCE_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::NullToolSink;
    use std::sync::Arc;

    fn ledger_with_encoder() -> (Arc<IntentLedger>, IntentSink<NullToolSink>) {
        let l = Arc::new(IntentLedger::new());
        let sink = IntentSink::new(l.clone(), NullToolSink);
        (l, sink)
    }

    fn finished(turn: &str, call: &str, name: &str, ok: bool) -> Vec<ToolEvent> {
        vec![
            ToolEvent::Started {
                turn_id: turn.into(),
                call_id: call.into(),
                name: name.into(),
                access: crate::schema::Access::Read,
            },
            ToolEvent::Finished {
                turn_id: turn.into(),
                call_id: call.into(),
                outcome: if ok {
                    ToolOutcome::Ok
                } else {
                    ToolOutcome::Abstained {
                        reason: "nothing there".into(),
                    }
                },
                payload_digest: "d".into(),
                inline_bytes: 1,
                full_bytes: 1,
                spill: None,
                repairs: 0,
            },
        ]
    }

    #[test]
    fn a_completion_with_nothing_behind_it_is_not_complete() {
        let (l, _sink) = ledger_with_encoder();
        let id = l.declare("turn_1", "rewrite the parser", Source::Todo);
        l.start("turn_1", id).unwrap();
        let i = l.complete("turn_1", id).unwrap();
        assert_eq!(i.status, Status::Settled(Verification::NoEffect));
        let b = l.board();
        assert_eq!((b.done, b.claimed, b.total), (0, 1, 1));
        assert!(b.to_string().contains("0 of 1 complete"), "{b}");
    }

    #[test]
    fn a_completion_with_a_successful_call_behind_it_is_complete() {
        let (l, mut sink) = ledger_with_encoder();
        let id = l.declare("turn_1", "rewrite the parser", Source::Todo);
        l.start("turn_1", id).unwrap();
        for e in finished("turn_1", "c1", "edit", true) {
            sink.emit(e);
        }
        let i = l.complete("turn_1", id).unwrap();
        assert!(matches!(
            i.status,
            Status::Settled(Verification::ByEffect { .. })
        ));
        assert_eq!(l.board().done, 1);
    }

    #[test]
    fn a_todo_call_is_never_its_own_evidence() {
        let (l, mut sink) = ledger_with_encoder();
        let id = l.declare("turn_1", "do the thing", Source::Todo);
        l.start("turn_1", id).unwrap();
        for e in finished("turn_1", "c1", "todo", true) {
            sink.emit(e);
        }
        assert_eq!(l.effects().len(), 0, "an intent tool is not an effect");
        assert_eq!(
            l.complete("turn_1", id).unwrap().status,
            Status::Settled(Verification::NoEffect)
        );
    }

    #[test]
    fn an_abstention_is_an_attempt_and_not_an_effect() {
        let (l, mut sink) = ledger_with_encoder();
        let id = l.declare("turn_1", "find the caller", Source::Todo);
        l.start("turn_1", id).unwrap();
        for e in finished("turn_1", "c1", "grep", false) {
            sink.emit(e);
        }
        let r = l.reconcile("turn_1", "");
        assert_eq!((r.effects, r.attempts), (0, 1));
        assert_eq!(
            l.complete("turn_1", id).unwrap().status,
            Status::Settled(Verification::NoEffect)
        );
    }

    #[test]
    fn a_missing_encoder_is_its_own_state_and_not_a_finding_about_the_model() {
        // No IntentSink: the ledger has no encoder.
        let l = IntentLedger::new();
        let id = l.declare("turn_1", "do the thing", Source::Todo);
        assert_eq!(
            l.complete("turn_1", id).unwrap().status,
            Status::Settled(Verification::NoEncoder)
        );
        let b = l.board();
        assert_eq!((b.done, b.claimed, b.unverifiable), (0, 0, 1));
        let r = l.reconcile("turn_1", "");
        assert_eq!(r.findings.first(), Some(&Finding::NoEncoder));
    }

    #[test]
    fn t21_3_announced_and_nothing_ran() {
        let (l, _sink) = ledger_with_encoder();
        l.declare("turn_1", "start the service", Source::Todo);
        let r = l.reconcile("turn_1", "");
        assert!(
            r.findings
                .iter()
                .any(|f| matches!(f, Finding::AnnouncedNotStarted { .. })),
            "{:?}",
            r.findings
        );
        assert!(r.steering().unwrap().contains("start the service"));
    }

    #[test]
    fn a_turn_that_did_the_work_is_told_nothing() {
        let (l, mut sink) = ledger_with_encoder();
        let id = l.declare("turn_1", "start the service", Source::Todo);
        l.start("turn_1", id).unwrap();
        for e in finished("turn_1", "c1", "write", true) {
            sink.emit(e);
        }
        l.complete("turn_1", id).unwrap();
        let r = l.reconcile("turn_1", "I'll start the service.");
        assert_eq!(r.findings, vec![], "a clean turn must produce no steering");
        assert_eq!(r.steering(), None);
    }

    #[test]
    fn the_prose_heuristic_fires_only_when_the_turn_ran_nothing() {
        let (l, _sink) = ledger_with_encoder();
        let r = l.reconcile("turn_1", "I'll restart the service now.");
        assert!(
            r.findings
                .iter()
                .any(|f| matches!(f, Finding::ProseCommitment { .. })),
            "{:?}",
            r.findings
        );

        let (l2, mut sink) = ledger_with_encoder();
        for e in finished("turn_1", "c1", "write", true) {
            sink.emit(e);
        }
        let r2 = l2.reconcile("turn_1", "I'll restart the service now.");
        assert_eq!(r2.findings, vec![]);
    }

    #[test]
    fn fenced_code_is_not_a_commitment() {
        let text = "here is the fix:\n```\nlet mempool = 1;\n```\nthat is all.";
        assert_eq!(commitments(text), Vec::<String>::new());
    }

    #[test]
    fn commitments_are_capped() {
        let text = (0..20)
            .map(|i| format!("I'll do thing {i}.\n"))
            .collect::<String>();
        assert!(commitments(&text).len() <= MAX_COMMITMENTS);
    }

    #[test]
    fn reconcile_is_idempotent() {
        let (l, _sink) = ledger_with_encoder();
        l.declare("turn_1", "do it", Source::Todo);
        let a = l.reconcile("turn_1", "");
        let b = l.reconcile("turn_1", "");
        assert_eq!(a, b);
    }

    #[test]
    fn the_board_always_carries_its_denominator() {
        let (l, mut sink) = ledger_with_encoder();
        for e in finished("turn_1", "c1", "write", true) {
            sink.emit(e);
        }
        for n in 0..7 {
            let id = l.declare("turn_1", &format!("item {n}"), Source::Todo);
            match n {
                0..=2 => {
                    l.start("turn_1", id).unwrap();
                    l.complete("turn_1", id).unwrap();
                }
                3..=4 => {
                    l.start("turn_1", id).unwrap();
                }
                _ => {
                    l.block("turn_1", id, "waiting on the operator").unwrap();
                }
            }
        }
        let s = l.board().to_string();
        assert!(s.contains("3 of 7 complete"), "{s}");
        assert!(s.contains("2 in progress"), "{s}");
        assert!(s.contains("2 blocked"), "{s}");
    }

    #[test]
    fn an_unknown_item_reports_the_ones_there_are() {
        let l = IntentLedger::new();
        l.declare("turn_1", "a", Source::Todo);
        let e = l.start("turn_1", 99).unwrap_err();
        assert!(e.to_string().contains("#1"), "{e}");
    }

    #[test]
    fn a_goal_needs_a_criterion_before_it_can_be_met() {
        let l = IntentLedger::new();
        assert_eq!(l.meet_goal("turn_1").unwrap_err(), LedgerError::NoGoal);
    }
}
