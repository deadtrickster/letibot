use super::*;
use crate::testing::*;

fn attach(hub: &Hub, queue: usize) -> Attached {
    hub.attach(
        "tui",
        "dead@lab2x1",
        Caps {
            queue,
            ..Caps::default()
        },
        0,
    )
}

/// One door for a warning: `(code, detail)` is the whole of it, so a site cannot fill the
/// variant's fields a second way and two doors cannot word one fact differently.
#[test]
fn a_warning_goes_through_the_one_door() {
    let hub = Hub::new("s-warn");
    // Named rather than spelled at the call: a literal there is a code the tree's census would
    // demand a severity row for, and this one is a sentence about the door, not a wire code.
    let the_code = "some_code";
    hub.warn(the_code, "a sentence the operator reads");
    let kept = hub.retained();
    let last = kept.last().expect("the warning is on the log");
    match &last.event {
        SessionEvent::Warning {
            code,
            detail,
            compaction,
        } => {
            assert_eq!(code, "some_code");
            assert_eq!(detail, "a sentence the operator reads");
            assert!(compaction.is_none(), "a plain warning carries no report");
        }
        other => panic!("the sink published {other:?}"),
    }
}

/// The one construction the doors without a hub use, and the door with one, are one event:
/// a fan-out announce and a hub's publish cannot differ in a field nobody names.
#[test]
fn the_constructor_and_the_sink_build_one_event() {
    let hub = Hub::new("s-warn-same");
    let the_code = "same_code";
    hub.warn(the_code, "same sentence");
    assert_eq!(
        hub.retained().last().map(|e| e.event.clone()),
        Some(SessionEvent::warning(the_code, "same sentence")),
    );
}

/// A round with no calls that is followed by a nudge is not the prompt's end: only the frame
/// with `ends_turn` is, so the hub keeps saying in-flight across the round boundary.
#[test]
fn a_turn_stays_in_flight_across_rounds_until_the_ending_frame() {
    let hub = Hub::new("s-flight");
    assert!(!hub.turn_in_flight());
    hub.publish(turn_started("t1"));
    assert!(hub.turn_in_flight());
    // Round one proposed no calls; a nudge follows.
    hub.publish(turn_finished_round("t1"));
    assert!(hub.turn_in_flight(), "no calls, no ending frame yet");
    hub.publish(turn_started("t2"));
    hub.publish(turn_finished_round("t2"));
    assert!(hub.turn_in_flight());
    hub.publish(turn_started("t3"));
    hub.publish(turn_finished("t3"));
    assert!(!hub.turn_in_flight(), "the ending frame stands it down");
}

/// An interrupt that lands after a round said the prompt goes on is the prompt's end.
#[test]
fn an_interrupt_between_rounds_stands_the_turn_down() {
    let hub = Hub::new("s-flight-interrupt");
    hub.publish(turn_started("t1"));
    hub.publish(proposed("t1", "c1", "bash"));
    hub.publish(turn_finished_round("t1"));
    assert!(hub.turn_in_flight());
    hub.publish(SessionEvent::TurnInterrupted {
        turn_id: "t1".into(),
        reason: "operator".into(),
        partial_kept: true,
    });
    assert!(!hub.turn_in_flight());
    assert!(!hub.end_the_prompt(), "nothing left to end");
}

/// A loop that returns after a round that said the prompt goes on gets its ending published
/// once, and a prompt that already ended gets none.
#[test]
fn a_prompt_left_between_rounds_is_ended_once() {
    let hub = Hub::new("s-flight-end");
    hub.publish(turn_started("t1"));
    let mut round = turn_finished_round("t1");
    if let SessionEvent::TurnFinished { usage, .. } = &mut round {
        usage.prompt_tokens = 1234;
        usage.cost_micros_usd = Some(5);
    }
    hub.publish(round);
    assert!(hub.turn_in_flight());
    assert!(hub.end_the_prompt());
    assert!(!hub.turn_in_flight());
    assert!(!hub.end_the_prompt(), "exactly once");
    let endings: Vec<_> = hub
        .retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::TurnFinished {
                ends_turn: true,
                usage,
                ..
            } => Some(*usage),
            _ => None,
        })
        .collect();
    assert_eq!(endings.len(), 1);
    // The head's context meter keeps the last round's number, and the cost is not billed twice.
    assert_eq!(endings[0].prompt_tokens, 1234);
    assert_eq!(endings[0].cost_micros_usd, None);
}

