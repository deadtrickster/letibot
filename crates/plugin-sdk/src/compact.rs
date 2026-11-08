//! The `compact_` hook (X15): what a compaction carries - the record's sections, a layer's own
//! instruction, the facts it pins, and whether the session goes on after it.
//!
//! Two things stay the host's: when to compact and which fold (that is `Fold::next`), and the size
//! of the record. A layer may choose what is in it. A pin past `pinned_tokens` is cut in the order
//! the answer asked for them, and the cut is said rather than silently dropped: a record that does
//! not carry `AGENTS.md` is a fact the operator reads.

/// The export-name prefix: `compact_<name>`, because a kind with no name after its prefix names no hook.
pub const PREFIX: &str = "compact_";

/// The one export every host asks: a session compacts one conversation at a time, so a stack answers
/// this once, top layer first.
pub const HOOK: &str = "compact_summary";

/// The hook a layer named `name` answers.
pub fn hook(name: &str) -> String {
    format!("compact_{name}")
}

/// What a candidate is: the four things a compaction can pin, and nothing else. A layer can pin a
/// candidate the host offered and cannot invent one - the host is what holds the text.
pub const KINDS: [&str; 4] = ["agents_md", "note", "row", "item"];

/// One thing that could be pinned, as the host scored it.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Candidate {
    /// The id a `pin` names: `agents_md`, `row:7`, `note:<path>`, `item:<index>`.
    pub id: String,
    /// One of [`KINDS`].
    pub kind: String,
    /// What carrying it verbatim would cost, in the host's tokens.
    #[serde(default)]
    pub tokens: u64,
    /// How much it has to do with the work in hand, 0..1, as the host scored it.
    #[serde(default)]
    pub score: f32,
}

/// What a record may spend: the record's own room, and the share of it that may be pinned verbatim.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Budget {
    /// The record's room, in tokens.
    pub summary_tokens: u64,
    /// What the pins may take of it, in tokens. Past this a pin is cut and the cut is said.
    pub pinned_tokens: u64,
}

/// What a compaction hands a layer.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    /// [`V`](crate::V).
    pub v: u32,
    /// The sections the harness's own template asks for, in order - what an answer choosing
    /// sections is choosing from.
    #[serde(default)]
    pub sections: Vec<String>,
    /// Everything this conversation could pin, scored by the host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<Candidate>,
    /// What the host will allow.
    #[serde(default)]
    pub budget: Budget,
}

impl Input {
    /// The input `plugin test` and the shadow gate use when a session has none recorded: a record
    /// with room, the harness's sections, and one file worth pinning.
    pub fn synthetic() -> Input {
        Input {
            v: crate::V,
            sections: vec![
                "## Objective".to_string(),
                "## Important Details".to_string(),
                "## Work State".to_string(),
                "### Completed".to_string(),
                "### Active".to_string(),
                "### Blocked".to_string(),
                "## Next Move".to_string(),
                "## Relevant Files".to_string(),
            ],
            candidates: vec![Candidate {
                id: "agents_md".to_string(),
                kind: "agents_md".to_string(),
                tokens: 900,
                score: 1.0,
            }],
            budget: Budget {
                summary_tokens: 8_000,
                pinned_tokens: 2_000,
            },
        }
    }
}

/// Whether the session goes on after the compaction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum After {
    /// The next turn begins on the record, as a wall's continuation does.
    #[default]
    Continue,
    /// The record is where the prompt stops.
    Wait,
}

/// What a layer answered.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Answer {
    /// The headings to keep, in order. Empty is every section the host asked for, which is what a
    /// layer that has no opinion answers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<String>,
    /// One sentence of the layer's own, appended to the harness's own instruction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_instruction: Option<String>,
    /// The candidate ids to carry verbatim, in the order the pin budget is spent on them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pin: Vec<String>,
    /// Whether the session goes on after this.
    #[serde(default)]
    pub after: After,
}

