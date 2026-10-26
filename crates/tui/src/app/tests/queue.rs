//! The review queue and the merge queue (R61), as this head draws them.

use super::*;

/// **`/queue` asks for the snapshot and draws nothing it was not sent** — and until one arrives
/// it says it is asking, rather than drawing an empty queue.
#[test]
fn the_queue_pane_asks_for_the_queues_and_draws_what_comes_back() {
    let mut a = app();
    a.session_id = "s1".into();
    assert_eq!(a.command("queue"), Some(Action::ListMergeQueue));
    assert!(a.is_open(Surface::Queue));
    let screen = a.screen(120, 30).join("\n");
    assert!(screen.contains("review queue · merge queue"), "{screen}");
    assert!(screen.contains("asking the daemon"), "{screen}");
    assert!(!screen.contains("none."), "not drawn as empty: {screen}");

    a.apply(queue_frame(queues(
        vec![queue_review_row("one", "reviewing")],
        vec![queue_merge_row("m-two", "queued")],
    )));
    let screen = a.screen(140, 40).join("\n");
    assert!(screen.contains("agent/one"), "{screen}");
    assert!(screen.contains("reviewing"), "{screen}");
    assert!(
        !screen.contains("agent/two"),
        "the review view lists the REVIEW queue only: {screen}"
    );
    assert_eq!(a.command("queue"), None);
    assert!(!a.is_open(Surface::Queue));
}

/// **Two queues, two views** — the operator's *"nor there are separate views for review and
/// merge queues"*; `tab` switches, and the strip counts each.
#[test]
fn tab_switches_between_the_review_queue_and_the_merge_queue() {
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    a.apply(queue_frame(queues(
        vec![queue_review_row("one", "waiting")],
        vec![queue_merge_row("m-two", "gating")],
    )));
    let strip = |rows: &[String]| {
        rows.iter()
            .find(|l| l.contains("tab switches"))
            .cloned()
            .expect("the strip")
    };
    let rows = a.screen(140, 40);
    assert!(
        strip(&rows).contains("[review queue (1)]"),
        "{:?}",
        strip(&rows)
    );
    assert_eq!(a.key(Key::Tab), None);
    let rows = a.screen(140, 40);
    assert!(
        strip(&rows).contains("[merge queue (1)]"),
        "{:?}",
        strip(&rows)
    );
    let screen = rows.join("\n");
    assert!(
        screen.contains("agent/two") && screen.contains("gating"),
        "{screen}"
    );
    assert!(!screen.contains("agent/one"), "{screen}");
}

/// **A row says how long it has been in its CURRENT state and who is working it** — R61 defect
/// 12: the age was since creation, and no row named its owner.
#[test]
fn a_row_says_time_in_state_and_its_owner() {
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let mut r = queue_review_row("one", "reviewing");
    r.owner = "lab/4242/1700000000000".into();
    r.created_ms = 0;
    r.state_ms = 2_000; // the snapshot is read at 62s: one minute in state
    a.apply(queue_frame(queues(vec![r], Vec::new())));
    let screen = a.screen(160, 40).join("\n");
    assert!(screen.contains("1m00s in state"), "{screen}");
    assert!(screen.contains("by lab/4242"), "{screen}");
}

/// **Who serves the queues, and how alive it is** — the lease's owner, build and heartbeat age;
/// a heartbeat past the lease's lifetime is STALE. And no lease at all is said.
#[test]
fn the_pane_says_who_serves_each_repository_and_a_stale_heartbeat() {
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    a.apply(queue_frame(queues(Vec::new(), Vec::new())));
    let screen = a.screen(160, 40).join("\n");
    assert!(
        screen.contains("letibot served by lab (pid 4242, harnessd 0.3.0) · heartbeat 2.0s ago"),
        "{screen}"
    );
    assert!(!screen.contains("STALE"), "{screen}");

    let mut stale = queues(Vec::new(), Vec::new());
    stale.leases[0].heartbeat_ms = 0;
    stale.at_ms = 200_000;
    a.apply(queue_frame(stale));
    let screen = a.screen(160, 40).join("\n");
    assert!(screen.contains("STALE"), "{screen}");

    let mut none = queues(Vec::new(), Vec::new());
    none.leases.clear();
    a.apply(queue_frame(none));
    let screen = a.screen(160, 40).join("\n");
    assert!(screen.contains("nothing is serving the queues"), "{screen}");
}

