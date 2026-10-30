//! **A queue notice that needs somebody to act, and the chain that reaches them** — one function,
//! so that the rule has one spelling in this tree.
//!
//! The rule, in one sentence: **a notice that needs somebody to act is put to a session, and the
//! hub card is what is left when there is no session to put it to** — so nothing that needs action
//! is hub-only.
//!
//! # The chain, and why the rungs are in that order
//!
//! 1. **The branch's own tree** — the session the row names: the child that wrote the branch, or
//!    the session that was working in the tree. It is the party that can answer in the branch.
//! 2. **Its root** — the top-level session that started it. The child has usually finished by the
//!    time a verdict or a sweep lands, and a tree whose child is gone is still that child's root's
//!    work (`mergequeue::root_session` walks the store's parent edges).
//! 3. **A live top-level session of the branch's workspace** — newest first. This rung exists
//!    because of the operator's own report of 2026-10-29: a card in their head
//!    (`session · review · agent/fix-tool-mount-landing · refused, told nobody`) while the row held
//!    everything needed to act — the child had finished hours earlier and its root was a session
//!    row with nobody attached, so the chain ended at nobody and the model that could have answered
//!    never heard.
//!
//! **The first rung that takes it wins**, so one notice is never two messages, and the fallbacks
//! are tried in the order of how much each can be expected to act on it.
//!
//! # What "live" and "the same workspace" are somebody else's answer to
//!
//! * **Live** is [`super::serve::QueueDoor::session_live`] — the door `RepoQueue::review_upkeep`
//!   already asks about a reviewer's child, so a session counts while this daemon holds it **or**
//!   has spawned it and not yet opened it ([`super::session_is_live`]). A session ROW in the store
//!   is not enough: the operator's report is exactly the case of a root that exists in the store
//!   with nobody attached, and a message to it is a message into a hole.
//! * **The same workspace** is [`super::repositories_of_workspace_cached`] over the session's own
//!   `workspace_root` — the queue's own reading of which repositories a folder is a session of, so
//!   a project folder of several checkouts answers for each of them and a worktree answers for its
//!   repository.
//! * **Top-level** is `parent_session_id` empty: a child is somebody's subagent, and its root is
//!   the session the rung above already tried. The review queue's own session is not a candidate
//!   either — it already holds the row, and a notice told to the queue is a notice told to nobody.

use std::path::Path;

use letibot_tokencore::store::Store;

/// **Which party a notice is being put to** — handed to the caller's `text` so the sentence can
/// say why *this* session is the one being told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recipient<'a> {
    /// The session the row names — the branch's own tree.
    Branch { id: &'a str },
    /// The root of that session's tree, when the branch's own session is gone.
    Root { id: &'a str, branch: &'a str },
    /// A live top-level session of the branch's workspace.
    Workspace { id: &'a str, workspace: &'a str },
}

/// Which rung of the chain a notice went out on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rung {
    Branch,
    Root,
    Workspace,
}

/// **What became of a notice** — the card's last clause, and the evidence *told nobody* has to
/// carry.
#[derive(Clone, Debug, Default)]
pub struct Told {
    /// The session that took it, and the rung it was on; `None` when nobody did.
    pub to: Option<(String, Rung)>,
    /// Every party the door tried, in that party's own words, plus — when there was none — the
    /// scan that found no live session of the workspace.
    pub tried: String,
}

impl Told {
    /// **The clause the hub card ends with** — who took it, or who was tried and why nobody did.
    pub fn sentence(&self) -> String {
        match &self.to {
            Some((id, Rung::Branch)) => {
                format!("Told to `{id}`, the session the row names.")
            }
            Some((id, Rung::Root)) => {
                format!("Told to `{id}`, the root of that session's tree.")
            }
            Some((id, Rung::Workspace)) => format!(
                "Told to `{id}`, the live top-level session of this repository's workspace."
            ),
            None => format!("Nobody could be told: {}.", self.tried),
        }
    }
}

