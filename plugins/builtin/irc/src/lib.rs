//! **The top-level session list as an IRC channel's nick list** — the thing a person glances at to
//! see who is here and who is busy, and the thing they switch with.
//!
//! # Why a column and not a screen
//!
//! This pane is meant for the right-hand side of a conversation, so it draws ONE NARROW COLUMN and
//! leaves the rest of the rectangle alone. The host decides the rectangle (a plugin never paints
//! cells, `docs/plugins-plan.md` §4.9) and today it hands a plugin pane the whole screen — so the
//! column is a DRAWING decision this layer makes, not a placement the host offers. Everything the
//! column needs therefore rides in its own `state`: the width (`w`/`n`), whether it is hidden
//! (`h`) and which scope it is narrowed to (`s`). Those are the three gestures a person would
//! expect the layout to give them, and they are the layer's because the layout is not the host's
//! to hand out one pane at a time.
//!
//! # What it reads
//!
//! One feed, `sessions` (`plugin.toml`'s `feeds = ["sessions"]`): the daemon's session list, which
//! the host already holds and adjudicates nothing about. `capabilities = []` — this layer runs
//! nothing, reads no file and asks nobody.
//!
//! **Top-level only.** A row whose `parent` is set is a subagent: it is somebody's work, not a
//! conversation you switch to, and a nick list that listed every subagent would bury the four
//! channels a person is actually in.
//!
//! # What it answers
//!
//! One panel, one `tree` block: a header line (`#letibot · 4 of 31 · 2 working`), one row per
//! session, and — when the scope hides a session that is WORKING — a divider and those rows under
//! it. A row carries `enter: {"switch": <id>}`, which is the host's own verb, and four action
//! letters (`w` `n` `h` `s`) that call this layer back with an `event` word.
//!
//! # The marks are the palette's
//!
//! | mark | is | role |
//! |---|---|---|
//! | `»` | the session you are in | `strong` |
//! | `@` | a head is attached — you are talking to it | `success` |
//! | `+` | a turn is running | `pending` |
//! | `·` | on disk; the daemon is not holding it | `muted` |
//! | ` ` | live and idle | `plain` |
//!
//! One column, the most significant mark, because a nick list's prefix is one character wide. A
//! session that is both `here` and working keeps its `»` and says *working* in the fact column's
//! role instead — the fact is `pending` while a turn runs, and `attention` when a running session
//! has not moved for [`STALE_MS`], which is the whole question this pane answers.
//!
//! # The fact column, and why it is an age
//!
//! `12m` is when the session last moved: a row (a prompt, a call, a result) was appended then.
//! The question a person asks of a list of agents is *is that one stuck?*, and it has no answer in
//! a model name or a row count — a session that has been "generating" for twenty minutes with
//! nothing appended is stuck, and one that appended a row a minute ago is not. So the age is the
//! fact, and its ROLE carries the answer: `pending` while a turn runs, `attention` when that turn
//! has gone quiet for ten minutes, `faint` otherwise.
//!
//! # The order is a roster, not a ranking
//!
//! A list that reorders while you look at it is worse than a list in the wrong order: the host's
//! cursor is an INDEX into these rows, so a re-sort moves the cursor onto somebody else. The order
//! is therefore pinned in `state` and recomputed **only when the set of drawn sessions changes**
//! (one appears, one goes, or the scope changes) — and the rule it is computed by is: the session
//! you are in, then the ones with a head attached, then the ones working, then the idle ones by
//! recency, then the ones only on disk. Ties are broken by id, so the same roster always sorts the
//! same way.

use letibot_guest_abi as abi;
use letibot_plugin::pane::{Act, Cell, Input, Role};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// **The narrowest column worth drawing** — at this width a row is a mark, a name of twelve and a
/// three-letter fact, which is the least that still answers *who* and *is it stuck*.
const MIN_W: usize = 20;

/// **The column a person gets before they say otherwise** — the width a name and a three-letter
/// fact fit in, and less than a third of a screen so that `w` still has somewhere to go.
const DEFAULT_W: usize = 30;

/// One press of `w` or `n`.
const STEP: usize = 4;

