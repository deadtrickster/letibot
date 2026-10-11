//! The delivery decision, asserted without a session: [`decide`] over [`Facts`], [`facts_of`]
//! over a board, and the record [`DeliveryLog`] writes.

use super::*;

use letibot_tokencore::store::TodoBy;

fn done(job: &str, kind: BackgroundKind, state: &str) -> JobCompletion {
    JobCompletion {
        memory: None,
        kind,
        owner: "s-me".into(),
        job: job.into(),
        command: if kind == BackgroundKind::Job {
            "cargo build".into()
        } else {
            String::new()
        },
        slug: None,
        state: state.into(),
        produced: 0,
        elapsed_ms: 0,
        detail: String::new(),
        arrived: SystemTime::UNIX_EPOCH,
    }
}

fn row(content: &str, when: Option<TodoCondition>, needs: Vec<TodoNeed>) -> TodoItem {
    TodoItem {
        content: content.into(),
        status: TodoStatus::InProgress,
        by: TodoBy::Model,
        when,
        needs,
    }
}

/// **The operator's table, row by row.** A routine result is held; everything the work waits on,
/// a monitor and an emergency are immediate; a failure is immediate only when something depends
/// on it.
#[test]
fn the_decision_holds_routine_results_and_delivers_what_the_work_waits_on() {
    let routine = Facts::routine(ResultKind::Job);
    assert_eq!(decide(&routine), Delivery::Held);
    assert_eq!(
        decide(&Facts::routine(ResultKind::Child)),
        Delivery::Held,
        "a child nobody waits on is held like a job"
    );
    assert_eq!(
        decide(&Facts {
            failed: true,
            ..routine
        }),
        Delivery::Held,
        "a failure NOTHING depends on is not an emergency"
    );
    assert_eq!(
        decide(&Facts::routine(ResultKind::Monitor)),
        Delivery::Immediate(Why::Monitor)
    );
    assert_eq!(
        decide(&Facts {
            emergency: true,
            ..routine
        }),
        Delivery::Immediate(Why::Emergency)
    );
    assert_eq!(
        decide(&Facts {
            when_row: true,
            ..routine
        }),
        Delivery::Immediate(Why::WhenJob)
    );
    assert_eq!(
        decide(&Facts {
            needed: true,
            ..routine
        }),
        Delivery::Immediate(Why::Needed)
    );
    assert_eq!(
        decide(&Facts {
            wanted: Some(WantedNow::Awaited),
            ..routine
        }),
        Delivery::Immediate(Why::Awaited)
    );
    assert_eq!(
        decide(&Facts {
            wanted: Some(WantedNow::WakeNow),
            ..routine
        }),
        Delivery::Immediate(Why::WakeNow)
    );
    for depends in [
        Facts {
            when_row: true,
            failed: true,
            ..routine
        },
        Facts {
            needed: true,
            failed: true,
            ..routine
        },
        Facts {
            wanted: Some(WantedNow::Awaited),
            failed: true,
            ..routine
        },
    ] {
        assert_eq!(
            decide(&depends),
            Delivery::Immediate(Why::DependencyFailed),
            "{depends:?}"
        );
    }
    assert_eq!(
        decide(&Facts {
            wanted: Some(WantedNow::WakeNow),
            failed: true,
            ..routine
        }),
        Delivery::Immediate(Why::WakeNow),
        "`wake: now` is timing, not a dependency"
    );
}

