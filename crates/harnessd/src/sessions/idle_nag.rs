//! **When the plan check is armed** — the rule the operator's scheduling question turned
//! into, tested where it lives rather than through a daemon.
//!
//! The operator: *"looks like our todo nag is overkill"*, then *"maybe wait for a timeout
//! actually. so send it when model is idling"*, then *"but certainly not after my message."*
//! The first was the turn-boundary check firing after every exchange; the second is the
//! timeout; the third is the boundary condition the timeout must not violate, and it is
//! `Sessions::note_operator_prompt`'s job.
//!
//! **And the fourth half is the state**: *"can we handle postponed todo item properly? i.e.
//! they persist but without nag"*. A row the operator has set aside is not work this check may
//! speak about, and that is asserted below as a predicate over ROWS — which is the only way it
//! can be told from a check that merely failed to arm.
use super::nag_should_arm;
// The snapshot the daemon hands the check (`sessions::child_sessions`), spelled in this
// module's tests the way the arming decision takes it.
use super::ChildSessions;
// The jobs snapshot the daemon hands the same check (`Harness::running_jobs`), spelled here the
// way the arming decision takes it.
use super::RunningJobs;
use letibot_tokencore::store::{TodoBy, TodoItem, TodoStatus};

/// **Nothing is running** — the reading every test here that is not about a child passes, and
/// the one a daemon with no subagents hands in. The child cases have their own tests below,
/// which build a snapshot of their own (`ChildSessions::none().running(…)`).
fn no_children() -> letibot_tools::builtins::todo::ChildSessions {
    letibot_tools::builtins::todo::ChildSessions::none()
}

/// One row of a plan, as the board holds it. The operator's half, because that is the half a
/// postponed row is written from.
fn row(content: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        content: content.into(),
        status,
        by: TodoBy::Operator,
        when: None,
        needs: Vec::new(),
    }
}

/// A row that waits on the rows it names — the DAG's edge, as these tests write one.
fn row_needing(content: &str, on: &[&str]) -> TodoItem {
    TodoItem {
        needs: on
            .iter()
            .map(|c| letibot_tokencore::store::TodoNeed::Row {
                content: (*c).to_string(),
            })
            .collect(),
        ..row(content, TodoStatus::Pending)
    }
}

/// **A row that waits on a CHILD** — the dispatch case, as these tests write one.
fn row_needing_a_child(content: &str, id: &str) -> TodoItem {
    TodoItem {
        needs: vec![letibot_tokencore::store::TodoNeed::Child { id: id.to_string() }],
        ..row(content, TodoStatus::Pending)
    }
}

/// **A row that waits on a JOB** — the case the operator asked for one layer below a child:
/// work that cannot start until a background job this session is running has ended.
fn row_needing_a_job(content: &str, handle: &str) -> TodoItem {
    TodoItem {
        needs: vec![letibot_tokencore::store::TodoNeed::Job {
            handle: handle.to_string(),
        }],
        ..row(content, TodoStatus::Pending)
    }
}

/// **Nothing running outside the board** — no job this session holds, which is the reading
/// every test here that is not about a job passes, and the one a daemon with no process host
/// hands in.
fn no_jobs() -> letibot_tools::builtins::todo::RunningJobs {
    letibot_tools::builtins::todo::RunningJobs::none()
}

/// **A child this daemon is running**, as a test states one: an age, what it last showed, and
/// whether a turn is in flight in it.
fn child(
    age_secs: u64,
    quiet_secs: Option<u64>,
    working: bool,
) -> letibot_tools::builtins::todo::RunningChild {
    letibot_tools::builtins::todo::RunningChild {
        age: std::time::Duration::from_secs(age_secs),
        quiet_for: quiet_secs.map(std::time::Duration::from_secs),
        working,
    }
}