/// **What the chain needs from its caller** — the store the tree is walked in, the repository the
/// notice is about, and the two doors that are not this module's to answer:
///
/// * `deliver` puts a message on a session's queue (`QueueDoor::deliver` — the hub's own submit and
///   wake, so a session between turns runs a turn for it and a running one hears it at its next
///   round boundary). It answers `Err` with the reason when nothing would read the message, which
///   is how a rung that is gone is told from one that took it.
/// * `live` is whether this daemon holds a session now (`QueueDoor::session_live`).
pub struct Reach<'a> {
    pub store: &'a Store,
    pub repository: &'a str,
    pub deliver: &'a dyn Fn(&str, &str) -> Result<String, String>,
    pub live: &'a dyn Fn(&str) -> bool,
}

/// **Put a notice on the chain and answer where it landed** — the three rungs of this module's
/// doc, in order, the first that takes it winning.
///
/// `text` builds the words for the rung being tried, given the parties already tried and why each
/// failed (`missed`, empty on the first rung): a refusal tells its author one thing, its root
/// another and a stranger a third, because *why am I being told this* is the first question a
/// session handed somebody else's work asks.
pub fn a_session(
    reach: &Reach<'_>,
    branch_session: &str,
    text: &dyn Fn(Recipient<'_>, &str) -> String,
) -> Told {
    let mut tried: Vec<String> = Vec::new();
    if !branch_session.is_empty() {
        let words = text(Recipient::Branch { id: branch_session }, "");
        match (reach.deliver)(branch_session, &words) {
            Ok(_) => {
                return Told {
                    to: Some((branch_session.to_string(), Rung::Branch)),
                    tried: String::new(),
                };
            }
            Err(e) => tried.push(format!("`{branch_session}`: {e}")),
        }
    }
    let root = super::root_session(reach.store, branch_session);
    if !root.is_empty() && root != branch_session {
        let words = text(
            Recipient::Root {
                id: &root,
                branch: branch_session,
            },
            &tried.join("; "),
        );
        match (reach.deliver)(&root, &words) {
            Ok(_) => {
                return Told {
                    to: Some((root, Rung::Root)),
                    tried: String::new(),
                };
            }
            Err(e) => tried.push(format!("`{root}`: {e}")),
        }
    }
    match live_session_in_workspace(
        reach.store,
        reach.repository,
        reach.live,
        &[branch_session, &root],
    ) {
        Some((id, workspace)) => {
            let words = text(
                Recipient::Workspace {
                    id: &id,
                    workspace: &workspace,
                },
                &tried.join("; "),
            );
            match (reach.deliver)(&id, &words) {
                Ok(_) => {
                    return Told {
                        to: Some((id, Rung::Workspace)),
                        tried: String::new(),
                    };
                }
                Err(e) => tried.push(format!("`{id}`: {e}")),
            }
        }
        None => tried.push(format!(
            "no live top-level session of `{}` is open in this daemon",
            reach.repository
        )),
    }
    Told {
        to: None,
        tried: tried.join("; "),
    }
}

/// **The live session of the branch's workspace the chain falls back to** — the newest top-level
/// session of `repository` that this daemon holds, with its workspace.
///
/// `skip` is the parties already tried, so a session that has already refused this message is not
/// asked for it twice. A row with no workspace cannot be said to be in the branch's, and neither
/// can a subagent (its root is the rung above).
pub fn live_session_in_workspace(
    store: &Store,
    repository: &str,
    live: &dyn Fn(&str) -> bool,
    skip: &[&str],
) -> Option<(String, String)> {
    let mut best: Option<(String, String, i64)> = None;
    for s in store.list_sessions().unwrap_or_default() {
        if s.parent_session_id.is_some_and(|p| !p.is_empty()) {
            continue;
        }
        if skip.iter().any(|x| !x.is_empty() && *x == s.id)
            || letibot_sessionlog::is_gatekeeper_session(&s.id)
        {
            continue;
        }
        if s.workspace_root.is_empty() || !live(&s.id) {
            continue;
        }
        let in_workspace = super::repositories_of_workspace_cached(Path::new(&s.workspace_root))
            .iter()
            .any(|repo| repo == repository);
        if !in_workspace {
            continue;
        }
        // **Newest**, by the store's own reading of a session's last activity — the same order
        // the picker lists them in.
        if best
            .as_ref()
            .is_none_or(|(_, _, ms)| s.last_activity_ms > *ms)
        {
            best = Some((s.id.clone(), s.workspace_root.clone(), s.last_activity_ms));
        }
    }
    best.map(|(id, workspace, _)| (id, workspace))
}
