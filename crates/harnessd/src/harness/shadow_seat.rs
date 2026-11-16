//! The shadow's hooks into a session: the position its verdicts are stamped with, the turn_end
//! trigger, and what a verdict does once it arrives. The decisions are `crate::shadow::turn_end`'s.

use super::*;
use crate::shadow::{Job, runner::Mode, turn_end};

/// A registry item for the during job: its entry, content version and file, if it has one.
type Registered = (
    crate::shadow::during::Entry,
    String,
    Option<std::path::PathBuf>,
);

impl Harness {
    /// This session's shadow as the annotator of the cards relayed to it, when its permission job
    /// is on. Read through the cell, so a shadow opened later is the one asked.
    pub(crate) fn relay_annotator(
        &self,
    ) -> Option<Arc<dyn letibot_sessionlog::pending_asks::Annotator>> {
        if !self.cfg.shadow.is_on(Job::Permission) {
            return None;
        }
        let bell = self.session_registry.bell().clone();
        let id = self.cfg.session_id.clone();
        self.shadow_answering
            .set_wake(Arc::new(move || bell.ring_wake(&id)));
        Some(Arc::new(crate::shadow::permission::Relay {
            cell: self.shadow_cell.clone(),
            home: std::env::var_os("HOME").map(Into::into).unwrap_or_default(),
            answering: self.shadow_answering.clone(),
            revocable: self.cfg.store_at().is_some(),
        }))
    }

    /// The rows the shadow owes this session for the reads it answered, appended now.
    fn shadow_answered_rows(&mut self) {
        let mut rows: Vec<TranscriptItem> = self
            .shadow_answering
            .take_rows()
            .into_iter()
            .map(|(title, body)| TranscriptItem::Offer {
                kind: letibot_transcript::OfferKind::Shadow,
                body: format!("{title}\n\n{body}"),
                title,
            })
            .collect();
        if !rows.is_empty() {
            let _ = self.append_offer_rows(&mut rows);
        }
    }

    /// `/shadow`: the reads the shadow answered and its breaker. `/shadow revoke [N]` revokes
    /// grant N (the newest when left out) through the same path as `/permissions revoke`.
    pub(crate) fn shadow_verb(
        &mut self,
        store: Option<&letibot_tokencore::store::Store>,
        set: &letibot_sessionlog::pending_asks::PendingAsks,
        revoke: Option<Option<usize>>,
    ) -> crate::slash::SlashReply {
        let Some(store) = store else {
            return crate::slash::SlashReply::refused(vec![
                "no store, so the shadow keeps no grant and answers none".into(),
            ]);
        };
        let state = |a: &crate::shadow::permission::Answering| {
            format!(
                "answer_reads: {} · {} of {} disagreements",
                match (a.is_on(), a.tripped()) {
                    (true, _) => "on",
                    (false, true) => "tripped, /shadow arm re-arms",
                    (false, false) => "off",
                },
                a.disagreements(),
                crate::shadow::permission::BREAKER_AT
            )
        };
        let listing = revoke.is_none();
        let mut reply = crate::grants::shadow(store, Some(set), revoke);
        if listing {
            reply.lines.insert(0, state(&self.shadow_answering));
        } else if reply.ok {
            reply.lines.push(state(&self.shadow_answering));
        }
        self.shadow_answered_rows();
        reply
    }

    /// `/shadow arm`: the operator's re-arm of a tripped breaker, said in the main session.
    pub(crate) fn shadow_arm(&mut self) -> crate::slash::SlashReply {
        let reply = match self.shadow_answering.arm() {
            Ok(line) => crate::slash::SlashReply::listing(vec![line]),
            Err(e) => crate::slash::SlashReply::refused(vec![e]),
        };
        self.shadow_answered_rows();
        reply
    }

