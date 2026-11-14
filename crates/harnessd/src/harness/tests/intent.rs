//! The intent ledger, the trail, and what a turn boundary says.

use super::*;

/// A `User` row this session appended - a job settlement, a salvage notice,
/// an intent check. The same variant on the wire and never the operator's words.
fn agent_row(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: letibot_transcript::Speaker::Agent,
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

/// A ledger with an encoder attached, which is what every session has. Constructing the decorator is the only way to
/// say so, since the flag is `pub(crate)` precisely so that an encoder declared and an encoder wired cannot be two
/// different facts.
fn encoded() -> IntentLedger {
    let l = IntentLedger::new();
    let l = Arc::new(l);
    drop(IntentSink::new(l.clone(), letibot_tools::NullToolSink));
    Arc::try_unwrap(l).expect("the sink was just dropped")
}

#[test]
fn a_downgrade_denies_the_seated_tools_of_its_classes_and_the_well_known_names() {
    use letibot_tools::permission::{Action, Rule};
    use letibot_tools::schema::{Access, Downgrade, ToolSchema};
    let seated = vec![
        ToolSchema::new("read", "r", serde_json::json!({}), Access::Read),
        ToolSchema::new("edit", "e", serde_json::json!({}), Access::Write),
        ToolSchema::new("bash", "b", serde_json::json!({}), Access::Exec),
        ToolSchema::new("flowy", "f", serde_json::json!({}), Access::Network),
        ToolSchema::new("odd_writer", "o", serde_json::json!({}), Access::Write),
    ];
    let parent = vec![Rule::new("edit", "*", Action::Allow)];
    let rules = downgraded_ruleset(&parent, &Downgrade::parse("no-write").unwrap(), &seated);
    let denied: Vec<&str> = rules
        .iter()
        .filter(|r| r.action == Action::Deny)
        .map(|r| r.permission.as_str())
        .collect();
    assert!(
        denied.contains(&"edit") && denied.contains(&"write") && denied.contains(&"odd_writer"),
        "{denied:?}"
    );
    // `notes` denies with the write class: a standing note outlives the
    // subagent that wrote it, so it is not a capability a `no-write`
    // child keeps.
    assert!(denied.contains(&"notes"), "{denied:?}");
    assert!(
        !denied.contains(&"read") && !denied.contains(&"bash") && !denied.contains(&"flowy"),
        "{denied:?}"
    );
    // Last rule wins: the parent's allow for `edit` is overridden.
    let r = letibot_tools::permission::evaluate("edit", "anything", &[&rules]);
    assert_eq!(r.action, Action::Deny);
    // No downgrade, no change.
    assert_eq!(
        downgraded_ruleset(&parent, &Downgrade::none(), &seated),
        parent
    );
}

/// A child's answer is settled once, and a path out with no answer still settles: a settlement that never fires leaves
/// the parent's `task_result` waiting for ever, and a second lets a path out overwrite the answer the child gave.
#[test]
fn a_childs_answer_is_settled_once_and_never_overwritten() {
    use letibot_tools::builtins::task::TaskStatus;

    // The ordinary ending.
    let slot = TaskSlot::new("s-parent");
    {
        let answer = AnswerOnce::new(&slot);
        answer.say(TaskStatus::Done {
            answer: "the child's answer".into(),
            child: None,
        });
    }
    assert_eq!(
        slot.status(),
        TaskStatus::Done {
            answer: "the child's answer".into(),
            child: None,
        }
    );

    // A path out with no answer still settles - the one that would hang a parent: a
    // thread ending with the slot still `Running` is a `task_result` that never returns.
    let slot = TaskSlot::new("s-parent");
    {
        let _answer = AnswerOnce::new(&slot);
    }
    assert!(
        matches!(slot.status(), TaskStatus::Failed { .. }),
        "an unsettled child stayed `running` for ever: {:?}",
        slot.status()
    );

    // And the first answer is the answer.
    let slot = TaskSlot::new("s-parent");
    {
        let answer = AnswerOnce::new(&slot);
        answer.say(TaskStatus::Done {
            answer: "said".into(),
            child: None,
        });
        answer.say(TaskStatus::Failed {
            why: "the park ended oddly".into(),
        });
    }
    assert_eq!(
        slot.status(),
        TaskStatus::Done {
            answer: "said".into(),
            child: None,
        },
        "a later path out overwrote what the child said"
    );
}

/// The turn boundary carries the intent finding and NOTHING about the plan. The old assertion joined the plan here to
/// keep the one-nudge budget the operator rejected; the plan is now the idle clock's, and its tests live there.
#[test]
fn the_turn_boundary_says_what_the_turn_promised_and_not_what_the_plan_is() {
    // A session with nothing declared has nothing to say at the boundary - the common case,
    // and the one that must stay silent.
    assert!(
        steer_for_turn(&encoded(), false, "t1", &[]).is_none(),
        "an encoded ledger with nothing declared has no boundary finding"
    );
    // And the finding that IS the boundary's own: a turn that declared something and did
    // nothing to show for it. It carries no plan text at all, which is the whole claim of
    // this test - the two checks are no longer one message.
    let bare = IntentLedger::new();
    let steer = steer_for_turn(&bare, false, "t1", &[])
        .expect("no encoder is itself a finding, on every turn");
    assert!(!steer.contains("[todo check]"), "{steer}");
}

/// An unattached encoder is loud, not silent: a diff computed against nothing would report every completion as
/// verified, which is the announced-but-not-done failure wearing the check's own clothes.
#[test]
fn a_ledger_with_no_encoder_says_so_in_the_diff_itself() {
    let bare = IntentLedger::new();
    let steer = steer_for_turn(&bare, false, "t1", &[])
        .expect("no encoder is itself a finding, on every turn");
    assert!(steer.contains("effect log"), "{steer}");
    assert!(
        steer_for_turn(&encoded(), false, "t1", &[]).is_none(),
        "an encoded ledger with nothing declared has nothing to say"
    );
}

// -- the authorisation trail --------------------------------------------

/// The harness must not be able to authorise itself: everything a session appends as a `User` item looks identical in
/// the transcript, so a trail walked over it would read the operator's prompt, a head's steering, a salvage notice and
/// an intent nudge as the operator's words.
#[test]
fn an_injected_notice_is_never_read_back_as_the_operator() {
    let m = TrailMirror::default();
    m.begin_turn();
    m.say(
        Speaker::Operator,
        "ssh to lubuntu2 and check the build",
        None,
    );
    m.say(
        Speaker::Agent,
        "[intent check] you said you would ssh",
        None,
    );

    let t = m.trail();
    let words: Vec<&str> = t.operator_words().iter().map(|u| u.text.as_str()).collect();
    assert_eq!(words, vec!["ssh to lubuntu2 and check the build"]);
    assert_eq!(
        t.utterances.len(),
        2,
        "both are carried, with their speakers"
    );
    assert!(
        t.utterances.iter().any(|u| u.speaker == Speaker::Agent),
        "the agent's own line is carried as context and never as authority"
    );
}

/// The denominator is what was LOOKED AT, and it is not derived from what came back: an adjudicator told the operator
/// said nothing when nobody looked would deny the thing that was asked for. `0 of 41` is a measurement and `0` is not.
#[test]
fn a_trail_with_no_operator_words_still_carries_its_denominator() {
    let m = TrailMirror::default();
    m.note_items(41);
    let t = m.trail();
    assert!(
        t.was_collected(),
        "somebody looked; this is not `NotCollected`"
    );
    assert!(t.operator_words().is_empty());
    match t.provenance {
        TrailProvenance::Scanned {
            messages_scanned,
            operator_messages,
        } => {
            assert_eq!(operator_messages, 0);
            assert!(
                messages_scanned > 0,
                "a scan of 41 items reported a denominator of {messages_scanned}"
            );
        }
        TrailProvenance::NotCollected { .. } => panic!("something looked"),
    }
}

/// A gate with no trail source installed reports `NotCollected`, not an empty trail: this is the state the whole seam
/// exists to leave behind, asserted so the safe default is a fact rather than a recollection.
#[test]
fn an_uninstalled_trail_is_not_collected_rather_than_empty() {
    let t = AuthorisationTrail::default();
    assert!(!t.was_collected());
    assert!(t.render().contains("NOT COLLECTED"), "{}", t.render());
}

/// Distance is half the evidence: a *"yeah restart"* from six turns ago is not
/// the same fact as one from this turn, and a trail that dropped the distance
/// would present them as identical.
#[test]
fn recency_is_carried_and_the_clock_is_absent_rather_than_invented() {
    let m = TrailMirror::default();
    m.begin_turn();
    m.say(Speaker::Operator, "old", None);
    for _ in 0..5 {
        m.begin_turn();
    }
    m.say(Speaker::Operator, "recent", Some(Instant::now()));

    let t = m.trail();
    let by = |s: &str| {
        t.utterances
            .iter()
            .find(|u| u.text == s)
            .unwrap_or_else(|| panic!("{s} is missing"))
    };
    assert_eq!(by("recent").turns_ago, 0);
    assert_eq!(by("old").turns_ago, 5);
    // `None` reads as *not recorded*, never as *just now* - which is why the
    // rebuilt-from-store rows keep it.
    assert_eq!(by("old").seconds_ago, None);
    assert!(by("recent").seconds_ago.is_some());
}

/// A rebuilt transcript seeds the trail, and the clock does not come back with it.
/// Asserted so that the resume note beside it is describing something true.
#[test]
fn a_resumed_trail_is_seeded_with_no_clock() {
    let m = TrailMirror::default();
    m.seed(&[
        user("first"),
        TranscriptItem::Assistant {
            text: "an answer".into(),
            tool_calls: vec![],
            truncated: false,
        },
        user("second"),
    ]);
    let t = m.trail();
    assert_eq!(t.operator_words().len(), 2);
    assert!(
        t.utterances.iter().all(|u| u.seconds_ago.is_none()),
        "nothing recorded when a stored row was said; a reconstructed clock \
             would be a guess presented as a measurement"
    );
}

/// A resumed session does not promote its own words to the operator's: `seed` stamped `Speaker::Operator` on every
/// row, so a job settlement came back as an operator utterance and the cite check had nothing to refuse. Both halves
/// are asserted, since only one is the bug: the agent row is still carried and no longer counted.
#[test]
fn a_resumed_trail_does_not_call_the_harnesss_own_rows_the_operators() {
    let m = TrailMirror::default();
    m.seed(&[
        user("fix the compaction"),
        agent_row("[job] a job you backgrounded has ended:\n  - `j6` exited 0"),
        user("restarted it"),
    ]);
    let t = m.trail();

    assert_eq!(
        t.utterances.len(),
        3,
        "the agent row is carried — it is context the operator's reply may lean on"
    );
    assert_eq!(
        t.operator_words().len(),
        2,
        "but it is not one of the operator's words"
    );

    // The citation check is the thing that was defeated, so assert it directly
    // rather than trusting the count above to stand in for it.
    let job = t
        .utterances
        .iter()
        .position(|u| u.text.starts_with("[job]"))
        .expect("the job row is on the page");
    assert_eq!(
        t.cited_operator_words(&[job]),
        Vec::<usize>::new(),
        "an ALLOW citing the harness's own job notice authorises nothing"
    );
    assert_eq!(
        t.cited_operator_words(&[0, job, 2]),
        vec![0, 2],
        "the operator's own lines still survive beside it"
    );
}

// -- the intent diff -----------------------------------------------------

/// closed. An item declared in a turn that then ran nothing is the
/// acceptance case, and the steering text is what the model is handed.
#[test]
fn an_intent_declared_and_not_acted_on_produces_steering() {
    let l = encoded();
    l.declare("t1", "restart the model server", intent_tools::Source::Todo);
    let steer = steer_for_turn(&l, false, "t1", &[]).expect("a declared item and no effect");
    assert!(steer.contains("intent check"), "{steer}");
    assert!(steer.contains("restart the model server"), "{steer}");
}

/// The default does not fire on prose, which is the whole reason the prose half is a flag: a turn that says it will
/// check the file and calls nothing is the normal shape of a plain answer, and nudging it would put the check into
/// conversations that were working.
#[test]
fn the_prose_heuristic_is_off_unless_it_is_asked_for() {
    let l = encoded();
    let items = [TranscriptItem::Assistant {
        text: "I'll check the file and get back to you.".into(),
        tool_calls: vec![],
        truncated: false,
    }];
    assert_eq!(
        steer_for_turn(&l, false, "t1", &items),
        None,
        "the deterministic half must not read prose"
    );
    let with_prose = steer_for_turn(&l, true, "t2", &items)
        .expect("the prose half is what the caller's `prose` bool buys");
    assert!(with_prose.contains("check the file"), "{with_prose}");
}

/// A turn that read files and then ended on a text-only round that promised the fix is told,
/// quoting the sentence. The same words in a round that also called a tool are not.
#[test]
fn a_promise_in_the_last_text_only_round_is_nudged_after_tools_ran() {
    let l = encoded();
    l.record_effect("t1", "c1", "read", &letibot_transcript::ToolOutcome::Ok);
    let said = "Found the actual hang. Fixing it properly: the ptys are drained on Editor::tick.";
    let last = [TranscriptItem::Assistant {
        text: said.into(),
        tool_calls: vec![],
        truncated: false,
    }];
    let steer = steer_for_turn(&l, true, "t1", &last).expect("the last round promised and stopped");
    assert!(steer.contains("\"Fixing it properly: the ptys"), "{steer}");
    assert!(steer.contains("do it, or say why not"), "{steer}");
    assert_eq!(
        steer_for_turn(&l, false, "t1", &last),
        None,
        "off for this model"
    );

    let working = [TranscriptItem::Assistant {
        text: said.into(),
        tool_calls: vec![letibot_transcript::ToolCall {
            id: "c2".into(),
            name: "edit".into(),
            arguments: "{}".into(),
        }],
        truncated: false,
    }];
    assert_eq!(
        steer_for_turn(&l, true, "t1", &working),
        None,
        "a round that did the work is not nudged"
    );
}

/// The diff is idempotent per turn. A turn boundary that fires twice is a thing
/// that happens, and a diff that grows each time it is read is not a
/// measurement.
#[test]
fn reading_the_same_turn_twice_says_the_same_thing() {
    let l = encoded();
    l.declare("t1", "write the report", intent_tools::Source::Todo);
    let a = steer_for_turn(&l, false, "t1", &[]);
    let b = steer_for_turn(&l, false, "t1", &[]);
    assert_eq!(a, b);
}

// -- the monitor notice --------------------------------------------------

/// A monitor that expired is not a monitor that fired, and the sentence the model gets has to keep them apart, since
/// reporting a deadline as a completion is one primitive over. The model's own `retire` and an owner ending are not
/// announced as anything.
#[test]
fn a_monitor_notice_says_which_of_the_four_endings_it_was() {
    use letibot_tools::exec::monitor::{Fired, Firing};
    use std::time::{Duration, SystemTime};
    let firing = |name: &str, fired: Fired| Firing {
        name: name.into(),
        watch: "job j1".into(),
        declared_by: "t-1".into(),
        fired,
        at: SystemTime::UNIX_EPOCH,
    };
    let after = Duration::from_secs(3);
    assert_eq!(monitor_notice(&[]), None);
    let text = monitor_notice(&[
        firing(
            "build",
            Fired::Fired {
                why: "j1 exited 0".into(),
                at: SystemTime::UNIX_EPOCH,
                after,
            },
        ),
        firing(
            "log",
            Fired::Expired {
                after,
                last_seen: "no match".into(),
            },
        ),
        firing(
            "mine",
            Fired::Cancelled {
                by: "the model".into(),
                after,
            },
        ),
    ])
    .expect("a firing and an expiry are news");
    assert_eq!(
        text,
        "[monitor] `build` fired: j1 exited 0.\n[monitor] `log` expired without firing."
    );
    assert!(!text.contains("t-1"), "no turn id: {text}");
    let own = monitor_notice(&[firing(
        "mine",
        Fired::Cancelled {
            by: "the model".into(),
            after,
        },
    )]);
    assert_eq!(own, None, "the model's own retire is not news to it");
    let released = monitor_notice(&[firing(
        "flowy",
        Fired::Cancelled {
            by: "the seat was released".into(),
            after,
        },
    )]);
    assert_eq!(
        released.as_deref(),
        Some("[monitor] `flowy` was retired: the seat was released.")
    );
}

/// The trail holds only what it reads: `trail()` takes the last `TRAIL_SCAN` and clips each to `UTTERANCE_CHARS`, while
/// the buffer kept every user-side text for the session's life. This asserts on the BUFFER, since nothing observable
/// through `trail()` changes, and pins what a clip must never lose.
#[test]
fn the_trail_holds_only_what_it_reads() {
    let m = TrailMirror::default();
    for i in 0..TRAIL_SCAN + 5 {
        m.say(Speaker::Operator, &format!("utterance {i}"), None);
    }
    let (bytes, held) = m.held();
    assert_eq!(held, TRAIL_SCAN, "the trail kept more than it can read");
    let expected: usize = (5..TRAIL_SCAN + 5)
        .map(|i| format!("utterance {i}").len())
        .sum();
    assert_eq!(
        bytes, expected,
        "it must keep the LAST ones — those are the ones `trail()` reads"
    );

    let long = "x".repeat(UTTERANCE_CHARS + 400);
    m.say(Speaker::Operator, &long, None);
    let (after, n) = m.held();
    assert_eq!(n, TRAIL_SCAN, "one more utterance does not grow the buffer");
    assert_eq!(
        after,
        bytes - "utterance 5".len() + UTTERANCE_CHARS,
        "the oldest went and the new one was stored clipped"
    );
    let t = m.trail();
    // The walk is backwards, so the NEWEST utterance is the first one in the list.
    let newest = t.utterances.first().expect("the newest utterance");
    assert!(
        newest.clipped,
        "a clipped authorisation must say it was clipped"
    );
    assert_eq!(newest.text.chars().count(), UTTERANCE_CHARS);
}