/// **The facts are read off the board as it is**: an open row's `when` and `needs` name a job by
/// its id or its name and a child by its handle; a finished row depends on nothing.
#[test]
fn the_facts_are_the_open_rows_that_name_the_handle() {
    let mut job = done("j12", BackgroundKind::Job, "exited 0");
    job.slug = Some("release-build".into());
    let child = done("s-1-sub-1", BackgroundKind::Subagent, "failed");

    let rows = vec![
        row(
            "ship it",
            None,
            vec![TodoNeed::Job {
                handle: "release-build".into(),
            }],
        ),
        row(
            "read the survey",
            None,
            vec![TodoNeed::Child {
                id: "s-1-sub-1".into(),
            }],
        ),
    ];
    let f = facts_of(&job, &rows, None);
    assert!(f.needed && !f.when_row && !f.failed, "{f:?}");
    let f = facts_of(&child, &rows, None);
    assert!(f.needed && f.failed, "{f:?}");
    assert_eq!(decide(&f), Delivery::Immediate(Why::DependencyFailed));

    let when = vec![row(
        "after the build",
        Some(TodoCondition::Job {
            handle: "j12".into(),
        }),
        Vec::new(),
    )];
    assert!(facts_of(&job, &when, None).when_row);

    let finished = vec![TodoItem {
        status: TodoStatus::Completed,
        ..rows[0].clone()
    }];
    assert_eq!(
        decide(&facts_of(&job, &finished, None)),
        Delivery::Held,
        "a finished row waits on nothing"
    );
    assert!(failed(&done(
        "j1",
        BackgroundKind::Job,
        "killed by job_kill"
    )));
    assert!(!failed(&done("j1", BackgroundKind::Job, "exited 0")));
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "{tag}-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// **A monitor firing is immediate and recorded as one**, with when it fired and when it was
/// told.
#[test]
fn a_monitor_firing_is_recorded_as_immediate() {
    let dir = scratch("delivery-monitor");
    let log = DeliveryLog::under(&dir, "s-me");
    let fired_at = UNIX_EPOCH + Duration::from_secs(1_000);
    let told = UNIX_EPOCH + Duration::from_secs(1_002);
    log.firings(
        &[letibot_tools::exec::monitor::Firing {
            name: "port-open".into(),
            watch: "port 8080".into(),
            declared_by: "s-me".into(),
            fired: letibot_tools::exec::monitor::Fired::Fired {
                why: "it answered".into(),
                at: fired_at,
                after: Duration::from_secs(2),
            },
            at: fired_at,
        }],
        told,
    );
    let text = std::fs::read_to_string(dir.join(LOG)).unwrap();
    let v: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(v["kind"], "monitor");
    assert_eq!(v["rule"], "immediate:monitor");
    assert_eq!(v["arrived_at_ms"], 1_000_000);
    assert_eq!(v["delivered_at_ms"], 1_002_000);
    assert_eq!(v["waited_ms"], 2_000);
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The turn-end heading says what is true, and the rules are told apart in the record.**
///
/// pg-noop, 2026-10-11: *"[meanwhile] 1 result arrived while you were busy"* reached a model that
/// had been idle for an hour, the moment the operator spoke. The operator's ruling: *"jobs were
/// ran for a reason and held for a reason. once turn ends no reason to hold"* — so a batch is
/// delivered the moment the turn it was held through ends, and its heading says the model was
/// WORKING and the results were held until its turn ended. The batch and starvation wording is
/// unchanged; a result that arrives while idle has no heading at all (asserted on the session).
#[test]
fn the_turn_end_heading_and_the_rule_words() {
    assert_eq!(
        meanwhile_heading(1, Rule::TurnEnd, HELD_RESULTS_CAP),
        "[meanwhile] 1 result arrived while you were working — nothing in your plan was waiting \
         on it, so it was held until your turn ended instead of interrupting you."
    );
    assert_eq!(
        meanwhile_heading(3, Rule::TurnEnd, HELD_RESULTS_CAP),
        "[meanwhile] 3 results arrived while you were working — nothing in your plan was waiting \
         on them, so they were held until your turn ended instead of interrupting you."
    );
    assert!(
        meanwhile_heading(2, Rule::Batch, HELD_RESULTS_CAP)
            .starts_with("[meanwhile] 2 results arrived while you were busy — ")
    );
    assert!(
        meanwhile_heading(2, Rule::Starvation, Duration::from_secs(600))
            .starts_with("[meanwhile] 2 results held for over 10 min while this turn ran")
    );
    assert_eq!(Rule::TurnEnd.word(), "turn_end");
    assert_eq!(Rule::Idle.word(), "idle");
    assert_eq!(Rule::Batch.word(), "batch");
    assert_eq!(Rule::Starvation.word(), "starvation");
}
