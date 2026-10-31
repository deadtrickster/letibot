//! **Provenance: who wrote these bytes** — DECISIONS.md D17's third arm, recorded where it
//! happens rather than inferred afterwards.
//!
//! The operator, 2026-10-31: *"Provenance is recorded, not inferred: the sha256 of every plugin
//! module and manifest a model writes is recorded at the write, so 'who wrote these bytes' is a
//! fact on disk rather than a guess from a path."*
//!
//! # Why the seam is here and not in a plugin crate
//!
//! What records the fact is a **write**, and a write is `write`/`edit` in this crate. What decides
//! whether a write is about a layer at all, and where the record goes, is `letibot_plugins`'
//! [`Trust`](https://docs.rs/letibot-plugins) — which this crate cannot depend on, because the
//! plugin crate depends on this one (`Gate`, `InvokeCtx`, the schema). So the split is the
//! tree's usual one: a tool says *this call wrote these bytes here*, and the daemon installs a
//! [`Sink`] that decides what that means. `harnessd`'s implementation filters to a layer's
//! artifacts and appends to the trust book; a test installs a recorder.
//!
//! # What it is NOT
//!
//! **Not a permission.** The trust ladder never consults a write record: a placed layer is
//! adjudicated on every call whatever wrote it, and only a *pin* makes it free. So the record is
//! the weaker fact and it can never widen what runs — which is the whole reason it is safe for it
//! to be a file a model's own write can put an entry in.
//!
//! **Not a record of every write.** A write of `README.md` is not provenance about a layer, and a
//! file of every write on the box would bury the ones that matter. The filter is the sink's,
//! because it is a plugin fact.

use std::path::{Path, PathBuf};

/// **A tool wrote these bytes here** — the one thing a `write` or an `edit` reports.
///
/// `path` is resolved against the session's workspace before it arrives (see
/// [`crate::runtime::InvokeCtx::wrote`]), so a sink never has to guess which directory a relative
/// path was relative to.
pub trait Sink: Send + Sync {
    /// The call has landed the bytes. Called **after** the backend accepted the write, so a
    /// refused or failed write records nothing.
    fn wrote(&self, path: &Path, bytes: &[u8]);
}

/// The path a write record names: the absolute path when the path can be resolved, so a record
/// taken by the daemon and a layer discovered by a stack are the same key.
pub(crate) fn absolute(path: &Path, workspace: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match workspace {
        Some(ws) => ws.join(path),
        None => std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf()),
    }
}
