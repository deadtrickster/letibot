//! **Carrying a conversation the other way: out of this head and into Claude Code.**
//!
//! The reader next door turns Claude Code's transcript into this head's rows. This module
//! is the return trip, and it exists because the operator asked for a way back: *"restore
//! - resume"* cuts both ways, and a conversation that can only be imported is a one-way
//! door.
//!
//! Two shapes, and they are different promises:
//!
//! * [`write_handover`] writes a **new** transcript — a fresh session id, a file of our
//!   own — that `claude --resume <absolute path>` opens. It also writes a Markdown
//!   transcript beside it: the JSONL's line format is internal to Claude Code, so a plain
//!   text a person can paste is the fallback a Claude release cannot break.
//! * [`append_note`] puts **one line** on the end of an existing transcript, so a note
//!   from this session lands in the conversation the operator already has. That is a
//!   write into a file another program owns, so it is the one operation here that takes a
//!   backup first, refuses when it cannot take one, refuses while a `claude` process is
//!   live for that project, and has a [`revert`] that puts the file back byte for byte.
//!
//! # What is the original's format, and what is ours
//!
//! Claude Code's transcript format is documented in outline and **internal in detail**:
//! one JSON object per line, append-only, one file per session, and the documented fields
//! are `type`, `uuid`, `parentUuid`, `timestamp`, `sessionId`, `cwd`, `gitBranch`,
//! `version`; an assistant `message.content` is an array of blocks; a `tool_result`
//! arrives in a later `type:"user"` line with the same `tool_use_id`. The same
//! documentation says the exact record types *evolve with Claude Code versions* and warns
//! tooling not to rely on undocumented ones.
//!
//! So this writer emits **the conservative subset only**:
//!
//! | field | where it comes from |
//! |---|---|
//! | `type` | ours: `user` or `assistant`, and nothing else |
//! | `uuid` | ours: a fresh v4 uuid per line |
//! | `parentUuid` | ours: the previous line's `uuid`, so the chain is linear and whole |
//! | `timestamp` | **the row's own time**, in the same ISO-8601 spelling; **omitted** when the row has none, because inventing one is the thing rule 3 forbids |
//! | `sessionId` | ours: a fresh uuid for a handover; **the file's own** for an append |
//! | `cwd`, `gitBranch` | the session's, as this head knows it |
//! | `version` | `letibot-<our version>` — see below |
//! | `message.role`, `message.content` | ours: `text`, `tool_use`, `tool_result` blocks |
//! | `isMeta`, `promptSource` | ours, **only** on a row this head wrote itself (`Speaker::Agent`), which is Claude Code's own spelling for *text the model was shown and the person did not type* |
//!
//! **`version` is deliberately not a Claude Code version.** Claude Code stamps its own
//! release there, and this writer is not that release: a transcript that claims to have
//! been written by a Claude it never met is the one lie a handover must not tell. The
//! field carries `letibot-0.5.1` (this crate's version), so a person reading the file can
//! see where it came from.
//!
//! # What is deliberately not written
//!
//! * **`thinking` blocks.** A `thinking` block is a signature plus text, and only the
//!   provider can mint the signature. A fabricated one is worse than a missing thought:
//!   Claude would either reject it or replay a lie. So a [`TranscriptItem::Reasoning`] row
//!   is **skipped and counted** ([`Skipped::thinking`]) and the summary says how many.
//! * **`usage`, `requestId`, `model`** on assistant messages: this head's accounting is
//!   not Anthropic's, and a token count in the wrong units is a wrong number.
//! * **The session's system prompt** ([`TranscriptItem::System`]): Claude Code composes
//!   its own, and a second one in the transcript would be a prompt nobody sent.
//! * **A compaction marker** ([`TranscriptItem::SegmentMark`]): the boundary subtypes are
//!   exactly the undocumented records the documentation warns about. Counted.
//! * **Media.** A [`letibot_transcript::UserPart::Image`] becomes the sentence
//!   `[an image]` in the text: the bytes are a data URI this writer does not inline.
//!
//! # A resume restores the conversation, not the launch
//!
//! `claude --resume <path>` restores the conversation and the state saved in it. It does
//! **not** restore launch flags — `--mcp-config`, `--settings`, `--add-dir` have to be
//! passed again. So the handover's job is the conversation; anyone expecting the other
//! half will be disappointed, and saying so here is cheaper than their finding out.

