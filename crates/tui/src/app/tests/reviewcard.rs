//! **A review's outcome, one line a notice, and the whole of it one click away.**
//!
//! The operator, after the red-target card landed, pasting a reviewer's *could not decide* that
//! drew as dozens of lines: *"same story about reviews - … one line + popup"*. See
//! `ui::transcript::review_card` and `ui::transcript::line_cards`.

use super::redcard::{holds, popup_text, rows_with, with_notice, with_warning};
use super::*;
use crate::ui::transcript::line_cards::{compact_cards, warning_card};
use crate::ui::transcript::review_card::{Verdict, decisive, notice_review, warning_review};
use rano::render::{Line, Span};
use rano::style::Role;

// Every fixture is what `harnessd` writes — pinned byte for byte by
// `mergequeue::notices::tests::the_tuis_review_card_fixtures_are_what_the_queue_writes`.

/// **The notice the operator pasted** — `notices::parked` for a branch whose reviewer could not
/// decide.
const CANNOT_DECIDE: &str = include_str!("fixtures/review_cannot_decide_notice.txt");
const BLOCKED: &str = include_str!("fixtures/review_blocked_notice.txt");
const RED_TIP_CANNOT_DECIDE: &str =
    include_str!("fixtures/review_red_tip_cannot_decide_notice.txt");
const RED_TIP_BLOCKED: &str = include_str!("fixtures/review_red_tip_blocked_notice.txt");
const REFUSED: &str = include_str!("fixtures/review_refused_message.txt");
const REFUSED_ROOT: &str = include_str!("fixtures/review_refused_root_message.txt");
const VETOED: &str = include_str!("fixtures/review_vetoed_message.txt");
const HANDED_OVER: &str = include_str!("fixtures/review_handed_over_notice.txt");
const NEEDS_HUMAN: &str = include_str!("fixtures/review_needs_human_warning.txt");
const BLOCKED_AFTER: &str = include_str!("fixtures/review_blocked_after_warning.txt");
const COULD_NOT_START: &str = include_str!("fixtures/review_could_not_start_warning.txt");
const CANNOT_RUN: &str = include_str!("fixtures/review_cannot_run_warning.txt");
const REFUSAL_UNDELIVERED: &str = include_str!("fixtures/review_refusal_undelivered_warning.txt");
/// The red target's notice, for a row that holds both.
const RED_NOTICE: &str = include_str!("fixtures/red_main_notice.txt");

const BRANCH: &str = "agent/panic-groupingsets-join";

/// **The operator's line** — the shape they asked for, word for word.
const OPERATORS_LINE: &str = "review · agent/panic-groupingsets-join · could not decide · BUT the \
                              ask's acceptance cannot be met on this base… · click for details";

/// **Every session shape, and the line it draws.**
fn session_shapes() -> Vec<(&'static str, &'static str, String)> {
    vec![
        ("parked, could not decide", CANNOT_DECIDE, OPERATORS_LINE.to_string()),
        (
            "parked, blocked",
            BLOCKED,
            format!(
                "review · {BRANCH} · blocked · the reviewer `s-gatekeeper-sub-17920` vanished \
                 without a verdict · click for details"
            ),
        ),
        (
            "parked, the red tip could not be decided",
            RED_TIP_CANNOT_DECIDE,
            "review · main's red tip 60bf819c · could not decide · BUT neither commit's own gate \
             run was kept… · click for details"
                .to_string(),
        ),
        (
            "parked, the red tip's review blocked",
            RED_TIP_BLOCKED,
            "review · main's red tip 60bf819c · blocked · a reviewer for `main` could not start… · \
             click for details"
                .to_string(),
        ),
        (
            "the refusal, to the child",
            REFUSED,
            format!(
                "review · {BRANCH} · refused · The panic is gone, but the patch catches it rather \
                 than removing its cause… · click for details"
            ),
        ),
        (
            "the refusal, to the root",
            REFUSED_ROOT,
            format!(
                "review · {BRANCH} · refused · The panic is gone, but the patch catches it rather \
                 than removing its cause… · click for details"
            ),
        ),
        (
            "the veto",
            VETOED,
            format!(
                "review · {BRANCH} · vetoed · the acceptance query has to return rows on the \
                 branch · click for details"
            ),
        ),
        (
            "handed to the project",
            HANDED_OVER,
            format!(
                "review · {BRANCH} · handed to you · the review's worktree is gone… · click for \
                 details"
            ),
        ),
    ]
}

