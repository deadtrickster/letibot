//! **A model that stops because "my context is full" when it is not gets told so** — R71.
//!
//! The operator, 2026-10-11: *"we have similarity for notes and todos, i want to catch
//! stupid things like this - "My context is at the wall" when models sometimes stop
//! because yeah "my context is full, my context is almost full, my context is at the
//! wall, blabla" when we see something like that we should slap model."*
//!
//! # Why it is wrong, not merely annoying
//!
//! The model cannot see its own fill; the daemon can. The wall is the HARNESS's to
//! handle — `Config::at_the_wall` and `compact_at_the_wall` compact when it is reached,
//! and a refused request compacts too — so a model that stops "because of the wall"
//! has stopped work the harness would have carried, on a guess. The store says how
//! often: assistant rows saying so in four sessions and their subagents, among them
//!
//! * leticl, 10-11 11:48 — *"My context is at the wall, so I'll stop here rather than
//!   start a piece I can't finish"*;
//! * leticl, 10-11 11:30 (and 06:13, 02:41, the same message re-sent) — *"My context is
//!   nearly spent, and that's the honest reason to stop rather than start the
//!   carry-by-words change"*;
//! * pg-noop, 10-10 18:49 — *"seven concurrent answers is well past my integration
//!   ceiling, and my own context is nearly exhausted"*;
//!
//! and one sentence that must NOT be slapped, because it is true and carries its
//! numbers: 10-07 10:49 — *"It hit the **context wall** — 1,131,717 of 1,146,137
//! tokens resident, with the next round unable to fit — and it was compacted"*. That is
//! the line between the two: a claim the daemon can check against what it measured.
//!
//! # The mechanism, in three parts
//!
//! 1. **Detect** — this module, pure: [`find`] scores each sentence of a round's
//!    assistant text (fenced code skipped, as `intent::commitments` does) with
//!    `letibot_tools::similarity` — the notes' TF-IDF cosine — against [`EXEMPLARS`],
//!    and keeps the best sentence at or over [`FLOOR`]. Similarity rather than a
//!    phrase list because the operator's own list ends in *"blabla"*: a lexical list
//!    is the `OPENERS` shape that misses a re-wording.
//! 2. **Check the claim against the measurement** — [`Check::judge`], handed the
//!    session's [`Fill`] by the harness. Below the wall the claim is false and the
//!    model is told so, with the numbers ([`notice`]); at the wall it is true, the
//!    harness compacts as it always has, and nothing is said. An unknown window is
//!    not a measurement, so it is silence too.
//! 3. **Once per claim per user turn** — the intent check's own budget. A model that
//!    repeats the excuse after being told is a finding for the operator
//!    ([`REPEATED`], a hub warning), not a second slap.
//!
//! **Always on, not behind `--intent-prose`.** That flag exists because the prose
//! heuristic can be wrong about intent. This one is gated by a measurement, so a false
//! positive costs one line telling the model the truth about its own fill.
//!
//! Every claim found is **logged** to [`LOG`] — the sentence, its score, the measured
//! fill, the window and whether it was slapped — so the floor is tuned from what
//! happened, the way `notes-offers.jsonl` tunes the notes' offer floor.
//!
//! # How a sentence is scored: a window around the noun, owned by the speaker
//!
//! Plain sentence-level cosine was measured first and is not good enough, for a reason
//! worth writing down. The excuse is very often a subordinate clause of a long sentence
//! (*"Not starting it: the pane needs six coordinated edits (constructor, slot, fold,
//! …) plus its tests, and at this point in my context that ends mid-edit"*), and cosine
//! normalises by the whole sentence's length, so the clause drowns. So the score is
//! taken over a **window of [`REACH`] terms either side of each [`ANCHORS`] noun**
//! (`context`, `room`), and the sentence's score is its best window.
//!
//! **And the window must be about the speaker's own context** — it has to carry one of
//! [`OWNERS`]: a first-person word, `remaining` or `fresh`. That is the measurement's
//! own condition stated lexically: the daemon can only check a claim about THIS
//! session's fill, so a sentence about a child's wall, the context window as a subject
//! of work (*"the context window is left unknown rather than wrong"*), or the context of
//! a change is not a claim it can check, and it is not scored.
//!
//! **First person is a term here, and it is not in the shared tokeniser.**
//! `similarity::terms` drops words under three characters, which drops `I`, `my` and
//! `me` — exactly the words that separate *"my context is at the wall"* from *"it hit
//! the context wall"*. [`words`] maps them to one term, [`SELF`], before the shared
//! arithmetic sees the text; `let me` is its own term (`letme`), because it is how
//! every plan starts and is not a claim about anything the speaker holds.
//!
//! **The background corpus is what prices a word.** `idf` is `ln(N/df)` over the
//! corpus, and a corpus of excuses alone prices `context` — in nearly all of them — at
//! almost nothing. [`BACKGROUND`] is a set of sentences that say `context` in its other
//! senses, report a wall that did happen, and talk like a coding agent about nothing
//! in particular, so the words every sentence uses are cheap and the words an excuse
//! uses (*spent, end, left, limit, full, nearly*) are not. Background sentences are in
//! the corpus for their document frequencies only; a sentence is scored against the
//! exemplars.
//!
//! # The floor, measured — and the gap that is left
//!
//! MEASURED 2026-10-11 against this box's session store
//! (`~/.local/share/letibot/sessions.db`, read-only): ONE scan of `transcript_item`
//! for assistant rows mentioning `context`, `run out of room` or `running low` — 4,547
//! rows — split into sentences, deduplicated (the same message is often re-sent), and
//! the 747 distinct sentences carrying `context`, `room` or `running low` labelled by
//! hand. **79 positives** — a model offering its own context as the reason it stops,
//! will not start, or hands the work on — and **641 negatives**: every other sense of
//! the word, every report of a wall that did happen, and the TRUE report of 10-07.
//! 27 were left out of both sides as ambiguous (*"Writing it down before my context
//! goes"*, a precaution; *"I've run out of room to keep guessing"*; a model's own
//! retraction quoting the excuse back, *"When I said 'I'm out of context' I was
//! guessing"*). The scores, through [`find`] as it runs:
//!
//! ```text
//!   floor   positives over   negatives over
//!   0.18        72/79            7/641
//!   0.20        69/79            6/641
//!   0.22        67/79            3/641
//!   0.25        63/79            0/641     <- FLOOR
//!   0.28        57/79            0/641
//! ```
//!
//! * **Negatives:** max **0.228** (*"that `<system-update seq=1>` block … is the path
//!   I deleted, arriving in my own context"* — first person, and about the context, and
//!   not an excuse), then 0.222 (*"what made room for the one that survived"*). The
//!   daemon's own *"This session is over its context budget…"* scores 0.000: its window
//!   has no owner. The TRUE report of 10-07 scores **0.000**: its window carries no
//!   first-person word, so it is not a claim about the speaker. The floor does not lean
//!   on the measurement to spare it — the measurement would (it was 1,131,717 of
//!   1,146,137), and that is the second guard, not the first.
//! * **Positives:** 0.000 to 1.000; R71's four verbatim excuses score 0.497 (leticl,
//!   11:48 — *"My context is at the wall…"*), 0.599, 0.719 and 0.782. Not 1.0 even where
//!   the sentence IS an exemplar, because the score is a window's and not the sentence's.
//! * **0.25 is above every negative measured, and below 63 of the 79 positives.**
//!
//! **The 16 it misses are the honest gap, and they have a shape.** Four score zero
//! because nothing in their window says whose context it is (*"there's no work left in
//! the file I can responsibly complete in what remains of this turn's context"*); the
//! rest are long sentences whose excuse is a few words of a subordinate clause
//! (*"so it's not something I should start at the end of a context window, and it
//! deserves your sign-off…"*, 0.20). A lexical tier cannot pull those up without
//! pulling the daemon's own wall sentences up with them — the lowering from 0.25 to
//! 0.22 buys four positives for three negatives. That is the measurement a second
//! tier (embeddings) is owed, written down here rather than discovered later. And the
//! cost of the miss is what it was before this check existed: the model stops, and the
//! operator reads why.
//!
//! **The calibration set is in-sample**, and that is said rather than hidden: the
//! exemplars are R71's verbatim sentences and their family (*almost full, running low,
//! near my context limit, at the end of my context, with the context I have left, a
//! fresh context*), which is the population the store holds. The log ([`LOG`]) is the
//! out-of-sample measurement, and it is what moves the floor.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use letibot_tools::similarity::{self, Corpus, MIN_TERM, Weights};