use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use letibot_transcript::{Speaker, ToolOutcome, TranscriptItem, UserPart};
use serde_json::{Value, json};

use crate::{ImportError, Result};

/// What every line of a handover carries, and where it came from.
#[derive(Debug, Clone)]
pub struct Meta {
    /// **A fresh uuid for a handover**, and the transcript's own id for an append.
    pub session_id: String,
    pub cwd: String,
    pub git_branch: Option<String>,
    /// `letibot-<version>`; see the module doc for why it is not a Claude Code version.
    pub version: String,
}

impl Meta {
    /// A handover's metadata: a fresh id, and the directory the conversation happened in.
    pub fn fresh(cwd: &Path, git_branch: Option<String>) -> Meta {
        Meta {
            session_id: new_session_id(),
            cwd: cwd.to_string_lossy().to_string(),
            git_branch,
            version: format!("letibot-{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

/// What a handover wrote, and what it could not.
#[derive(Debug, Clone, Default)]
pub struct Written {
    pub jsonl: PathBuf,
    pub markdown: PathBuf,
    pub lines: u64,
    pub skipped: Skipped,
}

/// **The rows that are not in the handover, and why.** A handover that silently dropped a
/// third of a conversation would be a transcript that reads as complete and is not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Skipped {
    /// `thinking` blocks: a signature only the provider can mint (see the module doc).
    pub thinking: u64,
    /// The session's own system prompt: Claude Code composes its own.
    pub system: u64,
    /// Compaction markers: their subtypes are the undocumented records.
    pub marks: u64,
    /// Rows with nothing to say (an assistant row with no text and no call).
    pub empty: u64,
    /// Rows whose time the source never recorded, so the line has no `timestamp`.
    pub no_time: u64,
}

impl Skipped {
    pub fn total(&self) -> u64 {
        self.thinking + self.system + self.marks + self.empty
    }

    /// The sentence the finish line carries.
    pub fn said(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.thinking > 0 {
            parts.push(format!(
                "{} thinking block(s) are not in it — a `thinking` block needs a signature only \
                 the provider can mint, and a fabricated one is a transcript Claude would reject \
                 or replay as a lie",
                self.thinking
            ));
        }
        if self.system > 0 {
            parts.push(format!(
                "{} system row(s) are not in it — Claude Code composes its own prompt",
                self.system
            ));
        }
        if self.marks > 0 {
            parts.push(format!(
                "{} compaction marker(s) are not in it — their subtypes are the undocumented \
                 records the format warns about",
                self.marks
            ));
        }
        if self.empty > 0 {
            parts.push(format!("{} row(s) had nothing to say", self.empty));
        }
        if self.no_time > 0 {
            parts.push(format!(
                "{} line(s) carry no `timestamp`, because the row had no time and this writer \
                 does not invent one",
                self.no_time
            ));
        }
        if parts.is_empty() {
            return "every row was carried".to_string();
        }
        format!("what is NOT in it: {}", parts.join("; "))
    }
}

impl fmt::Display for Skipped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.said())
    }
}