/// **Every hub warning shape**: its code, its detail, and the line it draws.
fn warning_shapes() -> Vec<(&'static str, &'static str, String)> {
    vec![
        ("queue_needs_human", NEEDS_HUMAN, OPERATORS_LINE.to_string()),
        (
            "queue_blocked",
            BLOCKED_AFTER,
            format!(
                "review · {BRANCH} · blocked · the reviewer `s-gatekeeper-sub-17920` vanished \
                 without a verdict · click for details"
            ),
        ),
        (
            "queue_blocked",
            COULD_NOT_START,
            format!(
                "review · {BRANCH} · reviewer could not start · the VM did not boot… · click for \
                 details"
            ),
        ),
        (
            "queue_blocked",
            CANNOT_RUN,
            format!(
                "review · {BRANCH} · cannot run · the branch `{BRANCH}` is gone from the \
                 repository · click for details"
            ),
        ),
        (
            "refusal_undelivered",
            REFUSAL_UNDELIVERED,
            format!("review · {BRANCH} · refused, told nobody · click for details"),
        ),
    ]
}

/// **The readers read what the daemon writes** — the fixtures ARE its output, and each comes to
/// the verdict its words say.
#[test]
fn the_readers_read_every_shape_the_queue_writes() {
    for (what, text, verdict) in [
        ("parked", CANNOT_DECIDE, Verdict::CouldNotDecide),
        ("parked", BLOCKED, Verdict::Blocked),
        ("parked", RED_TIP_CANNOT_DECIDE, Verdict::CouldNotDecide),
        ("parked", RED_TIP_BLOCKED, Verdict::Blocked),
        ("refused", REFUSED, Verdict::Refused),
        ("refused_to_root", REFUSED_ROOT, Verdict::Refused),
        ("vetoed", VETOED, Verdict::Vetoed),
        ("handed_over", HANDED_OVER, Verdict::HandedOver),
    ] {
        let r = notice_review(text).unwrap_or_else(|| panic!("{what}: {text}"));
        assert_eq!(r.verdict, verdict, "{what}");
    }
    for (detail, verdict) in [
        (NEEDS_HUMAN, Verdict::CouldNotDecide),
        (BLOCKED_AFTER, Verdict::Blocked),
        (COULD_NOT_START, Verdict::CouldNotStart),
        (CANNOT_RUN, Verdict::CannotRun),
        (REFUSAL_UNDELIVERED, Verdict::RefusedToldNobody),
    ] {
        let r = warning_review(detail).unwrap_or_else(|| panic!("{detail}"));
        assert_eq!(r.verdict, verdict, "{detail}");
        assert_eq!(r.subject, BRANCH);
    }
}

/// **The BUT clause is the decisive one** — the operator's rule: *"for "could not decide" the
/// sentence starting with BUT is the decisive one if present; otherwise the first sentence"* —
/// and its first clause is what the line carries.
#[test]
fn the_sentence_that_opens_with_but_is_the_reason() {
    let r = notice_review(CANNOT_DECIDE).expect("the operator's notice parses");
    assert_eq!(
        r.reason.as_deref(),
        Some("BUT the ask's acceptance cannot be met on this base…")
    );
    // Without a BUT, the first sentence.
    assert_eq!(
        decisive("The test asserts nothing about rows. BUT-less, as it were.").as_deref(),
        Some("The test asserts nothing about rows")
    );
    assert_eq!(
        decisive("It is fine; BUT, as written, it cannot land").as_deref(),
        Some("BUT, as written, it cannot land")
    );
    assert_eq!(decisive("   "), None);
}

/// **The operator's notice is ONE line** — the branch, the verdict, the BUT clause — and nothing
/// of the reasons or of the paragraph to the model.
#[test]
fn the_pasted_notice_draws_as_one_line() {
    let mut a = with_notice(CANNOT_DECIDE);
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    assert_eq!(rows_with(&screen, OPERATORS_LINE).len(), 1, "{joined}");
    for gone in [
        "The two root causes",
        "COPY FROM STDIN",
        "The operator is asked",
        "s-1789919514688401228-sub-1791743034603570463",
    ] {
        assert!(!joined.contains(gone), "`{gone}` is drawn: {joined}");
    }
    assert_eq!(
        a.notice_rows.len(),
        1,
        "one clickable line: {:?}",
        a.notice_rows
    );
}

/// **Every session shape is one line** with its own verdict.
#[test]
fn every_session_shape_draws_as_one_line() {
    for (what, text, line) in session_shapes() {
        let mut a = with_notice(text);
        let screen = a.screen(200, 40);
        let joined = screen.join("\n");
        assert_eq!(rows_with(&screen, &line).len(), 1, "{what}: {joined}");
        assert!(
            !joined.contains("message from your parent"),
            "{what}: {joined}"
        );
        assert_eq!(a.notice_rows.len(), 1, "{what}: {:?}", a.notice_rows);
    }
}

