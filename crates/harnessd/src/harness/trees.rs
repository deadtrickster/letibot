//! Where each live session's own watcher set and child slots are, by session id.
//!
//! A parked child is a session the daemon's WORKER runs later turns for, while the slot and the
//! queue that must move are its parent's - and the parent is on its own thread, with its harness
//! in no map the worker can read. This is the one table both sides reach: a harness registers
//! itself as it opens, and the worker asks it for the parent of the child it just ran a turn for.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::children::{TaskSlot, TaskSlots};
use crate::jobwatch::JobWatchers;

/// One session's own half of a tree: the watcher set its children join, and the slots it hands
/// out. Both are the session's own - a child's set is its own set, joined to its parent's.
#[derive(Clone)]
pub(crate) struct Tree {
    pub(crate) watch: Arc<JobWatchers>,
    pub(crate) slots: TaskSlots,
}

impl Tree {
    /// This session's slot for one handle, if it started a child under that name.
    pub(crate) fn slot(&self, handle: &str) -> Option<Arc<TaskSlot>> {
        self.slots
            .lock()
            .expect("task slots")
            .iter()
            .find(|(h, _)| h == handle)
            .map(|(_, s)| Arc::clone(s))
    }
}

/// Every session this daemon has open, by id. Cheap to clone - one `Arc` - because it rides
/// [`crate::harness::Parts`] and every open puts its own entry in.
#[derive(Clone, Default)]
pub(crate) struct Trees {
    inner: Arc<Mutex<HashMap<String, Tree>>>,
}

impl Trees {
    /// This session opened: it is now the parent of whatever it spawns.
    pub(crate) fn register(&self, session: &str, tree: Tree) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session.to_string(), tree);
    }

    /// A session that closed - or was reopened, which registers over itself. A settlement for a
    /// session that is gone has nowhere to go, so the entry goes with it.
    pub(crate) fn forget(&self, session: &str) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session);
    }

    pub(crate) fn of(&self, session: &str) -> Option<Tree> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session)
            .cloned()
    }
}
