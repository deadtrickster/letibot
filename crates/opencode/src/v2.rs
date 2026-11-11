//! opencode 2.x's layout: `session_v2`, `session_message` and `session_part`.
//!
//! A message is one row of `session_message` (its `type` column is the kind, its `data` the message
//! without `id`/`type`), and an assistant message's content is either `session_part` rows keyed by
//! `(message_id, ordinal)` or inline `data.content[]` - some rows carry both, which `withParts`
//! merges parts-first. Everything here reads those two shapes into the same `TranscriptItem`s the v1
//! reader produces, so a conversation reads the same before and after opencode's upgrade.

use std::collections::BTreeMap;

use letibot_transcript::{
    ReasoningField, Speaker, SystemOrigin, ToolCall, ToolOutcome, TranscriptItem, UserPart,
};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::{
    Event, Id, ImportError, Report, Result, Schema, Scrap, SessionMeta, Tokens, emit_row,
    interpret, parse_model, sql_err, str_field, tokens_from,
};

/// How much of a `blob` this build reads into a row. A tool that printed a log should show the log;
/// a megabyte of build artefact should be named, not carried in a transcript.
const BLOB_TEXT_MAX: i64 = 4096;

/// Message kinds that are punctuation rather than words: turn boundaries and switches. Counted in
/// `by_tag` and given neither a row nor a scrap, the way the v1 reader treats `step-start`.
const PUNCTUATION: [&str; 4] = [
    "idle",
    "agent-switched",
    "model-switched",
    "location-switched",
];

/// The tool names opencode's v1->v2 migration renamed (`bash`->`shell`, `task`->`subagent`,
/// `apply_patch`->`patch`, v1-migration.bun.ts:996-1048). Undone here, so one history reads the
/// same rows whichever layout it is stored in and a replayed call keeps the name this head's own
/// tools carry.
fn tool_name(name: &str) -> String {
    match name {
        "shell" => "bash",
        "subagent" => "task",
        "patch" => "apply_patch",
        other => other,
    }
    .to_string()
}

/// A message's `data`, or what stopped it from being read.
enum Body {
    Ok(Value),
    Unreadable(String),
}

impl Body {
    fn parse(kind: &str, raw: &str) -> Body {
        match serde_json::from_str::<Value>(raw) {
            Ok(v) => Body::Ok(upgrade_message(kind, v)),
            Err(e) => Body::Unreadable(e.to_string()),
        }
    }

    /// For a reader that has already said the row could not be read.
    fn value(&self) -> Option<&Value> {
        match self {
            Body::Ok(v) => Some(v),
            Body::Unreadable(_) => None,
        }
    }

    /// Why it could not be read, when it could not.
    fn why(&self) -> &str {
        match self {
            Body::Ok(_) => "",
            Body::Unreadable(e) => e,
        }
    }
}

struct Msg {
    id: String,
    kind: String,
    seq: i64,
    ts: u64,
    body: Body,
}

/// One `session_part` row of an assistant message.
struct Part {
    ordinal: i64,
    ts: u64,
    data: Body,
}

/// One content item of an assistant message, wherever it was stored.
struct Content {
    part_id: String,
    ts: u64,
    data: Body,
}

/// Where the read starts, and what replaced what it skipped. opencode loads a conversation from its
/// latest completed compaction (`history.ts::messageEntries`), so a read that went further back
/// would hand this head's model a history opencode itself had already dropped.
struct Window {
    /// The first message seq kept: the compaction's `metadata.keepID`, or the compaction itself when
    /// it kept nothing (a `keepID` naming a message this session does not have is the same thing).
    from: i64,
    /// The latest completed compaction, when there is one.
    marker: Option<Marker>,
}

struct Marker {
    seq: i64,
    summary: String,
    reason: String,
    /// A provider-native compaction: its replacement window is in `native.messages` and this build
    /// cannot map that onto transcript rows.
    native: Option<usize>,
    /// The compaction named a message it kept, so the tail below this row IS the kept one.
    kept: bool,
    dropped: u64,
}

impl Window {
    fn of(msgs: &[Msg]) -> Window {
        let latest = msgs.iter().rev().find(|m| {
            m.kind == "compaction"
                && m.body
                    .value()
                    .and_then(|d| d.get("status"))
                    .and_then(Value::as_str)
                    == Some("completed")
        });
        let Some(c) = latest else {
            return Window {
                from: i64::MIN,
                marker: None,
            };
        };
        let data = c.body.value();
        let native = data
            .and_then(|d| d.pointer("/native/messages"))
            .and_then(Value::as_array)
            .map(|a| a.len());
        // A native window replaces the history with canonical provider messages; there is nothing
        // here to keep, so nothing before the compaction is imported.
        let keep = data
            .and_then(|d| d.pointer("/metadata/keepID"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let from = match (native.is_some(), keep) {
            (false, Some(id)) => msgs
                .iter()
                .find(|m| m.id == id)
                .map(|m| m.seq)
                .unwrap_or(c.seq),
            _ => c.seq,
        };
        let kept = from < c.seq;
        let dropped = msgs.iter().filter(|m| m.seq < from).count() as u64;
        Window {
            from,
            marker: Some(Marker {
                seq: c.seq,
                summary: data.map(|d| str_field(d, "summary")).unwrap_or_default(),
                reason: data.map(|d| str_field(d, "reason")).unwrap_or_default(),
                native,
                kept,
                dropped,
            }),
        }
    }
}

/// The read's own bookkeeping: the connection, what it has to say, and how far it has got.
struct Reader<'a> {
    conn: &'a Connection,
    report: &'a mut Report,
    sink: &'a mut dyn FnMut(Event),
    total: u64,
    done: u64,
}

impl Reader<'_> {
    fn tick(&mut self) {
        self.done += 1;
        (self.sink)(Event::Progress {
            done: self.done,
            total: self.total,
        });
    }

    fn row(&mut self, source_session: &str, part_id: &str, ts: u64, item: TranscriptItem) {
        emit_row(
            source_session,
            part_id,
            ts,
            item,
            &mut *self.report,
            &mut *self.sink,
        );
    }

    fn scrap(&mut self, source_session: &str, part_id: &str, ts: u64, tag: &str, said: String) {
        self.report.scraps += 1;
        (self.sink)(Event::Scrap(Scrap {
            source_session: source_session.to_string(),
            ts_ms: ts,
            part_id: part_id.to_string(),
            tag: tag.to_string(),
            said,
        }));
    }

    /// A type this build does not read: counted by name and said, never dropped.
    fn unread(&mut self, source_session: &str, part_id: &str, ts: u64, tag: &str, what: &str) {
        *self.report.unknown_tags.entry(tag.to_string()).or_default() += 1;
        self.scrap(
            source_session,
            part_id,
            ts,
            tag,
            format!(
                "a `{what}` this build does not read — counted and left here rather than silently \
                 dropped"
            ),
        );
    }

    fn tag(&mut self, tag: &str) {
        *self.report.by_tag.entry(tag.to_string()).or_default() += 1;
    }
}

/// Read every session of a tree, in tree order, each in its own layout and its own order: `seq` then
/// `ordinal` in v2, `time_created` then id in the frozen v1 rows.
pub(crate) fn read_tree(
    conn: &Connection,
    ids: &[Id],
    report: &mut Report,
    total: u64,
    sink: &mut dyn FnMut(Event),
) -> Result<()> {
    let mut r = Reader {
        conn,
        report,
        sink,
        total,
        done: 0,
    };
    for id in ids {
        match id.schema {
            Schema::V2 => session(&mut r, &id.id)?,
            Schema::V1 => legacy_session(&mut r, &id.id)?,
        }
    }
    Ok(())
}

