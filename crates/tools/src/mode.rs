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
                "exec needs a cgroup v2 subtree and a usable unprivileged namespace; \
                 `Bwrap` probes for one and `NoConfinement` refuses rather than running \
                 unscoped"
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
    /// [`Mode::AUTO`]. **What it trades** is stated rather than implied: an edit
    /// inside the boundary takes effect with nobody consulted at all — not the
    /// operator, and not the model either. §3's asymmetry is why that is
    /// defensible while the same move on `exec` is not: the boundary guards the
    /// filesystem view, a `write` result is shaped by the tool, and `bash` is the
    /// one tool whose result is an arbitrary byte stream. So exec and network
    /// still ask, and the oracle still answers them.
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

    /// The named points, in widening order. The order is the one a banner lists them
    /// in and the one an operator reads as a ladder.
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
            // Inside a structural boundary there is nobody the list protects — see
            // [`Boundary`].
            Tier::AlwaysAsk { .. } => self.boundary == Boundary::Structural,
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
mod tests {
    use super::*;

    /// **The sixth point widens exactly one dial, and the secret stores are not
    /// behind it.** The operator, on adding it (2026-09-17): *"that being said -
    /// .ssh and friends must stay protected"*.
    ///
    /// Three separate mechanisms keep that true, and none of them is the mode:
    /// `Tier::Blocked` answers `false` here at every point; `Tier::AlwaysAsk`
    /// answers `false` at every point whose boundary is the operator's; and
    /// `NEVER_WRITE` is checked by the gate at step 1, before any point is
    /// consulted at all. This pins the two this file owns.
    #[test]
    fn automode_edits_widens_write_and_nothing_else() {
        let m = Mode::AUTO_EDITS;
        // The one dial that moved, against `automode`.
        assert!(m.admits_unasked(&Tier::MayApprove, Access::Write));
        assert!(!Mode::AUTO.admits_unasked(&Tier::MayApprove, Access::Write));
        // Everything else is `automode`'s, exactly.
        assert_eq!(m.exec, Mode::AUTO.exec);
        assert_eq!(m.network, Mode::AUTO.network);
        assert_eq!(m.decider, Mode::AUTO.decider);
        assert_eq!(m.boundary, Mode::AUTO.boundary);
        assert_eq!(m.requires, Mode::AUTO.requires);
        assert!(!m.admits_unasked(&Tier::MayApprove, Access::Exec));
        assert!(!m.admits_unasked(&Tier::MayApprove, Access::Network));

        // And the two tiers no point on the operator's box may move.
        let blocked = Tier::Blocked {
            rule: crate::adjudicate::FlowRule::SecretOffBox,
            evidence: "a key, leaving".into(),
        };
        let always = Tier::AlwaysAsk { rule: "credential_use", why: "a key, used".into() };
        for access in [Access::Read, Access::Write, Access::Exec, Access::Network] {
            assert!(!m.admits_unasked(&blocked, access), "{access:?} at a blocked tier");
            assert!(!m.admits_unasked(&always, access), "{access:?} at an always-ask tier");
        }
    }

    /// It is in the ladder, it parses by name, and it does not answer to
    /// `acceptEdits` — that name is `writes allowed`'s, and two points under one
    /// name would be resolved by list order rather than by meaning.
    #[test]
    fn the_sixth_point_is_named_and_reachable() {
        assert!(Mode::NAMED.iter().any(|m| m.name == "automode-edits"));
        assert_eq!(Mode::parse("automode-edits").unwrap().name, "automode-edits");
        assert_eq!(Mode::parse("automode_edits").unwrap().name, "automode-edits");
        assert_eq!(Mode::parse("acceptEdits").unwrap().name, "writes allowed");
        assert!(Mode::parse("automode-edit").is_err(), "a near miss is named, not guessed");
    }

    /// The two tiers a point on the operator's box may not move, at every such point
    /// including automode. This is the property the design rests on, so it is checked
    /// against the full cross product rather than against the case somebody
    /// remembered. `allow-all` is the one point that is not on the operator's box —
    /// its prerequisite is a confinement — and it is checked the other way below.
    #[test]
    fn no_point_on_the_operators_box_moves_always_ask_or_blocked() {
        let ask = Tier::AlwaysAsk {
            rule: "privilege_escalation",
            why: "test".into(),
        };
        let never = Tier::Blocked {
            rule: crate::adjudicate::FlowRule::SecretToTranscript,
            evidence: "test".into(),
        };
        for m in Mode::NAMED
            .iter()
            .filter(|m| m.boundary == Boundary::Operator)
        {
            assert_ne!(m.name, "allow-all");
            for access in [Access::Read, Access::Write, Access::Exec, Access::Network] {
                assert!(
                    !m.admits_unasked(&ask, access),
                    "{} admitted an always-ask for {access:?}",
                    m.name
                );
                assert!(
                    !m.admits_unasked(&never, access),
                    "{} admitted an blocked for {access:?}",
                    m.name
                );
            }
        }
    }

