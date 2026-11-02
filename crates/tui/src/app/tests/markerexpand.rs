//! `ctrl-n` then a run's expansion: the expansion re-walks the whole conversation
//! (`invalidate_history`), so the read mark has to be asked at the moment a note's line is
//! written - there is no second cache of note rows to consult.
//!
//! Each test expands a run holding one notice the mark covers and one it does not, and asserts
//! the screen holds exactly the second: a test that only says "nothing was drawn" is passed by a
//! walk that draws no notices at all. Neutering `already_read`/`covered_by_mark` turns them red.

use super::*;

/// The shape: a turn one round deep with two calls running, the counter on the assistant row, and
/// a retry ladder of notices already cleared by `ctrl-n`. The ladder is one kind, three instants,
/// and the third arrives AFTER the press, so it is the one line the screen owes the reader.
fn a_turn_with_a_cleared_wall() -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        testing::turn_started("t1"),
    )));
    a.apply(ServerFrame::Event(env(2, testing::appended("s.0", "user"))));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Operator,
                parts: vec![UserPart::Text {
                    text: "check the two files".into(),
                }],
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(
        4,
        testing::appended("s.1", "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        5,
        SessionEvent::TranscriptContent {
            item_id: "s.1".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "let me look at both:".into(),
                tool_calls: vec![
                    letibot_transcript::ToolCall {
                        id: "c0".into(),
                        name: "read".into(),
                        arguments: "{\"path\": \"a.rs\"}".into(),
                    },
                    letibot_transcript::ToolCall {
                        id: "c1".into(),
                        name: "read".into(),
                        arguments: "{\"path\": \"b.rs\"}".into(),
                    },
                ],
                truncated: false,
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(
        6,
        testing::appended("s.2", "reasoning"),
    )));
    a.apply(ServerFrame::Event(env(
        7,
        SessionEvent::TranscriptContent {
            item_id: "s.2".into(),
            item: Box::new(TranscriptItem::Reasoning {
                text: "two reads, then I will compare".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            }),
        },
    )));
    for (i, (id, name)) in [("c0", "read"), ("c1", "read")].iter().enumerate() {
        a.apply(ServerFrame::Event(env_at(
            8 + 2 * i as u64,
            1_100,
            testing::proposed("t1", id, name),
        )));
        a.apply(ServerFrame::Event(env_at(
            9 + 2 * i as u64,
            1_200,
            SessionEvent::ToolStarted {
                turn_id: "t1".into(),
                call_id: (*id).into(),
                name: (*name).into(),
                access: Default::default(),
            },
        )));
    }
    a.visibility = Visibility::of(Profile::CONVERSATION);
    // The retry ladder, three deep — one kind, three instants, one counted row.
    for (i, ts) in [(1u64, 1_000u64), (2, 2_000), (3, 3_000)] {
        a.apply(ServerFrame::Event(env_at(
            0,
            ts,
            SessionEvent::Warning {
                code: "model_endpoint_retry".into(),
                detail: format!("did not answer: attempt {i} of 5"),
                compaction: None,
            },
        )));
    }
    a.invalidate_history();
    a
}

/// One notice of the same kind, stamped after the press — the line the reader has not read.
fn news(a: &mut App, attempt: u64, ts: u64) {
    a.apply(ServerFrame::Event(env_at(
        0,
        ts,
        SessionEvent::Warning {
            code: "model_endpoint_retry".into(),
            detail: format!("did not answer: attempt {attempt} of 5"),
            compaction: None,
        },
    )));
}

/// The wall coming back in other words: the same instant, a different sentence. A key is
/// `code|ts|hash(detail)`, so a re-worded copy is a new key and the key set cannot see it; only
/// the mark - this kind is read up to here - covers it.
fn resay(a: &mut App, attempt: u64, ts: u64) {
    a.apply(ServerFrame::Event(env_at(
        0,
        ts,
        SessionEvent::Warning {
            code: "model_endpoint_retry".into(),
            detail: format!("[fallback] attempt {attempt} of 5 did not answer either"),
            compaction: None,
        },
    )));
}

