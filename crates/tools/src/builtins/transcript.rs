//! `transcript` — read this session's own conversation out of the store.
//!
//! **The tool `harness` deliberately did not carry, and the reason it gave is
//! exactly the case this one exists for.** That file says:
//!
//! > The transcript. A model asking to re-read its own conversation would be
//! > spending context to recover context it already has, and the rows it cannot
//! > see are the ones compaction deliberately replaced.
//!
//! Both halves were right about a session that has not compacted. After a
//! compaction they invert: the rows are no longer context the model already has
//! — they were folded into a summary — and the store is the only place they
//! still exist. The operator's ask, 2026-09-19: *"I should be able to point it at
//! the transcript, 'looks like it was compacted, look at the history'"*.
//!
//! So this tool is not a way to re-read what is on screen. It is the door to what
//! is **not** on screen any more, and its default reflects that: `history` is on,
//! and the search walks the fork chain backwards through every transcript the
//! session has had.
//!
//! # Find, not SQL
//!
//! The operator's other half: *"we need a standard tool to read transcript, so
//! the model won't be fiddling with paths, files, sql and what not… something
//! like `transcript "blabla"` which behaves similar to find, including limits"*.
//!
//! This is what a session does without it, caught by the operator on 2026-09-19
//! while this file was being written — *"look how letibot is doing it now, a
//! disaster"*:
//!
//! ```text
//! bash {"command": "sqlite3 /home/dead/.local/share/letibot/sessions.db \"SELECT
//!   seq, kind, substr(item_json,1,120) FROM transcript_item WHERE
//!   transcript_id='s-1789639478142928813#t7' ORDER BY seq DESC LIMIT 15\""}
//! ```
//!
//! Four things wrong with it, and each one is an argument of this tool:
//!
//! - **the store path is hardcoded**, so the call is wrong on any other box and
//!   on any session opened with a different `--store`;
//! - **`substr(item_json, 1, 120)`** returns 120 bytes of JSON — mostly the tag
//!   and the field names — where what was wanted was the row's text;
//! - **`transcript_id='…#t7'`** had to be found first, and `#t7` is one
//!   generation of a fork chain: the rows a compaction moved are in `#t6` and
//!   below, which is exactly where the answer usually is;
//! - **`ORDER BY seq DESC`** reads the conversation backwards.
//!
//! It also needs `bash`, which means it needs the gate, which means on a confined
//! seat it does not run at all. A read of one's own conversation should not be an
//! exec.
//!
//! Every argument is a **predicate**, and predicates AND together the way find's
//! do. `match` is the one positional idea — the text to look for — and the rest
//! narrow it:
//!
//! | argument | narrows to |
//! |---|---|
//! | `match` | rows whose text contains it (case-insensitive) |
//! | `kind` | `user`, `assistant`, `reasoning`, `tool_result`, `system`, `mark` |
//! | `tool` | `tool_result` rows from one tool |
//! | `last` | the newest N rows of the chain |
//! | `from` / `to` | a `seq` range in the current transcript |
//! | `session` | another session, by id or by part of its title |
//!
//! And `limit`, which is find's `| head` made explicit, because the alternative
//! is a tool that can return a 200k-token conversation into a context that has
//! room for none of it.
//!
//! # What a sweep costs, and the way out of paying it
//!
//! A transcript search is the one read in this tool set whose matches are
//! routinely larger than the window they are being read into. So the rows go
//! through the spiller like any other oversized output — and the notice that
//! comes back names **two** outs, not one: `read_spill` for the bytes, and
//! `digest` for an answer. The operator: *"we can do a summarization of all
//! findings by streaming them to a subagent… and offer it for big matches"*.
//!
//! That offer is not decoration. A model that has just been told "412 rows
//! matched, 18 shown" will otherwise page through them with `offset`, and paging
//! a conversation into a context to find one fact about it is the exact cost this
//! tool was built to avoid.
//!
//! # Read-only, and about this session by default
//!
//! Naming another session is allowed — the operator's own workflow is *"check
//! letibot letibot session"*, one session looking at another's history — and
//! nothing here can write. A transcript is append-only at the store level
//! (§18: no-UPDATE, no-DELETE triggers), so there is no write surface to expose
//! even if one were wanted.