/// **The widest the column may be** — about a third of the rectangle, because a nick list that
/// takes half the screen is a conversation you cannot read. The floor is [`MIN_W`]: a rect too
/// narrow to hold even the minimum still gets the minimum and the host clips it.
const THIRD: usize = 3;

/// The name is never cut below this: a list of four-letter stumps is not a list of sessions.
const MIN_NAME: usize = 4;

/// **How long a running session may go without a row before its age is drawn as news** — ten
/// minutes. Below it a turn is just slow; above it, something is wrong and the operator is the one
/// who has to decide.
const STALE_MS: u64 = 10 * 60 * 1000;

/// The pane's state, which is the whole of its memory: a plugin is stateless between calls
/// (`docs/plugins-plan.md` §1, principle 5 — state belongs to the host, and the host hands it
/// back), so the width, the hidden flag, the scope and the roster all ride here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    /// The column's width, or `None` for [`DEFAULT_W`] — `None` rather than the number so a later
    /// change to the default reaches everybody who never chose.
    w: Option<usize>,
    /// The list is folded to its strip; `h` brings it back, and the width and roster are kept.
    hidden: bool,
    /// Which sessions are shown. `None` is [`Scope::Path`], the workspace you are in.
    scope: Option<Scope>,
    /// **The roster, pinned** — the ids in drawn order. Recomputed when the set changes, and
    /// never while it does not, because the host's cursor is an index into these rows.
    order: Vec<String>,
}

impl State {
    /// The scope in force.
    fn scope(&self) -> Scope {
        self.scope.unwrap_or_default()
    }

    /// **The width this rect allows** — the chosen one, clamped to the rectangle's third and to
    /// the minimum, so a pane in a 24-column rect still draws a readable column.
    fn width(&self, rect: usize) -> usize {
        let most = if rect == 0 {
            DEFAULT_W
        } else {
            (rect / THIRD).max(MIN_W)
        };
        self.w.unwrap_or(DEFAULT_W).clamp(MIN_W, most.max(MIN_W))
    }

    /// **What one key did** — the four letters a row carries, and nothing else. An `event` word
    /// this layer does not know is ignored rather than guessed: a key nobody wrote a meaning for
    /// must not move the pane.
    fn apply(&mut self, action: &str, rect: usize) {
        let width = self.width(rect);
        match action {
            "wider" => self.w = Some(width.saturating_add(STEP)),
            "narrower" => self.w = Some(width.saturating_sub(STEP)),
            "hide" => self.hidden = true,
            "show" => self.hidden = false,
            "scope" => self.scope = Some(self.scope().next()),
            _ => {}
        }
    }

    /// **The roster, in the order the rows are drawn**: the ids of this call when the set has
    /// changed, and last call's order when it has not.
    ///
    /// `wanted` arrives already sorted by the rule, so this is the one place *stability* is
    /// decided: a set that is the same set keeps its order even though a member started working
    /// since — a row that moves under the cursor is a row the person has to find again.
    fn roster(&mut self, wanted: Vec<String>) -> Vec<String> {
        let same = self.order.len() == wanted.len() && wanted.iter().all(|id| self.order.contains(id));
        if !same {
            self.order = wanted;
        }
        self.order.clone()
    }
}

/// **Which sessions the list is narrowed to**, cycled by `s` — the vocabulary the merge queue's
/// own pane uses (`Filter::Every`/`Repo`/`Path`), so one word means one thing in this head.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Scope {
    /// The workspace this session is wired to — one folder, one conversation.
    #[default]
    Path,
    /// **The project**: the repository the workspace is a worktree of. The useful one, because a
    /// session in `…/letibot/worktrees/agent-x` and one in `…/letibot` are the same project.
    Repo,
    /// Every session this daemon holds, whatever folder it is in.
    Every,
}

impl Scope {
    /// The next scope in the cycle, and the word a row's action says.
    fn next(self) -> Scope {
        match self {
            Scope::Path => Scope::Repo,
            Scope::Repo => Scope::Every,
            Scope::Every => Scope::Path,
        }
    }
}

