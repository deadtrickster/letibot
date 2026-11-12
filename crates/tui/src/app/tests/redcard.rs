//! A red merge target, one line a notice, and the whole of it one click away. See
//! `ui::transcript::red_card`.

use super::*;
use crate::ui::transcript::red_card::{Trouble, notice_tip, unjudged_warning_tip, warning_tip};

/// The session notice, as `mergequeue::notices::health` writes it for the operator's red
/// `main` - pinned byte for byte by `harnessd`'s
/// `the_tuis_red_card_fixtures_are_what_the_queue_writes`.
const NOTICE: &str = include_str!("fixtures/red_main_notice.txt");
/// The `target_red` warning `RepoQueue::target_went_red` raises for the same tip, pinned by the
/// same test.
const WARNING: &str = include_str!("fixtures/red_main_warning.txt");

/// The session notice for a `main` whose gate step was killed at the gate's memory cap, as
/// `mergequeue::notices::health` writes it - pinned by the same test.
const KILLED_NOTICE: &str = include_str!("fixtures/unjudged_main_killed_notice.txt");
/// The `gate_killed` hub warning for the same tip (`mergequeue::notices::gate_killed`).
const KILLED_WARNING: &str = include_str!("fixtures/unjudged_main_killed_warning.txt");

/// The paragraph to the model, which the line does not draw.
const TO_THE_MODEL: &str = "See `queue` action: \"status\" for both queues";

/// A head attached to `s` with `notice` as its one row, spoken by the harness.
pub(super) fn with_notice(notice: &str) -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "user".into(),
            ledger_head: String::new(),
            item: None,
            body_bytes: 0,
        },
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Agent,
                parts: vec![UserPart::Text {
                    text: notice.into(),
                }],
            }),
        },
    )));
    a
}

/// A head attached to `s` with one hub warning.
pub(super) fn with_warning(code: &str, detail: &str) -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Warning {
            code: code.into(),
            detail: detail.into(),
            compaction: None,
        },
    )));
    a
}

pub(super) fn rows_with<'a>(screen: &'a [String], needle: &str) -> Vec<&'a String> {
    screen.iter().filter(|l| l.contains(needle)).collect()
}

