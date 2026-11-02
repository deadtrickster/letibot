//! A skill that offers itself, the way a note does: the name composed at the TAIL of the request -
//! never the prefix (a prefix change re-prefills the conversation) and never a row (a row cannot be
//! un-said). Earned by a score floor, at most two, silent when unsure, recomposed each round.

use std::collections::HashSet;
use std::path::Path;

use letibot_tokencore::store::TodoItem;
use letibot_tools::builtins::skill::SkillRegistry;
use letibot_transcript::{Speaker, ToolCall, TranscriptItem, UserPart};

/// The score a skill has to reach before it is offered - this offer's own floor, apart from
/// `skill find`'s. MEASURED over one project's skills plus this build's two: a subject about a
/// skill scored 0.33 to 0.63, and the highest any subject got from a word it merely shared with a
/// frontmatter was 0.14. The floor sits between them, because the first time a reader learns to
/// ignore the line the feature is dead.
pub const OFFER_FLOOR: f64 = 0.18;

/// How many skills one round may name. Two is the cap the operator asked for: a wall of names is a
/// list the model learns to skip, and the offer's whole value is that it is rare.
pub const OFFERS_MAX: usize = 2;

/// How many characters of the subject are scored, taken from the most recent end. A cap and not a
/// budget: the subject is *what is happening now*, and the 200 KB a tool last printed is not.
pub const SUBJECT_CHARS: usize = 8_000;

/// The file the take-up log is appended to, in the session's logs directory beside the notes'.
pub const LOG: &str = "skills-offers.jsonl";

/// One skill, offered, with the score that earned it.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub name: String,
    pub score: f64,
}

impl Offer {
    /// The line the model is given, as a transcript item that is not in the log: the request
    /// assembly renders it after everything the session holds, and the next round composes a fresh
    /// one. It names the skill and the verb that loads it, and it stops there.
    pub fn tail(&self) -> TranscriptItem {
        TranscriptItem::User {
            speaker: Speaker::Agent,
            parts: vec![UserPart::Text {
                text: format!(
                    "[skill offer - the harness's own line, not the operator's] `{}` looks \
                     relevant to what you are doing now (similarity {:.2} of 1.00, lexical over \
                     its frontmatter: description, when-to-use and keywords). Its body is not in \
                     this prompt. If it is worth the context, load it - `skill` action=\"load\", \
                     name=\"{}\". This line is composed for this round and is gone from the next \
                     one; the skill, once loaded, is not.",
                    self.name, self.score, self.name
                ),
            }],
        }
    }
}

/// The offers this turn has already made, and the ones the round in flight is carrying. Two facts
/// are kept apart on purpose: [`Offers::offered`] is the session's memory (a skill is offered once),
/// and [`Offers::open`] is the round's.
#[derive(Debug, Default)]
pub struct Offers {
    offered: HashSet<String>,
    open: Vec<Offer>,
    last: Vec<Offer>,
}

impl Offers {
    pub fn new() -> Offers {
        Offers::default()
    }

    /// Seed the memory with what this SESSION has already been offered: the log is the memory, so the
    /// fact survives a restart and there is no second copy of it.
    pub fn recall(&mut self, logs: &Path, session: &str) {
        let Ok(text) = std::fs::read_to_string(logs.join(LOG)) else {
            return;
        };
        for line in text.lines() {
            let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if row.get("session").and_then(|s| s.as_str()) != Some(session) {
                continue;
            }
            if let Some(name) = row.get("skill").and_then(|p| p.as_str()) {
                self.offered.insert(name.to_string());
            }
        }
    }

    /// Skip the skills whose body this session already holds, read off the transcript: a `skill` load
    /// the model made itself put the text in the conversation, and offering it again spends a line to
    /// say nothing.
    pub fn skip_loaded(&mut self, items: &[TranscriptItem]) {
        for call in items.iter().filter_map(tool_calls_of).flatten() {
            if let Some(name) = loaded_of(call) {
                self.offered.insert(name);
            }
        }
    }

