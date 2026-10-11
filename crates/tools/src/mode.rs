//! **A named point in the four-dimensional space this harness already had.**
//!
//! The space is real and it stays: a session is a coordinate in
//!
//! 1. **role** — which tools are seated
//! 2. **tier policy** — what a `MayApprove` finding costs, per access class
//! 3. **grant scope** — how far an answer travels
//! 4. **adjudicator** — who decides
//!
//! What was missing is not fewer axes, it is **a coordinate with a name**. Four
//! independent knobs are four ways to be wrong; four *named dots* are something an
//! operator can hold in their head, and the space stays fully reachable for anyone who
//! wants a coordinate nobody named.
//!
//! ```text
//! Mode { name: "writes allowed",
//!        role: coder, write→Admit, exec→Ask, read→Auto,
//!        grants: Session, adjudicator: Head,
//!        requires: [WritableBackend, ReachableAdjudicator] }
//! ```
//!
//! # The four the operator named
//!
//! > *"we could have read-only, always-ask, writes allowed, automode and only auto
//! > mode is blocked on vram. so the A thing from our intent conversation is fully
//! > buildable right now"*
//!
//! Correct, and it is the useful reading of the whole design: layer A — the
//! normaliser, the scoped intents, the tiers, the always-ask list — is deterministic
//! and exists. **Three of the four points are shippable with no model at all.** Only
//! [`Mode::AUTO`] wants an oracle.
//!
//! # Prerequisites are the load-bearing part
//!
//! A point declares what it [`requires`](Mode::requires), and selecting it checks
//! them. An unmet prerequisite **refuses by name, saying what is missing and how to
//! attach it** — never a silent downgrade to a weaker point.
//!
//! That last clause is the one worth spelling out, because a silent downgrade is the
//! attractive implementation: an operator asks for *writes allowed*, no adjudicator
//! can be reached, and the session opens at *always-ask*, which is safe. It is also a
//! session whose banner says one thing and whose behaviour is another, and the
//! operator finds out one wasted turn later — `docs/tool-design-brief.md` §3b forbids
//! exactly this for attachments and the reason carries over unchanged.
//!
//! # What a point may never do
//!
//! A point governs [`Tier::MayApprove`] and **nothing else**. It cannot move
//! [`Tier::AlwaysAsk`] — that tier means *the operator decides, every time, however
//! confident anything is* — and it cannot touch [`Tier::Blocked`], which nobody
//! can, at any point, including [`Mode::AUTO`]. Both properties are types rather than
//! checks: [`crate::adjudicate::Adjudicable`] is minted only for `MayApprove`, so
//! there is no value a point could hold that would widen either of the other two.
//!
//! This also answers a question the earlier design had open — *may a standing
//! permission cover an always-ask entry?* Under points it cannot, and it does not need
//! to: the thing an operator actually wants when privilege escalation asks forever is
//! not an exception buried in a grant table, it is a different point for that project,
//! visible in the banner, changeable in one move.

use crate::adjudicate::Tier;
use crate::schema::Access;

/// What a [`Tier::MayApprove`] finding costs for one access class.
///
/// Deliberately two values and not three. *"The model decides"* is not a third
/// disposition, it is the **adjudicator** axis: `Ask` means *whoever is attached is
/// asked*, and at [`Mode::AUTO`] that is an oracle. Folding the decider into the
/// disposition would put axes 2 and 4 in one field and then need a rule for what
/// happens when they disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Not seated at this point. The tools are absent from `tools_json` rather than
    /// refused at call time — D9's reasoning, that a capability boundary beats a
    /// flag, because a model carrying a tool definition it can never use will try it.
    Absent,
    /// Admitted without consulting anybody. Still recorded as a row: a standing
    /// admission that stops appearing in the audit is one nobody can review.
    Admit,
    /// The adjudicator is asked. Which adjudicator is axis 4.
    Ask,
}

impl Disposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Disposition::Absent => "absent",
            Disposition::Admit => "admit",
            Disposition::Ask => "ask",
        }
    }
}

/// Axis 3: how far one answer travels.
///
/// The ceiling is a **session**, and that is the design decision the earlier draft of
/// this got wrong. Anything standing beyond one session is a point — durable, named,
/// visible in the banner, changeable in one move — rather than a grant. A grant table
/// keyed by tool and class is a permanent widening nobody remembers making, which is
/// opencode's `always: ["*"]` defect (survey §1.1): one click silently disables config
/// denies including their own `.env` guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantScope {
    /// An answer settles one call and nothing else.
    Once,
    /// An answer may also settle later calls that normalise to the same
    /// `(program, ActionClass)` for the rest of the session. Never a text match —
    /// see [`crate::adjudicate`].
    Session,
}

impl GrantScope {
    pub fn as_str(self) -> &'static str {
        match self {
            GrantScope::Once => "once",
            GrantScope::Session => "session",
        }
    }
}

/// Axis 4, as a value a point can name before anything is constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decider {
    /// Nothing is attached and nothing can reach the gate, because nothing at this
    /// point is gated.
    None,
    /// A person, over whatever can reach one — the session socket, or the daemon's
    /// own console.
    Human,
    /// An oracle. Absent on this box; see [`Prereq::Oracle`].
    Model,
}