/// The popup's rows, as text - laid out by a frame first.
pub(super) fn popup_text(a: &mut App) -> String {
    a.screen(120, 30);
    let p = a.diff_popup.as_ref().expect("the popup is open");
    p.laid
        .as_ref()
        .expect("the popup was laid out")
        .rows
        .iter()
        .map(|l| l.plain())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether `hay` holds `needle`, the wrap's line breaks read as the spaces they replaced.
pub(super) fn holds(hay: &str, needle: &str) -> bool {
    let flat = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    flat(hay).contains(&flat(needle))
}

/// The parsers read what the daemon writes - the fixtures ARE its output.
#[test]
fn the_parsers_read_the_queues_own_texts() {
    let n = notice_tip(NOTICE).expect("the session notice parses");
    assert_eq!(n.trouble, Trouble::Red);
    assert_eq!(n.target, "main");
    assert_eq!(n.sha, "60bf819c3342");
    assert_eq!(n.repo.as_deref(), Some("/home/dead/Projects/firecode"));
    assert_eq!(n.step.as_deref(), Some("bin/firecode test"));
    assert_eq!(n.of, Some((1, 1)));
    assert_eq!(n.review.as_deref(), Some("red-main-60bf819c3342"));
    let w = warning_tip(WARNING).expect("the warning parses");
    assert_eq!(w.target, "main");
    assert_eq!(w.sha, "60bf819c33425f282d547ffa2df5e2d840a75bef");
    assert_eq!(w.repo.as_deref(), Some("/home/dead/Projects/firecode"));
    assert_eq!(w.step.as_deref(), Some("bin/firecode test"));
}

/// The session notice is ONE line: the target, the short sha, the repository's name, the
/// failing step and which of how many, the review - and nothing of the gate's output or of the
/// paragraph to the model.
#[test]
fn the_emergency_notice_draws_as_one_line() {
    let mut a = with_notice(NOTICE);
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    let line = "‼ main RED at 60bf819c · firecode · bin/firecode test failed (step 1/1) · review \
                red-main-60bf819c3342 · click for details";
    assert_eq!(rows_with(&screen, line).len(), 1, "{joined}");
    assert!(!joined.contains("[gate memory"), "{joined}");
    assert!(!joined.contains("frame_matches_golden"), "{joined}");
    assert!(!joined.contains(TO_THE_MODEL), "{joined}");
    assert!(!joined.contains("EMERGENCY —"), "{joined}");
    assert_eq!(
        a.notice_rows.len(),
        1,
        "one clickable line: {:?}",
        a.notice_rows
    );
}

/// A narrow screen cuts the line with the head's `…` - it never wraps.
#[test]
fn a_narrow_screen_cuts_the_line() {
    let mut a = with_notice(NOTICE);
    let screen = a.screen(70, 30);
    let rows = rows_with(&screen, "‼ main RED at 60bf819c");
    assert_eq!(rows.len(), 1, "{}", screen.join("\n"));
    assert!(rows[0].contains('…'), "{rows:?}");
    for l in &screen {
        assert!(visible_width(l) <= 70, "{l:?}");
    }
    assert_eq!(a.notice_rows.len(), 1);
}

/// A click on the line opens the whole notice - the gate's output tail and its memory line
/// included - in the job card's popup; Esc closes it.
#[test]
fn a_click_on_the_emergency_opens_the_whole_notice() {
    let mut a = with_notice(NOTICE);
    let before = a.screen(120, 30);
    let (y, _) = a.notice_rows[0];
    assert_eq!(a.key(Key::Click { x: 10, y: y as u16 }), None);
    assert!(a.diff_popup.is_some(), "the click opened the popup");
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("‼ main RED at 60bf819c · firecode"),
        "the edge names the target: {screen}"
    );
    let whole = popup_text(&mut a);
    for needle in [
        "EMERGENCY — `main` at 60bf819c3342 in /home/dead/Projects/firecode is RED",
        "The failing step is 1 of 1, `bin/firecode test`. The end of its output:",
        "frame_matches_golden",
        "(`red-main-60bf819c3342`)",
        "regate once the cause is gone.",
    ] {
        assert!(holds(&whole, needle), "the popup lost `{needle}`: {whole}");
    }
    // A peak far from its cap is the operator's (the warning keeps it), not the model's.
    assert!(!holds(&whole, "[gate memory: peak 1.6 GiB"), "{whole}");
    // Scrollable, like the job card's: the whole notice is taller than the screen.
    assert_eq!(a.key(Key::End), None);
    let end = a.screen(120, 30).join("\n");
    assert!(end.contains("regate once the cause"), "{end}");
    assert_eq!(a.key(Key::Esc), None);
    assert!(a.diff_popup.is_none());
    assert_eq!(a.screen(120, 30), before);
}