/// The feed, as this layer reads it — the host's session list, plus the two facts only a host
/// can know: `now` (a wasm guest has no clock) and `repo` (a repository root is a question for
/// git, not for a string).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Feed {
    sessions: Vec<Session>,
    /// The session this pane belongs to: the one the person is in.
    here: String,
    /// The host's home, so a path can be drawn as `~/…`.
    home: String,
    /// **The host's clock**, ms since the epoch. A guest on `wasm32-unknown-unknown` has no
    /// `SystemTime` — asking for one is a panic, not a zero — so the age of a row is the host's
    /// fact and not this layer's arithmetic.
    now: u64,
}

/// One session, as the feed spells it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Session {
    id: String,
    title: String,
    slug: String,
    /// Set for a subagent: it is somebody's work, not a conversation.
    parent: Option<String>,
    live: bool,
    running: bool,
    heads: usize,
    items: usize,
    stored: usize,
    model: String,
    workspace: String,
    waits_on: usize,
    nags_paused: bool,
    /// **When this session last moved** — the last row appended, ms since the epoch. `0` is
    /// *the host did not say*, which is drawn as `?` rather than as *now*.
    moved_ms: u64,
    /// **The repository the workspace is in**, when the host asked git. Empty is *the host did
    /// not say*, and [`repo_of`] falls back to the tree's own worktree convention.
    repo: String,
}

/// The pane's answer for one call: the state to keep and the view to draw.
fn answer(bytes: &[u8]) -> Option<String> {
    let input: Input = serde_json::from_slice(bytes).ok()?;
    let mut st = match input.state.clone() {
        Some(value) => serde_json::from_value::<State>(value).unwrap_or_default(),
        None => State::default(),
    };
    let rect = input.size.w as usize;
    if let Some(event) = &input.event {
        st.apply(&event.action, rect);
    }
    // **A feed the host did not hand is a screen that says so.** The layer passed would draw
    // nothing at all, and a pane that vanishes is a pane somebody thinks they closed.
    let Some(value) = input.feeds.get("sessions") else {
        return serde_json::to_string(&no_feed()).ok();
    };
    let feed: Feed = serde_json::from_value(value.clone()).unwrap_or_default();
    let view = if st.hidden {
        strip(&feed, &st, rect)
    } else {
        list(&feed, &mut st, rect)
    };
    serde_json::to_string(&view).ok()
}

/// **The pane a host hands no `sessions` feed**: one line naming the missing half, because an
/// empty nick list and a broken host look identical on a screen and mean opposite things.
fn no_feed() -> Value {
    json!({
        "panels": [{
            "id": "nicks",
            "summary": [{"tree": {
                "rows": [],
                "empty": "this host hands no `sessions` feed, so there is nothing to list — the \
                          pane needs a daemon that builds feeds (docs/plugins-plan.md §4.9).",
            }}],
        }],
    })
}

/// **The hidden pane**: one strip instead of nothing. A pane that hides itself to zero height is
/// a pane somebody has to guess their way back into; the strip is the bookmark, and it is a STOP
/// (an `id` of its own) so that the `h` that brings the list back is still on a row the cursor can
/// rest on.
fn strip(feed: &Feed, st: &State, rect: usize) -> Value {
    let counts = counts(feed, st);
    let said = format!(
        "· sessions · {} · {} working · h shows",
        if counts.shown == counts.total {
            format!("{} here", counts.shown)
        } else {
            format!("{} of {}", counts.shown, counts.total)
        },
        counts.working,
    );
    // **The strip is drawn at the RECTANGLE's width, not the column's** — it is the whole of what
    // a hidden pane says, so it spends the room it has.
    let said = if rect > 0 { fit(&said, rect) } else { said };
    json!({
        "badge": {"text": format!("{} here · {} working", counts.shown, counts.working), "role": "pending"},
        "panels": [{
            "id": "nicks",
            "summary": [{"tree": {"rows": [{
                "id": STRIP,
                "cells": [Cell::role(said, Role::Muted)],
                "actions": actions(true),
                "detail": "the nick list is hidden — `h` brings it back at the width and in the \
                           scope it was hidden in.",
            }]}}],
        }],
        "legend": legend(),
        "every_s": 5,
        "state": serde_json::to_value(st).unwrap_or(Value::Null),
    })
}

