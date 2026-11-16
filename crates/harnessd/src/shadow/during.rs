//! The `during` job's trigger and data: drift, skill offers and note recall, checked at a change of
//! subject or every [`EVERY`] tool calls. Only the skill-offer pick acts ([`skill_picks`]); drift
//! and note picks are recorded and do nothing.

use std::collections::BTreeSet;

use letibot_tokencore::store::{TodoItem, TodoStatus};
use letibot_transcript::{Speaker, ToolOutcome, TranscriptItem};

use super::verdict::Shown;

/// Tool calls between drift checks, counted only while a plan exists.
pub const EVERY: usize = 8;

/// The last calls a check is shown.
pub const LAST_CALLS: usize = 6;

/// Most shortlist entries a check is shown: the similarity pre-filter's cut.
pub const SHORTLIST: usize = 8;

/// Of [`SHORTLIST`], the most skills: the rest is left for notes.
pub const SKILLS_SHORT: usize = 5;

/// How much of one call's arguments is shown.
const ARGS_CHARS: usize = 200;

/// What the trigger has seen, so a change of subject is a change and not a repeat.
#[derive(Debug, Default)]
pub struct Clock {
    read: usize,
    calls: usize,
    operator_rows: usize,
    errors: BTreeSet<String>,
    kinds: BTreeSet<String>,
    todos: usize,
    /// Registry item to content version, as last seen. `None` until the first look, which is the
    /// baseline and no change.
    versions: Option<std::collections::BTreeMap<String, String>>,
}

/// What changed in the skill and note registry since the last look: new items and edited ones.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Changed {
    pub added: Vec<String>,
    pub edited: Vec<String>,
}

impl Changed {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.edited.is_empty()
    }

    /// The shadow session's row for it, e.g. `registry: +skill X, ~note Y`.
    pub fn row(&self) -> String {
        let name = |n: &str| match n.strip_prefix("note:") {
            Some(note) => format!("note {note}"),
            None => format!("skill {n}"),
        };
        let parts: Vec<String> = self
            .added
            .iter()
            .map(|n| format!("+{}", name(n)))
            .chain(self.edited.iter().map(|n| format!("~{}", name(n))))
            .collect();
        format!("registry: {}", parts.join(", "))
    }

    pub fn names(&self) -> BTreeSet<String> {
        self.added.iter().chain(&self.edited).cloned().collect()
    }
}

impl Clock {
    /// New or edited registry items since the last look, by content version.
    pub fn registry(&mut self, now: std::collections::BTreeMap<String, String>) -> Changed {
        let Some(was) = self.versions.replace(now.clone()) else {
            return Changed::default();
        };
        let mut c = Changed::default();
        for (name, v) in now {
            match was.get(&name) {
                None => c.added.push(name),
                Some(old) if *old != v => c.edited.push(name),
                Some(_) => {}
            }
        }
        c
    }

    /// Why a check is due after the rows up to `items.len()`, or `None`. Reads only the rows it
    /// has not read, so a long session costs the round's rows.
    pub fn due(&mut self, items: &[TranscriptItem], todos: &[TodoItem]) -> Option<String> {
        let mut why: Option<String> = None;
        let say = |why: &mut Option<String>, w: String| {
            why.get_or_insert(w);
        };
        for item in items.iter().skip(self.read) {
            match item {
                TranscriptItem::User {
                    speaker: Speaker::Operator,
                    ..
                } => {
                    self.operator_rows += 1;
                    self.calls = 0;
                    say(&mut why, "a new operator prompt".into());
                }
                TranscriptItem::User {
                    speaker: Speaker::Peer { from },
                    ..
                } => {
                    self.calls = 0;
                    say(&mut why, format!("a new message from {from}"));
                }
                TranscriptItem::Assistant { tool_calls, .. } => {
                    for c in tool_calls {
                        self.calls += 1;
                        if let Some(kind) = file_kind(&c.arguments)
                            && self.kinds.insert(kind.clone())
                        {
                            say(&mut why, format!("a new file kind, .{kind}"));
                        }
                    }
                }
                TranscriptItem::ToolResult {
                    name,
                    outcome: ToolOutcome::Failed { .. },
                    ..
                } if self.errors.insert(name.clone()) => {
                    say(&mut why, format!("a new error class, `{name}` failed"));
                }
                _ => {}
            }
        }
        self.read = items.len();
        let open = open_todos(todos).len();
        if open > self.todos {
            say(&mut why, "a new todo".into());
        }
        self.todos = open;
        let planned = open > 0 || announced(items);
        if why.is_none() && planned && self.calls >= EVERY {
            say(&mut why, format!("{EVERY} tool calls"));
        }
        if why.is_some() {
            self.calls = 0;
        }
        why
    }
}