impl Decider {
    pub fn as_str(self) -> &'static str {
        match self {
            Decider::None => "none",
            Decider::Human => "human",
            Decider::Model => "model",
        }
    }
}

/// Something a point needs before it can honestly be selected.
///
/// Every one of these is a capability seam that already exists, which is why the
/// operator's *"only automode is blocked on VRAM"* is exactly right: the other three
/// are blocked on wiring, not on hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Prereq {
    /// A backend opened for writing. `HostBackend` is read-only unless it was opened
    /// with `writable`, and an admitted write over a read-only backend is the "two
    /// gates and the second one is shut" state — a session that starts cleanly and
    /// fails every call.
    WritableBackend,
    /// An adjudicator that can actually reach whoever decides. Not *an adjudicator
    /// exists*: `ConsoleAdjudicator` over a daemon whose stdin nobody is typing into
    /// is attached and unreachable, which is D25 and is the distinction this
    /// prerequisite is named for.
    ReachableAdjudicator,
    /// An `AuthorisationOracle`. A loaded model, a budget it is abandoned for
    /// overrunning, and a scope earned by measurement.
    Oracle,
    /// A confinement the exec substrate can actually build. `crates/tools/src/exec`
    /// says plainly that it is **not** a sandbox on its own.
    Confinement,
}

impl Prereq {
    pub fn as_str(self) -> &'static str {
        match self {
            Prereq::WritableBackend => "writable backend",
            Prereq::ReachableAdjudicator => "a reachable adjudicator",
            Prereq::Oracle => "an authorisation oracle",
            Prereq::Confinement => "a confinement for exec",
        }
    }

    /// **How to attach it.** A refusal that says what is missing and not how to
    /// supply it is a refusal the operator routes around by picking a weaker point,
    /// which is the silent downgrade with an extra step.
    pub fn how(self) -> &'static str {
        match self {
            Prereq::WritableBackend => {
                "the backend is opened writable when the seated role declares write \
                 access; a role with no write tools cannot supply it"
            }
            Prereq::ReachableAdjudicator => {
                "attach a head that can decide (`letibot-tui`), or run the daemon in a \
                 foreground terminal with `--adjudicator console` so a person can answer \
                 on its stdin"
            }
            Prereq::Oracle => {
                // Was "nothing in this build supplies one ... the seam is not wired".
                // That was true and stopped being true when `HttpOracle` landed, and a
                // prerequisite that reports a missing CAPABILITY when what is missing
                // is a FLAG sends the operator to look for the wrong thing. The
                // message has to move with the build, which is why it names the flag
                // rather than the state of the code.
                "put the guard's address in ~/.config/letibot/providers.toml under \
                 `[gatekeeper] endpoint = \"HOST:PORT\"` — a llama.cpp `/completion` \
                 endpoint that answers `did the operator ask for this`. Its own \
                 endpoint, not `--endpoint`: the guard need not be the model doing \
                 the work. `--oracle HOST:PORT` overrides it for one run"
            }
            Prereq::Confinement => {
                // **This named the wrong thing and cost the operator a diagnosis.**
                //
                // It said exec needs a cgroup v2 subtree and an unprivileged
                // namespace, and that `Bwrap` probes for one. All true of the exec
                // substrate, and none of it is what this prerequisite reads:
                // `Harness::open` sets `backend_confined` from `placement ==
                // Firecode` and from nothing else, so on the operator's own box no
                // bwrap probe, no cgroup and no namespace could ever have supplied
                // it. Measured 2026-09-20 — they read this sentence, went looking at
                // the confinement machinery, and the answer was not there.
                //
                // Same rule as the `Oracle` arm above, which was fixed for the same
                // reason: name the thing the reader can actually change.
                "this is a firecode VM placement, and only that — `Harness::open` reads \
                 it off the placement, so no cgroup or namespace on this box supplies \
                 it. On your own box there is no confinement to build: pick the point \
                 with `/mode allow-all` in the head, which asks you to confirm that \
                 this box is the boundary and then runs the session at `allow-all` \
                 for as long as it lasts"
            }
        }
    }
}

/// **What the seated role actually put in `tools_json`.**
///
/// Read off the resolved schemas, never off which role was asked for: a role that
/// failed to resolve some of its tools seats fewer than its name suggests, and a
/// prerequisite check that trusted the name would be checking a session that does not
/// exist. `GateWiring` already pays for that distinction four times over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Seats {
    pub write: bool,
    pub exec: bool,
    pub network: bool,
}

impl Seats {
    /// Nothing that is not a read. What an orchestrator seats.
    pub const READS_ONLY: Seats = Seats {
        write: false,
        exec: false,
        network: false,
    };
}

