//! **A job's completion notice, two lines a job, and its whole card one click away.**
//!
//! The operator, pasting one: *"i dont need job completion card that huge - first line is
//! j<id> - slug - truncated cmd and second line is truncated output. click behaves similar to
//! diff click - popup with full card"*. See `ui::transcript::job_card`.

use super::*;
use crate::ui::transcript::job_card::{JobTail, ended_jobs, notice_jobs};

/// The daemon's own notices, written by `completion_notice` and compared byte for byte by
/// `harnessd`'s `the_tuis_job_notice_fixtures_are_what_completion_notice_writes`.
const MINE: &str = include_str!("fixtures/job_notice_mine.txt");
const THEIRS: &str = include_str!("fixtures/job_notice_theirs.txt");
/// R79's held batch: a `[meanwhile]` line over an ordinary `[job]` group, pinned by `harnessd`'s
/// `the_meanwhile_notice_is_the_heads_fixture`.
const MEANWHILE: &str = include_str!("fixtures/job_notice_meanwhile.txt");

/// The operator's paste, the notice's shape with one job — a command over several lines and the
/// memory clause, which is what used to defeat the fold and draw six raw rows.
const ONE: &str = "[job] a job you backgrounded has ended:\n  - `build-and-test-null · j1042` exited 0 after 2m54s, wrote 1008 bytes: bash -c 'cd /home/dead/Projects/dbtest/pg-noop; echo \"=== building release\"\n  cargo build --release 2>&1 | tail -3'\n    memory: peak 245 MiB of a 8 GiB cap\nThis is the completion arriving on its own — you do not need to wait for it, and `job_wait` would only block you for a result you already have. Read what it wrote with `job_output` (job=\"…\"), then carry on with what you were doing.";

const BOILERPLATE: &str = "This is the completion arriving on its own";

/// A head attached to `s` with `notice` as its one row, spoken by the harness.
fn with_notice(notice: &str) -> App {
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

/// The screen rows that carry `needle`.
fn rows_with<'a>(screen: &'a [String], needle: &str) -> Vec<&'a String> {
    screen.iter().filter(|l| l.contains(needle)).collect()
}

/// **The parser reads what the daemon writes** — the fixtures ARE `completion_notice`'s output,
/// so this is the half of the pin that says the TUI still understands it.
#[test]
fn the_notice_parser_reads_the_daemons_own_notices() {
    let (mine, jobs) = ended_jobs(MINE).expect("the owner's notice parses");
    assert!(mine);
    assert_eq!(jobs.len(), 2);
    let j = &jobs[0];
    assert_eq!(j.id, "j1042");
    assert_eq!(j.slug.as_deref(), Some("build-and-test-null"));
    assert_eq!(j.state, "exited 0");
    assert_eq!(j.after, "2m54s");
    assert_eq!(j.produced, 1008);
    assert_eq!(j.memory.as_deref(), Some("peak 245 MiB of a 8 GiB cap"));
    assert!(
        j.command
            .starts_with("bash -c 'cd /home/dead/Projects/dbtest/pg-noop;")
            && j.command.ends_with("tail -5'")
            && j.command.contains("\n\n"),
        "the whole command, its lines and its blank line kept: {:?}",
        j.command
    );
    assert_eq!(jobs[1].id, "j1043");
    assert_eq!(jobs[1].slug, None);
    assert_eq!(jobs[1].state, "exited 101");
    assert_eq!(jobs[1].memory, None);

    let (mine, jobs) = ended_jobs(THEIRS).expect("the handed-up notice parses");
    assert!(!mine);
    assert_eq!(jobs[0].id, "j7");
    assert_eq!(jobs[0].owner.as_deref(), Some("s-child"));
    assert_eq!(jobs[0].command, "sleep 3; echo done");
    assert_eq!(jobs[0].state, "killed by job_kill");

    // Both kinds, in one row, as one wake joins them.
    let both = format!("{MINE}\n\n{THEIRS}");
    assert_eq!(notice_jobs(&both).map(|j| j.len()), Some(3));
}

/// **What it does not know, it does not draw compact** — `folded_notice`'s rule, kept.
#[test]
fn a_notice_shape_the_parser_does_not_know_is_refused() {
    for (what, text) in [
        (
            "a bullet count the heading does not say",
            MINE.replace("2 jobs", "3 jobs"),
        ),
        (
            "a line after the closing",
            format!("{ONE}\nSomething new the daemon has started saying."),
        ),
        (
            "a heading this head was not told about",
            ONE.replace("a job you backgrounded has ended:", "a job is done:"),
        ),
        (
            "a closing this head was not told about",
            ONE.replace(BOILERPLATE, "Something new"),
        ),
        (
            "text after the memory clause",
            ONE.replace(
                "cap\nThis is",
                "cap\n  and something the daemon started saying\nThis is",
            ),
        ),
    ] {
        assert!(ended_jobs(&text).is_none(), "{what} parsed: {text}");
    }
}

