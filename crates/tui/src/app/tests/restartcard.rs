//! A restart and a stale prompt: one line each, and the whole of each one click away in a popup.
//! See `ui::transcript::restart_card` and `ui::transcript::stale_card`.

use super::redcard::{holds, popup_text, rows_with, with_notice, with_warning};
use super::*;
use crate::ui::transcript::line_cards::warning_card;
use crate::ui::transcript::restart_card::{Down, notice_restart, warning_restart};
use crate::ui::transcript::stale_card::stale_of;

// Every fixture but the two `pasted` ones is what `harnessd` writes - pinned byte for byte by
// `harness::tests::card_fixtures`.

/// The operator's restart, as the daemon writes it now: the old daemon kept no record, one
/// job reminded.
const UNKNOWN_NOTICE: &str = include_str!("fixtures/restart_unknown_notice.txt");
const UNKNOWN_WARNING: &str = include_str!("fixtures/restart_unknown_warning.txt");
const RESTORED_NOTICE: &str = include_str!("fixtures/restart_restored_notice.txt");
const RESTORED_WARNING: &str = include_str!("fixtures/restart_restored_warning.txt");
const LATE_NOTICE: &str = include_str!("fixtures/restart_late_notice.txt");
const LATE_WARNING: &str = include_str!("fixtures/restart_late_warning.txt");
const JOBS_ONLY_NOTICE: &str = include_str!("fixtures/restart_jobs_only_notice.txt");
const JOBS_ONLY_WARNING: &str = include_str!("fixtures/restart_jobs_only_warning.txt");
/// A restart with nothing to restore - the daemon came back, says when, and nothing was
/// running. Its own code (`restart_nothing`) because it is its own sentence.
const NOTHING_NOTICE: &str = include_str!("fixtures/restart_nothing_notice.txt");
const NOTHING_WARNING: &str = include_str!("fixtures/restart_nothing_warning.txt");
/// Nothing running, but a turn owed: the promise alone, and every clause `Owed` can say.
const OWED_PROMISE_NOTICE: &str = include_str!("fixtures/restart_owed_promise_notice.txt");
const OWED_WORK_NOTICE: &str = include_str!("fixtures/restart_owed_work_notice.txt");
/// The operator's paste, as the daemon before this build wrote it - *"down for 0s …
/// estimated from nothing (no record said when)"*. A head attached to an older daemon still
/// reads it; not pinned, because no daemon from this build writes it.
const PASTED_NOTICE: &str = include_str!("fixtures/restart_pasted_notice.txt");
const PASTED_WARNING: &str = include_str!("fixtures/restart_pasted_warning.txt");
/// A real restart, as its store and its daemon log hold it - the notice the
/// model got (the restart's header and the `[task]` group of the twelve children it handed over)
/// and the warning the daemon raised. Not pinned: real text, from the build before this change.
const DELIVERED_NOTICE: &str = include_str!("fixtures/restart_with_deliveries_notice.txt");
const DELIVERED_WARNING: &str = include_str!("fixtures/restart_with_deliveries_warning.txt");
/// The `prefix_stale` sentence pg-noop's head drew as a wall.
const STALE: &str = include_str!("fixtures/prefix_stale_warning.txt");
const STALE_MANY: &str = include_str!("fixtures/prefix_stale_many_warning.txt");

