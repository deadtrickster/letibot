//! Sessions put out of sight once their work is over (plan 1.6, R276). Archive only hides. One
//! rule set serves the daemon's sweep (facts newer than the daemon) and the backfill
//! (`letibot gc --archive-settled`, every fact, listed before it acts).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use letibot_tokencore::store::{
    OwnerKind, OwnerRow, OwnerState, QueueTransition, ReviewRow, ReviewState, SessionStub, Store,
    SubagentRun,
};

/// A reviewer with no verdict, no successor and a row that moved on is only judged abandoned
/// after this long, so the moment between its start and the queue naming it is never read as one.
pub const ORPHAN_GRACE_MS: i64 = 10 * 60 * 1000;

/// Why a session's work is over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// A review queue reviewer whose verdict the queue took.
    VerdictDelivered,
    /// A `--prompt` run's session, once nothing holds it open.
    OneShotEnded,
    /// A subagent whose answer reached its parent.
    ResultCollected,
    /// A package whose work is on its target.
    Landed(Option<String>),
    /// A package settled without landing.
    Abandoned,
    /// A reviewer replaced by a later one on the same row.
    Superseded(String),
    /// A reviewer whose row moved on with no verdict from it and nobody after it.
    ReviewAbandoned,
    /// A package whose worktree is no longer on this disk: there is nothing left to reopen.
    WorktreeGone,
}

impl Settled {
    /// The `archived_reason` it is written with.
    pub fn reason(&self) -> String {
        match self {
            Settled::VerdictDelivered => "verdict delivered".into(),
            Settled::OneShotEnded => "one-shot ended".into(),
            Settled::ResultCollected => "result collected".into(),
            Settled::Landed(Some(sha)) => format!("landed {}", super::worktree::short(sha)),
            Settled::Landed(None) => "landed".into(),
            Settled::Abandoned => "abandoned".into(),
            Settled::Superseded(by) => format!("superseded by {by}"),
            Settled::ReviewAbandoned => "review abandoned".into(),
            Settled::WorktreeGone => "worktree gone".into(),
        }
    }
}

/// One session the rules picked, and what became of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub id: String,
    pub rule: Settled,
    /// When the fact the rule read happened, unix ms.
    pub at_ms: i64,
    /// Why it was left in the list, when it was.
    pub refused: Option<String>,
    /// Written: false on a dry run, on a refusal and on a failed write.
    pub archived: bool,
}

impl Decision {
    /// One line for a listing.
    pub fn line(&self) -> String {
        let reason = self.rule.reason();
        match (&self.refused, self.archived) {
            (Some(why), _) => format!("{}  kept ({reason}): {why}", self.id),
            (None, true) => format!("{}  archived: {reason}", self.id),
            (None, false) => format!("{}  would archive: {reason}", self.id),
        }
    }
}

/// The daemon's own reading of a session it serves: `None` when it does not hold it,
/// `Some(None)` when it holds it idle, `Some(Some(why))` when a head, a turn or a card holds it.
pub type LiveFn<'a> = &'a dyn Fn(&str) -> Option<Option<String>>;

/// The write, by whoever owns the session's roster. `None` to [`sweep`] is a dry run.
pub type ActFn<'a> = &'a dyn Fn(&str, &str) -> Result<(), String>;

/// Where a writer claim is judged from: this box, this process and its `/proc`.
#[derive(Debug, Clone)]
pub struct Here {
    pub host: String,
    pub pid: u32,
    pub proc_root: PathBuf,
}

impl Here {
    pub fn this_process() -> Here {
        Here {
            host: super::box_id(),
            pid: std::process::id(),
            proc_root: PathBuf::from("/proc"),
        }
    }
}

/// What the rules read, gathered once per sweep.
struct Facts {
    owners: HashMap<String, OwnerRow>,
    reviews: HashMap<String, ReviewRow>,
    transitions: HashMap<String, Vec<QueueTransition>>,
    runs: HashMap<String, SubagentRun>,
}

