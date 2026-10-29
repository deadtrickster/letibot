//! **The review queue and the merge queue** (R61) — two queues, two views, one pane.
//!
//! The operator: *"review is review and merge is merge. separate steps. separate queues."*, and
//! of the pane before this one: *"it doesnt show me review queue state transitions"*, *"nor there
//! are separate views for review and merge queues"*. The review of 2026-10-11 named what the pane
//! could not say (R61 defect 12): a failed read drawn as an empty queue, an age measured from
//! creation rather than from the current state, no owner and no liveness, and no history.
//!
//! So the pane draws, from the daemon's one snapshot (`QueueSnapshot`):
//!
//! * **The emergencies first** — a red target branch at emergency severity (*"red main is a
//!   high-severity emergency"*), a target timed out or blocked, and a snapshot that could not be
//!   read, said in its own words and never drawn as an empty queue.
//! * **The view's queue** (`tab` switches): every open row, then the recent finished ones. Each
//!   row is ONE line — its state, how long it has been IN that state, and its reason — and the
//!   row under the cursor adds what only it needs: the id `/queue approve` takes, the sha, its
//!   last moves (from, to, when, by whom, why), the gate's steps and its verbs. Enter opens the
//!   row whole with its full history.
//! * **One chrome line under the rows** — who serves each repository (the lease's owner, its
//!   build, how old its heartbeat is; STALE past the lease's lifetime), the repositories that
//!   never asked for a queue, and the archive.
//!
//! The states are the daemon's WORDS (the wire carries words), and the marks are this head's
//! reading of them.
//!
//! **One line per entry is the operator's ruling, not a style** (2026-10-29): *"the queue pane is
//! illegible mess"*, *"the queue is the most automatic and opaque thing in the whole project rn"*.
//! Every entry used to carry a second line whose first field was a 39-character session id — an
//! address that exists so it can be typed into `/queue approve`, repeated on every row and read
//! by nobody — beside the repository and the owner the pane had already named. So a row says a
//! field ONCE (`Said`): the repository only when the snapshot holds more than one, the owner only
//! when the claim is not the serving daemon's.

use crate::app::*;
use crate::ui::render::{dur_human, row_strings, trim_to, visible_width, wrap};
use letibot_sessionlog::event::{
    MergeGateOutcome, MergeGateStep, QueueMerge, QueueMove, QueueReview, QueueSnapshot,
};
use rano::agent::pane::{self, PaneLines};
use rano::agent::text::{clean_line, one};
use rano::render::{Line, Span};
use rano::style::Role;

/// **How long a heartbeat may be silent before the pane calls the owner STALE** — the daemon's
/// lease lifetime (`mergequeue::lease::LEASE_TTL_MS`), so the pane's word and the daemon's
/// takeover agree.
pub(crate) const LEASE_STALE_MS: u64 = 90_000;

/// How often an open pane asks for a fresh snapshot — the heartbeats move without an event.
pub(crate) const REFRESH_MS: u64 = 5_000;

/// How long a finished row stays in the view — the recent past, not every row ever (rows are
/// never deleted; the history is the record).
pub(crate) const RECENT_MS: u64 = 24 * 60 * 60 * 1000;

/// **One row of the current view**, by index into the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QRow {
    Review(usize),
    Merge(usize),
}

fn review_open(state: &str) -> bool {
    matches!(state, "waiting" | "reviewing" | "blocked")
}
fn merge_open(state: &str) -> bool {
    matches!(state, "queued" | "gating" | "blocked")
}

impl App {
    /// **The snapshot's own "now", carried forward by this head's clock since it arrived.**
    fn queue_now(&self) -> u64 {
        let since = self.now_ms.saturating_sub(self.queues_got_ms);
        self.queues.at_ms.saturating_add(since)
    }

    /// **The rows the current view lists, in order** — open rows first (a red target's review
    /// before every other review), oldest first; then the finished ones of the last day, newest
    /// first. The one enumeration the drawn `▸`, the arrows, Enter, a click and a verb read.
    pub(crate) fn queue_rows(&self) -> Vec<QRow> {
        let now = self.queue_now();
        let recent = |ms: u64| now.saturating_sub(ms) <= RECENT_MS;
        match self.queue_view {
            QueueView::Review => {
                let r = &self.queues.reviews;
                let mut open: Vec<usize> =
                    (0..r.len()).filter(|&i| review_open(&r[i].state)).collect();
                open.sort_by_key(|&i| (r[i].kind != "target", r[i].created_ms, r[i].id.clone()));
                let mut done: Vec<usize> = (0..r.len())
                    .filter(|&i| !review_open(&r[i].state) && recent(r[i].state_ms))
                    .collect();
                done.sort_by_key(|&i| std::cmp::Reverse(r[i].state_ms));
                open.into_iter().chain(done).map(QRow::Review).collect()
            }
            QueueView::Merge => {
                let m = &self.queues.merges;
                let mut open: Vec<usize> =
                    (0..m.len()).filter(|&i| merge_open(&m[i].state)).collect();
                open.sort_by_key(|&i| (m[i].fixes.is_none(), m[i].created_ms, m[i].id.clone()));
                let mut done: Vec<usize> = (0..m.len())
                    .filter(|&i| !merge_open(&m[i].state) && recent(m[i].state_ms))
                    .collect();
                done.sort_by_key(|&i| std::cmp::Reverse(m[i].state_ms));
                open.into_iter().chain(done).map(QRow::Merge).collect()
            }
        }
    }

    /// The row the cursor is on, if the view has any.
    pub(crate) fn queue_selected(&self) -> Option<QRow> {
        let rows = self.queue_rows();
        rows.get(self.queue_sel.min(rows.len().saturating_sub(1)))
            .copied()
    }

