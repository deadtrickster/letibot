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

use letibot_sessionlog::hub::{CommandKind, QueuedCommand};
use letibot_sessionlog::registry::{Registry, SessionSource, SessionWiring, StoredBrief};
use letibot_sessionlog::{SessionEvent, hub::Hub};
use letibot_tokencore::store::Store;
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
const WALL_CONTINUES: usize = 3;

/// What the worker did with one command.
pub enum Outcome {
    Replied(Box<Reply>),
    /// The session was compacted: one summary turn, then a transcript fork.
    /// The report is the evidence, not the word.
    Compacted(Box<CompactReport>),
    Failed(String),
    Ignored,
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
        };
        let (tool, cond) = sessions.seat_tool(&id);
        let cfg = sessions.with_fabric(&id, cfg, cond.is_some());
        let harness = Harness::open_with_registry(parts, cfg, hub, None, tool, registry.clone())?;
        sessions.open.insert(id.clone(), harness);
        sessions.declare_flowy_monitor(&id, cond);
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
                    // 16 KiB: enough that a build log's tail is one read, and the
                    // reply names the next offset when it is not.
                    Some(j) => match h.job_output(&j, offset, 16 * 1024) {
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
                match h.set_provider(choice) {
                    Ok(line) => {
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
                            });
                        }
                    }
                }
                if let Some(hub) = &hub {
                    for note in h.open_notes() {
                        hub.publish(SessionEvent::Warning {
                            code: "open_note".into(),
                            detail: note.clone(),
                        });
                    }
                }
                self.publish_title(session_id);
                Ok(true)
            }
            Err(e) => {
                if let Some(hub) = &hub {
                    hub.publish(SessionEvent::Warning {
                        code: "resume_failed".into(),
                        detail: e.to_string(),
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
            let cfg = Config {
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
            let cfg = self.with_fabric(session_id, cfg, cond.is_some());
            let h = Harness::open_with_registry(
                self.parts,
                cfg,
                hub,
                None,
                tool,
                self.registry.clone(),
            )?;
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
        let hub = self.registry.get(session_id);
        // Opened here rather than held across the tidying below: a live borrow of
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
        out
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
        let scale = self.open.get(session_id).and_then(|h| h.config().ledger_scale);
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
        eprintln!(
            "  compacting: {resident} of {window} tokens resident, less than the {} the \
             next turn needs",
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
                    eprintln!("  compacted: {after} tokens resident now, was {resident}.{cut}");
                    if let Some(hub) = &hub {
                        hub.publish(SessionEvent::Warning {
                            code: "auto_compact".into(),
                            detail: format!(
                                "compacted: {after} tokens resident now, was {resident}.{cut}"
                            ),
                        });
                    }
                }
            }
            Err(e) => {
                // Said on stderr too. The first run of this code printed
                // `compacting:` and then nothing at all, because the failure went
                // only to a hub with no head attached — a compaction that silently
                // did not happen is worse than one that never fired.
                eprintln!("  compaction FAILED: {e}");
                if let Some(hub) = &hub {
                    hub.publish(SessionEvent::Warning {
                        code: "auto_compact_failed".into(),
                        detail: format!(
                            "the automatic compaction did not run: {e}. The turn you \
                             asked for succeeded; what failed is the tidying after it, \
                             and the next turn may hit the context wall. `/compact` \
                             retries it.",
                        ),
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
    pub fn wake(&mut self, session_id: &str) -> Outcome {
        let hub = self.registry.get(session_id);
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
                        });
                    }
                    if let Some(hub) = &hub {
                        hub.publish(SessionEvent::Warning {
                            code: "compacted".into(),
                            detail: compaction_said(
                                &r,
                                self.harness_of(session_id).and_then(|h| h.config().ledger_scale),
                            ),
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
                            hub.publish(SessionEvent::Warning {
                                code: "reseated".into(),
                                detail: said,
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
                            });
                        }
                        Outcome::Failed(e.to_string())
                    }
                }
            }
            CommandKind::Interrupt { reason } => {
                if let Some(hub) = &hub {
                    hub.publish(SessionEvent::Warning {
                        code: "interrupt_idle".into(),
                        detail: format!(
                            "interrupt ({reason}) arrived between turns; nothing was generating"
                        ),
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
            CommandKind::WithdrawPrompts => Outcome::Ignored,
            // The request is honoured mid-turn by the `bash` wait loop, which reads
            // the hub's promote channel. Reaching here means nothing was running, so
            // the request is stale — clear it and say so, rather than leaving it for
            // the next command to promote itself unprompted.
            CommandKind::Promote => {
                if let Some(hub) = &hub {
                    hub.take_promote_request();
                    hub.publish(SessionEvent::Warning {
                        code: "promote_idle".into(),
                        detail: "a background request arrived between turns; nothing \
                                 was running to move"
                            .into(),
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
                    });
                }
                if reply.ok {
                    Outcome::Ignored
                } else {
                    Outcome::Failed(format!("/{line} was refused"))
                }
            }
            CommandKind::Mode { name, consented } => {
                let mode = match letibot_tools::mode::Mode::parse(name) {
                    Ok(m) => m,
                    Err(e) => {
                        if let Some(hub) = &hub {
                            hub.publish(SessionEvent::Warning {
                                code: "mode_unknown".into(),
                                detail: e,
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
fn compaction_said(r: &crate::harness::CompactReport, scale: Option<(u64, u64)>) -> String {
    let shown = |ledger: usize| -> u64 {
        let n = ledger as u64;
        match scale {
            Some((l, p)) if l > 0 && p > 0 => ((n as u128 * p as u128) / l as u128) as u64,
            _ => n,
        }
    };
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
