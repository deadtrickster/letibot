//! `adjudicate_` - the permission gate's floor. A layer may make a call stricter and never looser:
//! the answer merges through the strictest tier, and a trap or a malformed answer is read as the
//! strictest tier rather than skipped, so nothing here can admit a call or lower one.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The family. Every hook of this kind is exported under it, as `pane_` and `notice_` are.
pub const PREFIX: &str = "adjudicate_";

/// The one export. A hook name carries a word after its prefix - `is_hook_kind("pane_")` is false
/// for the reason - and the word here names what the layer answers: a floor.
pub const HOOK: &str = "adjudicate_floor";

/// The action as layer A read it, which is all a floor is allowed to be about: a policy about what
/// the call IS, never a second guess at how it was spelled. Fields are strings rather than the
/// gate's own types because a guest links none of them; the words are the gate's, carried verbatim.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Action {
    /// The tool the model called.
    pub tool: String,
    /// Layer A's intents, in its own words: `network`, `disclose`, `destroy`.
    #[serde(default)]
    pub intents: Vec<String>,
    /// Layer A's tier, in the gate's own words: `auto`, `may_approve`, `always_ask`, `blocked`.
    pub tier: String,
    /// The program doing the work, when the action is a shell command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// Every path the call names, as it named them.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Where layer A placed the action: `workspace`, `home`, `root`, `secret:<store>`,
    /// `remote:<host>`. A policy about a production host is a match on one of these.
    #[serde(default)]
    pub regions: Vec<String>,
    /// The mode in force - the same word the gate's status line uses.
    pub mode: String,
    /// The project the session works in.
    pub project: String,
}

/// The input: the action, and the lower layer's answer when this is the second half of a
/// `{"below": …}` round trip.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdjudicateIn {
    /// [`V`](crate::V).
    pub v: u32,
    pub action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub below: Option<serde_json::Value>,
}

impl AdjudicateIn {
    /// One action, as the host builds it.
    pub fn new(action: Action) -> AdjudicateIn {
        AdjudicateIn {
            v: crate::V,
            action,
            below: None,
        }
    }

    /// The input the shadow gate and `plugin test` use when a session has none recorded: a
    /// production host reached from a project that named it.
    pub fn synthetic() -> AdjudicateIn {
        AdjudicateIn::new(Action {
            tool: "bash".into(),
            intents: vec!["network".into(), "execute_code".into()],
            tier: "may_approve".into(),
            program: Some("ssh".into()),
            paths: Vec::new(),
            regions: vec!["remote:prod-1".into()],
            mode: "writes-allowed".into(),
            project: "/work".into(),
        })
    }
}

/// How strict a floor is, ordered as the gate orders its own tiers so that [`Floor::strictest`] is
/// the greater of two. Ranks, not words: a floor raised from `ask` to `deny` is one step.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Floor {
    /// Nothing stricter than the gate's own reading. A floor may say this, and it does nothing.
    #[default]
    Auto,
    /// The model may approve it within what it has earned.
    MayApprove,
    /// A person decides, every time.
    Ask,
    /// Nothing promotes it: the call does not run.
    Deny,
}

impl Floor {
    /// Every rung, weakest first.
    pub const ALL: [Floor; 4] = [Floor::Auto, Floor::MayApprove, Floor::Ask, Floor::Deny];

    pub fn as_str(self) -> &'static str {
        match self {
            Floor::Auto => "auto",
            Floor::MayApprove => "may_approve",
            Floor::Ask => "ask",
            Floor::Deny => "deny",
        }
    }

    /// The floor a word names. `always_ask` and `blocked` are layer A's spellings of the same two
    /// rungs, accepted so a layer may copy back the tier it was handed instead of translating it.
    pub fn parse(word: &str) -> Option<Floor> {
        match word.trim() {
            "auto" => Some(Floor::Auto),
            "may_approve" => Some(Floor::MayApprove),
            "ask" | "always_ask" => Some(Floor::Ask),
            "deny" | "blocked" => Some(Floor::Deny),
            _ => None,
        }
    }

    /// The stricter of two floors - the only way two combine, so a merge can never relax one.
    pub fn strictest(self, other: Floor) -> Floor {
        if other > self { other } else { self }
    }
}

impl Serialize for Floor {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// An unknown floor is refused here rather than read as silence: a floor this host cannot place is
/// a policy nobody can say the gate acted on, and the gate fails closed on it.
impl<'de> Deserialize<'de> for Floor {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Floor, D::Error> {
        let word = String::deserialize(d)?;
        Floor::parse(&word).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "`{word}` is not a floor: a floor is `auto`, `may_approve`, `ask` or `deny`"
            ))
        })
    }
}

/// The answer: the names a layer gives the action, and the floor it requires. Absent fields are
/// absent on the wire, so *this layer said nothing* is an empty document and nothing else.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdjudicateOut {
    /// The layer's own names for what it found - a project's production host, its own secret
    /// store. They reach the card and the audit row and decide nothing: the floor does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intents: Vec<String>,
    /// The strictest tier this layer requires. Absent is *nothing stricter than the gate's own
    /// reading*, which is the one answer that cannot be mistaken for consent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor: Option<Floor>,
}

