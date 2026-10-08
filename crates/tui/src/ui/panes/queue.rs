//! **The merge queue**: what is waiting to land, and a review's output.

use crate::app::*;
use crate::ui::render::{dur_human, sgr, trim_to, wrap};
use crate::ui::*;
use letibot_ui::style::Role;
use letibot_ui::text::without_control_lines;

impl App {
    /// **The merge queue, as rows** — one entry per row plus its state and reason beneath it.
    ///
    /// Two lines an entry, like the jobs pane's rows and for the same reason: the facts a
    /// reader needs are (what is it) and (why is it where it is), and a single line would
    /// truncate the second to make room for the first.
    ///
    /// **The order is the daemon's** and is not sorted here. The queue is the queue's own
    /// scheduling (`created_ms`, then the priority), and a head that re-sorted it would be a
    /// second opinion about what should land next.
    ///
    /// Takes `&mut self` only to record where each row was drawn, which is what the arrows and
    /// a click scroll by.
    pub(crate) fn queue_lines(&mut self, w: usize) -> Vec<String> {
        let mut out = vec![colour(&self.cfg, sgr::BOLD, "merge queue")];
        out.push(String::new());
        if self.merge.is_empty() {
            out.push(dim(
                &self.cfg,
                "    none. A branch lands here when a `task_start` child finishes; nothing \
                 lands without the gatekeeper's verdict and the gate.",
            ));
            self.queue_stop_rows.clear();
            return out;
        }
        let cursor = self.queue_sel.min(self.merge.len().saturating_sub(1));
        let mut stop_rows: Vec<usize> = Vec::with_capacity(self.merge.len());
        for (i, e) in self.merge.iter().enumerate() {
            stop_rows.push(out.len());
            let picked = i == cursor;
            // **The state word is the daemon's**, and the mark is this head's reading of it —
            // the same split the jobs pane keeps. `waiting` is not a problem, `taken` is work
            // in progress, `landed` is done, and the three that park an entry for a person
            // are the loud ones.
            let (mark, tint) = match e.state {
                letibot_sessionlog::event::MergeState::Waiting => ("[ ]", sgr::YELLOW),
                letibot_sessionlog::event::MergeState::Taken => ("[~]", sgr::CYAN),
                letibot_sessionlog::event::MergeState::Landed => ("[x]", sgr::GREEN),
                letibot_sessionlog::event::MergeState::Failed
                | letibot_sessionlog::event::MergeState::Conflict
                | letibot_sessionlog::event::MergeState::Stale => ("[!]", sgr::RED),
            };
            out.push(format!(
                "{} {} {} {}",
                if picked { "\u{25b8}" } else { " " },
                colour(&self.cfg, tint, mark),
                without_control_lines(&e.branch),
                dim(
                    &self.cfg,
                    &format!(
                        "· {} · {}",
                        merge_state_word(e.state),
                        dur_human(self.now_ms.saturating_sub(e.created_ms))
                    )
                )
            ));
            // **The reason, and the review.** An entry that is waiting on a reviewer says so
            // rather than looking like an entry nothing has happened to: *no verdict yet* and
            // *nobody has asked* are different facts and the pane can tell them apart — see
            // [`App::review_of`].
            let review = match self.review_of(&e.id) {
                None => " · nobody has reviewed it".to_string(),
                Some(r) => match r.decision.as_deref() {
                    None => " · the reviewer has been asked and has not answered".to_string(),
                    Some(d) => format!(" · reviewer: {d}"),
                },
            };
            let evidence = if e.evidence.is_empty() {
                String::new()
            } else {
                format!(" — {}", without_control_lines(&e.evidence))
            };
            out.push(dim(
                &self.cfg,
                &trim_to(&format!("         {}{review}{evidence}", e.id), w),
            ));
        }
        out.push(String::new());
        out.push(dim(
            &self.cfg,
            "    enter opens the entry: the ask it was built from, the gate's own words, and \
             the reviewer's verdict",
        ));
        self.queue_stop_rows = stop_rows;
        out
    }

