//! A stretch of this project's conversation that the prompt no longer carries, offered at the TAIL
//! of the request - never the prefix (a prefix change re-prefills) and never a row. Earned by the
//! operator referring back or by a score over the floor, and silent otherwise: a line learned to be
//! ignored is a feature that is dead.
//!
//! What it points at is `transcript` or `digest`, and the `match` it fills in is a term the row really
//! carries - `transcript`'s `match` is a substring, not a bag of words.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use letibot_embeddings::{Tier, cosine};
use letibot_tokencore::store::RecallUnit;
use letibot_tools::similarity::{self, Corpus};
use letibot_transcript::{Speaker, ToolCall, TranscriptItem, UserPart};

/// The score a unit has to reach when nothing in the operator's words refers back. The notes'
/// measured 0.18 and a little over it, because a unit shares more words than an abstract; the
/// take-up ratio in [`LOG`] is what moves it.
pub const OFFER_FLOOR: f64 = 0.22;

/// The score a unit has to reach when the operator DID refer back. The words carried the pointer,
/// so one shared term is enough - which is what keeps this working on a box with no embedder.
pub const TRIGGER_FLOOR: f64 = 0.06;

/// The phrases that refer back, out of the operator's own text and never a tool result. A closed set
/// matched on word boundaries: these are the words that mean *we have been here before*.
pub const REFER_BACK: &[&str] = &[
    "remember",
    "we did",
    "did we",
    "last time",
    "earlier",
    "before",
    "already",
    "that thing",
    "again",
];

/// How many units the index is read for. A round scores every one, so this is the whole of what a
/// turn pays for recall: the newest units of the newest transcripts.
pub const UNITS_MAX: usize = 300;

/// How many transcripts of one session the scan reads. A chain of compactions is short; the bound is
/// here so a session with hundreds of forks cannot make the index a read of the whole store.
pub const TRANSCRIPTS_MAX: usize = 16;

/// How many sessions of the project the scan reads, newest activity first.
pub const SESSIONS_MAX: usize = 8;

/// How many rows one transcript is read for, so one enormous conversation cannot hold the round.
pub const ROWS_MAX: usize = 4_000;

/// What one unit's text is cut to. The embedder reads [`similarity::EMBED_CHARS`] and drops the
/// rest, and a person's turn plus the first line of the reply is what a unit is about.
pub const UNIT_CHARS: usize = 1_200;

/// How many units one turn embeds into the index. Bounded per turn rather than per round, so a
/// project whose history is not indexed yet is worked through over a few turns instead of stalling
/// one: the index is a cache, and a cache may be cold.
pub const EMBED_MAX: usize = 24;

/// How many terms the hint names as what earned it.
pub const TERMS_MAX: usize = 3;

/// How many turns one term has to match before the hint reaches for `digest`: one turn is a read,
/// and a word that is all over the project is a fold through a subagent.
pub const MANY_TURNS: usize = 3;

/// What of the operator's own question a `digest` hint carries, in bytes. A question is one line.
pub const ASKED_MAX: usize = 200;

/// The file the take-up log is appended to, in the session's logs directory beside the others'.
pub const LOG: &str = "recall-offers.jsonl";

/// The two tools this offer exists for.
pub const TOOLS: [&str; 2] = ["transcript", "digest"];

/// One unit of the past, offered: what the index found, why it earned the line, and what the round
/// did about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    /// The unit's id, `<session>#<seq>`, or empty when the operator's own words were the whole of
    /// it - a project with nothing indexed yet still deserves the pointer.
    pub unit: String,
    pub session: String,
    pub title: String,
    /// When the unit's turn was written: what the hint names instead of "recently".
    pub at_ms: i64,
    pub score: f64,
    /// Whether the meaning was part of the score: a line claiming it over a box with no embedder
    /// would be describing an arithmetic it did not use.
    pub blended: bool,
    /// The operator's phrase that earned the line, when one did.
    pub carried: Option<&'static str>,
    /// The terms the `match` argument names, best first. The first is what the call is given.
    pub matched: Vec<String>,
    /// The operator's own question, for `digest`'s `question`.
    pub asked: String,
    /// `transcript` or `digest`.
    pub tool: &'static str,
    /// Set by `settle`: the round made the call.
    pub taken: bool,
}

