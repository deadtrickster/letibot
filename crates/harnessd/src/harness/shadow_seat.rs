//! The shadow's hooks into a session: the position its verdicts are stamped with, the turn_end
//! trigger, and what a verdict does once it arrives. The decisions are `crate::shadow::turn_end`'s.

use super::*;
use crate::shadow::{Job, runner::Mode, turn_end};

impl Harness {
    /// Open the shadow once the session is stored, and move its position to the rows that count:
    /// an operator message or an answer. Harness notices and display rows do not move it.
    pub(crate) fn tend_shadow(&mut self) {
        if matches!(self.shadow, crate::shadow::Slot::NotYet) {
            let bell = self.session_registry.bell().clone();
            let id = self.cfg.session_id.clone();
            let wake: crate::shadow::Wake = Arc::new(move || bell.ring_wake(&id));
            self.shadow = crate::shadow::open_for(&self.cfg, &self.model_name(), wake);
        }
        let Some(s) = self.shadow.get() else {
            return;
        };
        let (from, mut n) = self.shadow_counted;
        let items = &self.session.items;
        for i in items.iter().skip(from) {
            if matches!(
                i,
                TranscriptItem::Assistant { .. }
                    | TranscriptItem::User {
                        speaker: letibot_transcript::Speaker::Operator,
                        ..
                    }
            ) {
                n += 1;
            }
        }
        self.shadow_counted = (items.len(), n);
        s.advance(n);
    }

    /// Whether the turn_end job takes this turn end: on, and not paused by its breaker. A paused
    /// job leaves the regex nudge to act at once, as it does with no shadow.
    pub(crate) fn shadow_takes_turn_end(&self) -> bool {
        self.shadow.get().is_some_and(|s| {
            s.states()
                .iter()
                .any(|(j, st)| *j == Job::TurnEnd && st.mode != Mode::Paused)
        })
    }

    /// Queue the turn_end check for the turn that just ended with `reply`.
    pub(crate) fn submit_turn_end(&mut self, reply: &str) {
        self.tend_shadow();
        let todos = self.todos.snapshot();
        let mut blocked = Vec::new();
        if reply.trim_end().ends_with('?') {
            blocked.push("the answer ends in a question to the operator".to_string());
        }
        if !letibot_tokencore::store::operator_asks(&todos).is_empty() {
            blocked.push("a todo row is waiting on the operator".to_string());
        }
        let operator = turn_end::last_operator(&self.session.items);
        let shown = turn_end::shown(operator.as_deref(), reply, &todos, &blocked);
        if let Some(s) = self.shadow.get() {
            s.submit(Job::TurnEnd, shown);
        }
    }

    /// The verdicts that arrived since the last wake, acted on: the notices a wake says, and the
    /// display rows (a proposed note) appended now.
    pub(crate) fn shadow_notices(&mut self) -> Vec<String> {
        let Some((events, recorder, shadow_id)) = self.shadow.parts() else {
            return Vec::new();
        };
        let events: Vec<crate::shadow::Event> = {
            let mut q = events.lock().unwrap_or_else(|e| e.into_inner());
            q.drain(..).collect()
        };
        let mut notices = Vec::new();
        let mut rows = Vec::new();
        for e in events {
            if e.job != Job::TurnEnd {
                continue;
            }
            let held = self.shadow_held.take();
            let reminded = turn_end::reminded_since_operator(&self.session.items);
            let (act, said) = turn_end::decide(&e.outcome, held.as_deref(), reminded, &shadow_id);
            if let Some(said) = said {
                recorder.row(&said, "");
            }
            match act {
                turn_end::Act::Remind(t) | turn_end::Act::Regex(t) => notices.push(t),
                turn_end::Act::Nothing => {}
            }
            if let crate::shadow::runner::Outcome::Verdict(
                crate::shadow::verdict::Verdict::TurnEnd {
                    note: Some(quote), ..
                },
            ) = &e.outcome
            {
                rows.push(turn_end::note_row(quote, &shadow_id));
            }
        }
        if !rows.is_empty() {
            let _ = self.append_offer_rows(&mut rows);
        }
        notices
    }

    /// The shadow's last turn_end verdict about the final answer, for the restart check: `Some(word,
    /// promise)` when it judged this answer, `None` when it has said nothing about it.
    pub(crate) fn shadow_on_the_last_answer(&self) -> Option<(String, Option<String>)> {
        if !self.cfg.shadow.is_on(Job::TurnEnd) {
            return None;
        }
        let at = self
            .session
            .items
            .iter()
            .filter(|i| {
                matches!(
                    i,
                    TranscriptItem::Assistant { .. }
                        | TranscriptItem::User {
                            speaker: letibot_transcript::Speaker::Operator,
                            ..
                        }
                )
            })
            .count() as u64;
        let store = self.cfg.store_at()?.open_existing().ok()?;
        let (position, word, promise) =
            crate::shadow::record::last_turn_end(&store, &self.cfg.session_id)?;
        (position == at).then_some((word, promise))
    }
}
