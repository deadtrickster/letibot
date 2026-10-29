//! **The queue pane as a plugin's pane** — `plugins/builtin/queue`, fed from the snapshot the head
//! already holds.
//!
//! # Why the head runs this layer and the daemon does not
//!
//! Every other plugin pane is answered by the daemon (`crates/harnessd/src/plugins.rs`) and reaches
//! a head as a `PluginPane` frame. This one cannot be: the data the pane draws — the review queue,
//! the landings, the target branches' health, the leases' heartbeats — arrives at the HEAD as one
//! `QueueSnapshot` (`App::queues`), and the head is what cuts it to this session's repository
//! (`/queue all`, `/queue /PATH`). A daemon-side layer would have to be handed the snapshot back,
//! and the snapshot is already here.
//!
//! So the head runs the layer in-process, over the same `Host` the session list uses
//! (`app::sessions_pane::Host`): one `Stack`, so *what is in force* has one answer, and the built-in
//! modules are compiled once.
//!
//! # What the feed is, and why it is words
//!
//! [`App::queue_feed`] is the contract: the daemon's own words, not this head's enum spellings. A
//! plugin author reads `"waiting"`, `"reviewing"`, `"gating"`, `"landed"` — the same strings
//! `mergequeue::verbs` takes and `sessionlog::event::QueueReview` carries — and the gate's steps
//! carry `"timed out"` rather than `MergeGateOutcome`'s serde spelling. The ages are milliseconds
//! *as of the host's clock* (`now_ms`), because a wasm guest cannot read a clock:
//! `std::time::SystemTime::now()` is a panic on `wasm32-unknown-unknown`, and a panic in a guest is
//! a trap (`.letibot/notes/the-pane-contracts-two-look-keys.md`).
//!
//! # The two things a plugin must not have to infer
//!
//! * **`live`** — whether the queue is working on a row or has finished with it. The states that
//!   mean *in flight* are the daemon's (`review_open`/`merge_open` in `ui/panes/queue.rs`), and a
//!   plugin guessing at them from a state word would be a second copy of that rule.
//! * **`verbs`** — which of the person's verbs would MOVE this row, and for each that would not,
//!   why. That is `mergequeue::verbs`' rule, mirrored by `ui/panes/queue.rs::verb_moves`; the feed
//!   carries the answer so a row can say the one verb that would do something instead of printing
//!   four and leaving the person to find out.
//!
//! # And the one thing it carries that a feed usually does not: `cursor`
//!
//! **A pane cannot see the cursor.** C3's input has no field for it, and this pane needs one: the
//! operator's ruling is one line per entry with the address (the id `/queue approve` takes, the sha
//! a person quotes) on the picked row and on no other, so a row has to know whether it is picked.
//! `cursor` is that answer, as the entry id the last frame's row named — so it is **one frame behind
//! on the very first draw** (there is no view yet to read a row from) and exact from then on: a
//! cursor moves, the feed changes, the plugin is asked again. `""` is *no row is picked*.
//!
//! That price is deliberate. The alternative is a second channel — the host telling an already
//! asked pane which row the cursor moved to — and a pane that is asked again on every arrow key is
//! exactly the call-per-keystroke a plugin pane should not be. A next pane that needs the cursor
//! should read this field rather than invent another; a next pane that does not should ignore it.
//!
//! # The seam back
//!
//! A row that carries `{"verb": "approve", "id": "r-1f2…"}` names the entry the person's `a`/`v`/
//! `r`/`d` act on ([`App::queue_verb_on`]), and the verb word is what the manifest's `actions`
//! gates.

use letibot_plugin::pane::{Feed, Input, Size};
use letibot_sessionlog::event::{MergeGateOutcome, MergeGateStep, QueueMove};
use serde_json::{Value, json};

use crate::app::sessions_pane::Host;
use crate::app::*;
use crate::ui::panes::queue::{
    Filter, LEASE_STALE_MS, QueueScope, Selected, Verb, gates_running, mark_of, owner_word,
    repo_name, verb_moves,
};

