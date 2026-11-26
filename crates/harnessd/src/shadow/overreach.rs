//! The turn monitor: what a turn did that nobody asked for.
//!
//! Two halves, and the cheap one is not a model. A rule reads the turn's own rows - the files it
//! changed, the commands it ran - and proves the findings it can prove; the turn_end call is then
//! asked, in the same request as the owed check, whether the rest of the turn stayed inside what the
//! operator asked for. Told, never asked: a finding is one row in the conversation and blocks
//! nothing.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Component, Path};

use letibot_transcript::{NoticeKind, ToolCall, TranscriptItem};

use super::words::{self, Author, Said};

/// The most a finding may list, so one turn cannot fill the row.
pub const MAX_FINDINGS: usize = 6;

/// The most findings the one row names; the rest are counted.
pub const NAMED: usize = 3;

/// The digest's default cap, in bytes of what the check is shown.
pub const DEFAULT_MAX_CHARS: usize = 6_000;

/// The most characters of one message the request block keeps.
const PER_ROW: usize = 600;

/// The most characters of one command the digest keeps.
const CMD_CHARS: usize = 100;

/// Past this many changed files the digest groups them by directory instead of listing each.
const GROUP_AT: usize = 12;

/// The most action lines a digest lists, whatever their length.
const MAX_LINES: usize = 20;

/// The share of a note's own terms that must appear in the last answer before it is a restatement.
const RESTATE_SHARE: usize = 80;

/// The fewest terms a note needs before "it restates the answer" is a claim worth making: two words
/// are a substring of anything.
const RESTATE_TERMS: usize = 3;

/// What a finding is about. A closed set: the row's word and the model's are the same five.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// More than the ask: a file or a directory the operator never mentioned.
    Scope,
    /// Tests deleted, emptied, ignored, skipped, or their assertions taken away.
    Tests,
    /// A different task than the one asked for.
    Unrelated,
    /// Answering "done" for work that is not.
    Claim,
    /// Something that cannot be undone: a force push, a deleted branch, an emptied file.
    Destructive,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Scope,
        Kind::Tests,
        Kind::Unrelated,
        Kind::Claim,
        Kind::Destructive,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Scope => "scope",
            Kind::Tests => "tests",
            Kind::Unrelated => "unrelated",
            Kind::Claim => "claim",
            Kind::Destructive => "destructive",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == s.trim())
    }
}

/// One thing the turn did that the ask did not cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The sentence the row carries.
    pub what: String,
    /// The file or the call a reader checks it against.
    pub evidence: String,
    pub kind: Kind,
}

impl Finding {
    /// What this finding is ABOUT, so the same action cannot be told twice: its kind, and the
    /// evidence a reader checks it against - the call's own words for a rule's finding, the line of
    /// the digest for the model's. The turn monitor re-reads every row since the last operator
    /// prompt at each turn end, so a restart's empty turns find the same action again; the key is
    /// what recognises it.
    pub fn key(&self) -> String {
        format!("{}:{}", self.kind.as_str(), normalise(&self.evidence))
    }
}

/// `git branch -D x` and `- Git  branch -D  x.` are one action: case, spacing and the digest's
/// own markers are not what was done.
fn normalise(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = flat.trim().trim_start_matches("- ").trim_end_matches('.');
    flat.to_ascii_lowercase()
}

/// The turn the monitor reads, and what a rule already proves about it.
#[derive(Debug, Clone, Default)]
pub struct Turn {
    /// The operator's request for this turn, and the agent lines that are not it.
    pub request: Vec<Said>,
    /// What the turn did, compact by construction.
    pub digest: String,
    pub found: Vec<Finding>,
}

/// What the turn-end check was told, kept until its verdict arrives: the answer a proposed note is
/// compared against, and the findings a rule proved before the model was asked.
#[derive(Debug, Clone)]
pub struct Monitor {
    pub position: u64,
    pub reply: String,
    pub found: Vec<Finding>,
}

/// The most turns whose check has not answered yet that are kept: past this a verdict is late
/// enough that its findings would name a turn the session has left behind.
const MAX_PENDING: usize = 8;