/// **A row that waits on a child gets its ROOM TO BREATHE, and then a sentence that steers.**
///
/// The operator's directive, 2026-10-10, and the failure that was MEASURED the night it was
/// written: a row marked in progress with a child working on it was nagged four times in twenty
/// minutes, and none of those nags could tell *the child is working* from *the child is stuck*.
///
/// The two halves are asserted on the SAME plan and the SAME child forty minutes apart in age,
/// so the only fact that changes is the age — and what changes with it is whether the check may
/// speak at all.
///
/// **The silence is `nag_should_arm`'s, which is the whole schedule's one answer**: nothing
/// armed means no clock, so no notice and no turn is spent. The other direction is the same
/// predicate letting the row through, and the sentence it then produces is
/// `unfinished_plan`'s — which is the half that has to say something the silence did not.
#[test]
fn a_young_child_keeps_the_check_silent_and_an_old_one_is_spoken_about() {
    let plan = [row_needing_a_child(
        "collect the child's answer",
        "s-1-sub-2",
    )];

    // **Five minutes old**: the check may not speak about the row at all — not asked about,
    // and not listed as blocked either, because `the_plan_as_checked` removes it before the
    // sentence is rendered. And a plan whose ONLY row is that one arms nothing at all.
    let young = ChildSessions::none().running("s-1-sub-2", child(5 * 60, Some(4), true));
    assert!(
        super::the_plan_as_checked(&plan, &young, &no_jobs()).is_empty(),
        "a row waiting on a five-minute-old child is not work this check may speak about"
    );
    assert!(
        !nag_should_arm(&plan, None, &young, &no_jobs()),
        "and that plan arms NOTHING — the silence is the schedule's, not a sentence that \
             happens to be empty"
    );
    // **AND THE SILENCE HAS AN END.** Nothing else would arm this clock: it is armed by a turn,
    // a board write or a prompt, and a child's AGE is none of those — so the deadline is the
    // moment the child leaves its room to breathe, and the steering arrives when it can.
    let t0 = std::time::Instant::now();
    let mut clock = super::TodoNagClock::with_window(std::time::Duration::from_secs(60));
    clock.rearm(&plan, t0, &young, &no_jobs());
    assert_eq!(
        clock.due_at(),
        Some(t0 + std::time::Duration::from_secs(25 * 60)),
        "a five-minute-old child's row is due the moment the child turns thirty"
    );

    // **Forty-five minutes old, and gone quiet WITH A TURN IN FLIGHT**: talkable, and what it
    // says is *check the child* — with the child named, its age given, and the two mechanisms
    // that act on it. **A turn in flight is what makes it the *stuck* case**: the same silence
    // with no turn in flight is a child between turns, whose sentence sends its reader to
    // `task_message` instead (the tools crate's
    // `a_child_between_turns_is_told_to_be_asked_and_not_killed` is that state's own).
    let old = ChildSessions::none().running("s-1-sub-2", child(45 * 60, Some(12 * 60), true));
    assert!(
        nag_should_arm(&plan, None, &old, &no_jobs()),
        "past the room to breathe, the check speaks"
    );
    let said = super::unfinished_plan(
        &super::the_plan_as_checked(&plan, &old, &no_jobs()),
        &plan,
        &old,
        &no_jobs(),
    )
    .expect("a stuck plan is a finding");
    assert!(
        said.contains("the child `s-1-sub-2`, running 45 minutes"),
        "the sentence names the child and its age: {said}"
    );
    assert!(
        said.contains("check whether it is stuck"),
        "and steers to the child rather than to the plan: {said}"
    );
    assert!(
        !said.contains("cannot start yet"),
        "not the sentence that tells the reader nothing: {said}"
    );
}

/// **A row that waits on a RUNNING JOB keeps the check silent, and the same row is asked about
/// once the job has ended.**
///
/// The operator's ask, 2026-10-11 — *"todo can be declared to be dependent on a subsession and
/// on a job ... and that should drastically decrease nags count"* — and the failure it answers
/// was measured the same night: each of the operator's long jobs had a row nagging while it ran.
///
/// **The two halves differ from the child's in the one way that matters here.** A young child's
/// silence has to be given a DEADLINE, because nothing else would ever end it; a running job's
/// does not, because its settlement is delivered to the model as a turn of its own and a turn's
/// end re-arms this very clock. So the whole of this rule is the predicate: while the job runs
/// nothing is armed and nothing is said, and the row is asked about the moment it is over.
#[test]
fn a_running_job_keeps_the_check_silent_and_the_end_of_it_asks_again() {
    let plan = [row_needing_a_job("push the branch", "j2070")];
    let running = RunningJobs::none().running("j2070");
    assert!(
        super::the_plan_as_checked(&plan, &no_children(), &running).is_empty(),
        "a row waiting on a running job is not work this check may speak about"
    );
    assert!(
        !nag_should_arm(&plan, None, &no_children(), &running),
        "and that plan arms NOTHING — the silence is the schedule's, not a sentence that \
             happens to be empty"
    );
    let t0 = std::time::Instant::now();
    let mut clock = super::TodoNagClock::with_window(std::time::Duration::from_secs(60));
    clock.rearm(&plan, t0, &no_children(), &running);
    assert_eq!(
        clock.due_at(),
        None,
        "and no deadline is invented for it: the job's own settlement is what ends the \
             silence, so there is nothing to arm"
    );

    // **The job ends** — the handle is not in the snapshot any more — and the row is work.
    let ended = RunningJobs::none();
    assert_eq!(
        super::the_plan_as_checked(&plan, &no_children(), &ended).len(),
        1,
        "the row is askable once the job it waited on is over"
    );
    assert!(
        nag_should_arm(&plan, None, &no_children(), &ended),
        "and the check is armed for it"
    );
    let said = super::unfinished_plan(
        &super::the_plan_as_checked(&plan, &no_children(), &ended),
        &plan,
        &no_children(),
        &ended,
    )
    .expect("the row is open work now");
    assert!(
        said.contains("  - push the branch"),
        "the sentence names the row itself: {said}"
    );
    assert!(
        !said.contains("j2070"),
        "and says nothing about the job, which is over: {said}"
    );
}

