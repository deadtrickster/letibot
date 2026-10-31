//! `host_changelog` — what a host's setting was, what it is now, why, and on whose words.
//!
//! The operator, on a box whose governor had been `performance` for a month:
//!
//! > *"a month later nobody remembers why the governor is `performance`, and 'the agent changed
//! > it' is not an answer a person can act on"*
//!
//! So a change to a host's configuration leaves a **record** — five facts and a number: `what`
//! (the setting), `before`, `after`, `why`, and `words` (whose words it rests on, quoted).
//! `why` and `words` are **required**, and a record missing either is refused by name, because a
//! record of *what* with no reason is exactly the thing being complained about, written down.
//!
//! # The doing stays where it was
//!
//! This tool changes nothing. A governor is set by a `bash` call, with `sudo` where there is a
//! tty, and that is where it stays — the changelog's job is to make the change answerable later,
//! not to perform it. `revert` is the same rule one step further: it hands back the recorded
//! `before` and marks the change undone, and the seat that made the change is what puts the value
//! back.
//!
//! # The file, and why it is a file
//!
//! `<config dir>/host-changelog.jsonl`, beside `providers.toml` — **one JSON object per line,
//! append-only**. Three reasons, and they are the feature's:
//!
//! * a host's configuration outlives every daemon, every session and every store row, and the
//!   config dir is the one place on the box that is already the operator's and already survives a
//!   store being rebuilt;
//! * **two writers cannot lose each other's line.** An append is one write on a descriptor opened
//!   `O_APPEND` ([`append`]), so the second session to record a change adds a line rather than
//!   rewriting a file the first one is also rewriting;
//! * a record is evidence, and evidence is not edited. A revert does not rewrite the change it
//!   undoes — it appends a line saying so, and [`read`] folds the two.
//!
//! # The number is the position, and it is derived
//!
//! A change's number is its ordinal among the file's **change** lines — 1-based, oldest first —
//! computed on every read rather than stored. No counter is shared and no lock is taken, so two
//! concurrent records cannot be handed one number; and the order a reader sees is the order the
//! bytes are in, which is the only order an append-only file has.
//!
//! A revert line names both the number and the `what` it undoes, so a file somebody truncated by
//! hand cannot make a revert silently apply to a different setting: [`read`] reports the mismatch
//! instead of applying it.
//!
//! # A line the file cannot read is reported, not skipped in silence
//!
//! [`Log::unreadable`] carries every line this build could not parse, with its number, and `list`
//! prints them. A record is evidence: a reader has to be told that a line is there and was not
//! understood, rather than shown a tidy list that is quietly missing one.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// The tool's name.
pub const TOOL: &str = "host_changelog";

/// The file's name, in the config dir beside `providers.toml`.
pub const FILE: &str = "host-changelog.jsonl";

/// How many changes `list` shows when it is not told — the operator's *"list recent changes"*.
pub const DEFAULT_LIMIT: usize = 20;

/// Where the changelog is and who is writing, as the daemon that owns the config dir sees it.
///
/// Supplied by the daemon, which owns the config dir and the session — this crate holds only the
/// trait, the same seam [`crate::builtins::notes::NotesScope`] is, so that `letibot-tools` keeps
/// depending on nothing above it and a test can point the tool at a temp directory.
pub trait ChangelogScope: Send + Sync {
    /// The file: `<config dir>/host-changelog.jsonl`.
    fn path(&self) -> PathBuf;
    /// **This box's name**, which is the default `host` of a record. A change made on a declared
    /// machine passes `host` explicitly; one made here names this box, so a reader a month later
    /// does not have to infer which machine a line is about.
    fn host(&self) -> String;
    /// Who is recording, in the daemon's spelling — the session's id. It goes on every line,
    /// because *which session wrote this* is the half of the answer a person can go and check.
    fn who(&self) -> String;
}

