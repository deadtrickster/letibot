//! **Reading a Claude Code conversation into this head's rows.**
//!
//! `letibot --session cc-<uuid>` opens the UI immediately and imports the conversation
//! in the background, rows landing as they are read — the same shape R6 gave
//! `oc-` (see `crates/opencode/src/lib.rs`), one namespace over. This crate is the
//! **reading** half: it opens Claude Code's transcript directory, walks one transcript,
//! and turns each JSONL line into a [`TranscriptItem`] this head already knows how to
//! draw. It renders nothing, publishes nothing, and knows nothing about the daemon — so
//! the four rules below are testable in isolation, which is the whole reason the halves
//! are split (this crate, the wiring in `letibot-harnessd`).
//!
//! # Where the conversation is, and what is never opened
//!
//! `~/.claude/projects/<mangled-project>/<uuid>.jsonl`, one JSON object per line. The id
//! this head carries is the file's stem: `cc-e90320f4-b550-4aa0-aa11-1c2673df88e8`.
//! Beside the transcript a session may own `<uuid>/subagents/agent-<id>.jsonl` (a
//! subagent's own conversation, with a `.meta.json` naming its type and the `Task` call
//! it answers) and `<uuid>/tool-results/`. **Only the transcript is read**; the two
//! directories are counted and named, never walked (see *`isSidechain`* below).
//!
//! **This crate reads transcripts and nothing else.** It never opens
//! `~/.claude/credentials`, `~/.claude/settings.json`, `~/.ssh`, or any file whose
//! subject is a secret: [`Source::open`] takes one directory, [`Source::sessions`] lists
//! `*.jsonl` one level down, and there is no path here that could render a credential
//! into a transcript. The transcripts themselves are read-only, which is rule 4.
//!
//! # The four rules that make this an import and not a forgery
//!
//! 1. **An imported row says it is imported, and whose it was.** Carried by
//!    [`SessionMeta`]: the transcript's uuid, the `cwd` it ran in, the git branch, and
//!    the model and Claude Code version that answered. The row itself cannot say it — no
//!    [`TranscriptItem`] has an origin field — so the session does, in its header. See
//!    [`SessionMeta::origin`].
//! 2. **The ledger does not claim the spend.** [`Report::spend`] is Claude Code's own
//!    `cost-state` line — `totalCostUSD` and the per-model token counts — in another
//!    provider's units and another vendor's money. It is reported for a label and is
//!    never folded into this session's budget. **It is the LAST such line, not a sum**:
//!    the field is cumulative, so adding the lines would multiply the bill by the number
//!    of times Claude Code wrote it down.
//! 3. **Time is taken, never invented.** Every [`Row`] carries `ts_ms` off the line it
//!    came from ([`parse_ts`]), and a line with no `timestamp` gets `0` — the source
//!    having none — rather than the previous row's time, which would be a clock this
//!    crate made up. Nothing here calls `now`.
//! 4. **Nothing is written to Claude Code's directory.** The transcript is opened with
//!    [`std::fs::File::open`] and read; there is no writer in this crate, and the tests
//!    prove the file's bytes and mtime are the same after a read as before it.
//!
//! # What a line becomes, and why one block can be two rows
//!
//! Lines are read in file order, which is the conversation's order, and mapped by their
//! `type` and then by each content block's own `type`:
//!
//! | claude | this head |
//! |---|---|
//! | `user` · `message.content` a string | [`TranscriptItem::User`], speaker from [`declared_speaker`] |
//! | `user` · `{"type":"text"}` | [`TranscriptItem::User`] |
//! | `user` · `{"type":"tool_result"}` | [`TranscriptItem::ToolResult`], paired to its call by `tool_use_id` |
//! | `user` · `{"type":"image"}` | a [`Scrap`] — *"an image the operator pasted"*, see below |
//! | `assistant` · `{"type":"text"}` | [`TranscriptItem::Assistant`], no calls |
//! | `assistant` · `{"type":"thinking"}` | [`TranscriptItem::Reasoning`] (an empty one is counted, not a row) |
//! | `assistant` · `{"type":"tool_use"}` | an [`TranscriptItem::Assistant`] carrying the one [`ToolCall`] |
//! | `assistant` · `{"type":"redacted_thinking"}` | a [`Scrap`] — the thought itself was removed by the source |
//! | `system` · `subtype: away_summary` | [`TranscriptItem::Assistant`] — the model's own recap, said to a returning operator |
//! | `system` · `subtype: compact_boundary` | [`TranscriptItem::SegmentMark`] — zero-width, renders nothing, names the compaction in its label |
//! | `system` · `subtype: informational` | a [`Scrap`] — a notice from the tool, not a conversation row |
//! | `system` · `subtype: turn_duration` | counted, no row: a stopwatch is not a sentence |
//! | `type: summary` | [`TranscriptItem::Assistant`] — the continuation summary, which is what was said |
//! | `cost-state` | the spend side-channel (rule 2), no row |
//! | `attachment`, `queue-operation`, `api-request*`, `file-history-*`, `ai-title`, `agent-name`, `last-prompt`, `mode`, `permission-mode`, `atis-latch`, `dev-mods` | counted per type, no row — Claude Code's own bookkeeping |
//! | anything else | a [`Scrap`] once for the type, and a count in [`Report::unknown_types`] |
//!
//! **A `tool_use` and its `tool_result` are two rows on purpose**, exactly as opencode's
//! `tool` part is. The head labels a tool card from the nearest preceding
//! `Assistant { tool_calls }` row (`crates/tui/src/app.rs`), because that row is the only
//! place the arguments survive; Claude Code keeps the call in an `assistant` line and the
//! result in a later `user` line, so mapping either one alone would lose half of every
//! card. The pairing is by `tool_use_id`, and the call's `name` is carried onto the
//! result from the call — a result whose call this build never saw is counted
//! ([`Report::unpaired_results`]) and named `tool` rather than left blank, so the card
//! still says *something* rather than rendering an anonymous outcome.
//!
//! **A message is written one block per line.** Claude Code's `assistant` lines repeat
//! the message's `id` and each carries a different slice of its content — MEASURED on the
//! 116 MB transcript of 2026-10-28: 14,736 assistant lines, 4,456 of them sharing an id
//! with another line, and **zero repeated `text` blocks and zero repeated `tool_use` ids**
//! across them. So the reader does not dedupe by message id: a block appears once, on the
//! line that carries it, and that line is where it is emitted.
//!
//! # An unknown type is counted and said, never dropped
//!
//! The same rule R3 applies to an unreadable frame: an importer that skips what it does
//! not recognise reproduces **inside the transcript** the exact failure R3 exists to
//! prevent. So an unrecognised line type becomes a [`Scrap`] — **once for the type**, at
//! its first line, saying what it is — and every occurrence is counted in
//! [`Report::unknown_types`]. Once, not once per line: `api-request-blob` alone is 4,076
//! lines in the measured transcript and would bury the conversation it is interleaved
//! with. The count is the fact, and the count is in the report the finish line prints.
//!
//! # `isSidechain`: a subagent's branch is NOT this conversation, and the skip says so
//!
//! A Claude Code subagent has its own transcript — `<uuid>/subagents/agent-<id>.jsonl`,
//! whose first line is `{"isSidechain": true, "agentId": …}` and whose `.meta.json` names
//! its `agentType`, its `description` and the `toolUseId` of the `Task` call that spawned
//! it (21 such files sit beside the measured 116 MB transcript). The same flag appears
//! inline in older transcripts, where the branch was written into the parent's file.
//!
//! **This importer skips it, and says so at the point of the skip.** Two reasons, and the
//! first is what makes skipping lose nothing:
//!
//! * **The parent's conversation already has the child in it.** The `Task` call is a
//!   `tool_use` in the parent and the child's final report is its `tool_result` — both
//!   rows are in the parent's file and both are imported. What is in the branch is the
//!   child's own working conversation: its prompt, its tool loop, its dead ends.
//! * **This head models that as a child session, not as rows.** A subagent here is its
//!   own session with its own transcript, seat and gate. Replaying a child's turns into
//!   the parent would put another agent's words in the parent's mouth — the exact
//!   confusion [`Speaker`] was added to end — and its tool calls into a conversation that
//!   never made them.
//!
//! So the first line of each branch becomes a [`Scrap`] naming it (`agentId` when the
//! source gives one), the branch's lines are counted ([`Report::sidechain_lines`]), and
//! [`Report::summary`] says how many branches and how many sibling transcripts were
//! deliberately left alone. A skip a reader can see is not the silent drop R3 forbids.
//!
//! # What is deliberately not carried
//!
//! * **Pictures.** A `user` line can carry `{"type":"image"}` with the bytes inline as
//!   base64 (27 blocks in the measured corpus). The row is a [`Scrap`] saying what the
//!   picture was, not a `UserPart::Image`: this head's `UserPart::Image` holds a data URI
//!   that rides every prompt rebuild, and deciding to store megabytes of another tool's
//!   screenshots is the attachment work's question, not an importer's.
//! * **Claude Code's context.** `attachment` lines are system reminders, tool listings,
//!   skills, plans, diagnostics, environment snapshots and the contents of files it
//!   attached — 10,358 lines in 32 kinds in the measured transcript. They are how Claude
//!   Code talked to *its* model, not what was said in the conversation; they are counted
//!   by kind ([`Report::attachments`]) and named once.
//! * **Its wire captures.** `api-request`, `api-request-blob` and `api-request-shape` are
//!   the request bodies Claude Code sent, 76,103 lines of them in one file. Counted, not
//!   read.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use letibot_transcript::{
    ReasoningField, Speaker, ToolCall, ToolOutcome, TranscriptItem, UserPart,
};
use serde_json::Value;