/// The hidden strip's row id. A row with an id is a stop, and a stop is what a key can be pressed
/// on — the reason the strip is a row and not a sentence.
const STRIP: &str = "·hidden";

/// What the header and the strip count.
struct Counts {
    shown: usize,
    total: usize,
    working: usize,
}

/// The counts, over the top-level sessions: how many this scope shows, how many there are, and how
/// many of the shown ones are working.
fn counts(feed: &Feed, st: &State) -> Counts {
    let here = here_of(feed);
    let top: Vec<&Session> = feed.sessions.iter().filter(|s| s.parent.is_none()).collect();
    let shown: Vec<&&Session> = top.iter().filter(|s| keeps(s, st.scope(), here)).collect();
    Counts {
        shown: shown.len(),
        total: top.len(),
        working: shown.iter().filter(|s| s.running).count(),
    }
}

/// **The list**, in the order it is drawn.
fn list(feed: &Feed, st: &mut State, rect: usize) -> Value {
    let width = st.width(rect);
    let here = here_of(feed);
    let top: Vec<&Session> = feed.sessions.iter().filter(|s| s.parent.is_none()).collect();
    let scope = st.scope();
    let inside: Vec<&Session> = top
        .iter()
        .copied()
        .filter(|s| keeps(s, scope, here))
        .collect();
    // **A session the scope hides but that is WORKING keeps a place** — a narrowed list that
    // hides the one agent doing something is how a person loses an agent. It is drawn under a
    // divider that says how many were left out, and its rows are real stops: Enter switches there.
    let outside: Vec<&Session> = top
        .iter()
        .copied()
        .filter(|s| !keeps(s, scope, here) && s.running)
        .collect();
    let mut wanted: Vec<String> = inside.iter().map(|s| s.id.clone()).collect();
    wanted.extend(outside.iter().map(|s| s.id.clone()));
    wanted.sort_by_key(|id| key_of(id, feed, &feed.here));
    let order = st.roster(wanted);

    let shown = inside.len();
    let total = top.len();
    let working = inside.iter().filter(|s| s.running).count();
    let mut rows: Vec<Value> = Vec::new();
    // The header is not a stop: it is the channel's name and its counts, and a cursor that could
    // rest on it would spend one of the list's keystrokes saying nothing.
    rows.push(json!({"cells": [
        Cell::role(header(scope, here, &Counts { shown, total, working }, width), Role::Strong),
    ]}));
    for id in &order {
        if let Some(s) = inside.iter().find(|s| &s.id == id) {
            rows.push(session_row(s, width, &feed.here, feed.now, &feed.home));
        }
    }
    if inside.is_empty() && outside.is_empty() {
        // **A list with nothing in it is still a row, not an `empty` sentence** — the sentence
        // leaves the pane with no stop at all, and the keys that widen the column, hide it or
        // widen the scope again are on a stop. So the empty state IS the row, and it says what it
        // is: the daemon has not answered, or there is nothing here to list.
        rows.push(json!({
            "id": EMPTY,
            "cells": [Cell::role(
                fit("· no sessions here — the daemon has not answered, or this scope is empty", width),
                Role::Faint,
            )],
            "actions": actions(false),
            "detail": "nothing to list — `s` widens the scope to the project, then to every session \
                       this daemon holds.",
        }));
    }
    let elsewhere = top.len() - shown;
    if !outside.is_empty() {
        rows.push(json!({"cells": [Cell::role(
            format!(
                "· {elsewhere} elsewhere · {} working",
                outside.len()
            ),
            Role::Faint,
        )]}));
        for id in &order {
            if let Some(s) = outside.iter().find(|s| &s.id == id) {
                rows.push(session_row(s, width, &feed.here, feed.now, &feed.home));
            }
        }
    }
    json!({
        "badge": {
            "text": format!("{shown} here · {working} working"),
            "role": if working > 0 { "pending" } else { "faint" },
        },
        "panels": [{
            "id": "nicks",
            "summary": [{"tree": {"rows": rows}}],
        }],
        "legend": legend(),
        // **Five seconds**: a nick list is about *who is busy*, and an age that is a minute stale
        // is a list that says `now` about a session that stopped. `MIN_EVERY_S` is the floor.
        "every_s": 5,
        "state": serde_json::to_value(&*st).unwrap_or(Value::Null),
    })
}

