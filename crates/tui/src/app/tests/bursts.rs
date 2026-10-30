//! **A wall of notices becomes a count** — one row per KIND, with the number, and the individual
//! notices one click away.
//!
//! The operator, twice: *"i still get walls of notices"*. The retry row got a counter and stopped
//! being a wall; every other kind did not. This is that rule applied to a kind rather than to one
//! endpoint's attempts — see `app::COALESCE_MS` for the window, `app::burst_at` for which note
//! draws, and `ui::panes::notes::burst_card` for the words.

use super::*;

fn warning(a: &mut App, seq: u64, ts: u64, code: &str, detail: &str) {
    a.apply(ServerFrame::Event(env_at(
        seq,
        ts,
        SessionEvent::Warning {
            code: code.into(),
            detail: detail.into(),
            compaction: None,
        },
    )));
}

fn head() -> App {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t1"));
    let mut a = app();
    a.clock(1_000);
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    a
}

/// A `queue_blocked` detail in the shape the queue writes it (`notices::blocked_after` and
/// `review_cannot_run`): the review, the branch it is about, and the verdict's word.
fn blocked(branch: &str) -> String {
    format!(
        "the review of `{branch}` (`s-1-sub-1`) is BLOCKED: 3 attempts to review it at 9d3e5a7 \
         ended without a verdict — no reviewer judged the branch: the reviewer \
         `s-gatekeeper-sub-1` failed to start. `/queue restart s-1-sub-1` asks again."
    )
}

/// How many rows of the screen contain `needle`.
fn rows(screen: &[String], needle: &str) -> usize {
    screen.iter().filter(|r| r.contains(needle)).count()
}

/// **At one there is no burst** — the notice's own card, byte for byte what this head drew before
/// there were bursts. The count is what a burst adds, and a count of one is not a count.
#[test]
fn one_notice_of_a_kind_is_drawn_exactly_as_it_was() {
    let mut a = head();
    warning(&mut a, 1, 1_000, "queue_blocked", &blocked("agent/a"));
    let screen = a.screen(120, 40).join("\n");
    assert!(
        screen.contains(
            "session · review · agent/a · blocked · 3 attempts at 9d3e5a7 ended without a \
             verdic… · click for details"
        ),
        "the one row is not the notice's own card:\n{screen}"
    );
    // And no count anywhere: `1 reviews could not run` is worse than the wall.
    assert!(!screen.contains("1 reviews"), "{screen}");
    assert!(!screen.contains("could not run"), "{screen}");
}

/// **Five of a kind are ONE row, and the row says five.** The operator's photograph, answered:
/// five `queue_blocked` in a minute is one line with a number rather than five lines.
#[test]
fn five_of_a_kind_are_one_row_with_a_count() {
    let mut a = head();
    warning(&mut a, 1, 1_000, "queue_blocked", &blocked("agent/a"));
    for i in 2..=5u64 {
        warning(
            &mut a,
            i,
            i * 1_000,
            "queue_blocked",
            &blocked(&format!("agent/{i}")),
        );
    }
    let screen = a.screen(120, 40);
    let all = screen.join("\n");
    assert_eq!(
        rows(&screen, "click for details"),
        1,
        "five notices, and not one row:\n{all}"
    );
    assert!(
        all.contains("session · ! 5 reviews could not run · click for details"),
        "the row's words are not the count's:\n{all}"
    );
    // **The count is the notes'**: five filed, five counted, none dropped.
    assert_eq!(a.notes.len(), 5, "a note was dropped, not counted");
    assert_eq!(a.retired_notes(), 0);
}

/// **The count and the expansion agree** — `5` means five, and the popup shows five.
///
/// The pair is what makes the row readable, and a count that does not add up is worse than the
/// wall it replaced. Each member is the row the head would have drawn for it alone, so the burst
/// loses nothing: the branch is on the member's own line, and its whole text is under it.
#[test]
fn the_count_and_the_expansion_agree() {
    let mut a = head();
    for i in 1..=5u64 {
        warning(
            &mut a,
            i,
            i * 1_000,
            "queue_blocked",
            &blocked(&format!("agent/{i}")),
        );
    }
    let popup = newest_notice(&mut a);
    assert_eq!(
        popup.matches("\n\n── ").count(),
        5,
        "the row says five and the list holds fewer:\n{popup}"
    );
    assert!(popup.starts_with("5 × queue_blocked"), "{popup}");
    // **Newest first**, which is the order the reader is looking for.
    let at = |b: &str| {
        popup
            .find(b)
            .unwrap_or_else(|| panic!("{b} missing:\n{popup}"))
    };
    assert!(at("agent/5") < at("agent/4"), "{popup}");
    assert!(at("agent/4") < at("agent/2"), "{popup}");
    assert!(at("agent/2") < at("agent/1"), "{popup}");
    // **Which branch, without reading the text under it** — the member's own line.
    for i in 1..=5u64 {
        assert!(
            popup.contains(&format!("· review · agent/{i} · blocked ·")),
            "the member for agent/{i} does not name its branch:\n{popup}"
        );
    }
    // And the whole text of each, not a summary of it.
    assert_eq!(
        popup
            .matches("`/queue restart s-1-sub-1` asks again.")
            .count(),
        5,
        "{popup}"
    );
}

