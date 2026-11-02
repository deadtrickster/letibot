//! A named point in the four axes this harness already had: role, tier policy, grant
//! scope and adjudicator. Four independent knobs are four ways to be wrong; a named
//! coordinate is one an operator can hold in their head.
//!
//! ```text
//! Mode { name: "writes allowed",
//!        role: coder, write→Admit, exec→Ask, read→Auto,
//!        grants: Session, adjudicator: Head,
//!        requires: [WritableBackend, ReachableAdjudicator] }
//! ```
//!
//! A point declares what it [`requires`](Mode::requires) and selecting it checks
//! them: an unmet prerequisite refuses by name, saying what is missing and how to
//! attach it, never a silent downgrade to a weaker point.
//!
//! A point governs [`Tier::MayApprove`] and nothing else, so a standing permission
//! cannot cover an always-ask entry: [`crate::adjudicate::Adjudicable`] is minted only
//! for `MayApprove`, and no point widens [`Tier::AlwaysAsk`] or `Blocked`.

use crate::adjudicate::Tier;
use crate::schema::Access;

/// What a [`Tier::MayApprove`] finding costs for one access class. Two values, not
/// three: the model deciding is the adjudicator axis, not a third disposition, and
/// folding it in would need a rule for when the two disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Not seated at this point: absent from `tools_json` rather than refused at call
    /// time, because a model carrying a tool it can never use will try it - a
    /// capability boundary beats a flag.
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

/// Axis 3: how far one answer travels. The ceiling is a session: anything beyond it
/// is a point - durable, named, visible in the banner - rather than a grant, which is
/// a permanent widening nobody remembers making.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantScope {
    /// An answer settles one call and nothing else.
    Once,
    /// An answer may also settle later calls that normalise to the same
    /// `(program, ActionClass)` for the rest of the session. Never a text match; see
    /// [`crate::adjudicate`].
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
    /// A person, over whatever can reach one - the session socket, or the daemon's own
    /// console.
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

/// Something a point needs before it can honestly be selected. Every one is a
/// capability seam that already exists, so a point is blocked on wiring rather than on
/// hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Prereq {
    /// A backend opened for writing. An admitted write over a read-only backend is a
    /// session that starts cleanly and fails every call: two gates, and the second one
    /// is shut.
    WritableBackend,
    /// An adjudicator that can actually reach whoever decides. Not "an adjudicator
    /// exists": one over a daemon whose stdin nobody types into is attached and
    /// unreachable, and that is the distinction this prerequisite is named for.
    ReachableAdjudicator,
    /// An `AuthorisationOracle`. A loaded model, a budget it is abandoned for
    /// overrunning, and a scope earned by measurement.
    Oracle,
    /// A confinement the exec substrate can actually build. `crates/tools/src/exec`
    /// says plainly that it is not a sandbox on its own.
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

    /// How to attach it. A refusal that says what is missing and not how to supply it
    /// is one the operator routes around by picking a weaker point, which is the silent
    /// downgrade with an extra step.
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
                // Name the flag rather than the state of the code: a prerequisite that
                // reports a missing capability when what is missing is a flag sends the
                // operator to look for the wrong thing.
                "put the guard's address in ~/.config/letibot/providers.toml under \
                 `[gatekeeper] endpoint = \"HOST:PORT\"` — a llama.cpp `/completion` \
                 endpoint that answers `did the operator ask for this`. Its own \
                 endpoint, not `--endpoint`: the guard need not be the model doing \
                 the work. `--oracle HOST:PORT` overrides it for one run"
            }
            Prereq::Confinement => {
                // Name the thing the reader can change: this reads the PLACEMENT
                // (`backend_confined` comes from `placement == Firecode` and nothing
                // else), so no cgroup or namespace on this box can supply it.
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

/// What the seated role actually put in `tools_json`. Read off the resolved schemas,
/// never off which role was asked for: a role that failed to resolve some of its tools
/// seats fewer than its name suggests.
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

/// Who bears the consequence of an action - the operator's box, or a boundary built so
/// that nothing crosses it. `Structural` admits the always-ask list because nothing
/// there reaches the operator; the flow rules are layer A's and refuse before any point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// The operator's own box: the always-ask list and the flow rules reach them.
    Operator,
    /// A boundary that makes the action structural: nothing here reaches the
    /// operator, so nothing asks.
    Structural,
    /// The operator's own box, and the operator said so: there is no structural boundary
    /// here, so `sudo` is `sudo` and a delete deletes their file. It does not move
    /// [`Tier::Blocked`]: consent widens what may be done without asking, not the flow rules.
    OperatorConsented,
}