/// **A repository that never asked for a queue is one muted line, and nothing is counted for it**
/// (2026-10-26) — the operator's supervisor held fourteen such rows, each drawn as a blocked
/// target and counted *unjudged* on every edge, beside the one repository the queue served. The
/// daemon keeps them out of `targets` (`mergequeue::snapshot`) and names them here.
#[test]
fn a_repository_that_is_not_queued_is_one_muted_line_and_no_count() {
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let mut snap = queues(Vec::new(), Vec::new());
    snap.not_queued = vec![LETIBOT.into()];
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    let screen = a.screen(160, 40).join("\n");
    assert!(
        screen.contains("not queued: no `Merge gate` section in AGENTS.md — letibot"),
        "{screen}"
    );
    assert!(
        !screen.contains("unjudged") && !screen.contains("blocked"),
        "nothing is counted or alarmed for it: {screen}"
    );
}

/// **A red target is an emergency, first on the pane and on the edge** — and its review is the
/// first row of the review queue.
#[test]
fn a_red_target_is_drawn_at_emergency_severity_first() {
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let mut snap = queues(
        vec![
            queue_review_row("one", "waiting"),
            QueueReviewExt::target(queue_review_row("red-main-abc", "reviewing")),
        ],
        Vec::new(),
    );
    snap.targets
        .push(letibot_sessionlog::event::TargetHealthView {
            repository: "/home/dead/Projects/letibot/letibot".into(),
            target: "main".into(),
            sha: "abc1234567890".into(),
            state: "red".into(),
            checked_ms: 61_000,
            evidence: "`cargo test` failed".into(),
            gate_steps: Vec::new(),
        });
    a.apply(queue_frame(snap));
    let rows = a.screen(160, 40);
    let screen = rows.join("\n");
    assert!(screen.contains("‼ EMERGENCY"), "{screen}");
    assert!(screen.contains("`main` is RED at abc1234567"), "{screen}");
    let red_at = rows
        .iter()
        .position(|l| l.contains("RED TARGET"))
        .expect("the red tip's row");
    let one_at = rows
        .iter()
        .position(|l| l.contains("agent/one"))
        .expect("the branch's row");
    assert!(red_at < one_at, "the red tip's review is first: {screen}");
    assert!(
        screen.contains("‼ 1 red target(s)"),
        "and the edge says so: {screen}"
    );
}

trait QueueReviewExt {
    fn target(self) -> Self;
}

impl QueueReviewExt for letibot_sessionlog::event::QueueReview {
    fn target(mut self) -> Self {
        self.kind = "target".into();
        self.branch = "main".into();
        self.created_ms = 50_000;
        self
    }
}

/// **A snapshot that could not be read says so** — never drawn as an empty queue (defect 12).
#[test]
fn an_unreadable_snapshot_is_said_not_drawn_empty() {
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let mut snap = queues(Vec::new(), Vec::new());
    snap.unreadable = Some("could not be read: the review queue: database is locked".into());
    a.apply(queue_frame(snap));
    let screen = a.screen(160, 40).join("\n");
    assert!(screen.contains("database is locked"), "{screen}");
}

/// **The selected row shows its last moves**, and Enter opens it whole with every move — the
/// history the v23 pane had none of.
#[test]
fn the_selected_row_shows_its_moves_and_enter_opens_it_whole() {
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let mut snap = queues(vec![queue_review_row("one", "reviewing")], Vec::new());
    for (from, to, why) in [
        (None, "waiting", "put up for review"),
        (Some("waiting"), "reviewing", "claimed for review at abc123"),
    ] {
        snap.recent.push(letibot_sessionlog::event::QueueMove {
            queue: "review".into(),
            entry_id: "one".into(),
            repository: "/r".into(),
            from: from.map(str::to_string),
            to: to.into(),
            at_ms: 30_000,
            by: "lab/4242/1".into(),
            why: why.into(),
        });
    }
    a.apply(queue_frame(snap));
    let screen = a.screen(160, 40).join("\n");
    assert!(screen.contains("waiting → reviewing"), "{screen}");
    assert_eq!(a.key(Key::Enter), None);
    assert_eq!(a.queue_open.as_deref(), Some("one"));
    let screen = a.screen(160, 60).join("\n");
    assert!(screen.contains("its moves"), "{screen}");
    assert!(screen.contains("(new) → waiting"), "{screen}");
    assert!(screen.contains("make the widget blue"), "the ask: {screen}");
    a.key(Key::Esc);
    assert!(a.queue_open.is_none());
    assert!(a.is_open(Surface::Queue), "esc goes back to the queue");
}