/// **The return trip**: writing a transcript `claude --resume` opens, and the one-line
/// append with a backup and a way back. See the module's own doc for which fields are the
/// original's format and which are ours.
pub mod handover;

/// Everything that can stop a read.
#[derive(Debug)]
pub enum ImportError {
    /// **The transcript directory is not there, or is not a directory.** Reported as
    /// *this* and not as "no such session", because the two are different facts — the
    /// same pair `letibot-opencode` tells apart, for the same reason.
    NoDirectory { path: PathBuf, err: String },
    /// The directory is there and no transcript has that id.
    NoSuchSession { id: String, dir: PathBuf },
    /// A file that the listing named could not be opened or read.
    Io { path: PathBuf, err: String },
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::NoDirectory { path, err } => write!(
                f,
                "no Claude Code transcript directory at {} ({err}). There is nothing to \
                 import from — this is the directory being absent or unreadable, not the \
                 session being absent.",
                path.display()
            ),
            ImportError::NoSuchSession { id, dir } => write!(
                f,
                "Claude Code has no transcript `{id}` under {}. The id is the file's own \
                 name without `.jsonl`, and `--list-claude` prints them with the `cc-` mark \
                 this head strips.",
                dir.display()
            ),
            ImportError::Io { path, err } => {
                write!(f, "reading {}: {err}", path.display())
            }
        }
    }
}

impl std::error::Error for ImportError {}

pub type Result<T> = std::result::Result<T, ImportError>;

/// Token counts, in whatever provider's units they were recorded — Claude Code's here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.reasoning + self.cache_read + self.cache_write
    }
}

/// One transcript's own metadata — what rule 1 needs, what a picker row shows, and the
/// progress denominator.
#[derive(Debug, Clone, Default)]
pub struct SessionMeta {
    /// The file's stem: the uuid this head prefixes with `cc-`.
    pub id: String,
    /// The mangled project directory the transcript lives in (`-Users-dead-Projects-x`),
    /// which is Claude Code's name for the tree, not a path.
    pub project: String,
    pub path: PathBuf,
    /// The `cwd` the session ran in, from the first line that carries one.
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    /// `message.model` from the first assistant line — `claude-opus-5-5`, not this box's
    /// alias.
    pub model: Option<String>,
    /// Claude Code's own version string.
    pub version: Option<String>,
    /// Claude Code's generated title (`ai-title`), when it wrote one.
    pub title: Option<String>,
    /// **The first thing the operator said** — the line a picker row shows.
    pub first: String,
    pub bytes: u64,
    /// The file's own mtime, in ms since the epoch. Taken from the filesystem, never
    /// from a clock read at import time.
    pub mtime_ms: u64,
}

impl SessionMeta {
    /// **Rule 1, in one line.** What every imported row is, and whose it was, so a reader
    /// who cannot tell whose sentence it is can weigh it.
    pub fn origin(&self) -> String {
        let model = self.model.as_deref().unwrap_or("another model");
        let mut s = format!(
            "imported from Claude Code — `{}` in {}",
            self.id,
            self.cwd
                .as_deref()
                .unwrap_or("a directory it did not record")
        );
        if let Some(b) = self.git_branch.as_deref().filter(|b| !b.is_empty()) {
            s.push_str(&format!(" (branch {b})"));
        }
        s.push_str(&format!(", run under {model}"));
        if let Some(v) = self.version.as_deref().filter(|v| !v.is_empty()) {
            s.push_str(&format!(" (claude code {v})"));
        }
        s
    }

    /// The picker/CLI line: the id a person types, then when, how big, whose model, and
    /// the first thing that was said.
    pub fn line(&self) -> String {
        let when = if self.mtime_ms == 0 {
            "never".to_string()
        } else {
            // The file's own mtime, as a UTC civil date — the same conversion the writer uses
            // to stamp a backup, because a number of seconds is exact and unreadable and a
            // second copy of the calendar arithmetic is how two answers to one question begin.
            let (y, mo, d, h, mi, _, _) = crate::handover::civil_of(self.mtime_ms);
            format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}Z")
        };
        format!(
            "cc-{}  {when}  {:>10}  {:<20}  {}",
            self.id,
            bytes_human(self.bytes),
            self.model.as_deref().unwrap_or("-"),
            if self.first.is_empty() {
                "(no operator line)"
            } else {
                &self.first
            }
        )
    }

    /// The plain data form, for `--tsv`: id, mtime, bytes, model, cwd, first line.
    pub fn tsv(&self) -> String {
        let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
        format!(
            "cc-{}\t{}\t{}\t{}\t{}\t{}",
            self.id,
            self.mtime_ms,
            self.bytes,
            clean(self.model.as_deref().unwrap_or("")),
            clean(self.cwd.as_deref().unwrap_or("")),
            clean(&self.first)
        )
    }
}

