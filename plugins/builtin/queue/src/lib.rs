//! **The queue, as a built-in plugin** - what the merge queue is DOING, then what it has finished.
//!
//! This is the queue's one renderer: the head hosts this layer's answer and draws nothing of its
//! own. The layer draws two things and never mixes them:
//!
//! * **`live`** - the rows the queue is working on now (`waiting`, `reviewing`, `blocked`, `queued`,
//!   `gating`), one line each, with the reason it is where it is. They fill the frame.
//! * **`finished`** - everything else, behind ONE folded line that says how many there are. It is
//!   `act = ["fold"]` on a row the host's `folded` map keeps, so opening it costs no call and
//!   survives the next answer. A `RED TARGET` row that was withdrawn or superseded is history and
//!   lives in here, not in the alarm above it: the alarm is the TARGET's health, which is its own
//!   line.
//!
//! # The row says one thing, and it is the verb that would DO something
//!
//! A row carries:
//!
//! * the state's mark and word, in the state's role - `[~] reviewing`;
//! * what the row is about - the branch, or `RED TARGET main`;
//! * how long it has been in that state;
//! * **the one phrase a person can act on**: `approve`, or `restart (it is reviewing)` when the
//!   verb you would reach for would do nothing. The feed carries the answer (`verbs`, with the
//!   daemon's own reason for each that would not move the row), so this layer does not re-derive
//!   `mergequeue::verbs`' rule - a second copy of it is a copy that drifts.
//!
//! The general meaning of every verb is in the legend (`l` on a plugin pane, and the hover), not
//! repeated on every row.
//!
//! # The row read whole
//!
//! Every row carries `detail`: the whole entry as prose - every field, the gate's steps with
//! their output, what a running gate has written and whether it has moved, and the row's moves.
//! The host's Enter reads it (C3: `detail` is prose read whole), so the overlay belongs to this
//! layer as much as the one-line rows do. The row whose detail is being read arrives in the feed
//! as `open`, and is drawn even when it has left the live list, so an overlay outlives the row
//! landing or being withdrawn under it.
//!
//! # The keys
//!
//! Every live row carries `enter: {"verb": …, "id": …}` - the act that names the entry it is
//! about. That is what lets the person's `a`/`v`/`r`/`d` act on the row they are LOOKING at: a
//! plugin row can draw one entry and the keys would otherwise act on whichever entry the host's
//! own cursor happened to be over. The verb word is what `plugin.toml`'s `actions` gates, so a
//! row asking for a verb this layer did not declare is refused by the host.
//!
//! # What it reads, and the one thing it cannot
//!
//! One feed, `queue` (`plugin.toml`'s `feeds = ["queue"]`): the snapshot the head already holds,
//! in the daemon's own words, with every age already in milliseconds against the host's clock.
//! **This module has no clock** - `std::time::SystemTime::now()` is a panic on
//! `wasm32-unknown-unknown` and a panic in a guest is a trap - so the feed hands it `now_ms`
//! and every age is a subtraction. Whether a gate's tail has MOVED is the host's to say
//! (`still_ms`), because only a reader of two snapshots can know it.

use letibot_guest_abi as abi;
use letibot_plugin::pane::{Act, Cell, Fold, Input, Role};
use serde::Deserialize;
use serde_json::{Value, json};

/// How many finished rows one folded group lists before it says *and N more*. The history is the
/// record and the store never deletes a row, so a project that has landed for a week has thousands;
/// a pane that drew them all would be the pane this design replaced.
const HISTORY_MAX: usize = 200;

/// How long a running step's output may stand still before the detail calls it stuck. A build
/// between two crates is minutes of silence; ten of them with no byte written is going nowhere.
const GATE_QUIET_MS: u64 = 10 * 60 * 1000;

/// How old the snapshot may be before the detail stops calling a quiet tail stuck. Past this the
/// head is reading its own clock and not the daemon's, and a verdict would be invented.
const GATE_SNAPSHOT_STALE_MS: u64 = 30_000;

