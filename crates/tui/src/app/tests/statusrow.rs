//! **Transient statuses ride the reserved status row, and the conversation never moves for them.**
//!
//! The operator, 2026-10-14: *"it showed me "resync..." in violet and "reading job output" very
//! nice! but they were on their bottom lines and conversation was jumping back and forth. can we
//! show such things [...] where Responding... line lives - it is preallocated and fixed"*.
//!
//! Every test here renders a frame before a transient, while it is up, and after it has gone, and
//! asserts that every row above the status row — the header and the whole transcript body — is
//! byte-identical in all three, and that the transient's words are on the status row itself.

use super::*;

const W: usize = 100;
const H: usize = 24;

/// **The three frames agree above the status row**, and the status row is where `needle` is.
///
/// The status row is found in `during` by its words; `before` and `after` must not carry them
/// there, and must be the same height — so the row is the same reserved line in all three, and
/// nothing above it moved.
fn still(before: &[String], during: &[String], after: &[String], needle: &str) -> usize {
    assert_eq!(before.len(), during.len(), "the frame changed height");
    assert_eq!(during.len(), after.len(), "the frame changed height");
    let at = during
        .iter()
        .position(|r| r.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} is not on the screen:\n{}", during.join("\n")));
    assert!(
        !before[at].contains(needle) && !after[at].contains(needle),
        "{needle:?} was there before or stayed after:\n{}\n---\n{}",
        before.join("\n"),
        after.join("\n")
    );
    for (name, other) in [("before", before), ("after", after)] {
        assert_eq!(
            &other[..at],
            &during[..at],
            "the rows above the status row moved ({name} vs during):\n{}\n---\n{}",
            other.join("\n"),
            during.join("\n")
        );
    }
    // **It is the status row** — the line directly above the composer's box, not a row of its
    // own somewhere else.
    assert!(
        during[at + 1].trim_start().starts_with('╭'),
        "{needle:?} is not on the row above the box:\n{}",
        during.join("\n")
    );
    at
}

/// A conversation with enough in it to fill the body, attached and idle.
fn conversation(busy: bool) -> (App, std::sync::Arc<Hub>) {
    let hub = Hub::new("s");
    hub.publish(testing::turn_started("t0"));
    for i in 0..30 {
        hub.publish(testing::delta("t0", &format!("line {i} of the answer\n\n")));
    }
    hub.publish(testing::turn_finished("t0"));
    if busy {
        hub.publish(testing::turn_started("t1"));
        hub.publish(testing::delta("t1", "working on it"));
    }
    let mut a = app();
    a.clock(1_000);
    a.apply(hello("s", vec![brief("s", "one", busy)], hub.snapshot()));
    a.screen(W, H);
    (a, hub)
}

fn accepted(a: &App, note: &str) -> ServerFrame {
    ServerFrame::Accepted {
        client_request_id: "r".into(),
        seq: a.seq,
        note: note.into(),
    }
}

fn boot(code: &str, detail: &str) -> SessionEvent {
    SessionEvent::Warning {
        code: code.into(),
        detail: detail.into(),
        compaction: None,
    }
}