/// **A merge row's gate steps draw in order, a timed-out step is its own outcome** (decision 9).
#[test]
fn a_merge_rows_gate_steps_draw_and_timed_out_is_its_own_word() {
    use letibot_sessionlog::event::MergeGateOutcome as O;
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let mut m = queue_merge_row("m-two", "timed_out");
    m.gate_steps = vec![
        gate_step("cargo clippy", O::Passed, "", 1_000),
        gate_step("cargo test", O::TimedOut, "still compiling", 1_800_000),
        gate_step("cargo build --release", O::NotRun, "", 0),
    ];
    a.apply(queue_frame(queues(Vec::new(), vec![m])));
    a.key(Key::Tab);
    let screen = a.screen(160, 40).join("\n");
    assert!(
        screen.contains("cargo test · TIMED OUT after 30m"),
        "{screen}"
    );
    assert!(
        screen.contains("cargo build --release · not run"),
        "{screen}"
    );
    assert!(screen.contains("timed out"), "{screen}");
}

/// **The verbs send `/queue VERB ID` for the row under the cursor, and a verb the daemon would
/// refuse is said, not sent** — approve over the reviewer on a rejected review (decision 5);
/// approve on a merge row is nothing.
#[test]
fn the_verbs_send_for_the_row_under_the_cursor_and_refuse_what_the_daemon_would() {
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    a.apply(queue_frame(queues(
        vec![queue_review_row("one", "rejected")],
        vec![queue_merge_row("m-two", "failed")],
    )));
    assert_eq!(
        a.key(Key::Char('a')),
        Some(Action::Slash {
            line: "queue approve one".into()
        })
    );
    assert_eq!(
        a.key(Key::Char('r')),
        Some(Action::Slash {
            line: "queue restart one".into()
        })
    );
    a.key(Key::Tab);
    assert_eq!(
        a.key(Key::Char('a')),
        None,
        "a merge row is approved already"
    );
    assert_eq!(
        a.key(Key::Char('r')),
        Some(Action::Slash {
            line: "queue restart m-two".into()
        })
    );
    // **`g` re-gates the row's TARGET** (R76 item 3), named with its repository.
    assert_eq!(
        a.key(Key::Char('g')),
        Some(Action::Slash {
            line: "queue regate /home/dead/Projects/letibot/letibot main".into()
        })
    );
    // A typed word is the composer's.
    a.set_composer("p");
    assert_eq!(a.key(Key::Char('d')), None);
    assert_eq!(a.editor.text(), "pd");
}

/// **A queue move asks for the snapshot again, open pane or not** (R92 item 5) — the edge draws
/// this session's repository's standing from the same snapshot, and a head that re-read only with
/// the pane open kept `‼ 1 red target(s)` on its edge after `main` went green. A burst of moves
/// is one read. FAILS before R92.5: a closed pane asked nothing.
#[test]
fn a_queue_move_asks_for_the_snapshot_again_whether_or_not_the_pane_is_open() {
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(queue_frame(queues(Vec::new(), Vec::new())));
    let moved = || {
        ServerFrame::Event(env(
            1,
            SessionEvent::QueueMoved {
                queue: "merge".into(),
                id: "m-two".into(),
                to: "landed".into(),
                why: "landed".into(),
            },
        ))
    };
    a.apply(moved());
    a.apply(moved());
    assert_eq!(
        a.take_actions(),
        vec![Action::ListMergeQueue],
        "closed: one read for a burst of moves"
    );
    a.command("queue");
    let _ = a.take_actions();
    a.apply(moved());
    a.apply(moved());
    assert_eq!(
        a.take_actions(),
        vec![Action::ListMergeQueue],
        "open: one read for a burst of moves"
    );
}