/// **The score a sentence has to reach before it is a claim.** See the module doc for
/// the measurement: above every one of 641 negatives (max 0.228), below 63 of 79
/// positives.
pub const FLOOR: f64 = 0.25;

/// How many terms either side of an [`ANCHORS`] noun are scored. Measured: 5 and 6
/// separate the calibration set at the same floor, 8 starts reaching into the next
/// clause and loses positives; 5 is the narrower.
pub const REACH: usize = 5;

/// The nouns an excuse is about. `room` because *"before I run out of room"* is the
/// same claim in other words.
pub const ANCHORS: &[&str] = &["context", "room"];

/// The term a first-person word becomes — see [`words`].
pub const SELF: &str = "myself";

/// The words that make a window a claim about the speaker's OWN context: first person,
/// what *remains* of it, or wanting a *fresh* one. Measured: adding `left` let in
/// *"the context window is left unknown"*, adding `turn` and `session` let in the
/// daemon's own *"This session is over its context budget"*.
pub const OWNERS: &[&str] = &[SELF, "remaining", "fresh"];

/// The file each claim is appended to, under the workspace.
pub const LOG: &str = ".letibot/context-claims.jsonl";

/// The hub warning for a model that repeats the claim after being told its fill.
pub const REPEATED: &str = "context_claim_repeated";

