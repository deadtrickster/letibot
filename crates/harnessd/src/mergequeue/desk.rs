//! **The `queue` tool's daemon half** (R76 item 2) — what a session's model can do about its
//! project's queues: read them, re-gate the target, restart or withdraw a row, and PROPOSE the
//! branch the project lands on.
//!
//! The operator, on pg-noop's red `main`: *"it should be able to get out of it itself
//! obviously"*. Every verb here is one the operator already had (`mergequeue::verbs`), moved under
//! the session's own name (`agent:<session>`) so the history says a model did it. One is not:
//! **setting `[queue] target` is the operator's decision**, so `propose_target` raises it on every
//! hub, naming the exact `leticode.toml` line, records the proposal in the target's history, and
//! writes no file.
//!
//! **`status` is scoped and bounded** (see the three shapes below): the session's own repository
//! and a screenful of it by default, one line per repository for `all`, and one repository in full
//! for `repository`. Every answer's first line says which of the three it printed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{
    HealthState, MergeRow, MergeState, QueueLease, ReviewRow, ReviewState, Store, TargetHealth,
};
use letibot_tools::builtins::queue::QueueDesk;

use super::gate::{self, now_ms};
use super::{git, notices, settings, verbs};

/// **The repository a session's queue verbs are about, and its target** — the one `workspace`
/// means (`repo_of_workspace`), keyed the way the queues key it, unless `named` is a path; and
/// the project's `[queue] target` (else `main`/`master`) when no target is named.
pub fn resolve(
    workspace: &Path,
    named: Option<&str>,
    target: Option<&str>,
) -> Result<(String, String), String> {
    let repo = match named {
        Some(p) => git::repo_of_worktree(Path::new(p))
            .ok_or_else(|| format!("{p} is not a git repository"))?,
        None => git::repo_of_workspace(workspace)
            .and_then(|r| git::repo_of_worktree(&r))
            .ok_or_else(|| {
                // **A project of several repositories names them**, so the one to name is a
                // choice among the right ones rather than a path to go and find.
                let many = git::repos_of_workspace(workspace);
                if many.len() > 1 {
                    let names: Vec<String> =
                        many.iter().map(|r| format!("`{}`", r.display())).collect();
                    return format!(
                        "the workspace {} is one project of {} repositories — {} — so which one is \
                         not something this can know; name it (`/queue regate /path/to/repo \
                         [TARGET]`, or the tool's `repository`)",
                        workspace.display(),
                        many.len(),
                        names.join(", ")
                    );
                }
                format!(
                    "the workspace {} names no one git repository; name it (`/queue regate \
                     /path/to/repo [TARGET]`, or the tool's `repository`)",
                    workspace.display()
                )
            })?,
    };
    let target = match target {
        Some(t) => t.to_string(),
        None => {
            let s = settings::for_repository(&repo).unwrap_or_default();
            settings::target_for(&repo, &s).ok_or_else(|| {
                format!("{} has no target branch to gate; name one", repo.display())
            })?
        }
    };
    Ok((repo.display().to_string(), target))
}

/// **One session's desk** — its store, its workspace, and the hubs a move is announced on.
pub struct SessionQueue {
    pub store_path: PathBuf,
    pub session: String,
    pub workspace: PathBuf,
    pub registry: Arc<Registry>,
}

impl SessionQueue {
    fn store(&self) -> Result<Store, String> {
        Store::open(&self.store_path).map_err(|e| e.to_string())
    }

    fn who(&self) -> String {
        format!("agent:{}", self.session)
    }

    fn events(&self) -> impl Fn(SessionEvent) + '_ {
        move |e| {
            self.registry.broadcast(e);
        }
    }
}

impl QueueDesk for SessionQueue {
    fn status(&self, all: bool, repository: Option<&str>) -> Result<String, String> {
        let store = self.store()?;
        match scope(&store, &self.workspace, all, repository)? {
            Scope::Every => every(&store),
            // **Naming a repository is asking for it in full** — that is the door the bounded
            // default leaves open, and the shape `/clear` has always printed.
            Scope::One { repo, named: true } => detail(&store, &repo),
            Scope::One { repo, named: false } => bounded(&store, &repo),
            Scope::Nowhere => Ok(nowhere(&self.workspace)),
        }
    }

    fn regate(&self, repository: Option<&str>, target: Option<&str>) -> Result<String, String> {
        let (repo, target) = resolve(&self.workspace, repository, target)?;
        verbs::regate(&self.store()?, &repo, &target, &self.who(), &self.events())
    }

    fn restart(&self, id: &str) -> Result<String, String> {
        verbs::restart_by(&self.store()?, id, &self.who(), &self.events())
    }

    fn withdraw(&self, id: &str, why: &str) -> Result<String, String> {
        let words = format!("withdrawn by {}: {why}", self.who());
        verbs::withdraw_by(&self.store()?, id, &self.who(), &words, &self.events())
    }

    fn propose_target(&self, target: &str, why: &str) -> Result<String, String> {
        propose_target(
            &self.store()?,
            &self.workspace,
            target,
            why,
            &self.who(),
            &self.events(),
        )
    }
}

/// **Put `[queue] target` to the operator** — raised on every hub as `queue_target_proposed`
/// with the exact lines and the file, recorded in the target's history; nothing is written.
pub fn propose_target(
    store: &Store,
    workspace: &Path,
    target: &str,
    why: &str,
    who: &str,
    events: &dyn Fn(SessionEvent),
) -> Result<String, String> {
    let (repo, now) = resolve(workspace, None, None)?;
    let repo_path = Path::new(&repo);
    if git::branch_head(repo_path, target).is_none() {
        return Err(format!("`{target}` is not a branch of {repo}"));
    }
    if now == target {
        return Err(format!("the queue already lands {repo} on `{target}`"));
    }
    let file = settings::for_repository(repo_path)
        .ok()
        .and_then(|s| s.path)
        .unwrap_or_else(|| repo_path.join("leticode.toml"));
    let detail = format!(
        "{who} PROPOSES that {repo} lands on `{target}` (the queue gates `{now}` now): {why}. \
         It is your decision and nothing was written. To take it, put in {}:\n\n[queue]\ntarget = \
         \"{target}\"\n\nThe queue reads it on its next pass; `/queue regate {repo} {target}` \
         then gates the new target's tip.",
        file.display()
    );
    events(SessionEvent::Warning {
        code: "queue_target_proposed".into(),
        detail: detail.clone(),
        compaction: None,
    });
    store
        .record_transition(
            "target",
            target,
            &repo,
            None,
            "proposed",
            who,
            &format!("proposed as the target instead of `{now}`: {why}"),
            now_ms(),
        )
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "Put to the operator (nothing was written): `[queue] target = \"{target}\"` in {}. Until \
         they take it, the queue gates `{now}`.",
        file.display()
    ))
}