/// **Every hub warning a review raises is one line**, and a click on it opens the detail whole.
#[test]
fn every_review_warning_draws_as_one_line_and_opens_whole() {
    for (code, detail, line) in warning_shapes() {
        let mut a = with_warning(code, detail);
        let screen = a.screen(200, 30);
        let joined = screen.join("\n");
        assert_eq!(rows_with(&screen, &line).len(), 1, "{code}: {joined}");
        assert!(!joined.contains("/queue restart"), "{code}: {joined}");
        assert_eq!(a.card_rows.len(), 1, "{code}: {:?}", a.card_rows);
        let (y, _) = a.card_rows[0];
        assert_eq!(a.key(Key::Click { x: 10, y: y as u16 }), None);
        let whole = popup_text(&mut a);
        assert!(
            holds(&whole, detail),
            "{code}: the popup lost words: {whole}"
        );
        assert_eq!(a.key(Key::Esc), None);
        assert!(a.diff_popup.is_none());
    }
}

/// **A narrow screen cuts the line with the head's `…`** — it never wraps.
#[test]
fn a_narrow_screen_cuts_the_line() {
    let mut a = with_notice(CANNOT_DECIDE);
    let screen = a.screen(70, 30);
    let rows = rows_with(&screen, "review · agent/panic-groupingsets-join");
    assert_eq!(rows.len(), 1, "{}", screen.join("\n"));
    assert!(rows[0].contains('…'), "{rows:?}");
    assert!(!rows[0].contains("click for details"), "{rows:?}");
    for l in &screen {
        assert!(visible_width(l) <= 70, "{l:?}");
    }
    assert_eq!(a.notice_rows.len(), 1);
}

/// **A click on the line opens the whole notice**, every reason in it; End reaches its last
/// words, Esc closes it and the screen is as it was.
#[test]
fn a_click_on_the_review_opens_the_whole_notice() {
    let mut a = with_notice(CANNOT_DECIDE);
    let before = a.screen(120, 20);
    let (y, _) = a.notice_rows[0];
    assert_eq!(a.key(Key::Click { x: 10, y: y as u16 }), None);
    assert!(a.diff_popup.is_some(), "the click opened the popup");
    let edge = a.screen(120, 20).join("\n");
    assert!(
        edge.contains("review · agent/panic-groupingsets-join · could not decide"),
        "the edge names the branch and the verdict: {edge}"
    );
    let whole = popup_text(&mut a);
    for needle in [
        "The reviewer of `agent/panic-groupingsets-join` \
         (`s-1789919514688401228-sub-1791743034603570463`), a branch of your tree, could not \
         decide: The two root causes are the right ones",
        "The guard the brief demands is there.",
        "The `join` patch is at the right site.",
        "BUT the ask's acceptance cannot be met on this base, and the branch's own measurements \
         say it was not met.",
        "COPY FROM STDIN only counts the rows it is given",
        "a new head goes back to review by itself.",
    ] {
        assert!(holds(&whole, needle), "the popup lost `{needle}`: {whole}");
    }
    assert_eq!(a.key(Key::End), None);
    let end = a.screen(120, 20).join("\n");
    assert!(end.contains("review by itself."), "{end}");
    assert_eq!(a.key(Key::Esc), None);
    assert!(a.diff_popup.is_none());
    assert_eq!(a.screen(120, 20), before);
}

/// The escape a role is painted with under the 16-colour palette.
fn sgr_of(role: Role, p: rano::style::Palette) -> String {
    let row =
        crate::ui::render::row_strings(&[Line::new(vec![Span::role("X".to_string(), role)])], p);
    let at = row[0].find('X').expect("the text");
    row[0][..at].to_string()
}

/// **The verdict is drawn in the role the queue pane gives its state** — a refusal and a
/// reviewer that could not decide in the attention register, a blocked review in the failure
/// register — on a session row and on a warning alike.
#[test]
fn the_verdict_is_coloured_by_what_it_means() {
    let cfg = RenderConfig {
        width: 160,
        color: true,
        ..RenderConfig::default()
    };
    let p = cfg.palette();
    assert_eq!(Verdict::CouldNotDecide.role(), Role::Attention);
    assert_eq!(Verdict::Refused.role(), Role::Attention);
    assert_eq!(Verdict::Blocked.role(), Role::Failure);
    assert_ne!(sgr_of(Role::Attention, p), sgr_of(Role::Failure, p));
    for (text, word, role) in [
        (CANNOT_DECIDE, "could not decide", Role::Attention),
        (REFUSED, "refused", Role::Attention),
        (BLOCKED, "blocked", Role::Failure),
    ] {
        let lines = compact_cards(text, &[], &Default::default(), 120).expect("a card row");
        let rows = crate::ui::session_card_block(&lines, 1, &cfg);
        assert_eq!(rows.len(), 1, "{rows:?}");
        let painted = format!("{}{word}", sgr_of(role, p));
        assert!(rows[0].contains(&painted), "{word}: {:?}", rows[0]);
    }
    let n = Note::Warned(letibot_sessionlog::view::Warned {
        code: "queue_blocked".into(),
        detail: BLOCKED_AFTER.into(),
        ts: 1,
    });
    let rows = crate::ui::card_row(&cfg, &n).expect("a card row");
    assert_eq!(rows.len(), 1);
    let painted = format!("{}blocked", sgr_of(Role::Failure, p));
    assert!(rows[0].contains(&painted), "{:?}", rows[0]);
}