/// The rule that applies to `s`, if any, with when its fact happened.
fn rule_for(s: &SessionStub, f: &Facts, now_ms: i64) -> Option<(Settled, i64)> {
    if is_reviewer(s)
        && let Some(found) = reviewer_rule(s, f, now_ms)
    {
        return Some(found);
    }
    if s.origin
        .as_deref()
        .is_some_and(|o| o == "one_shot" || o == "one-shot")
    {
        return Some((Settled::OneShotEnded, s.created_ms));
    }
    if s.parent_id.is_some()
        && let Some(run) = f.runs.get(&s.id)
        && run.delivered
        && run.state != "running"
    {
        return Some((Settled::ResultCollected, run.updated_ms as i64));
    }
    if let Some(o) = f.owners.get(&s.id)
        && o.kind == OwnerKind::Package
        && matches!(o.state, OwnerState::Settled | OwnerState::Swept)
    {
        let at = o.settled_ms.unwrap_or(o.created_ms);
        return Some(match &o.landed_sha {
            Some(sha) => (Settled::Landed(Some(sha.clone())), at),
            None => (Settled::Abandoned, at),
        });
    }
    // A package whose worktree is gone: nothing on this box can open it again, and no owner row is
    // needed - an old package predates the origin column and its row, so the tree it was opened in
    // is the only fact left. A gone folder that is NOT a worktree is never one of these: it may be a
    // conversation somebody moved off this disk, and hiding that is not a rule's to do. The
    // disappearance has no timestamp, so the session's own start stands for it.
    if !Path::new(&s.workspace_root).exists()
        && super::worktree::is_package_worktree(&s.workspace_root)
    {
        return Some((Settled::WorktreeGone, s.created_ms));
    }
    None
}

/// A child of the review queue's session. A `claude -p` reviewer (the daemon's
/// `mergequeue::review::claude`) is a PROCESS and has no session row at all, so there is nothing
/// here to archive: the rules are about the session list, and a run that never enters it cannot be
/// in it.
fn is_reviewer(s: &SessionStub) -> bool {
    s.parent_id
        .as_deref()
        .is_some_and(|p| p.starts_with("s-gatekeeper-"))
}

/// The review row a reviewer's tree names: `<clones>/review-<row>-<pid>-<n>`.
pub fn review_row_of_tree(workspace: &str) -> Option<String> {
    let name = Path::new(workspace).file_name()?.to_str()?;
    let rest = name.strip_prefix("review-")?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let mut fields = rest.rsplitn(3, '-');
    let n = fields.next().unwrap_or_default();
    let pid = fields.next().unwrap_or_default();
    match fields.next() {
        Some(id) if !id.is_empty() && digits(pid) && digits(n) => Some(id.to_string()),
        _ => Some(rest.to_string()),
    }
}

/// The handle a `reviewer <handle> started on <sha>` move names.
fn started_handle(t: &QueueTransition) -> Option<&str> {
    if t.to_state != "reviewing" {
        return None;
    }
    let rest = t.why.strip_prefix("reviewer ")?;
    let (handle, tail) = rest.split_once(' ')?;
    tail.starts_with("started on ").then_some(handle)
}

fn reviewer_row<'a>(s: &SessionStub, f: &'a Facts) -> Option<&'a ReviewRow> {
    if let Some(id) = review_row_of_tree(&s.workspace_root)
        && let Some(r) = f.reviews.get(&id)
    {
        return Some(r);
    }
    let by = format!("reviewer:{}", s.id);
    f.reviews.values().find(|r| {
        r.reviewer.as_deref() == Some(s.id.as_str()) || r.verdict_by.as_deref() == Some(&by)
    })
}

