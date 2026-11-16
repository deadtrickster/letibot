//! The header's context after a fork: the new base the daemon sized, in both the snapshot arm and
//! the live arm, whatever kind of fork it was.

use super::*;
use letibot_sessionlog::event::Envelope;

fn finished(hub: &Hub, prompt_tokens: u64) -> Envelope {
    hub.publish(testing::turn_started("t1"));
    hub.publish(SessionEvent::TurnFinished {
        turn_id: "t1".into(),
        finish_reason: letibot_sessionlog::event::FinishReason::Eos,
        usage: Usage {
            prompt_tokens,
            cached_tokens: prompt_tokens / 2,
            predicted_tokens: 300,
            cost_micros_usd: None,
            ..Default::default()
        },
        timings: Default::default(),
        ends_turn: true,
        prefix_check: None,
        dialect_template_sha: String::new(),
    })
}

/// Built from the wire's JSON, as a daemon sends it.
fn forked(transcript_id: &str, parent_id: &str, tokens: u64) -> SessionEvent {
    serde_json::from_value(serde_json::json!({
        "event": "transcript_forked",
        "transcript_id": transcript_id,
        "parent_id": parent_id,
        "context_after": {"tokens": tokens, "estimated": true},
    }))
    .expect("a fork event")
}

#[test]
fn a_snapshot_after_an_idle_compaction_shows_the_new_base_in_the_header() {
    let hub = Hub::new("s");
    finished(&hub, 90_000);
    hub.publish(forked("s#t1", "s#t0", 12_000));
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    let header = a.header_line(200);
    assert!(header.contains("12.0k ctx"), "{header}");
    assert!(!header.contains("90.0k"), "{header}");
    assert!(
        !header.contains("% cached"),
        "no round ran on the new base: {header}"
    );
}

#[test]
fn a_clear_moves_the_header_ctx() {
    let hub = Hub::new("s");
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    a.apply(ServerFrame::Event(finished(&hub, 180_000)));
    assert!(a.header_line(200).contains("180.0k ctx"));
    a.apply(ServerFrame::Event(
        hub.publish(forked("s#t1", "s#t0", 2_400)),
    ));
    a.apply(ServerFrame::Event(
        hub.publish(SessionEvent::Warning {
            code: "cleared".into(),
            detail:
                "conversation cleared: s#t0 → s#t1. The new conversation is message zero plus a \
                 9-line orientation built from this session's board, jobs, subagents and queue \
                 — 2400 tokens, down from 180000. Nothing of the old conversation was summarised \
                 or carried; it is still in the store as s#t0."
                    .into(),
            compaction: None,
        }),
    ));
    let header = a.header_line(200);
    assert!(header.contains("2400 ctx"), "{header}");
    assert!(!header.contains("180.0k"), "{header}");
}

/// A re-ingest says `re-seated:` and a bare re-seat names no arrow: the fork's own figure is what
/// moves the header, not the sentence.
#[test]
fn a_reingest_and_a_reseat_move_the_header_ctx() {
    for (code, detail) in [
        (
            "reseated",
            "re-seated: 64000 tokens of conversation carried onto the new prompt as they are, \
             on transcript s#t1. Nothing was summarised and nothing was dropped.",
        ),
        (
            "reseated",
            "this compaction also re-seated: the new prompt announces the tools this daemon \
             seats now",
        ),
    ] {
        let hub = Hub::new("s");
        let mut a = app();
        a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
        a.apply(ServerFrame::Event(finished(&hub, 70_000)));
        a.apply(ServerFrame::Event(
            hub.publish(forked("s#t1", "s#t0", 64_000)),
        ));
        a.apply(ServerFrame::Event(hub.publish(SessionEvent::Warning {
            code: code.into(),
            detail: detail.into(),
            compaction: None,
        })));
        let header = a.header_line(200);
        assert!(header.contains("64.0k ctx"), "{detail}: {header}");
    }
}
