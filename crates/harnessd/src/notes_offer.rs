//! **A note that offers itself** — the read hint, composed at the tail of the
//! request and never in it.
//!
//! The operator's ask: *"so, regarding digest - one liner without heading is bad,
//! and the truncation is also bad. but we cant let the notes consume entire
//! context lol. so the question is, how model can make a choice to consult notes
//! on its own… ideal solution is of course that a note is offered where
//! appropriate."* And the ruling on the mechanism: *"we can do a simple
//! similarity match over corpus - like having a vector db over notes. and then
//! inject hints and similarity score"*.
//!
//! # The one constraint that decides where this lives
//!
//! The notes section is in the **cached prefix**, and changing anything in the
//! prefix invalidates every cached token after it — `standing_notes`'s own
//! measurement, from `docs/compaction.md` §2/§3: *144.6 s to re-prefill 150k
//! tokens against 0.8 s for a cache hit*. So an offer is
//!
//! * **never a note injected into the prompt** — that is a prefix change and it
//!   would re-prefill the conversation to say *"there is a note you might want"*;
//! * **and not a transcript row either**, which is the sharper half. The module
//!   that landed the index put the hint here: *"The delivery is a transcript row,
//!   never the prefix."* The operator's ruling is further out: an offer is
//!   **provisional**, and its outcome keeps it or kills it — *"we offer a note and
//!   if model takes it - good, if it doesnt follow up with the note read - we can
//!   discard the offer from context altogether"*. A row cannot be un-said; an
//!   assembly can be recomposed. So the offer is composed **per round by the
//!   request assembly**, handed to the engine as a trailing item that is not in
//!   `session.items`, and dropped entirely when the round does not take it.
//!
//! *An offer that was taken needs no special handling:* the model's `read` is an
//! ordinary tool call, its result is an ordinary transcript row, and the note's
//! text is now permanent context. The offer's only job was to make the fetch
//! happen.
//!
//! # The four rules, which are what keep it from becoming a nag
//!
//! The design's own lesson, learned from the digest's teasers: *"a hint is a nag
//! by another name. It must be **earned** (a score floor), **rare** (once per note
//! per turn at most), and **silent when unsure** — the first time a reader learns
//! to ignore the hint line, the feature is dead."*
//!
//! 1. **Earned.** [`OFFER_FLOOR`], measured — see `letibot_tools::similarity`'s
//!    module doc for the numbers, and the honest gap between them.
//! 2. **Rare.** At most one note per round, and a note once per turn. A round
//!    that already offered a note does not offer a second; a note already offered
//!    this turn is not offered again even if it is still the best match.
//! 3. **Silent when unsure.** No score over the floor, no line. A corpus that is
//!    empty, a subject with no terms, a note already read in this session — all
//!    of them are silence rather than a weaker hint.
//! 4. **Never a fetch.** The line names the path and stops. It does not read the
//!    note, quote it, or summarise it: *"Never a `read`. A hint that silently
//!    spends the model's context on a note is the injected-material failure
//!    `docs/memory.md` §5 measures; the model decides whether to open what was
//!    offered."*
//!
//! # The comparison subject
//!
//! The person's prompt and the turn's tool results — never the whole
//! conversation, which is the design's own rule and the one that decides whether
//! a hint fires on what is happening now or on everything that ever happened.
//! [`subject`] walks the transcript backwards from the end, takes the tool
//! results, and stops at the person's own last row.
//!
//! # The take-up, which is what tunes the floor
//!
//! *"The take-up is observable, which is what keeps the floor honest: the signal
//! is did a tool call in the next round touch that path? Log offers and take-ups;
//! tune the floor from that ratio. And the same log answers a second question — an
//! offer nobody ever takes is a note to rewrite or retire."*
//!
//! So every offer that reaches a request is settled once the round's tool calls
//! are known, and one line per offer lands in
//! `<workspace>/.letibot/notes-offers.jsonl`: the path, the score, whether it was
//! taken, and what took it. The log is best-effort — a hint that fails to write a
//! line about itself is still a hint, and a notes feature that breaks a turn
//! because a file could not be appended to would be worse than no notes.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use letibot_transcript::{Speaker, ToolCall, TranscriptItem, UserPart};

use crate::standing_notes;
use letibot_tools::similarity::Corpus;

