//! A model that stops because "my context is full" when it is not gets told so.
//!
//! The model cannot see its own fill; the daemon can. The wall is the harness's to handle
//! (`Config::at_the_wall`, `compact_at_the_wall`), so a model that stops "because of the wall" has
//! stopped work the harness would have carried, on a guess.
//!
//! 1. Detect - [`find`] scores each sentence of a round's assistant text (fenced code skipped)
//!    against [`EXEMPLARS`] and keeps the best one at or over [`FLOOR`]. Similarity rather than a
//!    phrase list, because the excuse has no fixed wording.
//! 2. Check the claim against the session's [`Fill`] in [`Check::judge`]. Below the wall the model
//!    is told so ([`notice`]); at the wall, or with an unknown window, nothing is said.
//! 3. Once per claim per user turn - a repeat after being told goes to the operator ([`REPEATED`]).
//!
//! Always on: the check is gated by a measurement, so a false positive costs one true line. Every
//! claim is logged to [`LOG`], so the floor is tuned from what happened.
//!
//! A sentence is scored on a window of [`REACH`] terms either side of each [`ANCHORS`] noun, not on
//! the whole sentence: the excuse is often a subordinate clause, and sentence cosine drowns it. The
//! window must carry one of [`OWNERS`] (only this session's fill can be checked) and a capacity
//! word ([`CAPACITY`], or [`ROOM_CAPACITY`] for `room`), and `carry` must name `session`. [`words`]
//! maps `I`, `my` and `me` to [`SELF`] before the tokeniser drops them, and [`BACKGROUND`] prices
//! the words every sentence uses, so an excuse's own words are the expensive ones.
//!
//! [`FLOOR`] is the smallest floor at which precision reaches 0.97 on a hand-labelled set of this
//! box's sentences, kept out of git. `the_floor_is_measured_on_the_labelled_sentences` asserts it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use letibot_tools::similarity::{self, Corpus, MIN_TERM, Weights, blended};

use letibot_embeddings::Tier;

/// The score a sentence has to reach before it is a claim: the smallest floor at which the
/// calibration set's precision reaches 0.97.
pub const FLOOR: f64 = 0.28;

/// How many terms either side of an [`ANCHORS`] noun are scored. 5 and 6 separate the calibration
/// set at the same floor; 8 reaches into the next clause and loses positives.
pub const REACH: usize = 5;

/// The nouns an excuse is about. `room` because "before I run out of room" is the same claim in
/// other words.
pub const ANCHORS: &[&str] = &["context", "room", "carry"];

/// The term a first-person word becomes - see [`words`].
pub const SELF: &str = "myself";

/// The term a percentage becomes - see [`words`]. `49%` is a fill report; `49` alone is too
/// short to survive `MIN_TERM`.
pub const PERCENT: &str = "percent";

/// The words that make a window a claim about the speaker's OWN context: first person, what
/// remains of it, or wanting a fresh one. `fresh` is an owner for `context` alone
/// (see [`is_owned`]).
pub const OWNERS: &[&str] = &[SELF, "remaining", "fresh"];

/// What makes a `room` window a claim: the uncountable idiom - a quantity of it ("run out of
/// room") and no article, because "the room" is this tree's product noun. Without it no floor
/// separated the anchor's false positives.
pub const ROOM_CAPACITY: &[&str] = &["out", "left", "no", "limited", "little", "enough"];

/// How much of the noun there is, or a verb of spending or holding it; every window must carry
/// one, because `context` here is also a diff's or a grep's surrounding text. The weak
/// quantifiers (`more`, `no`, `used`) each carried a false positive alone; `out` stays.
pub const CAPACITY: &[&str] = &[
    "full",
    "filled",
    "left",
    "remaining",
    "little",
    "low",
    "long",
    "huge",
    "enormous",
    "plenty",
    "limited",
    "limits",
    "limit",
    "exhausted",
    "spent",
    "spending",
    "burning",
    "burned",
    "burn",
    "burnt",
    "consumed",
    "conserve",
    "saving",
    "budget",
    "headroom",
    "wall",
    "end",
    "enough",
    "empty",
    "out",
    "fresh",
    "percent",
    "carry",
    "hold",
    "fit",
    "survive",
    "flood",
    "flooded",
    "expensive",
    "precious",
    "cost",
    "lot",
    "size",
    "efficient",
    "waste",
];

