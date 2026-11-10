//! `card` - a head's card for one transcript row: the row's kind, its words and its typed facts in,
//! the parts of each line and the verb it is drawn as out. It never paints cells: the width, the cut
//! and the palette are the host's, because a card is cut to a terminal the layer cannot see.

use serde_json::Value;

pub mod facts;
pub mod readers;

/// The export-name prefix a card answers under: `card_<name>`.
pub const PREFIX: &str = "card_";

/// The hook a head asks about one row of its transcript.
pub const ITEM: &str = "card_item";

/// The hook a head asks about a tool call, for the verb the row is drawn as.
pub const CALL: &str = "card_call";

/// The two names, in the order a head asks them: the row first, the verb second.
pub const HOOKS: &[&str] = &[ITEM, CALL];

/// A row that is the session's own news: what the harness said, or delivered.
pub const KIND_NOTICE: &str = "notice";

/// A row that is a tool call's result - the one kind whose card is a verb as well as a line.
pub const KIND_CALL: &str = "call";

/// The kinds a card is asked about. A row of any other kind is one no card layer is asked for.
pub const KINDS: &[&str] = &[KIND_NOTICE, KIND_CALL];

/// The verbs a card may be drawn as: rano's `Verb::of` by meaning, and no others. A word outside
/// this list is refused rather than drawn as itself - a tool's own name is already what a row says
/// when no layer answers, so a layer answering nothing new is a layer that has answered wrong.
pub const VERBS: &[&str] = &["read", "edit", "write", "search", "list", "run", "fetch"];

/// One file a call changed, as a card reads it.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Changed {
    /// The path, as the row carries it.
    pub path: String,
    /// The call made the file, rather than changing one that was there.
    #[serde(default)]
    pub created: bool,
}

/// A tool row's own facts: the call whose result the row is. Not a `notice::Facts` - a tool row is
/// not a sentence the harness composed, and the two vocabularies are not one.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Call {
    /// The tool's name as the model called it: `notes`, `bash`, `read`.
    pub name: String,
    /// How the call ended, in the runtime's own word - `ok`, `failed`, `denied`, `backgrounded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// The files the call changed, in the row's order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<Changed>,
    /// The first line of what the call returned, bounded by the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

impl Call {
    /// A call that changed one file, or none: the shape a verb is decided from.
    pub fn new(name: impl Into<String>, changed: Vec<Changed>) -> Call {
        Call {
            name: name.into(),
            outcome: None,
            changed,
            result: None,
        }
    }
}

/// The row the head is drawing. `id` is the row's, and it is what the host keys its one ask per row
/// by: the same id twice is one ask, so a frame redrawn costs nothing.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Item {
    /// The row's id, as the transcript names it (`s#t12#41`).
    pub id: String,
    /// What the row is - [`KINDS`].
    pub kind: String,
    /// The row's words, already carrying any rewording the `notice_` door applied. A layer that
    /// reads facts never parses this; one with no facts for a shape reads it here.
    pub text: String,
    /// The typed facts the host holds for this row (`notice::Facts`, as one document), when it
    /// holds any. `None` is a row the daemon composed from nothing it kept - not an empty fact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts: Option<Value>,
    /// A tool row's call, when the row is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<Call>,
}

impl Item {
    /// One row, at its plainest: an id, a kind and the words.
    pub fn new(id: impl Into<String>, kind: impl Into<String>, text: impl Into<String>) -> Item {
        Item {
            id: id.into(),
            kind: kind.into(),
            text: text.into(),
            facts: None,
            call: None,
        }
    }
}

/// What a card is asked about: the row, and the notices the host split it into. The parts are the
/// host's split - a row of several deliveries is several lines, and the splitters are text rules the
/// host keeps - so an answer is positional: block `k` is the card for part `k`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Input {
    /// [`V`](crate::V).
    pub v: u32,
    /// The row.
    pub item: Item,
    /// The row's notices, in the order the host draws them. One block per part is the answer.
    pub parts: Vec<String>,
}

impl Input {
    /// One ask.
    pub fn new(item: Item, parts: Vec<String>) -> Input {
        Input {
            v: crate::V,
            item,
            parts,
        }
    }

    /// One ask about a whole row as one part - what a head asks when it has not split the row.
    pub fn whole(item: Item) -> Input {
        let text = item.text.clone();
        Input::new(item, vec![text])
    }

    /// The input `plugin test` and the shadow gate use when a session has none recorded: one
    /// monitor firing, as the daemon delivers one.
    pub fn synthetic() -> Input {
        let mut item = Item::new(
            "s#t1#1",
            "notice",
            "[monitor] `pr-24` fired: its checks finished.",
        );
        item.facts = Some(serde_json::json!({
            "kind": "monitor",
            "fired": [{
                "name": "pr-24",
                "said": "[monitor] `pr-24` fired: its checks finished.",
                "folded": 0,
                "ending": false
            }],
        }));
        let text = item.text.clone();
        Input::new(item, vec![text])
    }
}

