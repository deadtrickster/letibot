//! **firecode's capacity refusal is read as a wait; everything else stays a failure** — and the
//! sentences a review that stops for a person is built from.

use letibot_tokencore::store::QueueTransition;

use super::*;

/// **The refusal of 2026-10-12, as the reviewer's failure carried it** — firecode's stderr, line
/// breaks and indentation included (the sentence that makes it capacity is wrapped across two).
pub(crate) const FIRECODE_NO_SLOT: &str = "placing this session in a firecode VM: io: firecode up \
refused (exit status: 1): [firecode] starting a VM and leaving it up (log: \
/home/dead/Projects/firecode/runs/up.6DSWx1.log)
[firecode] the VM failed to start:
[firecode] error: this run needs tap fccode5, and creating one needs root - which cannot be
       asked for here: no cached credential, no SUDO_ASKPASS, no terminal.
       Nothing has been changed.

       4 tap(s) exist and 4 have carrier, so the slots below this
       one were taken. NOTE THAT A TAP KEEPS CARRIER WHILE ANY VM HOLDS IT,
       including one orphaned by a firecode that was killed - so the ceiling is
       the number of FREE taps, not the number that exist, and 'firecode ps'
       may show fewer VMs than there are busy slots.

       Either free a slot, or raise the ceiling once in a terminal and it stays
       raised:
         sudo ip tuntap add dev fccode5 mode tap user $(id -u)
         sudo ip addr add 172.16.5.1/30 dev fccode5 && sudo ip link set fccode5 up";

#[test]
fn firecodes_no_free_slot_refusal_is_capacity_with_its_counts_and_its_sudo_lines() {
    let cap = vm_capacity(FIRECODE_NO_SLOT).expect("capacity");
    assert_eq!(cap.tap.as_deref(), Some("fccode5"));
    assert_eq!(cap.slots, Some(4));
    assert_eq!(cap.busy, Some(4));
    assert_eq!(cap.busy_line(), "4 of 4 VM slots are busy");
    assert_eq!(
        cap.raise_ceiling,
        vec![
            "sudo ip tuntap add dev fccode5 mode tap user $(id -u)".to_string(),
            "sudo ip addr add 172.16.5.1/30 dev fccode5 && sudo ip link set fccode5 up".to_string(),
        ]
    );
}

/// **A real placement failure is not capacity** — firecode missing, a broken image, a copy over
/// the ceiling, a host on which no tap was ever made (waiting frees nothing there).
#[test]
fn a_real_placement_failure_is_not_capacity() {
    for real in [
        "placing this session in a firecode VM: io: running firecode: No such file or directory",
        "placing this session in a firecode VM: io: firecode up refused (exit status: 1): \
         [firecode] error: image rootfs.ext4 is corrupt",
        "placing this session in a firecode VM: io: the tree weighs 72.8 GiB, over the 8 GiB \
         ceiling",
        "placing this session in a firecode VM: io: firecode up refused (exit status: 1): \
         [firecode] error: this run needs tap fccode0, and creating one needs root - which cannot \
         be asked for here. 0 tap(s) exist and 0 have carrier, so the slots below this one were \
         taken.",
        "the reviewer vanished",
    ] {
        assert_eq!(vm_capacity(real), None, "{real}");
    }
}

#[test]
fn the_retry_schedule_is_one_two_five_then_every_five_minutes() {
    let mins: Vec<u64> = (1..=6).map(|n| retry_after_ms(n) / 60_000).collect();
    assert_eq!(mins, vec![1, 2, 5, 5, 5, 5]);
}

fn t(from: Option<&str>, to: &str, at: u64, why: &str) -> QueueTransition {
    QueueTransition {
        seq: 0,
        queue: "review".into(),
        entry_id: "e".into(),
        repository: "/r".into(),
        from_state: from.map(str::to_string),
        to_state: to.into(),
        at_ms: at,
        by: "t".into(),
        why: why.into(),
    }
}