    /// **One entry, whole** — the overlay the queue pane's Enter opens.
    ///
    /// Everything here is a row the head already holds: the entry's own fields, the evidence
    /// (which is the gate's own captured words when the gate failed, and the reviewer's
    /// rendered verdict when the review refused it), and the review record. Nothing is read
    /// again on the keypress, which is why a `MergeEntryMoved` arriving while the overlay is
    /// open shows the NEW state rather than the state at the moment Enter was pressed.
    ///
    /// The entry is looked up by id every draw, and an id the queue no longer holds — a
    /// `recover` moved it, or a head switched and took a fresh snapshot — says so rather than
    /// drawing an empty overlay.
    pub(crate) fn queue_out_lines(&self, w: usize) -> Vec<String> {
        let Some(id) = self.queue_open.as_deref() else {
            return Vec::new();
        };
        let p = self.cfg.palette();
        let mut out = Vec::new();
        let Some(e) = self.merge.iter().find(|e| e.id == id) else {
            out.push(colour(&self.cfg, sgr::BOLD, "merge queue · entry"));
            out.push(String::new());
            out.push(format!(
                "  `{id}` is not in the queue any more. esc goes back to the list."
            ));
            return out;
        };
        out.push(colour(&self.cfg, sgr::BOLD, "merge queue · entry"));
        out.push(String::new());
        for (k, v) in [
            ("branch", e.branch.clone()),
            ("state", merge_state_word(e.state).to_string()),
            ("entry", e.id.clone()),
            ("priority", merge_priority_word(e.priority).to_string()),
            ("base", e.base_sha.clone()),
            ("age", dur_human(self.now_ms.saturating_sub(e.created_ms))),
        ] {
            out.push(format!("  {}", p.paint(Role::Faint, &format!("{k:<9}"))));
            // The value goes on the same row as its label: one `format!` per row rather than
            // two pushes, because a label alone on a line reads as a heading.
            let last = out.len() - 1;
            out[last].push_str(&without_control_lines(&v));
        }
        if let Some(wt) = &e.worktree {
            out.push(format!(
                "  {}{}",
                p.paint(Role::Faint, &format!("{:<9}", "worktree")),
                without_control_lines(wt)
            ));
        }
        if let Some(tip) = &e.landed_sha {
            out.push(format!(
                "  {}{}",
                p.paint(Role::Faint, &format!("{:<9}", "landed")),
                tip
            ));
        }
        if !e.needs.is_empty() {
            out.push(format!(
                "  {}{}",
                p.paint(Role::Faint, &format!("{:<9}", "needs")),
                e.needs.join(", ")
            ));
        }
        out.push(String::new());
        out.push(colour(&self.cfg, sgr::BOLD, "  the ask it was built from"));
        out.push(String::new());
        if e.brief.trim().is_empty() {
            out.push(dim(
                &self.cfg,
                "  (nobody recorded one — the reviewer has nothing to review against)",
            ));
        } else {
            for l in e.brief.lines() {
                out.push(format!("  {}", without_control_lines(l)));
            }
        }
        out.push(String::new());
        out.push(colour(
            &self.cfg,
            sgr::BOLD,
            "  why it is where it is — the queue's own words",
        ));
        out.push(String::new());
        if e.evidence.is_empty() {
            out.push(dim(&self.cfg, "  (nothing yet)"));
        } else {
            // **Not trimmed to one line.** The gate's failure is up to four kilobytes of the
            // CI command's own output — see `EVIDENCE_BYTES` — and this overlay is the only
            // place it is readable at all, so it is wrapped rather than elided and the pane
            // scrolls.
            for l in e.evidence.lines() {
                for wrapped in wrap(l, w.saturating_sub(4)) {
                    out.push(format!("  {}", without_control_lines(&wrapped)));
                }
            }
        }
        out.push(String::new());
        out.push(colour(&self.cfg, sgr::BOLD, "  the reviewer's verdict"));
        out.push(String::new());
        match self.review_of(&e.id) {
            None => out.push(dim(
                &self.cfg,
                "  nobody has asked. An entry with no review does not land — the queue waits.",
            )),
            Some(r) => {
                out.push(format!(
                    "  {}{}",
                    p.paint(Role::Faint, &format!("{:<9}", "decision")),
                    match r.decision.as_deref() {
                        None => "(asked, no answer yet)".to_string(),
                        Some(d) => without_control_lines(d).into_owned(),
                    }
                ));
                out.push(format!(
                    "  {}{}",
                    p.paint(Role::Faint, &format!("{:<9}", "asked")),
                    dur_human(self.now_ms.saturating_sub(r.asked_ms))
                ));
                out.push(format!(
                    "  {}{}",
                    p.paint(Role::Faint, &format!("{:<9}", "session")),
                    // **Where the argument is.** A verdict is a summary of a review, and the
                    // review itself is a conversation — the operator can attach to it with
                    // the session's own id, which is the whole reason the reviewer is a
                    // session and not a call.
                    format!(
                        "{} — attach to it to read the argument",
                        without_control_lines(&r.session_id)
                    )
                ));
                out.push(String::new());
                out.push(dim(&self.cfg, "  reasons"));
                if r.reasons.is_empty() {
                    out.push(dim(&self.cfg, "    (none given)"));
                } else {
                    for reason in &r.reasons {
                        for wrapped in wrap(reason, w.saturating_sub(6)) {
                            out.push(format!("    - {}", without_control_lines(&wrapped)));
                        }
                    }
                }
                out.push(String::new());
                out.push(dim(&self.cfg, "  what it looked at"));
                if r.files.is_empty() && r.commands.is_empty() {
                    out.push(dim(
                        &self.cfg,
                        "    (nothing named — a verdict with no evidence is an opinion)",
                    ));
                } else {
                    for f in &r.files {
                        out.push(format!("    file    {}", without_control_lines(f)));
                    }
                    for c in &r.commands {
                        out.push(format!("    command {}", without_control_lines(c)));
                    }
                }
            }
        }
        out
    }
}

/// **Where a merge-queue entry is, as the head spells it** — the wire's word, and the one the
/// pane draws. A free function rather than a method on the wire type, which this crate does not
/// own, and a `match` rather than a `Debug` format so a state added to the closed set fails to
/// compile here rather than arriving as a word nobody recognises.
pub(crate) fn merge_state_word(state: letibot_sessionlog::event::MergeState) -> &'static str {
    use letibot_sessionlog::event::MergeState as S;
    match state {
        S::Waiting => "waiting",
        S::Taken => "taken",
        S::Landed => "landed",
        S::Failed => "failed",
        S::Conflict => "conflict",
        S::Stale => "stale",
    }
}

/// **An entry's rung, as the head spells it** — [`merge_state_word`]'s sibling, one field over.
pub(crate) fn merge_priority_word(
    priority: letibot_sessionlog::event::MergePriority,
) -> &'static str {
    use letibot_sessionlog::event::MergePriority as P;
    match priority {
        P::Urgent => "urgent",
        P::Subagent => "subagent",
    }
}