fn reviewer_rule(s: &SessionStub, f: &Facts, now_ms: i64) -> Option<(Settled, i64)> {
    let r = reviewer_row(s, f)?;
    let by = format!("reviewer:{}", s.id);
    let moves = f.transitions.get(&r.id).map(Vec::as_slice).unwrap_or(&[]);
    if let Some(t) = moves.iter().find(|t| t.by == by) {
        return Some((Settled::VerdictDelivered, t.at_ms as i64));
    }
    if r.verdict_by.as_deref() == Some(&by) {
        return Some((Settled::VerdictDelivered, r.state_ms as i64));
    }
    // The next reviewer started after this one's own start, or after its birth when the queue
    // never recorded one (a start that lost the race with a hold).
    let own = moves
        .iter()
        .position(|t| started_handle(t) == Some(s.id.as_str()));
    let successor = moves
        .iter()
        .enumerate()
        .filter(|(i, t)| match own {
            Some(o) => *i > o,
            None => t.at_ms as i64 >= s.created_ms,
        })
        .find_map(|(_, t)| {
            let h = started_handle(t)?;
            (h != s.id).then(|| (h.to_string(), t.at_ms as i64))
        });
    if let Some((next, at)) = successor {
        return Some((Settled::Superseded(next), at));
    }
    // A row still reviewing with no reviewer named may be about to name this one.
    let still_ours =
        r.state == ReviewState::Reviewing && r.reviewer.as_deref().is_none_or(|h| h == s.id);
    if !still_ours && now_ms.saturating_sub(s.created_ms) >= ORPHAN_GRACE_MS {
        return Some((Settled::ReviewAbandoned, r.state_ms as i64));
    }
    None
}

/// The operator's own conversations are never archived by a rule: a top-level session nothing
/// automatic opened. A row that RECORDED `operator` is theirs whatever its folder is; a row that
/// recorded nothing is theirs unless it sits in a package's worktree - a package's session from
/// before the origin column has no other mark, and is not a conversation anybody continues.
fn operators_own(s: &SessionStub, owner: Option<&OwnerRow>) -> bool {
    let automatic_owner = owner.is_some_and(|o| o.kind != OwnerKind::Session);
    let theirs = match s.origin.as_deref().filter(|o| !o.is_empty()) {
        Some(origin) => origin == "operator",
        None => !super::worktree::is_package_worktree(&s.workspace_root),
    };
    s.parent_id.is_none() && theirs && !automatic_owner
}

/// The project files beside a store's file. The store itself is read too, for its own tables.
fn project_stores(store: &Store) -> Vec<Store> {
    let mut out = Vec::new();
    let Some(dir) = store.path().and_then(Path::parent) else {
        return out;
    };
    let projects = dir.join(letibot_tokencore::store::split::PROJECTS_DIR);
    let Ok(entries) = std::fs::read_dir(&projects) else {
        return out;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "db"))
        .collect();
    files.sort();
    for f in files {
        if let Ok(s) = Store::open_reader(&f) {
            out.push(s);
        }
    }
    out
}

/// Sessions with an open job and sessions with an open board ask, in every project file.
struct Holds {
    jobs: HashSet<String>,
    asks: HashSet<String>,
}

fn holds(store: &Store, others: &[Store]) -> Holds {
    let mut h = Holds {
        jobs: HashSet::new(),
        asks: HashSet::new(),
    };
    for s in std::iter::once(store).chain(others.iter()) {
        for p in s.open_processes().unwrap_or_default() {
            if p.state.is_open() {
                h.jobs.insert(p.session.clone());
            }
        }
        h.asks.extend(
            s.sessions_waiting_on_operator()
                .unwrap_or_default()
                .into_keys(),
        );
    }
    h
}

/// Why `s` must stay in the list now, or `None` when nothing holds it.
fn refusal(
    store: &Store,
    s: &SessionStub,
    owner: Option<&OwnerRow>,
    holds: &Holds,
    here: &Here,
    live: LiveFn<'_>,
    rule: &Settled,
) -> Option<String> {
    // A gone package worktree has nothing left to reopen, and the rule proved it is one by the
    // folder alone - so the owner's mark is not a shield there. The operator's ruling: a package
    // session an older build saved as `operator` is hidden with the rest, and hiding is undoable.
    if !matches!(rule, Settled::WorktreeGone) && operators_own(s, owner) {
        return Some("the operator's own session".into());
    }
    let mine = match live(&s.id) {
        Some(Some(why)) => return Some(why),
        Some(None) => true,
        None => false,
    };
    if let Ok(Some(w)) = store.session_writer(&s.id) {
        let ours = w.host == here.host && w.pid == here.pid;
        if ours && !mine {
            return Some("held open by this process".into());
        }
        if !ours && w.host != here.host {
            return Some(format!("held open on {}", w.host));
        }
        if !ours && crate::firecode::held::alive(&here.proc_root, w.pid) {
            return Some(format!("held open by pid {}", w.pid));
        }
    }
    if holds.jobs.contains(&s.id) {
        return Some("a job is open".into());
    }
    if holds.asks.contains(&s.id) {
        return Some("an ask is open".into());
    }
    None
}