/// One card: the parts of its single line, which part is drawn in the register, and the popup's
/// top edge. The parts are joined with the host's own ` · `, so a layer says the facts and the
/// punctuation stays the head's.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Lines {
    /// The line's parts, in order - the first is the card's lead.
    pub parts: Vec<String>,
    /// Which part is drawn in [`Lines::role`]. Absent is the first, which is the lead.
    #[serde(default)]
    pub accent: usize,
    /// The register the accented part is drawn in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<crate::pane::Role>,
    /// The popup's top edge - a few words, not the line.
    pub title: String,
}

impl Lines {
    /// The card the host draws, or why it will not: a line with no parts, a part of nothing but
    /// space, an accent naming a part that is not there, or a title a reader would see as blank.
    pub fn checked(&self) -> Result<Lines, String> {
        if self.parts.is_empty() {
            return Err("the card has no parts, so it has no line".into());
        }
        if let Some(empty) = self.parts.iter().position(|p| p.trim().is_empty()) {
            return Err(format!(
                "part {} of the card is blank, and a blank part draws a stray ` · `",
                empty + 1
            ));
        }
        if self.accent >= self.parts.len() {
            return Err(format!(
                "the accent names part {}, and the card has {}",
                self.accent + 1,
                self.parts.len()
            ));
        }
        if self.title.trim().is_empty() {
            return Err("the card has no title, and the title is what the popup opens on".into());
        }
        Ok(self.clone())
    }
}

/// One block of a card answer. A set rather than a single shape because a head draws a row of
/// several notices and a layer may have more to say about some of them than others.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Block {
    /// `{"lines": {…}}` - the card for this part.
    Lines(Lines),
    /// A block kind this host does not draw, by the name the layer gave it. Kept, so a head can
    /// say which block it could not draw rather than pretending the layer said nothing.
    Unknown(String),
}

impl Block {
    /// One block out of the JSON a layer answered, by the pane contract's one-key spelling. A known
    /// kind whose body does not parse is an unknown of that name: an answer this host cannot read is
    /// a block it says so about, never a card drawn from half of one.
    pub fn of(v: Value) -> Block {
        let Value::Object(map) = v else {
            return Block::Unknown(String::new());
        };
        let Some((kind, body)) = map.into_iter().next() else {
            return Block::Unknown(String::new());
        };
        match kind.as_str() {
            "lines" => serde_json::from_value(body)
                .map(Block::Lines)
                .unwrap_or(Block::Unknown(kind)),
            other => Block::Unknown(other.to_string()),
        }
    }

    /// The block's name as the layer wrote it.
    pub fn kind(&self) -> &str {
        match self {
            Block::Lines(_) => "lines",
            Block::Unknown(k) if k.is_empty() => "unknown",
            Block::Unknown(k) => k,
        }
    }
}

/// The answer: one block per part the host named, and the verb when the row is a tool call.
/// Absent fields are absent on the wire, so *nothing to draw here* and *an empty card* cannot be
/// confused.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Out {
    /// The blocks, positionally: block `k` belongs to part `k`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<Value>,
    /// The verb a tool row is drawn as - [`VERBS`] - or absent to leave the host's own rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verb: Option<String>,
}

/// What a layer answered, read once: the cards, positionally, `None` where the layer drew nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cards {
    /// The card for each part, in the host's order.
    pub cards: Vec<Option<Lines>>,
    /// The verb the row is drawn as, when the layer named one.
    pub verb: Option<String>,
}

impl Out {
    /// This answer applied to the ask it answered, or why the host refuses it. A block count that
    /// is not the host's parts is refused whole - a card per part is positional, so a shift draws
    /// one notice's facts under another's - and a kind the host does not draw is `None`.
    pub fn cards(&self, parts: usize) -> Result<Cards, String> {
        if self.blocks.len() != parts {
            return Err(format!(
                "it answered {} card(s) for {parts} notice(s), and a card per part is what the \
                 host draws",
                self.blocks.len()
            ));
        }
        let mut cards = Vec::with_capacity(parts);
        for block in &self.blocks {
            match Block::of(block.clone()) {
                Block::Lines(lines) => cards.push(Some(lines.checked()?)),
                Block::Unknown(_) => cards.push(None),
            }
        }
        if let Some(verb) = &self.verb
            && !VERBS.contains(&verb.as_str())
        {
            return Err(format!(
                "`{verb}` is not a verb a row is drawn as: the words are {}",
                VERBS.join(", ")
            ));
        }
        Ok(Cards {
            cards,
            verb: self.verb.clone(),
        })
    }

