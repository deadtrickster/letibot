//! **The session list, as a built-in plugin** — the first of the layers `docs/plugins-plan.md`
//! §7 step 10 compiles into the binary.
//!
//! The head that draws this pane has a list of its own, and the point of this module is that it
//! should not have to: what each row says, what `WAITING ON YOU` means, which rows fold and what
//! Enter does on them are *policy*. A project that wants its reviewers' rows folded away, or its
//! one-shots named differently, says so in one small layer that calls below and edits what this one
//! answers — and that layer is a file, not a release.
//!
//! # What it reads
//!
//! One feed, `sessions` (`plugin.toml`'s `feeds = ["sessions"]`): the daemon's session list as the
//! head holds it, which is read-only data nobody adjudicates. The feed carries `here` (the session
//! the head is standing in) and `home` (so a path can be drawn as `~/…`), and the host's fold
//! state arrives under `folded`, by row id — a fold is the head's to keep, so pressing one costs no
//! call and survives the next answer.
//!
//! **The tree's two facts are the feed's**: `project` (with `project_name`) and `system`. Which
//! project a workspace belongs to is a rule about paths and which session is a reviewer is a rule
//! about a chain, and both need the whole list — the head has it, a guest does not. This layer
//! decides where a row goes *given* those two facts, and what it says.
//!
//! # What it answers
//!
//! One panel with one `tree` block: a project's line, the sessions nobody spawned in it and their
//! children at any depth, then `system` with one folded line per group — the number each session
//! takes (so `/switch N` and the drawn number agree), and one `enter` act per row — `switch` or
//! `fold` — that the host performs.
//!
//! # This file repeats rules on purpose
//!
//! The shape of the tree and the wording of a row are the head's `session_rows` and
//! `session_list`'s. What is here is a COPY — the copy that matters, because it is the one a
//! project's layer overrides.

use letibot_guest_abi as abi;
use letibot_plugin::pane::{Act, Cell, Fold, Input, Role};
use serde::Deserialize;
use serde_json::{Value, json};

/// **The groups under a project's `system` branch, in the order the list draws them** — the words,
/// and the sentence under each line.
///
/// **Which session is in which is not decided here.** A guest cannot walk a store, and the rule
/// needs the whole chain (a reviewer is known by the queue's session above it), so the head decides
/// it and the feed carries the answer as `system`. This table is only what a group is CALLED.
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

/// **How far in a row is drawn** — the renderer caps the indent at three steps (`rano`'s tree
/// list, and a name pushed four steps in is off the right edge), and the head's own copy of this
/// tree caps at the same number, so the two pictures are one.
const MAX_INDENT: usize = 3;

/// The feed, as this layer reads it.
#[derive(Debug, Deserialize)]
struct Feed {
    #[serde(default)]
    sessions: Vec<Session>,
    /// The session the head is standing in — its list is drawn around it.
    #[serde(default)]
    here: String,
    /// The head's home, so a workspace under it is drawn as `~/…`.
    #[serde(default)]
    home: String,
}

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
    /// `operator`, `package`, `queue` or `one_shot`. Read by nothing here — the tree's facts are the
    /// feed's (`project`, `system`) — and kept because a project's layer over it may want it.
    #[serde(default)]
    #[allow(dead_code)]
    origin: String,
    /// The seat the session's row records, when the head has it. Kept for the same reason as
    /// `origin`: this layer does not read it, a layer above this one may.
    #[serde(default)]
    #[allow(dead_code)]
    role: Option<String>,
    #[serde(default)]
    live: bool,
    #[serde(default)]
    running: bool,
    #[serde(default)]
    heads: usize,
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
    /// **The project this session is drawn under** — the key its row's project line is folded by,
    /// and the name that line says. Both come from the head, which is where the rule lives.
    #[serde(default)]
    project: String,
    #[serde(default)]
    project_name: String,
    /// **Which group under `system` this row belongs to** (`gatekeepers`, `reviewers`,
    /// `keepers`), or `None` for a session of the project's own tree.
    #[serde(default)]
    system: Option<String>,
}

