//! **The queue, as a built-in plugin** — what the merge queue is DOING, then what it has finished.
//!
//! The operator called the pane this replaces *"illegible mess"* and *"shitty"*, twice, and the
//! screenshot behind it showed why: two rows the queue was working on and **twelve it had finished
//! with**, interleaved, the past tense burying the present one. The pane exists to answer *what is
//! the queue doing*, and it was answering *what has it done*.
//!
//! So this layer draws two things and never mixes them:
//!
//! * **`live`** — the rows the queue is working on now (`waiting`, `reviewing`, `blocked`, `queued`,
//!   `gating`), one line each, with the reason it is where it is. They fill the frame.
//! * **`finished`** — everything else, behind ONE folded line that says how many there are. It is
//!   `act = ["fold"]` on a row the host's `folded` map keeps, so opening it costs no call and
//!   survives the next answer. A `RED TARGET` row that was withdrawn or superseded is history and
//!   lives in here, not in the alarm above it: the alarm is the TARGET's health, which is its own
//!   line.
//!
//! # The row says one thing, and it is the verb that would DO something
//!
//! The host pane this replaces printed every row's four verbs as three wrapped lines of prose, and
//! the key bar printed the same four keys in different words (`d drop` against `d rm`). Both are
//! gone. A row carries:
//!
//! * the state's mark and word, in the state's role — `[~] reviewing`;
//! * what the row is about — the branch, or `RED TARGET main`;
//! * how long it has been in that state;
//! * **the one phrase a person can act on**: `approve`, or `restart (it is reviewing)` when the
//!   verb you would reach for would do nothing. The feed carries the answer (`verbs`, with the
//!   daemon's own reason for each that would not move the row), so this layer does not re-derive
//!   `mergequeue::verbs`' rule — a second copy of it is a copy that drifts.
//!
//! The general meaning of every verb is in the legend (`l` on a plugin pane, and the hover), not
//! repeated on every row.
//!
//! # The keys
//!
//! Every live row carries `enter: {"verb": …, "id": …}` — the act that names the entry it is
//! about. That is what lets the person's `a`/`v`/`r`/`d` act on the row they are LOOKING at: before
//! `Act::Verb` existed a plugin row could draw one entry and the keys would act on whichever entry
//! the host's own cursor happened to be over. The verb word is what `plugin.toml`'s `actions`
//! gates, so a row asking for a verb this layer did not declare is refused by the host.
//!
//! # What it reads, and the one thing it cannot
//!
//! One feed, `queue` (`plugin.toml`'s `feeds = ["queue"]`): the snapshot the head already holds,
//! in the daemon's own words, with every age already in milliseconds against the host's clock.
//! **This module has no clock** — `std::time::SystemTime::now()` is a panic on
//! `wasm32-unknown-unknown` and a panic in a guest is a trap, which is the measured rake of this
//! slice (`.letibot/notes/the-pane-contracts-two-look-keys.md`) — so the feed hands it `now_ms`
//! and every age is a subtraction.

use letibot_guest_abi as abi;
use letibot_plugin::pane::{Act, Cell, Fold, Input, Role};
use serde::Deserialize;
use serde_json::{Value, json};

/// How many finished rows one folded group lists before it says *and N more*. The history is the
/// record and the store never deletes a row, so a project that has landed for a week has thousands;
/// a pane that drew them all would be the pane this replaces.
const HISTORY_MAX: usize = 200;

/// **The feed, as this layer reads it.**
#[derive(Debug, Default, Deserialize)]
struct Feed {
    #[serde(default)]
    scope: Scope,
    /// `review` or `merge` — which half the person's `tab` is on. The rows of that queue are drawn
    /// first, so the pane agrees with the strip the person pressed.
    #[serde(default)]
    view: String,
    #[serde(default)]
    unreadable: Option<String>,
    #[serde(default)]
    archived: usize,
    #[serde(default)]
    not_queued: Vec<String>,
    #[serde(default)]
    reviews: Vec<Review>,
    #[serde(default)]
    merges: Vec<Merge>,
    #[serde(default)]
    targets: Vec<Target>,
    #[serde(default)]
    leases: Vec<Lease>,
    #[serde(default)]
    counts: Counts,
    #[serde(default)]
    cursor: String,
}

#[derive(Debug, Default, Deserialize)]
struct Scope {
    #[serde(default)]
    words: String,
}