/// The hub warning is ONE line too - read by the table, since the conversation draws only the
/// model's own row and `/notes` is where a warning's sentence is opened.
#[test]
fn the_target_red_warning_reads_as_one_line_and_is_no_row() {
    let line = "‼ main RED at 60bf819c · firecode · bin/firecode test failed · click for details";
    let card = crate::ui::transcript::line_cards::warning_card("target_red", WARNING)
        .expect("the table reads the queue's sentence");
    assert_eq!(card.line, line);
    let mut a = with_warning("target_red", WARNING);
    let joined = a.screen(200, 40).join("\n");
    assert!(
        !joined.contains(line),
        "the warning drew a row of the conversation\n{joined}"
    );
    assert_eq!(a.card_rows.len(), 0, "{joined}");
    assert!(
        a.notes.iter().any(
            |(place, n)| matches!(n, Note::Warned(w) if w.code == "target_red")
                && *place == Placed::Before
        ),
        "/notes lost it: {:?}",
        a.notes
    );
    // The whole of the daemon's sentence is on the record, wherever it is opened from.
    let detail = a
        .notes
        .iter()
        .find_map(|(_, n)| match n {
            Note::Warned(w) if w.code == "target_red" => Some(w.detail.clone()),
            _ => None,
        })
        .expect("the warning is held");
    for needle in [
        "EMERGENCY: `main` is RED at 60bf819c33425f282d547ffa2df5e2d840a75bef in \
         /home/dead/Projects/firecode.",
        "approved branches go back to review; enqueueing goes on.",
        "… [12500 byte(s) dropped from the front]",
        "test scene::tests::case_000 ... ok",
        "[gate memory: peak 1.6 GiB of a 40.9 GiB cap]",
    ] {
        assert!(holds(&detail, needle), "the record lost `{needle}`");
    }
}

/// The line is drawn in the failure register, its head red.
#[test]
fn the_warning_line_is_the_alarm_register() {
    const RED: &str = "\u{1b}[31m";
    let cfg = RenderConfig {
        width: 120,
        color: true,
        ..RenderConfig::default()
    };
    let n = Note::Warned(letibot_sessionlog::view::Warned {
        code: "target_red".into(),
        detail: WARNING.into(),
        ts: 1,
    });
    let rows = crate::ui::card_row(&cfg, &n).expect("a red row");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].contains(RED), "{:?}", rows[0]);
}

/// The reviewer's `target_red` warnings are one line as well - a fix branch opened, one that
/// could not be, and a project that only raises.
#[test]
fn the_reviewers_warnings_draw_as_one_line() {
    let why = "gatekeeper-3 finds `main` broken at 60bf819c33425f282d547ffa2df5e2d840a75bef: \
               the golden frame was regenerated with the old palette";
    for (detail, says) in [
        (
            format!(
                "EMERGENCY: {why}. A fix branch `fix-main-60bf819c` was opened by s12 and goes \
                 through the queue; it is the only branch that lands while `main` is red."
            ),
            "fix branch fix-main-60bf819c opened by s12",
        ),
        (
            format!(
                "EMERGENCY: {why}. A fix branch could NOT be opened: no worktree. A person has \
                 to fix `main`."
            ),
            "NO fix branch could be opened",
        ),
        (
            format!(
                "EMERGENCY: {why}. This project's policy is `red_target = \"raise\"`: no fix \
                 branch was opened — the target needs a person's fix."
            ),
            "no fix branch: a person fixes it",
        ),
    ] {
        let line = format!(
            "‼ main RED at 60bf819c · gatekeeper-3 finds it broken · {says} · click for details"
        );
        let card = crate::ui::transcript::line_cards::warning_card("target_red", &detail)
            .expect("the reviewer's sentence parses");
        assert_eq!(card.line, line, "the table reads it");
        // And it is not a row of the conversation: the model got the same event as the root
        // session's `EMERGENCY - ` notice, and THAT is the row. See `warning::TWIN`.
        let mut a = with_warning("target_red", &detail);
        let screen = a.screen(200, 30);
        assert!(
            !screen.join("\n").contains(&line),
            "the warning drew a row of its own: {}",
            screen.join("\n")
        );
        assert_eq!(a.card_rows.len(), 0);
    }
}