/// Each transient, on and off, idle and mid-turn: resync, a job-output read acknowledged, the
/// link dropping for a moment, and the daemon's boot step.
#[test]
fn transients_are_drawn_in_the_status_row_and_the_conversation_never_moves() {
    for busy in [false, true] {
        // **resync** — the violet line the operator saw.
        let (mut a, hub) = conversation(busy);
        let before = a.screen(W, H);
        a.apply(ServerFrame::Resync {
            reason: "queue overflow".into(),
            dropped: 0,
            snapshot: Box::new(hub.snapshot()),
            scrubbed: Default::default(),
        });
        let during = a.screen(W, H);
        a.clock(1_000 + NOTICE_MS + 1);
        let after = a.screen(W, H);
        let at = still(&before, &during, &after, "resync: queue overflow");
        if busy {
            assert!(during[at].contains("Responding"), "{}", during[at]);
            let (turn, notice) = (
                during[at].find("Responding").unwrap(),
                during[at].find("resync").unwrap(),
            );
            assert!(
                turn < notice,
                "the turn's clock comes first: {}",
                during[at]
            );
        }

        // **"reading output of job …"** — the acknowledgement an older daemon sends for every
        // read, and so for every one-second refresh of the jobs pane.
        let (mut a, _hub) = conversation(busy);
        let before = a.screen(W, H);
        let f = accepted(&a, "reading output of job j7");
        a.apply(f);
        let during = a.screen(W, H);
        a.clock(1_000 + NOTICE_MS + 1);
        let after = a.screen(W, H);
        let at = still(&before, &during, &after, "reading output of job j7");
        assert_eq!(busy, during[at].contains("Responding"), "{}", during[at]);

        // **The link down for a moment** — a daemon restart, under the impatient threshold.
        let (mut a, _hub) = conversation(busy);
        let before = a.screen(W, H);
        a.link_down("the daemon closed the connection");
        let during = a.screen(W, H);
        // Up again. (The `Hello` that does this in life also files the "daemon is back" note in
        // the conversation — a row of history, at the point it happened, not a transient.)
        a.link = Link::Attached;
        let after = a.screen(W, H);
        still(&before, &during, &after, "reconnecting");

        // **The daemon's boot step** (R92.2), which used to be a row at the foot of the body.
        let (mut a, _hub) = conversation(busy);
        let before = a.screen(W, H);
        let seq = a.seq;
        a.apply(ServerFrame::Event(env(
            seq + 1,
            boot("daemon_starting", "starting: loading the vocabulary"),
        )));
        let during = a.screen(W, H);
        a.apply(ServerFrame::Event(env(
            seq + 2,
            boot("daemon_started", "started: the daemon takes prompts now"),
        )));
        let after = a.screen(W, H);
        still(
            &before,
            &during,
            &after,
            "daemon starting: loading the vocabulary",
        );
    }
}

/// **Mid-turn, a transient is the first thing the row gives up** — the turn's own words are never
/// cut for it, and a notice cut to the room it has ends in the click hint and is a target.
#[test]
fn a_narrow_status_row_drops_the_transient_before_the_turn() {
    let (mut a, _hub) = conversation(true);
    let f = accepted(
        &a,
        "reading output of job j7 — a long acknowledgement that cannot fit",
    );
    a.apply(f);
    // Room for the turn and a cut notice.
    let (row, cut) = a.status_row(90);
    assert!(row.contains("Responding"), "{row}");
    assert!(cut.is_some(), "the notice was not cut: {row}");
    assert!(
        row.contains(crate::ui::transcript::line_cards::CLICK),
        "{row}"
    );
    assert!(visible_width(&row) <= 90, "{row}");
    // No room for any of it: the notice goes, the turn stays whole.
    let (row, cut) = a.status_row(50);
    assert!(row.contains("Responding"), "{row}");
    assert!(!row.contains("reading"), "{row}");
    assert!(cut.is_none());
}

/// **The jobs pane's read and its one-second refresh**: with the output window on screen, each
/// re-read the older daemon acknowledges says `reading output of job …` — in the status row, and
/// the window above it does not move.
#[test]
fn the_job_output_refresh_never_moves_the_pane() {
    let window = |lines: Vec<&str>, to: u64| SessionEvent::JobOutput {
        job: "j16496".into(),
        from: 0,
        to,
        produced: to,
        dropped: 0,
        state: "running".into(),
        never_ran: false,
        lines: lines.into_iter().map(String::from).collect(),
        next: None,
    };
    let mut a = App::new(plain_cfg(W));
    a.session_id = "s1".into();
    a.clock(10_000);
    a.apply(jobs_frame(
        "s1",
        vec![daemon_job("j16496", "docker pull serenedb", true)],
    ));
    a.key(Key::CtrlQ);
    assert!(matches!(
        a.key(Key::Enter),
        Some(Action::ReadJobOutput { .. })
    ));
    // The first read: acknowledged (an older daemon), then answered.
    let f = accepted(&a, "reading output of job j16496");
    a.apply(f);
    let reading = a.screen(W, H);
    assert!(
        reading
            .iter()
            .any(|r| r.contains("reading output of job j16496")),
        "{}",
        reading.join("\n")
    );
    a.apply(ServerFrame::Event(env(1, window(vec!["Pulling"], 7))));
    a.clock(12_000);
    let before = a.screen(W, H);
    // The refresh: the clock asks again, the daemon acknowledges, the window lands unchanged.
    a.clock(13_000);
    assert!(
        a.take_actions()
            .iter()
            .any(|x| matches!(x, Action::ReadJobOutput { .. })),
        "no refresh was asked for"
    );
    let f = accepted(&a, "reading output of job j16496");
    a.apply(f);
    let during = a.screen(W, H);
    a.apply(ServerFrame::Event(env(2, window(vec!["Pulling"], 7))));
    a.clock(13_000 + NOTICE_MS + 1);
    let after = a.screen(W, H);
    still(&before, &during, &after, "reading output of job j16496");
}

