//! `brief_` - what a child is told and how narrowly it is seated (`docs/plugin-extension-points-wide.md`
//! X19). The hook shapes the brief a worktree child starts from, and may only NARROW what that
//! child holds: a layer can drop a class of its tools and cannot add one, name a role, or move the
//! child. Placement, depth and the perimeter are the host's, and there is no field for any of them.

/// The family. A hook is exported as `<family><name>`, as `digest_` and `link_` are.
pub const PREFIX: &str = "brief_";

/// The one export: the brief a `task_start` child works in a fresh worktree under.
pub const WORKTREE: &str = "brief_worktree";

/// The access classes a layer may drop from a child's seat, in the gate's own words. A tool is not
/// named: a name that this build has no tool for would be a silent no-op, and the class is what the
/// gate reads anyway.
pub const SEAT_WORDS: &[&str] = &["read-only", "no-write", "no-exec", "no-network"];

/// Where a child works: the tree the runner arranged, its branch and the base cut from.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Place {
    /// The worktree's path, as the runner made it.
    pub path: String,
    /// The branch the child's work lands on.
    pub branch: String,
    /// The base SHA the branch was cut from.
    pub base: String,
}

impl Place {
    pub fn new(
        path: impl Into<String>,
        branch: impl Into<String>,
        base: impl Into<String>,
    ) -> Place {
        Place {
            path: path.into(),
            branch: branch.into(),
            base: base.into(),
        }
    }
}

/// What `brief_worktree` is handed: the placement, the seat the child was asked for, and the brief
/// the host would give it - so a layer can add to what is said instead of replacing it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    /// [`V`](crate::V).
    pub v: u32,
    pub worktree: Place,
    /// The role the caller asked for, as the `task` call spelled it.
    pub role: String,
    /// The model the caller named, when it named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The brief as the host would send it, unshaped.
    pub brief: String,
}

impl Input {
    /// One child's brief, as the host builds it.
    pub fn new(worktree: Place, role: impl Into<String>, brief: impl Into<String>) -> Input {
        Input {
            v: crate::V,
            worktree,
            role: role.into(),
            model: None,
            brief: brief.into(),
        }
    }

    /// The synthetic input `plugin test` and the shadow gate use when a session has none recorded.
    pub fn synthetic() -> Input {
        Input::new(
            Place::new("/repo/worktrees/agent-k5", "agent/k5", "868e8fb2"),
            "coder",
            "[the deliverable] You work in the worktree",
        )
    }
}

/// What `brief_worktree` answers: words added to the child's brief, and classes dropped from its
/// seat. Absent is absent on the wire, so *this layer said nothing* is an empty document.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    /// Words appended to the brief the host composed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_add: Option<String>,
    /// Access classes this child may not have - [`SEAT_WORDS`]. Narrowing only: there is no field
    /// that seats a tool, and the host refuses an answer carrying a word it cannot place.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seat_drop: Vec<String>,
}

impl Answer {
    /// Nothing said: the native brief and the seat as asked.
    pub fn nothing() -> Answer {
        Answer::default()
    }

    /// These words after the brief, and nothing dropped.
    pub fn adding(brief_add: impl Into<String>) -> Answer {
        Answer {
            brief_add: Some(brief_add.into()),
            seat_drop: Vec::new(),
        }
    }

    /// Whether this answer asks for anything at all.
    pub fn says_nothing(&self) -> bool {
        self.brief_add
            .as_deref()
            .is_none_or(|s| s.trim().is_empty())
            && self.seat_drop.is_empty()
    }

    /// The words checked against [`SEAT_WORDS`]: a class this build has not got is refused with a
    /// sentence naming the four, because an ignored word is a seat nobody knows the shape of.
    pub fn checked(&self) -> Result<(), String> {
        for word in &self.seat_drop {
            if !SEAT_WORDS.contains(&word.as_str()) {
                return Err(format!(
                    "`{word}` is not a seat this host can narrow: the words are {} - and a layer can \
                     only drop a class of tools, never seat one",
                    SEAT_WORDS.join(", ")
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_export_is_under_the_family() {
        assert!(
            WORKTREE
                .strip_prefix(PREFIX)
                .is_some_and(|rest| !rest.is_empty()),
            "{WORKTREE} is not under {PREFIX}"
        );
    }

    /// The plan's own shape: words added to the brief and a tool dropped from the seat by class.
    #[test]
    fn the_plans_example_is_this_type() {
        let out: Answer =
            serde_json::from_str(r#"{"brief_add":"Commit as you go.","seat_drop":["no-exec"]}"#)
                .expect("an answer");
        assert_eq!(out.brief_add.as_deref(), Some("Commit as you go."));
        assert!(out.checked().is_ok());
        assert!(!out.says_nothing());
        assert!(Answer::nothing().says_nothing());
    }

    /// There is no field that seats a tool, and one is refused by name rather than dropped.
    #[test]
    fn a_seat_can_be_narrowed_and_never_widened() {
        let why = serde_json::from_str::<Answer>(r#"{"seat_add":["bash"]}"#).expect_err("no field");
        assert!(why.to_string().contains("seat_add"), "{why}");
        // And a word the host has not got is refused, with the four it has.
        let out: Answer = serde_json::from_str(r#"{"seat_drop":["bash"]}"#).expect("an answer");
        let why = out.checked().expect_err("a tool is not a class");
        assert!(why.contains("no-exec"), "{why}");
    }

    /// The synthetic input is the host's own shape.
    #[test]
    fn the_synthetic_input_is_the_hosts_shape() {
        let input = serde_json::to_value(Input::synthetic()).expect("serialises");
        for field in ["v", "worktree", "role", "brief"] {
            assert!(input.get(field).is_some(), "no `{field}`: {input}");
        }
        assert_eq!(input["v"], json!(crate::V));
        assert!(input.get("model").is_none(), "absent, not null: {input}");
    }
}