/// The empty scope's row id — a stop, so the keys stay reachable when there is nothing to switch to.
const EMPTY: &str = "·empty";

/// **One session's row**: a mark, a name, and one fact pinned to the column's right edge.
///
/// **The fact is not a `facts` cell.** The host right-aligns `facts` against the RECTANGLE, so on a
/// wide screen the fact would sit at column 100 with a name at column 3 — a column that stretches
/// is not a column. So the fact is the row's third cell and the name is padded to push it there,
/// which is what makes the column the width the person chose whatever the rectangle is.
fn session_row(s: &Session, width: usize, here: &str, now: u64, home: &str) -> Value {
    let (mark, mark_role) = mark_of(s, here);
    let (fact, fact_role) = fact_of(s, now);
    // The host spends two columns on its own lead (`▸` and a space) and one between each pair of
    // cells, so a row of `width` columns holds `width - 5 - <fact>` of name.
    let room = width.saturating_sub(5 + cells(&fact));
    let name = fit(&name_of(s), room.max(MIN_NAME));
    let pad = room.saturating_sub(cells(&name));
    json!({
        "id": s.id,
        "cells": [
            Cell::role(mark, mark_role),
            Cell::role(
                format!("{name}{}", " ".repeat(pad)),
                if s.id == here { Role::Strong } else { Role::Plain },
            ),
            Cell::role(fact, fact_role),
        ],
        "enter": Act::switch(s.id.clone()),
        "actions": actions(false),
        "detail": detail_of(s, here, home),
    })
}

/// **The four keys, on every stop.** They are row actions rather than pane keys because the
/// contract's keys are the host's: a plugin binds letters through a row's `actions`, so a letter
/// works wherever the cursor rests — and these four work on every row, which is what makes them
/// feel like the pane's own.
fn actions(hidden: bool) -> Value {
    json!([
        {"key": "w", "label": "wider", "event": "wider"},
        {"key": "n", "label": "narrower", "event": "narrower"},
        if hidden {
            json!({"key": "h", "label": "shows", "event": "show"})
        } else {
            json!({"key": "h", "label": "hide", "event": "hide"})
        },
        {"key": "s", "label": "scope", "event": "scope"},
    ])
}

/// **What every mark and every fact means** — the `l` view, and the text that answers on hover.
fn legend() -> Value {
    json!({
        "»": "the session you are in",
        "@": "a head is attached to it — you are talking to it",
        "+": "a turn is running in it",
        "·": "on disk — the daemon is not holding it; switching to it resumes it",
        "12m": "how long since this session last moved; orange while a turn has been quiet for \
                ten minutes",
        "!": "it is waiting on you — attach to it to answer",
        "w": "wider (`n` narrower) — the column's own width, kept for the next visit",
        "h": "hide the list to one strip; the same key brings it back as it was",
        "s": "narrow to this workspace, then to this project, then to every session",
    })
}

/// The header: the channel's name (which IS the scope), how many of how many, and how many work.
fn header(scope: Scope, here: Option<&Session>, c: &Counts, width: usize) -> String {
    let channel = format!("#{}", channel_of(scope, here));
    let long = if c.shown == c.total {
        format!("{channel} · {} here · {} working", c.shown, c.working)
    } else {
        format!("{channel} · {} of {} · {} working", c.shown, c.total, c.working)
    };
    if cells(&long) <= width {
        return long;
    }
    let short = format!("{channel} {}/{} {}+", c.shown, c.total, c.working);
    if cells(&short) <= width {
        return short;
    }
    channel
}

/// **The channel's name, which is the scope**: the folder of the workspace, the folder of the
/// repository, or `all`.
fn channel_of(scope: Scope, here: Option<&Session>) -> String {
    let Some(here) = here else {
        return "all".into();
    };
    let path = match scope {
        Scope::Every => return "all".into(),
        Scope::Path => here.workspace.as_str(),
        Scope::Repo => {
            let repo = repo_of(here);
            if repo.is_empty() {
                here.workspace.as_str()
            } else {
                return base_of(&repo);
            }
        }
    };
    let base = base_of(path);
    if base.is_empty() { "here".into() } else { base }
}

