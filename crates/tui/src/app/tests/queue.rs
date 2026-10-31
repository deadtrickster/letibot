//! The review queue and the merge queue (R61), as the HEAD's own pane draws them.
//!
//! **These are `ui/panes/queue.rs`'s tests**, so each one asks for that pane by name: `/queue`
//! opens the `queue` layer's pane now (`app/tests/queue_plugin.rs` is its tests), and the head's
//! own is what a person gets from `/queue builtin`, from a layer that is disabled, and from one
//! whose answer the host refuses. A pane that is only reachable through an escape still has to be
//! right, so it keeps its own tests rather than losing them to the conversion.

use super::*;

/// **The head's own queue pane** — `/queue builtin`, as a fixture.
///
/// The one difference from `app()`: the `queue` layer installed in this binary is told to stand
/// aside, so what these tests read is `ui/panes/queue.rs` and not whatever the layer draws today.
fn builtin() -> App {
    let mut a = app();
    a.queue_builtin = true;
    a
}

/// **`/queue` asks for the snapshot and draws nothing it was not sent** — and until one arrives
/// it says it is asking, rather than drawing an empty queue.
#[test]
fn the_queue_pane_asks_for_the_queues_and_draws_what_comes_back() {
    let mut a = builtin();
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
    let mut a = builtin();
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

/// **A row says how long it has been in its CURRENT state, and names an owner only when the
/// claim is not the serving daemon's** — R61 defect 12 was that the age was since creation and
/// no row named its owner at all. Since the declutter `by …` is for a claim from somewhere
/// else: the daemon holding the repository's lease is the pane's own, and its name on every row
/// was one of the three fields the operator was reading past.
#[test]
fn a_row_says_time_in_state_and_names_an_owner_only_from_elsewhere() {
    let mut a = builtin();
    a.session_id = "s1".into();
    a.command("queue");
    let mut r = queue_review_row("one", "reviewing");
    r.owner = "elsewhere/999/1700000000000".into();
    r.created_ms = 0;
    r.state_ms = 2_000; // the snapshot is read at 62s: one minute in state
    a.apply(queue_frame(queues(vec![r], Vec::new())));
    let screen = a.screen(160, 40).join("\n");
    assert!(screen.contains("reviewing for 1m00s"), "{screen}");
    assert!(screen.contains("by elsewhere/999"), "{screen}");

    // The fixture's lease is `lab/4242`'s, so a row IT claimed says nothing: the pane's one
    // chrome line already names who serves the repository.
    let mut mine = queue_review_row("two", "reviewing");
    mine.owner = "lab/4242/1700000000000".into();
    a.apply(queue_frame(queues(vec![mine], Vec::new())));
    let screen = a.screen(160, 40).join("\n");
    assert!(
        !screen.contains("by lab/4242"),
        "the serving daemon's own claim is not said again: {screen}"
    );
    assert!(screen.contains("lab (pid 4242"), "{screen}");
}

/// **Who serves the queues, and how alive it is** — the lease's owner, build and heartbeat age;
/// a heartbeat past the lease's lifetime is STALE. And no lease at all is said.
#[test]
fn the_pane_says_who_serves_each_repository_and_a_stale_heartbeat() {
    let mut a = builtin();
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
    let mut a = builtin();
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
    let mut a = builtin();
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
            running_gate: None,
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

/// **A snapshot that could not be read says so** — never drawn as an empty queue (defect 12).
#[test]
fn an_unreadable_snapshot_is_said_not_drawn_empty() {
    let mut a = builtin();
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
    let mut a = builtin();
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

/// **A row that is waiting says why, and it has no attempts to its name** (R92.34) — the pane drew
/// a row's `failure` only under `attempts > 0`, so a row parked with the reviewer's tree's own
/// words on it (a clone that failed, a VM slot wait) read as a row with no reason at all. The
/// operator's whole use of this pane is *why is it not moving*.
#[test]
fn a_waiting_row_says_why_even_with_no_attempts_counted() {
    let mut a = builtin();
    a.session_id = "s1".into();
    a.command("queue");
    let mut row = queue_review_row("one", "waiting");
    row.attempts = 0;
    row.failure = "WAITING FOR THE REVIEWER'S TREE: the reviewer's tree could not be made, so \
                   the review waits and is tried again — no attempt is spent on it: git clone: \
                   could not open '.git/objects/pack/tmp_pack_cgbKfF'"
        .into();
    a.apply(queue_frame(queues(vec![row], Vec::new())));
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

/// **A merge row's gate steps draw in order, a timed-out step is its own outcome** (decision 9).
#[test]
fn a_merge_rows_gate_steps_draw_and_timed_out_is_its_own_word() {
    use letibot_sessionlog::event::MergeGateOutcome as O;
    let mut a = builtin();
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
    let mut a = builtin();
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
    let mut a = builtin();
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
    let mut a = builtin();
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

/// **`/queue` is this session's repository; `/queue all` is every one** (R92 item 5) — the
/// operator's *"the /queue is still not project scoped"*. The pane, its rows, the edge and the
/// verbs' lines all follow the scope. FAILS before R92.5: every repository was drawn and
/// counted.
#[test]
fn the_queue_pane_and_the_edge_are_scoped_to_the_sessions_repository() {
    let mut a = builtin();
    a.session_id = "s1".into();
    a.command("queue");
    a.apply(queue_frame(two_repositories()));
    let rows = a.screen(160, 40);
    let screen = rows.join("\n");
    assert!(screen.contains("agent/mine"), "{screen}");
    assert!(!screen.contains("agent/theirs"), "{screen}");
    assert!(
        !screen.contains("EMERGENCY"),
        "another repository's red target is not this pane's emergency: {screen}"
    );
    assert!(screen.contains("this session's repository"), "{screen}");
    assert!(
        edge_row(&rows).contains("1 in review") && !edge_row(&rows).contains("red"),
        "the edge counts this repository only: {:?}",
        edge_row(&rows)
    );

    // **Explicitly every repository** — and a verb sent from it carries the scope.
    assert_eq!(a.command("queue all"), None, "the open pane is re-cut");
    let rows = a.screen(160, 40);
    let screen = rows.join("\n");
    assert!(screen.contains("agent/theirs"), "{screen}");
    assert!(screen.contains("EMERGENCY"), "{screen}");
    assert!(screen.contains("every repository"), "{screen}");
    assert!(
        edge_row(&rows).contains("1 in review") && !edge_row(&rows).contains("red"),
        "the EDGE stays this session's repository's whatever the pane shows: {:?}",
        edge_row(&rows)
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
    let mut a = builtin();
    a.session_id = "s1".into();
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
    assert!(!screen.contains("agent/"), "{screen}");
    assert!(
        !edge_row(&rows).contains("in review") && !edge_row(&rows).contains(" red"),
        "no edge standing for a session of no repository: {:?}",
        edge_row(&rows)
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
    let mut a = builtin();
    a.session_id = "s1".into();
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
    let screen = rows.join("\n");
    assert!(
        !edge_row(&rows).contains(" red") && !edge_row(&rows).contains("in review"),
        "green clears the alarm and the moot red-tip review: {:?}",
        edge_row(&rows)
    );
}

/// **The edge counts the queues by what stands**, and says nothing when nothing does — and the
/// four columns are the four questions the operator asked of them: what is waiting on a reviewer,
/// what is being landed, what is STUCK, and what landed today.
#[test]
fn the_edge_counts_the_queues_and_is_silent_when_nothing_stands() {
    let mut a = builtin();
    a.session_id = "s1".into();
    // **Nothing that stands** — an approved review is the merge queue's business now, and a
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
        screen.contains("1 in review · 1 being merged · 2 stuck"),
        "{screen}"
    );
}

/// **A landing is a standing, and the zeros are not** — the operator's two sentences of
/// 2026-10-29: *"i definitely want to see landed counter after parked, and wtf parked means?"*,
/// then *"so, label it as such - stuck?"*. A queue that landed two branches today and holds
/// nothing else still has something to say, so the words are drawn on the count alone: `2 landed`
/// and no `0 in review` in front of it.
#[test]
fn the_edge_says_the_four_columns_and_a_landing_stands_on_its_own() {
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(queue_frame(queues(
        Vec::new(),
        vec![
            queue_merge_row("m-a", "landed"),
            queue_merge_row("m-b", "landed"),
        ],
    )));
    let screen = a.screen(140, 24).join("\n");
    assert!(screen.contains("2 landed"), "{screen}");
    assert!(!screen.contains("0 in review"), "a zero is drawn: {screen}");
}

/// **A terminal row inflates no live column, and the operator's own shape is both halves of
/// that** (2026-10-29): a `rejected` review is terminal — the reviewer's complaints have gone
/// back to the branch's session and nothing will retry it — so it is not stuck; a `failed`
/// landing is the reader's to move, so it is. The snapshot carries both, because the daemon
/// sends every row it has (`mergequeue::snapshot` reads `review_rows(None)`).
#[test]
fn a_rejected_review_is_not_stuck_and_a_failed_landing_is() {
    let mut a = app();
    a.session_id = "s1".into();
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

    // **Take the landing away and the edge goes quiet**: two terminal reviews stand nothing, so
    // nothing is drawn — which is the whole defect `rejected`-in-the-column was.
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

/// **`landed` is the last day, not for ever** — the same window the pane's done list already
/// uses (`RECENT_MS`), so a queue that has worked all week reads as today's work and the number
/// falls back to zero on its own. The stamp is the row's own: `state_ms` is when the store moved
/// it to `landed` (`gating_to`).
#[test]
fn a_landing_older_than_the_recent_window_is_not_counted() {
    let mut a = app();
    a.session_id = "s1".into();
    // **A snapshot ten days old on its own clock**, so that a landing CAN be outside the window —
    // the fixture's default `at_ms` is 62 s after the epoch, and every stamp in it is inside.
    let mut snap = queues(Vec::new(), vec![queue_merge_row("m-old", "landed")]);
    snap.at_ms = 10 * crate::ui::panes::queue::RECENT_MS;
    snap.merges[0].state_ms = snap.at_ms - (crate::ui::panes::queue::RECENT_MS + 1_000);
    a.apply(queue_frame(snap));
    let screen = a.screen(140, 24).join("\n");
    assert!(
        !screen.contains("landed"),
        "a landing outside the window is not today's work: {screen}"
    );

    // **And one second inside it is** — the boundary is the window's, not the fixture's.
    let mut snap = queues(Vec::new(), vec![queue_merge_row("m-new", "landed")]);
    snap.at_ms = 10 * crate::ui::panes::queue::RECENT_MS;
    snap.merges[0].state_ms = snap.at_ms - (crate::ui::panes::queue::RECENT_MS - 1_000);
    a.apply(queue_frame(snap));
    let screen = a.screen(140, 24).join("\n");
    assert!(screen.contains("1 landed"), "{screen}");
}

/// **A red target still leads at its own severity, in front of the columns it grew into** —
/// `1 red` is the alarm, `Role::Failure` is what it is drawn in, and neither is a function of how
/// many columns follow it.
#[test]
fn a_red_target_still_leads_the_grown_columns_at_its_own_severity() {
    let mut a = app();
    a.session_id = "s1".into();
    let mut snap = queues(Vec::new(), vec![queue_merge_row("m-a", "landed")]);
    snap.targets.push(target(LETIBOT, "red"));
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    let screen = a.screen(160, 24).join("\n");
    assert!(screen.contains("1 red · 1 landed"), "{screen}");
    assert_eq!(
        a.queue_standings().role(),
        rano::style::Role::Failure,
        "the emergency is still the severity of the whole line"
    );
}

// -- what the line says when the news is a TARGET's --------------------------------------
//
// The operator, 2026-10-31, from a session in another repository: *"main is red, tests are
// going — nothing shows as in review or merged or stuck"*. Every row of that repository was
// terminal (11 approved, 2 rejected and 6 withdrawn reviews; 2 landed and 9 withdrawn landings),
// so all four columns were zero — and the only live fact the queue held was the red target. The
// four words were true and the line was a lie by omission, which is what a zero is: a count of
// nothing drawn where the reader is asking what there is.

/// **A repository whose only live fact is a red `main` reads `1 red`** — the four columns have
/// nothing to say and are not drawn, and the target is drawn in the register a stuck row's `[!]`
/// is drawn in. This is the operator's own shape: no open row anywhere, a red target.
#[test]
fn a_red_target_alone_reads_as_one_red_and_no_zeros() {
    let mut a = builtin();
    a.session_id = "s1".into();
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
    for zero in ["0 in review", "0 being merged", "0 stuck", "0 landed"] {
        assert!(!edge.contains(zero), "a zero is drawn — {zero} in {edge:?}");
    }
    assert_eq!(a.queue_standings().words().as_deref(), Some("1 red"));
    assert_eq!(
        a.queue_standings().role(),
        rano::style::Role::Failure,
        "the red target is the register a stuck row is drawn in"
    );
    // **And it is the badge**: the pane was never opened, so this is what a reader's edge says.
    assert!(!a.is_open(Surface::Queue));
}

/// **A red target and a stuck landing are both said** — the emergency and the row a person has
/// to move, with neither of them hiding the other and no zero between them.
#[test]
fn a_red_target_and_a_stuck_landing_are_both_said() {
    let mut a = builtin();
    a.session_id = "s1".into();
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

/// **A green target adds no word, and the columns read as they always have** — the four in their
/// order, each only while it has a count.
#[test]
fn an_all_green_repository_reads_the_columns_and_no_target_word() {
    let mut a = builtin();
    a.session_id = "s1".into();
    let mut snap = queues(
        vec![queue_review_row("one", "waiting")],
        vec![queue_merge_row("m-a", "gating")],
    );
    snap.targets.push(target(LETIBOT, "green"));
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    let rows = a.screen(120, 24);
    let edge = edge_row(&rows);
    assert!(edge.contains("1 in review · 1 being merged"), "{edge:?}");
    assert!(
        !edge.contains("red"),
        "a green target says nothing: {edge:?}"
    );
    assert!(!edge.contains("unjudged"), "{edge:?}");
    assert!(!edge.contains("stuck"), "{edge:?}");
    assert!(!edge.contains("landed"), "{edge:?}");
    assert_eq!(
        a.queue_standings().words().as_deref(),
        Some("1 in review · 1 being merged")
    );
}

/// **The longest line this head can draw fits an 80-column edge** — both target words and all
/// four columns non-zero is 70 cells, and the composer's edge is 76 inside the gutters, so the
/// whole line is drawn whole and keeps its click target. A cell narrower and `rano`'s `truncate`
/// eats the right end: `landed` first, `stuck`, `being merged`, `in review`, `unjudged` — and
/// `red` last, which is why the emergency leads the line.
#[test]
fn the_longest_standings_fit_on_an_eighty_column_edge() {
    let mut a = builtin();
    a.session_id = "s1".into();
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
    let words = "1 red · 1 unjudged · 1 in review · 1 being merged · 1 stuck · 2 landed";
    assert!(edge.contains(words), "the whole line at 80: {edge:?}");
    // **The composer is 76 cells inside its two-cell gutter** (`App::GUTTER`), and the row is
    // the gutter plus the edge — 78, with the right-hand gutter trimmed as every row's is.
    assert_eq!(
        line_width(&edge),
        78,
        "the edge is the 80-column edge's row"
    );
    assert_eq!(words.chars().count(), 70, "the premise: seventy cells");
    // **Drawn whole means clickable whole** — the record is the drawn span's own columns.
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
// The operator, 2026-11-01, twice in one minute: *"nothing about gate — and the same for other
// sessions — gate is invisible"*, then *"very nasty actually - fans just start spinning"*. The
// edge said `1 being merged` — true of every landing at every moment — while a gate compiled a
// step of four on another box. The step is the fact that turns the word into news.

/// A `GATING` target whose gate is on `step` of `of` — the snapshot a head is sent while it runs.
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

/// **The edge says which step of the gate is running**, at 80 columns, where the merge column used
/// to say only `1 being merged` — the operator's *"gate is invisible"*, on the one line every
/// screen carries.
#[test]
fn the_edge_names_the_gates_step_at_eighty_columns() {
    let mut a = builtin();
    a.session_id = "s1".into();
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
        Some("2 gating (step 3 of 4)"),
        "the column's count, worded by what the gate is doing"
    );
    let rows = a.screen(80, 24);
    let edge = edge_row(&rows);
    assert!(
        edge.contains("2 gating (step 3 of 4)"),
        "drawn whole at eighty columns: {edge:?}"
    );
    assert!(!edge.contains("being merged"), "{edge:?}");
    // **And it is still the door** — the span the frame drew is the span a click acts on, which
    // is the rule the standings keep at every width (`standings_rect` reads the drawn row).
    let words = "2 gating (step 3 of 4)";
    let rect = standings_rect(&a);
    assert_eq!(column_of(&edge, words), rect.x, "{edge:?}");
    assert_eq!(
        rect.w,
        words.chars().count(),
        "the target is the words' width"
    );
}

/// **A target that is not gating reads as it always did** — the word is the gate's, so a green
/// target leaves `1 being merged` alone, and a `gating` target whose snapshot carries no step
/// (a daemon between two steps) does not invent one.
#[test]
fn a_target_that_is_not_gating_keeps_the_old_word() {
    let mut a = builtin();
    a.session_id = "s1".into();
    let mut snap = queues(Vec::new(), vec![queue_merge_row("m-a", "gating")]);
    snap.targets.push(target(LETIBOT, "green"));
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    assert_eq!(
        a.queue_standings().words().as_deref(),
        Some("1 being merged")
    );
    assert!(edge_row(&a.screen(120, 24)).contains("1 being merged"));

    // A gate between two steps: the target is `gating` and there is no step to name.
    let mut a = builtin();
    a.session_id = "s1".into();
    let mut snap = queues(Vec::new(), vec![queue_merge_row("m-a", "gating")]);
    snap.targets.push(target(LETIBOT, "gating"));
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    assert_eq!(
        a.queue_standings().words().as_deref(),
        Some("1 being merged"),
        "no step is running, so none is claimed"
    );
}

// -- the edge's standings, as a click target -------------------------------------------
//
// The operator, 2026-10-27: *"i want to click '5 in review' and be taken to the merge
// queue"*. The standings are the one piece of the composer's bottom edge that is not about
// this head, and they are drawn on every screen — so the click is the only door to the queue
// a reader finds without knowing `/queue`. Every test draws a frame FIRST and aims the click
// at the rectangle that frame recorded, because the whole feature is that the target is where
// the draw put it: a click measured against columns a test worked out for itself would pass
// while the operator clicked the edge.

/// **The composer's bottom edge** — the row the standings are drawn on, and the row a reader
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

/// **The column a drawn row carries `needle` in**, counted in cells — the record's own
/// arithmetic done independently, so the two can disagree and say so.
fn column_of(row: &str, needle: &str) -> usize {
    let cells = crate::app::keyhints::cells(row);
    let want: Vec<char> = needle.chars().collect();
    (0..=cells.len() - want.len())
        .find(|i| cells[*i..*i + want.len()] == want[..])
        .unwrap_or_else(|| panic!("{needle:?} is not drawn on {row:?}"))
}

/// **A click on the standings opens the merge queue** — `/queue`'s act, through the verb's own
/// door, and the hover mark covers exactly the words the frame drew.
#[test]
fn a_click_on_the_standings_opens_the_queue() {
    let mut a = builtin();
    a.session_id = "s1".into();
    let mut snap = queues(vec![queue_review_row("one", "waiting")], Vec::new());
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    let screen = a.screen(120, 24);
    let rect = standings_rect(&a);
    let words = "1 in review";
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

    // **The hover lights the drawn span and nothing beside it** — what a click there would act
    // on, which is the rule the pointer follows everywhere (`App::relight`).
    a.hover_at(rect.x as u16 + 1, rect.y as u16);
    assert_eq!(
        a.hovered.as_ref().map(|h| h.rows.clone()),
        Some(vec![(rect.y, Some((rect.x, rect.x + rect.w)))]),
        "the hover does not cover the span the frame drew"
    );
    assert!(
        a.screen(120, 24)[rect.y].contains("\x1b[22;1m"),
        "the span is not drawn brighter under the pointer"
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
    let screen = a.screen(120, 24).join("\n");
    assert!(screen.contains("review queue · merge queue"), "{screen}");
}

/// **The click still lands after the words grew a column** (2026-10-29) — the standings were
/// three and are four, and the record is read off the drawn row (`screen::drawn_span`) rather than
/// rebuilt from the counts, so the longest form this head can draw is the case that proves it:
/// both alarms, every column non-zero. Arithmetic over the counts would place this rectangle
/// right of where the words were drawn the moment a column was added.
#[test]
fn the_standings_target_covers_the_words_they_grew_into() {
    let mut a = app();
    a.session_id = "s1".into();
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
    let words = "1 red · 1 unjudged · 1 in review · 1 being merged · 1 stuck · 2 landed";
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

/// **A click one cell outside the standings asks for nothing.** The edge around the words is
/// furniture, and a click on it is a click nobody claimed — it must not reach the queue by
/// rounding, luck or the gutter.
///
/// **Aimed at the GLASS, not at the record**: the two columns are the drawn span's own, worked
/// out from the frame, so a record one column too wide or one column to the left fails here
/// rather than agreeing with itself.
#[test]
fn a_click_one_cell_outside_the_standings_asks_for_nothing() {
    let mut a = builtin();
    a.session_id = "s1".into();
    let mut snap = queues(vec![queue_review_row("one", "waiting")], Vec::new());
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    let screen = a.screen(120, 24);
    let rect = standings_rect(&a);
    let words = "1 in review";
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

/// **The two target alarms carry their own word, and the click still opens the queue** — a
/// target nobody could judge says `1 unjudged` and a red one `1 red`, in the span the frame
/// draws, so they are part of the target and not a thing beside it.
#[test]
fn a_click_on_the_standings_with_an_alarm_prefix_opens_the_queue() {
    for (state, prefix) in [("timed_out", "1 unjudged"), ("red", "1 red")] {
        let mut a = builtin();
        a.session_id = "s1".into();
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

/// **A click while the queue is open closes it** — the click is `/queue`'s spelling and the verb
/// TOGGLES: the standings stay drawn on the queue's own edge, and pressing that door again
/// leaves, exactly as the count labels do for their panes.
#[test]
fn a_click_on_the_standings_of_an_open_queue_closes_it() {
    let mut a = builtin();
    a.session_id = "s1".into();
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

/// **The edge's standings are a target on whatever screen is up** — they are the one piece of
/// the composer's edge that is not about this head, and the operator reads them wherever they
/// are: a click from the subagents pane opens the queue just as one from the conversation does.
#[test]
fn the_standings_are_a_target_from_another_pane_too() {
    let mut a = builtin();
    a.session_id = "s1".into();
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

/// **A narrow edge keeps no target for standings it cut** — the edge truncates from the right
/// and the standings are the last thing on it, so a terminal too narrow to hold the words draws
/// a piece of them and records nothing. A target for words the frame cut is a click nobody can
/// see land, which is the failure `drawn_span` exists to make safe.
///
/// The fixture is the LONGEST line this head can draw (both target words and all four columns),
/// because that is the one a narrow edge cuts.
#[test]
fn a_narrow_edge_records_no_standings_target_for_words_it_cut() {
    let mut a = builtin();
    a.session_id = "s1".into();
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

/// **A queue that stands nothing has no standings, and the edge records no target for them** —
/// the counts are drawn only while they are true, so the click's target is absent with them:
/// the same frame, the same row, nothing to claim the pointer. A landing is a standing, so the
/// rows here are the ones that are the queue's PAST and not today's work: an approved review
/// (the merge queue's business now) and a landing that moved on.
#[test]
fn a_head_whose_queue_stands_nothing_records_no_standings_target() {
    let mut a = builtin();
    a.session_id = "s1".into();
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
///
/// The click used to only move the mark, so the row's detail was behind a key the person had
/// already pressed (`app/keys/mouse.rs`): a queue row is a thing you open, not a thing you point
/// at. What this test keeps from the old one is the half that matters — the row that opens is the
/// row the pointer landed on, not whichever row the host's enumeration had at that index.
#[test]
fn a_click_on_a_queue_row_opens_the_row_drawn_there() {
    let mut a = builtin();
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
    assert_eq!(
        a.queue_open.as_deref(),
        Some("three"),
        "the click opened the row it landed on"
    );
    assert_eq!(a.top_surface(), Some(Surface::QueueEntry));
    // **And the keyboard reaches the same row** — the cursor stayed where the click put it.
    a.key(Key::Esc);
    a.key(Key::Enter);
    assert_eq!(a.queue_open.as_deref(), Some("three"));
}

/// **The wheel and the page keys scroll the pane, not the conversation under it.**
#[test]
fn the_wheel_and_page_keys_scroll_the_queue_pane() {
    let mut a = builtin();
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
    let mut a = builtin();
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
    let mut a = builtin();
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
    let mut a = builtin();
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

// -- the declutter (2026-10-29) ---------------------------------------------------------
//
// The operator, of this pane: *"the queue pane is illegible mess"*, and *"the queue is the most
// automatic and opaque thing in the whole project rn"*. What was on every entry was a second
// line whose FIRST field was a 39-character session id — an address that exists so it can be
// typed into `/queue approve`, repeated on every row and read by nobody — plus the repository
// and the owner, both of which the pane had already said. So: an entry is ONE line, the reason
// it is parked is on that line, and the id and the sha are on the row under the cursor only.

const ID_A: &str = "s-1793010881933039000-sub-1793283793199051000";
const ID_B: &str = "s-1793010881933039000-sub-1793283793199051001";
const ID_C: &str = "s-1793010881933039000-sub-1793283793199051002";

/// **Three parked reviews and one landing** — the snapshot the declutter is judged on: what the
/// operator opens this pane for is a row that is not moving, and why it is not.
fn three_parked_and_a_landing() -> letibot_sessionlog::event::QueueSnapshot {
    let parked = |id: &str, name: &str, state: &str, verdict: &str, evidence: &str| {
        let mut r = queue_review_row(name, state);
        r.id = id.into();
        r.branch = format!("agent/{name}");
        r.head_sha = "0d5f8d1e5e4c3b2a19f8e7d6c5b4a39281706f5e".into();
        r.verdict = Some(verdict.into());
        r.evidence = evidence.into();
        r.owner = "lab/4242/1700000000000".into();
        r
    };
    let mut snap = queues(
        vec![
            parked(
                ID_A,
                "rev-alpha",
                "blocked",
                "failed",
                "`cargo test` failed: 3 tests red in letibot-tui\nand a second line the row \
                 does not carry",
            ),
            parked(
                ID_B,
                "rev-beta",
                "cannot_decide",
                "cannot_decide",
                "the reviewer could not read the diff: the clone was refused",
            ),
            parked(
                ID_C,
                "rev-gamma",
                "rejected",
                "rejected",
                "the operator vetoed it: the branch touches a file another package owns",
            ),
        ],
        vec![queue_merge_row("m-delta", "queued")],
    );
    snap.here = Some(here_in(LETIBOT));
    snap
}

/// The pane's lines that carry `needle`, by index.
fn lines_with(rows: &[String], needle: &str) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, l)| l.contains(needle))
        .map(|(i, _)| i)
        .collect()
}

/// **An entry is ONE line, and the id and the sha are on the picked one only.**
#[test]
fn an_entry_is_one_line_and_the_id_is_on_the_picked_row_only() {
    let mut a = builtin();
    a.session_id = "s1".into();
    a.command("queue");
    a.apply(queue_frame(three_parked_and_a_landing()));
    let rows = a.queue_lines(120);
    let drawn = rows.join("\n");

    // One line each: three entries, three rows, and the two unpicked ones are adjacent.
    for name in ["agent/rev-alpha", "agent/rev-beta", "agent/rev-gamma"] {
        assert_eq!(
            lines_with(&rows, name).len(),
            1,
            "`{name}` is not one line:\n{drawn}"
        );
    }
    let beta = lines_with(&rows, "agent/rev-beta")[0];
    let gamma = lines_with(&rows, "agent/rev-gamma")[0];
    assert_eq!(
        beta + 1,
        gamma,
        "an unpicked entry drew a second line:\n{drawn}"
    );

    // The id — what `/queue approve` takes — is on the row under the cursor, once, with the sha
    // it was judged at. The others are not drawn at all.
    let alpha = lines_with(&rows, "agent/rev-alpha")[0];
    assert_eq!(
        lines_with(&rows, ID_A).len(),
        1,
        "the picked row's id:\n{drawn}"
    );
    assert_eq!(
        lines_with(&rows, ID_A),
        vec![alpha + 1],
        "the id is not the picked row's own second line:\n{drawn}"
    );
    assert!(
        rows[alpha + 1].contains("head 0d5f8d1e5e"),
        "the picked row's line has no sha: {:?}",
        rows[alpha + 1]
    );
    for id in [ID_B, ID_C] {
        assert!(
            lines_with(&rows, id).is_empty(),
            "an unpicked row drew its id ({id}):\n{drawn}"
        );
    }
    // And the same in the merge view: the id and the sha of the landing under the cursor.
    a.key(Key::Tab);
    let rows = a.queue_lines(120);
    let drawn = rows.join("\n");
    assert_eq!(lines_with(&rows, "agent/delta").len(), 1, "{drawn}");
    let landing = lines_with(&rows, "agent/delta")[0];
    assert!(
        rows[landing + 1].contains("m-delta · approved abc1234567"),
        "the landing's id and sha are not its own second line: {:?}",
        rows[landing + 1]
    );
}

/// **The reason stays on the row** — for a parked row it is the whole value of the row, so the
/// verdict and the first line of the evidence are folded onto the entry's own line, and only
/// the first line: the evidence's second line is the overlay's.
#[test]
fn a_parked_rows_reason_is_on_its_own_line() {
    let mut a = builtin();
    a.session_id = "s1".into();
    a.command("queue");
    a.apply(queue_frame(three_parked_and_a_landing()));
    let rows = a.queue_lines(120);
    let drawn = rows.join("\n");
    let alpha = &rows[lines_with(&rows, "agent/rev-alpha")[0]];
    assert!(alpha.contains("blocked for 1m00s"), "{alpha:?}");
    assert!(
        alpha.contains("failed — `cargo test` failed: 3 tests red in letibot-tui"),
        "the verdict and the evidence are not on the row: {alpha:?}"
    );
    assert!(
        !drawn.contains("and a second line the row"),
        "the evidence's second line was drawn on the pane:\n{drawn}"
    );
    // A verdict that only repeats the state is not said twice.
    let beta = &rows[lines_with(&rows, "agent/rev-beta")[0]];
    assert!(beta.contains("cannot decide for 1m00s"), "{beta:?}");
    assert!(
        !beta.contains("cannot_decide —"),
        "the state and the verdict are one fact: {beta:?}"
    );
    assert!(
        beta.contains("· the reviewer could not read the diff"),
        "{beta:?}"
    );
}

/// **A field is said once** — the repository only when the pane has not already named it: not on
/// `/queue` (its scope line does), but always under `every repository`, where the scope line names
/// none and a row would otherwise leave the reader guessing which project it is in.
#[test]
fn a_row_names_its_repository_only_when_the_snapshot_holds_more_than_one() {
    let mut a = app();
    a.session_id = "s1".into();
    a.command("queue");
    let mut one = queues(vec![queue_review_row("one", "waiting")], Vec::new());
    one.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(one));
    let rows = a.queue_lines(120);
    let row = &rows[lines_with(&rows, "agent/one")[0]];
    assert!(
        !row.contains("letibot"),
        "the repository is on the row and in the scope line: {row:?}"
    );

    // **`every repository` names none**, so even one repository's rows say which.
    assert_eq!(a.command("queue all"), None);
    let mut snap = queues(vec![queue_review_row("one", "waiting")], Vec::new());
    snap.here = Some(here_in(LETIBOT));
    a.apply(queue_frame(snap));
    let rows = a.queue_lines(140);
    let row = &rows[lines_with(&rows, "agent/one")[0]];
    assert!(
        row.contains("· letibot"),
        "the scope line names no repository and neither does the row: {row:?}"
    );

    // Two repositories: each row says which, because the scope line cannot.
    a.apply(queue_frame(two_repositories()));
    let rows = a.queue_lines(140);
    let drawn = rows.join("\n");
    let mine = &rows[lines_with(&rows, "agent/mine")[0]];
    let theirs = &rows[lines_with(&rows, "agent/theirs")[0]];
    assert!(mine.contains("· letibot"), "{mine:?}\n{drawn}");
    assert!(theirs.contains("· pg-noop"), "{theirs:?}\n{drawn}");
}

/// **The chrome is one line** — who serves the repository and how alive it is, the repositories
/// that never asked for a queue, and the archive, all under the rows in one faint line; and the
/// sentence telling the reader what `enter` does is gone, because the hint bar and the picked
/// row's verbs both say it.
#[test]
fn the_lease_the_unqueued_repositories_and_the_archive_are_one_line() {
    let mut a = builtin();
    a.session_id = "s1".into();
    a.command("queue");
    let mut snap = queues(Vec::new(), Vec::new());
    snap.here = Some(here_in(LETIBOT));
    snap.not_queued = vec![LETIBOT.into()];
    snap.archived = 3;
    a.apply(queue_frame(snap));
    let rows = a.queue_lines(200);
    let chrome = lines_with(&rows, "served by");
    assert_eq!(chrome.len(), 1, "the lease is not one line: {rows:?}");
    let line = &rows[chrome[0]];
    assert!(line.contains("heartbeat 2.0s ago"), "{line:?}");
    assert!(
        line.contains("not queued: no `Merge gate` section in AGENTS.md — letibot"),
        "{line:?}"
    );
    assert!(
        line.contains("3 row(s) of the pre-split queue are archived"),
        "{line:?}"
    );
    assert!(
        !rows.iter().any(|l| l.contains("enter opens the row whole")),
        "the instruction sentence is still a line of its own: {rows:?}"
    );
}

/// **`o` goes into the reviewer's session** — the operator: *"there is also problem with /queue -
/// no way to reach reviewer session from here"*. From the list, and from a row opened whole; a row
/// nobody is reviewing says so.
#[test]
fn o_goes_into_the_reviewers_session() {
    let mut a = builtin();
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

/// **`d` is offered on a `gating` landing, and not on one that has landed** — *take it out of line*
/// is the person's answer to a gate that is going nowhere, and the offer has to be where it means
/// something.
///
/// The daemon's side of the same rule: `verbs::withdraw_merge` takes a `gating` row, and a landing
/// whose row has left `gating` does not land whatever its gate said
/// (`serve::tests::a_landing_withdrawn_while_its_gate_runs_does_not_land`).
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
    // **`restart` keeps its own ground** — a running gate is not a parked row, and a failed one is.
    assert!(
        verb_moves(Verb::Restart, &Selected::Merge(&gating)).is_err(),
        "a gate that is running is not restarted"
    );
    let failed = queue_merge_row("m-failed", "failed");
    assert_eq!(verb_moves(Verb::Restart, &Selected::Merge(&failed)), Ok(()));
}
