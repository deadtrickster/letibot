//! `review_` - the reviewer's brief and the verdict's floor (X18 of the wide review): the words a
//! reviewer is started with, and the rung its verdict must reach. A layer may require a stricter
//! verdict and may never raise one: the floor merges through [`Floor::strictest`], and nothing here
//! can admit a branch or skip the gate.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The family. Every hook of this kind is exported under it, as `pane_` and `adjudicate_` are.
pub const PREFIX: &str = "review_";

/// The reviewer's brief: what the child that judges one branch is told.
pub const BRIEF: &str = "review_brief";

/// The verdict's floor: the rung a layer requires of a verdict, never a way to reach one.
pub const VERDICT: &str = "review_verdict";

/// Whether `hook` is one of this family's, which is how the host knows to ask it of the queue's
/// stack rather than of a session's own.
pub fn is_review_hook(hook: &str) -> bool {
    hook == BRIEF || hook == VERDICT
}

/// The facts of one review, as the host knows them: the ask the work was for, and the placement.
/// There is no field for the subagent's own report, for the reason the native prompt has none -
/// a reviewer told what the author says about the work is reviewing the story, not the artifact.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    /// The ask, verbatim: the only framing the reviewer starts from.
    pub ask: String,
    /// The branch under review.
    pub branch: String,
    /// The base the diff is read against.
    pub base: String,
    /// The head under review - the commit that lands, which is not the branch's tip when a commit
    /// was made while the review ran.
    pub head: String,
    /// Where the reviewer's commands run, empty for a reviewer with no path to name (a VM).
    #[serde(default)]
    pub cwd: String,
}

impl Entry {
    pub fn of(
        ask: impl Into<String>,
        branch: impl Into<String>,
        base: impl Into<String>,
        head: impl Into<String>,
        cwd: impl Into<String>,
    ) -> Entry {
        Entry {
            ask: ask.into(),
            branch: branch.into(),
            base: base.into(),
            head: head.into(),
            cwd: cwd.into(),
        }
    }
}

/// What `review_brief` is handed. `entry` is `None` for the review queue's OWN session prompt:
/// the queue is told what its reviewers are for, and a project's policy paragraph belongs there
/// as much as in a reviewer's brief.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BriefIn {
    /// [`V`](crate::V).
    pub v: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<Entry>,
}

impl BriefIn {
    /// One reviewer's brief.
    pub fn of(entry: Entry) -> BriefIn {
        BriefIn {
            v: crate::V,
            entry: Some(entry),
        }
    }

    /// The review queue's own prompt, which is about its reviewers rather than about one entry.
    pub fn queue() -> BriefIn {
        BriefIn {
            v: crate::V,
            entry: None,
        }
    }

    /// The synthetic input `plugin test` and the shadow gate use when a session has none recorded.
    pub fn synthetic() -> BriefIn {
        BriefIn::of(Entry::of(
            "add a gatekeeper seat and a brief-first protocol",
            "agent/gatekeeper",
            "a4fbe70",
            "9d1c2ab",
            "/build/wt-gatekeeper",
        ))
    }
}

/// What `review_brief` answers: the words for the reviewer, in this layer's own voice. `brief`
/// REPLACES the host's policy body (kept as the fallback a layer that traps or stays silent falls
/// to), `brief_add` is appended after it, and `require` becomes rules of its own - so a layer that
/// adds a rule does not have to restate the host's.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Brief {
    /// The policy body, in this layer's words. `None` keeps the host's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
    /// Words added after the body: one rule this project adds to the host's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_add: Option<String>,
    /// What the reviewer MUST do, one item per line, rendered as rules of their own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub require: Vec<String>,
}

impl Brief {
    /// Nothing said: the host's own text stands.
    pub fn nothing() -> Brief {
        Brief::default()
    }

    /// This body in place of the host's.
    pub fn saying(body: impl Into<String>) -> Brief {
        Brief {
            brief: Some(body.into()),
            ..Brief::default()
        }
    }

