//! One daemon, several sessions: the `Harness` per `Hub`.
//!
//! ```text
//!   Registry ── "s-1" → Hub ──┐            ┌── Harness(s-1)  engine · ledger · region · store
//!            ── "s-2" → Hub ──┼─ Bell ─> Sessions ── Harness(s-2)  … its own of each
//!            ── "s-3" → Hub ──┘            └── (s-3: not opened yet)
//! ```
//!
//! # What is per session and what is shared, exactly
//!
//! Shared, because it is read-only and expensive: [`Parts`] — the vocabulary GGUF
//! and the dialect's renderer and parser. One 0.6 s `vocab_only` load for the
//! process, borrowed by every session's engine.
//!
//! Per session, because the prefix invariant is: **the `TurnEngine`, the `Session`,
//! the `TokenLedger` and its `TokenRegion`, the `ToolRuntime` and the store's
//! transcript row.** None of that is arranged here — it falls out of opening one
//! [`Harness`] per hub. `TokenLedger::new` creates its own memfd, `TokenRegion` is
//! not `Clone` and appending takes `&mut`, so *two sessions sharing a token region
//! is not an expression this crate could write.* That is the guarantee, and it is
//! structural rather than upheld.
//!
//! # Sessions are opened lazily, and that is a decision
//!
//! The daemon's first session is opened eagerly at startup, because that is where a
//! dialect that does not fit the vocabulary, a missing GGUF or an unwritable store
//! is caught — all three are silent at run time, which is why `Harness::open` raises
//! them at open time.
//!
//! A session created later by a head is opened on its **first command**. It uses
//! the same `Parts` and the same `Config`, so the three failures above cannot
//! newly appear: if they were going to, the daemon would not have started. What can
//! still fail is the store, and that failure is published as a `Warning` **on that
//! session's own log**, which is where the head that just prompted into it is
//! looking. A creation that reserved nothing and cost nothing is also what makes
//! `/new` instant rather than a two-second pause on a busy box.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use letibot_sessionlog::hub::{CommandKind, QueuedCommand};
use letibot_sessionlog::registry::{Registry, SessionSource, SessionWiring, StoredBrief};
use letibot_sessionlog::{SessionEvent, hub::Hub};
use letibot_tokencore::store::{Store, TodoItem};
// **The plan's own text, and the queue it is served from** — the one renderer, so the idle check
// and the tool's reply cannot come to describe two different plans.
use letibot_tools::builtins::todo::unfinished_plan;
use std::sync::{Arc, Mutex};

use crate::config::Config;
use crate::harness::{CompactReport, Harness, HarnessError, Parts, Reply};

/// How long a monitor waiter blocks before re-checking whether the daemon is
/// shutting down.
///
/// **Not a poll interval.** Delivery is the condvar inside
/// [`letibot_tools::exec::monitor::Monitors::wait_for_any`], which returns the
/// instant something settles. This is the only thing a thread blocked in that
/// condvar can do about `Bell::close`, which notifies a *different* condvar. Five
/// seconds because the cost of the delay is a thread that outlives a shutdown by
/// up to five seconds, and the cost of making it shorter is a wake-up that does
/// nothing, several times a minute, forever.
const SHUTDOWN_RECHECK: std::time::Duration = std::time::Duration::from_secs(5);

/// How many times one prompt may continue itself past the context wall.
///
/// Each continuation is a whole turn plus the compaction behind it, and the room
/// check in [`Sessions::after_turn`] is what normally ends the loop. The cap is
/// the runaway guard for the cycle the room check cannot see: a turn that fills
/// the window in one round, a summary that frees it, repeat — every cycle does
/// real work, so no flag ever says stop. Three wall-cycles inside one prompt is
/// not a conversation recovering from the wall any more, it is a task that does
/// not fit the window, and the fourth wall is the operator's to answer — by
/// `/compact`, a fresh session, or a bigger `--context-window`.
///
/// **Read by both doors that continue past a wall.** The daemon's own sessions
/// loop over it in [`Sessions::after_turn`]; a subagent's harness — which never
/// passes through `Sessions`, and is continued by its own thread in
/// [`Harness::submit_as_a_normal_session`] — bounds itself with the same number,
/// because "a child may retry one fewer time than its parent" is not a policy
/// anybody chose.
pub(crate) const WALL_CONTINUES: usize = 3;

/// How long a session sits IDLE before the unfinished-plan check is sent.
///
/// **The operator's scheduling, in their words:** *"maybe wait for a timeout actually. so send it
/// when model is idling"* — and, on the failure the turn-boundary check had, *"but certainly not
/// after my message."*
///
/// The check used to run at the END OF A TURN, on a budget of one per user turn. That has the
/// problem backwards in both directions: a session in constant conversation was checked after
/// every single exchange (the overkill the operator reported from the `rano` window), while a
/// session that went quiet with an unfinished plan was checked once and then never again — the
/// model would have to be spoken to before it was reminded. A minute of silence is the signal
/// that nobody is waiting, which is the only state in which a nudge can be anything but an
/// interruption.
///
/// A minute and not less: a turn of real work is often longer than that, and a check that
/// arrives while a person is still reading the answer they just got is the interruption this
/// exists to avoid. The window is measured from the end of the last turn, and ANY turn re-arms
/// it — a monitor's wake or a background job's completion is work too, and the model being busy
/// with something else is not idle.
const TODO_NAG_AFTER: Duration = Duration::from_secs(60);

/// **Whether a row is work the idle plan-check may speak about.**
///
/// The operator's ask, in their own words: *"can we handle postponed todo item properly? i.e. they
/// persist but without nag and with some counter visible to me"*. A POSTPONED row is one they set
/// aside: it stays on the board, the model still sees it (marked), and the whole of what the state
/// means is that it stops asking. So it is not work this check may speak about — while a `Pending`
/// or an `InProgress` row is, whoever wrote it.
///
/// **Beside `nag_should_arm` because it is the same decision**, and a free function for the same
/// reason that one is: the rule is the whole of the behaviour, and a rule that can only be
/// exercised through a daemon is a rule tested by accident. Three readers, one definition — the
/// arming decision below, `Harness::nag_notice` (the `[todo check]` text) and `due_rows` (the
/// `[todo] … is due` firing).
pub(crate) fn the_check_may_ask_about(row: &TodoItem) -> bool {
    !matches!(
        row.status,
        letibot_tokencore::store::TodoStatus::Completed
            | letibot_tokencore::store::TodoStatus::Postponed
    )
}

/// **The plan as the idle check reads it** — every row, minus the ones it may not speak about.
///
/// **This is where a plan is narrowed, and deliberately the only place.** `unfinished_plan` renders
/// whatever it is handed, so the narrowing has to happen before the message and not inside it; two
/// functions each deciding what a plan *is* is the two-answers failure this tree refuses, and a
/// postponed row reaching the queue would be named as the next thing to do.
pub(crate) fn the_plan_as_checked(rows: &[TodoItem]) -> Vec<TodoItem> {
    rows.iter()
        .filter(|row| the_check_may_ask_about(row))
        .cloned()
        .collect()
}

/// Should the idle check be armed for a plan of these ROWS, given what it was last NAGGED with?
///
/// **The whole schedule's decision, as one predicate, and it exists so the rule can be
/// tested without a daemon.** Both halves are load-bearing: a finished (or absent) plan has
/// nothing to check, and an UNCHANGED one has already been said — re-sending it is the nagging
/// the operator called overkill, at a slower rate instead of a faster one. A model that ignores
/// the check therefore gets silence rather than a metronome, while any real work on the plan
/// earns a fresh check at the next idle period.
///
/// **It takes the ROWS and not a rendered notice**, and that is what makes the third half of the
/// rule — *a postponed row does not arm this* — assertable as a predicate instead of through a
/// session. The reading it takes (`the_plan_as_checked`) is the same one `Harness::nag_notice`
/// takes, so a plan whose only unfinished row is postponed is silent on both counts: no clock, and
/// no `[todo check]` text to send if one were armed by an earlier state of the plan.
fn nag_should_arm(todos: &[TodoItem], nagged: Option<&str>) -> bool {
    match unfinished_plan(&the_plan_as_checked(todos)) {
        Some(text) => nagged != Some(text.as_str()),
        None => false,
    }
}

/// What the worker did with one command.
pub enum Outcome {
    Replied(Box<Reply>),
    /// The session was compacted: one summary turn, then a transcript fork.
    /// The report is the evidence, not the word.
    Compacted(Box<CompactReport>),
    Failed(String),
    Ignored,
    /// **The daemon ran no turn because the session has its OWN reader**, and that reader was
    /// handed the wake — see [`Sessions::wake`]. A subagent's harness lives on the thread its
    /// parent spawned it on, so the wake goes there rather than being discarded, and this is the
    /// word for that: not `Ignored` (the condition was acted on) and not `Replied` (no turn ran
    /// here). Nothing is printed for it — a settlement inside a working tree is ordinary.
    HandedOn,
}

/// Every session this daemon is serving, and the harness behind each one.
pub struct Sessions<'a> {
    parts: &'a Parts,
    /// The command line, minus the session id. Cloned per session with its own id
    /// substituted, so a second session is the same daemon and not a second
    /// configuration nobody typed.
    base: Config,
    registry: Arc<Registry>,
    open: HashMap<String, Harness<'a>>,
    /// Sessions whose monitor waiter is already running. **One waiter per name**,
    /// which is T24 requirement 4 — the fleet has already paid for what two
    /// processes under one reader costs: the roster shows a seat attached while the
    /// real one hears nothing.
    armed: std::collections::HashSet<String>,
    /// The flowy seat this daemon holds, when it holds one. Daemon-level, on
    /// purpose: a seat is a persistent identity with one inbox reader, and a
    /// session is a temporary consumer of it — see `letibot_flowy`'s crate docs.
    /// Set at start by `--flowy`, or later by `/flowy login` from a head.
    seat: Option<letibot_flowy::Seat>,
    /// Each root session's `flowy` tool slot: the door is always seated, and the
    /// slot is filled when a seat is attached — at open, or later.
    slots: HashMap<String, letibot_flowy::tool::SeatSlot>,
    /// Each root session's condition on the seat, so a title change can rename
    /// the session's address (`@seat/title`).
    seated: HashMap<String, Arc<letibot_flowy::InboxCondition>>,
    /// The fabric block each root session last saw, so a refresh after a
    /// compaction is a system update only when something changed.
    fabric_seen: HashMap<String, String>,
    /// When each session's idle plan-check comes due, or absent for a session with
    /// nothing to check. See [`Sessions::rearm_todo_nag`].
    nag_due: HashMap<String, Instant>,
    /// When to next look for a tool call nothing will answer. See `arm_sweep`.
    sweep_at: Option<Instant>,
    /// The plan notice each session was last NAGGED with.
    ///
    /// **This is what makes it once per idle period rather than every minute.** A check that
    /// was sent and not acted on must not be sent again — the model has been told, and telling
    /// it the same unchanged list again is the nagging the operator called overkill. So a
    /// re-arm compares the plan against what was last sent: unchanged means silence, and any
    /// change at all (an item added, one closed, one started) is a new thing to say.
    nagged: HashMap<String, String>,
}

/// Which sessions attach to the seat, and how the attachment is wired.
///
/// **Root sessions only.** A subagent spawned by `task` hears the room through
/// its parent and speaks through it: one name, one mind under it at a time. So
/// the tool and the monitor are given to a session with no `parent_session_id`
/// and to nothing else.
fn is_root(registry: &Registry, session_id: &str) -> bool {
    registry
        .list()
        .iter()
        .find(|b| b.session_id == session_id)
        .map(|b| b.parent_session_id.is_none())
        .unwrap_or(true)
}

/// **What the daemon can do with a wake for a session** — and which of the three it is.
///
/// Pure, so the routing decision is assertable without a daemon, a socket or a model: it is two
/// booleans the caller already has to look up, and the whole of the operator's requirement is
/// which arms exist. The operator's design, in their words: *"think about it like it is an
/// erlang supervision tree. we talk to parents and they own lifecycle."* — and a supervisor whose
/// exits nobody can deliver is not a supervisor, so there is no arm here that throws one away
/// while anything can still act on it.
///
/// * **`Drive`** — the daemon holds this session's harness (`Sessions::open`), so the worker runs
///   the turn: `Harness::wake` for a root, and for every session the daemon opened.
/// * **`ItsOwnReader`** — the session is live in the registry and the daemon does not hold it:
///   a subagent, whose harness lives on the thread its parent spawned it on. The daemon hands the
///   wake to that thread (`Hub::wake_its_own_reader`), which is what makes a ring naming a child
///   a promise rather than the `Ignored` R58 measured.
/// * **`Gone`** — no hub at all. Nothing to hand anything to, and nothing lost: a session that
///   closed handed its undrained settlements up a level first (`jobwatch::JobWatchers::stop`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WakeRoute {
    Drive,
    ItsOwnReader,
    Gone,
}

fn wake_route(held_by_the_daemon: bool, live_in_the_registry: bool) -> WakeRoute {
    match (held_by_the_daemon, live_in_the_registry) {
        (true, _) => WakeRoute::Drive,
        (false, true) => WakeRoute::ItsOwnReader,
        (false, false) => WakeRoute::Gone,
    }
}