// ---------------------------------------------------------------------------------------------
// **The long waits**: the fill, fold and announced-rows progress, one line in the same row.
//
// The operator, 2026-10-14: *"yes, move the compacting progress blocks too"*. Each was a block of
// two or three rows at the foot of the conversation that came when its operation started, changed
// height when a fold went from reading to writing, and went when it ended.
// ---------------------------------------------------------------------------------------------

/// [`conversation`], with a turn this head watched start — so mid-turn the row reads
/// `Responding · 4.0s` rather than the snapshot's long `started before this head attached`, and
/// what is measured is the progress after it rather than a row the turn already filled.
fn watched(busy: bool) -> (App, std::sync::Arc<Hub>) {
    let (mut a, hub) = conversation(busy);
    if let Some(t) = a.turn.as_mut() {
        t.started_ms = 1_000;
    }
    a.screen(W, H);
    (a, hub)
}

fn filling(what: &str, done: u64, total: u64) -> SessionEvent {
    SessionEvent::Filling {
        what: what.into(),
        unit: "rows".into(),
        done,
        total,
    }
}

fn folding(processed: u64, written: u64) -> SessionEvent {
    SessionEvent::CompactionProgress {
        half: 1,
        halves: 2,
        prompt_tokens: 66_000,
        processed,
        written,
        unit: "tokens".into(),
    }
}

/// Every frame in `frames` has the same height and the same rows above the status row, and the
/// status row is the row directly above the composer's box. Returns that row's index.
fn body_still(frames: &[(&str, &Vec<String>)]) -> usize {
    let (first_name, first) = frames[0];
    let at = first
        .iter()
        .position(|r| r.trim_start().starts_with('╭'))
        .expect("the composer's box")
        - 1;
    for (name, f) in frames {
        assert_eq!(f.len(), first.len(), "the frame changed height ({name})");
        assert!(
            f[at + 1].trim_start().starts_with('╭'),
            "the box moved ({name}):\n{}",
            f.join("\n")
        );
        assert_eq!(
            &f[..at],
            &first[..at],
            "the rows above the status row moved ({first_name} vs {name}):\n{}\n---\n{}",
            first.join("\n"),
            f.join("\n")
        );
    }
    at
}