/// **The feed, as this layer reads it.**
#[derive(Debug, Default, Deserialize)]
struct Feed {
    #[serde(default)]
    now_ms: u64,
    #[serde(default)]
    at_ms: u64,
    #[serde(default)]
    scope: Scope,
    /// `review` or `merge` - which half the person's `tab` is on. The rows of that queue are drawn
    /// first, so the pane agrees with the strip the person pressed.
    #[serde(default)]
    view: String,
    /// The entry whose detail the host is reading, if any. It is drawn even when it is history,
    /// because an overlay must not vanish under the row it is reading.
    #[serde(default)]
    open: String,
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
    recent: Vec<Move>,
    #[serde(default)]
    counts: Counts,
    #[serde(default)]
    cursor: String,
}

#[derive(Debug, Default, Deserialize)]
struct Scope {
    #[serde(default)]
    words: String,
    /// **Whether a row has to name its repository** - the host's answer to *is there more than one
    /// in scope to tell apart?* It is a field and not a reading of `words` because the words are
    /// prose: a path scope's contain no separator and a one-repository project's contain one, so
    /// grepping them names the repository when it is redundant and omits it when it is the only
    /// thing that says where a row is from.
    #[serde(default)]
    several: bool,
}

#[derive(Debug, Default, Deserialize)]
struct Counts {
    #[serde(default)]
    review: usize,
    #[serde(default)]
    merge: usize,
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
    reviewer: Option<String>,
    #[serde(default)]
    head_sha: String,
    #[serde(default)]
    short_sha: String,
    #[serde(default)]
    reviewed_sha: Option<String>,
    #[serde(default)]
    verdict: Option<String>,
    #[serde(default)]
    verdict_by: Option<String>,
    #[serde(default)]
    attempts: usize,
    #[serde(default)]
    failure: String,
    #[serde(default)]
    reasons: Vec<String>,
    #[serde(default)]
    evidence: String,
    #[serde(default)]
    brief: String,
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
    repository_path: String,
    #[serde(default)]
    target: String,
    #[serde(default)]
    branch: String,
    #[serde(default)]
    owner: String,
    #[serde(default)]
    owner_is_serving: bool,
    #[serde(default)]
    approved_sha: String,
    #[serde(default)]
    short_sha: String,
    #[serde(default)]
    approved_by: String,
    #[serde(default)]
    review_id: String,
    #[serde(default)]
    rebased_sha: Option<String>,
    #[serde(default)]
    landed_sha: Option<String>,
    #[serde(default)]
    pushed: String,
    #[serde(default)]
    evidence: String,
    #[serde(default)]
    fixes: Option<String>,
    #[serde(default)]
    waits_on: Option<String>,
    /// **The step its gate is running now** - `Some` only while the row is `gating`.
    #[serde(default)]
    running_gate: Option<RunningGate>,
    #[serde(default)]
    gate_steps: Vec<Step>,
    #[serde(default)]
    verbs: Vec<VerbWhy>,
}

/// The step a gate is running, as the feed carries it: which of how many, since when, how much it
/// has written, and whether its output has moved. Whether it moved is the host's fact: it takes
/// two snapshots to know, and the guest sees only the one it is handed.
#[derive(Debug, Default, Deserialize)]
struct RunningGate {
    #[serde(default)]
    command: String,
    #[serde(default)]
    step: usize,
    #[serde(default)]
    of: usize,
    #[serde(default)]
    running_ms: u64,
    #[serde(default)]
    bytes: u64,
    #[serde(default)]
    tail: String,
    #[serde(default)]
    still_ms: u64,
}

#[derive(Debug, Default, Deserialize)]
struct Step {
    #[serde(default)]
    command: String,
    #[serde(default)]
    outcome: String,
    #[serde(default)]
    elapsed_ms: u64,
    #[serde(default)]
    output: String,
}

#[derive(Debug, Default, Deserialize)]
struct Target {
    #[serde(default)]
    repository: String,
    #[serde(default)]
    repository_path: String,
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
    #[serde(default)]
    running_gate: Option<RunningGate>,
}