use std::sync::Arc;

use serde_json::Value;

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// One transcript row, as a reader sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptRow {
    pub transcript_id: String,
    pub seq: u32,
    /// The serde tag: `user`, `assistant`, `reasoning`, `tool_result`, `system`,
    /// `mark`.
    pub kind: String,
    /// For a `tool_result`, the tool that produced it. Empty otherwise.
    pub tool: String,
    /// The row's visible text — what a renderer would show, not the JSON.
    pub text: String,
    pub created_ms: i64,
    /// **How far back this row is from the conversation running now.** `0` is the
    /// live transcript; `1` its parent; and so on up the fork chain.
    ///
    /// Anything above `0` is history the current conversation no longer carries,
    /// which after a compaction is precisely what somebody is looking for. A row
    /// found at generation 2 is not a row the model forgot — it is a row the
    /// model was never given.
    pub generation: u32,
}

/// One transcript in a session's chain, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainLink {
    pub transcript_id: String,
    pub generation: u32,
    pub rows: u32,
    /// Where this transcript was cut from its parent, for a fork. `None` for the
    /// first transcript a session ever had.
    pub forked_at_seq: Option<u32>,
}

/// What one `transcript` call asked for. Every field is a predicate; they AND.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptQuery {
    /// `None` is this session. Otherwise a session id, or part of a title.
    pub session: Option<String>,
    /// Case-insensitive substring. `None` matches every row.
    pub matching: Option<String>,
    /// Empty matches every kind.
    pub kinds: Vec<String>,
    /// For `tool_result` rows: the tool's name.
    pub tool: Option<String>,
    pub from_seq: Option<u32>,
    pub to_seq: Option<u32>,
    /// The newest N rows of the chain, after the other predicates.
    pub last: Option<u32>,
    /// Walk the fork chain into earlier transcripts. On by default: the rows
    /// worth asking a tool for are usually the ones compaction took away.
    pub history: bool,
    /// How many rows to return, after everything else. Find's `| head`.
    pub limit: usize,
    /// Skip this many matches first, so a second call continues the first.
    pub offset: usize,
}

/// What a search found. `matched` counts before `limit` and `offset` apply,
/// because "18 of 412" is the fact that decides what the caller does next.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptHits {
    pub rows: Vec<TranscriptRow>,
    pub session_id: String,
    pub session_title: String,
    pub scanned: usize,
    pub matched: usize,
    pub chain: Vec<ChainLink>,
}

/// One session, for the listing a miss falls back to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub id: String,
    pub title: String,
    pub rows: u32,
    pub last_activity_ms: i64,
    /// True for the session this call is running in.
    pub current: bool,
}

/// The store, as this tool needs it. The daemon implements it over `Store`; the
/// trait lives here so `letibot-tools` keeps depending on nothing above it —
/// the same seam `harness` uses for `HarnessFacts` and `task` for `TaskRunner`.
pub trait TranscriptSource: Send + Sync {
    /// Sessions in the store, newest activity first.
    fn sessions(&self) -> Vec<SessionRow>;
    /// Run a query. `Err` is a store failure, never a miss — a miss is
    /// `TranscriptHits` with no rows and a chain that says what was searched.
    fn find(&self, q: &TranscriptQuery) -> Result<TranscriptHits, String>;
}

/// The default: no store behind it. It refuses by name rather than pretending a
/// session has no history, which is a different and much worse answer.
pub struct NoTranscript;

impl TranscriptSource for NoTranscript {
    fn sessions(&self) -> Vec<SessionRow> {
        Vec::new()
    }
    fn find(&self, _q: &TranscriptQuery) -> Result<TranscriptHits, String> {
        Err("this session has no store, so nothing was written down to search. A \
             session started without `--store` keeps its conversation only in memory, \
             and when it ends the rows are gone."
            .into())
    }
}

pub struct TranscriptTool {
    src: Arc<dyn TranscriptSource>,
}

impl TranscriptTool {
    pub fn new(src: Arc<dyn TranscriptSource>) -> Self {
        TranscriptTool { src }
    }
}