// ===== `status`: three shapes, one scope line =====
//
// The operator, 2026-10-30: *"the problem im solving is not mine but yours - it is more than 60
// rows for queue status tool call now - and you guys defocus and get confused"*. The cost is the
// model's attention, so the default answer is ONE repository and a screenful of it, `all` is one
// line per repository, and `repository: "<name>"` is the full shape. Every answer opens with the
// scope it printed: a bounded answer that does not say it is bounded is the same lie in a smaller
// font.

/// **How many rows a bounded answer prints in full** before it counts the rest.
const SHOWN_ROWS: usize = 10;

/// The lines of a failing step's tail a bounded answer keeps — the full shape keeps all 30.
const SHOWN_TAIL: usize = 3;

/// The characters a row's own words may run to in a bounded answer.
const SHOWN_EVIDENCE: usize = 200;

/// **How recently a step's log must have grown for the line to say `tail moving`** — a build
/// prints between crates and a test run prints between binaries, so a minute of silence is not a
/// stall; a minute is long enough that a moving tail is news and short enough that a reader sees
/// it in the same breath as the step.
const TAIL_MOVING_MS: u64 = 60_000;

/// **How long a step's log may stand still before the line says it may be stuck** — the same ten
/// minutes the head's queue pane waits (`GATE_QUIET_MS`), because it is the same question about
/// the same file, and two answers to it would be one too many.
const TAIL_STILL_MS: u64 = 10 * 60 * 1000;

/// **The states rows are counted in, in the queue's own order** — the flow, not the alphabet: what
/// waits, what is running, what is parked on a person.
const FLOW: [&str; 10] = [
    "waiting",
    "reviewing",
    "queued",
    "gating",
    "blocked",
    "rejected",
    "cannot_decide",
    "conflict",
    "failed",
    "timed_out",
];

/// **A review row somebody still has to deal with** — the queue's business (open) or a person's (a
/// verdict that needs a decision). `pub(super)` for `mergequeue::stuck`, which counts the half of
/// it that is nobody's: one rule, so the census's numbers are a sub-line of `status`'s.
pub(super) fn parked(r: ReviewState) -> bool {
    r.is_open() || matches!(r, ReviewState::Rejected | ReviewState::CannotDecide)
}

/// **A landing row somebody still has to deal with**, the same rule one queue over.
pub(super) fn parked_merge(m: MergeState) -> bool {
    m.is_open()
        || matches!(
            m,
            MergeState::Failed | MergeState::TimedOut | MergeState::Conflict
        )
}

/// **What one `status` call is about** — decided once, so the line the answer opens with and the
/// rows under it cannot disagree.
enum Scope {
    /// `all: true` — every repository, one line each.
    Every,
    /// One repository. `named` is whether the CALLER named it, which is what buys the full shape.
    One { repo: String, named: bool },
    /// The session is in no one repository, and named none.
    Nowhere,
}

/// **Which repository a `status` call is about.**
fn scope(
    store: &Store,
    workspace: &Path,
    all: bool,
    repository: Option<&str>,
) -> Result<Scope, String> {
    if let Some(asked) = repository {
        if all {
            return Err(
                "`all` and `repository` ask for two different scopes; name one of them".into(),
            );
        }
        return one(asked, &served(store)?, workspace).map(|repo| Scope::One { repo, named: true });
    }
    if all {
        return Ok(Scope::Every);
    }
    Ok(match current(workspace) {
        Some(repo) => Scope::One { repo, named: false },
        None => Scope::Nowhere,
    })
}

/// **The repository this session is in** — the workspace itself when git says it is in one, the
/// same-named child (`~/Projects/letibot` → `letibot`), or its only repository child; then mapped
/// through the WORKTREE to its main checkout, which is how the queues key a repository, so a
/// session in a worktree reads the queue its branch lands in.
///
/// A folder that is one project of several answers `None`, and that is not a hole to fill with the
/// whole box: which of `stroppy`, `website` and `stroppy-contrib` a session seated above them is
/// "in" is a fact nothing in this tree knows, so the answer says so and names both ways to widen.
fn current(workspace: &Path) -> Option<String> {
    git::repo_of_workspace(workspace)
        .and_then(|r| git::repo_of_worktree(&r))
        .map(|p| p.display().to_string())
}

/// **The one repository `asked` names** — a path git resolves, else a name matched against the
/// repositories the queue holds: a full path, a path's tail or a directory name.
///
/// Two matches is a refusal that names both. Picking one is how a model comes to read another
/// project's queue and believe it is its own.
fn one(asked: &str, served: &[String], workspace: &Path) -> Result<String, String> {
    let asked = asked.trim().trim_end_matches('/');
    let as_path = if Path::new(asked).is_absolute() {
        PathBuf::from(asked)
    } else {
        workspace.join(asked)
    };
    if let Some(repo) = git::repo_of_worktree(&as_path) {
        return Ok(repo.display().to_string());
    }
    let list = |keys: &[&String]| {
        keys.iter()
            .map(|k| format!("`{k}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let hits: Vec<&String> = served.iter().filter(|k| names(k, asked)).collect();
    match hits.as_slice() {
        [only] => Ok((*only).clone()),
        [] => Err(format!(
            "`{asked}` matched no repository — the queue holds {}: {}",
            served.len(),
            list(&served.iter().collect::<Vec<_>>())
        )),
        many => Err(format!(
            "`{asked}` matched {} repositories — {} — so which one is not something this can \
             know; name one of them in full, or ask for `all: true`",
            many.len(),
            list(many)
        )),
    }
}

/// **`key` is `asked`, or ends with it at a path boundary** — so `/a/letim`, `Projects/letim` and
/// `letim` all name `/a/letim`, and `im` names nothing.
fn names(key: &str, asked: &str) -> bool {
    key == asked
        || key
            .strip_suffix(asked)
            .is_some_and(|head| head.ends_with('/'))
}

/// **Every repository the queue holds** — the healths, the rows and the leases, deduped and
/// sorted: what `all` counts and what a name is matched against.
fn served(store: &Store) -> Result<Vec<String>, String> {
    let mut all: Vec<String> = store
        .target_healths()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|h| h.repository)
        .collect();
    all.extend(
        store
            .review_rows(None)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|r| r.repository),
    );
    all.extend(
        store
            .merge_rows(None)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|m| m.repository),
    );
    all.extend(
        store
            .queue_leases()
            .unwrap_or_default()
            .into_iter()
            .map(|l| l.repository),
    );
    all.retain(|r| !r.is_empty());
    all.sort();
    all.dedup();
    Ok(all)
}