/// **The score a note has to reach before it is offered at all.**
///
/// The hint's floor, and the reason it is not lower: *"the first time a reader
/// learns to ignore the hint line, the feature is dead."* MEASURED 2026-10-12
/// against this box's 23 standing notes — the numbers and the arithmetic are in
/// `letibot_tools::similarity`'s module doc. In short: across ten prompts about
/// work unrelated to that corpus the best score any note reached was 0.133, and
/// across four prompts squarely about a note the scores were 0.181, 0.126, 0.419
/// and 0.366. 0.14 is above every negative measured and below three of the four
/// positives.
///
/// **It is a starting point, not a finding**, and the design says so itself:
/// *"tune the floor from that ratio"* — the take-up log
/// (`<workspace>/.letibot/notes-offers.jsonl`) is the measurement that moves it.
///
/// **Moved to 0.18 on that measurement, 2026-10-10.** The fixtures had the honest
/// positives at 0.366–0.419; live offers land far lower, and the log of **121 offers
/// with 19 taken** says where the two populations actually divide: the lowest score
/// ever taken was 0.183, and of the 61 offers below it **not one was taken**. At 0.18
/// the offer count halves — 60 instead of 121 — and the take-ups stay 19, all of
/// them; 0.20 would cost one (41 offers, 18 taken) and 0.22 would cost ten (23
/// offers, 9 taken). Above the floor the shape is a cliff, not a slope, which is what
/// a floor is for.
///
/// The operator's word for the volume at 0.14 was *"the notes keep pilin up.
/// unacceptable"*, and the other half of that fix is the memory: a note is offered
/// once per SESSION, not once per turn — see [`Offers::recall`].
pub const OFFER_FLOOR: f64 = 0.18;

/// How many characters of the subject are scored, taken from the most recent end.
///
/// A cap and not a budget, for a reason that is about the subject rather than
/// about cost: the subject is *what is happening now*, and a 200 KB file the last
/// tool call printed is not what is happening now. Past the cap the older
/// material is dropped — the walk in [`subject`] goes backwards, so what survives
/// is the most recent text, which is the text a hint should fire on.
pub const SUBJECT_CHARS: usize = 8_000;

/// The file the take-up log is appended to, under the workspace.
pub const LOG: &str = ".letibot/notes-offers.jsonl";

/// One note, offered, with the score that earned it.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub path: PathBuf,
    pub score: f64,
}

impl Offer {
    /// The line the model is given, as a transcript item that is **not** in the
    /// log: the request assembly renders it after everything the session holds,
    /// and the next round composes a fresh one from scratch.
    ///
    /// Written in the harness's own voice and saying so, because a row that
    /// cannot be told from the operator's words is how a harness's suggestion
    /// comes to read as an instruction the person gave. It names the path and
    /// stops: no excerpt, no summary, no `read` on the model's behalf.
    pub fn tail(&self) -> TranscriptItem {
        TranscriptItem::User {
            speaker: Speaker::Agent,
            parts: vec![UserPart::Text {
                text: format!(
                    "[standing-notes offer — the harness's own line, not the operator's] `{}` \
                     looks relevant to what you are doing now (similarity {:.2} of 1.00, \
                     lexical over its abstract, title and headings). It is not in this prompt. \
                     If it is worth the context, open it — `read` that path, or `notes` \
                     action=\"read\". This line is composed for this round and is gone from \
                     the next one; the note itself, once read, is not.",
                    self.path.display(),
                    self.score
                ),
            }],
        }
    }
}

/// The offers this turn has already made, and the one the round in flight is
/// carrying.
///
/// Lives on the `Harness` for the length of a turn. Two facts are kept apart on
/// purpose: [`Offers::offered`] is the turn's memory (a note is offered once),
/// and [`Offers::open`] is the round's (an offer that has been composed and not
/// yet settled).
#[derive(Debug, Default)]
pub struct Offers {
    offered: HashSet<PathBuf>,
    open: Option<Offer>,
    last: Option<Offer>,
}

impl Offers {
    pub fn new() -> Offers {
        Offers::default()
    }

