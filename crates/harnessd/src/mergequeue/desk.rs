//! The `queue` tool's daemon half - what a session's model can do about its project's
//! queues: read them, re-gate the target, restart or withdraw a row, and PROPOSE the branch
//! the project lands on.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{
    HealthState, LiveGate, MergeRow, MergeState, QueueLease, ReviewRow, ReviewState, Store,
    TargetHealth,
};
use letibot_tools::builtins::queue::QueueDesk;
use letibot_tools::exec::diskio::Disks as _;
use letibot_transcript::text::clip_chars;

use super::gate::{self, now_ms};
use super::{git, notices, settings, verbs};

/// The repository a session's queue verbs are about, and its target - the one `workspace` means
/// (`repo_of_workspace`), keyed the way the queues key it, unless `named` is a path; and the project's
/// `[queue] target` (else `main`/`master`) when no target is named.
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
                // A project of several repositories names them, so the one to name is a choice among
                // the right ones rather than a path to go and find.
                let many = git::repos_of_workspace(workspace);
                if many.len() > 1 {
                    let names: Vec<String> =
                        many.iter().map(|r| format!("`{}`", r.display())).collect();
                    return format!(
                        "the workspace {} is one project of {} repositories — {} — so which one is \
                         not something this can know; name it with the tool's `repository` (or \
                         `/queue regate /path/to/repo [TARGET]`)",
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

/// One session's desk - its store, its workspace, and the hubs a move is announced on.
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
            // Naming a repository is asking for it in full - that is the door the bounded default
            // leaves open, and the shape `/clear` has always printed.
            Scope::One { repo, named: true } => detail(&store, &repo),
            Scope::One { repo, named: false } => bounded(&store, &repo),
            Scope::Nowhere => Ok(nowhere(&self.workspace)),
        }
    }

    fn regate(&self, repository: Option<&str>, target: Option<&str>) -> Result<String, String> {
        let (repo, target) = resolve(&self.workspace, repository, target)?;
        verbs::regate(&self.store()?, &repo, &target, &self.who(), &self.events())
    }

    fn enqueue(
        &self,
        branch: &str,
        target: Option<&str>,
        why: &str,
        repository: Option<&str>,
    ) -> Result<String, String> {
        let (repo, _) = resolve(&self.workspace, repository, Some("-"))?;
        let row = super::enqueue::enqueue_branch(
            &self.store()?,
            Path::new(&repo),
            branch,
            target,
            why,
            &self.session,
            now_ms(),
        )?;
        self.registry.broadcast(SessionEvent::QueueMoved {
            queue: "review".into(),
            id: row.id.clone(),
            to: row.state.as_str().into(),
            why: row.evidence.clone(),
        });
        Ok(format!(
            "`{}` is in the review queue as `{}` onto `{}`: a reviewer judges it against the \
             work you named, the merge queue then rebases it, gates it and lands it. \
             `queue` with `action: \"status\"` shows where it is and why.",
            row.branch, row.id, row.target
        ))
    }

    fn restart(&self, id: &str) -> Result<String, String> {
        verbs::restart_by(&self.store()?, id, &self.who(), &self.events())
    }

    fn withdraw(&self, id: &str, why: &str) -> Result<String, String> {
        let words = format!("withdrawn by {}: {why}", self.who());
        verbs::withdraw_by(&self.store()?, id, &self.who(), &words, &self.events())
    }

    fn stop(&self, repository: Option<&str>, why: &str) -> Result<String, String> {
        let (repo, _) = resolve(&self.workspace, repository, Some("-"))?;
        verbs::stop_by(&self.store()?, &repo, &self.who(), why, &self.events())
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

/// Put `[queue] target` to the operator - raised on every hub as `queue_target_proposed` with the
/// exact lines and the file, recorded in the target's history; nothing is written.
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
    events(SessionEvent::warning(
        "queue_target_proposed",
        detail.clone(),
    ));
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

// ===== `status`: three shapes, one scope line ===== The cost is the model's attention, so
// the default answer is ONE repository and a screenful of it `all` is one line per
// repository, and `repository: "<name>"` is the full shape.

/// How many rows a bounded answer prints in full before it counts the rest.
const SHOWN_ROWS: usize = 10;

/// How many entries a bounded answer explains; `repository: "."` explains all of them.
const SHOWN_WHY: usize = 5;

/// The lines of a failing step's tail a bounded answer keeps - the full shape keeps all 30.
const SHOWN_TAIL: usize = 3;

/// The characters a row's own words may run to in a bounded answer.
const SHOWN_EVIDENCE: usize = 200;

/// How recently a step's log must have grown for the line to say `tail moving` - a build
/// prints between crates and a test run prints between binaries, so a minute of silence is
/// not a stall
const TAIL_MOVING_MS: u64 = 60_000;

/// How long a step's log may stand still before the line says it may be stuck - the same ten minutes
/// the head's queue pane waits (`GATE_QUIET_MS`), because it is the same question about the same file
/// and two answers to it would be one too many.
const TAIL_STILL_MS: u64 = 10 * 60 * 1000;

/// The states rows are counted in, in the queue's own order - the flow, not the alphabet: what waits
/// what is running, what is parked on a person.
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

/// A review row somebody still has to deal with - the queue's business (open) or a person's (a verdict
/// that needs a decision). `pub(super)` for `mergequeue::stuck`, which counts the half of it that is
/// nobody's: one rule, so the census's numbers are a sub-line of `status`'s.
pub(super) fn parked(r: ReviewState) -> bool {
    r.is_open() || matches!(r, ReviewState::Rejected | ReviewState::CannotDecide)
}

/// A landing row somebody still has to deal with, the same rule one queue over.
pub(super) fn parked_merge(m: MergeState) -> bool {
    m.is_open()
        || matches!(
            m,
            MergeState::Failed
                | MergeState::TimedOut
                | MergeState::Conflict
                | MergeState::LandedLocal
        )
}

/// What one `status` call is about - decided once, so the line the answer opens with and the rows under
/// it cannot disagree.
enum Scope {
    /// `all: true` - every repository, one line each.
    Every,
    /// One repository. `named` is whether the CALLER named it, which is what buys the full shape.
    One { repo: String, named: bool },
    /// The session is in no one repository, and named none.
    Nowhere,
}

/// Which repository a `status` call is about.
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

/// The repository this session is in - the workspace itself when git says it is in one, the
/// same-named child (`~/Projects/letibot` → `letibot`), or its only repository child
fn current(workspace: &Path) -> Option<String> {
    git::repo_of_workspace(workspace)
        .and_then(|r| git::repo_of_worktree(&r))
        .map(|p| p.display().to_string())
}

/// The one repository `asked` names - a path git resolves, else a name matched against the
/// repositories the queue holds: a full path, a path's tail or a directory name. Two
/// matches is a refusal that names both.
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

/// `key` is `asked`, or ends with it at a path boundary - so `/a/letim`, `Projects/letim` and `letim`
/// all name `/a/letim`, and `im` names nothing.
fn names(key: &str, asked: &str) -> bool {
    key == asked
        || key
            .strip_suffix(asked)
            .is_some_and(|head| head.ends_with('/'))
}

/// A count said in words. The number is known here, so `row(s)` is a placeholder no reader of a
/// `queue` answer should be shown.
fn how_many(n: usize, one: &str, many: &str) -> String {
    match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    }
}

