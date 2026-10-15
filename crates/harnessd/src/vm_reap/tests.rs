use super::*;

const NOW: u64 = 1_792_089_000_000;
const MIN: u64 = 60_000;

fn run(child: &str, parent: &str, state: &str, instance: &str) -> SubagentRun {
    SubagentRun {
        child_id: child.into(),
        parent_id: parent.into(),
        state: state.into(),
        prompt: "do the thing".into(),
        spec_json: r#"{"role":"coder","placement":"firecode"}"#.into(),
        instance: instance.into(),
        resumes: 0,
        outcome: None,
        delivered: false,
        started_ms: NOW - 60 * MIN,
        updated_ms: NOW - 60 * MIN,
    }
}

fn life(beat_ms: u64, stopped_ms: Option<u64>) -> DaemonLife {
    DaemonLife {
        instance: "d-404553-1791825413495565654-0".into(),
        started_ms: beat_ms - 1000,
        beat_ms,
        stopped_ms,
    }
}

/// **The four tap holders of 2026-10-15, judged.** Three were children of `d-404553`, whose last
/// beat was 18:21:01 UTC with no stop written: `running`, under a gone daemon — reaped. The fourth
/// had no row at all (a daemon older than the record) — named, never reaped.
#[test]
fn a_vm_whose_owners_daemon_is_gone_is_reaped_and_one_with_no_record_is_not() {
    let gone = daemon_of(
        "d-404553-1791825413495565654-0",
        "d-2586681-1792060685248002817-0",
        Some(&life(NOW - 26 * 60 * MIN, None)),
        NOW,
        |_| false,
    );
    assert_eq!(
        gone,
        Daemon::Gone {
            since_ms: Some(NOW - 26 * 60 * MIN)
        }
    );
    let held = Owner::Running {
        child: "s-1791151788256943467-sub-1791998129420905273-sub-1791998266483397871".into(),
        instance: "d-404553-1791825413495565654-0".into(),
        daemon: gone,
    };
    let Verdict::Reap(why) = judge(&held, NOW) else {
        panic!(
            "a dead daemon's running child's VM is nobody's: {:?}",
            judge(&held, NOW)
        );
    };
    assert!(
        why.contains("which is gone") && why.contains("1560 min ago"),
        "{why}"
    );

    let fourth = Owner::Unrecorded {
        child: Some(
            "s-1789919514688401228-sub-1791757785761179646-sub-1791758176532683859-sub-\
             1791758330910016363"
                .into(),
        ),
    };
    assert!(
        !judge(&fourth, NOW).reaps(),
        "no record says it is finished, so it is not this daemon's to stop"
    );
    assert!(!judge(&Owner::Unrecorded { child: None }, NOW).reaps());
}

/// **Never a VM a live daemon's running child holds — this one's or another's.**
#[test]
fn a_running_owner_in_a_live_daemon_keeps_its_vm() {
    let me = "d-1-1-0";
    let mine = daemon_of(me, me, None, NOW, |_| false);
    assert_eq!(mine, Daemon::This);
    // Beating: alive by its beat.
    let beating = daemon_of(
        "d-404553-1-0",
        me,
        Some(&life(NOW - 10_000, None)),
        NOW,
        |_| false,
    );
    assert_eq!(beating, Daemon::Live);
    // Its beat is stale, but a `harnessd` still has its pid: alive, and somebody else's to judge.
    let wedged = daemon_of(
        "d-404553-1-0",
        me,
        Some(&life(NOW - 60 * MIN, None)),
        NOW,
        |p| p == 404553,
    );
    assert_eq!(wedged, Daemon::Live);
    // An orderly stop is gone, whatever runs at that pid now.
    let stopped = daemon_of(
        "d-404553-1-0",
        me,
        Some(&life(NOW - 60 * MIN, Some(NOW - 60 * MIN))),
        NOW,
        |_| true,
    );
    assert!(matches!(stopped, Daemon::Gone { .. }));
    // No life row (a build before the record) and no daemon at its pid: gone.
    assert_eq!(
        daemon_of("d-9-1-0", me, None, NOW, |_| false),
        Daemon::Gone { since_ms: None }
    );
    for daemon in [Daemon::This, Daemon::Live] {
        let owner = Owner::Running {
            child: "s-a-sub-1".into(),
            instance: "d-404553-1-0".into(),
            daemon,
        };
        assert!(!judge(&owner, NOW).reaps(), "{owner:?}");
    }
}