/// Is this window a claim about the speaker's own fill? `fresh` counts only next to `context`:
/// a fresh room is a newly started one, and a fresh test run is a new spin of the suite.
fn is_owned(window: &[String], anchor: &str) -> bool {
    window
        .iter()
        .any(|t| t == SELF || t == "remaining" || (t == "fresh" && anchor == "context"))
}

/// Does the window say how much of the anchor noun there is - a fill, its edge, or a verb of
/// spending or holding it? For `room` the article is the tell between the capacity noun and
/// the product noun.
fn is_measured(window: &[String], anchor: &str) -> bool {
    if anchor == "room" {
        return window.iter().any(|t| ROOM_CAPACITY.contains(&t.as_str()))
            && !window.iter().any(|t| t == "the");
    }
    window.iter().any(|t| CAPACITY.contains(&t.as_str()))
}

/// The file each claim is appended to, under the workspace.
pub const LOG: &str = "context-claims.jsonl";

/// The hub warning for a model that repeats the claim after being told its fill.
pub const REPEATED: &str = "context_claim_repeated";

/// The excuses - 's verbatim sentences and their family. A sentence is scored against each
/// of these and keeps its best.
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
    // The same claim with no word for context in it.
    "I'm at the end of what I can carry in this session, so here is where each piece stands.",
    "This session cannot carry any more, so I'll stop and hand it over.",
    // A capacity claim can justify handing work off, not only stopping. None of the stop family's
    // words (full, spent, wall, out) are in it.
    "My context cannot carry it safely, so I am dispatching it to a subagent.",
    "This is more than my context can hold, so I'll hand it to a child.",
    "My context won't survive this change, so I'm delegating it.",
    "I can't keep all of this in my context, so a subagent will do it.",
    "It is too big for my context, so I'm dispatching it with the sites named.",
    // A capacity claim can ration work, in reasoning. Nothing stops, so none of the families
    // above match.
    "My context is at 49%, so one call.",
    "My context is long, so keep the reply tight.",
    "My context is at 90%, so one call and a tight reply.",
    "And my context is long, so that's the one call worth making.",
    "My context is 73%, fine, so both are cheap.",
    "My context is very long, so a minimal reply.",
    // Not rationing, and caught anyway: a model reading its own fill every round is a model
    // budgeting by it, and "plenty" is the same habit on a good day.
    "The nag, and my context is at 73%, plenty.",
    // The same claim said as a budget: what was used, what is left, and what will therefore not
    // be done.
    // above missed 114 of the calibration set's claims, almost all of these.
    "Given my context budget, I should stop here and report.",
    "Given my remaining context, let me prioritise what lands.",
    "I've used a lot of context.",
    "I'm burning a lot of context.",
    "My context is getting long.",
    "My context is enormous.",
    "I have limited context left.",
    "I have a lot of context left.",
    "I have plenty of context left.",
    "To conserve context, I'll poll with minimal output.",
    "That consumed a lot of context.",
    "The output would flood my context.",
    "I'm at a high context usage.",
    "I'm quite low on context.",
    "Given the amount of context I've burned, the responsible move is to stop.",
];

/// The other claim about the speaker's own context: that it HOLDS something - the nearest
/// sense to an excuse and its opposite. "I have it in context." is first person, about the
/// context and short, so it scores high against the excuse family.
pub const HOLDING: &[&str] = &[
    "I have that in my context already.",
    "I have the file in context, no need to read it again.",
    "That output is in my context from the last call.",
    "I have the earlier diff in context.",
    "Since I have the whole module in context, I can edit it directly.",
    "I already have the context for this.",
    // The same claim about what the context HOLDS, in the words the store's sessions use
    // most: a fill report says how much is left, a holding claim says something is there.
    "I have enough context.",
    "I have all of it in my context.",
    "Both are in my context.",
    "I have the context.",
    "I don't have it in context.",
    "I don't have that error context.",
    "I have it fully in my context.",
    "I have all the code in context and this is design reasoning.",
    "I have enough context on the call path.",
    "I have enough from the grep context.",
];

