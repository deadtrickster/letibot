//! A brief the repository already did, said at the tail of the request like the notes and skills: earned by a score floor, once per record, silent when nothing clears it.
//!
//! The corpus is this workspace's recent commit subjects (keyed by short sha), bounded. A settled TODO entry lives in its closing commit's message, so the log covers it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use letibot_embeddings::Tier;
use letibot_tools::similarity::Corpus;
use letibot_transcript::{Speaker, TranscriptItem, UserPart};

/// The score a record must reach to be offered, this caller's own floor.
/// Above one shared word: *stop and check* costs a round and a wrong one costs the line its reader.
pub const OFFER_FLOOR: f64 = 0.35;

/// How many commits of the log are read. A commit subject is one line, so a real project's last
/// two hundred are what "the work as it stands" means; anything older has been summarised by the
/// tree itself.
pub const COMMITS: usize = 200;

/// The file the take-up log is appended to, in the session's logs directory beside the others.
pub const LOG: &str = "done-offers.jsonl";

/// One record: a commit with the score that earned it. `commit` stays so older log rows still parse.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub label: String,
    pub score: f64,
    pub commit: bool,
}

impl Offer {
    /// What a session is keyed by, so one record is offered once however it is spelled out.
    fn key(&self) -> String {
        format!("{}{}", if self.commit { "c:" } else { "s:" }, self.label)
    }

    /// The line the model is given: the request assembly renders it after everything the session
    /// holds, and it names the record and the one command that settles it.
    pub fn tail(&self) -> TranscriptItem {
        let check = format!("`git show {}` says what it changed", self.label);
        TranscriptItem::User {
            speaker: Speaker::Agent,
            parts: vec![UserPart::Text {
                text: format!(
                    "[done offer - the harness's own line, not the operator's] this brief looks \
                     like `{}` already did this (similarity {:.2} of 1.00) - check before \
                     starting: {check}. This line is composed for this round and is gone from the \
                     next one.",
                    self.label, self.score
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
    /// The box's second tier, when it has one: this workspace's records and nothing of another
    /// caller's. `None` is no score at all, which is the default.
    tier: Option<Tier>,
}

impl Offers {
    pub fn new() -> Offers {
        Offers::default()
    }

    /// Score with the box's second tier. Off is `None`, and then this caller says nothing at all:
    /// *the repository already did this* is a claim only a meaning can make.
    pub fn with_tier(mut self, tier: Option<Tier>) -> Offers {
        self.tier = tier;
        self
    }

    /// Score this round against the one subject the loop names for every offer.
    pub fn share_round(&self, text: Option<String>) {
        if let Some(tier) = &self.tier {
            tier.set_round(text);
        }
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
            let (Some(label), Some(commit)) = (
                row.get("record").and_then(|v| v.as_str()),
                row.get("commit").and_then(|v| v.as_bool()),
            ) else {
                continue;
            };
            self.offered.insert(
                Offer {
                    label: label.to_string(),
                    score: 0.0,
                    commit,
                }
                .key(),
            );
        }
    }

    /// Compose this round's offer, or decide to say nothing - `false` is the common case. A tier is
    /// what this caller exists for: with the mode off nothing is scored and nothing is offered, so
    /// a box that never wrote `[embeddings]` sees the request it saw before.
    pub fn compose(&mut self, records: &[(PathBuf, String)], brief: &str) -> bool {
        self.open = None;
        if brief.trim().is_empty() || records.is_empty() {
            return false;
        }
        let Some(tier) = self.tier.as_ref() else {
            return false;
        };
        let corpus = Corpus::of_texts(records);
        // A fill that cannot answer empties the tier, so what is left is the words, and a record
        // the floor had always chosen is still the one offered.
        let _ = corpus.fill(Some(tier));
        let Some((path, score)) = corpus.best_with(brief, Some(tier)) else {
            return false;
        };
        let offer = offer_of(path, score);
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

    /// Settle the round's offer against what the round did, and log both sides. A take-up is any call naming the record (a `git show` of the sha).
    /// Logged either way: the ratio tunes [`OFFER_FLOOR`].
    pub fn settle(
        &mut self,
        logs: &Path,
        session: &str,
        calls: &[letibot_transcript::ToolCall],
    ) -> Option<&Offer> {
        let offer = self.open.take()?;
        let named = offer.label.split_whitespace().next().unwrap_or_default();
        let taken = calls.iter().any(|c| c.arguments.contains(named));
        self.offered.insert(offer.key());
        let line = format!(
            "{{\"at\":{},\"session\":{},\"record\":{},\"commit\":{},\"score\":{:.3},\"taken\":{taken}}}\n",
            now_secs(),
            json(session),
            json(&offer.label),
            offer.commit,
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

/// One record as an offer: the key is a commit's short sha.
fn offer_of(path: &Path, score: f64) -> Offer {
    Offer {
        label: path.to_string_lossy().to_string(),
        score,
        commit: true,
    }
}

/// What a new brief is scored against: the workspace's recent commits. A workspace that is not a
/// repository is an empty corpus and never an error.
pub fn records(workspace: &Path) -> Vec<(PathBuf, String)> {
    commits(workspace)
}

/// The last [`COMMITS`] subjects, keyed by short sha. Bounded rather than timed: a local `git log`
/// of a bounded count is one fork and a page of output, and the daemon asks once a turn.
fn commits(workspace: &Path) -> Vec<(PathBuf, String)> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["log", "-n", &COMMITS.to_string(), "--format=%h%x09%s"])
        .output();
    let Ok(out) = out else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    subjects_of(&String::from_utf8_lossy(&out.stdout))
}

/// The log's text as records: one `sha<TAB>subject` line each, which is the format the call above
/// asks for. Split out so a test needs no repository.
pub fn subjects_of(text: &str) -> Vec<(PathBuf, String)> {
    text.lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(sha, subject)| (PathBuf::from(sha.trim()), subject.trim().to_string()))
        .filter(|(sha, subject)| !sha.as_os_str().is_empty() && !subject.is_empty())
        .collect()
}

/// Seconds since the unix epoch. No date library for one field of one log line.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A JSON string, escaped by `serde_json` rather than by hand: a commit subject is prose.
fn json(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// The census, in its own file, beside the other offers'.
#[cfg(test)]
mod tests;