fn open_todos(todos: &[TodoItem]) -> Vec<&TodoItem> {
    todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::Pending | TodoStatus::InProgress))
        .collect()
}

/// The last assistant text that announced work, which is the stated plan.
fn plan_of(items: &[TranscriptItem]) -> Option<&str> {
    items.iter().rev().find_map(|i| match i {
        TranscriptItem::Assistant { text, .. }
            if !letibot_tools::builtins::intent::commitments(text).is_empty() =>
        {
            Some(text.as_str())
        }
        _ => None,
    })
}

fn announced(items: &[TranscriptItem]) -> bool {
    plan_of(items).is_some()
}

/// The extension of a path argument, the cheapest sign the work moved to another kind of file.
fn file_kind(arguments: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(arguments).ok()?;
    let p = ["path", "file_path", "filePath"]
        .iter()
        .find_map(|k| v.get(*k).and_then(|p| p.as_str()))?;
    let ext = std::path::Path::new(p).extension()?.to_str()?;
    (ext.len() <= 8).then(|| ext.to_ascii_lowercase())
}

/// `s` cut to `n` characters, marked when cut.
pub fn cut(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((at, _)) => format!("{}…", &s[..at]),
        None => s.to_string(),
    }
}

/// The head a policy skill's shortlist line starts with; the prompt reads it as required.
pub const POLICY_HEAD: &str = "POLICY (box rules, required when it applies)";

/// A skill's line on the shortlist. A policy skill says so first, or the check weighs standing
/// rules as one more optional item.
pub fn about_skill(s: &letibot_tools::builtins::skill::Skill) -> String {
    let about = s.when_to_use.clone().unwrap_or(s.description.clone());
    match s.policy {
        true => format!("{POLICY_HEAD}: {about}"),
        false => about,
    }
}

/// A shortlist entry: a skill or a note, by the name an offer would use, with one line about it.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub about: String,
}

/// Cut `entries` to the ones sharing a word with `window`, best first: the pre-filter, not the pick.
pub fn shortlist(entries: &[Entry], window: &str, skip: &BTreeSet<String>) -> Vec<Entry> {
    let words: BTreeSet<String> = window
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4)
        .map(str::to_lowercase)
        .collect();
    let mut scored: Vec<(usize, &Entry)> = entries
        .iter()
        .filter(|e| !skip.contains(&e.name))
        .map(|e| {
            let hits = format!("{} {}", e.name, e.about)
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.len() >= 4 && words.contains(&w.to_lowercase()))
                .count();
            (hits, e)
        })
        .filter(|(h, _)| *h > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
    scored
        .into_iter()
        .take(SHORTLIST)
        .map(|(_, e)| e.clone())
        .collect()
}

/// The pre-filter: `ranked` skills (the offers' blended score, embedding plus lexical) up to
/// [`SKILLS_SHORT`], then notes by shared words. Names in `skip` are left out.
pub fn shortlist_ranked(
    ranked: &[(String, f64)],
    entries: &[Entry],
    window: &str,
    skip: &BTreeSet<String>,
) -> Vec<Entry> {
    let mut out: Vec<Entry> = ranked
        .iter()
        .filter(|(n, _)| !skip.contains(n))
        .filter_map(|(n, _)| entries.iter().find(|e| &e.name == n).cloned())
        .take(SKILLS_SHORT)
        .collect();
    let notes: Vec<Entry> = entries
        .iter()
        .filter(|e| e.name.starts_with("note:"))
        .cloned()
        .collect();
    let room = SHORTLIST - out.len();
    out.extend(shortlist(&notes, window, skip).into_iter().take(room));
    out
}

/// Whether a new request came after the row the verdict judged at `position`, counted as the shadow
/// counts (`words::moves_position`). The operator's message or an agent's line is a new request; a
/// pick for an older one is stale.
pub fn operator_since(items: &[TranscriptItem], position: u64) -> bool {
    let mut n = 0u64;
    for i in items {
        if !super::words::moves_position(i) {
            continue;
        }
        n += 1;
        let asked = matches!(i, TranscriptItem::User { .. });
        if asked && n > position {
            return true;
        }
    }
    false
}