/// **The wait is read off the history**: refusals in a row, the claims between them passed over,
/// anything else (a counted failure, a restart) ends it — and a raise already made is seen.
#[test]
fn the_streak_counts_refusals_in_a_row_and_sees_a_raise_already_made() {
    let wait = |at, extra: &str| {
        t(
            Some("reviewing"),
            "waiting",
            at,
            &format!("{WAITING_FOR_SLOT} for `b`: …{extra}"),
        )
    };
    let h = vec![
        t(None, "waiting", 1, "put up for review"),
        t(Some("waiting"), "reviewing", 2, "claimed"),
        t(
            Some("reviewing"),
            "waiting",
            3,
            "the reviewer `x` failed: it died",
        ),
        t(Some("waiting"), "reviewing", 4, "claimed"),
        t(Some("reviewing"), "reviewing", 5, "reviewer started"),
        wait(10, ""),
        t(Some("waiting"), "reviewing", 11, "claimed"),
        wait(20, &format!("; {SLOTS_EXHAUSTED}")),
        t(Some("waiting"), "reviewing", 21, "claimed"),
        wait(30, ""),
    ];
    assert_eq!(
        streak(&h),
        Streak {
            refusals: 3,
            since_ms: Some(10),
            raised: true
        }
    );
    assert_eq!(streak(&h[..5]), Streak::default());
}

#[test]
fn a_waiting_row_is_held_only_by_its_own_capacity_wait() {
    let why = format!("{WAITING_FOR_SLOT} for `b`");
    assert!(held_until(&why, Some(100), 50));
    assert!(!held_until(&why, Some(100), 150), "its time came");
    assert!(
        !held_until("", Some(100), 50),
        "a deadline alone holds nothing"
    );
    assert!(!held_until(&why, None, 50));
}

/// **The gist of a placement failure is firecode's own `error:` sentence**, not its banner — the
/// hub row of 2026-10-12 showed *"starting a VM and leaving it up (log: …)"* and nothing else.
#[test]
fn the_gist_of_a_firecode_refusal_is_its_error_sentence() {
    let g = gist(&format!("the reviewer `x` failed: {FIRECODE_NO_SLOT}"));
    assert!(
        g.starts_with("the reviewer's VM did not start — firecode: this run needs tap fccode5"),
        "{g}"
    );
    assert!(!g.contains("leaving it up"), "{g}");
    assert_eq!(gist("first line\nsecond"), "first line");
}

/// **The sentence a `needs_human` turned on** — the reviewer's `BUT …`, as in the verdict of
/// 2026-10-11 on `agent/panic-groupingsets-join`.
#[test]
fn the_decisive_reason_is_the_but_sentence_in_the_reviewers_own_words() {
    let reasons = vec![
        "The two root causes are the right ones and both patches are correct in themselves. \
         `groupingsets`: the vendored crate now carries `has_grouping_set` at aggregates/mod.rs:670."
            .to_string(),
        "BUT the ask's acceptance cannot be met on this base, and the branch's own measurements \
         say it was not met. On main the memory engine cannot hold row values."
            .to_string(),
        "So the change is correct but inert where the queue would land it.".to_string(),
    ];
    assert_eq!(
        decisive_reason(&reasons),
        "BUT the ask's acceptance cannot be met on this base, and the branch's own measurements \
         say it was not met."
    );
    assert_eq!(
        decisive_reason(&["It is fine. i.e. nothing. More here.".to_string()]),
        "It is fine. i.e. nothing."
    );
    assert_eq!(decisive_reason(&[]), "");
}

#[test]
fn the_heads_reviewers_could_not_decide_at_are_read_off_the_history() {
    let h = vec![
        t(
            Some("reviewing"),
            "cannot_decide",
            1,
            "reviewer:a could not decide at 2cefd4b97b: x",
        ),
        t(
            Some("reviewing"),
            "rejected",
            2,
            "rejected at 7198e87 by reviewer:b: y",
        ),
        t(
            Some("reviewing"),
            "cannot_decide",
            3,
            "reviewer:c could not decide at 8e4b727: z",
        ),
    ];
    assert_eq!(undecided_heads(&h), vec!["2cefd4b97b", "8e4b727"]);
}