/// **A daemon full of jobs says nothing about a board that waits on rows** — the compatibility
/// half of the same predicate, said the way the child one is.
#[test]
fn a_board_that_waits_on_nothing_is_unmoved_by_a_daemon_full_of_jobs() {
    let plan = [row("open", TodoStatus::Pending)];
    let busy = RunningJobs::none().running("j2070").running("j2081");
    assert_eq!(
        super::the_plan_as_checked(&plan, &no_children(), &busy).len(),
        1,
        "the row is askable whatever jobs the session happens to be running"
    );
    assert_eq!(
        nag_should_arm(&plan, None, &no_children(), &no_jobs()),
        nag_should_arm(&plan, None, &no_children(), &busy),
        "the arming decision is the same one"
    );
}

/// **The ordinary row-with-no-needs path is unchanged by any of this** — the compatibility
/// claim, at the one predicate that decides what the check may speak about.
///
/// A daemon that is running children is the NORMAL daemon, so a board that waits on nothing
/// must read exactly as it did before `ChildSessions` existed, whatever that snapshot holds.
#[test]
fn a_board_that_waits_on_nothing_is_unmoved_by_a_daemon_full_of_children() {
    let plan = [row("open", TodoStatus::Pending)];
    let busy = ChildSessions::none()
        .running("s-other", child(3 * 60, Some(1), true))
        .running("s-older", child(90 * 60, Some(60 * 60), false));
    assert_eq!(
        super::the_plan_as_checked(&plan, &no_children(), &no_jobs()).len(),
        1,
        "the row is askable"
    );
    assert_eq!(
        super::the_plan_as_checked(&plan, &busy, &no_jobs()).len(),
        1,
        "and it is askable whatever children the daemon happens to be running"
    );
    assert_eq!(
        nag_should_arm(&plan, None, &no_children(), &no_jobs()),
        nag_should_arm(&plan, None, &busy, &no_jobs()),
        "the arming decision is the same one"
    );
}

/// A plan with nothing open is not a plan to nag about.
#[test]
fn a_finished_plan_is_never_armed() {
    assert!(
        !nag_should_arm(&[], None, &no_children(), &no_jobs()),
        "no plan at all"
    );
    // even if something WAS nagged before and has since been finished
    assert!(!nag_should_arm(
        &[],
        Some("[todo check] 2 of 3"),
        &no_children(),
        &no_jobs(),
    ));
    // and a plan whose rows are all answered
    assert!(!nag_should_arm(
        &[row("done", TodoStatus::Completed)],
        None,
        &no_children(),
        &no_jobs(),
    ));
}

/// The first idle period after real work: armed.
#[test]
fn an_unfinished_plan_is_armed_once() {
    assert!(nag_should_arm(
        &[row("open", TodoStatus::Pending)],
        None,
        &no_children(),
        &no_jobs(),
    ));
}

