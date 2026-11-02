//! The merge queue, hosted: the pane on this surface is the `queue` layer's answer
//! (`plugins/builtin/queue`, drawn through `app::queue_pane`). This file keeps what a host owes it -
//! the input routing, the feed, and the reading of the snapshot the composer's edge draws.
//!
//! What stays here is what the layer cannot know: the standings (the edge is drawn on every screen,
//! not only on the pane), the scope (only the head knows which repository this session is in), the
//! verbs (the daemon's rule, mirrored once), and the gate tails (whether a step moved is a fact
//! about two snapshots, and the head is the only reader of two).

use crate::app::*;
use crate::ui::render::row_strings;
use letibot_sessionlog::event::QueueSnapshot;
use rano::agent::pane::{self, PaneLines};

/// **How long a heartbeat may be silent before the pane calls the owner STALE** - the daemon's
/// lease lifetime (`mergequeue::lease::LEASE_TTL_MS`), so the pane's word and the daemon's
/// takeover agree.
pub(crate) const LEASE_STALE_MS: u64 = 90_000;

/// How often an open pane asks for a fresh snapshot - the heartbeats move without an event.
pub(crate) const REFRESH_MS: u64 = 5_000;

/// How long a finished row counts as today's work - the window the edge's `landed` column reads.
pub(crate) const RECENT_MS: u64 = 24 * 60 * 60 * 1000;

/// **Is the review queue working on this row?** The daemon's own rule, in one place: the feed a
/// plugin reads carries the answer (`live`), so a layer never has to guess *in flight* from a state
/// word - a second copy of this predicate is a copy that drifts.
pub(crate) fn review_open(state: &str) -> bool {
    matches!(state, "waiting" | "reviewing" | "blocked")
}
/// The same for a landing. See [`review_open`].
pub(crate) fn merge_open(state: &str) -> bool {
    matches!(state, "queued" | "gating" | "blocked")
}

impl App {
    /// **The snapshot's own "now", carried forward by this head's clock since it arrived.**
    fn queue_now(&self) -> u64 {
        let since = self.now_ms.saturating_sub(self.queues_got_ms);
        self.queues.at_ms.saturating_add(since)
    }

    /// Note when each running gate's tail last CHANGED, once per snapshot: whether a step is stuck is
    /// a comparison and the daemon sends one half of it. A step whose tail is unchanged keeps its
    /// stamp, a new or grown one starts now, and a step that is gone drops out.
    pub(crate) fn track_gate_tails(&mut self) {
        let now = self.queue_now();
        let mut fresh: std::collections::HashMap<String, GateTail> = Default::default();
        let note =
            |fresh: &mut std::collections::HashMap<String, GateTail>, key: String, tail: &str| {
                let since_ms = match self.gate_tails.get(&key) {
                    Some(old) if old.text == tail => old.since_ms,
                    _ => now,
                };
                fresh.insert(
                    key,
                    GateTail {
                        text: tail.to_string(),
                        since_ms,
                    },
                );
            };
        for m in &self.queues.merges {
            if let Some(g) = &m.running_gate {
                note(&mut fresh, format!("{}#{}", m.id, g.step), &g.tail);
            }
        }
        for t in &self.queues.targets {
            if let Some(g) = &t.running_gate {
                note(
                    &mut fresh,
                    format!("{}#{}#{}", t.repository, t.target, g.step),
                    &g.tail,
                );
            }
        }
        self.gate_tails = fresh;
    }

    /// The edge's standings: this session's repository only, whatever scope the pane is showing. A
    /// session of no repository has none; a daemon too old to say which repository it is in gets
    /// every repository's.
    pub(crate) fn queue_standings(&self) -> Standings {
        let f = self.queue_filter(&QueueScope::Here);
        standings_where(&self.queues_all, self.queue_now(), |r| f.keeps(r))
    }

