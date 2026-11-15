//! A skill that offers itself, the way a note does: the name composed at the TAIL of the request -
//! never the prefix (a prefix change re-prefills the conversation) and never a row (a row cannot be
//! un-said). Earned by a score floor, at most two, silent when unsure, recomposed each round.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use crate::offer_evidence::{self, DECLINE_ROUNDS, Declined, Declines, Pending};
use letibot_tools::similarity::{Evidence, Seen};

use letibot_tokencore::store::{TodoBy, TodoItem};
use letibot_tools::builtins::skill::SkillRegistry;
use letibot_transcript::{Speaker, ToolCall, TranscriptItem, UserPart};

use letibot_embeddings::Tier;

use crate::shadow::verdict::Pick;

/// The score a skill must reach to be offered, apart from `skill find`'s floor.
/// Set above words merely shared with a frontmatter: once a reader learns to ignore the line, the feature is dead.
pub const OFFER_FLOOR: f64 = 0.18;

/// How many skills one round may name. Two is the cap the operator asked for: a wall of names is a
/// list the model learns to skip, and the offer's whole value is that it is rare.
pub const OFFERS_MAX: usize = 2;

/// How many characters of the subject are scored, taken from the most recent end. A cap and not a
/// budget: the subject is *what is happening now*, and the 200 KB a tool last printed is not.
pub const SUBJECT_CHARS: usize = 8_000;

/// Why a policy skill is skipped by the offers: it is never optional, and arrives as rules.
pub const POLICY: &str = "policy: delivered as rules at its trigger, never offered";

/// The file the take-up log is appended to, in the session's logs directory beside the notes'.
pub const LOG: &str = "skills-offers.jsonl";

/// Which of the two scores earned an offer. The log writes the word, because the ratio is what
/// tunes the floor and a "prompt" offer is evidence the operator asked, not that the round drifted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    Subject,
    Prompt,
    /// The shadow's `during` job picked it, with a verified quote.
    Shadow,
}

impl By {
    pub fn word(self) -> &'static str {
        match self {
            By::Subject => "subject",
            By::Prompt => "prompt",
            By::Shadow => "shadow",
        }
    }
}

/// One skill, offered, with the score that earned it and whether the meaning was part of that
/// score - a line that claimed meaning over a box with no embedder would describe an arithmetic
/// it did not use.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub name: String,
    pub score: f64,
    /// Which score this is: the blended subject, or the operator's own prompt against its keywords.
    pub by: By,
    pub blended: bool,
    /// Set by `settle`: the round loaded it.
    pub taken: bool,
    /// The shadow's quote showing the need, and the shadow session's id, for a [`By::Shadow`] offer.
    pub pick: Option<(String, String)>,
}

impl Offer {
    /// The line the model is given, as a transcript item that is not in the log: the request
    /// assembly renders it after everything the session holds, and the next round composes a fresh
    /// one. It names the skill and the verb that loads it, and it stops there.
    pub fn tail(&self) -> TranscriptItem {
        if let Some((quote, _)) = &self.pick {
            return TranscriptItem::User {
                speaker: Speaker::Agent,
                parts: vec![UserPart::Text {
                    text: format!(
                        "[skill offer - the harness's own line, not the operator's] `{}` looks \
                         relevant to what you are doing now: this session's shadow reviewer \
                         picked it for {quote:?}. Its body is not in this prompt. If it is worth \
                         the context, load it - `skill` action=\"load\", name=\"{}\". This line \
                         is composed for this round and is gone from the next one; the skill, once \
                         loaded, is not.",
                        self.name, self.name
                    ),
                }],
            };
        }
        TranscriptItem::User {
            speaker: Speaker::Agent,
            parts: vec![UserPart::Text {
                text: format!(
                    "[skill offer - the harness's own line, not the operator's] `{}` looks \
                     relevant to what you are doing now (similarity {:.2} of 1.00, {}). Its body \
                     is not in this prompt. If it is worth the context, load it - `skill` \
                     action=\"load\", name=\"{}\". This line is composed for this round and is \
                     gone from the next one; the skill, once loaded, is not.",
                    self.name,
                    self.score,
                    match (self.by, self.blended) {
                        (By::Prompt | By::Shadow, _) =>
                            "lexical over the skill's keywords, which the operator's own prompt named",
                        (By::Subject, true) => "lexical over its frontmatter, and its meaning",
                        (By::Subject, false) =>
                            "lexical over its frontmatter: description, when-to-use and keywords",
                    },
                    self.name
                ),
            }],
        }
    }
}