    /// Open the shadow once the session is stored, and move its position to the rows that count:
    /// an operator message, an agent's line or an answer. Harness notices and display rows do not.
    pub(crate) fn tend_shadow(&mut self) {
        if matches!(self.shadow, crate::shadow::Slot::NotYet) {
            let bell = self.session_registry.bell().clone();
            let id = self.cfg.session_id.clone();
            let wake: crate::shadow::Wake = Arc::new(move || bell.ring_wake(&id));
            self.shadow = crate::shadow::open_for(&self.cfg, &self.model_name(), wake);
            if let crate::shadow::Slot::On {
                shadow, recorder, ..
            } = &self.shadow
            {
                *self.shadow_cell.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((shadow.clone(), recorder.clone()));
            }
        }
        let Some(s) = self.shadow.get() else {
            return;
        };
        let (from, mut n) = self.shadow_counted;
        let items = &self.session.items;
        for i in items.iter().skip(from) {
            if crate::shadow::words::moves_position(i) {
                n += 1;
            }
        }
        self.shadow_counted = (items.len(), n);
        s.advance(n);
        // A persistent shadow is fed every new row once, with its author. A watched transcript
        // that shrank (compacted) is fed from where it now ends.
        if s.persistent() {
            let from = self.shadow_fed.min(items.len());
            s.feed(crate::shadow::feed::rows_of(items, from));
            self.shadow_fed = items.len();
        }
    }

    /// At a round boundary: queue the during check when the subject changed or enough calls ran.
    /// Nothing here waits; the verdict's skill pick is read at a later round.
    pub(crate) fn shadow_during_round(&mut self) {
        if !self.shadow.get().is_some_and(|s| s.is_on(Job::During)) {
            return;
        }
        let todos = self.todos.snapshot();
        let (entries, written) = self.shadow_registry();
        let versions = entries
            .iter()
            .map(|(e, v, _)| (e.name.clone(), v.clone()))
            .collect();
        let changed = self.shadow_during.registry(versions);
        // An item this session wrote is never offered back to it, so its change is no news either.
        let fresh: std::collections::BTreeSet<String> = changed
            .names()
            .into_iter()
            .filter(|n| !written.contains(n))
            .collect();
        if !changed.is_empty()
            && let Some((_, rec, _)) = self.shadow.parts()
        {
            rec.row(&changed.row(), "");
        }
        let due = self.shadow_during.due(&self.session.items, &todos);
        let why = match (due, fresh.is_empty()) {
            (Some(why), _) => why,
            (None, false) => changed.row(),
            (None, true) => return,
        };
        let entries: Vec<crate::shadow::during::Entry> = entries
            .into_iter()
            .filter(|(e, _, _)| !written.contains(&e.name))
            .map(|(e, _, _)| e)
            .collect();
        self.submit_during(why, entries, fresh, &todos);
    }

    /// Every skill and project note, with its content version and the set this session wrote.
    fn shadow_registry(&self) -> (Vec<Registered>, std::collections::BTreeSet<String>) {
        let wrote: Vec<std::path::PathBuf> = self
            .session
            .items
            .iter()
            .filter_map(|i| match i {
                TranscriptItem::Assistant { tool_calls, .. } => Some(tool_calls),
                _ => None,
            })
            .flatten()
            .filter_map(crate::notes_offer::written_of)
            .collect();
        let wrote_under = |dir: &std::path::Path| {
            wrote.iter().any(|w| match w.is_absolute() {
                true => w.starts_with(dir),
                false => w
                    .parent()
                    .is_some_and(|p| !p.as_os_str().is_empty() && dir.ends_with(p)),
            })
        };
        let mut out = Vec::new();
        let mut written = std::collections::BTreeSet::new();
        for s in self.skills.model_skills() {
            let dir = s
                .path
                .as_deref()
                .and_then(std::path::Path::parent)
                .map(|d| d.to_path_buf());
            if dir.as_deref().is_some_and(wrote_under) {
                written.insert(s.name.clone());
            }
            let v = crate::skills_offer::version_of(&s);
            out.push((
                crate::shadow::during::Entry {
                    about: crate::shadow::during::about_skill(&s),
                    name: s.name,
                },
                v,
                dir,
            ));
        }
        if let Ok(dir) = std::fs::read_dir(self.cfg.project_notes()) {
            for e in dir.flatten() {
                let file = e.file_name().to_string_lossy().into_owned();
                if !file.ends_with(".md") {
                    continue;
                }
                let text = std::fs::read_to_string(e.path()).unwrap_or_default();
                let name = format!("note:{file}");
                if wrote
                    .iter()
                    .any(|w| w.file_name() == Some(e.file_name().as_os_str()))
                {
                    written.insert(name.clone());
                }
                let about = text
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .to_string();
                out.push((
                    crate::shadow::during::Entry { name, about },
                    crate::offer_evidence::version(&text),
                    Some(e.path()),
                ));
            }
        }
        (out, written)
    }