/// **The head asks for the queues when it is seated, not when the pane first opens** (R92 item
/// 5) — the root of *"not all heads still show the queue"*: the only ask for the snapshot was
/// opening `/queue`, so a head that never opened it never drew a standing on its edge. FAILS
/// before R92.5.
#[test]
fn an_attach_asks_for_the_queues_so_the_edge_has_a_standing() {
    let mut a = app();
    a.apply(hello("s", Vec::new(), Hub::new("s").snapshot()));
    assert!(
        a.take_actions().contains(&Action::ListMergeQueue),
        "the attach asks for the queues"
    );
    let mut snap = queues(vec![queue_review_row("one", "waiting")], Vec::new());
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    let screen = a.screen(120, 24).join("\n");
    assert!(
        screen.contains("1 in review"),
        "the edge, without the pane ever opened: {screen}"
    );
}

const LETIBOT: &str = "/home/dead/Projects/letibot/letibot";
const OTHER: &str = "/home/dead/Projects/pg-noop";

fn here_in(repo: &str) -> letibot_sessionlog::event::QueueHere {
    letibot_sessionlog::event::QueueHere {
        workspace: repo.into(),
        repository: Some(repo.into()),
        repositories: vec![repo.into()],
    }
}

fn in_repo<T>(mut row: T, repo: &str, set: impl Fn(&mut T, String)) -> T {
    set(&mut row, repo.into());
    row
}

fn target(repo: &str, state: &str) -> letibot_sessionlog::event::TargetHealthView {
    letibot_sessionlog::event::TargetHealthView {
        repository: repo.into(),
        target: "main".into(),
        sha: "abc1234567890".into(),
        state: state.into(),
        checked_ms: 61_000,
        evidence: format!("main is {state}"),
        gate_steps: Vec::new(),
    }
}

/// Two repositories' rows, and the daemon saying this session is in letibot.
fn two_repositories() -> letibot_sessionlog::event::QueueSnapshot {
    let mut snap = queues(
        vec![
            queue_review_row("mine", "waiting"),
            in_repo(queue_review_row("theirs", "blocked"), OTHER, |r, v| {
                r.repository = v
            }),
        ],
        vec![in_repo(
            queue_merge_row("m-theirs", "gating"),
            OTHER,
            |m, v| m.repository = v,
        )],
    );
    snap.targets.push(target(OTHER, "red"));
    snap.here = Some(here_in(LETIBOT));
    snap
}

/// **`/queue` is this session's repository; `/queue all` is every one** (R92 item 5) — the
/// operator's *"the /queue is still not project scoped"*. The pane, its rows, the edge and the
/// verbs' lines all follow the scope. FAILS before R92.5: every repository was drawn and
/// counted.
#[test]
fn the_queue_pane_and_the_edge_are_scoped_to_the_sessions_repository() {
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    a.apply(queue_frame(two_repositories()));
    let screen = a.screen(160, 40).join("\n");
    assert!(screen.contains("agent/mine"), "{screen}");
    assert!(!screen.contains("agent/theirs"), "{screen}");
    assert!(
        !screen.contains("EMERGENCY"),
        "another repository's red target is not this pane's emergency: {screen}"
    );
    assert!(screen.contains("this session's repository"), "{screen}");
    assert!(
        screen.contains("1 in review · 0 being merged · 0 parked")
            && !screen.contains("red target"),
        "the edge counts this repository only: {screen}"
    );

    // **Explicitly every repository** — and a verb sent from it carries the scope.
    assert_eq!(a.command("queue all"), None, "the open pane is re-cut");
    let screen = a.screen(160, 40).join("\n");
    assert!(screen.contains("agent/theirs"), "{screen}");
    assert!(screen.contains("EMERGENCY"), "{screen}");
    assert!(screen.contains("every repository"), "{screen}");
    assert!(
        screen.contains("1 in review · 0 being merged · 0 parked")
            && !screen.contains("red target(s)"),
        "the EDGE stays this session's repository's whatever the pane shows: {screen}"
    );
    let theirs = a
        .queue_rows()
        .iter()
        .position(|r| a.queue_row_id(*r) == "theirs")
        .expect("the other repository's row");
    a.queue_sel = theirs;
    assert_eq!(
        a.key(Key::Char('d')),
        Some(Action::Slash {
            line: "queue all rm theirs".into()
        }),
        "the daemon is told the scope the row was drawn in"
    );

    // **A bare `/queue` closes, and reopens on this session's repository.**
    a.command("queue");
    assert!(!a.is_open(Surface::Queue));
    a.command("queue");
    let screen = a.screen(160, 40).join("\n");
    assert!(!screen.contains("agent/theirs"), "{screen}");
}

