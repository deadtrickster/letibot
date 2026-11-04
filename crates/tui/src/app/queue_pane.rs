//! The queue pane as a plugin's pane (`plugins/builtin/queue`), run in the HEAD rather than by the
//! daemon: the snapshot it draws arrives here, so a daemon-side layer would need it handed back. The
//! feed is the daemon's own state words plus `live`, `verbs` and `cursor`, none of it inferable.

use letibot_plugin::pane::{Feed, Input, Size};
use letibot_sessionlog::event::{
    MergeGateOutcome, MergeGateStep, QueueMerge, QueueMove, QueueReview,
};
use serde_json::{Value, json};

use crate::app::*;
use crate::ui::panes::queue::{
    Filter, LEASE_STALE_MS, QueueScope, Selected, Verb, gates_running, mark_of, merge_open,
    owner_word, repo_name, review_open, verb_moves,
};
use letibot_plugin::pane::Act;

/// The hook this pane answers under: `pane_queue` (the pane's id is the layer's, `queue/queue`).
pub(crate) const HOOK: &str = "pane_queue";

/// How many finished rows the feed carries. The layer draws at most this many behind the fold,
/// so a row beyond it is bytes the guest cannot show; the counts still say the true number.
const FINISHED_MAX: usize = 200;

/// How many characters of a finished row's long fields the feed carries. A guest's input region
/// is one mebibyte, so an unbounded history (the store never deletes a row) would leave a busy
/// project with no queue pane at all.
const FINISHED_TEXT: usize = 1_000;

/// How many bytes of finished rows the feed carries in all - the belt under [`FINISHED_TEXT`].
const FINISHED_BUDGET: usize = 500_000;

/// The resolution of the feed's clock - one second, because that is the finest thing a row can say.
/// The host answers a pane once per distinct `Input`, so a millisecond clock is a cache that misses
/// every frame: one wasm call and one serialisation of the snapshot per draw, for no drawn change.
const CLOCK_QUANTUM_MS: u64 = 1_000;

/// `now`, rounded down to [`CLOCK_QUANTUM_MS`].
fn quantised(now: u64) -> u64 {
    now / CLOCK_QUANTUM_MS * CLOCK_QUANTUM_MS
}

/// The feed's name in `plugin.toml` and in `Input::feeds` - one word, both sides.
pub(crate) const FEED: Feed = Feed::Queue;

/// What the feed is called in a sentence - a layer that failed is told which pane it was
/// drawing, and *the queue* is the word the operator uses.
const PANE: &str = "the queue";

/// Does the scope keep more than one repository to tell apart? - decided from the scope the head
/// RESOLVED, never from its prose, which was wrong in both directions. A row must say where it is
/// from whenever that is not already in the pane's title (`Filter::Repo`) and there is one at all.
fn several_of(filter: &Filter) -> bool {
    match filter {
        Filter::Repo(_) | Filter::Nowhere(_) => false,
        Filter::Repos(rs) => rs.len() > 1,
        Filter::Path(_) | Filter::Every => true,
    }
}

impl App {
    /// The snapshot's own "now", carried forward by this head's clock since it arrived - the same
    /// reading `ui/panes/queue.rs` makes, so a plugin's *4m* and the host pane's *4m* are the same
    /// four minutes.
    pub(crate) fn queue_plugin_now(&self) -> u64 {
        self.queues
            .at_ms
            .saturating_add(self.now_ms.saturating_sub(self.queues_got_ms))
    }