    /// Queue one during check. `fresh` registry items go on the shortlist whatever the lexical cut
    /// says: a new or edited item is checked once against the running session.
    fn submit_during(
        &mut self,
        why: String,
        entries: Vec<crate::shadow::during::Entry>,
        fresh: std::collections::BTreeSet<String>,
        todos: &[letibot_tokencore::store::TodoItem],
    ) {
        let loaded: std::collections::BTreeSet<String> = self
            .session
            .items
            .iter()
            .filter_map(|i| match i {
                TranscriptItem::Assistant { tool_calls, .. } => Some(tool_calls),
                _ => None,
            })
            .flatten()
            .filter_map(crate::skills_offer::loaded_of)
            .collect();
        let window: String = self
            .session
            .items
            .iter()
            .rev()
            .take(12)
            .map(letibot_sessionlog::view::body_of)
            .collect::<Vec<_>>()
            .join("\n");
        // The pre-filter is the similarity offers' own score, embedding plus lexical, over the
        // subject they read; the shadow picks from what it leaves.
        let subject = crate::skills_offer::subject(&self.session.items, todos);
        let ranked = self.skills.ranked_with(
            &subject,
            crate::shadow::during::SHORTLIST,
            self.cfg.tier().as_ref(),
        );
        let mut short =
            crate::shadow::during::shortlist_ranked(&ranked, &entries, &window, &loaded);
        for e in entries.iter().filter(|e| fresh.contains(&e.name)) {
            if !short.iter().any(|s| s.name == e.name) {
                short.push(e.clone());
            }
        }
        let mut shown = crate::shadow::during::shown(&self.session.items, todos, &short, &loaded);
        shown.data = format!("triggered by: {why}\n\n{}", shown.data);
        let q = crate::shadow::during::question(&why, todos, &short, &loaded);
        self.tend_shadow();
        if let Some(s) = self.shadow.get() {
            s.submit_asking(Job::During, shown, q);
        }
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
        let words = crate::shadow::words::recent(&self.session.items);
        let mut shown = turn_end::shown(&words, reply, &todos, &blocked);
        // The answer's own row, so a note quoting the model names where it said it.
        let at = self
            .session
            .items
            .iter()
            .rposition(|i| matches!(i, TranscriptItem::Assistant { .. }));
        for s in shown.spans.iter_mut() {
            if s.author == crate::shadow::words::Author::Assistant {
                s.row = at;
            }
        }
        let q = turn_end::question(turn_end::turn_start(&self.session.items), &todos, &blocked);
        if let Some(s) = self.shadow.get() {
            s.submit_asking(Job::TurnEnd, shown, q);
        }
    }