    /// Skip the skills this session was STARTED with, whose bodies are in its prompt already - the
    /// declared list a `task {skills}` child carries, which no row records.
    pub fn skip_names<'a>(&mut self, names: impl IntoIterator<Item = &'a String>) {
        for name in names {
            self.offered.insert(name.clone());
        }
    }

    /// Compose this round's offers, or decide to say nothing - `false` is the common case. A skill
    /// already offered in this session is skipped: the second time is a nag, and the first time is
    /// what the model was told to act on.
    pub fn compose(&mut self, registry: &SkillRegistry, text: &str) -> bool {
        self.open.clear();
        if text.trim().is_empty() {
            return false;
        }
        self.open = registry
            .ranked(text, OFFERS_MAX * 4)
            .into_iter()
            .filter(|(name, score)| *score >= OFFER_FLOOR && !self.offered.contains(name))
            .take(OFFERS_MAX)
            .map(|(name, score)| Offer { name, score })
            .collect();
        !self.open.is_empty()
    }

    /// The trailing items this round's request carries - empty when the round decided to say
    /// nothing, which is the same code path as *the model ignored the offer*.
    pub fn tail(&self) -> Vec<TranscriptItem> {
        self.open.iter().map(|o| o.tail()).collect()
    }

    /// Settle the round's offers against what the round actually did, and log both sides. A take-up
    /// is a `skill` load of the offered name, because nothing else puts the body in the conversation.
    /// Logged either way, because the ratio is what tunes [`OFFER_FLOOR`].
    pub fn settle(&mut self, logs: &Path, session: &str, calls: &[ToolCall]) -> Vec<Offer> {
        let open = std::mem::take(&mut self.open);
        if open.is_empty() {
            return Vec::new();
        }
        let mut lines = String::new();
        for offer in &open {
            let taken = calls
                .iter()
                .any(|c| loaded_of(c).as_deref() == Some(&offer.name));
            self.offered.insert(offer.name.clone());
            lines.push_str(&format!(
                "{{\"at\":{},\"session\":{},\"skill\":{},\"score\":{:.3},\"taken\":{taken}}}\n",
                now_secs(),
                json(session),
                json(&offer.name),
                offer.score
            ));
        }
        let log = logs.join(LOG);
        if let Some(dir) = log.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
        {
            let _ = f.write_all(lines.as_bytes());
        }
        self.last = open;
        self.last.clone()
    }

    /// The offers the round just settled, for a caller that reports on them.
    pub fn last(&self) -> &[Offer] {
        &self.last
    }
}

/// The subject a skill is scored against: the person's latest prompt, the plan, and the model's
/// latest text and reasoning - walked backwards from the newest end so [`SUBJECT_CHARS`] keeps what
/// is happening now. A note's offer stops at the prompt; a skill may be made relevant by the model's
/// own reasoning, so the walk takes that too.
pub fn subject(items: &[TranscriptItem], plan: &[TodoItem]) -> String {
    let mut out = String::new();
    let mut thought = false;
    let mut said = false;
    for item in items.iter().rev() {
        let text: &str = match item {
            // The model's own latest word, and its reasoning above it: each taken once, because
            // an older paragraph is what the work has moved past.
            TranscriptItem::Assistant { text: t, .. } if !said => {
                said = true;
                t
            }
            TranscriptItem::Reasoning { text: t, .. } if !thought => {
                thought = true;
                t
            }
            TranscriptItem::ToolResult { payload, .. } => payload,
            TranscriptItem::User { parts, speaker } => {
                if *speaker == Speaker::Agent {
                    continue;
                }
                // The person's own row ends the walk, and it is taken too: the prompt is the half
                // an offer should fire on when nothing else has been said yet.
                for p in parts {
                    if let UserPart::Text { text } = p {
                        out.push_str(text);
                        out.push('\n');
                    }
                }
                break;
            }
            _ => continue,
        };
        out.push_str(text);
        out.push('\n');
        if out.len() >= SUBJECT_CHARS {
            break;
        }
    }
    // The plan, as the board holds it: a row the model wrote is the subject it is working on even
    // when the prompt that started it said something else.
    for row in plan {
        out.push_str(&row.content);
        out.push('\n');
    }
    // The cut lands on a character boundary, never through one - `truncate` panics otherwise and
    // this string is the operator's words plus tool results.
    let cut = SUBJECT_CHARS.min(out.len());
    let cut = (0..=cut)
        .rev()
        .find(|&i| out.is_char_boundary(i))
        .unwrap_or(0);
    out.truncate(cut);
    out
}

/// The calls an assistant row made, for the two walks that read them.
fn tool_calls_of(item: &TranscriptItem) -> Option<&[ToolCall]> {
    match item {
        TranscriptItem::Assistant { tool_calls, .. } => Some(tool_calls),
        _ => None,
    }
}

/// The skill a tool call would put into the conversation: a `skill` load's `name`. Deliberately
/// narrow, and for the notes' reason - a `find` that named it was a search, not a use.
pub fn loaded_of(call: &ToolCall) -> Option<String> {
    if call.name != "skill" {
        return None;
    }
    let args: serde_json::Value = serde_json::from_str(&call.arguments).ok()?;
    match args.get("action").and_then(|v| v.as_str()) {
        Some("load") => args.get("name").and_then(|v| v.as_str()).map(String::from),
        _ => None,
    }
}

/// Seconds since the unix epoch. No date library for one field of one log line.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A JSON string, escaped by `serde_json` rather than by hand: a skill's name is usually a plain
/// word, and "usually" is how a log line becomes unparseable.
fn json(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// The census, in its own file, beside the notes offer's: these tests read the module's own rules.
#[cfg(test)]
mod tests;