/// **A STUCK PLAN STILL ARMS THE CHECK** — the degenerate case, and the one place the ready set
/// could have switched the check off by accident.
///
/// Every row left waits on another, so the ready set is EMPTY and there is no row to name. That
/// is exactly the state the agent has to act on — something has to finish, or a need has to be
/// corrected — so silence here would be the check failing at the moment it matters, which is
/// why `unfinished_plan_for` answers it with a SENTENCE rather than `None`. Both halves that
/// make this a clock question are asserted: the first arm fires, and the same stuck plan said
/// once is not a new notice, so it repeats at the ladder's rate instead of becoming a metronome.
///
/// **The dependency-free half of the same question is pinned by the tests above and below this
/// one**, unchanged: they are flat plans, so their arming is the arming `1d24fab` had.
#[test]
fn a_stuck_plan_arms_the_check_and_is_not_a_metronome() {
    let plan = [
        row_needing("deploy", &["run the tests"]),
        row_needing("run the tests", &["deploy"]),
    ];
    assert!(
        nag_should_arm(&plan, None, &no_children(), &no_jobs()),
        "nothing can start, so the check must speak rather than go quiet"
    );
    let said = super::unfinished_plan(
        &super::the_plan_as_checked(&plan, &no_children(), &no_jobs()),
        &plan,
        &no_children(),
        &no_jobs(),
    )
    .expect("a stuck plan is a finding");
    assert!(said.contains("NOTHING can start"), "{said}");
    assert!(
        !nag_should_arm(&plan, Some(&said), &no_children(), &no_jobs()),
        "the same stuck plan is the same notice, so it repeats at the ladder's rate"
    );
    // **And it is a NOTICE and not a hole in the schedule**: the clock arms it one window out.
    let t0 = std::time::Instant::now();
    let mut clock = super::TodoNagClock::with_window(std::time::Duration::from_secs(60));
    clock.rearm(&plan, t0, &no_children(), &no_jobs());
    assert_eq!(
        clock.due_at(),
        Some(t0 + std::time::Duration::from_secs(60)),
        "a stuck plan is checked on the same schedule as any other"
    );
}

/// **An unchanged plan is not a NEW notice** — the `moved` half of the schedule, which is what
/// this predicate is. It is no longer the whole of the arming decision: an unchanged plan comes
/// back at the clock's doubled-and-capped interval (the operator's *"hmm, yes obviously it
/// should be a repeated nag"*), which is `TodoNagClock`'s own test below. What is pinned here is
/// that the COMPARISON is about the notice and not about the clock's state.
///
/// The last-sent text is taken from the plan itself rather than written by hand, because that
/// is what the schedule actually stores — a hand-written string would test the comparison
/// against something this plan could never produce.
#[test]
fn an_unchanged_plan_is_not_a_new_notice() {
    let plan = [row("open", TodoStatus::Pending)];
    let sent = super::unfinished_plan(
        &super::the_plan_as_checked(&plan, &no_children(), &no_jobs()),
        &plan,
        &no_children(),
        &no_jobs(),
    )
    .expect("open work");
    assert!(!nag_should_arm(
        &plan,
        Some(&sent),
        &no_children(),
        &no_jobs()
    ));
}

/// Any change at all is a new thing to say — an item closed, one started, one added.
#[test]
fn a_plan_that_moved_is_armed_again() {
    let plan = [row("open", TodoStatus::Pending)];
    let sent = super::unfinished_plan(
        &super::the_plan_as_checked(&plan, &no_children(), &no_jobs()),
        &plan,
        &no_children(),
        &no_jobs(),
    )
    .expect("open work");
    assert!(nag_should_arm(
        &[row("open", TodoStatus::InProgress)],
        Some(&sent),
        &no_children(),
        &no_jobs(),
    ));
    assert!(nag_should_arm(
        &[
            row("open", TodoStatus::Pending),
            row("added", TodoStatus::Pending)
        ],
        Some(&sent),
        &no_children(),
        &no_jobs(),
    ));
}