/// The footprint is the harness's to answer and the hub's to carry. Three shapes are told
/// apart: an unregistered hub answers nothing, a registered one answers its numbers.
#[test]
fn a_hubs_census_carries_what_its_harness_holds() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let hub = Hub::new("s-footprint");
    assert_eq!(hub.census().footprint, Footprint::default());
    assert!(
        !hub.footprint().over_ceiling(),
        "no ceiling was stated, which is not the same fact as inside one"
    );

    let text = std::sync::Arc::new(AtomicUsize::new(1000));
    let read = text.clone();
    hub.set_footprint(std::sync::Arc::new(move || Footprint {
        items: 3,
        items_text_bytes: read.load(Ordering::Relaxed),
        ledger_region_bytes: 4096,
        ledger_tokens: 1024,
        trail_bytes: 10,
        trail_utterances: 1,
        ceiling_bytes: 4 << 20,
    }));
    let c = hub.census();
    assert_eq!(c.footprint.items, 3);
    assert_eq!(c.footprint.items_text_bytes, 1000);
    assert_eq!(c.footprint.ledger_tokens, 1024);
    assert_eq!(c.footprint.trail_utterances, 1);
    assert_eq!(c.footprint.total(), 1000 + 4096 + 10);
    assert!(!c.footprint.over_ceiling());

    text.store(8 << 20, Ordering::Relaxed);
    let grown = hub.census().footprint;
    assert_eq!(grown.items_text_bytes, 8 << 20);
    assert!(
        grown.over_ceiling(),
        "a harness past its own ceiling must say so"
    );
    // And the rest of the census is unaffected by it: the hub's own numbers are the hub's.
    assert_eq!(grown.ceiling_bytes, 4 << 20);
}

/// A head that attaches, reconnects or resyncs is told the held counts as they stand:
/// `HeldResults` is ephemeral and said only when it changes.
#[test]
fn a_head_that_attaches_or_reconnects_is_told_the_held_counts() {
    let held = |b: &crate::cursor::Batch| -> Vec<(u64, u64, u64, u64)> {
        b.events()
            .iter()
            .filter_map(|e| match e.event {
                SessionEvent::HeldResults {
                    jobs,
                    subagents,
                    failed,
                    room,
                } => Some((jobs, subagents, failed, room)),
                _ => None,
            })
            .collect()
    };
    // A marker after each attach, so a head with nothing else queued is not left blocked
    // in `next_batch` - the missing count reads as an assertion, not a hang.
    let mark = |hub: &Hub| {
        hub.publish(SessionEvent::Warning {
            code: "interrupt_idle".into(),
            detail: String::new(),
            compaction: None,
        });
    };
    let hub = Hub::new("s-held");
    hub.publish(SessionEvent::HeldResults {
        jobs: 2,
        subagents: 1,
        failed: 0,
        room: 0,
    });

    // A head that attaches after the count was said.
    let late = attach(&hub, 64);
    mark(&hub);
    let Delivery::Events(b) = hub.next_batch(&late.head_id, 64) else {
        panic!("expected events");
    };
    assert_eq!(held(&b), vec![(2, 1, 0, 0)], "the late head was not told");

    // The count goes to zero while a head is away; it reconnects with a replay, which
    // scrubs the ephemeral zero, and must still be told it.
    let since = hub.snapshot().seq;
    hub.detach(&late.head_id);
    hub.publish(SessionEvent::HeldResults {
        jobs: 0,
        subagents: 0,
        failed: 0,
        room: 0,
    });
    let back = hub.attach("tui", "dead@lab2x1", Caps::default(), since);
    assert!(
        back.backlog
            .iter()
            .all(|e| !matches!(e.event, SessionEvent::HeldResults { .. })),
        "the premise: the replay scrubs the held counts"
    );
    mark(&hub);
    let Delivery::Events(b) = hub.next_batch(&back.head_id, 64) else {
        panic!("expected events");
    };
    assert_eq!(
        held(&b),
        vec![(0, 0, 0, 0)],
        "the reconnected head was not told"
    );

    // And a head that takes a resync.
    hub.request_resync(&back.head_id, "test");
    let Delivery::Resync { .. } = hub.next_batch(&back.head_id, 64) else {
        panic!("expected a resync");
    };
    mark(&hub);
    let Delivery::Events(b) = hub.next_batch(&back.head_id, 64) else {
        panic!("expected events");
    };
    assert_eq!(
        held(&b),
        vec![(0, 0, 0, 0)],
        "the resynced head was not told"
    );
}