/// **Each progress kind, before / start / mid / end, idle and mid-turn**: nothing above the status
/// row moves, and the status row carries the one-line form — label, bar, numbers.
#[test]
fn every_progress_rides_the_status_row_and_the_conversation_never_moves() {
    for busy in [false, true] {
        // **A fill the daemon named** — a restore, an import, a compaction's carry.
        let (mut a, _hub) = watched(busy);
        let before = a.screen(W, H);
        let seq = a.seq;
        a.apply(ServerFrame::Event(env(
            seq + 1,
            filling("restoring the stored conversation", 0, 2_000),
        )));
        let start = a.screen(W, H);
        a.clock(5_000);
        a.apply(ServerFrame::Event(env(
            seq + 2,
            filling("restoring the stored conversation", 1_000, 2_000),
        )));
        let mid = a.screen(W, H);
        a.apply(ServerFrame::Event(env(
            seq + 3,
            filling("restoring the stored conversation", 2_000, 2_000),
        )));
        let end = a.screen(W, H);
        let at = body_still(&[
            ("before", &before),
            ("start", &start),
            ("mid", &mid),
            ("end", &end),
        ]);
        assert!(
            start[at].contains("Restoring the stored conversation · ▐"),
            "{}",
            start[at]
        );
        assert!(start[at].contains("   0/2000 rows"), "{}", start[at]);
        assert!(mid[at].contains(" 50%"), "{}", mid[at]);
        assert!(mid[at].contains("1000/2000 rows"), "{}", mid[at]);
        assert!(mid[at].contains("4.0s"), "the clock runs: {}", mid[at]);
        assert!(!end[at].contains("Restoring"), "{}", end[at]);
        assert!(!before[at].contains("Restoring"), "{}", before[at]);
        assert_eq!(busy, mid[at].contains("Responding"), "{}", mid[at]);
        if busy {
            assert!(
                mid[at].find("Responding").unwrap() < mid[at].find("Restoring").unwrap(),
                "the turn's words come first: {}",
                mid[at]
            );
        }

        // **A reseat's carry** (cb380cf): the render onto the new prompt ticks its own bar, then
        // the write has its own from zero — two operations, each its own label and clock, and
        // still nothing above the row moves.
        let (mut a, _hub) = watched(busy);
        let before = a.screen(W, H);
        let seq = a.seq;
        let render = "re-seating: rendering the conversation onto the new prompt";
        let write = "re-seating: writing the carried conversation";
        a.apply(ServerFrame::Event(env(seq + 1, filling(render, 0, 600))));
        let start = a.screen(W, H);
        a.clock(3_000);
        a.apply(ServerFrame::Event(env(seq + 2, filling(render, 320, 600))));
        let mid = a.screen(W, H);
        a.apply(ServerFrame::Event(env(seq + 3, filling(write, 0, 600))));
        let writing = a.screen(W, H);
        a.apply(ServerFrame::Event(env(seq + 4, filling(write, 600, 600))));
        let end = a.screen(W, H);
        let at = body_still(&[
            ("before", &before),
            ("start", &start),
            ("mid", &mid),
            ("writing", &writing),
            ("end", &end),
        ]);
        assert!(start[at].contains("Re-seating: rendering"), "{}", start[at]);
        // Idle the whole form fits; mid-turn, after the turn's words, the daemon's long label
        // keeps its place and the numbers are what give way.
        assert_eq!(!busy, mid[at].contains("320/600"), "{}", mid[at]);
        assert!(
            writing[at].contains("Re-seating: writing"),
            "{}",
            writing[at]
        );
        assert!(!end[at].contains("Re-seating"), "{}", end[at]);
        // The write is its own operation: its clock starts at its own first tick.
        assert_eq!(a.filling_since, 3_000);

        // **A fold**: reading its half (a bar), then writing (a count), then landing.
        let (mut a, _hub) = watched(busy);
        let before = a.screen(W, H);
        let seq = a.seq;
        a.apply(ServerFrame::Event(env(seq + 1, folding(6_600, 0))));
        let start = a.screen(W, H);
        a.clock(73_000);
        a.apply(ServerFrame::Event(env(seq + 2, folding(41_200, 0))));
        let mid = a.screen(W, H);
        a.apply(ServerFrame::Event(env(seq + 3, folding(66_000, 2_400))));
        let writing = a.screen(W, H);
        // The fold's end is the daemon's durable `compacted` warning, which also files its own
        // row of history; what is under test is the progress line, so it is ended the way that
        // arm ends it and nothing else is added.
        a.compacting = None;
        let end = a.screen(W, H);
        let at = body_still(&[
            ("before", &before),
            ("start", &start),
            ("mid", &mid),
            ("writing", &writing),
            ("end", &end),
        ]);
        assert!(
            start[at].contains("Compacting half 1 of 2 · ▐"),
            "{}",
            start[at]
        );
        assert!(start[at].contains(" 10%"), "{}", start[at]);
        assert!(mid[at].contains("41.2k/66.0k tok read"), "{}", mid[at]);
        assert!(mid[at].contains("1m12s"), "the clock runs: {}", mid[at]);
        assert!(
            writing[at].contains("Compacting half 1 of 2 · 2400 tokens written"),
            "writing is a count, not a bar: {}",
            writing[at]
        );
        assert!(!writing[at].contains('▐'), "{}", writing[at]);
        assert!(!end[at].contains("Compacting"), "{}", end[at]);

        // **A snapshot's announced rows**: the snapshot itself replaces the conversation, and
        // every body that lands is a row — so what is asserted is that the LINE costs no row:
        // arriving, then past the patience the finding, then cleared, all on one body.
        let mut a = app();
        a.clock(1_000);
        a.apply(snapshot_hello("snap", 6));
        let arriving = a.screen(W, H);
        a.clock(1_000 + BODY_PATIENCE + 1);
        let stalled = a.screen(W, H);
        a.bulk = None;
        let cleared = a.screen(W, H);
        let at = body_still(&[
            ("arriving", &arriving),
            ("stalled", &stalled),
            ("cleared", &cleared),
        ]);
        assert!(
            arriving[at].contains("Rows arriving · ▐"),
            "{}",
            arriving[at]
        );
        assert!(arriving[at].contains("0/6 rows"), "{}", arriving[at]);
        assert!(
            stalled[at].contains("6 row(s) never filled · snap.0, snap.1, snap.2, … (3 more)"),
            "{}",
            stalled[at]
        );
        assert!(!cleared[at].contains("never filled"), "{}", cleared[at]);
    }
}