/// One row this layer is about to draw.
struct Out {
    id: String,
    depth: usize,
    number: Option<usize>,
    cells: Vec<Cell>,
    facts: Vec<Cell>,
    under: Vec<String>,
    open: Option<Fold>,
    enter: Option<Act>,
}

/// What one call needs besides the feed itself.
struct Ctx<'a> {
    sessions: &'a [Session],
    here: &'a str,
    home: &'a str,
    folded: &'a std::collections::BTreeMap<String, bool>,
    /// The chain from `here` up to its root, so the way back is always on the list.
    path: Vec<String>,
}

impl Ctx<'_> {
    fn at(&self, id: &str) -> Option<usize> {
        self.sessions.iter().position(|s| s.id == id)
    }

    /// What the person has said about a row: `Some(true)` open, `Some(false)` folded away.
    fn opened(&self, id: &str) -> Option<bool> {
        self.folded.get(id).copied()
    }

    /// **Whether a node's rows are on the list** — the person's answer, then whether this head is
    /// standing in it, then the node's own default: a project and its `system` branch are open, a
    /// group inside `system` is folded.
    fn node_open(&self, key: &str, default: bool) -> bool {
        match self.opened(key) {
            Some(open) => open,
            None => default || self.on_the_way(key),
        }
    }

    /// **Whether a node is on the chain to where this head is standing** — the project, the
    /// `system` branch and the group of the session `here`. A list that hides where you are cannot
    /// answer *where am I*.
    fn on_the_way(&self, key: &str) -> bool {
        let Some(here) = self.at(self.here) else {
            return false;
        };
        let s = &self.sessions[here];
        let project = s.project.as_str();
        if key == project || key == format!("{project}/system") {
            return true;
        }
        s.system
            .as_deref()
            .is_some_and(|g| key == format!("{project}/system/{g}"))
    }

    /// Whether session `i`'s parent is on this feed **and drawn in this branch** — the rule that
    /// decides whether a row is a root of the branch or a child inside it.
    fn parent_in(&self, i: usize, project: &str, system: Option<&str>) -> bool {
        self.sessions[i]
            .parent
            .as_deref()
            .and_then(|p| self.at(p))
            .is_some_and(|p| self.in_branch(p, project, system))
    }

    /// **The projects this feed holds, in the order it lists them** — the key and the name to draw.
    fn projects(&self) -> Vec<(&str, &str)> {
        let mut out: Vec<(&str, &str)> = Vec::new();
        for s in self.sessions {
            if !out.iter().any(|(k, _)| *k == s.project) {
                out.push((&s.project, &s.project_name));
            }
        }
        out
    }

    /// **Whether a session is drawn in this branch** — the project, and the `system` group when the
    /// walk is under `system`.
    fn in_branch(&self, i: usize, project: &str, system: Option<&str>) -> bool {
        self.sessions
            .get(i)
            .is_some_and(|s| s.project == project && s.system.as_deref() == system)
    }

    /// Whether session `i` is drawn under `parent` **in this branch** — the fold glyph is about the
    /// tree inside a row, so a child in another branch is not a reason for a `+`.
    fn kids_in(&self, parent: &str, project: &str, system: Option<&str>) -> bool {
        self.sessions.iter().enumerate().any(|(i, s)| {
            s.parent.as_deref() == Some(parent) && self.in_branch(i, project, system)
        })
    }

    /// A session's fold state, folding taken from the person before the family rule and the default.
    fn fold_state(&self, id: &str, depth: usize) -> Fold {
        match self.opened(id) {
            Some(false) => return Fold::Closed,
            Some(true) => return Fold::Open,
            None => {}
        }
        // The family view's first level is open by rule: a family whose members were folded away
        // would be a list of one row.
        if depth == 0
            && self
                .at(self.here)
                .and_then(|i| self.sessions[i].parent.as_deref())
                == Some(id)
        {
            return Fold::Open;
        }
        Fold::Closed
    }
}