/// **Who bears the consequence of an action** — the operator's box, or a boundary
/// built so that nothing crosses it.
///
/// The always-ask list (privilege escalation, deletion outside the project, a host
/// never seen before) and the flow rules (a secret leaving the boundary) exist
/// because on the operator's own box those actions reach the operator, who cannot
/// consent in-session. Inside a firecode VM they reach a copy the VM was booted
/// on and a guest whose root is the guest's: `sudo` there is `sudo` in a throwaway,
/// a new host is the guest's network, and nothing of the host is behind either.
/// So `Structural` admits the always-ask list, and it is the coordinate that makes
/// `allow-all` the thing its name says — the operator's rule, 2026-09-14: *"allow-all
/// should be the true allow-all."* Every other point is `Operator`, where the list
/// reaches a person at every one of them, unchanged. The flow rules — a secret
/// leaving the boundary — are layer A's and are refused before any point is asked:
/// a credential in the copy opens the same hosts it opens from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// The operator's own box: the always-ask list and the flow rules reach them.
    Operator,
    /// A boundary that makes the action structural: nothing here reaches the
    /// operator, so nothing asks.
    Structural,
    /// **The operator's own box, and the operator said so.** There is no structural
    /// boundary here: `sudo` is `sudo` on this machine, a delete outside the project
    /// deletes the operator's file, and a new host is reached from their network. The
    /// always-ask list exists because those reach a person who cannot consent
    /// in-session — so this coordinate is the person consenting OUT of session,
    /// once, in advance, with the list in front of them.
    ///
    /// The operator, 2026-09-20: *"make it ask for confirmation on bare host and let
    /// it thru"*. The confirmation is the whole difference between this and
    /// [`Boundary::Structural`]; without one this is `Structural` with the safety
    /// argument deleted, which is why no daemon flag selects it and no `parse` of
    /// `allow-all` reaches it. It is named separately from `Structural` so the
    /// banner, the corpus row and this enum all keep saying which of the two facts
    /// was true — a consented box is not a confined one, and a reader six months
    /// from now must not have to infer that from the mode's name.
    ///
    /// What it does NOT move is [`Tier::Blocked`]: a secret leaving the box is layer
    /// A's refusal, taken before any point is consulted. Consent widens what may be
    /// done without asking; it does not repeal the flow rules.
    OperatorConsented,
}

/// **A named coordinate.**
///
/// The four constants below are the points the operator named. Others are legal —
/// this is a struct, and a caller may build a coordinate nobody named — but these four
/// are the ones with names, and a name is what makes a point auditable: a banner
/// saying *"writes allowed"* is a thing somebody can disagree with, and a banner
/// listing four axis values is a thing nobody reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub name: &'static str,
    /// Axis 1. The role name a session seats. **Roles answer which tools, never how
    /// much approval** — that conflation is why `--role coder` meant both "has write"
    /// and "asks about writes", and why an operator who hit *"no edit tools"* had no
    /// way to change it in place.
    pub role: &'static str,
    /// Axis 2, for a write.
    pub write: Disposition,
    /// Axis 2, for a command.
    pub exec: Disposition,
    /// Axis 2, for something that leaves the box.
    pub network: Disposition,
    /// Axis 3.
    pub grants: GrantScope,
    /// Axis 4.
    pub decider: Decider,
    /// Axis 5: who bears an action's consequence — and so whether the always-ask
    /// list and the flow rules still reach a person from this point.
    pub boundary: Boundary,
    /// What must be true before this point can be selected.
    pub requires: &'static [Prereq],
    /// One line for the banner, in the operator's terms rather than the axes'.
    pub summary: &'static str,
}

/// **What a child's point is, and where it came from** — the two arms of
/// [`Mode::inherited_by`].
///
/// A `bool` beside a `Mode` would be the shape in which *was this clamped* stops
/// being read. Naming the two states is what lets the disclosure — and the test —
/// ask for the sentence rather than for a flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inherited {
    /// The parent's point, carried unchanged, because this seat's own supplies
    /// satisfy it. **Consent travels this way and only this way** — see
    /// [`Mode::ALLOW_ALL_HERE`].
    Carried(Mode),
    /// **The nearest point this seat can carry**, because it cannot carry the
    /// parent's — and the sentence that says so, naming the seat. Never `Carried`
    /// of a wider point, and never a point the seat's own supplies cannot satisfy.
    Clamped { mode: Mode, why: String },
}

impl Inherited {
    /// The point the child opens at.
    pub fn mode(&self) -> Mode {
        match self {
            Inherited::Carried(m) => *m,
            Inherited::Clamped { mode, .. } => *mode,
        }
    }

    /// **Why this is not the parent's point**, or `None` when it is.
    ///
    /// Not a debugging aid: a clamp nobody can read is the silent downgrade under
    /// another name, so this is the sentence the child's own settings row and the
    /// note the open publishes carry.
    pub fn why(&self) -> Option<&str> {
        match self {
            Inherited::Carried(_) => None,
            Inherited::Clamped { why, .. } => Some(why),
        }
    }
}

impl Mode {
    /// Reads, and nothing else. Clause 4: a read inside the boundary never prompts,
    /// so there is nothing to attach and nothing that can refuse.
    pub const READ_ONLY: Mode = Mode {
        name: "read-only",
        role: "orchestrator",
        write: Disposition::Absent,
        exec: Disposition::Absent,
        network: Disposition::Absent,
        grants: GrantScope::Once,
        decider: Decider::None,
        boundary: Boundary::Operator,
        requires: &[],
        summary: "reads only. No write, no command, nothing that leaves the box — the \
                  tools are absent rather than refused, so nothing is claimed that \
                  cannot be done.",
    };

