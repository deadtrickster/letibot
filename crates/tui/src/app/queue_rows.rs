//! The Queues section of the rooms list: one row per repository the queue snapshot knows, with the
//! target's health, what waits, gates and needs a person, the queue moves since the operator last
//! looked, and a mute checkbox that speaks `/listen`.

use crate::app::rooms::ListRow;
use crate::app::*;
use crate::ui::panes::queue::{QueueScope, repo_name};

/// A target's standing as the row draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Health {
    Green,
    Red,
    Unjudged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueueRepo {
    pub repo: String,
    pub name: String,
    pub health: Health,
    pub waiting: usize,
    pub gating: usize,
    /// Entries stopped until a person rules.
    pub person: usize,
    pub unread: usize,
    pub muted: bool,
}

impl App {
    /// The repositories the queue snapshot lists, queue targets only.
    pub(crate) fn queue_repos(&self) -> Vec<QueueRepo> {
        let q = &self.queues_all;
        let mut repos: Vec<&str> = Vec::new();
        for r in q
            .targets
            .iter()
            .map(|t| t.repository.as_str())
            .chain(q.reviews.iter().map(|r| r.repository.as_str()))
            .chain(q.merges.iter().map(|m| m.repository.as_str()))
        {
            if !r.is_empty() && !repos.contains(&r) && !q.not_queued.iter().any(|n| n == r) {
                repos.push(r);
            }
        }
        repos.sort_by_key(|r| repo_name(r));
        repos
            .into_iter()
            .map(|repo| {
                let targets: Vec<&str> = q
                    .targets
                    .iter()
                    .filter(|t| t.repository == repo)
                    .map(|t| t.state.as_str())
                    .collect();
                let health = if targets.contains(&"red") {
                    Health::Red
                } else if !targets.is_empty() && targets.iter().all(|s| *s == "green") {
                    Health::Green
                } else {
                    Health::Unjudged
                };
                let (mut waiting, mut gating, mut person) = (0, 0, 0);
                for r in q.reviews.iter().filter(|r| r.repository == repo) {
                    match r.state.as_str() {
                        "waiting" => waiting += 1,
                        "reviewing" => gating += 1,
                        "blocked" | "cannot_decide" => person += 1,
                        _ => {}
                    }
                }
                for m in q.merges.iter().filter(|m| m.repository == repo) {
                    match m.state.as_str() {
                        "queued" => waiting += 1,
                        "gating" => gating += 1,
                        "blocked" | "failed" | "timed_out" | "conflict" | "landed_local" => {
                            person += 1
                        }
                        _ => {}
                    }
                }
                let seen = self.rooms.queue_seen.get(repo).copied().unwrap_or(0);
                let unread = q
                    .recent
                    .iter()
                    .filter(|m| m.repository == repo && m.queue != "lease" && m.at_ms > seen)
                    .count();
                QueueRepo {
                    name: repo_name(repo),
                    muted: self.listen_muted(&repo_name(repo)),
                    repo: repo.to_string(),
                    health,
                    waiting,
                    gating,
                    person,
                    unread,
                }
            })
            .collect()
    }

    /// Is `name` muted for this workspace: the daemon's `listen.NAME` setting row says `[x] muted`.
    fn listen_muted(&self, name: &str) -> bool {
        let key = format!("listen.{name}");
        self.settings
            .iter()
            .any(|r| r.key == key && r.value.starts_with("[x]"))
    }

    /// A repository seen for the first time starts at the snapshot's clock, so the moves already in
    /// its history are not news.
    pub(crate) fn queue_baseline(&mut self) {
        let at = self.queues_all.at_ms;
        let repos: Vec<String> = self.queue_repos().into_iter().map(|r| r.repo).collect();
        for r in repos {
            self.rooms.queue_seen.entry(r).or_insert(at);
        }
    }

    /// Enter or a click on a repository's queue row: its `/queue`, and its events count as read.
    pub(crate) fn queue_row_open(&mut self, repo: String) -> Option<Action> {
        let newest = self
            .queues_all
            .recent
            .iter()
            .filter(|m| m.repository == repo)
            .map(|m| m.at_ms)
            .max()
            .unwrap_or(0)
            .max(self.queues_all.at_ms);
        self.rooms.queue_seen.insert(repo.clone(), newest);
        self.queue_in(QueueScope::Path(repo))
    }

    /// The mute checkbox: `/listen -REPO` to mute, `/listen +REPO` to listen again.
    pub(crate) fn queue_row_mute(&mut self, repo: &str) -> Option<Action> {
        let name = repo_name(repo);
        let verb = if self.listen_muted(&name) { '+' } else { '-' };
        Some(Action::Slash {
            line: format!("listen {verb}{name}"),
        })
    }

    /// What a click on a list row does.
    pub(crate) fn list_row_act(&mut self, row: ListRow) -> Option<Action> {
        match row {
            ListRow::Chan(c) => self.sidebar_act(c),
            ListRow::Queue(repo) => self.queue_row_open(repo),
            ListRow::Mute(repo) => self.queue_row_mute(&repo),
        }
    }
}