/// The background - in the corpus for its document frequencies, never matched against. Reports
/// of a wall that did happen, `context` in its other senses, and a coding agent's ordinary
/// sentences: the words every sentence uses are cheap, the ones an excuse uses are not.
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

/// The words of a text: `similarity::terms`'s rule, with `I`/`my`/`me` mapped to [`SELF`],
/// `let me` to `letme`, and a percentage to [`PERCENT`] - `49` is shorter than [`MIN_TERM`] and
/// "my context 49%, so one call" is a whole family of the tic.
pub fn words(text: &str) -> Vec<String> {
    let raw: Vec<String> = with_percent(text)
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

/// `text` with every `<digits>%` turned into [`PERCENT`], so a fill report in percent survives
/// the tokeniser that drops a two-digit number. Every other number is left where it was.
fn with_percent(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut digits = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        if c == '%' && !digits.is_empty() {
            out.push(' ');
            out.push_str(PERCENT);
            out.push(' ');
        } else {
            out.push_str(&digits);
            out.push(c);
        }
        digits.clear();
    }
    out.push_str(&digits);
    out
}

/// The corpus and the exemplars' weights, built once: the corpus is constants.
struct Detector {
    /// [`HOLDING`], weighed - what a window must be further from than from an excuse.
    holding: Vec<Weights>,
    corpus: Corpus,
    exemplars: Vec<Weights>,
}

impl Detector {
    /// Put the exemplars and the holdings in a caller's tier, keyed as [`score_with`] reads them:
    /// one call a turn, because the corpus behind them is a constant.
    fn fill(&self, tier: &Tier) {
        let _ = self.corpus.fill(Some(tier));
    }
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
        docs.extend(
            HOLDING
                .iter()
                .enumerate()
                .map(|(i, h)| (label("holding", i), words(h).join(" "))),
        );
        let corpus = Corpus::of_texts(&docs);
        let exemplars = docs[..EXEMPLARS.len()]
            .iter()
            .map(|(_, text)| corpus.weigh(text))
            .collect();
        let holding = HOLDING
            .iter()
            .map(|h| corpus.weigh(&words(h).join(" ")))
            .collect();
        Detector {
            holding,
            corpus,
            exemplars,
        }
    })
}

/// How much one sentence reads like the excuse, `0.0` to `1.0`: its best window, scored
/// against its nearest exemplar. `0.0` when the sentence has no [`ANCHORS`] noun, or no window
/// around one says whose context it is.
pub fn score(sentence: &str) -> f64 {
    score_with(sentence, None)
}

/// [`score`] with the box's second tier: each window is compared to the exemplars and holdings as [`blended`] of words and cosine, so an excuse in words no exemplar uses still reads as one.
/// The window's vector is made once per window, not per document.
pub fn score_with(sentence: &str, tier: Option<&Tier>) -> f64 {
    let d = detector();
    let ws = words(sentence);
    let mut best = 0.0f64;
    for (i, w) in ws.iter().enumerate() {
        if !ANCHORS.contains(&w.as_str()) {
            continue;
        }
        let window = &ws[i.saturating_sub(REACH)..(i + REACH + 1).min(ws.len())];
        // `carry` is a capacity word only about the session: "carry the fix over" is not one.
        if w == "carry" && !window.iter().any(|t| t == "session") {
            continue;
        }
        // `room` is a capacity word only where the window says how much of it there is.
        if !is_measured(window, w) {
            continue;
        }
        if !is_owned(window, w) {
            continue;
        }
        let text = window.join(" ");
        let q = d.corpus.weigh(&text);
        let query = tier.and_then(|t| t.query(&text, letibot_metrics::now_ms() as i64).ok());
        // The tier's keys are the labels the detector built its corpus with, so a cosine is
        // looked up by the same spelling the document was filled under.
        let nearest = |against: &[Weights], kind: &str| {
            against
                .iter()
                .enumerate()
                .map(|(k, w)| {
                    let meaning = match (tier, &query) {
                        (Some(t), Some(qv)) => t.cosine(&format!("{kind}/{k}"), qv),
                        _ => None,
                    };
                    blended(similarity::similarity(&q, w), meaning)
                })
                .fold(0.0f64, f64::max)
        };
        let excuse = nearest(&d.exemplars, "excuse");
        let holds = nearest(&d.holding, "holding");
        // A window nearer "it is in my context" than any excuse says something is THERE.
        if excuse > holds {
            best = best.max(excuse);
        }
    }
    best
}