/// **A POSTPONED row does not arm the check, and an ordinary one beside it does.**
///
/// The operator's ask — a postponed item *"persists but without nag"* — and both directions
/// are load-bearing: the first assertion is the feature, and the second is what says the first
/// did not simply switch the check off for everything.
///
/// **Tested here, as a predicate over rows and no session**, which is the reason
/// `nag_should_arm` exists at all: through a daemon, *silent because the row is postponed* and
/// *silent because nothing armed the clock* are the same observation.
#[test]
fn a_postponed_row_does_not_arm_the_check() {
    assert!(
        !nag_should_arm(
            &[row("later", TodoStatus::Postponed)],
            None,
            &no_children(),
            &no_jobs()
        ),
        "the one row is set aside, so there is nothing to check"
    );
    // **Even after the check spoke about it**, which is the transition a session really makes:
    // the model was told, the operator then set the row aside, and the next idle period must
    // be silent rather than repeat the sentence.
    assert!(
        !nag_should_arm(
            &[row("later", TodoStatus::Postponed)],
            Some("[todo check] this turn is finished and one item is not done:\n  - later"),
            &no_children(),
            &no_jobs(),
        ),
        "a postponement is not a reason to say the same thing again"
    );
    // …and the ordinary row beside it is the control.
    assert!(
        nag_should_arm(
            &[row("now", TodoStatus::Pending)],
            None,
            &no_children(),
            &no_jobs()
        ),
        "an ordinary open row arms the check, so the silence above is the STATE and not a \
             check that stopped working"
    );

    // **A mixed plan asks about the ordinary row and says nothing about the other one.** The
    // reading is `the_plan_as_checked`, which is the same one `Harness::nag_notice` takes — so
    // the row is neither named nor counted as open, which are the two ways a postponed row
    // could still reach the model.
    let mixed = [
        row("now", TodoStatus::Pending),
        row("later", TodoStatus::Postponed),
    ];
    let notice = super::unfinished_plan(
        &super::the_plan_as_checked(&mixed, &no_children(), &no_jobs()),
        &mixed,
        &no_children(),
        &no_jobs(),
    )
    .expect("the ordinary row is open work");
    assert!(
        !notice.contains("later"),
        "the check must not name a row the operator set aside: {notice}"
    );
    assert!(
        !notice.contains("more open"),
        "and must not count it among what is open either: {notice}"
    );
}

/// **A row the operator STRUCK OFF does not arm the check either — and is not the same row as
/// one set aside.**
///
/// `Cancelled` arrives with the same ask as `Postponed` (*"so done items or canceled items
/// should be kept"*): the row stays on the board as the record the operator's ruling is about,
/// and nothing asks about it, because there is nothing owed. The two are asserted TOGETHER
/// because they are one predicate's neighbours — the reason to write this test is that a fifth
/// word could have been added to the store and forgotten here, and the symptom would be a check
/// that nags about work somebody has already decided against.
#[test]
fn a_cancelled_row_does_not_arm_the_check() {
    assert!(
        !nag_should_arm(
            &[row("struck off", TodoStatus::Cancelled)],
            None,
            &no_children(),
            &no_jobs(),
        ),
        "the one row is struck off, so there is nothing to check"
    );
    assert!(
        !nag_should_arm(
            &[row("struck off", TodoStatus::Cancelled)],
            Some("[todo check] this turn is finished and one item is not done:\n  - struck off"),
            &no_children(),
            &no_jobs(),
        ),
        "a row decided against is not a reason to say the same thing again"
    );
    // …and the ordinary row beside it is the control, as it is for `Postponed`.
    assert!(
        nag_should_arm(
            &[row("now", TodoStatus::Pending)],
            None,
            &no_children(),
            &no_jobs()
        ),
        "an ordinary open row arms the check, so the silence above is the STATE"
    );
    let mixed = [
        row("now", TodoStatus::Pending),
        row("struck off", TodoStatus::Cancelled),
    ];
    let notice = super::unfinished_plan(
        &super::the_plan_as_checked(&mixed, &no_children(), &no_jobs()),
        &mixed,
        &no_children(),
        &no_jobs(),
    )
    .expect("the ordinary row is open work");
    assert!(
        !notice.contains("struck off"),
        "the check must not name a row the operator struck off: {notice}"
    );
    assert!(
        !notice.contains("more open"),
        "and must not count it among what is open either: {notice}"
    );
}

/// **The clock itself, pinned at the boundary with the time injected** — because a test
/// that waited a real minute would not be a test, and because the clock is the half a
/// child now shares (`serve_child_under`), so its boundaries are worth asserting where
/// they live rather than through a daemon or a spawned thread.
///
/// The instants are arbitrary fixed points; the window is injected, so the arithmetic is
/// exact and not slept through.
#[test]
fn the_clock_arms_at_the_window_and_stands_down_when_nothing_is_askable() {
    use std::time::{Duration, Instant};

    let t0 = Instant::now();
    let mut clock = super::TodoNagClock::with_window(Duration::from_secs(60));
    assert!(
        clock.due_at().is_none(),
        "a fresh clock is armed for nothing — the waiter's sleep stays unbounded"
    );

    // An open plan arms at now + window — the boundary is exact, not "about a minute".
    clock.rearm(
        &[row("open", TodoStatus::Pending)],
        t0,
        &no_children(),
        &no_jobs(),
    );
    assert_eq!(clock.due_at(), Some(t0 + Duration::from_secs(60)));

    // **And stands down for a plan with nothing to ask about** — the operator's
    // postponed row is the case the rule exists for, so it is the case pinned here.
    clock.rearm(
        &[row("later", TodoStatus::Postponed)],
        t0,
        &no_children(),
        &no_jobs(),
    );
    assert!(
        clock.due_at().is_none(),
        "a postponed-only plan arms nothing: the operator's `[p]` stays theirs"
    );
}