/// The skill picks a during outcome acts on, or why it acts on none. A timeout, a void reply, an
/// `unsure` verdict or a stale one is no offer; note picks are observe-only and never offered.
pub fn skill_picks(
    outcome: &super::runner::Outcome,
    stale: bool,
) -> Result<Vec<super::verdict::Pick>, String> {
    use super::runner::Outcome;
    use super::verdict::{Drift, Verdict};
    let Outcome::Verdict(Verdict::During {
        drift,
        offers,
        strict,
    }) = outcome
    else {
        return Err("no verdict".into());
    };
    let skills: Vec<_> = offers
        .iter()
        .filter(|p| !p.name.starts_with("note:"))
        .take(super::verdict::MAX_OFFERS)
        .cloned()
        .collect();
    if skills.is_empty() {
        return Err("no skill picked".into());
    }
    if *drift == Drift::Unsure {
        return Err("the verdict is unsure".into());
    }
    if stale {
        return Err("stale: the operator spoke after the judged row".into());
    }
    // A stateless check has no spans, so a quote could be a tool's output: recorded, never acted on.
    if !strict {
        return Err("not strict: a stateless check's offers do not act".into());
    }
    Ok(skills)
}

/// The data a `during` check is shown: the stated plan, the open todos, the last calls, and the
/// shortlist with what this session already loaded or declined.
pub fn shown(
    items: &[TranscriptItem],
    todos: &[TodoItem],
    shortlist: &[Entry],
    loaded: &BTreeSet<String>,
) -> Shown {
    let mut head = Shown::default();
    super::turn_end::push_words(&mut head, &super::words::recent(items));
    let mut d = std::mem::take(&mut head.data);
    d.push_str("stated plan:\n");
    d.push_str(plan_of(items).unwrap_or("(none)").trim());
    d.push_str("\n\nopen todos:\n");
    let open = open_todos(todos);
    if open.is_empty() {
        d.push_str("(none)\n");
    }
    for t in open {
        d.push_str(&format!("- {}\n", t.content.trim()));
    }
    d.push_str("\nlast tool calls, oldest first:\n");
    let calls: Vec<String> = items
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::Assistant { tool_calls, .. } => Some(tool_calls),
            _ => None,
        })
        .flatten()
        .map(|c| format!("- {} {}", c.name, cut(&c.arguments, ARGS_CHARS)))
        .collect();
    for c in &calls[calls.len().saturating_sub(LAST_CALLS)..] {
        d.push_str(c);
        d.push('\n');
    }
    d.push_str("\nshortlist:\n");
    if shortlist.is_empty() {
        d.push_str("(none)\n");
    }
    for e in shortlist {
        d.push_str(&format!("- {}: {}\n", e.name, cut(&e.about, 160)));
    }
    if !loaded.is_empty() {
        d.push_str("\nalready loaded or declined this session: ");
        d.push_str(&loaded.iter().cloned().collect::<Vec<_>>().join(", "));
        d.push('\n');
    }
    Shown {
        data: d,
        shortlist: shortlist.iter().map(|e| e.name.clone()).collect(),
        call: None,
        spans: head.spans,
        ..Shown::default()
    }
}

/// The persistent shadow's question at a during trigger. The plan and the calls are rows it was
/// already fed; the question adds what is not in the transcript: the trigger, todos, shortlist.
pub fn question(
    why: &str,
    todos: &[TodoItem],
    shortlist: &[Entry],
    loaded: &BTreeSet<String>,
) -> super::conversation::Question {
    let mut d = format!("triggered by: {why}\n\nopen todos:\n");
    let open = open_todos(todos);
    if open.is_empty() {
        d.push_str("(none)\n");
    }
    for t in open {
        d.push_str(&format!("- {}\n", t.content.trim()));
    }
    d.push_str("\nshortlist:\n");
    if shortlist.is_empty() {
        d.push_str("(none)\n");
    }
    for e in shortlist {
        d.push_str(&format!("- {}: {}\n", e.name, cut(&e.about, 160)));
    }
    if !loaded.is_empty() {
        d.push_str("\nalready loaded or declined this session: ");
        d.push_str(&loaded.iter().cloned().collect::<Vec<_>>().join(", "));
        d.push('\n');
    }
    d.push_str(
        "\nThe stated plan is the assistant's last announcement above, and the last tool calls \
         are the newest `tool call` rows.\n",
    );
    super::conversation::Question {
        job: super::Job::During,
        facts: d,
        turn_from: None,
        rules: Vec::new(),
    }
}