    /// Ask for a fresh snapshot on the pane's own clock - the rows AND the detail, because a screen
    /// showing a running gate is one whose point is that the numbers move. The detail asks for
    /// itself: it is a surface of its own, so `queue_lines` never runs while a row is open.
    fn ask_for_queues(&mut self) {
        if self.now_ms.saturating_sub(self.queues_asked_ms) >= REFRESH_MS {
            self.queues_asked_ms = self.now_ms;
            if !self.queued.contains(&Action::ListMergeQueue) {
                self.queued.push(Action::ListMergeQueue);
            }
        }
    }

    /// The pane: the `queue` layer's answer, hosted. A snapshot that has not arrived is nobody's to
    /// draw - an empty `reviews` would read as an empty queue - and a layer that did not answer is
    /// said in the host's words with the way out. `queue_stop_rows` is recorded either way.
    pub(crate) fn queue_lines(&mut self, w: usize) -> Vec<String> {
        self.ask_for_queues();
        let mut out = PaneLines::new();
        if self.queues_got_ms == 0 {
            self.queue_plugin_stops = 0;
            self.queue_stop_rows = Vec::new();
            self.queue_held = None;
            out.push(pane::title("the queue"));
            out.blank();
            out.push(pane::faint("    asking the daemon for the queues…"));
            return row_strings(&out.lines, self.cfg.palette());
        }
        if let Some(lines) = self.queue_plugin_lines(w) {
            return lines;
        }
        self.queue_plugin_stops = 0;
        self.queue_stop_rows = Vec::new();
        self.queue_held = None;
        out.push(pane::title("the queue"));
        out.blank();
        let why = self.queue_why.as_deref().unwrap_or("it answered nothing");
        out.push(pane::faint(crate::ui::render::trim_to(
            &format!("    the queue layer did not draw the pane: {why}"),
            w,
        )));
        out.push(pane::faint(crate::ui::render::trim_to(
            "    `/queue plugin` asks it again",
            w,
        )));
        row_strings(&out.lines, self.cfg.palette())
    }

    /// The entry read whole: the plugin's `detail`, hosted. The layer writes the prose on the row;
    /// the host finds the row by the id Enter opened and re-asks on the pane's own clock, so a gate
    /// that writes while the overlay is up is drawn writing.
    pub(crate) fn queue_out_lines(&mut self, w: usize) -> Vec<String> {
        self.ask_for_queues();
        let Some(id) = self.queue_open.clone() else {
            return Vec::new();
        };
        let view = self.queue_plugin_view();
        let screen = crate::ui::panes::plugin::Screen::Overview;
        let lines = match view {
            Some(v) => {
                // The stop whose row IS the open entry. The feed carries the id as `open`, so the
                // layer draws the row even when it has left the live list; a row that is gone
                // altogether is the renderer's own "that row is gone".
                let n = crate::ui::panes::plugin::stops(&v, screen).len();
                let at = (0..n).find(|&i| {
                    crate::ui::panes::plugin::target_of(&v, screen, i).is_some_and(|t| t.id() == id)
                });
                crate::ui::panes::plugin::detail(&v, screen, at.unwrap_or(usize::MAX), w)
            }
            None => {
                let mut out = PaneLines::new();
                out.push(pane::title("the queue entry"));
                out.blank();
                let why = self.queue_why.as_deref().unwrap_or("it answered nothing");
                out.push(pane::faint(crate::ui::render::trim_to(
                    &format!("    the queue layer did not answer: {why} - esc goes back"),
                    w,
                )));
                out
            }
        };
        row_strings(&lines.lines, self.cfg.palette())
    }

    /// **Into the session of the reviewer working the row** - the row opened whole, else the one
    /// under the cursor. A row nobody is reviewing says so and goes nowhere.
    pub(crate) fn go_to_reviewer(&mut self) -> Option<Action> {
        let review = match &self.queue_open {
            Some(id) => self.queues.reviews.iter().find(|r| &r.id == id),
            None => self
                .queue_selected_id()
                .and_then(|id| self.queues.reviews.iter().find(|r| r.id == id)),
        };
        let Some(r) = review else {
            self.say("that row is a landing, not a review - it has no reviewer");
            self.redraw = true;
            return None;
        };
        let Some(id) = r.reviewer.clone().filter(|s| !s.is_empty()) else {
            let said = format!(
                "`{}` has no reviewer working it now ({})",
                r.branch, r.state
            );
            self.say(&said);
            self.redraw = true;
            return None;
        };
        self.queue_open = None;
        self.switch_to(id)
    }