/// **A narrow row gives up the numbers, then the bar — never the label.** Idle, at every width
/// from wide to tiny; and mid-turn the turn's own words are never cut for the progress.
#[test]
fn a_narrow_status_row_drops_the_numbers_then_the_bar_and_keeps_the_label() {
    let (mut a, _hub) = conversation(false);
    let seq = a.seq;
    a.apply(ServerFrame::Event(env(seq + 1, folding(41_200, 0))));
    a.clock(73_000);
    let label = "Compacting half 1 of 2";
    let mut seen_bar_without_counts = false;
    let mut seen_label_alone = false;
    for w in (24..=120).rev() {
        let (row, click) = a.status_row(w);
        assert!(visible_width(&row) <= w, "{w}: {row}");
        assert!(
            row.contains(label),
            "the label is never dropped ({w}): {row}"
        );
        let (bar, counts, clock) = (
            row.contains('▐'),
            row.contains("tok read"),
            row.contains("1m12s"),
        );
        // The order things go in: the clock, the counts, the bar.
        assert!(
            !clock || counts,
            "the clock outlived the counts ({w}): {row}"
        );
        assert!(!counts || bar, "the counts outlived the bar ({w}): {row}");
        seen_bar_without_counts |= bar && !counts;
        seen_label_alone |= !bar;
        // **The detail is a click away** — the fold's block said more than any row holds.
        let (title, text) = click.expect("a fold's row is a click target");
        assert_eq!(title, "progress");
        assert!(
            text.contains("conversation is kept either way"),
            "{w}: {text}"
        );
    }
    assert!(seen_bar_without_counts && seen_label_alone);
    // Too narrow for the whole label: cut, still the label's words.
    let (row, _) = a.status_row(16);
    assert!(row.contains("Compacting"), "{row}");

    // **Mid-turn** the turn's words stay whole and the progress follows them, cut or dropped.
    let (mut a, _hub) = conversation(true);
    let seq = a.seq;
    a.apply(ServerFrame::Event(env(seq + 1, folding(41_200, 0))));
    for w in [100, 60, 40, 30] {
        let (row, _) = a.status_row(w);
        assert!(visible_width(&row) <= w, "{w}: {row}");
        assert!(row.contains("Responding"), "{w}: {row}");
    }
    let (row, _) = a.status_row(100);
    assert!(row.contains("Compacting half 1 of 2 · ▐"), "{row}");
    // Too narrow even for the label after the turn's words: the row is the turn's.
    let turn_only = a.turn_status(30);
    let (row, _) = a.status_row(visible_width(&turn_only) + 3);
    assert!(!row.contains("Compacting"), "{row}");
}

/// **Two at once: the one that started last, and `+1`.** A fold carries its conversation onto the
/// new prompt — a fill — while the fold's own line is still up; the row shows the newer, counts
/// the other, and the click opens both.
#[test]
fn two_progresses_show_the_one_that_started_last_and_count_the_other() {
    let (mut a, _hub) = conversation(false);
    let before = a.screen(W, H);
    let seq = a.seq;
    a.clock(2_000);
    a.apply(ServerFrame::Event(env(seq + 1, folding(41_200, 0))));
    a.clock(3_000);
    a.apply(ServerFrame::Event(env(
        seq + 2,
        filling("compacting: rendering the new transcript", 300, 600),
    )));
    let both = a.screen(W, H);
    let at = body_still(&[("before", &before), ("both", &both)]);
    assert!(
        both[at].contains("Compacting: rendering the new transcript +1 · ▐"),
        "{}",
        both[at]
    );
    assert!(!both[at].contains("half 1 of 2"), "{}", both[at]);
    // The click opens every one that is live, the newest first.
    let (title, text) = a
        .status_click
        .clone()
        .expect("two progresses: the row clicks");
    assert_eq!(title, "progress");
    let (fill, fold) = (
        text.find("compacting: rendering the new transcript")
            .unwrap(),
        text.find("compacting half 1 of 2").unwrap(),
    );
    assert!(fill < fold, "{text}");
    a.key(Key::Click {
        x: 10,
        y: at as u16,
    });
    let popup = super::redcard::popup_text(&mut a);
    assert!(
        super::redcard::holds(&popup, "the conversation is kept either way"),
        "{popup}"
    );

    // The newer ends: the older is the row again, with no count.
    a.key(Key::Esc);
    a.apply(ServerFrame::Event(env(
        seq + 3,
        filling("compacting: rendering the new transcript", 600, 600),
    )));
    let one = a.screen(W, H);
    body_still(&[("before", &before), ("one", &one)]);
    assert!(
        one[at].contains("Compacting half 1 of 2 · ▐"),
        "{}",
        one[at]
    );
    assert!(!one[at].contains("+1"), "{}", one[at]);
}