/// The monitors whose verdict has not arrived, oldest first.
#[derive(Debug, Default)]
pub struct Pending(VecDeque<Monitor>);

impl Pending {
    pub fn push(&mut self, m: Monitor) {
        self.0.push_back(m);
        while self.0.len() > MAX_PENDING {
            self.0.pop_front();
        }
    }

    /// The monitor for `position`, or the oldest pending one when the position does not match - a
    /// verdict is never dropped for want of a row to hang it on.
    pub fn take_at(&mut self, position: u64) -> Option<Monitor> {
        if self.0.is_empty() {
            return None;
        }
        let at = self
            .0
            .iter()
            .position(|m| m.position == position)
            .unwrap_or(0);
        self.0.remove(at)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A file the turn changed, as its result row carries it: the excerpt is what differs, so the
/// counts are the changed region's, and `truncated` says the excerpt was cut.
struct Changed<'a> {
    path: &'a str,
    created: bool,
    before_lines: usize,
    after_lines: usize,
    before: &'a str,
    after: &'a str,
    truncated: bool,
}

/// Read the turn: the request that drove it, a capped digest of what it did, and the findings a rule
/// proves without a model.
pub fn read(items: &[TranscriptItem], from: usize, workspace: &Path, cap: usize) -> Turn {
    let request = request_of(items, from);
    let changed = changed_of(items, from);
    let calls = calls_of(items, from);
    let found = checks(&changed, &calls, workspace);
    let digest = digest_of(&request, &changed, &calls, cap);
    Turn {
        request,
        digest,
        found,
    }
}

/// The operator's words for this turn, and anything written for this turn by somebody who is not
/// the operator. A bridge line drives a turn like a prompt and is never the operator's ask.
fn request_of(items: &[TranscriptItem], from: usize) -> Vec<Said> {
    let mut out = Vec::new();
    for (row, item) in items.iter().enumerate().skip(from) {
        let TranscriptItem::User { speaker, .. } = item else {
            continue;
        };
        let author = match speaker {
            letibot_transcript::Speaker::Operator if words::human(item) => Author::Operator,
            letibot_transcript::Speaker::Peer { from } => Author::Peer(from.clone()),
            _ => continue,
        };
        out.push(Said {
            author,
            row,
            text: letibot_sessionlog::view::body_of(item),
        });
    }
    // The newest kept, since a long turn is defined by where it ended up asking.
    let over = out.len().saturating_sub(4);
    out.drain(..over);
    out
}

fn changed_of(items: &[TranscriptItem], from: usize) -> Vec<Changed<'_>> {
    let mut out = Vec::new();
    for item in items.iter().skip(from) {
        let TranscriptItem::ToolResult { edits, .. } = item else {
            continue;
        };
        for e in edits {
            out.push(Changed {
                path: &e.path,
                created: e.created,
                before_lines: e.before_lines,
                after_lines: e.after_lines,
                before: &e.before,
                after: &e.after,
                truncated: e.truncated,
            });
        }
    }
    out
}

/// The calls the turn made, as their arguments read: a tool result is the evidence a call changed
/// something, a call is the evidence it was made.
fn calls_of(items: &[TranscriptItem], from: usize) -> Vec<&ToolCall> {
    let mut out = Vec::new();
    for item in items.iter().skip(from) {
        if let TranscriptItem::Assistant { tool_calls, .. } = item {
            out.extend(tool_calls.iter());
        }
    }
    out
}