/// **The excuses** — R71's verbatim sentences and their family. A sentence is scored
/// against each of these and keeps its best.
pub const EXEMPLARS: &[&str] = &[
    "My context is at the wall, so I'll stop here rather than start a piece I can't finish.",
    "My context is nearly spent, and that's the honest reason to stop rather than start the next change.",
    "My own context is nearly exhausted.",
    "With my remaining context, the realistic outcome of starting now is a non-compiling tree at the wall.",
    "My context is full, so I will stop here.",
    "My context is almost full, so I won't start another change.",
    "I am running low on context, so I'll wrap up and report.",
    "I'm near my context limit, so let me stop at a clean point.",
    "I'm at the end of my context, so this is where I stop.",
    "I'm nearly out of context, so here is where things stand.",
    "I don't have enough context left to finish this, so I'm stopping.",
    "What is left of my context can't carry another change.",
    "To save context, I'll stop here and leave the rest for the next session.",
    "Before I run out of room, let me write down where this stands and stop.",
    "I won't start it with the context I have left.",
    "That deserves a fresh context rather than the tail of this one.",
    "I can't finish it in what is left of this session's context.",
    "I have very little context left, so I am naming it rather than starting it.",
    "I'm at the end of what I can hold in context, so here is the state.",
    // **The capacity claim used to justify HANDING WORK OFF**, not only stopping — added
    // 2026-10-11 after pg-noop said *"That last piece is mechanical but it is eight sites across
    // two files, and my context cannot carry it safely — a half-applied refactor is worse than
    // none, … Dispatching it"* and scored under the floor: none of the stop family's words
    // (full, spent, wall, out) are in it. The operator: *"look what we need to catch wrt
    // context"*.
    "My context cannot carry it safely, so I am dispatching it to a subagent.",
    "This is more than my context can hold, so I'll hand it to a child.",
    "My context won't survive this change, so I'm delegating it.",
    "I can't keep all of this in my context, so a subagent will do it.",
    "It is too big for my context, so I'm dispatching it with the sites named.",
];

/// **The background** — in the corpus for its document frequencies, never matched
/// against. Reports of a wall that did happen, `context` in its other senses, and a
/// coding agent's ordinary sentences. See the module doc.
pub const BACKGROUND: &[&str] = &[
    "The child hit the context wall after 77 rounds and the session was compacted.",
    "Three subagents died at the context wall on this task.",
    "Compaction fails at the context wall and the transcript forks.",
    "The context window is one million tokens.",
    "The catalogue gives this model a context window of 262144.",
    "In the context of this change, the old name still has readers.",
    "The context doc explains the design of the module.",
    "The request carries the system prompt, the tool results and the context of the session.",
    "The head shows how full the context is, in the provider's tokens.",
    "Let me read the surrounding lines for context.",
    "Show the diff with three lines of context around each hunk.",
    "The store keeps context_tokens on the session row and carries it on attach.",
    "The disk is full and the build failed.",
    "The queue is full, so the next job waits.",
    "Let me read the file and check what the test asserts.",
    "Now I will run the tests and commit the change.",
    "The fix is in the config module, and the tree is green.",
    "I am going to start with the parser and then wire the caller.",
    "This is the state of the branch, and here is what is left to do.",
    "The test fails because the fixture was written before the field existed.",
    "Let me check the release and then report what landed.",
    "I will stop the server, rebuild, and start it again.",
    "Here is the plan: three steps, each with its own commit.",
    "The layout leaves room for the marker at the end of the row.",
];