impl Offer {
    /// The one line the model is given, as a transcript item that never becomes a row: the request
    /// assembly renders it after everything the session holds, and the next round composes a fresh
    /// one. It names the tool, the `match` to hand it, and where the turn was - and stops there.
    pub fn tail(&self) -> TranscriptItem {
        let first = self.matched.first().map(String::as_str).unwrap_or_default();
        let call = match self.tool {
            "digest" => format!(
                "`digest` with match: \"{first}\", question: \"{}\"",
                self.question()
            ),
            _ => format!("`transcript` with match: \"{first}\", history: true"),
        };
        let terms = self.matched.join(", ");
        let source = match self.unit.is_empty() {
            true => String::new(),
            false => format!(
                ", in session `{}` \"{}\" on {}",
                self.session,
                self.title,
                date_of(self.at_ms)
            ),
        };
        TranscriptItem::User {
            speaker: Speaker::Agent,
            parts: vec![UserPart::Text {
                text: format!(
                    "[recall offer — the harness's own line, not the operator's] this project's past \
                     turns about {terms} are not in this prompt: call {call}{source}. This line is \
                     composed for this round and is gone from the next one; what the call finds is \
                     not."
                ),
            }],
        }
    }

    /// What `digest` is asked: the person's own question when the offer has it, because that is
    /// what they want folded, and the matched terms when it does not.
    fn question(&self) -> String {
        match self.asked.is_empty() {
            false => self.asked.clone(),
            true => format!("what did we say about {}?", self.matched.join(" ")),
        }
    }
}

/// The offers this turn has already made, and the one the round in flight is carrying. Two facts are
/// kept apart on purpose: [`Offers::offered`] is the session's memory (a unit is offered once), and
/// [`Offers::open`] is the round's.
#[derive(Debug, Default)]
pub struct Offers {
    offered: HashSet<String>,
    open: Option<Offer>,
    last: Option<Offer>,
    /// The box's second tier, when it has one: this index's vectors, and nothing of another
    /// caller's. `None` is the lexical score exactly, which is the default.
    tier: Option<Tier>,
    /// The project's past conversation, newest first, read from the index once per turn.
    units: Vec<RecallUnit>,
    /// The vectors the index already holds for this model, by unit id. A unit without one is
    /// scored on its words alone until a build reaches it.
    vectors: BTreeMap<String, Vec<f32>>,
    /// Whether `transcript` is seated: a hint naming a tool the seat has not got spends a round on
    /// a refusal, so an unseated seat hears nothing at all.
    seated: bool,
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