    /// **How many stops the pane on the screen has** — the plugin view's when a layer is drawing
    /// it, this file's own rows when not. The arrows clamp against this, so the cursor cannot walk
    /// off a list the plugin drew.
    pub(crate) fn queue_stop_count(&self) -> usize {
        if self.queue_plugin_stops > 0 {
            self.queue_plugin_stops
        } else {
            self.queue_rows().len()
        }
    }

    /// **The entry the row under the cursor NAMES** — the id `/queue approve` and the overlay take,
    /// and what the person's `a`/`v`/`r`/`d` act on.
    ///
    /// A plugin row answers with the entry its own act carries (`{"verb":…,"id":…}`), which is
    /// the whole point of `Act::Verb`: a row can draw one entry and mean it, whatever the host's
    /// own enumeration would have put there. The head's own pane answers with the row's id.
    pub(crate) fn queue_selected_id(&self) -> Option<String> {
        if self.queue_plugin_stops > 0 {
            return self.queue_plugin_entry();
        }
        self.queue_selected().map(|row| self.queue_row_id(row))
    }

    /// **Into the session of the reviewer working the row** — the row opened whole, else the one
    /// under the cursor. A row nobody is reviewing says so and goes nowhere.
    pub(crate) fn go_to_reviewer(&mut self) -> Option<Action> {
        let review = match &self.queue_open {
            Some(id) => self.queues.reviews.iter().find(|r| &r.id == id),
            None => self
                .queue_selected_id()
                .and_then(|id| self.queues.reviews.iter().find(|r| r.id == id)),
        };
        let Some(r) = review else {
            self.say("that row is a landing, not a review — it has no reviewer");
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

    /// The id of a row — what `/queue VERB ID` and the overlay take.
    pub(crate) fn queue_row_id(&self, row: QRow) -> String {
        match row {
            QRow::Review(i) => self.queues.reviews[i].id.clone(),
            QRow::Merge(i) => self.queues.merges[i].id.clone(),
        }
    }

    /// **The edge's standings: this session's repository only** (R92 item 5) — whatever scope
    /// the pane is showing, the composer's edge is about the project this session works in. A
    /// session of no repository has none; a daemon too old to say which repository the session
    /// is in gets every repository's, as before.
    pub(crate) fn queue_standings(&self) -> Standings {
        let f = self.queue_filter(&QueueScope::Here);
        standings_where(&self.queues_all, self.queue_now(), |r| f.keeps(r))
    }

    /// **The pane** — see the module docs. Takes `&mut self` to record where each row was drawn
    /// (the arrows and a click read it) and to ask for a fresh snapshot while the pane is open.
    ///
    /// **The plugin's pane, unless it cannot be one.** `plugins/builtin/queue` draws the queue from
    /// the snapshot the head holds (`app::queue_pane`), and `/queue builtin` — or a layer that is
    /// missing, broken or refused — falls back to this file's own rows. Both paths record
    /// `queue_stop_rows`, so the arrows, Enter and a click read what was drawn whichever it was.
    pub(crate) fn queue_lines(&mut self, w: usize) -> Vec<String> {
        if self.now_ms.saturating_sub(self.queues_asked_ms) >= REFRESH_MS {
            self.queues_asked_ms = self.now_ms;
            if !self.queued.contains(&Action::ListMergeQueue) {
                self.queued.push(Action::ListMergeQueue);
            }
        }
        // **A snapshot that has not arrived is nobody's to draw.** The plugin is not asked: an
        // empty `reviews` would be a queue it drew as empty, which is the one thing the host pane
        // never does (R61 defect 12).
        if self.queues_got_ms > 0
            && let Some(lines) = self.queue_plugin_lines(w)
        {
            return lines;
        }
        // The head's own pane from here down: the plugin did not answer, or `/queue builtin`.
        self.queue_held = None;
        self.queue_plugin_stops = 0;
        let now = self.queue_now();
        let mut out = PaneLines::new();
        out.push(pane::title("review queue · merge queue"));
        if self.queues_got_ms == 0 {
            out.blank();
            out.push(pane::faint("    asking the daemon for the queues…"));
            self.queue_stop_rows = Vec::new();
            self.queue_tabs = None;
            return row_strings(&out.lines, self.cfg.palette());
        }
        // ---- Whose queues these are (R92 item 5): this session's repository unless the
        // operator named another scope, and said either way. ONE resolution, because a row's
        // fields are decided by whether this line already names the repository.
        let filter = self.queue_filter(&self.queue_scope);
        match &filter {
            Filter::Nowhere(ws) => {
                out.push(note_line(
                    "◆",
                    Role::Attention,
                    &format!(
                        "this session's workspace {ws} is in no repository, so it has no queue of \
                         its own — `/queue all` shows every repository's queues"
                    ),
                    w,
                ));
                self.queue_stop_rows = Vec::new();
                self.queue_tabs = None;
                return row_strings(&out.lines, self.cfg.palette());
            }
            Filter::Repo(r) => out.push(pane::faint(&trim_to(
                &format!(
                    "    {} — this session's repository · `/queue all` shows every repository",
                    repo_name(r)
                ),
                w,
            ))),
            Filter::Repos(rs) => out.push(pane::faint(&trim_to(
                &format!(
                    "    {} — this session's repositories, one project · `/queue all` shows every \
                     repository",
                    rs.iter()
                        .map(|r| repo_name(r))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                w,
            ))),
            Filter::Path(p) => out.push(pane::faint(&trim_to(
                &format!(
                    "    {} — named · `/queue` alone is this session's repository",
                    p.display()
                ),
                w,
            ))),
            Filter::Every if self.queue_scope == QueueScope::All => {
                out.push(pane::faint(&trim_to(
                    "    every repository · `/queue` alone is this session's repository",
                    w,
                )))
            }
            Filter::Every => out.push(pane::faint(&trim_to(
                "    every repository — this daemon does not say which repository this session \
                 is in",
                w,
            ))),
        }

        // ---- Emergencies and failures to read, first. They are the reason to look at the pane,
        // so they stay above the rows; the lease, the repositories that never asked and the
        // archive are chrome and go in ONE faint line under them.
        let alarms = out.lines.len();
        if let Some(why) = &self.queues.unreadable {
            out.push(note_line(
                "⚠",
                Role::Failure,
                &format!("the store {why}"),
                w,
            ));
        }
        for t in &self.queues.targets {
            let short: String = t.sha.chars().take(10).collect();
            let repo = repo_name(&t.repository);
            match t.state.as_str() {
                "red" => out.push(Line::new(vec![
                    Span::role("  ‼ EMERGENCY ", Role::Failure),
                    Span::role(
                        trim_to(
                            &format!(
                                "`{}` is RED at {short} in {repo} — nothing lands but its fix; its \
                                 tip is first in review · {}",
                                t.target,
                                dur_human(now.saturating_sub(t.checked_ms))
                            ),
                            w.saturating_sub(14),
                        ),
                        Role::Failure,
                    ),
                ])),
                "timed_out" | "blocked" => out.push(note_line(
                    "⚠",
                    Role::Attention,
                    &format!(
                        "`{}` in {repo} is {} at {short}: {} — nothing lands until it is green",
                        t.target,
                        t.state.replace('_', " "),
                        first_line(&t.evidence)
                    ),
                    w,
                )),
                "gating" => out.push(note_line(
                    "…",
                    Role::Pending,
                    &format!(
                        "`{}` in {repo}: {}",
                        t.target,
                        gate_running_words(&t.sha, now.saturating_sub(t.checked_ms))
                    ),
                    w,
                )),
                _ => out.push(note_line(
                    "✓",
                    Role::Success,
                    &format!("`{}` in {repo} is green at {short}", t.target),
                    w,
                )),
            }
        }
        // ---- A landing's gate: what every other landing of that repository waits for.
        for m in self.queues.merges.iter().filter(|m| m.state == "gating") {
            if let Some(sha) = m.rebased_sha.as_deref() {
                out.push(note_line(
                    "…",
                    Role::Pending,
                    &format!(
                        "landing `{}` onto `{}` in {}: {}",
                        clean_line(&m.branch),
                        m.target,
                        repo_name(&m.repository),
                        gate_running_words(sha, now.saturating_sub(m.state_ms))
                    ),
                    w,
                ));
            }
        }
        if out.lines.len() > alarms {
            out.blank();
        }

        // ---- The strip.
        let (strip, at) = tab_row(
            self.queue_view,
            &standings_where(&self.queues, self.queue_now(), |_| true),
        );
        let tabs = QueueTabs {
            row: out.lines.len(),
            ..at
        };
        out.push(strip);
        out.blank();

        // ---- The view's rows.
        let rows = self.queue_rows();
        let cursor = self.queue_sel.min(rows.len().saturating_sub(1));
        if rows.is_empty() {
            out.push(pane::faint(match self.queue_view {
                QueueView::Review => {
                    "    none. A branch is put up for review when a `task_start` child finishes."
                }
                QueueView::Merge => {
                    "    none. A branch lands here once its review approves it, at the head that \
                     was reviewed."
                }
            }));
        }
        let running = gates_running(&self.queues, now);
        let said = Said::of(
            &self.queues,
            matches!(filter, Filter::Repo(_) | Filter::Repos(_)),
        );
        let mut stops = Vec::with_capacity(rows.len());
        for (k, row) in rows.iter().enumerate() {
            let picked = k == cursor;
            stops.push(out.lines.len());
            out.push_stop(match *row {
                QRow::Review(i) => review_row(&self.queues.reviews[i], &said, now, picked, w),
                QRow::Merge(i) => {
                    let m = &self.queues.merges[i];
                    let waits = (m.state == "queued")
                        .then(|| running.get(m.repository.as_str()))
                        .flatten()
                        .map(String::as_str);
                    merge_row(m, &said, now, waits, picked, w)
                }
            });
            // ---- Everything else about a row belongs to the row under the cursor: the id and
            // the sha `/queue approve` takes, its last moves, the gate's steps and the verbs.
            if !picked {
                continue;
            }
            match *row {
                QRow::Review(i) => {
                    let r = &self.queues.reviews[i];
                    out.push(detail_line(&r.id, "head", &r.head_sha, w));
                }
                QRow::Merge(i) => {
                    let m = &self.queues.merges[i];
                    out.push(detail_line(&m.id, "approved", &m.approved_sha, w));
                }
            }
            let id = self.queue_row_id(*row);
            for l in moves_lines(&self.queues.recent, &id, 3, now, w) {
                out.push(l);
            }
            match *row {
                QRow::Review(i) => {
                    out.extend_lines(verb_rows(&Selected::Review(&self.queues.reviews[i]), w))
                }
                QRow::Merge(i) => {
                    let m = &self.queues.merges[i];
                    for l in gate_lines(&m.gate_steps, m.state == "gating", w, GateRows::Pane) {
                        out.push(l);
                    }
                    out.extend_lines(verb_rows(&Selected::Merge(m), w));
                }
            }
        }
        out.blank();
        out.push(self.queue_chrome(now, w));
        self.queue_stop_rows = stops;
        self.queue_tabs = Some(tabs);
        row_strings(&out.lines, self.cfg.palette())
    }

    /// **The pane's ONE chrome line** — who serves each repository and how alive it is, the
    /// repositories that never asked for a queue, and the archive. None of it is what the pane is
    /// opened for and all of it is worth knowing once, so it is one faint line under the rows:
    /// the lease's heartbeat and the STALE warning keep their words, and a stale (or absent)
    /// lease turns the whole line into an alarm.
    fn queue_chrome(&self, now: u64, w: usize) -> Line {
        let mut said: Vec<String> = Vec::new();
        let mut stale = false;
        for l in &self.queues.leases {
            let age = now.saturating_sub(l.heartbeat_ms);
            let old = age > LEASE_STALE_MS;
            stale |= old;
            said.push(format!(
                "{} served by {} (pid {}, {}) · heartbeat {} ago{}",
                repo_name(&l.repository),
                l.host,
                l.pid,
                l.build,
                dur_human(age),
                if old {
                    " — STALE: another daemon takes it over"
                } else {
                    ""
                }
            ));
        }
        if self.queues.leases.is_empty() {
            stale = true;
            said.push("no daemon holds a queue lease — nothing is serving the queues".into());
        }
        // ---- The repositories that are no queue target at all (2026-10-26): the queue is the
        // repositories that asked for it, and asking is a `Merge gate` section.
        if !self.queues.not_queued.is_empty() {
            said.push(format!(
                "not queued: no `Merge gate` section in AGENTS.md — {}",
                self.queues
                    .not_queued
                    .iter()
                    .map(|r| repo_name(r))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if self.queues.archived > 0 {
            said.push(format!(
                "{} row(s) of the pre-split queue are archived (merge_queue_v23)",
                self.queues.archived
            ));
        }
        let role = if stale { Role::Attention } else { Role::Faint };
        Line::new(vec![
            Span::raw("  "),
            Span::role(if stale { "⚠" } else { "◆" }, role),
            Span::raw(" "),
            Span::role(trim_to(&said.join(" · "), w.saturating_sub(4)), role),
        ])
    }

    /// **One row, whole** — the overlay Enter opens: every field the snapshot carries, the
    /// reasons, the gate's steps in full, and the row's moves.
    pub(crate) fn queue_out_lines(&self, w: usize) -> Vec<String> {
        let Some(id) = self.queue_open.as_deref() else {
            return Vec::new();
        };
        let now = self.queue_now();
        let mut lines: Vec<Line> = vec![one(format!("  {id}"), Role::Strong), Line::default()];
        let field = |k: &str, v: &str| -> Vec<Line> {
            wrap(&clean_line(v), w.saturating_sub(16))
                .into_iter()
                .enumerate()
                .map(|(i, l)| {
                    Line::new(vec![
                        Span::role(
                            if i == 0 {
                                format!("  {k:<12}  ")
                            } else {
                                " ".repeat(16)
                            },
                            Role::Faint,
                        ),
                        Span::raw(l),
                    ])
                })
                .collect()
        };
        if let Some(r) = self.queues.reviews.iter().find(|r| r.id == id) {
            lines.extend(field("queue", "review"));
            lines.extend(field("kind", &r.kind));
            lines.extend(field("repository", &r.repository));
            lines.extend(field("target", &r.target));
            lines.extend(field("branch", &r.branch));
            lines.extend(field(
                "state",
                &format!(
                    "{} · {} in it",
                    r.state,
                    dur_human(now.saturating_sub(r.state_ms))
                ),
            ));
            if !r.owner.is_empty() {
                lines.extend(field("owner", &r.owner));
            }
            if let Some(c) = &r.reviewer {
                lines.extend(field("reviewer", c));
            }
            lines.extend(field("head", &r.head_sha));
            if let Some(v) = &r.verdict {
                lines.extend(field(
                    "verdict",
                    &format!(
                        "{v} at {} by {}",
                        r.reviewed_sha.as_deref().unwrap_or("?"),
                        r.verdict_by.as_deref().unwrap_or("?")
                    ),
                ));
            }
            for reason in &r.reasons {
                lines.extend(field("reason", reason));
            }
            if r.attempts > 0 {
                lines.extend(field(
                    "attempts",
                    &format!("{} — {}", r.attempts, r.failure),
                ));
            }
            if let Some(f) = &r.fixes {
                lines.extend(field("fixes", f));
            }
            lines.extend(field("why", &r.evidence));
            lines.extend(field("ask", &r.brief));
        } else if let Some(m) = self.queues.merges.iter().find(|m| m.id == id) {
            lines.extend(field("queue", "merge"));
            lines.extend(field("repository", &m.repository));
            lines.extend(field("target", &m.target));
            lines.extend(field("branch", &m.branch));
            lines.extend(field(
                "state",
                &format!(
                    "{} · {} in it",
                    m.state,
                    dur_human(now.saturating_sub(m.state_ms))
                ),
            ));
            if !m.owner.is_empty() {
                lines.extend(field("owner", &m.owner));
            }
            lines.extend(field(
                "approved",
                &format!(
                    "{} by {} (review {})",
                    m.approved_sha, m.approved_by, m.review_id
                ),
            ));
            if let Some(s) = &m.rebased_sha {
                lines.extend(field("rebased", s));
            }
            if let Some(s) = &m.landed_sha {
                lines.extend(field("landed", s));
            }
            if !m.pushed.is_empty() {
                lines.extend(field("push", &m.pushed));
            }
            lines.extend(field("why", &m.evidence));
            lines.push(Line::default());
            lines.push(one("  the gate's steps", Role::Strong));
            lines.extend(gate_lines(
                &m.gate_steps,
                m.state == "gating",
                w,
                GateRows::Whole,
            ));
        } else {
            lines.push(pane::faint(
                "    the row is not in the last snapshot — it may have moved out of the window",
            ));
        }
        lines.push(Line::default());
        lines.push(one("  its moves", Role::Strong));
        lines.extend(moves_lines(&self.queues.recent, id, usize::MAX, now, w));
        row_strings(&lines, self.cfg.palette())
    }
}

/// The repository's last path component — the name a person knows it by.
pub(crate) fn repo_name(repo: &str) -> String {
    if repo.is_empty() {
        return "(unresolved)".into();
    }
    std::path::Path::new(repo)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| repo.to_string())
}

fn first_line(s: &str) -> String {
    clean_line(s.lines().next().unwrap_or_default())
}

/// The owner as a person reads it: `host/pid`, the start stamp dropped.
pub(crate) fn owner_word(owner: &str) -> String {
    let mut parts = owner.split('/');
    match (parts.next(), parts.next()) {
        (Some(h), Some(p)) => format!("{h}/{p}"),
        _ => owner.to_string(),
    }
}

pub(crate) fn mark_of(state: &str) -> (&'static str, Role) {
    match state {
        "waiting" | "queued" => ("[ ]", Role::Pending),
        "reviewing" | "gating" => ("[~]", Role::Code),
        "approved" | "landed" => ("[x]", Role::Success),
        "blocked" | "failed" | "timed_out" | "conflict" => ("[!]", Role::Failure),
        "rejected" | "cannot_decide" => ("[!]", Role::Attention),
        _ => ("[-]", Role::Faint),
    }
}

/// **How many repositories the snapshot holds** — a row says which one it is of when there is more
/// than one to tell apart (see [`Said`]).
fn repositories(q: &QueueSnapshot) -> usize {
    let mut seen: Vec<&str> = q
        .reviews
        .iter()
        .map(|r| r.repository.as_str())
        .chain(q.merges.iter().map(|m| m.repository.as_str()))
        .collect();
    seen.sort_unstable();
    seen.dedup();
    seen.len()
}

/// **What the pane has already said**, so a row says a field once — the repository when the scope
/// line above has named it (and there is one to name), and the daemon serving it: a claim by the
/// daemon holding that repository's lease is this pane's own, and `by …` is for a claim from
/// somewhere else (a stale one, or another machine's).
struct Said<'a> {
    q: &'a QueueSnapshot,
    name_repository: bool,
}

impl<'a> Said<'a> {
    /// `scoped` is whether the pane's scope line already names the repositories the rows are in —
    /// `/queue` and `/queue /PATH`-of-one do, `every repository` does not, and then a row has to
    /// say which one it is of even when there is only one.
    fn of(q: &'a QueueSnapshot, scoped: bool) -> Said<'a> {
        Said {
            q,
            name_repository: !scoped || repositories(q) > 1,
        }
    }

    fn serves(&self, repository: &str, owner: &str) -> bool {
        let who = owner_word(owner);
        self.q
            .leases
            .iter()
            .any(|l| l.repository == repository && format!("{}/{}", l.host, l.pid) == who)
    }
}

/// **A row's one line** — its mark, its branch, the state it has been in for how long, whatever
/// else the pane has not already said, and then the REASON, which is what the row is for. The
/// reason is placed after the fixed fields and given every column they leave, so a long one is
/// cut at the pane's edge rather than crowding the row out of the pane.
fn row_line(
    picked: bool,
    mark: &str,
    role: Role,
    what: &str,
    what_role: Role,
    tail: &str,
    why: &str,
    w: usize,
) -> Line {
    let left = format!("{mark} {what} {tail}");
    // The mark column (`▸ ` or `  `) is drawn before this, so it is spent here too.
    let room = w.saturating_sub(visible_width(&left) + 2 + 3);
    let said = if why.is_empty() || room < 16 {
        String::new()
    } else {
        format!(" · {}", trim_to(why, room))
    };
    Line::new(vec![
        Span::raw(format!("{} ", pane::mark(picked))),
        Span::role(mark, role),
        Span::role(format!(" {what} "), what_role),
        Span::role(format!("{tail}{said}"), Role::Faint),
    ])
}

/// **The picked row's second line** — the id `/queue approve` takes and the sha it was judged at,
/// and nothing else: everything else about the row is one keypress away (`enter`).
fn detail_line(id: &str, what: &str, sha: &str, w: usize) -> Line {
    let short: String = sha.chars().take(10).collect();
    let mut said = id.to_string();
    if !short.is_empty() {
        said.push_str(&format!(" · {what} {short}"));
    }
    pane::faint(trim_to(&format!("      {said}"), w))
}

fn review_row(r: &QueueReview, said: &Said, now: u64, picked: bool, w: usize) -> Line {
    let (mark, role) = mark_of(&r.state);
    let what = if r.kind == "target" {
        format!("RED TARGET `{}`", r.target)
    } else {
        clean_line(&r.branch)
    };
    let state = r.state.replace('_', " ");
    let mut tail = format!(
        "· {state} for {}",
        dur_human(now.saturating_sub(r.state_ms))
    );
    if said.name_repository {
        tail.push_str(&format!(" · {}", repo_name(&r.repository)));
    }
    if !r.owner.is_empty() && !said.serves(&r.repository, &r.owner) {
        tail.push_str(&format!(" · by {}", owner_word(&r.owner)));
    }
    // **The verdict, unless it is the state said twice** — a `cannot_decide` review whose verdict
    // is `cannot_decide` has one fact, not two.
    let mut why = match &r.verdict {
        Some(v) if v.replace('_', " ") != state => v.clone(),
        _ => String::new(),
    };
    if !r.evidence.is_empty() {
        if !why.is_empty() {
            why.push_str(" — ");
        }
        why.push_str(&first_line(&r.evidence));
    }
    row_line(
        picked,
        mark,
        role,
        &what,
        if r.kind == "target" {
            Role::Failure
        } else {
            Role::Strong
        },
        &tail,
        &why,
        w,
    )
}

/// **`gate running on <sha> for Nm`** — the words for a gate in flight, from when it started.
pub(crate) fn gate_running_words(sha: &str, for_ms: u64) -> String {
    let short: String = sha.chars().take(10).collect();
    format!("gate running on {short} for {}m", for_ms / 60_000)
}

/// **The gate each repository is running** (one at a time: R92 item 8) — what its queued
/// landings wait for. The store has it: a `gating` target since its `checked_ms`, or a `gating`
/// landing since its `state_ms`, rebased.
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

fn merge_row(
    m: &QueueMerge,
    said: &Said,
    now: u64,
    waits: Option<&str>,
    picked: bool,
    w: usize,
) -> Line {
    let (mark, role) = mark_of(&m.state);
    let mut tail = format!(
        "· {} for {} · onto `{}`",
        m.state.replace('_', " "),
        dur_human(now.saturating_sub(m.state_ms)),
        m.target
    );
    if said.name_repository {
        tail.push_str(&format!(" · {}", repo_name(&m.repository)));
    }
    if !m.owner.is_empty() && !said.serves(&m.repository, &m.owner) {
        tail.push_str(&format!(" · by {}", owner_word(&m.owner)));
    }
    if m.fixes.is_some() {
        tail.push_str(" · FIX for the red target");
    }
    // **What a queued landing waits for** (R92 item 8) — the repository's gate, on its own line
    // no longer: it is the row's reason, so it goes where the reason goes.
    let mut why = waits.map(|g| format!("waits: {g}")).unwrap_or_default();
    if !m.evidence.is_empty() {
        if !why.is_empty() {
            why.push_str(" — ");
        }
        why.push_str(&first_line(&m.evidence));
    }
    row_line(
        picked,
        mark,
        role,
        &clean_line(&m.branch),
        Role::Strong,
        &tail,
        &why,
        w,
    )
}

/// **A row's moves** — the last `n` of them, from the snapshot's recent history.
fn moves_lines(recent: &[QueueMove], id: &str, n: usize, now: u64, w: usize) -> Vec<Line> {
    let mine: Vec<&QueueMove> = recent.iter().filter(|t| t.entry_id == id).collect();
    let from = mine.len().saturating_sub(n);
    if mine.is_empty() {
        return vec![note_line(
            "·",
            Role::Faint,
            "no moves in the recent history",
            w,
        )];
    }
    mine[from..]
        .iter()
        .map(|t| {
            note_line(
                "↳",
                Role::Faint,
                &format!(
                    "{} → {} · {} ago · {} — {}",
                    t.from.as_deref().unwrap_or("(new)"),
                    t.to,
                    dur_human(now.saturating_sub(t.at_ms)),
                    owner_word(&t.by),
                    first_line(&t.why)
                ),
                w,
            )
        })
        .collect()
}

/// **What the queue is DOING, in four columns** — drawn on the composer's bottom edge, and the
/// subject of [`standings_where`]'s reading of the snapshot.
///
/// **The third column is `stuck`, and the word is the operator's** (2026-10-29): *"wtf parked
/// means?"*, then *"so, label it as such - stuck?"*. `parked` was this head's word for a row the
/// queue had stopped on, and it said nothing a reader could act on — a failed landing and a
/// review nobody could run are not *parked*, they are stuck, and the column says what the reader
/// has to do about them. **The store's words are untouched** — a merge row's own states are
/// still `blocked`, `failed`, `timed_out` and `conflict` (`MergeState`), no table in this tree
/// has a `parked` in it, and the rename is the COLUMN's: what a person reads, not what the queue
/// records.
///
/// **The fourth is `landed`, and it counts a DAY** — the operator: *"i definitely want to see
/// landed counter after parked"*. For ever would be a number that only grows, so it is the same
/// window the pane's done list uses ([`RECENT_MS`], the last day): a queue that has worked all
/// week reads as today's work, and the number falls back to zero on its own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Standings {
    pub(crate) review: usize,
    pub(crate) merging: usize,
    /// **Open and stopped** — the rows a person has to move (see [`standings_where`]).
    pub(crate) stuck: usize,
    /// **Landed inside the last day** — not for ever.
    pub(crate) landed: usize,
    /// Target branches that are red — the emergency.
    pub(crate) red: usize,
    /// **Target branches nobody could judge** — timed out or blocked (R92 item 5). Nothing lands
    /// on them either, so they are an alarm too, not a quiet edge.
    pub(crate) unjudged: usize,
}

impl Standings {
    pub(crate) fn words(&self) -> Option<String> {
        // **A landing is a standing** — the operator asked for the count, so it is drawn on its
        // own: a queue that landed two branches today and holds nothing else still has something
        // to say. Only a queue that has stood nothing and landed nothing for a day is silent.
        let standing =
            self.review + self.merging + self.stuck + self.landed + self.red + self.unjudged;
        (standing > 0).then(|| {
            let mut s = format!(
                "{} in review · {} being merged · {} stuck · {} landed",
                self.review, self.merging, self.stuck, self.landed
            );
            if self.unjudged > 0 {
                s = format!("⚠ {} target(s) unjudged · {s}", self.unjudged);
            }
            if self.red > 0 {
                s = format!("‼ {} red target(s) · {s}", self.red);
            }
            s
        })
    }

    pub(crate) fn role(&self) -> Role {
        if self.red > 0 {
            Role::Failure
        } else if self.unjudged > 0 || self.stuck > 0 {
            Role::Attention
        } else {
            Role::Pending
        }
    }
}

/// **The standings of the rows `keep` admits by repository**, as of `now` — the snapshot's own
/// clock carried forward (`App::queue_now`), because the landed column asks about a window and
/// the snapshot's `at_ms` is a moment, not a clock.
///
/// # The snapshot is the wire's whole history, and these counters are a reading of it
///
/// `mergequeue::snapshot` reads `review_rows(None)` and `merge_rows(None)`: every row the store
/// has, in every state, because nothing in this tree deletes one (the history IS the record). So
/// the terminal rows are in the `QueueSnapshot` this head holds, and a counter that takes them at
/// face value is a counter about the past drawn where the reader is asking about now.
///
/// **That is what `parked` was doing** (measured 2026-10-29, reading): `rejected` was counted
/// into the column, and a rejected review is terminal — the reviewer's complaints have gone back
/// to the branch's session and nothing will retry the row — so it sat in a live column for ever,
/// beside a `blocked` landing somebody still had to move. The words changed to `stuck` and the
/// counting was fixed with them:
///
/// * **`in review`** — `waiting` and `reviewing`, the review queue's open states.
/// * **`being merged`** — `queued` and `gating`, the merge queue's working states.
/// * **`stuck`** — open and stopped: a review that could not be RUN (`blocked`) or whose reviewer
///   said `needs_human` (`cannot_decide`), and a landing whose gate was red (`failed`), ran out
///   of time (`timed_out`), would not rebase (`conflict`) or could not be started (`blocked`).
///   Each is a person's to move (`restart`, `approve`, `regate`), which is the test the column
///   applies: a row nothing will retry and nobody has to touch is not stuck.
/// * **`landed`** — the store's own `state`, and its own `state_ms` inside [`RECENT_MS`].
///
/// **Every other state is terminal and is counted nowhere**: `approved` (the review queue's
/// terminal state — the row is the merge queue's business from that moment, so counting it here
/// would count one branch twice), `rejected`, `withdrawn` and `archived` on a review; `landed`,
/// `moved` and `withdrawn` on a landing. A terminal row never inflates a live column.
///
/// A target's own red-tip review is not counted once that target is green again — the alarm
/// clears with the target, not with whoever withdraws the row.
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
            // is for — the one state in it the store does not call open.
            "blocked" | "cannot_decide" => s.stuck += 1,
            _ => {}
        }
    }
    for m in q.merges.iter().filter(|m| keep(&m.repository)) {
        match m.state.as_str() {
            "queued" | "gating" => s.merging += 1,
            "blocked" | "failed" | "timed_out" | "conflict" => s.stuck += 1,
            // **The store's own state and its own stamp**: `state_ms` is when the row entered
            // `landed` (`gating_to`), and `landed_sha` is the tree it landed. Outside the window
            // it is a landing this edge no longer counts.
            "landed" if recent(m.state_ms) => s.landed += 1,
            _ => {}
        }
    }
    for t in q.targets.iter().filter(|t| keep(&t.repository)) {
        match t.state.as_str() {
            "red" => s.red += 1,
            "timed_out" | "blocked" => s.unjudged += 1,
            _ => {}
        }
    }
    s
}

/// **Whose queues the pane shows** (R92 item 5) — the operator: *"the /queue is still not
/// project scoped"*. `/queue` is this session's repository; `/queue all` every repository's;
/// `/queue /PATH` the one at that path.
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
    /// Every repository — asked for, or a daemon too old to say which is this session's.
    Every,
    /// The queues' key of this session's repository.
    Repo(String),
    /// **The repositories of a folder that is one project of several** — the edge shows each.
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

/// **The snapshot cut to the repositories `f` keeps** — every list, so the pane, its rows, its
/// verbs and the overlay all read one scoped copy.
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

impl App {
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