    /// **How many stops the pane on the screen has** - the plugin view's, which is the only pane
    /// there is. Zero is a pane the arrows have nothing to walk.
    pub(crate) fn queue_stop_count(&self) -> usize {
        self.queue_plugin_stops
    }

    /// The entry the row under the cursor NAMES: what `/queue approve` and the overlay take, and what
    /// the person's `a`/`v`/`r`/`d` act on. A plugin row answers with the entry its own act carries,
    /// which is the point of `Act::Verb`: a row can draw one entry and mean it.
    pub(crate) fn queue_selected_id(&self) -> Option<String> {
        self.queue_plugin_entry()
    }

    /// **`scope`, resolved** against the snapshot's word about this session's repository.
    pub(crate) fn queue_filter(&self, scope: &QueueScope) -> Filter {
        match scope {
            QueueScope::All => Filter::Every,
            QueueScope::Path(p) => Filter::Path(
                std::fs::canonicalize(p).unwrap_or_else(|_| std::path::PathBuf::from(p)),
            ),
            QueueScope::Here => match &self.queues_all.here {
                None => Filter::Every,
                Some(h) => match (&h.repository, h.repositories.as_slice()) {
                    (Some(r), _) => Filter::Repo(r.clone()),
                    (None, []) => Filter::Nowhere(h.workspace.clone()),
                    (None, many) => Filter::Repos(many.to_vec()),
                },
            },
        }
    }

    /// **The pane's copy, cut again** - after a snapshot lands or the scope changes.
    pub(crate) fn rescope_queues(&mut self) {
        let f = self.queue_filter(&self.queue_scope);
        self.queues = scoped(&self.queues_all, &f);
    }

    /// **`/queue all` and `/queue /PATH`** - the pane, on that scope. Opening it asks for a
    /// fresh snapshot; switching an open pane's scope only re-cuts the one it has.
    pub(crate) fn queue_in(&mut self, scope: QueueScope) -> Option<Action> {
        self.queue_scope = scope;
        self.queue_sel = 0;
        self.rescope_queues();
        self.redraw = true;
        if self.is_open(Surface::Queue) {
            None
        } else {
            self.open_surface(Surface::Queue)
        }
    }
}

/// The tail a gate's step had when this head last looked, and when it last CHANGED. Only a reader
/// that has seen two snapshots can say whether it moved, and the head is that reader. Keyed by
/// `entry#step`, so a step moving on starts its own clock rather than inheriting the last one's.
#[derive(Debug, Clone, Default)]
pub(crate) struct GateTail {
    pub text: String,
    pub since_ms: u64,
}

/// The repository's last path component - the name a person knows it by.
pub(crate) fn repo_name(repo: &str) -> String {
    if repo.is_empty() {
        return "(unresolved)".into();
    }
    std::path::Path::new(repo)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| repo.to_string())
}

/// The owner as a person reads it: `host/pid`, the start stamp dropped.
pub(crate) fn owner_word(owner: &str) -> String {
    let mut parts = owner.split('/');
    match (parts.next(), parts.next()) {
        (Some(h), Some(p)) => format!("{h}/{p}"),
        _ => owner.to_string(),
    }
}

pub(crate) fn mark_of(state: &str) -> (&'static str, rano::style::Role) {
    match state {
        "waiting" | "queued" => ("[ ]", rano::style::Role::Pending),
        "reviewing" | "gating" => ("[~]", rano::style::Role::Code),
        "approved" | "landed" => ("[x]", rano::style::Role::Success),
        "blocked" | "failed" | "timed_out" | "conflict" => ("[!]", rano::style::Role::Failure),
        "rejected" | "cannot_decide" => ("[!]", rano::style::Role::Attention),
        _ => ("[-]", rano::style::Role::Faint),
    }
}