/// **A session of no repository is told so and offered `/queue all`** — and its edge draws no
/// other project's standing.
#[test]
fn a_session_outside_every_repository_is_told_and_offered_all() {
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let mut snap = two_repositories();
    snap.here = Some(letibot_sessionlog::event::QueueHere {
        workspace: "/home/dead".into(),
        repository: None,
        repositories: Vec::new(),
    });
    a.apply(queue_frame(snap));
    let screen = a.screen(160, 40).join("\n");
    assert!(
        screen.contains("/home/dead is in no repository") && screen.contains("`/queue all`"),
        "{screen}"
    );
    assert!(!screen.contains("agent/"), "{screen}");
    assert!(
        !screen.contains("in review ·"),
        "no edge standing for a session of no repository: {screen}"
    );
    a.command("queue all");
    let screen = a.screen(160, 40).join("\n");
    assert!(screen.contains("agent/theirs"), "{screen}");
}

/// **An unjudged target is an alarm on the edge, and a green one clears it** (R92 item 5) —
/// blocked and timed-out targets land nothing either, and the target's own red-tip review stops
/// counting once the target is green. FAILS before R92.5: only `red` was counted.
#[test]
fn the_edge_alarms_on_an_unjudged_target_and_clears_when_it_is_green() {
    let mut a = app();
    a.session_id = "s1".into();
    for state in ["timed_out", "blocked"] {
        let mut snap = queues(Vec::new(), Vec::new());
        snap.targets.push(target(LETIBOT, state));
        snap.here = Some(here_in(LETIBOT));
        a.apply(queue_frame(snap));
        let screen = a.screen(160, 24).join("\n");
        assert!(
            screen.contains("⚠ 1 target(s) unjudged"),
            "{state}: {screen}"
        );
    }
    let mut snap = queues(
        vec![QueueReviewExt::target(queue_review_row(
            "red-main-abc",
            "waiting",
        ))],
        Vec::new(),
    );
    snap.targets.push(target(LETIBOT, "red"));
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap.clone()));
    let screen = a.screen(160, 24).join("\n");
    assert!(
        screen.contains("‼ 1 red target(s) · 1 in review"),
        "{screen}"
    );
    snap.targets[0].state = "green".into();
    a.apply(queue_frame(snap));
    let screen = a.screen(160, 24).join("\n");
    assert!(
        !screen.contains("red target") && !screen.contains("in review ·"),
        "green clears the alarm and the moot red-tip review: {screen}"
    );
}

/// **The edge counts the queues by what stands**, and says nothing when nothing does.
#[test]
fn the_edge_counts_the_queues_and_is_silent_when_nothing_stands() {
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(queue_frame(queues(
        vec![
            queue_review_row("a", "approved"),
            queue_review_row("b", "withdrawn"),
        ],
        vec![queue_merge_row("m-a", "landed")],
    )));
    let screen = a.screen(120, 24).join("\n");
    assert!(!screen.contains("in review"), "{screen}");
    a.apply(queue_frame(queues(
        vec![
            queue_review_row("a", "waiting"),
            queue_review_row("b", "blocked"),
        ],
        vec![
            queue_merge_row("m-c", "gating"),
            queue_merge_row("m-d", "timed_out"),
        ],
    )));
    let screen = a.screen(120, 24).join("\n");
    assert!(
        screen.contains("1 in review · 1 being merged · 2 parked"),
        "{screen}"
    );
}

/// **A click on a row selects it, and Enter opens THAT row.**
#[test]
fn a_click_on_a_queue_row_selects_the_row_drawn_there() {
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    a.apply(queue_frame(queues(
        vec![
            queue_review_row("one", "waiting"),
            queue_review_row("two", "waiting"),
            queue_review_row("three", "waiting"),
        ],
        Vec::new(),
    )));
    let rows = a.screen(140, 40);
    let at = rows
        .iter()
        .position(|l| l.contains("agent/three"))
        .expect("drawn") as u16;
    assert_eq!(a.key(Key::Click { x: 6, y: at }), None);
    assert!(
        a.screen(140, 40)
            .iter()
            .any(|l| l.contains('▸') && l.contains("agent/three")),
        "the click selected the row it landed on"
    );
    a.key(Key::Enter);
    assert_eq!(a.queue_open.as_deref(), Some("three"));
}

