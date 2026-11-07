//! A held session's events as channel notifications: what `leticode-wait` told an orchestrator,
//! pushed, so a Claude Code session needs no waiter of its own.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::head::{Attention, Head};

/// How long one `wait` blocks before the watcher checks that its head is still held.
const WAIT: Duration = Duration::from_secs(30);

/// Longest turn-end text a notification carries; `leticode_transcript` has the rest.
const MAX_MESSAGE: usize = 1500;

/// Watch `head` for as long as the bridge holds it, sending each new event on `tx`.
pub fn watch(head: &Arc<Head>, tx: Sender<Value>) {
    let weak = Arc::downgrade(head);
    let _ = std::thread::Builder::new()
        .name(format!("leticode-push-{}", head.session))
        .spawn(move || run(weak, tx));
}

fn run(weak: Weak<Head>, tx: Sender<Value>) {
    let mut last: Option<String> = None;
    loop {
        let Some(head) = weak.upgrade() else {
            return;
        };
        let attention = head.wait(Instant::now() + WAIT);
        let event = event(&head, &attention);
        drop(head);
        match event {
            Some((key, note)) if last.as_deref() != Some(key.as_str()) => {
                if tx.send(note).is_err() {
                    return;
                }
                last = Some(key);
            }
            // The same state again: a quiet session answers `wait` at once, so it is said once.
            _ => std::thread::sleep(Duration::from_secs(1)),
        }
    }
}

/// The event's identity (what makes it news) and its notification. `None` for a timeout.
pub fn event(head: &Head, attention: &Attention) -> Option<(String, Value)> {
    let who = format!("leticode `{}` ({})", head.session, head.worktree.display());
    let (kind, key, content) = match attention {
        Attention::Timeout => return None,
        Attention::TurnEnded { message } => (
            "turn_ended",
            format!("end:{message}"),
            format!(
                "{who} ended its turn:\n{}",
                cut(message.trim(), MAX_MESSAGE)
            ),
        ),
        Attention::Ask(a) => (
            "ask",
            format!("ask:{}", a.req_id),
            format!(
                "{who} asks ({}): {} {} - answer with `leticode_answer`; `leticode_asks` lists \
                 the options.",
                a.kind, a.tool, a.summary
            ),
        ),
        Attention::Failed { why } => (
            "failed",
            format!("failed:{why}"),
            format!("{who} failed: {why}"),
        ),
        Attention::Trouble(t) => (
            "trouble",
            format!("trouble:{}:{}", t.kind, t.count),
            format!(
                "{who} is stuck on its provider ({} on {}, {} in a row): {}",
                t.kind, t.model, t.count, t.detail
            ),
        ),
    };
    let note = json!({
        "jsonrpc": "2.0",
        "method": "notifications/claude/channel",
        "params": {
            "content": content,
            "meta": { "source": "leticode", "session": head.session, "event": kind },
        },
    });
    Some((key, note))
}

/// `s` cut to at most `max` bytes on a char boundary, saying that it was cut.
fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{} ... (cut; `leticode_transcript` has the rest)",
        &s[..end]
    )
}