/// The counter's `tool calls` clause, as the frame drew it, and a click on it.
fn expand_the_tools(a: &mut App) {
    let screen = a.screen(120, 40);
    let frame = screen.join("\n");
    assert!(
        frame.contains("tool calls"),
        "the counter is not on the frame:\n{frame}"
    );
    let m = a
        .marker_rows
        .iter()
        .find(|m| m.show == Some(Show::Tools))
        .cloned()
        .unwrap_or_else(|| panic!("no `tool calls` clause on the frame:\n{frame}"));
    a.open_marker(m);
}

/// `ctrl-n`, then the expansion, then the collapse, then the expansion again: the retired wall is
/// drawn in none of them and the news is drawn in all, and the watermark is unchanged. The wall is
/// re-said in other words between the press and the expansion, which only the mark can cover.
#[test]
fn an_expansion_draws_no_notice_the_watermark_covers() {
    let mut a = a_turn_with_a_cleared_wall();
    let wall = a.screen(120, 40).join("\n");
    assert_eq!(
        wall.matches("model_endpoint_retry").count(),
        1,
        "the premise — three notices of one kind are one counted row:\n{wall}"
    );

    assert_eq!(a.key(Key::CtrlN), None);
    let cleared = a.screen(120, 40).join("\n");
    assert!(
        !cleared.contains("model_endpoint_retry"),
        "the press did not clear the wall:\n{cleared}"
    );
    assert_eq!(
        a.read_upto("model_endpoint_retry"),
        Some(3_000),
        "the mark is the newest one drawn"
    );

    // **The wall, re-said.** Same kind, same instants, other words — and then one notice that is
    // genuinely newer than the mark, which is the line the screen owes him.
    resay(&mut a, 1, 1_000);
    resay(&mut a, 2, 2_000);
    news(&mut a, 4, 4_000);

    expand_the_tools(&mut a);
    let open = a.screen(120, 40).join("\n");
    assert!(
        open.contains("Reading"),
        "the expansion drew nothing — the test would be vacuous:\n{open}"
    );
    assert_eq!(
        open.matches("model_endpoint_retry").count(),
        1,
        "the expansion drew the retired wall back:\n{open}"
    );
    assert!(
        open.contains("model_endpoint_retry"),
        "the one notice that is news is not drawn:\n{open}"
    );
    // Every notice but the newest is retired, and the mark retires the re-said ones - the key set
    // cannot, because `[fallback] attempt 1 of 5 ...` is a different sentence and so a different
    // key. Neutering `already_read`/`covered_by_mark` leaves the wall standing.
    assert_eq!(
        a.retired_notes(),
        a.notes.len() - 1,
        "a re-said notice the mark covers is live: {:?}",
        a.notes
            .iter()
            .map(|(_, n)| (note_stamp(n), a.is_retired(n)))
            .collect::<Vec<_>>()
    );
    // **And the row is the news one**, not a survivor: the head's last note is the fourth
    // attempt and it is live, while the two re-said copies are retired.
    let newest = a.notes.last().expect("the fourth was filed").1.clone();
    assert!(
        !a.is_retired(&newest),
        "the line drawn is not the notice that is news: {newest:?}"
    );
    assert!(
        matches!(note_stamp(&newest), Some(4_000)),
        "the line drawn is not the newest notice: {:?}",
        note_stamp(&newest)
    );

    // **And the next tool call does not change the answer.** The run is still open; the round's
    // rows land, `carry_live_open` re-applies the lift to them, and the wall stays where the press
    // put it — the operator's *"then they hide on the next tool call. then they show."*
    for (i, (id, path)) in [("c0", "a.rs"), ("c1", "b.rs")].iter().enumerate() {
        let item = format!("s.{}", 3 + i);
        a.apply(ServerFrame::Event(env(
            20 + 2 * i as u64,
            testing::appended(&item, "tool_result"),
        )));
        a.apply(ServerFrame::Event(env(
            21 + 2 * i as u64,
            SessionEvent::TranscriptContent {
                item_id: item,
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: (*id).into(),
                    name: "read".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: format!("CONTENTS-{path}"),
                    edit: None,
                    origin: None,
                    media: None,
                }),
            },
        )));
    }
    let landed = a.screen(120, 40).join("\n");
    assert!(
        landed.contains("CONTENTS-a.rs") && landed.contains("CONTENTS-b.rs"),
        "the panel lost the calls the round landed:\n{landed}"
    );
    assert_eq!(
        landed.matches("model_endpoint_retry").count(),
        1,
        "the round's rows brought the wall back:\n{landed}"
    );

    // **Collapse and expand again** — the toggle is the operator's *"then they show"*, and the
    // second door must answer what the first one did.
    a.key(Key::Esc);
    let folded = a.screen(120, 40).join("\n");
    assert_eq!(
        folded.matches("model_endpoint_retry").count(),
        1,
        "the collapse changed what the mark covers:\n{folded}"
    );
    expand_the_tools(&mut a);
    let again = a.screen(120, 40).join("\n");
    assert_eq!(
        again.matches("model_endpoint_retry").count(),
        1,
        "the second expansion drew the retired wall back:\n{again}"
    );

    // **And the watermark is the same value it was** — the expansion re-renders the transcript,
    // and a door that re-rendered it by *reading* it would move the mark and hide the notice that
    // is genuinely new.
    assert_eq!(
        a.read_upto("model_endpoint_retry"),
        Some(3_000),
        "the expansion moved the watermark: {:?}",
        a.read_marks
    );
}

