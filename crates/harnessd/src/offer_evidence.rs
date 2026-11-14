//! What the skills and notes offers share beyond the score: the session's own words, which an
//! offer earned by meaning must find its document's name in, and the project's declines, which
//! raise the bar for a document the model keeps passing over.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use letibot_tools::similarity::Seen;
use letibot_transcript::{Speaker, TranscriptItem, UserPart};

/// Rounds an offer has to be loaded in, its own included, before it counts as declined.
pub const DECLINE_ROUNDS: u32 = 3;

/// How much each earlier decline in this project adds to a document's floor.
pub const DECLINE_STEP: f64 = 0.05;

/// The project's decline log, beside nothing else: one row per declined offer, any session.
pub const DECLINES: &str = "offer-declines.jsonl";

/// Arguments that name what a session touched. A command line or a message is free text the model
/// wrote, and a stray word in it is not the session's subject.
const TOUCHED: &[&str] = &[
    "path",
    "paths",
    "file",
    "file_path",
    "files",
    "dir",
    "cwd",
    "pattern",
    "glob",
];

/// The session's own words: everything the operator wrote, and the files and patterns its tool
/// calls named. Tool output and the model's prose are left out, since they mention anything.
pub fn seen(items: &[TranscriptItem]) -> Seen {
    let mut texts: Vec<String> = Vec::new();
    for item in items {
        match item {
            TranscriptItem::User { parts, speaker } if *speaker == Speaker::Operator => {
                for p in parts {
                    if let UserPart::Text { text } = p {
                        texts.push(text.clone());
                    }
                }
            }
            TranscriptItem::Assistant { tool_calls, .. } => {
                for call in tool_calls {
                    let Ok(args) = serde_json::from_str::<serde_json::Value>(&call.arguments)
                    else {
                        continue;
                    };
                    for key in TOUCHED {
                        match args.get(key) {
                            Some(serde_json::Value::String(s)) => texts.push(s.clone()),
                            Some(serde_json::Value::Array(a)) => texts
                                .extend(a.iter().filter_map(|v| v.as_str()).map(str::to_string)),
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Seen::of_texts(texts.iter().map(String::as_str))
}

/// An offer the model has not loaded yet, counting down to a decline. `hits` are the identity
/// words that let it through, so a later offer can tell new evidence from the same.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub key: String,
    pub rounds: u32,
    pub hits: Vec<String>,
}

/// The project's declines by document, for one kind of offer.
#[derive(Debug, Default, Clone)]
pub struct Declines {
    log: Option<PathBuf>,
    kind: &'static str,
    counts: BTreeMap<String, u32>,
}

impl Declines {
    /// Read the project's log in `dir`. A missing log is no declines.
    pub fn read(dir: &Path, kind: &'static str) -> Declines {
        let log = dir.join(DECLINES);
        let mut counts = BTreeMap::new();
        if let Ok(text) = std::fs::read_to_string(&log) {
            for row in text
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            {
                if row.get("kind").and_then(|k| k.as_str()) != Some(kind) {
                    continue;
                }
                if let Some(name) = row.get("name").and_then(|n| n.as_str()) {
                    *counts.entry(name.to_string()).or_default() += 1;
                }
            }
        }
        Declines {
            log: Some(log),
            kind,
            counts,
        }
    }

    pub fn count(&self, key: &str) -> u32 {
        self.counts.get(key).copied().unwrap_or(0)
    }

    /// The floor for `key` in this project: `base`, raised by each earlier decline.
    pub fn floor(&self, base: f64, key: &str) -> f64 {
        base + DECLINE_STEP * self.count(key) as f64
    }

    /// Count one more decline and append it to the project's log.
    pub fn record(&mut self, session: &str, key: &str, at: u64) {
        *self.counts.entry(key.to_string()).or_default() += 1;
        let Some(log) = &self.log else {
            return;
        };
        let row = serde_json::json!({"at": at, "session": session, "kind": self.kind, "name": key});
        append(log, &format!("{row}\n"));
    }
}

/// Append to a log, creating its directory. A log that cannot be written loses a row, not a turn.
pub fn append(log: &Path, lines: &str) {
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
    {
        let _ = f.write_all(lines.as_bytes());
    }
}

/// Count the pending offers down by one round. Returns the ones that ran out: the declines.
pub fn tick(pending: &mut Vec<Pending>) -> Vec<Pending> {
    for p in pending.iter_mut() {
        p.rounds = p.rounds.saturating_sub(1);
    }
    let (out, keep): (Vec<Pending>, Vec<Pending>) = pending.drain(..).partition(|p| p.rounds == 0);
    *pending = keep;
    out
}

/// Whether evidence found now is new against the evidence an earlier decline was made on.
pub fn new_evidence(now: &[String], before: &[String]) -> bool {
    now.iter().any(|t| !before.contains(t))
}