    /// **The pane's copy, cut again** — after a snapshot lands or the scope changes.
    pub(crate) fn rescope_queues(&mut self) {
        let f = self.queue_filter(&self.queue_scope);
        self.queues = scoped(&self.queues_all, &f);
        let n = self.queue_rows().len();
        self.queue_sel = self.queue_sel.min(n.saturating_sub(1));
    }

    /// **`/queue all` and `/queue /PATH`** — the pane, on that scope. Opening it asks for a
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

/// **Which queue the pane is showing** — the operator's *"separate views for review and merge
/// queues"*. `tab` switches.
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

/// Where the two tabs were drawn — the record a click reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueueTabs {
    pub(crate) row: usize,
    pub(crate) review: (usize, usize),
    pub(crate) merge: (usize, usize),
}

const TAB_AT: &str = "  ";
const TAB_KEY: &str = " · tab switches";

fn tab_row(view: QueueView, s: &Standings) -> (Line, QueueTabs) {
    let label = |v: QueueView, n: usize| {
        let word = format!("{} queue ({n})", v.word());
        if v == view {
            (format!("[{word}]"), Role::Strong)
        } else {
            (format!(" {word} "), Role::Faint)
        }
    };
    let (review, review_role) = label(QueueView::Review, s.review);
    let (merge, merge_role) = label(QueueView::Merge, s.merging);
    let review_from = TAB_AT.len();
    let merge_from = review_from + review.chars().count();
    let merge_to = merge_from + merge.chars().count();
    let line = Line::new(vec![
        Span::raw(TAB_AT),
        Span::role(review, review_role),
        Span::role(merge, merge_role),
        Span::role(TAB_KEY, Role::Faint),
    ]);
    (
        line,
        QueueTabs {
            row: 0,
            review: (review_from, merge_from),
            merge: (merge_from, merge_to),
        },
    )
}

/// The row a verb would act on.
pub(crate) enum Selected<'a> {
    Review(&'a QueueReview),
    Merge(&'a QueueMerge),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    Approve,
    Veto,
    Restart,
    Rm,
    /// **The row's TARGET gated again** (R76 item 3) — `/queue regate /REPO TARGET`, about the
    /// target the row lands on rather than the row itself.
    Regate,
}

impl Verb {
    pub(crate) fn key(self) -> char {
        match self {
            Verb::Approve => 'a',
            Verb::Veto => 'v',
            Verb::Restart => 'r',
            Verb::Rm => 'd',
            Verb::Regate => 'g',
        }
    }

    pub(crate) fn word(self) -> &'static str {
        match self {
            Verb::Approve => "approve",
            Verb::Veto => "veto",
            Verb::Restart => "restart",
            Verb::Rm => "rm",
            Verb::Regate => "regate",
        }
    }

    fn does(self) -> &'static str {
        match self {
            Verb::Approve => {
                "your approval over the reviewer, at the branch's head now — the gate still runs"
            }
            Verb::Veto => "rejected in your words, sent back to the child",
            Verb::Restart => "back in line: reviewed, or gated, again",
            Verb::Rm => "withdrawn — the branch and its worktree stay, the history keeps the row",
            Verb::Regate => "the target's tip gated again, even if it was judged",
        }
    }
}

