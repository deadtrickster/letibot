//! Event builders and a recorded session.
//!
//! Public rather than `#[cfg(test)]` on purpose. `docs/workstreams.md` W8→W6 says
//! the TUI *"renders an event stream. Give it a recorded event log as a fixture and
//! it is built and demoed before a turn engine exists."* [`recorded_session`] is
//! that fixture, and a fixture that only exists inside this crate's test binary is
//! a fixture the head cannot use.

use letibot_transcript::{ToolOutcome, TranscriptItem, UserPart};

use crate::event::{
    Decider, DecisionOption, DecisionOutcome, DeltaTarget, FinishReason, OnTimeout, OptionKind,
    PromptProgress, SessionEvent, Timings, Usage,
};

pub fn warn(detail: &str) -> SessionEvent {
    SessionEvent::Warning {
        code: "test".into(),
        detail: detail.into(),
    }
}

pub fn turn_started(turn_id: &str) -> SessionEvent {
    SessionEvent::TurnStarted {
        turn_id: turn_id.into(),
        model: "qwen3-next-80b".into(),
        ledger_head: "0000".into(),
    }
}

pub fn progress(turn_id: &str) -> SessionEvent {
    SessionEvent::PromptProgress {
        turn_id: turn_id.into(),
        progress: PromptProgress {
            total: 1000,
            cache: 900,
            processed: 950,
            time_ms: 40,
        },
    }
}

pub fn delta(turn_id: &str, text: &str) -> SessionEvent {
    SessionEvent::Delta {
        turn_id: turn_id.into(),
        target: DeltaTarget::Text,
        text: text.into(),
    }
}

pub fn reasoning(turn_id: &str, text: &str) -> SessionEvent {
    SessionEvent::Delta {
        turn_id: turn_id.into(),
        target: DeltaTarget::Reasoning,
        text: text.into(),
    }
}

pub fn proposed(turn_id: &str, call_id: &str, name: &str) -> SessionEvent {
    proposed_on(turn_id, call_id, name, "")
}

/// A proposal with a §4.1 display target: what the call is *about*.
pub fn proposed_on(turn_id: &str, call_id: &str, name: &str, target: &str) -> SessionEvent {
    SessionEvent::ToolCallProposed {
        turn_id: turn_id.into(),
        call_id: call_id.into(),
        name: name.into(),
        args_digest: "fnv1a:0000000000000000".into(),
        target: target.into(),
    }
}

pub fn requested(req_id: &str, summary: &str) -> SessionEvent {
    SessionEvent::DecisionRequested {
        req_id: req_id.into(),
        kind: "exec".into(),
        call_id: Some("c1".into()),
        summary: summary.into(),
        options: vec![
            DecisionOption {
                option_id: "allow".into(),
                label: "Allow once".into(),
                kind: OptionKind::AllowOnce,
            },
            DecisionOption {
                option_id: "deny".into(),
                label: "Deny".into(),
                kind: OptionKind::RejectOnce,
            },
        ],
        deadline: None,
        on_timeout: OnTimeout::Deny,
    }
}

pub fn answered(req_id: &str, option_id: &str) -> SessionEvent {
    SessionEvent::DecisionAnswered {
        req_id: req_id.into(),
        outcome: DecisionOutcome::Selected {
            option_id: option_id.into(),
        },
        by: Decider {
            kind: "human".into(),
            identity: "dead@lab2x1".into(),
        },
        basis: "typed d".into(),
        late: false,
    }
}

pub fn tool_progress(call_id: &str, note: &str) -> SessionEvent {
    SessionEvent::ToolProgress {
        turn_id: "t1".into(),
        call_id: call_id.into(),
        note: note.into(),
    }
}

pub fn turn_finished(turn_id: &str) -> SessionEvent {
    SessionEvent::TurnFinished {
        turn_id: turn_id.into(),
        finish_reason: FinishReason::Eos,
        usage: Usage {
            prompt_tokens: 1000,
            cached_tokens: 900,
            predicted_tokens: 42,
        },
        timings: Timings {
            prompt_ms: 40.0,
            predicted_ms: 900.0,
            wall_ms: 950,
        },
    }
}

