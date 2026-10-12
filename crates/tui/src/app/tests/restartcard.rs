//! **A restart and a stale prompt, one line each, and the whole of each one click away.**
//!
//! The operator, pasting what pg-noop's head drew after its restart onto a new build — a
//! `prefix_stale` note, a `restart_restored` note and the session's `[restart] …` notice, each a
//! wrapped paragraph: *"these gray walls are hilarious … make them the usual one liners"*. See
//! `ui::transcript::restart_card` and `ui::transcript::stale_card`.

use super::redcard::{holds, popup_text, rows_with, with_notice, with_warning};
use super::*;
use crate::ui::transcript::line_cards::warning_card;
use crate::ui::transcript::restart_card::{Down, notice_restart, warning_restart};
use crate::ui::transcript::stale_card::stale_of;

// Every fixture but the two `pasted` ones is what `harnessd` writes — pinned byte for byte by
// `harness::tests::card_fixtures`.

/// **The operator's restart, as the daemon writes it now**: the old daemon kept no record, one
/// job reminded.
const UNKNOWN_NOTICE: &str = include_str!("fixtures/restart_unknown_notice.txt");
const UNKNOWN_WARNING: &str = include_str!("fixtures/restart_unknown_warning.txt");
const RESTORED_NOTICE: &str = include_str!("fixtures/restart_restored_notice.txt");
const RESTORED_WARNING: &str = include_str!("fixtures/restart_restored_warning.txt");
const LATE_NOTICE: &str = include_str!("fixtures/restart_late_notice.txt");
const LATE_WARNING: &str = include_str!("fixtures/restart_late_warning.txt");
const JOBS_ONLY_NOTICE: &str = include_str!("fixtures/restart_jobs_only_notice.txt");
const JOBS_ONLY_WARNING: &str = include_str!("fixtures/restart_jobs_only_warning.txt");
/// **The operator's paste, as the daemon before this build wrote it** — *"down for 0s …
/// estimated from nothing (no record said when)"*. A head attached to an older daemon still
/// reads it; not pinned, because no daemon from this build writes it.
const PASTED_NOTICE: &str = include_str!("fixtures/restart_pasted_notice.txt");
const PASTED_WARNING: &str = include_str!("fixtures/restart_pasted_warning.txt");
/// The `prefix_stale` sentence pg-noop's head drew as a wall.
const STALE: &str = include_str!("fixtures/prefix_stale_warning.txt");
const STALE_MANY: &str = include_str!("fixtures/prefix_stale_many_warning.txt");

/// **Every restart text is one line, the same for its warning and its notice.**
#[test]
fn every_restart_reads_as_its_one_line() {
    for (what, code, warning, notice, line) in [
        (
            "the operator's: no record of the stop",
            "restart_reminded",
            UNKNOWN_WARNING,
            UNKNOWN_NOTICE,
            "restarted · stop time unknown · reminded: 1 job · click for details",
        ),
        (
            "under the threshold, things brought back",
            "restart_restored",
            RESTORED_WARNING,
            RESTORED_NOTICE,
            "restarted · down 4m12s · restored: 2 subagents, 1 monitor · reminded: 1 job · click \
             for details",
        ),
        (
            "over the threshold, after a crash",
            "restart_reminded",
            LATE_WARNING,
            LATE_NOTICE,
            "restarted · down ~3h12m · reminded: 1 subagent, 1 monitor, 1 job · click for details",
        ),
        (
            "under the threshold, only a job",
            "restart_reminded",
            JOBS_ONLY_WARNING,
            JOBS_ONLY_NOTICE,
            "restarted · down 4s · reminded: 1 job · click for details",
        ),
        (
            "the operator's paste, from the build before",
            "restart_restored",
            PASTED_WARNING,
            PASTED_NOTICE,
            "restarted · stop time unknown · reminded: 1 job · click for details",
        ),
    ] {
        let w = warning_restart(warning).unwrap_or_else(|| panic!("{what}: the warning parses"));
        assert_eq!(w.line(), line, "{what}: warning");
        let n = notice_restart(notice).unwrap_or_else(|| panic!("{what}: the notice parses"));
        assert_eq!(n.line(), line, "{what}: notice");
        assert_eq!(
            warning_card(code, warning).map(|c| c.line).as_deref(),
            Some(line),
            "{what}: the registry reads it under `{code}`"
        );
    }
    assert_eq!(
        warning_restart(UNKNOWN_WARNING).unwrap().down,
        Down::Unknown
    );
}