/// A named coordinate. Others are legal - a caller may build one nobody named - but a
/// name is what makes a point auditable: a banner saying "writes allowed" can be
/// disagreed with, and a banner listing four axis values is one nobody reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub name: &'static str,
    /// Axis 1. The role name a session seats. Roles answer which tools, never how much
    /// approval: conflating them is why `--role coder` meant both "has write" and
    /// "asks about writes".
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
    /// Axis 5: who bears an action's consequence, and so whether the always-ask list
    /// and the flow rules still reach a person from this point.
    pub boundary: Boundary,
    /// What must be true before this point can be selected.
    pub requires: &'static [Prereq],
    /// One line for the banner, in the operator's terms rather than the axes'.
    pub summary: &'static str,
}

/// What a child's point is, and where it came from. A `bool` beside a `Mode` is the
/// shape in which "was this clamped" stops being read, so the two states are named and
/// the disclosure asks for the sentence rather than for a flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inherited {
    /// The parent's point, carried unchanged, because this seat's own supplies satisfy
    /// it. Consent travels this way and only this way - see [`Mode::ALLOW_ALL_HERE`].
    Carried(Mode),
    /// The nearest point this seat can carry, because it cannot carry the parent's, and
    /// the sentence that says so, naming the seat. Never `Carried` of a wider point, and
    /// never a point the seat's own supplies cannot satisfy.
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

    /// Why this is not the parent's point, or `None` when it is. Not a debugging aid: a
    /// clamp nobody can read is the silent downgrade under another name, so the settings
    /// row and the open's note carry this sentence.
    pub fn why(&self) -> Option<&str> {
        match self {
            Inherited::Carried(_) => None,
            Inherited::Clamped { why, .. } => Some(why),
        }
    }
}

impl Mode {
    /// Reads, and nothing else: a read inside the boundary never prompts, so there is
    /// nothing to attach and nothing that can refuse.
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

    /// Write and exec are seated and every non-read action asks.
    /// `WritableBackend` has to be here: a seated write over a read-only backend is a
    /// session where an admitted call still cannot reach the disk.
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

