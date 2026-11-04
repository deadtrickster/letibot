//! The quit card: what leaving would stop, and the choices for it - drawn by
//! `rano::agent::quit` from the counts this head keeps.

use crate::app::*;
use crate::ui::render::row_strings;
use rano::agent::quit::QuitCard;
use rano::render::{Line, Span, wrap};
use rano::style::Role;

impl App {
    /// The quit card's rows: what Enter does, and the consequence of it. The consequence rides the
    /// row because it is the whole reason the card exists - the two choices are not alike, and a
    /// card that drew them alike would answer for the operator.
    #[cfg(test)]
    pub(crate) fn quit_choices(&self) -> [(&'static str, String); 2] {
        self.quit_view().choices()
    }

    /// The card's facts: the selection, and what stopping the daemon would end.
    pub(crate) fn quit_view(&self) -> QuitCard {
        QuitCard {
            selected: self.quit_sel,
            other_heads: self.heads.saturating_sub(1),
            running_jobs: self.jobs.iter().filter(|j| j.running).count(),
            running_subagents: self
                .subagents
                .iter()
                .filter(|s| s.state == "running")
                .count(),
        }
    }

    pub(crate) fn quit_card_lines(&self, w: usize) -> Vec<String> {
        let mut rows = row_strings(&self.quit_view().lines(w), self.cfg.palette());
        // The card's own counts are this session's jobs and subagents, so the work that dies with the
        // daemon in OTHER sessions - their turns, a reviewer mid-verdict, a running gate - was invisible
        // and the cheap row and the killing row read alike on a busy box.
        if let Some(said) = self.also_killing() {
            let line = Line::new(vec![Span::role(said, Role::Faint)]);
            rows.extend(
                wrap(&line, w.saturating_sub(8))
                    .iter()
                    .map(|l| format!("       {}", l.plain())),
            );
        }
        rows
    }

    /// What else stopping the daemon ends, or `None` when nothing else is running.
    fn also_killing(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        let others = self
            .sessions
            .iter()
            .filter(|s| s.session_id != self.session_id && s.status.running)
            .count();
        match others {
            0 => {}
            1 => parts.push("1 other session's turn".into()),
            n => parts.push(format!("{n} other sessions' turns")),
        }
        let judging = self
            .queues
            .reviews
            .iter()
            .filter(|r| r.state == "reviewing")
            .count();
        match judging {
            0 => {}
            1 => parts.push("the reviewer's judgement".into()),
            n => parts.push(format!("{n} reviewers' judgements")),
        }
        let mut targets: Vec<&str> = self
            .queues
            .merges
            .iter()
            .filter(|m| m.state == "gating")
            .map(|m| m.target.as_str())
            .collect();
        targets.sort_unstable();
        targets.dedup();
        match targets.len() {
            0 => {}
            1 => parts.push(format!("the gate on `{}`", targets[0])),
            _ => parts.push(format!("the gates on `{}`", targets.join("`, `"))),
        }
        let (last, rest) = parts.split_last()?;
        let listed = match rest {
            [] => last.clone(),
            _ => format!("{} and {last}", rest.join(", ")),
        };
        Some(format!("stopping also kills {listed}."))
    }
}
