//! The tree, as the daemon holding it can see it - who is in a project's room: the project's
//! top-level sessions, their subagents at any depth, and the `system` agents that serve them. The
//! room cannot work this out (a subagent never connects), so the daemon that can sends the list.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use letibot_room::{Group, Member};
use letibot_sessionlog::registry::Registry;

/// Where a room's membership comes from. One implementation in the daemon (the registry); a
/// test stands in its own, which is what makes a link testable without a store.
pub trait RosterSource: Send + Sync {
    /// Every session in the project whose key is `project`, in the order the tree draws them.
    fn members(&self, project: &str) -> Vec<Member>;
}

/// The daemon's own answer: the registry's briefs, filtered to one project and grouped the way
/// the tree groups them.
pub struct Roster {
    registry: Arc<Registry>,
    /// The project each workspace is in, asked of git once. A `git rev-parse` per session per
    /// second would be a process per row per frame, and a workspace's repository does not move
    /// while a daemon runs. `""` is cached too, so a folder outside any repository costs one call.
    projects: Mutex<HashMap<String, String>>,
}

impl Roster {
    pub fn new(registry: Arc<Registry>) -> Arc<Roster> {
        Arc::new(Roster {
            registry,
            projects: Mutex::new(HashMap::new()),
        })
    }

    /// The project root `workspace` belongs to - the repository's own folder, so a session in a
    /// worktree or a review clone hangs under the project it was cut from. A folder outside any
    /// repository is no project, and answers empty: the box's own room.
    pub fn project_of(&self, workspace: &str) -> String {
        if workspace.trim().is_empty() {
            return String::new();
        }
        if let Ok(cache) = self.projects.lock()
            && let Some(found) = cache.get(workspace)
        {
            return found.clone();
        }
        let found = crate::plugins::feed::repository_of(std::path::Path::new(workspace));
        if let Ok(mut cache) = self.projects.lock() {
            cache.insert(workspace.to_string(), found.clone());
        }
        found
    }

    /// The project a session belongs to - the root of its chain's workspace, which is the
    /// operator's own rule for the tree: a session hangs under the project of the ROOT of its
    /// chain, because a child's workspace can be a review clone or a worktree.
    pub fn project_of_session(&self, rows: &[Row<'_>], id: &str) -> String {
        let mut at = id.to_string();
        for _ in 0..64 {
            let Some(row) = rows.iter().find(|r| r.session == at) else {
                break;
            };
            match row.parent {
                Some(parent) => at = parent.to_string(),
                None => return self.project_of(row.workspace),
            }
        }
        // A chain that loops or dangles still has a row: its own workspace is the honest answer.
        rows.iter()
            .find(|r| r.session == id)
            .map(|r| self.project_of(r.workspace))
            .unwrap_or_default()
    }
}

impl RosterSource for Roster {
    fn members(&self, project: &str) -> Vec<Member> {
        let briefs = self.registry.list();
        let rows: Vec<Row> = briefs
            .iter()
            .map(|b| Row {
                session: &b.session_id,
                title: &b.title,
                parent: b.parent_session_id.as_deref(),
                workspace: &b.wiring.workspace,
                origin: b.origin,
                live: b.live,
            })
            .collect();
        members_of(&rows, project, |w| self.project_of(w))
    }
}

/// One session, as the roster needs it - the fields of a [`SessionBrief`] the tree is drawn
/// from, and nothing else: six values a test can write down without a registry or a store.
#[derive(Debug, Clone, Copy)]
pub struct Row<'a> {
    pub session: &'a str,
    pub title: &'a str,
    pub parent: Option<&'a str>,
    pub workspace: &'a str,
    pub origin: letibot_sessionlog::SessionOrigin,
    pub live: bool,
}