/// **A finished owner's VM waits out its own `down`** — the copy-out is up to ten minutes, and a
/// reap inside that would race the delivery of the work.
#[test]
fn a_finished_owners_vm_is_reaped_after_its_own_down_had_time() {
    let fresh = Owner::Finished {
        child: "s-a-sub-1".into(),
        state: "done".into(),
        since_ms: NOW - 2 * MIN,
    };
    assert!(!judge(&fresh, NOW).reaps());
    let old = Owner::Finished {
        child: "s-a-sub-1".into(),
        state: "killed".into(),
        since_ms: NOW - FINISHED_GRACE_MS,
    };
    assert!(judge(&old, NOW).reaps());
}

/// **The owner is the child the copy was named for** — and only a copy under letibot's cache
/// names one: a reviewer's clone or a merge gate's landing project names nobody.
#[test]
fn a_vms_owner_is_the_child_its_copy_is_named_for() {
    let cache = Path::new("/home/dead/.cache/letibot/firecode");
    let vm = |project: &str| HeldVm {
        run: "firecode-20261014-191759-812644".into(),
        project: PathBuf::from(project),
        name: None,
        tap: Some("fccode1".into()),
        pids: vec![861706],
        started: None,
        cgroup: None,
    };
    assert_eq!(
        owner_of(
            &vm("/home/dead/.cache/letibot/firecode/s-1-sub-2-sub-3"),
            cache
        )
        .as_deref(),
        Some("s-1-sub-2-sub-3")
    );
    assert_eq!(
        owner_of(
            &vm("/home/dead/.local/share/letibot/landing/firecode-f7eb51674b04"),
            cache
        ),
        None
    );
}

/// **The store's half, end to end**: a gone daemon's running child is judged reapable through
/// the rows themselves, a finished one waits out its grace, and a child with no row is
/// unrecorded.
#[test]
fn an_owner_is_read_off_the_store() {
    let store = Store::open_in_memory().unwrap();
    let dead = "d-404553-1791825413495565654-0";
    let now = letibot_sessionlog::registry::now_ms();
    store.life_started(dead, now - 30 * 60 * MIN).unwrap();
    store.life_beat(dead, now - 26 * 60 * MIN).unwrap();
    store
        .put_subagent_run(&run("s-p-sub-1", "s-p", "running", dead))
        .unwrap();
    store
        .put_subagent_run(&run("s-p-sub-2", "s-p", "running", dead))
        .unwrap();
    store
        .settle_subagent_run("s-p-sub-2", "done", Some("ok"))
        .unwrap();

    let (o, _) = owner(&store, Some("s-p-sub-1".into()), "d-1-1-0", now);
    assert!(
        matches!(
            &o,
            Owner::Running {
                daemon: Daemon::Gone { .. },
                ..
            }
        ),
        "{o:?}"
    );
    assert!(judge(&o, now).reaps());
    let (o, _) = owner(&store, Some("s-p-sub-2".into()), "d-1-1-0", now);
    assert!(
        matches!(&o, Owner::Finished { state, .. } if state == "done"),
        "{o:?}"
    );
    assert!(
        !judge(&o, now).reaps(),
        "settled a moment ago: within the grace"
    );
    let (o, _) = owner(&store, Some("s-nobody".into()), "d-1-1-0", now);
    assert_eq!(
        o,
        Owner::Unrecorded {
            child: Some("s-nobody".into())
        }
    );
}

