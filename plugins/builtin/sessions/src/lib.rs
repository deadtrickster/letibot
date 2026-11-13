//! The session list, as a built-in plugin. What each row says, which rows fold and what Enter does are
//! policy, so a project says so in one small layer rather than in the head. Every line starts closed,
//! except the chain to where this head is standing.

use letibot_guest_abi as abi;
use letibot_plugin::pane::{Act, Cell, Fold, Role};
use letibot_plugin::sessions::{EMPTY, TITLE, clean, more, short_id};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// The groups under a project's `system` branch, in the order the list draws them - the words, and
/// the sentence under each line. Which session is in which is not decided here: the rule needs the
/// whole chain, so the head decides it and the feed carries it as `system`.
const SYSTEM: [(&str, &str); 3] = [
    (
        "gatekeepers",
        "the review queue's own session, one per project",
    ),
    ("reviewers", "a review's own child, and anything it spawned"),
    (
        "keepers",
        "the notes keeper, which repairs memory and runs nothing",
    ),
];

/// How far up a parent chain this list walks before it decides the chain loops.
const MAX_DEPTH: usize = 64;

/// How far in a row is drawn - the renderer caps the indent at three steps, and the head's own copy
/// of this tree caps at the same number, so the two pictures are one.
const MAX_INDENT: usize = 3;

/// The branch a walk is inside - the project, and the `system` group when the walk is under
/// `system`. `None` is the family view and it holds every session: kinship is not a display
/// category, so a family is walked with no branch at all.
type Branch<'a> = Option<(&'a str, Option<&'a str>)>;

/// The feed, as this layer reads it.
#[derive(Debug, Deserialize)]
struct Feed {
    #[serde(default)]
    sessions: Vec<Session>,
    /// Package sessions of other daemons, already worded by the head.
    #[serde(default)]
    fleet: Vec<Fleet>,
    /// The line ending the group when stopped packages are not listed, worded by the head.
    #[serde(default)]
    unlisted: Option<String>,
    /// Sessions the host could not put in the feed at all - its cap, not this list's. A number a
    /// reader can act on, because a list that stops at 250 without saying so is a lie by omission.
    #[serde(default)]
    more: usize,
    /// The session the head is standing in - its list is drawn around it.
    #[serde(default)]
    here: String,
    /// The head's home, so a workspace under it is drawn as `~/…`.
    #[serde(default)]
    home: String,
}

/// One package session of another daemon.
#[derive(Debug, Deserialize)]
struct Fleet {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    facts: String,
    #[serde(default)]
    under: String,
    #[serde(default)]
    running: bool,
}

/// The fold key of the packages line, the head's `PACKAGES_KEY`.
const PACKAGES_KEY: &str = "fleet/packages";
const UNLISTED_KEY: &str = "fleet/unlisted";
/// The fold keys of the two project groups, the head's `RECENT_KEY` and `OLDER_KEY`.
const RECENT_KEY: &str = "groups/recent";
const OLDER_KEY: &str = "groups/older";
/// Said under the first project line only.
const PROJECT_LEGEND: &str = "a workspace root, and every session that runs in it";
/// The archived group: its line says what the rows are, since a row of it is drawn like any other.
const ARCHIVED_LEGEND: &str = "put out of sight - every row of it is kept: enter resumes one";

/// One session, as the head holds it and as this list reads it.
#[derive(Debug, Deserialize)]
struct Session {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    parent: Option<String>,
    /// `operator`, `package`, `queue` or `one_shot`. Read by nothing here - the tree's facts are the
    /// feed's (`project`, `system`) - and kept because a project's layer over it may want it.
    #[serde(default)]
    #[allow(dead_code)]
    origin: String,
    /// The role (profile) the session is seated as: drawn on its row, so `explore` and `coder`
    /// children are two rows rather than one title and one model twice.
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    live: bool,
    #[serde(default)]
    running: bool,
    #[serde(default)]
    heads: usize,
    /// Room lines held for this session's idle digest: told, and not yet shown to its model.
    #[serde(default)]
    held: u64,
    /// Rows in the live log, for a session the daemon holds.
    #[serde(default)]
    items: usize,
    /// Rows in the store, for one it does not.
    #[serde(default)]
    stored: usize,
    #[serde(default)]
    model: String,
    #[serde(default)]
    workspace: String,
    /// How many questions this session is waiting on the operator to answer.
    #[serde(default)]
    waits_on: usize,
    /// The operator has paused its idle plan-check.
    #[serde(default)]
    nags_paused: bool,
    /// When it last moved, for the recency sort - the head decides the bucket, this list decides
    /// which project a reader sees first.
    #[serde(default)]
    moved_ms: u64,
    /// The same fact as one word (`3h`) - the right-hand column's last fact. Bucketed by the head,
    /// so an idle frame asks this layer for nothing.
    #[serde(default)]
    age: Option<String>,
    /// The project this session is drawn under - the key its row's project line is folded by, and the
    /// name that line says. Both come from the head, which is where the rule lives.
    #[serde(default)]
    project: String,
    #[serde(default)]
    project_name: String,
    /// `own`, `recent` or `older`: the top-level group the head put the project in.
    #[serde(default)]
    bucket: String,
    /// Which group under `system` this row belongs to (`gatekeepers`, `reviewers`, `keepers`), or
    /// `None` for a session of the project's own tree.
    #[serde(default)]
    system: Option<String>,
}