/// The rows of one project, as the tree draws them - the pure half, so a test can pin the
/// grouping without a registry, a store or a git. `project_of` is the caller's, because it is the
/// one thing that has to ask git.
pub fn members_of(
    rows: &[Row<'_>],
    project: &str,
    project_of: impl Fn(&str) -> String,
) -> Vec<Member> {
    let key = letibot_room::project_key(project);
    let mine: Vec<&Row<'_>> = rows
        .iter()
        .filter(|r| {
            // The ROOT of the chain decides, because a child's workspace can be a review clone
            // or a worktree - what says where it belongs is the tree it hangs from.
            let root = root_of(rows, r.session);
            let workspace = root
                .and_then(|root| rows.iter().find(|x| x.session == root))
                .map(|x| x.workspace)
                .unwrap_or(r.workspace);
            let theirs = project_of(workspace);
            if project.is_empty() {
                theirs.is_empty()
            } else {
                !theirs.is_empty() && letibot_room::project_key(&theirs) == key
            }
        })
        .collect();
    let mut out: Vec<Member> = mine
        .iter()
        .map(|r| Member {
            session: r.session.to_string(),
            label: label_of(r),
            group: group_of(&mine, r),
            parent: r.parent.map(str::to_string),
            depth: depth_of(&mine, r),
            live: r.live,
        })
        .collect();
    letibot_room::tree_order(&mut out);
    out
}

/// The session at the top of this one's chain, or `None` for a chain that dangles.
fn root_of(rows: &[Row<'_>], id: &str) -> Option<String> {
    let mut at = id.to_string();
    for _ in 0..64 {
        let row = rows.iter().find(|r| r.session == at)?;
        match row.parent {
            Some(parent) => at = parent.to_string(),
            None => return Some(at),
        }
    }
    None
}

/// How far under the project's top level a session sits - 0 for a top-level session, 1 for its
/// child, 2 for its child's child.
fn depth_of(rows: &[&Row<'_>], row: &Row<'_>) -> u32 {
    let mut depth = 0u32;
    let mut at = row.parent;
    while let Some(parent) = at {
        depth = depth.saturating_add(1);
        at = rows
            .iter()
            .find(|r| r.session == parent)
            .and_then(|r| r.parent);
        if depth > 64 {
            break;
        }
    }
    depth
}

/// Which group a row is in - its own marks first, then its chain's. The order is
/// `App::system_kind`'s, and it matters: the review queue's own session is a gatekeeper and not
/// a reviewer, and everything hanging from it - a review of a review - is a reviewer.
fn group_of(rows: &[&Row<'_>], row: &Row<'_>) -> Group {
    let mut at = row.session.to_string();
    for _ in 0..64 {
        let Some(here) = rows.iter().find(|r| r.session == at) else {
            break;
        };
        let me = here.session == row.session;
        if letibot_sessionlog::is_gatekeeper_session(here.session) {
            return if me {
                Group::Gatekeeper
            } else {
                Group::Reviewer
            };
        }
        if here.origin == letibot_sessionlog::SessionOrigin::Queue {
            return if me && here.parent.is_none() {
                Group::Gatekeeper
            } else {
                Group::Reviewer
            };
        }
        // A brief carries no seat, so a reviewer is known by the brief its title was cut from.
        if here
            .title
            .starts_with(letibot_sessionlog::GATEKEEPER_TITLE_PREFIX)
        {
            return Group::Reviewer;
        }
        if here
            .title
            .starts_with(letibot_sessionlog::NOTES_KEEPER_TITLE_PREFIX)
        {
            return Group::Keeper;
        }
        match here.parent {
            Some(parent) => at = parent.to_string(),
            None => break,
        }
    }
    if row.parent.is_some() {
        Group::Sub
    } else {
        Group::Top
    }
}

/// What the tree writes on the row - the session's title, and its id when it has none, because
/// a member a person cannot recognise is a row the tree draws as an id anyway.
fn label_of(row: &Row<'_>) -> String {
    let title = row.title.trim();
    if title.is_empty() {
        row.session.to_string()
    } else {
        title.to_string()
    }
}

#[cfg(test)]
mod tests;