/// The hook this pane answers under: `pane_queue` (the pane's id is the layer's, `queue/queue`).
pub(crate) const HOOK: &str = "pane_queue";

/// The feed's name in `plugin.toml` and in `Input::feeds` — one word, both sides.
pub(crate) const FEED: Feed = Feed::Queue;

/// **What the feed is called in a sentence** — a layer that failed is told which pane it was
/// drawing, and *the queue* is the word the operator uses.
const PANE: &str = "the queue";

/// **Is the review queue working on this row?** — `ui/panes/queue.rs`'s `review_open`, repeated
/// here because the feed must say it: a plugin guessing *in flight* from a state word would be a
/// second copy of the daemon's rule, and the two copies would drift.
pub(crate) fn review_open(state: &str) -> bool {
    matches!(state, "waiting" | "reviewing" | "blocked")
}

/// The same for a landing. See [`review_open`].
pub(crate) fn merge_open(state: &str) -> bool {
    matches!(state, "queued" | "gating" | "blocked")
}

impl App {
    /// **The snapshot's own "now", carried forward by this head's clock since it arrived.**
    ///
    /// The same reading `ui/panes/queue.rs` makes for its own rows, so a plugin's *4m* and the
    /// host pane's *4m* are the same four minutes.
    pub(crate) fn queue_plugin_now(&self) -> u64 {
        self.queues
            .at_ms
            .saturating_add(self.now_ms.saturating_sub(self.queues_got_ms))
    }