/// The notice's siblings - the gate TIMED OUT, and the gate could not judge the tip - are
/// one line with their own word.
#[test]
fn a_timed_out_or_unjudged_target_is_one_line() {
    let tail = "\n\n```\nstill compiling\n```\n\nSee `queue` action: \"status\" for both queues \
                and the gate's evidence, and regate once the cause is gone.";
    for (notice, line) in [
        (
            format!(
                "The merge queue's gate of `main` at 60bf819c3342 in /home/dead/Projects/firecode \
                 TIMED OUT, so the tip is unjudged and nothing lands on `main`.\n\nThe failing \
                 step is 2 of 3, `bin/firecode test`. The end of its output:{tail}"
            ),
            "‼ main gate TIMED OUT at 60bf819c · firecode · bin/firecode test timed out (step 2/3)",
        ),
        (
            format!(
                "The merge queue CANNOT JUDGE `main` at 60bf819c3342 in /home/dead/Projects/firecode: \
                 the gate's own setup failed.\n\nThe queue's words:{tail}"
            ),
            "‼ main CANNOT BE JUDGED at 60bf819c · firecode · the gate's setup failed",
        ),
    ] {
        let mut a = with_notice(&notice);
        let screen = a.screen(200, 30);
        assert_eq!(rows_with(&screen, line).len(), 1, "{}", screen.join("\n"));
        assert_eq!(a.notice_rows.len(), 1);
    }
}

/// A sentence the daemon learns later does not cost the line: the parser keys on the opening, the
/// target, the sha, the repository and the step, so a warning that grows a sentence still matches -
/// and the popup keeps the new sentence.
#[test]
fn an_extra_sentence_keeps_the_one_line_and_the_popup_shows_it() {
    const EXTRA: &str = "The landing tree has no `images/`, `vendor/` (gitignored, present in your \
                         checkout); if the gate needs them, set `[queue] gate_carry`.";
    let mid = NOTICE.replace(
        "\n\nNothing lands on",
        &format!("\n\n{EXTRA}\n\nNothing lands on"),
    );
    let end = format!("{NOTICE}\n\n{EXTRA}");
    for notice in [mid, end] {
        let mut a = with_notice(&notice);
        let screen = a.screen(200, 40);
        let joined = screen.join("\n");
        assert_eq!(
            rows_with(
                &screen,
                "‼ main RED at 60bf819c · firecode · bin/firecode test failed"
            )
            .len(),
            1,
            "{joined}"
        );
        assert!(!joined.contains("gate_carry"), "{joined}");
        let (y, _) = a.notice_rows[0];
        a.key(Key::Click { x: 10, y: y as u16 });
        assert!(holds(&popup_text(&mut a), EXTRA));
    }
    // And a warning whose evidence grew a sentence after the output: the table still reads it,
    // and it is still not a row - the notice above is the row for this event.
    let mut a = with_warning("target_red", &format!("{WARNING}\n{EXTRA}"));
    let screen = a.screen(200, 40);
    assert!(
        !screen.join("\n").contains("‼ main RED at 60bf819c"),
        "the warning drew a row of its own: {}",
        screen.join("\n")
    );
    assert!(
        a.notes.iter().any(|(place, n)| {
            matches!(n, Note::Warned(w) if w.code == "target_red") && *place == Placed::Before
        }),
        "and it is held beside the conversation: {:?}",
        a.notes
    );
}

/// An emergency and a job's completion in one wake are one line and two.
#[test]
fn an_emergency_beside_a_job_completion_keeps_both_compact() {
    const ONE: &str = "[job] a job you backgrounded has ended:\n  - `build · j7` exited 0 after \
                       4.4s, wrote 0 bytes: cargo build\nOutput: `job_output` \
                       job=\"j7\".";
    let mut a = with_notice(&format!("{NOTICE}\n\n{ONE}"));
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    assert_eq!(
        rows_with(&screen, "‼ main RED at 60bf819c").len(),
        1,
        "{joined}"
    );
    assert_eq!(
        rows_with(&screen, "j7 · build · exited 0 4.4s").len(),
        1,
        "{joined}"
    );
    assert!(!joined.contains(TO_THE_MODEL), "{joined}");
    assert_eq!(a.notice_rows.len(), 3, "{:?}", a.notice_rows);
}

