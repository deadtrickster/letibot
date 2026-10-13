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
    assert!(cut, "the notice was not cut: {row}");
    assert!(
        row.contains(crate::ui::transcript::line_cards::CLICK),
        "{row}"
    );
    assert!(visible_width(&row) <= 90, "{row}");
    // No room for any of it: the notice goes, the turn stays whole.
    let (row, cut) = a.status_row(50);
    assert!(row.contains("Responding"), "{row}");
    assert!(!row.contains("reading"), "{row}");
    assert!(!cut);
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