/// **Write a whole conversation as a transcript `claude --resume` opens, plus a Markdown
/// transcript a person can read.**
///
/// `items` is `(ts_ms, item)` in the order the conversation happened; `ts_ms` is the row's
/// own time, and `0` means the source recorded none. `dir` is created if it is not there,
/// and is deliberately **not** `~/.claude/projects`: by absolute path a resume needs the
/// file nowhere in particular, and writing into another tool's directory is a thing to do
/// only when there is no other way.
pub fn write_handover(items: &[(u64, TranscriptItem)], meta: &Meta, dir: &Path) -> Result<Written> {
    std::fs::create_dir_all(dir).map_err(|e| ImportError::Io {
        path: dir.to_path_buf(),
        err: e.to_string(),
    })?;
    let jsonl = dir.join(format!("{}.jsonl", meta.session_id));
    let markdown = dir.join(format!("{}.md", meta.session_id));

    let mut out = String::new();
    let mut md = String::new();
    let mut skipped = Skipped::default();
    let mut parent: Option<String> = None;
    let mut lines = 0u64;

    md.push_str(&handover_markdown_header(meta));
    for (ts, item) in items {
        let Some(line) = line_for(item, meta, &mut parent, *ts, &mut skipped) else {
            md.push_str(&markdown_for(item));
            continue;
        };
        out.push_str(&line.to_string());
        out.push('\n');
        lines += 1;
        md.push_str(&markdown_for(item));
    }

    std::fs::write(&jsonl, out.as_bytes()).map_err(|e| ImportError::Io {
        path: jsonl.clone(),
        err: e.to_string(),
    })?;
    std::fs::write(&markdown, md.as_bytes()).map_err(|e| ImportError::Io {
        path: markdown.clone(),
        err: e.to_string(),
    })?;
    Ok(Written {
        jsonl,
        markdown,
        lines,
        skipped,
    })
}

fn handover_markdown_header(meta: &Meta) -> String {
    format!(
        "# A letibot conversation, handed to Claude Code\n\n\
         - session: `{}`\n\
         - directory: `{}`\n\
         - branch: {}\n\
         - written by: {}\n\n\
         > This file is the readable half. The transcript beside it (`{}.jsonl`) is the one\n\
         > `claude --resume` opens; its line format is internal to Claude Code, so this text\n\
         > is what survives a release that moves it. A resume restores the conversation, not\n\
         > the launch flags — `--mcp-config`, `--settings` and `--add-dir` have to be passed\n\
         > again.\n\n---\n\n",
        meta.session_id,
        meta.cwd,
        meta.git_branch.as_deref().unwrap_or("(none recorded)"),
        meta.version,
        meta.session_id
    )
}

/// One row as one line, or `None` when the row is one this writer will not fake.
fn line_for(
    item: &TranscriptItem,
    meta: &Meta,
    parent: &mut Option<String>,
    ts_ms: u64,
    skipped: &mut Skipped,
) -> Option<Value> {
    let (role, content, agent) = match item {
        TranscriptItem::User { parts, speaker } => {
            let mut blocks: Vec<Value> = Vec::new();
            for p in parts {
                match p {
                    UserPart::Text { text } => {
                        blocks.push(json!({"type": "text", "text": text}));
                    }
                    UserPart::Image { .. } => {
                        // The bytes are a data URI this writer does not inline; the sentence
                        // says what was there so the conversation still reads.
                        blocks.push(json!({"type": "text", "text": "[an image]"}));
                    }
                    UserPart::FileRef { path, .. } => {
                        blocks.push(json!({"type": "text", "text": format!("[a file: {path}]")}));
                    }
                }
            }
            if blocks.is_empty() {
                skipped.empty += 1;
                return None;
            }
            ("user", Value::Array(blocks), *speaker == Speaker::Agent)
        }
        TranscriptItem::Assistant {
            text, tool_calls, ..
        } => {
            let mut blocks: Vec<Value> = Vec::new();
            if !text.is_empty() {
                blocks.push(json!({"type": "text", "text": text}));
            }
            for c in tool_calls {
                // The arguments are the bytes the model emitted; a handover carries them
                // verbatim when they parse, and as an empty object when they do not, so a
                // malformed call cannot make the whole line invalid JSON.
                let input: Value = serde_json::from_str(&c.arguments).unwrap_or_else(|_| json!({}));
                blocks
                    .push(json!({"type": "tool_use", "id": c.id, "name": c.name, "input": input}));
            }
            if blocks.is_empty() {
                skipped.empty += 1;
                return None;
            }
            ("assistant", Value::Array(blocks), false)
        }
        TranscriptItem::ToolResult {
            call_id,
            name,
            outcome,
            payload,
            ..
        } => {
            let is_error = !matches!(outcome, ToolOutcome::Ok | ToolOutcome::Abstained { .. });
            let mut text = payload.clone();
            // The outcome is not a field a `tool_result` has beyond `is_error`, and a
            // `Backgrounded`/`NotRun` outcome is a fact the payload alone would lose. Said
            // in the payload rather than dropped.
            if let ToolOutcome::Backgrounded { handle, .. } = outcome {
                text.push_str(&format!(
                    "\n[this call was moved to the background as `{handle}`]"
                ));
            } else if let ToolOutcome::NotRun { why } = outcome {
                text.push_str(&format!("\n[this call did not run: {why}]"));
            }
            let _ = name;
            (
                "user",
                Value::Array(vec![json!({
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": text,
                    "is_error": is_error,
                })]),
                false,
            )
        }
        TranscriptItem::Reasoning { .. } => {
            skipped.thinking += 1;
            return None;
        }
        TranscriptItem::System { .. } => {
            skipped.system += 1;
            return None;
        }
        TranscriptItem::SegmentMark { .. } => {
            skipped.marks += 1;
            return None;
        }
    };

    let uuid = new_session_id();
    let mut line = json!({
        "type": role,
        "uuid": uuid,
        "parentUuid": parent.clone(),
        "sessionId": meta.session_id,
        "cwd": meta.cwd,
        "version": meta.version,
        "message": {"role": role, "content": content},
    });
    if let Some(b) = meta.git_branch.as_deref() {
        line["gitBranch"] = json!(b);
    }
    if agent {
        // Claude Code's own spelling for text the person did not type — the same fact
        // `Speaker::Agent` carries on this side.
        line["isMeta"] = json!(true);
        line["promptSource"] = json!("system");
    }
    if ts_ms > 0 {
        line["timestamp"] = json!(iso_ts(ts_ms));
    } else {
        skipped.no_time += 1;
    }
    *parent = Some(uuid);
    Some(line)
}