/// One name's two scores: the subject's, and the operator's prompt's own when the prompt ranked it.
#[derive(Debug, Clone, Copy)]
struct Scored {
    best: f64,
    by: By,
    prompt: Option<f64>,
}

/// The offers this turn has already made, and the ones the round in flight is carrying. Two facts
/// are kept apart on purpose: [`Offers::offered`] is the session's memory (a skill is offered once),
/// and [`Offers::open`] is the round's.
#[derive(Debug, Default)]
pub struct Offers {
    offered: HashSet<String>,
    /// Names whose body this session holds - loaded, declared, or a take-up in the log. Never offered
    /// again: a name whose text is already in the prompt is the one line that cannot help.
    taken: HashSet<String>,
    /// The operator's newest prompt as the last round saw it. The same words next round are the same
    /// request, and asking twice is a nag; different words are new evidence.
    last_prompt: Option<String>,
    open: Vec<Offer>,
    last: Vec<Offer>,
    /// The box's second tier, when it has one: this registry's vectors, and nothing of another
    /// caller's. `None` is the lexical score exactly, which is the default.
    tier: Option<Tier>,
    /// The tools this seat has, lowercased. `None` filters nothing (a fixture, a caller with no seat).
    seat: Option<BTreeSet<String>>,
    /// `[skills] never_offer`: names refused whatever their score.
    never: HashSet<String>,
    /// Skills passed over this session and why, written to the log once each.
    skipped: Vec<(String, String)>,
    skip_logged: HashSet<String>,
    /// The identity words each open offer was let through on.
    open_hits: HashMap<String, (Vec<String>, String)>,
    /// Offers not loaded yet, counting down to a decline.
    pending: Vec<Pending>,
    /// Skills declined this session, with the words and the version each was offered on.
    declined: HashMap<String, Declined>,
    /// Paths this session's `write` and `edit` calls named: a skill it wrote is not news to it.
    wrote: Vec<std::path::PathBuf>,
    /// This project's declines, which raise a skill's floor.
    declines: Declines,
    /// The session's own words. An offer by meaning needs the skill's name among them.
    seen: Seen,
    /// Policy skills the shadow picked this round, with its quote: delivered as rules, not offered.
    required: Vec<(String, String)>,
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