#[derive(Debug, Default, Deserialize)]
struct Counts {
    #[serde(default)]
    finished: usize,
}

/// One verb, and why it would not move the row (`""` when it would).
#[derive(Debug, Default, Deserialize)]
struct VerbWhy {
    #[serde(default)]
    verb: String,
    #[serde(default)]
    why: String,
}

#[derive(Debug, Default, Deserialize)]
struct Review {
    #[serde(default)]
    id: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    live: bool,
    #[serde(default)]
    state: String,
    #[serde(default)]
    state_words: String,
    #[serde(default)]
    mark: String,
    #[serde(default)]
    in_state_ms: u64,
    #[serde(default)]
    repository: String,
    #[serde(default)]
    target: String,
    #[serde(default)]
    branch: String,
    #[serde(default)]
    owner: String,
    #[serde(default)]
    owner_is_serving: bool,
    #[serde(default)]
    short_sha: String,
    #[serde(default)]
    verdict: Option<String>,
    #[serde(default)]
    reasons: Vec<String>,
    #[serde(default)]
    evidence: String,
    #[serde(default)]
    fixes: Option<String>,
    #[serde(default)]
    verbs: Vec<VerbWhy>,
}

#[derive(Debug, Default, Deserialize)]
struct Merge {
    #[serde(default)]
    id: String,
    #[serde(default)]
    live: bool,
    #[serde(default)]
    state: String,
    #[serde(default)]
    state_words: String,
    #[serde(default)]
    mark: String,
    #[serde(default)]
    in_state_ms: u64,
    #[serde(default)]
    repository: String,
    #[serde(default)]
    target: String,
    #[serde(default)]
    branch: String,
    #[serde(default)]
    owner: String,
    #[serde(default)]
    owner_is_serving: bool,
    #[serde(default)]
    short_sha: String,
    #[serde(default)]
    evidence: String,
    #[serde(default)]
    fixes: Option<String>,
    #[serde(default)]
    waits_on: Option<String>,
    #[serde(default)]
    verbs: Vec<VerbWhy>,
}

#[derive(Debug, Default, Deserialize)]
struct Target {
    #[serde(default)]
    repository: String,
    #[serde(default)]
    target: String,
    #[serde(default)]
    short_sha: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    state_words: String,
    #[serde(default)]
    since_ms: u64,
    #[serde(default)]
    evidence: String,
}

#[derive(Debug, Default, Deserialize)]
struct Lease {
    #[serde(default)]
    repository: String,
    #[serde(default)]
    owner: String,
    #[serde(default)]
    build: String,
    #[serde(default)]
    heartbeat_ms: u64,
    #[serde(default)]
    stale: bool,
}

/// **One row this layer is about to draw.**
struct Out {
    id: String,
    cells: Vec<Cell>,
    facts: Vec<Cell>,
    under: Vec<String>,
    open: Option<Fold>,
    enter: Option<Act>,
}

/// **The phrase a person can act on**, from the feed's own answer — the verb that would move the
/// row, or the one they would reach for with the reason it would not.
///
/// This is the whole of the operator's second complaint: the pane printed four verbs of prose per
/// row and the bar printed the same four keys in other words. One phrase, and the legend carries
/// the meaning.
fn the_verb(verbs: &[VerbWhy]) -> Option<String> {
    let acts: Vec<&str> = verbs
        .iter()
        .filter(|v| v.why.is_empty() && !v.verb.is_empty())
        .map(|v| v.verb.as_str())
        .collect();
    if !acts.is_empty() {
        // The first that would do something is the one to name. Two of them (`approve` and `veto`
        // on a `reviewing` row) are both real choices, so both are said — separated by the pane's
        // own `·`, not by a sentence.
        return Some(acts.join(" · "));
    }
    // **Nothing would move it** — say the verb they would reach for and why it would not. `restart`
    // is the one a stuck row is stuck ON (the operator: *"so merge queue has 4 failed items, we
    // need a way to restart them"*), so its reason is the useful one.
    let restart = verbs.iter().find(|v| v.verb == "restart" && !v.why.is_empty());
    let any = restart.or_else(|| verbs.iter().find(|v| !v.why.is_empty()));
    any.map(|v| format!("{} ({})", v.verb, v.why))
}

