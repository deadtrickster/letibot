//! A note that offers itself - the read hint, composed at the TAIL of the request: never the
//! prefix (a prefix change re-prefills the conversation) and never a row (a row cannot be un-said).
//! Earned by a score floor, rare, silent when unsure, never a fetch, and recomposed each round.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use letibot_transcript::{Speaker, ToolCall, TranscriptItem, UserPart};

use crate::standing_notes;
use letibot_embeddings::Tier;
use letibot_tools::similarity::Corpus;

/// The score a note has to reach before it is offered at all - the hint's floor, because *the
/// first time a reader learns to ignore the hint line, the feature is dead*. MEASURED and moved to
/// 0.18 from the take-up log: nothing below 0.183 was ever taken. Tune it from that ratio.
pub const OFFER_FLOOR: f64 = 0.18;

/// How many characters of the subject are scored, taken from the most recent end. A cap and not a
/// budget: the subject is *what is happening now*, and a 200 KB file the last tool call printed is
/// not. [`subject`] walks backwards, so what survives the cut is the most recent text.
pub const SUBJECT_CHARS: usize = 8_000;

/// The file the take-up log is appended to, in the session's logs directory - it was
/// `<workspace>/.letibot/`, which made every child's worktree look dirty.
pub const LOG: &str = "notes-offers.jsonl";

/// One note, offered, with the score that earned it and whether the meaning was part of that
/// score - a line that claimed meaning over a box with no embedder would be describing an
/// arithmetic it did not use.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub path: PathBuf,
    pub score: f64,
    pub blended: bool,
}

impl Offer {
    /// The line the model is given, as a transcript item that is not in the log: the request
    /// assembly renders it after everything the session holds, and the next round composes a fresh
    /// one. Written in the harness's own voice and saying so, and it names the path and stops.
    pub fn tail(&self) -> TranscriptItem {
        TranscriptItem::User {
            speaker: Speaker::Agent,
            parts: vec![UserPart::Text {
                text: format!(
                    "[standing-notes offer — the harness's own line, not the operator's] `{}` \
                     looks relevant to what you are doing now (similarity {:.2} of 1.00, \
                     {}). It is not in this prompt. \
                     If it is worth the context, open it — `read` that path, or `notes` \
                     action=\"read\". This line is composed for this round and is gone from \
                     the next one; the note itself, once read, is not.",
                    self.path.display(),
                    self.score,
                    match self.blended {
                        true => "over its abstract, title and headings, and their meaning",
                        false => "lexical over its abstract, title and headings",
                    }
                ),
            }],
        }
    }
}

/// The offers this turn has already made, and the one the round in flight is carrying. Two facts are
/// kept apart on purpose: [`Offers::offered`] is the session's memory (a note is offered once), and
/// [`Offers::open`] is the round's (an offer composed and not yet settled).
#[derive(Debug, Default)]
pub struct Offers {
    offered: HashSet<PathBuf>,
    open: Option<Offer>,
    last: Option<Offer>,
    /// The box's second tier, when it has one: this offer's corpus's vectors, and nothing of
    /// another caller's. `None` is the lexical score exactly, which is the default.
    tier: Option<Tier>,
}

impl Offers {
    pub fn new() -> Offers {
        Offers::default()
    }

    /// Score with the box's second tier. Off is `None`, and then this offer is the one it was
    /// before the tier existed; the floor is this caller's either way.
    pub fn with_tier(mut self, tier: Option<Tier>) -> Offers {
        self.tier = tier;
        self
    }

