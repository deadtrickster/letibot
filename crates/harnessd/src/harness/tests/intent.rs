//! The intent ledger, the trail, and what a turn boundary says.

use super::*;

/// A `User` row **this session appended** — a job settlement, a salvage notice,
/// an intent check. The same variant on the wire and never the operator's words.
fn agent_row(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: letibot_transcript::Speaker::Agent,
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

/// A ledger with an encoder attached, which is what every session has.
///
/// Constructing the decorator is the *only* way to say so — the flag is
/// `pub(crate)` in `letibot-tools` precisely so that "an encoder was declared"
/// and "an encoder was wired" cannot be two different facts. The sink is
/// dropped; the attachment is not.
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

/// **A child's answer is settled once, and a path out with no answer still settles.**
///
/// Two failures this guard exists for, and both are silent without it: a settlement that
/// never fires leaves the parent's `task_result` waiting for ever, and a second one lets a
/// path out of the call overwrite the answer the child actually gave.
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

    // **A path out with no answer still settles** — the one that would hang a parent: a
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

    // **And the first answer is the answer.**
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

/// **The turn boundary carries the intent finding and NOTHING about the plan.**
///
/// This test used to assert the opposite — that the plan's finding was joined in at the turn
/// boundary because *"joining is what keeps this inside the one-nudge budget"*. That budget was
/// the thing the operator rejected: one nudge per user turn means a session in constant
/// conversation is checked after every exchange (*"looks like our todo nag is overkill"*) while
/// a quiet one is checked once and never again. The plan is now delivered by the IDLE clock
/// (`Sessions::rearm_todo_nag`, `Harness::nag_turn`), so what is left here is the finding that
/// really is about the turn that just ended.
///
/// The plan half has its own tests where it now lives: `unfinished_plan` in `letibot-tools` for
/// the text, and `nag_should_arm` below for the schedule.
#[test]
fn the_turn_boundary_says_what_the_turn_promised_and_not_what_the_plan_is() {
    // A session with nothing declared has nothing to say at the boundary — the common case,
    // and the one that must stay silent.
    assert!(
        steer_for_turn(&encoded(), false, "t1", &[]).is_none(),
        "an encoded ledger with nothing declared has no boundary finding"
    );
    // And the finding that IS the boundary's own: a turn that declared something and did
    // nothing to show for it. It carries no plan text at all, which is the whole claim of
    // this test — the two checks are no longer one message.
    let bare = IntentLedger::new();
    let steer = steer_for_turn(&bare, false, "t1", &[])
        .expect("no encoder is itself a finding, on every turn");
    assert!(!steer.contains("[todo check]"), "{steer}");
}

/// **An unattached encoder is loud, not silent.** The state this is asserting
/// is the one a session must never be in quietly: a diff computed against
/// nothing would report every completion as verified, which is exactly the
/// announced-but-not-done failure wearing the check's own clothes.
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

/// **The harness must not be able to authorise itself.**
///
/// Everything a session appends as a `User` item looks identical in the
/// transcript: the operator's prompt, a head's steering, §5.7's salvage notice
/// and the intent check's own *"you said you would X"*. A trail walked over the
/// transcript would read all four as the operator's words, and an agent whose
/// own text can authorise an action is the shape a prompt injection would most
/// like to take. This is the assertion that keeps the four apart.
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

/// The denominator is what was **looked at**, and it is not derived from what
/// came back.
///
/// `docs/tool-design-brief.md` §2.2 applied to a security input: an adjudicator
/// told *"the operator said nothing"* when nobody looked would deny the thing
/// that was asked for. `0 of 41` is a measurement; `0` is not.
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

/// A gate with no trail source installed reports `NotCollected` — **not** an
/// empty trail. This is the state the whole seam exists to leave behind, and it
/// is asserted here so that "the default is safe" is a fact rather than a
/// recollection.
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
    // `None` reads as *not recorded*, never as *just now* — which is why the
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

/// **A resumed session does not promote its own words to the operator's.**
///
/// The regression this closes was live and measurable: `seed` destructured
/// `TranscriptItem::User { parts, .. }` and stamped `Speaker::Operator` on every
/// row, so a job settlement came back as an operator utterance and
/// [`AuthorisationTrail::cited_operator_words`] — which exists precisely to refuse
/// an ALLOW citing agent text — had nothing left to refuse. 280 of 715 briefs the
/// oracle answered carried one.
///
/// Both halves are asserted, because only one of them is the bug: the agent row is
/// still **carried** (it is context, and `Speaker::Agent` says what it is), and it
/// is no longer **counted** as something that could authorise anything.
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

/// **T21.3, closed.** An item declared in a turn that then ran nothing is the
/// acceptance case, and the steering text is what the model is handed.
#[test]
fn an_intent_declared_and_not_acted_on_produces_steering() {
    let l = encoded();
    l.declare("t1", "restart the model server", intent_tools::Source::Todo);
    let steer = steer_for_turn(&l, false, "t1", &[]).expect("a declared item and no effect");
    assert!(steer.contains("intent check"), "{steer}");
    assert!(steer.contains("restart the model server"), "{steer}");
}

/// **The default does not fire on prose**, which is the whole reason the prose
/// half is a flag. A turn that says "I'll check the file" and calls nothing is
/// the *normal* shape of a plain answer, and nudging it would put the check into
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
    let with_prose =
        steer_for_turn(&l, true, "t2", &items).expect("the prose half is what --intent-prose buys");
    assert!(with_prose.contains("check the file"), "{with_prose}");
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

/// A monitor that expired is **not** a monitor that fired, and the sentence the
/// model gets has to keep them apart — reporting a deadline as a completion is
/// F5 one primitive over. And (R93.C8) the model's own `retire` and an owner
/// ending are not announced as anything.
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
    assert_eq!(released.as_deref(), Some("[monitor] `flowy` was retired: the seat was released."));
}