    /// **The queue, as a plugin reads it** — see this module's doc for what is here and why.
    pub(crate) fn queue_feed(&self) -> Value {
        let now = self.queue_plugin_now();
        let q = &self.queues;
        let serving = |repo: &str| q.leases.iter().find(|l| l.repository == repo);
        let serving_owner = |owner: &str, repo: &str| {
            !owner.is_empty() && serving(repo).is_some_and(|l| l.owner == owner)
        };
        // **A row's verbs, from the daemon's own rule** — which of them would move it, and for each
        // that would not, the sentence saying why. The plugin prints the ones that act.
        let verbs_of = |row: &Selected<'_>| -> Value {
            Value::Array(
                [Verb::Approve, Verb::Veto, Verb::Restart, Verb::Rm]
                    .into_iter()
                    .map(|verb| match verb_moves(verb, row) {
                        Ok(()) => json!({"verb": verb.word(), "why": ""}),
                        Err(why) => json!({"verb": verb.word(), "why": why}),
                    })
                    .collect(),
            )
        };
        let running = gates_running(q, now);
        let reviews: Vec<Value> = q
            .reviews
            .iter()
            .map(|r| {
                json!({
                    "queue": "review",
                    "id": r.id,
                    // `branch`, or `target` — the target's own red tip.
                    "kind": r.kind,
                    "live": review_open(&r.state),
                    "state": r.state,
                    "state_words": r.state.replace('_', " "),
                    "mark": mark_of(&r.state).0,
                    "in_state_ms": now.saturating_sub(r.state_ms),
                    "age_ms": now.saturating_sub(r.created_ms),
                    "repository": repo_name(&r.repository),
                    "repository_path": r.repository,
                    "target": r.target,
                    "branch": r.branch,
                    "owner": owner_word(&r.owner),
                    "owner_is_serving": serving_owner(&r.owner, &r.repository),
                    "reviewer": r.reviewer,
                    "head_sha": r.head_sha,
                    "short_sha": short_sha(&r.head_sha),
                    "reviewed_sha": r.reviewed_sha,
                    "verdict": r.verdict,
                    "verdict_by": r.verdict_by,
                    "attempts": r.attempts,
                    "failure": r.failure,
                    "reasons": r.reasons,
                    "evidence": r.evidence,
                    "brief": r.brief,
                    "fixes": r.fixes,
                    "deadline_ms": r.deadline_ms,
                    "verbs": verbs_of(&Selected::Review(r)),
                })
            })
            .collect();
        let merges: Vec<Value> = q
            .merges
            .iter()
            .map(|m| {
                json!({
                    "queue": "merge",
                    "id": m.id,
                    "review_id": m.review_id,
                    "live": merge_open(&m.state),
                    "state": m.state,
                    "state_words": m.state.replace('_', " "),
                    "mark": mark_of(&m.state).0,
                    "in_state_ms": now.saturating_sub(m.state_ms),
                    "age_ms": now.saturating_sub(m.created_ms),
                    "repository": repo_name(&m.repository),
                    "repository_path": m.repository,
                    "target": m.target,
                    "branch": m.branch,
                    "owner": owner_word(&m.owner),
                    "owner_is_serving": serving_owner(&m.owner, &m.repository),
                    "approved_sha": m.approved_sha,
                    "short_sha": short_sha(&m.approved_sha),
                    "approved_by": m.approved_by,
                    "rebased_sha": m.rebased_sha,
                    "landed_sha": m.landed_sha,
                    "pushed": m.pushed,
                    "evidence": m.evidence,
                    "fixes": m.fixes,
                    // **What a queued landing waits for** — the gate in flight in its repository,
                    // already in the pane's words (`gate running on 1f2a3b4 for 2m`).
                    "waits_on": (m.state == "queued")
                        .then(|| running.get(m.repository.as_str()).cloned())
                        .flatten(),
                    "gate_steps": m.gate_steps.iter().map(step_of).collect::<Vec<Value>>(),
                    "verbs": verbs_of(&Selected::Merge(m)),
                })
            })
            .collect();
        let targets: Vec<Value> = q
            .targets
            .iter()
            .map(|t| {
                json!({
                    "repository": repo_name(&t.repository),
                    "repository_path": t.repository,
                    "target": t.target,
                    "sha": t.sha,
                    "short_sha": short_sha(&t.sha),
                    "state": t.state,
                    "state_words": t.state.replace('_', " "),
                    "since_ms": now.saturating_sub(t.checked_ms),
                    "evidence": t.evidence,
                    "gate_steps": t.gate_steps.iter().map(step_of).collect::<Vec<Value>>(),
                })
            })
            .collect();
        let leases: Vec<Value> = q
            .leases
            .iter()
            .map(|l| {
                let age = now.saturating_sub(l.heartbeat_ms);
                json!({
                    "repository": repo_name(&l.repository),
                    "repository_path": l.repository,
                    "owner": owner_word(&l.owner),
                    "pid": l.pid,
                    "host": l.host,
                    "build": l.build,
                    "heartbeat_ms": age,
                    "stale": age > LEASE_STALE_MS,
                })
            })
            .collect();
        let recent: Vec<Value> = q.recent.iter().map(|m| move_of(m, now)).collect();
        let stale_leases = leases
            .iter()
            .filter(|l| l["stale"].as_bool() == Some(true))
            .count();
        json!({
            // **The host's clock**, so a plugin that wants to measure an age can — and so the
            // `*_ms` fields above are one subtraction and not a guess. A guest has no clock.
            "now_ms": now,
            "at_ms": q.at_ms,
            // **Whose queues these are** — the words the pane's own header uses, so a project's
            // layer can reword the scope without knowing how the head resolved it.
            "scope": {
                "kind": self.queue_scope.kind(),
                "words": self.queue_scope_words(),
                "repositories": self.queues.here.as_ref().map(|h| {
                    h.repositories.iter().map(|r| repo_name(r)).collect::<Vec<String>>()
                }).unwrap_or_default(),
            },
            "view": self.queue_view.word(),
            // **Which row the cursor is on**, by the entry id it names — so a row can print its own
            // id and sha where the person is looking, and nowhere else. The pane's ruling is one
            // line per entry with the address only under the cursor (the declutter branch's), and a
            // plugin cannot read a cursor the host never tells it about.
            //
            // It is the entry the LAST frame's row named, which is one frame behind on the very
            // first draw (there is no view yet) and exact from then on: a cursor moves, the feed
            // changes, the plugin is asked again. Empty is *no row is picked*.
            "cursor": self.queue_selected_id().unwrap_or_default(),
            "unreadable": q.unreadable,
            "archived": q.archived,
            "not_queued": q.not_queued.iter().map(|r| repo_name(r)).collect::<Vec<String>>(),
            "reviews": reviews,
            "merges": merges,
            "targets": targets,
            "leases": leases,
            "recent": recent,
            // **The counters the pane's tab strip carries**, so a plugin's header says the same
            // numbers without counting rows itself — and `finished` is the count the parent asked
            // for, because the finished rows are the ones that must not compete with live work.
            "counts": {
                "review": q.reviews.iter().filter(|r| review_open(&r.state)).count(),
                "merge": q.merges.iter().filter(|m| merge_open(&m.state)).count(),
                "finished": q.reviews.iter().filter(|r| !review_open(&r.state)).count()
                    + q.merges.iter().filter(|m| !merge_open(&m.state)).count(),
                "red_targets": q.targets.iter().filter(|t| t.state == "red").count(),
                "unjudged_targets": q
                    .targets
                    .iter()
                    .filter(|t| t.state == "timed_out" || t.state == "blocked")
                    .count(),
                "stale_leases": stale_leases,
            },
        })
    }

    /// **The scope, in the words the pane's own header uses** — `this session's repository —
    /// letibot`, `every repository`, `/path — named`. One string, so a plugin draws it rather than
    /// re-deriving it from `kind` and a path.
    fn queue_scope_words(&self) -> String {
        match self.queue_filter(&self.queue_scope) {
            Filter::Nowhere(ws) => format!(
                "this session's workspace {ws} is in no repository, so it has no queue of its own"
            ),
            Filter::Repo(r) => format!("this session's repository — {}", repo_name(&r)),
            Filter::Repos(rs) => format!(
                "this session's repositories, one project — {}",
                rs.iter()
                    .map(|r| repo_name(r))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Filter::Path(p) => format!("{} — named", p.display()),
            Filter::Every if self.queue_scope == QueueScope::All => "every repository".to_string(),
            Filter::Every => {
                "every repository — this daemon does not say which repository this session is in"
                    .to_string()
            }
        }
    }

    /// **The pane, from the plugin** — `None` when there is no layer to ask, when its answer is not
    /// a view this head can drive, or when it reached a verb the manifest does not declare. Each of
    /// those is the host pane's cue (`ui/panes/queue.rs`).
    pub(crate) fn queue_plugin_view(&self) -> Option<letibot_sessionlog::pane::View> {
        let slot = self.plugin_host();
        let host = slot.as_ref()?;
        host.reader(HOOK, FEED)?;
        let declared = host.declared(HOOK, FEED);
        let input = self.queue_input(host.state_of(HOOK));
        let answered = host.ask(HOOK, PANE, &input);
        let view = answered.view?;
        host.remember(HOOK, view.state.clone());
        // The host's rules, and the acts the layer declared (C3). A view that reaches a verb the
        // manifest does not name is refused here rather than drawn and then refused per keypress.
        view.validate_for(&declared).ok()?;
        Some(view)
    }

    /// **What the queue's layer is asked with** — the one feed it declared, the fold state of the
    /// rows the person has opened, and the size of the pane it will be drawn in.
    fn queue_input(&self, state: Option<Value>) -> Input {
        let mut feeds = std::collections::BTreeMap::new();
        feeds.insert(FEED.name().to_string(), self.queue_feed());
        Input {
            v: letibot_plugin::pane::V,
            state,
            look: None,
            event: None,
            size: Size {
                w: self.term_cols.max(20) as u32,
                h: self.screen_rows.max(10) as u32,
            },
            feeds,
            folded: self.queue_folds.clone(),
            below: None,
        }
    }

    /// **The plugin's answer, drawn** — the view through the head's one plugin-pane renderer, with
    /// the rows its stops landed on recorded so the arrows, Enter and a click read what was drawn.
    ///
    /// `None` when the pane should be the host's own: no layer, a view this head cannot drive, or
    /// `/queue builtin`.
    pub(crate) fn queue_plugin_lines(&mut self, w: usize) -> Option<Vec<String>> {
        if self.queue_builtin {
            return None;
        }
        let view = self.queue_plugin_view()?;
        let screen = crate::ui::panes::plugin::Screen::Overview;
        let content = crate::ui::panes::plugin::content(&view, screen, self.queue_sel, w);
        // **The stops are the view's**, so the arrows walk what the plugin drew and `queue_sel`
        // means one thing to the cursor, to Enter and to a click.
        self.queue_plugin_stops = crate::ui::panes::plugin::stops(&view, screen).len();
        self.queue_stop_rows = content.lines.stop_rows.clone();
        // The plugin's pane has no tab strip: its own header says which queue is which.
        self.queue_tabs = None;
        self.queue_held = Some(view);
        Some(crate::ui::render::row_strings(
            &content.lines.lines,
            self.cfg.palette(),
        ))
    }

    /// **The entry the row under the cursor names**, when the drawn pane is a plugin's — what the
    /// person's `a`/`v`/`r`/`d` act on.
    ///
    /// Read from the act the row CARRIES (`{"verb":…,"id":…}`), not from the host's own row
    /// enumeration: that is the whole of `Act::Verb`, and the reason a plugin row can now say which
    /// entry it means.
    pub(crate) fn queue_plugin_entry(&self) -> Option<String> {
        let view = self.queue_held.as_ref()?;
        let screen = crate::ui::panes::plugin::Screen::Overview;
        let target = crate::ui::panes::plugin::target_of(view, screen, self.queue_sel)?;
        match target {
            crate::ui::panes::plugin::Target::Row(r) => {
                r.enter.as_ref().and_then(|a| a.entry()).map(str::to_string)
            }
            // A line and a finding carry no act in this contract, so neither names an entry.
            _ => None,
        }
    }
}