pub(crate) fn session_meta(conn: &Connection, id: &str) -> Result<SessionMeta> {
    // `title` is nullable in `session_v2` (v1's was NOT NULL), and a legacy row may leave a numeric
    // column null, so every one of them is read as an Option and defaulted.
    let row = conn
        .query_row(
            "SELECT s.id, s.parent_id, s.title, s.directory, s.agent, s.model, s.cost, \
                    s.tokens_input, s.tokens_output, s.tokens_reasoning, s.tokens_cache_read, \
                    s.tokens_cache_write, s.time_created, s.time_updated, p.worktree \
               FROM session_v2 s LEFT JOIN project p ON p.id = s.project_id \
              WHERE s.id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<f64>>(6)?,
                    r.get::<_, Option<i64>>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                    r.get::<_, Option<i64>>(9)?,
                    r.get::<_, Option<i64>>(10)?,
                    r.get::<_, Option<i64>>(11)?,
                    r.get::<_, Option<i64>>(12)?,
                    r.get::<_, Option<i64>>(13)?,
                    r.get::<_, Option<String>>(14)?,
                ))
            },
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                ImportError::NoSuchSession { id: id.to_string() }
            }
            other => ImportError::Sql(other.to_string()),
        })?;

    // opencode stores `model` two ways across versions: a bare string, or `{providerID, modelID}`
    // in v1 and `{id, providerID, variant}` in v2. All three are read by the same parser.
    let (provider, model) = parse_model(row.5.as_deref());
    Ok(SessionMeta {
        id: row.0,
        parent_id: row.1,
        title: row.2.unwrap_or_default(),
        directory: row.3,
        worktree: row.14,
        provider,
        model,
        agent: row.4,
        cost: row.6.unwrap_or(0.0),
        tokens: Tokens {
            input: row.7.unwrap_or(0).max(0) as u64,
            output: row.8.unwrap_or(0).max(0) as u64,
            reasoning: row.9.unwrap_or(0).max(0) as u64,
            cache_read: row.10.unwrap_or(0).max(0) as u64,
            cache_write: row.11.unwrap_or(0).max(0) as u64,
        },
        time_created: row.12.unwrap_or(0).max(0) as u64,
        time_updated: row.13.unwrap_or(0).max(0) as u64,
    })
}

pub(crate) fn directory(conn: &Connection, id: &str) -> Result<String> {
    conn.query_row(
        "SELECT directory FROM session_v2 WHERE id = ?1",
        [id],
        |r| r.get::<_, String>(0),
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => ImportError::NoSuchSession { id: id.to_string() },
        other => ImportError::Sql(other.to_string()),
    })
}

/// One session stored in v2, message by message in `seq` order.
fn session(r: &mut Reader<'_>, session: &str) -> Result<()> {
    let conn = r.conn;
    let msgs = messages(conn, session)?;
    let mut parts = parts(conn, session)?;
    let window = Window::of(&msgs);

    for msg in &msgs {
        // Every message counts against the denominator, the skipped ones too: it was read, and the
        // summary of what it held is the marker's line.
        r.tick();
        if msg.seq < window.from {
            continue;
        }
        match msg.kind.as_str() {
            "assistant" => {
                let rows = parts.remove(&msg.id).unwrap_or_default();
                assistant(r, msg, rows);
            }
            "user" => user(r, msg),
            "compaction" => compaction(r, msg, &window),
            kind if PUNCTUATION.contains(&kind) => r.tag(kind),
            kind => {
                r.tag(kind);
                let said = match msg.body.value() {
                    None => format!("a `{kind}` message whose JSON this build could not read"),
                    Some(data) => {
                        let word = ["text", "command", "description"]
                            .iter()
                            .find_map(|k| data.get(*k).and_then(Value::as_str))
                            .unwrap_or_default();
                        if word.is_empty() {
                            format!("a `{kind}` message this build does not read")
                        } else {
                            format!("a `{kind}` message this build does not read: {word}")
                        }
                    }
                };
                r.unread(session, &msg.id, msg.ts, kind, &said);
            }
        }
    }

    // A part row whose message the session does not have: real, so counted and said, never dropped.
    for (message_id, rows) in parts {
        for p in rows {
            let part_id = format!("{message_id}#{}", p.ordinal);
            r.scrap(
                session,
                &part_id,
                p.ts,
                "<orphan part>",
                format!(
                    "a `session_part` row for message `{message_id}`, which this session does not \
                     have — counted and left here rather than dropped"
                ),
            );
        }
    }
    Ok(())
}