/// Rows per call when the caller does not say. Small on purpose: the common ask
/// is "find the bit about X", and a tool that answers it with forty rows has
/// spent the context the answer was for.
const DEFAULT_LIMIT: usize = 12;
/// The most any one call will return, however large a `limit` is asked for.
const MAX_LIMIT: usize = 200;
/// Per-row excerpt. A tool result can be a megabyte; the row is here to be
/// recognised, and `read_spill` or a `from`/`to` re-read gets the whole of one.
const EXCERPT: usize = 600;

/// The kinds a caller may name, and what each is.
const KINDS: &[(&str, &str)] = &[
    ("user", "what the operator said"),
    ("assistant", "what the model said, and the calls it made"),
    ("reasoning", "the model's thinking blocks"),
    ("tool_result", "what a tool returned"),
    ("system", "the system turn and anything injected into it"),
    ("mark", "segment delimiters, which render to nothing"),
];

impl Tool for TranscriptTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "transcript",
            "SEARCH THIS SESSION'S OWN CONVERSATION, including the parts compaction \
             replaced. Use it when the operator refers to something earlier that you \
             cannot see — \"look at the history\", \"we discussed this before it \
             compacted\", \"what did I say about X\" — instead of guessing or asking \
             them to repeat it. Arguments are predicates and they AND together, like \
             `find`: `match` (case-insensitive text), `kind`, `tool`, `last`, \
             `from`/`to`, `session`, and `limit`. By default it searches this session \
             and walks back through every transcript it has had, so a row a \
             compaction folded away is still findable. Read-only.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "match": {
                        "type": "string",
                        "description": "Text to look for, case-insensitive. Omit to list rows by the other predicates."
                    },
                    "kind": {
                        "type": "string",
                        "description": "One kind, or several comma-separated: user, assistant, reasoning, tool_result, system, mark."
                    },
                    "tool": {
                        "type": "string",
                        "description": "For tool_result rows: which tool produced them."
                    },
                    "last": {
                        "type": "integer",
                        "description": "Only the newest N rows of the conversation."
                    },
                    "from": {"type": "integer", "description": "First seq in the current transcript."},
                    "to": {"type": "integer", "description": "Last seq in the current transcript."},
                    "session": {
                        "type": "string",
                        "description": "Another session, by id or part of its title. Defaults to this one."
                    },
                    "history": {
                        "type": "boolean",
                        "description": "Walk back into transcripts this session forked from. Default true — that is where compacted rows live."
                    },
                    "limit": {"type": "integer", "description": "How many rows to return. Default 12."},
                    "offset": {"type": "integer", "description": "Skip this many matches, to continue a previous call."},
                    "what": {
                        "type": "string",
                        "enum": ["rows", "sessions", "chain"],
                        "description": "`rows` (the default) searches; `sessions` lists what is in the store; `chain` reports this session's transcripts and where each was forked."
                    }
                }
            }),
            Access::Read,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let what = args.get("what").and_then(|v| v.as_str()).unwrap_or("rows");
        match what {
            "sessions" => self.sessions(),
            "chain" => self.chain(args),
            "rows" => self.rows(args),
            other => Invocation::failed(
                format!("`{other}` is not something transcript reports"),
                "There are three: `rows` (the default — search the conversation), \
                 `sessions` (what is in the store) and `chain` (this session's \
                 transcripts, and where each was forked from the one before)."
                    .to_string(),
            ),
        }
    }
}