/// Every unarchived session a rule settles with a fact at or after `since_ms`, judged against
/// the guards, and written through `act` (none: a dry run). Each id is decided once.
pub fn sweep(
    store: &Store,
    here: &Here,
    live: LiveFn<'_>,
    since_ms: i64,
    now_ms: i64,
    act: Option<ActFn<'_>>,
) -> Vec<Decision> {
    let Ok(stubs) = store.unarchived_stubs() else {
        return Vec::new();
    };
    decide(store, here, live, &stubs, since_ms, now_ms, act)
}

/// [`sweep`] for one session, as a terminal event calls it.
pub fn settle_one(
    store: &Store,
    here: &Here,
    live: LiveFn<'_>,
    id: &str,
    now_ms: i64,
    act: Option<ActFn<'_>>,
) -> Option<Decision> {
    let stubs: Vec<SessionStub> = store
        .unarchived_stubs()
        .ok()?
        .into_iter()
        .filter(|s| s.id == id)
        .collect();
    decide(store, here, live, &stubs, 0, now_ms, act)
        .into_iter()
        .next()
}

fn decide(
    store: &Store,
    here: &Here,
    live: LiveFn<'_>,
    stubs: &[SessionStub],
    since_ms: i64,
    now_ms: i64,
    act: Option<ActFn<'_>>,
) -> Vec<Decision> {
    let reviews: HashMap<String, ReviewRow> = store
        .review_rows(None)
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();
    let owners: HashMap<String, OwnerRow> = store
        .owners()
        .unwrap_or_default()
        .into_iter()
        .map(|o| (o.id.clone(), o))
        .collect();
    let others = project_stores(store);
    let mut runs: HashMap<String, SubagentRun> = HashMap::new();
    let has_children = stubs.iter().any(|s| s.parent_id.is_some());
    if has_children {
        for s in std::iter::once(store).chain(others.iter()) {
            for r in s.subagent_runs().unwrap_or_default() {
                runs.insert(r.child_id.clone(), r);
            }
        }
    }
    let mut f = Facts {
        owners,
        reviews,
        transitions: HashMap::new(),
        runs,
    };
    // Only the rows a reviewer here can name, and only rows that moved inside the window.
    let wanted: HashSet<String> = stubs
        .iter()
        .filter(|s| is_reviewer(s))
        .filter_map(|s| reviewer_row(s, &f).map(|r| r.id.clone()))
        .filter(|id| {
            f.reviews
                .get(id)
                .is_some_and(|r| r.updated_ms as i64 >= since_ms)
        })
        .collect();
    for id in wanted {
        let moves = store.transitions("review", &id).unwrap_or_default();
        f.transitions.insert(id, moves);
    }
    let picked: Vec<(&SessionStub, Settled, i64)> = stubs
        .iter()
        .filter_map(|s| rule_for(s, &f, now_ms).map(|(rule, at)| (s, rule, at)))
        .filter(|(_, _, at)| *at >= since_ms)
        .collect();
    if picked.is_empty() {
        return Vec::new();
    }
    let held = holds(store, &others);
    let mut out = Vec::new();
    for (s, rule, at_ms) in picked {
        let refused = refusal(store, s, f.owners.get(&s.id), &held, here, live, &rule);
        let archived = match (&refused, act) {
            (None, Some(act)) => act(&s.id, &rule.reason()).is_ok(),
            _ => false,
        };
        out.push(Decision {
            id: s.id.clone(),
            rule,
            at_ms,
            refused,
            archived,
        });
    }
    out
}

/// The plain write, for a caller with no roster to update.
pub fn write_to(store: &Store) -> impl Fn(&str, &str) -> Result<(), String> + '_ {
    move |id, reason| {
        store
            .set_archived(id, true, Some(reason))
            .map_err(|e| e.to_string())
    }
}

/// No daemon's view: every writer claim is judged from the store and `/proc`.
pub fn no_daemon(_: &str) -> Option<Option<String>> {
    None
}