/// One row this layer is about to draw - and the row as the wire spells it, so a row is built once
/// instead of into a `Value` tree and then serialized out of it. The tree cost twenty allocations a
/// row, and that is the fuel a few hundred rows do not have.
#[derive(Serialize)]
struct Out {
    id: String,
    #[serde(skip_serializing_if = "is_zero")]
    depth: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    number: Option<usize>,
    cells: Vec<Cell>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    facts: Vec<Cell>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    under: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open: Option<Fold>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enter: Option<Act>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// The view as C3 spells it, written straight out for the reason [`Out`] gives.
#[derive(Serialize)]
struct View {
    title: &'static str,
    panels: [Panel; 1],
    legend: BTreeMap<&'static str, &'static str>,
}

#[derive(Serialize)]
struct Panel {
    id: &'static str,
    summary: [Summary; 1],
}

#[derive(Serialize)]
struct Summary {
    tree: Tree,
}

#[derive(Serialize)]
struct Tree {
    rows: Vec<Out>,
    /// The plugin's own sentence for an empty list, because *the daemon has not answered yet* and
    /// *this project keeps no such rows* are two different facts.
    empty: &'static str,
}

/// What one call needs besides the feed itself: the three indexes a walk reads once per row. Every one
/// of them was a scan of the whole list when this pane trapped on a daemon's own session list, and a
/// call's fuel is what a trap is made of.
struct Ctx<'a> {
    sessions: &'a [Session],
    fleet: &'a [Fleet],
    unlisted: Option<&'a str>,
    here: &'a str,
    home: &'a str,
    folded: &'a BTreeMap<String, bool>,
    /// Sessions the host's cap left out of the feed.
    more: usize,
    /// The chain from `here` up to its root, so the way back is always on the list.
    path: Vec<String>,
    /// A session's id to its place in the feed - the first, as `position` found it.
    index: HashMap<&'a str, usize>,
    /// A session's id to the places its children hold, in feed order.
    kids: HashMap<&'a str, Vec<usize>>,
    /// A project's key to its members' places, in feed order.
    by_project: HashMap<&'a str, Vec<usize>>,
    /// The places the archived rows hold, in feed order - in the feed and not in the tree.
    archived: Vec<usize>,
}