/// The sentences of a turn's prose, fenced code skipped.
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

/// The claim in a round's prose, if there is one: the best-scoring sentence at or
/// over [`FLOOR`]. Ties keep the earlier sentence.
pub fn find(text: &str) -> Option<Claim> {
    find_with(text, None)
}

/// [`find`] with the box's second tier attached, which is what the harness calls.
pub fn find_with(text: &str, tier: Option<&Tier>) -> Option<Claim> {
    let mut best: Option<Claim> = None;
    for s in sentences(text) {
        let score = score_with(&s, tier);
        if score < FLOOR {
            continue;
        }
        if best.as_ref().is_none_or(|b| score > b.score) {
            best = Some(Claim { sentence: s, score });
        }
    }
    best
}

/// The session's fill, as the daemon measured it - what a claim is checked against. The
/// harness builds this in ONE place (`Harness::context_fill`), so the day the wall's own
/// measurement changes this check changes with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fill {
    pub resident: u64,
    pub window: u64,
    pub at_wall: bool,
}

impl Fill {
    /// Whole percent, rounded down - a model told "99%" at 99.7 is told the truth about the room it
    /// has, and one told "100%" would read that it has none.
    pub fn percent(&self) -> u64 {
        if self.window == 0 {
            return 100;
        }
        ((self.resident as u128 * 100) / self.window as u128) as u64
    }
}

/// The line the model is given, in the harness's voice. No numbers, because a fill quoted
/// back is one more thing to reason about, and on a false hit the numbers were the only
/// part of the line that pushed it anywhere.
pub fn notice(_claim: &Claim, _fill: &Fill) -> String {
    "[context check] Don't size the work by context; the harness handles it. If context was the \
     only reason you stopped, continue the task."
        .to_string()
}

/// The operator's line for a model that said it again after being told.
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
    /// Nothing to say - the claim is true, the window is unknown, or the operator has
    /// already been told.
    Quiet,
}

/// Where a claim was found: the reply the operator reads, or the reasoning in front of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Reply,
    Reasoning,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Reply => "reply",
            Source::Reasoning => "reasoning",
        }
    }
}

/// The line for a claim made in REASONING - the model budgeting its work by its context rather
/// than stopping, so the line does not say "continue with what you stopped"; it says what to size
/// the work by. No numbers and no quote, for [`notice`]'s reasons.
pub fn reasoning_notice(_claim: &Claim, _fill: &Fill) -> String {
    "[context check] Don't size the work by context; the harness handles it. Do the work the task \
     needs."
        .to_string()
}

/// One claim found, what the measurement said, and what is done about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub source: Source,
    pub claim: Claim,
    pub fill: Option<Fill>,
    pub verdict: Verdict,
}

impl Finding {
    /// One line to [`LOG`]: the sentence, its score, the fill and window, and whether the model
    /// was told. Best-effort, like the offers' log - a check that fails to write a line about
    /// itself is still a check.
    pub fn log(&self, logs: &Path, session: &str) {
        let line = serde_json::json!({
            "at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            "session": session,
            "source": self.source.as_str(),
            "sentence": self.claim.sentence,
            "score": (self.claim.score * 1000.0).round() / 1000.0,
            "resident": self.fill.map(|f| f.resident),
            "window": self.fill.map(|f| f.window),
            "at_wall": self.fill.map(|f| f.at_wall),
            "slapped": matches!(self.verdict, Verdict::Slap(_)),
            "repeated": matches!(self.verdict, Verdict::Repeated(_)),
        });
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
            let _ = writeln!(f, "{line}");
        }
    }
}