impl<'a> Sessions<'a> {
    /// Start with one session, opened eagerly. See the module header for why the
    /// first one is not lazy.
    pub fn open_first(
        parts: &'a Parts,
        cfg: Config,
        registry: Arc<Registry>,
    ) -> Result<Sessions<'a>, HarnessError> {
        Self::open_first_with_seat(parts, cfg, registry, None)
    }

    /// [`Sessions::open_first`] with the daemon's flowy seat, which the first
    /// session attaches to like every root session after it.
    pub fn open_first_with_seat(
        parts: &'a Parts,
        cfg: Config,
        registry: Arc<Registry>,
        seat: Option<letibot_flowy::Seat>,
    ) -> Result<Sessions<'a>, HarnessError> {
        let id = cfg.session_id.clone();
        let hub = registry
            .get(&id)
            .ok_or_else(|| HarnessError::Setup(format!("session {id} is not in the registry")))?;
        let mut sessions = Sessions {
            parts,
            base: cfg.clone(),
            registry: registry.clone(),
            open: HashMap::new(),
            armed: std::collections::HashSet::new(),
            seat,
            slots: HashMap::new(),
            seated: HashMap::new(),
            fabric_seen: HashMap::new(),
            nag_due: HashMap::new(),
            sweep_at: None,
            nagged: HashMap::new(),
        };
        let (tool, cond) = sessions.seat_tool(&id);
        // Compose the system prompt from `prompts.toml`, once, at open. Message 0 is
        // written from this and never rewritten after — see `Config::compose_system`.
        let mut cfg = cfg;
        cfg.compose_system();
        let cfg = sessions.with_fabric(&id, cfg, cond.is_some());
        let mut harness =
            Harness::open_with_registry(parts, cfg, hub, None, tool, registry.clone())?;
        // **A `--continue` resumes the session on the model it was switched to.** The
        // first session is the common resume: without this call the row this change
        // writes would only be read on the lazy path a head's switch reaches, and the
        // one resume everybody types would go on ignoring it.
        harness.restore_provider_choice();
        sessions.open.insert(id.clone(), harness);
        sessions.declare_flowy_monitor(&id, cond);
        // **AND THE DAEMON'S OWN FIRST SESSION ARMS ITS CLOCK TOO.** This constructor inserts into
        // `open` directly and never goes through `Sessions::open`, so the arming added there does not
        // reach it — and this is the session a plain `letibot --continue` resumes, which makes it the
        // COMMON case rather than a corner. Same rule as there: the board was just rebuilt from the
        // store, so an unfinished plan is the model's own outstanding work and is worth one check.
        sessions.rearm_todo_nag(&id);
        Ok(sessions)
    }

    /// The seat this daemon holds, if any.
    pub fn seat(&self) -> Option<&letibot_flowy::Seat> {
        self.seat.as_ref()
    }

    /// The `flowy` tool for a root session — always, as a door — and, when the
    /// daemon holds a seat, the session's condition on it. A subagent gets
    /// neither: it routes through its parent.
    fn seat_tool(
        &mut self,
        session_id: &str,
    ) -> (
        Option<Box<dyn letibot_tools::runtime::Tool>>,
        Option<Arc<letibot_flowy::InboxCondition>>,
    ) {
        if !is_root(&self.registry, session_id) {
            return (None, None);
        }
        let (tool, slot) = letibot_flowy::Flowy::unattached();
        self.slots.insert(session_id.to_string(), slot.clone());
        let cond = self
            .seat
            .clone()
            .map(|seat| self.attach_session(&seat, session_id, &slot));
        (Some(Box::new(tool)), cond)
    }

    /// Attach one root session to the seat: its condition, its slot filled.
    fn attach_session(
        &mut self,
        seat: &letibot_flowy::Seat,
        session_id: &str,
        slot: &letibot_flowy::tool::SeatSlot,
    ) -> Arc<letibot_flowy::InboxCondition> {
        // The title is the session's short address on the fabric — `@seat/title`
        // beside `@seat/id` — so agents on one project can name each other.
        let title = self
            .registry
            .list()
            .iter()
            .find(|b| b.session_id == session_id)
            .map(|b| b.title.clone())
            .unwrap_or_default();
        let cond = seat.attach_as(session_id, &title, letibot_flowy::Attention::default());
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(letibot_flowy::tool::Attachment {
            seat: seat.clone(),
            cond: cond.clone(),
        });
        self.seated.insert(session_id.to_string(), cond.clone());
        cond
    }

    /// One slash verb from a head, against this session.
    pub fn slash(&mut self, session_id: &str, line: &str) -> crate::slash::SlashReply {
        self.slash_parsed(session_id, crate::slash::Slash::parse(line))
    }

    fn slash_parsed(
        &mut self,
        session_id: &str,
        parsed: crate::slash::Slash,
    ) -> crate::slash::SlashReply {
        use crate::slash::{Slash, SlashReply};
        match parsed {
            Slash::Help(h) => SlashReply {
                lines: vec![h],
                ok: false,
            },
            Slash::Gate(verb) => crate::slash::gate(self.base.store.as_deref(), &verb),
            Slash::Supervise { want, at } => {
                let Some(h) = self.open.get_mut(session_id) else {
                    return SlashReply {
                        lines: vec![format!("session {session_id} is not open")],
                        ok: false,
                    };
                };
                let mut lines = Vec::new();
                if let Some(addr) = at {
                    let ep = match letibot_turn::Endpoint::parse(&addr) {
                        Ok(e) => e,
                        Err(why) => {
                            return SlashReply {
                                lines: vec![why],
                                ok: false,
                            };
                        }
                    };
                    match h.attach_oracle(ep) {
                        Ok(line) => lines.push(line),
                        Err(why) => {
                            return SlashReply {
                                lines: vec![why],
                                ok: false,
                            };
                        }
                    }
                }
                match want {
                    // A report, not a change. `/supervise status` after a long turn
                    // is the cheapest way to answer "is this being measured", and it
                    // must not be the same keystroke as turning it on.
                    None => SlashReply {
                        lines: vec![if h.supervising() {
                            "supervised: the guard model answers every gated call \
                             before you do, and both verdicts land on the corpus row"
                                .into()
                        } else {
                            "not supervised: calls ask whoever the mode says, and no \
                             model verdict is recorded. `/supervise` turns it on."
                                .into()
                        }],
                        ok: true,
                    },
                    Some(on) => match h.set_supervision(on) {
                        Ok(line) => {
                            lines.push(line);
                            SlashReply { lines, ok: true }
                        }
                        Err(why) => {
                            lines.push(why);
                            SlashReply { lines, ok: false }
                        }
                    },
                }
            }
            Slash::FlowyStatus => crate::slash::flowy_status(self.seat.as_ref()),
            Slash::FlowyLogout => match self.detach_seat() {
                Some(name) => SlashReply {
                    lines: vec![format!(
                        "released seat `{name}`; the room is no longer heard"
                    )],
                    ok: true,
                },
                None => SlashReply {
                    lines: vec!["no seat was attached".into()],
                    ok: false,
                },
            },
            Slash::FlowyLogin {
                seat,
                addr,
                token,
                token_file,
                new_reader,
            } => {
                let (creds, mut lines) = match crate::slash::flowy_login_credentials(
                    seat.as_deref(),
                    addr.as_deref(),
                    token.as_deref(),
                    token_file.as_ref(),
                ) {
                    Ok(x) => x,
                    Err(lines) => return SlashReply { lines, ok: false },
                };
                lines.push(format!("credentials: {}", creds.describe()));
                let seat = match letibot_flowy::Seat::open(creds, None, None) {
                    Ok(s) => s,
                    Err(e) => {
                        lines.push(format!("{e}"));
                        return SlashReply { lines, ok: false };
                    }
                };
                if new_reader {
                    match seat.declare_reader() {
                        Ok(r) => lines.push(format!(
                            "declared reader `{}` at cursor {}",
                            r.reader, r.cursor
                        )),
                        Err(e) => {
                            lines.push(format!("declaring the reader: {e}"));
                            return SlashReply { lines, ok: false };
                        }
                    }
                } else {
                    match seat.reader() {
                        Ok(Some(r)) => {
                            lines.push(format!("reader `{}` at cursor {}", r.reader, r.cursor))
                        }
                        Ok(None) => {
                            lines.push(format!(
                                "reader `{}` is NOT DECLARED on the node. If this seat has never listened, \
                                 `/flowy login {} --new-reader`. If it has, its token was SWITCHED and the old \
                                 identity still holds every message since — read that first.",
                                seat.name(),
                                seat.name()
                            ));
                            return SlashReply { lines, ok: false };
                        }
                        Err(e) => lines.push(format!(
                            "node not answering yet ({e}); the listener will keep trying"
                        )),
                    }
                }
                let _ = session_id;
                lines.extend(self.attach_seat(seat));
                lines.push(
                    "attached. `/flowy status` for the seat; the `flowy` tool speaks as it.".into(),
                );
                SlashReply { lines, ok: true }
            }
            // The standing choice, and only that: no session changes here. The
            // operator asked for it as its own verb after `/models` doing both at
            // once cost them two questions.
            Slash::DefaultModel(want) => crate::slash::default_model(want.as_deref(), None),
            Slash::Tools => match self.open.get(session_id) {
                Some(h) => SlashReply {
                    lines: h.tools_lines(),
                    ok: true,
                },
                None => SlashReply {
                    lines: vec![format!("session {session_id} is not open")],
                    ok: false,
                },
            },
            Slash::Job { job, offset } => {
                let Some(h) = self.open.get(session_id) else {
                    return SlashReply {
                        lines: vec![format!("session {session_id} is not open")],
                        ok: false,
                    };
                };
                match job {
                    None => SlashReply {
                        lines: h.job_lines(),
                        ok: true,
                    },
                    // One page: a build log's tail is one read, and the reply names
                    // the next offset when it is not. See [`JOB_OUTPUT_WINDOW`].
                    Some(j) => match h.job_output(&j, offset, crate::harness::JOB_OUTPUT_WINDOW) {
                        Ok(lines) => SlashReply { lines, ok: true },
                        Err(e) => SlashReply {
                            lines: vec![e],
                            ok: false,
                        },
                    },
                }
            }
            Slash::Models => {
                let current = self
                    .open
                    .get(session_id)
                    .map(|h| h.provider_line())
                    .unwrap_or_else(|| "(session not open)".into());
                SlashReply {
                    lines: crate::slash::models_listing(&current),
                    ok: true,
                }
            }
            // **Switches this session and nothing else.** The standing choice is
            // `/default-model`'s; see `Slash::ModelsSet` for the three goes it
            // took to separate them.
            Slash::ModelsSet {
                provider,
                model,
                key,
            } => {
                let (choice, mut lines) = match crate::slash::models_choice(
                    &provider,
                    model.as_deref(),
                    key.as_deref(),
                    None,
                ) {
                    Ok(x) => x,
                    Err(lines) => return SlashReply { lines, ok: false },
                };
                let Some(h) = self.open.get_mut(session_id) else {
                    lines.push(format!("session {session_id} is not open"));
                    return SlashReply { lines, ok: false };
                };
                let applied = match choice {
                    crate::slash::ModelChoice::OwnServer => h.set_provider(None),
                    crate::slash::ModelChoice::Metered(pc) => h.set_provider(Some(pc)),
                    // Its own door, because it verifies the vocabulary before it
                    // moves anything — see `Harness::set_local_model`.
                    crate::slash::ModelChoice::Local(m) => h.set_local_model(&m),
                };
                match applied {
                    Ok(line) => {
                        // **The choice goes to the session row in the same breath** —
                        // `persist_provider_choice`'s own doc is why. A switch that
                        // reached the screen but not the row was a choice that expired
                        // with the process, which is the defect this whole change is.
                        if let Err(why) = h.persist_provider_choice() {
                            lines.push(why);
                        }
                        lines.push(line);
                        SlashReply { lines, ok: true }
                    }
                    Err(e) => {
                        lines.push(e.to_string());
                        SlashReply { lines, ok: false }
                    }
                }
            }
        }
    }

    /// **A seat arrives while sessions are open** — `/flowy login` from a head.
    /// Every open root session is attached: its slot filled, its `flowy` monitor
    /// declared, its wake armed, the shelf installed, the fabric block appended
    /// as a system update. Returns one line per session, for the head.
    pub fn attach_seat(&mut self, seat: letibot_flowy::Seat) -> Vec<String> {
        let mut report = Vec::new();
        if let Some(old) = self.seat.replace(seat.clone()) {
            old.stop();
            report.push(format!("released the previous seat `{}`", old.name()));
        }
        seat.start();
        self.parts
            .skills
            .set_shelf(std::sync::Arc::new(letibot_flowy::FabricShelf::new(
                seat.clone(),
            )));
        let ids: Vec<String> = self
            .open
            .keys()
            .filter(|id| is_root(&self.registry, id))
            .cloned()
            .collect();
        for id in ids {
            let Some(slot) = self.slots.get(&id).cloned() else {
                continue;
            };
            let cond = self.attach_session(&seat, &id, &slot);
            self.declare_flowy_monitor(&id, Some(cond));
            match self.refresh_fabric(&id) {
                Ok(true) => report.push(format!(
                    "{id}: attached; the fabric block went in as a system update"
                )),
                Ok(false) => report.push(format!("{id}: attached")),
                Err(e) => report.push(format!(
                    "{id}: attached; the fabric block could not be read: {e}"
                )),
            }
        }
        report
    }

    /// `/flowy logout`: stop the seat, empty every slot, retire the monitors.
    pub fn detach_seat(&mut self) -> Option<String> {
        let seat = self.seat.take()?;
        seat.stop();
        for slot in self.slots.values() {
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        self.seated.clear();
        for h in self.open.values() {
            if let Some(m) = h.monitors() {
                m.retire("flowy", "the seat was released");
            }
        }
        Some(seat.name().to_string())
    }

    /// The fabric block into a root session's system prompt, and its provenance
    /// into the disclosure. A session that is not seated gets neither, and the
    /// disclosure says `fabric: OFF` when a seat exists.
    ///
    /// Message 0 is never rewritten, so a resumed session keeps the block it
    /// was opened with; `refresh_fabric` appends a system update when a fresh
    /// reading differs — that is the path after a compaction too.
    fn with_fabric(&mut self, session_id: &str, mut cfg: Config, seated: bool) -> Config {
        let Some(seat) = &self.seat else {
            return cfg;
        };
        if !seated {
            return cfg;
        }
        let (block, line) = self.fabric_block(seat);
        cfg.system = format!("{}\n\n{block}", cfg.system.trim_end());
        cfg.fabric = Some(line);
        self.fabric_seen.insert(session_id.to_string(), block);
        cfg
    }

    /// The block and its one-line provenance.
    fn fabric_block(&self, seat: &letibot_flowy::Seat) -> (String, String) {
        use letibot_flowy::FabricSource;
        let (ctx, source) = seat.fabric();
        match (ctx, source) {
            (Some(ctx), src @ FabricSource::Live) => {
                let line = format!(
                    "{} skill summaries and {} memory titles from the node, read at {}; the \
                     model loads a body through `skill` or `flowy get` when it needs one",
                    ctx.skills.len(),
                    ctx.memories.len(),
                    ctx.read_at
                );
                (ctx.render(&src), line)
            }
            (Some(ctx), src @ FabricSource::Cached { .. }) => {
                let line = format!(
                    "CACHED — the node is unreachable; {} skills and {} memories from the copy \
                     read at {}, labelled stale in the prompt",
                    ctx.skills.len(),
                    ctx.memories.len(),
                    ctx.read_at
                );
                (ctx.render(&src), line)
            }
            (_, FabricSource::Unreachable { why }) => (
                letibot_flowy::FabricContext::render_unreachable(seat.name(), &why),
                format!("UNREACHABLE — {why}; no cached copy, and the prompt says so"),
            ),
            (None, src) => (
                letibot_flowy::FabricContext::render_unreachable(seat.name(), &format!("{src:?}")),
                "UNREACHABLE".into(),
            ),
        }
    }

    /// Re-read the fabric for a root session and, when the block changed, append
    /// it as a system update (§5.3). Called after a compaction. Returns whether
    /// an update went in.
    pub fn refresh_fabric(&mut self, session_id: &str) -> Result<bool, HarnessError> {
        let Some(seat) = self.seat.clone() else {
            return Ok(false);
        };
        if !self.seated.contains_key(session_id) {
            return Ok(false);
        }
        let (block, _line) = self.fabric_block(&seat);
        if self.fabric_seen.get(session_id) == Some(&block) {
            return Ok(false);
        }
        let h = self.harness(session_id)?;
        h.system_update(&block)?;
        self.fabric_seen.insert(session_id.to_string(), block);
        Ok(true)
    }

    /// Declare the session's `flowy` monitor — continuous, owned by the session
    /// scope, renewed by the seat's loop — and arm the wake. The operator's
    /// words: *"I don't want to fiddle with monitors."* The model never declares
    /// this one; it finds it in `job_list`.
    ///
    /// A session whose backend cannot hold a monitor gets the tool and no
    /// listener, and the disclosure says `NOT SEATED` rather than pretending.
    fn declare_flowy_monitor(
        &mut self,
        session_id: &str,
        cond: Option<Arc<letibot_flowy::InboxCondition>>,
    ) {
        let (Some(cond), Some(seat)) = (cond, &self.seat) else {
            return;
        };
        let Some(h) = self.open.get(session_id) else {
            return;
        };
        let (Some(monitors), Some(owner)) = (h.monitors().cloned(), h.session_scope()) else {
            if let Some(hub) = self.registry.get(session_id) {
                hub.publish(SessionEvent::Warning {
                    code: "flowy_not_seated".into(),
                    detail: format!(
                        "seat `{}` is attached to this session but its backend cannot hold \
                         a monitor, so nothing said on the fabric can wake it. The `flowy` \
                         tool still speaks; `flowy status` shows what is pending.",
                        seat.name()
                    ),

                    compaction: None,
                });
            }
            return;
        };
        use letibot_tools::exec::monitor::{CustomWatch, MAX_TTL, Watch};
        match monitors.declare(
            "flowy",
            owner,
            Watch::Custom(CustomWatch(cond)),
            None,
            "harnessd",
            MAX_TTL,
            true,
        ) {
            Ok(_) => {
                seat.keep_renewing(&monitors, "flowy");
                self.arm_wake(session_id);
            }
            Err(e) => {
                if let Some(hub) = self.registry.get(session_id) {
                    hub.publish(SessionEvent::Warning {
                        code: "flowy_not_seated".into(),
                        detail: format!("the `flowy` monitor could not be declared: {e}"),

                        compaction: None,
                    });
                }
            }
        }
    }

    /// What a session is attached to, from the daemon's own command line.
    pub fn wiring(cfg: &Config) -> SessionWiring {
        SessionWiring {
            model: cfg.model.clone(),
            dialect: cfg.dialect.name().to_string(),
            endpoint: cfg.endpoint.authority(),
            workspace: cfg.workspace.display().to_string(),
        }
    }

    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// Open a session now, rather than on its first prompt.
    ///
    /// This is what a `ResumeSession` turns into: a resume's chain verification, its
    /// dialect refusal and its republished transcript all live in `Harness::open`,
    /// and running them lazily would put all three inside the first prompt — so a
    /// head that resumed a session and attached to it would see an empty screen, and
    /// find out it had failed only by typing into it.
    ///
    /// The failure is announced on **that session's own log**, which is where the
    /// head that asked for it is looking, and the daemon keeps serving: one session
    /// that cannot be rebuilt is not a reason to take down the others.
    /// `Ok(false)` means it was already open — the daemon's own first session, which
    /// `open_first` opened eagerly and whose creation also queued a `Work::Open`.
    /// Reported rather than swallowed so the caller does not print the resume banner
    /// twice for one session.
    pub fn open(&mut self, session_id: &str) -> Result<bool, HarnessError> {
        if self.open.contains_key(session_id) {
            return Ok(false);
        }
        let hub = self.registry.get(session_id);
        match self.harness(session_id) {
            Ok(h) => {
                if let Some(r) = h.resumed() {
                    let r = r.clone();
                    if let Some(hub) = &hub {
                        for note in &r.notes {
                            hub.publish(SessionEvent::Warning {
                                code: "resume_note".into(),
                                detail: note.clone(),

                                compaction: None,
                            });
                        }
                    }
                }
                if let Some(hub) = &hub {
                    for note in h.open_notes() {
                        hub.publish(SessionEvent::Warning {
                            code: "open_note".into(),
                            detail: note.clone(),

                            compaction: None,
                        });
                    }
                }
                self.publish_title(session_id);
                // **A RESUMED SESSION ARMS ITS OWN CLOCK, because nothing else will.**
                //
                // `nag_due` is in-memory and the only other arming point used to be the end of a
                // turn — so a daemon that came back with an unfinished plan sat there for ever: no
                // turn had ended yet, so no deadline existed, so the idle worker had nothing to wake
                // for. The operator, watching exactly this on a resumed session: *"so when I bring
                // session back it will not fire right now - this is exactly what i see with rano"*.
                //
                // **It is safe because the plan was RESTORED, not invented.** `Harness::open`
                // rebuilds the todo board from the store (`store.todos(session_id)`), so the rows
                // this arms on are the plan the model was working from — the ones it wrote before
                // the restart. A session with a finished or empty plan arms nothing, which is the
                // ordinary case and still costs no wake.
                //
                // The nag is a real turn, so this is a session that will speak once, unprompted, a
                // minute after it is reopened with work outstanding. That is the intent: it is the
                // replacement for a model's memory that did not survive the restart either.
                self.rearm_todo_nag(session_id);
                Ok(true)
            }
            Err(e) => {
                if let Some(hub) = &hub {
                    hub.publish(SessionEvent::Warning {
                        code: "resume_failed".into(),
                        detail: e.to_string(),

                        compaction: None,
                    });
                }
                Err(e)
            }
        }
    }

    /// What a session was rebuilt from, for the daemon's own banner.
    pub fn resume_report(&self, session_id: &str) -> Option<crate::harness::ResumeReport> {
        self.open.get(session_id).and_then(|h| h.resumed().cloned())
    }

    /// What opening a NEW session decided that the operator should hear — the mode
    /// the project store chose over the flag, for one. Empty for a resumed session.
    pub fn open_notes(&self, session_id: &str) -> Vec<String> {
        self.open
            .get(session_id)
            .map(|h| h.open_notes().to_vec())
            .unwrap_or_default()
    }

    /// Move a title the harness derived into the registry, where a picker reads it.
    ///
    /// The harness holds a `Hub` and not a `Registry` — deliberately, so a `Harness`
    /// can be built in a test with nothing else — so the handover happens here, at
    /// the one layer that holds both.
    fn publish_title(&mut self, session_id: &str) {
        let Some(h) = self.open.get_mut(session_id) else {
            return;
        };
        if let Some(title) = h.take_new_title() {
            if let Some(c) = self.seated.get(session_id) {
                c.set_alias(&title);
            }
            self.registry.set_title(session_id, title);
        }
    }

    /// The harness for `session_id`, opening it if a head made the session and
    /// nothing has run in it yet.
    ///
    /// `Err` is a session that cannot be served at all; the caller announces it on
    /// that session's log rather than taking the daemon down, because every other
    /// session is still fine.
    fn harness(&mut self, session_id: &str) -> Result<&mut Harness<'a>, HarnessError> {
        if !self.open.contains_key(session_id) {
            let hub = self.registry.get(session_id).ok_or_else(|| {
                HarnessError::Setup(format!("no session {session_id} in this daemon"))
            })?;
            // The session's own tree, when the registry has one — a head that made
            // this session named the directory it was standing in, and a resumed one
            // carries the root from the store. `self.base.workspace` is the daemon's
            // command line, which is a fact about the daemon.
            let ws = self.registry.wiring(session_id).workspace;
            // Whether this is somebody's subagent is the registry's fact, not the
            // command line's: a resumed subagent must not be seated as a root.
            let parent = self
                .registry
                .list()
                .iter()
                .find(|b| b.session_id == session_id)
                .and_then(|b| b.parent_session_id.clone());
            let mut cfg = Config {
                session_id: session_id.to_string(),
                parent_session_id: parent,
                workspace: if ws.is_empty() {
                    self.base.workspace.clone()
                } else {
                    std::path::PathBuf::from(ws)
                },
                ..self.base.clone()
            };
            let (tool, cond) = self.seat_tool(session_id);
            // Compose the system prompt from `prompts.toml`, once, at open — the same
            // rule as the first session: message 0 is written from this and never
            // rewritten after.
            cfg.compose_system();
            let cfg = self.with_fabric(session_id, cfg, cond.is_some());
            let mut h = Harness::open_with_registry(
                self.parts,
                cfg,
                hub,
                None,
                tool,
                self.registry.clone(),
            )?;
            // **The lazy open is the resume a head reaches** — `/resume`, a switch
            // from the picker, a subagent's parent coming back. The row the switch
            // wrote is read here, so a session this daemon did not start running
            // still comes back on its own model rather than the CLI default.
            h.restore_provider_choice();
            self.open.insert(session_id.to_string(), h);
            self.declare_flowy_monitor(session_id, cond);
        }
        Ok(self.open.get_mut(session_id).expect("just inserted"))
    }

    /// Sessions with a harness behind them. The rest exist and have never run.
    pub fn opened(&self) -> usize {
        self.open.len()
    }

    /// Submit one prompt to a session and wait for the answer.
    ///
    /// The scripted path (`harnessd --prompt …`), which has no head and no socket
    /// traffic. It goes through the same `harness()` as the worker, so a scripted
    /// run and a driven one open a session the same way.
    pub fn submit(&mut self, session_id: &str, text: &str) -> Result<Reply, HarnessError> {
        self.run_prompt(session_id, text)
    }

    /// **One turn, and everything that has to happen around one.**
    ///
    /// Both ways a prompt reaches a session go through here — the scripted
    /// `--prompt` path and the command queue a head's prompt lands on — because
    /// they used to be two copies of this sequence and one of them was missing a
    /// step. The missing step was the automatic compaction, and the path missing it
    /// was the one every interactive session takes: a banner that said *"compacts
    /// automatically with 16384 left"* to sessions where nothing called it, and an
    /// operator who met the wall and was told a compaction had been attempted.
    /// *"it failed to compact lol"* — it had not failed, it had not run.
    ///
    /// A second copy of a sequence is a second place to forget one of its steps, so
    /// there is one.
    fn run_prompt(&mut self, session_id: &str, text: &str) -> Result<Reply, HarnessError> {
        // **The operator has spoken, so the plan check is earned back** — see
        // `note_operator_prompt`. Called here rather than in `dispatch`'s `Prompt` arm because
        // `run_prompt` is the one place a prompt from ANY door arrives (a head's enter, a script,
        // `--continue`), and a rule applied at one door is a rule the other doors do not have.
        self.note_operator_prompt(session_id);
        // **The wall is checked BEFORE the send, not only after the last turn.**
        //
        // `compact_if_at_the_wall`'s own contract is to compact *"when the NEXT
        // turn would not fit"* — and both of its call sites are AFTER a turn, so
        // the contract holds for every turn except the first one after a resume.
        // A session that has just been reopened has taken no turn since the daemon
        // started, so nothing has checked, and the prompt goes out whole.
        //
        // Measured 2026-10-02, and it is what made a dead head rather than a slow
        // one: a resumed leticl session sent 1,463,497 tokens against a 1,048,576
        // window. The provider's refusal comes back as a BACKEND error and not as
        // `HarnessError::ContextWall`, so `after_turn`'s `out.is_ok() || wall` is
        // false and the compaction is never reached — the session could not
        // recover on its own however many times the operator retyped. With this
        // check, the same session compacts first and the turn fits (~923k).
        //
        // Before the turn clock, not after: a compaction is the tidying, not a
        // round of the turn, and a composer that counted it would show the
        // operator a turn duration that is mostly the summary.
        //
        // Not a second copy of the tidying: the same function, the same predicate
        // and the same swallowed failure, called earlier on the one path every
        // prompt from every door takes (`--continue`, a head's enter, a script).
        // The invariant it restores is the one the call sites already assumed.
        self.compact_if_at_the_wall(session_id);
        // **A turn is one prompt however many ROUNDS it takes, so the clock starts HERE.**
        // `run_turn_steered` is called inside the round loop, so `TurnStarted` fires per round
        // and a head timing from it restarts at every one — the composer read `2.1s` a minute
        // into a turn. The operator's report: *"it should be still responding even while you do
        // tools calls and such, and not reset, currently it resets."* Stamped where the prompt
        // arrives, carried through every round of it, and cleared when the turn ends.
        let began = letibot_sessionlog::event::now_ms();
        if let Some(h) = self.open.get_mut(session_id) {
            h.begin_turn_clock(began);
        }
        let hub = self.registry.get(session_id);
        // Opened here rather than held across the tidying above: a live borrow of
        // `self.open` would stop a compaction from re-entering it.
        let out = match self.harness(session_id) {
            Ok(h) => h.submit(text),
            Err(e) => {
                if let Some(hub) = &hub {
                    hub.publish(SessionEvent::Warning {
                        code: "session_unavailable".into(),
                        detail: format!(
                            "this session could not be opened, so nothing was run: {e}"
                        ),

                        compaction: None,
                    });
                }
                return Err(e);
            }
        };
        self.publish_title(session_id);
        self.arm_wake(session_id);
        // `after_turn` owns the tail now, wall continuations included, so it
        // takes the outcome and returns the one that finally stands.
        self.after_turn(session_id, &hub, out)
    }

    /// **Everything that follows a turn, whatever started it.**
    ///
    /// A prompt from a script, a prompt from a head, a monitor firing — three ways
    /// in and one set of obligations on the way out. Kept in one place because the
    /// last time they were in three, one of them was missing the compaction.
    fn after_turn(
        &mut self,
        session_id: &str,
        hub: &Option<std::sync::Arc<Hub>>,
        mut out: Result<Reply, HarnessError>,
    ) -> Result<Reply, HarnessError> {
        // The failure reaches the head FIRST: it is the answer to what was asked,
        // and the tidying after it is not.
        if let Err(e) = &out {
            let turn_id = self
                .open
                .get(session_id)
                .map(|h| h.last_turn_id().to_string())
                .unwrap_or_default();
            if let Some(hub) = &hub {
                publish_failure(hub, &turn_id, e);
            }
        }

        // **The wall is the one failure that must still compact.** The turn stopped
        // precisely because the context is full; returning without compacting
        // leaves the NEXT turn to meet the same wall at round zero, and the one
        // after that, forever. Everything the turn produced is committed, so there
        // is nothing to lose by tidying now.
        let wall = matches!(out, Err(HarnessError::ContextWall { .. }));
        if out.is_ok() || wall {
            let attempted = self.compact_if_at_the_wall(session_id);
            // A wall that did not even reach the compaction threshold is a
            // contradiction, and the operator should be told which of the two
            // numbers disagreed rather than left to infer it from a silence — the
            // wall's own message says a compaction was attempted.
            if wall
                && !attempted
                && let Some(hub) = &hub
            {
                hub.publish(SessionEvent::Warning {
                    code: "auto_compact_skipped".into(),
                    detail: format!(
                        "the turn stopped at the context wall and nothing was \
                         compacted: automatic compaction is {} for this session. \
                         `/compact` does it by hand.",
                        if self.base.context_window.is_none() {
                            "not configured — no --context-window is set, so there is \
                             no wall to measure against"
                        } else {
                            "off"
                        }
                    ),

                    compaction: None,
                });
            }
        }

        // **A wall that compacted is not the end of the prompt.**
        //
        // Until now the wall ended the run: the turn stopped, the compaction
        // tidied, the worker went idle — and the operator typed "continue" by
        // hand to start the turn the prompt still owed them, every time, which
        // is the report this loop answers. opencode never had the hole, because
        // its compaction is a step inside the run loop (`prompt.ts`: overflow →
        // `compaction.create` → `continue`) and the run never ends; this harness
        // cannot do that, because the wall is the engine's stop and the
        // compaction is this struct's act, so the seam is here — after the
        // tidying, while the prompt is still open.
        //
        // The continuation is [`Harness::continue_after_wall`]'s user item, and
        // each one is a whole turn with this same tail: it publishes its own
        // failure, compacts after itself, and can meet the wall again, which is
        // what the loop is for. Two gates bound it. Room is MEASURED, not read
        // off a flag — the no-progress guard turns `auto_compact` off without
        // freeing anything, and a flag would then read as room that is not
        // there — and [`WALL_CONTINUES`] caps the cycles the room check cannot
        // see. When either gate closes, the wall error from the last turn is
        // what the caller gets, which is the honest answer.
        if wall {
            for _ in 0..WALL_CONTINUES {
                // **The session's own config, not the daemon's.** `ledger_len` is
                // counted off the ledger and `room_for_next_turn` has to compare
                // it in the same units — which means the window scaled by
                // `ledger_scale`, and that is measured per SESSION and lives on
                // the session's harness. `self.base` is the command line, where it
                // is always `None`, so this asked "does 991k ledger tokens fit in
                // a 1M provider window" and answered no for a conversation the
                // provider counted at 671k. The same unit confusion as the
                // `/reseat` refusal, one door along.
                let room = self
                    .open
                    .get(session_id)
                    .map(|h| h.config().room_for_next_turn(h.ledger_len() as u64))
                    .unwrap_or(false);
                if !room {
                    break;
                }
                out = match self.run_continuation(session_id, hub) {
                    // The continuation met the wall too; `run_continuation`
                    // already compacted after it, so the room check above decides
                    // whether there is another continuation in this prompt.
                    Err(HarnessError::ContextWall { .. }) => continue,
                    other => other,
                };
                break;
            }
        }
        // **and the turn's own clock stops with it.** A start that outlived its turn would be
        // inherited by the next prompt's first round, so a fresh prompt would open with the
        // PREVIOUS turn's duration on the composer.
        if let Some(h) = self.open.get_mut(session_id) {
            h.end_turn_clock();
        }
        // **Every turn ends here, so this is where the idle clock starts.** `after_turn` is the
        // one convergence point for a prompt, a scripted submit and a monitor's wake — the same
        // reason the compaction lives here and not at three call sites.
        self.rearm_todo_nag(session_id);
        out
    }

    /// Start (or stand down) the idle plan-check for SESSION, as of NOW.
    ///
    /// **Armed only when the plan is unfinished AND different from the one last sent.** Both
    /// halves are load-bearing: a finished plan has nothing to check, and an unchanged one has
    /// already been said — re-sending it is the nagging the operator called overkill, just at a
    /// slower rate. So a model that ignores the check gets silence rather than a metronome, while
    /// any real work on the plan earns a fresh check at the next idle period.
    ///
    /// **The rows are handed over whole and `nag_should_arm` does the reading**, so the decision
    /// about what the check may ask about — a postponed row is not it — lives in one place rather
    /// than being pre-applied here and asserted somewhere else.
    fn rearm_todo_nag(&mut self, session_id: &str) {
        let rows = self
            .open
            .get(session_id)
            .map(|h| h.todo_list())
            .unwrap_or_default();
        if nag_should_arm(&rows, self.nagged.get(session_id).map(String::as_str)) {
            self.nag_due
                .insert(session_id.to_string(), Instant::now() + TODO_NAG_AFTER);
        } else {
            // nothing to check, or the same thing we already said
            self.nag_due.remove(session_id);
        }
    }

    /// The operator has spoken to SESSION: a new idle period, and the check may be made again.
    ///
    /// **This is the half of the rule the operator stated as a negative** — *"but certainly not
    /// after my message"*. Their prompt ends a turn, `after_turn` arms the clock, and the check
    /// cannot arrive until a minute of silence has passed since it; when they speak again, the
    /// plan check is *earned back* because whatever they said may have changed what the plan
    /// should be. Without this the session would be checked once ever, and a plan re-written by
    /// the operator's own instruction would never be checked at all.
    pub fn note_operator_prompt(&mut self, session_id: &str) {
        self.nagged.remove(session_id);
    }

    /// When the next check comes due, for the worker's blocking wait.
    ///
    /// NIL when no session has one armed — which is the common case, and the one that lets the
    /// worker sleep until there is real work rather than waking every minute on behalf of nothing.
    /// The next moment this daemon has something to do that is not work arriving: a check to
    /// deliver, or a **sweep for a tool call nothing will answer**.
    ///
    /// The sweep needs its own deadline and cannot rely on a nag, and that is not a detail: the
    /// worker sleeps until this returns, so a session with no check armed and no commands would
    /// never be swept at all — and the stall this exists for happens while a command is RUNNING,
    /// which is exactly when nothing else is due. Armed after every turn (see `arm_sweep`), so a
    /// turn that ends with a call nobody answered is looked at a moment later.
    pub fn next_nag_at(&self) -> Option<Instant> {
        match (self.nag_due.values().min().copied(), self.sweep_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, b) => b,
        }
    }

    /// Ask for a sweep a moment from now, so the idle arm gets a chance to look.
    ///
    /// A short grace rather than none: `dispatch` returns when the turn is over, and a sweep in the
    /// same instant would be looking at a transcript whose last append may not be published yet.
    pub fn arm_sweep(&mut self, after: std::time::Duration) {
        let at = Instant::now() + after;
        self.sweep_at = Some(match self.sweep_at {
            Some(prev) => prev.min(at),
            None => at,
        });
    }

    /// Send every check whose deadline has passed, and answer how many ran.
    ///
    /// A check is a TURN — the model reads the plan and can act on it — so it goes through the
    /// same tail every other turn does (`after_turn`), which is what re-arms or stands the clock
    /// down afterwards.
    /// **Sweep every session for a tool call nothing will ever answer.**
    ///
    /// Called from the daemon's own idle arm, which is the same place the todo check is delivered —
    /// and here that placement is not tidiness, it is the whole liveness test. The round loop is
    /// SYNCHRONOUS: a turn blocks inside `runtime.invoke` until the tool answers, so while a round
    /// is in flight this cannot run at all, because the worker is the thread the round is holding.
    /// **The fact that we are here is the fact that no round owns a call**, which is exactly the
    /// condition under which a call with no result is a call nobody will answer. No clock, no
    /// thread introspection, no new bookkeeping.
    ///
    /// MEASURED, or this would not exist: a `grep` whose executor thread and process had both
    /// vanished left the operator's head showing `running` for eight minutes, with `esc esc` dead
    /// because the interrupt stops GENERATION and generation had already ended.
    pub fn sweep_abandoned_calls(&mut self) -> usize {
        self.sweep_at = None;
        let ids: Vec<String> = self.open.keys().cloned().collect();
        let mut settled = 0;
        for id in ids {
            if let Some(h) = self.open.get_mut(&id) {
                settled += h.sweep_abandoned_calls();
            }
        }
        settled
    }

    pub fn deliver_due_nags(&mut self) -> usize {
        let now = Instant::now();
        let due: Vec<String> = self
            .nag_due
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(id, _)| id.clone())
            .collect();
        let mut ran = 0;
        for session_id in due {
            // Disarmed BEFORE the turn: `after_turn` re-arms from the turn's own end, so a
            // delivery that left the old deadline in place would fire again immediately.
            self.nag_due.remove(&session_id);
            let hub = self.registry.get(&session_id);
            let Some(harness) = self.open.get_mut(&session_id) else {
                continue;
            };
            let notice = harness.nag_notice();
            let out = match harness.nag_turn() {
                Ok(Some(reply)) => Ok(reply),
                // nothing to say after all — the plan moved between the arming and the clock
                Ok(None) => continue,
                Err(e) => Err(e),
            };
            if let Some(text) = notice {
                self.nagged.insert(session_id.clone(), text);
            }
            self.publish_title(&session_id);
            let out = self.after_turn(&session_id, &hub, out);
            match out {
                Ok(_) => ran += 1,
                Err(e) => eprintln!("  {session_id} · todo check -> {e}"),
            }
        }
        ran
    }

    /// One continuation turn after a wall, and everything that follows it.
    ///
    /// The same tail [`Sessions::run_prompt`] gives a prompt, because a
    /// continuation IS a prompt — the harness's own, not the operator's — and
    /// the last time this tail lived in two places, one of them was missing the
    /// compaction.
    fn run_continuation(
        &mut self,
        session_id: &str,
        hub: &Option<std::sync::Arc<Hub>>,
    ) -> Result<Reply, HarnessError> {
        let out = match self.open.get_mut(session_id) {
            Some(h) => h.continue_after_wall(),
            None => {
                return Err(HarnessError::Setup(
                    "the session closed before the continuation could run".into(),
                ));
            }
        };
        self.publish_title(session_id);
        self.arm_wake(session_id);
        if let Err(e) = &out {
            let turn_id = self
                .open
                .get(session_id)
                .map(|h| h.last_turn_id().to_string())
                .unwrap_or_default();
            if let Some(hub) = hub {
                publish_failure(hub, &turn_id, e);
            }
        }
        // A continuation is a turn: it can reach the wall, and it tidies after
        // itself — the room the compaction leaves is what the loop in
        // `after_turn` reads.
        let wall = matches!(out, Err(HarnessError::ContextWall { .. }));
        if out.is_ok() || wall {
            self.compact_if_at_the_wall(session_id);
        }
        out
    }

    /// **Compact when the next turn would not fit.** `docs/compaction.md` §1:
    /// the trigger is the wall and nothing else — depth was measured not to hurt
    /// quality — so the policy is *as late as possible*, and this is the last
    /// moment that is still safe.
    ///
    /// Why it is here and not inside the engine: compaction reaches the store,
    /// the resume chain and the registry, which is why `turn::compaction` says
    /// building the new base is the caller's act. This is that caller.
    ///
    /// Measured 2026-09-15, which is why it exists at all: a session reached
    /// ~244k of 262144 and the NEXT turn came back `500 Context size has been
    /// exceeded` with nothing recorded. Nothing compacted on its own, because
    /// until now nothing could — compaction was only ever `/compact` from a head,
    /// and by the time a person notices, the turn that would have told them has
    /// already failed.
    ///
    /// A failure here is announced and swallowed: the turn the operator asked for
    /// SUCCEEDED, and turning its reply into an error because the tidying
    /// afterwards did not work would lose the thing they wanted.
    ///
    /// Returns whether a compaction was actually ATTEMPTED — `false` when the
    /// session is not at the threshold, or automatic compaction is off, or no
    /// window is configured. A caller that has just told the operator "a
    /// compaction was attempted" needs that to be a fact rather than a hope.
    fn compact_if_at_the_wall(&mut self, session_id: &str) -> bool {
        // **The SESSION's config, not the daemon's.** The wall is a property of
        // the model this conversation is on and of how its tokens relate to the
        // ledger's — both of which are per session and neither of which the base
        // knows. `self.base` was answering for every session at once, so a
        // session on a metered provider was judged by the daemon's unmeasured
        // numbers. See `Config::ledger_scale`.
        let scale = self
            .open
            .get(session_id)
            .and_then(|h| h.config().ledger_scale);
        let (resident, window, headroom, due) = match self.open.get(session_id) {
            Some(h) => {
                let r = h.ledger_len() as u64;
                let c = h.config();
                // **Shown in the operator's units**, not the ledger's; see
                // `Config::shown_tokens`. The DECISION is still made on the ledger
                // figure (`should_compact` below), because that is what the
                // planning arithmetic is in — only the telling converts.
                (
                    c.shown_tokens(r),
                    c.shown_tokens(c.planning_window().unwrap_or(0)),
                    c.shown_tokens(c.headroom()),
                    c.should_compact(r),
                )
            }
            None => return false,
        };
        if !due {
            return false;
        }
        let hub = self.registry.get(session_id);
        // **Said where it can be seen, not only where a head would see it.** The
        // hub reaches attached heads; a one-shot has none, and a compaction it
        // could not see is precisely the "a session doing something the operator
        // did not see coming" the banner promises against. Verified 2026-09-15:
        // the fork was in the store and the terminal said nothing.
        // **And it NAMES THE SESSION, which is the one thing this line could not say.** The
        // log is one file every daemon appends to (`~/logs/harnessd.log`), so this line and
        // the two below it arrived with no way to tell which of seven sessions they were
        // about — the `→ http 400` lines carry an id and these, the ones that say what
        // happened, did not. Measured 2026-10-03: attributing a compaction failure took an
        // hour of archaeology across `ps`, window prints and the store, and the answer was
        // still inferred rather than read. The id costs fourteen bytes and makes it a
        // measurement.
        eprintln!(
            "  {session_id}: compacting: {resident} of {window} tokens resident, less than \
             the {} the next turn needs",
            headroom
        );
        if let Some(hub) = &hub {
            hub.publish(SessionEvent::Warning {
                code: "auto_compact".into(),
                detail: format!(
                    "{resident} of {window} tokens resident, leaving less than the \
                     {} the next turn needs — compacting now, as one more message so \
                     the prefix the server already holds is reused. This is the wall, \
                     not a judgement about the conversation.",
                    headroom
                ),

                compaction: None,
            });
        }
        match self.compact(session_id) {
            Ok(report) => {
                // The same account the operator gets from `/compact`. An automatic
                // compaction is the one they did NOT ask for, so saying what it did
                // matters more here, not less.
                if let Some(hub) = &hub {
                    hub.publish(SessionEvent::Warning {
                        code: "compacted".into(),
                        detail: compaction_said(&report, scale),
                        compaction: Some(Box::new(compaction_wire(
                            &report,
                            "compacted",
                            scale,
                            resident,
                            window,
                            headroom,
                        ))),
                    });
                }
                let after = self
                    .open
                    .get(session_id)
                    .map(|h| h.ledger_len() as u64)
                    .unwrap_or(0);
                // **If it did not help, stop trying.** A summary that is itself
                // over the threshold would compact again next turn, and again —
                // a loop that spends a turn each time and never lets the
                // conversation continue. Better to say so once and let the wall
                // be the wall: the operator can `/compact` by hand, shorten the
                // session, or raise the window.
                // The session's own judgement again, for the same reason.
                let still_due = self
                    .open
                    .get(session_id)
                    .map(|h| h.config().should_compact(after))
                    .unwrap_or(false);
                if still_due {
                    self.base.auto_compact = false;
                    if let Some(h) = self.open.get_mut(session_id) {
                        h.config_mut().auto_compact = false;
                    }
                    if let Some(hub) = &hub {
                        hub.publish(SessionEvent::Warning {
                            code: "auto_compact_no_progress".into(),
                            detail: format!(
                                "compacted from {resident} to {after} tokens and that is \
                                 STILL within {} of the {window} window, so automatic \
                                 compaction is now off for this session rather than \
                                 looping once per turn. The summary itself is near the \
                                 wall: start a fresh session, or raise --context-window \
                                 if the server really has more.",
                                headroom
                            ),

                            compaction: None,
                        });
                    }
                } else {
                    // Two facts, not one: the compaction happened, AND the summary
                    // it produced may be partial. Collapsing them into "compacted"
                    // is how a truncated record reaches the operator looking whole.
                    let cut = if report.fork.truncated {
                        " The summary was CUT OFF at the model's length limit — it is                          incomplete, and the base says so too."
                    } else {
                        ""
                    };
                    // `after` comes off the report in ledger tokens, like
                    // `resident`; both are converted so the pair can be compared.
                    let after = self
                        .harness_of(session_id)
                        .map(|h| h.config().shown_tokens(after))
                        .unwrap_or(after);
                    eprintln!(
                        "  {session_id}: compacted: {after} tokens resident now, was \
                         {resident}.{cut}"
                    );
                    if let Some(hub) = &hub {
                        hub.publish(SessionEvent::Warning {
                            code: "auto_compact".into(),
                            detail: format!(
                                "compacted: {after} tokens resident now, was {resident}.{cut}"
                            ),

                            compaction: None,
                        });
                    }
                }
            }
            Err(e) => {
                // Said on stderr too. The first run of this code printed
                // `compacting:` and then nothing at all, because the failure went
                // only to a hub with no head attached — a compaction that silently
                // did not happen is worse than one that never fired.
                //
                // **Path-neutral, because there are two callers now.** This used to
                // say "the turn you asked for succeeded; what failed is the tidying
                // after it" — true of the after-turn call sites and FALSE of the one
                // `run_prompt` makes BEFORE the send. There no turn ran, and the
                // next one will not "may hit the wall": it WILL, because this
                // compaction was the thing making room for it. A message describing
                // the wrong one of two callers is the defect `republish_after`
                // already names, one layer down.
                eprintln!("  {session_id}: compaction FAILED: {e}");
                if let Some(hub) = &hub {
                    hub.publish(SessionEvent::Warning {
                        code: "auto_compact_failed".into(),
                        detail: format!(
                            "the automatic compaction did not run: {e}. This session is \
                             over its context budget and nothing made room, so the next \
                             turn will hit the context wall. `/compact` retries it.",
                        ),

                        compaction: None,
                    });
                }
            }
        }
        // It ran. Whether it HELPED is the branch above, which says so in its own
        // words either way; what this answers is the narrower question a caller
        // asks before claiming an attempt was made.
        true
    }

    /// Read-only access to a session's harness, for a caller that wants the ledger
    /// or the prefix. `None` for a session nothing has run in yet.
    pub fn harness_of(&self, session_id: &str) -> Option<&Harness<'a>> {
        self.open.get(session_id)
    }

    /// Compact a session: the summary turn, then the transcript fork.
    ///
    /// The same shape as [`Sessions::submit`] — open-or-reuse the harness, run,
    /// publish a failure on the session's own log — because a compaction is a turn
    /// with a fork behind it, not a different kind of thing. No title derivation
    /// and no wake-arming: a summary proposes nothing and monitors do not fire on
    /// it.
    pub fn compact(&mut self, session_id: &str) -> Result<CompactReport, HarnessError> {
        let hub = self.registry.get(session_id);
        let harness = self.harness(session_id)?;
        let out = harness.compact();
        match out {
            Ok(r) => {
                // The fabric may have moved since the session opened; after a
                // compaction is the moment to say so, as an update, never as a
                // rewrite of message 0.
                if let Err(e) = self.refresh_fabric(session_id)
                    && let Some(hub) = &hub
                {
                    hub.publish(SessionEvent::Warning {
                        code: "fabric_refresh_failed".into(),
                        detail: e.to_string(),

                        compaction: None,
                    });
                }
                Ok(r)
            }
            Err(e) => {
                let turn_id = self
                    .open
                    .get(session_id)
                    .map(|h| h.last_turn_id().to_string())
                    .unwrap_or_default();
                if let Some(hub) = &hub {
                    publish_failure(hub, &turn_id, &e);
                }
                Err(e)
            }
        }
    }

    /// **Arm the monitor wake for a session that has declared one.** T24's
    /// *"wakes the loop when it fires"*, which had no caller.
    ///
    /// # Why a thread, and why it is not a poll
    ///
    /// `Monitors::wait_for_any` is a `Condvar`: a waiter costs nothing while
    /// nothing is happening, and it returns the monitors that settled **during the
    /// call**, which is why it takes a cursor — a caller that asked twice would
    /// otherwise be handed the same firing twice and act on it twice. The thread
    /// blocks in it and rings the bell. `Registry::next_work` is already blocked on
    /// that bell, so the worker wakes on the firing itself rather than on a clock.
    ///
    /// The deadline the waiter passes is **not** message delivery — the condvar is.
    /// It is how a blocked thread notices the registry closing, because
    /// `Bell::close` notifies the bell's condvar and not the monitors'. That is a
    /// shutdown re-check, and it is worth naming because "no timer, no poll loop"
    /// is a property this daemon states about itself (§18.1-I12) and a re-check
    /// that went undescribed would read as a violation of it.
    ///
    /// # Why it is lazy
    ///
    /// **No monitors, no thread** — the same rule `Monitors`' own poller keeps, and
    /// the same rule T24 exists to enforce: this entry is about watchers that
    /// accumulate. A session with an exec backend and nothing watching gets no
    /// waiter; the first `monitor` call gets it one, and it lives until the daemon
    /// stops.
    ///
    /// **One waiter per name**, tracked in `armed`. Two waiters on one registry
    /// would each ring for the same firing, and the second wake would find the
    /// cursor already advanced and run nothing — a wasted `next_work` round rather
    /// than a duplicated turn, but it is the shape of defect this fleet has paid
    /// for and it is cheap to make unspellable.
    pub fn arm_wake(&mut self, session_id: &str) {
        if self.armed.contains(session_id) {
            return;
        }
        let Some(h) = self.open.get_mut(session_id) else {
            return;
        };
        let Some(monitors) = h.monitors().cloned() else {
            return;
        };
        // Read, not assumed: arm only once something is actually watching.
        if monitors.live_names().is_empty() {
            return;
        }
        h.declare_monitor_wake();
        let bell = self.registry.bell().clone();
        let id = session_id.to_string();
        let spawned = std::thread::Builder::new()
            .name(format!("monitor-wake-{}", &id[..id.len().min(24)]))
            .spawn(move || {
                // The waiter's **own** cursor, advanced by what it has already rung
                // for. The harness has a second one, shared with that session's
                // steering source, which decides what is actually delivered — a
                // firing picked up mid-turn is not delivered again by the wake.
                // Two cursors because they answer two questions: "have I rung for
                // this?" and "has the model seen this?"
                let mut notified = monitors.settled_count();
                while !bell.is_closed() {
                    let fired = monitors.wait_for_any(notified, SHUTDOWN_RECHECK);
                    if bell.is_closed() {
                        break;
                    }
                    if fired.is_empty() {
                        continue;
                    }
                    notified += fired.len();
                    bell.ring_wake(&id);
                }
            });
        match spawned {
            Ok(_) => {
                self.armed.insert(session_id.to_string());
            }
            Err(e) => {
                // Not fatal and not silent. A session whose wake could not be armed
                // still has monitors and `job_list` still shows them; what it does
                // not have is the wake, and saying so is the difference between a
                // poll and a poll nobody was told about.
                if let Some(hub) = self.registry.get(session_id) {
                    hub.publish(SessionEvent::Warning {
                        code: "monitor_wake_not_armed".into(),
                        detail: format!(
                            "this session's monitors will be POLLED, not woken: the waiter \
                             thread could not be started ({e}). A condition that fires \
                             between turns reaches the model only when something calls \
                             `job_list`."
                        ),

                        compaction: None,
                    });
                }
            }
        }
    }

    /// **Something fired while nothing was running.** The worker's half of the wake.
    ///
    /// `Outcome::Ignored` is the honest answer to a wake that raced a mid-turn
    /// pickup: the steering source and the harness share a cursor, so the firing
    /// had already reached the model and running a turn about it again would be
    /// telling the model the same thing twice.
    ///
    /// # A session the daemon does not HOLD is not a session nobody can serve
    ///
    /// This is R58's finding, and the correction is the operator's design in their own words:
    /// *"think about it like it is an erlang supervision tree. we talk to parents and they own
    /// lifecycle."* A subagent's harness is built inside the runner's thread and adopted into the
    /// `registry` — which is exactly what lets a head peek at it and attach to it — and it is
    /// **not** in `open`, so `Sessions::wake` cannot run its turn. What that used to mean was
    /// that a ring naming a child was a condition that fires and is discarded, which made depth
    /// 2 a hole: a grandchild's settlement reached the bell and stopped there.
    ///
    /// So the daemon HANDS IT ON. [`Hub::wake_its_own_reader`] wakes the hub's own condvar — the
    /// one the thread that runs that session is blocked in — and the wake is answered there by
    /// `harness::serve_child`, which calls [`crate::harness::Harness::wake`] exactly as this
    /// function does for a root. **Every session that can start a child is now a session the
    /// daemon can serve**; it is served by the thread that owns it, and the ring names the
    /// session that owns the settlement (its parent) rather than the tree's root.
    ///
    /// `Ignored` is left for what it honestly means: the session is gone from the registry
    /// altogether. Nothing is lost when that happens — a session that has closed handed its
    /// undrained settlements up a level first (`jobwatch::JobWatchers::stop`).
    pub fn wake(&mut self, session_id: &str) -> Outcome {
        let hub = self.registry.get(session_id);
        // **The decision is `wake_route`'s, so it can be asserted without a daemon.** The two
        // facts it reads are asked of the layers that own them: `open` is this struct's, the
        // registry is the daemon's.
        match wake_route(self.open.contains_key(session_id), hub.is_some()) {
            WakeRoute::Drive => {}
            WakeRoute::ItsOwnReader => {
                let handed = hub.map(|h| h.wake_its_own_reader()).unwrap_or(false);
                return if handed {
                    Outcome::HandedOn
                } else {
                    Outcome::Ignored
                };
            }
            WakeRoute::Gone => return Outcome::Ignored,
        }
        let Some(harness) = self.open.get_mut(session_id) else {
            return Outcome::Ignored;
        };
        let woke = harness.wake();
        // `Ignored` is not a turn: nothing ran, nothing grew, and there is nothing
        // to tidy after it.
        let Some(out) = (match woke {
            Ok(None) => None,
            Ok(Some(reply)) => Some(Ok(reply)),
            Err(e) => Some(Err(e)),
        }) else {
            return Outcome::Ignored;
        };
        self.publish_title(session_id);
        // A monitor's turn is a turn: it appends rows, it can reach the wall, and a
        // session that only ever woke would have compacted never. The wall's
        // continuation is in there too, so a monitor's work resumes the same way a
        // prompt's does.
        let out = self.after_turn(session_id, &hub, out);
        match out {
            Ok(reply) => Outcome::Replied(Box::new(reply)),
            Err(e) => Outcome::Failed(e.to_string()),
        }
    }

    /// Run one command against its session.
    pub fn dispatch(&mut self, session_id: &str, cmd: &QueuedCommand) -> Outcome {
        let hub = self.registry.get(session_id);
        match &cmd.kind {
            CommandKind::Prompt { text } => {
                // One line, because everything a turn needs around it — the title,
                // the wake, the failure notice, the compaction at the wall — is in
                // `run_prompt`, which the scripted path also calls. This arm used
                // to be the second copy of that sequence, and the copy was missing
                // the compaction.
                match self.run_prompt(session_id, text) {
                    Ok(reply) => Outcome::Replied(Box::new(reply)),
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            // A compaction is a whole-session act and runs through
            // [`Sessions::compact`], as a prompt runs through
            // [`Sessions::submit`] — which also opens its harness, so nothing here
            // holds one.
            CommandKind::Compact => match self.compact(session_id) {
                Ok(r) => {
                    // **A compaction that re-seated says so.** It forks onto the
                    // prompt this daemon seats now, so a conversation older than a
                    // tool picks that tool up by compacting — and a capability
                    // that appears silently is one nobody uses.
                    if let Some(hub) = &hub
                        && !(r.gained.is_empty() && r.lost.is_empty())
                    {
                        let mut said =
                            "this compaction also re-seated: the new prompt announces the \
                             tools this daemon seats now"
                                .to_string();
                        if !r.gained.is_empty() {
                            said.push_str(&format!(
                                ". The model can now call: {}",
                                r.gained.join(", ")
                            ));
                        }
                        if !r.lost.is_empty() {
                            said.push_str(&format!(". It has lost: {}", r.lost.join(", ")));
                        }
                        said.push_str(
                            ". `/reseat` would have cost a second summary turn and a second \
                             cold prefill; this one is already paid for.",
                        );
                        hub.publish(SessionEvent::Warning {
                            code: "reseated".into(),
                            detail: said,

                            compaction: None,
                        });
                    }
                    if let Some(hub) = &hub {
                        let scale = self
                            .harness_of(session_id)
                            .and_then(|h| h.config().ledger_scale);
                        // The same three numbers the sentence was built from, read off
                        // the harness rather than recomputed: a report that judged the
                        // window differently from the decision would be two answers to
                        // one question.
                        let (resident, window, headroom) = self
                            .harness_of(session_id)
                            .map(|h| {
                                let c = h.config();
                                (
                                    c.shown_tokens(h.ledger_len() as u64),
                                    c.shown_tokens(c.planning_window().unwrap_or(0)),
                                    c.shown_tokens(c.headroom()),
                                )
                            })
                            .unwrap_or((0, 0, 0));
                        hub.publish(SessionEvent::Warning {
                            code: "compacted".into(),
                            detail: compaction_said(&r, scale),
                            compaction: Some(Box::new(compaction_wire(
                                &r,
                                "compacted",
                                scale,
                                resident,
                                window,
                                headroom,
                            ))),
                        });
                    }
                    Outcome::Compacted(Box::new(r))
                }
                Err(e) => Outcome::Failed(e.to_string()),
            },
            // A re-seat is a compaction that lands on a different prompt, so it
            // runs through the same door and reports through the same one. What it
            // adds is the sentence naming which tools the model can call now that
            // it could not before — the reason anybody types this.
            CommandKind::Reseat { summarise } => {
                // Two ways to change message zero, and the difference is what
                // happens to everything under it: `reseat` pays a summary turn,
                // `reingest` carries the conversation across and pays a cold
                // prefill instead.
                let summarise = *summarise;
                let out = match self.harness(session_id) {
                    Ok(h) => {
                        if summarise {
                            h.reseat()
                        } else {
                            h.reingest()
                        }
                    }
                    Err(e) => Err(e),
                };
                match out {
                    Ok(r) => {
                        if let Some(hub) = &hub {
                            let mut said = if summarise {
                                format!(
                                    "this conversation now speaks the prompt this daemon \
                                     seats: {} tokens of history replaced by a {}-token \
                                     summary, and the tool list rebuilt",
                                    r.fork.was_tokens, r.fork.base_tokens
                                )
                            } else {
                                format!(
                                    "this conversation now speaks the prompt this daemon \
                                     seats: the tool list was rebuilt and all {} tokens of \
                                     history carried across as they are — nothing \
                                     summarised, nothing dropped",
                                    r.fork.base_tokens
                                )
                            };
                            let gained = r.gained.clone();
                            let lost = r.lost.clone();
                            if !gained.is_empty() {
                                said.push_str(&format!(
                                    ". The model can now call: {}",
                                    gained.join(", ")
                                ));
                            }
                            if !lost.is_empty() {
                                said.push_str(&format!(". It has lost: {}", lost.join(", ")));
                            }
                            said.push_str(if summarise {
                                ". The server's cache for the new prompt is cold, so the \
                                 next turn prefills from scratch — that is what changing \
                                 the announced tools costs."
                            } else {
                                ". The server's cache for the new prompt is cold, so the \
                                 next turn prefills the WHOLE conversation from scratch — \
                                 that is what keeping it costs. `/reseat summarise` is the \
                                 cheaper, lossy way."
                            });
                            // **A summarising re-seat is a compaction**, so it carries
                            // the structure too: the record it produced is the base
                            // every later turn reads, and a head drawing a row for the
                            // tool-list change should be able to draw the same row it
                            // draws for `/compact`. A bare re-seat is not a compaction —
                            // nothing was summarised — and carries none, which is why
                            // ``because`` is empty rather than ``local_model``.
                            let compaction = summarise.then(|| {
                                let c = self
                                    .harness_of(session_id)
                                    .map(|h| h.config().clone())
                                    .unwrap_or_else(|| self.base.clone());
                                let resident = c.shown_tokens(r.fork.was_tokens as u64);
                                Box::new(compaction_wire(
                                    &crate::harness::CompactReport {
                                        fork: r.fork.clone(),
                                        summary_turn: r.summary_turn.clone(),
                                        gained: Vec::new(),
                                        lost: Vec::new(),
                                        summary_was_streamed: true,
                                    },
                                    "reseated",
                                    c.ledger_scale,
                                    resident,
                                    c.shown_tokens(c.planning_window().unwrap_or(0)),
                                    c.shown_tokens(c.headroom()),
                                ))
                            });
                            hub.publish(SessionEvent::Warning {
                                code: "reseated".into(),
                                detail: said,
                                compaction,
                            });
                        }
                        Outcome::Compacted(Box::new(crate::harness::CompactReport {
                            fork: r.fork,
                            summary_turn: r.summary_turn,
                            gained: r.gained,
                            lost: r.lost,
                            // A re-seat runs the ordinary compaction, which streams.
                            summary_was_streamed: true,
                        }))
                    }
                    Err(e) => {
                        if let Some(hub) = &hub {
                            hub.publish(SessionEvent::Warning {
                                code: "reseat_refused".into(),
                                detail: e.to_string(),

                                compaction: None,
                            });
                        }
                        Outcome::Failed(e.to_string())
                    }
                }
            }
            CommandKind::Interrupt { reason } => {
                // **A stop delivered to a session the daemon does not hold belongs to the thread
                // that owns it.** Both readers take from ONE queue (`Hub::take_own_work` for the
                // thread that runs the session, `Hub::try_command` for the worker), so the worker
                // can win an interrupt a parent aimed at its child — and its answer would be
                // *"nothing was generating"*, which for a subagent that is mid-turn in its own
                // thread is false. This is not about `Prompt`, which the daemon CAN serve by
                // opening the session lazily: an interrupt has nothing to act on unless the turn
                // already running is reached, and the daemon cannot reach one it does not hold.
                // So it is put back for that thread, which is the same door a relayed wake takes.
                if !self.open.contains_key(session_id)
                    && let Some(hub) = &hub
                    && hub.give_back_to_its_own_reader(cmd.clone())
                {
                    return Outcome::HandedOn;
                }
                // **AND THE SUBTREE GOES FIRST, whichever session this is.** A stop is a stop
                // whether or not a turn is running: the children this session owns are stopped
                // before it, so nothing is left computing for nobody — the operator's design in
                // one line, *"we talk to parents and they own lifecycle"*.
                if let Some(h) = self.open.get_mut(session_id) {
                    h.stop_children();
                }
                if let Some(hub) = &hub {
                    hub.publish(SessionEvent::Warning {
                        code: "interrupt_idle".into(),
                        detail: format!(
                            "interrupt ({reason}) arrived between turns; nothing was generating"
                        ),

                        compaction: None,
                    });
                }
                Outcome::Ignored
            }
            // A take-back reaching the worker means the turn ended before the
            // steering poll saw it: whatever prompts it named have already run
            // as their own turns or are about to, and are no longer the
            // operator's to take back. Quietly nothing — the head cleared its
            // own echo when it recalled the line, and a warning here would be
            // noise about a no-op.
            // **The operator's todos onto the board — ONE LIST, TWO AUTHORS.**
            //
            // The operator's ruling: *"the existing getter should return mine and yours, and the
            // rest is also the same. the only difference is who created and that is it."* So this
            // is not a second list on the daemon side: it is the other half of the SAME board, and
            // `TodoBoard::snapshot` is their union. Everything that reads the board then sees the
            // operator's rows for free — the pane's `Todos` reply, the prompt the model is sent, and
            // the idle NAG (`Harness::nag_notice` → `unfinished_plan`), which is the whole reason
            // this exists: until now the nag could only ever fire for work the MODEL had written.
            CommandKind::SetOperatorTodos { items } => {
                let converted: Vec<letibot_tokencore::store::TodoItem> = items
                    .iter()
                    .map(|e| letibot_tokencore::store::TodoItem {
                        content: e.content.clone(),
                        // **The wire's condition, as the STORE spells it** — the other half of
                        // `todo_entry`'s conversion, and a `match` for the same reason: the two
                        // crates own the type separately, so a new variant must be named here.
                        when: e.when.as_ref().map(|c| match c {
                            letibot_sessionlog::event::TodoCondition::Job { handle } => {
                                letibot_tokencore::store::TodoCondition::Job {
                                    handle: handle.clone(),
                                }
                            }
                        }),
                        status: match e.status {
                            letibot_sessionlog::event::TodoStatus::Pending => {
                                letibot_tokencore::store::TodoStatus::Pending
                            }
                            letibot_sessionlog::event::TodoStatus::InProgress => {
                                letibot_tokencore::store::TodoStatus::InProgress
                            }
                            letibot_sessionlog::event::TodoStatus::Completed => {
                                letibot_tokencore::store::TodoStatus::Completed
                            }
                            letibot_sessionlog::event::TodoStatus::Postponed => {
                                letibot_tokencore::store::TodoStatus::Postponed
                            }
                        },
                        by: letibot_tokencore::store::TodoBy::Operator,
                    })
                    .collect();
                // The head is the source of truth for its own rows, so its list REPLACES that half.
                let n = converted.len();
                if let Some(h) = self.open.get_mut(session_id) {
                    h.set_operator_todos(converted);
                }
                let _ = n;
                // **ARMING HERE IS THE WHOLE POINT OF A ROW THE OPERATOR ADDS.** The idle clock used
                // to start in exactly one place — the end of a turn (`after_turn`) — so a row added
                // to an IDLE session had no clock at all and was never checked: the model is quiet,
                // the operator has just said what they want done, and nothing ever reminds it. The
                // operator found it the direct way: *"my todos will be reminded to a model?"*, and
                // on a resumed session, *"this is exactly what i see with rano"*.
                //
                // It is the same arm the turn end makes, deliberately: a board that MOVED is a board
                // worth checking, and `rearm_todo_nag` already decides — an unchanged plan is not
                // re-armed, so a head that re-pushes the same rows (any add, any delete, every
                // HELLO) does not become a metronome.
                self.rearm_todo_nag(session_id);
                Outcome::Ignored
            }
            CommandKind::WithdrawPrompts => Outcome::Ignored,
            // The request is honoured mid-turn by the `bash` wait loop, which reads
            // the hub's promote channel. Reaching here means nothing was running, so
            // the request is stale — clear it and say so, rather than leaving it for
            // the next command to promote itself unprompted.
            // **The operator's own call, admitted before it runs** — R24 part two.
            //
            // The admission is written here, by the worker, through the same corpus sink the
            // gate writes every other decision through. The allowlist was already enforced on
            // the connection's thread (a name outside it never reaches this queue), so what is
            // left is the record and the permission to go.
            CommandKind::OperatorCall {
                call_id,
                name,
                arguments,
                who,
                execute,
            } => {
                let call_id = call_id.clone();
                let name = name.clone();
                let arguments = arguments.clone();
                let who = who.clone();
                let admitted = match self.open.get_mut(session_id) {
                    Some(h) => h.admit_operator_call(&call_id, &name, &arguments, &who),
                    None => Err(format!("session {session_id} is not open")),
                };
                match admitted {
                    Ok(()) => {
                        // **Pending until the result arrives**, keyed by the HEAD that asked —
                        // so a head that dies between the two frames leaves a sentence rather
                        // than a silent admission. See `Hub::detach`.
                        //
                        // A daemon-run call is noted too and then cleared by the run below,
                        // rather than being a special case: the pending set means *an
                        // admission this daemon is holding*, and a call the daemon runs and
                        // finishes in the same arm has one for the length of that arm.
                        if let Some(hub) = &hub {
                            hub.note_operator_call(&call_id, &cmd.head_id, &name, &who);
                        }
                        // **R31: the daemon runs it, when the head asked it to.** The head
                        // that asked has no tool runtime and no HTTP client; the daemon has
                        // both, and — the reason that is not merely convenient — the payload
                        // then comes from *this* program, bounded by this session's byte
                        // caps and spill policy. A head fetching a page itself would write a
                        // corpus row saying the operator ran `web_fetch` about another
                        // program's answer.
                        if *execute {
                            let said = match self.open.get_mut(session_id) {
                                Some(h) => h.run_operator_call(&call_id, &name, &arguments, &who),
                                None => Err(format!("session {session_id} is not open")),
                            };
                            if let Some(hub) = &hub {
                                hub.take_operator_call(&call_id);
                            }
                            if let Err(e) = said {
                                return Outcome::Failed(e);
                            }
                        }
                        Outcome::Ignored
                    }
                    Err(e) => Outcome::Failed(e),
                }
            }
            // **The operator's shell line, run by this daemon and recorded as two rows**
            // (`! ls .` → the operator's own `User` row + a `bash` `ToolResult` with
            // `origin: Operator`). Between turns it runs here; mid-turn the round
            // boundary takes it (`apply_queued_head_run`), which is the door's own
            // timing and for its own reason.
            CommandKind::OperatorShell { line, who } => {
                let line = line.clone();
                let who = who.clone();
                let ran = match self.open.get_mut(session_id) {
                    Some(h) => h.run_operator_shell(&line, &who),
                    None => Err(format!("session {session_id} is not open")),
                };
                if let Err(e) = ran {
                    return Outcome::Failed(e);
                }
                // **And the turn its rows are for** — the operator's correction, in their
                // words: *"my commands should start a turn and should be printed to me"*.
                // The rows above are the printing; this is the turn. `Ignored` here was
                // the whole defect: the deposit sat in the transcript until something else
                // started a turn, which is a command nobody answered.
                let out = match self.open.get_mut(session_id) {
                    Some(h) => h.run_after_operator_shell(),
                    None => return Outcome::Failed(format!("session {session_id} is not open")),
                };
                self.publish_title(session_id);
                // A `!` turn is a turn: it appends rows, it can reach the wall, and a
                // session that only ever ran shell lines would have compacted never — the
                // same routing `wake` uses, for the same reason.
                let out = self.after_turn(session_id, &hub, out);
                match out {
                    Ok(reply) => Outcome::Replied(Box::new(reply)),
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            // **What it produced.** The row goes in with its `origin` set, so every head
            // draws it as the person's act and the model sees the result.
            CommandKind::OperatorResult {
                call_id,
                outcome,
                payload,
            } => {
                let call_id = call_id.clone();
                let outcome = outcome.clone();
                let payload = payload.clone();
                // The pending entry is the admission's other half: a `call_id` this daemon
                // never admitted is refused by name rather than appended, because a row with
                // no admission behind it is a row nothing can be checked against.
                let pending = match &hub {
                    Some(h) => h.take_operator_call(&call_id),
                    None => None,
                };
                let Some((name, who)) = pending else {
                    return Outcome::Failed(format!(
                        "no admitted operator call `{call_id}` is pending in this session, so \
                         its result was not appended. A row whose admission is missing is a \
                         row nothing stands behind."
                    ));
                };
                let appended = match self.open.get_mut(session_id) {
                    Some(h) => h
                        .finish_operator_call(&call_id, &name, &who, outcome, &payload)
                        .map_err(|e| e.to_string()),
                    None => Err(format!("session {session_id} is not open")),
                };
                match appended {
                    Ok(()) => Outcome::Ignored,
                    Err(e) => Outcome::Failed(e),
                }
            }
            // **A parent's message that lost its race with the child's turn.** The runner
            // refuses these by name when the child is not running (`HarnessTaskRunner::send`),
            // so reaching the between-turn worker means the turn ended between that check and
            // this drain. Said rather than dropped: the parent was told the message was
            // accepted, and this sentence is the only thing that corrects that.
            CommandKind::Message { from, text } => {
                if let Some(hub) = &hub {
                    let said = match text.chars().count() > 200 {
                        true => format!("{}…", text.chars().take(200).collect::<String>()),
                        false => text.clone(),
                    };
                    hub.publish(SessionEvent::Warning {
                        code: "message_idle".into(),
                        detail: format!(
                            "a message from `{from}` arrived after the turn it was meant to \
                             steer had ended, so nothing will deliver it: {said}. The parent's \
                             `task_message` was accepted and did not land — its child's answer \
                             is what `task_result` reads."
                        ),
                        compaction: None,
                    });
                }
                Outcome::Ignored
            }
            CommandKind::Promote => {
                if let Some(hub) = &hub {
                    hub.take_promote_request();
                    hub.publish(SessionEvent::Warning {
                        code: "promote_idle".into(),
                        detail: "a background request arrived between turns; nothing \
                                 was running to move"
                            .into(),

                        compaction: None,
                    });
                }
                Outcome::Ignored
            }
            // **An answer that got this far had nowhere better to go.** A session
            // with an adjudicator installs an `AnswerSink`, and `Hub::submit` then
            // delivers straight to the thread waiting for it rather than queueing —
            // see `crate::answers` for why the queue is a deadlock. So reaching this
            // arm means one of two things, and the message says which rather than
            // repeating a sentence about M1 that stopped being true when roles became
            // reachable.
            CommandKind::Answer { req_id, .. } => {
                if let Some(hub) = &hub {
                    let detail = match hub.answer_sink_describes() {
                        Some(who) => format!(
                            "an answer to {req_id} reached the command queue even though \
                             this session can be answered ({who}). That is a defect: the \
                             answer was not delivered to whatever asked, and the decision \
                             is still open or has already timed out. Nothing was run."
                        ),
                        None => format!(
                            "an answer to {req_id} arrived and this session has nothing \
                             that asks: its tools are read-only, and a read never prompts \
                             (clause 4). Nothing was waiting for it and nothing was run."
                        ),
                    };
                    hub.publish(SessionEvent::Warning {
                        code: "answer_unclaimed".into(),
                        detail,

                        compaction: None,
                    });
                }
                Outcome::Ignored
            }
            CommandKind::Slash { line } => {
                let line = line.clone();
                let reply = self.slash(session_id, &line);
                if let Some(hub) = &hub {
                    hub.publish(SessionEvent::Warning {
                        code: if reply.ok {
                            "slash".into()
                        } else {
                            "slash_refused".into()
                        },
                        detail: format!("/{line}\n{}", reply.lines.join("\n")),

                        compaction: None,
                    });
                }
                if reply.ok {
                    Outcome::Ignored
                } else {
                    Outcome::Failed(format!("/{line} was refused"))
                }
            }
            // **A read of a job's output, for the pane that asked.** The same read
            // `/job ID` does, answered with the offsets **beside** the text instead
            // of folded into a footer sentence — the jobs pane draws its own header
            // and its own paging. Published on the log like every other verb's
            // reply, so both heads see the window the operator opened and neither
            // has to be the one that asked.
            CommandKind::ReadJobOutput { job, offset } => {
                let job = job.clone();
                let offset = *offset;
                let window = match self.harness_of(session_id) {
                    Some(h) => h.job_output_window(&job, offset, crate::harness::JOB_OUTPUT_WINDOW),
                    None => Err(format!("session {session_id} is not open")),
                };
                if let Some(hub) = &hub {
                    match window {
                        Ok(w) => hub.publish(SessionEvent::JobOutput {
                            job: job.clone(),
                            from: w.from,
                            to: w.to,
                            produced: w.produced,
                            dropped: w.dropped,
                            state: w.state,
                            never_ran: w.never_ran,
                            lines: w.lines,
                            next: w.next,
                        }),
                        // A refused read is a `Warning`, the same as a refused slash,
                        // and the code says which read it was: the pane renders it as
                        // a line rather than a window.
                        Err(e) => hub.publish(SessionEvent::Warning {
                            code: "job_output_refused".into(),
                            detail: e,

                            compaction: None,
                        }),
                    };
                }
                Outcome::Ignored
            }
            CommandKind::Mode { name, consented } => {
                let mode = match letibot_tools::mode::Mode::parse(name) {
                    Ok(m) => m,
                    Err(e) => {
                        if let Some(hub) = &hub {
                            hub.publish(SessionEvent::Warning {
                                code: "mode_unknown".into(),
                                detail: e,

                                compaction: None,
                            });
                        }
                        return Outcome::Failed("unknown mode".into());
                    }
                };
                // The project root this session is confined to, read off the harness
                // (the session's own, not the daemon's start directory), then cloned so
                // the harness borrow ends before the store write below.
                let workspace = {
                    let Some(harness) = self.harness_of(session_id) else {
                        return Outcome::Failed(format!("session {session_id} is not open"));
                    };
                    harness.workspace().to_path_buf()
                };
                // **A consented `allow-all` is never written to the row.**
                //
                // The row is what every LATER session in this project opens at, and
                // `allow-all` needs a confinement that a bare host cannot supply. So
                // writing it here is writing a point that refuses at every future
                // open — which is exactly what happened in leticl on 2026-09-20: the
                // session move failed the prerequisite, the row was written anyway,
                // and from then on every subagent died at open with *"a confinement
                // for exec"*. The operator saw *"subagents dont work"*.
                //
                // Consent is a thing a person gave once, in front of one session,
                // and `Harness::set_mode_consented` keeps it there. Persisting it
                // would be replaying their answer at every later start, for a
                // boundary claim nobody re-made.
                let persist =
                    !(*consented && mode.name == letibot_tools::mode::Mode::ALLOW_ALL.name);
                if !persist && let Some(hub) = &hub {
                    hub.publish(SessionEvent::Warning {
                        code: "mode_session_only".into(),
                        detail: format!(
                            "`allow-all` is this session's, not {}'s: nothing confines \
                             this box, so the point stands on the confirmation you just \
                             gave and is not written to the project store. A new session \
                             here starts where it did before, and asks again.",
                            workspace.display()
                        ),

                        compaction: None,
                    });
                }
                if let Err(e) = (if persist {
                    self.parts.mode_store.write().unwrap().set(&workspace, mode)
                } else {
                    Ok(())
                }) {
                    if let Some(hub) = &hub {
                        hub.publish(SessionEvent::Warning {
                            code: "mode_unpersisted".into(),
                            detail: format!(
                                "could not record `{}` for {}: {e}",
                                mode.name,
                                workspace.display()
                            ),

                            compaction: None,
                        });
                    }
                    return Outcome::Failed(format!("persisting mode: {e}"));
                }
                // **And this session moves too.** The row above is the project's
                // point for every session that opens later; this is the one in
                // front of the operator, which used to be told it would keep the
                // point it opened at. True, and asked about three times, because
                // nobody typing `/mode automode` means "next time". The gate reads
                // its mode at decision time, so the move is cheap; what it has to
                // get right is in `Harness::set_mode` — the prerequisite check the
                // open would have run, the grants taken under the old point, and
                // the guard model for a point whose decider it is.
                //
                // A refusal keeps the SESSION where it was and says why; the project
                // row stays written, because the next session may well be able to
                // carry the point this one cannot (a `coder` with no shell cannot go
                // to `allow-all`; a daemon started with `--bash` can).
                let moved = match self.open.get_mut(session_id) {
                    // The operator's answer travels with the command, and only
                    // this call reads it: the project row above is written from
                    // the NAME, which stays `allow-all` and stays refused for the
                    // next daemon. Consent is for the session in front of them.
                    // **Which point it actually landed on**, not which one was
                    // asked for. A consented `allow-all` becomes
                    // `allow-all (this box, consented)`, and the sentence below
                    // used to name the requested one and print ITS summary — so a
                    // bare host was told "the VM is the boundary and nothing inside
                    // it reaches this box", two lines above the same card saying
                    // the confinement prerequisite refuses that point here.
                    Some(h) => h
                        .set_mode_consented(mode, *consented)
                        .map(|said| (said, h.config().mode)),
                    None => Err("this session has no harness open".into()),
                };
                if let Some(hub) = &hub {
                    let _ = match moved {
                        Ok((said, applied)) => hub.publish(SessionEvent::Warning {
                            code: "mode_set".into(),
                            // **Say what is true of the project separately from what
                            // is true of this session**, because with a consented
                            // `allow-all` they differ. This sentence claimed the row
                            // had been written and that every later session would
                            // start there — neither of which happens when `persist`
                            // is false — and then printed the REQUESTED point's
                            // summary over the applied one.
                            detail: {
                                let row = self
                                    .parts
                                    .mode_store
                                    .read()
                                    .ok()
                                    .map(|st| st.for_project(&workspace).name)
                                    .unwrap_or(mode.name);
                                format!(
                                    "{said}. {}. {}",
                                    if persist {
                                        format!(
                                            "{} is at `{}` from every later session too",
                                            workspace.display(),
                                            mode.name
                                        )
                                    } else {
                                        format!(
                                            "{}'s row is unchanged at `{row}`, so a new \
                                             session here starts there",
                                            workspace.display()
                                        )
                                    },
                                    applied.summary
                                )
                            },

                            compaction: None,
                        }),
                        Err(why) => hub.publish(SessionEvent::Warning {
                            code: "mode_set_next_session_only".into(),
                            detail: format!(
                                "{} is set to `{}` for every LATER session in this \
                                 project; this one stays where it opened, because it \
                                 cannot carry that point: {why}",
                                workspace.display(),
                                mode.name
                            ),

                            compaction: None,
                        }),
                    };
                }
                Outcome::Ignored
            }
        }
    }
}

/// Announce a failed turn on the session's own log, as a **terminal** event.
///
/// `crates/ui/DESIGN.md` §4.5, paid. This used to publish a `Warning` and nothing
/// else, so a head's `TurnState` stayed `Running` for the rest of the session and
/// its spinner span at a dead turn — the head covered for it with *"nothing
/// received for 17.0s"*, which cannot tell a failed turn from a slow one. Only the
/// party that saw the error can, and that party is this function.
///
/// Both frames go out: `TurnFailed` is what stops the spinner and puts the reason
/// under the turn, and the `Warning` is what puts it in the scrollback with a code
/// somebody can grep the log for. They are not the same disclosure — one is state,
/// the other is history — and neither is a substitute for the other.
fn publish_failure(hub: &Arc<Hub>, turn_id: &str, e: &HarnessError) {
    hub.publish(SessionEvent::TurnFailed {
        turn_id: turn_id.to_string(),
        error: e.to_string(),
        // §5.7 commits nothing on a failed turn, so there is no partial to keep.
        // The field is here so a future failure that *does* keep one cannot arrive
        // looking like this.
        partial_kept: false,
    });
    hub.publish(SessionEvent::Warning {
        code: "turn_failed".into(),
        detail: e.to_string(),

        compaction: None,
    });
}

/// The store, as somewhere a registry can find sessions it is not holding.
///
/// Its **own** connection, behind a `Mutex`. Not the `Harness`'s: `rusqlite`'s
/// `Connection` is `Send` and not `Sync`, the registry is shared across every
/// connection thread, and a head listing sessions must not be able to block a turn
/// that is writing rows. SQLite in WAL mode is built for exactly this — one writer,
/// many readers — so a second connection is the cheap answer rather than a
/// compromise.
pub struct StoreSessions {
    store: Mutex<Store>,
    wiring: SessionWiring,
}

impl StoreSessions {
    /// `None` when the daemon has no store: there is then nothing on disk to list,
    /// and a source that answered "no sessions" would be indistinguishable from a
    /// store that is empty.
    pub fn open(cfg: &Config) -> Option<Arc<StoreSessions>> {
        let path = cfg.store.as_ref()?;
        let store = Store::open(path).ok()?;
        Some(Arc::new(StoreSessions {
            store: Mutex::new(store),
            wiring: Sessions::wiring(cfg),
        }))
    }
}

/// **The same second connection, answering a row the view has trimmed** (R19.2b).
///
/// One instance does both jobs because they are one store asked two questions, and
/// because a third connection would be a third thing to keep in WAL's reader set. The
/// daemon passes this same `Arc` to `Registry::set_source` and `Registry::set_row_source`.
/// **R11's locator, against the store** — leticl's ask, second half.
///
/// The same shape as [`RowSource`]: one implementation over the daemon's second connection,
/// set once at startup, and a registry with none answers `None` exactly as it did before.
impl letibot_sessionlog::registry::DiagnosticSource for StoreSessions {
    fn diagnostic(
        &self,
        request_id: &str,
        kind: letibot_sessionlog::protocol::DiagnosticKind,
    ) -> Option<String> {
        // The wire name and the column name are two vocabularies on purpose: the first is what
        // a head asks for, the second is what the store has always called it. The map lives
        // here, in the one place that knows both, rather than being flattened into either.
        let column = match kind {
            letibot_sessionlog::protocol::DiagnosticKind::Brief => "brief",
            letibot_sessionlog::protocol::DiagnosticKind::Reply => "reply",
        };
        let store = self.store.lock().ok()?;
        store.diagnostic(request_id, column).ok().flatten()
    }
}

impl letibot_sessionlog::registry::RowSource for StoreSessions {
    fn row_body(&self, session_id: &str, row: usize) -> Option<String> {
        // A session is not going to have four billion rows; a `usize` that does not fit is
        // a request for something that cannot exist, and `None` is the honest answer.
        let seq = u32::try_from(row).ok()?;
        let store = self.store.lock().ok()?;
        // **The ordinal is relative to the CURRENT transcript.** After a fork — a
        // compaction, a reseat — ordinal 3 is a row of the new base, so reading the old
        // transcript's row 3 would answer with a row nobody asked about.
        let transcript_id = store.current_transcript_id(session_id).ok()??;
        let json = store.row_json_at(&transcript_id, seq).ok()??;
        // The row's own type decides what its body is, and that match lives once — in
        // `letibot_sessionlog::body_of`, which the view's own reader calls too. Two copies
        // is how the two tiers would come to disagree about what a row's `body` is.
        let item: letibot_transcript::TranscriptItem = serde_json::from_str(&json).ok()?;
        Some(letibot_sessionlog::body_of(&item))
    }
}

/// **Ledger tokens as the operator's screen counts them.**
///
/// One function for the two callers that render a compaction — the sentence and the
/// structure beside it — because two copies of this arithmetic is how a head comes to
/// print two different numbers for one quantity. `None` scale is the ledger's own figure,
/// which is the right answer on a local endpoint and the only one available before a
/// metered turn has measured the ratio.
fn shown_tokens(scale: Option<(u64, u64)>) -> impl Fn(usize) -> u64 {
    move |ledger: usize| -> u64 {
        let n = ledger as u64;
        match scale {
            Some((l, p)) if l > 0 && p > 0 => ((n as u128 * p as u128) / l as u128) as u64,
            _ => n,
        }
    }
}

/// **What a compaction tells the head.**
///
/// The result used to go to the daemon's stderr and nowhere else. On the
/// ordinary path that was invisible, because the summary turn runs through the
/// session's own sink and every head watches the text being written. The overrun
/// paths cannot — they summarise a SCRATCH transcript through a `NullSink` on
/// purpose — so they streamed nothing, and a `/compact` that folded 1.5M tokens
/// correctly looked from the head like it had stopped partway. The operator,
/// 2026-09-20: *"started summarization of the first 1500+ and then scrolled some
/// s-tasks and that is it, not summary output, nothing"*.
///
/// So: always the numbers, and the summary itself exactly when nobody saw it
/// written. Printing it on the streamed path too would be the same text twice.
/// `scale` is the session's `ledger_scale` — **the session's, not the daemon's**.
/// `Sessions::base` is the command line, where it is always `None`, so reading it
/// there would convert nothing and quietly print the ledger's figures again.
///
/// The sentence is shared by the two doors that publish it —
/// [`Sessions::compact_if_at_the_wall`] for the daemon's own sessions and
/// [`Harness::submit_as_a_normal_session`] for a subagent's — because a second
/// copy of the wording would drift from the first exactly the way the seam
/// itself did.
pub(crate) fn compaction_said(
    r: &crate::harness::CompactReport,
    scale: Option<(u64, u64)>,
) -> String {
    let shown = shown_tokens(scale);
    // **An empty summary is a re-ingest**, which is the same signal
    // `fork_to_summary` reads to decide what note to write. Nothing was
    // summarised, so "compacted" would be a lie and `was → base` would be one
    // number printed twice.
    if r.summary_turn.summary.is_empty() {
        return format!(
            "re-seated: {} tokens of conversation carried onto the new prompt as they are, \
             on transcript {}. Nothing was summarised and nothing was dropped.",
            shown(r.fork.base_tokens),
            r.fork.transcript_id
        );
    }
    // In the units the operator's header shows; see `Config::shown_tokens`. This
    // line said `1352917 → 11353` beside a header reading 900k, and the operator
    // read the first number as a lie rather than as the other unit.
    let mut said = format!(
        "compacted: {} → {} tokens, on transcript {}.",
        shown(r.fork.was_tokens),
        shown(r.fork.base_tokens),
        r.fork.transcript_id
    );
    // **What was carried rather than described.** Zero on a local model, and a
    // non-zero count on a remote one, so the sentence is evidence of which path
    // ran rather than a restatement of the policy (R27).
    if r.fork.tail_items > 0 {
        said.push_str(&format!(
            " {} item(s) of the most recent exchange(s) were carried over verbatim \
             rather than summarised.",
            r.fork.tail_items
        ));
    }
    if let Some(dropped) = r.fork.tail_dropped {
        said.push_str(&format!(
            " The verbatim part begins inside an exchange — {dropped} item(s) of it are \
             gone — so its first message answers something that is no longer there."
        ));
    }
    // **And WHICH zero, when there is no tail.** This comment two paragraphs up claims the
    // non-zero count is *"evidence of which path ran rather than a restatement of the policy
    // (R27)"* — and that was true in one direction only: zero printed nothing, so policy and
    // accident were one appearance, on the screen and in the record. See
    // `CompactionTail::why_line`.
    if r.fork.tail_items == 0
        && let Some(line) = r.fork.tail_why.as_ref().and_then(|t| t.why_line())
    {
        said.push_str(&format!(" {line}"));
    }
    if !r.summary_was_streamed {
        said.push_str(&format!(
            " Nothing of the summary turn reached this screen — it ran over a scratch \
             transcript — so here is what the model now reads in place of everything \
             before it:\n\n{}",
            r.summary_turn.summary
        ));
    }
    said
}

/// **The structure behind the sentence** — R27's `warning.compaction`.
///
/// The `detail` sentence is written for a person and stays exactly as it was, because it
/// is the fallback every reader that cannot use this lands on. This is the same
/// compaction as fields: the numbers the sentence spells out, the record split into the
/// template's sections, and the verbatim tail. A head that draws a *row* needs these and
/// cannot get them by parsing English out of the sentence — which is R24 frame 2's defect
/// one layer up.
///
/// `kind` is the warning code the caller is publishing under, so the two cannot disagree
/// about which compaction this is.
///
/// **The units are the shown ones**, the same conversion the sentence was rendered
/// through. A head that printed a ledger figure beside this sentence would be printing
/// two different numbers for one quantity — the defect `Config::shown_tokens`' own
/// comment records.
pub(crate) fn compaction_wire(
    r: &crate::harness::CompactReport,
    kind: &str,
    scale: Option<(u64, u64)>,
    resident: u64,
    window: u64,
    headroom: u64,
) -> letibot_sessionlog::event::CompactionReport {
    let shown = shown_tokens(scale);
    use letibot_sessionlog::event::{CompactionReport, CompactionSection, CompactionTail};
    CompactionReport {
        kind: kind.to_string(),
        tokens_before: shown(r.fork.was_tokens),
        tokens_after: shown(r.fork.base_tokens),
        transcript: r.fork.transcript_id.clone(),
        resident,
        window,
        headroom,
        cut_off: r.fork.truncated,
        template: letibot_sessionlog::event::COMPACTION_TEMPLATE.to_string(),
        // **The record, split by the daemon, not by the head.** A missing section here
        // means the model wrote no such heading; an empty body means it wrote the
        // heading and nothing under it. Two facts, and the list is the only shape that
        // keeps them apart.
        sections: letibot_turn::parse_summary_sections(&r.summary_turn.summary)
            .into_iter()
            .map(|(name, body)| CompactionSection { name, body })
            .collect(),
        // **Present on every compaction, including a local one**, where it is an object
        // with zero turns. The local artefact is the remote one with an empty tail.
        tail: CompactionTail {
            turns: r.fork.tail_turns.clone(),
            carried: r.fork.tail_items as u64,
            because: r.fork.tail_because.clone(),
            dropped: r.fork.tail_dropped.unwrap_or(0) as u64,
        },
    }
}

impl SessionSource for StoreSessions {
    fn set_title(&self, session_id: &str, title: &str) -> Result<(), String> {
        let g = self.store.lock().unwrap_or_else(|e| e.into_inner());
        g.set_title(session_id, title).map_err(|e| e.to_string())
    }

    fn todos(&self, session_id: &str) -> Vec<letibot_sessionlog::event::TodoEntry> {
        let g = self.store.lock().unwrap_or_else(|e| e.into_inner());
        g.todos(session_id)
            .unwrap_or_default()
            .into_iter()
            .map(crate::harness::Harness::todo_entry)
            .collect()
    }

    /// **The merge queue, whole, for a head's bootstrap read** — the snapshot half of the
    /// snapshot-plus-events the wire is written as.
    ///
    /// Read through this source's own connection, which is the same file the merge-queue
    /// thread writes: a `SELECT` sees what the daemon committed, and the queue is daemon-level,
    /// so there is no `session_id` to scope it by.
    ///
    /// **Every state, and the evidence on each row.** A read that answered only the actionable
    /// entries would say *"that is all the work there is"* about a queue holding a `Failed`
    /// entry with a reason on it — the one thing the queue's own read refuses to do, and this
    /// is that read, one hop out.
    ///
    /// **A read that FAILED is said, not answered as empty.** The trait has no room for an
    /// error, and the alternative to a line here is a head drawing an empty queue over a store
    /// that could not be read — two states that look identical from the outside, which is
    /// exactly the confusion the `Corrupt` variants exist to prevent.
    fn merge_entries(&self) -> Vec<letibot_sessionlog::event::MergeEntry> {
        let g = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let rows = match g.merge_entries() {
            Ok(rows) => rows,
            Err(e) => {
                eprintln!("  merge queue: the queue could not be read: {e}");
                return Vec::new();
            }
        };
        crate::mergequeue::wire_queue(&rows)
    }

    /// **The reviewer's verdicts, whole, for a head's bootstrap read** — the other half of what
    /// the queue pane draws.
    ///
    /// The same posture as [`SessionSource::merge_entries`], one table over: a read that FAILED
    /// is said out loud rather than answered as empty, because a pane drawing no verdicts over a
    /// store that could not be read is the same confusion between *nobody asked* and *nothing is
    /// there* that the whole queue is written to avoid.
    fn merge_reviews(&self) -> Vec<letibot_sessionlog::event::MergeReview> {
        let g = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let rows = match g.reviews() {
            Ok(rows) => rows,
            Err(e) => {
                eprintln!("  merge queue: the verdicts could not be read: {e}");
                return Vec::new();
            }
        };
        crate::mergequeue::wire_reviews(&rows)
    }

    fn list(&self) -> Vec<StoredBrief> {
        let g = self.store.lock().unwrap_or_else(|e| e.into_inner());
        g.list_sessions()
            .unwrap_or_default()
            .into_iter()
            .map(|s| StoredBrief {
                session_id: s.id,
                title: s.title.unwrap_or_default(),
                items: s.items,
                last_activity_ms: s.last_activity_ms.max(0) as u64,
                parent_session_id: s.parent_session_id,
                context_tokens: s.context_tokens,
                context_cached: s.context_cached,
                wiring: SessionWiring {
                    // The session's own model and workspace, from its row. The
                    // dialect and endpoint are this daemon's — they are not stored
                    // per session, and a plausible guess on a picker row is a guess
                    // somebody quotes.
                    model: s.model_id,
                    workspace: s.workspace_root,
                    ..self.wiring.clone()
                },
            })
            .collect()
    }
}

#[cfg(test)]
mod idle_nag {
    //! **When the plan check is armed** — the rule the operator's scheduling question turned
    //! into, tested where it lives rather than through a daemon.
    //!
    //! The operator: *"looks like our todo nag is overkill"*, then *"maybe wait for a timeout
    //! actually. so send it when model is idling"*, then *"but certainly not after my message."*
    //! The first was the turn-boundary check firing after every exchange; the second is the
    //! timeout; the third is the boundary condition the timeout must not violate, and it is
    //! `Sessions::note_operator_prompt`'s job.
    //!
    //! **And the fourth half is the state**: *"can we handle postponed todo item properly? i.e.
    //! they persist but without nag"*. A row the operator has set aside is not work this check may
    //! speak about, and that is asserted below as a predicate over ROWS — which is the only way it
    //! can be told from a check that merely failed to arm.
    use super::nag_should_arm;
    use letibot_tokencore::store::{TodoBy, TodoItem, TodoStatus};

    /// One row of a plan, as the board holds it. The operator's half, because that is the half a
    /// postponed row is written from.
    fn row(content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            content: content.into(),
            status,
            by: TodoBy::Operator,
            when: None,
        }
    }

    /// A plan with nothing open is not a plan to nag about.
    #[test]
    fn a_finished_plan_is_never_armed() {
        assert!(!nag_should_arm(&[], None), "no plan at all");
        // even if something WAS nagged before and has since been finished
        assert!(!nag_should_arm(&[], Some("[todo check] 2 of 3")));
        // and a plan whose rows are all answered
        assert!(!nag_should_arm(&[row("done", TodoStatus::Completed)], None));
    }

    /// The first idle period after real work: armed.
    #[test]
    fn an_unfinished_plan_is_armed_once() {
        assert!(nag_should_arm(&[row("open", TodoStatus::Pending)], None));
    }

    /// **And silence after that, while the plan says the same thing.** This is the difference
    /// between a schedule and a metronome: a model that has been told and has not acted must not
    /// be told again every minute.
    ///
    /// The last-sent text is taken from the plan itself rather than written by hand, because that
    /// is what the schedule actually stores — a hand-written string would test the comparison
    /// against something this plan could never produce.
    #[test]
    fn an_unchanged_plan_is_not_armed_again() {
        let plan = [row("open", TodoStatus::Pending)];
        let sent = super::unfinished_plan(&super::the_plan_as_checked(&plan)).expect("open work");
        assert!(!nag_should_arm(&plan, Some(&sent)));
    }

    /// Any change at all is a new thing to say — an item closed, one started, one added.
    #[test]
    fn a_plan_that_moved_is_armed_again() {
        let plan = [row("open", TodoStatus::Pending)];
        let sent = super::unfinished_plan(&super::the_plan_as_checked(&plan)).expect("open work");
        assert!(nag_should_arm(
            &[row("open", TodoStatus::InProgress)],
            Some(&sent)
        ));
        assert!(nag_should_arm(
            &[
                row("open", TodoStatus::Pending),
                row("added", TodoStatus::Pending)
            ],
            Some(&sent)
        ));
    }

    /// **A POSTPONED row does not arm the check, and an ordinary one beside it does.**
    ///
    /// The operator's ask — a postponed item *"persists but without nag"* — and both directions
    /// are load-bearing: the first assertion is the feature, and the second is what says the first
    /// did not simply switch the check off for everything.
    ///
    /// **Tested here, as a predicate over rows and no session**, which is the reason
    /// `nag_should_arm` exists at all: through a daemon, *silent because the row is postponed* and
    /// *silent because nothing armed the clock* are the same observation.
    #[test]
    fn a_postponed_row_does_not_arm_the_check() {
        assert!(
            !nag_should_arm(&[row("later", TodoStatus::Postponed)], None),
            "the one row is set aside, so there is nothing to check"
        );
        // **Even after the check spoke about it**, which is the transition a session really makes:
        // the model was told, the operator then set the row aside, and the next idle period must
        // be silent rather than repeat the sentence.
        assert!(
            !nag_should_arm(
                &[row("later", TodoStatus::Postponed)],
                Some("[todo check] this turn is finished and one item is not done:\n  - later")
            ),
            "a postponement is not a reason to say the same thing again"
        );
        // …and the ordinary row beside it is the control.
        assert!(
            nag_should_arm(&[row("now", TodoStatus::Pending)], None),
            "an ordinary open row arms the check, so the silence above is the STATE and not a \
             check that stopped working"
        );

        // **A mixed plan asks about the ordinary row and says nothing about the other one.** The
        // reading is `the_plan_as_checked`, which is the same one `Harness::nag_notice` takes — so
        // the row is neither named nor counted as open, which are the two ways a postponed row
        // could still reach the model.
        let mixed = [
            row("now", TodoStatus::Pending),
            row("later", TodoStatus::Postponed),
        ];
        let notice = super::unfinished_plan(&super::the_plan_as_checked(&mixed))
            .expect("the ordinary row is open work");
        assert!(
            !notice.contains("later"),
            "the check must not name a row the operator set aside: {notice}"
        );
        assert!(
            !notice.contains("more open"),
            "and must not count it among what is open either: {notice}"
        );
    }
}

#[cfg(test)]
mod the_wake_route {
    //! **Which of the three things the daemon does with a wake for a session** — the operator's
    //! design in their own words: *"think about it like it is an erlang supervision tree. we talk
    //! to parents and they own lifecycle."*
    //!
    //! Tested here rather than through a daemon because the whole of it is the decision, and the
    //! decision is two facts the caller already looks up. The defect it replaces was one arm
    //! short: a ring naming a session the daemon does not hold was DISCARDED (R58), which is what
    //! made depth 2 a hole — a grandchild's settlement reached the bell and stopped there.
    use super::{WakeRoute, wake_route};

    /// The daemon's own session: the worker runs the turn, as it always did.
    #[test]
    fn a_session_the_daemon_holds_is_driven() {
        assert_eq!(wake_route(true, true), WakeRoute::Drive);
        // And `open` is the fact that decides it — a session in `open` whose hub has gone is
        // still the daemon's to run, and the harness inside is what runs it.
        assert_eq!(wake_route(true, false), WakeRoute::Drive);
    }

    /// **A live session the daemon does not hold is handed to the thread that does.** This is
    /// the arm that was missing, and it is a subagent: its harness lives on the thread its
    /// parent spawned it on, so the wake goes to that thread's own condvar rather than being
    /// dropped.
    #[test]
    fn a_session_the_daemon_does_not_hold_goes_to_its_own_reader() {
        assert_eq!(wake_route(false, true), WakeRoute::ItsOwnReader);
    }

    /// A session with no hub at all has nobody to hand anything to — and that is not a loss:
    /// a session that closed handed its undrained settlements up a level first
    /// (`jobwatch::JobWatchers::stop`), so the wake it would have been told by is no longer the
    /// one holding the notice.
    #[test]
    fn a_session_that_is_gone_is_nothing() {
        assert_eq!(wake_route(false, false), WakeRoute::Gone);
    }
}

#[cfg(test)]
mod the_wire_report {
    //! **R27's `warning.compaction`, built and read back.**
    //!
    //! The builder is private to this module and takes a `CompactReport`, so this is
    //! where it can be exercised without a store, a model or a turn: what a head
    //! draws a row from is exactly this function's output, and every field of it is a
    //! fact the daemon already owned and was spelling out in English.

    use super::*;
    use crate::harness::{CompactReport, ForkReport};
    use letibot_sessionlog::event::CompactionTurn;

    /// **Zero has three causes, and the sentence says which** — R41's shape one document
    /// over, and the operator's own ask after an automatic compaction of leticl's wrote no
    /// tail:
    ///
    /// *"the head should be able to say which treatment a compaction got. A reader who cannot
    /// tell whether the tail was omitted by policy or by accident is in the position R41's job
    /// pane was in: an absence with two causes and one appearance."*
    ///
    /// The comment above `compaction_said` claimed the non-zero count was *"evidence of which
    /// path ran rather than a restatement of the policy (R27)"* — true in one direction only.
    /// All three of these are `carried == 0` and they were one screen.
    #[test]
    fn a_compaction_with_no_tail_says_which_zero_it_is() {
        let none = |because: &str| report("a summary", Vec::new(), 0, because);
        let local = compaction_said(&none("local_model"), None);
        let nothing = compaction_said(&none("nothing_fits"), None);
        let empty = compaction_said(&none("no_turns"), None);
        assert!(local.contains("LOCAL model"), "{local}");
        assert!(local.contains("R27"), "the ruling must be named: {local}");
        assert!(
            nothing.contains("larger than the whole tail budget"),
            "{nothing}"
        );
        assert!(empty.contains("nothing to carry"), "{empty}");
        // **And they are three different sentences**, which is the whole requirement: an
        // absence with one appearance was the defect.
        assert_ne!(local, nothing);
        assert_ne!(local, empty);
        assert_ne!(nothing, empty);
        // **`budget` stays silent**: the tail exists and the count beside it says how much, so
        // a sentence explaining that what fitted was what fitted is furniture.
        let with_tail = compaction_said(
            &report(
                "a summary",
                vec![CompactionTurn {
                    role: "operator".into(),
                    text: "hi".into(),
                }],
                1,
                "budget",
            ),
            None,
        );
        assert!(!with_tail.contains("No verbatim tail"), "{with_tail}");
        assert!(with_tail.contains("carried over verbatim"), "{with_tail}");
        // **A reason this build does not know is shown, not swallowed** — the wire's own rule
        // for `because`, and the shape that would catch a daemon one version ahead.
        let unknown = compaction_said(&none("something_new"), None);
        assert!(unknown.contains("something_new"), "{unknown}");
    }

    fn report(
        summary: &str,
        tail: Vec<CompactionTurn>,
        carried: usize,
        because: &str,
    ) -> CompactReport {
        CompactReport {
            fork: ForkReport {
                transcript_id: "s-1#t2".into(),
                parent_id: "s-1#t1".into(),
                forked_at: 42,
                was_tokens: 240_000,
                base_tokens: 9_000,
                truncated: true,
                tail_items: carried,
                // A fork with no tail cannot have dropped any of it: the daemon
                // derives this from the plan, and a plan that carries nothing has no
                // split. Set here the same way, so the test cannot assert one.
                tail_dropped: (carried > 0).then_some(2),
                // The same reason in the wire's shape, built here the way the daemon builds
                // it so a test of the sentence is a test of what a reader meets.
                tail_why: (!because.is_empty()).then(|| {
                    letibot_sessionlog::event::CompactionTail {
                        turns: tail.clone(),
                        carried: carried as u64,
                        because: because.into(),
                        dropped: u64::from(carried > 0) * 2,
                    }
                }),
                tail_turns: tail,
                tail_because: because.into(),
            },
            summary_turn: letibot_turn::CompactionOutcome {
                turn_id: "s-1#t1#9".into(),
                summary: summary.into(),
                tool_calls: 0,
                truncated: true,
                cached_tokens: 0,
                reusable: 0,
                generated_tokens: 0,
            },
            gained: Vec::new(),
            lost: Vec::new(),
            summary_was_streamed: false,
        }
    }

    /// **A local compaction carries the template and an empty tail, and that is not
    /// an absent field.** The ruled split (R27) says a local model gets no verbatim
    /// turns; the *shape* still has to be the remote one, because a session compacted
    /// locally and resumed against a remote model must not meet a record its reader
    /// cannot read. So the tail is an object with zero turns and a reason saying why.
    #[test]
    fn a_local_compaction_carries_an_empty_tail_and_still_says_why() {
        let r = report("## Objective\n\n- ship it", Vec::new(), 0, "local_model");
        let wire = compaction_wire(&r, "compacted", None, 240_000, 262_144, 16_384);
        assert_eq!(wire.kind, "compacted");
        assert_eq!(wire.tail.turns, Vec::new());
        assert_eq!(wire.tail.carried, 0);
        assert_eq!(wire.tail.because, "local_model");
        assert_eq!(wire.tail.dropped, 0);
        assert_eq!(
            wire.template,
            letibot_sessionlog::event::COMPACTION_TEMPLATE
        );
        assert!(wire.cut_off, "the report's cut-off is the wire's");
        assert_eq!(wire.transcript, "s-1#t2");
        assert_eq!(wire.resident, 240_000);
        assert_eq!(wire.window, 262_144);
        assert_eq!(wire.headroom, 16_384);
    }

    /// **A remote compaction carries the turns themselves**, in order, with a role
    /// each — and the two facts about a mid-exchange start are both here rather than
    /// left to be parsed out of the sentence.
    #[test]
    fn a_remote_compaction_carries_the_turns_and_the_split() {
        let r = report(
            "## Objective\n\n- ship it\n\n## Blocked\n\n",
            vec![
                CompactionTurn {
                    role: "operator".into(),
                    text: "carry the recent past".into(),
                },
                CompactionTurn {
                    role: "agent".into(),
                    text: "working on it".into(),
                },
            ],
            7,
            "budget",
        );
        let wire = compaction_wire(&r, "compacted", None, 1, 2, 3);
        assert_eq!(wire.tail.carried, 7, "items, not turns");
        assert_eq!(wire.tail.turns.len(), 2, "turns");
        assert_eq!(wire.tail.turns[0].role, "operator");
        assert_eq!(wire.tail.because, "budget");
        assert_eq!(wire.tail.dropped, 2);
    }

    /// **The record is split section by section, and the two absences stay apart.**
    /// `Blocked` is present with an empty body — *nothing is blocked* — while
    /// `Relevant Files` is absent altogether — *nobody said*. A reader that collapsed
    /// them would report silence as a clean bill of health.
    #[test]
    fn the_sections_keep_written_and_empty_apart_from_never_written() {
        let r = report(
            "## Objective\n\n- ship it\n\n## Work State\n\n### Blocked\n\n### Active\n\n- going",
            Vec::new(),
            0,
            "local_model",
        );
        let wire = compaction_wire(&r, "compacted", None, 1, 2, 3);
        let names: Vec<&str> = wire.sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Objective", "Active", "Blocked"]);
        let blocked = wire.sections.iter().find(|s| s.name == "Blocked").unwrap();
        assert_eq!(blocked.body, "", "written and empty");
        assert!(
            !names.contains(&"Relevant Files"),
            "never written at all, which is a different fact"
        );
        // And a record that ignored the template yields an empty list rather than
        // invented sections — the raw text is still in `detail`.
        let prose = report(
            "I could not follow the format.",
            Vec::new(),
            0,
            "local_model",
        );
        assert!(
            compaction_wire(&prose, "compacted", None, 1, 2, 3)
                .sections
                .is_empty()
        );
    }

    /// **The units are the operator's, the same conversion the sentence uses**, so a
    /// head cannot print one number beside a sentence carrying another: this is the
    /// defect `Config::shown_tokens` was written for, one layer along.
    #[test]
    fn the_numbers_are_the_same_units_the_sentence_is_in() {
        // 3 ledger tokens to 2 provider tokens: 240,000 ledger is 160,000 shown.
        let scale = Some((3u64, 2u64));
        let r = report("## Objective\n\n- x", Vec::new(), 0, "local_model");
        let wire = compaction_wire(&r, "compacted", scale, 300, 300_000, 18_000);
        assert_eq!(wire.tokens_before, 160_000);
        assert_eq!(wire.tokens_after, 6_000);
        assert_eq!(
            wire.resident, 300,
            "the caller's own conversion, passed through"
        );
        // And the sentence beside it agrees, which is the property that matters.
        let said = compaction_said(&r, scale);
        assert!(said.contains("160000 → 6000"), "{said}");
    }
}