    /// **Seed the memory with what this SESSION has already been offered.**
    ///
    /// The memory used to be the turn's alone — *"rare (once per note per turn at
    /// most)"* — which is right about one turn and wrong about a session. Measured
    /// 2026-10-10 on this box, in one workspace: of 120 offers, 19 were taken, one
    /// note had been offered 22 times and another 20, and every repeat is a row in
    /// the operator's transcript. Their words: *"the notes keep pilin up.
    /// unacceptable"*. A note is offered once per session now, not once per turn.
    ///
    /// **The log is the memory, rather than a set beside it.** It already carries the
    /// session id and the path of every offer — [`Offers::settle`] writes both — so
    /// the fact survives a daemon restart, and there is no second copy of it to
    /// disagree with the file. A line that cannot be parsed is skipped: this is the
    /// notes path, where a hint that fails is still a hint.
    pub fn recall(&mut self, workspace: &Path, session: &str) {
        let Ok(text) = std::fs::read_to_string(workspace.join(LOG)) else {
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

    /// **Compose this round's offer**, or decide to say nothing. `false` is the
    /// common case and the one the design asks for.
    ///
    /// The corpus is the notes the harness injects — the same three sources, in
    /// the same order, read once through [`standing_notes::gather`] — so an offer
    /// can only ever name a note a `read` can then find.
    ///
    /// A note is skipped when this turn already offered it, when the session has
    /// already read it, and **when the prompt already carries it whole**. Each of
    /// those is an offer to fetch something the model is looking at, which is the
    /// purest form of the nag this module exists not to be — and the last one is
    /// not hypothetical: below the notes budget every note is injected verbatim,
    /// and the note most likely to score is exactly the one that is already there.
    pub fn compose(
        &mut self,
        workspace: &Path,
        global: &Path,
        system: &str,
        items: &[TranscriptItem],
    ) -> bool {
        self.open = None;
        let text = subject(items);
        if text.trim().is_empty() {
            return false;
        }
        let files = standing_notes::gather(workspace, global);
        if files.is_empty() {
            return false;
        }
        let corpus = Corpus::new(&files);
        let already: HashSet<PathBuf> = items
            .iter()
            .filter_map(|i| match i {
                TranscriptItem::Assistant { tool_calls, .. } => Some(tool_calls.as_slice()),
                _ => None,
            })
            .flatten()
            .filter_map(read_of)
            .collect();
        // The best note over the floor that is neither spent, nor already in the
        // conversation, nor already in the prompt. `best` answers with one, so a
        // note that is skipped is skipped for good this round rather than fallen
        // through to — which is the honest reading of "silent when unsure"
        // anyway: if the top match is one we must not offer, the round says
        // nothing.
        let Some((path, score)) = corpus.best(&text) else {
            return false;
        };
        if score < OFFER_FLOOR
            || self.offered.contains(path)
            || already.contains(path)
            || already.iter().any(|p| same_note(p, path))
            || carried_verbatim(system, path)
        {
            return false;
        }
        self.open = Some(Offer {
            path: path.to_path_buf(),
            score,
        });
        true
    }

    /// The trailing item this round's request carries — empty when the round
    /// decided to say nothing, which is the same code path as *the model ignored
    /// the offer*: nothing is composed, so nothing is sent.
    pub fn tail(&self) -> Vec<TranscriptItem> {
        self.open
            .as_ref()
            .map(|o| vec![o.tail()])
            .unwrap_or_default()
    }

    /// **Settle the round's offer against what the round actually did**, and log
    /// both sides.
    ///
    /// The signal is the design's own: *"did a tool call in the next round touch
    /// that path?"* A take-up is a `read` of the offered path, or a `notes` read
    /// of it — nothing else counts, because nothing else puts the note in the
    /// conversation.
    ///
    /// Logged whichever way it went, because the ratio is the whole point: a note
    /// offered and never taken is a note to rewrite, retire, or stop offering,
    /// and a note offered and taken is evidence the floor is not too high.
    pub fn settle(
        &mut self,
        workspace: &Path,
        session: &str,
        calls: &[ToolCall],
    ) -> Option<&Offer> {
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
        let log = workspace.join(LOG);
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

/// **Whether the prompt already carries this note whole.**
///
/// The injected section writes each file as `### <path>` and then its text, or —
/// when the file did not fit what was left of the notes budget — as
/// `### <path> — N line(s), indexed: …` and then headings. So the question is
/// which of those two shapes the section holds for this path, and it is asked of
/// the prompt rather than remembered beside it, because a copy of that fact is a
/// copy that can disagree with the block the model was actually given.
///
/// A path the prompt does not name at all is not carried: it may have arrived
/// after the prompt was composed, and offering a note nobody has seen is the
/// whole point of this module.
pub fn carried_verbatim(system: &str, path: &Path) -> bool {
    let marker = format!("### {}", path.display());
    system.lines().any(|line| {
        line.strip_prefix(&marker)
            .is_some_and(|rest| !rest.contains("indexed"))
    })
}

/// The subject a hint is scored against: **the person's prompt and the turn's
/// tool results**, and nothing else.
///
/// Walked backwards from the end of the transcript, so the most recent material
/// is what survives [`SUBJECT_CHARS`]. Tool results are taken as they are found;
/// the walk stops at the person's own last row, which is where the current turn
/// begins. Rows the harness wrote in its own voice (`Speaker::Agent`) are not the
/// person's words and do not stop the walk — a notice between two tool results
/// must not hide the prompt under it.
///
/// **Newest first, and the order is not read.** The only consumer is a bag of
/// terms, so what the order buys is the one thing that matters: when the turn has
/// produced more than [`SUBJECT_CHARS`] of text it is the RECENT text that
/// survives the cut, because *what is happening now* is the question and the
/// first tool result of a long turn is history.
pub fn subject(items: &[TranscriptItem]) -> String {
    let mut out = String::new();
    for item in items.iter().rev() {
        let text: &str = match item {
            TranscriptItem::ToolResult { payload, .. } => payload,
            TranscriptItem::User { parts, speaker } => {
                if *speaker == Speaker::Agent {
                    continue;
                }
                // The person's own row ends the walk, and it is taken too: the
                // prompt is half of what a hint should fire on. Every text part
                // of it, joined.
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
    // **The cut lands on a character boundary, never through one.** `truncate`
    // panics when the length it is handed is not a boundary of the string, and
    // this string is the operator's own words plus the turn's tool results —
    // full of `—`, `⚠`, `→`, box-drawing from a `capture-pane` and Cyrillic.
    // Measured 2026-10-10, on this box: an `assertion failed:
    // self.is_char_boundary(new_len)` raised right here took `thread 'main'` down
    // with it, so the daemon died from the offer path — a path whose own contract
    // is that a hint which fails is still a hint, and whose worst case was meant
    // to be a missing line in a log. Three panics in one evening, one of them a
    // subagent's thread, each beside the offer it was composing.
    //
    // Walk to the nearest boundary at or below the cap: byte 0 is always one, so
    // the search cannot come up empty.
    let cut = SUBJECT_CHARS.min(out.len());
    let cut = (0..=cut)
        .rev()
        .find(|&i| out.is_char_boundary(i))
        .unwrap_or(0);
    out.truncate(cut);
    out
}

/// **The note a tool call would put into the conversation** — `read`'s `path`, or
/// a `notes` read's `path`/`name`. `None` for every other call.
///
/// Deliberately narrow: `grep`, `search` and `glob` can all *mention* a note
/// without opening it, and counting those as take-ups would make the ratio
/// measure interest rather than use. What the log is for is *did the offer turn
/// into the note being read*, which only a read can answer.
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

/// Whether two spellings name the same note: the path a call carries may be
/// absolute, workspace-relative, or a bare name (`notes` accepts all three, and
/// `list` reports the absolute one), so the comparison is by trailing components
/// rather than by string equality.
///
/// Two notes with the same file name in two sources (`AGENTS.md` in a workspace
/// and a box-wide `AGENTS.md`) are the ambiguous case, and it is resolved toward
/// *not the same note* only when both paths are absolute and differ.
pub fn same_note(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    if a.is_absolute() && b.is_absolute() {
        return false;
    }
    // `.md` optional on either side: the `notes` tool takes a bare stem and the
    // offer names the file, so `the-offer-and-the-tail` and
    // `/ws/.letibot/notes/the-offer-and-the-tail.md` are one note.
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

/// A JSON string, escaped by `serde_json` rather than by hand — a note's path is
/// usually a plain path, and "usually" is how a log line becomes unparseable.
fn json(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// **The offer's census, in its own file** — the shape `standing_notes` has, and
/// for the same reason: these tests read the module's private state (`Offers`'
/// two facts) and its fixtures are the module's own business.
#[cfg(test)]
mod tests;