    /// What this answer means, in one line, for `plugin test` and the gate's report.
    pub fn said(&self, parts: usize) -> String {
        match self.cards(parts) {
            Err(why) => format!("refused: {why}"),
            Ok(cards) => {
                let drawn = cards.cards.iter().filter(|c| c.is_some()).count();
                let what = format!("it draws {drawn} of {} card(s)", cards.cards.len());
                match &cards.verb {
                    Some(verb) => format!("{what}, and the row as `{verb}`"),
                    None => what,
                }
            }
        }
    }
}

/// The answer off the wire, or why it is not one.
pub fn read(answer: &Value) -> Result<Out, String> {
    serde_json::from_value(answer.clone()).map_err(|e| format!("not a card answer ({e})"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lines(parts: &[&str]) -> Value {
        json!({"lines": {
            "parts": parts,
            "accent": 0,
            "role": "attention",
            "title": parts.first().copied().unwrap_or("card"),
        }})
    }

    /// The input carries everything a card is drawn from, and a row with nothing to say still says
    /// which kind it is: a layer cannot be handed a shape it has to guess at.
    #[test]
    fn the_synthetic_ask_is_the_hosts_own_shape() {
        let asked = serde_json::to_value(Input::synthetic()).expect("serialises");
        assert_eq!(asked["v"], json!(crate::V));
        for field in ["item", "parts"] {
            assert!(asked.get(field).is_some(), "no `{field}`: {asked}");
        }
        for field in ["id", "kind", "text", "facts"] {
            assert!(
                asked["item"].get(field).is_some(),
                "no `item.{field}`: {asked}"
            );
        }
        assert_eq!(asked["item"]["kind"], json!("notice"));
        assert_eq!(asked["parts"].as_array().map(Vec::len), Some(1));
        // And an empty answer is the empty document, not nulls: it draws nothing and says nothing.
        assert_eq!(
            serde_json::to_value(Out::default()).expect("serialises"),
            json!({})
        );
    }

    /// One block per part, in the host's order: the third part's card is the third block.
    #[test]
    fn a_card_is_positional_and_a_block_this_host_cannot_read_is_silence() {
        let answer =
            json!({"blocks": [lines(&["todo", "a row"]), {"bar": null}, lines(&["answer"])]});
        let cards = read(&answer)
            .expect("an answer")
            .cards(3)
            .expect("three parts, three blocks");
        assert_eq!(
            cards.cards[0].as_ref().map(|l| l.parts.clone()),
            Some(vec!["todo".to_string(), "a row".to_string()])
        );
        assert_eq!(cards.cards[1], None, "an unknown block is the readers' cue");
        assert_eq!(
            cards.cards[2].as_ref().map(|l| l.accent),
            Some(0),
            "a block keeps the part it accents"
        );
    }

    /// A row of three notices answered with two cards is refused whole: positional answers that are
    /// one short draw one notice's facts under another notice's words.
    #[test]
    fn an_answer_that_is_not_one_card_per_part_is_refused_whole() {
        let answer = json!({"blocks": [lines(&["a"]), lines(&["b"])]});
        let why = read(&answer)
            .expect("an answer")
            .cards(3)
            .expect_err("two cards for three parts");
        assert!(why.contains("2 card(s) for 3"), "{why}");
    }

    /// A verb a row cannot be drawn as is refused, and the seven rano knows by meaning are not.
    #[test]
    fn a_verb_outside_the_hosts_seven_is_refused() {
        let answer = json!({"blocks": [lines(&["a"])], "verb": "frobnicate"});
        let why = read(&answer)
            .expect("an answer")
            .cards(1)
            .expect_err("not a verb");
        assert!(why.contains("frobnicate"), "{why}");
        let edit = json!({"blocks": [], "verb": "edit"});
        assert_eq!(
            read(&edit)
                .expect("an answer")
                .cards(0)
                .expect("no parts")
                .verb,
            Some("edit".to_string())
        );
    }

    /// The checks a card's own line is held to: a blank part, an accent that names no part, and a
    /// title a popup would open on nothing.
    #[test]
    fn a_malformed_card_is_refused_by_name() {
        let blank = json!({"blocks": [lines(&["todo", " "])]});
        assert!(
            read(&blank)
                .expect("an answer")
                .cards(1)
                .expect_err("a blank part")
                .contains("part 2")
        );
        let past = json!({"blocks": [{"lines": {"parts": ["a"], "accent": 3, "title": "t"}}]});
        assert!(
            read(&past)
                .expect("an answer")
                .cards(1)
                .expect_err("an accent past the end")
                .contains("accent names part 4")
        );
        let untitled = json!({"blocks": [{"lines": {"parts": ["a"], "title": ""}}]});
        assert!(
            read(&untitled)
                .expect("an answer")
                .cards(1)
                .expect_err("no title")
                .contains("no title")
        );
    }
}