fn bytes_human(n: u64) -> String {
    if n >= 1 << 30 {
        format!("{:.1}G", n as f64 / (1u64 << 30) as f64)
    } else if n >= 1 << 20 {
        format!("{:.1}M", n as f64 / (1u64 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{:.1}K", n as f64 / (1u64 << 10) as f64)
    } else {
        format!("{n}B")
    }
}

/// A whole transcript, as far as the header needs it before the first row.
#[derive(Debug, Clone)]
pub struct Tree {
    pub root: SessionMeta,
    /// Subagent transcripts sitting beside this one (`<uuid>/subagents/agent-*.jsonl`) —
    /// counted so the report can say what was deliberately not walked.
    pub subagents: u64,
}

/// One row to append: a transcript item, the transcript it came from, and the moment it
/// happened. `ts_ms` is the line's own `timestamp`, taken and never invented.
#[derive(Debug, Clone)]
pub struct Row {
    /// The transcript's uuid (rule 1's *whose*), so a row read out of a sibling file
    /// would still name its source.
    pub source_session: String,
    /// The 1-based line number in the source file — how a reader finds it again.
    pub line: u64,
    /// The line's own `uuid`, when it has one.
    pub uuid: String,
    pub ts_ms: u64,
    pub item: TranscriptItem,
}

/// **A line that is real but is not a transcript row** — an unknown type, a picture, a
/// redacted thought, a sidechain branch's first line. Counted and said, never dropped.
#[derive(Debug, Clone)]
pub struct Scrap {
    pub source_session: String,
    pub line: u64,
    pub uuid: String,
    pub ts_ms: u64,
    pub tag: String,
    pub said: String,
}

/// What [`Source::read`] hands its sink, in order.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Event {
    /// **The importer's own counter** — bytes of the file consumed against its size. Not
    /// derived from what has not arrived: *an indicator must be the fact, not a rendering
    /// of the fact.* The denominator is the file's length, which is known before the first
    /// row without a counting pass; a line count would cost a second pass over 116 MB.
    Progress {
        bytes: u64,
        total: u64,
    },
    Row(Row),
    Scrap(Scrap),
}

/// The spend rule 2 is about: Claude Code's own cost and tokens, in another provider's
/// units.
#[derive(Debug, Clone, Default)]
pub struct Spend {
    pub cost: f64,
    pub tokens: Tokens,
}

/// What a whole read produced — the numbers the finish line and `/status` keep.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// The file's size (the denominator).
    pub bytes: u64,
    /// Lines in the file, blank ones included.
    pub lines: u64,
    /// Transcript rows emitted.
    pub rows: u64,
    /// Lines that became a sentence rather than a row.
    pub scraps: u64,
    /// Every line type seen, as seen. The census `/status` can print whole.
    pub by_type: BTreeMap<String, u64>,
    /// **Line types this build does not read**, counted individually. Non-empty is a fact
    /// to report, not a silence.
    pub unknown_types: BTreeMap<String, u64>,
    /// `attachment` lines by their own `type` — Claude Code's context, counted by kind.
    pub attachments: BTreeMap<String, u64>,
    /// Subagent transcripts beside this one, which were not walked.
    pub subagents: u64,
    /// Sidechain branches skipped, and the lines in them.
    pub sidechains: u64,
    pub sidechain_lines: u64,
    /// `compact_boundary` markers carried as [`TranscriptItem::SegmentMark`].
    pub compactions: u64,
    /// `summary` / `away_summary` lines carried as the model's own words.
    pub summaries: u64,
    /// `tool_result` blocks whose `tool_use` this build never saw.
    pub unpaired_results: u64,
    /// `thinking` blocks the source wrote empty (a signature with no thought in it).
    pub empty_thinking: u64,
    /// Lines whose JSON would not parse.
    pub malformed: u64,
    /// Claude Code's spend for the session, for the label (rule 2).
    pub spend: Spend,
}

impl Report {
    /// The sentence the finish line and `/status` carry.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "imported {} row(s) from a {} transcript of {} line(s)",
            self.rows,
            bytes_human(self.bytes),
            self.lines
        );
        if self.spend.cost > 0.0 || self.spend.tokens.total() > 0 {
            s.push_str(&format!(
                "; claude code spent ${:.4} and {} token(s) on it, in another provider's \
                 units — history, not this session's budget",
                self.spend.cost,
                self.spend.tokens.total()
            ));
        }
        if self.summaries > 0 || self.compactions > 0 {
            s.push_str(&format!(
                "; {} continuation summar(y|ies) and {} compaction marker(s) carried, because \
                 that is what was said",
                self.summaries, self.compactions
            ));
        }
        if self.sidechains > 0 {
            s.push_str(&format!(
                "; {} subagent branch(es) ({} line(s)) are another conversation and were left \
                 to their own session, said at the point of the skip",
                self.sidechains, self.sidechain_lines
            ));
        }
        if self.subagents > 0 {
            s.push_str(&format!(
                "; {} subagent transcript(s) sit beside this one and were not walked — the \
                 `Task` call and its report are already rows here",
                self.subagents
            ));
        }
        if self.unpaired_results > 0 {
            s.push_str(&format!(
                "; {} tool result(s) answer a call this build never saw and are named `tool`",
                self.unpaired_results
            ));
        }
        if self.malformed > 0 {
            s.push_str(&format!(
                "; {} line(s) would not parse and were said, not dropped",
                self.malformed
            ));
        }
        if !self.unknown_types.is_empty() {
            let named: Vec<String> = self
                .unknown_types
                .iter()
                .map(|(t, n)| format!("`{t}` × {n}"))
                .collect();
            s.push_str(&format!(
                "; {} line type(s) this build does not read were counted and said, not \
                 dropped: {}",
                self.unknown_types.len(),
                named.join(", ")
            ));
        }
        s
    }
}

/// A read-only handle on Claude Code's transcript directory.
pub struct Source {
    dir: PathBuf,
}