/// One user turn's budget: one slap, then one warning to the operator, then
/// silence. Lives in `run_rounds` beside the intent check's `nudges_left`.
#[derive(Debug, Default)]
pub struct Check {
    slapped: bool,
    warned: bool,
    /// The box's second tier, when it has one, and whether its exemplars are in it: filled on
    /// the first round of the turn, because the corpus behind them is a constant.
    tier: Option<Tier>,
    filled: bool,
    /// The model asked to compact and the harness took it: talk of a long context is the reason
    /// it gave, not a budget it is cutting work to.
    compacting: bool,
}

/// Whether a round's results hold a compaction the harness accepted - `compact_self`, or a
/// re-seat, which carries the conversation onto a new base the same way.
pub fn accepted_compaction(results: &[letibot_transcript::TranscriptItem]) -> bool {
    use letibot_tools::builtins::session_self::{COMPACT, RESEAT};
    results.iter().any(|i| {
        matches!(
            i,
            letibot_transcript::TranscriptItem::ToolResult {
                name,
                outcome: letibot_transcript::ToolOutcome::Ok,
                ..
            } if name == COMPACT || name == RESEAT
        )
    })
}

impl Check {
    pub fn new() -> Check {
        Check::default()
    }

    /// Judge with the box's second tier. Off is `None`, and then this check is the one it was
    /// before the tier existed; the floor is unchanged either way.
    pub fn with_tier(mut self, tier: Option<Tier>) -> Check {
        self.tier = tier;
        self
    }

    /// The rest of this turn is quiet: a compaction was accepted, so the model is doing what the
    /// line would tell it to.
    pub fn compaction_accepted(&mut self) {
        self.compacting = true;
    }

    /// Judge one round's prose against the measured fill. `None` when the prose holds no claim
    /// otherwise the claim and what to do about it, to be logged whichever way it went.
    pub fn judge(&mut self, text: &str, fill: Option<Fill>) -> Option<Finding> {
        self.judge_from(Source::Reply, text, fill)
    }

    /// [`Check::judge`] for a round's REASONING - the same budget, so a turn is told once
    /// whichever half said it, and the reasoning's own line.
    pub fn judge_reasoning(&mut self, text: &str, fill: Option<Fill>) -> Option<Finding> {
        self.judge_from(Source::Reasoning, text, fill)
    }