/// What it does not know, it does not fold - drawn as it arrived, every word on the screen,
/// and no click target.
#[test]
fn a_shape_the_parser_does_not_know_falls_back() {
    for (what, text) in [
        (
            "an opening this head was not told about",
            NOTICE.replace("EMERGENCY — ", "ALERT — "),
        ),
        (
            "a sha that is not one",
            NOTICE.replace("60bf819c3342 in", "tip-of-main in"),
        ),
        (
            "no repository",
            NOTICE.replace(" in /home/dead/Projects/firecode is RED", " is RED"),
        ),
    ] {
        assert!(notice_tip(&text).is_none(), "{what} parsed");
        let mut a = with_notice(&text);
        let screen = a.screen(200, 60);
        let joined = screen.join("\n");
        // Not read as red - no `‼` guessed - and still ONE line: the generic
        // first clause, the whole notice a click away.
        assert!(!joined.contains("‼"), "{what}: {joined}");
        assert_eq!(
            rows_with(&screen, "session · notice · ").len(),
            1,
            "{what}: {joined}"
        );
        assert_eq!(a.notice_rows.len(), 1, "{what}");
        assert!(
            holds(&newest_notice(&mut a), "its own merge gate fails there"),
            "{what}"
        );
    }
    for (what, detail) in [
        (
            "a warning opening this head was not told about",
            WARNING.replace("EMERGENCY: ", "URGENT: "),
        ),
        (
            "a reviewer's ending this head was not told about",
            "EMERGENCY: gk finds `main` broken at 60bf819c3342: it is. Something new.".into(),
        ),
    ] {
        assert!(warning_tip(&detail).is_none(), "{what} parsed");
        // The table refuses the shape, so `/notes` draws the generic line for it - and it is
        // still not a row of the conversation, because the model got this event too.
        assert!(
            crate::ui::transcript::line_cards::warning_card("target_red", &detail).is_none(),
            "{what}: the table read a shape it does not know"
        );
        let mut a = with_warning("target_red", &detail);
        let screen = a.screen(200, 60);
        let joined = screen.join("\n");
        assert!(!joined.contains("‼"), "{what}: {joined}");
        assert!(
            !a.hist_lines.join("\n").contains(&detail),
            "{what}: it drew a row: {joined}"
        );
    }
    // Another code with the same words is not this card - and it is not a row either.
    let mut a = with_warning("queue_blocked", WARNING);
    let screen = a.screen(200, 60);
    let joined = screen.join("\n");
    assert_eq!(
        rows_with(&screen, "! queue_blocked · ").len(),
        0,
        "{joined}"
    );
    assert!(!joined.contains("‼"), "{joined}");
}

/// **A gate killed at its memory cap is UNJUDGED, not red - one line, from the session notice
/// and from the `gate_killed` warning alike**, and the whole text one click away.
#[test]
fn a_gate_killed_at_its_cap_is_one_unjudged_line() {
    let line = "‼ main UNJUDGED at 9562d70c · letibot · gate killed at its memory cap · click for \
                details";
    let n = notice_tip(KILLED_NOTICE).expect("the killed notice parses");
    assert_eq!(n.trouble, Trouble::Killed);
    assert_eq!(n.line(), line);
    let w = unjudged_warning_tip(KILLED_WARNING).expect("the gate_killed warning parses");
    assert_eq!(w.trouble, Trouble::Killed);
    assert_eq!(w.line(), line);

    let mut a = with_notice(KILLED_NOTICE);
    let screen = a.screen(200, 40);
    assert_eq!(rows_with(&screen, line).len(), 1, "{}", screen.join("\n"));
    assert!(!screen.join("\n").contains("RED at"), "never drawn as red");

    let mut a = with_warning("gate_killed", KILLED_WARNING);
    let screen = a.screen(200, 40);
    // Not a row of the conversation: the root session's notice is the row for this event, and
    // the table reads the warning's shape for `/notes`.
    assert!(
        !screen.join("\n").contains(line),
        "the warning drew a row of its own: {}",
        screen.join("\n")
    );
    assert_eq!(a.card_rows.len(), 0, "{:?}", a.card_rows);
    let detail = a
        .notes
        .iter()
        .find_map(|(_, n)| match n {
            Note::Warned(w) if w.code == "gate_killed" => Some(w.detail.clone()),
            _ => None,
        })
        .expect("the warning is held beside the conversation");
    for needle in [
        "peak 24.1 GiB of a 24.9 GiB cap",
        "the tip is UNJUDGED, not red",
    ] {
        assert!(holds(&detail, needle), "the record lost `{needle}`");
    }
}