    /// The operator's rule, 2026-09-14: *"allow-all should be the true allow-all."*
    /// Measured before it: inside a VM, `sudo -n true` and `git clone github.com`
    /// each raised a decision nobody was attached to answer. The point requires a
    /// confinement, and inside one nothing on the list reaches the operator.
    #[test]
    fn allow_all_inside_a_boundary_admits_the_always_ask_list() {
        let ask = Tier::AlwaysAsk {
            rule: "privilege_escalation",
            why: "test".into(),
        };
        let never = Tier::Blocked {
            rule: crate::adjudicate::FlowRule::SecretToTranscript,
            evidence: "test".into(),
        };
        assert_eq!(Mode::ALLOW_ALL.boundary, Boundary::Structural);
        assert!(Mode::ALLOW_ALL.requires.contains(&Prereq::Confinement));
        for access in [Access::Write, Access::Exec, Access::Network] {
            assert!(Mode::ALLOW_ALL.admits_unasked(&ask, access), "{access:?}");
            // A secret across the boundary is layer A's refusal, before any point.
            assert!(
                !Mode::ALLOW_ALL.admits_unasked(&never, access),
                "{access:?}"
            );
        }
        // The same coordinate on the operator's box is not allow-all, whatever it
        // is called: the boundary axis is what admits, not the name.
        let on_host = Mode {
            boundary: Boundary::Operator,
            ..Mode::ALLOW_ALL
        };
        assert!(!on_host.admits_unasked(&ask, Access::Exec));
    }

    /// Clause 4 the other way round: a read never prompts, at any point, and a point
    /// that could make one prompt would be a point that breaks the harness by being
    /// selected.
    #[test]
    fn a_read_never_prompts_at_any_point() {
        for m in Mode::NAMED {
            assert!(m.admits_unasked(&Tier::Auto, Access::Read));
            assert!(m.admits_unasked(&Tier::MayApprove, Access::Read));
        }
    }

    /// **`Tier::Auto` is not a blanket yes**, and this is the case that says why.
    ///
    /// `bash("cat x")` derives `Tier::Auto` — the action is a read inside the
    /// boundary — but it arrived here because the *tool* declared exec. A point that
    /// admitted it unasked would make `bash` unaskable for every read command, and
    /// `bash` is the tool whose result is an arbitrary byte stream. Clause 4 lives in
    /// the runtime, which never consults the gate for a read-only tool at all.
    #[test]
    fn an_auto_tier_over_an_exec_tool_still_asks() {
        assert!(
            !Mode::ALWAYS_ASK.admits_unasked(&Tier::Auto, Access::Exec),
            "a read run through `bash` is still a `bash` call"
        );
        assert!(!Mode::WRITES_ALLOWED.admits_unasked(&Tier::Auto, Access::Exec));
        // And a write tool at writes-allowed does go through, tier notwithstanding.
        assert!(Mode::WRITES_ALLOWED.admits_unasked(&Tier::Auto, Access::Write));
    }

    /// **The space is fully reachable.** The four are the *named* points, not the only
    /// legal ones: a caller may build a coordinate nobody named, which is the whole
    /// reason this is a struct with four fields rather than an enum with four
    /// variants.
    #[test]
    fn an_unnamed_coordinate_is_constructible() {
        let strict_writes = Mode {
            name: "ask about writes, remember the answer",
            write: Disposition::Ask,
            grants: GrantScope::Session,
            ..Mode::WRITES_ALLOWED
        };
        assert!(!strict_writes.admits_unasked(&Tier::MayApprove, Access::Write));
        assert_eq!(strict_writes.grants, GrantScope::Session);
        assert!(
            Mode::parse(strict_writes.name).is_err(),
            "an unnamed point is reachable in code and not by name, which is what \
             `NAMED` being a list of four means"
        );
    }

    /// The operator's own table: writes go through at *writes allowed*, exec still
    /// asks there, and both ask at *always-ask*.
    #[test]
    fn the_named_points_behave_as_the_operator_named_them() {
        let t = Tier::MayApprove;
        assert!(!Mode::ALWAYS_ASK.admits_unasked(&t, Access::Write));
        assert!(!Mode::ALWAYS_ASK.admits_unasked(&t, Access::Exec));

        assert!(Mode::WRITES_ALLOWED.admits_unasked(&t, Access::Write));
        assert!(
            !Mode::WRITES_ALLOWED.admits_unasked(&t, Access::Exec),
            "exec still asks at writes-allowed: §3's transcript choke point does not \
             exist, and `bash` is the tool whose result is an arbitrary byte stream"
        );

        // Automode changes who answers, not what asks.
        assert!(!Mode::AUTO.admits_unasked(&t, Access::Write));
        assert_eq!(Mode::AUTO.decider, Decider::Model);

        assert!(!Mode::READ_ONLY.seats(Access::Write));
        assert!(!Mode::READ_ONLY.seats(Access::Exec));
    }

