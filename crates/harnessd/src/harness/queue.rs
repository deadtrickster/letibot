//! The merge queue's operator verbs, as one session reaches them: `/queue restart`, `approve`,
//! `veto`, `rm`, `reset`, `clean` and `regate`, each with the repository scope it acts in, plus the
//! review board the queue's own session draws.

use super::*;

impl Harness {
    /// Whether this harness is the review queue's own session - the one
    /// [`crate::sessions::gatekeeper::session`] names for this daemon's workspace, whose board is
    /// the queue.
    pub(crate) fn is_the_queue(&self) -> bool {
        self.hub.session_id() == crate::sessions::gatekeeper::session(&self.cfg.workspace)
    }

    /// The review queue, onto this session's board - one row per review this daemon's queues
    /// serve that is waiting for a reviewer or under one, the rows a child is on marked with it.
    /// A VIEW, replaced whole from the store; the states stay in the store.
    pub fn fill_queue_board(&mut self) -> usize {
        let Some(store) = self.store.as_ref() else {
            return 0;
        };
        let me = crate::mergequeue::lease::this_daemon().owner.clone();
        let open = self.reviews.open(&self.hub.session_id());
        let rows: Vec<letibot_tokencore::store::TodoItem> =
            crate::sessions::gatekeeper::waiting(store, &me)
                .iter()
                .map(|r| {
                    let child = open
                        .iter()
                        .find(|o| o.entry_id == r.id)
                        .map(|o| o.child.as_str());
                    crate::sessions::gatekeeper::row(r, child)
                })
                .collect();
        let n = self.todos.set_queue(rows);
        self.flush_todos_or_say("the review queue's rows");
        n
    }

