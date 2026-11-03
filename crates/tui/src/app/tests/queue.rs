//! The queue, hosted: the routing, the edge, and the feed the layer reads. The pane is the `queue`
//! layer's answer drawn through the production host, so a test asserts what the HOST owes it; the
//! layer's own policy is in the layer's tests.

use super::*;

/// **The head, with the real `queue` layer answering** - the pane's one renderer, through the
/// same host a production head builds for this session.
fn layer(mut a: App) -> App {
    a.session_id = "s1".into();
    *a.sessions_host.get_mut() = Some(crate::app::sessions_pane::Host::build(
        "s1",
        &std::env::temp_dir(),
    ));
    a
}

/// **The stop index of the row whose cells carry `needle`**, read from the view the last draw
/// kept - the same list the cursor and the verbs walk, so a test cannot aim a key at a row the
/// pane does not have.
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
    panic!("{needle} is not on the pane");
}

/// **`/queue` asks for the snapshot and draws nothing it was not sent** - and until one arrives
/// the host says it is asking, rather than letting the layer draw an empty queue.
#[test]
fn the_queue_pane_asks_for_the_queues_and_draws_what_comes_back() {
    let mut a = layer(app());
    assert_eq!(a.command("queue"), Some(Action::ListMergeQueue));
    assert!(a.is_open(Surface::Queue));
    let screen = a.screen(120, 30).join("\n");
    assert!(screen.contains("asking the daemon"), "{screen}");
    assert!(!screen.contains("idle"), "not drawn as empty: {screen}");

    a.apply(queue_frame(queues(
        vec![queue_review_row("one", "reviewing")],
        vec![queue_merge_row("m-two", "queued")],
    )));
    let screen = a.screen(140, 40).join("\n");
    assert!(screen.contains("agent/one"), "the layer drew it: {screen}");
    assert_eq!(a.command("queue"), None);
    assert!(!a.is_open(Surface::Queue));
}

/// **`tab` switches the view**, and the pane's header follows: the strip the layer draws says
/// which queue is drawn first, so the person can tell what `tab` did.
#[test]
fn tab_switches_between_the_review_queue_and_the_merge_queue() {
    let mut a = layer(app());
    a.command("queue");
    a.apply(queue_frame(queues(
        vec![queue_review_row("one", "waiting")],
        vec![queue_merge_row("m-two", "gating")],
    )));
    let screen = a.screen(140, 40).join("\n");
    assert!(
        screen.contains("1 in review · 1 to land"),
        "the drawn view leads: {screen}"
    );
    assert_eq!(a.key(Key::Tab), None);
    let screen = a.screen(140, 40).join("\n");
    assert!(
        screen.contains("1 to land · 1 in review"),
        "and follows the key: {screen}"
    );
    assert!(
        screen.contains("agent/two") && screen.contains("gating"),
        "{screen}"
    );
}

/// **A row names an owner only when the claim is not the serving daemon's** - the daemon holding
/// the repository's lease is the pane's own, and its name on every row was one of the fields the
/// operator read past.
#[test]
fn a_row_names_an_owner_only_from_elsewhere() {
    let mut a = layer(app());
    a.command("queue");
    let mut r = queue_review_row("one", "reviewing");
    r.owner = "elsewhere/999/1700000000000".into();
    a.apply(queue_frame(queues(vec![r.clone()], Vec::new())));
    let screen = a.screen(160, 40).join("\n");
    assert!(screen.contains("elsewhere/999"), "{screen}");

    // The fixture's lease is `lab/4242/1`'s, so a row IT claimed says nothing.
    let mut mine = r;
    mine.id = "two".into();
    mine.branch = "agent/two".into();
    mine.owner = "lab/4242/1".into();
    a.apply(queue_frame(queues(vec![mine], Vec::new())));
    let screen = a.screen(160, 40).join("\n");
    assert!(
        !screen.contains("lab/4242"),
        "the serving daemon's own claim is not said again: {screen}"
    );
}

/// **A repository that never asked for a queue is one muted line**, and nothing is counted for
/// it - the daemon keeps them out of `targets` and names them in `not_queued`.
#[test]
fn a_repository_that_is_not_queued_is_one_muted_line_and_no_count() {
    let mut a = layer(app());
    a.command("queue");
    let mut snap = queues(Vec::new(), Vec::new());
    snap.not_queued = vec![LETIBOT.into()];
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    let rows = a.screen(239, 61);
    let screen = rows.join("\n");
    assert!(
        screen.contains("it declares no `Merge gate` in its AGENTS.md"),
        "{screen}"
    );
    assert!(
        !screen.contains("unjudged") && !screen.contains("blocked"),
        "nothing is counted or alarmed for it: {screen}"
    );
}

