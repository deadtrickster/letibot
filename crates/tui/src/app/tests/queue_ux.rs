//! What the queue's surfaces say to somebody who has never seen them: one realistic fixture at eighty and a hundred
//! and twenty columns. The surfaces are the queue pane, the composer's bottom edge, a reviewer's verdict (line and
//! popup) and a hub notice, all pictures of one fixture so pane and card cannot disagree about a row.

use super::redcard::{popup_text, with_notice, with_warning};
use super::*;

/// The branch a reviewer judged and refused, with findings.
const REFUSED: &str = "agent/billing-retry";
/// The branch a reviewer is reading right now.
const REVIEWING: &str = "agent/panic-groupingsets-join";
/// The branch waiting for a reviewer.
const WAITING: &str = "agent/widget-colour";
/// The branch whose gate is running.
const GATING: &str = "agent/otel-spans";
/// The branch that landed.
const LANDED: &str = "agent/icons-fix";

/// The repository every row of the fixture is in.
const REPO: &str = "/home/dead/Projects/letibot/letibot";

/// The reviewer child that refused [`REFUSED`].
const REVIEWER: &str = "s-gatekeeper-sub-17917";

/// The notice and the warning the two verdict surfaces read - the daemon's own words, the
/// fixtures `harnessd`'s `the_tuis_review_card_fixtures_are_what_the_queue_writes` pins.
const REFUSED_MSG: &str = include_str!("fixtures/review_refused_message.txt");
const NEEDS_HUMAN: &str = include_str!("fixtures/review_needs_human_warning.txt");

/// The head, with the real `queue` layer answering - the pane's one renderer, through the
/// same host a production head builds for this session.
fn layer(mut a: App) -> App {
    a.session_id = "s1".into();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::build(
        "s1",
        &std::env::temp_dir(),
    ));
    a
}

/// One review row of the fixture.
fn review(
    id: &str,
    branch: &str,
    state: &str,
    state_ms: u64,
) -> letibot_sessionlog::event::QueueReview {
    letibot_sessionlog::event::QueueReview {
        id: id.into(),
        repository: REPO.into(),
        target: "main".into(),
        kind: "branch".into(),
        session_id: "s-child".into(),
        branch: branch.into(),
        state: state.into(),
        head_sha: "9f2c1ab77d34e5".into(),
        evidence: format!("`{branch}` is {state}"),
        brief: "make the retry loop stop after three goes".into(),
        created_ms: 1_000,
        state_ms,
        ..Default::default()
    }
}

/// One landing row of the fixture.
fn landing(
    id: &str,
    branch: &str,
    state: &str,
    state_ms: u64,
) -> letibot_sessionlog::event::QueueMerge {
    letibot_sessionlog::event::QueueMerge {
        id: id.into(),
        review_id: format!("r-{id}"),
        repository: REPO.into(),
        target: "main".into(),
        branch: branch.into(),
        state: state.into(),
        approved_sha: "9f2c1ab77d34e5".into(),
        approved_by: format!("reviewer:{REVIEWER}"),
        evidence: format!("`{branch}` is {state}"),
        created_ms: 1_000,
        state_ms,
        ..Default::default()
    }
}

/// The fixture: three branches in review (one being read, one waiting, one refused with
/// findings), two landings (one queued, one gating at step 2 of 5), one that landed, and a merge
/// target whose own tip is red.
fn snapshot() -> letibot_sessionlog::event::QueueSnapshot {
    let mut refused = review("r-billing", REFUSED, "rejected", 4 * 60_000);
    refused.reviewer = Some(REVIEWER.into());
    refused.verdict = Some("reject".into());
    refused.verdict_by = Some(format!("reviewer:{REVIEWER}"));
    refused.reviewed_sha = Some("9f2c1ab77d34e5".into());
    refused.reasons = vec![
        "the retry loop can spin for ever when the server answers 429".into(),
        "no test covers the third attempt".into(),
    ];
    refused.attempts = 1;

    let mut reviewing = review("r-panic", REVIEWING, "reviewing", 3 * 60_000);
    reviewing.reviewer = Some("s-gatekeeper-sub-17920".into());

    let mut gating = landing("m-otel", GATING, "gating", 6 * 60_000);
    gating.rebased_sha = Some("1f2a3b4c5d6e7f80".into());
    gating.running_gate = Some(letibot_sessionlog::event::QueueRunningGate {
        command: "cargo test --workspace --no-fail-fast".into(),
        step: 2,
        of: 5,
        started_ms: 0,
        tail: "test result: FAILED. 3 passed; 1 failed".into(),
        log_bytes: 4_096,
    });

    let red = letibot_sessionlog::event::TargetHealthView {
        repository: REPO.into(),
        target: "main".into(),
        sha: "60bf819c1a2b3c4d".into(),
        state: "red".into(),
        checked_ms: 60_000,
        evidence: "`cargo test -p letibot-tui` failed: 2 tests failed".into(),
        ..Default::default()
    };

    letibot_sessionlog::event::QueueSnapshot {
        at_ms: 62_000,
        reviews: vec![
            reviewing,
            review("r-widget", WAITING, "waiting", 30_000),
            refused,
        ],
        merges: vec![
            landing("m-docs", "agent/docs-pass", "queued", 40_000),
            gating,
            landing("m-icons", LANDED, "landed", 50_000),
        ],
        leases: vec![letibot_sessionlog::event::QueueLeaseView {
            repository: REPO.into(),
            owner: "lab/4242/1".into(),
            pid: 4242,
            host: "lab".into(),
            build: "harnessd 0.5.1".into(),
            heartbeat_ms: 60_000,
            acquired_ms: 1_000,
        }],
        targets: vec![red],
        ..Default::default()
    }
}