/// **The warning is ONE note line** — none of the header's prose, no `/notes` seam — and a click
/// opens the detail whole.
#[test]
fn the_restart_warning_is_one_line_and_opens_whole() {
    let mut a = with_warning("restart_restored", PASTED_WARNING);
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    let line = "restarted · stop time unknown · reminded: 1 job · click for details";
    assert_eq!(rows_with(&screen, line).len(), 1, "{joined}");
    assert!(!joined.contains("estimated from nothing"), "{joined}");
    assert!(!joined.contains("/notes"), "no `+N lines` seam: {joined}");
    assert_eq!(a.card_rows.len(), 1, "{:?}", a.card_rows);
    let (y, _) = a.card_rows[0];
    assert_eq!(a.key(Key::Click { x: 10, y: y as u16 }), None);
    let whole = popup_text(&mut a);
    for needle in [
        "[restart] the daemon was down for 0s",
        "estimated from nothing (no record said when)",
        "- job `j46`: reminded (not re-run)",
    ] {
        assert!(holds(&whole, needle), "the popup lost `{needle}`: {whole}");
    }
    assert_eq!(a.key(Key::Esc), None);
    assert!(a.diff_popup.is_none());

    let mut a = with_warning("restart_restored", RESTORED_WARNING);
    let screen = a.screen(200, 40);
    assert_eq!(
        rows_with(
            &screen,
            "restarted · down 4m12s · restored: 2 subagents, 1 monitor · reminded: 1 job"
        )
        .len(),
        1,
        "{}",
        screen.join("\n")
    );
}

/// **The session notice the model got is ONE `session ·` line** — not the job's command, not
/// its explanation — and a click opens all of it.
#[test]
fn the_restart_notice_is_one_session_line_and_opens_whole() {
    for (notice, line) in [
        (
            PASTED_NOTICE,
            "session · restarted · stop time unknown · reminded: 1 job · click for details",
        ),
        (
            UNKNOWN_NOTICE,
            "session · restarted · stop time unknown · reminded: 1 job · click for details",
        ),
        (
            RESTORED_NOTICE,
            "session · restarted · down 4m12s · restored: 2 subagents, 1 monitor · reminded: 1 \
             job · click for details",
        ),
    ] {
        let mut a = with_notice(notice);
        let screen = a.screen(200, 40);
        let joined = screen.join("\n");
        assert_eq!(rows_with(&screen, line).len(), 1, "{joined}");
        assert!(!joined.contains("run_acceptance"), "{joined}");
        assert!(!joined.contains("cargo build"), "{joined}");
        assert_eq!(a.notice_rows.len(), 1, "{:?}", a.notice_rows);
        let (y, _) = a.notice_rows[0];
        assert_eq!(a.key(Key::Click { x: 10, y: y as u16 }), None);
        let whole = popup_text(&mut a);
        let first = notice.lines().next().unwrap();
        assert!(holds(&whole, first), "the popup lost the header: {whole}");
        assert!(holds(&whole, "To run it again: `bash {"), "{whole}");
    }
}

/// **A narrow screen cuts the line** — it never wraps.
#[test]
fn a_narrow_screen_cuts_the_restart_line() {
    let mut a = with_warning("restart_restored", RESTORED_WARNING);
    let screen = a.screen(50, 30);
    let rows = rows_with(&screen, "restarted · down 4m12s");
    assert_eq!(rows.len(), 1, "{}", screen.join("\n"));
    assert!(rows[0].contains('…'), "{rows:?}");
    for l in &screen {
        assert!(visible_width(l) <= 50, "{l:?}");
    }
}