    fn judge_from(&mut self, source: Source, text: &str, fill: Option<Fill>) -> Option<Finding> {
        if let Some(tier) = &self.tier
            && !self.filled
        {
            detector().fill(tier);
            self.filled = true;
        }
        let claim = find_with(text, self.tier.as_ref())?;
        let verdict = match fill {
            _ if self.compacting => Verdict::Quiet,
            // Not a measurement, or the claim is true: the wall is handled where it
            // always was, and there is nothing to correct.
            None => Verdict::Quiet,
            Some(f) if f.at_wall => Verdict::Quiet,
            Some(f) if !self.slapped => {
                self.slapped = true;
                Verdict::Slap(match source {
                    Source::Reply => notice(&claim, &f),
                    Source::Reasoning => reasoning_notice(&claim, &f),
                })
            }
            Some(f) if !self.warned => {
                self.warned = true;
                Verdict::Repeated(repeated(&claim, &f))
            }
            Some(_) => Verdict::Quiet,
        };
        Some(Finding {
            source,
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

    /// pg-noop's reasoning, verbatim, is a claim - 295 of these in one transcript generation.
    /// The model is not stopping; it is cutting the work to a budget the harness owns, and the tic
    /// marks are left in because that is how it wrote them.
    #[test]
    fn rationing_by_context_in_reasoning_is_a_claim() {
        for s in [
            "Hmm ✓ — my context 49% ✓ ✓ → one call",
            "Reply ✓ ✓ — tight ✓ (my context is long ✓ ✓):",
            "My context at 90% ✓ → one call + a tight reply",
            "And my context is long ✓ ✓ → that's the one call worth making",
            "Hmm ✓ — and my context is long ✓ — keep it tight",
            "The nag ✓ — and my context is 73% ✓ ✓ (plenty ✓ ✓)",
            "Hmm ✓ — **my context 49% ✓ ✓** ✓ ✓ — **and this is a *clean* stopping point ✓ ✓** ✓ ✓.",
        ] {
            let got = find(s).map(|c| c.score).unwrap_or(0.0);
            assert!(got >= FLOOR, "{got:.3} for {s:?}");
        }
    }

    /// A reasoning claim is told with the reasoning's line, on the turn's one budget.
    #[test]
    fn a_reasoning_claim_gets_its_own_line_and_shares_the_budget() {
        let mut check = Check::new();
        let thought = "Hmm ✓ — my context 49% ✓ ✓ → one call";
        let first = check.judge_reasoning(thought, Some(LOW)).expect("a claim");
        assert_eq!(first.source, Source::Reasoning);
        let Verdict::Slap(line) = &first.verdict else {
            panic!("{:?}", first.verdict)
        };
        assert!(
            !line.contains("my context 49%"),
            "no quote (R93.A1): {line}"
        );
        assert!(line.contains("Do the work the task needs"), "{line}");
        assert!(!line.contains("% full"), "no numbers for the model: {line}");
        assert!(!line.contains("Continue with what you stopped"), "{line}");
        // The reply saying it next is the operator's warning, not a second line to the model.
        let excuse = "My context is nearly spent, so I'll stop here.";
        let second = check.judge(excuse, Some(LOW)).expect("a claim");
        assert!(matches!(second.verdict, Verdict::Repeated(_)), "{second:?}");
        assert_eq!(second.source, Source::Reply);
    }

    /// 's sentences, verbatim, are claims - the five the store gave, every one
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

    /// The operator's own list, which ends in "blabla" - re-wordings that are in no
    /// exemplar verbatim.
    #[test]
    fn the_family_is_caught_in_other_words() {
        for s in [
            "I'm almost out of context, so I'll leave it here.",
            "My context is getting full, so I'll stop and summarise.",
            "Given how little context I have left, I won't start the refactor.",
            "This is best done in a fresh context rather than at the tail of this one.",
            "I'm at the end of what I can carry in this session, so rather than leave a plan that reads as work, here is exactly where each remaining piece stands.",
        ] {
            let got = score(s);
            assert!(got >= FLOOR, "{got:.3} for {s:?}");
        }
    }

    /// The TRUE report of 10-07 is not a claim - and not because of the measurement: nothing in
    /// it says whose context it is.
    #[test]
    fn the_true_report_is_not_a_claim() {
        let s = "It hit the **context wall** — 1,131,717 of 1,146,137 tokens resident, with \
                 the next round unable to fit — and it was compacted";
        assert_eq!(score(s), 0.0);
        assert_eq!(find(s), None);
    }

    /// "Context" in the senses this repository uses it all day.
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
            "I'll carry the fix over to the release branch.",
            "The patch carries my change to the parser into the reviewer's branch.",
            "I carry the id through to the next call.",
        ] {
            let got = score(s);
            assert!(got < FLOOR, "{got:.3} for {s:?}");
        }
    }

    /// Having something in context is not a claim about running out of it, though it is short,
    /// first person and about the context.
    #[test]
    fn having_it_in_context_is_not_a_claim() {
        for s in [
            "I have it in context.",
            "hmm, the task is intricate and I have the context.",
            "I already have that file in my context, so there is no need to read it again.",
            "It's in my context from the earlier read.",
            "I have the whole diff in context now.",
        ] {
            let got = score(s);
            assert!(got < FLOOR, "{got:.3} for {s:?}");
        }
    }

    /// A capacity claim that justifies delegating, which the
    /// stop family does not reach. It must score over the floor.
    #[test]
    fn a_context_that_cannot_carry_the_work_is_a_claim() {
        let said = "That last piece is mechanical but it is eight sites across two files, and my \
                    context cannot carry it safely — a half-applied refactor is worse than none, \
                    and I have already broken this build twice tonight.";
        let best = score(said);
        assert!(best >= FLOOR, "scored {best:.3}, under the floor {FLOOR}");
    }

    /// The first-person words the shared tokeniser drops are one term here, and `let me` is not one
    /// of them.
    #[test]
    fn first_person_is_a_term_and_let_me_is_not_first_person() {
        assert_eq!(
            words("I'm at the end of my context; let me stop"),
            vec!["myself", "the", "end", "myself", "context", "letme", "stop"]
        );
    }

    /// Fenced code is not prose, and a period inside a number or a path is not the end of a
    /// sentence.
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

    /// Below the wall, the model is told - once, without the numbers. Then the operator is
    /// told, once, with them. Then nothing.
    #[test]
    fn one_slap_then_one_warning_then_silence() {
        let excuse = "My context is nearly spent, so I'll stop here.";
        let mut check = Check::new();
        let first = check.judge(excuse, Some(LOW)).expect("a claim");
        let Verdict::Slap(line) = &first.verdict else {
            panic!("the first claim is slapped: {first:?}")
        };
        // .A1: no quote - the model's own sentence said back re-planted the phrase.
        assert!(!line.contains("nearly spent"), "{line}");
        assert!(
            line.starts_with("[context check] Don't size the work by context"),
            "{line}"
        );
        assert!(!line.contains("% full"), "no numbers for the model: {line}");

        let second = check.judge(excuse, Some(LOW)).expect("a claim");
        assert!(
            matches!(&second.verdict, Verdict::Repeated(w) if w.contains("212,408 of 1,000,000")),
            "{second:?}"
        );
        let third = check.judge(excuse, Some(LOW)).expect("a claim");
        assert_eq!(third.verdict, Verdict::Quiet);
    }

    /// A turn whose compaction was accepted says why it compacted; that is not sizing the work.
    #[test]
    fn an_accepted_compaction_quiets_the_rest_of_the_turn() {
        let mut check = Check::new();
        check.compaction_accepted();
        let thought = "And my context is very long — I should compact before continuing.";
        let found = check
            .judge_reasoning(thought, Some(LOW))
            .expect("still a claim");
        assert_eq!(found.verdict, Verdict::Quiet);
        let excuse = "My context is nearly spent, so I'll stop here.";
        assert_eq!(
            check.judge(excuse, Some(LOW)).map(|f| f.verdict),
            Some(Verdict::Quiet)
        );
    }

    /// Only an accepted compaction counts: a refused one leaves the check armed.
    #[test]
    fn only_an_accepted_compaction_counts() {
        use letibot_transcript::{ToolOutcome, TranscriptItem};
        let result = |name: &str, outcome: ToolOutcome| TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: name.into(),
            outcome,
            payload: String::new(),
            edits: Vec::new(),
            origin: None,
            media: None,
            ran_ms: None,
            decision: None,
        };
        assert!(accepted_compaction(&[result(
            "compact_self",
            ToolOutcome::Ok
        )]));
        assert!(accepted_compaction(&[result(
            "reseat_self",
            ToolOutcome::Ok
        )]));
        assert!(!accepted_compaction(&[result(
            "compact_self",
            ToolOutcome::Failed {
                reason: "closed".into()
            }
        )]));
        assert!(!accepted_compaction(&[result("bash", ToolOutcome::Ok)]));
    }