/// The readable half: one collapsed block per row, and a tool call on one line.
fn markdown_for(item: &TranscriptItem) -> String {
    match item {
        TranscriptItem::User { parts, speaker } => {
            let mut text = String::new();
            for p in parts {
                match p {
                    UserPart::Text { text: t } => {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(t);
                    }
                    UserPart::Image { .. } => text.push_str("[an image]"),
                    UserPart::FileRef { path, .. } => text.push_str(&format!("[a file: {path}]")),
                }
            }
            if text.trim().is_empty() {
                return String::new();
            }
            let who = match speaker {
                Speaker::Operator => "operator",
                // Named, because a reader of the Markdown has no `speaker` field to check.
                Speaker::Agent => "letibot (the session's own voice, not the operator's)",
            };
            format!("**{who}**\n\n{text}\n\n")
        }
        TranscriptItem::Assistant {
            text, tool_calls, ..
        } => {
            let mut s = String::new();
            if !text.trim().is_empty() {
                s.push_str(&format!("**assistant**\n\n{text}\n\n"));
            }
            for c in tool_calls {
                s.push_str(&format!(
                    "> tool call `{}` — `{}`\n\n",
                    c.name,
                    one_line(&c.arguments, 400)
                ));
            }
            s
        }
        TranscriptItem::ToolResult {
            name,
            outcome,
            payload,
            ..
        } => {
            let lines = payload.lines().count();
            let tail = if lines > 1 {
                format!(" … ({lines} lines)")
            } else {
                String::new()
            };
            let verdict = match outcome {
                ToolOutcome::Ok => "ok".to_string(),
                other => format!("{other:?}").to_lowercase(),
            };
            format!(
                "> tool result `{name}` — {verdict}: {}{tail}\n\n",
                one_line(payload, 200)
            )
        }
        TranscriptItem::Reasoning { text, .. } => {
            format!("> _thinking_ — {}\n\n", one_line(text, 200))
        }
        TranscriptItem::System { text, .. } => {
            format!("> _system_ — {}\n\n", one_line(text, 200))
        }
        TranscriptItem::SegmentMark { label, .. } => format!("> *{label}*\n\n"),
    }
}