/// **One change, as it was recorded.** The five facts the operator asked for, plus the two that
/// make them checkable: when, and by which session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Change {
    pub at_ms: u64,
    /// The host the setting is on — this box's name, or a declared machine's.
    pub host: String,
    /// The setting, in the words a person would search for.
    pub what: String,
    /// What it was. Empty is a fact — *there was no such setting* — and not a gap.
    pub before: String,
    /// What it is now.
    pub after: String,
    /// The problem the change solved.
    pub why: String,
    /// Whose words it rests on, quoted — the operator's own sentence.
    pub words: String,
    /// The session that recorded it.
    pub who: String,
}

/// **A change marked undone.** A second line, never an edit: the change keeps its own line, and
/// the pair is what a reader a month later needs — *this was changed, and then put back*.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Revert {
    pub at_ms: u64,
    /// The change's number, as `list` prints it.
    pub change: usize,
    /// The setting it undoes. Carried so a line that outlived its change (or a file somebody
    /// truncated) is caught rather than applied to whatever now sits at that number.
    pub what: String,
    /// The session that marked it.
    pub who: String,
    /// The operator's words for the revert, when there were any. Undoing your own change is not
    /// a new decision, so this is optional — the line still says who asked.
    pub words: String,
}

/// One line of the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Line {
    Change(Change),
    Revert(Revert),
}

/// One change as the file holds it now: the change, its number, and the revert that undid it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The change's number: its ordinal among the file's change lines, 1-based.
    pub n: usize,
    pub change: Change,
    pub reverted: Option<Revert>,
}

/// **The whole file, folded.** Every change in the order it was made, with its revert.
#[derive(Debug, Clone, Default)]
pub struct Log {
    pub entries: Vec<Entry>,
    /// `(line number, why)` for every line this build could not read, and every revert that names
    /// a change the file does not hold. A record is evidence, so a line it cannot read is
    /// reported rather than dropped.
    pub unreadable: Vec<(usize, String)>,
}

/// **Read the file and fold it.** A missing file is an empty log, not an error: a box nobody has
/// recorded a change on has no changelog, which is an answer.
pub fn read(path: &Path) -> Log {
    let mut log = Log::default();
    let Ok(bytes) = std::fs::read(path) else {
        return log;
    };
    // Lossy rather than strict: a byte somebody's editor mangled must cost one line, not the
    // whole record.
    let text = String::from_utf8_lossy(&bytes);
    for (i, line) in text.lines().enumerate() {
        let at = i + 1;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Line>(line) {
            Ok(Line::Change(change)) => log.entries.push(Entry {
                n: log.entries.len() + 1,
                change,
                reverted: None,
            }),
            Ok(Line::Revert(revert)) => {
                let target = revert
                    .change
                    .checked_sub(1)
                    .and_then(|i| log.entries.get_mut(i));
                match target {
                    // The `what` is checked only when the revert names one: a hand-written line
                    // that leaves it out is taken at its number, and one that names a different
                    // setting is reported rather than applied to the wrong change.
                    Some(entry) if revert.what.is_empty() || revert.what == entry.change.what => {
                        entry.reverted = Some(revert);
                    }
                    Some(entry) => log.unreadable.push((
                        at,
                        format!(
                            "a revert of #{} names `{}`, and #{} is `{}`",
                            revert.change, revert.what, revert.change, entry.change.what
                        ),
                    )),
                    None => log.unreadable.push((
                        at,
                        format!(
                            "a revert of #{}, which this file does not have",
                            revert.change
                        ),
                    )),
                }
            }
            Err(e) => log.unreadable.push((at, e.to_string())),
        }
    }
    log
}

/// **Add one line to the file** — the only write in this module.
///
/// `O_APPEND` (what `append(true)` asks for) makes the kernel choose the offset at the moment of
/// the write, so two processes appending at once cannot overwrite each other's bytes and the file
/// never grows a gap. The whole line is one `write_all` on that descriptor, so a reader never sees
/// half a record.
pub fn append(path: &Path, line: &Line) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(line).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(&bytes)?;
    // A record that did not reach the disk is a record that is not there a month later, which is
    // the one thing this file exists for.
    file.sync_all()
}

/// The `host_changelog` tool.
pub struct HostChangelogTool {
    /// `None` is a registry with no daemon behind it — see [`HostChangelogTool::nowhere`].
    scope: Option<Arc<dyn ChangelogScope>>,
}

