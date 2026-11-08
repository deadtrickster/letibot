//! `nag_<name>` - the plan-check's decision: which eligible row it names, in what words, and when
//! it may come due again. One subject today, [`TODO`] - the board (`docs/plugin-extension-points-wide.md`
//! X13).
//!
//! The decision is handed over; the clock and the eligibility filter stay native. A layer may
//! choose, word or stay quiet, and it may never name a row outside [`Input::eligible`], never go
//! quiet past the repeat cap while a row is open, and never move a queue row's nag. `next_ms` is
//! clamped to [`Clock::floor_ms`]..[`Clock::cap_ms`] by the clock, not here.

use serde_json::Value;

/// The export-name prefix a nag answers under: `nag_<name>`.
pub const PREFIX: &str = "nag_";

/// The one subject the host asks about today: the operator's and the model's own board.
pub const TODO: &str = "todo";

/// The export name for one nag subject.
pub fn hook(name: &str) -> String {
    format!("{PREFIX}{name}")
}

/// The hook the host asks.
pub const HOOK: &str = "nag_todo";

/// One row of the board, as a layer reads it. `id` is the row's own words - the board has no other
/// name for a row, and it is what [`Input::eligible`] and the answer both spell.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Row {
    /// The row's words, exactly as the board holds them.
    pub id: String,
    /// The same words, for a layer that reads `content` rather than `id` - kept apart so an answer
    /// names the row the same way the input does.
    pub content: String,
    /// `pending`, `in_progress`, or the row's own status word.
    pub status: String,
    /// Whose row it is: `operator`, `model`, `queue`, or a parent's author string.
    pub by: String,
    /// What the row waits on, one `kind:target` each - a layer may read them and decides nothing
    /// with them: the eligibility filter has already answered whether the row is the check's to
    /// speak about.
    #[serde(default)]
    pub waits: Vec<String>,
    /// Whether a merge queue's review is in flight on the row.
    #[serde(default)]
    pub under_review: bool,
}

/// The clock's numbers, the bounds the answer is held to, and the two idle readings a layer can
/// pace itself by. A layer that decides *not yet* says so with `next_ms`; these are what it has to
/// reason with, so no layer has to keep a clock of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Clock {
    /// How long since the check was last DELIVERED, or absent before the first one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_last_ms: Option<u64>,
    /// How many times in a row this plan has been checked - the ladder's exponent.
    pub repeats: u32,
    /// How long this session has been idle: no turn, no operator, no event.
    pub idle_ms: u64,
    /// How long since the operator last ADDED a row to the board.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_added_ms: Option<u64>,
    /// The shortest interval a check may ask for - the idle window.
    pub floor_ms: u64,
    /// The longest interval a check may ask for - the repeat cap.
    pub cap_ms: u64,
}

impl Clock {
    /// Whether the session has been quiet long enough that staying quiet is no longer the layer's
    /// to decide: the cap is how long a stalled plan may go unmentioned.
    pub fn past_the_cap(&self) -> bool {
        matches!(self.since_last_ms, Some(since) if since >= self.cap_ms)
    }
}

/// What a layer is handed: the board, the rows the check may speak about, the clock, and what the
/// harness's own nag says - so a layer may keep it, reword it, or choose another eligible row.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    /// [`V`](crate::V).
    pub v: u32,
    /// The whole board, so a layer may see what the eligibility filter left out and why.
    pub board: Vec<Row>,
    /// The rows the check may speak about - the only rows an answer may name.
    pub eligible: Vec<String>,
    /// The clock as of this ask.
    pub clock: Clock,
    /// The background jobs this session is running, by handle.
    #[serde(default)]
    pub jobs: Vec<String>,
    /// The children it is running, by id.
    #[serde(default)]
    pub children: Vec<String>,
    /// The wall clock, in milliseconds.
    pub now_ms: u64,
    /// The harness's own sentence, which is what the model reads if this hook answers nothing.
    pub text: String,
    /// The row the harness's own sentence names, if it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Input {
    /// One ask, as the harness builds it.
    pub fn new(
        board: Vec<Row>,
        eligible: Vec<String>,
        clock: Clock,
        now_ms: u64,
        text: &str,
    ) -> Input {
        Input {
            v: crate::V,
            board,
            eligible,
            clock,
            jobs: Vec::new(),
            children: Vec::new(),
            now_ms,
            text: text.to_string(),
            name: None,
        }
    }

    /// Whether a name is one the answer may use.
    pub fn names_a_row(&self, name: &str) -> bool {
        self.eligible.iter().any(|row| row == name)
    }

    /// The eligible rows that are the merge queue's own: a nag may never be quiet about one of
    /// them - the queue's rows are the queue's, and its silence would be a stuck review nobody
    /// mentions.
    pub fn a_queue_row_is_open(&self) -> bool {
        self.board
            .iter()
            .any(|row| row.by == "queue" && self.names_a_row(&row.id))
    }

    /// The input `plugin test` and the shadow gate use when a session has none recorded: one
    /// operator row, ready, and the sentence the harness would say about it.
    pub fn synthetic() -> Input {
        let mut input = Input::new(
            vec![Row {
                id: "land the release".into(),
                content: "land the release".into(),
                status: "pending".into(),
                by: "operator".into(),
                waits: Vec::new(),
                under_review: false,
            }],
            vec!["land the release".into()],
            Clock {
                since_last_ms: Some(15 * 60_000),
                repeats: 1,
                idle_ms: 15 * 60_000,
                operator_added_ms: None,
                floor_ms: 60_000,
                cap_ms: 30 * 60_000,
            },
            15 * 60_000,
            "[todo check] Not done:\n  - land the release — the operator's\n",
        );
        input.name = Some("land the release".into());
        input
    }
}