impl Source {
    /// **Open the directory**, and prove it is one now rather than on the first read: a
    /// path that is a file, or is gone, is [`ImportError::NoDirectory`] here instead of a
    /// confusing emptiness later.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::read_dir(dir).map_err(|e| ImportError::NoDirectory {
            path: dir.to_path_buf(),
            err: e.to_string(),
        })?;
        Ok(Source {
            dir: dir.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// **Every transcript, newest first** — one row per `<project>/<uuid>.jsonl`, with the
    /// first operator line, the model and the `cwd` read from the file's head.
    ///
    /// `subagents/` is not listed: a subagent's conversation is a branch of its parent,
    /// not a session of its own (see the module doc), and listing it would offer a person
    /// a second copy of a child.
    pub fn sessions(&self) -> Result<Vec<SessionMeta>> {
        let mut out = Vec::new();
        let projects = std::fs::read_dir(&self.dir).map_err(|e| ImportError::NoDirectory {
            path: self.dir.clone(),
            err: e.to_string(),
        })?;
        for entry in projects.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let project = entry.file_name().to_string_lossy().to_string();
            let Ok(files) = std::fs::read_dir(&dir) else {
                continue;
            };
            for f in files.flatten() {
                let path = f.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                // A transcript whose head will not read is still a session on disk; it is
                // listed with what could be read rather than dropped from the listing.
                if let Ok(meta) = self.meta(id, &project, &path) {
                    out.push(meta);
                }
            }
        }
        out.sort_by(|a, b| b.mtime_ms.cmp(&a.mtime_ms).then_with(|| a.id.cmp(&b.id)));
        Ok(out)
    }

    /// The whole transcript `id` names: its metadata and the count of subagent transcripts
    /// beside it.
    pub fn tree(&self, id: &str) -> Result<Tree> {
        let path = self.find(id)?;
        let project = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let root = self.meta(id, &project, &path)?;
        let subagents = self.subagent_count(id, path.parent());
        Ok(Tree { root, subagents })
    }

    /// **Just the session's `cwd`**, so the imported session's tools are confined to the
    /// tree the conversation was had in rather than the daemon's start directory. Empty
    /// when the transcript recorded none — the caller keeps its own root then, which is
    /// the same rule the stored workspace follows.
    pub fn directory(&self, id: &str) -> Result<String> {
        Ok(self.tree(id)?.root.cwd.unwrap_or_default())
    }

    /// **Read the transcript, line by line, in the order it was written.** One callback
    /// per event, so the caller can publish as it goes and keep the screen live — a
    /// function that returned a `Vec` would have read 116 MB before drawing anything.
    pub fn read(&self, id: &str, sink: &mut dyn FnMut(Event)) -> Result<Report> {
        let path = self.find(id)?;
        let tree = self.tree(id)?;
        let total = tree.root.bytes;
        let file = std::fs::File::open(&path).map_err(|e| ImportError::Io {
            path: path.clone(),
            err: e.to_string(),
        })?;
        let mut reader = BufReader::with_capacity(1 << 16, file);

        let mut report = Report {
            bytes: total,
            subagents: tree.subagents,
            ..Default::default()
        };
        let mut st = State::default();
        let mut buf = String::new();
        let mut line_no = 0u64;
        let mut done = 0u64;
        loop {
            buf.clear();
            let got = reader.read_line(&mut buf).map_err(|e| ImportError::Io {
                path: path.clone(),
                err: e.to_string(),
            })?;
            if got == 0 {
                break;
            }
            line_no += 1;
            // Clamped: the last line of a file may have no trailing newline, so the sum of
            // `read_line` lengths can pass the file's size by one.
            done = (done + got as u64).min(total);
            sink(Event::Progress { bytes: done, total });
            interpret(
                id,
                line_no,
                buf.trim_end(),
                buf.ends_with('\n'),
                &mut st,
                &mut report,
                sink,
            );
        }
        report.lines = line_no;
        Ok(report)
    }

    /// Where `<id>` is, by walking the projects one level deep. The id is the file's stem.
    fn find(&self, id: &str) -> Result<PathBuf> {
        let not_found = || ImportError::NoSuchSession {
            id: id.to_string(),
            dir: self.dir.clone(),
        };
        if id.is_empty() {
            return Err(not_found());
        }
        let projects = std::fs::read_dir(&self.dir).map_err(|e| ImportError::NoDirectory {
            path: self.dir.clone(),
            err: e.to_string(),
        })?;
        for entry in projects.flatten() {
            let candidate = entry.path().join(format!("{id}.jsonl"));
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        Err(not_found())
    }

    fn meta(&self, id: &str, project: &str, path: &Path) -> Result<SessionMeta> {
        let md = std::fs::metadata(path).map_err(|e| ImportError::Io {
            path: path.to_path_buf(),
            err: e.to_string(),
        })?;
        let mtime_ms = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let head = head_scan(path);
        Ok(SessionMeta {
            id: id.to_string(),
            project: project.to_string(),
            path: path.to_path_buf(),
            cwd: head.cwd,
            git_branch: head.branch,
            model: head.model,
            version: head.version,
            title: head.title,
            first: head.first,
            bytes: md.len(),
            mtime_ms,
        })
    }

    fn subagent_count(&self, id: &str, project_dir: Option<&Path>) -> u64 {
        let Some(project_dir) = project_dir else {
            return 0;
        };
        let dir = project_dir.join(id).join("subagents");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return 0;
        };
        entries
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
            .count() as u64
    }
}

/// The reader's own state across lines: the calls it has seen (so a result can carry its
/// name), whether it is inside a sidechain branch, and which unknown types it has already
/// said.
#[derive(Default)]
struct State {
    tools: HashMap<String, String>,
    in_sidechain: bool,
    said_unknown: BTreeSet<String>,
}

/// What the head of a transcript says — read without walking the whole file.
#[derive(Default)]
struct Head {
    first: String,
    model: Option<String>,
    cwd: Option<String>,
    branch: Option<String>,
    version: Option<String>,
    title: Option<String>,
}

/// **How much of a transcript the listing reads.** The first operator line is the third
/// line of a real session and never past the first page of one; the cap exists so that a
/// transcript that begins with ten thousand `attachment` lines (a resumed session can)
/// still answers in milliseconds.
const HEAD_LINES: u64 = 400;

fn head_scan(path: &Path) -> Head {
    let mut head = Head::default();
    let Ok(file) = std::fs::File::open(path) else {
        return head;
    };
    let mut reader = BufReader::new(file);
    let mut buf = String::new();
    let mut n = 0u64;
    let mut have_first = false;
    while n < HEAD_LINES {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        n += 1;
        let Ok(v) = serde_json::from_str::<Value>(buf.trim_end()) else {
            continue;
        };
        if head.cwd.is_none() {
            head.cwd = v
                .get("cwd")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.is_empty());
        }
        if head.branch.is_none() {
            head.branch = v
                .get("gitBranch")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.is_empty());
        }
        if head.version.is_none() {
            head.version = v
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.is_empty());
        }
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                if head.model.is_none() {
                    head.model = v
                        .pointer("/message/model")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .filter(|s| !s.is_empty());
                }
            }
            Some("ai-title") | Some("agent-name") => {
                if head.title.is_none() {
                    head.title = v
                        .get("aiTitle")
                        .or_else(|| v.get("agentName"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .filter(|s| !s.is_empty());
                }
            }
            Some("user") if !have_first => {
                let declared = declared_speaker(&v);
                let texts = user_texts(&v);
                for t in texts {
                    if let Said::Words { text, speaker } = classify(&t, declared)
                        && speaker == Speaker::Operator
                        && !text.is_empty()
                    {
                        head.first = first_line_of(&text);
                        break;
                    }
                }
                // **Only a line that HAD the operator's words ends the search.** The first
                // `user` line of a resumed session is often machinery — a
                // `<command-name>`, a `<local-command-stdout>`, a meta notice — and setting
                // this before the check left a listing row reading *(no operator line)* for
                // two of the thirteen transcripts on this box while their second user line
                // said exactly what the conversation was about.
                have_first = !head.first.is_empty();
            }
            _ => {}
        }
        if have_first && head.model.is_some() && head.cwd.is_some() {
            break;
        }
    }
    head
}

