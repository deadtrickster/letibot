use super::*;

/// The three things a limit does to a session, in the words the channel carries: an instant the
/// provider named, the model the daemon moved to, and the resume it made by itself.
#[test]
fn a_limit_reads_as_limited_until_switched_to_or_resumed() {
    let news = LimitNews {
        kind: "limited",
        model: "glm/glm-5.3".into(),
        until_ms: Some(1_794_085_347_000),
        switched_to: None,
        said: "the provider put a time on its limit".into(),
    };
    assert_eq!(news.line(), "glm/glm-5.3 limited until 21:02:27 UTC");
    let moved = LimitNews {
        switched_to: Some("deepseek/deepseek-flash".into()),
        ..news.clone()
    };
    assert_eq!(
        moved.line(),
        "glm/glm-5.3 limited until 21:02:27 UTC, switched to deepseek/deepseek-flash"
    );
    let resumed = LimitNews {
        kind: "resumed",
        until_ms: None,
        switched_to: None,
        said: "the limit on glm/glm-5.3 lifted".into(),
        ..news.clone()
    };
    assert_eq!(resumed.line(), "the limit on glm/glm-5.3 lifted");
    // A park names no model move and still says when.
    let waiting = LimitNews {
        kind: "limited",
        until_ms: Some(1_794_085_347_000),
        said: String::new(),
        ..news.clone()
    };
    assert_eq!(waiting.line(), "glm/glm-5.3 limited until 21:02:27 UTC");
}

