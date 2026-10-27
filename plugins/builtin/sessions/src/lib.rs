//! **The session list, as a built-in plugin** — the first of the layers `docs/plugins-plan.md`
//! §7 step 10 compiles into the binary.
//!
//! The head that draws this pane has a list of its own, and the point of this module is that it
//! should not have to: which sessions are grouped together, what each row says, what
//! `WAITING ON YOU` means, which rows fold and what Enter does on them are *policy*. A project
//! that wants its reviewers' children folded away, or its one-shots hidden, says so in one small
//! layer that calls below and edits what this one answers — and that layer is a file, not a
//! release.
//!
//! # What it reads
//!
//! One feed, `sessions` (`plugin.toml`'s `feeds = ["sessions"]`): the daemon's session list as the
//! head holds it, which is read-only data nobody adjudicates. The feed carries `here` (the session
//! the head is standing in) and `home` (so a path can be drawn as `~/…`), and the host's fold
//! state arrives under `folded`, by row id — a fold is the head's to keep, so pressing one costs no
//! call and survives the next answer.
//!
//! # What it answers
//!
//! One panel with one `tree` block: the groups in their order, the rows inside them nested by their
//! parent, the number each session takes (so `/switch N` and the drawn number agree), and one
//! `enter` act per row — `switch` or `fold` — that the host performs.
//!
//! # This file repeats rules on purpose
//!
//! The grouping rule and the wording of a row are `letibot_sessionlog::origin`'s and the head's
//! `session_rows`' today. What is here is a COPY — the copy that matters, because it is the one a
//! project's layer overrides.

use letibot_guest_abi as abi;
use letibot_plugin::pane::{Act, Cell, Fold, Input, Role};
use serde::Deserialize;
use serde_json::{Value, json};

/// **The groups, in the order the list draws them** — the operator's own first, then one line per
/// kind of session the harness makes for itself.
const GROUPS: [Group; 5] = [
    Group {
        key: "mine",
        title: "mine",
        legend: "the conversations you opened",
        foldable: false,
    },
    Group {
        key: "subagents",
        title: "subagents",
        legend: "task children, and their children",
        foldable: true,
    },
    Group {
        key: "review-queue",
        title: "review queue",
        legend: "the review queue's session and its reviewers",
        foldable: true,
    },
    Group {
        key: "packages",
        title: "packages",
        legend: "conversations a package started (letim)",
        foldable: true,
    },
    Group {
        key: "one-shots",
        title: "one-shots",
        legend: "what a `--prompt` run left behind",
        foldable: true,
    },
];

/// One group: the word in the data, the word on the line, and whether it can be folded.
struct Group {
    key: &'static str,
    title: &'static str,
    legend: &'static str,
    foldable: bool,
}

/// How far up a parent chain the grouping rule walks before it decides the chain loops.
const MAX_DEPTH: usize = 64;

/// The review queue's session id begins with this and a twelve-hex workspace key
/// (`letibot_sessionlog::GATEKEEPER_SESSION_PREFIX`), the fallback mark for a row written before
/// `origin` existed. Repeated because a plugin is a separate crate from the library.
const GATEKEEPER_PREFIX: &str = "s-gatekeeper";

/// The title a reviewer's row is cut from (`letibot_sessionlog::GATEKEEPER_TITLE_PREFIX`).
const GATEKEEPER_TITLE: &str = "You are the gatekeeper.";