/// **A red burst keeps the red register** — the count is not a way to make a failure quiet.
///
/// A burst is one KIND, and a kind's register is its code's (`sessionlog::warning::class`), so
/// every member of a failure burst is a failure: the row carries `!`, and it is not on a timer
/// (`FLEETING` holds only `Routine` codes, which is the guard that keeps a fault off one).
#[test]
fn a_failure_burst_carries_the_failure_register_and_never_expires() {
    let mut a = head();
    for i in 1..=2u64 {
        warning(
            &mut a,
            i,
            i * 1_000,
            "target_red",
            &format!(
                "EMERGENCY: `main` is RED at 60bf819{i} in letibot. The merge queue lands nothing."
            ),
        );
    }
    let all = a.screen(120, 40).join("\n");
    assert!(
        all.contains("session · ! 2 targets RED · click for details"),
        "{all}"
    );
    // **Not on a timer**, an hour later.
    a.clock(3_600_000);
    let later = a.screen(120, 40).join("\n");
    assert!(
        later.contains("2 targets RED"),
        "a red burst went quiet on a timer:\n{later}"
    );

    // **And a routine burst is not red** — the quiet register is the count alone, because the
    // `·` it would carry is the one the frame's own `session ·` already ends in.
    let mut b = head();
    for i in 1..=3u64 {
        warning(
            &mut b,
            i,
            i * 1_000,
            "queue_branch_kept",
            &format!("after landing `agent/{i}`: the worktree is dirty"),
        );
    }
    let quiet = b.screen(120, 40).join("\n");
    assert!(
        quiet.contains("session · 3 trees kept · click for details"),
        "{quiet}"
    );
    assert!(!quiet.contains("! 3 trees"), "{quiet}");
}

/// **Do not coalesce across kinds** — a count is a statement about ONE kind of thing, and
/// `5 notices` that are three different kinds is not a number a reader can act on.
#[test]
fn two_kinds_are_two_rows() {
    let mut a = head();
    for (i, code, detail) in [
        (1u64, "queue_blocked", blocked("agent/a")),
        (
            2,
            "queue_branch_kept",
            "after landing `agent/b`: the worktree is dirty".to_string(),
        ),
        (3, "queue_blocked", blocked("agent/c")),
        (
            4,
            "queue_branch_kept",
            "after landing `agent/d`: the worktree is dirty".to_string(),
        ),
    ] {
        warning(&mut a, i, i * 1_000, code, &detail);
    }
    let screen = a.screen(120, 40);
    let all = screen.join("\n");
    assert_eq!(rows(&screen, "click for details"), 2, "{all}");
    assert!(all.contains("! 2 reviews could not run"), "{all}");
    assert!(all.contains("2 trees kept"), "{all}");
    // The interleaving does not split a kind: both `queue_blocked` are in one burst, which is
    // the case the operator photographs (a wall of alternating raises).
    assert!(!all.contains("1 reviews"), "{all}");
}

/// **A notice that is still live and changing is never counted** — `LIVE`'s rule, and the reason
/// it exists: a burst is a count of things that HAPPENED, and a line that says *this is still
/// true* would become a number nobody can act on.
#[test]
fn a_live_notice_is_never_coalesced() {
    let mut a = head();
    for i in 1..=3u64 {
        warning(
            &mut a,
            i,
            i * 1_000,
            "store_busy_wait",
            &format!(
                "the session store is busy — this session is waiting, not failing ({}s so far).",
                i * 5
            ),
        );
    }
    let screen = a.screen(120, 40);
    let all = screen.join("\n");
    assert_eq!(
        rows(&screen, "store_busy_wait"),
        3,
        "a standing condition was folded into a count:\n{all}"
    );
    assert!(!all.contains("3 store_busy_wait"), "{all}");
}