/// The first line of a message, bounded — a picker row is one line.
fn first_line_of(text: &str) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    const CAP: usize = 160;
    if first.chars().count() <= CAP {
        return first.to_string();
    }
    let cut: String = first.chars().take(CAP).collect();
    format!("{cut}…")
}

/// **Who a `user` line's words belong to, as the source itself declares it** — R42's two
/// speakers, read off Claude Code's own fields rather than guessed from the text.
///
/// MEASURED over the 13 transcripts on this box (2026-10-28): `origin.kind` is `human`
/// 938 times, `task-notification` 655, `peer` 439, `auto-continuation` 3 and absent
/// 11,487; `promptSource` is `typed` 840, `queued` 54, `suggestion_accepted` 44,
/// `system` 1,097 and absent 11,487. So the declaration is there for the lines that need
/// it, and absent is the older spelling of *the operator*, which is also what a head drew
/// for those rows before the field existed — the reading
/// `TranscriptItem::User::speaker`'s own doc takes for an absent field.
///
/// `isMeta` wins over both: a meta line is Claude Code's own text shown to the model
/// (a `<local-command-caveat>`, a subagent's hand-back), never the person's.
fn declared_speaker(v: &Value) -> Speaker {
    if v.get("isMeta").and_then(Value::as_bool).unwrap_or(false) {
        return Speaker::Agent;
    }
    if let Some(kind) = v.pointer("/origin/kind").and_then(Value::as_str) {
        return match kind {
            "human" => Speaker::Operator,
            // Another agent's message, a job's settlement, or the harness continuing
            // itself. Never the person's words.
            _ => Speaker::Agent,
        };
    }
    match v.get("promptSource").and_then(Value::as_str) {
        Some("typed") | Some("queued") | Some("suggestion_accepted") => Speaker::Operator,
        Some("system") => Speaker::Agent,
        _ => Speaker::Operator,
    }
}

/// Every text block of a `user` line, in order, with a string `content` treated as one.
fn user_texts(v: &Value) -> Vec<String> {
    match v.pointer("/message/content") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .map(|b| str_field(b, "text"))
            .collect(),
        _ => Vec::new(),
    }
}

/// A frame Claude Code wraps a message in — `<bash-input>`, `<pasted_content id="17da">`,
/// `<task-notification>`, `<local-command-caveat>`, …
struct Frame<'a> {
    tag: &'a str,
    body: &'a str,
    /// Just past the closing tag, in the text this was parsed from.
    end: usize,
}

/// **A frame, when the text is exactly one element.** The closing tag may carry attributes
/// of its own — `<pasted_content id="17da">…</pasted_content id="17da">` is the real
/// spelling — so the close is matched by name and whatever follows the `>` must be blank.
fn frame(text: &str) -> Option<Frame<'_>> {
    let f = leading_frame(text)?;
    if text.trim()[f.end..].trim().is_empty() {
        Some(f)
    } else {
        None
    }
}

/// **A frame at the START of the text**, whether or not anything follows it. This is the
/// one the stripper needs: Claude Code puts `<local-command-caveat>` and
/// `<command-name>` in front of the person's own words, and what is behind them is what
/// they said.
fn leading_frame(text: &str) -> Option<Frame<'_>> {
    let t = text.trim();
    let rest = t.strip_prefix('<')?;
    let name_end = rest.find(|c: char| c == '>' || c.is_whitespace())?;
    let tag = &rest[..name_end];
    if tag.is_empty()
        || !tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let open_end = rest.find('>')? + 1;
    let tail = &rest[open_end..];
    let close = format!("</{tag}");
    let at = tail.find(&close)?;
    let after = &tail[at + close.len()..];
    let gt = after.find('>')?;
    Some(Frame {
        tag,
        body: &tail[..at],
        // **Plus one for the `<` that was stripped**, so `end` is an index into the text
        // this was handed rather than into the slice after it — the caller slices the
        // original, and an off-by-one there silently kept the whole frame as the operator's
        // words (measured: `<bash-input> gh pr merge 1</bash-input>` came out whole).
        end: 1 + open_end + at + close.len() + gt + 1,
    })
}

/// What one piece of a `user` line turned out to be.
enum Said {
    /// Somebody's words, and whose.
    Words {
        text: String,
        speaker: Speaker,
    },
    /// Claude Code's own plumbing — a slash command's caveat, a command's stdout, a
    /// system reminder. Nothing anybody said.
    Machinery(String),
    /// A frame this build does not read. Said, never invented into a row.
    UnknownFrame(String),
    Nothing,
}

/// Frames that are Claude Code talking to its model, not a person talking to anybody.
const MACHINERY: &[&str] = &[
    "local-command-caveat",
    "local-command-stdout",
    "command-name",
    "command-message",
    "command-args",
    "system-reminder",
];

/// Frames that name their own author, so the author wins over the line's declaration.
const OTHER_AGENT: &[&str] = &[
    "task-notification",
    "agent-message",
    "cross-session-message",
];

/// **What a `user` line's text becomes.** A frame that carries the person's act is carried
/// in this head's own spelling for that act (`! cmd`); a frame that names another author is
/// theirs; a frame that is Claude Code's plumbing is said, not drawn; and a frame this
/// build does not know is said in the report rather than invented as a row.
fn classify(text: &str, declared: Speaker) -> Said {
    let t = text.trim();
    if t.is_empty() {
        return Said::Nothing;
    }
    if let Some(f) = frame(t) {
        return classify_frame(&f);
    }
    // Not one element: strip the plumbing off the front and keep what the person said.
    let mut rest = t;
    let mut removed: Vec<&str> = Vec::new();
    while let Some(f) = leading_frame(rest) {
        if MACHINERY.contains(&f.tag) {
            removed.push(f.tag);
            let next = &rest[f.end..];
            if next.trim().is_empty() {
                return Said::Machinery(removed.join(", "));
            }
            rest = next.trim_start();
        } else {
            break;
        }
    }
    let rest = rest.trim();
    if rest.is_empty() {
        return Said::Nothing;
    }
    Said::Words {
        text: rest.to_string(),
        speaker: declared,
    }
}

fn classify_frame(f: &Frame<'_>) -> Said {
    match f.tag {
        // The operator ran a shell command with `!`. This head spells that the same way,
        // so the row is the act in the vocabulary the reader already knows.
        "bash-input" => Said::Words {
            text: format!("! {}", f.body.trim()),
            speaker: Speaker::Operator,
        },
        // A paste is the operator's own words, with the frame taken off.
        "pasted_content" => Said::Words {
            text: f.body.trim().to_string(),
            speaker: Speaker::Operator,
        },
        // A job's settlement and a subagent's hand-back: this head appends both as a
        // `User` row with `speaker: Agent`, which is exactly what these are.
        t if OTHER_AGENT.contains(&t) => Said::Words {
            text: f.body.trim().to_string(),
            speaker: Speaker::Agent,
        },
        t if MACHINERY.contains(&t) => Said::Machinery(t.to_string()),
        other => Said::UnknownFrame(other.to_string()),
    }
}