/// The answer: the rows this layer names, its words for them, the interval it wants, or quiet.
/// Every field is absent when the layer says nothing about it, so *unchanged* and *empty* cannot be
/// confused.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Out {
    /// The rows to name - all of them in [`Input::eligible`], in the layer's own order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Vec<String>>,
    /// The sentence the model reads instead of the harness's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Do not run a check now. Honoured up to the repeat cap, and never while a queue row is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet: Option<bool>,
    /// How long until this layer wants the next check; the clock clamps it to its own bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_ms: Option<u64>,
}

/// What the host does with an answer, once the rules above have been applied.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// The rows the check names - all members of `eligible`.
    pub name: Vec<String>,
    /// The sentence to say.
    pub text: String,
    /// Whether a check runs at all. `false` is quiet, which is only ever a layer's answer: the
    /// native nag always says.
    pub says: bool,
    /// The interval the layer asked for, in milliseconds, unclamped - the clock owns the bounds.
    pub next_ms: Option<u64>,
}

impl Decision {
    /// The row the check named first, for the follow-up that asks whether the turn moved it.
    pub fn first(&self) -> Option<String> {
        self.name.first().cloned()
    }
}

impl Out {
    /// This answer applied to the input it was given, or why the host refuses it.
    ///
    /// Three rules and no others: a name outside `eligible` refuses the whole answer (half of an
    /// answer is a sentence nobody wrote); quiet is overridden at the repeat cap and while a queue
    /// row is open; and a layer that says nothing about the text leaves the harness's own.
    pub fn decide(&self, input: &Input) -> Result<Decision, String> {
        let mut name: Vec<String> = self.name.clone().unwrap_or_default();
        if let Some(unknown) = name.iter().find(|n| !input.names_a_row(n)) {
            return Err(format!(
                "it named `{unknown}`, which the check may not speak about: the rows it may name \
                 are the `eligible` ones ({}), and nothing outside them",
                input.eligible.join(", ")
            ));
        }
        if self.quiet == Some(true) {
            // Overridden, not refused: the layer's quiet does not stand and the check is said in
            // the harness's own words, because this is the rule the cap exists for.
            if input.a_queue_row_is_open() || input.clock.past_the_cap() {
                return Ok(Decision {
                    name: input.name.iter().cloned().collect(),
                    text: input.text.clone(),
                    says: true,
                    next_ms: self.next_ms,
                });
            }
            return Ok(Decision {
                name: Vec::new(),
                text: String::new(),
                says: false,
                next_ms: self.next_ms,
            });
        }
        if name.is_empty() {
            name = input.name.iter().cloned().collect();
        }
        Ok(Decision {
            name,
            text: self.text.clone().unwrap_or_else(|| input.text.clone()),
            says: true,
            next_ms: self.next_ms,
        })
    }

    /// What this answer means, in one line, for `plugin test` and the gate's report.
    pub fn said(&self, input: &Input) -> String {
        match self.decide(input) {
            Err(why) => format!("refused: {why}"),
            Ok(decision) if !decision.says => "it stays quiet about the plan".to_string(),
            Ok(decision) => {
                let rows = if decision.name.is_empty() {
                    "no row".to_string()
                } else {
                    format!("`{}`", decision.name.join("`, `"))
                };
                match decision.next_ms {
                    Some(ms) => format!("it names {rows} and asks for the next in {ms}ms"),
                    None => format!("it names {rows}"),
                }
            }
        }
    }
}