impl HostChangelogTool {
    /// The daemon's own: the config dir, this box's name, the session's id.
    pub fn new(scope: Arc<dyn ChangelogScope>) -> Self {
        Self { scope: Some(scope) }
    }

    /// **No config dir behind this tool.** A test registry, or a run with no daemon: the tool
    /// says so in words at every verb rather than guessing a path and writing to the operator's
    /// real config, which is the failure a default here would produce.
    pub fn nowhere() -> Self {
        Self { scope: None }
    }
}

impl Tool for HostChangelogTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            TOOL,
            "The host's own changelog: what a setting on this machine was, what it is now, why \
             it was changed, and on whose words. `record` one change, `list` the recent ones \
             oldest first with their age and whether each was reverted, `revert` one by its \
             number. The change itself is made where it always is — a `bash` call, `sudo` where \
             there is a tty; this records it, so that a month later the reason is answerable. \
             `why` and `words` are required and a record without either is refused: `why` is the \
             problem the change solved, `words` is the operator's own sentence quoted, and 'the \
             agent changed it' is not an answer a person can act on. `revert` applies nothing — \
             it hands back the recorded `before` and marks the change, and the seat that made it \
             undoes it.",
            json!({
                "type": "object",
                "properties": {
                    "verb": {
                        "type": "string",
                        "enum": ["record", "list", "revert"],
                        "description": "`record` a change (needs `what`, `before`, `after`, `why`, `words`); `list` the recent ones (optional `limit`); `revert` one (needs `change`)."
                    },
                    "what": {
                        "type": "string",
                        "description": "For `record`: the setting, in the words a person would search for — `cpu governor`, `vm.swappiness`, `the postgres unit's LimitNOFILE`, `the fstab entry's noatime`."
                    },
                    "before": {
                        "type": "string",
                        "description": "For `record`: what it was, exactly — the value, not a summary. `\"\"` when there was no such setting, which is a fact a revert needs."
                    },
                    "after": {
                        "type": "string",
                        "description": "For `record`: what it is now, exactly."
                    },
                    "why": {
                        "type": "string",
                        "description": "For `record`: why it was changed — the problem it solved, in your own words. Required."
                    },
                    "words": {
                        "type": "string",
                        "description": "For `record`: whose words it rests on, quoted — the operator's own sentence. Required. For `revert`: theirs, if they gave any."
                    },
                    "host": {
                        "type": "string",
                        "description": "For `record`: which host the setting is on. Omitted, this box."
                    },
                    "change": {
                        "type": "integer",
                        "description": "For `revert`: the change's number, as `list` printed it (`#3` is 3)."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "For `list`: how many of the most recent changes to show, oldest first. Default 20; 0 shows every change."
                    }
                },
                "required": ["verb"]
            }),
            // It writes one file the operator owns, under the config dir — the same class `notes`
            // declares for the same reason, and `list` narrows to a read below.
            Access::Write,
        )
    }

    /// `list` only ever reads, and a read that reaches the gate is a question the operator is
    /// asked about a fact. Narrowing only, per the trait: the schema's `Write` stands for the two
    /// verbs that write.
    fn access_for(&self, args: &Value) -> Option<Access> {
        match verb_of(args).as_str() {
            "list" => Some(Access::Read),
            _ => None,
        }
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(scope) = self.scope.as_ref() else {
            return nowhere();
        };
        // The second, independent gate: the adjudicator may have admitted this call, and a
        // backend opened read-only is what refuses. Writing around it would be a tool with its
        // own boundary policy. Only the two writing verbs are asked, so a read-only seat can
        // still read the record — which is the point of `list` narrowing to `Read`.
        if matches!(verb_of(args).as_str(), "record" | "revert") && !ctx.backend.is_writable() {
            return Invocation::failed(
                "this session's backend was opened read-only",
                format!(
                    "that is a second, independent gate below the adjudication one, and it \
                     applies to the host changelog as it does to `write` and `edit` ({}). \
                     Nothing on disk changed.",
                    ctx.backend.describe()
                ),
            );
        }
        match dispatch(scope.as_ref(), args, now_ms()) {
            Ok(said) => Invocation::ok(said),
            Err(failure) => failure,
        }
    }
}

