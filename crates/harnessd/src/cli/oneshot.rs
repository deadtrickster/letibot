//! The one-shot's job tail: a `--prompt` run does not exit while a job it started is still open.
//!
//! A one-shot's daemon has no worker, so the settlement a job's end rings had nobody to run the
//! turn it was for. The run printed its answer, said `kept (one-shot ended): a job is open` and
//! exited 0 with the work half done and nothing to resume it. This is the missing half: the same
//! bell the worker waits on, the same [`crate::Sessions::wake`] door it takes, and a wall budget
//! for the waiting - because a job may be wedged, and one-shot must still end.

use std::time::{Duration, Instant};

use letibot_sessionlog::registry::Bell;
use letibot_tokencore::store::Store;

use crate::Outcome;

/// How long a one-shot may run in total - its steps, and then the jobs it left running - when
/// `--one-shot-budget` says nothing. Six hours: longer than a build or a test run anybody starts
/// from the command line, and short enough that a wedged job is not waited on for ever.
pub const BUDGET: Duration = Duration::from_secs(6 * 60 * 60);

/// The exit status of a one-shot that ran out of budget with a job still open. Distinct from `1`
/// (a step failed) and `2` (the command line was refused), and `EX_TEMPFAIL`'s own number because
/// what it says is "not now": nothing is lost, and resuming the session is the next step.
pub const EXIT_STILL_OPEN: i32 = 75;

/// What the job tail came to.
pub enum Tail {
    /// Every job this session started has ended. `turns` is how many wake turns ran, which is 0
    /// for the ordinary one-shot that started nothing.
    Settled { turns: usize },
    /// The budget ran out with these handles still open.
    Over { jobs: Vec<String> },
}

/// `--one-shot-budget SPAN`'s whole body, in a function so the one thing it must get right is a
/// value a test can read. A span is the files' own spelling (`90s`, `1h30m`, `6h`), and `0` is
/// refused: this is the only bound on the waiting, so a run with no bound is a run nobody can stop
/// from a script.
pub fn budget_from_flag(word: &str) -> Result<Duration, String> {
    let span = crate::leticode_config::parse_span(word)
        .map_err(|why| format!("--one-shot-budget: {why}"))?;
    if span.is_zero() {
        return Err(
            "--one-shot-budget: a one-shot needs a bound, and 0 is not one - a job that \
                    never ends still has to end the run, and the waiting is the half this bounds"
                .into(),
        );
    }
    Ok(span)
}

/// The status a one-shot ends on. `0` is the only pass, and it needs BOTH halves: no step failed
/// and no job is still open. `75` outranks `1` because it is the more specific fact - the work is
/// not lost, and the session named on the last line resumes it.
pub fn status(jobs_still_open: bool, failed_steps: usize) -> i32 {
    match (jobs_still_open, failed_steps) {
        (true, _) => EXIT_STILL_OPEN,
        (false, 0) => 0,
        (false, _) => 1,
    }
}

/// The handles this session has open in the store - what the archive guard calls "a job is open",
/// so this and the sentence the operator saw are one fact. A store that cannot be read answers
/// nothing: no rows is not the same as no jobs, and a run that cannot tell must not wait for ever.
pub fn open_jobs(store: &Store, session: &str) -> Vec<String> {
    store
        .open_processes()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.session == session && p.state.is_open())
        .map(|p| p.handle)
        .collect()
}

/// The handles as a person reads them: one backticked id, or several.
fn handles(jobs: &[String]) -> String {
    jobs.iter()
        .map(|j| format!("`{j}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The one line a run out of budget ends on: the jobs still open, and the door that resumes them.
pub fn over_budget_line(session: &str, jobs: &[String], budget: Duration) -> String {
    format!(
        "the one-shot's budget of {} ran out with {} still open - the work is not lost: \
         `letibot --session {session}` resumes the session and delivers each job's result",
        crate::leticode_config::span_words(budget),
        handles(jobs),
    )
}

/// Wait for the jobs this session left running, running the turn each settlement owes, until none
/// is open or `deadline` passes.
///
/// The wait is the bell's - the settlement a job's end rung is taken off the queue, and `wake` is
/// the daemon's own door ([`crate::Sessions::wake`]), so the model hears `[job] … has ended` in the
/// harness's own words rather than in a second dialect. Nothing here polls: a job that is still
/// running rings nothing, and the deadline is the only clock - which is why a box a test runs on
/// can drive the whole loop with a store, a bell and a closure.
pub fn drain(
    store: &Store,
    session: &str,
    bell: &Bell,
    deadline: Instant,
    mut wake: impl FnMut() -> Outcome,
) -> Tail {
    let mut turns = 0;
    loop {
        let jobs = open_jobs(store, session);
        if jobs.is_empty() {
            return Tail::Settled { turns };
        }
        if Instant::now() >= deadline {
            return Tail::Over { jobs };
        }
        // The wake may already be queued - a job that ended while the turn that started it was
        // still writing its tail rang it then - so this answers at once in that case.
        if !bell.wait_wake(session, Some(deadline)) {
            // Either the deadline passed or the daemon is stopping. The store decides which of
            // those leaves a job open, because that is the fact a resumer reads.
            let jobs = open_jobs(store, session);
            return if jobs.is_empty() {
                Tail::Settled { turns }
            } else {
                Tail::Over { jobs }
            };
        }
        match wake() {
            // A turn ran, so the settlement reached the model and the job tail moved on.
            Outcome::Replied(_) | Outcome::Compacted(_) => turns += 1,
            // A wake with nothing to say. The job is still open in the store, so the loop waits
            // for the next settlement rather than spinning.
            Outcome::Ignored | Outcome::HandedOn => {}
            Outcome::Failed(why) => eprintln!("  ! a turn after a job's end failed: {why}"),
        }
    }
}