/// **`4m`** — an age, from the milliseconds the feed carries. The guest has no clock, so this is
/// arithmetic and nothing else.
fn age(ms: u64) -> String {
    if ms < 60_000 {
        format!("{}s", ms / 1000)
    } else if ms < 3_600_000 {
        format!("{}m", ms / 60_000)
    } else {
        format!("{}h", ms / 3_600_000)
    }
}

/// The first line of a text, with control characters flattened — a reason is somebody's prose and
/// an escape in it would be measured one way and drawn another.
fn one_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// **What the row is about** — the branch, or the target a red-tip review is of. A target's own
/// review is not a branch and never reads like one.
fn what_of(kind: &str, branch: &str, target: &str) -> String {
    if kind == "target" {
        format!("RED TARGET {target}")
    } else {
        branch.to_string()
    }
}

/// **The reason the row is where it is**, in the order a person reads it: a fix the queue asked
/// for, then the words it was given, then the state's own verdict.
fn reason_of(
    fixes: Option<&str>,
    verdict: Option<&str>,
    reasons: &[String],
    evidence: &str,
) -> String {
    if let Some(f) = fixes.filter(|f| !f.trim().is_empty()) {
        return format!("FIX for the red target — {}", one_line(f));
    }
    if let Some(r) = reasons.iter().find(|r| !r.trim().is_empty()) {
        return one_line(r);
    }
    if let Some(v) = verdict.filter(|v| !v.trim().is_empty()) {
        return format!("verdict {v}");
    }
    one_line(evidence)
}

/// **One live review, one line.**
fn review_row(r: &Review, show_repo: bool, picked: bool) -> Out {
    let mut facts: Vec<Cell> = Vec::new();
    // **The owner only when the claim is not the serving daemon's** — the pane's own rule, and the
    // reason a row is one line: the serving daemon's name on every row says nothing.
    if !r.owner.is_empty() && !r.owner_is_serving {
        facts.push(Cell::role(r.owner.clone(), Role::Faint));
    }
    facts.push(Cell::role(age(r.in_state_ms), Role::Faint));
    if show_repo {
        facts.push(Cell::role(r.repository.clone(), Role::Faint));
    }
    if let Some(v) = the_verb(&r.verbs) {
        facts.push(Cell::role(v, Role::Pending));
    }
    let mut under = vec![reason_of(
        r.fixes.as_deref(),
        r.verdict.as_deref(),
        &r.reasons,
        &r.evidence,
    )];
    // **The address is only under the cursor.** The id exists so it can be typed into
    // `/queue approve`; repeating it on every row was half of what made the pane illegible.
    if picked {
        under.push(format!("{} · head {}", r.id, r.short_sha));
    }
    Out {
        id: r.id.clone(),
        cells: vec![
            Cell::role(r.mark.clone(), role_of(&r.state)),
            Cell::role(what_of(&r.kind, &r.branch, &r.target), Role::Strong),
            Cell::role(r.state_words.clone(), role_of(&r.state)),
        ],
        facts,
        under,
        open: None,
        // **The act that names this entry** — `a`/`v`/`r`/`d` act on it, whatever the host's own
        // cursor is over. The verb word is what the manifest gates.
        enter: Some(Act::verb_on(primary_verb(&r.verbs), &r.id)),
    }
}

/// **One live landing, one line.**
fn merge_row(m: &Merge, show_repo: bool, picked: bool) -> Out {
    let mut facts: Vec<Cell> = Vec::new();
    if !m.owner.is_empty() && !m.owner_is_serving {
        facts.push(Cell::role(m.owner.clone(), Role::Faint));
    }
    facts.push(Cell::role(age(m.in_state_ms), Role::Faint));
    if show_repo {
        facts.push(Cell::role(m.repository.clone(), Role::Faint));
    }
    if let Some(v) = the_verb(&m.verbs) {
        facts.push(Cell::role(v, Role::Pending));
    }
    let mut under = vec![match &m.waits_on {
        // **What it is waiting for**, in the pane's own words (`gate running on 1f2a3b4 for 2m`).
        Some(w) => format!("waits: {w}"),
        None => reason_of(m.fixes.as_deref(), None, &[], &m.evidence),
    }];
    if picked {
        under.push(format!("{} · approved {}", m.id, m.short_sha));
    }
    Out {
        id: m.id.clone(),
        cells: vec![
            Cell::role(m.mark.clone(), role_of(&m.state)),
            Cell::role(m.branch.clone(), Role::Strong),
            Cell::role(format!("onto {}", m.target), Role::Faint),
            Cell::role(m.state_words.clone(), role_of(&m.state)),
        ],
        facts,
        under,
        open: None,
        enter: Some(Act::verb_on(primary_verb(&m.verbs), &m.id)),
    }
}

