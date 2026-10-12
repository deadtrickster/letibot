//! **Every notice is ONE line, and its details a click — or alt-n — away** (R89).
//!
//! The operator's standing rule: *"make them the usual one liners"*, *"one line + popup"*. These
//! are the walls the review of 2026-10-12 measured on real rows, drawn through the whole head.

use super::redcard::{holds, popup_text, rows_with, with_notice, with_warning};
use super::*;

/// How many of the frame's rows say `needle`.
fn rows<'a>(screen: &'a [String], needle: &str) -> usize {
    rows_with(screen, needle).len()
}

/// **The worst row the review found** — one `Tool call X was not executed` sentence for each of
/// 6,902 truncated calls, 13,800 screen rows — is one line, and a click opens it whole.
#[test]
fn a_length_cap_salvage_is_one_line_and_a_click_opens_it() {
    let mut text = String::from(
        "Tool call read was not executed: the response hit the output token limit, so its \
         arguments may be truncated. Re-issue the tool call with complete arguments.",
    );
    for _ in 0..40 {
        text.push_str(
            "\nTool call bash was not executed: the response hit the output token limit, so its \
             arguments may be truncated. Re-issue the tool call with complete arguments.",
        );
    }
    let mut a = with_notice(&text);
    let screen = a.screen(160, 40);
    let joined = screen.join("\n");
    assert_eq!(
        rows(
            &screen,
            "session · length cap · 41 calls not executed (bash ×40, read ×1) · click for details"
        ),
        1,
        "{joined}"
    );
    assert!(
        !joined.contains("Re-issue"),
        "the wall is on the glass: {joined}"
    );
    assert_eq!(a.notice_rows.len(), 1, "{:?}", a.notice_rows);
    let (y, _) = a.notice_rows[0];
    a.key(Key::Click { x: 10, y: y as u16 });
    let whole = popup_text(&mut a);
    assert!(holds(&whole, "Tool call read was not executed"), "{whole}");
    // **The popup's title is the card's**, not a bare `notice` in red.
    let top = a.screen(160, 40).join("\n");
    assert!(top.contains("length cap · 41 calls not executed"), "{top}");
}

/// **A plan check that does not fold, and the stale prompt's paragraph after it, are two
/// notices — two lines**, the stale one the same card the `prefix_stale` warning draws.
#[test]
fn a_plan_check_and_the_stale_paragraph_under_it_are_two_lines() {
    let text = "[todo check] this turn is finished and one item is not done (1 more open):\n  - \
                review-agent/a-pass (entry s-1): Your branch is cut from x\nthis row is the merge \
                queue's, not your plan's: it is one entry of this daemon's review queue.\n\nsession \
                s-gatekeeper was seated 10-10 22:42 with a prompt this daemon would not compose \
                now: the tool schemas have changed since. This session keeps the prefix it was \
                created with. Say this to the person in your reply.";
    let mut a = with_notice(text);
    let screen = a.screen(160, 40);
    let joined = screen.join("\n");
    assert_eq!(
        rows(
            &screen,
            "session · todo · review-agent/a-pass (entry s-1)… · queue's · 1 more open"
        ),
        1,
        "{joined}"
    );
    assert_eq!(
        rows(&screen, "prompt stale · seated 10-10 22:42"),
        1,
        "{joined}"
    );
    assert!(!joined.contains("Say this to the person"), "{joined}");
    assert_eq!(newest_notice(&mut a), text, "the popup is the row whole");
}

/// **A System row is one line** — a compaction's summary was drawn whole, ~190 screen rows of
/// the model's own record — and alt-n opens the newest notice, this one, whole.
#[test]
fn a_system_row_is_one_line_and_alt_n_opens_it() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("s#t2.0", "system"),
    )));
    let summary = format!(
        "This conversation was compacted: what was said before this point is replaced by the \
         summary below.\n\n{}",
        "THE RECORD ".repeat(400)
    );
    a.record_item(
        "s#t2.0",
        TranscriptItem::System {
            text: summary.clone(),
            origin: letibot_transcript::SystemOrigin::Update,
        },
    );
    a.visibility = Visibility::of(Profile::LOUD);
    a.invalidate_history_from(0);
    let screen = a.screen(120, 40);
    let joined = screen.join("\n");
    assert_eq!(
        rows(
            &screen,
            "session · system · compacted summary (4.5k chars) · click for details"
        ),
        1,
        "{joined}"
    );
    assert!(!joined.contains("THE RECORD"), "{joined}");
    assert_eq!(newest_notice(&mut a), summary);
}