/// **`all` — one line per repository**, so the whole box fits in a screenful: which target is in
/// what state, what is open, and whether anybody serves it. Detail is what the other two forms are
/// for.
///
/// **A repository whose gate is running says so here too** — `letibot  GATING · step 3 of 4 ·
/// 6m12s · 3 queued` — because the session that needs this most is the one in ANOTHER repository:
/// it has no row for this gate and no reason to open one, and the fans it hears are this. The
/// step and the clock only: the tail is the detail's, and this line is a screenful for the box.
fn every(store: &Store) -> Result<String, String> {
    let now = now_ms();
    let repos = served(store)?;
    let healths = store.target_healths().map_err(|e| e.to_string())?;
    let reviews = store.review_rows(None).map_err(|e| e.to_string())?;
    let merges = store.merge_rows(None).map_err(|e| e.to_string())?;
    let leases = store.queue_leases().unwrap_or_default();
    let red = repos
        .iter()
        .filter(|r| newest(&healths, r).is_some_and(|h| h.state == HealthState::Red))
        .count();
    let mut header = repos.len().to_string();
    if red > 0 {
        header.push_str(&format!(", {red} red"));
    }
    let mut out = format!("every repository ({header})\n");
    for (repo, name) in repos.iter().zip(short_names(&repos)) {
        let health = newest(&healths, repo);
        let mut state = match health {
            Some(h) => h.state.as_str().to_uppercase(),
            None => "no target".into(),
        };
        if let Some(g) = health.and_then(running_now) {
            state.push_str(&format!(
                " · step {} of {} · {}",
                g.step.max(1),
                g.of.max(1),
                dur_human(now.saturating_sub(g.started_ms))
            ));
        }
        let open = states_of(
            reviews
                .iter()
                .filter(|r| r.repository == *repo && parked(r.state))
                .map(|r| r.state.as_str())
                .chain(
                    merges
                        .iter()
                        .filter(|m| m.repository == *repo && parked_merge(m.state))
                        .map(|m| m.state.as_str()),
                ),
        );
        let unserved = !open.is_empty() && !live_lease(&leases, repo);
        out.push_str(&format!(
            "{name}  {state} · {}{}\n",
            if open.is_empty() {
                "0 open".to_string()
            } else {
                open.join(" · ")
            },
            if unserved { " · unserved" } else { "" }
        ));
    }
    Ok(out)
}

/// **The default answer: this repository, and no more than a screenful of it** — the target, what
/// each queue holds by state, the ten rows somebody has to act on first, and who serves it.
fn bounded(store: &Store, repo: &str) -> Result<String, String> {
    let healths: Vec<TargetHealth> = store
        .target_healths()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|h| h.repository == repo)
        .collect();
    let reviews: Vec<ReviewRow> = store
        .review_rows(None)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|r| r.repository == repo)
        .collect();
    let merges: Vec<MergeRow> = store
        .merge_rows(None)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|m| m.repository == repo)
        .collect();
    let leases: Vec<QueueLease> = store
        .queue_leases()
        .unwrap_or_default()
        .into_iter()
        .filter(|l| l.repository == repo)
        .collect();
    let today = midnight_ms(now_ms());
    let approved = reviews
        .iter()
        .filter(|r| r.state == ReviewState::Approved && r.state_ms >= today)
        .count();
    let landed = merges
        .iter()
        .filter(|m| m.state == MergeState::Landed && m.state_ms >= today)
        .count();
    let mut out = format!("the queue for {repo}\n");
    if healths.is_empty() {
        out.push_str("no target is gated here — the queue has not looked at this repository\n");
    }
    out.push_str(&targets(&healths, Some(SHOWN_TAIL)));
    out.push_str(&format!(
        "review queue: {}\n",
        counted(
            reviews
                .iter()
                .filter(|r| parked(r.state))
                .map(|r| r.state.as_str()),
            approved,
            "approved"
        )
    ));
    out.push_str(&format!(
        "merge queue: {}\n",
        counted(
            merges
                .iter()
                .filter(|m| parked_merge(m.state))
                .map(|m| m.state.as_str()),
            landed,
            "landed"
        )
    ));
    let (lines, more) = first_rows(&reviews, &merges);
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    if more > 0 {
        out.push_str(&format!(
            "+{more} more row(s) — `queue` with `action: \"status\"` and `repository: \".\"` prints \
             every row of this repository\n"
        ));
    }
    out.push_str(&served_by(&leases));
    Ok(out)
}

/// **`repository: "<name>"` — one repository in full**: the target with the whole tail of its
/// failing step, every open or parked row, and who serves it.
fn detail(store: &Store, repo: &str) -> Result<String, String> {
    Ok(format!(
        "the queue for {repo}\n{}",
        status(store, Some(repo))?
    ))
}

/// **A session that is in no one repository** — one sentence and the two ways to widen. Printing
/// the whole box is the thing this exists to stop.
fn nowhere(workspace: &Path) -> String {
    let many = git::repos_of_workspace(workspace);
    let mut out = if many.is_empty() {
        format!(
            "this session is in no one repository — {} is in none and holds none\n",
            workspace.display()
        )
    } else {
        let keys: Vec<String> = many.iter().map(|p| p.display().to_string()).collect();
        format!(
            "this session is in no one repository — {} is one project of {} ({}) and which one it \
             is in is not something this can know\n",
            workspace.display(),
            many.len(),
            short_names(&keys).join(", ")
        )
    };
    out.push_str(
        "widen it with `all: true` (one line per repository on the box) or `repository: \"<path or \
         name>\"` (one repository in full)\n",
    );
    out
}

/// **Both queues and the targets of one repository, as a model reads them, in full** — what
/// `/clear` has always printed and what `repository: "<name>"` still prints.
pub fn status(store: &Store, repository: Option<&str>) -> Result<String, String> {
    let here = |r: &str| repository.is_none_or(|k| k == r);
    let healths: Vec<TargetHealth> = store
        .target_healths()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|h| here(&h.repository))
        .collect();
    let mut out = targets(&healths, None);
    let reviews = store.review_rows(None).map_err(|e| e.to_string())?;
    let open_reviews: Vec<&ReviewRow> = reviews
        .iter()
        .filter(|r| here(&r.repository) && parked(r.state))
        .collect();
    out.push_str(&format!(
        "review queue: {} row(s) open or parked\n",
        open_reviews.len()
    ));
    for r in open_reviews {
        out.push_str(&review_line(r, None));
        out.push('\n');
    }
    let merges = store.merge_rows(None).map_err(|e| e.to_string())?;
    let open_merges: Vec<&MergeRow> = merges
        .iter()
        .filter(|m| here(&m.repository) && parked_merge(m.state))
        .collect();
    out.push_str(&format!(
        "merge queue: {} row(s) open or parked\n",
        open_merges.len()
    ));
    for m in open_merges {
        out.push_str(&merge_line(m, None));
        out.push('\n');
    }
    let leases: Vec<QueueLease> = store
        .queue_leases()
        .unwrap_or_default()
        .into_iter()
        .filter(|l| here(&l.repository))
        .collect();
    out.push_str(&served_by(&leases));
    Ok(out)
}