    /// Seed the memory with what this SESSION has already been offered. The memory used to be the
    /// turn's alone, which is wrong about a session
    /// The log is the memory, so the fact survives a restart and there is no second copy of it.
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
            if let Some(path) = row.get("path").and_then(|p| p.as_str()) {
                self.offered.insert(PathBuf::from(path));
            }
        }
    }

    /// Compose this round's offer, or decide to say nothing. `false` is the common case. The
    /// corpus is [`standing_notes::gather`]; a note is skipped when this session already offered
    /// read or wrote it, or when the prompt already carries it whole - the purest nag of all.
    pub fn compose(
        &mut self,
        files: &standing_notes::Reader<'_>,
        global: &Path,
        box_dirs: &leticode_config::BoxDirs,
        system: &str,
        items: &[TranscriptItem],
    ) -> bool {
        self.open = None;
        let text = subject(items);
        if text.trim().is_empty() {
            return false;
        }
        let files = standing_notes::gather(files, global, box_dirs);
        if files.is_empty() {
            return false;
        }
        let corpus = Corpus::new(&files);
        // The tier is filled from THIS corpus, through the one door that keys it as the score
        // reads it. A fill that cannot answer leaves no vectors, so the words stand alone.
        let _ = corpus.fill(self.tier.as_ref());
        let already: HashSet<PathBuf> = items
            .iter()
            .filter_map(|i| match i {
                TranscriptItem::Assistant { tool_calls, .. } => Some(tool_calls.as_slice()),
                _ => None,
            })
            .flatten()
            .filter_map(read_of)
            .collect();
        // And the notes this session wrote, which no `read` call will have recorded: see
        // [`written_of`]. Their author has the content already.
        let wrote: HashSet<PathBuf> = items
            .iter()
            .filter_map(|i| match i {
                TranscriptItem::Assistant { tool_calls, .. } => Some(tool_calls.as_slice()),
                _ => None,
            })
            .flatten()
            .filter_map(written_of)
            .collect();
        // The best note over the floor that is neither spent, nor already in the conversation, nor
        // already in the prompt. `best` answers with ONE, so a note that is skipped is skipped for
        // good this round: if the top match must not be offered, the round says nothing.
        let Some((path, score)) = corpus.best_with(&text, self.tier.as_ref()) else {
            return false;
        };
        if score < OFFER_FLOOR
            || self.offered.contains(path)
            || already.contains(path)
            || already.iter().any(|p| same_note(p, path))
            || wrote.iter().any(|p| same_note(p, path))
            || carried_verbatim(system, path)
        {
            return false;
        }
        self.open = Some(Offer {
            path: path.to_path_buf(),
            score,
            blended: self.tier.is_some(),
        });
        true
    }

    /// The trailing item this round's request carries - empty when the round decided to say nothing
    /// which is the same code path as *the model ignored the offer*.
    pub fn tail(&self) -> Vec<TranscriptItem> {
        self.open
            .as_ref()
            .map(|o| vec![o.tail()])
            .unwrap_or_default()
    }

    /// Settle the round's offer against what the round actually did, and log both sides. A
    /// take-up is a `read` of the offered path, or a `notes` read of it, because nothing else puts
    /// the note in the conversation. Logged either way, because the ratio tunes [`OFFER_FLOOR`].
    pub fn settle(&mut self, logs: &Path, session: &str, calls: &[ToolCall]) -> Option<&Offer> {
        let offer = self.open.take()?;
        let taken = calls.iter().any(|c| {
            read_of(c)
                .map(|p| same_note(&p, &offer.path))
                .unwrap_or(false)
        });
        self.offered.insert(offer.path.clone());
        let line = format!(
            "{{\"at\":{},\"session\":{},\"path\":{},\"score\":{:.3},\"taken\":{taken}}}\n",
            now_secs(),
            json(session),
            json(&offer.path.display().to_string()),
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

/// Whether the prompt already carries this note whole. The injected section writes `### <path>`
/// and then the text, or `### <path> - N line(s), indexed: …` when the file did not fit. Asked of
/// the prompt, because a copy of that fact can disagree with the block the model was given.
pub fn carried_verbatim(system: &str, path: &Path) -> bool {
    let marker = format!("### {}", path.display());
    system.lines().any(|line| {
        line.strip_prefix(&marker)
            .is_some_and(|rest| !rest.contains("indexed"))
    })
}

/// The subject a hint is scored against: the person's prompt and the turn's tool results, and
/// nothing else. Walked backwards, so the most recent material survives [`SUBJECT_CHARS`]; the walk
/// stops at the person's own last row, and a harness notice does not stop it.
pub fn subject(items: &[TranscriptItem]) -> String {
    let mut out = String::new();
    for item in items.iter().rev() {
        let text: &str = match item {
            TranscriptItem::ToolResult { payload, .. } => payload,
            TranscriptItem::User { parts, speaker } => {
                if *speaker == Speaker::Agent {
                    continue;
                }
                // The person's own row ends the walk, and it is taken too: the prompt is half of
                // what a hint should fire on. Every text part of it, joined.
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
    // The cut lands on a character boundary, never through one - `truncate` panics otherwise
    // and this string is the operator's words plus tool results, full of `-`, `⚠` and Cyrillic.
    // Byte 0 is always a boundary.
    let cut = SUBJECT_CHARS.min(out.len());
    let cut = (0..=cut)
        .rev()
        .find(|&i| out.is_char_boundary(i))
        .unwrap_or(0);
    out.truncate(cut);
    out
}

/// A note this session WROTE - `notes` with `add`/`append`/`replace`, or a `write`/`edit` naming
/// one. Its author has the content already, and such a note wins its round: a note about what you
/// are doing scores highest against the subject you are writing about.
pub fn written_of(call: &ToolCall) -> Option<PathBuf> {
    let args: serde_json::Value = serde_json::from_str(&call.arguments).ok()?;
    match call.name.as_str() {
        "notes" => match args.get("action").and_then(|v| v.as_str())? {
            "add" | "append" | "replace" => args
                .get("path")
                .or_else(|| args.get("name"))
                .and_then(|v| v.as_str())
                .map(PathBuf::from),
            _ => None,
        },
        "write" | "edit" => args.get("path").and_then(|v| v.as_str()).map(PathBuf::from),
        _ => None,
    }
}

/// The note a tool call would put into the conversation - `read`'s `path`, or a `notes` read's
/// `path`/`name`. Deliberately narrow: `grep`, `search` and `glob` can mention a note without
/// opening it, and counting those would make the ratio measure interest rather than use.
pub fn read_of(call: &ToolCall) -> Option<PathBuf> {
    let args: serde_json::Value = serde_json::from_str(&call.arguments).ok()?;
    match call.name.as_str() {
        "read" => args.get("path").and_then(|v| v.as_str()).map(PathBuf::from),
        "notes" => {
            if args.get("action").and_then(|v| v.as_str()) != Some("read") {
                return None;
            }
            args.get("path")
                .or_else(|| args.get("name"))
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
        }
        _ => None,
    }
}

/// Whether two spellings name the same note: the path a call carries may be absolute
/// workspace-relative, or a bare name, so the comparison is by trailing components. Two notes with
/// the same file name in two sources are *not the same note* when both paths are absolute.
pub fn same_note(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    if a.is_absolute() && b.is_absolute() {
        return false;
    }
    // `.md` optional on either side: the `notes` tool takes a bare stem and the offer names the
    // file, so `the-offer-and-the-tail` and `/ws/.letibot/notes/the-offer-and-the-tail.md` are one.
    let norm = |p: &Path| p.display().to_string().trim_end_matches(".md").to_string();
    let (a, b) = (norm(a), norm(b));
    a == b || a.ends_with(&format!("/{b}")) || b.ends_with(&format!("/{a}"))
}

/// Seconds since the unix epoch. No date library for one field of one log line.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A JSON string, escaped by `serde_json` rather than by hand - a note's path is usually a plain
/// path, and "usually" is how a log line becomes unparseable.
fn json(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// The offer's census, in its own file - the shape `standing_notes` has, and for the same
/// reason: these tests read the module's private state and its fixtures are its own business.
#[cfg(test)]
mod tests;