/// **The tool's whole decision, with no context** — what the tests drive.
///
/// Split out of [`Tool::invoke`] for the same reason `room`'s is: the checks this tool has (*a
/// record without `why` never reaches the file*, *a second revert is refused*) are its contract,
/// and a clock a test can choose is what makes an age assertable.
pub fn dispatch(scope: &dyn ChangelogScope, args: &Value, now_ms: u64) -> Result<String, Invocation> {
    match verb_of(args).as_str() {
        "record" => record(scope, args, now_ms),
        "list" => list(scope, args, now_ms),
        "revert" => revert(scope, args, now_ms),
        "" => Err(Invocation::failed(
            "no verb",
            "the `host_changelog` tool needs a `verb`: `record` a change, `list` the recent ones, \
             or `revert` one by its number (see the schema). Nothing was written.",
        )),
        other => Err(Invocation::failed(
            format!("unknown `host_changelog` verb `{other}`"),
            "the verbs are `record`, `list` and `revert`. Nothing was written.",
        )),
    }
}

fn record(scope: &dyn ChangelogScope, args: &Value, now_ms: u64) -> Result<String, Invocation> {
    // **The five facts, and two of them are the whole feature.** `before` may be empty — *it was
    // not set* is a fact and the one a revert needs — but the field itself is not optional.
    let mut missing = Vec::new();
    for field in ["what", "after", "why", "words"] {
        if text(args, field).is_none_or(|v| v.is_empty()) {
            missing.push(field);
        }
    }
    if args.get("before").is_none() {
        missing.push("before");
    }
    if !missing.is_empty() {
        return Err(needs(
            "record",
            &missing,
            "a host config record is worth keeping only with all five facts. `why` is the problem \
             the change solved and `words` is the operator's own sentence, quoted — a month later \
             those two ARE the answer to *why is the governor `performance`?*, and 'the agent \
             changed it' is not an answer a person can act on. `before` may be `\"\"` when there \
             was no such setting, and it is what a revert hands back.",
        ));
    }
    let change = Change {
        at_ms: now_ms,
        host: text(args, "host")
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| scope.host()),
        what: text(args, "what").unwrap_or_default(),
        before: text(args, "before").unwrap_or_default(),
        after: text(args, "after").unwrap_or_default(),
        why: text(args, "why").unwrap_or_default(),
        words: text(args, "words").unwrap_or_default(),
        who: scope.who(),
    };
    let path = scope.path();
    append(&path, &Line::Change(change.clone())).map_err(|e| {
        Invocation::failed(
            format!("the changelog could not be written: {e}"),
            format!(
                "{} could not be appended to. Nothing was recorded; a change with no record is \
                 the state this tool exists to end, so record it again once the file is \
                 writable.",
                path.display()
            ),
        )
    })?;
    // **The number is read back, not guessed.** It is the line's position among the change
    // lines, so the only way to know it is to fold the file after the append — and a second
    // writer's line landing in between must not make this reply name the wrong number.
    let log = read(&path);
    let n = log
        .entries
        .iter()
        .rev()
        .find(|e| e.change == change)
        .map(|e| e.n);
    let number = match n {
        Some(n) => format!("#{n}"),
        // The append returned `Ok` and the line is not in the fold: the file changed under us.
        // Say the fact rather than a number that may be somebody else's.
        None => "a line this read could not find again".to_string(),
    };
    let undo = match n {
        Some(n) => format!("`revert {n}` is what undoes it"),
        None => "`list` is where it can be found".to_string(),
    };
    Ok(format!(
        "recorded {number}: `{}` {} → {} on {}\n  why: {}\n  words: {}\n  by {}\n\
         {} change(s) in {} now; {undo}, and `list` prints it with its age.",
        change.what,
        value(&change.before),
        value(&change.after),
        change.host,
        change.why,
        quote(&change.words),
        change.who,
        log.entries.len(),
        path.display(),
    ))
}