impl TranscriptTool {
    fn query(&self, args: &Value) -> TranscriptQuery {
        let s = |k: &str| {
            args.get(k)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let n = |k: &str| args.get(k).and_then(|v| v.as_u64()).map(|v| v as u32);
        TranscriptQuery {
            session: s("session"),
            matching: s("match"),
            kinds: s("kind")
                .map(|k| {
                    k.split(',')
                        .map(|p| p.trim().to_ascii_lowercase())
                        .filter(|p| !p.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            tool: s("tool"),
            from_seq: n("from"),
            to_seq: n("to"),
            last: n("last"),
            // On unless it is switched off: the rows worth a tool call are usually
            // the ones the current transcript no longer has.
            history: args
                .get("history")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            limit: args
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize)
                .unwrap_or(DEFAULT_LIMIT)
                .clamp(1, MAX_LIMIT),
            offset: args
                .get("offset")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize,
        }
    }

    fn sessions(&self) -> Invocation {
        let all = self.src.sessions();
        if all.is_empty() {
            return Invocation::ok(
                "the store holds no sessions. That is the store being empty, not this \
                 session being unrecorded — if a turn has run here, it has rows."
                    .to_string(),
            );
        }
        let mut out = format!("{} session(s) in the store, newest activity first:\n\n", all.len());
        for s in &all {
            out.push_str(&format!(
                "  {}{}  {} rows  {}\n",
                if s.current { "* " } else { "  " },
                s.id,
                s.rows,
                if s.title.is_empty() { "(unnamed)" } else { &s.title }
            ));
        }
        out.push_str("\n`*` is this session. Pass `session=` an id, or part of a title.\n");
        Invocation::ok(out)
    }

    fn chain(&self, args: &Value) -> Invocation {
        let mut q = self.query(args);
        q.history = true;
        // A chain report is about the transcripts, not their rows; asking for none
        // keeps a `chain` call from dragging a conversation through the spiller.
        q.limit = 1;
        let hits = match self.src.find(&q) {
            Ok(h) => h,
            Err(e) => return self.store_miss(&q, e),
        };
        if hits.chain.is_empty() {
            return Invocation::ok(format!(
                "session {} has no transcript yet, so there is no chain. A session gets \
                 its first transcript when its first turn runs.",
                hits.session_id
            ));
        }
        let mut out = format!(
            "session {}{}: {} transcript(s), newest first.\n\n",
            hits.session_id,
            if hits.session_title.is_empty() {
                String::new()
            } else {
                format!(" ({})", hits.session_title)
            },
            hits.chain.len()
        );
        for l in &hits.chain {
            out.push_str(&format!(
                "  gen {}  {}  {} rows{}\n",
                l.generation,
                l.transcript_id,
                l.rows,
                match l.forked_at_seq {
                    Some(s) => format!("  — forked from the one below at seq {s}"),
                    None => "  — the first, forked from nothing".to_string(),
                }
            ));
        }
        if hits.chain.len() > 1 {
            out.push_str(
                "\nA fork is what a compaction leaves behind: generation 0 is the \
                 conversation running now, and everything above it is history this \
                 conversation no longer carries. `transcript` searches all of them \
                 unless you pass `history=false`.\n",
            );
        }
        Invocation::ok(out)
    }

    fn rows(&self, args: &Value) -> Invocation {
        let q = self.query(args);
        let hits = match self.src.find(&q) {
            Ok(h) => h,
            Err(e) => return self.store_miss(&q, e),
        };
        if hits.matched == 0 {
            return self.miss(&q, &hits);
        }
        let mut out = format!(
            "{} row(s) matched{}, {} shown{}. Searched {} row(s) across {} transcript(s) \
             of session {}.\n",
            hits.matched,
            describe(&q),
            hits.rows.len(),
            if q.offset > 0 {
                format!(" from offset {}", q.offset)
            } else {
                String::new()
            },
            hits.scanned,
            hits.chain.len(),
            hits.session_id,
        );
        // Generation is the fact a caller most often needs and least often thinks
        // to ask for, so it is named once up front rather than only in the rows.
        if hits.rows.iter().any(|r| r.generation > 0) {
            out.push_str(
                "Rows marked `gen N` with N above 0 are from a transcript this \
                 conversation forked away from — history a compaction replaced, which \
                 is not in the model's context and was never dropped by it.\n",
            );
        }
        out.push('\n');
        for r in &hits.rows {
            out.push_str(&render_row(r));
            out.push('\n');
        }
        // The offer, at the bottom where it is read after the rows rather than
        // instead of them.
        let shown = q.offset + hits.rows.len();
        if hits.matched > shown {
            out.push_str(&format!(
                "\n{} more match(es). `offset={}` reads the next page — but if what you \
                 want is an ANSWER rather than the rows, `digest` streams all {} of them \
                 to a subagent and returns what it found, which costs you the answer \
                 instead of the conversation.\n",
                hits.matched - shown,
                shown,
                hits.matched,
            ));
        }
        Invocation::ok(out)
    }

    /// A store that could not be asked. Clause 1: say what is missing and what
    /// still works, never an empty answer that reads like "no history".
    fn store_miss(&self, q: &TranscriptQuery, why: String) -> Invocation {
        let all = self.src.sessions();
        let mut hint = String::new();
        if let Some(want) = &q.session {
            hint.push_str(&format!("`session={want}` was what this call asked for. "));
        }
        if all.is_empty() {
            hint.push_str("No session in the store can be listed either.");
        } else {
            hint.push_str(&format!("The store does hold {} session(s):\n", all.len()));
            for s in all.iter().take(10) {
                hint.push_str(&format!(
                    "  {}{}  {} rows  {}\n",
                    if s.current { "* " } else { "  " },
                    s.id,
                    s.rows,
                    if s.title.is_empty() { "(unnamed)" } else { &s.title }
                ));
            }
        }
        Invocation::failed(why, hint)
    }

    /// Nothing matched. Clause 1 again: a miss produces more than a hit, and every
    /// extra byte is something to act on without another guess.
    fn miss(&self, q: &TranscriptQuery, hits: &TranscriptHits) -> Invocation {
        let mut out = format!(
            "nothing matched{}. {} row(s) were searched across {} transcript(s) of \
             session {}, so the search ran — this is an empty result, not a failure.\n",
            describe(q),
            hits.scanned,
            hits.chain.len(),
            hits.session_id,
        );
        if hits.scanned == 0 {
            out.push_str(
                "\nNo rows at all: this session has not written a transcript yet, or \
                 the predicates excluded every one before the text was looked at.\n",
            );
        }
        // Which predicate is the likely culprit, named rather than left to a
        // second guess.
        if !q.kinds.is_empty() {
            out.push_str(&format!(
                "\n`kind={}` narrowed this. Dropping it searches every kind. The kinds \
                 are:\n",
                q.kinds.join(",")
            ));
            for (k, meaning) in KINDS {
                out.push_str(&format!("  {k} — {meaning}\n"));
            }
        }
        if q.tool.is_some() {
            out.push_str(
                "\n`tool=` only ever matches `tool_result` rows. A tool named in the \
                 model's own words is an `assistant` row, and a tool named by the \
                 operator is a `user` row.\n",
            );
        }
        if !q.history {
            out.push_str(
                "\n`history=false` kept this to the transcript running now. If the \
                 conversation compacted, what you are looking for is in an earlier \
                 one — drop the flag, or call with `what=chain` to see them.\n",
            );
        } else if hits.chain.len() > 1 {
            out.push_str(&format!(
                "\nAll {} transcripts in the chain were searched, so the text is not in \
                 this session's history under that spelling. A shorter `match` catches \
                 more: the search is a plain case-insensitive substring, not a regex, \
                 so punctuation and line breaks in the middle of a phrase will miss.\n",
                hits.chain.len()
            ));
        } else {
            out.push_str(
                "\nThe search is a plain case-insensitive substring, not a regex. A \
                 shorter `match` catches more.\n",
            );
        }
        if q.session.is_none() {
            let others = self.src.sessions();
            if others.len() > 1 {
                out.push_str(&format!(
                    "\nThis searched only the current session. There are {} others in \
                     the store; `what=sessions` lists them, and `session=` names one.\n",
                    others.len() - 1
                ));
            }
        }
        // A miss returns the shape of what IS there, so the next call is informed
        // rather than another guess.
        Invocation::ok(out)
    }
}

/// The predicates, in the sentence that reports what was searched for. Built once
/// so the hit line and the miss line cannot describe the same call differently.
fn describe(q: &TranscriptQuery) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(m) = &q.matching {
        parts.push(format!("`{m}`"));
    }
    if !q.kinds.is_empty() {
        parts.push(format!("kind {}", q.kinds.join("/")));
    }
    if let Some(t) = &q.tool {
        parts.push(format!("from tool `{t}`"));
    }
    if let Some(n) = q.last {
        parts.push(format!("in the last {n} rows"));
    }
    match (q.from_seq, q.to_seq) {
        (Some(a), Some(b)) => parts.push(format!("seq {a}..{b}")),
        (Some(a), None) => parts.push(format!("seq {a} onwards")),
        (None, Some(b)) => parts.push(format!("up to seq {b}")),
        (None, None) => {}
    }
    if parts.is_empty() {
        return String::new();
    }
    format!(" for {}", parts.join(", "))
}

/// One row, headed by everything needed to ask for more of it.
fn render_row(r: &TranscriptRow) -> String {
    let head = format!(
        "[gen {} seq {} {}{}]",
        r.generation,
        r.seq,
        r.kind,
        if r.tool.is_empty() {
            String::new()
        } else {
            format!(" {}", r.tool)
        }
    );
    let body = excerpt(&r.text, EXCERPT);
    format!("{head}\n{body}\n")
}

/// Head and tail, so a long row keeps both the thing that matched near the start
/// and the thing that concluded it — and says how much went missing rather than
/// trailing off into an ellipsis that could mean anything.
fn excerpt(text: &str, cap: usize) -> String {
    let t = text.trim();
    if t.len() <= cap {
        return t.to_string();
    }
    let half = cap / 2;
    let head: String = t.chars().take(half).collect();
    let tail: String = {
        let all: Vec<char> = t.chars().collect();
        all[all.len().saturating_sub(half)..].iter().collect()
    };
    format!(
        "{head}\n… {} bytes omitted …\n{tail}",
        t.len().saturating_sub(head.len() + tail.len())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        rows: Vec<TranscriptRow>,
        sessions: Vec<SessionRow>,
    }

    impl TranscriptSource for Fake {
        fn sessions(&self) -> Vec<SessionRow> {
            self.sessions.clone()
        }
        fn find(&self, q: &TranscriptQuery) -> Result<TranscriptHits, String> {
            let mut rows: Vec<TranscriptRow> = self
                .rows
                .iter()
                .filter(|r| q.history || r.generation == 0)
                .filter(|r| q.kinds.is_empty() || q.kinds.contains(&r.kind))
                .filter(|r| match &q.matching {
                    None => true,
                    Some(m) => r.text.to_lowercase().contains(&m.to_lowercase()),
                })
                .cloned()
                .collect();
            let matched = rows.len();
            rows = rows.into_iter().skip(q.offset).take(q.limit).collect();
            Ok(TranscriptHits {
                rows,
                session_id: "s1".into(),
                session_title: "the session".into(),
                scanned: self.rows.len(),
                matched,
                chain: vec![
                    ChainLink {
                        transcript_id: "s1#t1".into(),
                        generation: 0,
                        rows: 2,
                        forked_at_seq: Some(4),
                    },
                    ChainLink {
                        transcript_id: "s1#t0".into(),
                        generation: 1,
                        rows: 4,
                        forked_at_seq: None,
                    },
                ],
            })
        }
    }

    fn row(generation: u32, seq: u32, kind: &str, text: &str) -> TranscriptRow {
        TranscriptRow {
            transcript_id: format!("s1#t{generation}"),
            seq,
            kind: kind.into(),
            tool: String::new(),
            text: text.into(),
            created_ms: 0,
            generation,
        }
    }

    fn fake() -> Arc<Fake> {
        Arc::new(Fake {
            rows: vec![
                row(0, 0, "user", "what did we decide about the gate"),
                row(0, 1, "assistant", "the gate fails closed"),
                row(1, 7, "user", "the oracle should reach unknown intents"),
                row(1, 8, "assistant", "covers() filters Intent::Unknown"),
            ],
            sessions: vec![
                SessionRow {
                    id: "s1".into(),
                    title: "the session".into(),
                    rows: 6,
                    last_activity_ms: 2,
                    current: true,
                },
                SessionRow {
                    id: "s2".into(),
                    title: "another".into(),
                    rows: 3,
                    last_activity_ms: 1,
                    current: false,
                },
            ],
        })
    }

    fn result(src: Arc<dyn TranscriptSource>, args: Value) -> crate::ToolResult {
        let mut reg = crate::runtime::Registry::new();
        reg.register(Box::new(TranscriptTool::new(src))).unwrap();
        let d = crate::backend::tempdir::TempDir::new();
        let b = crate::backend::HostBackend::new(d.path()).unwrap();
        let mut rt = crate::runtime::ToolRuntime::new(reg, Box::new(b));
        rt.invoke(
            "t",
            &letibot_transcript::ToolCall {
                id: "c".into(),
                name: "transcript".into(),
                arguments: args.to_string(),
            },
            &mut crate::NullToolSink,
        )
    }

    fn ask(src: Arc<dyn TranscriptSource>, args: Value) -> String {
        result(src, args).payload
    }

    fn call(args: Value) -> String {
        ask(fake(), args)
    }

    /// The whole point: a row that a compaction forked away is still found, and the
    /// answer says it is history rather than presenting it as current context.
    #[test]
    fn a_compacted_row_is_found_and_labelled_as_history() {
        let out = call(serde_json::json!({"match": "oracle"}));
        assert!(out.contains("gen 1"), "{out}");
        assert!(out.contains("unknown intents"), "{out}");
        assert!(
            out.contains("forked away") || out.contains("compaction replaced"),
            "the answer says what generation means: {out}"
        );
    }

    /// `history=false` is the caller saying "only what is live", and a miss then
    /// names that flag as the reason rather than reporting no history.
    #[test]
    fn history_off_misses_and_the_miss_names_the_flag() {
        let out = call(serde_json::json!({"match": "oracle", "history": false}));
        assert!(out.contains("nothing matched"), "{out}");
        assert!(out.contains("`history=false`"), "{out}");
        assert!(out.contains("what=chain"), "the miss offers the chain: {out}");
    }

    /// Predicates AND, the way find's do.
    #[test]
    fn kind_and_match_narrow_together() {
        let out = call(serde_json::json!({"match": "gate", "kind": "assistant"}));
        assert!(out.contains("fails closed"), "{out}");
        assert!(!out.contains("what did we decide"), "the user row is out: {out}");
    }

    /// A miss on a kind lists the kinds there are — clause 1, so the retry is
    /// informed rather than a second guess.
    #[test]
    fn a_kind_that_matches_nothing_lists_the_kinds() {
        let out = call(serde_json::json!({"match": "gate", "kind": "reasoning"}));
        assert!(out.contains("nothing matched"), "{out}");
        assert!(out.contains("tool_result —"), "{out}");
    }

    /// The operator's rule for big matches: the notice offers `digest`, not just
    /// another page.
    #[test]
    fn a_match_bigger_than_the_limit_offers_digest() {
        let out = call(serde_json::json!({"limit": 1}));
        assert!(out.contains("more match"), "{out}");
        assert!(out.contains("offset=1"), "{out}");
        assert!(
            out.contains("`digest` streams"),
            "the answer-shaped way out is offered: {out}"
        );
    }

    /// The chain is the fork history, and it says what a fork means.
    #[test]
    fn the_chain_reports_both_transcripts_and_where_the_fork_cut() {
        let out = call(serde_json::json!({"what": "chain"}));
        assert!(out.contains("gen 0"), "{out}");
        assert!(out.contains("gen 1"), "{out}");
        assert!(out.contains("seq 4"), "{out}");
        assert!(out.contains("compaction leaves behind"), "{out}");
    }

    #[test]
    fn sessions_lists_the_store_and_marks_this_one() {
        let out = call(serde_json::json!({"what": "sessions"}));
        assert!(out.contains("* s1"), "{out}");
        assert!(out.contains("s2"), "{out}");
    }

    /// No store is a refusal by name, never an empty answer — "no history" and
    /// "nowhere to look" are different facts and only one of them is about the
    /// conversation.
    #[test]
    fn no_store_refuses_by_name() {
        let r = result(Arc::new(NoTranscript), serde_json::json!({"match": "x"}));
        let why = match &r.outcome {
            letibot_transcript::ToolOutcome::Failed { reason } => reason.clone(),
            other => panic!("a missing store is a failure, not {other:?}"),
        };
        assert!(why.contains("no store"), "{why}");
        // And the payload still says what CAN be reached, so the next call is not
        // another guess at the same door.
        assert!(
            r.payload.contains("No session in the store"),
            "{}",
            r.payload
        );
    }

    #[test]
    fn a_long_row_is_excerpted_head_and_tail_with_the_omission_counted() {
        let long = "a".repeat(100) + "MIDDLE" + &"z".repeat(100);
        let out = excerpt(&long, 40);
        assert!(out.starts_with("aaaa"), "{out}");
        assert!(out.ends_with("zzzz"), "{out}");
        assert!(out.contains("bytes omitted"), "{out}");
        assert!(!out.contains("MIDDLE"), "the middle is what went: {out}");
    }
}