/// **The wheel and the page keys scroll the pane, not the conversation under it.**
#[test]
fn the_wheel_and_page_keys_scroll_the_queue_pane() {
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let rows = (0..30)
        .map(|i| queue_review_row(&format!("c{i}"), "waiting"))
        .collect();
    a.apply(queue_frame(queues(rows, Vec::new())));
    a.screen(100, 16);
    assert!(a.pane_len > a.pane_room);
    assert_eq!(a.key(Key::WheelDown), None);
    assert_eq!(a.pane_scroll, 3);
    assert!(a.following());
    a.key(Key::PageDown);
    assert!(a.pane_scroll > 3);
}

/// **The bar names the verbs** — generated from the `QUEUE` table.
#[test]
fn the_hint_line_names_the_person_verbs() {
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let bar = a.hint_bar(160);
    for want in ["a approve", "v veto", "d drop", "r restart"] {
        assert!(bar.contains(want), "the bar names `{want}`: {bar}");
    }
}

/// **A queued landing says what it waits for** (R92 item 8) — the repository's gate, by sha and
/// how long it has run, rather than sitting `queued` with no reason.
#[test]
fn a_running_gate_is_said_and_the_queued_landings_wait_for_it() {
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let mut gating = queue_merge_row("m-one", "gating");
    gating.rebased_sha = Some("fedcba9876543210".into());
    a.apply(queue_frame(queues(
        vec![],
        vec![gating, queue_merge_row("m-two", "queued")],
    )));
    assert_eq!(a.key(Key::Tab), None);
    let screen = a.screen(200, 50).join("\n");
    assert!(
        screen.contains(
            "landing `agent/one` onto `main` in letibot: gate running on fedcba9876 for 1m"
        ),
        "{screen}"
    );
    assert!(
        screen.contains("waits: gate running on fedcba9876 for 1m"),
        "the queued row says why: {screen}"
    );
}

/// **A folder that is one project of several repositories sees each of their queues** — the
/// operator's `~/Projects/stroppy-io`, whose repositories share the `stroppy-io` org: the edge and
/// the pane hold every one of them, and nothing of a repository outside it.
#[test]
fn a_one_project_folder_sees_the_queues_of_each_of_its_repositories() {
    let mut a = app();
    a.command("queue");
    let mut snap = two_repositories();
    snap.here = Some(letibot_sessionlog::event::QueueHere {
        workspace: "/home/dead/Projects".into(),
        repository: None,
        repositories: vec![LETIBOT.into(), OTHER.into()],
    });
    a.apply(queue_frame(snap));
    let f = a.queue_filter(&a.queue_scope);
    assert!(f.keeps(LETIBOT) && f.keeps(OTHER), "{f:?}");
    assert!(!f.keeps("/home/dead/Projects/elsewhere"), "{f:?}");
    let screen = a.screen(160, 40).join("\n");
    assert!(
        screen.contains("this session's repositories, one project"),
        "{screen}"
    );
    assert!(!screen.contains("is in no repository"), "{screen}");
}

/// **`o` goes into the reviewer's session** — the operator: *"there is also problem with /queue -
/// no way to reach reviewer session from here"*. From the list, and from a row opened whole; a row
/// nobody is reviewing says so.
#[test]
fn o_goes_into_the_reviewers_session() {
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let mut reviewing = queue_review_row("one", "reviewing");
    reviewing.reviewer = Some("s-gatekeeper-sub-7".into());
    a.apply(queue_frame(queues(
        vec![reviewing, queue_review_row("two", "waiting")],
        Vec::new(),
    )));
    a.screen(160, 40);
    let went = a.key(Key::Char('o'));
    assert!(
        matches!(&went, Some(Action::Switch(id)) if id == "s-gatekeeper-sub-7"),
        "{went:?}"
    );
    let mut b = app();
    b.session_id = "s1".into();
    b.command("queue");
    b.apply(queue_frame(queues(
        vec![queue_review_row("two", "waiting")],
        Vec::new(),
    )));
    b.screen(160, 40);
    assert_eq!(b.key(Key::Char('o')), None);
    assert!(
        b.notice
            .as_deref()
            .unwrap_or("")
            .contains("no reviewer working it"),
        "{:?}",
        b.notice
    );
}