/// A head with the fixture in it and the queue pane open.
fn pane() -> App {
    let mut a = layer(app());
    let mut snap = snapshot();
    // This session's repository, so the pane is about one project and not about every queue
    // on the box - the scope a person meets on `/queue`.
    snap.here = Some(letibot_sessionlog::event::QueueHere {
        workspace: REPO.into(),
        repository: Some(REPO.into()),
        repositories: vec![REPO.into()],
    });
    a.command("queue");
    a.apply(ServerFrame::Queues { snapshot: snap });
    a
}

/// The lines the pane itself drew, without the conversation's chrome around it.
fn pane_rows(a: &mut App, w: usize) -> Vec<String> {
    a.screen(w, 40)
}

/// Where `needle` sits in the drawn pane - the stop the cursor moves to, read off the view
/// the frame kept, so a test cannot aim at a row the pane does not have.
fn stop_of(a: &App, needle: &str) -> usize {
    let view = a.queue_held.as_ref().expect("a drawn pane");
    for i in 0..a.queue_plugin_stops {
        if let Some(crate::ui::panes::plugin::Target::Row(r)) =
            crate::ui::panes::plugin::target_of(view, crate::ui::panes::plugin::Screen::Overview, i)
            && r.cells.iter().any(|c| c.text.contains(needle))
        {
            return i;
        }
    }
    panic!("`{needle}` is not on the pane");
}

/// Rows of a pane as text, so an assertion failure reads as the screen.
fn joined(rows: &[String]) -> String {
    rows.join("\n")
}

/// The verdict surfaces, drawn - the one line a notice becomes, its popup, and the queue
/// notice the hub raises. Written beside the pane's render so the before and after of a change to
/// the words is a diff of two files.
#[test]
fn dump_verdict_surfaces() {
    let mut out = String::new();
    for (name, notice) in [
        ("refused message", REFUSED_MSG),
        ("needs-human warning", NEEDS_HUMAN),
    ] {
        let mut a = if name.ends_with("message") {
            with_notice(notice)
        } else {
            with_warning("queue_needs_human", notice)
        };
        for w in [80usize, 120] {
            out.push_str(&format!("\n===== {name}, {w} cols =====\n"));
            let rows = a.screen(w, 30);
            out.push_str(&rows.join("\n"));
            let Some(y) = rows.iter().position(|r| r.contains("click for details")) else {
                continue;
            };
            out.push_str(&format!("\n===== {name}, popup, {w} cols =====\n"));
            a.key(Key::Click { x: 10, y: y as u16 });
            out.push_str(&popup_text(&mut a));
            a.key(Key::Esc);
        }
    }
    let dir = std::path::Path::new("scratch/queue-ux");
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("verdict-after.txt"), &out).unwrap();
}