/// **The verb a row leads with** — the first that would move it, else the one a stuck row is stuck
/// on. The act's `verb` is what the manifest gates; the keys the person presses are their own.
fn primary_verb(verbs: &[VerbWhy]) -> String {
    verbs
        .iter()
        .find(|v| v.why.is_empty() && !v.verb.is_empty())
        .map(|v| v.verb.clone())
        .or_else(|| {
            verbs
                .iter()
                .find(|v| v.verb == "restart")
                .map(|v| v.verb.clone())
        })
        .or_else(|| verbs.first().map(|v| v.verb.clone()))
        .unwrap_or_else(|| "approve".to_string())
}

/// The palette's role for a state — the same reading the host pane's marks use, so a `blocked` row
/// is the same red in both panes.
fn role_of(state: &str) -> Role {
    match state {
        "waiting" | "queued" => Role::Pending,
        "reviewing" | "gating" => Role::Working,
        "approved" | "landed" => Role::Success,
        "blocked" | "failed" | "timed_out" | "conflict" => Role::Failure,
        "rejected" | "cannot_decide" => Role::Attention,
        _ => Role::Faint,
    }
}

/// **The history, folded away behind one line** — the count, and whether it is open.
///
/// `act = ["fold"]` and the host's `folded` map are the mechanism: the fold is instant, it survives
/// the next answer, and a pane nobody has opened asks for nothing.
fn history_row(feed: &Feed, folded: bool) -> Out {
    let n = feed.counts.finished;
    let shown = feed
        .reviews
        .iter()
        .filter(|r| !r.live)
        .count()
        + feed.merges.iter().filter(|m| !m.live).count();
    Out {
        id: "finished".to_string(),
        cells: vec![
            Cell::role(if folded { "[-]" } else { "[+]" }, Role::Faint),
            Cell::role(
                format!(
                    "finished · {n}{}",
                    if shown < n {
                        format!(" (the last {shown})")
                    } else {
                        String::new()
                    }
                ),
                Role::Faint,
            ),
            Cell::role(
                if folded {
                    "enter folds it away"
                } else {
                    "enter opens it"
                },
                Role::Faint,
            ),
        ],
        facts: Vec::new(),
        under: Vec::new(),
        open: Some(if folded { Fold::Open } else { Fold::Closed }),
        enter: Some(Act::fold()),
    }
}

/// **The chrome above the rows**: the emergencies, whose queues these are, who serves them.
///
/// One `lines` block, so it is a few rows and never a paragraph — the pane this replaces put the
/// lease, the archive and the "not queued" list above the rows as well, and that is the shape the
/// operator asked for; what changes is that the rows below it are now only the live ones.
fn chrome(feed: &Feed) -> Vec<Value> {
    let mut lines: Vec<Value> = Vec::new();
    if let Some(why) = &feed.unreadable {
        lines.push(json!({"label": "the store", "value": why, "role": "failure"}));
    }
    for t in &feed.targets {
        match t.state.as_str() {
            "red" => lines.push(json!({
                "label": format!("RED {} {}", t.target, t.repository),
                "value": format!("{} at {}", one_line(&t.evidence), t.short_sha),
                "tail": age(t.since_ms),
                "role": "failure",
            })),
            "timed_out" | "blocked" => lines.push(json!({
                "label": format!("{} {} {}", t.target, t.state_words, t.repository),
                "value": one_line(&t.evidence),
                "tail": age(t.since_ms),
                "role": "attention",
            })),
            // A green target is not news, and a pane that says so on every frame spends a row on
            // the absence of a problem.
            _ => {}
        }
    }
    for l in feed.leases.iter().filter(|l| l.stale) {
        lines.push(json!({
            "label": format!("STALE {}", l.repository),
            "value": format!("{} ({}) has not said anything", l.owner, l.build),
            "tail": age(l.heartbeat_ms),
            "role": "attention",
        }));
    }
    if !feed.not_queued.is_empty() {
        lines.push(json!({
            "label": "not queued",
            "value": format!(
                "{} — no `Merge gate` section in its AGENTS.md",
                feed.not_queued.join(", ")
            ),
            "role": "faint",
        }));
    }
    if feed.archived > 0 {
        lines.push(json!({
            "label": "archived",
            "value": format!("{} row(s) of the pre-split queue (merge_queue_v23)", feed.archived),
            "role": "faint",
        }));
    }
    lines
}

