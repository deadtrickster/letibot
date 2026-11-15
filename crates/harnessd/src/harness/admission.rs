//! How many subagents run at once: `[subagents] max_running` per root session's tree and
//! `daemon_max_running` over the whole daemon. A spawn over either waits for a slot or is
//! refused, by `when_full`. Queue is the default, as grok and omp do it.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

use crate::leticode_config::ChildLimits;

/// The daemon's running-children counts, by tree. One per daemon, carried in [`super::trees::Trees`].
#[derive(Default)]
pub(crate) struct Admission {
    held: Mutex<Counts>,
    freed: Condvar,
}

#[derive(Default)]
struct Counts {
    total: usize,
    by_tree: HashMap<usize, usize>,
}

/// One running child's place under the caps. Dropping it frees the place and wakes the queue.
pub(crate) struct Permit {
    from: Arc<Admission>,
    tree: usize,
}

impl std::fmt::Debug for Permit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Permit").field("tree", &self.tree).finish()
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut c = self.from.held.lock().unwrap_or_else(|e| e.into_inner());
        c.total = c.total.saturating_sub(1);
        if let Some(n) = c.by_tree.get_mut(&self.tree) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                c.by_tree.remove(&self.tree);
            }
        }
        drop(c);
        self.from.freed.notify_all();
    }
}

/// Which cap a spawn met, with the counts at that moment, for the refusal and the queue note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Full {
    pub(crate) tree_running: usize,
    pub(crate) daemon_running: usize,
    pub(crate) limits: ChildLimits,
}

impl Full {
    /// The cap that is reached, in the config's own words.
    pub(crate) fn which(&self) -> String {
        if self.tree_running >= self.limits.per_tree {
            format!(
                "{} children of this session's tree are running and `[subagents] max_running` \
                 is {}",
                self.tree_running, self.limits.per_tree
            )
        } else {
            format!(
                "{} children are running in this daemon and `[subagents] daemon_max_running` \
                 is {}",
                self.daemon_running, self.limits.per_daemon
            )
        }
    }
}

/// A tree's key: the address of the slot list every session of one tree shares.
pub(crate) fn tree_key(slots: &super::children::TaskSlots) -> usize {
    Arc::as_ptr(slots) as *const () as usize
}

impl Admission {
    fn room(c: &Counts, tree: usize, limits: ChildLimits) -> Result<(), Full> {
        let tree_running = c.by_tree.get(&tree).copied().unwrap_or(0);
        // A cap of 0 would queue for ever; the parser refuses it and this clamps what a test hands in.
        if tree_running < limits.per_tree.max(1) && c.total < limits.per_daemon.max(1) {
            return Ok(());
        }
        Err(Full {
            tree_running,
            daemon_running: c.total,
            limits,
        })
    }

    fn take(self: &Arc<Self>, c: &mut Counts, tree: usize) -> Permit {
        c.total += 1;
        *c.by_tree.entry(tree).or_insert(0) += 1;
        Permit {
            from: self.clone(),
            tree,
        }
    }

    /// A place now, or which cap is full.
    pub(crate) fn try_admit(
        self: &Arc<Self>,
        tree: usize,
        limits: ChildLimits,
    ) -> Result<Permit, Full> {
        let mut c = self.held.lock().unwrap_or_else(|e| e.into_inner());
        Self::room(&c, tree, limits)?;
        Ok(self.take(&mut c, tree))
    }

    /// A place once one frees, or `None` when `given_up` says the wait is over (a stop, a kill).
    /// `given_up` is polled because a kill does not notify this condvar.
    pub(crate) fn admit_or_wait(
        self: &Arc<Self>,
        tree: usize,
        limits: ChildLimits,
        given_up: impl Fn() -> bool,
    ) -> Option<Permit> {
        let mut c = self.held.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if given_up() {
                return None;
            }
            if Self::room(&c, tree, limits).is_ok() {
                return Some(self.take(&mut c, tree));
            }
            c = self
                .freed
                .wait_timeout(c, std::time::Duration::from_millis(200))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// A place whatever the caps say: a child brought back by a restart or a reopen counts, and is
    /// never held back, because it was already admitted once.
    pub(crate) fn force(self: &Arc<Self>, tree: usize) -> Permit {
        let mut c = self.held.lock().unwrap_or_else(|e| e.into_inner());
        self.take(&mut c, tree)
    }

    #[cfg(test)]
    pub(crate) fn running(&self) -> usize {
        self.held.lock().unwrap_or_else(|e| e.into_inner()).total
    }
}

/// How a spawn was let in: with its place, waiting for one, or outside the caps (the review
/// queue's reviewers, which `[queue] reviewers_at_once` caps on its own).
pub(crate) enum Admit {
    Now(Permit),
    Wait(ChildLimits),
    Exempt,
}

/// An objective as the duplicate check compares it: case and spacing do not make a new task.
pub(crate) fn normalised_objective(prompt: &str) -> String {
    prompt
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['.', '!', '?'])
        .to_lowercase()
}