/// **The stale prompt's note is a pointer** — what changed, and the key that shows the card.
#[test]
fn the_stale_prompt_note_is_one_line_pointing_at_the_card() {
    let s = stale_of(STALE).expect("the operator's sentence parses");
    assert_eq!(s.session, "s-1789919514688401228");
    assert_eq!(
        s.line(),
        "prompt stale · seated 09-20 17:51 · new: task_resume · alt-p for the card"
    );
    assert_eq!(
        stale_of(STALE_MANY).expect("parses").line(),
        "prompt stale · seated 09-20 17:51 · instructions changed · new: task_resume, pkill · \
         gone: job_wait · changed: todo_write · alt-p for the card"
    );
    let instructions_only = STALE.replace("the tool schemas have", "the system instructions have");
    let instructions_only = instructions_only.replace(
        ". Seated here but not in this conversation's prompt, so the model cannot call them: \
         task_resume",
        "",
    );
    assert_eq!(
        stale_of(&instructions_only).expect("parses").line(),
        "prompt stale · seated 09-20 17:51 · instructions changed · alt-p for the card"
    );

    let mut a = with_warning("prefix_stale", STALE);
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    let line = "prompt stale · seated 09-20 17:51 · new: task_resume · alt-p for the card";
    assert_eq!(rows_with(&screen, line).len(), 1, "{joined}");
    assert!(!joined.contains("message 0 is frozen"), "{joined}");
    assert!(!joined.contains("+1 lines"), "{joined}");
    assert_eq!(a.card_rows.len(), 1, "{:?}", a.card_rows);
    let (y, _) = a.card_rows[0];
    assert_eq!(a.key(Key::Click { x: 10, y: y as u16 }), None);
    let whole = popup_text(&mut a);
    for needle in [
        "session s-1789919514688401228 was seated 09-20 17:51",
        "so the model cannot call them: task_resume",
        "`/reseat` rebuilds the prompt",
    ] {
        assert!(holds(&whole, needle), "the popup lost `{needle}`: {whole}");
    }
}

/// **What it does not know, it does not fold** — drawn as it always was, and no click target.
#[test]
fn a_restart_or_stale_shape_this_head_does_not_know_falls_back() {
    for (code, detail) in [
        (
            "restart_restored",
            "[restart] the daemon came back somehow\n  - job `j1`: reminded (not re-run)",
        ),
        ("restart_reminded", "the daemon restarted; nothing to say"),
        // A header with nothing under it this head can count.
        (
            "restart_restored",
            "[restart] the daemon was down for 4s (stopped at x; back at y) — under it:",
        ),
        ("prefix_stale", "this session's prompt is stale, somehow"),
        (
            "prefix_stale",
            "session s was seated 09-20 17:51 with a different prompt",
        ),
    ] {
        assert!(warning_card(code, detail).is_none(), "{code}: {detail}");
        let mut a = with_warning(code, detail);
        let joined = a.screen(200, 40).join("\n");
        assert!(joined.contains(code), "drawn as a note: {joined}");
        assert!(a.card_rows.is_empty(), "{code}");
    }
    // The same words under another code are not these cards.
    assert!(warning_card("compacted", RESTORED_WARNING).is_none());
    assert!(warning_card("reseated", STALE).is_none());
    // A session row that opens with some other `[restart]` sentence is drawn as before.
    let mut a = with_notice(
        "[restart] subagent `s-1` (x) was running when the daemon restarted and has been resumed.",
    );
    let joined = a.screen(200, 40).join("\n");
    assert!(joined.contains("has been resumed"), "{joined}");
    assert!(a.notice_rows.is_empty(), "{:?}", a.notice_rows);
}

/// **A restart notice beside a job's completion in one wake** is one line and the job's.
#[test]
fn a_restart_notice_beside_a_job_completion_keeps_both_compact() {
    const ONE: &str = "[job] a job you backgrounded has ended:\n  - `build · j7` exited 0 after \
                       4.4s, wrote 0 bytes: cargo build\nThis is the completion arriving on its \
                       own — you do not need to wait for it, and `job_wait` would only block you \
                       for a result you already have. Read what it wrote with `job_output` \
                       (job=\"…\"), then carry on with what you were doing.";
    let mut a = with_notice(&format!("{RESTORED_NOTICE}\n\n{ONE}"));
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    assert_eq!(
        rows_with(&screen, "session · restarted · down 4m12s").len(),
        1,
        "{joined}"
    );
    assert_eq!(
        rows_with(&screen, "j7 · build · exited 0 4.4s").len(),
        1,
        "{joined}"
    );
    assert!(!joined.contains("Their wait is RELEASED"), "{joined}");
}
