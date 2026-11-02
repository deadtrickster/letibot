//! Provenance: who wrote these bytes, recorded at the write rather than inferred from a path.
//! The record is never a permission - only a pin makes a placed layer free - so it can safely be
//! a file a model's own write appends to. The [`Sink`] the daemon installs decides what counts.

use std::path::{Path, PathBuf};

/// A tool wrote these bytes here: the one thing a `write` or an `edit` reports. `path` is
/// resolved against the session's workspace before it arrives (see
/// [`crate::runtime::InvokeCtx::wrote`]), so a sink never has to guess which directory that was.
pub trait Sink: Send + Sync {
    /// The call has landed the bytes. Called after the backend accepted the write, so a
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