/// **The whole list**, in drawing order — a tree of projects, and a `system` branch inside each.
///
/// The shape is the head's (`App::session_rows`), and the two are held together by
/// `the_heads_list_and_the_plugins_list_draw_the_same_rows`: a project's line, the sessions nobody
/// spawned in it and their children at any depth, then `system` with one folded line per group.
fn rows(ctx: &Ctx<'_>) -> Vec<Out> {
    // **A head standing in a sub-session draws its FAMILY**: the parent and its children, and no
    // tree at all — a list of two or three rows around the one the person is in.
    if let Some(parent) = ctx.at(ctx.here).and_then(|i| ctx.sessions[i].parent.clone())
        && let Some(p) = ctx.at(&parent)
    {
        let project = ctx.sessions[p].project.clone();
        let system = ctx.sessions[p].system.clone();
        let fold = has_fold(ctx, p, 0, &project, system.as_deref());
        let mut out = vec![session_row(ctx, p, 0, 1, fold)];
        let mut number = 1;
        children(ctx, &mut out, &parent, 1, &mut number, true, &project, system.as_deref());
        return out;
    }
    let mut out = Vec::new();
    let mut number = 0;
    for (project, name) in ctx.projects() {
        let members: Vec<usize> = (0..ctx.sessions.len())
            .filter(|&i| ctx.sessions[i].project == project)
            .collect();
        let key = project.to_string();
        let open = ctx.node_open(&key, true);
        out.push(node_row(
            key,
            0,
            format!("{name} ({})", members.len()),
            "a workspace root, and every session that runs in it",
            open,
        ));
        if !open {
            continue;
        }
        // **The project's own tree** — the sessions nobody spawned, and their children.
        for &i in &members {
            if ctx.sessions[i].system.is_some() || ctx.parent_in(i, project, None) {
                continue;
            }
            number += 1;
            let fold = has_fold(ctx, i, 1, project, None);
            out.push(session_row(ctx, i, 1, number, fold));
            children(ctx, &mut out, &ctx.sessions[i].id, 2, &mut number, false, project, None);
        }
        // **The `system` branch**, when this project has one — and no line at all when it has not.
        let system: Vec<usize> = members
            .iter()
            .copied()
            .filter(|&i| ctx.sessions[i].system.is_some())
            .collect();
        if system.is_empty() {
            continue;
        }
        let skey = format!("{project}/system");
        let open = ctx.node_open(&skey, true);
        out.push(node_row(
            skey,
            1,
            format!("system ({})", system.len()),
            "the sessions the harness runs for itself",
            open,
        ));
        if !open {
            continue;
        }
        for (group, legend) in SYSTEM {
            let group_members: Vec<usize> = system
                .iter()
                .copied()
                .filter(|&i| ctx.sessions[i].system.as_deref() == Some(group))
                .collect();
            if group_members.is_empty() {
                continue;
            }
            let gkey = format!("{project}/system/{group}");
            let open = ctx.node_open(&gkey, false);
            out.push(node_row(
                gkey,
                2,
                format!("{group} ({})", group_members.len()),
                legend,
                open,
            ));
            if !open {
                continue;
            }
            for &i in &group_members {
                // **A row is drawn once**, under its parent when that parent is in this same
                // group; anything else is a root of the group's tree.
                if ctx.parent_in(i, project, Some(group)) {
                    continue;
                }
                number += 1;
                let fold = has_fold(ctx, i, 3, project, Some(group));
                out.push(session_row(ctx, i, 3, number, fold));
                children(
                    ctx,
                    &mut out,
                    &ctx.sessions[i].id,
                    4,
                    &mut number,
                    false,
                    project,
                    Some(group),
                );
            }
        }
    }
    out
}

/// One node's line: what it is, how many rows it holds, how it folds and what Enter does to it.
fn node_row(key: String, depth: usize, title: String, legend: &str, open: bool) -> Out {
    Out {
        id: key,
        depth: depth.min(MAX_INDENT),
        number: None,
        cells: vec![Cell::role(title, Role::Strong)],
        facts: Vec::new(),
        under: vec![legend.to_string()],
        open: Some(if open { Fold::Open } else { Fold::Closed }),
        enter: Some(Act::fold()),
    }
}