fn one_line(s: &str, cap: usize) -> String {
    let first = s.lines().next().unwrap_or("").trim();
    let cut: String = first.chars().take(cap).collect();
    if cut.len() < first.len() {
        format!("{cut}…")
    } else {
        cut
    }
}

// --- the append path: one line, a backup first, and a way back -----------------------

/// What an append did.
#[derive(Debug, Clone)]
pub struct Appended {
    /// **The backup taken before the write**, so the file's state before this line is on
    /// disk under a name nobody will overwrite.
    pub backup: PathBuf,
    /// The line, exactly as it went in (with its newline).
    pub line: String,
    /// Bytes appended.
    pub bytes: u64,
}

/// **Who is running Claude Code right now.** A seam, because a test cannot start one and
/// a real check cannot be a stub: the append refuses to write into a transcript a live
/// process is writing, and that refusal has to be provable in both directions.
pub trait Live {
    /// The processes (as `pid: command`) alive for this project, or an error when the
    /// check itself could not run — which the caller must treat as *somebody is there*.
    fn live_for(
        &self,
        project: &Path,
        session_id: &str,
    ) -> std::result::Result<Vec<String>, String>;
}

/// Nothing is running. For a fixture, and for a caller that has already checked.
pub struct NoOneLive;

impl Live for NoOneLive {
    fn live_for(&self, _: &Path, _: &str) -> std::result::Result<Vec<String>, String> {
        Ok(Vec::new())
    }
}

/// **The real check**, through `ps`. A candidate is a process whose command's program is
/// `claude` and whose command line names this project's directory or this session id, or
/// whose working directory is the project. A candidate whose working directory cannot be
/// read counts as live: the check failing is not the same fact as nobody being there, and
/// this is a write into somebody else's file.
pub struct PsLive;