    /// Write and exec are seated and **every** non-read action asks.
    ///
    /// `WritableBackend` is on this list and was not on the table it came from. It has
    /// to be: this point seats `write`, and a seated write over a read-only backend is
    /// a session where an admitted call still cannot reach the disk. Selecting a point
    /// whose every write fails after a person approved it is worse than refusing the
    /// point, because the person spent an answer on it.
    pub const ALWAYS_ASK: Mode = Mode {
        name: "always-ask",
        role: "coder",
        write: Disposition::Ask,
        exec: Disposition::Ask,
        network: Disposition::Ask,
        grants: GrantScope::Once,
        decider: Decider::Human,
        boundary: Boundary::Operator,
        requires: &[Prereq::WritableBackend, Prereq::ReachableAdjudicator],
        summary: "write and exec are seated and every one of them asks you, every time. \
                  An answer settles that call by default; `allow_session` in the ask \
                  lets the whole session through for that class, and `allow_always` \
                  writes a durable rule.",
    };

    /// Writes go through. Exec still asks, and so does the always-ask list.
    ///
    /// The asymmetry between `write` and `exec` is §3's, not a hedge: the boundary
    /// guards the filesystem *view*, and nothing yet stops a tool result carrying
    /// secret bytes into the transcript — `bash` is the tool whose result is an
    /// arbitrary byte stream, so it is the one that turns that gap from theoretical
    /// into reachable. A `write` result is shaped by the tool and does not.
    pub const WRITES_ALLOWED: Mode = Mode {
        name: "writes allowed",
        role: "coder",
        write: Disposition::Admit,
        exec: Disposition::Ask,
        network: Disposition::Ask,
        grants: GrantScope::Session,
        decider: Decider::Human,
        boundary: Boundary::Operator,
        requires: &[Prereq::WritableBackend, Prereq::ReachableAdjudicator],
        summary: "writes go through without asking. Commands and anything that leaves \
                  the box still ask, and so does the always-ask list — privilege \
                  escalation, deletion outside the project, a host never seen before.",
    };

    /// The model adjudicates within `MayApprove`.
    ///
    /// **Selectable and it refuses**, which is the point of it existing: a point that
    /// is missing reads as *this build cannot do that*, and a point that says what it
    /// needs reads as *this build can, and here is the piece that is absent*. Same
    /// shape as retrieval being INERT rather than unlisted.
    ///
    /// It does not widen `write` to `Admit`. What automode changes is **who answers**,
    /// not what asks — the 83% figure in §1 is about a classifier standing where a
    /// fatigued human stood, not about removing the question.
    pub const AUTO: Mode = Mode {
        name: "automode",
        role: "coder",
        write: Disposition::Ask,
        exec: Disposition::Ask,
        network: Disposition::Ask,
        grants: GrantScope::Session,
        decider: Decider::Model,
        boundary: Boundary::Operator,
        requires: &[
            Prereq::WritableBackend,
            Prereq::ReachableAdjudicator,
            Prereq::Oracle,
        ],
        summary: "a model answers, within the scope it has earned. The always-ask list \
                  still reaches you and nothing promotes an blocked action.",
    };

    /// **Writes go through and a model answers for the rest.** Claude Code's own
    /// auto-accept posture, and the operator's ask (2026-09-17): *"if automode
    /// inherits allow_edits, why i still asked about edits?"*
    ///
    /// It did not inherit it. [`Mode::AUTO`] is deliberately `write: Ask` — its
    /// own doc says *"what automode changes is who answers, not what asks"* — so
    /// every edit became a question the oracle normally cleared, and the moment
    /// the oracle could not (down, past its budget, an intent outside its earned
    /// scope) the question landed on the operator. Eighteen of them did on
    /// 2026-09-15.
    ///
    /// This point is the honest way to have what that operator wanted: the write
    /// disposition of [`Mode::WRITES_ALLOWED`] with the decider of
    /// [`Mode::AUTO`]. **What it trades** is stated rather than implied, and the operator
    /// ruled on it directly (2026-10-05) after being shown that
    /// `write({path: "/etc/cron.d/x"})` is admitted here with nobody consulted:
    ///
    /// > *"yes, writes allowed means writes allowed — but Blocked is Blocked — i want my .ssh
    /// > to stay protected"*
    ///
    /// So the write disposition is **deliberately not path-aware**: this point admits a WRITE
    /// wherever it lands, and what keeps the operator's secrets safe is not this dial but the
    /// tier above it — a write into the never-write list or a secret store is [`Tier::Blocked`]
    /// before this point is read, and neither a point nor an adjudicator may promote one. The
    /// wording that stood here said *"an edit inside the boundary takes effect with nobody
    /// consulted"*, which was the intent rather than the code; the ruling is what closes that gap.
    ///
    /// §3's asymmetry is why the same move on `exec` would not be defensible: the boundary
    /// guards the filesystem view, a `write` result is shaped by the tool, and `bash` is the one
    /// tool whose result is an arbitrary byte stream. So exec and network still ask, and the
    /// oracle still answers them.
    ///
    /// Unchanged from every other point: the always-ask list still reaches the
    /// operator, and nothing promotes a blocked action. A write into a secret
    /// store is `Blocked` before this dial is read.
    pub const AUTO_EDITS: Mode = Mode {
        name: "automode-edits",
        role: "coder",
        write: Disposition::Admit,
        exec: Disposition::Ask,
        network: Disposition::Ask,
        grants: GrantScope::Session,
        decider: Decider::Model,
        boundary: Boundary::Operator,
        // The same three as `automode`: this point still consults a model, so a
        // session that cannot reach one must refuse the point by name rather
        // than quietly standing at always-ask.
        requires: &[
            Prereq::WritableBackend,
            Prereq::ReachableAdjudicator,
            Prereq::Oracle,
        ],
        summary: "writes inside the boundary go through with nobody asked — not you \
                  and not the model. Commands and anything that leaves the box still \
                  ask, and a model answers those within the scope it has earned. The \
                  always-ask list still reaches you and nothing promotes a blocked \
                  action.",
    };