/// Every repository the queue holds - the healths, the rows and the leases, deduped and sorted: what
/// `all` counts and what a name is matched against.
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

/// `all` - one line per repository, so the whole box fits in a screenful: which target is
/// in what state, what is open, and whether anybody serves it. Detail is what the other two
/// forms are for.
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
            Some(h) => plain_state(h.state.as_str()),
            None => "no target".into(),
        };
        if let Ok(Some(g)) = store.live_gate(repo) {
            state.push_str(&format!(
                " · step {} of {} · {}",
                g.step.max(1),
                g.of_step.max(1),
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

/// The default answer: this repository, and no more than a screenful of it - the target, what each
/// queue holds by state, the ten rows somebody has to act on first, and who serves it.
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
    let gate = running_gate(store, repo);
    let gate_seen = gate.as_ref().map(|(g, machine)| (g, machine.as_str()));
    if healths.is_empty() {
        out.push_str("no target is gated here — the queue has not looked at this repository\n");
    }
    out.push_str(&targets(&healths, Some(SHOWN_TAIL), gate_seen));
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
    let (lines, more) = first_rows(&reviews, &merges, gate_seen);
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    if more > 0 {
        out.push_str(&format!(
            "+{} — `queue` with `action: \"status\"` and `repository: \".\"` prints \
             every row of this repository\n",
            how_many(more, "more row", "more rows")
        ));
    }
    out.push_str(&served_by(&leases));
    out.push_str(&super::explain::read(store, repo, Some(SHOWN_WHY))?);
    Ok(out)
}

/// `repository: "<name>"` - one repository in full: the target with the whole tail of its failing
/// step, every open or parked row, and who serves it.
fn detail(store: &Store, repo: &str) -> Result<String, String> {
    Ok(format!(
        "the queue for {repo}\n{}{}",
        status(store, Some(repo))?,
        super::explain::read(store, repo, None)?
    ))
}

/// A session that is in no one repository - one sentence and the two ways to widen. Printing the whole
/// box is the thing this exists to stop.
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

/// Both queues and the targets of one repository, as a model reads them, in full - what `/clear` has
/// always printed and what `repository: "<name>"` still prints.
pub fn status(store: &Store, repository: Option<&str>) -> Result<String, String> {
    let here = |r: &str| repository.is_none_or(|k| k == r);
    let healths: Vec<TargetHealth> = store
        .target_healths()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|h| here(&h.repository))
        .collect();
    // What is gated, and what the machine running it says about itself - read once, so every line that
    // mentions the gate mentions the same gate.
    let gate = repository.and_then(|repo| running_gate(store, repo));
    let gate_seen = gate.as_ref().map(|(g, machine)| (g, machine.as_str()));
    let mut out = targets(&healths, None, gate_seen);
    let reviews = store.review_rows(None).map_err(|e| e.to_string())?;
    let open_reviews: Vec<&ReviewRow> = reviews
        .iter()
        .filter(|r| here(&r.repository) && parked(r.state))
        .collect();
    out.push_str(&format!(
        "review queue: {} open or parked\n",
        how_many(open_reviews.len(), "row", "rows")
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
        "merge queue: {} open or parked\n",
        how_many(open_merges.len(), "row", "rows")
    ));
    for m in open_merges {
        out.push_str(&merge_line(m, None, gate_seen));
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

/// A repository's target, and the gate that failed on it - the whole tail in the full shape
/// (`None`) the last [`SHOWN_TAIL`] lines of it in a bounded one, with the count of what
/// was cut. A target whose gate is running says what it is doing, on its own line
fn targets(
    healths: &[TargetHealth],
    tail: Option<usize>,
    gate: Option<(&LiveGate, &str)>,
) -> String {
    let now = now_ms();
    let mut out = String::new();
    for h in healths {
        // The tip's OWN gate, not a landing's: one repository runs one gate at a time, and the row's
        // line is where a landing's is said. The health row is what says a gate is on this tip at all -
        // a published row left by a worker that died is not a gate.
        let running = gate
            .filter(|(g, _)| {
                h.state == HealthState::Gating && g.kind == "target" && g.target == h.target
            })
            .map(|(g, machine)| format!(" · {}", gate_said(g, now, machine)))
            .unwrap_or_default();
        out.push_str(&format!(
            "target `{}` in {}: {} at {}{}\n  {}\n",
            h.target,
            h.repository,
            plain_state(h.state.as_str()),
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
                "    | … {}; `repository: \".\"` prints them all\n",
                how_many(lines.len() - keep, "more line", "more lines")
            ));
        }
    }
    out
}

/// The gate this repository is running - the published row and what the machine running it says about
/// itself. `None` when nothing is gated here, which is the honest answer for a repository at rest.
fn running_gate(store: &Store, repo: &str) -> Option<(LiveGate, String)> {
    let gate = store.live_gate(repo).ok().flatten()?;
    let machine = machine_facts(&gate);
    Some((gate, machine))
}

/// The machine a gate runs on, and - when this daemon is the one running it - whether its output is
/// still moving, which no published row can carry.
fn machine_facts(g: &LiveGate) -> String {
    let mut said = machine_words(g);
    if let Some(step) = gate::running(&g.repository) {
        said.push_str(&format!(" · {}", tail_words(&step, now_ms())));
    }
    said
}

/// The load on the machine a gate runs on and the room left where its build goes. A local gate is read
/// from this box's own files; a host's is one bounded ssh call, and a read that does not answer says so.
fn machine_words(g: &LiveGate) -> String {
    let host = g.host.strip_suffix(" (firecode)").unwrap_or(&g.host);
    if host == "local" {
        let load = letibot_tools::firecode::boot::LoadSample::read()
            .map(|s| s.said())
            .unwrap_or_else(|| "load not read".to_string());
        let room = match letibot_tools::exec::diskio::HostDisks.space(Path::new(&g.dir)) {
            Some(s) => format!("{} free at its build dir", bytes_human(s.free)),
            None => "room left not read (its build dir is not there yet)".to_string(),
        };
        return format!("{load} · {room}");
    }
    match remote_facts(host, &g.dir) {
        Ok(said) => said,
        Err(why) => format!("load not read ({why})"),
    }
}

/// How long a read of a gate host's own facts may take: one round trip over the connection the daemon
/// already has open for it, and a host that does not answer in that is not worth a `status` call.
const FACTS_BOUND: Duration = Duration::from_secs(10);

/// A declared host's load and the room left at the gate's build dir, in ONE call that only reads. It
/// reuses the daemon's `ControlMaster` for that host, so it is a round trip and not a handshake.
fn remote_facts(host: &str, dir: &str) -> Result<String, String> {
    let hosts = letibot_tools::remote::Hosts::load();
    let Some(h) = hosts.get(host).cloned() else {
        return Err(format!("`{host}` is not a declared host any more"));
    };
    let ssh =
        letibot_tools::remote::Ssh::connect(h, None, letibot_tools::remote::Paths::from_env())?;
    let dir = dir.replace('\'', "'\\''");
    let script = format!(
        "cat /proc/loadavg 2>/dev/null | cut -d' ' -f1,2,3; nproc 2>/dev/null; \
         df -Pk '{dir}' 2>/dev/null | tail -1 | awk '{{print $4}}'"
    );
    let argv = ssh.argv(&ssh.remote_command(&script));
    let out = match letibot_tools::rpc::run_bounded(ssh.program(), &argv, FACTS_BOUND) {
        letibot_tools::rpc::Ended::Answered(out) => out,
        letibot_tools::rpc::Ended::TimedOut => {
            return Err("it did not answer in 10 s".to_string());
        }
        letibot_tools::rpc::Ended::Unstartable(why) => return Err(why),
    };
    let said = String::from_utf8_lossy(&out.stdout);
    let field = |n: usize| said.lines().nth(n).map(str::trim).unwrap_or_default();
    let load: String = field(0)
        .split_whitespace()
        .next()
        .map(str::to_string)
        .unwrap_or_default();
    let cores = field(1);
    let free_kb = field(2).parse::<u64>().ok();
    if load.is_empty() {
        return Err(format!("`{host}` said nothing about its load"));
    }
    let mut words = format!("load {load} on {cores} cores");
    if let Some(kb) = free_kb {
        words.push_str(&format!(
            " · {} free at its build dir",
            bytes_human(kb.saturating_mul(1024))
        ));
    }
    Ok(words)
}

/// Is the step's output moving? - answered from ONE read, because the log's own mtime is when its last
/// byte landed. The head's pane needs two snapshots to answer this; a `status` answer is one shot, and
/// the file already carries the second half of the comparison.
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

/// A running gate in one clause: the step it is on, how long, and where - with the machine's own facts
/// (`machine`, empty when nobody could read them) when the caller had them.
pub(super) fn gate_said(g: &LiveGate, now: u64, machine: &str) -> String {
    let step = match (g.step, g.of_step) {
        (n, of) if n > 0 && of > 0 => format!("step {n} of {of} `{}`", g.command),
        _ => "not on a step yet".to_string(),
    };
    let mut said = vec![
        step,
        format!("{} so far", dur_human(now.saturating_sub(g.started_ms))),
        format!("on {}", host_words(&g.host)),
    ];
    if !machine.is_empty() {
        said.push(machine.to_string());
    }
    said.join(" · ")
}

/// Where a gate runs, as a sentence: `local`, a declared host's name, or either with ` (firecode)`.
fn host_words(host: &str) -> String {
    match host.strip_suffix(" (firecode)") {
        Some(name) => format!("{name}, in a firecode VM"),
        None => host.to_string(),
    }
}

/// A duration as a person reads it - the shape the head's queue pane uses (`6m12s`), so a model reading
/// `status` and a person reading the pane are told the same thing in the same words.
pub(super) fn dur_human(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// Bytes as a person reads them - a step's output is thousands of them, and the exact count is a fact
/// nobody acts on.
fn bytes_human(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{} KiB", n / 1024)
    } else if n < 1024 * 1024 * 1024 {
        format!("{} MiB", n / (1024 * 1024))
    } else {
        format!("{:.1} GiB", n as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// The rows a bounded answer prints, worst first - what is parked on a person, then what is running, then
/// what waits; an operator's urgent entry before a subagent's, oldest first inside each. So what the
/// count drops is the tail of the queue and nothing anybody must act on.
fn first_rows(
    reviews: &[ReviewRow],
    merges: &[MergeRow],
    gate: Option<(&LiveGate, &str)>,
) -> (Vec<String>, usize) {
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
            merge_line(m, Some(SHOWN_EVIDENCE), gate),
        ));
    }
    lines.sort_by_key(|(rank, _)| *rank);
    let more = lines.len().saturating_sub(SHOWN_ROWS);
    lines.truncate(SHOWN_ROWS);
    (lines.into_iter().map(|(_, line)| line).collect(), more)
}

/// A state as a reader of this text understands it. The store's column names are codes
/// (`cannot_decide`, `timed_out`), and this answer is read by a model that repeats it to a person;
/// an unknown state falls back to the store's own word rather than to a guess.
pub(crate) fn plain_state(state: &str) -> String {
    let word = match state {
        // The target's health, which is not a row's state.
        "red" => "the gate is red",
        "green" => "green",
        // The review queue.
        "waiting" => "waiting for a reviewer",
        "reviewing" => "being reviewed",
        "approved" => "approved, waiting to land",
        "rejected" => "refused",
        "cannot_decide" => "the reviewer could not decide",
        // The merge queue.
        "queued" => "waiting for the gate",
        "gating" => "the gate is running",
        "landed" => "landed",
        "landed_local" => "landed locally, not pushed",
        "failed" => "the gate failed",
        "timed_out" => "the gate timed out",
        "conflict" => "it does not merge cleanly",
        "moved" => "replaced by a newer row",
        // Both.
        "blocked" => "stopped - a person has to act",
        "withdrawn" => "withdrawn",
        "archived" => "archived",
        other => return other.replace('_', " "),
    };
    word.to_string()
}

/// Who judged the row, when the store recorded it - `reviewer:s-x` reads as `s-x`, and a
/// `human:` verdict as the person. Empty when nobody has.
fn judged_by(verdict_by: Option<&str>) -> String {
    let Some(by) = verdict_by.filter(|b| !b.is_empty()) else {
        return String::new();
    };
    let who = by
        .strip_prefix("reviewer:")
        .or_else(|| by.strip_prefix("human:"))
        .unwrap_or(by);
    format!(" by {who}")
}

/// One review row as one line - the id, what state it is in and who put it there, the branch and
/// the target, and what the queue said. `cap` is the bounded answer's limit on the row's own words.
fn review_line(r: &ReviewRow, cap: Option<usize>) -> String {
    format!(
        "  `{}` {}{} `{}` → `{}` at {}: {}",
        r.id,
        plain_state(r.state.as_str()),
        judged_by(r.verdict_by.as_deref()),
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
fn merge_line(m: &MergeRow, cap: Option<usize>, gate: Option<(&LiveGate, &str)>) -> String {
    // A landing under the gate says what it is doing: the row alone can only say `gating`.
    let running = gate
        .filter(|(g, _)| m.state == MergeState::Gating && g.entries().contains(&m.id.as_str()))
        .map(|(g, machine)| format!("\n      the gate: {}", gate_said(g, now_ms(), machine)))
        .unwrap_or_default();
    format!(
        "  `{}` {} `{}` → `{}`: {}{running}",
        m.id,
        plain_state(m.state.as_str()),
        m.branch,
        m.target,
        shorten(m.evidence.lines().next().unwrap_or_default(), cap)
    )
}

/// Who serves it, and how long ago it said so - the one line that says a queue is nobody's.
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

/// Rows counted by state, in the queue's own order - only the states that are there, so a line says
/// `3 waiting · 1 timed out` and never `0 of nine things`. Never the store's own code: a summary
/// line is repeated to a person, and `cannot_decide` is not a word one uses.
pub(super) fn states_of(states: impl Iterator<Item = &'static str>) -> Vec<String> {
    counts_of(states)
        .into_iter()
        .map(|(s, n)| format!("{n} {}", count_word(s)))
        .collect()
}

/// A state as a COUNT on a summary line, where a row says the whole sentence: `1 timed out` beside
/// `the gate timed out`, and never the store's `timed_out` or a bare `cannot_decide`.
fn count_word(state: &str) -> String {
    match state {
        "cannot_decide" => "undecided".to_string(),
        "rejected" => "refused".to_string(),
        "conflict" => "in conflict".to_string(),
        other => other.replace('_', " "),
    }
}

/// [`states_of`]'s counts, unformatted - the same `FLOW` order and the same rule, for a reader that
/// wants the numbers rather than the line (`mergequeue::stuck`, whose counts have to be `status`'s counts
/// and not a second opinion about them).
pub(super) fn counts_of(states: impl Iterator<Item = &'static str>) -> Vec<(&'static str, usize)> {
    let mut counts: Vec<(&'static str, usize)> = FLOW.iter().map(|s| (*s, 0)).collect();
    for s in states {
        if let Some(slot) = counts.iter_mut().find(|(name, _)| *name == s) {
            slot.1 += 1;
        }
    }
    counts.into_iter().filter(|(_, n)| *n > 0).collect()
}

/// One queue's line: the states somebody can act on, then the day's successes - the second number is the
/// one that says the queue is still moving.
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

/// The target health a repository is judged by - the newest, for a project that moved its target and
/// still carries both rows.
fn newest<'a>(healths: &'a [TargetHealth], repo: &str) -> Option<&'a TargetHealth> {
    healths
        .iter()
        .filter(|h| h.repository == repo)
        .max_by_key(|h| h.checked_ms)
}

/// What to call each repository in one line - its directory name, or the whole path for the ones that
/// share a name (two checkouts called `website`).
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

pub(super) fn short_sha(sha: &str) -> String {
    sha.chars().take(12).collect()
}

/// A row's own words, cut to `cap` characters when a bounded answer asked for it.
fn shorten(s: &str, cap: Option<usize>) -> String {
    match cap {
        Some(n) => clip_chars(s, n),
        None => s.to_string(),
    }
}

/// Local midnight, in Unix ms - what "today" means to the person reading it. The `localtime_r` shape is
/// the tree's own (`harness::seated_at`): a harness is opened from more than one thread and `localtime`
/// hands back a shared static.
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
        MergeGateOutcome, MergeGateStep, MergePriority, Move, NewReview, ReviewKind, ReviewState,
        TargetHealth,
    };
    use std::sync::Mutex;

    /// The model's way out, end to end against a real repository and a temp store: it reads the red target
    /// with the failing step's tail, re-gates it, withdraws a stale row under its own name, and PROPOSES
    /// the target - which writes no file and puts the exact line to the operator.
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
            said.contains("target `main`") && said.contains("the gate is red"),
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

        // Proposed, not set: the operator is shown the exact line, and no file is written.
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

    // ===== `status` is scoped, and says so =====

    /// A repository named `name` under `under`, one commit, keyed the way the queues key it - and with
    /// an `origin` on `github.com/<owner>/…`, so a folder holding two of them is ONE project
    /// ([`git::repos_of_workspace`] keeps children that share an owner).
    fn a_repo(under: &Path, name: &str, owner: Option<&str>) -> String {
        let p = under.join(name);
        std::fs::create_dir_all(&p).unwrap();
        sh(&p, &["init", "-q", "-b", "main"]);
        letibot_tools::testing::repo_config(&p);
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

    /// A gate as the worker publishes it: run 1 of it, on the step a fixture wants, in `dir`.
    fn a_live_gate(
        repo: &str,
        kind: &str,
        entry: &str,
        dir: &str,
        started_ms: u64,
    ) -> letibot_tokencore::store::LiveGate {
        letibot_tokencore::store::LiveGate {
            repository: repo.into(),
            run_id: format!("ec3cb01e8a6f-{started_ms}"),
            kind: kind.into(),
            target: "main".into(),
            entry: entry.into(),
            sha: "ec3cb01e8a6f4d2c9b1a".into(),
            host: "local".into(),
            dir: dir.into(),
            step: 3,
            of_step: 4,
            command: "cargo test --offline --workspace --no-fail-fast".into(),
            step_ms: started_ms,
            started_ms,
            stop_by: String::new(),
            stop_why: String::new(),
        }
    }

    /// A running gate says WHERE it runs, WHICH step and its command, how long it has been at it, and -
    /// from the string the caller read off that machine - its load and the room left. The machine's own
    /// read is faked here: that is what lets the shape be asserted without a host.
    #[test]
    fn a_running_gate_says_where_and_what_step_and_the_machines_own_facts() {
        let gate = a_live_gate("/repo", "target", "", "/build/gate/repo-1f/target", 1_000);
        let said = gate_said(
            &gate,
            185_000,
            "load 1.40 on 24 cores · 41.2 GiB free at its build dir · tail moving (36 KiB)",
        );
        assert_eq!(
            said,
            "step 3 of 4 `cargo test --offline --workspace --no-fail-fast` · 3m04s so far · on \
             local · load 1.40 on 24 cores · 41.2 GiB free at its build dir · tail moving (36 KiB)",
            "the whole clause, in order"
        );
        // A gate waiting for the box's lock is on no step yet, and says so rather than `step 0 of 0`.
        let mut waiting = gate.clone();
        waiting.step = 0;
        waiting.of_step = 0;
        assert!(gate_said(&waiting, 61_000, "").starts_with("not on a step yet · 1m00s so far"));
        // A firecode gate runs on a VM on that machine: the line says so in a word a reader knows.
        waiting.host = "lab2x1 (firecode)".into();
        assert!(gate_said(&waiting, 61_000, "").contains("on lab2x1, in a firecode VM"));
    }

    /// The machine's own facts: this box's files for a local gate, and a host that cannot be asked says
    /// so rather than leaving the line short.
    #[test]
    fn the_machines_facts_are_read_locally_and_a_read_that_fails_says_so() {
        let dir = TempDir::new("desk-machine");
        let here = a_live_gate(
            "/repo",
            "target",
            "",
            &dir.0.display().to_string(),
            now_ms(),
        );
        let said = machine_words(&here);
        assert!(said.starts_with("load "), "this box's loadavg: {said}");
        assert!(
            said.contains("cores") && said.contains("free at its build dir"),
            "and the room at the build dir: {said}"
        );
        // A host nobody declares (this test's box declares none) is a read that cannot be made.
        let gone = a_live_gate("/repo", "target", "", "/build/gate/x/target", now_ms());
        let gone = letibot_tokencore::store::LiveGate {
            host: "a-host-nobody-declares".into(),
            ..gone
        };
        let said = machine_words(&gone);
        assert!(said.starts_with("load not read"), "{said}");
        assert!(said.contains("not a declared host"), "why: {said}");
    }

    /// A landing under the gate says which step it is on and where - the row alone can only say
    /// `the gate is running`, and that is the state a model most needs to act on.
    #[test]
    fn status_shows_the_gate_a_landing_row_is_under() {
        let dir = TempDir::new("desk-landing-gate");
        let mine = a_repo(&dir.0, "mine", None);
        let db = dir.0.join("sessions.db");
        let store = Store::open(&db).unwrap();
        let (id, merge) = a_gating_row(&store, &mine);
        store
            .gate_began(&a_live_gate(
                &mine,
                "land",
                &id,
                &dir.0.display().to_string(),
                now_ms().saturating_sub(60_000),
            ))
            .unwrap();
        let desk = a_desk(&db, Path::new(&mine));

        let said = desk.status(false, None).unwrap();
        let at = said
            .find(&format!("`{merge}`"))
            .unwrap_or_else(|| panic!("the row's line: {said}"));
        let row_line = &said[at..];
        let row_line = row_line.lines().take(2).collect::<Vec<_>>().join("\n");
        assert!(
            row_line.contains("the gate is running") && row_line.contains("the gate: step 3 of 4"),
            "the row and what its gate is doing: {row_line}"
        );
        assert!(row_line.contains(" on local"), "where it runs: {row_line}");
    }

    /// A merge row in `gating` for `repo`, born the way the queue makes one - answered with its review
    /// id and its merge id.
    fn a_gating_row(store: &Store, repo: &str) -> (String, String) {
        a_review(store, repo, "r-gating", "accepted", 10);
        store
            .move_review(
                "r-gating",
                ReviewState::Waiting,
                ReviewState::Reviewing,
                &Move::new("reviewer:sub-1", "taken"),
                11,
            )
            .unwrap();
        let merge = "m-r-gating".to_string();
        assert!(
            store
                .approve_review(
                    "r-gating",
                    &letibot_tokencore::store::Approval {
                        expect: ReviewState::Reviewing,
                        expect_owner: None,
                        reviewed_sha: "ec3cb01e8a6f4d2c9b1a".into(),
                        by: "reviewer:sub-1".into(),
                        reasons: vec![],
                        files: vec![],
                        commands: vec![],
                        why: "accepted".into(),
                        merge_id: merge.clone(),
                    },
                    12,
                )
                .unwrap()
        );
        assert!(
            store
                .move_merge(
                    &merge,
                    MergeState::Queued,
                    MergeState::Gating,
                    &Move::new("test", "taken").set("owner", "test"),
                    13,
                )
                .unwrap()
        );
        (merge.clone(), merge)
    }

    /// The default scope is the session's own repository - not nineteen sections of other people's
    /// projects for one question about one of them.
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
            said.contains("target `main` in") && said.contains("the gate is red"),
            "{said}"
        );
        assert!(said.contains("review queue: 1 waiting"), "{said}");
        assert!(said.contains("merge queue: 0 open"), "{said}");
    }

    /// A row says its state in words, and who judged it - the store's column names
    /// (`cannot_decide`, `timed_out`) are codes, and this answer is quoted to a person.
    #[test]
    fn a_row_says_its_state_in_words_and_who_judged_it() {
        // The words themselves, every state either queue has.
        for (state, want) in [
            ("waiting", "waiting for a reviewer"),
            ("reviewing", "being reviewed"),
            ("cannot_decide", "the reviewer could not decide"),
            ("rejected", "refused"),
            ("queued", "waiting for the gate"),
            ("gating", "the gate is running"),
            ("timed_out", "the gate timed out"),
            ("landed", "landed"),
            ("landed_local", "landed locally, not pushed"),
            ("blocked", "stopped - a person has to act"),
            ("something_new", "something new"),
        ] {
            assert_eq!(plain_state(state), want, "{state}");
        }
        // Who judged it, from the way the store keys the decider.
        assert_eq!(
            judged_by(Some("reviewer:s-gatekeeper-sub-17917")),
            " by s-gatekeeper-sub-17917"
        );
        assert_eq!(judged_by(Some("human:dead")), " by dead");
        assert_eq!(judged_by(None), "");

        // And the row the full answer prints carries them.
        let dir = TempDir::new("desk-words");
        let mine = a_repo(&dir.0, "mine", None);
        let db = dir.0.join("sessions.db");
        let store = Store::open(&db).unwrap();
        a_review(&store, &mine, "r-waiting", "waiting its turn", 10);
        let desk = a_desk(&db, &dir.0.join("mine"));
        let said = desk.status(false, None).unwrap();
        assert!(
            said.contains("`r-waiting` waiting for a reviewer `feat/r-waiting` → `main`"),
            "the row in words: {said}"
        );
    }

    /// The counts are a word of their own. `1 waiting` is a count, read beside rows that say
    /// `waiting for a reviewer`: a count names the state, and never the store's own code.
    #[test]
    fn the_counts_say_a_word_a_reader_uses() {
        for state in FLOW {
            assert_eq!(
                states_of(std::iter::once(state)),
                [format!("1 {}", count_word(state))],
                "{state}"
            );
            assert!(
                !count_word(state).contains('_'),
                "a count says the store's code: {state}"
            );
        }
    }

    /// `all` is every repository, one line each - the whole box in a screenful, and it says how many and
    /// how many are red.
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
                .any(|l| l.starts_with("mine  the gate is red · 1 waiting · unserved")),
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

    /// `repository: "<name>"` prints that one in full - by its tail, by its path, and with the whole
    /// tail of a failing step rather than three lines of it.
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

        // By its directory name - which is not a path under this session's workspace.
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
            said.contains("review queue: 1 row open or parked"),
            "{said}"
        );

        // And by its path, which is the same repository.
        // The two reads are a wall-clock second apart under load, so the age is masked.
        let by_path = desk.status(false, Some(&other)).unwrap();
        assert_eq!(without_ages(&by_path), without_ages(&said));
    }

    /// `text` with every `(... ago)` age cut, so two reads of one state compare equal.
    fn without_ages(text: &str) -> String {
        text.lines()
            .map(|l| match (l.find('('), l.find(" ago)")) {
                (Some(a), Some(b)) if a < b => format!("{}{}", &l[..a], &l[b + 5..]),
                _ => l.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A name that matches two repositories is refused in words that name both - picking one is how a
    /// model comes to read another project's queue and believe it is its own.
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

    /// A session seated above several repositories says so in one sentence and names both ways to
    /// widen - it does not print the box.
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

    /// More rows than a screenful is ten and a count - and what the count drops is the tail of the queue
    /// a row parked on a person is never the one that goes.
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
            said.contains("review queue: 11 waiting · 1 refused"),
            "{said}"
        );
        assert!(said.contains("r-parked"), "the parked row is kept: {said}");
        assert!(said.contains("+2 more rows"), "{said}");
        assert!(said.contains("r-waiting-8"), "the oldest waiting: {said}");
        assert!(
            !said.contains("r-waiting-9"),
            "the tail of the queue: {said}"
        );
        assert!(
            said.contains("… 3 more lines; `repository: \".\"` prints them all"),
            "the tail is cut, and it says so: {said}"
        );
        assert!(said.lines().count() <= 40, "a screenful: {said}");

        // The escape the bounded answer names is the one that works - `.` is this session's own
        // repository, and it prints every row rather than ten.
        let full = desk.status(false, Some(".")).unwrap();
        assert!(full.contains("r-waiting-10"), "every row: {full}");
        assert!(
            full.contains("review queue: 12 rows open or parked"),
            "{full}"
        );
        assert!(!full.contains("+2 more"), "nothing is counted away: {full}");
    }

    // ===== The gate is visible where a person looks first ===== The queue said `gating`, and
    // nothing a session could read said WHICH STEP or for how long - the merge row's detail
    // said it, and a detail is a click away, in a pane, on a row nobody had a reason to open.

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

    /// The gate's clause is the facts a merge row's detail carries, in one line - where it runs, which
    /// step of how many and its command, how long, and (from the caller) whether its output is moving.
    /// The exact words, so the two readers of this line (a model's `status` and a person's eye) cannot
    /// drift apart.
    #[test]
    fn a_running_gates_clause_names_the_step_the_clock_and_the_moving_tail() {
        let now = 1_000_000 + 372_000; // 6m12s in
        let g = a_gate(3, 4, 372_000, 36 * 1024, Some(2_000));
        let live = a_live_gate("/repo", "target", "", "/w/target", 1_000_000);
        assert_eq!(
            gate_said(&live, now, &tail_words(&g, now)),
            "step 3 of 4 `cargo test --offline --workspace --no-fail-fast` · 6m12s so far · on \
             local · tail moving (36 KiB)"
        );
        // A tail that has stood still is not called moving, and the two bands are the pane's own
        // minutes of silence is a build between crates, ten of them is a step going nowhere.
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
        // A log nothing can read is not a stall - the bytes are said and the verdict withheld.
        let unreadable = a_gate(1, 2, 4_000, 12, None);
        assert_eq!(tail_words(&unreadable, 1_004_000), "12 B written");
    }

    /// The target's line says the gate while it runs, and nothing about it when the target is green - in
    /// the one place every session reads: the default `status`, which is this session's own repository
    /// and a screenful of it.
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
        // The worker publishes the run; the desk reads it, not a thread of its own.
        let started = now_ms().saturating_sub(3 * 60_000);
        store
            .gate_began(&a_live_gate(
                &mine,
                "target",
                "",
                &wt.display().to_string(),
                started,
            ))
            .unwrap();
        let desk = a_desk(&db, Path::new(&mine));

        let said = desk.status(false, None).unwrap();
        let line = said
            .lines()
            .find(|l| l.starts_with("target `main`"))
            .unwrap_or_else(|| panic!("the target's line: {said}"));
        // The state, the tip, the step and the command it is on, how long, and where.
        assert!(
            line.starts_with(&format!(
                "target `main` in {mine}: the gate is running at ec3cb01e8a6f · step 3 of 4 \
                 `cargo test --offline --workspace --no-fail-fast` · "
            )),
            "the state, the tip, the step and its command: {line}"
        );
        assert!(line.contains(" on local"), "where it runs: {line}");
        // This box's own /proc and the room at the build dir are read for the line, so the numbers
        // are not pinned here - only that the read was made.
        assert!(line.contains("load ") && line.contains("cores"), "{line}");
        assert!(
            line.ends_with(" · tail moving (36 KiB)"),
            "and whether the output is moving: {line}"
        );
        // A headline, not the story - the clause carries a step's COMMAND here, which is the one part
        // that must not be cut (a truncated command is a wrong one), and this fixture's is long.
        let clause = line.split(": ").nth(1).unwrap_or_default();
        assert!(
            clause.chars().count() < 260,
            "the gate's clause is still a headline: {clause:?}"
        );
        // And `repository:` prints it too - the third scope is the same target line, in full.
        let full = desk.status(false, Some(".")).unwrap();
        assert!(
            full.contains("the gate is running at ec3cb01e8a6f · step 3 of 4 ")
                && full.contains("tail moving (36 KiB)"),
            "the named scope carries the gate as well: {full}"
        );

        // The same gate, judged green, says nothing about it - the clause belongs to the state, not to
        // a step entry left lying in the gate's own registry.
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
            format!("target `main` in {mine}: green at ec3cb01e8a6f"),
            "a green target's line is unchanged: {said}"
        );
        assert!(!said.contains("step 3 of 4"), "{said}");
        drop(gate::step_guard(&wt));
    }

    /// `all` carries it too, per repository - the session that needs this most is the one in ANOTHER
    /// repository: it has no row for this gate, no reason to open one, and the fans it hears are this.
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
            .gate_began(&a_live_gate(
                &mine,
                "target",
                "",
                &wt.display().to_string(),
                now_ms(),
            ))
            .unwrap();
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
            line.starts_with("mine  the gate is running · step 3 of 4 · ")
                && line.contains(" · 1 waiting"),
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

    /// The `queue` answer is read by the model, and every count in it is known: `1 row` and
    /// `3 rows`, never `row(s)`.
    #[test]
    fn a_count_is_said_in_words_not_as_a_placeholder() {
        let dir = TempDir::new("desk-counts");
        let mine = a_repo(&dir.0, "mine", None);
        let db = dir.0.join("sessions.db");
        let store = Store::open(&db).unwrap();
        red_target(&store, &mine, "cargo build ok\nFAIL: no binary\n");
        a_review(&store, &mine, "r-mine", "waiting its turn", 10);
        a_review(&store, &mine, "r-two", "waiting its turn", 11);
        let desk = a_desk(&db, &dir.0.join("mine"));
        let said = desk.status(false, None).unwrap();

        assert_eq!(how_many(1, "row", "rows"), "1 row");
        assert_eq!(how_many(3, "row", "rows"), "3 rows");
        // The full shape (`repository: "<name>"`) counts both queues in words.
        let full = status(&store, None).unwrap();
        assert!(
            full.contains("review queue: 2 rows open or parked"),
            "{full}"
        );
        assert!(
            full.contains("merge queue: 0 rows open or parked"),
            "{full}"
        );
        for placeholder in ["(s)", "(es)"] {
            assert!(!said.contains(placeholder), "{said}");
            assert!(!full.contains(placeholder), "{full}");
        }
    }
}