/// **A notice while a progress is up**: the progress gives up its numbers to keep the notice
/// whole, keeps its label, and gets the numbers back when the notice goes.
#[test]
fn a_notice_over_a_progress_takes_its_numbers_not_its_label() {
    let (mut a, _hub) = conversation(false);
    let seq = a.seq;
    a.apply(ServerFrame::Event(env(
        seq + 1,
        filling("restoring the stored conversation", 1_000, 2_000),
    )));
    let quiet = a.screen(W, H);
    let f = accepted(&a, "reading output of job j7 and a few more words of it");
    a.apply(f);
    let noisy = a.screen(W, H);
    let at = body_still(&[("quiet", &quiet), ("noisy", &noisy)]);
    assert!(quiet[at].contains("1000/2000 rows"), "{}", quiet[at]);
    assert!(
        noisy[at].contains("Restoring the stored conversation"),
        "{}",
        noisy[at]
    );
    assert!(
        noisy[at].contains("reading output of job j7 and a few more words of it"),
        "the notice is whole: {}",
        noisy[at]
    );
    a.clock(1_000 + NOTICE_MS + 1);
    let after = a.screen(W, H);
    assert!(after[at].contains("1000/2000 rows"), "{}", after[at]);
}

/// **A model switch the daemon has taken and not yet applied is the status row's, and it comes
/// down the moment the switch lands.**
///
/// MEASURED 2026-10-26, the operator's own session: `/models deepseek/deepseek-flash` typed during
/// a turn is recorded and applied when the turn ends — so the honest answer to *did that take?* is
/// *not yet*, and it has to stay on the screen until it does. The `slash` reply that says so
/// scrolls away inside the very turn it is about, and the header goes on naming the old model,
/// which is the truth and half of it.
///
/// **The needle is the sentence's own middle**, because the row is trimmed to the frame: on the
/// turn's line it follows `Responding · 12.3s`, and a longer needle would be asserting the width.
#[test]
fn a_switch_waiting_for_the_turn_rides_the_status_row_until_it_lands() {
    const SAID: &str = "turns go to glm/glm-4.6 when this turn ends";
    for busy in [false, true] {
        let (mut a, _hub) = conversation(busy);
        let seq = a.seq;
        let before = a.screen(W, H);
        a.apply(ServerFrame::Event(env(
            seq + 1,
            boot(
                "model_switch_pending",
                &format!("{SAID}; this one finishes on deepseek/deepseek-chat"),
            ),
        )));
        let during = a.screen(W, H);
        // Nothing above the status row moved: a state the daemon holds is not a row.
        let at = body_still(&[("before", &before), ("pending", &during)]);
        assert!(
            !before[at].contains(SAID),
            "the switch was on the row before the daemon said so: {}",
            before[at]
        );
        assert!(
            during[at].contains(SAID),
            "the pending switch is not on the status row: {}",
            during[at]
        );
        if busy {
            // **After the turn's own clock**, which is how every other segment on this row is
            // ordered: what the turn is doing, then what else is true of the session.
            let (turn, switch) = (
                during[at].find("Responding").expect("the turn's words"),
                during[at].find(SAID).unwrap(),
            );
            assert!(
                turn < switch,
                "the turn's clock comes first: {}",
                during[at]
            );
        }

        // **And it goes when the switch lands** — the applied sentence replaces it in the record,
        // and the row stops claiming something is waiting.
        a.apply(ServerFrame::Event(env(
            seq + 2,
            boot(
                "model_switch_applied",
                "the turn has ended, so the switch stands: turns go to glm/glm-4.6 from the next \
                 one on — METERED",
            ),
        )));
        let after = a.screen(W, H);
        assert!(
            !after.iter().any(|r| r.contains(SAID)),
            "the pending switch outlived the switch that landed:\n{}",
            after.join("\n")
        );
    }
}