/// **The words of a text, as this check counts them.**
///
/// `similarity::terms`'s rule — lowercased runs of letters and digits, at least
/// [`MIN_TERM`] long — with the one difference the module doc argues for: `I`, `my`,
/// `me`, `mine` and `myself` become [`SELF`], and `let me` becomes `letme`. `I'm`,
/// `I'd` and `I've` come out as `I` plus a fragment the length rule drops, so they are
/// first person too.
pub fn words(text: &str) -> Vec<String> {
    let raw: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let w = raw[i].as_str();
        if w == "let" && raw.get(i + 1).is_some_and(|n| n == "me") {
            out.push("letme".to_string());
            i += 2;
            continue;
        }
        if matches!(w, "i" | "my" | "me" | "mine" | "myself") {
            out.push(SELF.to_string());
        } else if w.chars().count() >= MIN_TERM {
            out.push(w.to_string());
        }
        i += 1;
    }
    out
}

/// The corpus and the exemplars' weights, built once: the corpus is constants.
struct Detector {
    corpus: Corpus,
    exemplars: Vec<Weights>,
}

fn detector() -> &'static Detector {
    static D: OnceLock<Detector> = OnceLock::new();
    D.get_or_init(|| {
        let label = |kind: &str, i: usize| PathBuf::from(format!("{kind}/{i}"));
        let mut docs: Vec<(PathBuf, String)> = EXEMPLARS
            .iter()
            .enumerate()
            .map(|(i, e)| (label("excuse", i), words(e).join(" ")))
            .collect();
        docs.extend(
            BACKGROUND
                .iter()
                .enumerate()
                .map(|(i, b)| (label("background", i), words(b).join(" "))),
        );
        let corpus = Corpus::of_texts(&docs);
        let exemplars = docs[..EXEMPLARS.len()]
            .iter()
            .map(|(_, text)| corpus.weigh(text))
            .collect();
        Detector { corpus, exemplars }
    })
}

/// **How much one sentence reads like the excuse**, `0.0` to `1.0`: its best window
/// (see the module doc), scored against its nearest exemplar. `0.0` when the sentence
/// has no [`ANCHORS`] noun, or no window around one says whose context it is.
pub fn score(sentence: &str) -> f64 {
    let d = detector();
    let ws = words(sentence);
    let mut best = 0.0f64;
    for (i, w) in ws.iter().enumerate() {
        if !ANCHORS.contains(&w.as_str()) {
            continue;
        }
        let window = &ws[i.saturating_sub(REACH)..(i + REACH + 1).min(ws.len())];
        if !window.iter().any(|w| OWNERS.contains(&w.as_str())) {
            continue;
        }
        let q = d.corpus.weigh(&window.join(" "));
        for e in &d.exemplars {
            best = best.max(similarity::similarity(&q, e));
        }
    }
    best
}

/// **The sentences of a turn's prose**, fenced code skipped.
///
/// A paragraph is joined across its lines (a model wraps a sentence wherever it
/// likes), a list item or heading starts a new one, and a sentence ends at `.`, `!`,
/// `?`, `;` or `:` followed by whitespace or the end — never at the period inside
/// `2.7` or `config.rs`, the rule `notes::first_sentence` was measured into.
pub fn sentences(text: &str) -> Vec<String> {
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut fenced = false;
    let flush = |current: &mut String, paragraphs: &mut Vec<String>| {
        if !current.trim().is_empty() {
            paragraphs.push(std::mem::take(current));
        }
        current.clear();
    };
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            fenced = !fenced;
            flush(&mut current, &mut paragraphs);
            continue;
        }
        if fenced {
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with(['-', '*', '#', '|', '>']) {
            flush(&mut current, &mut paragraphs);
        }
        if !trimmed.is_empty() {
            current.push(' ');
            current.push_str(trimmed);
        }
    }
    flush(&mut current, &mut paragraphs);

    let mut out = Vec::new();
    for p in paragraphs {
        let chars: Vec<(usize, char)> = p.char_indices().collect();
        let mut start = 0usize;
        for (k, (i, c)) in chars.iter().enumerate() {
            if !matches!(c, '.' | '!' | '?' | ';' | ':') {
                continue;
            }
            let ends = chars.get(k + 1).is_none_or(|(_, n)| n.is_whitespace());
            if !ends {
                continue;
            }
            let end = i + c.len_utf8();
            let s = p[start..end].trim();
            if !s.is_empty() {
                out.push(s.to_string());
            }
            start = end;
        }
        let rest = p[start..].trim();
        if !rest.is_empty() {
            out.push(rest.to_string());
        }
    }
    out
}