/// **A repository's target, and the gate that failed on it** — the whole tail in the full shape
/// (`None`), the last [`SHOWN_TAIL`] lines of it in a bounded one, with the count of what was cut.
///
/// **A target whose gate is running says what it is doing, on its own line** (2026-11-01). The
/// operator, hearing the fans and looking at a screen: *"nothing about gate — and the same for
/// other sessions — gate is invisible"*. Everything about the running step was already in the
/// snapshot and in a merge row's detail, and a detail is a click away — so a session in this
/// repository, and a person reading its `status`, had no way to learn that the machine was
/// working rather than stuck. This is the headline: which step, how long, and whether its output
/// is moving. The tail itself stays the detail's business.
fn targets(healths: &[TargetHealth], tail: Option<usize>) -> String {
    let now = now_ms();
    let mut out = String::new();
    for h in healths {
        let running = running_now(h)
            .map(|g| format!(" · {}", gate_words(&g, now)))
            .unwrap_or_default();
        out.push_str(&format!(
            "target `{}` in {}: {} at {}{}\n  {}\n",
            h.target,
            h.repository,
            h.state.as_str().to_uppercase(),
            short_sha(&h.sha),
            running,
            h.evidence.lines().next().unwrap_or_default()
        ));
        if h.state == HealthState::Green {
            continue;
        }
        let Some((n, of, step, text)) = notices::failing_step(&h.gate_steps) else {
            continue;
        };
        out.push_str(&format!(
            "  failing step {n} of {of}: `{}`; the end of its output:\n",
            step.command
        ));
        let lines: Vec<&str> = text.lines().collect();
        let keep = tail.unwrap_or(lines.len()).min(lines.len());
        for l in &lines[lines.len() - keep..] {
            out.push_str(&format!("    | {l}\n"));
        }
        if keep < lines.len() {
            out.push_str(&format!(
                "    | … {} more line(s); `repository: \".\"` prints them all\n",
                lines.len() - keep
            ));
        }
    }
    out
}

/// **The gate `h`'s target is running, if it is running one** — the health row says whether a gate
/// is on the tip, and the gate says which step. Two facts from two places, asked together so no
/// caller can pair them wrongly.
fn running_now(h: &TargetHealth) -> Option<gate::RunningGate> {
    (h.state == HealthState::Gating)
        .then(|| gate::running(&h.repository))
        .flatten()
}

/// **The gate a target is running, in one clause** — which step of how many, how long it has been
/// on it, and whether its output is still moving. The same three facts a merge row's detail gives
/// at length, on the target's own line, because that line is where a person looks first and the
/// detail is a click away. The tail itself is not here: a headline is not the whole story.
fn gate_words(g: &gate::RunningGate, now: u64) -> String {
    format!(
        "step {} of {} · {} · {}",
        g.step.max(1),
        g.of.max(1),
        dur_human(now.saturating_sub(g.started_ms)),
        tail_words(g, now)
    )
}

/// **Is the step's output moving?** — answered from ONE read, because the log's own mtime is when
/// its last byte landed. The head's pane needs two snapshots to answer this; a `status` answer is
/// one shot, and the file already carries the second half of the comparison.
fn tail_words(g: &gate::RunningGate, now: u64) -> String {
    let wrote = bytes_human(g.log_bytes);
    let Some(at) = g.wrote_ms else {
        return format!("{wrote} written");
    };
    let still = now.saturating_sub(at);
    if still < TAIL_MOVING_MS {
        format!("tail moving ({wrote})")
    } else if still < TAIL_STILL_MS {
        format!("tail moved {} ago ({wrote})", dur_human(still))
    } else {
        format!(
            "tail still for {} — it may be stuck ({wrote})",
            dur_human(still)
        )
    }
}

/// **A duration as a person reads it** — the shape the head's queue pane uses (`6m12s`), so a model
/// reading `status` and a person reading the pane are told the same thing in the same words.
fn dur_human(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// **Bytes as a person reads them** — a step's output is thousands of them, and the exact count is
/// a fact nobody acts on.
fn bytes_human(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{} KiB", n / 1024)
    } else {
        format!("{} MiB", n / (1024 * 1024))
    }
}

/// **The rows a bounded answer prints, worst first** — what is parked on a person, then what is
/// running, then what waits; an operator's urgent entry before a subagent's, oldest first inside
/// each. So what the count drops is the tail of the queue and nothing anybody must act on.
fn first_rows(reviews: &[ReviewRow], merges: &[MergeRow]) -> (Vec<String>, usize) {
    let mut lines: Vec<((u8, usize, u64), String)> = Vec::new();
    for r in reviews.iter().filter(|r| parked(r.state)) {
        let band = match r.state {
            ReviewState::Rejected | ReviewState::CannotDecide | ReviewState::Blocked => 0,
            ReviewState::Reviewing => 1,
            _ => 2,
        };
        lines.push((
            (band, r.priority.rank(), r.state_ms),
            review_line(r, Some(SHOWN_EVIDENCE)),
        ));
    }
    for m in merges.iter().filter(|m| parked_merge(m.state)) {
        let band = match m.state {
            MergeState::Failed
            | MergeState::TimedOut
            | MergeState::Conflict
            | MergeState::Blocked => 0,
            MergeState::Gating => 1,
            _ => 2,
        };
        lines.push((
            (band, m.priority.rank(), m.state_ms),
            merge_line(m, Some(SHOWN_EVIDENCE)),
        ));
    }
    lines.sort_by_key(|(rank, _)| *rank);
    let more = lines.len().saturating_sub(SHOWN_ROWS);
    lines.truncate(SHOWN_ROWS);
    (lines.into_iter().map(|(_, line)| line).collect(), more)
}

/// One review row as one line — the id, the state, the branch and the target, and what the queue
/// said. `cap` is the bounded answer's limit on the row's own words.
fn review_line(r: &ReviewRow, cap: Option<usize>) -> String {
    format!(
        "  `{}` {} `{}` → `{}` at {}: {}",
        r.id,
        r.state.as_str(),
        r.branch,
        r.target,
        short_sha(&r.head_sha),
        shorten(
            if r.failure.is_empty() {
                &r.evidence
            } else {
                &r.failure
            },
            cap
        )
    )
}

/// One landing row as one line, the same shape one queue over.
fn merge_line(m: &MergeRow, cap: Option<usize>) -> String {
    format!(
        "  `{}` {} `{}` → `{}`: {}",
        m.id,
        m.state.as_str(),
        m.branch,
        m.target,
        shorten(m.evidence.lines().next().unwrap_or_default(), cap)
    )
}

/// **Who serves it, and how long ago it said so** — the one line that says a queue is nobody's.
fn served_by(leases: &[QueueLease]) -> String {
    let mut out = String::new();
    for l in leases {
        let age = now_ms().saturating_sub(l.heartbeat_ms) / 1000;
        out.push_str(&format!(
            "served by {} ({}, heartbeat {age}s ago{})\n",
            l.owner,
            l.build,
            if age * 1000 >= super::lease::LEASE_TTL_MS {
                " — STALE: nobody is serving it"
            } else {
                ""
            }
        ));
    }
    out
}

/// Whether anybody's heartbeat on this repository is young enough to count.
fn live_lease(leases: &[QueueLease], repo: &str) -> bool {
    let now = now_ms();
    leases.iter().any(|l| {
        l.repository == repo && now.saturating_sub(l.heartbeat_ms) < super::lease::LEASE_TTL_MS
    })
}