impl<'a> Ctx<'a> {
    /// The context for one call. The path is the chain from `here` up, capped at [`MAX_DEPTH`], and
    /// the indexes are built here because every walk below is a lookup and no walk may scan.
    fn new(feed: &'a Feed, folded: &'a BTreeMap<String, bool>) -> Ctx<'a> {
        let mut index: HashMap<&str, usize> = HashMap::new();
        let mut kids: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut by_project: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut archived: Vec<usize> = Vec::new();
        for (i, s) in feed.sessions.iter().enumerate() {
            // An archived row is in the feed and not in the tree: it is drawn flat under its own
            // group, so it must not be a parent here, a member of a project, or the row this head
            // is standing in - a row that was in the tree and in the group would be drawn twice.
            if s.bucket == letibot_plugin::sessions::ARCHIVED_BUCKET {
                archived.push(i);
                continue;
            }
            index.entry(s.id.as_str()).or_insert(i);
            by_project.entry(s.project.as_str()).or_default().push(i);
            if let Some(parent) = s.parent.as_deref() {
                kids.entry(parent).or_default().push(i);
            }
        }
        let mut path = vec![feed.here.clone()];
        let mut cur = feed.here.as_str();
        for _ in 0..MAX_DEPTH {
            let Some(s) = index.get(cur).map(|&i| &feed.sessions[i]) else {
                break;
            };
            let Some(parent) = s.parent.as_deref() else {
                break;
            };
            path.push(parent.to_string());
            cur = parent;
        }
        Ctx {
            sessions: &feed.sessions,
            fleet: &feed.fleet,
            unlisted: feed.unlisted.as_deref(),
            here: &feed.here,
            home: &feed.home,
            folded,
            more: feed.more,
            path,
            index,
            kids,
            by_project,
            archived,
        }
    }

    fn at(&self, id: &str) -> Option<usize> {
        self.index.get(id).copied()
    }

    /// What the person has said about a row: `Some(true)` open, `Some(false)` folded away.
    fn opened(&self, id: &str) -> Option<bool> {
        self.folded.get(id).copied()
    }

    /// Whether a node's rows are on the list - the person's answer, then whether this head is
    /// standing in it. Every node's default is closed, the project's line included, and that is why
    /// there is no default parameter: what a closed list must still answer is *where am I*.
    fn node_open(&self, key: &str) -> bool {
        self.opened(key).unwrap_or_else(|| self.on_the_way(key))
    }

    /// Whether a node is on the chain to where this head is standing - the project, the `system`
    /// branch and the group of the session `here`. A list that hides where you are cannot answer
    /// *where am I*. The `system` branch is on the chain only when `here` is drawn in it.
    fn on_the_way(&self, key: &str) -> bool {
        let Some(here) = self.at(self.here) else {
            return false;
        };
        let s = &self.sessions[here];
        let project = s.project.as_str();
        if key == project {
            return true;
        }
        let Some(group) = s.system.as_deref() else {
            return false;
        };
        key == format!("{project}/system") || key == format!("{project}/system/{group}")
    }

    /// Whether session `i`'s parent is on this feed and drawn in this branch - the rule that decides
    /// whether a row is a root of the branch or a child inside it.
    fn parent_in(&self, i: usize, project: &str, system: Option<&str>) -> bool {
        self.sessions[i]
            .parent
            .as_deref()
            .and_then(|p| self.at(p))
            .is_some_and(|p| self.in_branch(p, Some((project, system))))
    }

    /// The projects this feed holds, the one that moved most recently first - the key and the name
    /// to draw. First-seen order is the daemon's, and it buries the project somebody is working on
    /// behind whatever the store listed first; a tie keeps that order, because a stable sort is the
    /// only thing that can.
    fn projects(&self, bucket: &str) -> Vec<(&str, &str)> {
        let mut out: Vec<(&str, &str, u64)> = Vec::new();
        let mut at: HashMap<&str, usize> = HashMap::new();
        for s in self.sessions.iter().filter(|s| s.bucket == bucket) {
            match at.get(s.project.as_str()) {
                Some(&i) => out[i].2 = out[i].2.max(s.moved_ms),
                None => {
                    at.insert(&s.project, out.len());
                    out.push((&s.project, &s.project_name, s.moved_ms));
                }
            }
        }
        out.sort_by_key(|p| std::cmp::Reverse(p.2));
        out.into_iter().map(|(key, name, _)| (key, name)).collect()
    }

    /// How many of these rows are waiting on the operator, and how many are generating - the
    /// rollup a group's line carries, so a session waiting inside a folded group is not invisible.
    fn tally(&self, ids: &[usize]) -> (usize, usize) {
        let mut waiting = 0;
        let mut generating = 0;
        for &i in ids {
            let Some(s) = self.sessions.get(i) else {
                continue;
            };
            waiting += usize::from(s.waits_on > 0);
            generating += usize::from(s.running);
        }
        (waiting, generating)
    }

    /// Whether this session - or anything it spawned - is generating. A folded parent whose child
    /// is working is a row that would otherwise read as idle, and opencode's spinner counts the
    /// children for exactly that reason.
    fn generating(&self, id: &str) -> bool {
        let mut stack = vec![id];
        // A feed cannot cycle (a child names one parent), but a malformed one must not spin: the
        // walk is capped by the rows there are.
        let mut seen = 0;
        while let Some(id) = stack.pop() {
            seen += 1;
            if seen > self.sessions.len() + 1 {
                break;
            }
            if let Some(&i) = self.index.get(id)
                && self.sessions[i].running
            {
                return true;
            }
            if let Some(kids) = self.kids.get(id) {
                stack.extend(kids.iter().map(|&k| self.sessions[k].id.as_str()));
            }
        }
        false
    }

    /// The places a project's members hold, in feed order.
    fn members(&self, project: &str) -> &[usize] {
        self.by_project.get(project).map_or(&[], Vec::as_slice)
    }

    /// Whether a session is drawn in this branch - the project, and the `system` group when the walk
    /// is under `system`. `None` holds every session and is the family view's branch: a reviewer's
    /// parent is a gatekeeper while the reviewer is a `reviewer`, so filtering would drop it.
    fn in_branch(&self, i: usize, branch: Branch<'_>) -> bool {
        let Some((project, system)) = branch else {
            return true;
        };
        self.sessions
            .get(i)
            .is_some_and(|s| s.project == project && s.system.as_deref() == system)
    }

    /// Whether session `i` is drawn under `parent` in this branch - the fold glyph is about the tree
    /// inside a row, so a child in another branch is not a reason for a `+`.
    fn kids_in(&self, parent: &str, branch: Branch<'_>) -> bool {
        self.kids
            .get(parent)
            .is_some_and(|kids| kids.iter().any(|&i| self.in_branch(i, branch)))
    }

    /// Whether one of `id`'s children is on the chain to the session `here` - the second reason a
    /// row's children are on the list at all (see [`children`]), and so a reason for a `-`.
    fn child_on_path(&self, id: &str) -> bool {
        self.kids.get(id).is_some_and(|kids| {
            kids.iter()
                .any(|&i| self.path.contains(&self.sessions[i].id))
        })
    }

    /// A session's fold state - the chain to `here` first, then the person's answer. The glyph is
    /// those two facts and nothing else: the head's `children_open`, `expanded || child_on_path`.
    fn fold_state(&self, id: &str) -> Fold {
        // A `+` over a row whose child is on the screen is the one thing this list must not say, and
        // the chain is drawn whatever the person last said about the fold: a session the head is
        // standing in cannot be hidden, so its parent says `-` even when it was folded away.
        if self.child_on_path(id) {
            return Fold::Open;
        }
        match self.opened(id) {
            Some(true) => Fold::Open,
            _ => Fold::Closed,
        }
    }
}