    /// Offer only what this seat can use: `tools` are its tool names, `never` the operator's veto.
    pub fn with_seat<'a>(
        mut self,
        tools: impl IntoIterator<Item = &'a String>,
        never: impl IntoIterator<Item = &'a String>,
    ) -> Offers {
        self.seat = Some(tools.into_iter().map(|t| t.to_ascii_lowercase()).collect());
        self.never = never.into_iter().cloned().collect();
        self
    }

    /// Count declines against this project's log in `dir`.
    pub fn with_project(mut self, dir: &Path) -> Offers {
        self.declines = Declines::read(dir, "skill");
        self
    }

    /// Score this round against the one subject the loop names for every offer.
    pub fn share_round(&self, text: Option<String>) {
        if let Some(tier) = &self.tier {
            tier.set_round(text);
        }
    }

    /// Read the session's own words off the transcript, for this round's evidence.
    pub fn observe(&mut self, items: &[TranscriptItem]) {
        self.seen = offer_evidence::seen(items);
        self.wrote = items
            .iter()
            .filter_map(tool_calls_of)
            .flatten()
            .filter(|c| matches!(c.name.as_str(), "write" | "edit"))
            .filter_map(crate::notes_offer::written_of)
            .collect();
    }

    /// Seed the memory with what this SESSION has already been offered: the log is the memory, so the
    /// fact survives a restart and there is no second copy of it.
    pub fn recall(&mut self, logs: &Path, session: &str) {
        let Ok(text) = std::fs::read_to_string(logs.join(LOG)) else {
            return;
        };
        let mut open: BTreeMap<String, (Vec<String>, String)> = BTreeMap::new();
        for line in text.lines() {
            let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if row.get("skipped").is_some() {
                continue;
            }
            if row.get("session").and_then(|s| s.as_str()) != Some(session) {
                continue;
            }
            if let Some(name) = row.get("skill").and_then(|p| p.as_str()) {
                self.offered.insert(name.to_string());
                let hits = hits_of(&row);
                let version = row
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // A row that says the round loaded it settles the rule for the rest of the session:
                // only an offer nobody took may be earned a second time.
                if row.get("taken").and_then(|t| t.as_bool()) == Some(true) {
                    self.taken.insert(name.to_string());
                    open.remove(name);
                } else if row.get("declined").and_then(|t| t.as_bool()) == Some(true) {
                    self.declined
                        .insert(name.to_string(), Declined { hits, version });
                    open.remove(name);
                } else {
                    open.insert(name.to_string(), (hits, version));
                }
            }
        }
        // An offer the last turn left open still has its rounds to be loaded in.
        self.pending
            .extend(open.into_iter().map(|(key, (hits, version))| Pending {
                key,
                rounds: DECLINE_ROUNDS - 1,
                hits,
                version,
            }));
    }

    /// Skip the skills whose body this session already holds, read off the transcript: a `skill` load
    /// the model made itself put the text in the conversation, and offering it again spends a line to
    /// say nothing. A load is a take-up, so it is never offered again however the words move.
    pub fn skip_loaded(&mut self, items: &[TranscriptItem]) {
        for call in items.iter().filter_map(tool_calls_of).flatten() {
            if let Some(name) = loaded_of(call) {
                self.taken.insert(name);
            }
        }
    }

    /// Skip the skills this session was STARTED with, whose bodies are in its prompt already - the
    /// declared list a `task {skills}` child carries, which no row records.
    pub fn skip_names<'a>(&mut self, names: impl IntoIterator<Item = &'a String>) {
        for name in names {
            self.taken.insert(name.clone());
        }
    }

    /// Compose this round's offers, or decide to say nothing - `false` is the common case.
    ///
    /// `text` is the blended subject the round is doing; `prompt` is the operator's newest row, or
    /// `None` when this session has not been spoken to. A skill is offered when either score passes
    /// [`OFFER_FLOOR`], and a name the session has already been told may be said once more - and only
    /// by a prompt whose words are NEW - because asking again is evidence the subject never is.
    pub fn compose(&mut self, registry: &SkillRegistry, text: &str, prompt: Option<&str>) -> bool {
        self.open.clear();
        let prompt = prompt.map(str::trim).filter(|p| !p.is_empty());
        if text.trim().is_empty() && prompt.is_none() {
            return false;
        }
        // Read before it is remembered: the same words on the next round of this turn are the same
        // request, and re-offering on them would spend a line the operator already answered.
        let fresh = prompt.is_some_and(|p| self.last_prompt.as_deref() != Some(p));
        if let Some(p) = prompt {
            self.last_prompt = Some(p.to_string());
        }
        let mut scored: BTreeMap<String, Scored> = BTreeMap::new();
        if !text.trim().is_empty() {
            for (name, score) in registry.ranked_with(text, OFFERS_MAX * 4, self.tier.as_ref()) {
                scored.insert(
                    name,
                    Scored {
                        best: score,
                        by: By::Subject,
                        prompt: None,
                    },
                );
            }
        }
        if let Some(words) = prompt {
            for (name, score) in registry.ranked_by_keywords(words, OFFERS_MAX * 4) {
                match scored.get_mut(&name) {
                    Some(row) => {
                        row.prompt = Some(score);
                        if score > row.best {
                            row.best = score;
                            row.by = By::Prompt;
                        }
                    }
                    None => {
                        scored.insert(
                            name,
                            Scored {
                                best: score,
                                by: By::Prompt,
                                prompt: Some(score),
                            },
                        );
                    }
                }
            }
        }
        let live = match scored.is_empty() {
            false => registry.live_skills(),
            true => Vec::new(),
        };
        let version_of = |name: &str| match live.iter().find(|s| s.name == name) {
            Some(s) => version_of(s),
            None => offer_evidence::version(name),
        };
        let evidence = match (&self.tier, scored.is_empty()) {
            (Some(_), false) => Some(evidence_of(registry, scored.keys())),
            _ => None,
        };
        let mut kept: Vec<(String, f64, By)> = Vec::new();
        self.open_hits.clear();
        for (name, mut row) in scored {
            // A body in the conversation is never offered; a name said once is said again only when
            // a new prompt's own score earns it, which is the untaken offer the operator asked about.
            let version = version_of(&name);
            let floor = self.declines.floor(OFFER_FLOOR, &name, &version);
            if self.taken.contains(&name) || row.best < floor || self.wrote_it(&live, &name) {
                continue;
            }
            // Meaning finds neighbours of anything the session does, so a subject offer under the
            // tier also needs its name, keywords or an uncommon description word in the operator's
            // words or touched files.
            let mut hits = Vec::new();
            if let (Some(ev), By::Subject) = (&evidence, row.by) {
                match ev.found(&name, &self.seen) {
                    Some(h) => hits = h,
                    None => match row.prompt.filter(|p| *p >= floor) {
                        Some(p) => {
                            row.best = p;
                            row.by = By::Prompt;
                        }
                        None => continue,
                    },
                }
            }
            let asked_again = fresh && row.prompt.is_some_and(|score| score >= floor);
            // A decline holds for the session until the skill is named in new words or edited.
            let renewed = self
                .declined
                .get(&name)
                .is_some_and(|before| before.reopened_by(&hits, &version));
            if self.offered.contains(&name) && !asked_again && !renewed {
                continue;
            }
            self.open_hits.insert(name.clone(), (hits, version));
            let policy = live.iter().any(|s| s.name == name && s.policy);
            let why = if self.never.contains(&name) {
                Some("never_offer".to_string())
            } else if policy {
                // Rules are not optional help: the call hook delivers them at their trigger.
                Some(POLICY.to_string())
            } else {
                match (&self.seat, live.iter().find(|s| s.name == name)) {
                    (Some(seat), Some(skill)) => registry.unmet(skill, seat),
                    _ => None,
                }
            };
            match why {
                Some(why) => {
                    if !self.skipped.iter().any(|(n, _)| *n == name) {
                        self.skipped.push((name, why));
                    }
                }
                None => kept.push((name, row.best, row.by)),
            }
        }
        // Ranked by the higher of the two scores, ties by name so the same round says the same thing.
        kept.sort_by(|(na, sa, _), (nb, sb, _)| {
            sb.partial_cmp(sa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| na.cmp(nb))
        });
        self.open = kept
            .into_iter()
            .take(OFFERS_MAX)
            .map(|(name, score, by)| Offer {
                name,
                score,
                by,
                blended: by == By::Subject && self.tier.is_some(),
                taken: false,
                pick: None,
            })
            .collect();
        !self.open.is_empty()
    }

    /// The shadow's picks as this round's offers, in place of [`Offers::compose`]. Its guardrails
    /// already ran; these are the rules no model may loosen: hidden, vetoed, not for this seat,
    /// loaded, written by this session, or offered once already.
    pub fn adopt(&mut self, registry: &SkillRegistry, picks: &[Pick], shadow: &str) -> bool {
        self.open.clear();
        self.open_hits.clear();
        self.required.clear();
        let live = registry.model_skills();
        for p in picks {
            if self.open.len() >= OFFERS_MAX {
                break;
            }
            let Some(skill) = live.iter().find(|s| s.name == p.name) else {
                continue;
            };
            if self.taken.contains(&p.name)
                || self.offered.contains(&p.name)
                || self.wrote_it(&live, &p.name)
                || self.open.iter().any(|o| o.name == p.name)
            {
                continue;
            }
            // A policy the shadow sees applying is required, so it leaves the offers entirely.
            if skill.policy && !self.never.contains(&p.name) {
                if !self.required.iter().any(|(n, _)| *n == p.name) {
                    self.required.push((p.name.clone(), p.quote.clone()));
                }
                continue;
            }
            let why = match self.never.contains(&p.name) {
                true => Some("never_offer".to_string()),
                false => self
                    .seat
                    .as_ref()
                    .and_then(|seat| registry.unmet(skill, seat)),
            };
            if let Some(why) = why {
                if !self.skipped.iter().any(|(n, _)| *n == p.name) {
                    self.skipped.push((p.name.clone(), why));
                }
                continue;
            }
            self.open_hits
                .insert(p.name.clone(), (Vec::new(), version_of(skill)));
            self.open.push(Offer {
                name: p.name.clone(),
                score: 1.0,
                by: By::Shadow,
                blended: false,
                taken: false,
                pick: Some((p.quote.clone(), shadow.to_string())),
            });
        }
        !self.open.is_empty()
    }

    /// The policy skills the shadow's picks made required this round, each with its quote. The
    /// caller delivers them as rules, once per session.
    pub fn take_required(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.required)
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
        let mut open = std::mem::take(&mut self.open);
        let mut lines = String::new();
        // Earlier offers first: a late load is a take-up, and one never loaded is a decline.
        let loaded = |name: &str| calls.iter().any(|c| loaded_of(c).as_deref() == Some(name));
        let mut waiting = Vec::new();
        for p in std::mem::take(&mut self.pending) {
            if loaded(&p.key) {
                self.taken.insert(p.key.clone());
                lines.push_str(&format!(
                    "{{\"at\":{},\"session\":{},\"skill\":{},\"taken\":true,\"late\":true}}\n",
                    now_secs(),
                    json(session),
                    json(&p.key)
                ));
            } else if !self.taken.contains(&p.key) {
                waiting.push(p);
            }
        }
        self.pending = waiting;
        for p in offer_evidence::tick(&mut self.pending) {
            self.declines
                .record(session, &p.key, &p.version, now_secs());
            lines.push_str(&format!(
                "{{\"at\":{},\"session\":{},\"skill\":{},\"declined\":true,\"hits\":{},\"version\":{}}}\n",
                now_secs(),
                json(session),
                json(&p.key),
                serde_json::json!(p.hits),
                json(&p.version)
            ));
            self.declined.insert(
                p.key,
                Declined {
                    hits: p.hits,
                    version: p.version,
                },
            );
        }
        for (name, why) in std::mem::take(&mut self.skipped) {
            if self.skip_logged.insert(name.clone()) {
                lines.push_str(&format!(
                    "{{\"at\":{},\"session\":{},\"skill\":{},\"skipped\":{}}}\n",
                    now_secs(),
                    json(session),
                    json(&name),
                    json(&why)
                ));
            }
        }
        if open.is_empty() && lines.is_empty() {
            return Vec::new();
        }
        for offer in &mut open {
            let taken = calls
                .iter()
                .any(|c| loaded_of(c).as_deref() == Some(&offer.name));
            offer.taken = taken;
            self.offered.insert(offer.name.clone());
            let (hits, version) = self.open_hits.remove(&offer.name).unwrap_or_default();
            if taken {
                self.taken.insert(offer.name.clone());
            } else {
                self.declined.remove(&offer.name);
                self.pending.push(Pending {
                    key: offer.name.clone(),
                    rounds: DECLINE_ROUNDS - 1,
                    hits: hits.clone(),
                    version: version.clone(),
                });
            }
            lines.push_str(&format!(
                "{{\"at\":{},\"session\":{},\"skill\":{},\"score\":{:.3},\"by\":{},\"taken\":{taken},\"hits\":{},\"version\":{}}}\n",
                now_secs(),
                json(session),
                json(&offer.name),
                offer.score,
                json(offer.by.word()),
                serde_json::json!(hits),
                json(&version)
            ));
        }
        offer_evidence::append(&logs.join(LOG), &lines);
        self.last = open;
        self.last.clone()
    }

    /// Whether this session wrote the skill's `SKILL.md` or a file in its directory.
    fn wrote_it(&self, live: &[letibot_tools::builtins::skill::Skill], name: &str) -> bool {
        let Some(dir) = live
            .iter()
            .find(|s| s.name == name)
            .and_then(|s| s.path.as_deref())
            .and_then(Path::parent)
        else {
            return false;
        };
        // A relative path is the workspace's; its directory part is enough to place it.
        self.wrote.iter().any(|w| match w.is_absolute() {
            true => w.starts_with(dir),
            false => w
                .parent()
                .is_some_and(|p| !p.as_os_str().is_empty() && dir.ends_with(p)),
        })
    }

    /// The offers the round just settled, for a caller that reports on them.
    pub fn last(&self) -> &[Offer] {
        &self.last
    }
}