/// Ten characters of a sha, or the whole thing when it is shorter — the pane's own reading.
fn short_sha(sha: &str) -> String {
    sha.chars().take(10).collect()
}

/// **One gate step, in the words a person reads** — `"timed out"`, not `timed_out`; the outcome's
/// own word rather than the Rust enum's serde spelling.
fn step_of(step: &MergeGateStep) -> Value {
    let (word, failed) = match step.outcome {
        MergeGateOutcome::Passed => ("passed", false),
        MergeGateOutcome::Failed => ("failed", true),
        MergeGateOutcome::NotRun => ("not run", false),
        MergeGateOutcome::NoGate => ("no gate", false),
        MergeGateOutcome::TimedOut => ("timed out", true),
        MergeGateOutcome::Killed => ("killed", true),
    };
    json!({
        "command": step.command,
        "outcome": word,
        "failed": failed,
        "elapsed_ms": step.elapsed_ms,
        "output": step.output,
    })
}

/// One move of one row, as the pane's `↳` line reads it.
fn move_of(m: &QueueMove, now: u64) -> Value {
    json!({
        "queue": m.queue,
        "entry_id": m.entry_id,
        "repository": repo_name(&m.repository),
        "from": m.from,
        "to": m.to,
        "ago_ms": now.saturating_sub(m.at_ms),
        "by": owner_word(&m.by),
        "why": m.why,
    })
}

/// The scope's `kind`, as the feed spells it — `here`, `all`, `path`.
impl QueueScope {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            QueueScope::Here => "here",
            QueueScope::All => "all",
            QueueScope::Path(_) => "path",
        }
    }
}

#[cfg(test)]
mod tests;