/// The whole list, in drawing order - a tree of projects, and a `system` branch inside each. The
/// shape is the head's (`App::session_rows`), and the walk drawn here is the only one: the head
/// draws this answer and keeps no tree of its own.
fn rows(ctx: &Ctx<'_>) -> Vec<Out> {
    let mut out = rows_of_the_branches(ctx);
    // What the daemon put out of sight, last of all and in BOTH walks: a family view is drawn from
    // the same daemon's list, so it can be brought back from here too. They are KEPT and are not on
    // the tree, so they are flat under one line rather than a project of their own.
    if !ctx.archived.is_empty() {
        let key = letibot_plugin::sessions::ARCHIVED_KEY;
        let open = ctx.node_open(key);
        out.push(node_row(
            key.to_string(),
            0,
            format!("archived ({})", ctx.archived.len()),
            ARCHIVED_LEGEND,
            open,
            ctx.tally(&ctx.archived),
        ));
        if open {
            for &i in &ctx.archived {
                out.push(archived_row(ctx, i));
            }
        }
    }
    if ctx.more > 0 {
        // No id: a line a reader can act on, not a row a key can enter. The head keeps such a row
        // out of its stops for exactly that reason.
        out.push(Out {
            id: String::new(),
            depth: 0,
            number: None,
            cells: vec![Cell::role(more(ctx.more), Role::Faint)],
            facts: Vec::new(),
            under: Vec::new(),
            open: None,
            enter: None,
        });
    }
    out
}