    /// **Everything is admitted, nothing asks.** The firecode-native point.
    ///
    /// `write`, `exec` and `network` are all [`Disposition::Admit`] and the decider is
    /// [`Decider::None`] because there is nothing to decide: the point is only sane
    /// *inside a boundary that makes the action structural* — a firecode microVM whose
    /// guest sees one project and nothing of the host. On a bare host this is
    /// opencode's `bypassPermissions` posture, and the boundary is the only thing that
    /// makes it safe. The `Confinement` prerequisite is the load-bearing part: this
    /// point refuses to open where no confinement can be built.
    pub const ALLOW_ALL: Mode = Mode {
        name: "allow-all",
        role: "coder",
        write: Disposition::Admit,
        exec: Disposition::Admit,
        network: Disposition::Admit,
        grants: GrantScope::Session,
        decider: Decider::None,
        boundary: Boundary::Structural,
        requires: &[Prereq::WritableBackend, Prereq::Confinement],
        summary: "write, exec and network all go through without asking, the always-ask \
                  list included: the VM is the boundary and nothing inside it reaches \
                  this box. Only a secret leaving the boundary is still refused. The \
                  confinement prerequisite is what refuses this point on a bare host.",
    };

    /// **`allow-all` on a box with no boundary, which the operator consented to.**
    ///
    /// Identical to [`Mode::ALLOW_ALL`] on every axis but two, and both differences
    /// are the point:
    ///
    ///   `boundary`   `OperatorConsented`, not `Structural`. Nothing here is
    ///                structurally contained; a person said the sentence instead.
    ///   `requires`   no [`Prereq::Confinement`], because there is none and that is
    ///                the admitted fact rather than a hidden one.
    ///
    /// **Why this exists at all.** `allow-all` requires a confinement, and
    /// `backend_confined` is only ever true for a firecode placement — so on the
    /// operator's own box `allow-all` could not be reached by any route, and the
    /// refusal sent them to build a bwrap confinement that would not have satisfied
    /// it either. Measured 2026-09-20, their words: *"allow-all doesnt work"*, then
    /// *"ah, interesting, but it doesnt tell me that, make it ask for confirmation on
    /// bare host and let it thru"*.
    ///
    /// **The confirmation is not here.** This constant is inert on its own: what
    /// makes it honest is that the only thing that selects it is a head that asked
    /// the operator first. It is deliberately NOT in [`Mode::NAMED`] and so not
    /// reachable through [`Mode::parse`] — a name that a config file, a project row
    /// or `--mode` could carry would be consent nobody gave, recorded once and
    /// replayed by every later daemon. It is selected by
    /// [`Harness::consent_to_allow_all`] and nothing else.
    ///
    /// # A child of the session that consented carries it
    ///
    /// The confirmation was given for **a session and the work it is doing**, not for
    /// one process. A subagent is that work: it is spawned by this session, opened by
    /// it, answers into its root, and is bound by its downgrade. So the point travels
    /// down through [`Mode::inherited_by`], which carries it unchanged when the
    /// child's own seat can carry it and lands the child on the nearest point that
    /// seat can carry when it cannot.
    ///
    /// That does not weaken the sentences above about it lasting as long as the
    /// session — *no file records it and no later daemon starts here*. What travels is
    /// the **live point of one session, down the one tree that session is spawning,
    /// for as long as that session lives**; nothing is written down, and a daemon
    /// started tomorrow gets no row for it and opens at whatever the store says. The
    /// two are the same fact about the same session, not an exception to it.
    ///
    /// Nor can a child *acquire* it without one: it is not in [`Mode::NAMED`], so
    /// [`Mode::parse`] cannot reach it, and it is not on the ladder
    /// [`Mode::inherited_by`] clamps over, so a clamp can narrow it and can never
    /// produce it. The one route to it stays a person's answer in a session that then
    /// has children.
    pub const ALLOW_ALL_HERE: Mode = Mode {
        name: "allow-all (this box, consented)",
        role: "coder",
        write: Disposition::Admit,
        exec: Disposition::Admit,
        network: Disposition::Admit,
        grants: GrantScope::Session,
        decider: Decider::None,
        boundary: Boundary::OperatorConsented,
        requires: &[Prereq::WritableBackend],
        summary: "write, exec and network all go through without asking, the always-ask \
                  list included — on this box, with nothing confining it, because the \
                  operator confirmed it. Only a secret leaving the box is still refused. \
                  The confirmation is RECORDED with the project — a third field on its row \
                  in `modes.tsv` — so it survives a restart and is read aloud at every \
                  startup; `/mode <other>` takes it back and forgets the answer.",
    };