/// The last component of a path.
fn base_of(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string()
}

/// **Whether this scope shows this session.**
fn keeps(s: &Session, scope: Scope, here: Option<&Session>) -> bool {
    match scope {
        Scope::Every => true,
        Scope::Path => here.map(|h| h.workspace == s.workspace).unwrap_or(true),
        Scope::Repo => {
            let want = here.map(repo_of).unwrap_or_default();
            want.is_empty() || repo_of(s) == want
        }
    }
}

/// **The repository a session's workspace is in** — the feed's `repo` when the host asked git, and
/// otherwise this tree's own worktree convention (`<repo>/worktrees/<name>`,
/// `<repo>/.claude/worktrees/<name>`), which is where every worktree of this repository lives.
fn repo_of(s: &Session) -> String {
    if !s.repo.trim().is_empty() {
        return s.repo.trim_end_matches('/').to_string();
    }
    conventional_repo(&s.workspace)
}

/// The worktree convention, applied to a path. The longer marker is tried first: `.claude/worktrees`
/// ENDS in `/worktrees/`, so the short one alone would answer `<repo>/.claude` for a head's own
/// worktree.
fn conventional_repo(path: &str) -> String {
    for marker in ["/.claude/worktrees/", "/worktrees/"] {
        if let Some(at) = path.find(marker) {
            return path[..at].to_string();
        }
    }
    path.trim_end_matches('/').to_string()
}

/// The session the pane belongs to.
fn here_of(feed: &Feed) -> Option<&Session> {
    feed.sessions.iter().find(|s| s.id == feed.here)
}

/// **The order the roster is computed by**, when it is computed at all: you, then a head attached,
/// then working, then idle by recency, then only-on-disk. Ties by id, so one roster always sorts
/// the same way.
fn key_of(id: &str, feed: &Feed, here: &str) -> (u8, std::cmp::Reverse<u64>, String) {
    let Some(s) = feed.sessions.iter().find(|s| s.id == id) else {
        return (5, std::cmp::Reverse(0), id.to_string());
    };
    let rank = if id == here {
        0
    } else if s.heads > 0 {
        1
    } else if s.running {
        2
    } else if s.live {
        3
    } else {
        4
    };
    (rank, std::cmp::Reverse(s.moved_ms), id.to_string())
}

/// The mark and the role it is drawn in — one column, the most significant fact first.
fn mark_of(s: &Session, here: &str) -> (&'static str, Role) {
    if s.id == here {
        ("»", Role::Strong)
    } else if s.heads > 0 {
        ("@", Role::Success)
    } else if s.running {
        ("+", Role::Pending)
    } else if !s.live {
        ("·", Role::Muted)
    } else {
        (" ", Role::Plain)
    }
}

/// A session's name — its title, or the tail of its id when it has none — with the two things that
/// must not be missed: `!` for *it is waiting on you*, and the paused-nag mark.
fn name_of(s: &Session) -> String {
    let name = if s.title.trim().is_empty() {
        short_id(&s.id).to_string()
    } else {
        s.title.clone()
    };
    let name = clean(&name);
    let name = if s.waits_on > 0 {
        format!("{name}!")
    } else {
        name
    };
    if s.nags_paused {
        format!("{name} (nags paused)")
    } else {
        name
    }
}

/// **The one fact**: how long since this session last moved, and its role says whether that is
/// news. See this module's note — this is the question *is that one stuck?*, answered per row.
fn fact_of(s: &Session, now: u64) -> (String, Role) {
    let age = age_of(s, now);
    let text = match age {
        Some(ms) => ago(ms),
        None => "?".to_string(),
    };
    let role = if s.running {
        if age.is_some_and(|ms| ms > STALE_MS) {
            Role::Attention
        } else {
            Role::Pending
        }
    } else {
        Role::Faint
    };
    (text, role)
}