/// A project's tree and a `system` branch inside each - everything [`rows`] draws but the line that
/// says the feed itself was cut.
fn rows_of_the_branches(ctx: &Ctx<'_>) -> Vec<Out> {
    // A head standing in a sub-session draws its family: the parent and its children, and no tree at
    // all. The family is walked with no branch, because kinship is not a display category - filtered
    // by the parent's group, this walk dropped every child and drew the one row beside the head.
    if let Some(parent) = ctx
        .at(ctx.here)
        .and_then(|i| ctx.sessions[i].parent.clone())
        && let Some(p) = ctx.at(&parent)
    {
        let fold = has_fold(ctx, p, None);
        let mut out = vec![session_row(ctx, p, 0, 1, fold)];
        let mut number = 1;
        children(ctx, &mut out, &parent, 1, &mut number, true, None);
        return out;
    }
    let mut out = Vec::new();
    let mut number = 0;
    let mut said = false;
    let mut say = || {
        let l = if said { "" } else { PROJECT_LEGEND };
        said = true;
        l
    };
    for p in ctx.projects("own") {
        push_project(ctx, &mut out, &mut number, p, 0, say());
    }
    for (bucket, key, title, legend) in [
        (
            "recent",
            RECENT_KEY,
            "other projects",
            "projects with a session active in the last 24 hours",
        ),
        (
            "older",
            OLDER_KEY,
            "older",
            "projects with nothing in the last 24 hours",
        ),
    ] {
        let group = ctx.projects(bucket);
        if group.is_empty() {
            continue;
        }
        let open = ctx.node_open(key);
        // What is inside every project of this group, folded away or not.
        let inside: Vec<usize> = group
            .iter()
            .flat_map(|(p, _)| ctx.members(p).iter().copied())
            .collect();
        out.push(node_row(
            key.to_string(),
            0,
            format!("{title} ({})", group.len()),
            legend,
            open,
            ctx.tally(&inside),
        ));
        if open {
            for p in group {
                push_project(ctx, &mut out, &mut number, p, 1, say());
            }
        }
    }
    if !ctx.fleet.is_empty() || ctx.unlisted.is_some() {
        let open = ctx.node_open(PACKAGES_KEY);
        // Another daemon's rows are not in this feed, so this line rolls up nothing rather than
        // counting what it cannot see.
        out.push(node_row(
            PACKAGES_KEY.to_string(),
            0,
            format!("packages in other daemons ({})", ctx.fleet.len()),
            "package sessions of other folders' daemons - enter says how to attach",
            open,
            (0, 0),
        ));
        if open {
            for f in ctx.fleet {
                out.push(Out {
                    id: f.id.clone(),
                    depth: 1,
                    number: None,
                    cells: vec![Cell::role(clean(&f.name), Role::Plain)],
                    facts: vec![Cell::role(
                        clean(&f.facts),
                        if f.running {
                            Role::Pending
                        } else {
                            Role::Faint
                        },
                    )],
                    under: vec![clean(&f.under)],
                    open: None,
                    enter: Some(Act::switch(f.id.clone())),
                });
            }
            if let Some(line) = ctx.unlisted {
                out.push(Out {
                    id: UNLISTED_KEY.to_string(),
                    depth: 1,
                    number: None,
                    cells: vec![Cell::role(clean(line), Role::Faint)],
                    facts: Vec::new(),
                    under: Vec::new(),
                    open: None,
                    enter: None,
                });
            }
        }
    }
    out
}

/// One row of the archived group: what it is, when it last moved, and the way back. No number and no
/// fold - nothing here is on the tree, and a fold glyph would offer rows this list does not hold.
fn archived_row(ctx: &Ctx<'_>, at: usize) -> Out {
    let s = &ctx.sessions[at];
    Out {
        id: s.id.clone(),
        depth: 1,
        number: None,
        cells: vec![mark_of(ctx, s), Cell::role(name_of(s), Role::Plain)],
        facts: facts_of(s),
        under: vec![under_of(s, ctx.home)],
        open: None,
        // Enter opens it: the daemon takes it back into the list on the way, which is why the row
        // leaves this group rather than staying in it.
        enter: Some(Act::switch(s.id.clone())),
    }
}