/// **A gone daemon's running child under a finished parent is settled `abandoned`** — the case
/// of 2026-10-14: parents `done`/`failed` at 17:18–17:22, their VM children `running` under
/// `d-404553` for a day after it died, because only the parent's open reads them. A child whose
/// parent is itself still running (or `paused`, which `task_resume` brings back) is left to that
/// parent's restart pass.
#[test]
fn a_gone_daemons_child_under_a_finished_parent_is_abandoned() {
    let store = Store::open_in_memory().unwrap();
    let dead = "d-404553-1791825413495565654-0";
    let now = letibot_sessionlog::registry::now_ms();
    store.life_started(dead, now - 30 * 60 * MIN).unwrap();
    store.life_beat(dead, now - 26 * 60 * MIN).unwrap();
    for r in [
        run("s-r-sub-a", "s-r", "done", dead),
        run("s-r-sub-a-sub-vm", "s-r-sub-a", "running", dead),
        run("s-r-sub-b", "s-r", "paused", dead),
        run("s-r-sub-b-sub-vm", "s-r-sub-b", "running", dead),
        run("s-r-sub-c", "s-r", "running", dead),
        run("s-r-sub-c-sub-vm", "s-r-sub-c", "running", dead),
    ] {
        store.put_subagent_run(&r).unwrap();
    }
    let said = pass(&store, None, "d-1-1-0", None);
    let state = |id: &str| store.subagent_run(id).unwrap().unwrap().state;
    assert_eq!(state("s-r-sub-a-sub-vm"), "abandoned", "{said:?}");
    let why = store
        .subagent_run("s-r-sub-a-sub-vm")
        .unwrap()
        .unwrap()
        .outcome
        .unwrap();
    assert!(why.contains(dead) && why.contains("`s-r-sub-a`"), "{why}");
    assert_eq!(
        state("s-r-sub-b-sub-vm"),
        "running",
        "a paused parent comes back"
    );
    assert_eq!(
        state("s-r-sub-c-sub-vm"),
        "running",
        "its parent's open decides"
    );
    assert_eq!(
        state("s-r-sub-c"),
        "running",
        "a root's child: its root's open decides"
    );
    assert_eq!(said.len(), 1, "{said:?}");
}

/// **This daemon's child, stalled under a finished parent, is stopped** — written `killed` with
/// why — and only when BOTH clocks have run: the parent finished half an hour ago, and the
/// child has written nothing for as long. A child that is still writing is working.
#[test]
fn a_stalled_child_under_a_finished_parent_is_stopped() {
    let me = "d-1-1-0";
    let mut parent = run("s-r-sub-a", "s-r", "failed", me);
    parent.updated_ms = NOW - 40 * MIN;
    let mut child = run("s-r-sub-a-sub-vm", "s-r-sub-a", "running", me);
    child.updated_ms = NOW - 45 * MIN;
    let why = stalled_why(&child, Some(&parent), me, NOW - 44 * MIN, NOW)
        .expect("parent failed 40 min ago, child quiet 44 min");
    assert!(
        why.contains("`s-r-sub-a`") && why.contains("failed"),
        "{why}"
    );
    assert_eq!(
        stalled_why(&child, Some(&parent), me, NOW - 5 * MIN, NOW),
        None,
        "it wrote something five minutes ago"
    );
    let mut recent = parent.clone();
    recent.updated_ms = NOW - 5 * MIN;
    assert_eq!(
        stalled_why(&child, Some(&recent), me, NOW - 44 * MIN, NOW),
        None
    );
    let mut working = parent.clone();
    working.state = "running".into();
    assert_eq!(
        stalled_why(&child, Some(&working), me, NOW - 44 * MIN, NOW),
        None
    );
    assert_eq!(
        stalled_why(&child, Some(&parent), "d-other-1-0", NOW - 44 * MIN, NOW),
        None,
        "another daemon's child is that daemon's"
    );
}

/// **A capacity notice's line names the slot, the VM and the owner, and what happens to it.**
#[test]
fn a_holders_line_names_the_tap_the_pid_the_owner_and_the_fate() {
    let h = Holder {
        vm: HeldVm {
            run: "firecode-20261014-191759-812644".into(),
            project: PathBuf::from("/c/s-1-sub-2"),
            name: None,
            tap: Some("fccode1".into()),
            pids: vec![861706],
            started: Some("2026-10-14 19:17:59".into()),
            cgroup: None,
        },
        owner: Owner::Unrecorded { child: None },
        verdict: Verdict::Keep("no record names its owner — not this daemon's to stop".into()),
        run: None,
    };
    assert_eq!(
        h.line(),
        "fccode1: firecracker pid 861706, run firecode-20261014-191759-812644 started \
         2026-10-14 19:17:59; no record names its owner — not this daemon's to stop — left up"
    );
}
