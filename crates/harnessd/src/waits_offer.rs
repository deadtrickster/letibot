//! A brief that is already somebody's todo, said at the TAIL of the request - never the prefix (a
//! prefix change re-prefills the conversation) and never a row (a row cannot be un-said). Earned by
//! a score floor, once per row, silent when nothing clears it.
//!
//! What is scored is the brief in hand: the person's latest prompt and the model's own words since,
//! which is where a `task` call's prompt is written. What it is scored against is the open rows of
//! this session, its parent and its children - one tree's work, so a child starting what its
//! parent's plan already holds is the case this exists for.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use letibot_embeddings::Tier;
use letibot_tokencore::store::{TodoItem, TodoStatus};
use letibot_tools::similarity::Corpus;
use letibot_transcript::{Speaker, TranscriptItem, UserPart};

/// The score a row has to reach before it is offered at all - this caller's own floor, apart from
/// the notes' and the skills'. MEASURED over the fixture's corpus: a brief carrying two of a row's
/// four terms scored 0.387, one term 0.327, and an unrelated brief 0.047 to 0.129, so the floor
/// sits above one shared term because a hint worth stopping for names more than a word.
pub const OFFER_FLOOR: f64 = 0.35;

/// How many chars of a brief are scored, from the newest end: a task prompt is the brief, and the
/// 200 KB a tool result printed before it is not.
pub const DRAFT_CHARS: usize = 8_000;

/// The file the take-up log is appended to, in the session's logs directory beside the others.
pub const LOG: &str = "waits-offers.jsonl";

/// One open row, with whose it is: `this session`, or the label of the relative that owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRow {
    pub owner: String,
    pub content: String,
}

/// Is this row still work somebody is doing? A strike-off and a postponement are the operator's
/// answers, and a finished row is a dispatch that already happened.
pub fn open(item: &TodoItem) -> bool {
    !matches!(
        item.status,
        TodoStatus::Completed | TodoStatus::Cancelled | TodoStatus::Postponed
    )
}

/// One row, offered, with the score that earned it.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub owner: String,
    pub content: String,
    pub score: f64,
}

impl Offer {
    /// What a session is keyed by, so the same row is offered once however it is spelled out.
    fn key(&self) -> String {
        format!("{}\u{1f}{}", self.owner, self.content)
    }

    /// The line the model is given, as a transcript item that is not in the log: the request
    /// assembly renders it after everything the session holds, and the next round composes a fresh
    /// one. It names the row, whose it is and the edge that would join them, and it stops there.
    pub fn tail(&self) -> TranscriptItem {
        TranscriptItem::User {
            speaker: Speaker::Agent,
            parts: vec![UserPart::Text {
                text: format!(
                    "[todo offer - the harness's own line, not the operator's] this brief looks \
                     like the open todo `{}`, owned by {} (similarity {:.2} of 1.00). If it is the \
                     same work, `todo_write` a row with a `needs` entry naming it, or wait on it, \
                     instead of starting it a second time. This line is composed for this round \
                     and is gone from the next one.",
                    self.content, self.owner, self.score
                ),
            }],
        }
    }
}

/// The offers this turn has already made, and the one the round in flight is carrying.
#[derive(Debug, Default)]
pub struct Offers {
    offered: HashSet<String>,
    open: Option<Offer>,
    last: Option<Offer>,
    /// The box's second tier, when it has one: this caller's rows and nothing of another's. `None`
    /// is no score at all, which is the default and a box that wrote no `[embeddings]`.
    tier: Option<Tier>,
}

impl Offers {
    pub fn new() -> Offers {
        Offers::default()
    }

    /// Score with the box's second tier. Off is `None`, and then this caller says nothing at all:
    /// what it has to offer is *this is somebody's work*, which only a meaning can tell it.
    pub fn with_tier(mut self, tier: Option<Tier>) -> Offers {
        self.tier = tier;
        self
    }