#[derive(Debug, Default, Deserialize)]
struct Lease {
    #[serde(default)]
    repository: String,
    /// **Who holds it**, as the pane names the holder: `lab (pid 4242, harnessd 0.3.0)`. The
    /// claim's opaque owner string is in the feed too and this layer does not read it - the two
    /// halves a person recognises are the host and the pid.
    #[serde(default)]
    pid: u64,
    #[serde(default)]
    host: String,
    #[serde(default)]
    build: String,
    /// How long since the holder last said anything - an age, not a stamp: the host's clock is the
    /// only one there is, and a guest that read its own would be a trap.
    #[serde(default)]
    heartbeat_ms: u64,
    #[serde(default)]
    stale: bool,
}

/// One move of one row, as the recent history carries it.
#[derive(Debug, Default, Deserialize)]
struct Move {
    #[serde(default)]
    entry_id: String,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: String,
    #[serde(default)]
    ago_ms: u64,
    #[serde(default)]
    by: String,
    #[serde(default)]
    why: String,
}

/// **One row this layer is about to draw.**
struct Out {
    id: String,
    cells: Vec<Cell>,
    facts: Vec<Cell>,
    under: Vec<String>,
    open: Option<Fold>,
    enter: Option<Act>,
    detail: String,
}

/// **The phrase a person can act on**, from the feed's own answer - the verb that would move the
/// row, or the one they would reach for with the reason it would not.
///
/// One phrase per row is the whole design: four verbs of prose per row was what made the pane this
/// replaced illegible. One phrase, and the legend carries the meaning.
fn the_verb(verbs: &[VerbWhy]) -> Option<String> {
    let acts: Vec<&str> = verbs
        .iter()
        .filter(|v| v.why.is_empty() && !v.verb.is_empty())
        .map(|v| v.verb.as_str())
        .collect();
    if !acts.is_empty() {
        // The first that would do something is the one to name. Two of them (`approve` and `veto`
        // on a `reviewing` row) are both real choices, so both are said.
        return Some(acts.join(" · "));
    }
    // **Nothing would move it** - say the verb they would reach for and why it would not. `restart`
    // is the one a stuck row is stuck ON, so its reason is the useful one.
    let restart = verbs
        .iter()
        .find(|v| v.verb == "restart" && !v.why.is_empty());
    let any = restart.or_else(|| verbs.iter().find(|v| !v.why.is_empty()));
    any.map(|v| format!("{} ({})", v.verb, v.why))
}

/// **`4m`** - an age, from the milliseconds the feed carries. The guest has no clock, so this is
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

/// The first line of a text, with control characters flattened - a reason is somebody's prose and
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

/// **What the row is about** - the branch, or the target a red-tip review is of. A target's own
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
        return format!("FIX for the red target: {}", one_line(f));
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
fn review_row(r: &Review, feed: &Feed, show_repo: bool, picked: bool) -> Out {
    let mut facts: Vec<Cell> = Vec::new();
    // **The owner only when the claim is not the serving daemon's** - the serving daemon's name on
    // every row says nothing.
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
        under.extend(moves_under(feed, &r.id, 3));
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
        // **The act that names this entry** - `a`/`v`/`r`/`d` act on it, whatever the host's own
        // cursor is over. The verb word is what the manifest gates.
        enter: Some(Act::verb_on(primary_verb(&r.verbs), &r.id)),
        detail: review_detail(r, feed),
    }
}

/// **One live landing, one line.**
fn merge_row(m: &Merge, feed: &Feed, show_repo: bool, picked: bool) -> Out {
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
        // **Or what its own gate is doing**, when the row is the one gating.
        None => match &m.running_gate {
            Some(g) => format!(
                "gate step {} of {} · running {} · {} byte(s) written",
                g.step.max(1),
                g.of.max(1),
                age(g.running_ms),
                g.bytes
            ),
            // A gating landing between two steps still says the gate is on it: the sha it was
            // rebased to and how long the state has stood.
            None if m.state == "gating"
                && m.rebased_sha.as_deref().is_some_and(|s| !s.is_empty()) =>
            {
                let short: String = m
                    .rebased_sha
                    .as_deref()
                    .unwrap_or_default()
                    .chars()
                    .take(10)
                    .collect();
                format!("gate running on {} for {}", short, minutes(m.in_state_ms))
            }
            None => reason_of(m.fixes.as_deref(), None, &[], &m.evidence),
        },
    }];
    if picked {
        under.push(format!("{} · approved {}", m.id, m.short_sha));
        under.extend(moves_under(feed, &m.id, 3));
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
        detail: merge_detail(m, feed),
    }
}