/// A head that attaches in the middle of a named operation is told which one: the same rule
/// `HeldResults` and `ActivityChanged` follow.
#[test]
fn a_head_that_attaches_mid_operation_is_told_which_one() {
    let fills = |b: &crate::cursor::Batch| -> Vec<(String, u64, u64)> {
        b.events()
            .iter()
            .filter_map(|e| match &e.event {
                SessionEvent::Filling {
                    what, done, total, ..
                } => Some((what.clone(), *done, *total)),
                _ => None,
            })
            .collect()
    };
    let mark = |hub: &Hub| {
        hub.publish(SessionEvent::Warning {
            code: "interrupt_idle".into(),
            detail: String::new(),
            compaction: None,
        });
    };
    let hub = Hub::new("s-filling");
    hub.publish(SessionEvent::Filling {
        what: crate::RESTORING_ROWS.into(),
        unit: "rows".into(),
        done: 0,
        total: 166,
    });

    let late = attach(&hub, 64);
    mark(&hub);
    let Delivery::Events(b) = hub.next_batch(&late.head_id, 64) else {
        panic!("expected events");
    };
    assert_eq!(
        fills(&b),
        vec![(crate::RESTORING_ROWS.to_string(), 0, 166)],
        "the head was handed an empty snapshot and not told why"
    );

    // And a finished operation is not re-announced: the restore's own last tick says
    // `166 of 166`, and a head attaching afterwards must not be told a restore still runs.
    hub.publish(SessionEvent::Filling {
        what: crate::RESTORING_ROWS.into(),
        unit: "rows".into(),
        done: 166,
        total: 166,
    });
    let after = attach(&hub, 64);
    mark(&hub);
    let Delivery::Events(b) = hub.next_batch(&after.head_id, 64) else {
        panic!("expected events");
    };
    assert_eq!(
        fills(&b),
        Vec::new(),
        "a completed fill has nothing left to say"
    );
}