fn list(scope: &dyn ChangelogScope, args: &Value, now_ms: u64) -> Result<String, Invocation> {
    let path = scope.path();
    let log = read(&path);
    let mut out = String::new();
    if log.entries.is_empty() {
        // A miss that says what there is: no change recorded, the file's own path, and the one
        // call that would put something in it.
        out.push_str(&format!(
            "no host config change has been recorded here yet: {} holds no change line. A change \
             is recorded with `record` — `what`, `before`, `after`, `why` and `words` — at the \
             moment it is made.",
            path.display()
        ));
        if !log.unreadable.is_empty() {
            out.push('\n');
            out.push_str(&unreadable_note(&log));
        }
        return Ok(out);
    }
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(DEFAULT_LIMIT);
    let shown = if limit == 0 {
        log.entries.len()
    } else {
        limit.min(log.entries.len())
    };
    let first = log.entries.len() - shown;
    for entry in &log.entries[first..] {
        out.push_str(&say(entry, now_ms));
        out.push('\n');
    }
    let older = first;
    out.push_str(&format!(
        "— {} change(s) in {}, oldest first; showing {} ({}).",
        log.entries.len(),
        path.display(),
        if shown == 1 {
            "1 change".to_string()
        } else {
            format!("the last {shown}")
        },
        if older == 0 {
            "all of them".to_string()
        } else {
            format!("{older} older — `list` with a bigger `limit`, or 0 for every change")
        }
    ));
    if !log.unreadable.is_empty() {
        out.push('\n');
        out.push_str(&unreadable_note(&log));
    }
    Ok(out)
}

fn revert(scope: &dyn ChangelogScope, args: &Value, now_ms: u64) -> Result<String, Invocation> {
    let Some(n) = args.get("change").and_then(|v| v.as_u64()).map(|v| v as usize) else {
        return Err(needs(
            "revert",
            &["change"],
            "`revert` takes the change's number — the `#N` that `list` prints, and that `record` \
             answers with. Nothing was written.",
        ));
    };
    let path = scope.path();
    let log = read(&path);
    let Some(entry) = log.entries.iter().find(|e| e.n == n) else {
        let there = if log.entries.is_empty() {
            "the file holds no change at all".to_string()
        } else {
            let numbers: Vec<String> = log
                .entries
                .iter()
                .rev()
                .take(10)
                .map(|e| format!("#{} `{}`", e.n, e.change.what))
                .collect();
            format!("it holds {}", numbers.join(", "))
        };
        return Err(Invocation::failed(
            format!("there is no change #{n}"),
            format!(
                "`{n}` is not a change this box's changelog has: {there}. `list` prints them with \
                 their numbers and their age. Nothing was written."
            ),
        ));
    };
    if let Some(was) = &entry.reverted {
        return Err(Invocation::failed(
            format!("#{n} was already reverted"),
            format!(
                "`{}` was marked reverted {} by {}, and a change is undone once — the line is in \
                 {} and stays there. If the value has to move again, that is a new change: \
                 `record` it, with what it is now and why. Nothing was written.",
                entry.change.what,
                age_of(now_ms, was.at_ms),
                was.who,
                path.display()
            ),
        ));
    }
    let revert = Revert {
        at_ms: now_ms,
        change: n,
        what: entry.change.what.clone(),
        who: scope.who(),
        words: text(args, "words").unwrap_or_default(),
    };
    append(&path, &Line::Revert(revert.clone()))
        .map_err(|e| {
            Invocation::failed(
                format!("the changelog could not be written: {e}"),
                format!(
                    "{} could not be appended to, so #{n} is NOT marked reverted. Nothing \
                     changed; try again once the file is writable.",
                    path.display()
                ),
            )
        })?;
    Ok(format!(
        "#{n} `{}` is marked reverted — {} after the change{}.\n\
         \n  before: {}\n  after:  {}\n  why:    {}\n  words:  {}\n\
         \nNOTHING has been applied: the record says the change is to be undone, and undoing it \
         is the seat's own work (a `bash` call, `sudo` where there is a tty). The line is \
         appended to {}; the change's own line is untouched, because a record is evidence.",
        entry.change.what,
        age_of(now_ms, entry.change.at_ms),
        if revert.words.is_empty() {
            String::new()
        } else {
            format!(", on {}", quote(&revert.words))
        },
        value(&entry.change.before),
        value(&entry.change.after),
        entry.change.why,
        quote(&entry.change.words),
        path.display(),
    ))
}