/// The last `n` moves of one entry, as the faint lines under a picked row - the recent history a
/// person reads to know the row has not always been here.
fn moves_under(feed: &Feed, id: &str, n: usize) -> Vec<String> {
    let mine: Vec<&Move> = feed.recent.iter().filter(|m| m.entry_id == id).collect();
    let from = mine.len().saturating_sub(n);
    mine[from..].iter().map(|m| move_line(m)).collect()
}

/// One move as one line.
fn move_line(m: &Move) -> String {
    format!(
        "{} → {} · {} ago · {}: {}",
        m.from.as_deref().unwrap_or("(new)"),
        m.to,
        age(m.ago_ms),
        m.by,
        one_line(&m.why)
    )
}

/// **The verb a row leads with** - the first that would move it, else the one a stuck row is stuck
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

/// The palette's role for a state.
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

/// **The history, folded away behind one line** - the count, and whether it is open.
///
/// `act = ["fold"]` and the host's `folded` map are the mechanism: the fold is instant, it survives
/// the next answer, and a pane nobody has opened asks for nothing.
fn history_row(feed: &Feed, folded: bool) -> Out {
    let n = feed.counts.finished;
    let shown = feed.reviews.iter().filter(|r| !r.live).count()
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
        detail: String::new(),
    }
}