/// The answer off the wire, or why it is not one.
pub fn read(answer: &Value) -> Result<Out, String> {
    serde_json::from_value(answer.clone()).map_err(|e| format!("not a nag answer ({e})"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn two_rows() -> Input {
        Input::new(
            vec![
                Row {
                    id: "r3".into(),
                    content: "r3".into(),
                    status: "pending".into(),
                    by: "operator".into(),
                    waits: Vec::new(),
                    under_review: false,
                },
                Row {
                    id: "r7".into(),
                    content: "r7".into(),
                    status: "in_progress".into(),
                    by: "model".into(),
                    waits: vec!["job:j7".into()],
                    under_review: false,
                },
            ],
            vec!["r3".into(), "r7".into()],
            Clock {
                since_last_ms: Some(60_000),
                repeats: 0,
                idle_ms: 60_000,
                operator_added_ms: None,
                floor_ms: 60_000,
                cap_ms: 30 * 60_000,
            },
            60_000,
            "[todo check] Not done: r3",
        )
    }

    /// An answer that says nothing is the harness's own sentence, naming the row it named.
    #[test]
    fn an_empty_answer_leaves_the_native_nag_standing() {
        let input = two_rows();
        let decision = Out::default().decide(&input).expect("unchanged");
        assert_eq!(decision.text, input.text);
        assert!(decision.says);
        assert_eq!(decision.name, Vec::<String>::new());
    }

    /// One row, its own words, and the interval it asked for - with the clock's bounds untouched.
    #[test]
    fn a_layer_may_choose_a_row_and_word_it() {
        let input = two_rows();
        let out = Out {
            name: Some(vec!["r7".into()]),
            text: Some("r7 is the one that matters".into()),
            quiet: None,
            next_ms: Some(240_000),
        };
        let decision = out.decide(&input).expect("a choice");
        assert_eq!(decision.first().as_deref(), Some("r7"));
        assert_eq!(decision.text, "r7 is the one that matters");
        assert_eq!(decision.next_ms, Some(240_000));
    }

    /// The one thing a name may never be: a row the check may not speak about.
    #[test]
    fn a_name_outside_the_eligible_set_is_refused() {
        let mut input = two_rows();
        input.eligible = vec!["r3".into()];
        let out = Out {
            name: Some(vec!["r3".into(), "r7".into()]),
            ..Out::default()
        };
        let why = out.decide(&input).expect_err("half an answer is no answer");
        assert!(why.contains("r7"), "{why}");
    }

    /// Quiet is the layer's up to the cap, and at the cap the check is said in the harness's own
    /// words.
    #[test]
    fn quiet_holds_below_the_cap_and_not_at_it() {
        let input = two_rows();
        let quiet = Out {
            quiet: Some(true),
            ..Out::default()
        };
        assert!(!quiet.decide(&input).expect("quiet").says);
        let mut past = input.clone();
        past.clock.since_last_ms = Some(past.clock.cap_ms);
        let decision = quiet.decide(&past).expect("overridden");
        assert!(decision.says);
        assert_eq!(decision.text, past.text, "the harness's own sentence");
    }

    /// And quiet is never the layer's while an eligible row is the merge queue's.
    #[test]
    fn a_queue_rows_nag_is_never_held_back() {
        let mut input = two_rows();
        input.board[0].by = "queue".into();
        let quiet = Out {
            quiet: Some(true),
            ..Out::default()
        };
        assert!(quiet.decide(&input).expect("overridden").says);
    }

    /// The wire carries the host's shape: the v, the eligibility, the clock, and the native
    /// sentence a layer may keep.
    #[test]
    fn the_synthetic_input_is_the_hosts_own_shape() {
        let input = serde_json::to_value(Input::synthetic()).expect("serialises");
        for field in ["v", "board", "eligible", "clock", "now_ms", "text", "name"] {
            assert!(input.get(field).is_some(), "no `{field}`: {input}");
        }
        assert_eq!(input["v"], json!(crate::V));
        assert_eq!(input["clock"]["cap_ms"], json!(30 * 60_000));
        // And an empty answer round-trips as the empty document, not as nulls.
        let out = serde_json::to_value(Out::default()).expect("serialises");
        assert_eq!(out, json!({}));
        assert!(
            read(&out)
                .expect("an empty answer")
                .decide(&Input::synthetic())
                .is_ok()
        );
    }
}