/// A `gate_killed` warning in a shape this head does not know is not read as unjudged - and, like
/// every twin, it is not a row of the conversation at all: `/notes` holds the daemon's sentence.
#[test]
fn an_unknown_gate_killed_warning_falls_back() {
    let mut a = with_warning("gate_killed", "the gate was killed, somehow");
    let screen = a.screen(200, 30);
    let joined = screen.join("\n");
    assert_eq!(a.card_rows.len(), 0, "{joined}");
    assert!(!joined.contains("UNJUDGED"), "{joined}");
    // Not a row: the sentence is on the status line (a `Failure` is said as it lands) and in
    // `/notes`, and the conversation has no row for it.
    assert!(
        !a.hist_lines
            .join("\n")
            .contains("the gate was killed, somehow"),
        "the warning drew a row: {joined}"
    );
    assert!(
        a.notes.iter().any(|(place, n)| {
            matches!(n, Note::Warned(w) if w.code == "gate_killed") && *place == Placed::Before
        }),
        "and it is held beside the conversation: {:?}",
        a.notes
    );
}

/// One event, one row - and the row is the NOTICE's: the `target_red` warning twins it, so it is
/// filed beside the conversation and the notice draws the red card.
#[test]
fn a_red_warning_and_its_emergency_notice_are_one_row() {
    let mut a = with_notice(NOTICE);
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::Warning {
            code: "target_red".into(),
            detail: WARNING.into(),
            compaction: None,
        },
    )));
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    // ONE row, and it is the notice's: the warning is filed beside the conversation, so the
    // event is on the screen exactly as often as the notice draws it - once.
    assert_eq!(a.notice_rows.len(), 1, "{joined}");
    assert_eq!(a.card_rows.len(), 0, "{joined}");
    assert_eq!(
        a.hist_lines
            .iter()
            .filter(|l| l.contains("‼ main RED at 60bf819c"))
            .count(),
        1,
        "the event is on the screen once: {}",
        a.hist_lines.join("\n")
    );
    let (y, _) = a.notice_rows[0];
    a.key(Key::Click { x: 10, y: y as u16 });
    let whole = popup_text(&mut a);
    let first = NOTICE.lines().next().unwrap();
    assert!(holds(&whole, first), "the notice is in the popup: {whole}");
}

/// The head splits a row of notices after the closing words the notice carries: the one
/// `mergequeue::notices` writes today, and the longer one rows already in a store end with. Keyed
/// on one closing only, a reworded tail swallowed the notice after it into the red card.
#[test]
fn a_notice_splits_after_the_closing_it_was_written_with() {
    use crate::ui::transcript::line_cards::split_card;
    let next = "[job] a job you backgrounded has ended:";
    let rest_of = |text: String| split_card(&text).map(|(_, rest, _)| rest.to_string());
    assert_eq!(
        rest_of(format!("{NOTICE}\n\n{next}")).as_deref(),
        Some(next),
        "the closing the queue writes now ends the notice"
    );
    let stored = NOTICE.replace(
        "and regate once the cause is gone.",
        "say so: `\"propose_target\"` puts it to the operator, whose decision it is.",
    );
    assert_eq!(
        rest_of(format!("{stored}\n\n{next}")).as_deref(),
        Some(next),
        "and so does the one a stored row ends with"
    );
}