/// The subject a skill is scored against: plan, the person's latest prompt, and the model's latest text and reasoning, walked from the newest end so [`SUBJECT_CHARS`] keeps what is happening now.
/// A note's offer stops at the prompt; the model's own reasoning can make a skill relevant.
pub fn subject(items: &[TranscriptItem], plan: &[TodoItem]) -> String {
    // The plan first, and out of the walk's budget: a row the model wrote is the work in hand, and a
    // long tool result at the newest end must not take the whole subject and cut it away.
    let mut out = String::new();
    for row in plan.iter().filter(|r| !matches!(r.by, TodoBy::Queue)) {
        out.push_str(&row.content);
        out.push('\n');
    }
    let room = SUBJECT_CHARS.saturating_sub(out.len());
    let mut walked = String::new();
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
                        walked.push_str(text);
                        walked.push('\n');
                    }
                }
                break;
            }
            _ => continue,
        };
        walked.push_str(text);
        walked.push('\n');
        if walked.len() >= room {
            break;
        }
    }
    // The cut lands on a character boundary, never through one - `truncate` panics otherwise and
    // this string is the operator's words plus tool results.
    let cut = room.min(walked.len());
    let cut = (0..=cut)
        .rev()
        .find(|&i| walked.is_char_boundary(i))
        .unwrap_or(0);
    walked.truncate(cut);
    out.push_str(&walked);
    out
}