/// The seat a review is recorded under (`letibot_sessionlog::GATEKEEPER_SEAT`).
const GATEKEEPER_SEAT: &str = "gatekeeper";

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
    /// `operator`, `package`, `queue` or `one_shot`.
    #[serde(default)]
    origin: String,
    /// The seat the session's row records, when the head has it.
    #[serde(default)]
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

    /// **Which group a session is drawn in** — `letibot_sessionlog::origin::group_of`'s rule: the
    /// queue propagates up a chain and nothing else does.
    fn group_of(&self, id: &str) -> &'static str {
        let mut cur = id;
        for _ in 0..MAX_DEPTH {
            let Some(row) = self.sessions.iter().find(|s| s.id == cur) else {
                break;
            };
            if is_queue_work(row) {
                return "review-queue";
            }
            match row.parent.as_deref() {
                Some(parent) => cur = parent,
                None => break,
            }
        }
        let Some(row) = self.sessions.iter().find(|s| s.id == id) else {
            return "mine";
        };
        // The parent edge before the origin, because it is the mark the tree already draws.
        if row.parent.is_some() {
            return "subagents";
        }
        match row.origin.as_str() {
            "package" => "packages",
            "one_shot" | "one-shot" => "one-shots",
            "queue" => "review-queue",
            _ => "mine",
        }
    }

    /// A row's fold state, folding taken from the person before the family rule and the default.
    fn fold_state(&self, id: &str, depth: usize) -> Fold {
        match self.opened(id) {
            Some(false) => return Fold::Closed,
            Some(true) => return Fold::Open,
            None => {}
        }
        // The group this head is standing in opens itself: a list that hides where you are
        // cannot answer *where am I*.
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

fn is_queue_work(row: &Session) -> bool {
    row.origin == "queue"
        || is_gatekeeper_session(&row.id)
        || row.role.as_deref() == Some(GATEKEEPER_SEAT)
        || row.title.starts_with(GATEKEEPER_TITLE)
}

fn is_gatekeeper_session(id: &str) -> bool {
    if id == GATEKEEPER_PREFIX {
        return true;
    }
    match id
        .strip_prefix(GATEKEEPER_PREFIX)
        .and_then(|rest| rest.strip_prefix('-'))
    {
        Some(key) => key.len() == 12 && key.bytes().all(|b| b.is_ascii_hexdigit()),
        None => false,
    }
}

/// **The whole list**, in drawing order.
fn rows(ctx: &Ctx<'_>) -> Vec<Out> {
    let of: Vec<&'static str> = ctx.sessions.iter().map(|s| ctx.group_of(&s.id)).collect();
    // **A head standing in a sub-session draws its FAMILY**: the parent and its children, and no
    // groups at all — a list of two or three rows around the one the person is in.
    if let Some(parent) = ctx
        .at(ctx.here)
        .and_then(|i| ctx.sessions[i].parent.clone())
        && let Some(p) = ctx.at(&parent)
    {
        let fold = has_fold(ctx, p, 0, None, &of);
        let mut out = vec![session_row(ctx, p, 0, 1, fold)];
        children(ctx, &mut out, &parent, 1, 1, true, None, &of);
        return out;
    }
    let kinds = GROUPS.iter().filter(|g| of.contains(&g.key)).count();
    let mut out = Vec::new();
    let mut number = 0;
    for group in &GROUPS {
        let members: Vec<usize> = (0..ctx.sessions.len())
            .filter(|&i| of[i] == group.key)
            .collect();
        if members.is_empty() {
            continue;
        }
        // **With no header there is no fold to open**, so the one group's rows are simply the
        // list — a lone closed group would draw nothing at all.
        // **A group that cannot fold is always open**, and one group alone draws no header at
        // all — so its rows are simply the list.
        let open = kinds == 1 || !group.foldable || ctx.opened(group.title).unwrap_or(false);
        if kinds > 1 {
            let fold = if !group.foldable {
                Fold::Fixed
            } else if open {
                Fold::Open
            } else {
                Fold::Closed
            };
            out.push(group_row(group, members.len(), fold));
        }
        if !open {
            continue;
        }
        let depth = usize::from(kinds > 1);
        for &i in &members {
            // **A session is drawn ONCE**, under the group it is in: a child whose parent is in
            // this same group is drawn under it, and anything else is a root here.
            let under_parent = ctx.sessions[i]
                .parent
                .as_deref()
                .and_then(|p| ctx.at(p))
                .is_some_and(|p| of[p] == group.key);
            if under_parent {
                continue;
            }
            number += 1;
            let fold = has_fold(ctx, i, depth, Some(group.key), &of);
            out.push(session_row(ctx, i, depth, number, fold));
            children(
                ctx,
                &mut out,
                &ctx.sessions[i].id,
                depth + 1,
                number,
                false,
                Some(group.key),
                &of,
            );
        }
    }
    out
}

/// One group's line: what it holds, how many, how it folds and what it is.
fn group_row(group: &Group, count: usize, open: Fold) -> Out {
    Out {
        id: group.title.to_string(),
        depth: 0,
        number: None,
        cells: vec![Cell::role(
            format!("{} ({count})", group.title),
            Role::Strong,
        )],
        facts: Vec::new(),
        under: vec![group.legend.to_string()],
        open: Some(open),
        // **A group that cannot fold carries no act on Enter** — `mine` is always open, and a row
        // where Enter does nothing must not offer a key that does nothing.
        enter: group.foldable.then(Act::fold),
    }
}

/// One session's row.
fn session_row(ctx: &Ctx<'_>, at: usize, depth: usize, number: usize, open: Option<Fold>) -> Out {
    let s = &ctx.sessions[at];
    Out {
        id: s.id.clone(),
        depth,
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

/// **Whether this row has a fold, and which way** — about the tree *inside a group*, and only for
/// a session whose children are drawn in the group it is drawn in: a `-` over a row would promise
/// rows this list does not hold.
fn has_fold(
    ctx: &Ctx<'_>,
    at: usize,
    depth: usize,
    group: Option<&str>,
    of: &[&'static str],
) -> Option<Fold> {
    let id = ctx.sessions[at].id.as_str();
    let kids = ctx
        .sessions
        .iter()
        .filter(|s| s.parent.as_deref() == Some(id))
        .count();
    let drawn_here = match group {
        None => kids > 0 || ctx.sessions.iter().any(|s| s.parent.as_deref() == Some(id)),
        Some(group) => ctx
            .sessions
            .iter()
            .enumerate()
            .any(|(k, c)| c.parent.as_deref() == Some(id) && of.get(k) == Some(&group)),
    };
    if kids == 0 || !drawn_here {
        return None;
    }
    Some(ctx.fold_state(id, depth))
}

/// A session's children, one step deeper — the recursive half of [`rows`].
#[allow(clippy::too_many_arguments)]
fn children(
    ctx: &Ctx<'_>,
    out: &mut Vec<Out>,
    parent: &str,
    depth: usize,
    mut number: usize,
    force: bool,
    group: Option<&str>,
    of: &[&'static str],
) {
    // **A row's children are drawn when it is OPENED, and not before** — the head's own rule:
    // a collapsed default is what keeps thirty subagents from burying four conversations, and
    // `None` (nobody has said) is closed.
    let open = force || ctx.opened(parent) == Some(true);
    for (i, s) in ctx.sessions.iter().enumerate() {
        if s.parent.as_deref() != Some(parent) {
            continue;
        }
        // A row belongs to exactly one group, so a child of another group's session is not drawn
        // under it — it is a root where it is drawn.
        if let Some(group) = group
            && of.get(i) != Some(&group)
        {
            continue;
        }
        if !open && !ctx.path.iter().any(|p| *p == s.id) {
            continue;
        }
        number += 1;
        let fold = has_fold(ctx, i, depth, group, of);
        out.push(session_row(ctx, i, depth, number, fold));
        children(ctx, out, &s.id, depth + 1, number, false, group, of);
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
    let sessions = drawn.iter().filter(|r| r.number.is_some()).count();
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
    let _ = sessions;
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