pub fn appended(item_id: &str, kind: &str) -> SessionEvent {
    SessionEvent::TranscriptAppended {
        item_id: item_id.into(),
        kind: kind.into(),
        ledger_head: "beef".into(),
    }
}

/// The body for a row `appended` announced.
pub fn content(item_id: &str, text: &str) -> SessionEvent {
    SessionEvent::TranscriptContent {
        item_id: item_id.into(),
        item: Box::new(letibot_transcript::TranscriptItem::User {
            parts: vec![letibot_transcript::UserPart::Text { text: text.into() }],
        }),
    }
}

/// One of every variant, so an exhaustiveness assertion has something to walk.
pub fn one_of_each() -> Vec<SessionEvent> {
    vec![
        turn_started("t1"),
        progress("t1"),
        delta("t1", "x"),
        proposed("t1", "c1", "read"),
        requested("r1", "do it"),
        answered("r1", "allow"),
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "read".into(),
            access: "read".into(),
        },
        tool_progress("c1", "half"),
        SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            outcome: ToolOutcome::Ok,
            payload_digest: "fnv1a:1".into(),
            inline_bytes: 12,
            full_bytes: 12,
            spill: None,
            repairs: 0,
        },
        turn_finished("t1"),
        SessionEvent::TurnInterrupted {
            turn_id: "t1".into(),
            reason: "operator".into(),
            partial_kept: true,
        },
        appended("s.0", "user"),
        content("s.0", "hi"),
        SessionEvent::HeadAttached {
            head_id: "h1".into(),
            kind: "tui".into(),
            identity: "dead".into(),
        },
        SessionEvent::HeadDetached {
            head_id: "h1".into(),
            kind: "tui".into(),
            identity: "dead".into(),
        },
        warn("careful"),
        SessionEvent::Explain {
            turn_id: "t1".into(),
            plan: serde_json::json!({"stage": "render"}),
        },
        SessionEvent::CommandIssued {
            head_id: "h1".into(),
            identity: "dead".into(),
            command: "prompt".into(),
            client_request_id: "c1".into(),
            note: "queued".into(),
        },
        SessionEvent::DenialRaised {
            request_id: "adj-s1-0001".into(),
            turn_id: "t1".into(),
            call_id: "c1".into(),
            tool: "bash".into(),
            summary: "bash(command: systemctl --user restart llama)".into(),
            baseline: "1 command; intents: privilege_escalation(system)".into(),
            by: "human:dead".into(),
            basis: "always-ask: privilege escalation".into(),
            tier: "always_ask".into(),
            outcome: "denied".into(),
            repeat_count: 1,
            breaker_open: false,
            grant: "grant `adj-s1-0001` (bash) for this session".into(),
        },
    ]
}