    /// The named points, in widening order. The order is the one a banner lists them
    /// in and the one an operator reads as a ladder.
    ///
    /// [`Mode::ALLOW_ALL_HERE`] is deliberately absent: see its own doc. A point that
    /// stands for a person's confirmation must not be selectable by a string.
    pub const NAMED: &'static [Mode] = &[
        Mode::READ_ONLY,
        Mode::ALWAYS_ASK,
        Mode::WRITES_ALLOWED,
        Mode::AUTO,
        // Wider than `automode` on exactly one axis — `write` — and identical on
        // every other, which is why it sits here and not beside `writes
        // allowed`.
        Mode::AUTO_EDITS,
        Mode::ALLOW_ALL,
    ];

    /// The name opencode uses for this point, when the point has one. Used so a
    /// session driven by an opencode agent can select modes by opencode's vocabulary
    /// rather than learning letibot's.
    pub fn opencode_name(&self) -> Option<&'static str> {
        match self.name {
            "read-only" => Some("plan"),
            "always-ask" => Some("default"),
            "writes allowed" => Some("acceptEdits"),
            "allow-all" => Some("bypassPermissions"),
            // `automode-edits` is the closest thing this build has to what
            // Claude Code calls auto-accept-edits, and the operator named it as
            // such. Not mapped to `acceptEdits` here, though: that name already
            // belongs to `writes allowed`, and two points answering to one name
            // would make `parse` pick by list order rather than by meaning.
            _ => None,
        }
    }

    /// Look one up by the name an operator types or a store holds — either letibot's
    /// own name, or the opencode permission-mode name it maps to.
    ///
    /// Every unknown value names the four rather than falling back to a default. A
    /// typo that silently seated the read-only point would be an operator who thinks
    /// they have `edit` and does not — which is the same defect `Seat::parse` already
    /// pays for one layer up.
    pub fn parse(s: &str) -> Result<Mode, String> {
        let want = s.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        Mode::NAMED
            .iter()
            .find(|m| {
                m.name.replace(' ', "-") == want
                    || m.opencode_name().map(|n| n.to_ascii_lowercase()) == Some(want.clone())
            })
            .copied()
            .ok_or_else(|| {
                format!(
                    "unknown mode `{s}`; this build has {}",
                    Mode::NAMED
                        .iter()
                        .map(|m| m.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }

    /// What a `MayApprove` finding costs for one access class at this point.
    ///
    /// [`Access::Read`] is [`Disposition::Admit`] at **every** point, including
    /// read-only: clause 4 says a read inside the boundary never prompts, and a point
    /// that could make a read ask would be a point that could make the harness
    /// unusable by moving one dial.
    /// **Does this point admit everything** — write, exec and network unasked, the
    /// always-ask list included? True for both `allow-all` coordinates (a structural
    /// boundary, or the operator's own box with their consent) and nothing else. The gate
    /// asks this to let a command the grammar could not read go on to the mode, which
    /// admits it anyway, instead of refusing it before the mode is consulted.
    pub fn admits_everything(&self) -> bool {
        self.write == Disposition::Admit
            && self.exec == Disposition::Admit
            && self.network == Disposition::Admit
            && self.boundary != Boundary::Operator
    }

    pub fn disposition(&self, access: Access) -> Disposition {
        match access {
            Access::Write => self.write,
            Access::Exec => self.exec,
            Access::Network => self.network,
            _ => Disposition::Admit,
        }
    }

    /// **Whether this point admits a call without consulting anybody.**
    ///
    /// The whole of the point's authority, in one function, and it takes the tier so
    /// that the two tiers a point may not move cannot be reached by a caller that
    /// forgot to check them. `AlwaysAsk` and `Blocked` answer `false` here at
    /// every point including [`Mode::AUTO`].
    ///
    /// # Why `Tier::Auto` is not a blanket yes here
    ///
    /// It reads like one — clause 4 says a read inside the boundary never prompts —
    /// and writing it that way was wrong in a way a test caught. Clause 4 is enforced
    /// by the **runtime**, which consults the gate only when the tool's declared
    /// access is not read (`ToolRuntime::invoke`). So a call that has reached this
    /// function is a call whose *tool* declared write, exec or network, whatever layer
    /// A concluded about the *action*.
    ///
    /// `bash` is the case: `bash("cat x")` derives `Tier::Auto`, because the action is
    /// a read inside the boundary. Admitting it here would make `bash` unaskable for
    /// every read command — and `bash` is precisely the tool §3 names as the one whose
    /// result is an arbitrary byte stream, which is why exec is opt-in in the first
    /// place. So `Auto` goes through the access class's disposition like anything
    /// else, and `Access::Read` is the class that always admits.
    pub fn admits_unasked(&self, tier: &Tier, access: Access) -> bool {
        match tier {
            Tier::Auto | Tier::MayApprove => self.disposition(access) == Disposition::Admit,
            // Not negotiable by a point on the operator's box, in either direction.
            // Inside a structural boundary there is nobody the list protects, and at
            // `OperatorConsented` the person it protects took it off themselves with
            // the list in front of them — see [`Boundary`]. Written as "not
            // `Operator`" rather than as a two-arm match so that adding a third
            // boundary later is a compile error here, where the decision belongs.
            Tier::AlwaysAsk { .. } => self.boundary != Boundary::Operator,
            // Layer A's own tier — a secret disclosed across the boundary — is
            // refused by the gate before any point is consulted, and a point cannot
            // reach it. A credential in the copy is still a credential.
            Tier::Blocked { .. } => false,
        }
    }

    /// Whether an access class is seated at all here.
    pub fn seats(&self, access: Access) -> bool {
        self.disposition(access) != Disposition::Absent
    }

    /// **What this point needs, given what the session actually seats.**
    ///
    /// A prerequisite is about a capability the session will *use*, and axis 1 —
    /// which tools are seated — is chosen separately from the point. So a read-only
    /// role at `always-ask` needs no writable backend: it has nothing to write with,
    /// and there is nothing for the missing backend to break.
    ///
    /// This is not a softening, and the difference is worth stating because getting it
    /// wrong is what a test caught. Requiring a writable backend unconditionally made
    /// **every read-only session refuse to open** — the default point is `always-ask`,
    /// the default role seats no write tools, and the two together produced a harness
    /// that would not start. A prerequisite that fires for a capability nobody has is
    /// not conservative, it is wrong, and it is the kind of wrong that reads as
    /// safety.
    pub fn requires_given(&self, seats: Seats) -> Vec<Prereq> {
        self.requires
            .iter()
            .copied()
            .filter(|p| match p {
                // Only a session that can write needs somewhere to write.
                Prereq::WritableBackend => seats.write,
                // Only a session that can run a command needs it confined.
                Prereq::Confinement => seats.exec,
                // A session with nothing gated needs nobody to decide.
                Prereq::ReachableAdjudicator => seats.write || seats.exec || seats.network,
                // The oracle is the point's own claim about who answers, and it is
                // false whatever is seated. `automode` with no oracle is `always-ask`
                // wearing a different banner, which is the lie this refuses.
                Prereq::Oracle => true,
            })
            .collect()
    }

    /// **Check the prerequisites, and refuse by name.**
    ///
    /// `have` is what the caller could actually build — read off the backend, the
    /// adjudicator and the confinement probe, never off which point was asked for.
    /// `seats` is what the role put in `tools_json`. Returns the refusal text, which
    /// names every missing piece **and how to attach it**, or `Ok(())`.
    ///
    /// There is deliberately no `fn best_available_mode`. A function that picked a
    /// weaker point for the operator is the silent downgrade, and having it available
    /// is how it gets called.
    pub fn check(&self, have: &[Prereq], seats: Seats) -> Result<(), String> {
        let needs = self.requires_given(seats);
        let missing: Vec<Prereq> = needs
            .iter()
            .copied()
            .filter(|p| !have.contains(p))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let mut s = format!(
            "the `{}` mode needs {} for what this session seats, and {}.\n\n",
            self.name,
            plural(needs.len(), "prerequisite"),
            if missing.len() == needs.len() {
                "none of them are here".to_string()
            } else {
                format!("{} of them are missing", missing.len())
            }
        );
        for p in &missing {
            s.push_str(&format!("  missing: {}\n    {}\n", p.as_str(), p.how()));
        }
        s.push_str(
            "\nThis refuses rather than opening at a weaker mode. A session whose banner \
             says one thing and whose behaviour is another costs a turn to discover, and \
             the operator would be the one to discover it.",
        );
        Err(s)
    }

    /// **Whether this point is no wider than `other`** — the clamp's rule, written
    /// once so that it is one rule rather than four comparisons in a caller.
    ///
    /// The axes that **admit** are compared, because those are the ones a point can
    /// widen: a disposition may narrow ([`Disposition::Admit`] to `Ask`, `Ask` to
    /// `Absent`), a grant scope may shorten ([`GrantScope::Session`] to `Once`), and
    /// a boundary may lose its licence (a consented box to the operator's, a
    /// structural one to either).
    ///
    /// The axes that **name** are deliberately not compared. `role` is a label for a
    /// seat this function must not touch — the tools are the seat's and a point does
    /// not seat one; `decider` says *who answers*, not *how much is allowed*, and
    /// `automode` is not wider than `writes allowed` on any axis that admits
    /// anything; and `requires` is not a width at all, it is what the seat must
    /// supply, which is [`Mode::check`]'s question and not this one's.
    pub fn no_wider_than(self, other: Mode) -> bool {
        fn rank(d: Disposition) -> u8 {
            match d {
                Disposition::Absent => 0,
                Disposition::Ask => 1,
                Disposition::Admit => 2,
            }
        }
        fn licence(b: Boundary) -> u8 {
            match b {
                // The operator's own box, with the always-ask list reaching them.
                Boundary::Operator => 0,
                // The same box with the list taken off, by a person, in advance.
                Boundary::OperatorConsented => 1,
                // Nothing inside reaches the operator at all.
                Boundary::Structural => 2,
            }
        }
        !matches!(self.grants, GrantScope::Session if other.grants == GrantScope::Once)
            && rank(self.write) <= rank(other.write)
            && rank(self.exec) <= rank(other.exec)
            && rank(self.network) <= rank(other.network)
            && licence(self.boundary) <= licence(other.boundary)
    }

    /// **The point a child of a session at this point opens at, given that child's
    /// own seat.**
    ///
    /// Two arms, and the second one exists because a child's point is not a choice
    /// anybody makes: nothing asks a subagent what it would like to be. The parent's
    /// point is **carried** down — that is the whole of [`Mode::ALLOW_ALL_HERE`]'s
    /// paragraph about consent belonging to a session and its work rather than to one
    /// process — and the child's *seat* decides whether it can carry it.
    ///
    /// # Why this is not the `best_available_mode` that `check` refuses to provide
    ///
    /// [`Mode::check`]'s own doc says there is deliberately no function that picks a
    /// weaker point, because a function that picks one **for the operator** is the
    /// silent downgrade — a banner saying one thing and a session doing another. That
    /// rule is about a person who asked for a point by name and it is unchanged:
    /// `/mode`, `--mode` and a project row still refuse by name and still hand back
    /// no weaker point. This is the other case, where the point was not chosen at
    /// all. A child inherits, and a seat that cannot carry what it inherited would
    /// otherwise be **refused at open** — measured 2026-09-20, where a row naming a
    /// point the child could not carry killed every subagent and the operator saw
    /// only *"subagents dont work"*. So the child gets the nearest point its seat
    /// *can* carry, and **says so by name**: the seat, the point that was inherited,
    /// what was missing, and what it opened at instead.
    ///
    /// # Never wider than the seat
    ///
    /// A point is permission and a seat is capability, and they are not
    /// interchangeable. `tools_json` is built from the **seat**, from the schemas the
    /// role actually resolved — nothing here reads [`Mode::role`], seats a tool, or
    /// widens one, so a point cannot hand a child a write tool it does not have: a
    /// read-only reviewer is a read-only reviewer at every point, because the tool it
    /// does not carry is not in its prompt at all (D9's rule — a capability boundary
    /// beats a flag — and the reason [`Disposition::Absent`] is a schema decision
    /// rather than a runtime check). What the clamp does own is the half that *can* be
    /// got wrong, and it is checked against what the seat really seated rather than
    /// against which role was asked for:
    ///
    ///   * the child's point is one this seat's own supplies satisfy
    ///     ([`Mode::check`], over the classes this seat put in `tools_json` and the
    ///     backend it actually built), and
    ///   * it is **never wider than the parent's** ([`Mode::no_wider_than`]), so
    ///     authority still only ever flows down: a stricter parent cannot spawn a
    ///     looser child, whatever that child's seat could carry.
    ///
    /// # The ladder, and why the consented point is not on it
    ///
    /// The search is [`Mode::NAMED`], widest first, keeping only points that are no
    /// wider than the parent's and that the seat can carry. [`Mode::ALLOW_ALL_HERE`]
    /// is deliberately absent from that ladder: it is reachable **only by being
    /// carried**, which is what makes *the consent was given for this session and
    /// its work* true without it becoming *consent for any child on any box*. A clamp
    /// can narrow a point that stands on a person's answer; it can never manufacture
    /// one.
    ///
    /// `read-only` is the last rung, it requires nothing, and it is no wider than any
    /// point — which is why the fallback below is unreachable rather than an error
    /// path, and why `a_clamp_always_lands_somewhere_the_seat_can_carry` is a test
    /// over the whole cross product rather than over the cases somebody remembered.
    pub fn inherited_by(self, have: &[Prereq], seats: Seats, seat: &str) -> Inherited {
        if self.check(have, seats).is_ok() {
            return Inherited::Carried(self);
        }
        let mode = Mode::NAMED
            .iter()
            .rev()
            .copied()
            .find(|m| m.no_wider_than(self) && m.check(have, seats).is_ok())
            .unwrap_or(Mode::READ_ONLY);
        let missing: Vec<&str> = self
            .requires_given(seats)
            .into_iter()
            .filter(|p| !have.contains(p))
            .map(|p| p.as_str())
            .collect();
        let what = if missing.is_empty() {
            "something this seat cannot supply".to_string()
        } else {
            missing.join(", ")
        };
        Inherited::Clamped {
            mode,
            why: format!(
                "the `{seat}` seat cannot carry `{}`: missing {what}. This child opens at \
                 `{}` instead — the nearest point that seat can carry, and never a wider \
                 one than its parent's.",
                self.name, mode.name
            ),
        }
    }

    /// One line for the startup disclosure.
    pub fn describe(&self) -> String {
        format!(
            "mode `{}` — {} (role {}, write {}, exec {}, network {}, grants {}, decided by {})",
            self.name,
            self.summary,
            self.role,
            self.write.as_str(),
            self.exec.as_str(),
            self.network.as_str(),
            self.grants.as_str(),
            self.decider.as_str(),
        )
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// **What an unseen project starts at.**
///
/// Not read-only, and not writes-allowed. A directory nobody has said anything about
/// gets the point where the operator is asked about everything that is not a read:
/// nothing happens without them, and the first thing they do is not *"why can this
/// session not edit"*, which is the complaint that started this.
///
/// Moving off it is one act, recorded per project — see the daemon's mode store.
pub const UNSEEN_PROJECT: Mode = Mode::ALWAYS_ASK;

#[cfg(test)]
mod tests;