/// **The operator's ruling, through the clock**: *"hmm, yes obviously it should be a repeated
/// nag."* A delivered check is not silence for ever — the same notice comes back at an interval
/// that doubles per delivery — and a plan that MOVED is a new thing to say, one base window
/// from the re-arm.
///
/// **This is the test that fails on the once-per-plan rule**: there, the second `rearm` below
/// armed nothing at all.
#[test]
fn a_said_check_repeats_and_a_moved_plan_starts_the_ladder_over() {
    use std::time::{Duration, Instant};

    let t0 = Instant::now();
    let mut clock = super::TodoNagClock::with_window(Duration::from_secs(60));
    let plan = [row("open", TodoStatus::Pending)];
    let said = super::unfinished_plan(
        &super::the_plan_as_checked(&plan, &no_children(), &no_jobs()),
        &plan,
        &no_children(),
        &no_jobs(),
    )
    .expect("open work");

    // The first check of a plan is one base window after the turn that armed it.
    clock.rearm(&plan, t0, &no_children(), &no_jobs());
    assert_eq!(clock.due_at(), Some(t0 + Duration::from_secs(60)));

    // Delivered: the notice is recorded, the deadline is spent — and the REPEAT is armed, one
    // doubled interval after the DELIVERY.
    assert!(clock.due_now(t0 + Duration::from_secs(60)));
    clock.said(Some(said.clone()), t0 + Duration::from_secs(60));
    clock.rearm(
        &plan,
        t0 + Duration::from_secs(60),
        &no_children(),
        &no_jobs(),
    );
    assert_eq!(
        clock.due_at(),
        Some(t0 + Duration::from_secs(60 + 120)),
        "the same notice comes back at the doubled interval, measured from its delivery"
    );

    // **And a turn that changes nothing does not postpone it.** The anchor is the delivery, so
    // a wake-driven turn in between arms the SAME moment rather than a fresh window — which is
    // what keeps a session that keeps waking from pushing its own reminder back for ever.
    clock.rearm(
        &plan,
        t0 + Duration::from_secs(100),
        &no_children(),
        &no_jobs(),
    );
    assert_eq!(
        clock.due_at(),
        Some(t0 + Duration::from_secs(180)),
        "unchanged by the turn"
    );

    // Delivered again: the ladder doubles again.
    assert!(clock.due_now(t0 + Duration::from_secs(180)));
    clock.said(Some(said.clone()), t0 + Duration::from_secs(180));
    clock.rearm(
        &plan,
        t0 + Duration::from_secs(180),
        &no_children(),
        &no_jobs(),
    );
    assert_eq!(clock.due_at(), Some(t0 + Duration::from_secs(180 + 240)));

    // **A plan that moved is a new thing to say** — one row started — and the ladder starts
    // over at the base window, however long the old notice stood.
    let moved = [row("open", TodoStatus::InProgress)];
    clock.rearm(
        &moved,
        t0 + Duration::from_secs(400),
        &no_children(),
        &no_jobs(),
    );
    assert_eq!(
        clock.due_at(),
        Some(t0 + Duration::from_secs(460)),
        "a moved plan is armed one base window from the re-arm"
    );

    // **And a speaker earns the check back** — `note_operator_prompt`'s rule, which a parent
    // steering a child exercises too (`serve_child_under`'s prompt arm) — at the base rate and
    // not at whatever gap the ladder had grown to.
    let moved_said = super::unfinished_plan(
        &super::the_plan_as_checked(&moved, &no_children(), &no_jobs()),
        &moved,
        &no_children(),
        &no_jobs(),
    )
    .expect("open work");
    clock.said(Some(moved_said), t0 + Duration::from_secs(400));
    clock.rearm(
        &moved,
        t0 + Duration::from_secs(400),
        &no_children(),
        &no_jobs(),
    );
    assert_eq!(
        clock.due_at(),
        Some(t0 + Duration::from_secs(520)),
        "the moved plan's own notice repeats at its doubled interval"
    );
    clock.prompt_arrived();
    clock.rearm(
        &moved,
        t0 + Duration::from_secs(430),
        &no_children(),
        &no_jobs(),
    );
    assert_eq!(
        clock.due_at(),
        Some(t0 + Duration::from_secs(490)),
        "whoever prompts the session earns the check back one base window away"
    );
}