/// **Whether a verb moves this row — the daemon's own rule** (`mergequeue::verbs`), so a verb it
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
        (Verb::Veto | Verb::Rm, Selected::Merge(m)) => matches!(
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

fn verb_rows(row: &Selected, w: usize) -> Vec<Line> {
    let mut said = String::new();
    for (i, verb) in [Verb::Approve, Verb::Veto, Verb::Restart, Verb::Rm]
        .into_iter()
        .enumerate()
    {
        if i > 0 {
            said.push_str(" · ");
        }
        match verb_moves(verb, row) {
            Ok(()) => said.push_str(&format!("{} {} ({})", verb.key(), verb.word(), verb.does())),
            Err(why) => said.push_str(&format!("{} {} (nothing: {why})", verb.key(), verb.word())),
        }
    }
    let room = w.saturating_sub(STEP_AT.len() + 6);
    wrap(&clean_line(&said), room)
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            Line::new(vec![
                Span::raw(STEP_AT),
                Span::role(if i == 0 { "verbs " } else { "      " }, Role::Faint),
                Span::raw(l),
            ])
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateRows {
    Pane,
    Whole,
}

const STEP_AT: &str = "         ";
const OUTPUT_AT: &str = "             ";
const FAILURE_LINES: usize = 4;

fn gate_lines(steps: &[MergeGateStep], running: bool, w: usize, how: GateRows) -> Vec<Line> {
    if steps.is_empty() {
        return vec![if running {
            note_line("…", Role::Pending, "the gate is running", w)
        } else {
            note_line("·", Role::Faint, "the gate has not run on this row", w)
        }];
    }
    let mut out = Vec::new();
    if running {
        out.push(note_line(
            "…",
            Role::Pending,
            "the gate is running — the rows below are the last run's",
            w,
        ));
    }
    for step in steps {
        out.extend(step_lines(step, w, how));
    }
    out
}

fn step_lines(step: &MergeGateStep, w: usize, how: GateRows) -> Vec<Line> {
    use MergeGateOutcome as O;
    if step.outcome == O::NoGate {
        return vec![note_line(
            "⚠",
            Role::Attention,
            &format!("no gate declared — {}", step.output),
            w,
        )];
    }
    let (mark, role) = match step.outcome {
        O::Passed => ("✓", Role::Success),
        O::Failed => ("✗", Role::Failure),
        O::TimedOut => ("⧗", Role::Failure),
        O::Killed => ("✗", Role::Attention),
        O::NotRun | O::NoGate => ("·", Role::Faint),
    };
    let mut said = clean_line(&step.command);
    match step.outcome {
        O::Passed | O::Failed => said.push_str(&format!(" · {}", dur_human(step.elapsed_ms))),
        O::TimedOut => said.push_str(&format!(
            " · TIMED OUT after {}",
            dur_human(step.elapsed_ms)
        )),
        O::Killed => said.push_str(" · killed"),
        O::NotRun | O::NoGate => said.push_str(" · not run"),
    }
    let mut out = vec![Line::new(vec![
        Span::raw(STEP_AT),
        Span::role(mark, role),
        Span::raw(" "),
        Span::raw(trim_to(&said, w.saturating_sub(STEP_AT.len() + 2))),
    ])];
    if matches!(step.outcome, O::Failed | O::TimedOut) {
        let lines: Vec<&str> = step.output.lines().collect();
        let from = match how {
            GateRows::Pane => lines.len().saturating_sub(FAILURE_LINES),
            GateRows::Whole => 0,
        };
        let room = w.saturating_sub(OUTPUT_AT.len());
        for l in &lines[from..] {
            for piece in wrap(&clean_line(l), room) {
                out.push(Line::new(vec![Span::raw(OUTPUT_AT), Span::raw(piece)]));
                if how == GateRows::Pane {
                    break;
                }
            }
        }
    }
    out
}

fn note_line(mark: &str, role: Role, text: &str, w: usize) -> Line {
    Line::new(vec![
        Span::raw("  "),
        Span::role(mark, role),
        Span::raw(" "),
        Span::raw(trim_to(&clean_line(text), w.saturating_sub(4))),
    ])
}

/// `PaneLines` has no bulk push; this is the one the pane uses.
trait ExtendLines {
    fn extend_lines(&mut self, lines: Vec<Line>);
}

impl ExtendLines for PaneLines {
    fn extend_lines(&mut self, lines: Vec<Line>) {
        for l in lines {
            self.push(l);
        }
    }
}
