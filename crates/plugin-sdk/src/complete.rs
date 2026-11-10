//! The `complete_<name>` hook: what a pane's own input asks for, and the row of candidates it gets
//! back. Text never reaches the layer per keystroke: the host owns the editor and asks on Tab and on
//! `@` or `/` at a word start, debounced, so a layer sees a line, a caret and its feeds, never a key.

/// The export name a completion answers under: `complete_<name>`.
pub const PREFIX: &str = "complete_";

/// The export name a completion answers under: `complete_<name>`.
pub fn hook(name: &str) -> String {
    format!("complete_{name}")
}

/// How many candidates the host will draw: one row, and a list longer than the row is a list whose
/// tail nobody reads. Refused rather than truncated, because the layer chose the order.
pub const MAX_CANDIDATES: usize = 8;

/// What a completion is asked with.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    /// The contract version this host speaks - [`crate::V`].
    pub v: u32,
    /// The line as the person has typed it, whole.
    pub text: String,
    /// Where the caret is, as a byte offset into `text` - always on a character boundary.
    pub cursor: usize,
    /// The pane's input key (`View.input.key`), so a layer completes per channel rather than per
    /// session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The feeds this layer declared, and only those, by name (`pane::Feed::name`).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub feeds: std::collections::BTreeMap<String, serde_json::Value>,
    /// The host's clock, unix millis - absent from a host that predates it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now_ms: Option<u64>,
    /// The person's local offset from UTC, in seconds east of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utc_offset_s: Option<i32>,
}

/// What a completion answers: the span of the line the chosen candidate replaces, and the candidates
/// themselves, best first.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Completion {
    /// The first byte of `Input::text` the accepted candidate replaces.
    #[serde(default)]
    pub from: usize,
    /// One past the last byte. `from == to` is an insertion at the caret.
    #[serde(default)]
    pub to: usize,
    /// At most [`MAX_CANDIDATES`], in the order the row draws them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<Candidate>,
}

/// One candidate: what gets inserted, and how the row shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Candidate {
    /// What replaces `from..to` when this one is accepted.
    pub insert: String,
    /// What the row draws instead of `insert` - a session's title beside its id, say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The faint word after it: why this one, or where it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The hue the row draws it in (`pane::Role::Hue`), so one speaker keeps one colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hue: Option<u8>,
}

impl Completion {
    /// Refuse an answer the host cannot draw or apply, with the sentence the layer reads back. The
    /// span is checked against the line, because a span outside it would edit text nobody typed.
    pub fn validate(&self, text: &str) -> Result<(), String> {
        if self.from > self.to {
            return Err(format!(
                "the span is {}..{}: `from` is past `to`",
                self.from, self.to
            ));
        }
        if self.to > text.len() {
            return Err(format!(
                "the span is {}..{} and the line is {} bytes",
                self.from,
                self.to,
                text.len()
            ));
        }
        if !text.is_char_boundary(self.from) || !text.is_char_boundary(self.to) {
            return Err(format!(
                "the span {}..{} splits a character: a span is byte offsets on boundaries",
                self.from, self.to
            ));
        }
        if self.candidates.len() > MAX_CANDIDATES {
            return Err(format!(
                "{} candidates: the row draws at most {MAX_CANDIDATES}",
                self.candidates.len()
            ));
        }
        for (i, c) in self.candidates.iter().enumerate() {
            if c.insert.is_empty() {
                return Err(format!("candidate {i} inserts nothing"));
            }
            if c.hue.is_some_and(|h| h >= crate::pane::HUES) {
                return Err(format!(
                    "candidate {i} asks for hue {}: the palette is `hue:0` to `hue:{}`",
                    c.hue.unwrap_or_default(),
                    crate::pane::HUES - 1
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_input_carries_the_line_the_caret_and_the_clock() {
        let input = Input {
            v: crate::V,
            text: "@fix".into(),
            cursor: 4,
            key: Some("project/letibot".into()),
            now_ms: Some(1_762_000_000_000),
            utc_offset_s: Some(3600),
            ..Input::default()
        };
        let wire = serde_json::to_value(&input).expect("serialises");
        assert_eq!(wire["text"], serde_json::json!("@fix"));
        assert_eq!(wire["cursor"], serde_json::json!(4));
        assert_eq!(wire["key"], serde_json::json!("project/letibot"));
        assert_eq!(wire["now_ms"], serde_json::json!(1_762_000_000_000u64));
        assert_eq!(serde_json::from_value::<Input>(wire).unwrap(), input);
    }

    #[test]
    fn an_answer_replaces_a_span_and_names_its_candidates() {
        let json = serde_json::json!({
            "from": 1, "to": 4,
            "candidates": [
                {"insert": "@fix-room-push", "label": "fix-room-push", "note": "project/letibot", "hue": 3},
                {"insert": "@fix-room-push-cron"}
            ]
        });
        let answer: Completion = serde_json::from_value(json.clone()).expect("an answer parses");
        assert_eq!(answer.from, 1);
        assert_eq!(answer.candidates[0].hue, Some(3));
        assert_eq!(
            serde_json::to_value(&answer).unwrap(),
            json,
            "written back as it came"
        );
        answer.validate("@fix").expect("the span is the line's");
        // An empty answer is a hook that had nothing to offer, not a refusal.
        Completion::default().validate("").expect("nothing to say");
    }

    #[test]
    fn an_answer_the_host_cannot_apply_is_refused_with_the_reason() {
        let candidate = Candidate {
            insert: "x".into(),
            ..Candidate::default()
        };
        let one = |from: usize, to: usize| Completion {
            from,
            to,
            candidates: vec![candidate.clone()],
        };
        assert!(one(4, 1).validate("@fix").unwrap_err().contains("past"));
        assert!(one(0, 9).validate("@fix").unwrap_err().contains("4 bytes"));

        let long = Completion {
            from: 0,
            to: 0,
            candidates: vec![candidate.clone(); MAX_CANDIDATES + 1],
        };
        let why = long.validate("@fix").expect_err("a row is 8 wide");
        assert!(why.contains("at most 8"), "{why}");

        let empty = Completion {
            from: 0,
            to: 0,
            candidates: vec![Candidate::default()],
        };
        assert!(empty.validate("").unwrap_err().contains("nothing"));

        let hued = Completion {
            from: 0,
            to: 0,
            candidates: vec![Candidate {
                insert: "x".into(),
                hue: Some(crate::pane::HUES),
                ..Candidate::default()
            }],
        };
        let why = hued.validate("").expect_err("hue:8 is outside the palette");
        assert!(why.contains("hue:0"), "{why}");

        // A span that splits a two-byte character would edit bytes the person never typed.
        let split = Completion {
            from: 1,
            to: 1,
            candidates: vec![],
        };
        let why = split.validate("é").expect_err("`é` is two bytes");
        assert!(why.contains("splits a character"), "{why}");
    }
}