    /// Whether this answer asks for anything at all.
    pub fn says_nothing(&self) -> bool {
        self.brief.is_none() && self.brief_add.is_none() && self.require.is_empty()
    }
}

/// A verdict's rung, ordered the way the queue treats it: a `reject` outranks `needs_human`, which
/// outranks `accept`, so [`Floor::strictest`] is the greater of two and a floor can only ever be
/// the more cautious of the two answers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Floor {
    /// The artifact may land: the reviewer's own `accept` stands.
    #[default]
    Accept,
    /// A person decides: the row goes to the operator rather than landing or being refused.
    NeedsHuman,
    /// The artifact is refused: the row is rejected, and the author is told why.
    Reject,
}

impl Floor {
    /// Every rung, weakest first.
    pub const ALL: [Floor; 3] = [Floor::Accept, Floor::NeedsHuman, Floor::Reject];

    pub fn as_str(self) -> &'static str {
        match self {
            Floor::Accept => "accept",
            Floor::NeedsHuman => "needs_human",
            Floor::Reject => "reject",
        }
    }

    /// The rung a word names - the same three words a verdict is written in, so a layer spells a
    /// floor the way the reviewer spells its decision.
    pub fn parse(word: &str) -> Option<Floor> {
        Floor::ALL.into_iter().find(|f| f.as_str() == word.trim())
    }

    /// The stricter of two rungs: the only way two combine, so a merge can never relax a verdict.
    pub fn strictest(self, other: Floor) -> Floor {
        if other > self { other } else { self }
    }
}

impl Serialize for Floor {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// An unknown rung is refused here rather than read as silence: a floor this host cannot place is
/// a policy nobody can say the queue acted on, and the host then asks a person instead.
impl<'de> Deserialize<'de> for Floor {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Floor, D::Error> {
        let word = String::deserialize(d)?;
        Floor::parse(&word).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "`{word}` is not a verdict's rung: a floor is `accept`, `needs_human` or `reject`"
            ))
        })
    }
}

/// What `review_verdict` is handed: the verdict the reviewer reached, and its reasons.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VerdictIn {
    /// [`V`](crate::V).
    pub v: u32,
    /// The word the reviewer handed in.
    pub verdict: Floor,
    #[serde(default)]
    pub reasons: Vec<String>,
}

impl VerdictIn {
    /// One verdict, as the queue reads it off the desk.
    pub fn new(verdict: Floor, reasons: Vec<String>) -> VerdictIn {
        VerdictIn {
            v: crate::V,
            verdict,
            reasons,
        }
    }

    /// The synthetic input `plugin test` and the shadow gate use when a session has none recorded.
    pub fn synthetic() -> VerdictIn {
        VerdictIn::new(
            Floor::Accept,
            vec!["the change does what the brief asked".into()],
        )
    }
}

/// What `review_verdict` answers: the rung this layer requires, and why. Merged through
/// [`Floor::strictest`], `accept` over a reviewer's `reject` changes nothing and `reject` over an
/// `accept` parks the branch; there is no field that admits one.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verdict {
    /// The rung this layer requires. Absent is *nothing stricter than the reviewer's own verdict*.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor: Option<Floor>,
    /// Why, in the layer's words - it reaches the row and the operator's card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

impl Verdict {
    /// Nothing asked: the verdict stands as it was handed in.
    pub fn nothing() -> Verdict {
        Verdict::default()
    }

    /// This rung at least, and this reason.
    pub fn requiring(floor: Floor, why: impl Into<String>) -> Verdict {
        Verdict {
            floor: Some(floor),
            why: Some(why.into()),
        }
    }