/// **A red target and a parked review in one wake** are a line each — the daemon puts the
/// target's trouble before the parked rows (`Harness::queue_emergency_notices`) — and a job's
/// completion after them keeps its own two lines.
#[test]
fn a_row_of_several_notices_is_a_line_a_notice() {
    const JOB: &str = "[job] a job you backgrounded has ended:\n  - `build · j7` exited 0 after \
                       4.4s, wrote 0 bytes: cargo build\nThis is the completion arriving on its \
                       own — you do not need to wait for it, and `job_wait` would only block you \
                       for a result you already have. Read what it wrote with `job_output` \
                       (job=\"…\"), then carry on with what you were doing.";
    let row =
        format!("{HANDED_OVER}\n\n{RED_NOTICE}\n\n{CANNOT_DECIDE}\n\n{RED_TIP_BLOCKED}\n\n{JOB}");
    let mut a = with_notice(&row);
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    for line in [
        "review · agent/panic-groupingsets-join · handed to you",
        "‼ main RED at 60bf819c",
        OPERATORS_LINE,
        "review · main's red tip 60bf819c · blocked",
        "j7 · build · exited 0 4.4s",
    ] {
        assert_eq!(rows_with(&screen, line).len(), 1, "`{line}`: {joined}");
    }
    assert!(!joined.contains("COPY FROM STDIN"), "{joined}");
    // One clickable line per card, two per job; the click opens the row whole.
    assert_eq!(a.notice_rows.len(), 6, "{:?}", a.notice_rows);
    let (y, _) = a.notice_rows[0];
    assert_eq!(a.key(Key::Click { x: 10, y: y as u16 }), None);
    let whole = popup_text(&mut a);
    assert!(holds(&whole, "COPY FROM STDIN only counts"), "{whole}");
    assert!(holds(&whole, "frame_matches_golden"), "{whole}");
}

/// **What it does not know, it does not fold** — drawn as it arrived, every word on the screen,
/// and no click target.
#[test]
fn a_shape_the_reader_does_not_know_falls_back() {
    for (what, text) in [
        (
            "an opening this head was not told about",
            CANNOT_DECIDE.replace("The reviewer of `", "A reviewer of `"),
        ),
        (
            "a verdict this head was not told about",
            CANNOT_DECIDE.replace("could not decide: ", "shrugged: "),
        ),
        (
            "a branch that is not one word",
            CANNOT_DECIDE.replace(BRANCH, "agent/panic groupingsets"),
        ),
        (
            "a closing this head was not told about",
            CANNOT_DECIDE.replace(". The operator is asked", ". A person is asked"),
        ),
        (
            "a row that opens with something else",
            format!("Something the queue says first.\n\n{CANNOT_DECIDE}"),
        ),
    ] {
        let mut a = with_notice(&text);
        let screen = a.screen(200, 80);
        let joined = screen.join("\n");
        assert!(!joined.contains("click for details"), "{what}: {joined}");
        assert!(joined.contains("COPY FROM STDIN"), "{what}: {joined}");
        assert!(a.notice_rows.is_empty(), "{what}");
    }
    // A `queue_blocked` about a landing is the merge queue's, not a review's.
    for (code, detail) in [
        (
            "queue_blocked",
            "`agent/x` cannot land: the landing tree is gone",
        ),
        (
            "queue_blocked",
            &*BLOCKED_AFTER.replace("is BLOCKED after", "is STUCK after"),
        ),
        ("queue_lease_lost", NEEDS_HUMAN),
    ] {
        assert!(warning_card(code, detail).is_none(), "{code}: {detail}");
        let mut a = with_warning(code, detail);
        let joined = a.screen(200, 60).join("\n");
        assert!(joined.contains(code), "{code}: drawn as a note: {joined}");
        assert!(a.card_rows.is_empty(), "{code}");
    }
}
