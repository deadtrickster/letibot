//! A queue notice that needs somebody to act, and the chain that reaches them - one
//! function, so the rule has one spelling in this tree: such a notice is put to a SESSION,
//! and the hub card is what is left when there is no session to put it to. Nothing that
//! needs action is hub-only. The rungs, in order, the first that takes it winning (so one
//! notice is never two messages) the session the row names (the branch's own tree), its
//! root, then the newest live top-level session of the branch's workspace.

use std::path::Path;

use letibot_tokencore::store::Store;

/// Which party a notice is being put to - handed to the caller's `text` so the sentence can say why
/// *this* session is the one being told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recipient<'a> {
    /// The session the row names - the branch's own tree.
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

/// What became of a notice - the card's last clause, and the evidence *told nobody* has to carry.
#[derive(Clone, Debug, Default)]
pub struct Told {
    /// The session that took it, and the rung it was on; `None` when nobody did.
    pub to: Option<(String, Rung)>,
    /// Every party the door tried, in that party's own words, plus - when there was none - the
    /// scan that found no live session of the workspace.
    pub tried: String,
}

impl Told {
    /// The clause the hub card ends with - who took it, or who was tried and why nobody did.
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

/// What the chain needs from its caller: the store the tree is walked in, the repository
/// the notice is about, and the two doors that are not this module's to answer.
pub struct Reach<'a> {
    pub store: &'a Store,
    pub repository: &'a str,
    pub deliver: &'a dyn Fn(&str, &str) -> Result<String, String>,
    pub live: &'a dyn Fn(&str) -> bool,
}

/// Put a notice on the chain and answer where it landed - the three rungs of this module's
/// doc, in order, the first that takes it winning.
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

/// The live session of the branch's workspace the chain falls back to: the newest top-level
/// session of `repository` that this daemon holds, with its workspace.
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
        // Newest, by the store's own reading of a session's last activity - the same order the
        // picker lists them in.
        if best
            .as_ref()
            .is_none_or(|(_, _, ms)| s.last_activity_ms > *ms)
        {
            best = Some((s.id.clone(), s.workspace_root.clone(), s.last_activity_ms));
        }
    }
    best.map(|(id, workspace, _)| (id, workspace))
}