/// How long since the session moved, or `None` when the host did not say (or its clock is behind
/// the row's).
fn age_of(s: &Session, now: u64) -> Option<u64> {
    if now == 0 || s.moved_ms == 0 || s.moved_ms > now {
        return None;
    }
    Some(now - s.moved_ms)
}

/// An age in the fewest characters that stay honest — a fact column is three characters wide.
fn ago(ms: u64) -> String {
    let minutes = ms / 60_000;
    if minutes == 0 {
        "now".into()
    } else if minutes < 60 {
        format!("{minutes}m")
    } else if minutes < 24 * 60 {
        format!("{}h", minutes / 60)
    } else {
        format!("{}d", (minutes / (24 * 60)).min(99))
    }
}

/// The prose a row answers on Enter — the whole of what the one fact cannot say.
fn detail_of(s: &Session, here: &str, home: &str) -> String {
    let mut out = vec![format!("`{}`", s.id)];
    if s.id == here {
        out.push("this is the session you are in".into());
    }
    if s.heads > 0 {
        out.push(format!(
            "{} head{} attached",
            s.heads,
            if s.heads == 1 { "" } else { "s" }
        ));
    }
    if s.running {
        out.push("a turn is running".into());
    }
    if !s.live {
        out.push("on disk — switching to it resumes it".into());
    }
    if !s.model.is_empty() {
        out.push(format!("model `{}`", s.model));
    }
    if !s.workspace.is_empty() {
        out.push(format!("workspace `{}`", tilde(&s.workspace, home)));
    }
    if !s.slug.is_empty() {
        out.push(format!("slug `{}`", s.slug));
    }
    if s.waits_on > 0 {
        out.push(format!("waiting on you: {} question(s)", s.waits_on));
    }
    out.push(format!(
        "{} row{}",
        if s.stored > 0 { s.stored } else { s.items },
        if s.stored.max(s.items) == 1 { "" } else { "s" },
    ));
    out.join(" · ")
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

/// A session's id, cut to the tail a person recognises — the head's own `short_id` rule.
fn short_id(id: &str) -> &str {
    let tail = id.rsplit(['-', ':']).next().unwrap_or(id);
    if tail.len() >= 6 { tail } else { id }
}

/// Control characters out of a string this layer is about to draw: a title is somebody's, and an
/// escape in it would be measured one way and drawn another.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// `text`, cut to `room` columns with an ellipsis — the name column's arithmetic, and the one place
/// a narrow column degrades: the mark and the fact never go, the name does.
fn fit(text: &str, room: usize) -> String {
    if cells(text) <= room {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = char_cells(c);
        if used + w > room.saturating_sub(1) {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// **How many columns a run of text takes**, in the terminal's cells rather than in characters: a
/// name is somebody's, and a title in a script with wide glyphs measured by `chars().count()`
/// pushes the fact column one column right per glyph — the misalignment this layer exists not to
/// have.
fn cells(text: &str) -> usize {
    text.chars().map(char_cells).sum()
}

/// One character's width: two cells for the East Asian wide and fullwidth blocks and the emoji
/// planes a terminal draws double, one for everything else.
fn char_cells(c: char) -> usize {
    let u = c as u32;
    let wide = (0x1100..=0x115F).contains(&u)
        || ((0x2E80..=0xA4CF).contains(&u) && u != 0x303F)
        || (0xAC00..=0xD7A3).contains(&u)
        || (0xF900..=0xFAFF).contains(&u)
        || (0xFE30..=0xFE6F).contains(&u)
        || (0xFF00..=0xFF60).contains(&u)
        || (0xFFE0..=0xFFE6).contains(&u)
        || (0x1F300..=0x1F64F).contains(&u)
        || (0x1F900..=0x1F9FF).contains(&u)
        || (0x20000..=0x3FFFD).contains(&u);
    if wide { 2 } else { 1 }
}

/// The layer's policy, tested natively — see `tests.rs`.
#[cfg(test)]
mod tests;

/// The pane hook — `pane_<name>`, the name a head opens by (`irc/nicks`).
#[unsafe(no_mangle)]
pub extern "C" fn pane_nicks(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, answer)
}