/// **A daemon older than its store says so in RED on the status row, and keeps saying it.**
///
/// The operator's ask of 2026-10-26, verbatim: *an older binary should keep working when the newer
/// schema only ADDED things, and must say so in red on the head's usual status line*. Three claims
/// and this test is all three: it is on the STATUS row (the line directly above the composer's
/// box, which is the row `Responding…` uses and which costs the conversation no line at all); it
/// is in the red register (the span's role is `Role::Failure`, and the code is classified `Failure`
/// in `sessionlog::warning`, which is the one place a register is decided); and it is STATE, not a
/// note — the same row says it on the next frame, and on the frame after a minute has passed.
///
/// It is drawn at 160 columns because the sentence is long on purpose: it names both schemas AND
/// the command that fixes it, and a test at the row's usual 100 would be asserting a cut.
#[test]
fn a_daemon_older_than_its_store_says_so_in_red_and_stays() {
    const SAID: &str = "this daemon (schema 33) is older than its store (34) — restart it: \
                        letibot --stop · running /home/dead/bin/letibot-harnessd, built 10-26 \
                        22:39";
    const WIDE: usize = 160;
    for busy in [false, true] {
        let (mut a, _hub) = conversation(busy);
        let before = a.screen(WIDE, H);
        assert!(
            !before.iter().any(|r| r.contains("older than its store")),
            "an ordinary daemon says nothing about the store:\n{}",
            before.join("\n")
        );

        let seq = a.seq;
        a.apply(ServerFrame::Event(env(seq + 1, boot("store_newer", SAID))));
        let during = a.screen(WIDE, H);
        let at = during
            .iter()
            .position(|r| r.contains("older than its store"))
            .unwrap_or_else(|| {
                panic!(
                    "the store's age is not on the screen:\n{}",
                    during.join("\n")
                )
            });
        // **It is the status row**: the line directly above the composer's box, and the whole
        // sentence is there — the fix included, which is the half a cut would take.
        assert!(
            during[at + 1].trim_start().starts_with('╭'),
            "the store's age is not on the row above the box:\n{}",
            during.join("\n")
        );
        assert!(
            during[at].contains("restart it: letibot --stop"),
            "the fix is not on the row: {}",
            during[at]
        );
        if busy {
            // After the turn's own words, like every other segment of this row: what the turn is
            // doing, then what else is true.
            let (turn, store) = (
                during[at].find("Responding").expect("the turn's words"),
                during[at].find("older than its store").unwrap(),
            );
            assert!(turn < store, "the turn's clock comes first: {}", during[at]);
        }
        // **Nothing above it moved** — a transient in this row takes no line from the conversation.
        assert_eq!(
            &before[..at],
            &during[..at],
            "the row above the status row moved:\n{}",
            during.join("\n")
        );

        // **RED**, and not by the spelling of a colour code: the first transient's role is
        // `Role::Failure`, and the code the daemon sent is classified `Failure` in the one table
        // that decides registers. Both, because a head could paint it red and a second head could
        // classify it quietly.
        let (text, role, is_notice) = a.transients().remove(0);
        assert!(text.contains("older than its store"), "{text}");
        assert_eq!(role, Role::Failure, "{text}");
        assert!(!is_notice, "the red line is state, not the flash notice");
        assert_eq!(
            letibot_sessionlog::warning::class("store_newer"),
            letibot_sessionlog::warning::Class::Failure,
            "`store_newer` is drawn in the register this row paints red"
        );

        // **And it is state, not a note** — a minute later the same row still says it, and no
        // notice timer takes it down.
        a.clock(1_000 + 60_000);
        let later = a.screen(WIDE, H);
        assert!(
            later[at].contains("older than its store"),
            "the store's age went away on its own:\n{}",
            later.join("\n")
        );
    }
}