/// A session an upgraded database keeps only in its frozen v1 rows. Read with the v1 columns, mapped
/// by the v1 reader, and counted in the same unit as the rest of the tree: one stored message.
fn legacy_session(r: &mut Reader<'_>, session: &str) -> Result<()> {
    let conn = r.conn;
    let messages: Vec<(String, String, i64)> = conn
        .prepare(
            "SELECT id, data, time_created FROM message WHERE session_id = ?1 \
             ORDER BY time_created, id",
        )
        .map_err(sql_err)?
        .query_map([session], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(sql_err)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_err)?;
    let mut parts: BTreeMap<String, Vec<(String, i64, String)>> = BTreeMap::new();
    for row in conn
        .prepare(
            "SELECT message_id, id, time_created, data FROM part WHERE session_id = ?1 \
             ORDER BY time_created, id",
        )
        .map_err(sql_err)?
        .query_map([session], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(sql_err)?
    {
        let (message_id, part_id, ts, data) = row.map_err(sql_err)?;
        parts
            .entry(message_id)
            .or_default()
            .push((part_id, ts, data));
    }

    for (id, msg_data, _ts) in messages {
        r.tick();
        for (part_id, p_ts, data) in parts.remove(&id).unwrap_or_default() {
            interpret(
                session,
                &part_id,
                p_ts.max(0) as u64,
                &msg_data,
                &data,
                &mut *r.report,
                &mut *r.sink,
            );
        }
    }
    // A part whose message is not here: real, so counted and said, never dropped.
    for (message_id, rows) in parts {
        for (part_id, p_ts, _) in rows {
            r.scrap(
                session,
                &part_id,
                p_ts.max(0) as u64,
                "<orphan part>",
                format!(
                    "a `part` row for message `{message_id}`, which this session does not have \
                     — counted and left here rather than dropped"
                ),
            );
        }
    }
    Ok(())
}

fn messages(conn: &Connection, session: &str) -> Result<Vec<Msg>> {
    let mut out = Vec::new();
    for row in conn
        .prepare(
            "SELECT id, type, seq, time_created, data FROM session_message \
             WHERE session_id = ?1 ORDER BY seq, id",
        )
        .map_err(sql_err)?
        .query_map([session], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .map_err(sql_err)?
    {
        let (id, kind, seq, ts, raw) = row.map_err(sql_err)?;
        out.push(Msg {
            body: Body::parse(&kind, &raw),
            id,
            kind,
            seq,
            ts: ts.max(0) as u64,
        });
    }
    Ok(out)
}

/// Assistant content stored as rows, by message, in `ordinal` order.
fn parts(conn: &Connection, session: &str) -> Result<BTreeMap<String, Vec<Part>>> {
    let mut out: BTreeMap<String, Vec<Part>> = BTreeMap::new();
    for row in conn
        .prepare(
            "SELECT message_id, ordinal, time_created, type, data FROM session_part \
             WHERE session_id = ?1 ORDER BY message_id, ordinal",
        )
        .map_err(sql_err)?
        .query_map([session], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .map_err(sql_err)?
    {
        let (message_id, ordinal, ts, kind, raw) = row.map_err(sql_err)?;
        out.entry(message_id).or_default().push(Part {
            ordinal,
            ts: ts.max(0) as u64,
            data: Body::parse(&kind, &raw),
        });
    }
    Ok(out)
}

fn assistant(r: &mut Reader<'_>, msg: &Msg, rows: Vec<Part>) {
    let conn = r.conn;
    r.tag("assistant");
    let data = msg.body.value();
    if let Some(c) = data.and_then(|d| d.get("cost")).and_then(Value::as_f64) {
        r.report.spend.cost += c;
    }
    if let Some(t) = data.and_then(|d| d.get("tokens")) {
        add_tokens(&mut r.report.spend.tokens, t);
    }
    if let Some(e) = data.and_then(|d| d.get("error")) {
        let said = error_sentence(e);
        r.scrap(
            &msg.id,
            &msg.id,
            msg.ts,
            "error",
            format!("opencode's step failed here: {said}"),
        );
    }

    // The rows first, then the inline ones: `history.ts::withParts` merges them that way, because the
    // projector moves a message's content into rows without clearing an older inline copy.
    let mut items: Vec<Content> = Vec::new();
    for p in rows {
        items.push(Content {
            part_id: format!("{}#{}", msg.id, p.ordinal),
            ts: p.ts,
            data: p.data,
        });
    }
    if let Some(inline) = data
        .and_then(|d| d.get("content"))
        .and_then(Value::as_array)
    {
        for (i, v) in inline.iter().enumerate() {
            items.push(Content {
                part_id: format!("{}#c{i}", msg.id),
                ts: msg.ts,
                data: Body::Ok(piece(v.clone())),
            });
        }
    }

    for c in items {
        let tag = match c.data.value() {
            Some(v) => str_field(v, "type"),
            None => {
                r.tag("<unparseable>");
                let why = c.data.why();
                r.scrap(
                    &msg.id,
                    &c.part_id,
                    c.ts,
                    "<unparseable>",
                    format!("a content part whose JSON this build could not read ({why})"),
                );
                continue;
            }
        };
        r.tag(&tag);
        let Some(v) = c.data.value() else { continue };
        match tag.as_str() {
            "text" => {
                let text = str_field(v, "text");
                if !text.is_empty() {
                    r.row(
                        &msg.id,
                        &c.part_id,
                        c.ts,
                        TranscriptItem::Assistant {
                            text,
                            tool_calls: Vec::new(),
                            truncated: false,
                        },
                    );
                }
            }
            "reasoning" => {
                let text = str_field(v, "text");
                if !text.is_empty() {
                    r.row(
                        &msg.id,
                        &c.part_id,
                        c.ts,
                        TranscriptItem::Reasoning {
                            text,
                            field: reasoning_field(v),
                            truncated: false,
                        },
                    );
                }
            }
            "tool" => tool(r, conn, &msg.id, &c.part_id, c.ts, v),
            other => r.unread(&msg.id, &c.part_id, c.ts, other, other),
        }
    }
}

/// A user message: one string of text, and the files beside it.
fn user(r: &mut Reader<'_>, msg: &Msg) {
    let conn = r.conn;
    r.tag("user");
    let Some(data) = msg.body.value() else {
        r.scrap(
            &msg.id,
            &msg.id,
            msg.ts,
            "<unparseable>",
            "a user message whose JSON this build could not read".to_string(),
        );
        return;
    };
    let mut parts = Vec::new();
    let text = str_field(data, "text");
    if !text.is_empty() {
        parts.push(UserPart::Text { text });
    }
    if let Some(files) = data.get("files").and_then(Value::as_array) {
        for f in files {
            parts.push(UserPart::Text {
                text: media_text(
                    conn,
                    &str_field(f, "uri"),
                    &str_field(f, "mime"),
                    &str_field(f, "name"),
                ),
            });
        }
    }
    if parts.is_empty() {
        return;
    }
    r.row(
        &msg.id,
        &msg.id,
        msg.ts,
        TranscriptItem::User {
            // An imported conversation's user message is the person's: nothing this importer reads
            // is the harness's own voice.
            speaker: Speaker::Operator,
            parts,
        },
    );
}

/// A completed compaction, as the row that says what it replaced.
///
/// The choice: the summary and the kept tail, not the whole history with a marker. Our rows are what
/// this head's model is sent, so importing what opencode itself dropped would hand it a context
/// opencode refused; the tail from `metadata.keepID` on is kept as its own rows, where opencode folds
/// its text into the summary, because these rows still carry the tool calls and reasoning it held.
/// Nothing is silently lost: one marker row counts what went and carries the summary word for word.
fn compaction(r: &mut Reader<'_>, msg: &Msg, window: &Window) {
    r.tag("compaction");
    let mine = window.marker.as_ref().filter(|m| m.seq == msg.seq);
    let Some(marker) = mine else {
        // A compaction inside the kept tail, or one that is still running or failed.
        let status = msg
            .body
            .value()
            .map(|d| str_field(d, "status"))
            .unwrap_or_default();
        r.scrap(
            &msg.id,
            &msg.id,
            msg.ts,
            "compaction",
            format!("opencode compacted this conversation here (status: `{status}`)"),
        );
        return;
    };
    let mut said = match marker.native {
        Some(n) => format!(
            "opencode replaced this conversation's window here with a provider-native one \
             ({} message(s), reason: {}). This build cannot map that window onto transcript rows, \
             so nothing before this point is imported.",
            n,
            if marker.reason.is_empty() {
                "unrecorded"
            } else {
                &marker.reason
            }
        ),
        None if marker.kept => format!(
            "opencode compacted this conversation here (reason: {}). The {} message(s) before the \
             first one it kept were folded into the summary below and are not imported; the rows \
             from that message up to this one are the tail it kept verbatim.",
            if marker.reason.is_empty() {
                "unrecorded"
            } else {
                &marker.reason
            },
            marker.dropped
        ),
        None => format!(
            "opencode compacted this conversation here (reason: {}). It kept no message verbatim, \
             so nothing before this point is imported.",
            if marker.reason.is_empty() {
                "unrecorded"
            } else {
                &marker.reason
            },
        ),
    };
    if !marker.summary.is_empty() {
        said.push_str("\n\n");
        said.push_str(&marker.summary);
    }
    r.row(
        &msg.id,
        &msg.id,
        msg.ts,
        TranscriptItem::System {
            text: said,
            // A later change to the conversation's framing, appended: a summary is exactly that,
            // and the transcript model has no way to rewrite what came before it anyway.
            origin: SystemOrigin::Update,
        },
    );
}

/// A tool call: the call row and the result row, as the v1 reader emits them, because a head labels a
/// tool card from the nearest preceding `Assistant { tool_calls }` and a lone result would show an
/// outcome with no command.
fn tool(r: &mut Reader<'_>, conn: &Connection, session: &str, part_id: &str, ts: u64, v: &Value) {
    let name = tool_name(&str_field(v, "name"));
    let id = str_field(v, "id");
    let call_id = if id.is_empty() {
        part_id.to_string()
    } else {
        id
    };
    let state = v.get("state").cloned().unwrap_or(Value::Null);
    let status = str_field(&state, "status");
    let input = match state.get("input") {
        // `streaming` holds the partial JSON as a string; a running or finished call holds an object.
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => "{}".to_string(),
    };
    let failed = state
        .get("error")
        .map(error_sentence)
        .filter(|s| !s.is_empty());
    let mut pieces: Vec<String> = Vec::new();
    if let Some(content) = state.get("content").and_then(Value::as_array) {
        for item in content {
            match item.get("type").and_then(Value::as_str) {
                Some("text") => pieces.push(str_field(item, "text")),
                Some("file") => pieces.push(media_text(
                    conn,
                    &str_field(item, "uri"),
                    &str_field(item, "mime"),
                    &str_field(item, "name"),
                )),
                _ => {}
            }
        }
    }
    let payload = if !pieces.is_empty() {
        pieces.join("\n")
    } else {
        failed.clone().unwrap_or_default()
    };

    r.row(
        session,
        part_id,
        ts,
        TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: call_id.clone(),
                name: name.clone(),
                arguments: input,
            }],
            truncated: false,
        },
    );
    let outcome = match status.as_str() {
        "completed" => ToolOutcome::Ok,
        "error" => ToolOutcome::Failed {
            reason: failed.unwrap_or_else(|| "opencode recorded this call as an error".to_string()),
        },
        other => ToolOutcome::NotRun {
            why: format!("opencode recorded this call as `{other}`"),
        },
    };
    r.row(
        session,
        part_id,
        ts,
        TranscriptItem::ToolResult {
            call_id,
            name,
            outcome,
            payload,
            edits: Vec::new(),
            origin: None,
            // opencode keeps a result file's bytes in `blob`; the payload names it, and a head shows
            // the text a text file held, not the picture a binary one is.
            media: None,
        },
    );
}