/// One project's line and, when it is open, its tree and `system` branch. `base` is the line's own depth,
/// and `legend` the sentence under it (empty after the first project line).
fn push_project(
    ctx: &Ctx<'_>,
    out: &mut Vec<Out>,
    number: &mut usize,
    (project, name): (&str, &str),
    base: usize,
    legend: &str,
) {
    let members = ctx.members(project);
    let key = project.to_string();
    let open = ctx.node_open(&key);
    out.push(node_row(
        key,
        base,
        format!("{name} ({})", members.len()),
        legend,
        open,
        ctx.tally(members),
    ));
    if !open {
        return;
    }
    // The project's own tree - the sessions nobody spawned, and their children.
    for &i in members {
        if ctx.sessions[i].system.is_some() || ctx.parent_in(i, project, None) {
            continue;
        }
        *number += 1;
        let fold = has_fold(ctx, i, Some((project, None)));
        out.push(session_row(ctx, i, base + 1, *number, fold));
        children(
            ctx,
            out,
            &ctx.sessions[i].id,
            base + 2,
            number,
            false,
            Some((project, None)),
        );
    }
    // The `system` branch, when this project has one - and no line at all when it has not.
    let system: Vec<usize> = members
        .iter()
        .copied()
        .filter(|&i| ctx.sessions[i].system.is_some())
        .collect();
    if system.is_empty() {
        return;
    }
    let skey = format!("{project}/system");
    let open = ctx.node_open(&skey);
    out.push(node_row(
        skey,
        base + 1,
        format!("system ({})", system.len()),
        "the sessions the harness runs for itself",
        open,
        ctx.tally(&system),
    ));
    if !open {
        return;
    }
    for (group, group_legend) in SYSTEM {
        let group_members: Vec<usize> = system
            .iter()
            .copied()
            .filter(|&i| ctx.sessions[i].system.as_deref() == Some(group))
            .collect();
        if group_members.is_empty() {
            continue;
        }
        let gkey = format!("{project}/system/{group}");
        let open = ctx.node_open(&gkey);
        out.push(node_row(
            gkey,
            base + 2,
            format!("{group} ({})", group_members.len()),
            group_legend,
            open,
            ctx.tally(&group_members),
        ));
        if !open {
            continue;
        }
        for &i in &group_members {
            // A row is drawn once, under its parent when that parent is in this same group;
            // anything else is a root of the group's tree.
            if ctx.parent_in(i, project, Some(group)) {
                continue;
            }
            *number += 1;
            let fold = has_fold(ctx, i, Some((project, Some(group))));
            out.push(session_row(ctx, i, base + 3, *number, fold));
            children(
                ctx,
                out,
                &ctx.sessions[i].id,
                base + 4,
                number,
                false,
                Some((project, Some(group))),
            );
        }
    }
}

/// One node's line: what it is, how many rows it holds, how many of them are asking for somebody,
/// how it folds and what Enter does to it. `tally` is what is inside it - the two counts a fold
/// would hide, which is why a folded group must still say them.
fn node_row(
    key: String,
    depth: usize,
    title: String,
    legend: &str,
    open: bool,
    tally: (usize, usize),
) -> Out {
    Out {
        id: key,
        depth: depth.min(MAX_INDENT),
        number: None,
        // A project's line is a path, which is somebody's too - the same rule as a row's words.
        cells: vec![Cell::role(clean(&title), Role::Strong)],
        facts: rollup(tally),
        under: if legend.is_empty() {
            Vec::new()
        } else {
            vec![legend.to_string()]
        },
        open: Some(if open { Fold::Open } else { Fold::Closed }),
        enter: Some(Act::fold()),
    }
}

/// One session's row: a gutter mark, the name, the facts a reader scans for - ONE line, so thirty
/// sessions are thirty rows and not sixty.
fn session_row(ctx: &Ctx<'_>, at: usize, depth: usize, number: usize, open: Option<Fold>) -> Out {
    let s = &ctx.sessions[at];
    Out {
        id: s.id.clone(),
        depth: depth.min(MAX_INDENT),
        number: Some(number),
        cells: vec![
            mark_of(ctx, s),
            Cell::role(
                name_of(s),
                if s.id == ctx.here {
                    Role::Strong
                } else {
                    Role::Plain
                },
            ),
        ],
        facts: facts_of(s),
        // What tells two sessions of one title apart. The head draws it under the row the cursor
        // is on, and nowhere else (`App::tree_of`): an id and a path under every row doubled the
        // list's length for facts a person reads one row at a time.
        under: vec![under_of(s, ctx.home)],
        open,
        enter: Some(Act::switch(s.id.clone())),
    }
}

/// The one column before a row's name: `?` for a session waiting on the operator - opencode's
/// gutter, and the reason `WAITING ON YOU` is not spelled into the name any more - `~` for a turn
/// that is running, its children's included. The column is reserved on every row, or a name's
/// column would be a function of its state.
fn mark_of(ctx: &Ctx<'_>, s: &Session) -> Cell {
    if s.waits_on > 0 {
        return Cell::role("?", Role::Attention);
    }
    if ctx.generating(&s.id) {
        return Cell::role("~", Role::Pending);
    }
    Cell::role(" ", Role::Faint)
}