/// One line, from JSON to rows.
fn interpret(
    sid: &str,
    line_no: u64,
    raw: &str,
    complete: bool,
    st: &mut State,
    report: &mut Report,
    sink: &mut dyn FnMut(Event),
) {
    if raw.trim().is_empty() {
        return;
    }
    let v: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            report.malformed += 1;
            report.scraps += 1;
            let said = if complete {
                format!("a line whose JSON this build could not read ({e})")
            } else {
                format!(
                    "the last line of the transcript is incomplete ({e}) — which is what a \
                     conversation Claude Code is still writing looks like"
                )
            };
            sink(Event::Scrap(Scrap {
                source_session: sid.to_string(),
                line: line_no,
                uuid: String::new(),
                ts_ms: 0,
                tag: "<unparseable>".to_string(),
                said,
            }));
            return;
        }
    };
    let ty = v
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("<no type>")
        .to_string();
    *report.by_type.entry(ty.clone()).or_default() += 1;
    let ts_ms = v
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_ts)
        .unwrap_or(0);
    let uuid = str_field(&v, "uuid");

    // **The sidechain rule, said at the point of the skip.** One sentence per branch, at
    // the branch's first line — not one per line, which for a subagent's transcript would
    // be thousands of notices about the same skip.
    let side = v
        .get("isSidechain")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if side != st.in_sidechain {
        st.in_sidechain = side;
        if side {
            report.sidechains += 1;
            report.scraps += 1;
            let who = v
                .get("agentId")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| uuid.clone());
            sink(Event::Scrap(Scrap {
                source_session: sid.to_string(),
                line: line_no,
                uuid: uuid.clone(),
                ts_ms,
                tag: "isSidechain".to_string(),
                said: format!(
                    "a subagent's branch begins here (`{who}`) and is NOT this conversation: \
                     its words and its tool calls belong to that child's own session, while \
                     the `Task` call that spawned it and the report it handed back are rows \
                     in this one. Left to its own session, on purpose."
                ),
            }));
        }
    }
    if side {
        report.sidechain_lines += 1;
        return;
    }

    match ty.as_str() {
        "user" => user_line(sid, line_no, &uuid, ts_ms, &v, st, report, sink),
        "assistant" => assistant_line(sid, line_no, &uuid, ts_ms, &v, st, report, sink),
        "system" => system_line(sid, line_no, &uuid, ts_ms, &v, st, report, sink),
        "summary" => {
            // The continuation summary: what Claude Code wrote about the conversation it
            // carried forward. It is the model's own words, and a reader who cannot see
            // them cannot tell what the conversation it continues was about.
            let text = v
                .get("summary")
                .or_else(|| v.get("content"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if text.is_empty() {
                return;
            }
            report.summaries += 1;
            emit_row(
                sid,
                line_no,
                &uuid,
                ts_ms,
                TranscriptItem::Assistant {
                    text,
                    tool_calls: Vec::new(),
                    truncated: false,
                },
                report,
                sink,
            );
        }
        // Rule 2's side-channel. The field is CUMULATIVE, so the last line wins.
        "cost-state" => {
            let cost = v
                .get("totalCostUSD")
                .and_then(Value::as_f64)
                .unwrap_or(report.spend.cost);
            let mut tokens = Tokens::default();
            if let Some(models) = v.get("modelUsage").and_then(Value::as_object) {
                for m in models.values() {
                    let n = |k: &str| m.get(k).and_then(Value::as_u64).unwrap_or(0);
                    tokens.input += n("inputTokens");
                    tokens.output += n("outputTokens");
                    tokens.reasoning += n("thinkingTokens");
                    tokens.cache_read += n("cacheReadInputTokens");
                    tokens.cache_write += n("cacheCreationInputTokens");
                }
            }
            report.spend = Spend { cost, tokens };
        }
        "attachment" => {
            let kind = v
                .pointer("/attachment/type")
                .and_then(Value::as_str)
                .unwrap_or("<no type>")
                .to_string();
            *report.attachments.entry(kind).or_default() += 1;
            say_once(
                st,
                report,
                sink,
                sid,
                line_no,
                &uuid,
                ts_ms,
                "attachment",
                "`attachment` lines are Claude Code's own context — system reminders, tool \
                 listings, skills, plans, diagnostics, and the files it attached — counted by \
                 kind in the report. None of them is a conversation row, and none is dropped: \
                 the count is the fact."
                    .to_string(),
            );
        }
        // Bookkeeping this build reads and does not draw: counted in `by_type` and, where
        // it matters, read for the listing (a title) or the spend (cost-state). No scrap:
        // these are not conversation, and one sentence per kind would be a page of notices
        // about a file's own index.
        "ai-title"
        | "agent-name"
        | "last-prompt"
        | "mode"
        | "permission-mode"
        | "atis-latch"
        | "dev-mods"
        | "queue-operation"
        | "file-history-snapshot"
        | "file-history-delta"
        | "api-request"
        | "api-request-blob"
        | "api-request-shape" => {}
        other => {
            *report.unknown_types.entry(other.to_string()).or_default() += 1;
            say_once(
                st,
                report,
                sink,
                sid,
                line_no,
                &uuid,
                ts_ms,
                other,
                format!(
                    "a `{other}` line this build does not read — counted in the report and \
                     said here once, rather than silently dropped"
                ),
            );
        }
    }
}

/// Say a fact about a line *type* once, at its first line — see the module doc.
#[allow(clippy::too_many_arguments)]
fn say_once(
    st: &mut State,
    report: &mut Report,
    sink: &mut dyn FnMut(Event),
    sid: &str,
    line_no: u64,
    uuid: &str,
    ts_ms: u64,
    tag: &str,
    said: String,
) {
    if !st.said_unknown.insert(tag.to_string()) {
        return;
    }
    report.scraps += 1;
    sink(Event::Scrap(Scrap {
        source_session: sid.to_string(),
        line: line_no,
        uuid: uuid.to_string(),
        ts_ms,
        tag: tag.to_string(),
        said,
    }));
}

#[allow(clippy::too_many_arguments)]
fn user_line(
    sid: &str,
    line_no: u64,
    uuid: &str,
    ts_ms: u64,
    v: &Value,
    st: &mut State,
    report: &mut Report,
    sink: &mut dyn FnMut(Event),
) {
    let declared = declared_speaker(v);
    let content = v.pointer("/message/content");
    match content {
        Some(Value::String(s)) => {
            utterance(sid, line_no, uuid, ts_ms, s, declared, report, sink);
        }
        Some(Value::Array(blocks)) => {
            for b in blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let t = str_field(b, "text");
                        utterance(sid, line_no, uuid, ts_ms, &t, declared, report, sink);
                    }
                    Some("tool_result") => {
                        tool_result_line(sid, line_no, uuid, ts_ms, b, st, report, sink)
                    }
                    Some("image") => {
                        let mt = b
                            .pointer("/source/media_type")
                            .and_then(Value::as_str)
                            .unwrap_or("an image");
                        report.scraps += 1;
                        sink(Event::Scrap(Scrap {
                            source_session: sid.to_string(),
                            line: line_no,
                            uuid: uuid.to_string(),
                            ts_ms,
                            tag: "image".to_string(),
                            said: format!(
                                "a picture ({mt}) the transcript carries inline as base64. This \
                                 importer carries no pictures: the bytes would ride every prompt \
                                 rebuild, and whether this head should store another tool's \
                                 screenshots is the attachment work's question."
                            ),
                        }));
                    }
                    Some(other) => {
                        let key = format!("user:{other}");
                        *report.unknown_types.entry(key.clone()).or_default() += 1;
                        say_once(
                            st,
                            report,
                            sink,
                            sid,
                            line_no,
                            uuid,
                            ts_ms,
                            &key,
                            format!(
                                "a `{other}` block in a user message, which this build does not \
                                 read — counted and said, not dropped"
                            ),
                        );
                    }
                    None => {}
                }
            }
        }
        _ => {}
    }
}