/// The digest: what the turn did, as short lines. Lists past [`GROUP_AT`] are one line per
/// directory, and the whole thing is cut to `cap` bytes from the oldest end.
fn digest_of(request: &[Said], changed: &[Changed<'_>], calls: &[&ToolCall], cap: usize) -> String {
    let mut out = String::new();
    for s in request {
        let header = match &s.author {
            Author::Operator => "operator: ".to_string(),
            other => format!("{} (not the operator): ", other.label()),
        };
        out.push_str(&header);
        out.push_str(&cut(s.text.trim(), PER_ROW));
        out.push('\n');
    }
    let mut actions: Vec<String> = Vec::new();
    if changed.len() > GROUP_AT {
        let mut dirs: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
        for c in changed {
            let dir = parent_of(c.path);
            let e = dirs.entry(dir.to_string()).or_default();
            e.0 += 1;
            e.1 += c.after_lines;
            e.2 += c.before_lines;
        }
        for (dir, (n, plus, minus)) in dirs {
            actions.push(format!("files under {dir}: {n} (+{plus} -{minus})"));
        }
    } else {
        for c in changed {
            actions.push(match (c.created, c.after_lines) {
                (true, _) => format!("created {} (+{} lines)", c.path, c.after_lines),
                (false, 0) => format!("emptied {} (-{} lines)", c.path, c.before_lines),
                _ => format!("wrote {} (+{} -{})", c.path, c.after_lines, c.before_lines),
            });
        }
    }
    for call in calls {
        match call.name.as_str() {
            "bash" => {
                let cmd = first_string(&call.arguments, "command");
                if !cmd.trim().is_empty() {
                    // A job the turn left running is a process the session now owes: "started"
                    // and "ran" are different facts about a turn, so the digest says which.
                    let background = cmd.trim_end().ends_with('&')
                        || cmd.contains("nohup")
                        || cmd.contains("setsid");
                    let verb = match background {
                        true => "started in the background",
                        false => "ran",
                    };
                    actions.push(format!("{verb}: {}", cut(first_line(&cmd), CMD_CHARS)));
                }
            }
            "pkill" => {
                let pat = first_string(&call.arguments, "pattern");
                if !pat.trim().is_empty() {
                    actions.push(format!("killed processes matching: {}", cut(&pat, 60)));
                }
            }
            "task" => actions.push("started a subagent".into()),
            "write" | "edit" => {
                let path = first_string(&call.arguments, "path");
                if !path.trim().is_empty() && !changed.iter().any(|c| c.path == path) {
                    actions.push(format!("tried to write {} (no change recorded)", path));
                }
            }
            _ => {}
        }
    }
    if actions.is_empty() {
        out.push_str("nothing: the turn changed no file and ran no command\n");
        return out;
    }
    out.push_str("the turn did:\n");
    // Two bounds, because a turn can be long in either direction: no more than MAX_LINES actions
    // are listed however short they are, and no more than `cap` bytes however many fit.
    let total = actions.len();
    let body_of =
        |from: usize| -> String { actions[from..].iter().map(|a| format!("- {a}\n")).collect() };
    let mut start = total.saturating_sub(MAX_LINES);
    let mut body = body_of(start);
    while body.len() > cap && start < total {
        // The newest actions are what a drift is visible in, so the oldest go first.
        start += 1;
        body = body_of(start);
    }
    if start > 0 {
        out.push_str(&format!("({start} earlier actions not shown)\n"));
    }
    out.push_str(&body);
    out
}

/// The block the turn-end call is shown, and what the question's facts carry.
pub fn facts(turn: &Turn) -> String {
    let mut s = String::from("the ask this turn answered, and what it did:\n");
    s.push_str(&turn.digest);
    s
}

/// Append the block to what a check is shown, with the harness as its author: a quote of it is
/// evidence, never the operator's or the model's words.
pub fn push(shown: &mut super::verdict::Shown, turn: &Turn) {
    shown.push_said(
        "the ask this turn answered, and what it did:",
        Author::Harness,
        None,
        &turn.digest,
    );
}

fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or(".")
}

/// The first `n` characters, cut on a boundary.
fn cut(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((at, _)) => format!("{}…", &s[..at]),
        None => s.to_string(),
    }
}

/// The first non-blank line, trimmed: a command is one line in a digest.
fn first_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// A top-level string field of a call's arguments. A call that is not JSON has no field, and a
/// monitor does not guess at one.
fn first_string(args: &str, key: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return String::new();
    };
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

