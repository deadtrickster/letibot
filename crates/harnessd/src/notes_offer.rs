//! A note that offers itself - the read hint, composed at the TAIL of the request: never the
//! prefix (a prefix change re-prefills the conversation) and never a row (a row cannot be un-said).
//! Earned by a score floor, rare, silent when unsure, never a fetch, and recomposed each round.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::offer_evidence::{self, DECLINE_ROUNDS, Declined, Declines, Pending};
use letibot_tools::similarity::{self, Evidence};

use letibot_transcript::{Speaker, ToolCall, TranscriptItem, UserPart};

use crate::standing_notes;
use letibot_embeddings::Tier;
use letibot_tools::similarity::Corpus;

/// The score a note has to reach before it is offered at all, so a reader never learns to ignore
/// the hint line. Set from the take-up log, where nothing below it was taken; tune it from that.
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
    /// Set by `settle`: the round read it.
    pub taken: bool,
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
                    "[note offer — the harness's own line, not the operator's] `{}` may be relevant to what you are doing now, and it \
                     is not in this prompt. Read it — `read` that path, or `notes` \
                     action=\"read\". This line is composed for this round and is gone from \
                     the next one; the note itself, once read, is not.",
                    self.path.display(),
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
    /// The identity words and the version the open offer was let through on.
    open_hits: (Vec<String>, String),
    /// Offers not read yet, counting down to a decline. Keyed by the offered path.
    pending: Vec<Pending>,
    /// Notes declined this session, with the words each was offered on.
    declined: HashMap<PathBuf, Declined>,
    /// This project's declines, which raise a note's floor.
    declines: Declines,
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

    /// Count declines against this project's log in `dir`.
    pub fn with_project(mut self, dir: &Path) -> Offers {
        self.declines = Declines::read(dir, "note");
        self
    }

    /// Score this round against the one subject the loop names for every offer.
    pub fn share_round(&self, text: Option<String>) {
        if let Some(tier) = &self.tier {
            tier.set_round(text);
        }
    }

    /// Seed the memory with what this SESSION has already been offered. The memory is not the
    /// turn's alone, which would be wrong about a session
    /// The log is the memory, so the fact survives a restart and there is no second copy of it.
    pub fn recall(&mut self, logs: &Path, session: &str) {
        let Ok(text) = std::fs::read_to_string(logs.join(LOG)) else {
            return;
        };
        let mut open: BTreeMap<String, (Vec<String>, String)> = BTreeMap::new();
        for line in text.lines() {
            let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if row.get("session").and_then(|s| s.as_str()) != Some(session) {
                continue;
            }
            if let Some(path) = row.get("path").and_then(|p| p.as_str()) {
                self.offered.insert(PathBuf::from(path));
                let hits: Vec<String> = row
                    .get("hits")
                    .and_then(|h| h.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let version = row
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if row.get("taken").and_then(|t| t.as_bool()) == Some(true) {
                    open.remove(path);
                } else if row.get("declined").and_then(|t| t.as_bool()) == Some(true) {
                    self.declined
                        .insert(PathBuf::from(path), Declined { hits, version });
                    open.remove(path);
                } else {
                    open.insert(path.to_string(), (hits, version));
                }
            }
        }
        // An offer the last turn left open still has its rounds to be read in.
        self.pending
            .extend(open.into_iter().map(|(key, (hits, version))| Pending {
                key,
                rounds: DECLINE_ROUNDS - 1,
                hits,
                version,
            }));
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
        let key = path.display().to_string();
        let version = files
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, text)| offer_evidence::version(text))
            .unwrap_or_default();
        // Under the tier the note's title must be in the operator's words or the touched files:
        // meaning alone finds a neighbour of whatever the session is doing.
        let hits = match &self.tier {
            Some(_) => match evidence_of(&files).found(&key, &offer_evidence::seen(items)) {
                Some(h) => h,
                None => return false,
            },
            None => Vec::new(),
        };
        // A declined note comes back only on words the decline was not made on, or edited.
        let renewed = self
            .declined
            .get(path)
            .is_some_and(|before| before.reopened_by(&hits, &version));
        if score < self.declines.floor(OFFER_FLOOR, &key, &version)
            || (self.offered.contains(path) && !renewed)
            || already.contains(path)
            || already.iter().any(|p| same_note(p, path))
            || wrote.iter().any(|p| same_note(p, path))
            || carried_verbatim(system, path)
        {
            return false;
        }
        self.open_hits = (hits, version);
        self.open = Some(Offer {
            path: path.to_path_buf(),
            score,
            blended: self.tier.is_some(),
            taken: false,
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
        let read = |path: &Path| {
            calls
                .iter()
                .any(|c| read_of(c).map(|p| same_note(&p, path)).unwrap_or(false))
        };
        // Earlier offers first: a late read is a take-up, and one never read is a decline.
        let mut lines = String::new();
        let mut waiting = Vec::new();
        for p in std::mem::take(&mut self.pending) {
            if read(Path::new(&p.key)) {
                lines.push_str(&format!(
                    "{{\"at\":{},\"session\":{},\"path\":{},\"taken\":true,\"late\":true}}\n",
                    now_secs(),
                    json(session),
                    json(&p.key)
                ));
            } else {
                waiting.push(p);
            }
        }
        self.pending = waiting;
        for p in offer_evidence::tick(&mut self.pending) {
            self.declines
                .record(session, &p.key, &p.version, now_secs());
            lines.push_str(&format!(
                "{{\"at\":{},\"session\":{},\"path\":{},\"declined\":true,\"hits\":{},\"version\":{}}}\n",
                now_secs(),
                json(session),
                json(&p.key),
                serde_json::json!(p.hits),
                json(&p.version)
            ));
            self.declined.insert(
                PathBuf::from(&p.key),
                Declined {
                    hits: p.hits,
                    version: p.version,
                },
            );
        }
        let Some(mut offer) = self.open.take() else {
            offer_evidence::append(&logs.join(LOG), &lines);
            return None;
        };
        let taken = read(&offer.path);
        offer.taken = taken;
        self.offered.insert(offer.path.clone());
        let (hits, version) = std::mem::take(&mut self.open_hits);
        let key = offer.path.display().to_string();
        if !taken {
            self.declined.remove(&offer.path);
            self.pending.push(Pending {
                key: key.clone(),
                rounds: DECLINE_ROUNDS - 1,
                hits: hits.clone(),
                version: version.clone(),
            });
        }
        lines.push_str(&format!(
            "{{\"at\":{},\"session\":{},\"path\":{},\"score\":{:.3},\"taken\":{taken},\"hits\":{},\"version\":{}}}\n",
            now_secs(),
            json(session),
            json(&key),
            offer.score,
            serde_json::json!(hits),
            json(&version)
        ));
        offer_evidence::append(&logs.join(LOG), &lines);
        self.last = Some(offer);
        self.last.as_ref()
    }

    #[cfg(test)]
    fn last_open_score(&self) -> f64 {
        self.open.as_ref().map(|o| o.score).unwrap_or(0.0)
    }

    /// The offer the round just settled, for a caller that reports on it.
    pub fn last(&self) -> Option<&Offer> {
        self.last.as_ref()
    }
}

/// Each note's identity - its title, the file stem with separators opened - weighted by rarity
/// across the notes' index texts.
fn evidence_of(files: &[(PathBuf, String)]) -> Evidence {
    let docs: Vec<(String, String, String)> = files
        .iter()
        .map(|(path, text)| {
            let title = path
                .file_stem()
                .map(|s| s.to_string_lossy().replace(['-', '_'], " "))
                .unwrap_or_default();
            (
                path.display().to_string(),
                title,
                similarity::index_text(path, text),
            )
        })
        .collect();
    Evidence::new(
        docs.iter()
            .map(|(k, i, f)| (k.as_str(), i.as_str(), f.as_str())),
    )
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
    // file, so `the-offer-and-the-tail` and `/notes/proj/the-offer-and-the-tail.md` are one.
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