/// Which field this provider replays its reasoning into. opencode keeps the replay data in a part's
/// `native`, keyed by the producing provider's metadata key, and names the field there
/// (`reasoningField`, as Moonshot's `reasoning_content` and Z.AI's `reasoning` do). `reasoning_content`
/// is GLM's separate field, which is what [`ReasoningField::ReasoningContent`] means; any other name a
/// provider gives - `reasoning`, `reasoning_text`, `reasoning_details` - is replayed inside the turn,
/// which is [`ReasoningField::Inline`]. A part with no `native` at all is what the v1 reader has
/// always said, and saying anything else would change how a v1-lineage session replays.
fn reasoning_field(part: &Value) -> ReasoningField {
    let Some(native) = part.get("native").and_then(Value::as_object) else {
        return ReasoningField::ReasoningContent;
    };
    let named = native
        .get("reasoningField")
        .and_then(Value::as_str)
        .or_else(|| {
            native
                .values()
                .find_map(|v| v.get("reasoningField").and_then(Value::as_str))
        });
    match named {
        Some("reasoning_content") | None => ReasoningField::ReasoningContent,
        Some(_) => ReasoningField::Inline,
    }
}

fn add_tokens(into: &mut Tokens, t: &Value) {
    let t = tokens_from(t);
    into.input = into.input.saturating_add(t.input);
    into.output = into.output.saturating_add(t.output);
    into.reasoning = into.reasoning.saturating_add(t.reasoning);
    into.cache_read = into.cache_read.saturating_add(t.cache_read);
    into.cache_write = into.cache_write.saturating_add(t.cache_write);
}

/// `{type, message}` in v2 (`session-error.ts`), or the bare string v1 stored.
fn error_sentence(e: &Value) -> String {
    if let Some(s) = e.as_str() {
        return s.to_string();
    }
    let kind = str_field(e, "type");
    let message = str_field(e, "message");
    match (kind.is_empty(), message.is_empty()) {
        (false, false) => format!("{kind}: {message}"),
        (false, true) => kind,
        (true, false) => message,
        (true, true) => String::new(),
    }
}

