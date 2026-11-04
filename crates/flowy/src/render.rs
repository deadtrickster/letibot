//! A delivery as the sentence the model is given: one message costs its words plus one header line.
//! Each header field prevents a wrong reply: room name, sender kind (needs an answer or not), the id
//! a citation names, and the node's clock.

use crate::attention::Identity;
use crate::client::Event;

/// One rendered message.
pub fn message(e: &Event, me: &Identity) -> String {
    let mut s = String::new();
    let who = if e.actor_name().is_empty() {
        e.actor.clone()
    } else {
        e.actor_name().to_string()
    };
    let kind = match e.actor_kind() {
        "user" => " (person)",
        "agent" => "",
        _ => " (kind unknown)",
    };
    let place = if e.private || (e.room.is_empty() && !e.addressee.is_empty()) {
        "DM".to_string()
    } else {
        match &e.project {
            Some(p) => format!("{p}/#{}", e.room),
            None => format!("#{}", e.room),
        }
    };
    s.push_str(&format!("{place} · {who}{kind}"));
    if !e.addressee.is_empty() {
        let to = if me.is_me(&e.addressee) {
            "you".to_string()
        } else if !e.addressee_name.is_empty() {
            e.addressee_name.clone()
        } else {
            e.addressee.clone()
        };
        s.push_str(&format!(" → {to}"));
    }
    if !e.created.is_empty() {
        s.push_str(&format!(" · {}", e.created));
    }
    s.push_str(&format!(" · id {}", e.id));
    if !e.thread.is_empty() {
        s.push_str(&format!(" · thread {}", e.thread));
    }
    if e.kind == "todo.note" {
        s.push_str(&format!(" · a note on row {}", e.artifact));
    } else if e.kind != "chat" {
        s.push_str(&format!(" · {}", e.kind));
    }
    if let Some(st) = &e.standing {
        let spoken = st.get("spoken").and_then(|v| v.as_bool()).unwrap_or(false);
        let root_mine = st
            .get("root_mine")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if root_mine {
            s.push_str(" · your thread");
        } else if spoken {
            s.push_str(" · you have spoken here");
        }
    }
    if e.disowned.is_some() {
        s.push_str(" · RETRACTED by its author");
    }
    s.push('\n');
    for line in e.body.lines() {
        s.push_str("  ");
        s.push_str(line);
        s.push('\n');
    }
    if let Some(c) = &e.citation {
        let text = c.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let readable = c.get("readable").and_then(|v| v.as_bool()).unwrap_or(true);
        let msg = c.get("message").and_then(|v| v.as_str()).unwrap_or("");
        let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if !readable {
            s.push_str(&format!("  ↳ quotes {msg}, which you cannot read\n"));
        } else if !text.is_empty() {
            let trunc = c
                .get("truncated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            s.push_str(&format!(
                "  ↳ quoting {msg}{}: \"{text}{}\"\n",
                if name.is_empty() {
                    String::new()
                } else {
                    format!(" ({name})")
                },
                if trunc { "…" } else { "" }
            ));
        }
    }
    s
}

/// A batch: every message, then the count of what went past, then the node's
/// clock. `local_skipped` is what the attention table filtered here,
/// `server_skipped` what the node filtered: "busy and none for me" is not "silent".
pub fn batch(
    seat: &str,
    rendered: &[String],
    local_skipped: usize,
    server_skipped: i64,
    now: &str,
) -> String {
    let mut s = format!("[flowy] {} message(s) for seat `{seat}`:\n", rendered.len());
    for r in rendered {
        s.push_str(r);
    }
    let mut passed = Vec::new();
    if local_skipped > 0 {
        passed.push(format!(
            "{local_skipped} went past that your attention table did not ask for"
        ));
    }
    if server_skipped > 0 {
        passed.push(format!(
            "{server_skipped} the node filtered before delivery"
        ));
    }
    if !passed.is_empty() {
        s.push_str(&format!("({})\n", passed.join("; ")));
    }
    if !now.is_empty() {
        s.push_str(&format!("The node's clock reads {now}.\n"));
    }
    s.push_str(
        "Reply with the `flowy` tool (`say`, with `thread` set to reply under one). \
         Three lines is a message; ten is a report and belongs in a filed row.",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_message_header_says_where_who_whom_and_when() {
        let me = Identity {
            user_id: "U".into(),
            agent_id: "A".into(),
            name: "seat".into(),
        };
        let e: Event = serde_json::from_value(json!({
            "id": "01M1", "type": "chat", "project": "Lab", "room": "general",
            "thread": "01M0", "actor": "OP", "addressee": "A", "created": "2026-09-14T08:00:00Z",
            "meta": {"actor_kind": "user", "actor_name": "deadtrickster"},
            "body": "gating X - run Y on Z\nsecond line",
            "citation": {"message": "01MZ", "name": "claude-host", "text": "dedup by name", "readable": true},
            "standing": {"spoken": true, "root_mine": false}
        }))
        .unwrap();
        let r = message(&e, &me);
        assert!(r.starts_with("Lab/#general · deadtrickster (person) → you · 2026-09-14T08:00:00Z · id 01M1 · thread 01M0 · you have spoken here\n"), "{r}");
        assert!(r.contains("  gating X - run Y on Z\n  second line\n"));
        assert!(r.contains("↳ quoting 01MZ (claude-host): \"dedup by name\""));
    }
}