/// A realistic session: a user turn, reasoning, a tool call gated by a decision,
/// and an answer with markdown in it.
///
/// The text is deliberately long enough and blocky enough to exercise §13.3's
/// incremental lexer — several paragraphs, a fenced code block, a list — and it is
/// emitted as small deltas, which is how it arrives from the engine.
pub fn recorded_session() -> Vec<SessionEvent> {
    let mut out = vec![
        appended("s.0", "system"),
        appended("s.1", "user"),
        turn_started("s#1"),
        progress("s#1"),
    ];
    for w in chunks("The user is asking about the prefix cache. I should check the ledger first.") {
        out.push(reasoning("s#1", &w));
    }
    // With its §4.1 display target, because `--demo` is the fixture people look at
    // to decide whether the head is any good, and a demo that shows `Read (c1)`
    // demonstrates the thing that was fixed as though it had not been.
    out.push(proposed_on(
        "s#1",
        "c1",
        "read",
        "/home/dead/Projects/letibot/TODO.md",
    ));
    out.push(requested("d1", "read /home/dead/Projects/letibot/TODO.md"));
    out.push(answered("d1", "allow"));
    out.push(SessionEvent::ToolStarted {
        turn_id: "s#1".into(),
        call_id: "c1".into(),
        name: "read".into(),
        access: "read".into(),
    });
    out.push(tool_progress("c1", "40 of 276 lines"));
    // The assistant row that MADE the call, appended before the row that answers
    // it — which is what the daemon does (`harnessd::harness` appends the
    // assistant item, invokes the round, then appends its results) and what the
    // store holds for every real session.
    //
    // It was missing, and it is the row a head reads a settled call's display
    // target out of: the arguments live on it and nowhere else once the proposal
    // event has gone by. So `--demo` — the fixture people look at to decide
    // whether the head is any good — rendered `▸ Read (c1)`, demonstrating the
    // thing §4.1 fixed as though it had not been. A fixture missing a row the real
    // producer always emits is a fixture that exercises a head nobody runs.
    out.push(appended("s.2", "assistant"));
    out.push(SessionEvent::ToolFinished {
        turn_id: "s#1".into(),
        call_id: "c1".into(),
        outcome: ToolOutcome::Ok,
        payload_digest: "fnv1a:deadbeefdeadbeef".into(),
        inline_bytes: 8_412,
        full_bytes: 8_412,
        spill: None,
        repairs: 0,
    });
    out.push(appended("s.3", "tool_result"));
    for w in chunks(MARKDOWN) {
        out.push(delta("s#1", &w));
    }
    out.push(appended("s.4", "assistant"));
    out.push(turn_finished("s#1"));
    out
}

/// The transcript rows `recorded_session` announces, so a head fixture can be
/// reconciled the way the daemon reconciles a live one.
pub fn recorded_items() -> Vec<(String, TranscriptItem)> {
    vec![
        (
            "s.0".into(),
            TranscriptItem::System {
                text: "You are letibot.".into(),
                origin: letibot_transcript::SystemOrigin::Bootstrap,
            },
        ),
        (
            "s.1".into(),
            TranscriptItem::User {
                parts: vec![UserPart::Text {
                    text: "Why is the prefix cache missing?".into(),
                }],
            },
        ),
        (
            // The row that made the call. Its `arguments` are the only surviving
            // copy of what the call was about once the proposal event has gone by,
            // and they are what `letibot_sessionlog::display_target` reads.
            "s.2".into(),
            TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![letibot_transcript::ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: r#"{"path":"/home/dead/Projects/letibot/TODO.md"}"#.into(),
                }],
            },
        ),
        (
            "s.3".into(),
            TranscriptItem::ToolResult {
                call_id: "c1".into(),
                name: "read".into(),
                outcome: ToolOutcome::Ok,
                payload: "…276 lines…".into(),
            },
        ),
        (
            "s.4".into(),
            TranscriptItem::Assistant {
                text: MARKDOWN.into(),
                tool_calls: vec![],
            },
        ),
    ]
}

/// Split into small pieces the way a token stream arrives: 1–6 characters, never
/// aligned to a block boundary, which is what makes the incremental lexer's guards
/// matter.
fn chunks(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut want = 3usize;
    for c in s.chars() {
        cur.push(c);
        if cur.chars().count() >= want {
            out.push(std::mem::take(&mut cur));
            want = 1 + (out.len() * 7) % 6;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub const MARKDOWN: &str = "\
## Why the cache missed

The short answer is `reasoning_content`. Three things had to line up:

1. The dialect replays prior reasoning into the field the model expects.
2. The ledger appends **ids**, never re-derived text.
3. The stable prefix is frozen before the first turn.

Here is the shape of the check:

```rust
assert_eq!(
    cached_tokens(n + 1),
    prompt_tokens(n) + committed_tokens(n),
);
```

> Note that `predicted_tokens` is the wrong term here — a trailing stop token is
> stripped before commit, so the witness must record **committed** tokens.

That is the whole of it. The remaining divergence is the server's own checkpoint
behaviour on a hybrid model, which is a measurement of llama.cpp and not a
violation of the invariant.
";