#[test]
fn the_queue_pane_reads_for_somebody_who_has_never_seen_it() {
    let mut a = pane();
    a.screen(80, 40);
    let rows = pane_rows(&mut a, 80);
    let screen = joined(&rows);

    // A title that says what this is. Not `the queue`: the merge queue, and what it lists.
    assert!(screen.contains("the merge queue"), "{screen}");
    assert!(
        rows.iter()
            .any(|r| r.contains("in review") && r.contains("to land")),
        "the panel says what it lists: {screen}"
    );

    // Each row in plain words - what it is, and what state it is in.
    for word in [
        "a reviewer is reading it", // reviewing
        "waiting for a reviewer",   // waiting
        "waiting for the gate",     // queued
        "the gate is running",      // gating
    ] {
        assert!(screen.contains(word), "`{word}` is missing: {screen}");
    }

    // No code, glyph or abbreviation a newcomer has to guess: not the bracket mark, not
    // `byte(s)`, not the bare `rm`.
    for jargon in ["[~]", "[ ]", "byte(s)", "· rm", " · ]"] {
        assert!(!screen.contains(jargon), "`{jargon}` is jargon: {screen}");
    }
    // The summary of the finished ones is a sentence too.
    assert!(screen.contains("finished"), "{screen}");
    assert!(!screen.contains("[+]"), "{screen}");

    // The pane says whose branch each row is, in a word.
    assert!(
        screen.contains("in letibot"),
        "the repository is named: {screen}"
    );

    // The bar names the keys that would act on the row under the cursor - and only those. On
    // the row a reviewer is reading, approve and withdraw both move it.
    let bar = a.hint_bar(80);
    for want in ["enter opens it whole", "a approve", "v veto", "d withdraw"] {
        assert!(bar.contains(want), "the bar names `{want}`: {bar}");
    }

    // On the landing whose gate is running, approve would do nothing, so the bar does not offer
    // it - the keys that WOULD move the row are not pushed off the bar by ones that would not.
    let gating = stop_of(&a, GATING);
    while a.queue_sel < gating {
        a.key(Key::Down);
    }
    a.screen(80, 40);
    let bar = a.hint_bar(80);
    assert!(bar.contains("d withdraw"), "{bar}");
    assert!(
        !bar.contains("a approve"),
        "approve does nothing here: {bar}"
    );
}

/// The composer's edge says which queue it counts - it is a person's only sight of the queue
/// until they open the pane, so the numbers lead with what they are about, and each count carries
/// the noun it counts.
#[test]
fn the_edge_says_which_queue_it_counts() {
    let mut a = layer(app());
    let mut snap = snapshot();
    snap.here = Some(letibot_sessionlog::event::QueueHere {
        workspace: REPO.into(),
        repository: Some(REPO.into()),
        repositories: vec![REPO.into()],
    });
    a.command("queue");
    a.apply(ServerFrame::Queues { snapshot: snap });
    a.key(Key::Esc);
    let screen = a.screen(120, 30).join("\n");
    let edge = screen
        .lines()
        .find(|l| l.contains("queue:"))
        .unwrap_or_else(|| panic!("the edge says nothing about the queue: {screen}"));
    for want in ["1 red", "2 in review", "2 to land", "1 done"] {
        assert!(edge.contains(want), "the edge says `{want}`: {edge}");
    }
}

/// A reviewer's verdict says who reviewed what, and what came of it - the whole line, and the
/// popup's title above the daemon's own words.
#[test]
fn a_verdict_says_who_reviewed_what_and_how_it_went() {
    // A refusal arrives as the reviewer's own message, so the sender is the reviewer.
    let mut a = with_notice(REFUSED_MSG);
    let screen = a.screen(160, 30).join("\n");
    let line = screen
        .lines()
        .find(|l| l.contains("review ·"))
        .unwrap_or_else(|| panic!("the verdict drew as one line: {screen}"));
    assert!(
        line.contains("agent/panic-groupingsets-join"),
        "what it reviewed: {line}"
    );
    assert!(line.contains("refused"), "the outcome: {line}");
    assert!(
        line.contains("by s-gatekeeper-0123456789ab"),
        "who reviewed it: {line}"
    );

    // A warning that stops for a person names the reviewer that could not decide - on the status
    // line, which is where live state goes, and in `/notes`.
    let mut a = with_warning("queue_needs_human", NEEDS_HUMAN);
    let _ = a.screen(160, 30);
    assert!(
        a.notice.as_deref().is_some_and(|n| n.contains(NEEDS_HUMAN)),
        "the status line is silent: {:?}",
        a.notice
    );
    assert!(
        crate::ui::transcript::line_cards::warning_card("queue_needs_human", NEEDS_HUMAN)
            .is_some_and(|c| c
                .line
                .contains("could not decide by s-gatekeeper-sub-17917")),
        "the table reads the sentence"
    );
}

/// Every surface drawn, for the report - the pane at eighty and a hundred and twenty columns,
/// and the same fixture's edge. Written under `scratch/queue-ux/`, which the build never reads.
#[test]
fn dump_the_pane() {
    let mut out = String::new();
    for w in [80usize, 120] {
        let mut a = pane();
        // Twice: the bar is built from the rows the last frame recorded, so the first frame -
        // one no person ever reads - still offers every verb.
        a.screen(w, 40);
        out.push_str(&format!("\n===== the queue pane, {w} cols =====\n"));
        out.push_str(&pane_rows(&mut a, w).join("\n"));
    }
    let dir = std::path::Path::new("scratch/queue-ux");
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("pane-after.txt"), &out).unwrap();
}