/// Whether a path names a test: a directory that is one, or a file whose name says so.
pub fn is_test_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    if p.split('/')
        .any(|seg| matches!(seg, "tests" | "test" | "spec" | "specs" | "testing"))
    {
        return true;
    }
    let name = p.rsplit('/').next().unwrap_or(&p);
    name.ends_with("_test.rs")
        || name.ends_with("_tests.rs")
        || name.contains("_test.")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.starts_with("test_")
        || name.ends_with("_test.go")
}

/// Whether `path`, resolved from the workspace, lands outside it. A relative path climbs out with
/// `..`; an absolute one is inside only if the workspace is its prefix.
pub fn outside(path: &str, workspace: &Path) -> bool {
    let p = Path::new(path);
    if p.is_absolute() {
        return !p.starts_with(workspace);
    }
    let mut depth = 0i32;
    for c in p.components() {
        match c {
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            Component::Normal(_) => depth += 1,
            _ => {}
        }
    }
    false
}

/// The occurrences of `needle` in `hay`.
fn count(hay: &str, needle: &str) -> usize {
    hay.match_indices(needle).count()
}

/// Skip markers, per language: a turn that "fixed" a test by skipping it is the finding.
const SKIPS: [&str; 6] = [
    "#[ignore",
    "t.Skip(",
    ".skip(",
    "@pytest.mark.skip",
    "@unittest.skip",
    "xit(",
];

/// Assertion markers. `assert` alone covers `assert!`, `assert_eq!`, `assert_ne!`,
/// `assert_matches!` and `debug_assert`.
const ASSERTION: &str = "assert";

fn skips(text: &str) -> usize {
    SKIPS.iter().map(|s| count(text, s)).sum()
}