/// **The ladder stops growing**, and that is what keeps a nine-hour stall a working night
/// rather than a retreat into silence: the interval doubles per delivery and holds at
/// [`super::NAG_REPEAT_CAP`].
///
/// A one-second window rather than the production minute, so the ladder reaches the cap in a
/// handful of deliveries; the SHAPE is the production one, because the interval is computed
/// from the window the clock was built with.
#[test]
fn the_repeat_stops_growing_at_the_cap() {
    use std::time::{Duration, Instant};

    let window = Duration::from_secs(1);
    let mut clock = super::TodoNagClock::with_window(window);
    let plan = [row("open", TodoStatus::Pending)];
    let said = super::unfinished_plan(
        &super::the_plan_as_checked(&plan, &no_children(), &no_jobs()),
        &plan,
        &no_children(),
        &no_jobs(),
    )
    .expect("open work");
    let mut at = Instant::now();
    let mut gaps: Vec<Duration> = Vec::new();
    clock.rearm(&plan, at, &no_children(), &no_jobs());
    for _ in 0..40 {
        let due = clock.due_at().expect("an unfinished plan is always armed");
        gaps.push(due - at);
        assert!(clock.due_now(due), "the deadline's own instant is due");
        clock.said(Some(said.clone()), due);
        at = due;
        clock.rearm(&plan, at, &no_children(), &no_jobs());
    }

    // Every gap is the one before it doubled, or the cap — never more than both.
    for pair in gaps.windows(2) {
        assert!(
            pair[1] <= (pair[0] * 2).min(super::NAG_REPEAT_CAP),
            "the gap may double or hit the cap and nothing else: {pair:?}"
        );
    }
    assert!(
        gaps.contains(&(window * 2)),
        "the ladder really did double before it capped: {gaps:?}"
    );
    assert_eq!(
        gaps.last(),
        Some(&super::NAG_REPEAT_CAP),
        "and it holds at the cap: {gaps:?}"
    );
}

/// **A plan with nothing to ask about never repeats**, whatever the ladder has counted — the
/// disarm is the plan's own reading and not the clock's state.
#[test]
fn a_plan_with_nothing_to_ask_about_never_repeats() {
    use std::time::{Duration, Instant};

    let t0 = Instant::now();
    let mut clock = super::TodoNagClock::with_window(Duration::from_secs(60));
    let plan = [row("open", TodoStatus::Pending)];
    let said = super::unfinished_plan(
        &super::the_plan_as_checked(&plan, &no_children(), &no_jobs()),
        &plan,
        &no_children(),
        &no_jobs(),
    )
    .expect("open work");
    clock.rearm(&plan, t0, &no_children(), &no_jobs());
    assert!(clock.due_now(t0 + Duration::from_secs(60)));
    clock.said(Some(said), t0 + Duration::from_secs(60));
    clock.rearm(
        &plan,
        t0 + Duration::from_secs(60),
        &no_children(),
        &no_jobs(),
    );
    assert!(
        clock.due_at().is_some(),
        "the repeat is armed for the unfinished plan"
    );

    // The operator sets the only row aside: nothing to ask about, however long the ladder has
    // been running.
    clock.rearm(
        &[row("open", TodoStatus::Postponed)],
        t0 + Duration::from_secs(70),
        &no_children(),
        &no_jobs(),
    );
    assert!(
        clock.due_at().is_none(),
        "a postponed-only plan arms nothing: the operator's `[p]` stays theirs"
    );
    // And every row answered: the same.
    clock.rearm(
        &[row("open", TodoStatus::Completed)],
        t0 + Duration::from_secs(80),
        &no_children(),
        &no_jobs(),
    );
    assert!(clock.due_at().is_none(), "a finished plan arms nothing");

    // The row comes back unfinished, and the notice is the one already sent: the repeat is
    // armed at the ladder's own gap (the delivery plus two windows), not at a fresh window.
    clock.rearm(
        &plan,
        t0 + Duration::from_secs(90),
        &no_children(),
        &no_jobs(),
    );
    assert_eq!(
        clock.due_at(),
        Some(t0 + Duration::from_secs(180)),
        "the clock holds the plan and the notice, not a grudge about the interlude"
    );
}