/// Every restart text is one line, the same for its warning and its notice.
#[test]
fn every_restart_reads_as_its_one_line() {
    for (what, code, warning, notice, line) in [
        (
            "the operator's: no record of the stop",
            "restart_reminded",
            UNKNOWN_WARNING,
            UNKNOWN_NOTICE,
            "crashed, stop time unknown · reminded: 1 job · click for details",
        ),
        (
            "under the threshold, things brought back",
            "restart_restored",
            RESTORED_WARNING,
            RESTORED_NOTICE,
            "restarted, stopped by operator · down 4m12s · restored: 2 subagents, 1 monitor · \
             reminded: 1 job · click for details",
        ),
        (
            "over the threshold, after a crash",
            "restart_reminded",
            LATE_WARNING,
            LATE_NOTICE,
            "crashed, down ~3h12m · reminded: 1 subagent, 1 monitor, 1 job · click for details",
        ),
        (
            "under the threshold, only a job",
            "restart_reminded",
            JOBS_ONLY_WARNING,
            JOBS_ONLY_NOTICE,
            "restarted, stopped by operator · down 4s · reminded: 1 job · click for details",
        ),
        (
            "the operator's paste, from the build before",
            "restart_restored",
            PASTED_WARNING,
            PASTED_NOTICE,
            "crashed, stop time unknown · reminded: 1 job · click for details",
        ),
        (
            "a restart with nothing to restore",
            "restart_nothing",
            NOTHING_WARNING,
            NOTHING_NOTICE,
            "restarted, stopped by operator · down 4s · click for details",
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

/// The store knows an orderly stop from a crash and the card follows it: a restart after
/// `letibot --stop` is news drawn in no alarm register, and only one nobody asked for needs a
/// person. Both halves of one event - the hub warning and the session notice - say the same.
#[test]
fn an_orderly_restart_is_news_and_only_a_crash_is_attention() {
    use crate::ui::transcript::line_cards::session_card;
    use rano::style::Role;
    for (what, code, warning, notice) in [
        (
            "things brought back",
            "restart_restored",
            RESTORED_WARNING,
            RESTORED_NOTICE,
        ),
        (
            "only a job",
            "restart_reminded",
            JOBS_ONLY_WARNING,
            JOBS_ONLY_NOTICE,
        ),
        (
            "nothing running",
            "restart_nothing",
            NOTHING_WARNING,
            NOTHING_NOTICE,
        ),
        (
            "deliveries handed over",
            "restart_restored",
            DELIVERED_WARNING,
            DELIVERED_NOTICE,
        ),
    ] {
        let card = warning_card(code, warning).unwrap_or_else(|| panic!("{what}: no card"));
        assert_eq!(card.role, Role::Info, "{what}: the warning's register");
        assert!(
            card.line.starts_with("restarted, stopped by operator"),
            "{what}: {}",
            card.line
        );
        let said = session_card(notice).unwrap_or_else(|| panic!("{what}: no session card"));
        assert_eq!(said.role, Role::Info, "{what}: the notice's register");
        assert_ne!(
            said.role,
            Role::Attention,
            "{what}: the operator's own restart"
        );
    }
    // A daemon that went down without recording its stop: an estimate from a heartbeat, or no
    // record at all. That is the one a person is asked to look at.
    for (what, code, warning, notice) in [
        (
            "an estimate from a heartbeat",
            "restart_reminded",
            LATE_WARNING,
            LATE_NOTICE,
        ),
        (
            "no record of the stop",
            "restart_reminded",
            UNKNOWN_WARNING,
            UNKNOWN_NOTICE,
        ),
        (
            "the paste of an older build",
            "restart_restored",
            PASTED_WARNING,
            PASTED_NOTICE,
        ),
    ] {
        let card = warning_card(code, warning).unwrap_or_else(|| panic!("{what}: no card"));
        assert_eq!(card.role, Role::Attention, "{what}: the warning's register");
        let said = session_card(notice).unwrap_or_else(|| panic!("{what}: no session card"));
        assert_eq!(said.role, Role::Attention, "{what}: the notice's register");
        assert!(said.line.starts_with("crashed, "), "{what}: {}", said.line);
    }
}

/// A restart's warning is no longer a row of the conversation: the model got the `[restart]`
/// notice, and that is the line. The shape the warning carries is still read - `/notes` opens it -
/// so its line is asked of the table, and the screen is asked for nothing.
#[test]
fn the_restart_warning_reads_as_one_line_and_is_no_row() {
    for (code, detail, line) in [
        (
            "restart_restored",
            PASTED_WARNING,
            "crashed, stop time unknown · reminded: 1 job · click for details",
        ),
        (
            "restart_restored",
            RESTORED_WARNING,
            "restarted, stopped by operator · down 4m12s · restored: 2 subagents, 1 monitor · \
             reminded: 1 job · click for details",
        ),
    ] {
        assert_eq!(
            warning_card(code, detail).map(|c| c.line).as_deref(),
            Some(line),
            "the table stopped reading the warning's shape"
        );
        let mut a = with_warning(code, detail);
        let joined = a.screen(200, 40).join("\n");
        assert!(!joined.contains(line), "the warning drew a row\n{joined}");
        assert_eq!(a.card_rows.len(), 0, "{joined}");
        assert!(
            a.notes
                .iter()
                .any(|(place, n)| matches!(n, Note::Warned(w) if w.code == code)
                    && *place == Placed::Before),
            "`{code}` left no record beside the conversation: {:?}",
            a.notes
        );
    }
}

/// The session notice the model got is ONE `session ·` line - not the job's command, not
/// its explanation - and a click opens all of it.
#[test]
fn the_restart_notice_is_one_session_line_and_opens_whole() {
    for (notice, line) in [
        (
            PASTED_NOTICE,
            "session · crashed, stop time unknown · reminded: 1 job · click for details",
        ),
        (
            UNKNOWN_NOTICE,
            "session · crashed, stop time unknown · reminded: 1 job · click for details",
        ),
        (
            RESTORED_NOTICE,
            "session · restarted, stopped by operator · down 4m12s · restored: 2 subagents, 1 \
             monitor · reminded: 1 job · click for details",
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

/// The empty restart is one `session ·` line too, and the colon in its sentence does not end
/// the notice: the card splits a row only where another notice follows, and nothing follows this.
#[test]
fn an_empty_restart_notice_is_one_session_line() {
    let mut a = with_notice(NOTHING_NOTICE);
    a.visibility = Visibility::of(Profile::LOUD);
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    assert_eq!(
        rows_with(
            &screen,
            "session · restarted, stopped by operator · down 4s · click for details"
        )
        .len(),
        1,
        "{joined}"
    );
    assert_eq!(a.notice_rows.len(), 1, "{:?}", a.notice_rows);
    let (y, _) = a.notice_rows[0];
    assert_eq!(a.key(Key::Click { x: 10, y: y as u16 }), None);
    let whole = popup_text(&mut a);
    assert!(
        holds(&whole, NOTHING_NOTICE),
        "the popup lost the sentence: {whole}"
    );
}

/// A narrow screen cuts the line - it never wraps.
#[test]
fn a_narrow_screen_cuts_the_restart_line() {
    let mut a = with_notice(RESTORED_NOTICE);
    let screen = a.screen(50, 30);
    let rows = rows_with(&screen, "restarted, stopped by operator");
    assert_eq!(rows.len(), 1, "{}", screen.join("\n"));
    assert!(rows[0].contains('…'), "{rows:?}");
    for l in &screen {
        assert!(visible_width(l) <= 50, "{l:?}");
    }
}

/// The stale prompt's note is a pointer - what changed, and the key that shows the card.
#[test]
fn the_stale_prompt_note_is_one_line_pointing_at_the_card() {
    let s = stale_of(STALE).expect("the operator's sentence parses");
    assert_eq!(s.session, "s-1789919514688401228");
    assert_eq!(
        s.line(),
        "prompt stale · seated 09-20 17:51 · new: task_resume · click for details"
    );
    assert_eq!(
        stale_of(STALE_MANY).expect("parses").line(),
        "prompt stale · seated 09-20 17:51 · instructions changed · new: task_resume, pkill · \
         gone: job_wait · changed: todo_write · click for details"
    );
    let instructions_only = STALE.replace("the tool schemas have", "the system instructions have");
    let instructions_only = instructions_only.replace(
        ". Seated here but not in this conversation's prompt, so the model cannot call them: \
         task_resume",
        "",
    );
    assert_eq!(
        stale_of(&instructions_only).expect("parses").line(),
        "prompt stale · seated 09-20 17:51 · instructions changed · click for details"
    );

    let mut a = with_warning("prefix_stale", STALE);
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    let line = "prompt stale · seated 09-20 17:51 · new: task_resume · click for details";
    // Not a row: the stale prompt is a note and the reseat card is the reader's move.
    assert!(!joined.contains(line), "the note drew a row: {joined}");
    assert_eq!(a.card_rows.len(), 0, "{:?}", a.card_rows);
    assert!(
        crate::ui::transcript::line_cards::warning_card("prefix_stale", STALE)
            .is_some_and(|c| c.line == line),
        "the table stopped reading the sentence"
    );
    let held = a
        .notes
        .iter()
        .find_map(|(_, n)| match n {
            Note::Warned(w) if w.code == "prefix_stale" => Some(w.detail.clone()),
            _ => None,
        })
        .expect("held in /notes");
    for needle in [
        "session s-1789919514688401228 was seated 09-20 17:51",
        "so the model cannot call them: task_resume",
        "`/reseat` rebuilds the prompt",
    ] {
        assert!(holds(&held, needle), "the note lost `{needle}`: {held}");
    }
    assert!(a.reseat.is_some(), "and the card is up");
}

/// What it does not know, it does not read - and the note is still ONE line: the
/// generic `<mark> <code> · <first clause>`, the detail a click away. Nothing is guessed.
#[test]
fn a_restart_or_stale_shape_this_head_does_not_know_falls_back() {
    // The codes whose shape no reader knows, and which the head still DRAWS: a twin
    // (`restart_*`) is filed beside the conversation instead, and that is asserted below.
    for (code, detail) in [
        ("prefix_stale", "this session's prompt is stale, somehow"),
        (
            "prefix_stale",
            "session s was seated 09-20 17:51 with a different prompt",
        ),
    ] {
        assert!(warning_card(code, detail).is_none(), "{code}: {detail}");
        let mut a = with_warning(code, detail);
        let screen = a.screen(200, 40);
        let joined = screen.join("\n");
        // No generic line either: no warning is a row of the conversation, whatever its shape.
        assert!(
            !screen.iter().any(|l| l.contains(&format!("· {code} · "))),
            "drew a row: {joined}"
        );
        assert_eq!(a.card_rows.len(), 0, "{code}");
        assert!(
            a.notes
                .iter()
                .any(|(_, n)| matches!(n, Note::Warned(w) if w.code == code && w.detail == detail)),
            "{code} left no record: {:?}",
            a.notes
        );
    }
    // A restart's shape this head does not know is not drawn at all: the `[restart]` notice the
    // model got is the row, and the warning is filed beside it.
    for (code, detail) in [
        (
            "restart_restored",
            "[restart] the daemon came back somehow\n  - job `j1`: reminded (not re-run)",
        ),
        ("restart_reminded", "the daemon restarted; nothing to say"),
        (
            "restart_restored",
            "[restart] the daemon was down for 4s (stopped at x; back at y) — under it:",
        ),
    ] {
        assert!(warning_card(code, detail).is_none(), "{code}: {detail}");
        let mut a = with_warning(code, detail);
        let screen = a.screen(200, 40);
        assert_eq!(a.card_rows.len(), 0, "{code}: {}", screen.join("\n"));
        assert!(
            a.notes.iter().any(|(place, n)| {
                matches!(n, Note::Warned(w) if w.code == code) && *place == Placed::Before
            }),
            "{code} left no record: {:?}",
            a.notes
        );
    }
    // The same words under another code are not these cards.
    assert!(warning_card("compacted", RESTORED_WARNING).is_none());
    assert!(warning_card("reseated", STALE).is_none());
    // A session row that opens with some other `[restart]` sentence is the generic line.
    let mut a = with_notice(
        "[restart] subagent `s-1` (x) was running when the daemon restarted and has been resumed.",
    );
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    assert_eq!(
        rows_with(&screen, "session · restart · subagent `s-1`").len(),
        1,
        "{joined}"
    );
    assert_eq!(a.notice_rows.len(), 1, "{:?}", a.notice_rows);
}

/// A restart notice beside a job's completion in one wake is one line and the job's.
#[test]
fn a_restart_notice_beside_a_job_completion_keeps_both_compact() {
    const ONE: &str = "[job] a job you backgrounded has ended:\n  - `build · j7` exited 0 after \
                       4.4s, wrote 0 bytes: cargo build\nOutput: `job_output` \
                       job=\"j7\".";
    let mut a = with_notice(&format!("{RESTORED_NOTICE}\n\n{ONE}"));
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    assert_eq!(
        rows_with(
            &screen,
            "session · restarted, stopped by operator · down 4m12s"
        )
        .len(),
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

/// One event, one row. A restart's warning and the session's `[restart]` notice are one
/// line - the warning's card - whichever arrives first, and its popup carries the notice too.
#[test]
fn a_restart_warning_and_its_notice_are_one_line() {
    for warning_first in [false, true] {
        let mut a = if warning_first {
            let mut a = with_warning("restart_restored", RESTORED_WARNING);
            a.apply(ServerFrame::Event(env(
                2,
                SessionEvent::TranscriptAppended {
                    item_id: "s.0".into(),
                    kind: "user".into(),
                    ledger_head: String::new(),
                    item: None,
                    body_bytes: 0,
                },
            )));
            a.apply(ServerFrame::Event(env(
                3,
                SessionEvent::TranscriptContent {
                    item_id: "s.0".into(),
                    item: Box::new(TranscriptItem::User {
                        speaker: letibot_transcript::Speaker::Agent,
                        parts: vec![UserPart::Text {
                            text: RESTORED_NOTICE.into(),
                        }],
                    }),
                },
            )));
            a
        } else {
            let mut a = with_notice(RESTORED_NOTICE);
            a.apply(ServerFrame::Event(env(
                3,
                SessionEvent::Warning {
                    code: "restart_restored".into(),
                    detail: RESTORED_WARNING.into(),
                    compaction: None,
                },
            )));
            a
        };
        let screen = a.screen(200, 40);
        let joined = screen.join("\n");
        assert_eq!(
            rows_with(&screen, "restarted, stopped by operator · down 4m12s").len(),
            1,
            "warning first: {warning_first}\n{joined}"
        );
        // ONE row, and it is the notice's: the `restart_reminded` warning twins it and is filed
        // beside the conversation, whichever order the two arrive in.
        assert_eq!(a.notice_rows.len(), 1, "{:?}", a.notice_rows);
        assert_eq!(a.card_rows.len(), 0, "{:?}", a.card_rows);
        let (y, _) = a.notice_rows[0];
        a.key(Key::Click { x: 10, y: y as u16 });
        let whole = popup_text(&mut a);
        assert!(
            holds(&whole, "To run it again: `bash {"),
            "the notice is in the popup: {whole}"
        );
    }
}

/// One restart, one header. The notice the card absorbed is the warning's text in more words and opens
/// with the same header, so the popup is the notice alone: the header once, every job's line, nothing
/// lost.
#[test]
fn a_restart_card_opens_with_its_header_once() {
    for warning_first in [false, true] {
        let mut a = if warning_first {
            let mut a = with_warning("restart_reminded", UNKNOWN_WARNING);
            a.apply(ServerFrame::Event(env(
                2,
                SessionEvent::TranscriptAppended {
                    item_id: "s.0".into(),
                    kind: "user".into(),
                    ledger_head: String::new(),
                    item: None,
                    body_bytes: 0,
                },
            )));
            a.apply(ServerFrame::Event(env(
                3,
                SessionEvent::TranscriptContent {
                    item_id: "s.0".into(),
                    item: Box::new(TranscriptItem::User {
                        speaker: letibot_transcript::Speaker::Agent,
                        parts: vec![UserPart::Text {
                            text: UNKNOWN_NOTICE.into(),
                        }],
                    }),
                },
            )));
            a
        } else {
            let mut a = with_notice(UNKNOWN_NOTICE);
            a.apply(ServerFrame::Event(env(
                3,
                SessionEvent::Warning {
                    code: "restart_reminded".into(),
                    detail: UNKNOWN_WARNING.into(),
                    compaction: None,
                },
            )));
            a
        };
        let screen = a.screen(200, 40);
        assert_eq!(a.notice_rows.len(), 1, "{}", screen.join("\n"));
        assert_eq!(a.card_rows.len(), 0, "{}", screen.join("\n"));
        let (y, _) = a.notice_rows[0];
        a.key(Key::Click { x: 10, y: y as u16 });
        let whole = popup_text(&mut a);
        let flat = whole.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(
            flat.matches("[restart] The daemon restarted").count(),
            1,
            "warning first: {warning_first} — the header twice:\n{whole}"
        );
        assert!(holds(&whole, "To run it again: `bash {"), "{whole}");
        assert!(holds(&whole, "- job `j46`: NOT run again"), "{whole}");
    }
}

/// A restart and the children it handed over are ONE line. The `[task]` group is the restart's own:
/// counted on its line and in the popup, never drawn as a fold with a line per child under it.
#[test]
fn a_restart_and_the_children_it_handed_over_are_one_line() {
    let line = "restarted, stopped by operator · down 2s · restored: 12 subagents · click for \
                details";
    assert_eq!(
        warning_restart(DELIVERED_WARNING)
            .expect("the warning parses")
            .line(),
        line
    );
    assert_eq!(
        notice_restart(DELIVERED_NOTICE)
            .expect("the notice parses")
            .line(),
        line,
        "the notice counts the children it carries"
    );

    // The notice alone: one session line, no child lines, the whole of it a click away.
    let mut a = with_notice(DELIVERED_NOTICE);
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    assert_eq!(
        rows_with(&screen, &format!("session · {line}")).len(),
        1,
        "{joined}"
    );
    assert!(!joined.contains("you-are-reading-part"), "{joined}");
    assert!(!joined.contains("Agent "), "{joined}");
    assert_eq!(a.notice_rows.len(), 1, "{:?}", a.notice_rows);
    let (y, _) = a.notice_rows[0];
    a.key(Key::Click { x: 10, y: y as u16 });
    let whole = popup_text(&mut a);
    assert!(
        holds(&whole, "12 subagents you started have finished"),
        "{whole}"
    );

    // With its warning, either order: one row for the one restart.
    for warning_first in [false, true] {
        let mut a = if warning_first {
            let mut a = with_warning("restart_restored", DELIVERED_WARNING);
            a.apply(ServerFrame::Event(env(
                2,
                SessionEvent::TranscriptAppended {
                    item_id: "s.0".into(),
                    kind: "user".into(),
                    ledger_head: String::new(),
                    item: None,
                    body_bytes: 0,
                },
            )));
            a.apply(ServerFrame::Event(env(
                3,
                SessionEvent::TranscriptContent {
                    item_id: "s.0".into(),
                    item: Box::new(TranscriptItem::User {
                        speaker: letibot_transcript::Speaker::Agent,
                        parts: vec![UserPart::Text {
                            text: DELIVERED_NOTICE.into(),
                        }],
                    }),
                },
            )));
            a
        } else {
            let mut a = with_notice(DELIVERED_NOTICE);
            a.apply(ServerFrame::Event(env(
                3,
                SessionEvent::Warning {
                    code: "restart_restored".into(),
                    detail: DELIVERED_WARNING.into(),
                    compaction: None,
                },
            )));
            a
        };
        let screen = a.screen(200, 40);
        let joined = screen.join("\n");
        assert_eq!(
            rows_with(&screen, "restarted, stopped by operator · down 2s").len(),
            1,
            "warning first: {warning_first}\n{joined}"
        );
        assert!(!joined.contains("you-are-reading-part"), "{joined}");
        // One row, the notice's: the warning is filed beside the conversation.
        assert_eq!(a.notice_rows.len(), 1, "{:?}", a.notice_rows);
        assert_eq!(a.card_rows.len(), 0, "{:?}", a.card_rows);
    }
}

/// A restart carries `[task]` groups and nothing else - a `[job]` that ended in the same wake
/// is still its own two lines, and a row with one is not absorbed into the warning.
#[test]
fn a_restart_with_a_job_beside_its_deliveries_keeps_the_job() {
    const JOB: &str = "[job] a job you backgrounded has ended:\n  - `build · j7` exited 0 after \
                       4.4s, wrote 0 bytes: cargo build\nOutput: `job_output` job=\"j7\".";
    let mut a = with_notice(&format!("{DELIVERED_NOTICE}\n\n{JOB}"));
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    assert_eq!(
        rows_with(
            &screen,
            "session · restarted, stopped by operator · down 2s · restored: 12 subagents · click \
             for details"
        )
        .len(),
        1,
        "{joined}"
    );
    assert_eq!(
        rows_with(&screen, "j7 · build · exited 0 4.4s").len(),
        1,
        "{joined}"
    );
    assert!(!joined.contains("you-are-reading-part"), "{joined}");
}

/// The operator's own restart that owed nothing is drawn only at `loud`. The model still has the
/// row; a crash, an unknown stop or owed work is drawn at every rung.
#[test]
fn an_operators_restart_owing_nothing_is_drawn_only_at_loud() {
    let line = "session · restarted, stopped by operator · down 4s · click for details";
    for p in [Profile::CONVERSATION, Profile::TERSE, Profile::NORMAL] {
        let mut a = with_notice(NOTHING_NOTICE);
        a.visibility = Visibility::of(p);
        let screen = a.screen(200, 40);
        assert!(
            rows_with(&screen, "restarted").is_empty(),
            "{}: {}",
            p.name,
            screen.join("\n")
        );
    }
    let mut a = with_notice(NOTHING_NOTICE);
    a.visibility = Visibility::of(Profile::LOUD);
    assert_eq!(rows_with(&a.screen(200, 40), line).len(), 1);
    for (notice, line) in [
        (
            "[restart] The daemon restarted (down ~4s). Nothing was running.",
            "session · crashed, down ~4s · click for details",
        ),
        (
            "[restart] The daemon restarted (stop time unknown). Nothing was running.",
            "session · crashed, stop time unknown · click for details",
        ),
        (
            OWED_PROMISE_NOTICE,
            "session · restarted · carrying on: your promise · click for details",
        ),
    ] {
        let mut a = with_notice(notice);
        let screen = a.screen(200, 40);
        assert_eq!(rows_with(&screen, line).len(), 1, "{}", screen.join("\n"));
    }
}

/// A restart that owes work says what it asks, not that the operator restarted it.
#[test]
fn an_owed_restart_names_what_it_asks() {
    let r = notice_restart(OWED_WORK_NOTICE).expect("the owed notice reads");
    assert_eq!(
        r.owed.as_deref(),
        Some(
            "a turn cut off, 2 undelivered results, 1 due todo row, 2 open todo rows, your promise"
        ),
        "{OWED_WORK_NOTICE}"
    );
    assert!(
        r.line()
            .starts_with("restarted · carrying on: a turn cut off, "),
        "{}",
        r.line()
    );
    assert_eq!(notice_restart(NOTHING_NOTICE).unwrap().owed, None);
}