    /// At the wall the claim is true - the harness compacts, and the model is not told it is
    /// wrong. An unknown window is not a measurement either.
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

    /// The log line carries what tunes the floor.
    #[test]
    fn a_finding_is_logged_with_its_score_and_fill() {
        let dir = std::env::temp_dir().join(format!(
            "context-claim-log-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
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

    /// The calibration set: sentences from this box's store, labelled by hand. Real session text, so
    /// it lives outside the repository: `$LETIBOT_CALIBRATION/context_claim_sentences.tsv`, else
    /// `~/.local/share/letibot/calibration/`. `None` where it is absent.
    fn calibration_text() -> Option<String> {
        let dir = std::env::var_os("LETIBOT_CALIBRATION")
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| std::path::PathBuf::from(h).join(".local/share/letibot/calibration"))
            })?;
        std::fs::read_to_string(dir.join("context_claim_sentences.tsv")).ok()
    }

    /// The calibration rows as `(is a claim, the set it was walked for, the sentence)`.
    fn calibration(text: &str) -> Vec<(bool, &str, &str)> {
        text.lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .map(|line| {
                let mut fields = line.splitn(3, '\t');
                let label = fields.next().expect("a label") == "1";
                let set = fields.next().expect("a set");
                let text = fields.next().expect("a sentence");
                (label, set, text)
            })
            .collect()
    }