/// **`due_now` at the boundary, and the disarm in both arms of it.** The deadline's
/// own instant is due; a hair before is not; and a `due_now` that answered false has
/// still SPENT nothing (the deadline stands), while one that answered true has spent
/// it — which is the difference between a check that fires once and one that fires
/// every idle pass after it.
#[test]
fn the_boundary_is_due_and_a_delivery_disarms_either_way() {
    use std::time::{Duration, Instant};

    let t0 = Instant::now();
    let mut clock = super::TodoNagClock::with_window(Duration::from_secs(60));
    clock.rearm(
        &[row("open", TodoStatus::Pending)],
        t0,
        &no_children(),
        &no_jobs(),
    );
    let due = t0 + Duration::from_secs(60);

    assert!(
        !clock.due_now(due - Duration::from_millis(1)),
        "a hair before the deadline is not due"
    );
    assert_eq!(
        clock.due_at(),
        Some(due),
        "a `due_now` that said no spent nothing — the deadline stands"
    );
    assert!(clock.due_now(due), "the deadline's own instant is due");
    assert!(clock.due_at().is_none(), "and the delivery disarmed it");

    // `fired` is `serve_child_under`'s half of the same disarm, told by
    // `OwnWork::TimedOut` instead of a return value.
    clock.rearm(
        &[row("open", TodoStatus::Pending)],
        t0,
        &no_children(),
        &no_jobs(),
    );
    clock.fired();
    assert!(clock.due_at().is_none());
}

/// **R77: an operator's ADD earns the check back and arms it at the shorter window** — a notice
/// that had climbed the ladder is not where their row waits, and neither is the full minute.
#[test]
fn an_operator_add_earns_the_check_back_at_the_shorter_window() {
    use std::time::{Duration, Instant};

    let t0 = Instant::now();
    let mut clock = super::TodoNagClock::with_window(Duration::from_secs(60));
    let plan = [TodoItem {
        by: TodoBy::Model,
        ..row("the model's", TodoStatus::Pending)
    }];
    let said = super::unfinished_plan(&plan, &plan, &no_children(), &no_jobs()).expect("open");
    clock.rearm(&plan, t0, &no_children(), &no_jobs());
    assert!(clock.due_now(t0 + Duration::from_secs(60)));
    clock.said(Some(said), t0 + Duration::from_secs(60));

    let added = [
        plan[0].clone(),
        row("make tier 1 green", TodoStatus::Pending),
    ];
    let at = t0 + Duration::from_secs(70);
    clock.operator_added(&added, at, &no_children(), &no_jobs());
    assert_eq!(
        clock.due_at(),
        Some(at + super::OPERATOR_ADD_NAG_AFTER),
        "the operator's add is due at its own window, from the add"
    );

    // A clock whose window is already shorter keeps it — a test's 300ms is not lengthened.
    let mut quick = super::TodoNagClock::with_window(Duration::from_millis(300));
    quick.operator_added(&added, t0, &no_children(), &no_jobs());
    assert_eq!(quick.due_at(), Some(t0 + Duration::from_millis(300)));
}

/// **An add is told from a re-push by the words**: the head sends its whole half on every add,
/// delete and HELLO, and only a new open row is the operator asking for something.
#[test]
fn an_operator_add_is_a_new_open_row_on_their_half_and_nothing_else() {
    let before = [row("one", TodoStatus::Pending)];
    assert!(!super::the_operator_added(&before, &before), "a re-push");
    assert!(super::the_operator_added(
        &before,
        &[
            row("one", TodoStatus::Pending),
            row("two", TodoStatus::Pending)
        ]
    ));
    assert!(
        super::the_operator_added(&before, &[row("one, re-worded", TodoStatus::Pending)]),
        "a re-wording is a new row by its words"
    );
    assert!(
        !super::the_operator_added(&before, &[]),
        "a delete is not an add"
    );
    assert!(
        !super::the_operator_added(
            &before,
            &[
                row("one", TodoStatus::Pending),
                row("done", TodoStatus::Completed)
            ]
        ),
        "a row pushed as done is not work being asked for"
    );
    assert!(
        !super::the_operator_added(
            &before,
            &[
                row("one", TodoStatus::Pending),
                TodoItem {
                    by: TodoBy::Model,
                    ..row("the model's", TodoStatus::Pending)
                }
            ]
        ),
        "only the operator's half"
    );
}