/// **Two lines a job, cut to the width, and the paragraph to the model is not drawn.**
#[test]
fn a_completion_notice_draws_as_two_lines_per_job() {
    let mut a = with_notice(ONE);
    let screen = a.screen(100, 30);
    let joined = screen.join("\n");
    let first = rows_with(
        &screen,
        "j1042 · build-and-test-null · exited 0 2m54s · bash -c 'cd",
    );
    assert_eq!(first.len(), 1, "line 1 is the job: {joined}");
    assert!(
        first[0].contains('…'),
        "the command is cut to the width: {joined}"
    );
    assert!(
        joined.contains("⎿ wrote 1008 bytes · memory: peak 245 MiB of a 8 GiB cap"),
        "line 2, before the output lands, is what the notice knows: {joined}"
    );
    assert!(!joined.contains(BOILERPLATE), "{joined}");
    assert!(!joined.contains("job_output"), "{joined}");
    assert!(
        !joined.contains("cargo build --release"),
        "the command's second line: {joined}"
    );
    for l in &screen {
        assert!(visible_width(l) <= 100, "{l:?}");
    }
    // Exactly the two lines: the row is the notice's whole drawing.
    assert_eq!(a.notice_rows.len(), 2, "{:?}", a.notice_rows);
}

/// **Several jobs in one notice are two lines each**, and the handed-up group too.
#[test]
fn several_jobs_in_one_notice_are_two_lines_each() {
    let mut a = with_notice(&format!("{MINE}\n\n{THEIRS}"));
    let screen = a.screen(120, 30);
    let joined = screen.join("\n");
    assert_eq!(
        rows_with(&screen, "j1042 · build-and-test-null · exited 0 2m54s").len(),
        1
    );
    assert_eq!(
        rows_with(
            &screen,
            "j1043 · exited 101 4.4s · cargo test -p letibot-tui"
        )
        .len(),
        1
    );
    assert_eq!(
        rows_with(
            &screen,
            "j7 · release-build · killed by job_kill 3.0s · started by s-child · sleep 3; echo done"
        )
        .len(),
        1,
        "{joined}"
    );
    assert!(joined.contains("⎿ wrote nothing"), "{joined}");
    assert!(
        joined.contains("⎿ wrote 13 bytes · output not readable here"),
        "{joined}"
    );
    assert!(!joined.contains(BOILERPLATE), "{joined}");
    assert!(!joined.contains("This session is told because"), "{joined}");
    assert_eq!(a.notice_rows.len(), 6, "{:?}", a.notice_rows);
}

/// **At a narrow width the two lines stay two lines** — cut, not wrapped.
#[test]
fn a_narrow_screen_cuts_the_lines_and_does_not_wrap_them() {
    let mut a = with_notice(ONE);
    let screen = a.screen(40, 20);
    let joined = screen.join("\n");
    assert_eq!(a.notice_rows.len(), 2, "{joined}");
    assert_eq!(rows_with(&screen, "j1042").len(), 1, "{joined}");
    assert_eq!(rows_with(&screen, "⎿").len(), 1, "{joined}");
    for l in &screen {
        assert!(visible_width(l) <= 40, "{l:?}");
    }
}

/// **The output's last line is line 2** — asked for once, when the notice is on the screen,
/// at the tail of what the job wrote, and drawn when it lands.
#[test]
fn the_second_line_is_the_end_of_what_the_job_wrote() {
    let mut a = with_notice(ONE);
    a.screen(100, 30);
    let asked = a.take_actions();
    assert!(
        asked.contains(&Action::ReadJobOutput {
            job: "j1042".into(),
            offset: 0,
        }),
        "1008 bytes is less than the tail it asks for, so it reads from the start: {asked:?}"
    );
    a.screen(100, 30);
    assert!(
        !a.take_actions()
            .iter()
            .any(|x| matches!(x, Action::ReadJobOutput { .. })),
        "once per job, not once per frame"
    );
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::JobOutput {
            job: "j1042".into(),
            from: 0,
            to: 1008,
            produced: 1008,
            dropped: 0,
            state: "exited 0".into(),
            never_ran: false,
            lines: vec![
                "=== building release".into(),
                "test result: ok. 12 passed; 0 failed".into(),
                String::new(),
            ],
            next: None,
        },
    )));
    let joined = a.screen(100, 30).join("\n");
    assert!(
        joined.contains("⎿ test result: ok. 12 passed; 0 failed"),
        "the last line it wrote: {joined}"
    );
    assert!(!joined.contains("=== building release"), "{joined}");
}