/// A daemon on an older protocol number is named in the status, and a line this build cannot
/// read is counted there instead of vanishing.
#[test]
fn skew_and_unreadable_lines_reach_the_status() {
    use std::io::{BufRead, BufReader, Write};
    let socket = letibot_sessionlog::testing::scratch_root()
        .join(format!("bridge-skew-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut lines = BufReader::new(stream.try_clone().expect("clone")).lines();
        let _attach = lines.next();
        let hello = ServerFrame::Hello {
            protocol_version: letibot_sessionlog::protocol::PROTOCOL_VERSION - 1,
            session_id: "s".into(),
            head_id: "h1".into(),
            dropped: 0,
            snapshot: Some(Box::new(
                letibot_sessionlog::view::SessionView::new(
                    "s",
                    letibot_sessionlog::view::ViewBounds::default(),
                )
                .snapshot(0, 0),
            )),
            resumed_from: None,
            scrubbed: letibot_sessionlog::ScrubReport::default(),
            wiring: letibot_sessionlog::SessionWiring::default(),
            sessions: Vec::new(),
        };
        let _ = writeln!(stream, "{}", serde_json::to_string(&hello).unwrap());
        let _ = writeln!(stream, "{{\"frame\":\"from_the_future\"}}");
        for _ in lines {}
    });
    let d = crate::daemon::Daemon {
        workspace: socket.clone(),
        key: "k".into(),
        socket,
        log: Default::default(),
        store: Default::default(),
        placement: None,
        started: false,
    };
    let h = Head::attach(&d, "s").expect("attach");
    let end = Instant::now() + Duration::from_secs(10);
    while h.snapshot().unreadable == 0 {
        assert!(
            Instant::now() < end,
            "the unreadable line was never counted"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let s = h.snapshot();
    assert_eq!(s.unreadable, 1);
    let skew = s.skew.expect("a daemon one protocol behind is named");
    assert!(
        skew.contains("OLDER") && skew.contains("letibot --stop"),
        "{skew}"
    );
}

/// A close the daemon never acks is reported with the wait, not as a closed session.
#[test]
fn a_close_nobody_acks_is_reported_as_not_confirmed() {
    let socket = letibot_sessionlog::testing::scratch_root()
        .join(format!("bridge-noack-{}.sock", std::process::id()));
    let hub = letibot_sessionlog::hub::Hub::new("s");
    let server = letibot_sessionlog::server::serve(hub, socket.clone()).expect("bind");
    let d = crate::daemon::Daemon {
        workspace: socket.clone(),
        key: "k".into(),
        socket,
        log: Default::default(),
        store: Default::default(),
        placement: None,
        started: false,
    };
    let h = Head::attach(&d, "s").expect("attach");
    // No time to be acked: the answer says so rather than claiming a close.
    let why = h
        .stop_within(Duration::ZERO)
        .expect_err("no time to be acked");
    assert!(why.contains("did not ack"), "{why}");
    server.shutdown();
}

/// A daemon that accepts the attach and never answers `/models`: the silence is reported with
/// the wait that ran out and what that means, and the real wait is longer than one round trip.
#[test]
fn a_silent_session_is_reported_with_the_wait_and_the_cause() {
    let socket = letibot_sessionlog::testing::scratch_root()
        .join(format!("bridge-silent-{}.sock", std::process::id()));
    let server =
        letibot_sessionlog::server::serve(letibot_sessionlog::hub::Hub::new("s"), socket.clone())
            .expect("bind");
    let d = crate::daemon::Daemon {
        workspace: socket.clone(),
        key: "k".into(),
        socket,
        log: Default::default(),
        store: Default::default(),
        placement: None,
        started: false,
    };
    let h = Head::attach(&d, "s").expect("attach");
    let why = h
        .set_model_within("glm", Duration::from_millis(300))
        .expect_err("nobody answers");
    assert!(
        why.contains("300ms") && why.contains("still opening") && why.contains("once"),
        "{why}"
    );
    assert!(
        MODEL_PATIENCE >= Duration::from_secs(60),
        "a host's first open fits"
    );
    server.shutdown();
}

/// A question is answered by its choice, its number, typed words, or an abstention.
#[test]
fn a_question_is_answered_by_choice_number_words_or_abstention() {
    let choices = vec!["Lift it".to_string(), "Stop".to_string()];
    let by_text = question_answer("Stop", None, &choices).expect("a choice by its words");
    assert_eq!(by_text.option, Some(1));
    let by_number = question_answer("1", Some("and say why"), &choices).expect("by number");
    assert_eq!(by_number.option, Some(0));
    assert_eq!(by_number.note.as_deref(), Some("and say why"));
    let free = question_answer("free:neither", None, &choices).expect("typed words");
    assert_eq!(free.free.as_deref(), Some("neither"));
    assert!(
        question_answer("abstain", None, &choices)
            .expect("abstain")
            .abstain
    );
    let refused = question_answer("3", None, &choices).expect_err("no third choice");
    assert!(
        refused.contains("1. Lift it") && refused.contains("2. Stop"),
        "{refused}"
    );
}
use letibot_sessionlog::testing;
use letibot_transcript::Backgrounding;

/// One recorded event, as the fold receives it: the log's own envelope, never a
/// hand-built shape - a head that folded something the daemon does not send would pass
/// every test here and fail on the first real session.
fn env(seq: u64, event: SessionEvent) -> Envelope {
    Envelope {
        session_id: "s-recorded".into(),
        seq,
        ts: 1_700_000_000_000 + seq,
        event,
    }
}

fn finished(call_id: &str, outcome: ToolOutcome) -> SessionEvent {
    SessionEvent::ToolFinished {
        turn_id: "t1".into(),
        call_id: call_id.into(),
        outcome,
        payload_digest: "fnv1a:0000000000000000".into(),
        inline_bytes: 0,
        full_bytes: 0,
        spill: None,
        repairs: 0,
        edits: Vec::new(),
    }
}

fn settled(job: &str, state: &str, elapsed_ms: u64) -> SessionEvent {
    SessionEvent::JobSettled {
        job: job.into(),
        state: state.into(),
        produced: 4096,
        elapsed_ms,
        edits: Vec::new(),
    }
}

fn jobs_reply(seq: u64, running: &[&str]) -> ServerFrame {
    ServerFrame::Jobs {
        session_id: "s".into(),
        seq,
        monitors: Vec::new(),
        jobs: running
            .iter()
            .map(|id| letibot_sessionlog::protocol::JobEntry {
                id: (*id).into(),
                command: String::new(),
                slug: String::new(),
                how: "asked".into(),
                state: "running".into(),
                running: true,
                never_ran: false,
                redirect: None,
                produced: 0,
                cgroup: String::new(),
                elapsed_ms: 0,
            })
            .collect(),
    }
}

/// The attach-time `ListJobs` answer can land after a job started. The head discards it,
/// asks again, and ends with the job listed from the daemon's newer answer.
#[test]
fn a_job_list_older_than_a_job_event_is_discarded_and_asked_again() {
    let mut state = State {
        jobs_in_flight: true,
        ..State::default()
    };
    state.fold(ServerFrame::Event(env(
        7,
        finished(
            "c1",
            ToolOutcome::Backgrounded {
                handle: "j1".into(),
                ran_for_ms: 1,
                how: Backgrounding::Asked,
                next: String::new(),
            },
        ),
    )));
    assert!(!state.take_jobs_ask(), "one ask is already out");
    // The reply built at seq 5, before the job existed.
    state.fold(jobs_reply(5, &[]));
    assert!(state.jobs_open.is_empty());
    assert!(state.take_jobs_ask(), "a stale list asks again");
    assert!(!state.take_jobs_ask(), "and only once");
    // The answer to the second ask is built after the event.
    state.fold(jobs_reply(8, &["j1"]));
    assert!(state.jobs_open.contains("j1"), "the job is listed");
    assert!(!state.take_jobs_ask());
    // A daemon that does not stamp is taken as it answers.
    state.fold(jobs_reply(0, &[]));
    assert!(state.jobs_open.is_empty());
}

fn assistant(text: &str) -> SessionEvent {
    SessionEvent::TranscriptContent {
        item_id: "tr.9".into(),
        item: Box::new(TranscriptItem::Assistant {
            text: text.into(),
            tool_calls: Vec::new(),
            truncated: false,
        }),
    }
}

/// A snapshot as the daemon cuts one: events folded by the view this crate does not own.
/// Used where a test needs what a head that attached LATE was handed, which is the one path
/// the fold alone cannot exercise.
fn snapshot_of(events: Vec<SessionEvent>) -> Snapshot {
    let mut view = letibot_sessionlog::view::SessionView::new(
        "s-recorded",
        letibot_sessionlog::view::ViewBounds::default(),
    );
    for (i, event) in events.into_iter().enumerate() {
        view.apply(&env(i as u64, event));
    }
    view.snapshot(0, 0)
}

/// A round's end is not the turn's, and the head no longer has to work that out. The
/// engine publishes `TurnFinished` per round and the round's calls run after it; what this
/// reads now is the daemon's `Activity`, quiet only when there is nothing left.
#[test]
fn the_daemons_activity_says_what_the_rounds_events_cannot() {
    let mut state = State::default();
    let mut seq = 0;
    let mut feed = |state: &mut State, event: SessionEvent| {
        seq += 1;
        state.fold(ServerFrame::Event(env(seq, event)));
    };
    let activity = |waiting_on: Vec<Waiting>| SessionEvent::ActivityChanged {
        activity: Activity::of(waiting_on),
    };
    feed(&mut state, testing::turn_started("t1"));
    feed(&mut state, activity(vec![Waiting::Turn]));
    assert_eq!(state.word(), "running");

    // The round ended and its calls are running: still busy, and the round's own
    // `TurnFinished` below does not change that.
    feed(&mut state, activity(vec![Waiting::Calls]));
    feed(&mut state, testing::turn_finished("t1"));
    assert_eq!(
        state.word(),
        "running",
        "the round's calls are still to run"
    );

    // The next round starts, and the whole turn ends quiet.
    feed(&mut state, activity(vec![Waiting::Turn]));
    assert_eq!(state.word(), "running");
    feed(&mut state, testing::turn_finished("t1"));
    feed(&mut state, activity(Vec::new()));
    assert_eq!(state.word(), "idle", "nothing left: the turn is over");
    assert!(state.activity.is_quiet());

    // Published only when it changes is the daemon's, so a repeated value is not news
    // here either: the ring gains no second entry for a value it already held.
    let before = state.events.page(0, RING).next;
    feed(&mut state, activity(Vec::new()));
    assert_eq!(
        state.events.page(0, RING).next,
        before,
        "an unchanged value says nothing"
    );
}

/// A turn that ended to wait on its own job is not the end of the work, and whether a
/// job holds the work open is the DAEMON's answer now. The job the model PARKED with
/// `wake: "later"` is not something the work waits on and must not hold a later turn open.
#[test]
fn a_job_the_work_waits_on_keeps_the_session_busy_and_a_server_does_not() {
    let mut state = State::default();
    let mut seq = 0;
    let mut feed = |state: &mut State, event: SessionEvent| {
        seq += 1;
        state.fold(ServerFrame::Event(env(seq, event)));
    };
    let activity = |waiting_on: Vec<Waiting>| SessionEvent::ActivityChanged {
        activity: Activity::of(waiting_on),
    };
    feed(&mut state, testing::turn_started("t1"));
    feed(
        &mut state,
        testing::proposed_on("t1", "c1", "bash", "cargo test"),
    );
    feed(
        &mut state,
        finished(
            "c1",
            ToolOutcome::Backgrounded {
                handle: "j9".into(),
                ran_for_ms: 900,
                how: Backgrounding::Asked,
                next: "job_output j9".into(),
            },
        ),
    );
    state.fold(jobs_reply(0, &["j9"]));
    feed(&mut state, testing::turn_finished("t1"));
    // An open job the model PARKED (`bash {background: true, wake: "later"}`), which is
    // what a dev server left running is: the daemon says the session is quiet, and this head
    // says exactly what the daemon said.
    feed(&mut state, activity(Vec::new()));
    assert_eq!(state.word(), "idle");
    assert!(state.jobs_open.contains("j9"), "the job is still running");
    assert!(
        !state.waiting_on_jobs(),
        "a job the model parked holds nothing open"
    );

    // The work waits on it now. The daemon re-reads the board and says `job j9`.
    feed(&mut state, activity(vec![Waiting::Job("j9".into())]));
    assert!(state.waiting_on_jobs(), "the plan depends on j9");
    assert_eq!(state.waiting_on(), vec!["job j9".to_string()]);

    // It settled, and the turn its settlement wakes has not started: the daemon says
    // `delivery`, which is the window QC's `wait` exited in.
    feed(&mut state, settled("j9", "exited 0", 60_000));
    state.fold(jobs_reply(0, &[]));
    assert!(state.jobs_open.is_empty(), "its settlement closes it");
    feed(&mut state, activity(vec![Waiting::Delivery]));
    assert!(!state.activity.is_quiet(), "the wake is owed");
    assert!(
        !state.waiting_on_jobs(),
        "the job is over; what is left is the turn it wakes"
    );

    feed(&mut state, testing::turn_started("t1"));
    feed(&mut state, activity(vec![Waiting::Turn]));
    assert_eq!(state.word(), "running", "the wake started");
}

/// A head that attaches mid-turn is not blind: the value travels in the snapshot, so a
/// `wait` that attached as the brief started sees `turn` rather than an idle session.
#[test]
fn a_snapshot_carries_the_activity_it_was_taken_with() {
    let mut state = State::default();
    let mut snap = snapshot_of(testing::one_of_each());
    snap.activity = Activity::of(vec![Waiting::Calls, Waiting::Job("j8032".into())]);
    state.seed(&snap);
    assert_eq!(state.word(), "running");
    assert!(state.waiting_on_jobs());
    assert_eq!(
        state.waiting_on(),
        vec!["calls".to_string(), "job j8032".to_string()]
    );
}

/// A round's finish is not the turn's: the state says *over* only on the frame the daemon marked
/// as the prompt's end, so a bridge that reported on every round reported over running work.
#[test]
fn a_rounds_finish_does_not_end_the_turn_until_the_daemon_marks_it() {
    let mut state = State::default();
    let mut seq = 0;
    let mut feed = |state: &mut State, event: SessionEvent| {
        seq += 1;
        state.fold(ServerFrame::Event(env(seq, event)));
    };
    feed(&mut state, testing::turn_started("t1"));
    feed(&mut state, testing::turn_finished_round("t1"));
    assert_eq!(
        state.rounds, 1,
        "the round is counted, and its figures with it"
    );
    assert_eq!(
        state.turn,
        Turn::Running,
        "a round that proposed calls is not the turn's end"
    );
    feed(&mut state, testing::turn_finished("t1"));
    assert_eq!(state.turn, Turn::Idle, "the marked end is");
}

/// A row that rode its own announcement is read from it: the daemon sends the body no second time
/// to a head that declared `row_bodies`, so the bridge's last message must come from the append.
#[test]
fn a_row_on_its_announcement_is_the_last_message() {
    let mut state = State::default();
    state.fold(ServerFrame::Event(env(
        1,
        SessionEvent::TranscriptAppended {
            item_id: "tr.1".into(),
            kind: "assistant".into(),
            ledger_head: String::new(),
            item: Some(Box::new(TranscriptItem::Assistant {
                text: "done, all green".into(),
                tool_calls: Vec::new(),
                truncated: false,
            })),
            body_bytes: 15,
        },
    )));
    assert_eq!(state.last_message(), "done, all green");
}

/// The report outlives the chatter after it, and the spend is the package's. A run that
/// reported, was woken by its own jobs, said *nothing new in them* and stopped: the
/// report must still be there, and the cost must be every round's, not the last one's.
#[test]
fn the_report_and_the_spend_survive_the_rounds_after_them() {
    let mut state = State::default();
    let mut seq = 0;
    let mut feed = |state: &mut State, event: SessionEvent| {
        seq += 1;
        state.fold(ServerFrame::Event(env(seq, event)));
    };
    let priced = |micros| match testing::turn_finished("t1") {
        SessionEvent::TurnFinished {
            turn_id,
            finish_reason,
            mut usage,
            timings,
            ends_turn,
            prefix_check,
            dialect_template_sha,
        } => {
            usage.cost_micros_usd = Some(micros);
            SessionEvent::TurnFinished {
                turn_id,
                finish_reason,
                usage,
                timings,
                ends_turn,
                prefix_check,
                dialect_template_sha,
            }
        }
        _ => unreachable!(),
    };
    let report = "## What I built\n\nthe pane\n\n## Test results\n\nall green";
    feed(&mut state, testing::turn_started("t1"));
    feed(&mut state, assistant(report));
    feed(&mut state, priced(1_500));
    feed(&mut state, testing::turn_started("t1"));
    feed(&mut state, assistant("Nothing new in those jobs."));
    feed(&mut state, priced(250));

    assert_eq!(state.last_message(), "Nothing new in those jobs.");
    assert_eq!(state.last_report.as_deref(), Some(report));
    assert_eq!(state.rounds, 2);
    assert_eq!(state.spent.cost_micros_usd, Some(1_750));
    assert_eq!(state.spent.prompt_tokens, 2_000);
    assert_eq!(
        state.meter.cost_micros_usd,
        Some(250),
        "the meter is one round"
    );
}

/// The two facts reach `leticode_status`: which check judged the last ending and
/// which dialect rendered it. Without the check an orchestrator reads zero reuse as a prompt
/// that diverged; a daemon older than the fields answers `null` and an empty string.
#[test]
fn an_endings_prefix_check_and_dialect_reach_the_status() {
    let mut state = State::default();
    let mut seq = 0;
    let mut feed = |state: &mut State, event: SessionEvent| {
        seq += 1;
        state.fold(ServerFrame::Event(env(seq, event)));
    };
    feed(&mut state, testing::turn_started("t1"));
    feed(
        &mut state,
        SessionEvent::TurnFinished {
            turn_id: "t1".into(),
            finish_reason: letibot_sessionlog::event::FinishReason::Eos,
            usage: Default::default(),
            timings: Default::default(),
            ends_turn: true,
            prefix_check: Some(letibot_sessionlog::event::PrefixCheck::Held {
                expected_cached_min: 12_340,
                cached: 12_300,
                shortfall: 40,
            }),
            dialect_template_sha: "3f2a9c".into(),
        },
    );
    let measured: std::collections::BTreeMap<&str, Value> =
        state.measured_keys().into_iter().collect();
    assert_eq!(
        measured.get("dialect_template_sha"),
        Some(&json!("3f2a9c")),
        "{measured:?}"
    );
    assert_eq!(
        measured.get("prefix_check"),
        Some(&json!({"held": {
            "expected_cached_min": 12_340,
            "cached": 12_300,
            "shortfall": 40,
        }})),
        "the verdict is the daemon's, in its own shape: {measured:?}"
    );

    // A daemon older than the two fields: the keys are present and say nothing was measured,
    // which is a different statement from a check that ran.
    let old: std::collections::BTreeMap<&str, Value> =
        State::default().measured_keys().into_iter().collect();
    assert_eq!(old.get("prefix_check"), Some(&Value::Null), "{old:?}");
    assert_eq!(old.get("dialect_template_sha"), Some(&json!("")), "{old:?}");
}

/// A recorded turn folds into the compact entries `leticode_events` answers with. The
/// sequence is a turn as this daemon actually publishes one, with the interactive noise
/// (deltas, prompt progress, token counts) interleaved where the daemon puts it.
#[test]
fn a_recorded_turn_folds_into_compact_entries() {
    let mut state = State::default();
    let mut seq = 0;
    let mut feed = |state: &mut State, event: SessionEvent| {
        seq += 1;
        state.fold(ServerFrame::Event(env(seq, event)));
    };
    feed(&mut state, testing::turn_started("t1"));
    feed(&mut state, testing::progress("t1"));
    feed(&mut state, testing::tokens_generated("t1", 42));
    feed(&mut state, testing::delta("t1", "thinking about it"));
    feed(
        &mut state,
        testing::proposed_on("t1", "c1", "bash", "cargo test --offline -p letibot-bridge"),
    );
    feed(&mut state, finished("c1", ToolOutcome::Ok));
    feed(
        &mut state,
        testing::requested("r1", "bash wants exec access"),
    );
    feed(&mut state, testing::answered("r1", "deny"));
    feed(
        &mut state,
        testing::proposed_on("t1", "c2", "bash", "cargo test --workspace"),
    );
    feed(
        &mut state,
        finished(
            "c2",
            ToolOutcome::Backgrounded {
                handle: "j7".into(),
                ran_for_ms: 1_200,
                how: Backgrounding::Asked,
                next: "job_output j7".into(),
            },
        ),
    );
    feed(&mut state, settled("j7", "exited 0", 30_400));
    feed(
        &mut state,
        testing::warn("the prompt moved, so this session is re-seated"),
    );
    feed(
        &mut state,
        assistant("## What I built\n\nthe ring\n\n## Test results\n\nall green"),
    );
    feed(&mut state, testing::turn_finished("t1"));

    let page = state.events.page(0, 50);
    let said: Vec<(&str, &str, &str, &str)> = page
        .entries
        .iter()
        .map(|e| (e.kind, e.state, e.name.as_str(), e.text.as_str()))
        .collect();
    assert_eq!(
        said,
        vec![
            ("turn", "started", "", ""),
            (
                "tool",
                "proposed",
                "bash",
                "cargo test --offline -p letibot-bridge"
            ),
            ("result", "ok", "bash", ""),
            ("ask", "opened", "r1", "bash wants exec access"),
            ("ask", "answered", "r1", "deny"),
            ("tool", "proposed", "bash", "cargo test --workspace"),
            ("job", "started", "j7", ""),
            ("job", "ended", "j7", "exited 0"),
            (
                "warning",
                "",
                "",
                "the prompt moved, so this session is re-seated"
            ),
            ("message", "", "", "## What I built"),
            ("turn", "ended", "", ""),
        ],
        "the fold's whole vocabulary: {said:?}"
    );
    // Deltas, prompt progress and token counts are `status`'s business and not the
    // ring's: an entry per delta would be the transcript.
    assert!(
        page.entries
            .iter()
            .all(|e| !e.text.contains("thinking about it")),
        "a delta is not an entry: {said:?}"
    );
    // The cursors are one per entry, in order, starting at 1.
    assert_eq!(
        page.entries.iter().map(|e| e.cursor).collect::<Vec<_>>(),
        (1..=11).collect::<Vec<u64>>()
    );
    assert_eq!(page.next, 11);
    assert_eq!(page.dropped, 0);
    // And the facts the entry carries beyond its words: a job's duration, a card's
    // kind, and the message clamped to its first line.
    let job = page
        .entries
        .iter()
        .find(|e| e.kind == "job")
        .expect("a job");
    assert_eq!(job.ms, Some(1_200));
    let ask = page
        .entries
        .iter()
        .find(|e| e.kind == "ask")
        .expect("an ask");
    assert_eq!(ask.code, "permission");
    let answered = page
        .entries
        .iter()
        .find(|e| e.state == "answered")
        .expect("an answer");
    assert_eq!(answered.code, "human", "who settled it, from the event");
    let message = page
        .entries
        .iter()
        .find(|e| e.kind == "message")
        .expect("a message");
    assert_eq!(
        message.text, "## What I built",
        "the first line, and only it"
    );

    // The same turn through the tool's own JSON: the fields that are set, and no others.
    let json = message.to_json();
    assert_eq!(json["kind"], "message");
    assert_eq!(json["cursor"], 10);
    assert!(json.get("state").is_none(), "{json}");
    assert!(json.get("name").is_none(), "{json}");
    let ended = page.entries.last().expect("the last entry").to_json();
    assert_eq!(ended["state"], "ended");
    assert_eq!(page.entries[7].to_json()["ms"], 30_400);
}

/// A tool's ending says what happened to it, and a failure carries the reason.
#[test]
fn a_failed_call_and_a_failed_turn_are_named() {
    let mut state = State::default();
    state.event(env(
        1,
        testing::proposed_on("t1", "c1", "bash", "git push origin main"),
    ));
    state.event(env(
        2,
        finished(
            "c1",
            ToolOutcome::Failed {
                reason: "exit 128: no configured push destination".into(),
            },
        ),
    ));
    state.event(env(
        3,
        SessionEvent::TurnFailed {
            turn_id: "t1".into(),
            error: "the provider refused round 3".into(),
            partial_kept: false,
        },
    ));
    let page = state.events.page(0, 50);
    assert_eq!(page.entries[1].state, "failed");
    assert_eq!(
        page.entries[1].name, "bash",
        "the call's name, joined by id"
    );
    assert_eq!(
        page.entries[1].text,
        "exit 128: no configured push destination"
    );
    assert_eq!(page.entries[2].kind, "failure");
    assert_eq!(page.entries[2].code, "turn_failed");
    assert_eq!(page.entries[2].text, "the provider refused round 3");
    assert!(state.turned, "a failed turn is a turn that ran");
}

/// A reviewer's start and end are two lines, and they name the branch. A reviewer
/// is a child session, and the only thing a head attached to the parent is told about it is
/// `SessionEvent::Subagent` - so this is what `leticode_events` on `s-gatekeeper` can say.
#[test]
fn a_reviewers_start_and_end_are_one_line_each() {
    let task = "You are the gatekeeper. A subagent has finished work on a branch.\n\n\
                    The ask, verbatim:\n\nkeep the last record\n\nThe placement:\n\
                    - branch: agent/P1\n- head:   abc123\n- base:   def456\n";
    let mut state = State::default();
    let mut seq = 0;
    let mut child = |state: &mut State, child_state: &str, said: &str, answer: Option<&str>| {
        seq += 1;
        state.fold(ServerFrame::Event(env(
            seq,
            SessionEvent::Subagent {
                subagent_id: "s-sub-1".into(),
                state: child_state.into(),
                prompt: said.into(),
                role: "gatekeeper".into(),
                task: task.into(),
                model: String::new(),
                answer: answer.map(str::to_string),
                slug: "keep-the-last-record".into(),
            },
        )));
    };
    child(&mut state, "opening", "You are the gatekeeper.", None);
    // The second state of the same child is not a second start: `opening` then `running`
    // is one reviewer beginning, and two entries would say it began twice.
    child(&mut state, "running", "You are the gatekeeper.", None);
    child(
        &mut state,
        "done",
        "accepted: the reader keeps the last record",
        Some("accepted: the reader keeps the last record"),
    );
    let page = state.events.page(0, 50);
    let said: Vec<(&str, &str, &str, &str)> = page
        .entries
        .iter()
        .map(|e| (e.kind, e.state, e.name.as_str(), e.text.as_str()))
        .collect();
    assert_eq!(
        said,
        vec![
            ("subagent", "started", "keep-the-last-record", "agent/P1"),
            (
                "subagent",
                "ended",
                "keep-the-last-record",
                "agent/P1 — accepted: the reader keeps the last record",
            ),
        ],
        "one line per reviewer per phase: {said:?}"
    );

    // A child that FAILED carries no answer, so the line falls back to what it was asked
    // - an ending that named no branch and said nothing would be a row nobody can act on.
    seq += 1;
    state.fold(ServerFrame::Event(env(
        seq,
        SessionEvent::Subagent {
            subagent_id: "s-sub-2".into(),
            state: "failed".into(),
            prompt: "You are the gatekeeper.".into(),
            role: "gatekeeper".into(),
            task: task.into(),
            model: String::new(),
            answer: None,
            slug: String::new(),
        },
    )));
    let last = state.events.page(0, 50).entries.pop().expect("an ending");
    assert_eq!(last.kind, "subagent");
    assert_eq!(last.state, "failed");
    assert_eq!(
        last.name, "s-sub-2",
        "a child with no slug is named by its id"
    );
    assert_eq!(last.text, "agent/P1 — You are the gatekeeper.");
}

/// A reviewer that is already running when the head attaches is announced from the
/// snapshot, and a resync completes the ones that ended while it was demoted. Attaching
/// mid-review is the case: the snapshot's `subagents` is the only place they exist.
#[test]
fn a_child_already_running_is_read_from_the_snapshot() {
    let hub = letibot_sessionlog::hub::Hub::new("s-gatekeeper");
    let published = |id: &str, state: &str, answer: Option<&str>| SessionEvent::Subagent {
        subagent_id: id.into(),
        state: state.into(),
        prompt: "You are the gatekeeper.".into(),
        role: "gatekeeper".into(),
        task: "The placement:\n- branch: agent/B6\n".into(),
        model: String::new(),
        answer: answer.map(str::to_string),
        slug: id.into(),
    };
    // One reviewer in flight and one that finished before this head existed.
    hub.publish(published("sub-running", "running", None));
    hub.publish(published("sub-old", "done", Some("accepted")));
    let snap = hub
        .attach("remote", "probe", Caps::default(), 0)
        .snapshot
        .expect("a snapshot");

    let mut state = State::default();
    state.seed(&snap);
    let said: Vec<(String, String)> = state
        .events
        .page(0, 50)
        .entries
        .iter()
        .map(|e| (e.name.clone(), e.text.clone()))
        .collect();
    assert_eq!(
        said,
        vec![("sub-running".to_string(), "agent/B6".to_string())],
        "the child still running is news; the one that ended before the attach is history"
    );

    // The reviewer finishes while this head is demoted (a `Resync` carries a fresh
    // snapshot): the ending must not be lost just because the event was never delivered.
    hub.publish(published("sub-running", "done", Some("rejected: no test")));
    let again = hub
        .attach("remote", "probe", Caps::default(), 0)
        .snapshot
        .expect("a snapshot");
    state.seed(&again);
    let entries = state.events.page(0, 50).entries;
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(entries[1].state, "ended");
    assert_eq!(entries[1].text, "agent/B6 — rejected: no test");
    // And nothing is announced twice: the child is off the running list now.
    state.seed(&again);
    assert_eq!(state.events.page(0, 50).entries.len(), 2);
}

/// A line is clamped, counted in characters, and the ellipsis is inside the budget:
/// a cap of 120 that yields 121 characters is not a cap.
#[test]
fn a_long_line_is_cut_within_its_budget() {
    let long = "cargo test --workspace --all-targets ".repeat(20);
    let line = one_line(&long, LINE);
    assert_eq!(line.chars().count(), LINE);
    assert!(line.ends_with('…'), "{line}");
    let text = "first line\nsecond line";
    assert_eq!(one_line(text, LINE), "first line");
    assert_eq!(one_line("\n\n  padded  \nmore", LINE), "padded");
    // Fewer characters than the cap stay untouched, and a wide character counts once.
    assert_eq!(one_line("日本語", 5), "日本語");
}

/// A cursor older than the ring says what it dropped, and the cursors go on past it:
/// a ring that reused a cursor would let a caller ask for what it already has and never
/// learn that it missed anything.
#[test]
fn a_cursor_older_than_the_ring_reports_what_it_dropped() {
    let mut ring = Ring::default();
    for i in 0..(RING + 10) {
        ring.push(Entry::of("tool", "proposed").named(format!("call-{i}")));
    }
    let page = ring.page(0, RING);
    assert_eq!(page.entries.len(), RING);
    assert_eq!(page.entries[0].cursor, 11, "the first ten fell off");
    assert_eq!(page.entries[0].name, "call-10");
    assert_eq!(page.next, RING as u64 + 10);
    assert_eq!(page.dropped, 10, "ten entries are gone from this view");
    // A caller that is inside the ring has lost nothing.
    assert_eq!(ring.page(20, RING).dropped, 0);
    assert_eq!(ring.page(RING as u64 + 10, RING).dropped, 0);
    // And nothing to say: the cursor comes back unchanged, so a poll loop keeps its
    // place rather than resetting to zero.
    let none = ring.page(RING as u64 + 10, RING);
    assert!(none.entries.is_empty(), "{none:?}");
    assert_eq!(none.next, RING as u64 + 10);
}

/// A limit pages, and `next` is where the caller got to - not the newest cursor, or
/// a caller that took the first fifty of two hundred would be told there was nothing
/// else.
#[test]
fn a_limit_pages_from_the_cursor_it_was_given() {
    let mut ring = Ring::default();
    for i in 0..10 {
        ring.push(Entry::of("turn", "started").named(format!("t{i}")));
    }
    let first = ring.page(0, 3);
    assert_eq!(first.entries.len(), 3);
    assert_eq!(first.next, 3);
    let second = ring.page(first.next, 3);
    assert_eq!(
        second.entries.iter().map(|e| e.cursor).collect::<Vec<_>>(),
        vec![4, 5, 6]
    );
    assert_eq!(second.next, 6);
    // The default page is fifty entries; a limit of zero is one entry rather than none,
    // because an answer that cannot advance a cursor is a caller that loops for ever.
    assert_eq!(ring.page(0, 0).entries.len(), 1);
}

/// A socket that ends with no `Bye` is the end of the session too: the head is over with its
/// own reason, and a `wait` on it answers at once instead of reading the quiet as a turn end.
#[test]
fn eof_without_bye_marks_the_head_over() {
    use std::io::{BufRead, BufReader, Write};
    let socket = letibot_sessionlog::testing::scratch_root()
        .join(format!("bridge-eof-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    let (hang_up, hung_up) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut lines = BufReader::new(stream.try_clone().expect("clone")).lines();
        let _attach = lines.next();
        let hello = ServerFrame::Hello {
            protocol_version: letibot_sessionlog::protocol::PROTOCOL_VERSION,
            session_id: "s".into(),
            head_id: "h1".into(),
            dropped: 0,
            snapshot: Some(Box::new(
                letibot_sessionlog::view::SessionView::new(
                    "s",
                    letibot_sessionlog::view::ViewBounds::default(),
                )
                .snapshot(0, 0),
            )),
            resumed_from: None,
            scrubbed: letibot_sessionlog::ScrubReport::default(),
            wiring: letibot_sessionlog::SessionWiring::default(),
            sessions: Vec::new(),
        };
        let _ = writeln!(stream, "{}", serde_json::to_string(&hello).unwrap());
        let _ = hung_up.recv();
        let _ = stream.shutdown(std::net::Shutdown::Both);
    });
    let d = crate::daemon::Daemon {
        workspace: socket.clone(),
        key: "k".into(),
        socket: socket.clone(),
        log: Default::default(),
        store: Default::default(),
        placement: None,
        started: false,
    };
    let h = Head::attach(&d, "s").expect("attach");
    assert!(!h.is_over(), "a live socket is not over");
    hang_up.send(()).expect("the fake daemon is up");
    let end = Instant::now() + Duration::from_secs(10);
    let answer = h.wait(end);
    assert!(
        matches!(&answer, Attention::Failed { why } if why.contains(WENT_AWAY)),
        "{answer:?}"
    );
    assert!(Instant::now() < end, "the wait sat out its deadline");
    assert!(h.is_over());
    assert_eq!(h.snapshot().closed.as_deref(), Some(WENT_AWAY));
    let _ = std::fs::remove_file(&socket);
}