/// The two counts a group's line carries, in the order a reader acts on them: somebody waiting on
/// the operator first, work in progress second, and nothing at all for a group with neither.
fn rollup((waiting, generating): (usize, usize)) -> Vec<Cell> {
    let mut facts = Vec::new();
    if waiting > 0 {
        facts.push(Cell::role(format!("{waiting} waiting"), Role::Attention));
    }
    if generating > 0 {
        facts.push(Cell::role(
            format!("{generating} generating"),
            Role::Pending,
        ));
    }
    facts
}

/// Whether this row has a fold, and which way - about the tree *inside the branch it is drawn in*:
/// a `-` over a row would promise rows this list does not hold.
fn has_fold(ctx: &Ctx<'_>, at: usize, branch: Branch<'_>) -> Option<Fold> {
    let id = ctx.sessions[at].id.as_str();
    ctx.kids_in(id, branch).then(|| ctx.fold_state(id))
}

/// A session's children, one step deeper - the recursive half of [`rows`]. `branch` is the row's
/// place in the tree, and `None` is the family view's own walk - see [`Ctx::in_branch`] for why a
/// family passes none.
#[allow(clippy::too_many_arguments)]
fn children(
    ctx: &Ctx<'_>,
    out: &mut Vec<Out>,
    parent: &str,
    depth: usize,
    number: &mut usize,
    force: bool,
    branch: Branch<'_>,
) {
    // A row's children are drawn when it is opened, and not before - a collapsed default is what
    // keeps thirty subagents from burying four conversations, and `None` is closed.
    let depth = depth.min(MAX_INDENT);
    let open = force || ctx.opened(parent) == Some(true);
    let Some(kids) = ctx.kids.get(parent) else {
        return;
    };
    for &i in kids {
        let s = &ctx.sessions[i];
        // A row belongs to exactly one branch, so a child of another branch is not drawn under it.
        if !ctx.in_branch(i, branch) {
            continue;
        }
        if !open && !ctx.path.contains(&s.id) {
            continue;
        }
        // The number is the n-th session drawn, carried through the whole walk: a subtree that
        // numbered itself would hand two rows the same number.
        *number += 1;
        let n = *number;
        let fold = has_fold(ctx, i, branch);
        out.push(session_row(ctx, i, depth, n, fold));
        children(ctx, out, &s.id, depth + 1, number, false, branch);
    }
}

/// A session's name - its title, or a short id when it has none - with the slug its parent and its
/// branch call it by. What the row is waiting for is the gutter's and the facts', not the name's: a
/// name is what a person is looking for, and a mark inside it hides the word they scan for.
fn name_of(s: &Session) -> String {
    let name = if s.title.is_empty() {
        short_id(&s.id)
    } else {
        s.title.clone()
    };
    let name = if s.slug.trim().is_empty() {
        name
    } else {
        format!("{} · {name}", s.slug)
    };
    // A title is somebody's, and a control character in it would be measured one way and drawn
    // another - the same rule the under line is drawn with.
    clean(&name)
}

/// The right-hand column: how much conversation there is and how long ago it moved - the two facts
/// a row is read for, and the last of them flush with the edge where the eye finds it. A running
/// turn is the gutter's, and the id, the path and the model are the detail line's.
fn facts_of(s: &Session) -> Vec<Cell> {
    // One string with ` · ` between the facts that are there, not a `Vec<String>` joined at the
    // end: a join is a second copy of the row's whole right column, once per row, and this is a
    // list of them.
    let mut facts = String::new();
    if !s.live {
        // The whole difference between a row that costs one keystroke and a row that costs a
        // resume.
        push_fact(&mut facts, format_args!("on disk"));
    }
    if s.nags_paused {
        push_fact(&mut facts, format_args!("nags paused"));
    }
    let rows = if s.stored > 0 { s.stored } else { s.items };
    if rows > 0 {
        push_fact(&mut facts, format_args!("{rows} rows"));
    }
    // What it has been told and its model has not been shown: invisible on every other column.
    if s.held > 0 {
        push_fact(&mut facts, format_args!("{} held", s.held));
    }
    if let Some(role) = s.role.as_deref().filter(|r| !r.is_empty()) {
        push_fact(&mut facts, format_args!("{role}"));
    }
    if let Some(age) = s.age.as_deref().filter(|a| !a.is_empty()) {
        push_fact(&mut facts, format_args!("{}", clean(age)));
    }
    if facts.is_empty() {
        return Vec::new();
    }
    vec![Cell::role(facts, Role::Faint)]
}