/// **The window is a window** — a notice that arrives long after the one before it starts its own
/// row, because a morning's work is history and not a wall.
#[test]
fn a_notice_outside_the_window_starts_its_own_row() {
    let mut a = head();
    warning(&mut a, 1, 1_000, "queue_blocked", &blocked("agent/a"));
    warning(&mut a, 2, 2_000, "queue_blocked", &blocked("agent/b"));
    // One burst: inside the window.
    let screen = a.screen(120, 40);
    assert_eq!(
        rows(&screen, "click for details"),
        1,
        "{}",
        screen.join("\n")
    );

    // And one an hour later is its own row, alone.
    warning(&mut a, 3, 3_600_000, "queue_blocked", &blocked("agent/c"));
    let screen = a.screen(120, 40);
    let all = screen.join("\n");
    assert_eq!(
        rows(&screen, "click for details"),
        2,
        "an hour apart is two pieces of news:\n{all}"
    );
    assert!(all.contains("! 2 reviews could not run"), "{all}");
    assert!(all.contains("review · agent/c · blocked"), "{all}");
}

/// **`ctrl-n` clears the whole burst**, and the count is hidden rather than dropped — the
/// watermark's own rule (`App::mark_read` takes the newest `ts` per kind, and a burst's members
/// are all at or before it).
#[test]
fn ctrl_n_clears_the_whole_burst() {
    let mut a = head();
    for i in 1..=4u64 {
        warning(
            &mut a,
            i,
            i * 1_000,
            "queue_blocked",
            &blocked(&format!("agent/{i}")),
        );
    }
    assert_eq!(
        a.screen(120, 40)
            .join("\n")
            .matches("click for details")
            .count(),
        1
    );
    assert_eq!(a.key(Key::CtrlN), None);
    let after = a.screen(120, 40).join("\n");
    assert!(
        !after.contains("reviews could not run") && !after.contains("agent/4"),
        "the press left the burst standing:\n{after}"
    );
    assert_eq!(a.notes.len(), 4, "a note was dropped, not retired");
    assert_eq!(a.retired_notes(), 4);
    assert_eq!(a.read_upto("queue_blocked"), Some(4_000));
}

/// **A retired member does not take the burst with it.**
///
/// `/notes dismiss N` acts on ONE notice, and its key is its own — so the row moves to the next
/// live member and counts what is left. The alternative is a row that hides four notices because
/// the reader retired the first one, which is the defect a burst could most easily introduce.
#[test]
fn a_retired_member_leaves_the_others_drawn() {
    let mut a = head();
    for i in 1..=4u64 {
        warning(
            &mut a,
            i,
            i * 1_000,
            "queue_blocked",
            &blocked(&format!("agent/{i}")),
        );
    }
    let head_key = note_key(&a.notes[0].1);
    assert_eq!(a.retire(vec![head_key]), 1);
    let all = a.screen(120, 40).join("\n");
    assert!(
        all.contains("! 3 reviews could not run"),
        "the burst lost its head and went with it:\n{all}"
    );
    // **And the count is the live members', not the chain's** — three, not four.
    assert_eq!(a.notes.len(), 4);
    assert_eq!(a.retired_notes(), 1);
}

/// **The count is true on the screen as the burst grows** — the walk is incremental, so a member
/// arriving after the row was drawn has to rewind the row that changed (`App::file_note`).
#[test]
fn the_count_grows_on_the_screen_when_a_member_arrives() {
    let mut a = head();
    warning(&mut a, 1, 1_000, "queue_blocked", &blocked("agent/a"));
    // Draw it once, alone: the notice's own card.
    assert!(
        a.screen(120, 40).join("\n").contains("review · agent/a"),
        "the first is not drawn alone"
    );
    warning(&mut a, 2, 2_000, "queue_blocked", &blocked("agent/b"));
    let two = a.screen(120, 40).join("\n");
    assert!(
        two.contains("! 2 reviews could not run"),
        "the row was drawn once and never grew:\n{two}"
    );
    warning(&mut a, 3, 3_000, "queue_blocked", &blocked("agent/c"));
    let three = a.screen(120, 40).join("\n");
    assert!(
        three.contains("! 3 reviews could not run"),
        "the count stopped at two:\n{three}"
    );
    assert_eq!(
        three.matches("click for details").count(),
        1,
        "the burst grew a second row:\n{three}"
    );
}

/// **The count grows when a member of the same kind arrives with another kind in between** —
/// the interleaved case, and the one a look-back at "the last note filed" gets wrong.
///
/// A `queue_blocked` and a `queue_branch_kept` alternate, which is exactly what the queue does
/// when a host dies: both kinds raise per row. The second `queue_blocked` chains with the FIRST,
/// not with the note beside it, so the row it changes is the first one's — two rows back.
#[test]
fn a_member_behind_another_kind_still_grows_its_own_burst() {
    let mut a = head();
    warning(&mut a, 1, 1_000, "queue_blocked", &blocked("agent/a"));
    // Drawn alone, so the row exists before the second member arrives.
    assert!(a.screen(120, 40).join("\n").contains("review · agent/a"));
    warning(
        &mut a,
        2,
        2_000,
        "queue_branch_kept",
        "after landing `agent/b`: the worktree is dirty",
    );
    assert!(a.screen(120, 40).join("\n").contains("agent/b"));
    warning(&mut a, 3, 3_000, "queue_blocked", &blocked("agent/c"));
    let all = a.screen(120, 40).join("\n");
    assert!(
        all.contains("! 2 reviews could not run"),
        "the interleaved member did not grow the burst behind it:\n{all}"
    );
    assert!(
        all.contains("session · queue_branch_kept · after landing `agent/b`"),
        "the other kind is still its own row, alone:\n{all}"
    );
    assert_eq!(
        all.matches("click for details").count(),
        2,
        "two kinds, two rows:\n{all}"
    );
}