/// A head that attaches inside a retry series is told the series; one that attaches after it
/// is not. The event is the row's whole value, so a replay would draw it at a count the series
/// has already passed - the `HeldResults` rule, and a live row depends on it.
#[test]
fn a_head_that_attaches_mid_series_is_told_the_series_and_a_late_one_is_not() {
    let series = |attempts: u32, ended: Option<&str>| SessionEvent::ModelRetry {
        host: "api.deepseek.com".into(),
        attempts,
        said: format!("retrying: {attempts} attempts, 429 rate limited, 41s"),
        log: vec!["10:31:22 · 429 rate limited".into()],
        ended: ended.map(str::to_string),
        gone: false,
    };
    let rows = |b: &crate::cursor::Batch| -> Vec<u32> {
        b.events()
            .iter()
            .filter_map(|e| match &e.event {
                SessionEvent::ModelRetry { attempts, .. } => Some(*attempts),
                _ => None,
            })
            .collect()
    };
    let mark = |hub: &Hub| {
        hub.publish(SessionEvent::Warning {
            code: "interrupt_idle".into(),
            detail: String::new(),
            compaction: None,
        });
    };
    let hub = Hub::new("s-retrying");
    hub.publish(series(2, None));

    let late = attach(&hub, 64);
    mark(&hub);
    let Delivery::Events(b) = hub.next_batch(&late.head_id, 64) else {
        panic!("expected events");
    };
    assert_eq!(
        rows(&b),
        vec![2],
        "the head was handed a snapshot and not the series it attached inside"
    );

    // The series ends, and a head attaching afterwards is not told a call is being retried.
    hub.detach(&late.head_id);
    hub.publish(series(4, Some("answered after 5 attempts, 52s")));
    let after = attach(&hub, 64);
    mark(&hub);
    let Delivery::Events(b) = hub.next_batch(&after.head_id, 64) else {
        panic!("expected events");
    };
    assert_eq!(
        rows(&b),
        Vec::<u32>::new(),
        "a finished series was re-said as live"
    );

    // And a head that RESYNCS into a running series is told it, the queue being thrown away.
    hub.publish(series(6, None));
    hub.request_resync(&after.head_id, "test");
    let Delivery::Resync { .. } = hub.next_batch(&after.head_id, 64) else {
        panic!("expected a resync");
    };
    mark(&hub);
    let Delivery::Events(b) = hub.next_batch(&after.head_id, 64) else {
        panic!("expected events");
    };
    assert_eq!(rows(&b), vec![6], "the resynced head was not told");
}

#[test]
fn a_promote_request_is_recorded_with_who_and_taken_once() {
    let hub = Hub::new("s");
    let a = attach(&hub, 4);
    // A head asks, and the channel resolves its identity.
    assert_eq!(
        hub.request_promote_from(&a.head_id).as_deref(),
        Some("dead@lab2x1")
    );
    assert_eq!(hub.take_promote_request().as_deref(), Some("dead@lab2x1"));
    // Taken, not read: a second take sees nothing, so the bash wait loop cannot
    // promote the same command twice.
    assert_eq!(hub.take_promote_request(), None);
    // An unattached head resolves to no request.
    assert_eq!(hub.request_promote_from("nobody"), None);
}