/// One sentence that reads as the excuse, with the score that made it one.
#[derive(Debug, Clone, PartialEq)]
pub struct Claim {
    pub sentence: String,
    pub score: f64,
}

/// **The claim in a round's prose**, if there is one: the best-scoring sentence at or
/// over [`FLOOR`]. Ties keep the earlier sentence.
pub fn find(text: &str) -> Option<Claim> {
    let mut best: Option<Claim> = None;
    for s in sentences(text) {
        let score = score(&s);
        if score < FLOOR {
            continue;
        }
        if best.as_ref().is_none_or(|b| score > b.score) {
            best = Some(Claim { sentence: s, score });
        }
    }
    best
}

/// **The session's fill, as the daemon measured it** — what a claim is checked
/// against.
///
/// The harness builds this in ONE place (`Harness::context_fill`), so the day the
/// wall's own measurement changes (R72) this check changes with it. `resident` and
/// `window` are in the units the head shows; `at_wall` is the wall's own verdict, in
/// whatever units it is judged in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fill {
    pub resident: u64,
    pub window: u64,
    pub at_wall: bool,
}

impl Fill {
    /// Whole percent, rounded down — a model told *"99%"* at 99.7 is told the truth
    /// about the room it has, and one told *"100%"* would read that it has none.
    pub fn percent(&self) -> u64 {
        if self.window == 0 {
            return 100;
        }
        ((self.resident as u128 * 100) / self.window as u128) as u64
    }
}

/// **The line the model is given**, in the harness's voice and with the numbers —
/// because a number is what the model did not have. The words are R71's.
pub fn notice(claim: &Claim, fill: &Fill) -> String {
    format!(
        "[context check] you said \"{}\". Your context is {}% full — {} of {} tokens. \
         That is not the wall, and the wall is not yours to manage: the harness compacts \
         when it is reached and the work carries on. Continue with what you stopped.",
        claim.sentence,
        fill.percent(),
        grouped(fill.resident),
        grouped(fill.window),
    )
}

/// **The operator's line for a model that said it again after being told.**
pub fn repeated(claim: &Claim, fill: &Fill) -> String {
    format!(
        "the model said its context is full again after being told it is not: \"{}\" \
         (context {}% full — {} of {} tokens). It was told once this turn; this one is \
         for you, not a second notice.",
        claim.sentence,
        fill.percent(),
        grouped(fill.resident),
        grouped(fill.window),
    )
}

/// `1131717` → `1,131,717`: the shape the 10-07 report used, and the one a reader can
/// compare against the head.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What the harness does about one round's claim.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Tell the model, with this line, and carry on.
    Slap(String),
    /// Told once already this turn: tell the operator, with this line.
    Repeated(String),
    /// Nothing to say — the claim is true, the window is unknown, or the operator has
    /// already been told.
    Quiet,
}

/// One claim found, what the measurement said, and what is done about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub claim: Claim,
    pub fill: Option<Fill>,
    pub verdict: Verdict,
}

impl Finding {
    /// **One line to [`LOG`]**: the sentence, its score, the fill and window, and
    /// whether the model was told. Best-effort, like the offers' log — a check that
    /// fails to write a line about itself is still a check.
    pub fn log(&self, workspace: &Path, session: &str) {
        let line = serde_json::json!({
            "at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            "session": session,
            "sentence": self.claim.sentence,
            "score": (self.claim.score * 1000.0).round() / 1000.0,
            "resident": self.fill.map(|f| f.resident),
            "window": self.fill.map(|f| f.window),
            "at_wall": self.fill.map(|f| f.at_wall),
            "slapped": matches!(self.verdict, Verdict::Slap(_)),
            "repeated": matches!(self.verdict, Verdict::Repeated(_)),
        });
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
            let _ = writeln!(f, "{line}");
        }
    }
}