/// **A folded `[task]` row is clickable** — eight finished children were eight lines with no
/// way to the notice behind them.
#[test]
fn a_folded_task_row_opens_its_notice() {
    let text = "[task] a subagent you started has finished:\n  - `s-1-sub-2` done: all green.\n\
                This is the completion arriving on its own — you do not need to wait for it.";
    let mut a = with_notice(text);
    let screen = a.screen(120, 30);
    assert_eq!(
        rows(&screen, "Agent s-1-sub-2 · done: all green."),
        1,
        "{}",
        screen.join("\n")
    );
    assert_eq!(a.notice_rows.len(), 1, "{:?}", a.notice_rows);
    assert_eq!(newest_notice(&mut a), text);
}

/// **A `[job]` and a `[monitor]` in one wake keep the job's two lines and give the monitor
/// one** — the monitor was not folded, so the WHOLE row fell to raw.
#[test]
fn a_job_beside_a_monitor_keeps_both_compact() {
    let text = "[job] a job you backgrounded has ended:\n  - `build · j7` exited 0 after 4.4s, \
                wrote 0 bytes: cargo build\nThis is the completion arriving on its own — you do \
                not need to wait for it, and `job_wait` would only block you for a result you \
                already have.\n\n[monitor] 1 watch(es) fired:\n  - `ci` (command `gh run watch` \
                exiting), declared by turn s#t8#226: FIRED after 0.3s — the command exited 2\n\
                Only a FIRED watch is an answer about the world.";
    let mut a = with_notice(text);
    let screen = a.screen(160, 30);
    let joined = screen.join("\n");
    assert_eq!(rows(&screen, "j7 · build · exited 0 4.4s"), 1, "{joined}");
    assert_eq!(
        rows(&screen, "monitor · ci FIRED after 0.3s"),
        1,
        "{joined}"
    );
    assert!(!joined.contains("Only a FIRED watch"), "{joined}");
}

/// **A warning no card reads is one line in its register, clickable, its code said once** — it
/// was three wrapped lines and `… +N lines · /notes` that nothing could open.
#[test]
fn a_warning_no_card_reads_is_one_clickable_line() {
    let detail = "`deadtrickster` asked this daemon to stop. Every head detaches, the socket \
                  goes, and nothing new is started. Runs in flight are ended.";
    let mut a = with_warning("daemon_stopping", detail);
    let screen = a.screen(160, 30);
    let joined = screen.join("\n");
    assert_eq!(
        rows(
            &screen,
            "session · daemon_stopping · `deadtrickster` asked this daemon to stop · click for \
             details"
        ),
        1,
        "{joined}"
    );
    assert!(!joined.contains("/notes"), "{joined}");
    assert_eq!(a.card_rows.len(), 1, "{:?}", a.card_rows);
    let (y, _) = a.card_rows[0];
    a.key(Key::Click { x: 10, y: y as u16 });
    assert!(holds(&popup_text(&mut a), "Runs in flight are ended."));
    // The title says what it is, in its register — not `notice` in red.
    let top = a.screen(160, 30).join("\n");
    assert!(
        top.contains("daemon_stopping · `deadtrickster` asked"),
        "{top}"
    );
}

/// **The hint is the last thing a narrow screen cuts** — the facts go first.
#[test]
fn a_narrow_screen_cuts_the_facts_and_keeps_the_hint() {
    let mut a = with_notice(
        "message from your parent session `s-gatekeeper`: Two new constraints from the \
         operator, fold them into the plan before you finish and say so in your reply.",
    );
    let screen = a.screen(80, 30);
    let line = rows_with(&screen, "parent s-gatekeeper")
        .first()
        .map(|l| l.to_string())
        .unwrap_or_else(|| panic!("{}", screen.join("\n")));
    assert!(line.contains("… · click for details"), "{line}");
    for l in &screen {
        assert!(visible_width(l) <= 80, "{l:?}");
    }
}

/// **alt-n with nothing to open says so**, and opens nothing.
#[test]
fn alt_n_with_no_notice_says_so() {
    let mut a = app();
    a.screen(80, 24);
    a.key(Key::AltN);
    assert!(a.diff_popup.is_none());
    assert!(
        a.notice.as_deref().is_some_and(|n| n.contains("no notice")),
        "{:?}",
        a.notice
    );
}