    /// The rung this answer requires, or `None` for silence. A floor of `accept` is silence: it is
    /// the weakest rung, so requiring it can only ever be the reviewer's own word back.
    pub fn required(&self) -> Option<Floor> {
        self.floor.filter(|f| *f != Floor::Accept)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Both exports are the family's, which is what makes a manifest's `hooks` and the module's
    /// exports the same words.
    #[test]
    fn both_exports_are_under_the_family() {
        for hook in [BRIEF, VERDICT] {
            assert!(
                hook.strip_prefix(PREFIX)
                    .is_some_and(|rest| !rest.is_empty()),
                "{hook} is not under {PREFIX}"
            );
            assert!(is_review_hook(hook), "{hook}");
        }
        assert!(!is_review_hook(PREFIX), "a name is not a hook");
        assert!(!is_review_hook("review_"));
    }

    /// The plan's own shape: the brief's answer adds to the host's, and its requirements are a list.
    #[test]
    fn the_briefs_answer_is_the_plans_shape() {
        let out: Brief = serde_json::from_str(
            r#"{"brief_add":"Wherever a test is touched, say which one ran.","require":["tests touched for crates/tools"]}"#,
        )
        .expect("the plan's example parses");
        assert!(out.brief.is_none(), "and it replaces nothing");
        assert_eq!(out.require.len(), 1);
        assert!(!out.says_nothing());
        assert!(Brief::nothing().says_nothing());
    }

    /// A field the host has not got is refused by name rather than dropped: an answer that
    /// believed it was adding a rule must not read as silence.
    #[test]
    fn an_answer_with_a_field_of_its_own_is_refused() {
        let why = serde_json::from_str::<Brief>(r#"{"brief_add":"x","seat_add":["bash"]}"#)
            .expect_err("no such field");
        assert!(why.to_string().contains("seat_add"), "{why}");
        let why = serde_json::from_str::<Verdict>(r#"{"floor":"accept","admit":true}"#)
            .expect_err("no such field");
        assert!(why.to_string().contains("admit"), "{why}");
    }

    /// The rungs are the verdict's own three words, and one word out is not one of them.
    #[test]
    fn the_floor_words_are_the_verdicts_three() {
        for f in Floor::ALL {
            assert_eq!(Floor::parse(f.as_str()), Some(f));
        }
        assert_eq!(Floor::ALL.len(), 3);
        assert_eq!(Floor::parse("Accept"), None, "the words are exact");
        assert_eq!(Floor::parse("maybe"), None);
        let why = serde_json::from_str::<Verdict>(r#"{"floor":"ship"}"#).expect_err("no rung");
        assert!(why.to_string().contains("needs_human"), "{why}");
    }

    /// A floor merges one way: the stricter of the two. `accept` over a `reject` is a `reject`,
    /// which is the whole of *a layer can never raise a reject to an accept*.
    #[test]
    fn a_floor_can_only_ever_tighten() {
        assert_eq!(Floor::Accept.strictest(Floor::Reject), Floor::Reject);
        assert_eq!(Floor::Reject.strictest(Floor::Accept), Floor::Reject);
        assert_eq!(
            Floor::NeedsHuman.strictest(Floor::Accept),
            Floor::NeedsHuman
        );
        assert_eq!(Floor::Accept.strictest(Floor::Accept), Floor::Accept);
        // And requiring the weakest rung is silence: it can only repeat the reviewer's own word.
        assert_eq!(Verdict::requiring(Floor::Accept, "fine").required(), None);
        assert_eq!(
            Verdict::requiring(Floor::Reject, "the test asserts nothing").required(),
            Some(Floor::Reject)
        );
    }

    /// The synthetic inputs are the host's own shape, and a queue input carries no entry.
    #[test]
    fn the_synthetic_inputs_are_the_hosts_shape() {
        let brief = serde_json::to_value(BriefIn::synthetic()).expect("serialises");
        assert_eq!(brief["v"], json!(crate::V));
        assert_eq!(brief["entry"]["branch"], json!("agent/gatekeeper"));
        let queue = serde_json::to_value(BriefIn::queue()).expect("serialises");
        assert!(queue.get("entry").is_none(), "absent, not null: {queue}");
        let verdict = serde_json::to_value(VerdictIn::synthetic()).expect("serialises");
        assert_eq!(verdict["verdict"], json!("accept"));
    }
}