    /// Seed the memory with what this SESSION has already been offered: the log is the memory, so
    /// the fact survives a restart and there is no second copy of it.
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
            let (Some(owner), Some(content)) = (
                row.get("owner").and_then(|v| v.as_str()),
                row.get("row").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            self.offered
                .insert(format!("{owner}\u{1f}{content}").to_string());
        }
    }

    /// Compose this round's offer, or decide to say nothing - `false` is the common case.
    ///
    /// A tier is what this caller exists for: mode `off` scores nothing and therefore offers
    /// nothing, so a box that never wrote `[embeddings]` sees exactly the request it saw before.
    /// The best row over the floor is the one offered; a row already offered this session is
    /// skipped, and no second-best is put in its place.
    pub fn compose(&mut self, rows: &[OpenRow], brief: &str) -> bool {
        self.open = None;
        if brief.trim().is_empty() || rows.is_empty() {
            return false;
        }
        let Some(tier) = self.tier.as_ref() else {
            return false;
        };
        let docs = corpus(rows);
        let corpus = Corpus::of_texts(&docs);
        // A fill that cannot answer empties the tier, so every cosine is `None` and what is left
        // is the words: the row it names is still the row the floor had always chosen.
        let _ = corpus.fill(Some(tier));
        let Some((path, score)) = corpus.best_with(brief, Some(tier)) else {
            return false;
        };
        let Some(i) = docs.iter().position(|(p, _)| *p == path) else {
            return false;
        };
        let Some(row) = rows.get(i) else {
            return false;
        };
        let offer = Offer {
            owner: row.owner.clone(),
            content: row.content.clone(),
            score,
        };
        if score < OFFER_FLOOR || self.offered.contains(&offer.key()) {
            return false;
        }
        self.open = Some(offer);
        true
    }

    /// The trailing item this round's request carries - empty when the round decided to say
    /// nothing, which is the same code path as *the model ignored the offer*.
    pub fn tail(&self) -> Vec<TranscriptItem> {
        self.open
            .as_ref()
            .map(|o| vec![o.tail()])
            .unwrap_or_default()
    }

    /// Settle the round's offer against what the round actually did, and log both sides. A take-up
    /// is a `todo_write` that names the offered row - a `needs` entry or a `when` - because nothing
    /// else puts the edge in the plan. Logged either way, because the ratio tunes [`OFFER_FLOOR`].
    pub fn settle(
        &mut self,
        logs: &Path,
        session: &str,
        calls: &[letibot_transcript::ToolCall],
    ) -> Option<&Offer> {
        let offer = self.open.take()?;
        let taken = calls.iter().any(|c| names_row(c, &offer.content));
        self.offered.insert(offer.key());
        let line = format!(
            "{{\"at\":{},\"session\":{},\"owner\":{},\"row\":{},\"score\":{:.3},\"taken\":{taken}}}\n",
            now_secs(),
            json(session),
            json(&offer.owner),
            json(&offer.content),
            offer.score
        );
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
            let _ = f.write_all(line.as_bytes());
        }
        self.last = Some(offer);
        self.last.as_ref()
    }

    /// The offer the round just settled, for a caller that reports on it.
    pub fn last(&self) -> Option<&Offer> {
        self.last.as_ref()
    }
}

/// The corpus a brief is scored against, keyed by the row's index so a score comes back as a row.
fn corpus(rows: &[OpenRow]) -> Vec<(PathBuf, String)> {
    rows.iter()
        .enumerate()
        .map(|(i, row)| (PathBuf::from(format!("{i}")), row.content.clone()))
        .collect()
}

/// The brief in hand: every `task` prompt the model's latest row proposed, the person's latest
/// prompt, and the model's own latest words. The subagent prompt leads because it is the most
/// specific brief of the three, and the cap falls at the oldest end so the brief being written now
/// is what survives it.
pub fn draft(items: &[TranscriptItem]) -> String {
    let mut prompt = String::new();
    let mut said = String::new();
    let mut asked = String::new();
    let mut took_said = false;
    for item in items.iter().rev() {
        match item {
            TranscriptItem::Assistant {
                text, tool_calls, ..
            } if !took_said => {
                took_said = true;
                said.push_str(text);
                said.push('\n');
                for call in tool_calls {
                    if let Some(p) = task_prompt(call) {
                        asked.push_str(&p);
                        asked.push('\n');
                    }
                }
            }
            // The person's own latest row is the brief when the model has not written one, and it
            // ends the walk: an older prompt is what the work has moved past.
            TranscriptItem::User { parts, speaker }
                if prompt.is_empty() && *speaker != Speaker::Agent =>
            {
                for part in parts {
                    if let UserPart::Text { text } = part {
                        prompt.push_str(text);
                        prompt.push('\n');
                    }
                }
                if !prompt.is_empty() {
                    break;
                }
            }
            _ => {}
        }
    }
    let mut out = asked;
    out.push_str(&prompt);
    out.push_str(&said);
    // The cut lands on a character boundary, never through one: this string is the person's
    // words plus a subagent prompt, both of which carry multi-byte characters.
    let cut = DRAFT_CHARS.min(out.len());
    let cut = (0..=cut)
        .rev()
        .find(|&i| out.is_char_boundary(i))
        .unwrap_or(0);
    out.truncate(cut);
    out.trim().to_string()
}

/// The prompt a `task` call writes into a child's session: the subagent prompt this caller exists
/// for, and only that key - the call's other arguments are a seat and a model, not a brief.
pub fn task_prompt(call: &letibot_transcript::ToolCall) -> Option<String> {
    if call.name != "task" && call.name != "task_start" {
        return None;
    }
    let args: serde_json::Value = serde_json::from_str(&call.arguments).ok()?;
    args.get("prompt")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Would this call join the offered row to the plan? A `todo_write` whose arguments name the row:
/// a `needs` entry, a `when`, or an edited row that quotes it. Anything else is not the edge.
fn names_row(call: &letibot_transcript::ToolCall, content: &str) -> bool {
    if call.name != "todo_write" && call.name != "todo" {
        return false;
    }
    call.arguments.contains(content)
}

/// Seconds since the unix epoch. No date library for one field of one log line.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A JSON string, escaped by `serde_json` rather than by hand: a row's words are prose, and prose
/// is how a hand-built log line becomes unparseable.
fn json(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// The census, in its own file, beside the other offers'.
#[cfg(test)]
mod tests;