/// What a stored file is: the text itself when the bytes are text and small, otherwise a sentence
/// naming the mime, the size and the reference. The bytes live in `blob`, under the sha256 a
/// `media:<hash>` uri names.
fn media_text(conn: &Connection, uri: &str, mime: &str, name: &str) -> String {
    let named = if name.is_empty() {
        String::new()
    } else {
        format!("{name} ")
    };
    let Some(hash) = uri.strip_prefix("media:") else {
        // An attachment that was never externalized carries its bytes in the uri itself: named,
        // never pasted, because a base64 picture is megabytes and a row is not the place for it.
        let what = if uri.starts_with("data:") {
            format!("{mime}, inline data not carried")
        } else if uri.is_empty() {
            format!("{mime}, no bytes named")
        } else {
            uri.to_string()
        };
        return format!("[{named}{what}]");
    };
    // A blob this build cannot read is named, not fatal: the reference is still the fact.
    let row: Option<(String, i64, Option<Vec<u8>>)> = conn
        .query_row(
            "SELECT mime, bytes, data FROM blob WHERE hash = ?1",
            [hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sql_err)
        .ok()
        .flatten();
    let placeholder =
        |bmime: &str, bytes: i64| format!("[{named}{bmime}, {bytes} byte(s), {uri}, not read]");
    match row {
        Some((bmime, bytes, Some(data)))
            if is_text_mime(&bmime) && (0..=BLOB_TEXT_MAX).contains(&bytes) =>
        {
            match String::from_utf8(data) {
                Ok(text) => text,
                Err(_) => placeholder(&bmime, bytes),
            }
        }
        Some((bmime, bytes, _)) => placeholder(&bmime, bytes),
        None => format!("[{named}{mime}, {uri}, not in this database's blob table]"),
    }
}

fn is_text_mime(mime: &str) -> bool {
    let mime = mime.split(';').next().unwrap_or(mime).trim();
    mime.starts_with("text/")
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
        || matches!(
            mime,
            "application/json"
                | "application/xml"
                | "application/javascript"
                | "application/x-yaml"
                | "application/yaml"
        )
}

/// The renames `packages/schema/src/session-legacy.ts` applies to every row written before v2.1:
/// `providerState` (and a text or reasoning part's `state`) became `native`, a tool's
/// `providerResultState` folded into it, and a provider compaction's window moved from
/// `providerContext` to `native.messages`. A row written then must read like a current one.
fn upgrade_message(kind: &str, data: Value) -> Value {
    match kind {
        "assistant" => {
            let mut data = rename(data, "providerState", "native");
            if let Some(content) = data.get_mut("content").and_then(Value::as_array_mut) {
                for part in content.iter_mut() {
                    *part = piece(part.clone());
                }
            }
            data
        }
        "compaction" => provider_context(rename(data, "providerState", "native")),
        _ => data,
    }
}

/// Upgrades one assistant content part: a text or reasoning part's `state` is its native data, and a
/// tool's `providerState`/`providerResultState` are (a tool's own `state` is its lifecycle status).
fn piece(part: Value) -> Value {
    match part.get("type").and_then(Value::as_str) {
        Some("text") | Some("reasoning") => rename(part, "state", "native"),
        Some("tool") => {
            let part = rename(part, "providerState", "native");
            let extra = part
                .get("providerResultState")
                .and_then(Value::as_object)
                .cloned();
            let mut part = omit(part, "providerResultState");
            if let Some(extra) = extra
                && let Value::Object(map) = &mut part
            {
                let native = map
                    .get("native")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let native: serde_json::Map<String, Value> =
                    native.into_iter().chain(extra).collect();
                map.insert("native".to_string(), Value::Object(native));
            }
            part
        }
        _ => part,
    }
}

fn provider_context(data: Value) -> Value {
    let Some(context) = data.get("providerContext") else {
        return data;
    };
    let messages = context.get("messages").cloned();
    let mut data = omit(data, "providerContext");
    if let (Some(messages), Value::Object(map)) = (messages, &mut data) {
        map.insert("native".to_string(), {
            let mut native = serde_json::Map::new();
            native.insert("messages".to_string(), messages);
            Value::Object(native)
        });
    }
    data
}

/// The current name wins when a payload carries both, as `session-legacy.ts`'s `rename` does.
fn rename(data: Value, from: &str, to: &str) -> Value {
    let Value::Object(mut map) = data else {
        return data;
    };
    let Some(moved) = map.remove(from) else {
        return Value::Object(map);
    };
    if !map.contains_key(to) {
        map.insert(to.to_string(), moved);
    }
    Value::Object(map)
}

fn omit(data: Value, key: &str) -> Value {
    let Value::Object(mut map) = data else {
        return data;
    };
    map.remove(key);
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::{ImportError, Row, Source};

    /// A throwaway database file, by the crate's convention: a file and not `:memory:`, because the
    /// point is to open it read-only afterwards and an in-memory database has no second opener.
    fn fixture(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "letibot-opencode-v2-{tag}-{}-{}.db",
            std::process::id(),
            {
                use std::sync::atomic::{AtomicU32, Ordering};
                static N: AtomicU32 = AtomicU32::new(0);
                N.fetch_add(1, Ordering::Relaxed)
            }
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// opencode 2.x's tables, as `schema.gen.ts` creates them: the columns this reader touches, with
    /// their real nullability (`title` nullable, `session_part` keyed by `(message_id, ordinal)`).
    fn schema(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT NOT NULL, sandboxes TEXT NOT NULL);
             CREATE TABLE session_v2 (
                id TEXT PRIMARY KEY, project_id TEXT NOT NULL, workspace_id TEXT, parent_id TEXT,
                fork_session_id TEXT, fork_boundary TEXT, slug TEXT NOT NULL, directory TEXT NOT NULL,
                path TEXT, title TEXT, version TEXT NOT NULL, share_url TEXT,
                summary_additions INTEGER, summary_deletions INTEGER, summary_files INTEGER,
                summary_diffs TEXT, metadata TEXT, cost REAL NOT NULL DEFAULT 0,
                tokens_input INTEGER NOT NULL DEFAULT 0, tokens_output INTEGER NOT NULL DEFAULT 0,
                tokens_reasoning INTEGER NOT NULL DEFAULT 0,
                tokens_cache_read INTEGER NOT NULL DEFAULT 0,
                tokens_cache_write INTEGER NOT NULL DEFAULT 0,
                revert TEXT, permission TEXT, agent TEXT, model TEXT,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
                time_busy INTEGER, time_idle INTEGER, time_viewed INTEGER, idle_outcome TEXT,
                time_compacting INTEGER, time_archived INTEGER, time_suspended INTEGER,
                resume_attempts INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE session_message (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL, type TEXT NOT NULL, seq INTEGER NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);
             CREATE TABLE session_part (
                message_id TEXT NOT NULL, session_id TEXT NOT NULL, ordinal INTEGER NOT NULL,
                type TEXT NOT NULL, call_id TEXT, time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL, data TEXT NOT NULL,
                PRIMARY KEY (message_id, ordinal));
             CREATE TABLE blob (
                hash TEXT PRIMARY KEY, mime TEXT NOT NULL, bytes INTEGER NOT NULL,
                width INTEGER, height INTEGER, data BLOB NOT NULL, time_created INTEGER NOT NULL);",
        )
        .unwrap();
    }

    /// opencode 1.x's tables, as an upgraded database still holds them, frozen.
    fn v1_tables(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY, project_id TEXT NOT NULL, parent_id TEXT,
                slug TEXT NOT NULL DEFAULT '', directory TEXT NOT NULL,
                title TEXT NOT NULL DEFAULT '', version TEXT NOT NULL DEFAULT '',
                cost REAL NOT NULL DEFAULT 0,
                tokens_input INTEGER NOT NULL DEFAULT 0,
                tokens_output INTEGER NOT NULL DEFAULT 0,
                tokens_reasoning INTEGER NOT NULL DEFAULT 0,
                tokens_cache_read INTEGER NOT NULL DEFAULT 0,
                tokens_cache_write INTEGER NOT NULL DEFAULT 0,
                agent TEXT, model TEXT,
                time_created INTEGER NOT NULL DEFAULT 0,
                time_updated INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE message (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
                data TEXT NOT NULL);
             CREATE TABLE part (
                id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
                data TEXT NOT NULL);",
        )
        .unwrap();
    }

    fn project(conn: &Connection) {
        conn.execute(
            "INSERT INTO project (id, worktree, sandboxes) VALUES ('p1', '/home/dead/Projects/x', '[]')",
            [],
        )
        .unwrap();
    }

    fn add_session(
        conn: &Connection,
        id: &str,
        parent: Option<&str>,
        title: Option<&str>,
        dir: &str,
    ) {
        conn.execute(
            "INSERT INTO session_v2 (id, project_id, parent_id, slug, directory, title, version, \
                                     agent, model, time_created, time_updated) \
             VALUES (?1, 'p1', ?2, ?3, ?4, ?5, '2.2.7', 'build', \
                     '{\"id\":\"deepseek-v4-pro\",\"providerID\":\"deepseek\"}', 1000, 2000)",
            rusqlite::params![id, parent, format!("slug-{id}"), dir, title],
        )
        .unwrap();
    }

    fn add_message(
        conn: &Connection,
        id: &str,
        session: &str,
        kind: &str,
        seq: i64,
        ts: i64,
        data: &str,
    ) {
        conn.execute(
            "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
            rusqlite::params![id, session, kind, seq, ts, data],
        )
        .unwrap();
    }

    fn add_part(conn: &Connection, message: &str, session: &str, ordinal: i64, data: &str) {
        let kind: String = serde_json::from_str::<Value>(data)
            .ok()
            .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        conn.execute(
            "INSERT INTO session_part (message_id, session_id, ordinal, type, call_id, \
                                       time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, NULL, 100, 100, ?5)",
            rusqlite::params![message, session, ordinal, kind, data],
        )
        .unwrap();
    }

    fn add_v1_session(conn: &Connection, id: &str, dir: &str) {
        conn.execute(
            "INSERT INTO session (id, project_id, directory, title) VALUES (?1, 'p1', ?2, ?3)",
            rusqlite::params![id, dir, format!("title {id}")],
        )
        .unwrap();
    }

    fn add_v1_message(conn: &Connection, id: &str, session: &str, ts: i64, data: &str) {
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?3, ?4)",
            rusqlite::params![id, session, ts, data],
        )
        .unwrap();
    }

    fn add_v1_part(conn: &Connection, id: &str, message: &str, session: &str, ts: i64, data: &str) {
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            rusqlite::params![id, message, session, ts, data],
        )
        .unwrap();
    }

    struct Read {
        rows: Vec<Row>,
        scraps: Vec<String>,
        progress: Vec<(u64, u64)>,
        report: Report,
    }

    fn read(path: &Path, root: &str) -> Read {
        let src = Source::open(path).unwrap();
        let mut rows = Vec::new();
        let mut scraps = Vec::new();
        let mut progress = Vec::new();
        let report = src
            .read_tree(root, &mut |e| match e {
                Event::Row(r) => rows.push(r),
                Event::Scrap(s) => scraps.push(s.said),
                Event::Progress { done, total } => progress.push((done, total)),
            })
            .unwrap();
        Read {
            rows,
            scraps,
            progress,
            report,
        }
    }

    /// The words a row carries, whichever kind it is - what a reader would see.
    fn said(row: &Row) -> String {
        match &row.item {
            TranscriptItem::User { parts, .. } => parts
                .iter()
                .map(|p| match p {
                    UserPart::Text { text } => text.clone(),
                    UserPart::Image { media_type, .. } => format!("<image {media_type}>"),
                    UserPart::FileRef { path, .. } => path.clone(),
                })
                .collect::<Vec<_>>()
                .join(" "),
            TranscriptItem::Assistant { text, .. } => text.clone(),
            TranscriptItem::Reasoning { text, .. } => text.clone(),
            TranscriptItem::System { text, .. } => text.clone(),
            TranscriptItem::ToolResult { payload, .. } => payload.clone(),
            other => format!("{other:?}"),
        }
    }

    fn words(r: &Read) -> Vec<String> {
        r.rows.iter().map(said).collect()
    }

    /// The operator's own install is 1.18.31 and has only v1 tables, so that path must read the same
    /// rows it did before the v2 reader existed - and say it is counting parts.
    #[test]
    fn a_v1_only_database_is_still_read_as_v1() {
        let path = fixture("v1only");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT NOT NULL);
             CREATE TABLE session (
                id TEXT PRIMARY KEY, project_id TEXT NOT NULL, parent_id TEXT,
                slug TEXT NOT NULL DEFAULT '', directory TEXT NOT NULL,
                title TEXT NOT NULL DEFAULT '', version TEXT NOT NULL DEFAULT '',
                cost REAL NOT NULL DEFAULT 0,
                tokens_input INTEGER NOT NULL DEFAULT 0,
                tokens_output INTEGER NOT NULL DEFAULT 0,
                tokens_reasoning INTEGER NOT NULL DEFAULT 0,
                tokens_cache_read INTEGER NOT NULL DEFAULT 0,
                tokens_cache_write INTEGER NOT NULL DEFAULT 0,
                agent TEXT, model TEXT,
                time_created INTEGER NOT NULL DEFAULT 0,
                time_updated INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE message (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);
             CREATE TABLE part (
                id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);",
        )
        .unwrap();
        conn.execute("INSERT INTO project (id, worktree) VALUES ('p1', '/w')", [])
            .unwrap();
        add_v1_session(&conn, "ses_root", "/home/dead");
        add_v1_message(&conn, "m1", "ses_root", 10, r#"{"role":"user"}"#);
        add_v1_part(
            &conn,
            "pt1",
            "m1",
            "ses_root",
            10,
            r#"{"type":"text","text":"hello from 1.18"}"#,
        );
        drop(conn);

        let src = Source::open(&path).unwrap();
        assert_eq!(src.schema(), crate::Schema::V1, "no session_v2, so v1");
        let tree = src.tree("ses_root").unwrap();
        assert_eq!((tree.parts, tree.unit), (1, "part"));
        let r = read(&path, "ses_root");
        assert_eq!(words(&r), vec!["hello from 1.18"]);
        assert!(
            r.report.summary().contains("from 1 part(s)"),
            "{}",
            r.report.summary()
        );
    }

    /// A fresh 2.x install has no v1 tables at all: before this reader, every call failed here with
    /// "no such table: session".
    #[test]
    fn a_fresh_v2_database_is_read_message_by_message() {
        let path = fixture("fresh");
        let conn = Connection::open(&path).unwrap();
        schema(&conn);
        project(&conn);
        add_session(&conn, "ses_root", None, Some("a title"), "/home/dead");
        add_session(
            &conn,
            "ses_child",
            Some("ses_root"),
            Some("child"),
            "/home/dead",
        );
        // Inserted out of `seq` order on purpose: `seq` is the order, not the id and not the clock.
        add_message(
            &conn,
            "msg_2",
            "ses_root",
            "user",
            2,
            20,
            r#"{"text":"hello"}"#,
        );
        add_message(
            &conn,
            "msg_3",
            "ses_root",
            "assistant",
            3,
            30,
            r#"{"content":[{"type":"text","text":"hi back"}]}"#,
        );
        add_message(
            &conn,
            "msg_1",
            "ses_root",
            "user",
            1,
            10,
            r#"{"text":"first"}"#,
        );
        drop(conn);

        let src = Source::open(&path).unwrap();
        assert_eq!(
            src.schema(),
            crate::Schema::V2,
            "session_v2 alone is a v2 database"
        );
        let tree = src.tree("ses_root").unwrap();
        assert_eq!(tree.sessions(), 2, "the child is in the tree");
        assert_eq!((tree.parts, tree.unit), (3, "message"));
        assert_eq!(tree.root.title, "a title");
        assert_eq!(src.directory("ses_root").unwrap(), "/home/dead");
        // `session_v2.model` is `{id, providerID, variant}`, and rule 1's origin names both.
        assert!(
            tree.root.origin().contains("deepseek/deepseek-v4-pro"),
            "{}",
            tree.root.origin()
        );
        assert_eq!(
            tree.unit_plural(),
            "messages",
            "the head's word after the counts"
        );

        let r = read(&path, "ses_root");
        assert_eq!(words(&r), vec!["first", "hello", "hi back"]);
        // One tick per stored message, the denominator known from the start and reached.
        assert_eq!(r.progress.first(), Some(&(1, 3)), "{:?}", r.progress);
        assert_eq!(r.progress.last(), Some(&(3, 3)), "{:?}", r.progress);
        assert_eq!(r.progress.len(), 3);
        assert!(
            r.report
                .summary()
                .contains("imported 3 row(s) from 3 message(s)"),
            "{}",
            r.report.summary()
        );
    }

    /// A file that is not an opencode database at all: said as that, never as a failed query.
    #[test]
    fn a_database_with_neither_layout_is_said_plainly() {
        let path = fixture("neither");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE notes (x TEXT);").unwrap();
        drop(conn);

        let err = match Source::open(&path) {
            Ok(_) => panic!("a database with neither layout must not open"),
            Err(e) => e,
        };
        let said = format!("{err}");
        assert!(matches!(err, ImportError::NeitherSchema { .. }), "{said}");
        assert!(said.contains("neither of opencode's layouts"), "{said}");
        assert!(said.contains("session_v2"), "{said}");
        assert!(said.contains("notes"), "the tables it does have: {said}");
        assert!(!said.contains("no such table"), "{said}");
    }

    /// An upgraded database: sessions the background migration reached are read from v2, a session it
    /// has not reached is read from the frozen v1 rows, and an id in both is read from v2 alone.
    #[test]
    fn an_upgraded_database_reads_v2_where_it_has_the_session_and_v1_where_it_does_not() {
        let path = fixture("upgraded");
        let conn = Connection::open(&path).unwrap();
        schema(&conn);
        v1_tables(&conn);
        project(&conn);

        // Created before the upgrade and not migrated yet: only v1 has it.
        add_v1_session(&conn, "ses_old", "/old-dir");
        add_v1_message(&conn, "m1", "ses_old", 10, r#"{"role":"user"}"#);
        add_v1_part(
            &conn,
            "p1",
            "m1",
            "ses_old",
            10,
            r#"{"type":"text","text":"old history"}"#,
        );
        // Created after it: only v2 has it.
        add_session(&conn, "ses_new", None, Some("new"), "/new-dir");
        add_message(
            &conn,
            "msg_a",
            "ses_new",
            "user",
            1,
            10,
            r#"{"text":"new history"}"#,
        );
        // In both, and v2 is longer: v1 rows are frozen at the upgrade.
        add_v1_session(&conn, "ses_both", "/both-v1");
        add_v1_message(&conn, "m2", "ses_both", 10, r#"{"role":"user"}"#);
        add_v1_part(
            &conn,
            "p2",
            "m2",
            "ses_both",
            10,
            r#"{"type":"text","text":"before the upgrade"}"#,
        );
        add_session(&conn, "ses_both", None, Some("both"), "/both-v2");
        add_message(
            &conn,
            "msg_b",
            "ses_both",
            "user",
            1,
            10,
            r#"{"text":"before the upgrade"}"#,
        );
        add_message(
            &conn,
            "msg_c",
            "ses_both",
            "assistant",
            2,
            20,
            r#"{"content":[{"type":"text","text":"after the upgrade"}]}"#,
        );
        drop(conn);

        let src = Source::open(&path).unwrap();
        assert_eq!(src.schema(), crate::Schema::V2);
        assert_eq!(src.directory("ses_old").unwrap(), "/old-dir");
        assert_eq!(
            src.directory("ses_both").unwrap(),
            "/both-v2",
            "v2 wins for an id it has"
        );

        // The frozen session is still readable, through its own layout.
        let old = read(&path, "ses_old");
        assert_eq!(words(&old), vec!["old history"]);
        assert_eq!(old.report.parts, 1, "one stored v1 message");
        // The continued one is read from v2 alone: v1's row stops at the upgrade, so reading it
        // would cut the session off there.
        let both = read(&path, "ses_both");
        assert_eq!(
            words(&both),
            vec!["before the upgrade", "after the upgrade"],
            "the whole conversation, not the frozen prefix"
        );
        // And the v2-only session is not "no such session" any more.
        assert_eq!(words(&read(&path, "ses_new")), vec!["new history"]);
    }

    /// Both write paths exist in one database: parts as rows, parts inline, and a message with both.
    /// `withParts` merges the rows before the inline copy, and so does this.
    #[test]
    fn inline_content_and_part_rows_are_merged_parts_first() {
        let path = fixture("merge");
        let conn = Connection::open(&path).unwrap();
        schema(&conn);
        project(&conn);
        add_session(&conn, "ses_root", None, Some("t"), "/d");
        add_message(
            &conn,
            "msg_1",
            "ses_root",
            "assistant",
            1,
            10,
            r#"{"content":[{"type":"text","text":"from the row"}]}"#,
        );
        add_part(
            &conn,
            "msg_1",
            "ses_root",
            1,
            r#"{"type":"text","text":"a part"}"#,
        );
        add_part(
            &conn,
            "msg_1",
            "ses_root",
            0,
            r#"{"type":"reasoning","text":"think"}"#,
        );
        // A second message with only part rows: the `experimental.part_rows` shape.
        add_message(
            &conn,
            "msg_2",
            "ses_root",
            "assistant",
            2,
            20,
            r#"{"content":[]}"#,
        );
        add_part(
            &conn,
            "msg_2",
            "ses_root",
            0,
            r#"{"type":"text","text":"rows only"}"#,
        );
        drop(conn);

        let r = read(&path, "ses_root");
        assert_eq!(
            words(&r),
            vec!["think", "a part", "from the row", "rows only"]
        );
        let ids: Vec<&str> = r.rows.iter().map(|r| r.part_id.as_str()).collect();
        assert_eq!(ids, vec!["msg_1#0", "msg_1#1", "msg_1#c0", "msg_2#0"]);
        assert_eq!(r.report.parts, 2, "two stored messages");
    }

    /// A tool call: the name opencode 2.x uses is read back as the v1 one, the text content is the
    /// payload, and a result file's bytes are read when they are text and small - otherwise named.
    #[test]
    fn a_tool_call_carries_its_text_and_names_its_files() {
        let path = fixture("tool");
        let conn = Connection::open(&path).unwrap();
        schema(&conn);
        project(&conn);
        add_session(&conn, "ses_root", None, Some("t"), "/d");
        conn.execute(
            "INSERT INTO blob (hash, mime, bytes, data, time_created) \
             VALUES ('h1', 'text/plain', 8, CAST('log line' AS BLOB), 10), \
                    ('h2', 'image/png', 123456, X'00', 10)",
            [],
        )
        .unwrap();
        add_message(
            &conn,
            "msg_1",
            "ses_root",
            "assistant",
            1,
            10,
            r#"{"content":[{"type":"tool","id":"call_1","name":"shell","state":{
                 "status":"completed","input":{"command":"ls"},
                 "content":[{"type":"text","text":"a\nb"},
                            {"type":"file","uri":"media:h1","mime":"text/plain","name":"out.txt"},
                            {"type":"file","uri":"media:h2","mime":"image/png"}]}}]}"#,
        );
        drop(conn);

        let r = read(&path, "ses_root");
        // Two rows for one call: the call, then the result - as the v1 reader emits them.
        assert_eq!(r.rows.len(), 2);
        let TranscriptItem::Assistant { tool_calls, .. } = &r.rows[0].item else {
            panic!("the first row is the call: {:?}", r.rows[0].item);
        };
        assert_eq!(
            tool_calls[0].name, "bash",
            "opencode 2.x's `shell` is v1's `bash`"
        );
        assert_eq!(tool_calls[0].id, "call_1");
        assert_eq!(tool_calls[0].arguments, r#"{"command":"ls"}"#);
        let TranscriptItem::ToolResult {
            outcome, payload, ..
        } = &r.rows[1].item
        else {
            panic!("the second row is the result: {:?}", r.rows[1].item);
        };
        assert_eq!(*outcome, ToolOutcome::Ok);
        assert_eq!(
            payload, "a\nb\nlog line\n[image/png, 123456 byte(s), media:h2, not read]",
            "text is carried, a file that is not text is named"
        );
    }

    /// An error tool call: v2's error is an object, so the reason names its type and its message.
    #[test]
    fn an_error_tool_call_says_the_type_and_the_message() {
        let path = fixture("toolerr");
        let conn = Connection::open(&path).unwrap();
        schema(&conn);
        project(&conn);
        add_session(&conn, "ses_root", None, Some("t"), "/d");
        add_message(
            &conn,
            "msg_1",
            "ses_root",
            "assistant",
            1,
            10,
            r#"{"content":[{"type":"tool","id":"call_2","name":"apply_patch","state":{
                 "status":"error","input":{"patch":"x"},
                 "error":{"type":"tool.execution","message":"no such file"}}}]}"#,
        );
        drop(conn);

        let r = read(&path, "ses_root");
        let TranscriptItem::Assistant { tool_calls, .. } = &r.rows[0].item else {
            panic!("the first row is the call");
        };
        assert_eq!(tool_calls[0].name, "apply_patch", "already the v1 name");
        let TranscriptItem::ToolResult {
            outcome, payload, ..
        } = &r.rows[1].item
        else {
            panic!("the second row is the result");
        };
        assert_eq!(
            *outcome,
            ToolOutcome::Failed {
                reason: "tool.execution: no such file".to_string()
            }
        );
        assert_eq!(payload, "tool.execution: no such file");
    }

    /// A compaction keeps the summary and the tail it kept verbatim, drops what opencode itself
    /// dropped, and says how many messages went - the choice is argued on `compaction`.
    #[test]
    fn a_compaction_keeps_the_tail_and_says_what_it_replaced() {
        let path = fixture("compaction");
        let conn = Connection::open(&path).unwrap();
        schema(&conn);
        project(&conn);
        add_session(&conn, "ses_root", None, Some("t"), "/d");
        add_message(
            &conn,
            "msg_1",
            "ses_root",
            "user",
            1,
            10,
            r#"{"text":"one"}"#,
        );
        add_message(
            &conn,
            "msg_2",
            "ses_root",
            "assistant",
            2,
            20,
            r#"{"content":[{"type":"text","text":"two"}]}"#,
        );
        add_message(
            &conn,
            "msg_3",
            "ses_root",
            "user",
            3,
            30,
            r#"{"text":"three"}"#,
        );
        add_message(
            &conn,
            "msg_4",
            "ses_root",
            "user",
            4,
            40,
            r#"{"text":"four"}"#,
        );
        add_message(
            &conn,
            "msg_5",
            "ses_root",
            "assistant",
            5,
            50,
            r#"{"content":[{"type":"text","text":"five"}]}"#,
        );
        add_message(
            &conn,
            "msg_6",
            "ses_root",
            "compaction",
            6,
            60,
            r#"{"status":"completed","reason":"auto","summary":"the summary","metadata":{"keepID":"msg_4"}}"#,
        );
        // And a kept message after it, which is live history.
        add_message(
            &conn,
            "msg_7",
            "ses_root",
            "user",
            7,
            70,
            r#"{"text":"seven"}"#,
        );
        drop(conn);

        let r = read(&path, "ses_root");
        let kept = words(&r);
        assert_eq!(
            kept.len(),
            4,
            "two kept, the marker, and what came after: {kept:?}"
        );
        assert_eq!(
            kept[0], "four",
            "the tail starts at the message `keepID` names"
        );
        assert_eq!(kept[1], "five");
        assert!(kept[2].contains("3 message(s)"), "{}", kept[2]);
        assert!(kept[2].contains("the summary"), "{}", kept[2]);
        assert!(kept[2].contains("reason: auto"), "{}", kept[2]);
        assert_eq!(kept[3], "seven");
        assert!(
            matches!(
                r.rows[2].item,
                TranscriptItem::System {
                    origin: SystemOrigin::Update,
                    ..
                }
            ),
            "the summary is a later change appended, not a rewrite"
        );
        // Every message is still counted: it was read, and the marker says what it held.
        assert_eq!(r.progress.len(), 7);
        assert_eq!(r.progress.last(), Some(&(7, 7)));
        assert_eq!(r.report.parts, 7);
    }

    /// A provider-native compaction replaces the window with canonical provider messages this build
    /// cannot map, and says so rather than importing a tail that starts nowhere.
    #[test]
    fn a_native_compaction_says_it_cannot_map_the_window() {
        let path = fixture("native");
        let conn = Connection::open(&path).unwrap();
        schema(&conn);
        project(&conn);
        add_session(&conn, "ses_root", None, Some("t"), "/d");
        add_message(
            &conn,
            "msg_1",
            "ses_root",
            "user",
            1,
            10,
            r#"{"text":"gone"}"#,
        );
        add_message(
            &conn,
            "msg_2",
            "ses_root",
            "compaction",
            2,
            20,
            r#"{"status":"completed","reason":"auto","summary":"s","native":{"messages":[{},{},{}]}}"#,
        );
        add_message(
            &conn,
            "msg_3",
            "ses_root",
            "user",
            3,
            30,
            r#"{"text":"kept"}"#,
        );
        drop(conn);

        let r = read(&path, "ses_root");
        let words = words(&r);
        assert_eq!(words.len(), 2, "the marker and what came after: {words:?}");
        assert!(
            words[0].contains("provider-native one (3 message(s)"),
            "{}",
            words[0]
        );
        assert_eq!(words[1], "kept");
    }

    /// A summary compaction that named no kept message replaced the whole history: nothing before it
    /// is imported, and the row says so rather than implying a tail it does not have.
    #[test]
    fn a_compaction_with_no_kept_message_imports_nothing_before_it() {
        let path = fixture("nokeep");
        let conn = Connection::open(&path).unwrap();
        schema(&conn);
        project(&conn);
        add_session(&conn, "ses_root", None, Some("t"), "/d");
        add_message(
            &conn,
            "msg_1",
            "ses_root",
            "user",
            1,
            10,
            r#"{"text":"gone"}"#,
        );
        add_message(
            &conn,
            "msg_2",
            "ses_root",
            "compaction",
            2,
            20,
            r#"{"status":"completed","reason":"auto","summary":"the summary"}"#,
        );
        add_message(
            &conn,
            "msg_3",
            "ses_root",
            "user",
            3,
            30,
            r#"{"text":"after"}"#,
        );
        drop(conn);

        let r = read(&path, "ses_root");
        assert_eq!(words(&r), vec![
            "opencode compacted this conversation here (reason: auto). It kept no message verbatim, \
             so nothing before this point is imported.\n\nthe summary"
                .to_string(),
            "after".to_string()
        ]);
    }

    /// `title` is nullable in `session_v2`, and `r.get::<String>` on a NULL is an error - which is
    /// what the old reader would have raised on every untitled session.
    #[test]
    fn a_null_title_reads_as_no_title() {
        let path = fixture("title");
        let conn = Connection::open(&path).unwrap();
        schema(&conn);
        project(&conn);
        add_session(&conn, "ses_root", None, None, "/d");
        add_message(
            &conn,
            "msg_1",
            "ses_root",
            "user",
            1,
            10,
            r#"{"text":"hi"}"#,
        );
        drop(conn);

        let src = Source::open(&path).unwrap();
        let tree = src.tree("ses_root").unwrap();
        assert_eq!(tree.root.title, "");
        assert!(tree.root.origin().contains("ses_root"));
        assert_eq!(words(&read(&path, "ses_root")), vec!["hi"]);
    }

    /// Spend moved from `step-finish` parts to the assistant message itself; the totals are the same
    /// formula, summed over the messages.
    #[test]
    fn the_spend_is_the_sum_of_the_assistant_messages() {
        let path = fixture("spend");
        let conn = Connection::open(&path).unwrap();
        schema(&conn);
        project(&conn);
        add_session(&conn, "ses_root", None, Some("t"), "/d");
        add_message(
            &conn,
            "msg_1",
            "ses_root",
            "assistant",
            1,
            10,
            r#"{"content":[{"type":"text","text":"a"}],"cost":0.25,
                "tokens":{"input":100,"output":20,"reasoning":5,"cache":{"read":30,"write":7}}}"#,
        );
        add_message(
            &conn,
            "msg_2",
            "ses_root",
            "assistant",
            2,
            20,
            r#"{"content":[{"type":"text","text":"b"}],"cost":0.5,
                "tokens":{"input":50,"output":10,"reasoning":0,"cache":{"read":3,"write":0}}}"#,
        );
        // Not an assistant message: its own usage is not the conversation's.
        add_message(&conn, "msg_3", "ses_root", "user", 3, 30, r#"{"text":"x"}"#);
        drop(conn);

        let r = read(&path, "ses_root");
        assert_eq!(r.report.spend.cost, 0.75);
        let t = r.report.spend.tokens;
        assert_eq!((t.input, t.output, t.reasoning), (150, 30, 5));
        assert_eq!((t.cache_read, t.cache_write), (33, 7));
        assert_eq!(t.total(), 225, "input+output+reasoning+cache read+write");
        assert!(
            r.report
                .summary()
                .contains("opencode spent $0.7500 and 225 token(s)"),
            "{}",
            r.report.summary()
        );
    }

    /// Before v2.1 a part's replay data was `state`, and a compaction's window `providerContext`:
    /// rows written then must read like current ones.
    #[test]
    fn a_pre_v2_1_row_is_upgraded_before_it_is_read() {
        let path = fixture("legacy");
        let conn = Connection::open(&path).unwrap();
        schema(&conn);
        project(&conn);
        add_session(&conn, "ses_root", None, Some("t"), "/d");
        add_message(
            &conn,
            "msg_1",
            "ses_root",
            "compaction",
            1,
            10,
            r#"{"status":"completed","reason":"manual","summary":"s",
                "providerContext":{"messages":[{}]}}"#,
        );
        add_message(
            &conn,
            "msg_2",
            "ses_root",
            "assistant",
            2,
            20,
            r#"{"content":[{"type":"reasoning","text":"think",
                            "state":{"zai":{"reasoningField":"reasoning"}}}]}"#,
        );
        drop(conn);

        let r = read(&path, "ses_root");
        let TranscriptItem::Reasoning { field, .. } = &r.rows[1].item else {
            panic!("a reasoning row: {:?}", r.rows[1].item);
        };
        assert_eq!(
            *field,
            ReasoningField::Inline,
            "`state` became `native`, and its provider names `reasoning`"
        );
        assert!(
            said(&r.rows[0]).contains("provider-native one (1 message(s)"),
            "`providerContext` became `native.messages`: {}",
            said(&r.rows[0])
        );
    }

    /// Which replay field a provider's own metadata names, and what a part with none says.
    #[test]
    fn the_reasoning_field_comes_from_the_provider_metadata() {
        let cases = [
            (
                serde_json::json!({"type": "reasoning", "text": "t"}),
                ReasoningField::ReasoningContent,
            ),
            (
                serde_json::json!({"native": {"moonshot": {"reasoningField": "reasoning_content"}}}),
                ReasoningField::ReasoningContent,
            ),
            (
                serde_json::json!({"native": {"zai": {"reasoningField": "reasoning"}}}),
                ReasoningField::Inline,
            ),
            (
                serde_json::json!({"native": {"reasoningField": "reasoning_details"}}),
                ReasoningField::Inline,
            ),
            // Native data with no field named (a signature, an opaque blob) is not a different field.
            (
                serde_json::json!({"native": {"anthropic": {"signature": "abc"}}}),
                ReasoningField::ReasoningContent,
            ),
        ];
        for (part, want) in cases {
            assert_eq!(reasoning_field(&part), want, "{part}");
        }
    }

    /// opencode 2.x renamed three tools on the way in; a history reads the same either way.
    #[test]
    fn the_three_renamed_tools_read_as_their_v1_names() {
        assert_eq!(tool_name("shell"), "bash");
        assert_eq!(tool_name("subagent"), "task");
        assert_eq!(tool_name("patch"), "apply_patch");
        assert_eq!(
            tool_name("read"),
            "read",
            "everything else stands as written"
        );
    }

    /// A user message's files are named, not silently dropped, and one that is text and small is read.
    #[test]
    fn a_user_message_names_the_files_it_carried() {
        let path = fixture("userfiles");
        let conn = Connection::open(&path).unwrap();
        schema(&conn);
        project(&conn);
        add_session(&conn, "ses_root", None, Some("t"), "/d");
        conn.execute(
            "INSERT INTO blob (hash, mime, bytes, data, time_created) \
             VALUES ('h3', 'text/plain', 11, CAST('read me too' AS BLOB), 10)",
            [],
        )
        .unwrap();
        add_message(
            &conn,
            "msg_1",
            "ses_root",
            "user",
            1,
            10,
            r#"{"text":"look","files":[{"uri":"media:h3","mime":"text/plain","name":"notes.txt"}]}"#,
        );
        drop(conn);

        let r = read(&path, "ses_root");
        assert_eq!(words(&r), vec!["look read me too"]);
    }

    /// A message kind this build does not read is counted and said, never silently dropped; the
    /// turn-boundary markers are counted and left quiet, as the v1 reader leaves `step-start`.
    #[test]
    fn an_unread_message_kind_is_counted_and_said() {
        let path = fixture("unread");
        let conn = Connection::open(&path).unwrap();
        schema(&conn);
        project(&conn);
        add_session(&conn, "ses_root", None, Some("t"), "/d");
        add_message(
            &conn,
            "msg_1",
            "ses_root",
            "system",
            1,
            10,
            r#"{"text":"be nice"}"#,
        );
        add_message(
            &conn,
            "msg_2",
            "ses_root",
            "idle",
            2,
            20,
            r#"{"outcome":"succeeded"}"#,
        );
        add_message(
            &conn,
            "msg_3",
            "ses_root",
            "user",
            3,
            30,
            r#"{"text":"ok"}"#,
        );
        drop(conn);

        let r = read(&path, "ses_root");
        assert_eq!(r.report.unknown_tags.get("system"), Some(&1));
        assert_eq!(
            r.report.unknown_tags.get("idle"),
            None,
            "punctuation is not unknown"
        );
        assert_eq!(r.report.by_tag.get("idle"), Some(&1), "but it is counted");
        assert!(
            r.scraps.iter().any(|s| s.contains("be nice")),
            "the text is named, not lost: {:?}",
            r.scraps
        );
        assert_eq!(words(&r), vec!["ok"]);
    }
}