    /// Offer only what this seat can use, by the seat's own tool names.
    pub fn with_tools<'a>(mut self, tools: impl IntoIterator<Item = &'a String>) -> Offers {
        self.seated = tools
            .into_iter()
            .any(|t| t.eq_ignore_ascii_case("transcript"));
        self
    }

    /// Whether the seat this offer was built for can make the call at all.
    pub fn seated(&self) -> bool {
        self.seated
    }

    /// Score this round against the one subject the loop names for every offer.
    pub fn share_round(&self, text: Option<String>) {
        if let Some(tier) = &self.tier {
            tier.set_round(text);
        }
    }

    /// Replace this turn's units with what the index holds. Read once per turn and not per round:
    /// the index changes only when a turn is written, and nothing a round does rewrites the past.
    pub fn units(&mut self, units: Vec<RecallUnit>, vectors: BTreeMap<String, Vec<f32>>) {
        self.units = units;
        self.vectors = vectors;
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
            if let Some(unit) = row.get("unit").and_then(|u| u.as_str()) {
                self.offered.insert(unit.to_string());
            }
        }
    }

    /// Compose this round's offer, or decide to say nothing - `false` is the common case. Two
    /// triggers, either earns it: the operator referred back, or the subject scored over the floor
    /// against a unit of the past that this prompt no longer carries.
    pub fn compose(&mut self, subject: &str, operator: &str) -> bool {
        self.open = None;
        if !self.seated || subject.trim().is_empty() {
            return false;
        }
        let carried = refers_back(operator);
        let asked = one_line(operator, ASKED_MAX);
        let found = match self.best(subject) {
            Some(found) => found,
            // Nothing indexed says anything about the subject. A back-reference still earns the
            // pointer: the index being cold is not an answer, and the person's own words are the
            // best `match` there is.
            None => {
                let matched = subject_terms(subject);
                if carried.is_none() || matched.is_empty() {
                    return false;
                }
                let cold = String::new();
                if self.offered.contains(&cold) {
                    return false;
                }
                self.open = Some(Offer {
                    unit: cold,
                    session: String::new(),
                    title: String::new(),
                    at_ms: 0,
                    score: 0.0,
                    blended: false,
                    carried,
                    matched,
                    asked,
                    tool: "transcript",
                    taken: false,
                });
                return true;
            }
        };
        let floor = match carried.is_some() {
            true => TRIGGER_FLOOR,
            false => OFFER_FLOOR,
        };
        if found.score < floor || self.offered.contains(&found.unit) {
            return false;
        }
        // One turn is a read; a term that is all over the project is a fold through a subagent, so
        // a match this wide reaches for `digest` instead. Counted over the units and not guessed.
        let first = found
            .matched
            .first()
            .map(|t| t.to_lowercase())
            .unwrap_or_default();
        let counted = self
            .units
            .iter()
            .filter(|u| u.text.to_lowercase().contains(&first))
            .count();
        let wide = counted >= MANY_TURNS;
        self.open = Some(Offer {
            unit: found.unit,
            session: found.session,
            title: found.title,
            at_ms: found.at_ms,
            score: found.score,
            blended: found.blended,
            carried,
            matched: found.matched,
            asked,
            tool: match wide {
                true => "digest",
                false => "transcript",
            },
            taken: false,
        });
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
    /// is a `transcript` or a `digest` whose own `match` overlaps the one offered, because nothing
    /// else reaches the past. Logged either way: the ratio is what tunes [`OFFER_FLOOR`].
    pub fn settle(&mut self, logs: &Path, session: &str, calls: &[ToolCall]) -> Option<&Offer> {
        let mut offer = self.open.take()?;
        offer.taken = taken(calls, &offer);
        self.offered.insert(offer.unit.clone());
        let line = format!(
            "{{\"at\":{},\"session\":{},\"unit\":{},\"source\":{},\"tool\":{},\"match\":{},\
             \"score\":{:.3},\"carried\":{},\"taken\":{}}}\n",
            now_secs(),
            json(session),
            json(&offer.unit),
            json(&offer.session),
            json(offer.tool),
            json(
                offer
                    .matched
                    .first()
                    .map(String::as_str)
                    .unwrap_or_default()
            ),
            offer.score,
            offer.carried.is_some(),
            offer.taken
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

    /// The unit this subject is most like, with the score that earned it and the terms the hint
    /// names. The words come first and the meaning second, through one blend for every caller.
    fn best(&self, subject: &str) -> Option<Found> {
        if self.units.is_empty() {
            return None;
        }
        let documents: Vec<(PathBuf, String)> = self
            .units
            .iter()
            .map(|u| (PathBuf::from(&u.unit), u.text.clone()))
            .collect();
        let corpus = Corpus::of_texts(&documents);
        let lexical: BTreeMap<String, f64> = corpus
            .ranked(subject, self.units.len())
            .into_iter()
            .map(|(path, score)| (path.to_string_lossy().into_owned(), score))
            .collect();
        // One embedding for the round's subject, and only when there is a vector to compare it
        // with: a box with no embedder asks nothing and scores the words, which is the point.
        let query = self
            .tier
            .as_ref()
            .filter(|_| !self.vectors.is_empty())
            .and_then(|t| t.query(similarity::bounded(subject), now_ms()).ok());
        let mut best: Option<(&RecallUnit, f64, bool)> = None;
        for unit in &self.units {
            let words = lexical.get(&unit.unit).copied().unwrap_or(0.0);
            let meaning = match (&query, self.vectors.get(&unit.unit)) {
                (Some(query), Some(vector)) => Some(cosine(vector, query)),
                _ => None,
            };
            let score = similarity::blended(words, meaning);
            if score <= 0.0 {
                continue;
            }
            // The newer turn wins a tie: two units that say the same thing are one answer, and the
            // one nearer the work in hand is the one worth naming.
            let better = match best {
                None => true,
                Some((had, was, _)) => score > was || (score == was && unit.at_ms > had.at_ms),
            };
            if better {
                best = Some((unit, score, meaning.is_some()));
            }
        }
        let (unit, score, blended) = best?;
        Some(Found {
            unit: unit.unit.clone(),
            session: unit.session.clone(),
            title: unit.title.clone(),
            at_ms: unit.at_ms,
            score,
            blended,
            matched: matched_terms(&corpus, subject, &unit.text),
        })
    }
}

/// The unit a round's subject found, and what the hint says about it.
struct Found {
    unit: String,
    session: String,
    title: String,
    at_ms: i64,
    score: f64,
    /// Whether a vector was part of the winning score.
    blended: bool,
    matched: Vec<String>,
}

/// The terms the hint names: what the subject and the unit both carry, weighted as the corpus
/// weights them, so `etcd` comes before `the`. A term weighing nothing is in every unit and earned
/// none of the score, and then the subject's own most specific words are what to search for.
pub fn matched_terms(corpus: &Corpus, subject: &str, text: &str) -> Vec<String> {
    let weights = corpus.weigh(subject);
    let words: HashSet<String> = words_of(text);
    let mut shared: Vec<(String, f64, usize)> = Vec::new();
    for (at, term) in similarity::terms(subject).into_iter().enumerate() {
        let weight = weights.weight(&term);
        if weight <= 0.0 || !words.contains(&term) || shared.iter().any(|(t, _, _)| *t == term) {
            continue;
        }
        shared.push((term, weight, at));
    }
    // Heaviest first, and among equals the LATER word: a question ends on what it is about
    // ("…the etcd archives?"), and its last content word is the one to search for.
    shared.sort_by(|(ta, wa, ai), (tb, wb, bi)| {
        wb.partial_cmp(wa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| bi.cmp(ai))
            .then_with(|| ta.cmp(tb))
    });
    let out: Vec<String> = shared.into_iter().map(|(t, _, _)| t).collect();
    match out.is_empty() {
        true => subject_terms(subject),
        false => out.into_iter().take(TERMS_MAX).collect(),
    }
}

/// The subject's own terms, most specific first, for the two cases with no corpus to weigh them: a
/// `match` is a SUBSTRING, so the longest term is the one most likely to appear and the one least
/// likely to match half the project.
pub fn subject_terms(subject: &str) -> Vec<String> {
    let mut terms: Vec<(String, usize)> = Vec::new();
    for (at, term) in similarity::terms(subject).into_iter().enumerate() {
        if terms.iter().any(|(t, _)| *t == term) {
            continue;
        }
        terms.push((term, at));
    }
    terms.sort_by(|(ta, ai), (tb, bi)| {
        tb.chars()
            .count()
            .cmp(&ta.chars().count())
            .then_with(|| bi.cmp(ai))
    });
    terms.into_iter().map(|(t, _)| t).take(TERMS_MAX).collect()
}

/// The phrase in the operator's own words that refers back, if there is one. Boundaries are checked
/// because `again` inside `against` is a different word, and `before` in `before you commit` is the
/// one cost of a closed list.
pub fn refers_back(operator: &str) -> Option<&'static str> {
    let text = operator.to_lowercase();
    REFER_BACK
        .iter()
        .copied()
        .find(|phrase| whole_word(&text, phrase))
}

/// Whether `needle` appears in `hay` as a word or a phrase of words: a letter or a digit on either
/// side makes it something else.
fn whole_word(hay: &str, needle: &str) -> bool {
    let mut from = 0;
    while let Some(at) = hay[from..].find(needle) {
        let start = from + at;
        let end = start + needle.len();
        let before = hay[..start].chars().next_back();
        let after = hay[end..].chars().next();
        if before.is_none_or(|c| !c.is_alphanumeric()) && after.is_none_or(|c| !c.is_alphanumeric())
        {
            return true;
        }
        from = end;
    }
    false
}

/// The words of a text, lowercased, for asking whether a term is in it.
fn words_of(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// The operator's own last words: the newest row the person wrote, and nothing this session
/// appended. The trigger reads this and only this - a tool result saying "before" is not somebody
/// referring back.
pub fn operator_text(items: &[TranscriptItem]) -> String {
    for item in items.iter().rev() {
        let TranscriptItem::User { parts, speaker } = item else {
            continue;
        };
        if *speaker == Speaker::Agent {
            continue;
        }
        let mut out = String::new();
        for part in parts {
            if let UserPart::Text { text } = part {
                out.push_str(text);
                out.push(' ');
            }
        }
        return out;
    }
    String::new()
}

/// One unit of a transcript: a person's turn and the reply it got, as the index keeps them. The
/// reply is the model's own words and not its reasoning or a tool's output: a unit is what was
/// SAID about the work, and the payloads of a turn would swamp it.
///
/// `rows` is `(seq, kind, item_json, at_ms)` in `seq` order, which is what the scan reads. A unit's
/// id names its transcript as well as its seq, because a fork keeps its parent's rows: seq 0 exists
/// in every generation of one session, and a key without the transcript would fold them together.
pub fn units(
    rows: &[(u32, String, String, i64)],
    session: &str,
    transcript: &str,
    title: &str,
) -> Vec<RecallUnit> {
    let mut out: Vec<RecallUnit> = Vec::new();
    let mut open: Option<(u32, i64, String)> = None;
    for (seq, kind, item_json, at_ms) in rows {
        if !matches!(kind.as_str(), "user" | "assistant") {
            continue;
        }
        let Ok(item) = serde_json::from_str::<TranscriptItem>(item_json) else {
            continue;
        };
        match item {
            TranscriptItem::User { parts, speaker } => {
                if speaker == Speaker::Agent {
                    continue;
                }
                if let Some((at, when, text)) = open.take() {
                    out.push(unit_of(at, when, &text, session, transcript, title));
                }
                let mut text = String::new();
                for part in parts {
                    if let UserPart::Text { text: t } = part {
                        text.push_str(&t);
                        text.push(' ');
                    }
                }
                open = Some((*seq, *at_ms, text));
            }
            TranscriptItem::Assistant { text, .. } => {
                if let Some((_, _, said)) = &mut open {
                    said.push_str(&text);
                    said.push(' ');
                }
            }
            _ => continue,
        }
    }
    if let Some((at, when, text)) = open.take() {
        out.push(unit_of(at, when, &text, session, transcript, title));
    }
    out
}

/// One unit, with its text cut to [`UNIT_CHARS`] on a character boundary.
fn unit_of(
    seq: u32,
    at_ms: i64,
    text: &str,
    session: &str,
    transcript: &str,
    title: &str,
) -> RecallUnit {
    RecallUnit {
        unit: format!("{transcript}#{seq}"),
        session: session.to_string(),
        title: title.to_string(),
        at_ms,
        text: cut(text.trim(), UNIT_CHARS),
    }
}

/// The first `most` bytes of a text, cut on a character boundary: `truncate` panics through one, and
/// this text is the person's words.
pub fn cut(text: &str, most: usize) -> String {
    let at = (0..=most.min(text.len()))
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0);
    text[..at].trim().to_string()
}

/// One line of a text: whitespace collapsed, a quote made a quote, cut to `most`. What a `question`
/// and a log field can carry is one line, and an operator's paste is not.
pub fn one_line(text: &str, most: usize) -> String {
    let flat: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('"', "'");
    cut(&flat, most)
}

/// Whether the round made the call the offer named: a `transcript` or a `digest` whose `match`
/// overlaps any term the hint named. An offer that named no unit is the person being pointed at the
/// past with nothing found yet, and then any look back is the take-up.
pub fn taken(calls: &[ToolCall], offer: &Offer) -> bool {
    calls.iter().any(|call| {
        if asked_tool(call).is_none() {
            return false;
        }
        if offer.unit.is_empty() {
            return true;
        }
        let Some(called) = call_match(call) else {
            return false;
        };
        if called.is_empty() {
            return false;
        }
        offer.matched.iter().any(|wanted| {
            let wanted = wanted.to_lowercase();
            !wanted.is_empty() && (called.contains(&wanted) || wanted.contains(&called))
        })
    })
}

/// The tool a call is, when it is one of the two that reach the past.
pub fn asked_tool(call: &ToolCall) -> Option<&str> {
    TOOLS.iter().copied().find(|t| call.name == *t)
}

/// A call's `match` argument, lowercased, for the overlap test.
fn call_match(call: &ToolCall) -> Option<String> {
    let args: serde_json::Value = serde_json::from_str(&call.arguments).ok()?;
    let m = args.get("match").and_then(|v| v.as_str())?;
    Some(m.trim().to_lowercase())
}

/// Seconds since the unix epoch. No date library for one field of one log line.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The tier's clock, as the embeddings cache stamps it.
fn now_ms() -> i64 {
    letibot_metrics::now_ms() as i64
}

/// `YYYY-MM-DD` for a unix millisecond stamp: the hint says when the turn was, and a date is what a
/// person can place. Howard Hinnant's `civil_from_days`, which is the whole of what a calendar is
/// here - a date library for one field of one sentence would be a dependency for the same nine lines.
pub fn date_of(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 to a civil date, proleptic Gregorian.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = match mp < 10 {
        true => mp + 3,
        false => mp - 9,
    } as u32;
    (
        match m <= 2 {
            true => y + 1,
            false => y,
        },
        m,
        d,
    )
}

/// A JSON string, escaped by `serde_json` rather than by hand: a title is usually a plain line, and
/// "usually" is how a log becomes unparseable.
fn json(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// The offer's census, in its own file: these tests read the module's own rules.
#[cfg(test)]
mod tests;