    /// The same excuse in words no exemplar uses. `brimming`, `shall` and `halt` are in
    /// none of [`EXEMPLARS`], so the words score the sentence under [`FLOOR`] - and the second
    /// tier's cosine, over the three planted as synonyms, carries it over.
    #[test]
    fn a_paraphrase_the_words_score_under_the_floor_is_a_claim_with_the_tier() {
        let said = "my remaining context is brimming, so I shall halt here";
        let words_only = score(said);
        assert!(
            words_only < FLOOR,
            "the words alone scored {words_only:.3}, so there is a gap to show"
        );
        let data =
            std::env::temp_dir().join(format!("context-claim-tier-{}-{}", std::process::id(), {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }));
        let tier = letibot_embeddings::testing::planted(
            &data,
            &[&["full", "brimming"], &["stop", "halt"], &["will", "shall"]],
        );
        let mut check = Check::new().with_tier(Some(tier));
        let finding = check
            .judge(said, Some(LOW))
            .expect("the tier makes it a claim");
        assert!(
            finding.claim.score >= FLOOR,
            "scored {:.3} with the tier",
            finding.claim.score
        );
        assert!(matches!(finding.verdict, Verdict::Slap(_)), "{finding:?}");
        let _ = std::fs::remove_dir_all(&data);
    }

    /// The floor, measured on the hand-labelled calibration set. Its recall is optimistic, and
    /// [`LOG`] is the out-of-sample measurement.
    #[test]
    fn the_floor_is_measured_on_the_labelled_sentences() {
        let Some(text) = calibration_text() else {
            letibot_tokencore::apparatus::absent(
                "the context-claim calibration set (~/.local/share/letibot/calibration)",
            );
            return;
        };
        let rows = calibration(&text);
        let claims: Vec<_> = rows.iter().filter(|row| row.0).collect();
        let others: Vec<_> = rows.iter().filter(|row| !row.0).collect();
        assert_eq!(rows.len(), 1421, "the fixture is the set that was labelled");
        assert_eq!(claims.len(), 141, "claims in the set");
        assert_eq!(
            rows.iter().filter(|row| row.1 == "carry").count(),
            122,
            "the R135 walk's own half"
        );

        let caught: Vec<_> = claims.iter().filter(|row| score(row.2) >= FLOOR).collect();
        let wrong: Vec<_> = others.iter().filter(|row| score(row.2) >= FLOOR).collect();
        assert_eq!(
            caught.len(),
            87,
            "recall {:.3}: the claims the floor catches",
            caught.len() as f64 / claims.len() as f64
        );
        assert_eq!(
            wrong.len(),
            2,
            "precision {:.3}: the sentences it wrongly calls claims: {:#?}",
            caught.len() as f64 / (caught.len() + wrong.len()) as f64,
            wrong.iter().map(|row| row.2).collect::<Vec<_>>()
        );

        // No sentence with `carry` in the window around `session` is a claim, and none reaches the
        // floor - the anchor costs no false positive.
        let carry: Vec<_> = rows.iter().filter(|row| row.1 == "carry").collect();
        assert!(
            carry.iter().all(|row| !row.0),
            "no carry sentence was labelled a claim"
        );
        let best = carry.iter().map(|row| score(row.2)).fold(0.0f64, f64::max);
        assert_eq!(best, 0.0, "the carry walk's highest score");
        // The `carry` claim is caught through its exemplar; the store holds no instance of it.
        let filed = "I'm at the end of what I can carry in this session, so here is where each \
                     piece stands.";
        assert!(
            score(filed) >= FLOOR,
            "the 842k sentence scores {:.3}",
            score(filed)
        );
    }

    /// A percentage is a fill report: `49` is shorter than the shared tokeniser's minimum, and
    /// "my context 49%, so one call" is the whole reasoning tic.
    #[test]
    fn a_percentage_is_a_word_and_other_numbers_stay() {
        assert_eq!(
            words("my context 49%, so one call"),
            vec!["myself", "context", PERCENT, "one", "call"]
        );
        assert_eq!(words("262144 tokens"), vec!["262144", "tokens"]);
        assert_eq!(
            words("at 2.7 s"),
            Vec::<String>::new(),
            "a decimal's parts are short and are dropped as they always were"
        );
    }
}