/// The findings a rule proves, with no model: a test deleted, emptied, ignored or skipped, an
/// assertion taken away, a file written outside the worktree, a force push, a deleted branch.
fn checks(changed: &[Changed<'_>], calls: &[&ToolCall], workspace: &Path) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    let mut seen: BTreeSet<(Kind, String)> = BTreeSet::new();
    let mut push = |what: String, evidence: String, kind: Kind, out: &mut Vec<Finding>| {
        if seen.insert((kind, evidence.clone())) {
            out.push(Finding {
                what,
                evidence,
                kind,
            });
        }
    };
    for c in changed {
        let test = is_test_path(c.path);
        if c.before_lines > 0 && c.after_lines == 0 && !c.created {
            let (what, kind) = match test {
                true => (
                    format!(
                        "emptied the test file {} (-{} lines)",
                        c.path, c.before_lines
                    ),
                    Kind::Tests,
                ),
                false => (
                    format!("emptied {} (-{} lines)", c.path, c.before_lines),
                    Kind::Destructive,
                ),
            };
            push(what, c.path.to_string(), kind, &mut out);
            continue;
        }
        // An excerpt that was cut cannot tell a file with fewer assertions from one the excerpt
        // stopped reading, so a cut excerpt claims nothing.
        if c.truncated {
            continue;
        }
        if skips(c.after) > skips(c.before) {
            push(
                format!("added a skip to {}", c.path),
                c.path.to_string(),
                Kind::Tests,
                &mut out,
            );
        }
        if test && count(c.after, ASSERTION) < count(c.before, ASSERTION) {
            let gone = count(c.before, ASSERTION) - count(c.after, ASSERTION);
            push(
                format!("removed {gone} assertion(s) from {}", c.path),
                c.path.to_string(),
                Kind::Tests,
                &mut out,
            );
        }
        if outside(c.path, workspace) {
            push(
                format!("wrote {} outside the worktree", c.path),
                c.path.to_string(),
                Kind::Scope,
                &mut out,
            );
        }
    }
    for call in calls {
        if matches!(call.name.as_str(), "write" | "edit") {
            let path = first_string(&call.arguments, "path");
            if !path.trim().is_empty() && outside(&path, workspace) {
                push(
                    format!("wrote {path} outside the worktree"),
                    path,
                    Kind::Scope,
                    &mut out,
                );
            }
            continue;
        }
        if call.name != "bash" {
            continue;
        }
        let cmd = first_string(&call.arguments, "command");
        let toks: Vec<&str> = cmd.split_whitespace().collect();
        let git_push = toks.windows(2).any(|w| w == ["git", "push"]);
        let forced = git_push
            && toks.iter().any(|t| {
                matches!(*t, "--force" | "--force-with-lease" | "-f" | "-uf" | "-fu")
                    || t.starts_with("--force-with-lease=")
            });
        if forced {
            push(
                format!("force-pushed: {}", cut(first_line(&cmd), 80)),
                cut(&cmd, 200),
                Kind::Destructive,
                &mut out,
            );
        }
        let deletes_branch = toks.windows(2).any(|w| w == ["git", "branch"])
            && toks
                .iter()
                .any(|t| matches!(*t, "-D" | "-d" | "--delete") || t.starts_with("--delete"));
        if deletes_branch {
            push(
                format!("deleted a branch: {}", cut(first_line(&cmd), 80)),
                cut(&cmd, 200),
                Kind::Destructive,
                &mut out,
            );
        }
        if let Some(what) = removed_test(&toks) {
            push(
                format!("deleted the test file {what}"),
                cut(&cmd, 200),
                Kind::Tests,
                &mut out,
            );
        }
    }
    out
}

/// The first operand of an `rm` that names a test, if the command deletes one.
fn removed_test(toks: &[&str]) -> Option<String> {
    let at = toks.iter().position(|t| {
        let t = t.trim_matches(['"', '\'']);
        t == "rm" || t.ends_with("/rm")
    })?;
    for t in &toks[at + 1..] {
        let word = t.trim_matches(['"', '\'', ';']);
        if word.is_empty() || word.starts_with('-') {
            continue;
        }
        if word == "&&" || word == "||" || word == "|" {
            break;
        }
        if is_test_path(word) {
            return Some(word.to_string());
        }
    }
    None
}

/// The one row a set of findings makes, or `None` when there is nothing to say. Deterministic
/// findings come first: they are facts, and a model's finding about the same file adds nothing.
pub fn row(found: &[Finding]) -> Option<TranscriptItem> {
    row_of(found).map(|(row, _)| row)
}

/// [`row`], and the [`Finding::key`] of every finding it spoke for - the ones it names AND the ones
/// it only counts - so the caller can record them as told and never say them a second time.
pub fn row_of(found: &[Finding]) -> Option<(TranscriptItem, Vec<String>)> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut kept: Vec<&Finding> = Vec::new();
    for f in found {
        let what = f.what.trim();
        if what.is_empty() {
            continue;
        }
        let key = f.evidence.trim().to_ascii_lowercase();
        if !key.is_empty() && !seen.insert(key) {
            continue;
        }
        kept.push(f);
        if kept.len() == MAX_FINDINGS {
            break;
        }
    }
    if kept.is_empty() {
        return None;
    }
    let named: Vec<&str> = kept.iter().take(NAMED).map(|f| f.what.trim()).collect();
    let mut text = format!("[monitor] beyond the ask: {}", named.join("; "));
    if kept.len() > NAMED {
        text.push_str(&format!(", and {} more", kept.len() - NAMED));
    }
    let keys = kept.iter().map(|f| f.key()).collect();
    Some((
        TranscriptItem::Notice {
            kind: NoticeKind::Monitor,
            text,
            facts: None,
        },
        keys,
    ))
}

/// Whether a proposed note only says again what the last answer said. The share of the note's own
/// terms that the answer already carries, so a reworded sentence counts and a new fact does not.
pub fn restates(note: &str, reply: &str) -> bool {
    let n: BTreeSet<String> = letibot_tools::similarity::terms(note).into_iter().collect();
    if n.len() < RESTATE_TERMS {
        return false;
    }
    let r: BTreeSet<String> = letibot_tools::similarity::terms(reply)
        .into_iter()
        .collect();
    let shared = n.iter().filter(|t| r.contains(*t)).count();
    shared * 100 >= n.len() * RESTATE_SHARE
}