    /// An unmet prerequisite refuses **by name**, says how to attach it, and does not
    /// hand back a weaker point. The last assertion is the one that matters: there is
    /// no `Ok` on this path at all.
    #[test]
    fn a_missing_prerequisite_refuses_by_name_and_never_downgrades() {
        let seats = Seats {
            write: true,
            exec: true,
            network: false,
        };
        let e = Mode::AUTO
            .check(
                &[
                    Prereq::WritableBackend,
                    Prereq::ReachableAdjudicator,
                    Prereq::Confinement,
                ],
                seats,
            )
            .unwrap_err();
        assert!(e.contains("automode"), "{e}");
        assert!(e.contains("authorisation oracle"), "{e}");
        assert!(
            e.contains("refuses by name") || e.contains("weaker mode"),
            "{e}"
        );
        // And it says how, not only what — which today is a flag, not a build
        // state: the oracle seam exists, so "nothing supplies one" would send the
        // operator to read code instead of passing an argument.
        assert!(e.contains("gatekeeper"), "{e}");

        // Everything present is fine.
        assert!(
            Mode::AUTO
                .check(
                    &[
                        Prereq::WritableBackend,
                        Prereq::ReachableAdjudicator,
                        Prereq::Confinement,
                        Prereq::Oracle
                    ],
                    seats
                )
                .is_ok()
        );
        // Read-only needs nothing, which is why it is always available.
        assert!(Mode::READ_ONLY.check(&[], Seats::default()).is_ok());
    }

    /// **A prerequisite is about a capability the session will use.**
    ///
    /// The regression this is written against: requiring a writable backend
    /// unconditionally made every read-only session refuse to open, because the
    /// default point is `always-ask` and the default role seats no write tools. A
    /// prerequisite that fires for a capability nobody has is not conservative, it is
    /// wrong — and it is the kind of wrong that reads as safety.
    #[test]
    fn a_read_only_seat_needs_no_writable_backend_whatever_the_point_says() {
        assert!(
            Mode::ALWAYS_ASK.check(&[], Seats::READS_ONLY).is_ok(),
            "a session with nothing to write with needs nowhere to write"
        );
        // And it still needs one the moment write is seated.
        let e = Mode::ALWAYS_ASK
            .check(
                &[Prereq::ReachableAdjudicator],
                Seats {
                    write: true,
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(e.contains("writable backend"), "{e}");

        // `automode`'s oracle is not seat-dependent: a point that claims a model
        // answers and has none is `always-ask` wearing a different banner, whatever
        // the session seats.
        let e = Mode::AUTO.check(&[], Seats::READS_ONLY).unwrap_err();
        assert!(e.contains("oracle"), "{e}");
    }

    /// Automode is **selectable**. A point that is missing reads as "this build cannot
    /// do that"; a point that refuses by name reads as "this build can, and here is
    /// the piece that is absent".
    #[test]
    fn automode_exists_and_says_what_it_needs() {
        let m = Mode::parse("automode").expect("automode is a point, not a plan");
        assert_eq!(m.decider, Decider::Model);
        assert!(m.requires.contains(&Prereq::Oracle));
    }

    #[test]
    fn an_unknown_mode_names_the_four_rather_than_defaulting() {
        let e = Mode::parse("yolo").unwrap_err();
        for name in ["read-only", "always-ask", "writes allowed", "automode"] {
            assert!(e.contains(name), "{e}");
        }
        // The spellings a person actually types.
        assert_eq!(
            Mode::parse("writes-allowed").unwrap().name,
            "writes allowed"
        );
        assert_eq!(
            Mode::parse("writes allowed").unwrap().name,
            "writes allowed"
        );
        assert_eq!(Mode::parse("READ_ONLY").unwrap().name, "read-only");
    }

    /// An unseen directory starts where nothing happens without the operator, and not
    /// at read-only — the complaint that started this was a session that could not
    /// edit and had no way to change that in place.
    #[test]
    fn an_unseen_project_starts_at_always_ask() {
        assert_eq!(UNSEEN_PROJECT.name, "always-ask");
        assert!(UNSEEN_PROJECT.seats(Access::Write));
    }

    /// Every named point's prerequisites are things this crate has a name for, and
    /// every prerequisite says how to attach it. A refusal that says what is missing
    /// and not how to supply it is one the operator routes around.
    #[test]
    fn every_prerequisite_says_how() {
        for p in [
            Prereq::WritableBackend,
            Prereq::ReachableAdjudicator,
            Prereq::Oracle,
            Prereq::Confinement,
        ] {
            assert!(!p.as_str().is_empty());
            assert!(p.how().len() > 40, "{:?} has no real instruction", p);
        }
    }
}