/// One pin, as it was fitted to the budget.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pin {
    /// The candidate's id, which is how the host finds the text it read.
    pub id: String,
    /// One of [`KINDS`].
    pub kind: String,
    /// What it costs.
    pub tokens: u64,
}

/// What the pin budget came to: what is carried, and what was cut for not fitting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pins {
    /// In the order the answer asked for them, which is the order they are carried in.
    pub kept: Vec<Pin>,
    /// The ones the budget could not hold, in the same order.
    pub cut: Vec<Pin>,
    /// What the kept pins cost.
    pub tokens: u64,
    /// What the pins were allowed, so the sentence can say what did not fit in what.
    pub budget: u64,
}

impl Pins {
    /// The sentence saying what was cut, or `None` when everything the layer pinned fits. A pin
    /// that vanished without a word is the failure this hook's budget must not have.
    pub fn cut_said(&self) -> Option<String> {
        if self.cut.is_empty() {
            return None;
        }
        let named: Vec<String> = self
            .cut
            .iter()
            .map(|p| format!("`{}` ({} tokens)", p.id, p.tokens))
            .collect();
        Some(format!(
            "the compaction pinned {} in a budget of {} token(s), and {} did not fit, so the \
             record does not carry {}",
            if self.kept.is_empty() {
                "nothing".to_string()
            } else {
                self.kept
                    .iter()
                    .map(|p| format!("`{}`", p.id))
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            self.budget,
            named.join(", "),
            if self.cut.len() == 1 { "it" } else { "them" }
        ))
    }
}

impl Answer {
    /// The answer a layer wrote, or the sentence saying why it is not one.
    pub fn parse(v: serde_json::Value) -> Result<Answer, String> {
        serde_json::from_value(v).map_err(|e| format!("not a compaction answer: {e}"))
    }

    /// A section or a pin the host did not offer is refused: a heading it cannot render is a
    /// template nobody carries, and a pinned id it did not score names text it does not have.
    pub fn validate(&self, input: &Input) -> Result<(), String> {
        if let Some(section) = self
            .sections
            .iter()
            .find(|s| !input.sections.iter().any(|has| has == *s))
        {
            return Err(format!(
                "it asked for the section `{section}`, and this compaction's sections are {}",
                input.sections.join(", ")
            ));
        }
        if let Some(id) = self
            .pin
            .iter()
            .find(|id| !input.candidates.iter().any(|c| &c.id == *id))
        {
            return Err(format!(
                "it pinned `{id}`, which is not one of the {} candidate(s) this compaction \
                 offered - a layer pins what the host scored and cannot raise what it did not",
                input.candidates.len()
            ));
        }
        Ok(())
    }

    /// The sections this answer keeps: its own list, or the host's when it named none.
    pub fn kept_sections<'a>(&'a self, input: &'a Input) -> &'a [String] {
        if self.sections.is_empty() {
            &input.sections
        } else {
            &self.sections
        }
    }