/// **The chrome above the rows**: the emergencies, the targets' health, whose queues these are,
/// and who serves them.
///
/// One `lines` block, so it is a few rows and never a paragraph. Every target state is drawn: red
/// and unjudged as alarms, gating as the fact that a gate is on it, green as the quiet line that
/// says where the target stands.
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
            // A gate on the target tip is what every queued landing of that repository waits for,
            // so it is said here even though no row of either queue carries it.
            "gating" => lines.push(json!({
                "label": format!("{} {}", t.target, t.repository),
                "value": format!(
                    "gate running on {} for {}",
                    t.short_sha,
                    minutes(t.since_ms)
                ),
                "role": "pending",
            })),
            // Green is not an alarm, but it is where the target stands: a pane that never said it
            // left the reader to infer health from the absence of a line.
            "green" => lines.push(json!({
                "label": format!("{} {}", t.target, t.repository),
                "value": format!("green at {}", t.short_sha),
                "role": "faint",
            })),
            _ => {}
        }
    }
    // ---- Who serves each repository, and how alive it is. **Every lease, healthy or not**: a
    // pane that only spoke when one was stale left a person unable to tell *nobody is serving
    // this* from *it is being served and nobody said so*.
    for l in &feed.leases {
        let who = format!("{} (pid {}, {})", l.host, l.pid, l.build);
        if l.stale {
            lines.push(json!({
                "label": format!("STALE {}", l.repository),
                "value": format!("{who} has not said anything"),
                "tail": age(l.heartbeat_ms),
                "role": "attention",
            }));
        } else {
            lines.push(json!({
                "label": l.repository,
                "value": format!("served by {who} · heartbeat"),
                "tail": age(l.heartbeat_ms),
                "role": "faint",
            }));
        }
    }
    if feed.leases.is_empty() {
        lines.push(json!({
            "label": "no lease",
            "value": "no daemon holds a queue lease - nothing is serving the queues",
            "role": "attention",
        }));
    }
    if !feed.not_queued.is_empty() {
        lines.push(json!({
            "label": "not queued",
            "value": format!(
                "{} - no `Merge gate` section in its AGENTS.md",
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

/// Whole minutes, for a gate that has run a while - the resolution the words claim.
fn minutes(ms: u64) -> String {
    format!("{}m", ms / 60_000)
}

/// The empty-list sentence of the view the person is on. The two queues arrive differently, and
/// the sentence is the one thing that says which half is empty.
fn empty_of(view: &str) -> &'static str {
    if view == "merge" {
        "nothing is in flight - a branch lands here once its review approves it, at the head \
         that was reviewed"
    } else {
        "nothing is in flight - a branch is put up for review when a `task_start` child finishes"
    }
}

/// The pane's answer, or `None` for *unchanged*.
fn answer(bytes: &[u8]) -> Option<String> {
    let input: Input = serde_json::from_slice(bytes).ok()?;
    let feed: Feed = serde_json::from_value(input.feeds.get("queue")?.clone()).ok()?;
    let folded = input.folded.get("finished").copied().unwrap_or(false);
    // **One repository in scope, or several?** A row names its repository only when there is more
    // than one to tell apart. The answer is the host's (`scope.several`), because the host is what
    // resolved the scope.
    let show_repo = feed.scope.several;

    let mut rows: Vec<Value> = Vec::new();
    // **The queue the person is looking at goes first.** `tab` is the head's, and a pane whose
    // header did not say which half was drawn would be a pane the strip lied about.
    let reviews_first = feed.view != "merge";
    let live_review = || feed.reviews.iter().filter(|r| r.live);
    let live_merge = || feed.merges.iter().filter(|m| m.live);
    let picked = |id: &str| !feed.cursor.is_empty() && feed.cursor == id;
    let push_review = |rows: &mut Vec<Value>, r: &Review| {
        rows.push(row_value(&review_row(r, &feed, show_repo, picked(&r.id))));
    };
    let push_merge = |rows: &mut Vec<Value>, m: &Merge| {
        rows.push(row_value(&merge_row(m, &feed, show_repo, picked(&m.id))));
    };
    if reviews_first {
        for r in live_review() {
            push_review(&mut rows, r);
        }
    }
    for m in live_merge() {
        push_merge(&mut rows, m);
    }
    if !reviews_first {
        for r in live_review() {
            push_review(&mut rows, r);
        }
    }
    let live = rows.len();
    // **The entry whose detail is being read is drawn even when it is history**, so the overlay
    // the host holds over the pane outlives the row landing or being withdrawn under it.
    if !feed.open.is_empty() && !folded {
        if let Some(r) = feed.reviews.iter().find(|r| r.id == feed.open && !r.live) {
            push_review(&mut rows, r);
        }
        if let Some(m) = feed.merges.iter().find(|m| m.id == feed.open && !m.live) {
            push_merge(&mut rows, m);
        }
    }
    // **The history, only when it is open.** The folded row is always drawn; its rows are not,
    // which is what keeps twelve finished landings from burying two live ones.
    if folded {
        let mut done: Vec<Value> = Vec::new();
        for r in feed.reviews.iter().filter(|r| !r.live).take(HISTORY_MAX) {
            push_review(&mut done, r);
        }
        for m in feed.merges.iter().filter(|m| !m.live).take(HISTORY_MAX) {
            push_merge(&mut done, m);
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
    // **Both queues' counts, the drawn view first** - the strip that says which half `tab` is on.
    let strip = |first: &str, n: usize, other: &str, m: usize| {
        format!("{first} queue ({n}) · {other} queue ({m})")
    };
    panels.push(json!({
        "id": "rows",
        "title": if live == 0 {
            "the queue is idle".to_string()
        } else if reviews_first {
            strip("review", feed.counts.review, "merge", feed.counts.merge)
        } else {
            strip("merge", feed.counts.merge, "review", feed.counts.review)
        },
        "summary": [{"tree": {
            "rows": rows,
            "empty": empty_of(&feed.view),
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
            "approve": "your approval over the reviewer, at the branch's head now - the gate still runs",
            "veto": "rejected in your words, sent back to the child",
            "restart": "back in line: reviewed, or gated, again",
            "rm": "withdrawn - the branch and its worktree stay, the history keeps the row",
            "RED TARGET": "the target branch's own tip is red: nothing lands but its fix",
            "STALE": "the daemon serving this queue has stopped saying so - another takes it over",
        },
    });
    serde_json::to_string(&view).ok()
}

/// One `label  value` line of a detail, the label padded so the prose reads as fields.
fn field(k: &str, v: &str) -> String {
    if v.is_empty() {
        String::new()
    } else {
        format!("{k:<10} {v}")
    }
}

/// **A review read whole** - every field the feed carries, then its moves.
fn review_detail(r: &Review, feed: &Feed) -> String {
    let mut l: Vec<String> = Vec::new();
    l.push(field("queue", "review"));
    if !r.kind.is_empty() {
        l.push(field("kind", &r.kind));
    }
    l.push(field("repository", &r.repository));
    l.push(field("target", &r.target));
    l.push(field("branch", &r.branch));
    l.push(field(
        "state",
        &format!("{} · {} in it", r.state_words, age(r.in_state_ms)),
    ));
    if !r.owner.is_empty() {
        l.push(field("owner", &r.owner));
    }
    if r.reviewer.as_deref().is_some_and(|s| !s.is_empty()) {
        l.push(field("reviewer", r.reviewer.as_deref().unwrap_or("")));
    }
    l.push(field("head", &r.head_sha));
    if let Some(v) = r.verdict.as_deref().filter(|v| !v.trim().is_empty()) {
        let at: String = r
            .reviewed_sha
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(10).collect())
            .unwrap_or_else(|| "?".to_string());
        let by = r
            .verdict_by
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("?");
        l.push(field("verdict", &format!("{v} at {at} by {by}")));
    }
    for reason in &r.reasons {
        l.push(field("reason", reason));
    }
    // The failure is drawn whether or not an attempt was counted: a row waiting for a reviewer's
    // tree carries the reason with no attempt to its name, and the reason is the whole value.
    if !r.failure.is_empty() {
        l.push(field("failure", &r.failure));
    }
    if r.attempts > 0 {
        l.push(field("attempts", &r.attempts.to_string()));
    }
    if let Some(f) = r.fixes.as_deref().filter(|f| !f.is_empty()) {
        l.push(field("fixes", f));
    }
    l.push(field("why", &r.evidence));
    l.push(field("ask", &r.brief));
    l.push(String::new());
    l.extend(moves_detail(feed, &r.id));
    l.join("\n")
}

/// **A landing read whole** - its fields, the gate it runs or waits on, its steps with their
/// output, and its moves.
fn merge_detail(m: &Merge, feed: &Feed) -> String {
    let mut l: Vec<String> = Vec::new();
    l.push(field("queue", "merge"));
    l.push(field("repository", &m.repository));
    l.push(field("target", &m.target));
    l.push(field("branch", &m.branch));
    l.push(field(
        "state",
        &format!("{} · {} in it", m.state_words, age(m.in_state_ms)),
    ));
    if !m.owner.is_empty() {
        l.push(field("owner", &m.owner));
    }
    l.push(field(
        "approved",
        &format!(
            "{} by {} (review {})",
            m.approved_sha, m.approved_by, m.review_id
        ),
    ));
    if let Some(s) = m.rebased_sha.as_deref().filter(|s| !s.is_empty()) {
        l.push(field("rebased", s));
    }
    if let Some(s) = m.landed_sha.as_deref().filter(|s| !s.is_empty()) {
        l.push(field("landed", s));
    }
    if !m.pushed.is_empty() {
        l.push(field("push", &m.pushed));
    }
    l.push(field("why", &m.evidence));
    // The gate in flight is this row's when it is `gating`, and its TARGET's when it is queued:
    // the other way a queue sits still with nothing failing.
    let gate = m.running_gate.as_ref().or_else(|| target_gate_of(feed, m));
    if let Some(g) = gate {
        l.push(String::new());
        l.extend(gate_detail(g, m.running_gate.is_none(), feed));
    }
    l.push(String::new());
    l.push("the gate's steps".to_string());
    if m.gate_steps.is_empty() {
        l.push(if m.state == "gating" {
            "the gate is running".to_string()
        } else {
            "the gate has not run on this row".to_string()
        });
    }
    for s in &m.gate_steps {
        l.push(step_line(s));
        // The whole output of a step that failed is the evidence a person came for.
        if matches!(s.outcome.as_str(), "failed" | "timed out") && !s.output.is_empty() {
            for line in s.output.lines() {
                l.push(format!("  {}", one_line(line)));
            }
        }
    }
    l.push(String::new());
    l.extend(moves_detail(feed, &m.id));
    l.join("\n")
}

/// The target's running gate a queued landing of the same repository and target waits on.
fn target_gate_of<'a>(feed: &'a Feed, m: &Merge) -> Option<&'a RunningGate> {
    if m.state != "queued" {
        return None;
    }
    feed.targets
        .iter()
        .find(|t| t.repository_path == m.repository_path && t.target == m.target)
        .and_then(|t| t.running_gate.as_ref())
}

/// **A gate in flight, read whole**: which step of how many, the step itself, whether its output
/// has moved, and the end of what it has written - the answer to *is it stuck?*.
///
/// `waiting` says the gate is the TARGET's rather than the row's own, so the reader knows whose
/// step they are reading. A snapshot older than [`GATE_SNAPSHOT_STALE_MS`] is said as no answer
/// and never turned into a verdict about the gate.
fn gate_detail(g: &RunningGate, waiting: bool, feed: &Feed) -> Vec<String> {
    let head = if waiting {
        format!(
            "waiting on the target's gate: step {} of {} · running {}",
            g.step.max(1),
            g.of.max(1),
            age(g.running_ms)
        )
    } else {
        format!(
            "step {} of {} · running {}",
            g.step.max(1),
            g.of.max(1),
            age(g.running_ms)
        )
    };
    let mut l = vec![field("gate", &head)];
    if !g.command.is_empty() {
        l.push(field("step", &g.command));
    }
    let snapshot_age = feed.now_ms.saturating_sub(feed.at_ms);
    let moving = if snapshot_age > GATE_SNAPSHOT_STALE_MS {
        format!(
            "no snapshot for {} - this is what the daemon said then ({} byte(s) written)",
            age(snapshot_age),
            g.bytes
        )
    } else if g.still_ms >= GATE_QUIET_MS {
        format!(
            "the tail has not moved in {} - it may be stuck ({} byte(s) written)",
            age(g.still_ms),
            g.bytes
        )
    } else if g.still_ms < 1_000 {
        format!("the tail moved just now ({} byte(s) written)", g.bytes)
    } else {
        format!(
            "the tail moved {} ago ({} byte(s) written)",
            age(g.still_ms),
            g.bytes
        )
    };
    l.push(field("moving", &moving));
    if g.tail.is_empty() {
        l.push(field("tail", "nothing written yet"));
    } else {
        l.push(field("tail", "the end of what it has written"));
        for line in g.tail.lines() {
            l.push(format!("  {}", one_line(line)));
        }
    }
    l
}

/// One gate step as one line, its outcome in the words a person reads.
fn step_line(s: &Step) -> String {
    let tail = match s.outcome.as_str() {
        "passed" => format!("· {}", age(s.elapsed_ms)),
        "failed" => format!("· {}", age(s.elapsed_ms)),
        "timed out" => format!("· TIMED OUT after {}", age(s.elapsed_ms)),
        "killed" => "· killed".to_string(),
        _ => "· not run".to_string(),
    };
    format!("{} {} {}", mark_of(&s.outcome), one_line(&s.command), tail)
}

/// The mark of a step's outcome.
fn mark_of(outcome: &str) -> &'static str {
    match outcome {
        "passed" => "✓",
        "failed" => "✗",
        "timed out" => "⧗",
        "killed" => "✗",
        _ => "·",
    }
}

/// All the moves of one entry, oldest first - the row's whole recent history.
fn moves_detail(feed: &Feed, id: &str) -> Vec<String> {
    let mut l = vec!["its moves".to_string()];
    let mine: Vec<&Move> = feed.recent.iter().filter(|m| m.entry_id == id).collect();
    if mine.is_empty() {
        l.push("none in the recent history".to_string());
        return l;
    }
    for m in mine {
        l.push(move_line(m));
    }
    l
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
    if !row.detail.is_empty() {
        out.insert("detail".into(), json!(row.detail));
    }
    Value::Object(out)
}

/// The layer's policy, tested natively - see `tests.rs`.
#[cfg(test)]
mod tests;

/// The pane hook - `pane_<name>`, the name a head opens by.
#[unsafe(no_mangle)]
pub extern "C" fn pane_queue(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, answer)
}