/// One fact, after the ` · ` that separates it - written into the row's own string, so a fact costs
/// no allocation of its own.
fn push_fact(into: &mut String, fact: std::fmt::Arguments<'_>) {
    use std::fmt::Write;
    if !into.is_empty() {
        into.push_str(" · ");
    }
    let _ = into.write_fmt(fact);
}

/// The faint line under a row - the full id, the workspace, the model and the heads: what tells two
/// sessions of one title apart and where a session actually runs. Every row carries it and the head
/// draws it for the row under the cursor alone.
fn under_of(s: &Session, home: &str) -> String {
    let mut line = clean(&s.id);
    if !s.workspace.is_empty() {
        line.push_str("  ");
        line.push_str(&tilde(&clean(&s.workspace), home));
    }
    if !s.model.is_empty() {
        line.push_str(" · ");
        line.push_str(&clean(&s.model));
    }
    if s.heads > 0 {
        let plural = if s.heads == 1 { "" } else { "s" };
        line.push_str(&format!(" · {} head{plural}", s.heads));
    }
    line
}

/// `$HOME` written as `~` - the head's own shortening, done here because the row's text is this
/// layer's to decide.
fn tilde(path: &str, home: &str) -> String {
    if home.is_empty() {
        return path.to_string();
    }
    match path.strip_prefix(home) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// The input this layer reads: the envelope's own fields, and the `sessions` feed typed on the way
/// in. The SDK's `pane::Input` holds `feeds` as a `Value` tree, so reading the feed from it parses a
/// daemon's whole session list twice, and the second pass is what made this pane exhaust its fuel.
#[derive(Deserialize)]
struct Ask {
    #[serde(default)]
    folded: BTreeMap<String, bool>,
    #[serde(default)]
    feeds: Feeds,
}

#[derive(Deserialize, Default)]
struct Feeds {
    #[serde(default)]
    sessions: Option<Feed>,
}

/// The pane a host hands no `sessions` feed: one line naming the missing half, because an empty list
/// and a broken host look identical on a screen and mean opposite things.
fn no_feed() -> View {
    View {
        title: TITLE,
        panels: [Panel {
            id: "sessions",
            summary: [Summary {
                tree: Tree {
                    rows: Vec::new(),
                    empty: "this host hands no `sessions` feed, so there is nothing to list — the \
                            pane needs a daemon that builds feeds (docs/plugins-plan.md §4.9).",
                },
            }],
        }],
        legend: BTreeMap::new(),
    }
}

/// The pane's answer, or `None` for *unchanged* - a layer with nothing to say passes, and what is
/// below it answers.
fn answer(bytes: &[u8]) -> Option<String> {
    let ask: Ask = serde_json::from_slice(bytes).ok()?;
    // A feed the host did not hand is a screen that says so. Passing would draw nothing at all, and
    // a pane that vanishes is a pane somebody thinks they closed.
    let Some(feed) = ask.feeds.sessions else {
        return serde_json::to_string(&no_feed()).ok();
    };
    let ctx = Ctx::new(&feed, &ask.folded);
    let drawn = rows(&ctx);
    let view = View {
        title: TITLE,
        panels: [Panel {
            id: "sessions",
            summary: [Summary {
                tree: Tree {
                    rows: drawn,
                    empty: EMPTY,
                },
            }],
        }],
        legend: BTreeMap::from([
            (
                "?",
                "this session has a question for you — attach to it to answer",
            ),
            ("~", "a turn is running in it, or in something it spawned"),
            (
                "on disk",
                "the daemon is not holding it; switching to it resumes it",
            ),
        ]),
    };
    serde_json::to_string(&view).ok()
}

/// The layer's policy, tested natively - see `tests.rs`.
#[cfg(test)]
mod tests;

/// The pane hook - `pane_<name>`, the name a head opens by.
#[unsafe(no_mangle)]
pub extern "C" fn pane_sessions(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, answer)
}