/// One session's row.
fn session_row(ctx: &Ctx<'_>, at: usize, depth: usize, number: usize, open: Option<Fold>) -> Out {
    let s = &ctx.sessions[at];
    Out {
        id: s.id.clone(),
        depth: depth.min(MAX_INDENT),
        number: Some(number),
        cells: vec![Cell::role(
            name_of(s),
            if s.id == ctx.here {
                Role::Strong
            } else {
                Role::Plain
            },
        )],
        facts: facts_of(s),
        under: vec![under_of(s, ctx.home)],
        open,
        enter: Some(Act::switch(s.id.clone())),
    }
}

/// **Whether this row has a fold, and which way** — about the tree *inside the branch it is drawn
/// in*: a `-` over a row would promise rows this list does not hold.
fn has_fold(
    ctx: &Ctx<'_>,
    at: usize,
    depth: usize,
    project: &str,
    system: Option<&str>,
) -> Option<Fold> {
    let id = ctx.sessions[at].id.as_str();
    ctx.kids_in(id, project, system)
        .then(|| ctx.fold_state(id, depth))
}

/// A session's children, one step deeper — the recursive half of [`rows`].
#[allow(clippy::too_many_arguments)]
fn children(
    ctx: &Ctx<'_>,
    out: &mut Vec<Out>,
    parent: &str,
    depth: usize,
    number: &mut usize,
    force: bool,
    project: &str,
    system: Option<&str>,
) {
    // **A row's children are drawn when it is OPENED, and not before** — the head's own rule:
    // a collapsed default is what keeps thirty subagents from burying four conversations, and
    // `None` (nobody has said) is closed.
    let depth = depth.min(MAX_INDENT);
    let open = force || ctx.opened(parent) == Some(true);
    for (i, s) in ctx.sessions.iter().enumerate() {
        if s.parent.as_deref() != Some(parent) {
            continue;
        }
        // A row belongs to exactly one branch, so a child of another branch is not drawn under it.
        if !ctx.in_branch(i, project, system) {
            continue;
        }
        if !open && !ctx.path.iter().any(|p| *p == s.id) {
            continue;
        }
        // **The number is the n-th SESSION DRAWN**, carried through the whole walk: a subtree that
        // numbered itself would hand two rows the same number.
        *number += 1;
        let n = *number;
        let fold = has_fold(ctx, i, depth, project, system);
        out.push(session_row(ctx, i, depth, n, fold));
        children(ctx, out, &s.id, depth + 1, number, false, project, system);
    }
}

/// A session's name — its title, or a short id when it has none — with the three things a row says
/// before anything else: the slug its parent and its branch call it by, `WAITING ON YOU`, and
/// paused nags.
fn name_of(s: &Session) -> String {
    let name = if s.title.is_empty() {
        short_id(&s.id).to_string()
    } else {
        s.title.clone()
    };
    let name = if s.slug.trim().is_empty() {
        name
    } else {
        format!("{} · {name}", s.slug)
    };
    // **A session waiting on YOU says so, louder and first**: a person who is being waited on has
    // to see it from the list rather than by attaching to every session to find out.
    let name = match s.waits_on {
        0 => name,
        1 => format!("{name} · WAITING ON YOU"),
        n => format!("{name} · WAITING ON YOU ({n})"),
    };
    if s.nags_paused {
        format!("{name} · nags paused")
    } else {
        name
    }
}

/// The right-hand column: what the row IS, in one run of facts.
fn facts_of(s: &Session) -> Vec<Cell> {
    let mut facts: Vec<String> = Vec::new();
    if s.running {
        facts.push("generating".into());
    }
    if !s.live {
        // The whole difference between a row that costs one keystroke and a row that costs a
        // resume.
        facts.push("on disk".into());
    }
    let rows = if s.stored > 0 { s.stored } else { s.items };
    if rows > 0 {
        facts.push(format!("{rows} rows"));
    }
    if s.heads > 0 {
        facts.push(format!(
            "{} head{}",
            s.heads,
            if s.heads == 1 { "" } else { "s" }
        ));
    }
    if !s.model.is_empty() {
        facts.push(s.model.clone());
    }
    if facts.is_empty() {
        return Vec::new();
    }
    vec![Cell::role(
        facts.join(" · "),
        if s.running {
            Role::Pending
        } else {
            Role::Faint
        },
    )]
}