/// **The same, on a run of settled rows** — the expansion there is the *other* walk's state
/// (`hist_lines` rebuilt from row 0 rather than a pane's), and the mark has to answer in both.
#[test]
fn a_settled_runs_expansion_draws_no_notice_the_watermark_covers() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "assistant".into(),
            ledger_head: String::new(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "here is what I am about to do:".into(),
                tool_calls: vec![
                    letibot_transcript::ToolCall {
                        id: "c0".into(),
                        name: "read".into(),
                        arguments: "{\"path\": \"a.rs\"}".into(),
                    },
                    letibot_transcript::ToolCall {
                        id: "c1".into(),
                        name: "grep".into(),
                        arguments: "{\"path\": \"b.rs\"}".into(),
                    },
                ],
                truncated: false,
            }),
        },
    )));
    for (i, name) in ["read", "grep"].iter().enumerate() {
        let id = format!("s.{}", i + 1);
        a.apply(ServerFrame::Event(env(
            (i as u64) * 2 + 3,
            testing::appended(&id, "tool_result"),
        )));
        a.apply(ServerFrame::Event(env(
            (i as u64) * 2 + 4,
            SessionEvent::TranscriptContent {
                item_id: id,
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: format!("c{i}"),
                    name: (*name).into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: format!("CONTENTS-{i}"),
                    edit: None,
                    origin: None,
                    media: None,
                }),
            },
        )));
    }
    a.visibility = Visibility::of(Profile::CONVERSATION);
    for (i, ts) in [(1u64, 1_000u64), (2, 2_000), (3, 3_000)] {
        a.apply(ServerFrame::Event(env_at(
            0,
            ts,
            SessionEvent::Warning {
                code: "model_endpoint_retry".into(),
                detail: format!("did not answer: attempt {i} of 5"),
                compaction: None,
            },
        )));
    }
    a.invalidate_history();
    assert_eq!(a.key(Key::CtrlN), None);
    assert!(
        !a.screen(120, 40)
            .join("\n")
            .contains("model_endpoint_retry"),
        "the press did not clear the wall"
    );
    // The wall re-said in other words, and one notice that is genuinely newer than the mark.
    resay(&mut a, 1, 1_000);
    resay(&mut a, 2, 2_000);
    news(&mut a, 4, 4_000);

    expand_the_tools(&mut a);
    let open = a.screen(120, 40).join("\n");
    assert!(
        open.contains("CONTENTS-0") && open.contains("CONTENTS-1"),
        "the run did not open:\n{open}"
    );
    assert_eq!(
        open.matches("model_endpoint_retry").count(),
        1,
        "the settled run's expansion drew the retired wall back:\n{open}"
    );
    assert_eq!(
        a.retired_notes(),
        a.notes.len() - 1,
        "a re-said notice the mark covers is live: {:?}",
        a.notes
            .iter()
            .map(|(_, n)| (note_stamp(n), a.is_retired(n)))
            .collect::<Vec<_>>()
    );
    assert_eq!(a.read_upto("model_endpoint_retry"), Some(3_000));
}