/// **A read the notice asked for and the daemon refused is the notice's**, not a warning in the
/// record: the operator did not ask for it.
#[test]
fn a_refused_tail_read_is_not_a_row_in_the_conversation() {
    let mut a = with_notice(ONE);
    a.screen(100, 30);
    let _ = a.take_actions();
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::Warning {
            code: "job_output_refused".into(),
            detail: "no job `j1042` here; `/job` with no argument lists them".into(),
            compaction: None,
        },
    )));
    assert!(matches!(a.job_tails.get("j1042"), Some(JobTail::Gone(_))));
    let joined = a.screen(100, 30).join("\n");
    assert!(!joined.contains("no job `j1042` here"), "{joined}");
    assert!(joined.contains("⎿ wrote 1008 bytes"), "{joined}");
}

/// **A click on either line opens the whole card in the popup; Esc closes it** — the diff
/// popup's mechanism, R69's one hit-test.
#[test]
fn a_click_on_a_notice_opens_the_full_card_and_esc_closes_it() {
    let mut a = with_notice(ONE);
    let before = a.screen(100, 30);
    let _ = a.take_actions();
    for k in 0..2 {
        let (y, _) = a.notice_rows[k];
        assert_eq!(a.key(Key::Click { x: 10, y: y as u16 }), None);
        assert!(a.diff_popup.is_some(), "line {k} opens the popup");
        let popup = a.screen(100, 30).join("\n");
        assert!(
            popup.contains("j1042 · a job ended"),
            "the edge names the job: {popup}"
        );
        assert!(
            popup.contains(BOILERPLATE),
            "the full card, the model's paragraph too: {popup}"
        );
        assert!(
            popup.contains("cargo build --release 2>&1 | tail -3'"),
            "{popup}"
        );
        assert!(
            popup.contains("memory: peak 245 MiB of a 8 GiB cap"),
            "{popup}"
        );
        assert!(
            popup.contains("what build-and-test-null · j1042 wrote, at its end"),
            "{popup}"
        );
        assert!(popup.contains("esc closes"), "{popup}");
        assert!(
            !popup.contains("ctrl-] edits it"),
            "a notice has no file to edit: {popup}"
        );
        assert_eq!(a.key(Key::Esc), None);
        assert!(a.diff_popup.is_none(), "esc closed it");
        assert_eq!(
            a.screen(100, 30),
            before,
            "and the conversation is as it was"
        );
    }
}

/// **The popup fills in when the output lands** — it was asked for when the notice was drawn,
/// and the popup is laid out again on the answer.
#[test]
fn the_popup_shows_the_output_when_it_lands() {
    let mut a = with_notice(ONE);
    a.screen(100, 30);
    let (y, _) = a.notice_rows[0];
    a.key(Key::Click { x: 10, y: y as u16 });
    let reading = a.screen(100, 30).join("\n");
    assert!(reading.contains("reading…"), "{reading}");
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::JobOutput {
            job: "j1042".into(),
            from: 0,
            to: 1008,
            produced: 1008,
            dropped: 0,
            state: "exited 0".into(),
            never_ran: false,
            lines: vec!["=== testing".into(), "test result: ok".into()],
            next: None,
        },
    )));
    let landed = a.screen(100, 30).join("\n");
    assert!(
        landed.contains("=== testing") && landed.contains("test result: ok"),
        "{landed}"
    );
    assert!(!landed.contains("reading…"), "{landed}");
}

/// **A row that is not a notice is not a notice target**, and an operator's own `[job] ` words
/// are theirs: the parser reads the harness's rows only.
#[test]
fn only_the_harnesss_rows_are_notices() {
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
        },
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(operator_row(ONE)),
        },
    )));
    let joined = a.screen(100, 30).join("\n");
    assert!(a.notice_rows.is_empty(), "{joined}");
    assert!(!joined.contains("⎿ wrote"), "{joined}");
}

/// **The held batch (R79) keeps the compact rows**: the `[meanwhile]` line folds to one line, and
/// the jobs under it are two lines each, exactly as a wake's are.
#[test]
fn a_meanwhile_batch_draws_its_heading_and_two_lines_per_job() {
    let jobs = notice_jobs(MEANWHILE).expect("the batch's jobs parse");
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].1.id, "j1042");
    let mut a = with_notice(MEANWHILE);
    let screen = a.screen(120, 30);
    let joined = screen.join("\n");
    assert_eq!(
        rows_with(&screen, "Meanwhile: 2 results arrived while you were busy").len(),
        1,
        "{joined}"
    );
    assert_eq!(
        rows_with(&screen, "j1042 · release-build · exited 0 2m54s").len(),
        1,
        "{joined}"
    );
    assert_eq!(
        rows_with(&screen, "j1043 · exited 101 4.4s").len(),
        1,
        "{joined}"
    );
    assert!(!joined.contains(BOILERPLATE), "{joined}");
    assert_eq!(a.notice_rows.len(), 5, "{:?}", a.notice_rows);
}