/// One change, as `list` prints it: a head line a person can scan, and one line of evidence.
fn say(entry: &Entry, now_ms: u64) -> String {
    let c = &entry.change;
    let mut head = format!(
        "#{} · {} · {} · {}: {} → {}",
        entry.n,
        age_of(now_ms, c.at_ms),
        c.host,
        c.what,
        value(&c.before),
        value(&c.after),
    );
    if let Some(r) = &entry.reverted {
        head.push_str(&format!(
            " · REVERTED {} by {}",
            age_of(now_ms, r.at_ms),
            r.who
        ));
    }
    let mut out = head;
    out.push_str(&format!(
        "\n    why: {} · words: {} · by {}",
        c.why,
        quote(&c.words),
        c.who
    ));
    if let Some(r) = &entry.reverted {
        if !r.words.is_empty() {
            out.push_str(&format!(" · revert asked on {}", quote(&r.words)));
        }
    }
    out
}

fn unreadable_note(log: &Log) -> String {
    let lines: Vec<String> = log
        .unreadable
        .iter()
        .map(|(n, why)| format!("line {n}: {why}"))
        .collect();
    format!(
        "{} line(s) of this file could not be read and are NOT in the list above — {}",
        log.unreadable.len(),
        lines.join("; ")
    )
}

fn nowhere() -> Invocation {
    Invocation::failed(
        "this session has no host changelog",
        "nothing told the tool where this box keeps its config, so there is no file to record a \
         change in or read one from. A daemon started with a config dir seats this tool; a \
         registry built with no daemon has no changelog behind it. Nothing was written.",
    )
}

/// The refusal for a verb missing its fields, naming every one of them.
fn needs(verb: &str, fields: &[&str], why: &str) -> Invocation {
    let names: Vec<String> = fields.iter().map(|f| format!("`{f}`")).collect();
    let listed = match names.len() {
        0 => String::new(),
        1 => names[0].clone(),
        _ => format!(
            "{} and {}",
            names[..names.len() - 1].join(", "),
            names[names.len() - 1]
        ),
    };
    Invocation::failed(format!("`{verb}` needs {listed}"), why.to_string())
}

fn verb_of(args: &Value) -> String {
    args.get("verb")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// A field as text. A number or a bool is accepted and stringified: `swappiness: 60` is the value
/// the caller means, and refusing it would cost a round trip to learn nothing.
fn text(args: &Value, key: &str) -> Option<String> {
    match args.get(key) {
        Some(Value::String(s)) => Some(s.trim().to_string()),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::Bool(b)) => Some(b.to_string()),
        _ => None,
    }
}

/// A value as it is printed: backticked, or `(not set)` when there was nothing there.
fn value(v: &str) -> String {
    if v.trim().is_empty() {
        "(not set)".to_string()
    } else {
        format!("`{v}`")
    }
}

/// Somebody's words, quoted. Left alone when they already carry quotes — a quote doubled reads as
/// a different sentence.
fn quote(v: &str) -> String {
    let first = v.trim_start().chars().next();
    if matches!(first, Some('"') | Some('\'') | Some('“') | Some('`')) {
        v.to_string()
    } else {
        format!("\"{v}\"")
    }
}

/// **How long ago**, in the register `ps` and the standing-notes pane use: a reader asking *is
/// this from this week* wants a word, not an epoch.
fn age_of(now_ms: u64, at_ms: u64) -> String {
    let secs = now_ms.saturating_sub(at_ms) / 1000;
    match secs {
        0..=59 => "under a minute ago".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        86_400..=2_591_999 => format!("{} d ago", secs / 86_400),
        _ => format!("{} mo ago", secs / 2_592_000),
    }
}

fn now_ms() -> u64 {
    u64::try_from(letibot_tokencore::store::Store::lease_now_ms()).unwrap_or(0)
}

#[cfg(test)]
mod tests;