/// A wake handed to the session's own reader is taken there, once, and rings no bell: a
/// subagent's settlement is its parent's news and not the worker's.
#[test]
fn a_wake_handed_to_the_own_reader_is_taken_there_and_rings_nothing() {
    use crate::registry::{Registry, SessionWiring, Work, WorkOrIdle};

    // The bell is read without blocking: `next_work` waits for ever when nothing is pending,
    // so "nothing was rung" is asked as "the worker has nothing to do as of now".
    let quiet = |r: &Registry| {
        matches!(
            r.next_work_until(Some(std::time::Instant::now())),
            WorkOrIdle::Idle
        )
    };

    let r = Registry::new();
    let hub = r
        .create("s-child", "", SessionWiring::default())
        .expect("the child registers");
    // Drain the create's `Open`, so the next thing off the bell is the settlement's - and
    // there must not be one.
    assert!(matches!(r.next_work(), Some(Work::Open(id)) if id == "s-child"));

    assert!(hub.wake_its_own_reader(), "a live hub takes a wake");
    assert!(
        matches!(hub.take_own_work(), OwnWork::Wake),
        "the thread that owns the session is woken by it"
    );
    assert!(
        quiet(&r),
        "and the DAEMON's worker is told nothing: this wake is not work it can do"
    );
    // The take is a take: a keeper flag would spend a turn on every pass of a loop
    // that has nothing left to read.
    hub.wake_its_own_reader();
    assert!(matches!(hub.take_own_work(), OwnWork::Wake));

    // A stop put back for its own reader: the worker can win an interrupt the parent aimed
    // at its child, and its answer to one is `interrupt_idle` - false for a child mid-turn.
    let cmd = QueuedCommand {
        head_id: DAEMON_SUBMITTER.to_string(),
        identity: DAEMON_SUBMITTER.to_string(),
        client_request_id: "job_kill-1".into(),
        at_seq: hub.head_seq(),
        enqueued_ms: 0,
        kind: CommandKind::Interrupt {
            reason: "job_kill".into(),
        },
    };
    assert!(hub.give_back_to_its_own_reader(cmd));
    match hub.take_own_work() {
        OwnWork::Command(c) => assert!(
            matches!(c.kind, CommandKind::Interrupt { .. }),
            "the interrupt is back in front of the session's own reader: {c:?}"
        ),
        other => panic!("expected the interrupt back, got {other:?}"),
    }
    assert!(quiet(&r), "and still nothing for the daemon's worker");

    // And a parent's message, the second half of the same fact: a message for a child is a
    // turn the child's own reader starts, so the worker answering it would be both the
    // second writer and a lie.
    let message = QueuedCommand {
        head_id: DAEMON_SUBMITTER.to_string(),
        identity: "s-parent".into(),
        client_request_id: "task_message-1".into(),
        at_seq: hub.head_seq(),
        enqueued_ms: 0,
        kind: CommandKind::Message {
            from: "s-parent".into(),
            text: "stop and report what you have".into(),
        },
    };
    assert!(hub.give_back_to_its_own_reader(message));
    match hub.take_own_work() {
        OwnWork::Command(c) => assert!(
            matches!(c.kind, CommandKind::Message { .. }),
            "the message is back in front of the session's own reader: {c:?}"
        ),
        other => panic!("expected the message back, got {other:?}"),
    }
    assert!(quiet(&r), "and still nothing for the daemon's worker");

    // Only those two go back: a prompt is work the daemon CAN serve, by opening the
    // session, and putting one back would take a head's own words away from it.
    let prompt = QueuedCommand {
        head_id: "h1".into(),
        identity: "dead@lab2x1".into(),
        client_request_id: "c1".into(),
        at_seq: hub.head_seq(),
        enqueued_ms: 0,
        kind: CommandKind::Prompt {
            text: "hello".into(),
            attachments: Vec::new(),
            context: None,
        },
    };
    assert!(
        !hub.give_back_to_its_own_reader(prompt),
        "a prompt is not a stop and is not this door's"
    );

    // A hub that is closed has no reader to hand anything to, which is a `false`
    // rather than a promise: the settlement that prompted a wake was handed up a
    // level when this session stopped (`JobWatchers::stop`).
    hub.close();
    assert!(
        !hub.wake_its_own_reader(),
        "a closed hub has nobody to wake"
    );
    assert!(matches!(hub.take_own_work(), OwnWork::Closed));
}