/// The pane's answer, or `None` for *unchanged*.
fn answer(bytes: &[u8]) -> Option<String> {
    let input: Input = serde_json::from_slice(bytes).ok()?;
    let feed: Feed = serde_json::from_value(input.feeds.get("queue")?.clone()).ok()?;
    let folded = input.folded.get("finished").copied().unwrap_or(false);
    // **One repository in scope, or several?** A row names its repository only when there is more
    // than one to tell apart — the pane's own rule, and the other half of why its rows were wide.
    let show_repo = feed.scope.words.contains(", ") || feed.scope.words.contains("every repository");

    let mut rows: Vec<Value> = Vec::new();
    // **The queue the person is looking at goes first.** `tab` is the head's, and a plugin that
    // ignored it would be a pane whose strip said `review` over the landings.
    let reviews_first = feed.view != "merge";
    let live_review = || feed.reviews.iter().filter(|r| r.live);
    let live_merge = || feed.merges.iter().filter(|m| m.live);
    if reviews_first {
        for r in live_review() {
            let picked = !feed.cursor.is_empty() && feed.cursor == r.id;
            rows.push(row_value(&review_row(r, show_repo, picked)));
        }
    }
    for m in live_merge() {
        let picked = !feed.cursor.is_empty() && feed.cursor == m.id;
        rows.push(row_value(&merge_row(m, show_repo, picked)));
    }
    if !reviews_first {
        for r in live_review() {
            let picked = !feed.cursor.is_empty() && feed.cursor == r.id;
            rows.push(row_value(&review_row(r, show_repo, picked)));
        }
    }
    let live = rows.len();
    // **The history, only when it is open.** The folded row is always drawn; its rows are not,
    // which is what keeps twelve finished landings from burying two live ones.
    if folded {
        let mut done: Vec<Value> = Vec::new();
        for r in feed.reviews.iter().filter(|r| !r.live).take(HISTORY_MAX) {
            let picked = !feed.cursor.is_empty() && feed.cursor == r.id;
            done.push(row_value(&review_row(r, show_repo, picked)));
        }
        for m in feed.merges.iter().filter(|m| !m.live).take(HISTORY_MAX) {
            let picked = !feed.cursor.is_empty() && feed.cursor == m.id;
            done.push(row_value(&merge_row(m, show_repo, picked)));
        }
        rows.extend(done);
    }

    let mut panels: Vec<Value> = Vec::new();
    let lines = chrome(&feed);
    if !lines.is_empty() {
        panels.push(json!({
            "id": "queue",
            "title": feed.scope.words,
            "summary": [{"lines": lines}],
        }));
    }
    panels.push(json!({
        "id": "rows",
        "title": if live == 0 {
            "the queue is idle".to_string()
        } else {
            format!("live · {live}")
        },
        "summary": [{"tree": {
            "rows": rows,
            "empty": "nothing is in flight — a branch arrives when a `task_start` child finishes",
        }}],
    }));
    if feed.counts.finished > 0 {
        panels.push(json!({
            "id": "history",
            "title": format!("finished · {}", feed.counts.finished),
            "summary": [{"tree": {"rows": [row_value(&history_row(&feed, folded))]}}],
        }));
    }
    // **The one bar that is not a row's.** A row says the verb that would act on IT; the legend
    // says what each verb means in general, which is where the prose belongs.
    let view = json!({
        "title": "the queue",
        "panels": panels,
        "legend": {
            "approve": "your approval over the reviewer, at the branch's head now — the gate still runs",
            "veto": "rejected in your words, sent back to the child",
            "restart": "back in line: reviewed, or gated, again",
            "rm": "withdrawn — the branch and its worktree stay, the history keeps the row",
            "RED TARGET": "the target branch's own tip is red: nothing lands but its fix",
            "STALE": "the daemon serving this queue has stopped saying so — another takes it over",
        },
    });
    serde_json::to_string(&view).ok()
}

/// One row as C3 spells it.
fn row_value(row: &Out) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("id".into(), json!(row.id));
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
pub extern "C" fn pane_queue(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, answer)
}