/// **A fleeting burst goes together, and the count is honest on the way out.**
///
/// `merge_queued` is the one code in `FLEETING`; its members expire on their own clocks, so a
/// burst of two loses one member first — and the row must then be the survivor's own card rather
/// than a count of two with one of them gone.
#[test]
fn a_fleeting_burst_loses_its_members_one_clock_at_a_time() {
    let mut a = head();
    warning(
        &mut a,
        1,
        1_000,
        "merge_queued",
        "`agent/child-one` is in the merge queue as `c1`",
    );
    warning(
        &mut a,
        2,
        2_000,
        "merge_queued",
        "`agent/child-two` is in the merge queue as `c2`",
    );
    let all = a.screen(120, 40).join("\n");
    assert!(all.contains("2 branches queued"), "{all}");

    // The first is due; the second is not yet.
    a.clock(1_000 + FLEETING_MS);
    let one = a.screen(120, 40).join("\n");
    assert!(
        one.contains("review") || one.contains("`agent/child-two`") || one.contains("child-two"),
        "the survivor is not drawn:\n{one}"
    );
    assert!(
        !one.contains("2 branches queued"),
        "a count outlived one of its members:\n{one}"
    );

    a.clock(2_000 + FLEETING_MS);
    let none = a.screen(120, 40).join("\n");
    assert!(!none.contains("child-two"), "the last did not go:\n{none}");
    assert!(a.notes.is_empty(), "{:?}", a.notes);
}

/// **A refusal is one of a KIND, not a kind of its own** — `denied:<request id>` is one code a
/// request, and the id is the notice's identity rather than its kind
/// (`letibot_sessionlog::warning::family`). Without that, five refusals are five kinds and the
/// wall of red the operator photographs is never counted.
///
/// Each member keeps its own id on its own line in the popup, which is the half the brief calls
/// *an actionable notice keeps its own identity inside the row*.
#[test]
fn refusals_are_counted_by_family_and_keep_their_own_ids() {
    use letibot_sessionlog::view::Warned;
    let mut a = head();
    for i in 1..=5u64 {
        a.note(Note::Warned(Warned {
            code: format!("denied:req-{i}"),
            detail: format!(
                "REFUSED write — `src/{i}.rs`. denied by boundary:host: not this session's tree."
            ),
            ts: i * 1_000,
        }));
    }
    let screen = a.screen(120, 40);
    let all = screen.join("\n");
    assert_eq!(rows(&screen, "click for details"), 1, "{all}");
    assert!(
        all.contains("session · ! 5 calls the gate refused · click for details"),
        "{all}"
    );
    let popup = newest_notice(&mut a);
    assert_eq!(popup.matches("\n\n── ").count(), 5, "{popup}");
    for i in 1..=5u64 {
        assert!(
            popup.contains(&format!("denied:req-{i}")),
            "req-{i} lost its identity:\n{popup}"
        );
    }
    // **And `ctrl-n` is a mark about the family**, so it clears all five — the burst is what the
    // reader is clearing.
    assert_eq!(a.key(Key::CtrlN), None);
    assert!(
        !a.screen(120, 40)
            .join("\n")
            .contains("calls the gate refused")
    );
    assert_eq!(a.read_upto("denied"), Some(5_000));
    assert_eq!(a.notes.len(), 5, "hidden, not dropped");
}

/// **One member of a burst is still one click from its own text**, and a click on the row opens
/// the list — the click map has one entry and it covers the row (`note_cards`).
#[test]
fn the_row_is_the_click_target_and_it_opens_the_list() {
    let mut a = head();
    for i in 1..=3u64 {
        warning(
            &mut a,
            i,
            i * 1_000,
            "queue_blocked",
            &blocked(&format!("agent/{i}")),
        );
    }
    a.screen(120, 40);
    assert_eq!(a.note_cards.len(), 1, "the burst is not one card");
    let card = a.note_cards[0].clone();
    assert_eq!(card.lines, 1, "a burst is one line");
    assert_eq!(card.title.0, "3 × queue_blocked");
    // The same text `alt-n` opens, so a click and the chord cannot disagree.
    assert_eq!(card.text, newest_notice(&mut a));
}
