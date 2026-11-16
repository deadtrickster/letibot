//! Rows waiting behind a target that cannot land anything, and what its fix has come to. A red
//! target stops every row behind it without moving any of them, so the rows' sessions are told
//! when it goes red and again each time its fix stops.

use letibot_tokencore::store::{
    HealthState, MergeRow, MergeState, ReviewKind, ReviewRow, ReviewState, Store, StoreError,
    TargetHealth,
};

/// How long a fixer may have ended before its branch is in review: its branch is enqueued after
/// the child's own end is recorded.
pub const FIXER_GRACE_MS: u64 = 120_000;

/// What the fix of a red tip has come to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fix {
    /// Still going, or not started: the words say which.
    Going(String),
    /// Stopped: `key` names the stop, so each stop wakes the waiters once.
    Stopped { key: String, words: String },
}

impl Fix {
    pub fn key(&self) -> &str {
        match self {
            Fix::Going(_) => "red",
            Fix::Stopped { key, .. } => key,
        }
    }
    pub fn words(&self) -> &str {
        match self {
            Fix::Going(w) | Fix::Stopped { words: w, .. } => w,
        }
    }
}

/// One row waiting on the target, and the session that put it there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiter {
    pub session: String,
    pub row: String,
    pub branch: String,
    /// The landing that turned the target red, not a row in line.
    pub culprit: bool,
}

/// One target that cannot land, its fix, and who waits on it.
#[derive(Debug, Clone)]
pub struct Behind {
    pub health: TargetHealth,
    pub fix: Fix,
    pub waiting: Vec<Waiter>,
}

impl Behind {
    /// The once-only key of one wake: the tip, its state and the fix's stop.
    pub fn key(&self) -> String {
        format!(
            "behind:{}:{}:{}:{}:{}",
            self.health.repository,
            self.health.target,
            self.health.sha,
            self.health.state.as_str(),
            self.fix.key()
        )
    }
}

/// Every target of `repository` (all, for `None`) that cannot land, with its waiters.
pub fn behind(
    store: &Store,
    repository: Option<&str>,
    now_ms: u64,
) -> Result<Vec<Behind>, StoreError> {
    let healths: Vec<TargetHealth> = store
        .target_healths()?
        .into_iter()
        .filter(|h| repository.is_none_or(|r| h.repository == r))
        .filter(super::serve::emergency_health)
        .collect();
    if healths.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for h in healths {
        let reviews = store.review_rows(Some(&h.repository))?;
        let merges = store.merge_rows(Some(&h.repository))?;
        let fix = fix_of(store, &h, &reviews, &merges, now_ms)?;
        let mut waiting = Vec::new();
        for m in merges.iter().filter(|m| m.target == h.target) {
            let culprit = m.state == MergeState::Landed && m.landed_sha.as_deref() == Some(&h.sha);
            if !(culprit || (m.state == MergeState::Queued && m.fixes.is_none())) {
                continue;
            }
            if let Some(r) = reviews.iter().find(|r| r.id == m.review_id)
                && !r.session_id.is_empty()
            {
                waiting.push(Waiter {
                    session: r.session_id.clone(),
                    row: m.id.clone(),
                    branch: m.branch.clone(),
                    culprit,
                });
            }
        }
        for r in reviews.iter().filter(|r| {
            r.kind == ReviewKind::Branch
                && r.fixes.is_none()
                && r.target == h.target
                && matches!(r.state, ReviewState::Waiting | ReviewState::Reviewing)
                && !r.session_id.is_empty()
        }) {
            waiting.push(Waiter {
                session: r.session_id.clone(),
                row: r.id.clone(),
                branch: r.branch.clone(),
                culprit: false,
            });
        }
        if !waiting.is_empty() {
            out.push(Behind {
                health: h,
                fix,
                waiting,
            });
        }
    }
    Ok(out)
}