/// A skill's version: its frontmatter and body, so any edit to `SKILL.md` that a reader sees.
pub fn version_of(skill: &letibot_tools::builtins::skill::Skill) -> String {
    offer_evidence::version(&format!("{}\n{}", skill.document(), skill.body))
}

/// The identity words a log row records.
fn hits_of(row: &serde_json::Value) -> Vec<String> {
    row.get("hits")
        .and_then(|h| h.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Each scored skill's identity - its name, keywords and the description's uncommon words -
/// weighted by rarity across the registry.
fn evidence_of<'a>(registry: &SkillRegistry, names: impl Iterator<Item = &'a String>) -> Evidence {
    let mut docs: Vec<(String, String, String, String)> = registry
        .live_skills()
        .iter()
        .map(|s| {
            let identity = format!("{} {}", s.name, s.keywords.join(" "));
            let described = format!(
                "{} {}",
                s.description,
                s.when_to_use.as_deref().unwrap_or("")
            );
            (s.name.clone(), identity, described, s.document())
        })
        .collect();
    // A shelf entry is scored on its title alone, which is then its identity too.
    for name in names {
        if !docs.iter().any(|(n, _, _, _)| n == name) {
            docs.push((name.clone(), name.clone(), String::new(), name.clone()));
        }
    }
    Evidence::described(
        docs.iter()
            .map(|(k, i, d, f)| (k.as_str(), i.as_str(), d.as_str(), f.as_str())),
    )
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