impl Live for PsLive {
    fn live_for(
        &self,
        project: &Path,
        session_id: &str,
    ) -> std::result::Result<Vec<String>, String> {
        let out = std::process::Command::new("ps")
            .args(["-A", "-o", "pid=,command="])
            .output()
            .map_err(|e| format!("`ps` could not be run: {e}"))?;
        if !out.status.success() {
            return Err(format!("`ps` exited {}", out.status));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let dir = project.to_string_lossy().to_string();
        let mut live = Vec::new();
        for line in text.lines() {
            let line = line.trim_start();
            let Some((pid, command)) = line.split_once(char::is_whitespace) else {
                continue;
            };
            let program = command.split_whitespace().next().unwrap_or("");
            let is_claude = program
                .rsplit('/')
                .next()
                .map(|n| n == "claude" || n.starts_with("claude-"))
                .unwrap_or(false);
            if !is_claude {
                continue;
            }
            if command.contains(&dir) || command.contains(session_id) {
                live.push(format!("{pid}: {}", one_line(command, 120)));
                continue;
            }
            match process_cwd(pid) {
                // Somebody's Claude is open in this very directory.
                Some(cwd) if cwd == project => {
                    live.push(format!("{pid}: {}", one_line(command, 120)))
                }
                Some(_) => {}
                // The working directory could not be read: treat it as live. A write into a
                // file another program is appending to is the risk this check exists for.
                None => live.push(format!(
                    "{pid}: {} (its working directory could not be read)",
                    one_line(command, 120)
                )),
            }
        }
        Ok(live)
    }
}

fn process_cwd(pid: &str) -> Option<PathBuf> {
    // Linux: the link is the answer, and reading it needs no process to cooperate.
    if let Ok(p) = std::fs::read_link(format!("/proc/{pid}/cwd")) {
        return Some(p);
    }
    // macOS: `lsof` knows, when it is installed.
    let out = std::process::Command::new("lsof")
        .args(["-a", "-p", pid, "-d", "cwd", "-Fn"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find_map(|l| l.strip_prefix('n').map(PathBuf::from))
}

/// **Put one line on the end of a transcript, with a backup first.**
///
/// In this order, and each step is a refusal rather than a warning:
///
/// 1. **Nobody live.** A `claude` process appending to this project while we append is two
///    writers on one file, and the check failing counts as somebody being there.
/// 2. **A backup, proven.** `<name>.jsonl` → `<name>.jsonl.bak-<YYYYmmdd-HHMMSS>` with
///    `cp -p` semantics (mode and mtime copied), then read back and compared byte for byte.
///    **No backup, no write** — a note is not worth the risk, and a backup that was written
///    but not proven is not a backup.
/// 3. **One `O_APPEND` write**, of one complete line, so a reader of the file never sees a
///    half line and two writers cannot interleave inside ours.
///
/// The line is the conservative one: `type: "user"`, a plain-string `message.content`, a
/// fresh `uuid`, `parentUuid` = the file's last line's `uuid`, and the file's own
/// `sessionId`/`cwd`/`gitBranch`/`version` copied off that last line — because a note that
/// invents a new session id is a note in no conversation.
pub fn append_note(path: &Path, text: &str, now_ms: u64, live: &dyn Live) -> Result<Appended> {
    let project = path.parent().unwrap_or(Path::new("."));
    let tail = last_line(path)?;
    let session_id = tail
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    match live.live_for(project, &session_id) {
        Ok(v) if v.is_empty() => {}
        Ok(v) => {
            return Err(ImportError::Io {
                path: path.to_path_buf(),
                err: format!(
                    "refusing to append: a Claude Code process is live for this project ({}). Two \
                     writers on one transcript is how a conversation gets a half line in the \
                     middle of it.",
                    v.join("; ")
                ),
            });
        }
        Err(e) => {
            return Err(ImportError::Io {
                path: path.to_path_buf(),
                err: format!(
                    "refusing to append: whether a Claude Code process is live could not be \
                     checked ({e}). A check that did not run is not a check that passed."
                ),
            });
        }
    }

    let backup = backup(path, now_ms)?;

    let uuid = new_session_id();
    let mut line = json!({
        "type": "user",
        "uuid": uuid,
        "parentUuid": tail.get("uuid").cloned().unwrap_or(Value::Null),
        "timestamp": iso_ts(now_ms),
        "sessionId": tail.get("sessionId").cloned().unwrap_or(Value::Null),
        "cwd": tail.get("cwd").cloned().unwrap_or(Value::Null),
        "gitBranch": tail.get("gitBranch").cloned().unwrap_or(Value::Null),
        "version": tail.get("version").cloned().unwrap_or(Value::Null),
        "isMeta": true,
        "promptSource": "system",
        "message": {"role": "user", "content": text},
    });
    // A field the file did not have is omitted rather than written as null: a `null`
    // `cwd` is a claim, and the source made none.
    for k in ["sessionId", "cwd", "gitBranch", "version"] {
        if line.get(k).map(Value::is_null).unwrap_or(false) {
            line.as_object_mut().unwrap().remove(k);
        }
    }
    let mut out = line.to_string();
    out.push('\n');

    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| ImportError::Io {
            path: path.to_path_buf(),
            err: e.to_string(),
        })?;
    f.write_all(out.as_bytes()).map_err(|e| ImportError::Io {
        path: path.to_path_buf(),
        err: e.to_string(),
    })?;
    f.sync_all().map_err(|e| ImportError::Io {
        path: path.to_path_buf(),
        err: e.to_string(),
    })?;
    Ok(Appended {
        backup,
        bytes: out.len() as u64,
        line: out,
    })
}

/// **Copy the file aside, with `cp -p` semantics, and prove the copy.**
///
/// The name is `<file>.bak-<YYYYmmdd-HHMMSS>`; a second backup in the same second gets
/// `-2`, `-3`, … rather than overwriting the first, so the pristine original is always the
/// oldest backup and every step is revertible.
///
/// The proof is a byte-for-byte read-back, not a size comparison: a short write, a full
/// disk or a page of zeroes from a failing disk all have the right size, and only one of
/// them is a backup.
pub fn backup(path: &Path, now_ms: u64) -> Result<PathBuf> {
    let stamp = stamp_utc(now_ms);
    let base = PathBuf::from(format!("{}.bak-{stamp}", path.display()));
    let mut target = base.clone();
    let mut n = 2;
    while target.exists() {
        target = PathBuf::from(format!("{}.bak-{stamp}-{n}", path.display()));
        n += 1;
    }
    let md = std::fs::metadata(path).map_err(|e| ImportError::Io {
        path: path.to_path_buf(),
        err: e.to_string(),
    })?;
    let mut src = std::fs::File::open(path).map_err(|e| ImportError::Io {
        path: path.to_path_buf(),
        err: e.to_string(),
    })?;
    let mut out = std::fs::File::create(&target).map_err(|e| ImportError::Io {
        path: target.clone(),
        err: e.to_string(),
    })?;
    std::io::copy(&mut src, &mut out).map_err(|e| ImportError::Io {
        path: target.clone(),
        err: e.to_string(),
    })?;
    out.sync_all().map_err(|e| ImportError::Io {
        path: target.clone(),
        err: e.to_string(),
    })?;
    drop(out);
    // `cp -p`: the mode and the mtime are part of what a file is, and a backup that loses
    // them is a file that cannot be told apart from one that was written now.
    std::fs::set_permissions(&target, md.permissions()).map_err(|e| ImportError::Io {
        path: target.clone(),
        err: e.to_string(),
    })?;
    if let Ok(modified) = md.modified()
        && let Ok(f) = std::fs::OpenOptions::new().write(true).open(&target)
    {
        let times = std::fs::FileTimes::new()
            .set_modified(modified)
            .set_accessed(modified);
        let _ = f.set_times(times);
    }
    // **Proven, not assumed.**
    let a = std::fs::read(path).map_err(|e| ImportError::Io {
        path: path.to_path_buf(),
        err: e.to_string(),
    })?;
    let b = std::fs::read(&target).map_err(|e| ImportError::Io {
        path: target.clone(),
        err: e.to_string(),
    })?;
    if a != b {
        let _ = std::fs::remove_file(&target);
        return Err(ImportError::Io {
            path: target,
            err: format!(
                "the backup did not match the file it copied ({} bytes against {}), so it was \
                 removed and nothing was written",
                b.len(),
                a.len()
            ),
        });
    }
    Ok(target)
}

/// **Put the transcript back, byte for byte, from a named backup.**
///
/// The backup is *not* consumed: it stays where it is, so a revert can be reverted by
/// reverting again. And this refuses while a `claude` process is live for the project, for
/// the same reason the append does — it is a write into a file another program owns.
pub fn revert(path: &Path, backup: &Path, live: &dyn Live) -> Result<u64> {
    let bytes = std::fs::read(backup).map_err(|e| ImportError::Io {
        path: backup.to_path_buf(),
        err: e.to_string(),
    })?;
    let project = path.parent().unwrap_or(Path::new("."));
    let id = last_line(path)
        .ok()
        .and_then(|v| {
            v.get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    match live.live_for(project, &id) {
        Ok(v) if v.is_empty() => {}
        Ok(v) => {
            return Err(ImportError::Io {
                path: path.to_path_buf(),
                err: format!(
                    "refusing to revert: a Claude Code process is live for this project ({}).",
                    v.join("; ")
                ),
            });
        }
        Err(e) => {
            return Err(ImportError::Io {
                path: path.to_path_buf(),
                err: format!("refusing to revert: the live check could not run ({e})."),
            });
        }
    }
    let md = std::fs::metadata(backup).map_err(|e| ImportError::Io {
        path: backup.to_path_buf(),
        err: e.to_string(),
    })?;
    std::fs::write(path, &bytes).map_err(|e| ImportError::Io {
        path: path.to_path_buf(),
        err: e.to_string(),
    })?;
    std::fs::set_permissions(path, md.permissions()).map_err(|e| ImportError::Io {
        path: path.to_path_buf(),
        err: e.to_string(),
    })?;
    Ok(bytes.len() as u64)
}

/// The file's last complete line, parsed. A last line with no newline is a transcript
/// somebody was killed in the middle of writing, and appending after it would put a note
/// inside a broken record.
fn last_line(path: &Path) -> Result<Value> {
    let mut f = std::fs::File::open(path).map_err(|e| ImportError::Io {
        path: path.to_path_buf(),
        err: e.to_string(),
    })?;
    let len = f
        .metadata()
        .map_err(|e| ImportError::Io {
            path: path.to_path_buf(),
            err: e.to_string(),
        })?
        .len();
    if len == 0 {
        return Err(ImportError::Io {
            path: path.to_path_buf(),
            err: "the transcript is empty, so there is no conversation to add a note to"
                .to_string(),
        });
    }
    // A line of a transcript can be megabytes (a tool result), so read back in growing
    // windows until a complete line is found.
    let mut window = 64 * 1024u64;
    loop {
        let start = len.saturating_sub(window);
        f.seek(SeekFrom::Start(start))
            .map_err(|e| ImportError::Io {
                path: path.to_path_buf(),
                err: e.to_string(),
            })?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(|e| ImportError::Io {
            path: path.to_path_buf(),
            err: e.to_string(),
        })?;
        if !buf.ends_with(b"\n") {
            return Err(ImportError::Io {
                path: path.to_path_buf(),
                err: "the transcript's last line is incomplete, which is what a Claude Code \
                      process killed mid-write leaves behind. Appending after it would put a \
                      note inside a broken record."
                    .to_string(),
            });
        }
        let text = String::from_utf8_lossy(&buf);
        let trimmed = text.trim_end_matches('\n');
        if let Some(last) = trimmed.rsplit('\n').next()
            && !last.trim().is_empty()
            && let Ok(v) = serde_json::from_str::<Value>(last)
        {
            return Ok(v);
        }
        if start == 0 {
            return Err(ImportError::Io {
                path: path.to_path_buf(),
                err: "no complete JSON line could be read at the end of the transcript".to_string(),
            });
        }
        window *= 8;
    }
}

// --- ids and times ------------------------------------------------------------------

/// **A fresh v4 uuid.** From `/dev/urandom`, because a handover's session id must not
/// collide with anything: a duplicated id is exactly the failure the documentation names
/// (*a session id resolves only when one project holds it*), so this cannot be a counter.
/// A box with no `/dev/urandom` gets a time-and-pid id instead, and [`new_session_id`]'s
/// caller cannot tell — which is why the fallback still sets the version nibble.
pub fn new_session_id() -> String {
    let mut b = [0u8; 16];
    let filled = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .is_ok();
    if !filled {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id() as u128;
        let mix = nanos ^ (pid << 64) ^ (nanos.rotate_left(37));
        b[..16].copy_from_slice(&mix.to_le_bytes());
    }
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 1
    let h: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        h[0..4].concat(),
        h[4..6].concat(),
        h[6..8].concat(),
        h[8..10].concat(),
        h[10..16].concat()
    )
}

/// Epoch ms as the transcript's own ISO-8601 spelling, `2026-10-07T12:28:34.023Z`.
pub fn iso_ts(ms: u64) -> String {
    let (y, mo, d, h, mi, s, milli) = civil_of(ms);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z")
}

/// Epoch ms as `YYYYmmdd-HHMMSS` in UTC — the backup's name. UTC and not local time
/// because a timezone is a fact this crate does not have, and a name that shifts with
/// `TZ` is a name two machines disagree about.
pub fn stamp_utc(ms: u64) -> String {
    let (y, mo, d, h, mi, s, _) = civil_of(ms);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// The inverse of [`crate::days_from_civil`]: `(year, month, day, hour, min, sec, ms)`.
///
/// `pub(crate)` because the listing prints a civil date too: one conversion, two readers.
pub(crate) fn civil_of(ms: u64) -> (i64, i64, i64, i64, i64, i64, i64) {
    let secs = (ms / 1000) as i64;
    let milli = (ms % 1000) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant's `civil_from_days`.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, h, mi, s, milli)
}

#[cfg(test)]
mod tests;