    /// Fit the pins to the budget, in the order asked for: a pin that does not fit is cut, and so
    /// is every one after it, because the order is the layer's statement about what matters.
    pub fn fit(&self, input: &Input, pinned_tokens: u64) -> Pins {
        let mut pins = Pins {
            budget: pinned_tokens,
            ..Pins::default()
        };
        let mut full = false;
        for id in &self.pin {
            let Some(candidate) = input.candidates.iter().find(|c| &c.id == id) else {
                continue;
            };
            let pin = Pin {
                id: candidate.id.clone(),
                kind: candidate.kind.clone(),
                tokens: candidate.tokens,
            };
            if !full && pins.tokens + pin.tokens <= pinned_tokens {
                pins.tokens += pin.tokens;
                pins.kept.push(pin);
            } else {
                full = true;
                pins.cut.push(pin);
            }
        }
        pins
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Three candidates, and a pin budget that holds the first and not the second.
    fn input() -> Input {
        Input {
            v: crate::V,
            sections: vec!["## Objective".into(), "## Next Move".into()],
            candidates: vec![
                Candidate {
                    id: "agents_md".into(),
                    kind: "agents_md".into(),
                    tokens: 900,
                    score: 1.0,
                },
                Candidate {
                    id: "row:7".into(),
                    kind: "row".into(),
                    tokens: 200,
                    score: 0.9,
                },
                Candidate {
                    id: "note:docs/design.md".into(),
                    kind: "note".into(),
                    tokens: 4_000,
                    score: 0.5,
                },
            ],
            budget: Budget {
                summary_tokens: 8_000,
                pinned_tokens: 1_000,
            },
        }
    }

    /// K4's rule: the pins are spent in order, the one that does not fit is cut, and the cut is
    /// said by name - and so is everything after it, because order is the layer's statement.
    #[test]
    fn a_pin_past_the_budget_is_cut_and_said() {
        let input = input();
        let answer = Answer::parse(json!({
            "pin": ["agents_md", "row:7", "note:docs/design.md"]
        }))
        .expect("an answer");
        answer.validate(&input).expect("every id was offered");
        let pins = answer.fit(&input, input.budget.pinned_tokens);
        assert_eq!(pins.budget, 1_000);
        assert_eq!(
            pins.kept.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["agents_md"],
            "900 tokens fit in 1,000, and the 200 that would also fit come after one that did not"
        );
        assert_eq!(pins.tokens, 900);
        let said = pins.cut_said().expect("a cut is said");
        assert!(said.contains("`row:7` (200 tokens)"), "{said}");
        assert!(said.contains("`note:docs/design.md`"), "{said}");
        assert!(said.contains("does not carry"), "{said}");
        // Everything that fits says nothing at all.
        let all = Answer {
            pin: vec!["agents_md".into()],
            ..Answer::default()
        };
        assert_eq!(all.fit(&input, 1_000).cut_said(), None);
    }

    /// The host's vocabulary is the whole of it: a section it does not render and an id it did not
    /// score are refused, and an answer that names none keeps the host's own list.
    #[test]
    fn an_answer_chooses_among_what_the_host_offered() {
        let input = input();
        let invented = Answer::parse(json!({"sections": ["## Feelings"]})).expect("parses");
        let why = invented.validate(&input).expect_err("a section to render");
        assert!(why.contains("## Objective"), "{why}");
        let raised =
            Answer::parse(json!({"pin": ["agents_md", "note:/etc/passwd"]})).expect("parses");
        let why = raised.validate(&input).expect_err("a scored candidate");
        assert!(why.contains("note:/etc/passwd"), "{why}");
        let quiet = Answer::parse(json!({})).expect("an empty answer");
        quiet.validate(&input).expect("nothing to refuse");
        assert_eq!(
            quiet.kept_sections(&input),
            input.sections.as_slice(),
            "no opinion is the harness's template"
        );
        assert_eq!(quiet.after, After::Continue);
    }

    /// The wire is the plan's shape, and the synthetic input is the host's.
    #[test]
    fn the_plans_example_is_this_type() {
        let answer: Answer = serde_json::from_value(json!({
            "sections": ["## Objective"],
            "extra_instruction": "name the branch",
            "pin": ["agents_md", "row:r4"],
            "after": "wait",
        }))
        .expect("the example parses");
        assert_eq!(answer.extra_instruction.as_deref(), Some("name the branch"));
        assert_eq!(answer.after, After::Wait);
        let input = serde_json::to_value(Input::synthetic()).expect("serialises");
        for field in ["v", "sections", "candidates", "budget"] {
            assert!(input.get(field).is_some(), "no `{field}`: {input}");
        }
        assert_eq!(input["v"], json!(crate::V));
        assert_eq!(input["candidates"][0]["kind"], json!("agents_md"));
        assert_eq!(hook("summary"), HOOK);
    }
}