/// **A red target is an alarm on the pane and on the edge**, and its own review reads as the
/// target it is about.
#[test]
fn a_red_target_is_an_alarm_and_its_review_reads_as_the_target() {
    let mut a = layer(app());
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
            running_gate: None,
        });
    a.apply(queue_frame(snap));
    let rows = a.screen(160, 40);
    let screen = rows.join("\n");
    assert!(screen.contains("main in letibot is red"), "the alarm: {screen}");
    assert!(screen.contains("`cargo test` failed"), "{screen}");
    assert!(
        screen.contains("main's own tip"),
        "the target's own review reads as a target: {screen}"
    );
    assert!(
        screen.contains("1 red"),
        "and the edge says so, in the target's own words: {screen}"
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

/// **A snapshot that could not be read says so** - never drawn as an empty queue.
#[test]
fn an_unreadable_snapshot_is_said_not_drawn_empty() {
    let mut a = layer(app());
    a.command("queue");
    let mut snap = queues(Vec::new(), Vec::new());
    snap.unreadable = Some("could not be read: the review queue: database is locked".into());
    a.apply(queue_frame(snap));
    let screen = a.screen(239, 61).join("\n");
    assert!(screen.contains("database is locked"), "{screen}");
}

/// **The selected row carries its last moves, and Enter opens it whole** - the moves are the
/// layer's faint lines under the picked row; the overlay is the layer's `detail` prose, hosted.
#[test]
fn the_selected_row_shows_its_moves_and_enter_opens_it_whole() {
    let mut a = layer(app());
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
    a.screen(140, 40);
    a.queue_sel = stop_of(&a, "agent/one");
    let screen = a.screen(140, 40).join("\n");
    assert!(
        screen.contains("waiting for a reviewer → a reviewer is reading it"),
        "{screen}"
    );
    assert_eq!(a.key(Key::Enter), None);
    assert_eq!(a.queue_open.as_deref(), Some("one"));
    let screen = a.screen(160, 60).join("\n");
    assert!(screen.contains("its moves"), "{screen}");
    assert!(screen.contains("(new) → waiting"), "{screen}");
    a.key(Key::Esc);
    assert!(a.queue_open.is_none());
    assert!(a.is_open(Surface::Queue), "esc goes back to the queue");
}

/// **A row that is waiting says why, and it has no attempts to its name** - the overlay draws
/// the failure whether or not an attempt was counted, which is the whole use of the pane.
#[test]
fn a_waiting_row_says_why_even_with_no_attempts_counted() {
    let mut a = layer(app());
    a.command("queue");
    let mut row = queue_review_row("one", "waiting");
    row.attempts = 0;
    row.failure = "WAITING FOR THE REVIEWER'S TREE: the reviewer's tree could not be made, so \
                   the review waits and is tried again — no attempt is spent on it: git clone: \
                   could not open '.git/objects/pack/tmp_pack_cgbKfF'"
        .into();
    a.apply(queue_frame(queues(vec![row], Vec::new())));
    a.screen(140, 40);
    a.queue_sel = stop_of(&a, "agent/one");
    assert_eq!(a.key(Key::Enter), None);
    assert_eq!(a.queue_open.as_deref(), Some("one"));
    let screen = a.screen(160, 60).join("\n");
    assert!(
        screen.contains("the reviewer's tree could not be made"),
        "a waiting row with a reason on it must draw the reason: {screen}"
    );
    assert!(
        screen.contains("tmp_pack_cgbKfF"),
        "and the reason whole, not its first clause: {screen}"
    );
    assert!(
        !screen.contains("attempts"),
        "zero attempts is not a fact worth a field: {screen}"
    );
}

/// **A merge row's gate steps draw in the overlay**, a timed-out step in its own words.
#[test]
fn a_merge_rows_gate_steps_draw_and_timed_out_is_its_own_word() {
    use letibot_sessionlog::event::MergeGateOutcome as O;
    let mut a = layer(app());
    a.command("queue");
    let mut m = queue_merge_row("m-two", "timed_out");
    m.gate_steps = vec![
        gate_step("cargo clippy", O::Passed, "", 1_000),
        gate_step("cargo test", O::TimedOut, "still compiling", 1_800_000),
        gate_step("cargo build --release", O::NotRun, "", 0),
    ];
    a.apply(queue_frame(queues(Vec::new(), vec![m])));
    // The timed-out landing is a finished row: open the fold so it is on the pane.
    a.queue_folds.insert("finished".into(), true);
    a.screen(140, 40);
    a.queue_sel = stop_of(&a, "agent/two");
    assert_eq!(a.key(Key::Enter), None);
    let screen = a.screen(160, 60).join("\n");
    assert!(
        screen.contains("timed out: cargo test · 30m"),
        "{screen}"
    );
    assert!(
        screen.contains("not run: cargo build --release · 0s"),
        "{screen}"
    );
    assert!(screen.contains("timed out"), "{screen}");
}

/// **The verbs send `/queue VERB ID` for the row under the cursor, and a verb the daemon would
/// refuse is said, not sent**.
#[test]
fn the_verbs_send_for_the_row_under_the_cursor_and_refuse_what_the_daemon_would() {
    let mut a = layer(app());
    a.command("queue");
    a.apply(queue_frame(queues(
        vec![
            queue_review_row("one", "waiting"),
            queue_review_row("rej", "rejected"),
        ],
        vec![queue_merge_row("m-two", "failed")],
    )));
    // Open the fold: the rejected review and the failed landing are finished rows the verbs
    // must still reach.
    a.queue_folds.insert("finished".into(), true);
    a.screen(140, 40);
    a.queue_sel = stop_of(&a, "agent/one");
    assert_eq!(
        a.key(Key::Char('a')),
        Some(Action::Slash {
            line: "queue approve one".into()
        })
    );
    a.queue_sel = stop_of(&a, "agent/rej");
    assert_eq!(
        a.key(Key::Char('r')),
        Some(Action::Slash {
            line: "queue restart rej".into()
        })
    );
    a.queue_sel = stop_of(&a, "agent/two");
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
    // **`g` re-gates the row's TARGET**, named with its repository.
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

/// **A queue move asks for the snapshot again, open pane or not** - the edge draws this session's
/// repository's standing from the same snapshot, and a head that re-read only with the pane open
/// kept `‼ 1 red target(s)` on its edge after `main` went green. A burst of moves is one read.
#[test]
fn a_queue_move_asks_for_the_snapshot_again_whether_or_not_the_pane_is_open() {
    let mut a = app();
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

/// **The head asks for the queues when it is seated, not when the pane first opens** - the root
/// of *"not all heads still show the queue"*: a head that never opened the pane never drew a
/// standing on its edge.
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

/// **The edge's snapshot is asked for again on the head's own clock** - a gate's step moves while
/// no row does, and the daemon only answers, so a standing nobody opened anything to see was the
/// one from the first answer for ever.
#[test]
fn the_edge_asks_for_a_fresh_snapshot_without_the_pane() {
    let mut a = app();
    a.apply(hello("s", Vec::new(), Hub::new("s").snapshot()));
    let _ = a.take_actions();
    a.now_ms = 100_000;
    let mut snap = queues(vec![queue_review_row("one", "waiting")], Vec::new());
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    assert!(!a.is_open(Surface::Queue), "the pane was never opened");

    // Inside the window: the answer just arrived, so nothing is asked.
    a.clock(100_000 + crate::ui::panes::queue::REFRESH_MS - 1);
    assert!(
        a.take_actions().is_empty(),
        "a snapshot a moment old is not asked for again"
    );
    // A window later, and the edge asks for the standing it would otherwise keep drawing.
    a.clock(100_000 + crate::ui::panes::queue::REFRESH_MS);
    assert_eq!(a.take_actions(), vec![Action::ListMergeQueue]);
    // And once, not once a tick: the ask is on the same clock the pane's own refresh is.
    a.clock(100_000 + crate::ui::panes::queue::REFRESH_MS + 1);
    assert!(a.take_actions().is_empty(), "asked twice in one window");
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
        running_gate: None,
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

/// **`/queue` is this session's repository; `/queue all` is every one** - the pane, the edge and
/// the verbs' lines all follow the scope.
#[test]
fn the_queue_pane_and_the_edge_are_scoped_to_the_sessions_repository() {
    let mut a = layer(app());
    a.command("queue");
    a.apply(queue_frame(two_repositories()));
    let rows = a.screen(160, 40);
    let screen = rows.join("\n");
    assert!(screen.contains("agent/mine"), "{screen}");
    assert!(!screen.contains("agent/theirs"), "{screen}");
    assert!(
        !screen.contains("main in pg-noop is red"),
        "another repository's red target is not this pane's emergency: {screen}"
    );
    assert!(screen.contains("this session's repository"), "{screen}");
    assert!(
        edge_row(&rows).contains("1 in review") && !edge_row(&rows).contains("red"),
        "the edge counts this repository only: {:?}",
        edge_row(&rows)
    );

    // **Explicitly every repository** - and a verb sent from it carries the scope.
    assert_eq!(a.command("queue all"), None, "the open pane is re-cut");
    a.screen(140, 40);
    let screen = a.screen(160, 40).join("\n");
    assert!(screen.contains("agent/theirs"), "{screen}");
    assert!(screen.contains("main in pg-noop is red"), "{screen}");
    assert!(screen.contains("every repository"), "{screen}");
    assert!(
        edge_row(&a.screen(160, 40)).contains("1 in review")
            && !edge_row(&a.screen(160, 40)).contains("red"),
        "the EDGE stays this session's repository's whatever the pane shows"
    );
    a.queue_sel = stop_of(&a, "agent/theirs");
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

/// **A session of no repository is told so and offered `/queue all`** - and its edge draws no
/// other project's standing.
#[test]
fn a_session_outside_every_repository_is_told_and_offered_all() {
    let mut a = layer(app());
    a.command("queue");
    let mut snap = two_repositories();
    snap.here = Some(letibot_sessionlog::event::QueueHere {
        workspace: "/home/dead".into(),
        repository: None,
        repositories: Vec::new(),
    });
    a.apply(queue_frame(snap));
    let rows = a.screen(160, 40);
    let screen = rows.join("\n");
    assert!(
        screen.contains("/home/dead is in no repository") && screen.contains("`/queue all`"),
        "{screen}"
    );
    assert!(!screen.contains("agent/mine"), "{screen}");
    assert!(
        !edge_row(&rows).contains("in review") && !edge_row(&rows).contains(" red"),
        "no edge standing for a session of no repository: {:?}",
        edge_row(&rows)
    );
    a.command("queue all");
    let screen = a.screen(160, 40).join("\n");
    assert!(screen.contains("agent/theirs"), "{screen}");
}

/// **An unjudged target is an alarm on the edge, and a green one clears it** - and the target's
/// own red-tip review stops counting once the target is green.
#[test]
fn the_edge_alarms_on_an_unjudged_target_and_clears_when_it_is_green() {
    let mut a = app();
    for state in ["timed_out", "blocked"] {
        let mut snap = queues(Vec::new(), Vec::new());
        snap.targets.push(target(LETIBOT, state));
        snap.here = Some(here_in(LETIBOT));
        a.apply(queue_frame(snap));
        let screen = a.screen(160, 24).join("\n");
        assert!(screen.contains("1 unjudged"), "{state}: {screen}");
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
    assert!(screen.contains("1 red · 1 in review"), "{screen}");
    snap.targets[0].state = "green".into();
    a.apply(queue_frame(snap));
    let rows = a.screen(160, 24);
    assert!(
        !edge_row(&rows).contains(" red") && !edge_row(&rows).contains("in review"),
        "green clears the alarm and the moot red-tip review: {:?}",
        edge_row(&rows)
    );
}

/// **The edge counts the queues by what stands**, and says nothing when nothing does.
#[test]
fn the_edge_counts_the_queues_and_is_silent_when_nothing_stands() {
    let mut a = app();
    // **Nothing that stands** - an approved review is the merge queue's business now, and a
    // rejected one, a withdrawn one and a landing that moved on are the queue's past.
    a.apply(queue_frame(queues(
        vec![
            queue_review_row("a", "approved"),
            queue_review_row("b", "rejected"),
            queue_review_row("c", "withdrawn"),
        ],
        vec![
            queue_merge_row("m-a", "withdrawn"),
            queue_merge_row("m-b", "moved"),
        ],
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
        screen.contains("queue: 1 in review · 1 to land · 2 stuck"),
        "{screen}"
    );
}

/// **A landing is a standing, and the zeros are not** - a queue that landed two branches today
/// and holds nothing else still has something to say, so the words are drawn on the count alone.
#[test]
fn the_edge_says_the_four_columns_and_a_landing_stands_on_its_own() {
    let mut a = app();
    a.apply(queue_frame(queues(
        Vec::new(),
        vec![
            queue_merge_row("m-a", "landed"),
            queue_merge_row("m-b", "landed"),
        ],
    )));
    let screen = a.screen(140, 24).join("\n");
    assert!(screen.contains("queue: 2 done"), "{screen}");
    assert!(!screen.contains("0 in review"), "a zero is drawn: {screen}");
}

/// **A terminal row inflates no live column**: a `rejected` review is terminal, so it is not
/// stuck; a `failed` landing is the reader's to move, so it is.
#[test]
fn a_rejected_review_is_not_stuck_and_a_failed_landing_is() {
    let mut a = app();
    a.apply(queue_frame(queues(
        vec![
            queue_review_row("one", "rejected"),
            queue_review_row("two", "withdrawn"),
        ],
        vec![queue_merge_row("m-one", "failed")],
    )));
    let screen = a.screen(140, 24).join("\n");
    assert!(screen.contains("1 stuck"), "{screen}");
    assert_eq!(
        a.queue_standings().stuck,
        1,
        "the failed landing is the one"
    );

    // **Take the landing away and the edge goes quiet.**
    a.apply(queue_frame(queues(
        vec![
            queue_review_row("one", "rejected"),
            queue_review_row("two", "withdrawn"),
        ],
        Vec::new(),
    )));
    let rows = a.screen(140, 24);
    assert!(
        !edge_row(&rows).contains("stuck") && !edge_row(&rows).contains("in review"),
        "a terminal row drew a standing: {:?}",
        edge_row(&rows)
    );
    assert_eq!(a.queue_standings().stuck, 0);
}

/// **`landed` is the last day, not for ever** - the same window the edge's landed column uses,
/// and the stamp is the row's own: `state_ms` is when the store moved it to `landed`.
#[test]
fn a_landing_older_than_the_recent_window_is_not_counted() {
    let mut a = app();
    // **A snapshot ten days old on its own clock**, so that a landing CAN be outside the window.
    let mut snap = queues(Vec::new(), vec![queue_merge_row("m-old", "landed")]);
    snap.at_ms = 10 * crate::ui::panes::queue::RECENT_MS;
    snap.merges[0].state_ms = snap.at_ms - (crate::ui::panes::queue::RECENT_MS + 1_000);
    a.apply(queue_frame(snap));
    let screen = a.screen(140, 24).join("\n");
    assert!(
        !screen.contains("done"),
        "a landing outside the window is not today's work: {screen}"
    );

    // **And one second inside it is** - the boundary is the window's, not the fixture's.
    let mut snap = queues(Vec::new(), vec![queue_merge_row("m-new", "landed")]);
    snap.at_ms = 10 * crate::ui::panes::queue::RECENT_MS;
    snap.merges[0].state_ms = snap.at_ms - (crate::ui::panes::queue::RECENT_MS - 1_000);
    a.apply(queue_frame(snap));
    let screen = a.screen(140, 24).join("\n");
    assert!(screen.contains("1 done"), "{screen}");
}

/// **A red target still leads at its own severity**, and neither the word nor the role is a
/// function of how many columns follow it.
#[test]
fn a_red_target_still_leads_the_grown_columns_at_its_own_severity() {
    let mut a = app();
    let mut snap = queues(Vec::new(), vec![queue_merge_row("m-a", "landed")]);
    snap.targets.push(target(LETIBOT, "red"));
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    let screen = a.screen(160, 24).join("\n");
    assert!(screen.contains("1 red · 1 done"), "{screen}");
    assert_eq!(
        a.queue_standings().role(),
        rano::style::Role::Failure,
        "the emergency is still the severity of the whole line"
    );
}

// -- what the line says when the news is a TARGET's --------------------------------------
//
// All four columns can be zero while a red target is the only live fact, and a zero is not news.

/// **A repository whose only live fact is a red `main` reads `1 red`** - the four columns have
/// nothing to say and are not drawn.
#[test]
fn a_red_target_alone_reads_as_one_red_and_no_zeros() {
    let mut a = app();
    let mut snap = queues(
        vec![
            queue_review_row("a", "approved"),
            queue_review_row("b", "rejected"),
            queue_review_row("c", "withdrawn"),
        ],
        vec![
            queue_merge_row("m-a", "landed"),
            queue_merge_row("m-b", "withdrawn"),
        ],
    );
    // The two landed rows are ten days old, so even `landed` has nothing to say today.
    snap.at_ms = 10 * crate::ui::panes::queue::RECENT_MS;
    for m in &mut snap.merges {
        m.state_ms = 1_000;
    }
    snap.targets.push(target(LETIBOT, "red"));
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    let rows = a.screen(120, 24);
    let edge = edge_row(&rows);
    assert!(edge.contains("1 red"), "{edge:?}");
    for zero in ["0 in review", "0 to land", "0 stuck", "0 landed"] {
        assert!(!edge.contains(zero), "a zero is drawn — {zero} in {edge:?}");
    }
    assert_eq!(a.queue_standings().words().as_deref(), Some("queue: 1 red"));
    assert_eq!(
        a.queue_standings().role(),
        rano::style::Role::Failure,
        "the red target is the register a stuck row is drawn in"
    );
    // **And it is the badge**: the pane was never opened, so this is what a reader's edge says.
    assert!(!a.is_open(Surface::Queue));
}

/// **A red target and a stuck landing are both said** - neither hides the other, and no zero
/// between them.
#[test]
fn a_red_target_and_a_stuck_landing_are_both_said() {
    let mut a = app();
    let mut snap = queues(Vec::new(), vec![queue_merge_row("m-a", "failed")]);
    snap.targets.push(target(LETIBOT, "red"));
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    let rows = a.screen(120, 24);
    let edge = edge_row(&rows);
    assert!(edge.contains("1 red · 1 stuck"), "{edge:?}");
    assert!(!edge.contains("in review"), "{edge:?}");
    assert!(!edge.contains("landed"), "{edge:?}");
}

/// **A green target adds no word**, and the columns read as they always have.
#[test]
fn an_all_green_repository_reads_the_columns_and_no_target_word() {
    let mut a = app();
    let mut snap = queues(
        vec![queue_review_row("one", "waiting")],
        vec![queue_merge_row("m-a", "gating")],
    );
    snap.targets.push(target(LETIBOT, "green"));
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    let rows = a.screen(120, 24);
    let edge = edge_row(&rows);
    assert!(edge.contains("queue: 1 in review · 1 to land"), "{edge:?}");
    assert!(
        !edge.contains("red"),
        "a green target says nothing: {edge:?}"
    );
    assert!(!edge.contains("unjudged"), "{edge:?}");
    assert!(!edge.contains("stuck"), "{edge:?}");
    assert!(!edge.contains("landed"), "{edge:?}");
    assert_eq!(
        a.queue_standings().words().as_deref(),
        Some("queue: 1 in review · 1 to land")
    );
}

/// The longest line this head can draw fits an 80-column edge: both target words and all four
/// columns non-zero is 70 cells, and the composer's edge is 76 inside the gutters - which is why
/// the day's successes say `done` and the emergency leads the line.
#[test]
fn the_longest_standings_fit_on_an_eighty_column_edge() {
    let mut a = app();
    let mut snap = queues(
        vec![queue_review_row("one", "waiting")],
        vec![
            queue_merge_row("m-a", "gating"),
            queue_merge_row("m-b", "failed"),
            queue_merge_row("m-c", "landed"),
            queue_merge_row("m-d", "landed"),
        ],
    );
    snap.targets.push(target(LETIBOT, "red"));
    snap.targets.push(target(LETIBOT, "timed_out"));
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    let rows = a.screen(80, 24);
    let edge = edge_row(&rows);
    let words = "queue: 1 red · 1 unjudged · 1 in review · 1 to land · 1 stuck · 2 done";
    assert!(edge.contains(words), "the whole line at 80: {edge:?}");
    assert_eq!(
        line_width(&edge),
        78,
        "the edge is the 80-column edge's row"
    );
    assert_eq!(words.chars().count(), 70, "the premise: seventy cells");
    // **Drawn whole means clickable whole** - the record is the drawn span's own columns.
    let rect = standings_rect(&a);
    assert_eq!(column_of(&edge, words), rect.x, "{edge:?}");
    assert_eq!(rect.w, 70, "the target is the whole line");
    assert_eq!(
        a.key(Key::Click {
            x: rect.x as u16,
            y: rect.y as u16
        }),
        Some(Action::ListMergeQueue),
        "the line is a door at 80 columns too"
    );
}

// -- the gate, on the edge a person reads without opening anything ---------------------
//
// `1 being merged` is true of every landing at every moment; the gate's step is what makes it news.

/// A `GATING` target whose gate is on `step` of `of` - the snapshot a head is sent while it runs.
fn gating_target(
    repo: &str,
    step: usize,
    of: usize,
) -> letibot_sessionlog::event::TargetHealthView {
    let mut t = target(repo, "gating");
    t.running_gate = Some(letibot_sessionlog::event::QueueRunningGate {
        command: "cargo test --offline --workspace --no-fail-fast".into(),
        step,
        of,
        started_ms: 1_000,
        tail: "building\n".into(),
        log_bytes: 36 * 1024,
    });
    t
}

/// The edge says which step of the gate is running, at 80 columns, where the merge column used to
/// say only `1 being merged` - and it says it without fusing the two queues: the step rides in
/// parentheses beside the merge queue's own word instead of standing in for it.
#[test]
fn the_edge_names_the_gates_step_at_eighty_columns() {
    let mut a = app();
    let mut snap = queues(
        Vec::new(),
        vec![
            queue_merge_row("m-a", "gating"),
            queue_merge_row("m-b", "queued"),
        ],
    );
    snap.targets.push(gating_target(LETIBOT, 3, 4));
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));

    // The words first, because they are what the line is made of.
    assert_eq!(
        a.queue_standings().words().as_deref(),
        Some("queue: 2 to land (gate step 3 of 4)"),
        "the merge queue's own word, with the gate's step beside it"
    );
    let rows = a.screen(80, 24);
    let edge = edge_row(&rows);
    assert!(
        edge.contains("queue: 2 to land (gate step 3 of 4)"),
        "drawn whole at eighty columns: {edge:?}"
    );
    // **And it is still the door** - the span the frame drew is the span a click acts on.
    let words = "queue: 2 to land (gate step 3 of 4)";
    let rect = standings_rect(&a);
    assert_eq!(column_of(&edge, words), rect.x, "{edge:?}");
    assert_eq!(
        rect.w,
        words.chars().count(),
        "the target is the words' width"
    );
}

/// **A target that is not gating reads as it always did** - a green target leaves `1 to land`
/// alone, and a `gating` target whose snapshot carries no step does not invent one.
#[test]
fn a_target_that_is_not_gating_keeps_the_old_word() {
    let mut a = app();
    let mut snap = queues(Vec::new(), vec![queue_merge_row("m-a", "gating")]);
    snap.targets.push(target(LETIBOT, "green"));
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    assert_eq!(
        a.queue_standings().words().as_deref(),
        Some("queue: 1 to land")
    );
    assert!(edge_row(&a.screen(120, 24)).contains("queue: 1 to land"));

    // A gate between two steps: the target is `gating` and there is no step to name.
    let mut a = app();
    let mut snap = queues(Vec::new(), vec![queue_merge_row("m-a", "gating")]);
    snap.targets.push(target(LETIBOT, "gating"));
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    assert_eq!(
        a.queue_standings().words().as_deref(),
        Some("queue: 1 to land"),
        "no step is running, so none is claimed"
    );
}

// -- the edge's standings, as a click target -------------------------------------------
//
// The standings ride every screen, so the click is the only queue door a reader finds without `/queue`.

/// **The composer's bottom edge** - the row the standings are drawn on, and the row a reader
/// who never opens the pane reads them off.
fn edge_row(rows: &[String]) -> String {
    rows.iter()
        .rev()
        .find(|l| l.contains('╰'))
        .expect("the composer's bottom edge")
        .clone()
}

/// **The standings' rectangle, as the last frame recorded it.**
fn standings_rect(a: &App) -> crate::app::hits::Rect {
    a.hits
        .iter()
        .find(|h| h.target == Target::QueueStandings)
        .map(|h| h.rect)
        .unwrap_or_else(|| {
            panic!(
                "no standings target on the frame: {:?}",
                a.hits.iter().map(|h| h.target).collect::<Vec<_>>()
            )
        })
}

/// **The column a drawn row carries `needle` in**, counted in cells - the record's own
/// arithmetic done independently, so the two can disagree and say so.
fn column_of(row: &str, needle: &str) -> usize {
    let cells = crate::app::keyhints::cells(row);
    let want: Vec<char> = needle.chars().collect();
    (0..=cells.len() - want.len())
        .find(|i| cells[*i..*i + want.len()] == want[..])
        .unwrap_or_else(|| panic!("{needle:?} is not drawn on {row:?}"))
}

/// **A click on the standings opens the merge queue** - `/queue`'s act, through the verb's own
/// door, and the hover mark covers exactly the words the frame drew.
#[test]
fn a_click_on_the_standings_opens_the_queue() {
    let mut a = app();
    let mut snap = queues(vec![queue_review_row("one", "waiting")], Vec::new());
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    let screen = a.screen(120, 24);
    let rect = standings_rect(&a);
    let words = "queue: 1 in review";
    // The record and the glass agree — the rectangle IS the span the frame drew, gutter and
    // all — or the click below proves nothing.
    let row = screen
        .get(rect.y)
        .unwrap_or_else(|| panic!("the recorded row is not in the frame: {rect:?}"));
    assert_eq!(
        column_of(row, words),
        rect.x,
        "the target is not where the words are: {row:?}"
    );
    assert_eq!(
        rect.w,
        words.chars().count(),
        "the target is not the words' width"
    );

    // **The hover lights the drawn span and nothing beside it.**
    a.hover_at(rect.x as u16 + 1, rect.y as u16);
    assert_eq!(
        a.hovered.as_ref().map(|h| h.rows.clone()),
        Some(vec![(rect.y, Some((rect.x, rect.x + rect.w)))]),
        "the hover does not cover the span the frame drew"
    );

    assert_eq!(
        a.key(Key::Click {
            x: rect.x as u16 + 1,
            y: rect.y as u16
        }),
        Some(Action::ListMergeQueue),
        "the click ran `/queue`'s act, table and all"
    );
    assert_eq!(
        a.top_surface(),
        Some(Surface::Queue),
        "and the pane it opened is the queue's"
    );
}

/// **The click still lands after the words grew a column** - the record is read off the drawn
/// row (`screen::drawn_span`) rather than rebuilt from the counts, so the longest form this
/// head can draw is the case that proves it.
#[test]
fn the_standings_target_covers_the_words_they_grew_into() {
    let mut a = app();
    let mut snap = queues(
        vec![queue_review_row("one", "waiting")],
        vec![
            queue_merge_row("m-a", "gating"),
            queue_merge_row("m-b", "failed"),
            queue_merge_row("m-c", "landed"),
            queue_merge_row("m-d", "landed"),
        ],
    );
    snap.targets.push(target(LETIBOT, "red"));
    snap.targets.push(target(LETIBOT, "timed_out"));
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    let screen = a.screen(200, 30);
    let rect = standings_rect(&a);
    let words = "queue: 1 red · 1 unjudged · 1 in review · 1 to land · 1 stuck · 2 done";
    let row = screen
        .get(rect.y)
        .unwrap_or_else(|| panic!("the recorded row is not in the frame: {rect:?}"));
    assert_eq!(
        column_of(row, words),
        rect.x,
        "the target is not where the frame drew the four columns: {row:?}"
    );
    assert_eq!(
        rect.w,
        words.chars().count(),
        "the target is not their width"
    );
    assert_eq!(
        a.key(Key::Click {
            x: rect.x as u16 + 1,
            y: rect.y as u16
        }),
        Some(Action::ListMergeQueue),
        "the queue's door, on the words this head draws now"
    );
    assert_eq!(a.top_surface(), Some(Surface::Queue));
}

/// A click one cell outside the standings asks for nothing: the edge around the words is furniture
/// and a click on it is a click nobody claimed. Aimed at the GLASS and not at the record, so a
/// record one column too wide or too far left fails here rather than agreeing with itself.
#[test]
fn a_click_one_cell_outside_the_standings_asks_for_nothing() {
    let mut a = app();
    let mut snap = queues(vec![queue_review_row("one", "waiting")], Vec::new());
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    let screen = a.screen(120, 24);
    let rect = standings_rect(&a);
    let words = "queue: 1 in review";
    let row = screen
        .get(rect.y)
        .expect("the recorded row is in the frame");
    let drawn = column_of(row, words);
    assert_eq!(drawn, rect.x, "the premise: the record is the drawn span");
    // The separator before the words, and the first cell after the last drawn character.
    for x in [drawn.saturating_sub(1), drawn + words.chars().count()] {
        assert_eq!(
            a.key(Key::Click {
                x: x as u16,
                y: rect.y as u16
            }),
            None,
            "a click at {x} acted — one cell outside the drawn span is nobody's"
        );
        assert!(!a.is_open(Surface::Queue), "the queue opened from {x}");
    }
}

/// **The two target alarms carry their own word, and the click still opens the queue.**
#[test]
fn a_click_on_the_standings_with_an_alarm_prefix_opens_the_queue() {
    for (state, prefix) in [("timed_out", "queue: 1 unjudged"), ("red", "queue: 1 red")] {
        let mut a = app();
        let mut snap = queues(Vec::new(), Vec::new());
        snap.targets.push(target(LETIBOT, state));
        snap.here = Some(here_in(LETIBOT));
        a.apply(queue_frame(snap));
        let screen = a.screen(120, 24);
        let rect = standings_rect(&a);
        let words = prefix.to_string();
        let row = screen
            .get(rect.y)
            .expect("the recorded row is in the frame");
        assert!(row.contains(prefix), "{state}: the premise — {row:?}");
        assert_eq!(
            column_of(row, &words),
            rect.x,
            "{state}: the target is not the span the frame drew: {row:?}"
        );
        assert_eq!(
            rect.w,
            words.chars().count(),
            "{state}: the target is not the word's width"
        );
        assert_eq!(
            a.key(Key::Click {
                x: rect.x as u16,
                y: rect.y as u16
            }),
            Some(Action::ListMergeQueue),
            "{state}: a click on the alarm prefix is a click on the standings"
        );
        assert_eq!(a.top_surface(), Some(Surface::Queue), "{state}");
    }
}

/// **A click while the queue is open closes it** - the click is `/queue`'s spelling and the verb
/// TOGGLES: the standings stay drawn on the queue's own edge, and pressing that door again
/// leaves, exactly as the count labels do for their panes.
#[test]
fn a_click_on_the_standings_of_an_open_queue_closes_it() {
    let mut a = app();
    let mut snap = queues(vec![queue_review_row("one", "waiting")], Vec::new());
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    assert_eq!(a.command("queue"), Some(Action::ListMergeQueue));
    assert!(a.is_open(Surface::Queue), "the premise: the pane is up");
    a.screen(120, 24);
    let rect = standings_rect(&a);
    assert_eq!(
        a.key(Key::Click {
            x: rect.x as u16 + 1,
            y: rect.y as u16
        }),
        None,
        "closing asks the daemon for nothing"
    );
    assert!(
        !a.is_open(Surface::Queue),
        "the second press left, as `/queue` does"
    );
}

/// **The edge's standings are a target on whatever screen is up** - a click from the subagents
/// pane opens the queue just as one from the conversation does.
#[test]
fn the_standings_are_a_target_from_another_pane_too() {
    let mut a = app();
    let mut snap = queues(vec![queue_review_row("one", "waiting")], Vec::new());
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    a.key(Key::CtrlG);
    assert_eq!(a.top_surface(), Some(Surface::Subagents), "the premise");
    a.screen(120, 24);
    let rect = standings_rect(&a);
    assert_eq!(
        a.key(Key::Click {
            x: rect.x as u16 + 1,
            y: rect.y as u16
        }),
        Some(Action::ListMergeQueue),
        "the standings are the queue's door from another pane"
    );
    assert_eq!(a.top_surface(), Some(Surface::Queue));
}

/// **A narrow edge keeps no target for standings it cut** - the edge truncates from the right
/// and the standings are the last thing on it, so a terminal too narrow to hold the words draws
/// a piece of them and records nothing.
#[test]
fn a_narrow_edge_records_no_standings_target_for_words_it_cut() {
    let mut a = app();
    let mut snap = queues(
        vec![queue_review_row("one", "waiting")],
        vec![
            queue_merge_row("m-a", "gating"),
            queue_merge_row("m-b", "failed"),
            queue_merge_row("m-c", "landed"),
        ],
    );
    snap.targets.push(target(LETIBOT, "red"));
    snap.targets.push(target(LETIBOT, "timed_out"));
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    let screen = a.screen(30, 24);
    let edge = screen
        .iter()
        .find(|l| l.contains('╰'))
        .expect("the composer's bottom edge");
    assert!(
        !edge.contains("landed"),
        "the premise: thirty columns cut the words: {edge:?}"
    );
    assert!(
        a.hits.iter().all(|h| h.target != Target::QueueStandings),
        "a target exists for standings the edge truncated: {:?}",
        a.hits.iter().map(|h| h.target).collect::<Vec<_>>()
    );
}

/// **A queue that stands nothing has no standings, and the edge records no target for them.**
#[test]
fn a_head_whose_queue_stands_nothing_records_no_standings_target() {
    let mut a = app();
    a.apply(queue_frame(queues(
        vec![queue_review_row("a", "approved")],
        vec![queue_merge_row("m-a", "moved")],
    )));
    let screen = a.screen(120, 24);
    assert!(
        !screen.iter().any(|l| l.contains("in review")),
        "the premise: nothing stands, so nothing is drawn: {}",
        screen.join("\n")
    );
    assert!(
        a.hits.iter().all(|h| h.target != Target::QueueStandings),
        "a target exists for words the edge did not draw: {:?}",
        a.hits.iter().map(|h| h.target).collect::<Vec<_>>()
    );
    // And behaviourally: a sweep of the edge's own row opens nothing.
    let y = screen
        .iter()
        .position(|l| l.contains('╰'))
        .expect("the composer's bottom edge") as u16;
    for x in (0..120u16).step_by(3) {
        assert_eq!(a.key(Key::Click { x, y }), None, "a click at {x} acted");
    }
    assert!(!a.is_open(Surface::Queue), "some pane opened anyway");
}

/// **A click on a row opens THAT row, and so does Enter on the row the cursor is on.**
#[test]
fn a_click_on_a_queue_row_opens_the_row_drawn_there() {
    let mut a = layer(app());
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
    assert_eq!(
        a.queue_open.as_deref(),
        Some("three"),
        "the click opened the row it landed on"
    );
    assert_eq!(a.top_surface(), Some(Surface::QueueEntry));
    // **And the keyboard reaches the same row** - the cursor stayed where the click put it.
    a.key(Key::Esc);
    a.key(Key::Enter);
    assert_eq!(a.queue_open.as_deref(), Some("three"));
}

/// **The wheel and the page keys scroll the pane, not the conversation under it.**
#[test]
fn the_wheel_and_page_keys_scroll_the_queue_pane() {
    let mut a = layer(app());
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

/// **The bar names the verbs in the words a person uses** - generated from the `QUEUE` table, so a
/// key cannot be advertised with a word the pane no longer uses (`rm` is `withdraw` now).
#[test]
fn the_hint_line_names_the_person_verbs() {
    let mut a = layer(app());
    a.command("queue");
    let bar = a.hint_bar(160);
    for want in ["a approve", "v veto", "d withdraw", "r restart"] {
        assert!(bar.contains(want), "the bar names `{want}`: {bar}");
    }
}

/// **A folder that is one project of several repositories sees each of their queues** - the
/// edge and the pane hold every one of them, and nothing of a repository outside it.
#[test]
fn a_one_project_folder_sees_the_queues_of_each_of_its_repositories() {
    let mut a = layer(app());
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

/// **`o` goes into the reviewer's session** - from the list, and from a row opened whole; a row
/// nobody is reviewing says so.
#[test]
fn o_goes_into_the_reviewers_session() {
    let mut a = layer(app());
    a.command("queue");
    let mut reviewing = queue_review_row("one", "reviewing");
    reviewing.reviewer = Some("s-gatekeeper-sub-7".into());
    a.apply(queue_frame(queues(
        vec![reviewing, queue_review_row("two", "waiting")],
        Vec::new(),
    )));
    a.screen(160, 40);
    a.queue_sel = stop_of(&a, "agent/one");
    let went = a.key(Key::Char('o'));
    assert!(
        matches!(&went, Some(Action::Switch(id)) if id == "s-gatekeeper-sub-7"),
        "{went:?}"
    );
    let mut b = layer(app());
    b.command("queue");
    b.apply(queue_frame(queues(
        vec![queue_review_row("two", "waiting")],
        Vec::new(),
    )));
    b.screen(160, 40);
    b.queue_sel = stop_of(&b, "agent/two");
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

/// `d` is offered on a `gating` landing and not on one that has landed: taking it out of line is the
/// person's answer to a gate that is going nowhere, so the offer belongs where it means something.
/// The daemon agrees - `verbs::withdraw_merge` takes a `gating` row and nothing else.
#[test]
fn withdraw_is_offered_on_a_gating_landing_and_not_on_a_landed_one() {
    use crate::ui::panes::queue::{Selected, Verb, verb_moves};
    let gating = queue_merge_row("m-gating", "gating");
    assert_eq!(
        verb_moves(Verb::Rm, &Selected::Merge(&gating)),
        Ok(()),
        "a running gate can be taken out of line"
    );
    let landed = queue_merge_row("m-landed", "landed");
    assert!(
        verb_moves(Verb::Rm, &Selected::Merge(&landed)).is_err(),
        "a landed row has nothing to withdraw"
    );
    // **`restart` keeps its own ground** - a running gate is not a parked row, and a failed one is.
    assert!(
        verb_moves(Verb::Restart, &Selected::Merge(&gating)).is_err(),
        "a gate that is running is not restarted"
    );
    let failed = queue_merge_row("m-failed", "failed");
    assert_eq!(verb_moves(Verb::Restart, &Selected::Merge(&failed)), Ok(()));
}