/// **`gate running on <sha> for Nm`** - the words for a gate in flight, from when it started.
pub(crate) fn gate_running_words(sha: &str, for_ms: u64) -> String {
    let short: String = sha.chars().take(10).collect();
    format!("gate running on {short} for {}m", for_ms / 60_000)
}

/// **The gate each repository is running** (one at a time) - what its queued landings wait for.
/// The store has it: a `gating` target since its `checked_ms`, or a `gating` landing since its
/// `state_ms`, rebased.
pub(crate) fn gates_running(
    q: &QueueSnapshot,
    now: u64,
) -> std::collections::HashMap<&str, String> {
    let mut out = std::collections::HashMap::new();
    for t in q.targets.iter().filter(|t| t.state == "gating") {
        out.insert(
            t.repository.as_str(),
            gate_running_words(&t.sha, now.saturating_sub(t.checked_ms)),
        );
    }
    for m in q.merges.iter().filter(|m| m.state == "gating") {
        if let Some(sha) = m.rebased_sha.as_deref() {
            out.insert(
                m.repository.as_str(),
                gate_running_words(sha, now.saturating_sub(m.state_ms)),
            );
        }
    }
    out
}

/// What the queue is DOING, in six words, drawn on the composer's bottom edge: the four columns and
/// the two things a TARGET can be that no row can say - red, and unjudged. `stuck` is the column's
/// word, not the store's; `landed` counts a day, the window the pane's done list uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Standings {
    pub(crate) review: usize,
    pub(crate) merging: usize,
    /// **Open and stopped** - the rows a person has to move (see [`standings_where`]).
    pub(crate) stuck: usize,
    /// **Landed inside the last day** - not for ever.
    pub(crate) landed: usize,
    /// Target branches that are red - the emergency.
    pub(crate) red: usize,
    /// **Target branches nobody could judge** - timed out or blocked. Nothing lands on them
    /// either, so they are an alarm too, not a quiet edge.
    pub(crate) unjudged: usize,
    /// **The step the target's own gate is on**, when this repository's target is `gating` -
    /// `(step, of)`. The edge is one line that must still fit at eighty columns, so two numbers.
    pub(crate) gate: Option<(usize, usize)>,
}

impl Standings {
    /// The words, and a count of nothing is not a word: a repository whose only live fact is a red
    /// target reads `1 red` and not four zeros. The order is the pane's and `red` leads it, because
    /// the edge is cut from its right end. The step is parenthesised and last, so it is lost first.
    pub(crate) fn words(&self) -> Option<String> {
        let mut words: Vec<String> = Vec::new();
        if self.red > 0 {
            words.push(format!("{} red", self.red));
        }
        if self.unjudged > 0 {
            words.push(format!("{} unjudged", self.unjudged));
        }
        for (n, word) in [
            (self.review, "in review"),
            (self.merging, "being merged"),
            (self.stuck, "stuck"),
            (self.landed, "landed"),
        ] {
            if n == 0 {
                continue;
            }
            match (word, self.gate) {
                ("being merged", Some((step, of))) => {
                    words.push(format!("{n} being merged (gate step {step} of {of})"))
                }
                _ => words.push(format!("{n} {word}")),
            }
        }
        (!words.is_empty()).then(|| words.join(" · "))
    }

    pub(crate) fn role(&self) -> rano::style::Role {
        if self.red > 0 {
            rano::style::Role::Failure
        } else if self.unjudged > 0 || self.stuck > 0 {
            rano::style::Role::Attention
        } else {
            rano::style::Role::Pending
        }
    }
}

