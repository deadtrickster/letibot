//! The running-children cap and the same-objective refusal (`[subagents]`).

use std::sync::Arc;

use crate::harness::admission::{Admission, Admit};
use crate::harness::children::{
    QUEUED_STOP, TaskSlot, admit_under_caps, finish_child_thread, same_objective_refusal,
};
use crate::leticode_config::{ChildLimits, WhenFull};
use letibot_tools::builtins::task::TaskStatus;

fn limits(per_tree: usize, per_daemon: usize, when_full: WhenFull) -> ChildLimits {
    ChildLimits {
        per_tree,
        per_daemon,
        when_full,
    }
}

fn done() -> TaskStatus {
    TaskStatus::Done {
        answer: "finished".into(),
        child: None,
    }
}

/// Two running children fill a tree capped at two: the third is refused by name, another tree
/// still gets a place, and the daemon cap holds across trees.
#[test]
fn the_cap_holds_per_tree_and_per_daemon() {
    let adm = Arc::new(Admission::default());
    let cap = limits(2, 3, WhenFull::Refuse);
    let a = Arc::new(TaskSlot::new("s-root"));
    let b = Arc::new(TaskSlot::new("s-root"));
    let slots = vec![
        ("s-root-sub-1".to_string(), a.clone()),
        ("s-root-sub-2".to_string(), b.clone()),
    ];
    for slot in [&a, &b] {
        match admit_under_caps(&adm, 1, cap, &slots) {
            Ok(Admit::Now(p)) => slot.hold(p),
            _ => panic!("a place under the cap"),
        }
    }
    let why = match admit_under_caps(&adm, 1, cap, &slots) {
        Err(why) => why,
        Ok(_) => panic!("a third child in a tree capped at two"),
    };
    assert!(why.contains("`[subagents] max_running` is 2"), "{why}");
    assert!(
        why.contains("s-root-sub-1") && why.contains("s-root-sub-2"),
        "{why}"
    );
    assert!(why.contains("task_result"), "{why}");

    // Another tree has its own two, and the daemon's three stop it at one.
    let other = Arc::new(TaskSlot::new("s-other"));
    match admit_under_caps(&adm, 2, cap, &[]) {
        Ok(Admit::Now(p)) => other.hold(p),
        _ => panic!("another tree has room"),
    }
    let why = match admit_under_caps(&adm, 2, cap, &[]) {
        Err(why) => why,
        Ok(_) => panic!("a fourth child in a daemon capped at three"),
    };
    assert!(why.contains("daemon_max_running` is 3"), "{why}");
    assert_eq!(adm.running(), 3);

    // An answer frees the place.
    a.settle(done());
    assert_eq!(adm.running(), 2);
    assert!(matches!(
        admit_under_caps(&adm, 1, cap, &slots),
        Ok(Admit::Now(_))
    ));
}

/// Under `queue` a spawn over the cap waits, and starts as soon as a running child answers.
#[test]
fn a_queued_spawn_starts_when_a_running_child_finishes() {
    let adm = Arc::new(Admission::default());
    let cap = limits(1, 32, WhenFull::Queue);
    let running = Arc::new(TaskSlot::new("s-root"));
    match admit_under_caps(&adm, 7, cap, &[]) {
        Ok(Admit::Now(p)) => running.hold(p),
        _ => panic!("the first child runs"),
    }
    let waiting = match admit_under_caps(&adm, 7, cap, &[]) {
        Ok(Admit::Wait(l)) => l,
        _ => panic!("the second child queues"),
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let adm2 = adm.clone();
    std::thread::spawn(move || {
        let permit = adm2.admit_or_wait(7, waiting, || false);
        tx.send(permit.is_some()).expect("send");
        drop(permit);
    });
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(400))
            .is_err(),
        "the queued child must not start while the cap is full"
    );
    running.settle(done());
    assert_eq!(
        rx.recv_timeout(std::time::Duration::from_secs(5)),
        Ok(true),
        "the answer frees the place and the queued child takes it"
    );
}

/// A queued child that is stopped leaves the queue and settles as never started.
#[test]
fn a_queued_child_that_is_stopped_never_starts() {
    let adm = Arc::new(Admission::default());
    let cap = limits(1, 32, WhenFull::Queue);
    let _held = adm.try_admit(9, cap).expect("the running child");
    let slot = Arc::new(TaskSlot::new("s-root"));
    slot.mark_queued(true);
    let s2 = slot.clone();
    let adm2 = adm.clone();
    let waiter = std::thread::spawn(move || adm2.admit_or_wait(9, cap, || s2.was_killed()));
    slot.kill();
    assert!(waiter.join().expect("join").is_none());
    let registry = letibot_sessionlog::registry::Registry::new();
    finish_child_thread(&slot, Err(QUEUED_STOP.into()), &registry);
    assert!(matches!(slot.status(), TaskStatus::Failed { ref why } if why == QUEUED_STOP));
    assert_eq!(adm.running(), 1, "the stopped child took no place");
}

/// A spawn with a running sibling's objective, up to case and spacing, is refused naming it; a
/// settled sibling or another session's child does not count.
#[test]
fn a_duplicate_objective_is_refused_naming_the_running_child() {
    let running = Arc::new(TaskSlot::new("s-root").with_objective("Count the files\nin   src."));
    let answered = Arc::new(TaskSlot::new("s-root").with_objective("write the changelog"));
    answered.settle(done());
    let theirs = Arc::new(TaskSlot::new("s-other").with_objective("fix the build"));
    let slots = vec![
        ("s-root-sub-1".to_string(), running),
        ("s-root-sub-2".to_string(), answered),
        ("s-other-sub-1".to_string(), theirs),
    ];
    let why = same_objective_refusal(&slots, "s-root", "count the files in src")
        .expect("the same objective is refused");
    assert!(why.contains("s-root-sub-1"), "{why}");
    assert!(why.contains("task_result"), "{why}");
    assert_eq!(
        same_objective_refusal(&slots, "s-root", "write the changelog"),
        None
    );
    assert_eq!(
        same_objective_refusal(&slots, "s-root", "fix the build"),
        None
    );
    assert_eq!(
        same_objective_refusal(&slots, "s-root", "count the tests"),
        None
    );
}