    /// The queue, as a plugin reads it - see this module's doc for what is here and why.
    pub(crate) fn queue_feed(&self) -> Value {
        // The clock is quantised - see [`CLOCK_QUANTUM_MS`]: the input is the host's cache key,
        // so a millisecond clock is a wasm call per frame for a number no row can draw.
        let now = quantised(self.queue_plugin_now());
        let q = &self.queues;
        let scope = self.queue_filter(&self.queue_scope);
        let serving = |repo: &str| q.leases.iter().find(|l| l.repository == repo);
        let serving_owner = |owner: &str, repo: &str| {
            !owner.is_empty() && serving(repo).is_some_and(|l| l.owner == owner)
        };
        // A row's verbs, from the daemon's own rule - which of them would move it, and for each
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
        // The entry being read whole travels whole; a finished row's long fields are cut,
        // because the store never deletes a row and the guest's input region is one mebibyte.
        let open = self.queue_open.clone().unwrap_or_default();
        let cut = |text: &str, whole: bool| -> String {
            if whole || text.chars().count() <= FINISHED_TEXT {
                text.to_string()
            } else {
                format!(
                    "{} … (cut at {FINISHED_TEXT} characters)",
                    text.chars().take(FINISHED_TEXT).collect::<String>()
                )
            }
        };
        let review_of = |r: &QueueReview| {
            let whole = review_open(&r.state) || r.id == open;
            json!({
                "queue": "review",
                "id": r.id,
                // `branch`, or `target` - the target's own red tip.
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
                "failure": cut(&r.failure, whole),
                "reasons": r.reasons,
                "evidence": cut(&r.evidence, whole),
                "brief": cut(&r.brief, whole),
                "fixes": r.fixes.as_deref().map(|f| cut(f, whole)),
                "deadline_ms": r.deadline_ms,
                "verbs": verbs_of(&Selected::Review(r)),
            })
        };
        let merge_of = |m: &QueueMerge| {
            let whole = merge_open(&m.state) || m.id == open;
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
                "evidence": cut(&m.evidence, whole),
                "fixes": m.fixes.as_deref().map(|f| cut(f, whole)),
                // What a queued landing waits for - the gate in flight in its repository,
                // already in the pane's words (`gate running on 1f2a3b4 for 2m`).
                "waits_on": (m.state == "queued")
                    .then(|| running.get(m.repository.as_str()).cloned())
                    .flatten(),
                "gate_steps": m
                    .gate_steps
                    .iter()
                    .map(|s| step_of(s, whole))
                    .collect::<Vec<Value>>(),
                // The step its gate is running right now, with the tail and how long since it
                // last changed - the facts that tell whether a `gating` row is working or stuck
                // without opening a file on the daemon's box.
                "running_gate": m.running_gate.as_ref().map(|g| {
                    let key = format!("{}#{}", m.id, g.step);
                    gate_of(g, self.gate_tails.get(&key).map(|t| t.since_ms), now)
                }),
                "verbs": verbs_of(&Selected::Merge(m)),
            })
        };
        let mut reviews: Vec<Value> = q
            .reviews
            .iter()
            .filter(|r| review_open(&r.state))
            .map(review_of)
            .collect();
        let mut merges: Vec<Value> = q
            .merges
            .iter()
            .filter(|m| merge_open(&m.state))
            .map(merge_of)
            .collect();
        // The finished rows, newest first, within the budget. The counts still say the true
        // numbers, so the layer's folded line says `the last N` rather than pretending.
        let mut done: Vec<&QueueReview> = q
            .reviews
            .iter()
            .filter(|r| !review_open(&r.state))
            .collect();
        done.sort_by_key(|r| std::cmp::Reverse(r.state_ms));
        let mut spent = 0usize;
        for r in done.into_iter().take(FINISHED_MAX) {
            if spent > FINISHED_BUDGET {
                break;
            }
            let v = review_of(r);
            spent += serde_json::to_string(&v).map_or(0, |s| s.len());
            reviews.push(v);
        }
        let mut done: Vec<&QueueMerge> =
            q.merges.iter().filter(|m| !merge_open(&m.state)).collect();
        done.sort_by_key(|m| std::cmp::Reverse(m.state_ms));
        let mut spent = 0usize;
        for m in done.into_iter().take(FINISHED_MAX) {
            if spent > FINISHED_BUDGET {
                break;
            }
            let v = merge_of(m);
            spent += serde_json::to_string(&v).map_or(0, |s| s.len());
            merges.push(v);
        }
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
                    "gate_steps": t
                        .gate_steps
                        .iter()
                        .map(|s| step_of(s, true))
                        .collect::<Vec<Value>>(),
                    // A queued landing's detail reads the TARGET's gate, which is the gate it
                    // waits on, so the tail travels with the target too.
                    "running_gate": t.running_gate.as_ref().map(|g| {
                        let key = format!("{}#{}#{}", t.repository, t.target, g.step);
                        gate_of(g, self.gate_tails.get(&key).map(|x| x.since_ms), now)
                    }),
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
            // The host's clock, so a plugin that wants to measure an age can - and so the
            // `*_ms` fields above are one subtraction and not a guess. A guest has no clock.
            "now_ms": now,
            "at_ms": q.at_ms,
            // Whose queues these are - the words the pane's own header uses, so a project's
            // layer can reword the scope without knowing how the head resolved it.
            "scope": {
                "kind": self.queue_scope.kind(),
                "words": self.queue_scope_words(),
                // Whether a row must name its repository - the host's answer, because the
                // question is *how many are in scope* and the head is what resolved the scope.
                // A plugin reading this instead of the words is the whole point of the field.
                "several": several_of(&scope),
                "repositories": self.queues.here.as_ref().map(|h| {
                    h.repositories.iter().map(|r| repo_name(r)).collect::<Vec<String>>()
                }).unwrap_or_default(),
            },
            "view": self.queue_view.word(),
            // The entry whose detail the overlay is reading, so the layer draws that row even
            // when it has left the live list - an overlay must not vanish under the row it reads.
            "open": open,
            // Which row the cursor is on, by the entry id it names, so a row can print its id and sha
            // only under the cursor - a plugin cannot read a cursor the host never tells it about. It
            // is the entry the LAST frame's row named, so it is one frame behind on the first draw.
            "cursor": self.queue_selected_id().unwrap_or_default(),
            "unreadable": q.unreadable,
            "archived": q.archived,
            "not_queued": q.not_queued.iter().map(|r| repo_name(r)).collect::<Vec<String>>(),
            "reviews": reviews,
            "merges": merges,
            "targets": targets,
            "leases": leases,
            "recent": recent,
            // The counters the pane's tab strip carries, so a plugin's header says the same
            // numbers without counting rows itself - and `finished` is the count the parent asked
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

    /// The scope, in the words the pane's own header uses - `this session's repository  -
    /// letibot`, `every repository`, `/path - named`. One string, so a plugin draws it rather than
    /// re-deriving it from `kind` and a path.
    fn queue_scope_words(&self) -> String {
        match self.queue_filter(&self.queue_scope) {
            Filter::Nowhere(ws) => format!(
                "this session's workspace {ws} is in no repository, so it has no queue of its \
                 own - `/queue all` shows every repository's queues"
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

    /// The pane, from the plugin - `None` when there is no layer to ask, when its answer is not a
    /// view this head can drive, or when it reached a verb the manifest does not declare. Each of
    /// those lands in `queue_why`: with one renderer the refusal is the whole pane.
    pub(crate) fn queue_plugin_view(&mut self) -> Option<letibot_sessionlog::pane::View> {
        let (view, why) = self.queue_answered();
        self.queue_why = why;
        view
    }

    /// The ask itself, split out so `queue_why` can be written after the host's borrow ends: the
    /// view when there is one, and the sentence for the person when there is not.
    fn queue_answered(&self) -> (Option<letibot_sessionlog::pane::View>, Option<String>) {
        let slot = self.plugin_host();
        let Some(host) = slot.as_ref() else {
            return (
                None,
                Some("this head has no plugin stack for this folder".to_string()),
            );
        };
        if host.reader(HOOK, FEED).is_none() {
            return (
                None,
                Some("no installed layer reads the queue feed".to_string()),
            );
        }
        let declared = host.declared(HOOK, FEED);
        let input = self.queue_input(host.state_of(HOOK));
        let answered = host.ask(HOOK, PANE, &input);
        let Some(view) = answered.view else {
            return (
                None,
                Some(
                    host.peek_problem()
                        .unwrap_or_else(|| "its answer was not a pane this head can read".into()),
                ),
            );
        };
        host.remember(HOOK, view.state.clone());
        // The host's rules, and the acts the layer declared (C3). A view that reaches a verb the
        // manifest does not name is refused here rather than drawn and then refused per keypress.
        match view.validate_for(&declared) {
            Ok(()) => (Some(view), None),
            Err(why) => (None, Some(why)),
        }
    }

    /// What the queue's layer is asked with - the one feed it declared, the fold state of the
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

    /// The plugin's answer, drawn - the view through the head's one plugin-pane renderer, with
    /// the rows its stops landed on recorded so the arrows, Enter and a click read what was drawn.
    pub(crate) fn queue_plugin_lines(&mut self, w: usize) -> Option<Vec<String>> {
        let view = self.queue_plugin_view()?;
        let screen = crate::ui::panes::plugin::Screen::Overview;
        // The stops are the view's, so the arrows walk what the plugin drew and `queue_sel`
        // means one thing to the cursor, to Enter and to a click.
        self.queue_plugin_stops = crate::ui::panes::plugin::stops(&view, screen).len();
        // The cursor cannot be left past the end of the list it is on: folding the history away
        // removes every row it could have been on, and a `queue_sel` past the last stop draws a pane
        // with no mark on it. Clamped against the count just read, so the mark and the keys agree.
        if self.queue_plugin_stops > 0 {
            self.queue_sel = self.queue_sel.min(self.queue_plugin_stops - 1);
        }
        let content = crate::ui::panes::plugin::content(&view, screen, self.queue_sel, w);
        self.queue_stop_rows = content.lines.stop_rows.clone();
        self.queue_held = Some(view);
        Some(crate::ui::render::row_strings(
            &content.lines.lines,
            self.cfg.palette(),
        ))
    }

    /// The entry the row under the cursor names, when the drawn pane is a plugin's - what the
    /// person's `a`/`v`/`r`/`d` act on. Read from the act the row CARRIES (`{"verb":...,"id":...}`),
    /// not from the host's own enumeration, which is what lets a plugin row say which entry it means.
    pub(crate) fn queue_plugin_entry(&self) -> Option<String> {
        let (_, act) = self.queue_plugin_row()?;
        act.and_then(|a| a.entry().map(str::to_string))
    }

    /// The row under the cursor, and the act it carries - `None` when the pane on screen is the
    /// head's own, or the cursor is on a line rather than a row. Read from the view the last draw kept
    /// ([`App::queue_held`]): re-deriving the rows would read a different list from the one drawn.
    fn queue_plugin_row(&self) -> Option<(String, Option<Act>)> {
        let view = self.queue_held.as_ref()?;
        let screen = crate::ui::panes::plugin::Screen::Overview;
        match crate::ui::panes::plugin::target_of(view, screen, self.queue_sel)? {
            crate::ui::panes::plugin::Target::Row(r) => Some((r.id().to_string(), r.enter.clone())),
            // A line and a finding carry no act in this contract, so neither names a row.
            _ => None,
        }
    }

    /// Does the row under the cursor fold, and which way does it stand? - `None` for every row that
    /// is not a fold. The hint bar reads this, so its Enter says `opens the history` over the fold row
    /// and `opens the row` over an entry.
    pub(crate) fn queue_row_folds(&self) -> Option<bool> {
        let (id, act) = self.queue_plugin_row()?;
        matches!(act, Some(Act::Fold { .. }))
            .then(|| self.queue_folds.get(&id).copied().unwrap_or(false))
    }

    /// Would this key move the row under the cursor? The bar asks it per verb: `a` is not offered over a landing
    /// (approved already), `o` only over a row with a reviewer. A head with no row drawn yet answers yes, since a
    /// bar that hid everything before the first frame would say nothing.
    pub(crate) fn queue_key_moves(&self, act: crate::app::keys::bindings::Act) -> bool {
        use crate::app::keys::bindings::Act;
        let verb = match act {
            Act::Verb(v) => v,
            Act::Reviewer => return self.queue_row_has_a_reviewer(),
            _ => return true,
        };
        let Some(id) = self.queue_selected_id() else {
            return true;
        };
        let row = self
            .queues
            .reviews
            .iter()
            .find(|r| r.id == id)
            .map(Selected::Review)
            .or_else(|| {
                self.queues
                    .merges
                    .iter()
                    .find(|m| m.id == id)
                    .map(Selected::Merge)
            });
        row.is_none_or(|row| verb_moves(verb, &row).is_ok())
    }

    /// Whether the entry the row under the cursor names has a reviewer to go into.
    fn queue_row_has_a_reviewer(&self) -> bool {
        let Some(id) = self.queue_selected_id() else {
            return true;
        };
        self.queues
            .reviews
            .iter()
            .any(|r| r.id == id && r.reviewer.as_deref().is_some_and(|s| !s.is_empty()))
    }

    /// Enter on a row that folds - the host's `folded` map, keyed by the row's own id, so a fold is
    /// instant, costs no call and survives the next answer (C3's rule). Answers whether the row under
    /// the cursor folded, so Enter can fall through to its own meaning when it did not.
    pub(crate) fn queue_fold(&mut self) -> bool {
        let Some((id, Some(Act::Fold { .. }))) = self.queue_plugin_row() else {
            return false;
        };
        // The act's own `fold` is not the direction. `Act::fold()` carries `true` because the
        // wire needs a bool; what the key means is *toggle*, the same way the session list's Enter
        // reads it (`sessions_pane::perform`).
        let open = !self.queue_folds.get(&id).copied().unwrap_or(false);
        self.queue_folds.insert(id, open);
        self.redraw = true;
        true
    }

    /// The pane, drawn again as it was - what `/queue plugin` does: asks the layer again with the
    /// scope untouched, which is also the door after a `plugin load`. Choosing who draws the pane is
    /// not choosing WHICH queues, so the scope survives.
    pub(crate) fn queue_drawing_again(&mut self) -> Option<Action> {
        self.redraw = true;
        if self.is_open(Surface::Queue) {
            None
        } else {
            self.open_surface(Surface::Queue)
        }
    }
}

/// Ten characters of a sha, or the whole thing when it is shorter - the pane's own reading.
fn short_sha(sha: &str) -> String {
    sha.chars().take(10).collect()
}

/// One running gate as the feed carries it: the step's facts, the tail, and `still_ms` - how
/// long since the tail last changed, the head's own two-snapshot fact.
fn gate_of(
    g: &letibot_sessionlog::event::QueueRunningGate,
    since_ms: Option<u64>,
    now: u64,
) -> Value {
    json!({
        "command": g.command,
        "step": g.step,
        "of": g.of,
        "running_ms": now.saturating_sub(g.started_ms),
        "bytes": g.log_bytes,
        "tail": g.tail,
        "still_ms": since_ms.map_or(0, |at| now.saturating_sub(at)),
    })
}

/// One gate step, in the words a person reads - `"timed out"`, not `timed_out`; the outcome's
/// own word rather than the Rust enum's serde spelling. `whole` is whether the row is live or
/// being read: a finished row's output is cut, so the history cannot outgrow the guest's region.
fn step_of(step: &MergeGateStep, whole: bool) -> Value {
    let (word, failed) = match step.outcome {
        MergeGateOutcome::Passed => ("passed", false),
        MergeGateOutcome::Failed => ("failed", true),
        MergeGateOutcome::NotRun => ("not run", false),
        MergeGateOutcome::NoGate => ("no gate", false),
        MergeGateOutcome::TimedOut => ("timed out", true),
        MergeGateOutcome::Killed => ("killed", true),
    };
    // A finished row's output is cut, so the history cannot outgrow the guest's region.
    let output = if whole || step.output.chars().count() <= FINISHED_TEXT {
        step.output.clone()
    } else {
        format!(
            "{} … (cut at {FINISHED_TEXT} characters)",
            step.output.chars().take(FINISHED_TEXT).collect::<String>()
        )
    };
    json!({
        "command": step.command,
        "outcome": word,
        "failed": failed,
        "elapsed_ms": step.elapsed_ms,
        "output": output,
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

/// The scope's `kind`, as the feed spells it - `here`, `all`, `path`.
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