/// The standings of the rows `keep` admits, as of `now` - the snapshot's clock carried forward,
/// because `landed` asks about a window and `at_ms` is a moment. The snapshot holds every row the
/// store ever had, so a terminal row inflates no live column: `stuck` is open-and-stopped only.
pub(crate) fn standings_where(
    q: &QueueSnapshot,
    now: u64,
    keep: impl Fn(&str) -> bool,
) -> Standings {
    let green = |repo: &str, target: &str| {
        q.targets
            .iter()
            .any(|t| t.repository == repo && t.target == target && t.state == "green")
    };
    let recent = |ms: u64| now.saturating_sub(ms) <= RECENT_MS;
    let mut s = Standings::default();
    for r in q.reviews.iter().filter(|r| keep(&r.repository)) {
        if r.kind == "target" && green(&r.repository, &r.target) {
            continue;
        }
        match r.state.as_str() {
            "waiting" | "reviewing" => s.review += 1,
            // **`cannot_decide` is here and not in the live columns**: the store calls it
            // terminal *until a person rules*, and a row waiting on a person is what this column
            // is for.
            "blocked" | "cannot_decide" => s.stuck += 1,
            _ => {}
        }
    }
    for m in q.merges.iter().filter(|m| keep(&m.repository)) {
        match m.state.as_str() {
            "queued" | "gating" => s.merging += 1,
            "blocked" | "failed" | "timed_out" | "conflict" => s.stuck += 1,
            "landed" if recent(m.state_ms) => s.landed += 1,
            _ => {}
        }
    }
    for t in q.targets.iter().filter(|t| keep(&t.repository)) {
        match t.state.as_str() {
            "red" => s.red += 1,
            "timed_out" | "blocked" => s.unjudged += 1,
            "gating" => {
                if let Some(g) = &t.running_gate {
                    s.gate = Some((g.step.max(1), g.of.max(1)));
                }
            }
            _ => {}
        }
    }
    s
}

/// **Whose queues the pane shows** - `/queue` is this session's repository; `/queue all` every
/// repository's; `/queue /PATH` the one at that path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum QueueScope {
    #[default]
    Here,
    All,
    Path(String),
}

impl QueueScope {
    /// The words a `/queue` verb line carries so the daemon acts in the scope the pane shows.
    pub(crate) fn prefix(&self) -> String {
        match self {
            QueueScope::Here => String::new(),
            QueueScope::All => "all ".into(),
            QueueScope::Path(p) => format!("{p} "),
        }
    }
}

/// **A scope, resolved against what the daemon said about this session.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Filter {
    /// Every repository - asked for, or a daemon too old to say which is this session's.
    Every,
    /// The queues' key of this session's repository.
    Repo(String),
    /// **The repositories of a folder that is one project of several** - the edge shows each.
    Repos(Vec<String>),
    /// A path the operator named: a row's repository is it, or holds it.
    Path(std::path::PathBuf),
    /// This session's workspace is in no repository.
    Nowhere(String),
}

impl Filter {
    pub(crate) fn keeps(&self, repository: &str) -> bool {
        match self {
            Filter::Every => true,
            Filter::Repo(r) => r == repository,
            Filter::Repos(rs) => rs.iter().any(|r| r == repository),
            Filter::Path(p) => !repository.is_empty() && p.starts_with(repository),
            Filter::Nowhere(_) => false,
        }
    }
}

/// **The snapshot cut to the repositories `f` keeps** - every list, so the pane's feed and the
/// edge read one scoped copy.
pub(crate) fn scoped(q: &QueueSnapshot, f: &Filter) -> QueueSnapshot {
    if *f == Filter::Every {
        return q.clone();
    }
    QueueSnapshot {
        at_ms: q.at_ms,
        reviews: q
            .reviews
            .iter()
            .filter(|r| f.keeps(&r.repository))
            .cloned()
            .collect(),
        merges: q
            .merges
            .iter()
            .filter(|m| f.keeps(&m.repository))
            .cloned()
            .collect(),
        leases: q
            .leases
            .iter()
            .filter(|l| f.keeps(&l.repository))
            .cloned()
            .collect(),
        targets: q
            .targets
            .iter()
            .filter(|t| f.keeps(&t.repository))
            .cloned()
            .collect(),
        not_queued: q
            .not_queued
            .iter()
            .filter(|r| f.keeps(r))
            .cloned()
            .collect(),
        recent: q
            .recent
            .iter()
            .filter(|m| f.keeps(&m.repository))
            .cloned()
            .collect(),
        archived: q.archived,
        unreadable: q.unreadable.clone(),
        here: q.here.clone(),
    }
}