    /// The verdicts that arrived since the last wake, acted on: the notices a wake says, and the
    /// display rows (a proposed note) appended now.
    pub(crate) fn shadow_notices(&mut self) -> Vec<String> {
        self.shadow_answered_rows();
        let Some((events, recorder, shadow_id)) = self.shadow.parts() else {
            return Vec::new();
        };
        // The position as the rows stand now, so a fallback about an older turn is seen as one.
        self.tend_shadow();
        let now = self.shadow.get().map_or(0, |s| s.position());
        let mut notices = Vec::new();
        let mut rows = Vec::new();
        let events: Vec<crate::shadow::Event> = {
            let mut q = events.lock().unwrap_or_else(|e| e.into_inner());
            let (mine, rest): (std::collections::VecDeque<_>, _) = std::mem::take(&mut *q)
                .into_iter()
                .partition(|e| e.job == Job::TurnEnd);
            *q = rest;
            mine.into()
        };
        for e in events {
            let held = self.shadow_held.take();
            let reminded = turn_end::reminded_since_operator(&self.session.items);
            let moved = now > e.position;
            let (act, said) =
                turn_end::decide(&e.outcome, held.as_deref(), reminded, moved, &shadow_id);
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
                let at = quote.row.and_then(|r| self.row_clock(r));
                let mut row = turn_end::note_row(quote, at.as_deref(), &shadow_id);
                if let TranscriptItem::Offer { body, .. } = &mut row {
                    let n = self.session.items.len() + rows.len();
                    body.push_str(&format!("\n\n{}", crate::note_accept::hint(n)));
                }
                rows.push(row);
            }
        }
        if !rows.is_empty() {
            let _ = self.append_offer_rows(&mut rows);
        }
        notices
    }

    /// Who decides this round's skill offers. `None` is similarity, as with no shadow: the during
    /// job is off or paused. `Some((picks, shadow id))` is the shadow's verified picks since the last
    /// round, empty for no offer; each pick acted on or dropped is a row in the shadow session.
    pub(crate) fn shadow_skill_picks(
        &mut self,
    ) -> Option<(Vec<crate::shadow::verdict::Pick>, String)> {
        let s = self.shadow.get()?;
        let during = s.states().into_iter().find(|(j, _)| *j == Job::During)?.1;
        if during.mode == Mode::Paused {
            return None;
        }
        let (events, recorder, shadow_id) = self.shadow.parts()?;
        let mine: Vec<crate::shadow::Event> = {
            let mut q = events.lock().unwrap_or_else(|e| e.into_inner());
            let (mine, rest): (std::collections::VecDeque<_>, _) = std::mem::take(&mut *q)
                .into_iter()
                .partition(|e| e.job == Job::During);
            *q = rest;
            mine.into()
        };
        let mut picks = Vec::new();
        for e in mine {
            let stale = crate::shadow::during::operator_since(&self.session.items, e.position);
            match crate::shadow::during::skill_picks(&e.outcome, stale) {
                Ok(p) => {
                    let names: Vec<&str> = p.iter().map(|p| p.name.as_str()).collect();
                    recorder.row(
                        &format!("during · offer · {} · at #{}", names.join(", "), e.position),
                        "the pick acts: offered in the watched session's next request",
                    );
                    picks = p;
                }
                Err(why) if why != "no skill picked" && why != "no verdict" => {
                    recorder.row(&format!("during · no offer · at #{}", e.position), &why)
                }
                Err(_) => {}
            }
        }
        Some((picks, shadow_id))
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
            .filter(|i| crate::shadow::words::moves_position(i))
            .count() as u64;
        let store = self.cfg.store_at()?.open_existing().ok()?;
        let (position, word, promise) =
            crate::shadow::record::last_turn_end(&store, &self.cfg.session_id)?;
        (position == at).then_some((word, promise))
    }

    /// A transcript row's clock time, `HH:MM` UTC, from the store.
    fn row_clock(&self, row: usize) -> Option<String> {
        let store = self.cfg.store_at()?.open_existing().ok()?;
        let ms = store
            .item_created_at(&self.session.transcript_id, row as u64)
            .ok()??;
        let secs = (ms / 1000).rem_euclid(86_400);
        Some(format!("{:02}:{:02} UTC", secs / 3600, (secs / 60) % 60))
    }
}