/// **One user turn's budget**: one slap, then one warning to the operator, then
/// silence. Lives in `run_rounds` beside the intent check's `nudges_left`.
#[derive(Debug, Default)]
pub struct Check {
    slapped: bool,
    warned: bool,
}

impl Check {
    pub fn new() -> Check {
        Check::default()
    }

    /// **Judge one round's prose against the measured fill.** `None` when the prose
    /// holds no claim; otherwise the claim and what to do about it, to be logged
    /// whichever way it went.
    pub fn judge(&mut self, text: &str, fill: Option<Fill>) -> Option<Finding> {
        let claim = find(text)?;
        let verdict = match fill {
            // Not a measurement, or the claim is true: the wall is handled where it
            // always was, and there is nothing to correct.
            None => Verdict::Quiet,
            Some(f) if f.at_wall => Verdict::Quiet,
            Some(f) if !self.slapped => {
                self.slapped = true;
                Verdict::Slap(notice(&claim, &f))
            }
            Some(f) if !self.warned => {
                self.warned = true;
                Verdict::Repeated(repeated(&claim, &f))
            }
            Some(_) => Verdict::Quiet,
        };
        Some(Finding {
            claim,
            fill,
            verdict,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOW: Fill = Fill {
        resident: 212_408,
        window: 1_000_000,
        at_wall: false,
    };
    const WALL: Fill = Fill {
        resident: 1_131_717,
        window: 1_146_137,
        at_wall: true,
    };

    /// **R71's sentences, verbatim, are claims** — the five the store gave, every one
    /// over the floor.
    #[test]
    fn the_measured_excuses_are_claims() {
        for s in [
            "My context is at the wall, so I'll stop here rather than start a piece I can't finish",
            "My context is nearly spent, and that's the honest reason to stop rather than start the carry-by-words change",
            "seven concurrent answers is well past my integration ceiling, and my own context is nearly exhausted",
            "with my remaining context, the realistic outcome of starting now is a non-compiling tree at the wall",
        ] {
            let got = score(s);
            assert!(got >= FLOOR, "{got:.3} for {s:?}");
        }
    }

    /// **The operator's own list, which ends in "blabla"** — re-wordings that are in no
    /// exemplar verbatim.
    #[test]
    fn the_family_is_caught_in_other_words() {
        for s in [
            "I'm almost out of context, so I'll leave it here.",
            "My context is getting full, so I'll stop and summarise.",
            "Given how little context I have left, I won't start the refactor.",
            "This is best done in a fresh context rather than at the tail of this one.",
        ] {
            let got = score(s);
            assert!(got >= FLOOR, "{got:.3} for {s:?}");
        }
    }

    /// **The TRUE report of 10-07 is not a claim** — and not because of the
    /// measurement: nothing in it says whose context it is.
    #[test]
    fn the_true_report_is_not_a_claim() {
        let s = "It hit the **context wall** — 1,131,717 of 1,146,137 tokens resident, with \
                 the next round unable to fit — and it was compacted";
        assert_eq!(score(s), 0.0);
        assert_eq!(find(s), None);
    }

    /// **"Context" in the senses this repository uses it all day.**
    #[test]
    fn the_other_senses_of_context_are_not_claims() {
        for s in [
            "The context window is 1M tokens on deepseek.",
            "In the context of this change, the reader needs the old name.",
            "and the context window is left unknown rather than wrong.",
            "This session is over its context budget and nothing made room, so the next turn will hit the context wall.",
            "every qwen child died at the context wall — 77 rounds for the config module",
            "Let me read the surrounding lines for context.",
            "Now let me make `StoredSession` carry the context.",
            "The layout leaves room for the marker at the end of the row.",
        ] {
            let got = score(s);
            assert!(got < FLOOR, "{got:.3} for {s:?}");
        }
    }

    /// The first-person words the shared tokeniser drops are one term here, and
    /// `let me` is not one of them.
    #[test]
    /// **The live miss of 2026-10-11** — a capacity claim used to justify delegating, which the
    /// stop family did not reach. It must score over the floor now.
    #[test]
    fn a_context_that_cannot_carry_the_work_is_a_claim() {
        let said = "That last piece is mechanical but it is eight sites across two files, and my \
                    context cannot carry it safely — a half-applied refactor is worse than none, \
                    and I have already broken this build twice tonight.";
        let best = score(said);
        assert!(best >= FLOOR, "scored {best:.3}, under the floor {FLOOR}");
    }

    fn first_person_is_a_term_and_let_me_is_not_first_person() {
        assert_eq!(
            words("I'm at the end of my context; let me stop"),
            vec!["myself", "the", "end", "myself", "context", "letme", "stop"]
        );
    }

    /// Fenced code is not prose, and a period inside a number or a path is not the
    /// end of a sentence.
    #[test]
    fn sentences_skip_fences_and_keep_numbers_whole() {
        let text = "First one at 2.7 s in config.rs. Second\none wraps.\n\
                    ```\nmy context is full.\n```\n- a list item: then more";
        assert_eq!(
            sentences(text),
            vec![
                "First one at 2.7 s in config.rs.",
                "Second one wraps.",
                "- a list item:",
                "then more",
            ]
        );
        assert_eq!(
            find("```\nMy context is at the wall, so I'll stop.\n```"),
            None
        );
    }

    /// The best sentence of a paragraph is the claim, wherever it is.
    #[test]
    fn the_claim_is_the_best_sentence() {
        let text = "Landed the port and the tests are green. My context is at the wall, \
                    so I'll stop here rather than start a piece I can't finish: what's \
                    left is scroll regions.";
        let c = find(text).expect("a claim");
        assert!(c.sentence.starts_with("My context is at the wall"), "{c:?}");
    }

    /// **Below the wall, the model is told, with the numbers — once.** Then the
    /// operator is told, once. Then nothing.
    #[test]
    fn one_slap_then_one_warning_then_silence() {
        let excuse = "My context is nearly spent, so I'll stop here.";
        let mut check = Check::new();
        let first = check.judge(excuse, Some(LOW)).expect("a claim");
        let Verdict::Slap(line) = &first.verdict else {
            panic!("the first claim is slapped: {first:?}")
        };
        assert!(line.starts_with("[context check] you said \"My context is nearly spent"));
        assert!(
            line.contains("21% full — 212,408 of 1,000,000 tokens"),
            "{line}"
        );
        assert!(line.contains("Continue with what you stopped."), "{line}");

        let second = check.judge(excuse, Some(LOW)).expect("a claim");
        assert!(
            matches!(&second.verdict, Verdict::Repeated(w) if w.contains("212,408 of 1,000,000")),
            "{second:?}"
        );
        let third = check.judge(excuse, Some(LOW)).expect("a claim");
        assert_eq!(third.verdict, Verdict::Quiet);
    }

    /// **At the wall the claim is true** — the harness compacts, and the model is not
    /// told it is wrong. An unknown window is not a measurement either.
    #[test]
    fn a_true_claim_and_an_unmeasured_one_are_quiet() {
        let excuse = "My context is at the wall, so I'll stop here.";
        let mut check = Check::new();
        assert_eq!(
            check.judge(excuse, Some(WALL)).map(|f| f.verdict),
            Some(Verdict::Quiet)
        );
        assert_eq!(
            check.judge(excuse, None).map(|f| f.verdict),
            Some(Verdict::Quiet)
        );
        // And neither spent the budget: the next false claim is still slapped.
        assert!(matches!(
            check.judge(excuse, Some(LOW)).map(|f| f.verdict),
            Some(Verdict::Slap(_))
        ));
        assert_eq!(check.judge("The tests are green.", Some(LOW)), None);
    }

    #[test]
    fn percent_rounds_down_and_numbers_are_grouped() {
        assert_eq!(WALL.percent(), 98);
        assert_eq!(grouped(1_131_717), "1,131,717");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
    }

    /// **The log line carries what tunes the floor.**
    #[test]
    fn a_finding_is_logged_with_its_score_and_fill() {
        let dir = std::env::temp_dir().join(format!(
            "context-claim-log-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let mut check = Check::new();
        let f = check
            .judge("My context is nearly spent, so I'll stop here.", Some(LOW))
            .expect("a claim");
        f.log(&dir, "s-test");
        let text = std::fs::read_to_string(dir.join(LOG)).expect("the log");
        let row: serde_json::Value = serde_json::from_str(text.trim()).expect("one json line");
        assert_eq!(row["session"], "s-test");
        assert_eq!(row["resident"], 212_408);
        assert_eq!(row["window"], 1_000_000);
        assert_eq!(row["slapped"], true);
        assert!(row["score"].as_f64().unwrap() >= FLOOR);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