/// **Which queue the pane is showing** - the operator's *"separate views for review and merge
/// queues"*. `tab` switches; the feed carries the word so the layer draws the right half first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum QueueView {
    #[default]
    Review,
    Merge,
}

impl QueueView {
    pub(crate) fn word(self) -> &'static str {
        match self {
            QueueView::Review => "review",
            QueueView::Merge => "merge",
        }
    }

    pub(crate) fn other(self) -> QueueView {
        match self {
            QueueView::Review => QueueView::Merge,
            QueueView::Merge => QueueView::Review,
        }
    }
}

/// The row a verb would act on.
pub(crate) enum Selected<'a> {
    Review(&'a letibot_sessionlog::event::QueueReview),
    Merge(&'a letibot_sessionlog::event::QueueMerge),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    Approve,
    Veto,
    Restart,
    Rm,
    /// **The row's TARGET gated again** - `/queue regate /REPO TARGET`, about the
    /// target the row lands on rather than the row itself.
    Regate,
}

impl Verb {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Verb::Approve => "approve",
            Verb::Veto => "veto",
            Verb::Restart => "restart",
            Verb::Rm => "rm",
            Verb::Regate => "regate",
        }
    }
}

/// **Whether a verb moves this row - the daemon's own rule** (`mergequeue::verbs`), so a verb it
/// would refuse is said here and not sent. `Err` is why not.
pub(crate) fn verb_moves(verb: Verb, row: &Selected) -> Result<(), String> {
    let ok = match (verb, row) {
        (Verb::Approve, Selected::Review(r)) => {
            r.kind == "branch"
                && matches!(
                    r.state.as_str(),
                    "waiting" | "reviewing" | "rejected" | "cannot_decide" | "blocked"
                )
        }
        (Verb::Approve, Selected::Merge(_)) => false,
        (Verb::Veto, Selected::Review(r)) => review_open(&r.state),
        (Verb::Restart, Selected::Review(r)) => matches!(
            r.state.as_str(),
            "rejected" | "cannot_decide" | "blocked" | "withdrawn"
        ),
        (Verb::Restart, Selected::Merge(m)) => matches!(
            m.state.as_str(),
            "failed" | "timed_out" | "conflict" | "blocked" | "withdrawn"
        ),
        (Verb::Rm, Selected::Review(r)) => matches!(
            r.state.as_str(),
            "waiting" | "reviewing" | "blocked" | "rejected" | "cannot_decide"
        ),
        (Verb::Regate, _) => true,
        // **`rm` is the way out of a row that is going nowhere, `gating` included**. The daemon's
        // `withdraw_merge` takes a `gating` landing too, and a landing whose row has left
        // `gating` does not land whatever its gate said, so the offer is not a lie.
        (Verb::Rm, Selected::Merge(m)) => matches!(
            m.state.as_str(),
            "queued" | "gating" | "blocked" | "failed" | "timed_out" | "conflict"
        ),
        (Verb::Veto, Selected::Merge(m)) => matches!(
            m.state.as_str(),
            "queued" | "blocked" | "failed" | "timed_out" | "conflict"
        ),
    };
    if ok {
        return Ok(());
    }
    let state = match row {
        Selected::Review(r) => &r.state,
        Selected::Merge(m) => &m.state,
    };
    Err(match (verb, row, state.as_str()) {
        (Verb::Approve, Selected::Merge(_), _) => "a merge row is approved already".into(),
        (Verb::Approve, Selected::Review(r), _) if r.kind == "target" => {
            "the red target's review is not a branch to land".into()
        }
        (_, _, "gating") => "the queue is gating it right now".into(),
        (_, _, "landed") => "it has landed".into(),
        (_, _, "approved") => "it is approved and in the merge queue".into(),
        (_, _, s) => format!("it is `{s}`"),
    })
}