fn fix_of(
    store: &Store,
    h: &TargetHealth,
    reviews: &[ReviewRow],
    merges: &[MergeRow],
    now_ms: u64,
) -> Result<Fix, StoreError> {
    if h.state != HealthState::Red {
        return Ok(Fix::Going(format!(
            "no fixer is opened for a tip that is {} - a person regates it once the cause is gone",
            h.state.as_str()
        )));
    }
    let Some(t) = reviews
        .iter()
        .filter(|r| r.kind == ReviewKind::Target && r.target == h.target && r.head_sha == h.sha)
        .max_by_key(|r| r.state_ms)
    else {
        return Ok(Fix::Going("the red tip waits for its reviewer".into()));
    };
    let stop = |what: &str, id: &str, words: String| Fix::Stopped {
        key: format!("{what}:{id}"),
        words,
    };
    match t.state {
        ReviewState::Waiting | ReviewState::Reviewing => {
            return Ok(Fix::Going("the red tip is being reviewed".into()));
        }
        ReviewState::Blocked | ReviewState::CannotDecide => {
            return Ok(stop(
                t.state.as_str(),
                &t.id,
                format!(
                    "the red tip's review `{}` is {}: {}",
                    t.id,
                    t.state.as_str(),
                    first_line(&t.evidence)
                ),
            ));
        }
        ReviewState::Rejected => {}
        _ => return Ok(Fix::Going("the red tip is being gated again".into())),
    }
    let Some(fixer) = t.fixes.as_deref() else {
        return Ok(stop(
            "no-fixer",
            &t.id,
            format!(
                "no fixer was opened ({}), so a person has to fix `{}`",
                first_line(&t.evidence),
                h.target
            ),
        ));
    };
    let fixes = reviews
        .iter()
        .filter(|r| r.kind == ReviewKind::Branch && r.fixes.as_deref() == Some(&h.sha))
        .max_by_key(|r| r.state_ms);
    let Some(f) = fixes else {
        let ended = store
            .subagent_run(fixer)?
            .filter(|run| !matches!(run.state.as_str(), "running" | "opening"))
            .filter(|run| run.updated_ms.saturating_add(FIXER_GRACE_MS) <= now_ms);
        return Ok(match ended {
            Some(run) => stop(
                "gave-up",
                fixer,
                format!(
                    "the fixer `{fixer}` ended ({}) without putting a branch up for review",
                    run.state
                ),
            ),
            None => Fix::Going(format!("the fixer `{fixer}` is working on it")),
        });
    };
    let words = |what: &str| {
        format!(
            "the fix `{}` by `{fixer}` was {what}: {}",
            f.branch,
            first_line(&f.evidence)
        )
    };
    Ok(match f.state {
        ReviewState::Waiting | ReviewState::Reviewing => {
            Fix::Going(format!("the fix `{}` by `{fixer}` is in review", f.branch))
        }
        ReviewState::Rejected => stop("rejected", &f.id, words("rejected")),
        ReviewState::CannotDecide | ReviewState::Blocked | ReviewState::Withdrawn => {
            stop(f.state.as_str(), &f.id, words(f.state.as_str()))
        }
        ReviewState::Approved => match merges
            .iter()
            .filter(|m| m.review_id == f.id)
            .max_by_key(|m| m.state_ms)
        {
            Some(m) if super::notices::wakes_owner(m.state) || m.state == MergeState::Withdrawn => {
                stop(
                    m.state.as_str(),
                    &m.id,
                    format!(
                        "the fix `{}`'s landing `{}` is {}: {}",
                        f.branch,
                        m.id,
                        m.state.as_str(),
                        first_line(&m.evidence)
                    ),
                )
            }
            _ => Fix::Going(format!("the fix `{}` is landing", f.branch)),
        },
        ReviewState::Archived => Fix::Going(format!("the fix `{}` is archived", f.branch)),
    })
}

/// Why the tip is red, in one line: the failing step and its last line of output.
pub fn cause(h: &TargetHealth) -> String {
    match super::notices::failing_step(&h.gate_steps) {
        Some((n, of, s, tail)) => {
            let last = tail
                .lines()
                .rev()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("it printed nothing");
            format!("step {n} of {of} `{}` failed: {last}", s.command)
        }
        None => first_line(&h.evidence).to_string(),
    }
}

/// The one row a session is told: the target, why, the fix, its own rows, and the next step.
pub fn told(b: &Behind, mine: &[&Waiter]) -> String {
    let h = &b.health;
    let rows: Vec<String> = mine
        .iter()
        .map(|w| {
            if w.culprit {
                format!("`{}` (your landing of `{}` is the tip)", w.row, w.branch)
            } else {
                format!("`{}` (`{}`)", w.row, w.branch)
            }
        })
        .collect();
    let next = match &b.fix {
        Fix::Going(_) => "nothing to do yet - you are told again if the fix stops".to_string(),
        Fix::Stopped { .. } => format!(
            "fix `{}` yourself and put the fix up for review, or tell the operator; the rows wait \
             until `{}` is green",
            h.target, h.target
        ),
    };
    format!(
        "[queue waits] `{}` in {} is {} at {} because {}. Fix: {}. Your rows wait behind it: {}. \
         Next: {next}.",
        h.target,
        h.repository,
        h.state.as_str(),
        h.sha.chars().take(12).collect::<String>(),
        cause(h),
        b.fix.words(),
        rows.join(", "),
    )
}

fn first_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("(no evidence recorded)")
        .trim_end_matches('.')
}
