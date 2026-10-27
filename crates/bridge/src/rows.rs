//! **A session's transcript ROWS, as a head is served them** — the answer to
//! `leticode_transcript`.
//!
//! # Why the rows and not the store
//!
//! The operator's ruling, 2026-10-26: reading `sessions.db` from a package's pane is *"not a
//! good idea"*. They are right for two reasons that have nothing to do with taste. The store is
//! one file every daemon on the box shares, so a reader there sees rows from sessions it was
//! never told about and rewrites nothing it should; and the shape of a row is the STORE's —
//! `item_json` beside tokens and a hash chain — where what a reader wants is the item.
//!
//! So this module renders what the daemon already serves a head: a [`Snapshot`]'s items, the
//! same rows a TUI attached to that session is drawing, asked for with
//! `ClientFrame::Peek { shape: Rows }` (`PROTOCOL_VERSION`, "the session's own rows … the same
//! thing an attach answers with"). Nothing here opens a file.
//!
//! # The three things a row can be, and the one it cannot
//!
//! A row is either its item (user, assistant, tool result, reasoning, a segment mark), or it is
//! `content: null` — a row the daemon saw announced and has not reconciled, which is *"there is
//! a row here and this build cannot show you what is in it"*. The second is a real state and it
//! is not an empty message: `SnapshotItem::item`'s own doc says a head renders it as a
//! placeholder, and so does this.
//!
//! # The cap, and why it is stated
//!
//! A tool result can be a megabyte and a prompt can be a book, so every string is cut at
//! [`CAP`] characters — **characters, not bytes**, because a reader counting what it got and a
//! cap counting bytes disagree the first time a row carries a glyph. A string that was cut says
//! so on its own row (`"cut": true`) and every answer states the cap in force, because a
//! caller that does not know the number cannot tell a complete 8000-character payload from one
//! that stopped there.
//!
//! **Generous on purpose.** This is a READER's view, not a model's context: the whole point is
//! that a package's orchestrator can read a real tool result rather than the one-line summary
//! `leticode_events` gives it, so the cap is the largest thing that is still a screen of text.

use letibot_sessionlog::view::{Snapshot, SnapshotItem};
use letibot_transcript::{ToolOutcome, TranscriptItem, UserPart};
use serde_json::{Value, json};

/// **How many characters of one string a row may carry.** See the module note: characters,
/// stated in every answer, and generous because a reader is not a context window.
pub const CAP: usize = 8_000;

/// How many rows one answer carries when the caller names no `limit`. A screenful of
/// conversation: a turn that produced more than this is history, and the cursor reads on.
pub const LIMIT: usize = 100;

/// One row of the answer, or `None` for a row this view does not carry at all (a reasoning
/// block the caller did not ask for).
pub fn row(i: u64, item: &SnapshotItem, reasoning: bool) -> Option<Value> {
    let mut out = match item.item.as_ref() {
        Some(carried) => carried_row(carried, reasoning)?,
        // **A row whose content the daemon has not reconciled.** `kind` is the store's own word
        // for it, which is all anybody knows about it.
        None => json!({ "kind": item.kind.clone(), "content": null }),
    };
    out["i"] = json!(i);
    // The daemon's clock for the row, when it recorded one: zero is *unknown* and is left out
    // rather than drawn as 1970 (the rule `SnapshotItem::ts` states one layer down).
    if item.ts > 0 {
        out["ts"] = json!(item.ts);
    }
    Some(out)
}