impl AdjudicateOut {
    /// Nothing to say: the gate's own reading stands.
    pub fn nothing() -> AdjudicateOut {
        AdjudicateOut::default()
    }

    /// These names, at this floor.
    pub fn raised(intents: Vec<String>, floor: Floor) -> AdjudicateOut {
        AdjudicateOut {
            intents,
            floor: Some(floor),
        }
    }

    /// Whether this answer asks for anything at all. An empty document is silence, and so is a
    /// floor of `auto` with no names: neither says the action is smaller than the gate read it.
    pub fn says_nothing(&self) -> bool {
        self.floor.is_none_or(|f| f == Floor::Auto) && self.intents.is_empty()
    }

    /// The floor this answer requires, or `None` for silence. A layer that names an action without
    /// a floor has labelled it and asked for nothing stricter.
    pub fn required(&self) -> Option<Floor> {
        self.floor.filter(|f| *f != Floor::Auto)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The two names cannot drift apart: the export is one of the family's, so a manifest that
    /// lists the family and a module that exports the hook are the same word.
    #[test]
    fn the_export_is_one_of_the_familys_names() {
        assert_eq!(
            HOOK.strip_prefix(PREFIX),
            Some("floor"),
            "{HOOK} is not under {PREFIX}"
        );
    }

    /// Nothing in this contract can admit a call, lower a tier or answer one. The two fields are
    /// the whole answer, and neither is a decision the gate asks anybody for: this is the
    /// type-level half of the tighten-only rule, asserted rather than promised in prose.
    #[test]
    fn there_is_no_field_by_which_a_layer_could_admit_a_call() {
        let full = AdjudicateOut::raised(vec!["prod-host".into()], Floor::Ask);
        let out = serde_json::to_value(&full).expect("serialises");
        for forbidden in [
            "admit",
            "allow",
            "allowed",
            "permit",
            "permission",
            "grant",
            "answer",
            "refuse",
            "tier",
            "mode",
            "access",
            "skip",
        ] {
            assert!(
                out.get(forbidden).is_none(),
                "`{forbidden}` is in the contract: {out}"
            );
        }
        let input = serde_json::to_value(AdjudicateIn::synthetic()).expect("serialises");
        for forbidden in ["admit", "allow", "permission", "grant"] {
            assert!(
                input.get(forbidden).is_none(),
                "`{forbidden}` is in the input: {input}"
            );
        }
    }

    /// The plan's own example, byte for byte: `{"intents":["prod-host"], "floor":"ask"}`.
    #[test]
    fn the_plans_example_answer_is_the_contract() {
        let out = AdjudicateOut::raised(vec!["prod-host".into()], Floor::Ask);
        assert_eq!(
            serde_json::to_value(&out).unwrap(),
            json!({"intents": ["prod-host"], "floor": "ask"})
        );
        assert_eq!(
            serde_json::from_value::<AdjudicateOut>(
                json!({"intents": ["prod-host"], "floor": "ask"})
            )
            .unwrap(),
            out
        );
    }

    /// A word this host cannot place is a bad answer, never silence: the gate reads the refusal as
    /// the strictest tier, and a floor read as silence would be a policy that stopped working.
    #[test]
    fn a_floor_this_host_cannot_place_is_refused_by_name() {
        let why = serde_json::from_value::<AdjudicateOut>(json!({"floor": "maybe"})).unwrap_err();
        let why = why.to_string();
        assert!(
            why.contains("may_approve"),
            "the refusal names the floors: {why}"
        );
        for rung in Floor::ALL {
            let word = json!({"floor": rung.as_str()});
            assert_eq!(
                serde_json::from_value::<AdjudicateOut>(word).unwrap().floor,
                Some(rung)
            );
        }
        // And layer A's own spellings of the two rungs it can hand a layer are accepted.
        assert_eq!(Floor::parse("always_ask"), Some(Floor::Ask));
        assert_eq!(Floor::parse("blocked"), Some(Floor::Deny));
    }

    /// Combining floors has one direction: the stricter wins, so a merge can never lower a tier
    /// and no ordering of two layers can end up weaker than either.
    #[test]
    fn a_merge_of_two_floors_is_never_looser_than_either() {
        for a in Floor::ALL {
            for b in Floor::ALL {
                let merged = a.strictest(b);
                assert!(merged >= a && merged >= b, "{a:?} + {b:?} = {merged:?}");
                assert_eq!(merged, b.strictest(a), "a merge is symmetric");
            }
        }
    }

    /// Silence has three shapes and the host must not read one of them as consent: an empty
    /// document, a floor of `auto`, and a name with no floor.
    #[test]
    fn saying_nothing_asks_nothing() {
        assert!(AdjudicateOut::nothing().says_nothing());
        assert_eq!(AdjudicateOut::nothing().required(), None);
        let auto = AdjudicateOut::raised(Vec::new(), Floor::Auto);
        assert!(auto.says_nothing());
        assert_eq!(auto.required(), None);
        let named = AdjudicateOut::raised(vec!["prod-host".into()], Floor::Auto);
        assert!(!named.says_nothing(), "a name is said, even with no floor");
        assert_eq!(named.required(), None, "and it raises nothing");
        assert_eq!(
            AdjudicateOut::raised(Vec::new(), Floor::Deny).required(),
            Some(Floor::Deny)
        );
    }
}