    /// Writes go through. Exec still asks, and so does the always-ask list, except
    /// inside a boundary letibot has measured, where a command that crosses nothing is
    /// admitted without a card; the `exec` disposition here stays `Ask`.
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
        summary: "writes go through without asking, and so does a command inside a \
                  confinement letibot has measured — one that crosses nothing and stays in \
                  the checkout (D16). What still asks is what crosses: the network, a path \
                  outside the project, the always-ask list — and, where no boundary was \
                  measured, any command that runs the project's own code, every time, if \
                  `[permissions] ask_unconfined_code` is on (R98; off by default).",
    };

    /// The model adjudicates within `MayApprove`. Selectable and it refuses, which is the
    /// point: a missing point reads as "this build cannot do that". It does not widen
    /// `write` to `Admit`: what automode changes is who answers, not what asks.
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
                  still reaches you — and no mode promotes an action the boundary blocks.",
    };

    /// Writes go through and a model answers for the rest. The write is deliberately not
    /// path-aware: a write into the never-write list is [`Tier::Blocked`] before this point
    /// is read. Exec and network still ask, and the oracle answers them.
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

    /// Everything is admitted, nothing asks. Only sane inside a boundary that makes the
    /// action structural: on a bare host this is opencode's `bypassPermissions` posture,
    /// and the `Confinement` prerequisite is what refuses it there.
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

    /// `allow-all` on a box with no boundary, which the operator consented to: the boundary
    /// is `OperatorConsented` and there is no confinement to require. It is not in
    /// [`Mode::NAMED`], so only a head that asked the operator first selects it.
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
                  The confirmation is RECORDED with the project and the session, in the \
                  session store, so it survives a restart and a reseat and is read aloud at \
                  every startup; `/mode <other>` takes it back and forgets the answer.",
    };

    /// The named points, in widening order - the order a banner lists them in and an
    /// operator reads as a ladder. [`Mode::ALLOW_ALL_HERE`] is deliberately absent: a
    /// point that stands for a person's confirmation must not be selectable by a string.
    pub const NAMED: &'static [Mode] = &[
        Mode::READ_ONLY,
        Mode::ALWAYS_ASK,
        Mode::WRITES_ALLOWED,
        Mode::AUTO,
        // Wider than `automode` on exactly one axis - `write` - and identical on every
        // other, which is why it sits here and not beside `writes allowed`.
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
            // Not mapped to `acceptEdits`: that name belongs to `writes allowed`, and two
            // points answering to one name would make `parse` pick by list order rather
            // than by meaning.
            _ => None,
        }
    }

    /// Look one up by the name an operator types or a store holds - letibot's own name,
    /// or the opencode permission-mode name it maps to. An unknown value names the four
    /// rather than falling back: a silent read-only seat is a typo nobody sees.
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

    /// Does this point admit everything - write, exec and network unasked, the always-ask
    /// list included? True for both `allow-all` coordinates and nothing else; the gate asks
    /// so a command the grammar could not read goes on to the mode rather than being refused.
    pub fn admits_everything(&self) -> bool {
        self.write == Disposition::Admit
            && self.exec == Disposition::Admit
            && self.network == Disposition::Admit
            && self.boundary != Boundary::Operator
    }

    /// What a `MayApprove` finding costs for one access class at this point.
    /// [`Access::Read`] is [`Disposition::Admit`] at every point: a point that could make
    /// a read ask could make the harness unusable by moving one dial.
    pub fn disposition(&self, access: Access) -> Disposition {
        match access {
            Access::Write => self.write,
            Access::Exec => self.exec,
            Access::Network => self.network,
            _ => Disposition::Admit,
        }
    }

    /// Whether this point admits a call without consulting anybody. `Tier::Auto` is not a
    /// blanket yes: the runtime consults the gate only when the tool's declared access is
    /// not read, so a call here is one whose tool declared write, exec or network.
    pub fn admits_unasked(&self, tier: &Tier, access: Access) -> bool {
        match tier {
            Tier::Auto | Tier::MayApprove => self.disposition(access) == Disposition::Admit,
            // Inside a structural boundary there is nobody the list protects, and a
            // consented box took it off with the list in front of them - see [`Boundary`].
            // Written as "not `Operator`" so a third boundary is a compile error here.
            Tier::AlwaysAsk { .. } => self.boundary != Boundary::Operator,
            // Layer A's own tier - a secret disclosed across the boundary - is refused by
            // the gate before any point is consulted, and a point cannot reach it. A
            // credential in the copy is still a credential.
            Tier::Blocked { .. } => false,
        }
    }

    /// Whether an access class is seated at all here.
    pub fn seats(&self, access: Access) -> bool {
        self.disposition(access) != Disposition::Absent
    }

    /// What this point needs, given what the session actually seats. A prerequisite is about
    /// a capability the session will use, so a read-only role at `always-ask` needs no
    /// writable backend; requiring one anyway would refuse to open every read-only session.
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

    /// Check the prerequisites, and refuse by name. `have` is what the caller could build,
    /// never what was asked for; `seats` is what the role put in `tools_json`. There is
    /// deliberately no `best_available_mode`, because a silent downgrade is how it gets called.
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

    /// Whether this point is no wider than `other` - the clamp's rule, written once. The axes
    /// that admit are compared; `role`, `decider` and `requires` only name, and `requires` is
    /// [`Mode::check`]'s question rather than a width.
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

    /// The point a child of a session at this point opens at, given that child's own seat.
    /// The parent's point is carried when the seat can carry it, and otherwise the child opens
    /// at the nearest point it can and says so by name. Never wider than the parent's.
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

/// What an unseen project starts at. Not read-only and not writes-allowed: the operator
/// is asked about everything that is not a read, and moving off it is one act recorded
/// per project.
pub const UNSEEN_PROJECT: Mode = Mode::ALWAYS_ASK;

#[cfg(test)]
mod tests;