fn carried_row(item: &TranscriptItem, reasoning: bool) -> Option<Value> {
    Some(match item {
        TranscriptItem::System { text, origin } => json!({
            "kind": "system",
            "origin": origin,
            "text": cut(text).0,
            "cut": cut(text).1,
        }),
        TranscriptItem::User { parts, speaker } => {
            // **The text, then what is not text.** A picture's `data_ref` is a data URI of the
            // whole file, and a reader that asked for the conversation did not ask for that;
            // the media type and the fact that it is there is the reading a pane can use.
            let text = parts
                .iter()
                .filter_map(|p| match p {
                    UserPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let rest: Vec<Value> = parts
                .iter()
                .filter_map(|p| match p {
                    UserPart::Text { .. } => None,
                    UserPart::Image { media_type, .. } => Some(json!({
                        "type": "image",
                        "media_type": media_type,
                    })),
                    UserPart::FileRef { path, .. } => Some(json!({
                        "type": "file",
                        "path": path,
                    })),
                })
                .collect();
            let (text, was_cut) = cut(&text);
            let mut row = json!({
                "kind": "user",
                "speaker": speaker.as_str(),
                "text": text,
                "cut": was_cut,
            });
            if !rest.is_empty() {
                row["attachments"] = Value::Array(rest);
            }
            row
        }
        // **The model's own thinking, and only when asked for.** It is the one part of a
        // transcript nothing downstream reads (the compaction tail's own note), and a caller
        // following a package wants the work rather than 25 000 tokens of `+ 0 + 0`.
        TranscriptItem::Reasoning {
            text,
            field,
            truncated,
        } => {
            if !reasoning {
                return None;
            }
            json!({
                "kind": "reasoning",
                "field": field,
                "text": cut(text).0,
                "cut": cut(text).1,
                "abandoned": truncated,
            })
        }
        TranscriptItem::Assistant {
            text,
            tool_calls,
            truncated,
        } => {
            let (text, was_cut) = cut(text);
            let calls: Vec<Value> = tool_calls
                .iter()
                .map(|c| {
                    let (args, args_cut) = cut(&c.arguments);
                    json!({
                        "id": c.id,
                        "name": c.name,
                        "arguments": args,
                        "cut": args_cut,
                    })
                })
                .collect();
            let mut row = json!({
                "kind": "assistant",
                "text": text,
                "cut": was_cut,
                "truncated": truncated,
            });
            if !calls.is_empty() {
                row["calls"] = Value::Array(calls);
            }
            row
        }
        TranscriptItem::ToolResult {
            call_id,
            name,
            outcome,
            payload,
            origin,
            ..
        } => {
            let (payload, was_cut) = cut(payload);
            let mut row = json!({
                "kind": "tool_result",
                "call": call_id,
                "name": name,
                "outcome": outcome_word(outcome),
                "payload": payload,
                "cut": was_cut,
            });
            // The rest of the outcome, which the word does not carry: why it failed, which job
            // it became. `outcome_word` is the same vocabulary `leticode_events` files a call
            // under, so a reader's two tools agree about what happened.
            match outcome {
                ToolOutcome::Failed { reason } | ToolOutcome::Abstained { reason } => {
                    row["why"] = json!(cut(reason).0);
                }
                ToolOutcome::NotRun { why } => {
                    row["why"] = json!(cut(why).0);
                }
                ToolOutcome::Backgrounded { handle, next, .. } => {
                    row["job"] = json!(handle);
                    row["next"] = json!(next);
                }
                _ => {}
            }
            // **Who ran it, when it was not the model** — a call the operator ran from their own
            // console is a different fact about a row whose subject is who acted.
            if let Some(letibot_transcript::CallOrigin::Operator { who }) = origin {
                row["ran_by"] = json!(who);
            }
            row
        }
        // A delimiter that renders to nothing. It is on the wire as a row, so it is one here —
        // `kind` and its label — rather than a gap in the numbering.
        TranscriptItem::SegmentMark {
            segment_id,
            label,
            kind,
            edge,
        } => json!({
            "kind": "segment_mark",
            "segment_id": segment_id,
            "label": label,
            "edge": edge,
            "mark": kind,
        }),
    })
}

/// **One page of a session's rows**, with the cursor that reads on from it.
///
/// `since` is the `next` of the previous answer — *the number of rows this reader has already
/// seen* — and the arithmetic is against `items_dropped`, because a snapshot is the TAIL of a
/// conversation: row 3 of the window is row `items_dropped + 3` of the session, and a cursor
/// that counted window positions would silently re-read or skip every time the window moved.
///
/// `dropped` is present only when the caller's cursor is older than the window can reach —
/// the same rule `leticode_events` follows for its ring: a reader that believes it has the
/// whole story when it has the tail of it would read ten calls as a turn that made ten calls.
pub fn page(snap: &Snapshot, since: u64, limit: usize, reasoning: bool) -> Value {
    let dropped = snap.items_dropped;
    let skip = since.saturating_sub(dropped) as usize;
    let mut rows: Vec<Value> = Vec::new();
    let mut next = since;
    for (at, item) in snap.items.iter().enumerate().skip(skip) {
        if rows.len() >= limit {
            break;
        }
        let i = dropped + at as u64;
        next = i + 1;
        if let Some(row) = row(i, item, reasoning) {
            rows.push(row);
        }
    }
    let total = dropped + snap.items.len() as u64;
    let mut out = json!({
        "rows": rows,
        "next": next.max(since),
        "rows_total": total,
        "cap": CAP,
        "reasoning": reasoning,
    });
    if since < dropped {
        out["dropped"] = json!(dropped - since);
    }
    out
}

/// **The word a tool's ending is filed under**, shared with `leticode_events` so the two
/// answers cannot name one call's ending differently. A `denied` call that reported `failed`
/// would be a gate's decision recorded as the tool's fault.
pub fn outcome_word(outcome: &ToolOutcome) -> &'static str {
    match outcome {
        ToolOutcome::Ok => "ok",
        ToolOutcome::Abstained { .. } => "abstained",
        ToolOutcome::Failed { .. } => "failed",
        ToolOutcome::Denied { .. } => "denied",
        ToolOutcome::Timeout => "timeout",
        ToolOutcome::NotRun { .. } => "not_run",
        ToolOutcome::Backgrounded { .. } => "backgrounded",
    }
}

/// The string, cut to [`CAP`] characters, and whether it had to be.
fn cut(text: &str) -> (String, bool) {
    if text.chars().count() <= CAP {
        return (text.to_string(), false);
    }
    (text.chars().take(CAP).collect(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_sessionlog::view::SnapshotItem;

    fn item(i: u64, kind: &str, item: Option<TranscriptItem>) -> SnapshotItem {
        SnapshotItem {
            item_id: format!("row-{i}"),
            kind: kind.into(),
            ledger_head: String::new(),
            ts: 0,
            item,
            citations: Vec::new(),
        }
    }

    fn snap(items_dropped: u64, rows: Vec<SnapshotItem>) -> Snapshot {
        Snapshot {
            session_id: "s-1".into(),
            seq: 0,
            dropped: 0,
            items_dropped,
            items: rows,
            turn: None,
            open_decisions: Vec::new(),
            settled_decisions: Vec::new(),
            warnings: Vec::new(),
            heads: Vec::new(),
            subagents: Vec::new(),
            activity: Default::default(),
        }
    }

    /// **A long payload comes back whole up to the cap, says so, and says what the cap was.**
    #[test]
    fn a_long_payload_is_cut_at_the_stated_cap() {
        let long = "x".repeat(CAP + 500);
        let s = snap(
            0,
            vec![item(
                0,
                "tool_result",
                Some(TranscriptItem::ToolResult {
                    call_id: "call_0".into(),
                    name: "bash".into(),
                    outcome: ToolOutcome::Ok,
                    payload: long.clone(),
                    edit: None,
                    origin: None,
                    media: None,
                }),
            )],
        );
        let answer = page(&s, 0, 10, false);
        assert_eq!(answer["cap"], json!(CAP));
        let row = &answer["rows"][0];
        assert_eq!(
            row["payload"].as_str().map(str::chars).map(Iterator::count),
            Some(CAP)
        );
        assert_eq!(row["cut"], json!(true));
        // And a short one is whole, with nothing claiming it was cut.
        let s = snap(
            0,
            vec![item(
                0,
                "tool_result",
                Some(TranscriptItem::ToolResult {
                    call_id: "call_0".into(),
                    name: "bash".into(),
                    outcome: ToolOutcome::Ok,
                    payload: "short".into(),
                    edit: None,
                    origin: None,
                    media: None,
                }),
            )],
        );
        let answer = page(&s, 0, 10, false);
        assert_eq!(answer["rows"][0]["payload"], "short");
        assert_eq!(answer["rows"][0]["cut"], json!(false));
    }

    /// **The cursor counts rows in the SESSION, not positions in the window.** A snapshot is a
    /// tail, so the same row is at a different index in two of them — and a cursor that counted
    /// window positions would re-read or skip every time the window moved.
    #[test]
    fn the_cursor_is_a_session_row_and_the_window_moving_does_not_move_it() {
        let row = |i: u64| {
            item(
                i,
                "assistant",
                Some(TranscriptItem::Assistant {
                    text: format!("row {i}"),
                    tool_calls: Vec::new(),
                    truncated: false,
                }),
            )
        };
        // Three rows in a two-row window: rows 1 and 2, with row 0 trimmed.
        let s = snap(1, vec![row(1), row(2)]);
        let answer = page(&s, 0, 10, false);
        assert_eq!(answer["rows"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            answer["rows"][0]["i"],
            json!(1),
            "the window starts at row 1"
        );
        assert_eq!(answer["next"], json!(3));
        assert_eq!(answer["rows_total"], json!(3));
        assert_eq!(answer["dropped"], json!(1), "row 0 is out of reach");
        // Read on from 2, with the window moved one row further on.
        let s = snap(2, vec![row(2), row(3)]);
        let answer = page(&s, 2, 10, false);
        assert_eq!(
            answer["rows"].as_array().map(Vec::len),
            Some(2),
            "row 2 is the first one the cursor had not seen: {}",
            answer["rows"]
        );
        assert_eq!(answer["rows"][0]["i"], json!(2));
        assert_eq!(answer["next"], json!(4));
        assert!(
            answer.get("dropped").is_none(),
            "nothing was missed: {answer}"
        );
    }

    /// **Reasoning is left out unless it was asked for** — and when it is, it is a row of its
    /// own rather than a field on the answer, because the interleaving is what the model's own
    /// cache depends on and a reader deserves the same order.
    #[test]
    fn reasoning_is_a_row_only_when_it_was_asked_for() {
        let s = snap(
            0,
            vec![item(
                0,
                "reasoning",
                Some(TranscriptItem::Reasoning {
                    text: "thinking".into(),
                    field: letibot_transcript::ReasoningField::Inline,
                    truncated: false,
                }),
            )],
        );
        assert_eq!(page(&s, 0, 10, false)["rows"], json!([]));
        let answer = page(&s, 0, 10, true);
        assert_eq!(answer["rows"][0]["kind"], "reasoning");
        assert_eq!(answer["rows"][0]["text"], "thinking");
        // And its numbering is the session's either way: a row left out does not renumber the
        // one after it.
        assert_eq!(answer["rows"][0]["i"], json!(0));
    }

    /// A row the daemon announced and never reconciled is a row with no content, not a row with
    /// an empty message — the two are different statements and only one of them is true.
    #[test]
    fn a_row_with_no_content_says_so_rather_than_looking_empty() {
        let s = snap(0, vec![item(0, "assistant", None)]);
        let answer = page(&s, 0, 10, false);
        assert_eq!(answer["rows"][0]["kind"], "assistant");
        assert_eq!(answer["rows"][0]["content"], Value::Null);
        assert!(answer["rows"][0].get("text").is_none(), "{answer}");
    }
}