/// One piece of a `user` line's text: a row, a scrap, or nothing.
#[allow(clippy::too_many_arguments)]
fn utterance(
    sid: &str,
    line_no: u64,
    uuid: &str,
    ts_ms: u64,
    text: &str,
    declared: Speaker,
    report: &mut Report,
    sink: &mut dyn FnMut(Event),
) {
    match classify(text, declared) {
        Said::Nothing => {}
        Said::Words { text, speaker } => emit_row(
            sid,
            line_no,
            uuid,
            ts_ms,
            TranscriptItem::User {
                parts: vec![UserPart::Text { text }],
                speaker,
            },
            report,
            sink,
        ),
        Said::Machinery(tags) => {
            report.scraps += 1;
            sink(Event::Scrap(Scrap {
                source_session: sid.to_string(),
                line: line_no,
                uuid: uuid.to_string(),
                ts_ms,
                tag: tags.clone(),
                said: format!(
                    "a message that was only Claude Code's own plumbing ({tags}) — no words of \
                     anybody's in it to carry"
                ),
            }));
        }
        Said::UnknownFrame(tag) => {
            let key = format!("frame:{tag}");
            *report.unknown_types.entry(key.clone()).or_default() += 1;
            report.scraps += 1;
            sink(Event::Scrap(Scrap {
                source_session: sid.to_string(),
                line: line_no,
                uuid: uuid.to_string(),
                ts_ms,
                tag: tag.clone(),
                said: format!(
                    "a `<{tag}>` frame this build does not read. Said here rather than \
                     invented as a row: a frame whose meaning is unknown would be a row whose \
                     speaker is a guess."
                ),
            }));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn tool_result_line(
    sid: &str,
    line_no: u64,
    uuid: &str,
    ts_ms: u64,
    b: &Value,
    st: &State,
    report: &mut Report,
    sink: &mut dyn FnMut(Event),
) {
    let call_id = str_field(b, "tool_use_id");
    let is_error = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
    let payload = flatten_result(b.get("content"));
    // The name comes from the call, which is the only place it exists. A result whose call
    // this build never saw is counted and named `tool`, so the card says something.
    let name = match st.tools.get(&call_id) {
        Some(n) => n.clone(),
        None => {
            report.unpaired_results += 1;
            "tool".to_string()
        }
    };
    let outcome = if is_error {
        ToolOutcome::Failed {
            reason: first_line_of(&payload),
        }
    } else {
        ToolOutcome::Ok
    };
    emit_row(
        sid,
        line_no,
        uuid,
        ts_ms,
        TranscriptItem::ToolResult {
            call_id,
            name,
            outcome,
            payload,
            edit: None,
            origin: None,
            // An imported row carries no media, and that is a fact about the importer
            // rather than a gap left by accident — the same sentence opencode's importer
            // makes about its own attachments.
            media: None,
        },
        report,
        sink,
    );
}

#[allow(clippy::too_many_arguments)]
fn assistant_line(
    sid: &str,
    line_no: u64,
    uuid: &str,
    ts_ms: u64,
    v: &Value,
    st: &mut State,
    report: &mut Report,
    sink: &mut dyn FnMut(Event),
) {
    let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) else {
        return;
    };
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = str_field(b, "text");
                if text.is_empty() {
                    continue;
                }
                emit_row(
                    sid,
                    line_no,
                    uuid,
                    ts_ms,
                    TranscriptItem::Assistant {
                        text,
                        tool_calls: Vec::new(),
                        truncated: false,
                    },
                    report,
                    sink,
                );
            }
            Some("thinking") => {
                let text = str_field(b, "thinking");
                if text.is_empty() {
                    // A signature with no thought in it: Claude Code writes the block so the
                    // signature can be replayed, and there is nothing of it to show. Counted
                    // rather than drawn as an empty thought.
                    report.empty_thinking += 1;
                    continue;
                }
                emit_row(
                    sid,
                    line_no,
                    uuid,
                    ts_ms,
                    TranscriptItem::Reasoning {
                        text,
                        field: ReasoningField::ReasoningContent,
                        truncated: false,
                    },
                    report,
                    sink,
                );
            }
            Some("tool_use") => {
                let id = str_field(b, "id");
                let name = str_field(b, "name");
                let arguments = b
                    .get("input")
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                // The call's name, for the result that answers it. Kept here because the
                // result carries only the id.
                st.tools.insert(id.clone(), name.clone());
                emit_row(
                    sid,
                    line_no,
                    uuid,
                    ts_ms,
                    TranscriptItem::Assistant {
                        text: String::new(),
                        tool_calls: vec![ToolCall {
                            id,
                            name,
                            arguments,
                        }],
                        truncated: false,
                    },
                    report,
                    sink,
                );
            }
            Some("redacted_thinking") => {
                report.scraps += 1;
                sink(Event::Scrap(Scrap {
                    source_session: sid.to_string(),
                    line: line_no,
                    uuid: uuid.to_string(),
                    ts_ms,
                    tag: "redacted_thinking".to_string(),
                    said: "a thought Claude Code removed from its own transcript and replaced \
                           with a signature — there is nothing left of it to carry"
                        .to_string(),
                }));
            }
            Some(other) => {
                let key = format!("assistant:{other}");
                *report.unknown_types.entry(key.clone()).or_default() += 1;
                say_once(
                    st,
                    report,
                    sink,
                    sid,
                    line_no,
                    uuid,
                    ts_ms,
                    &key,
                    format!(
                        "a `{other}` block in an assistant message, which this build does not \
                         read — counted and said, not dropped"
                    ),
                );
            }
            None => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn system_line(
    sid: &str,
    line_no: u64,
    uuid: &str,
    ts_ms: u64,
    v: &Value,
    st: &mut State,
    report: &mut Report,
    sink: &mut dyn FnMut(Event),
) {
    let subtype = v
        .get("subtype")
        .and_then(Value::as_str)
        .unwrap_or("<no subtype>");
    match subtype {
        // **The model's own recap, said to an operator who stepped away.** It is a sentence
        // that was on the screen, in the model's voice, and it is carried as such.
        "away_summary" => {
            let text = v
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if text.is_empty() {
                return;
            }
            report.summaries += 1;
            emit_row(
                sid,
                line_no,
                uuid,
                ts_ms,
                TranscriptItem::Assistant {
                    text,
                    tool_calls: Vec::new(),
                    truncated: false,
                },
                report,
                sink,
            );
        }
        // **A boundary, not a sentence.** The conversation either side of a compaction is
        // two transcripts of one conversation, and this is the seam between them: a
        // `SegmentMark` renders nothing and says where it happened.
        "compact_boundary" => {
            report.compactions += 1;
            let trigger = v
                .pointer("/compactMetadata/trigger")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let pre = v
                .pointer("/compactMetadata/preTokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let post = v
                .pointer("/compactMetadata/postTokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let label = if pre > 0 || post > 0 {
                format!(
                    "claude code compacted this conversation here ({trigger}; {pre} → {post} tokens)"
                )
            } else {
                format!("claude code compacted this conversation here ({trigger})")
            };
            emit_row(
                sid,
                line_no,
                uuid,
                ts_ms,
                TranscriptItem::SegmentMark {
                    segment_id: format!("claude-compact-{line_no}"),
                    label,
                    kind: "compaction".to_string(),
                    edge: letibot_transcript::SegmentEdge::Open,
                },
                report,
                sink,
            );
        }
        // A notice from the tool — a usage limit, a reset — shown to the operator. It is
        // machinery, not conversation: said once per kind, counted.
        "informational" => {
            let text = v
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            report.scraps += 1;
            sink(Event::Scrap(Scrap {
                source_session: sid.to_string(),
                line: line_no,
                uuid: uuid.to_string(),
                ts_ms,
                tag: "informational".to_string(),
                said: format!(
                    "a notice Claude Code showed the operator, not a conversation row: {text}"
                ),
            }));
        }
        // A stopwatch. Counted in `by_type`, no row: a duration is not a sentence, and a row
        // would put a number where the conversation has none.
        "turn_duration" => {}
        other => {
            let key = format!("system:{other}");
            *report.unknown_types.entry(key.clone()).or_default() += 1;
            say_once(
                st,
                report,
                sink,
                sid,
                line_no,
                uuid,
                ts_ms,
                &key,
                format!(
                    "a `system` line of subtype `{other}`, which this build does not read — \
                     counted and said, not dropped"
                ),
            );
        }
    }
}

/// A tool result's content is a string, or a list of blocks (text, images, tool
/// references). Flattened in order, with the parts this head cannot carry named in place —
/// a picture is `[an image]`, never nothing.
fn flatten_result(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => str_field(b, "text"),
                Some(other) => format!("[{other}]"),
                None => b.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn emit_row(
    sid: &str,
    line_no: u64,
    uuid: &str,
    ts_ms: u64,
    item: TranscriptItem,
    report: &mut Report,
    sink: &mut dyn FnMut(Event),
) {
    report.rows += 1;
    sink(Event::Row(Row {
        source_session: sid.to_string(),
        line: line_no,
        uuid: uuid.to_string(),
        ts_ms,
        item,
    }));
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// **An ISO-8601 timestamp to ms since the epoch** — rule 3's only clock.
///
/// Claude Code writes `2026-10-07T12:28:34.023Z`. The fraction is optional, an offset is
/// accepted because the field is ISO-8601 and not a private format, and anything else is
/// `None` — a time this crate cannot read is a row with no time, never a guessed one.
pub fn parse_ts(s: &str) -> Option<u64> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> { s.get(from..to)?.parse::<i64>().ok() };
    if b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b't') {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let y = num(0, 4)?;
    let mo = num(5, 7)?;
    let d = num(8, 10)?;
    let h = num(11, 13)?;
    let mi = num(14, 16)?;
    let sec = num(17, 19)?;
    // A field outside its range is not a time: `2026-10-07T25:00:00Z` is what a mangled
    // line looks like, and turning it into the next day would be inventing an hour.
    if !(1..=12).contains(&mo)
        || !(1..=31).contains(&d)
        || !(0..=23).contains(&h)
        || !(0..=59).contains(&mi)
        || !(0..=60).contains(&sec)
    {
        return None;
    }
    let mut rest = &s[19..];
    let mut millis = 0i64;
    if let Some(r) = rest.strip_prefix('.') {
        let digits: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return None;
        }
        // `023` is 23 ms; more than three digits are truncated, which is what a
        // millisecond resolution means.
        let mut ms = digits.clone();
        ms.truncate(3);
        while ms.len() < 3 {
            ms.push('0');
        }
        millis = ms.parse().ok()?;
        rest = &r[digits.len()..];
    }
    let offset_min = if let Some(r) = rest.strip_prefix(['Z', 'z']) {
        if !r.is_empty() {
            return None;
        }
        0
    } else if let Some(sign) = rest.strip_prefix(['+', '-']) {
        let mult = if rest.starts_with('-') { -1 } else { 1 };
        let (oh, om) = match sign.len() {
            5 if sign.as_bytes()[2] == b':' => (num_str(&sign[..2])?, num_str(&sign[3..])?),
            4 => (num_str(&sign[..2])?, num_str(&sign[2..])?),
            2 => (num_str(sign)?, 0),
            _ => return None,
        };
        mult * (oh * 60 + om)
    } else if rest.is_empty() {
        0
    } else {
        return None;
    };

    let days = days_from_civil(y, mo, d);
    let secs = days * 86_400 + h * 3_600 + mi * 60 + sec - offset_min * 60;
    if secs < 0 {
        return None;
    }
    Some((secs as u64) * 1000 + millis as u64)
}

fn num_str(s: &str) -> Option<i64> {
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Howard Hinnant's `days_from_civil`: the number of days from 1970-01-01 to `y-m-d` in
/// the proleptic Gregorian calendar. Written out rather than pulled in, because this crate
/// needs one function of a calendar and not a date library.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// **The idempotency rule, as a function of the two facts it reads** — Package H's `cc-`
/// arm, in the one place it can be tested without a daemon.
///
/// A session whose id begins `cc-` names a Claude Code conversation to read in, and the
/// rest of the id is the transcript's own uuid. **Only when the session is not already a
/// resume**: re-running the same `--session cc-<uuid>` finds the stored session and
/// resumes it, and importing a second time would append a second copy of the conversation
/// to a session that already holds it — the one way an import can corrupt a transcript.
/// The `oc-` arm beside it in `Harness::open` follows the same rule, and this is that rule
/// spelled out rather than repeated.
pub fn import_target(resumed: bool, session_id: &str) -> Option<&str> {
    if resumed {
        return None;
    }
    session_id.strip_prefix("cc-").filter(|id| !id.is_empty())
}

/// **Where Claude Code's transcripts are on this box**, if they are anywhere.
///
/// `$CLAUDE_PROJECTS_DIR` wins (a test, or another install); otherwise
/// `~/.claude/projects`, the measured location. Absence is reported by [`Source::open`] as
/// `NoDirectory`, with this path in the sentence.
pub fn default_projects_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CLAUDE_PROJECTS_DIR") {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".claude/projects"))
}

#[cfg(test)]
mod tests;