#[test]
fn a_head_that_was_already_attached_is_sent_the_body_it_was_promised() {
    // The fault this variant exists for. Attaching first is the whole point:
    // a head that attaches *later* was always fine, because the snapshot
    // carried the content, and that is what hid this for so long.
    let hub = Hub::new("s");
    let a = attach(&hub, 64);
    hub.publish(appended("s.0", "user"));
    hub.record_item(
        "s.0",
        letibot_transcript::TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![letibot_transcript::UserPart::Text {
                text: "the operator's own prompt".into(),
            }],
        },
    );
    let Delivery::Events(b) = hub.next_batch(&a.head_id, 64) else {
        panic!("expected events");
    };
    let bodies: Vec<_> = b
        .events()
        .iter()
        .filter(|e| matches!(e.event, SessionEvent::TranscriptContent { .. }))
        .collect();
    assert_eq!(
        bodies.len(),
        1,
        "the body never reached a head that was watching: {:?}",
        b.events()
            .iter()
            .map(|e| e.event.kind())
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_slow_head_is_demoted_and_told_and_never_stalls_the_publisher() {
    let hub = Hub::new("s");
    let a = attach(&hub, 4);
    for i in 0..1000 {
        hub.publish(warn(&format!("w{i}")));
    }
    assert_eq!(hub.head_seq(), 1001, "publishing never blocked");
    match hub.next_batch(&a.head_id, 64) {
        Delivery::Resync {
            reason, snapshot, ..
        } => {
            assert!(reason.contains("overflow"), "{reason}");
            assert_eq!(snapshot.seq, 1001, "and it is told where it now is");
        }
        other => panic!("expected a resync, got {other:?}"),
    }
}

#[test]
fn a_late_head_gets_a_snapshot_and_then_the_very_next_event() {
    let hub = Hub::new("s");
    hub.publish(turn_started("t1"));
    hub.publish(delta("t1", "hel"));
    hub.publish(delta("t1", "lo"));
    let a = attach(&hub, 64);
    assert_eq!(
        a.snapshot.as_ref().unwrap().turn.as_ref().unwrap().text,
        "hello"
    );
    let at = a.snapshot.as_ref().unwrap().seq;
    hub.publish(delta("t1", "!"));
    let Delivery::Events(b) = hub.next_batch(&a.head_id, 64) else {
        panic!()
    };
    assert_eq!(b.events()[0].seq, at + 1, "no gap, no duplicate");
}

#[test]
fn detaching_the_last_head_does_not_stop_anything() {
    let hub = Hub::new("s");
    let a = attach(&hub, 64);
    hub.publish(turn_started("t1"));
    hub.detach(&a.head_id);
    assert_eq!(hub.attached_heads(), 0);
    for i in 0..10 {
        hub.publish(delta("t1", &format!("{i}")));
    }
    // Idle is quiet, not unwatched: the work carried on and a new head sees it.
    let b = attach(&hub, 64);
    assert_eq!(b.snapshot.unwrap().turn.unwrap().text, "0123456789");
}

#[test]
fn a_resume_is_scrubbed_and_the_count_travels_with_it() {
    let hub = Hub::new("s");
    let a = attach(&hub, 64);
    let from = hub.head_seq();
    hub.publish(requested("r1", "rm -rf /"));
    hub.publish(progress("t1"));
    hub.publish(answered("r1", "deny"));
    hub.detach(&a.head_id);

    let b = hub.attach("tui", "dead@lab2x1", Caps::default(), from);
    assert!(
        !b.backlog
            .iter()
            .any(|e| matches!(e.event, SessionEvent::DecisionRequested { .. })),
        "a resumed head must not be re-asked a settled question"
    );
    assert_eq!(b.scrubbed.settled_decisions, 1);
    assert_eq!(b.scrubbed.prompt_progress, 1);
}

#[test]
fn answering_a_settled_decision_is_refused_rather_than_held() {
    let hub = Hub::new("s");
    let a = attach(&hub, 64);
    hub.publish(requested("r1", "rm -rf /"));
    hub.publish(answered("r1", "deny"));
    let f = hub.submit(
        &a.head_id,
        "c1",
        0,
        CommandKind::Answer {
            req_id: "r1".into(),
            reply: Reply::Permission {
                option_id: "allow".into(),
                pattern: None,
                note: None,
            },
        },
    );
    assert!(matches!(
        f,
        ServerFrame::Rejected { ref reason, .. } if reason == REJECT_UNKNOWN_DECISION
    ));
}

#[test]
fn a_second_heads_prompt_is_queued_not_rejected_and_says_who() {
    let hub = Hub::new("s");
    let a = hub.attach("tui", "alice", Caps::default(), 0);
    let b = hub.attach("remote", "bob", Caps::default(), 0);
    hub.publish(turn_started("t1"));
    let f = hub.submit(
        &b.head_id,
        "c9",
        1, // deliberately stale
        CommandKind::Prompt {
            text: "also check the logs".into(),
            attachments: Vec::new(),
            context: None,
        },
    );
    let ServerFrame::Accepted { note, .. } = f else {
        panic!("a prompt from a second head must not be rejected")
    };
    assert!(note.contains("queued"), "{note}");

    // Both heads see it, and who did it.
    let Delivery::Events(batch) = hub.next_batch(&a.head_id, 64) else {
        panic!()
    };
    let issued = batch
        .events()
        .iter()
        .find_map(|e| match &e.event {
            SessionEvent::CommandIssued {
                identity, command, ..
            } => Some((identity.clone(), command.clone())),
            _ => None,
        })
        .expect("the queuing is announced");
    assert_eq!(issued, ("bob".to_string(), "prompt".to_string()));

    let cmd = hub.try_command().unwrap();
    assert_eq!(cmd.identity, "bob");
}

#[test]
fn a_read_only_head_cannot_answer() {
    let hub = Hub::new("s");
    let a = hub.attach(
        "flowy",
        "room:general",
        Caps {
            can_decide: false,
            ..Caps::default()
        },
        0,
    );
    hub.publish(requested("r1", "rm -rf /"));
    let f = hub.submit(
        &a.head_id,
        "c1",
        0,
        CommandKind::Answer {
            req_id: "r1".into(),
            reply: Reply::Permission {
                option_id: "allow".into(),
                pattern: None,
                note: None,
            },
        },
    );
    assert!(matches!(f, ServerFrame::Rejected { ref reason, .. } if reason == REJECT_READ_ONLY));
}

#[test]
fn a_question_answer_settles_the_same_request_a_permission_would() {
    let hub = Hub::new("s");
    let a = attach(&hub, 64);
    hub.publish(asked(
        "r1",
        "which approach?",
        &["rebuild first", "patch in place"],
        "both work and they differ in cost",
    ));
    let f = hub.submit(
        &a.head_id,
        "c1",
        0,
        CommandKind::Answer {
            req_id: "r1".into(),
            reply: Reply::Question(
                crate::question::QuestionAnswer::choosing(1).with_note("only on CUDA"),
            ),
        },
    );
    match f {
        ServerFrame::Accepted { note, .. } => assert_eq!(note, "question answered"),
        other => panic!("expected acceptance, got {other:?}"),
    }
}

/// The deadlock this seam exists to avoid, as a test: with a sink installed the answer must
/// not also be on the command queue.
#[test]
fn an_answer_reaches_the_sink_and_never_the_queue() {
    struct Recorder(Mutex<Vec<(String, String, String)>>);
    impl AnswerSink for Recorder {
        fn answer(&self, req_id: &str, identity: &str, reply: &Reply) -> bool {
            self.0.lock().unwrap().push((
                req_id.to_string(),
                identity.to_string(),
                reply.as_str().to_string(),
            ));
            true
        }
        fn cancel(&self, _why: &str) {}
        fn describe(&self) -> String {
            "recorder".into()
        }
    }
    let hub = Hub::new("s");
    let sink = Arc::new(Recorder(Mutex::new(Vec::new())));
    hub.set_answer_sink(sink.clone());
    let a = hub.attach("tui", "alice", Caps::default(), 0);
    hub.publish(requested("r1", "write src/main.rs"));

    let f = hub.submit(
        &a.head_id,
        "c1",
        0,
        CommandKind::Answer {
            req_id: "r1".into(),
            reply: Reply::Permission {
                option_id: "allow".into(),
                pattern: None,
                note: None,
            },
        },
    );
    assert!(matches!(f, ServerFrame::Accepted { .. }), "{f:?}");
    assert_eq!(
        sink.0.lock().unwrap().as_slice(),
        [(
            "r1".to_string(),
            "alice".to_string(),
            "permission".to_string()
        )],
        "the answer must reach whoever is blocked on it, with who said it"
    );
    assert!(
        hub.try_command().is_none(),
        "an answer must not also be queued: the worker that would drain it is the \
             thread waiting for it"
    );
    // And the announcement still happens, because a second head has to see that
    // somebody answered.
    let Delivery::Events(batch) = hub.next_batch(&a.head_id, 64) else {
        panic!()
    };
    assert!(
        batch.events().iter().any(|e| matches!(
            &e.event,
            SessionEvent::CommandIssued { command, .. } if command == "answer"
        )),
        "the answer is still announced on the log"
    );
}

/// Without a sink nothing changes: the command queues, which is what a daemon
/// with no adjudication does and what every other test here drives.
#[test]
fn with_no_sink_an_answer_queues_exactly_as_it_did() {
    let hub = Hub::new("s");
    let a = attach(&hub, 64);
    hub.publish(requested("r1", "rm -rf /"));
    hub.submit(
        &a.head_id,
        "c1",
        0,
        CommandKind::Answer {
            req_id: "r1".into(),
            reply: Reply::Permission {
                option_id: "allow".into(),
                pattern: None,
                note: None,
            },
        },
    );
    let cmd = hub.try_command().expect("queued as before");
    assert!(matches!(cmd.kind, CommandKind::Answer { .. }));
}

/// A question's answer may not settle a permission: both types exist so a free-form
/// sentence cannot land where a policy engine reads a grant, where it would validate
/// against an empty ladder and be accepted.
#[test]
fn the_two_answer_vocabularies_do_not_cross() {
    let hub = Hub::new("s");
    let a = attach(&hub, 64);
    hub.publish(requested("r1", "write src/main.rs"));
    hub.publish(asked("r2", "which?", &["a", "b"], "stuck"));

    let f = hub.submit(
        &a.head_id,
        "c1",
        0,
        CommandKind::Answer {
            req_id: "r1".into(),
            reply: Reply::Question(crate::question::QuestionAnswer::free("go ahead")),
        },
    );
    assert!(
        matches!(f, ServerFrame::Rejected { ref reason, .. } if reason.contains("permission")),
        "{f:?}"
    );

    let f = hub.submit(
        &a.head_id,
        "c2",
        0,
        CommandKind::Answer {
            req_id: "r2".into(),
            reply: Reply::Permission {
                option_id: "allow".into(),
                pattern: None,
                note: None,
            },
        },
    );
    assert!(
        matches!(f, ServerFrame::Rejected { ref reason, .. } if reason.contains("question")),
        "{f:?}"
    );
}

#[test]
fn a_malformed_question_answer_leaves_the_question_open() {
    // An answer that does not conform is refused HERE, before anything acts on it:
    // accepting it would leave a settled decision behind an unanswered question.
    let hub = Hub::new("s");
    let a = attach(&hub, 64);
    hub.publish(asked(
        "r1",
        "which approach?",
        &["rebuild first", "patch in place"],
        "both work",
    ));
    for bad in [
        crate::question::QuestionAnswer::default(),
        crate::question::QuestionAnswer::choosing(9),
        crate::question::QuestionAnswer {
            note: Some("hmm".into()),
            ..Default::default()
        },
    ] {
        let f = hub.submit(
            &a.head_id,
            "c1",
            0,
            CommandKind::Answer {
                req_id: "r1".into(),
                reply: Reply::Question(bad.clone()),
            },
        );
        match f {
            ServerFrame::Rejected { reason, .. } => {
                assert!(
                    reason.starts_with(crate::question::REJECT_MALFORMED_ANSWER),
                    "{reason}"
                );
                assert!(reason.contains("still open"), "{reason}");
            }
            other => panic!("{bad:?} was accepted: {other:?}"),
        }
    }
    // And it really is still open, so the person can be asked again.
    assert!(
        hub.snapshot()
            .open_decisions
            .iter()
            .any(|d| d.req_id == "r1"),
        "the question must survive a malformed answer"
    );
}