/// The faint line under every row: the full id, and the workspace when there is one — for a stored
/// session the only thing on the row that says what the conversation was about.
fn under_of(s: &Session, home: &str) -> String {
    let id = clean(&s.id);
    if s.workspace.is_empty() {
        id
    } else {
        format!("{id}  {}", tilde(&clean(&s.workspace), home))
    }
}

/// `$HOME` written as `~` — the head's own shortening, done here because the row's text is this
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

/// Control characters out of a string this layer is about to draw: a title is somebody's, and an
/// escape in it would be measured one way and drawn another.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// A session's id, cut to the tail a person recognises — the head's `short_id` rule.
fn short_id(id: &str) -> &str {
    let tail = id.rsplit(['-', ':']).next().unwrap_or(id);
    if tail.len() >= 6 { tail } else { id }
}

/// The pane's answer, or `None` for *unchanged* — a layer with nothing to say passes, and what is
/// below it (or the head's own list) answers.
fn answer(bytes: &[u8]) -> Option<String> {
    let input: Input = serde_json::from_slice(bytes).ok()?;
    let feed: Feed = serde_json::from_value(input.feeds.get("sessions")?.clone()).ok()?;
    let mut path = vec![feed.here.clone()];
    let mut cur = feed.here.clone();
    for _ in 0..MAX_DEPTH {
        match feed.sessions.iter().find(|s| s.id == cur) {
            Some(s) => match s.parent.clone() {
                Some(p) => {
                    path.push(p.clone());
                    cur = p;
                }
                None => break,
            },
            None => break,
        }
    }
    let ctx = Ctx {
        sessions: &feed.sessions,
        here: &feed.here,
        home: &feed.home,
        folded: &input.folded,
        path,
    };
    let drawn = rows(&ctx);
    let view = json!({
        "title": "sessions in this daemon",
        "panels": [{
            "id": "sessions",
            "summary": [{"tree": {
                "rows": drawn.iter().map(row_value).collect::<Vec<Value>>(),
                // **The plugin's own sentence for an empty list**, because *the daemon has not
                // answered yet* and *this project keeps no such rows* are two different facts.
                "empty": "none listed yet — the daemon has not answered, or this head is \
                          replaying a recorded log and has no daemon to ask.",
            }}],
        }],
        "legend": {
            "WAITING ON YOU": "this session has a question for you — attach to it to answer",
            "on disk": "the daemon is not holding it; switching to it resumes it",
        },
    });
    serde_json::to_string(&view).ok()
}

/// One row as C3 spells it.
fn row_value(row: &Out) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("id".into(), json!(row.id));
    // **How far in the row is drawn.** The list is FLAT — C3 nests by `children`, and a list
    // that has already walked its own tree says the depth instead, which is what keeps a row's
    // column a function of where it is rather than of how the host happened to read it.
    if row.depth > 0 {
        out.insert("depth".into(), json!(row.depth));
    }
    if let Some(number) = row.number {
        out.insert("number".into(), json!(number));
    }
    out.insert("cells".into(), json!(row.cells));
    if !row.facts.is_empty() {
        out.insert("facts".into(), json!(row.facts));
    }
    if !row.under.is_empty() {
        out.insert("under".into(), json!(row.under));
    }
    if let Some(open) = row.open {
        out.insert("open".into(), json!(open));
    }
    if let Some(act) = &row.enter {
        out.insert("enter".into(), json!(act));
    }
    Value::Object(out)
}

/// The layer's policy, tested natively — see `tests.rs`.
#[cfg(test)]
mod tests;

/// The pane hook — `pane_<name>`, the name a head opens by.
#[unsafe(no_mangle)]
pub extern "C" fn pane_sessions(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, answer)
}