/// **Rows counted by state, in the queue's own order** — only the states that are there, so a line
/// says `3 waiting · 1 gating` and never `0 of nine things`.
pub(super) fn states_of(states: impl Iterator<Item = &'static str>) -> Vec<String> {
    counts_of(states)
        .into_iter()
        .map(|(s, n)| format!("{n} {s}"))
        .collect()
}

/// **[`states_of`]'s counts, unformatted** — the same `FLOW` order and the same rule, for a reader
/// that wants the numbers rather than the line (`mergequeue::stuck`, whose counts have to be
/// `status`'s counts and not a second opinion about them).
pub(super) fn counts_of(states: impl Iterator<Item = &'static str>) -> Vec<(&'static str, usize)> {
    let mut counts: Vec<(&'static str, usize)> = FLOW.iter().map(|s| (*s, 0)).collect();
    for s in states {
        if let Some(slot) = counts.iter_mut().find(|(name, _)| *name == s) {
            slot.1 += 1;
        }
    }
    counts.into_iter().filter(|(_, n)| *n > 0).collect()
}

/// **One queue's line: the states somebody can act on, then the day's successes** — the second
/// number is the one that says the queue is still moving.
fn counted(states: impl Iterator<Item = &'static str>, today: usize, word: &str) -> String {
    let mut said = states_of(states);
    if today > 0 {
        said.push(format!("{today} {word} today"));
    }
    if said.is_empty() {
        return "0 open".into();
    }
    said.join(" · ")
}

/// **The target health a repository is judged by** — the newest, for a project that moved its
/// target and still carries both rows.
fn newest<'a>(healths: &'a [TargetHealth], repo: &str) -> Option<&'a TargetHealth> {
    healths
        .iter()
        .filter(|h| h.repository == repo)
        .max_by_key(|h| h.checked_ms)
}

/// **What to call each repository in one line** — its directory name, or the whole path for the
/// ones that share a name (two checkouts called `website`).
fn short_names(repos: &[String]) -> Vec<String> {
    let tails: Vec<String> = repos
        .iter()
        .map(|r| {
            Path::new(r)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| r.clone())
        })
        .collect();
    repos
        .iter()
        .zip(&tails)
        .map(|(r, t)| {
            if tails.iter().filter(|x| *x == t).count() > 1 {
                r.clone()
            } else {
                t.clone()
            }
        })
        .collect()
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(12).collect()
}

/// A row's own words, cut to `cap` characters when a bounded answer asked for it.
fn shorten(s: &str, cap: Option<usize>) -> String {
    match cap {
        Some(n) if s.chars().count() > n => {
            let mut kept: String = s.chars().take(n).collect();
            kept.push('…');
            kept
        }
        _ => s.to_string(),
    }
}