    fn queue_verb(
        &self,
        what: &str,
        act: impl FnOnce(&Store, &dyn Fn(SessionEvent)) -> Result<String, String>,
    ) -> Result<String, String> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| format!("this daemon has no store, so there are no queues to {what}"))?;
        let registry = self.session_registry.clone();
        act(store, &move |e| {
            registry.broadcast(e);
        })
    }

    /// The repository a `/queue` verb may act in - `None` is every repository.
    /// [`QueueScope::Here`](crate::slash::QueueScope::Here) is this session's own, and a session
    /// of no repository is told so with the line that reaches every repository.
    pub fn queue_scope(&self, scope: &crate::slash::QueueScope) -> Result<Option<String>, String> {
        crate::mergequeue::verbs::scope_repository(&self.cfg.workspace, scope)
    }

    /// [`Harness::queue_scope`] for a session of several repositories - see
    /// [`crate::mergequeue::verbs::scope_repositories`].
    pub fn queue_scopes(
        &self,
        scope: &crate::slash::QueueScope,
    ) -> Result<Vec<Option<String>>, String> {
        crate::mergequeue::verbs::scope_repositories(&self.cfg.workspace, scope)
    }

    /// Which of `scopes` holds the row `id` - the first when there is one scope (the verb
    /// refuses a row outside it by name), else the one whose repository the row is of.
    pub fn queue_scope_of_row(&self, scopes: &[Option<String>], id: &str) -> Option<String> {
        if scopes.len() == 1 {
            return scopes[0].clone();
        }
        let its = self
            .store
            .as_ref()
            .and_then(|s| crate::mergequeue::verbs::row_repository(s, id));
        match its {
            Some(r) if scopes.iter().any(|s| s.as_deref() == Some(r.as_str())) => Some(r),
            // A row of none of them: the first scope, whose verb refuses it naming its repository.
            _ => scopes.first().cloned().flatten(),
        }
    }

    /// `/queue restart ID` - a parked review back to waiting, a parked landing back in line.
    pub fn restart_review(
        &self,
        entry_id: &str,
        repository: Option<&str>,
    ) -> Result<String, String> {
        let me = self.hub.session_id();
        self.queue_verb("restart anything in", |s, ev| {
            crate::mergequeue::verbs::in_scope(s, entry_id, repository)?;
            crate::mergequeue::verbs::restart(s, entry_id, &me, ev)
        })
    }

    /// `/queue approve ID` - the operator's approval over the reviewer (decision 5).
    pub fn approve_review(
        &self,
        entry_id: &str,
        by: &str,
        repository: Option<&str>,
    ) -> Result<String, String> {
        self.queue_verb("approve anything in", |s, ev| {
            crate::mergequeue::verbs::in_scope(s, entry_id, repository)?;
            crate::mergequeue::verbs::approve(s, entry_id, by, ev)
        })
    }

    /// `/queue veto ID WHY` - the operator's rejection, delivered to the child that did the work.
    pub fn veto_review(
        &self,
        entry_id: &str,
        why: &str,
        repository: Option<&str>,
    ) -> Result<String, String> {
        let me = self.hub.session_id();
        let runner = self.subagents.clone();
        self.queue_verb("veto anything in", |s, ev| {
            crate::mergequeue::verbs::in_scope(s, entry_id, repository)?;
            crate::mergequeue::verbs::veto(
                s,
                entry_id,
                why,
                &me,
                &|to, text| letibot_tools::builtins::task::TaskRunner::send(&*runner, to, text),
                ev,
            )
        })
    }

    /// `/queue rm ID` - withdrawn, with the history kept.
    pub fn remove_merge_entry(
        &self,
        entry_id: &str,
        repository: Option<&str>,
    ) -> Result<String, String> {
        let me = self.hub.session_id();
        self.queue_verb("remove anything from", |s, ev| {
            crate::mergequeue::verbs::in_scope(s, entry_id, repository)?;
            crate::mergequeue::verbs::remove(s, entry_id, &me, ev)
        })
    }

    /// `/queue reset [ID]` - every parked row of `repository` restarted, or the one named.
    pub fn reset_reviews(
        &self,
        entry: Option<&str>,
        repository: Option<&str>,
    ) -> Result<String, String> {
        let me = self.hub.session_id();
        self.queue_verb("reset", |s, ev| {
            if let Some(id) = entry {
                crate::mergequeue::verbs::in_scope(s, id, repository)?;
            }
            crate::mergequeue::verbs::reset(s, entry, repository, &me, ev)
        })
    }

    /// `/queue clean [ID]` - every parked row of `repository` withdrawn, or the one named.
    pub fn clean_entries(
        &self,
        entry: Option<&str>,
        repository: Option<&str>,
    ) -> Result<String, String> {
        let me = self.hub.session_id();
        self.queue_verb("clean", |s, ev| {
            if let Some(id) = entry {
                crate::mergequeue::verbs::in_scope(s, id, repository)?;
            }
            crate::mergequeue::verbs::clean(s, entry, repository, &me, ev)
        })
    }

    /// The repository this session's queue verbs are about, and its target - the one its
    /// workspace means (`mergequeue::repo_of_workspace`), keyed the way the queues key it, unless
    /// `named` is a path; and `[queue] target` (else `main`/`master`) when no target is named.
    pub fn queue_repository(
        &self,
        named: Option<&str>,
        target: Option<&str>,
    ) -> Result<(String, String), String> {
        crate::mergequeue::desk::resolve(&self.cfg.workspace, named, target)
    }

    /// `/queue regate [/REPO] [TARGET]` and the `queue` tool's `regate` - the target's tip gated
    /// again even when it was already judged. `who` goes on the move.
    pub fn regate_target(
        &self,
        repository: Option<&str>,
        target: Option<&str>,
        who: &str,
    ) -> Result<String, String> {
        let (repo, target) = self.queue_repository(repository, target)?;
        self.queue_verb("re-gate anything in", |s, ev| {
            crate::mergequeue::verbs::regate(s, &repo, &target, who, ev)
        })
    }

    /// Whether the queues' news is this daemon's to tell a model: false under `--no-queue`, and that is
    /// the whole of the model-facing half of the flag. Every notice would be about work another daemon
    /// owns. The heads still draw the queue's alerts; only the model's copy is withheld.
    fn queue_news_is_mine(&self) -> bool {
        self.cfg.serve_queues
    }

    /// This project's stuck rows, as the idle check reads them: `None` when this session is not the
    /// project's, when the queue is not this daemon's to speak about, or when nothing is stuck. The
    /// cached resolution, because this is read on every turn's end and one workspace does not change.
    pub fn stuck_census(&self) -> Option<crate::mergequeue::stuck::Stuck> {
        if !self.queue_news_is_mine() || self.is_the_queue() {
            return None;
        }
        let store = self.store.as_ref()?;
        let me = self.hub.session_id();
        if crate::mergequeue::root_session(store, &me) != me {
            return None;
        }
        let mine = crate::mergequeue::repositories_of_workspace_cached(&self.cfg.workspace);
        if mine.is_empty() {
            return None;
        }
        let census = crate::mergequeue::stuck::of(store, &mine).ok()?;
        (!census.is_empty()).then_some(census)
    }

    /// What the idle check's clock needs to know about this project's queue: one reading, so the
    /// clock's arming, the sentence and the identity cannot disagree. `says` is `Stuck::is_news` and
    /// not *there are bad rows*, because a row that just went bad is the transition notice's.
    pub fn queue_news(&self) -> crate::sessions::QueueNews {
        let Some(census) = self.stuck_census() else {
            return crate::sessions::QueueNews::none();
        };
        let now = letibot_sessionlog::registry::now_ms();
        crate::sessions::QueueNews {
            says: census.is_news(now),
            matures_in: census.news_in_ms(now).map(std::time::Duration::from_millis),
        }
    }

    /// The census as a sentence, when there is one to say: `None` for a queue with nothing stuck, or
    /// one whose oldest row is too young to be news. Composed from the store at the moment it is said,
    /// so a row that recovered between arming and delivery is not announced as stuck.
    pub fn stuck_census_text(&self) -> Option<String> {
        let now = letibot_sessionlog::registry::now_ms();
        let census = self.stuck_census()?;
        census
            .is_news(now)
            .then(|| crate::mergequeue::notices::stuck_census(&census, now))
    }

    /// The census as a TURN: the sentence `stuck_census_text` composed, said by the harness and answered
    /// by the model. As a notice it reached nobody for hours, because a mailbox is read at the model's next
    /// turn and a quiet session has none. The held batch rides in with it, as for any break.
    pub fn census_turn(&mut self, text: &str) -> Result<Reply, HarnessError> {
        self.submit_spoken(Speaker::Agent, text)
    }

    pub fn queue_notices(&mut self) -> Vec<String> {
        if !self.queue_news_is_mine() {
            return Vec::new();
        }
        let me = self.hub.session_id();
        let Some(store) = self.store.as_ref() else {
            return Vec::new();
        };
        let healths = store.target_healths().unwrap_or_default();
        let mut out = Vec::new();
        let live = store
            .review_rows_in(None, &letibot_tokencore::store::ReviewState::QUEUED)
            .unwrap_or_default();
        for r in live {
            if crate::mergequeue::root_session(store, &r.session_id) != me {
                continue;
            }
            let Some(h) = healths.iter().find(|h| {
                h.repository == r.repository
                    && h.target == r.target
                    && h.state == letibot_tokencore::store::HealthState::Blocked
                    && h.evidence.starts_with(crate::mergequeue::WAITING_FOR_GATE)
            }) else {
                continue;
            };
            let key = format!("gate:{}", h.repository);
            if !self.gate_told.insert(key) {
                continue;
            }
            out.push(format!(
                "The merge queue cannot land anything in `{repo}`: its target `{target}` has no \
                 `Merge gate` section in AGENTS.md, so there is nothing to judge a branch by \
                 (`{branch}` is waiting there). Call `merge_gate` with repo `{repo}` to see what it \
                 suggests and ask the operator with `ask_user_question` (options = those choices; \
                 they may type their own command). Exception to the queue rule: once they pick, \
                 add the section to that repository's AGENTS.md and commit it to `{target}` \
                 yourself. The queue goes on by itself when the section is there.",
                repo = h.repository,
                target = h.target,
                branch = r.branch,
            ));
        }
        out
    }

    /// A target emergency, told to the model of the project that has to act: the queue's warnings are
    /// drawn by heads and read by no model, so a ROOT session is told once per daemon, with the failing
    /// step and its output first. A branch handed over under `merge_via_session` is `handover_notices`'s.
    pub fn queue_emergency_notices(&mut self) -> Vec<String> {
        if !self.queue_news_is_mine() {
            return Vec::new();
        }
        let me = self.hub.session_id();
        if self.is_the_queue() {
            return Vec::new();
        }
        let Some(store) = self.store.as_ref() else {
            return Vec::new();
        };
        if crate::mergequeue::root_session(store, &me) != me {
            return Vec::new();
        }
        let all_healths = store.target_healths().unwrap_or_default();
        let greens: Vec<_> = all_healths
            .iter()
            .filter(|h| h.state == letibot_tokencore::store::HealthState::Green)
            .cloned()
            .collect();
        let healths: Vec<_> = all_healths
            .into_iter()
            .filter(crate::mergequeue::serve::emergency_health)
            .collect();
        let parked: Vec<_> = store
            .review_rows_in(
                None,
                &[
                    letibot_tokencore::store::ReviewState::Blocked,
                    letibot_tokencore::store::ReviewState::CannotDecide,
                ],
            )
            .unwrap_or_default()
            .into_iter()
            .filter(|r| !r.evidence.starts_with(crate::mergequeue::HANDED_TO_PROJECT))
            .collect();
        // The repository this session is the project of - read only when there is news.
        // Every repository of the workspace - one, or each of a one-org project folder's.
        let mine = crate::mergequeue::repositories_of_workspace(&self.cfg.workspace);
        let mut out = Vec::new();
        // Green again: an emergency this session was told about is over - said once,
        // and forgotten, so the next red of the same tip is news again.
        for h in &greens {
            if !mine.contains(&h.repository) {
                continue;
            }
            let prefix = format!("emergency:{}:{}:", h.repository, h.target);
            let held = self.gate_told.iter().any(|k| k.starts_with(&prefix));
            self.gate_told.retain(|k| !k.starts_with(&prefix));
            let stored = store
                .untell_prefix(&format!("told:{me}:{prefix}"))
                .unwrap_or(0);
            if held || stored > 0 {
                out.push(crate::mergequeue::notices::green_again(h, &h.repository));
            }
        }
        if healths.is_empty() && parked.is_empty() {
            return out;
        }
        for h in healths {
            if !mine.contains(&h.repository)
                || !told_once(
                    &mut self.gate_told,
                    store,
                    &me,
                    format!(
                        "emergency:{}:{}:{}:{}",
                        h.repository,
                        h.target,
                        h.sha,
                        h.state.as_str()
                    ),
                )
            {
                continue;
            }
            out.push(crate::mergequeue::notices::health(&h));
        }
        for r in &parked {
            let ours = match r.kind {
                letibot_tokencore::store::ReviewKind::Target => mine.contains(&r.repository),
                letibot_tokencore::store::ReviewKind::Branch => {
                    !r.session_id.is_empty()
                        && crate::mergequeue::root_session(store, &r.session_id) == me
                }
            };
            if !ours
                || !told_once(
                    &mut self.gate_told,
                    store,
                    &me,
                    format!("parked:{}:{}:{}", r.id, r.state.as_str(), r.state_ms),
                )
            {
                continue;
            }
            out.push(crate::mergequeue::notices::parked(r));
        }
        out
    }

    /// The reviews the queue handed to THIS project's main session (the
    /// per-project option, `[queue] merge_via_session = true`): a review that could not run, so
    /// the project gates and merges the branch itself - told once, read off the row.
    pub fn handover_notices(&mut self) -> Vec<String> {
        if !self.queue_news_is_mine() {
            return Vec::new();
        }
        let me = self.hub.session_id();
        let Some(store) = self.store.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let blocked = store
            .review_rows_in(None, &[letibot_tokencore::store::ReviewState::Blocked])
            .unwrap_or_default();
        for r in blocked {
            if !r.evidence.starts_with(crate::mergequeue::HANDED_TO_PROJECT)
                || crate::mergequeue::root_session(store, &r.session_id) != me
                || !told_once(&mut self.gate_told, store, &me, format!("handed:{}", r.id))
            {
                continue;
            }
            out.push(crate::mergequeue::notices::handed_over(&r));
        }
        out
    }

    /// The reviews this daemon's queues claimed, served by THIS session's runner: the review queue's
    /// session is the host and every other session serves nothing. It starts a reviewer per claimed row,
    /// writes the verdict it handed in, counts an attempt that reached none, and kills a stale one.
    pub fn serve_reviews(&mut self) -> usize {
        if !self.is_the_queue() {
            return 0;
        }
        let Some(store) = self.store.as_ref() else {
            return 0;
        };
        let me = crate::mergequeue::lease::this_daemon().owner.clone();
        let placement = crate::sessions::gatekeeper::reviewer_placement(
            self.cfg.reviewers == crate::config::Reviewers::Vm,
            letibot_tools::firecode::firecode_available(),
        );
        let registry = self.session_registry.clone();
        let runner = self.subagents.clone();
        let deliver = move |to: &str, text: &str| {
            letibot_tools::builtins::task::TaskRunner::send(&*runner, to, text)
        };
        let host = self.hub.session_id();
        let pool = crate::mergequeue::review::Pool {
            store,
            runner: &*self.subagents,
            desk: &self.reviews,
            host: &host,
            me: &me,
            placement,
            events: &{
                let registry = registry.clone();
                move |e| {
                    registry.broadcast(e);
                }
            },
            warn: &|repository: &str, e| {
                crate::mergequeue::publish_for_repository(
                    &registry,
                    self.cfg.store.as_deref(),
                    repository,
                    e,
                );
            },
            deliver: &deliver,
            // The queue's own liveness door, asked of a refusal whose author and root are
            // both dead: the session that takes it has to be one this daemon holds now.
            live: &|id: &str| crate::mergequeue::session_is_live(&registry, id),
            settings: &crate::mergequeue::settings::for_repository,
            // The reviewers' declared skills, from this daemon's project file.
            reviewer_skills: &self.cfg.reviewer_skills,
            machines: self.cfg.review_hosts.clone(),
            reviewer_hosts: self.cfg.reviewer_hosts.clone(),
            clones: self.cfg.review_clones.clone().unwrap_or_else(|| {
                std::env::temp_dir().join(crate::mergequeue::review::CLONES_DIR)
            }),
            slots: &|| crate::vm_reap::slot_holders(store, self.session_registry.instance()),
        };
        let served = match crate::mergequeue::review::serve(&pool) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("  review queue: the reviewers could not be served: {e}");
                return 0;
            }
        };
        if served.moved {
            self.fill_queue_board();
        }
        served.started
    }
}
