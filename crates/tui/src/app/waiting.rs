//! **What waits on you, folded away** — the operator, 2026-10-15: *"so that I can hide 'waits on
//! you' todos questions - they are in yellow and very visible - very good, but sometimes i need to
//! do additional digging and they just keep space and irritate. we need some counter in the right
//! bottom corner near triangle - yellow and visible"*, and *"also a way for me to see all
//! questions without answering them"*.
//!
//! alt-h folds the yellow `WAITING ON YOU` / `PUT AWAY` line ([`App::waiting_line`]) and the
//! composer's bottom edge says `N waiting · alt-h` instead; alt-h again unfolds it. **A question
//! that arrives after the fold is news**, so the line comes back by itself: the fold remembers
//! what it hid, not that it hid. `/asks` lists every question waiting, whole, without opening any
//! card.

use super::*;
use crate::app::todos::TODO_ASK;

impl App {
    /// **Everything the waiting line speaks for**, one key each: the operator's own command
    /// blocked on a line, every question or permission card put away in this session, every todo
    /// row's question. What a fold remembers, and what decides that the fold still holds.
    pub(crate) fn waiting_keys(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(p) = self.prompt.as_ref().filter(|_| self.prompt_away) {
            out.push(format!("cmd:{}", p.command));
        }
        for a in &self.away {
            if !a.is_todo() && a.session_id == self.session_id {
                out.push(a.decision.req_id.clone());
            }
        }
        for a in self.operator_asks() {
            out.push(format!("{TODO_ASK}{}\u{1f}{}", a.content, a.question));
        }
        out
    }

    /// **Whether the waiting line is folded now**: a fold holds while everything waiting is
    /// something it hid. A new question, or nothing waiting at all, ends it.
    pub(crate) fn asks_folded(&self) -> bool {
        let Some(hidden) = &self.asks_hidden else {
            return false;
        };
        let now = self.waiting_keys();
        !now.is_empty() && now.iter().all(|k| hidden.contains(k))
    }

    /// The count the bottom edge draws while folded: how many things wait on you.
    pub(crate) fn asks_folded_count(&self) -> Option<usize> {
        self.asks_folded().then(|| self.waiting_keys().len())
    }

    /// **alt-h** — fold what waits on you away, or bring it back.
    pub(crate) fn toggle_asks_folded(&mut self) {
        if self.asks_folded() {
            self.asks_hidden = None;
            self.redraw = true;
            return;
        }
        let keys = self.waiting_keys();
        if keys.is_empty() {
            self.say("nothing is waiting on you");
            return;
        }
        let n = keys.len();
        self.asks_hidden = Some(keys);
        self.say(&format!(
            "{n} waiting on you, folded away — the box's edge counts them; alt-h shows them again, \
             /asks lists them, alt-a answers. A new one shows itself."
        ));
        self.redraw = true;
    }

    /// **`/asks`: every question waiting on you, whole, answering none** — the cards put away,
    /// then the todo rows' questions, each with its options.
    pub(crate) fn asks_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(p) = self.prompt.as_ref().filter(|_| self.prompt_away) {
            out.push(format!("your command is waiting: `{}`", p.command.trim()));
            if let Some(q) = &p.question {
                out.push(format!("  asks \"{}\"", q.trim()));
            }
            out.push(String::new());
        }
        for a in self
            .away
            .iter()
            .filter(|a| !a.is_todo() && a.session_id == self.session_id)
        {
            let d = &a.decision;
            let kind = if d.kind == "question" {
                "question"
            } else {
                "permission"
            };
            out.push(format!("{kind}, put away: \"{}\"", d.summary.trim()));
            if !d.target.trim().is_empty() {
                out.push(format!("  on {}", d.target.trim()));
            }
            for c in &d.choices {
                out.push(format!("  · {}", c.trim()));
            }
            if let Some(at) = d.deadline {
                out.push(format!(
                    "  {} left",
                    crate::ui::status::time_left(at.saturating_sub(self.now_ms))
                ));
            }
            out.push(String::new());
        }
        for a in self.operator_asks() {
            out.push(format!(
                "on `{}`: \"{}\"",
                letibot_sessionlog::event::todo_title(&a.content),
                a.question.trim()
            ));
            for o in &a.options {
                out.push(format!("  · {}", o.trim()));
            }
            out.push(String::new());
        }
        if out.is_empty() {
            out.push("nothing is waiting on you".into());
        } else {
            out.push("alt-a answers the first · alt-h folds the line away".into());
        }
        out
    }
}