/// **Local midnight, in Unix ms** — what "today" means to the person reading it. The
/// `localtime_r` shape is the tree's own (`harness::seated_at`): a harness is opened from more
/// than one thread and `localtime` hands back a shared static.
fn midnight_ms(now: u64) -> u64 {
    let secs = (now / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes into `tm` and reads `secs`; both are owned here.
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return now.saturating_sub(86_400_000);
    }
    let into_the_day = tm.tm_hour as i64 * 3600 + tm.tm_min as i64 * 60 + tm.tm_sec as i64;
    ((secs as i64 - into_the_day).max(0) as u64) * 1000
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mergequeue::serve::tests::{TempDir, commit, repo, sh};
    use letibot_tokencore::store::{
        MergeGateOutcome, MergeGateStep, MergePriority, NewReview, ReviewKind, ReviewState,
        TargetHealth,
    };
    use std::sync::Mutex;

    /// **The model's way out, end to end against a real repository and a temp store** (R76 item
    /// 2): it reads the red target with the failing step's tail, re-gates it, withdraws a stale
    /// row under its own name, and PROPOSES the target — which writes no file and puts the exact
    /// line to the operator. FAILS before R76: there was no such door at all.
    #[test]
    fn the_model_reads_regates_withdraws_and_only_proposes_the_target() {
        let dir = TempDir::new("desk");
        let repo = repo(&dir.0);
        sh(&repo, &["branch", "feat/memory-engine"]);
        let tip = commit(&repo, "b.txt", "b\n", "tip");
        let key = repo.display().to_string();
        let db = dir.0.join("sessions.db");
        let store = Store::open(&db).unwrap();
        store
            .set_target_health(
                &TargetHealth {
                    repository: key.clone(),
                    target: "main".into(),
                    sha: tip.clone(),
                    state: HealthState::Red,
                    checked_ms: 1,
                    evidence: "`scripts/merge_gate.sh` failed".into(),
                    gate_steps: vec![MergeGateStep {
                        command: "scripts/merge_gate.sh".into(),
                        outcome: MergeGateOutcome::Failed,
                        output: "cargo build ok\nFAIL: no binary at target/release/letidb".into(),
                        started_ms: 0,
                        elapsed_ms: 0,
                    }],
                },
                "test",
            )
            .unwrap();
        store
            .enqueue_review(
                &NewReview {
                    id: "red-main-ec3cb01e8a6f".into(),
                    repository: key.clone(),
                    target: "main".into(),
                    kind: ReviewKind::Target,
                    session_id: String::new(),
                    branch: "main".into(),
                    worktree: None,
                    base_sha: String::new(),
                    head_sha: "ec3cb01e8a6f".into(),
                    brief: "red".into(),
                    priority: MergePriority::Urgent,
                    fixes: None,
                    evidence: "red".into(),
                },
                "test",
                1,
            )
            .unwrap();
        let desk = SessionQueue {
            store_path: db.clone(),
            session: "s-project".into(),
            workspace: dir.0.clone(),
            registry: letibot_sessionlog::registry::Registry::new(),
        };

        let said = desk.status(false, None).unwrap();
        assert!(
            said.contains("target `main`") && said.contains("RED"),
            "{said}"
        );
        assert!(
            said.contains("`scripts/merge_gate.sh`") && said.contains("| FAIL: no binary"),
            "the failing step and its tail: {said}"
        );
        assert!(said.contains("red-main-ec3cb01e8a6f"), "{said}");

        let said = desk.regate(None, None).unwrap();
        assert!(said.contains("`main`"), "{said}");
        let h = store.target_health(&key, "main").unwrap().unwrap();
        assert_eq!(h.state, HealthState::Gating);
        assert!(h.evidence.contains("agent:s-project"), "{}", h.evidence);

        desk.withdraw("red-main-ec3", "its commit was amended away")
            .unwrap();
        let r = store.review_row("red-main-ec3cb01e8a6f").unwrap().unwrap();
        assert_eq!(r.state, ReviewState::Withdrawn);
        let moves = store.transitions("review", &r.id).unwrap();
        let last = moves.last().unwrap();
        assert_eq!(last.by, "agent:s-project");
        assert!(last.why.contains("amended away"), "{}", last.why);

        // **Proposed, not set**: the operator is shown the exact line, and no file is written.
        let raised = Mutex::new(Vec::new());
        let said = propose_target(
            &store,
            &dir.0,
            "feat/memory-engine",
            "main builds pgnoop; the gate names letidb",
            "agent:s-project",
            &|e| raised.lock().unwrap().push(e),
        )
        .unwrap();
        assert!(said.contains("nothing was written"), "{said}");
        assert!(!repo.join("leticode.toml").exists(), "no config written");
        let raised = raised.lock().unwrap();
        let detail = raised
            .iter()
            .find_map(|e| match e {
                SessionEvent::Warning { code, detail, .. } if code == "queue_target_proposed" => {
                    Some(detail.clone())
                }
                _ => None,
            })
            .expect("put to the operator on the hubs");
        assert!(
            detail.contains("[queue]\ntarget = \"feat/memory-engine\"")
                && detail.contains("leticode.toml"),
            "{detail}"
        );
        assert!(
            propose_target(&store, &dir.0, "no-such-branch", "x", "a", &|_| {}).is_err(),
            "a branch that does not exist is not proposed"
        );
    }

    // ===== `status` is scoped, and says so (2026-10-30) =====

    /// **A repository named `name` under `under`**, one commit, keyed the way the queues key it —
    /// and with an `origin` on `github.com/<owner>/…`, so a folder holding two of them is ONE
    /// project ([`git::repos_of_workspace`] keeps children that share an owner).
    fn a_repo(under: &Path, name: &str, owner: Option<&str>) -> String {
        let p = under.join(name);
        std::fs::create_dir_all(&p).unwrap();
        sh(&p, &["init", "-q", "-b", "main"]);
        sh(&p, &["config", "user.email", "q@test"]);
        sh(&p, &["config", "user.name", "queue test"]);
        if let Some(owner) = owner {
            sh(
                &p,
                &[
                    "remote",
                    "add",
                    "origin",
                    &format!("git@github.com:{owner}/{name}.git"),
                ],
            );
        }
        std::fs::write(p.join("a.txt"), "one\n").unwrap();
        sh(&p, &["add", "."]);
        sh(&p, &["commit", "-q", "-m", "base"]);
        std::fs::canonicalize(&p).unwrap().display().to_string()
    }

    /// A `RED` target for `repo`, with a failing step whose output is `tail`.
    fn red_target(store: &Store, repo: &str, tail: &str) {
        store
            .set_target_health(
                &TargetHealth {
                    repository: repo.into(),
                    target: "main".into(),
                    sha: "ec3cb01e8a6f4d2c9b1a".into(),
                    state: HealthState::Red,
                    checked_ms: 1,
                    evidence: "the gate is red".into(),
                    gate_steps: vec![MergeGateStep {
                        command: "scripts/merge_gate.sh".into(),
                        outcome: MergeGateOutcome::Failed,
                        output: tail.into(),
                        started_ms: 0,
                        elapsed_ms: 0,
                    }],
                },
                "test",
            )
            .unwrap();
    }

    /// One review row waiting in `repo`, with `evidence` as its words.
    fn a_review(store: &Store, repo: &str, id: &str, evidence: &str, now: u64) {
        store
            .enqueue_review(
                &NewReview {
                    id: id.into(),
                    repository: repo.into(),
                    target: "main".into(),
                    kind: ReviewKind::Branch,
                    session_id: String::new(),
                    branch: format!("feat/{id}"),
                    worktree: None,
                    base_sha: String::new(),
                    head_sha: "ec3cb01e8a6f4d2c9b1a".into(),
                    brief: "brief".into(),
                    priority: MergePriority::Subagent,
                    fixes: None,
                    evidence: evidence.into(),
                },
                "test",
                now,
            )
            .unwrap();
    }

    fn a_desk(db: &Path, workspace: &Path) -> SessionQueue {
        SessionQueue {
            store_path: db.to_path_buf(),
            session: "s-project".into(),
            workspace: workspace.to_path_buf(),
            registry: letibot_sessionlog::registry::Registry::new(),
        }
    }

    /// **The default scope is the session's own repository** — the operator's report was nineteen
    /// sections of other people's projects for one question about one of them.
    #[test]
    fn status_with_no_scope_is_one_repository_and_says_which() {
        let dir = TempDir::new("desk-scope");
        let mine = a_repo(&dir.0, "mine", None);
        let other = a_repo(&dir.0, "other", None);
        let db = dir.0.join("sessions.db");
        let store = Store::open(&db).unwrap();
        red_target(&store, &mine, "cargo build ok\nFAIL: no binary\n");
        red_target(&store, &other, "the other project is red\n");
        a_review(&store, &mine, "r-mine", "waiting its turn", 10);
        a_review(&store, &other, "r-other", "waiting its turn", 11);
        let desk = a_desk(&db, &dir.0.join("mine"));

        let said = desk.status(false, None).unwrap();
        assert_eq!(
            said.lines().next().unwrap(),
            format!("the queue for {mine}").as_str(),
            "the scope line: {said}"
        );
        assert!(said.contains("r-mine"), "{said}");
        assert!(!said.contains("r-other"), "another project's row: {said}");
        assert!(!said.contains(&other), "another project at all: {said}");
        // The shape is the one the operator already reads, plus the counts.
        assert!(
            said.contains("target `main` in") && said.contains("RED"),
            "{said}"
        );
        assert!(said.contains("review queue: 1 waiting"), "{said}");
        assert!(said.contains("merge queue: 0 open"), "{said}");
    }

    /// **`all` is every repository, one line each** — the whole box in a screenful, and it says
    /// how many and how many are red.
    #[test]
    fn all_is_one_line_per_repository_in_the_snapshot() {
        let dir = TempDir::new("desk-all");
        let mine = a_repo(&dir.0, "mine", None);
        let other = a_repo(&dir.0, "other", None);
        let db = dir.0.join("sessions.db");
        let store = Store::open(&db).unwrap();
        red_target(&store, &mine, "cargo build ok\nFAIL: no binary\n");
        a_review(&store, &mine, "r-mine", "waiting its turn", 10);
        a_review(&store, &other, "r-other", "waiting its turn", 11);
        let desk = a_desk(&db, &dir.0.join("mine"));

        let said = desk.status(true, None).unwrap();
        let lines: Vec<&str> = said.lines().collect();
        assert_eq!(lines[0], "every repository (2, 1 red)", "{said}");
        assert_eq!(lines.len(), 3, "a header and one line each: {said}");
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("mine  RED · 1 waiting · unserved")),
            "{said}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("other  no target · 1 waiting · unserved")),
            "a repository whose target was never gated says so: {said}"
        );
        assert!(
            !said.contains("r-mine"),
            "detail is the other forms': {said}"
        );
    }

    /// **`repository: "<name>"` prints that one in full** — by its tail, by its path, and with the
    /// whole tail of a failing step rather than three lines of it.
    #[test]
    fn a_named_repository_is_printed_in_full_by_its_tail_or_its_path() {
        let dir = TempDir::new("desk-named");
        let mine = a_repo(&dir.0, "mine", None);
        let other = a_repo(&dir.0, "other", None);
        let db = dir.0.join("sessions.db");
        let store = Store::open(&db).unwrap();
        red_target(&store, &mine, "mine is fine\n");
        red_target(
            &store,
            &other,
            "one\ntwo\nthree\nfour\nfive\nFAIL: no binary\n",
        );
        a_review(&store, &mine, "r-mine", "waiting its turn", 10);
        a_review(&store, &other, "r-other", "waiting its turn", 11);
        let desk = a_desk(&db, &dir.0.join("mine"));

        // By its directory name — which is not a path under this session's workspace.
        let said = desk.status(false, Some("other")).unwrap();
        assert_eq!(
            said.lines().next().unwrap(),
            format!("the queue for {other}").as_str(),
            "{said}"
        );
        assert!(said.contains("r-other"), "{said}");
        assert!(!said.contains("r-mine"), "{said}");
        assert!(
            said.contains("one\n") && said.contains("| FAIL: no binary"),
            "the whole tail, not three lines of it: {said}"
        );
        assert!(
            said.contains("review queue: 1 row(s) open or parked"),
            "{said}"
        );

        // And by its path, which is the same repository.
        let by_path = desk.status(false, Some(&other)).unwrap();
        assert_eq!(by_path, said);
    }

    /// **A name that matches two repositories is refused in words that name both** — picking one
    /// is how a model comes to read another project's queue and believe it is its own.
    #[test]
    fn an_ambiguous_name_is_refused_and_names_every_match() {
        let dir = TempDir::new("desk-ambiguous");
        let a = a_repo(&dir.0.join("a"), "letim", None);
        let b = a_repo(&dir.0.join("b"), "letim", None);
        let db = dir.0.join("sessions.db");
        let store = Store::open(&db).unwrap();
        a_review(&store, &a, "r-a", "waiting its turn", 10);
        a_review(&store, &b, "r-b", "waiting its turn", 11);
        let desk = a_desk(&db, &dir.0.join("a").join("letim"));

        let said = desk.status(false, Some("letim")).unwrap_err();
        assert!(
            said.starts_with("`letim` matched 2 repositories"),
            "the scope answer, in the refusal: {said}"
        );
        assert!(said.contains(&a) && said.contains(&b), "{said}");
        assert!(said.contains("all: true"), "and the way out: {said}");
        // And the two scopes at once is a refusal too, rather than one of them winning silently.
        let both = desk.status(true, Some("letim")).unwrap_err();
        assert!(both.contains("two different scopes"), "{both}");
    }

    /// **A session seated above several repositories says so in one sentence** and names both ways
    /// to widen — it does not print the box, which is the defect this change exists to fix.
    #[test]
    fn a_session_in_no_one_repository_says_so_and_names_both_escapes() {
        let dir = TempDir::new("desk-nowhere");
        a_repo(&dir.0, "stroppy", Some("stroppy-io"));
        a_repo(&dir.0, "website", Some("stroppy-io"));
        let db = dir.0.join("sessions.db");
        let store = Store::open(&db).unwrap();
        red_target(
            &store,
            &dir.0.join("stroppy").display().to_string(),
            "red\n",
        );
        let desk = a_desk(&db, &dir.0);

        let said = desk.status(false, None).unwrap();
        let first = said.lines().next().unwrap();
        assert!(
            first.starts_with("this session is in no one repository"),
            "the scope line: {said}"
        );
        assert!(
            first.contains("one project of 2 (stroppy, website)"),
            "and which ones it could be: {said}"
        );
        assert!(
            said.contains("all: true") && said.contains("repository:"),
            "both escapes: {said}"
        );
        assert_eq!(said.lines().count(), 2, "a sentence, not a listing: {said}");
    }

    /// **More rows than a screenful is ten and a count** — and what the count drops is the tail of
    /// the queue: a row parked on a person is never the one that goes.
    #[test]
    fn more_rows_than_a_screenful_is_ten_and_a_count() {
        let dir = TempDir::new("desk-bounded");
        let mine = a_repo(&dir.0, "mine", None);
        let db = dir.0.join("sessions.db");
        let store = Store::open(&db).unwrap();
        red_target(
            &store,
            &mine,
            "one\ntwo\nthree\nfour\nfive\nFAIL: no binary\n",
        );
        // The one that is parked on a person is ENQUEUED LAST and would sort last by age.
        for n in 0..11 {
            a_review(
                &store,
                &mine,
                &format!("r-waiting-{n}"),
                "waiting its turn",
                10 + n,
            );
        }
        a_review(&store, &mine, "r-parked", "the reviewer said no", 99);
        store
            .move_review(
                "r-parked",
                ReviewState::Waiting,
                ReviewState::Rejected,
                &letibot_tokencore::store::Move::new("test", "said no"),
                100,
            )
            .unwrap();
        let desk = a_desk(&db, Path::new(&mine));

        let said = desk.status(false, None).unwrap();
        assert!(
            said.contains("review queue: 11 waiting · 1 rejected"),
            "{said}"
        );
        assert!(said.contains("r-parked"), "the parked row is kept: {said}");
        assert!(said.contains("+2 more row(s)"), "{said}");
        assert!(said.contains("r-waiting-8"), "the oldest waiting: {said}");
        assert!(
            !said.contains("r-waiting-9"),
            "the tail of the queue: {said}"
        );
        assert!(
            said.contains("… 3 more line(s); `repository: \".\"` prints them all"),
            "the tail is cut, and it says so: {said}"
        );
        assert!(said.lines().count() <= 25, "a screenful: {said}");

        // **The escape the bounded answer names is the one that works** — `.` is this session's
        // own repository, and it prints every row rather than ten.
        let full = desk.status(false, Some(".")).unwrap();
        assert!(full.contains("r-waiting-10"), "every row: {full}");
        assert!(
            full.contains("review queue: 12 row(s) open or parked"),
            "{full}"
        );
        assert!(!full.contains("+2 more"), "nothing is counted away: {full}");
    }

    // ===== The gate is visible where a person looks first (2026-11-01) =====
    //
    // The operator, twice in one minute: *"nothing about gate — and the same for other sessions —
    // gate is invisible"*, then *"very nasty actually - fans just start spinning"*. The fans were
    // the gate compiling on another box, the queue said `gating`, and nothing a session could read
    // said WHICH STEP or for how long — the merge row's detail said it, and a detail is a click
    // away, in a pane, on a row nobody had a reason to open.

    /// A `RunningGate` as the gate's own read hands one back, with its clock and its log placed
    /// where the test wants them.
    fn a_gate(
        step: usize,
        of: usize,
        ran_ms: u64,
        bytes: u64,
        wrote: Option<u64>,
    ) -> gate::RunningGate {
        gate::RunningGate {
            command: "cargo test --offline --workspace --no-fail-fast".into(),
            step,
            of,
            started_ms: 1_000_000,
            tail: String::new(),
            log_bytes: bytes,
            wrote_ms: wrote.map(|ago| 1_000_000 + ran_ms - ago),
        }
    }

    /// **The gate's clause is the three facts a merge row's detail carries, in one line** — which
    /// step of how many, how long, and whether the output is moving. The exact words, so the two
    /// readers of this line (a model's `status` and a person's eye) cannot drift apart.
    #[test]
    fn a_running_gates_clause_names_the_step_the_clock_and_the_moving_tail() {
        let now = 1_000_000 + 372_000; // 6m12s in
        let g = a_gate(3, 4, 372_000, 36 * 1024, Some(2_000));
        assert_eq!(
            gate_words(&g, now),
            "step 3 of 4 · 6m12s · tail moving (36 KiB)"
        );
        // **A tail that has stood still is not called moving**, and the two bands are the pane's
        // own: minutes of silence is a build between crates, ten of them is a step going nowhere.
        let quiet = a_gate(3, 4, 372_000, 36 * 1024, Some(300_000));
        assert_eq!(
            tail_words(&quiet, now),
            "tail moved 5m00s ago (36 KiB)",
            "five minutes of silence is said, not hidden"
        );
        let stuck = a_gate(3, 4, 900_000, 36 * 1024, Some(700_000));
        assert_eq!(
            tail_words(&stuck, 1_000_000 + 900_000),
            "tail still for 11m40s — it may be stuck (36 KiB)",
            "past ten minutes the line stops short of a verdict it cannot support"
        );
        // **A log nothing can read is not a stall** — the bytes are said and the verdict withheld.
        let unreadable = a_gate(1, 2, 4_000, 12, None);
        assert_eq!(tail_words(&unreadable, 1_004_000), "12 B written");
    }

    /// **The target's line says the gate while it runs, and nothing about it when the target is
    /// green** — the operator's *"gate is invisible"*, in the one place every session reads: the
    /// default `status`, which is this session's own repository and a screenful of it.
    #[test]
    fn the_default_status_names_the_gates_step_and_not_when_the_target_is_green() {
        let dir = TempDir::new("desk-gate");
        let mine = a_repo(&dir.0, "mine", None);
        let db = dir.0.join("sessions.db");
        let store = Store::open(&db).unwrap();
        let wt = dir.0.join("landing");
        std::fs::create_dir_all(&wt).unwrap();
        let log = dir.0.join("step.log");
        std::fs::write(&log, "x".repeat(36 * 1024)).unwrap();
        gate::watch(&wt, &mine);
        gate::watch_step(
            &wt,
            "cargo test --offline --workspace --no-fail-fast",
            3,
            4,
            &log,
        );
        let mut health = TargetHealth {
            repository: mine.clone(),
            target: "main".into(),
            sha: "ec3cb01e8a6f4d2c9b1a".into(),
            state: HealthState::Gating,
            checked_ms: 1,
            evidence: "gating `main`'s tip".into(),
            gate_steps: vec![],
        };
        store.set_target_health(&health, "test").unwrap();
        let desk = a_desk(&db, Path::new(&mine));

        let said = desk.status(false, None).unwrap();
        let line = said
            .lines()
            .find(|l| l.starts_with("target `main`"))
            .unwrap_or_else(|| panic!("the target's line: {said}"));
        assert!(
            line.starts_with(&format!(
                "target `main` in {mine}: GATING at ec3cb01e8a6f · step 3 of 4 · "
            )),
            "the state, the tip and the step: {line}"
        );
        assert!(
            line.ends_with(" · tail moving (36 KiB)"),
            "and whether the output is moving: {line}"
        );
        // **A headline, not the story** — the clause is bounded by its own words, so a long path
        // is the only thing that can make this line long.
        let clause = line.split(": ").nth(1).unwrap_or_default();
        assert!(
            clause.chars().count() < 100,
            "the gate's clause is a headline: {clause:?}"
        );
        // **And `repository:` prints it too** — the third scope is the same target line, in full.
        let full = desk.status(false, Some(".")).unwrap();
        assert!(
            full.contains("GATING at ec3cb01e8a6f · step 3 of 4 · ")
                && full.contains("tail moving (36 KiB)"),
            "the named scope carries the gate as well: {full}"
        );

        // **The same gate, judged green, says nothing about it** — the clause belongs to the
        // state, not to a step entry left lying in the gate's own registry.
        health.state = HealthState::Green;
        health.evidence = "gating `main`'s tip".into();
        store.set_target_health(&health, "test").unwrap();
        let said = desk.status(false, None).unwrap();
        let line = said
            .lines()
            .find(|l| l.starts_with("target `main`"))
            .unwrap_or_else(|| panic!("the target's line: {said}"));
        assert_eq!(
            line,
            format!("target `main` in {mine}: GREEN at ec3cb01e8a6f"),
            "a green target's line is unchanged: {said}"
        );
        assert!(!said.contains("step 3 of 4"), "{said}");
        drop(gate::step_guard(&wt));
    }

    /// **`all` carries it too, per repository** — the session that needs this most is the one in
    /// ANOTHER repository: it has no row for this gate, no reason to open one, and the fans it
    /// hears are this.
    #[test]
    fn all_says_which_step_another_repositorys_gate_is_on() {
        let dir = TempDir::new("desk-gate-all");
        let mine = a_repo(&dir.0, "mine", None);
        let other = a_repo(&dir.0, "other", None);
        let db = dir.0.join("sessions.db");
        let store = Store::open(&db).unwrap();
        let wt = dir.0.join("landing");
        std::fs::create_dir_all(&wt).unwrap();
        let log = dir.0.join("step.log");
        std::fs::write(&log, "building\n").unwrap();
        gate::watch(&wt, &mine);
        gate::watch_step(&wt, "cargo test --offline --workspace", 3, 4, &log);
        store
            .set_target_health(
                &TargetHealth {
                    repository: mine.clone(),
                    target: "main".into(),
                    sha: "ec3cb01e8a6f4d2c9b1a".into(),
                    state: HealthState::Gating,
                    checked_ms: 1,
                    evidence: "gating `main`'s tip".into(),
                    gate_steps: vec![],
                },
                "test",
            )
            .unwrap();
        a_review(&store, &mine, "r-mine", "waiting its turn", 10);
        a_review(&store, &other, "r-other", "waiting its turn", 11);
        // The session is in `other`: this is the reader the line exists for.
        let desk = a_desk(&db, Path::new(&other));

        let said = desk.status(true, None).unwrap();
        let line = said
            .lines()
            .find(|l| l.starts_with("mine  "))
            .unwrap_or_else(|| panic!("the other repository's line: {said}"));
        assert!(
            line.starts_with("mine  GATING · step 3 of 4 · ") && line.contains(" · 1 waiting"),
            "the state, the step, its clock, then what is open: {line}"
        );
        assert!(
            !line.contains("tail"),
            "a screenful for the box keeps the headline, not the tail: {line}"
        );
        // And a repository whose target was never gated says exactly what it said before.
        let quiet = said
            .lines()
            .find(|l| l.starts_with("other  "))
            .unwrap_or_else(|| panic!("{said}"));
        assert_eq!(quiet, "other  no target · 1 waiting · unserved", "{said}");
        drop(gate::step_guard(&wt));
    }
}
