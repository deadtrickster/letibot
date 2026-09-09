//! What this session has been shown of the operator's files, and what it may
//! therefore change.
//!
//! # Read-before-write, actually enforced
//!
//! Both harnesses whose edit tools have met real users *tell the model* they
//! enforce this and then do not:
//!
//! - opencode's `edit.txt:4` says *"You must use your Read tool at least once in
//!   the conversation before editing. This tool will error if you attempt an edit
//!   without reading the file."* There is no such check anywhere in the tree — no
//!   path→time map, no digest, nothing. Its V2 rewrite added a compare-and-swap
//!   (`file-mutation.ts`, *"File changed after permission approval. Read it again
//!   before editing."*) which covers read → prompt → write **inside one call**,
//!   and still tracks nothing across the session.
//! - grok-build is honest about it instead: `search_replace/mod.rs:803` —
//!   *"read-before-edit is encouraged via description and RL grading, not
//!   runtime-enforced"* — with a test named
//!   `consecutive_edits_succeed_without_prior_read`. Its only trace of the concern
//!   is a default-on **hint** in the no-match message: *"The user may have changed
//!   the file since you last read it."*
//!
//! A hint is what you write when you cannot check. This module is the check, and
//! it turns grok-build's sentence into a fact: [`FileLedger`] holds the digest of
//! what was last handed to the model for each path, so *"the file changed since
//! you read it"* is either true or false rather than a nudge.
//!
//! # Why a digest and not an mtime
//!
//! An mtime is the filesystem's opinion about a file, and three ordinary things
//! move it without changing a byte: a `touch`, a checkout that restores identical
//! content, and a formatter that rewrote the file to what it already was. A digest
//! answers the question actually being asked — *are the bytes the model was shown
//! still the bytes on disk* — and it works unchanged over a backend that has no
//! mtime, which is the point of [`crate::backend::ExecBackend`].
//!
//! # The rule, stated exactly
//!
//! An edit or an overwrite may only land on content this session has **shown to
//! the model**. Two consequences, both deliberate:
//!
//! - a failed read-before-write check **records the content it is refusing over**,
//!   so the same call that refuses also supplies what was missing and the retry
//!   succeeds. That is clause 1 applied to a guard: the guard costs one call and
//!   never costs a wrong edit. The invariant it protects is unchanged — no write
//!   lands on bytes the model was not shown — because the refusal is what shows
//!   them;
//! - creating a file that does not exist needs no prior read. There is nothing to
//!   have read, and demanding a read of a non-existent path would be a rule about
//!   a ritual rather than about the content.

use std::collections::BTreeMap;
use std::sync::Mutex;

/// What was last shown to the model for one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    /// The content hash of the **whole file** at the moment it was read, not of
    /// the slice that was displayed. Staleness is a property of the file.
    pub digest: String,
    pub bytes: usize,
    /// Whether the model saw the whole file or a window of it.
    ///
    /// Only ever used to add a note. It is deliberately not a second gate: an
    /// exact match of a search string is itself evidence that the model has the
    /// text right, and refusing an edit whose target text matched byte for byte
    /// because the display happened to stop at line 40 would be a rule that
    /// punishes a correct model.
    pub whole_file: bool,
}

/// Per-session: what has been read, and what it looked like.
///
/// Interior mutability because [`crate::runtime::Tool::invoke`] takes `&self` —
/// tools are values in a registry and are shared across calls, which is what
/// keeps them free of per-call state.
#[derive(Debug, Default)]
pub struct FileLedger {
    seen: Mutex<BTreeMap<String, Seen>>,
}

impl FileLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the model has been shown this content for this path.
    pub fn record(&self, path: &str, content: &[u8], whole_file: bool) {
        let key = normalise(path);
        let digest = crate::spill::content_hash(content);
        let Ok(mut g) = self.seen.lock() else { return };
        // A later partial read of the **same bytes** does not un-see a whole-file
        // read: the strongest claim about what the model was shown is the one that
        // stands. A partial read of *different* bytes does, because the file moved
        // and the old claim is about content that is gone.
        let still_whole = g
            .get(&key)
            .is_some_and(|s| s.whole_file && s.digest == digest);
        g.insert(
            key,
            Seen {
                digest,
                bytes: content.len(),
                whole_file: whole_file || still_whole,
            },
        );
    }

    pub fn seen(&self, path: &str) -> Option<Seen> {
        self.seen.lock().ok()?.get(&normalise(path)).cloned()
    }

    /// Paths this session has read, in a stable order. Clause 1's material for a
    /// refusal: *"you have read these, and this is not one of them."*
    pub fn paths(&self) -> Vec<String> {
        match self.seen.lock() {
            Ok(g) => g.keys().cloned().collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn forget(&self, path: &str) {
        if let Ok(mut g) = self.seen.lock() {
            g.remove(&normalise(path));
        }
    }
}

/// The key two spellings of one path share.
///
/// `./src/lib.rs`, `src//lib.rs` and `src/lib.rs` are one file, and a ledger that
/// keyed on the model's spelling would let a re-spelling walk past the guard.
/// This is lexical only: the backend's own `stat` is what resolves a symlink, and
/// [`crate::builtins::edit`] prefers its answer when there is one.
pub fn normalise(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_spellings_of_one_path_are_one_entry() {
        let l = FileLedger::new();
        l.record("./src//lib.rs", b"hello", true);
        assert!(l.seen("src/lib.rs").is_some());
        assert!(l.seen("src/util/../lib.rs").is_some());
        assert_eq!(l.paths(), vec!["src/lib.rs".to_string()]);
    }

    #[test]
    fn a_changed_file_has_a_different_digest() {
        let l = FileLedger::new();
        l.record("a", b"one", true);
        let before = l.seen("a").unwrap();
        l.record("a", b"two", true);
        assert_ne!(before.digest, l.seen("a").unwrap().digest);
    }

    #[test]
    fn a_partial_read_after_a_whole_one_does_not_un_see_the_file() {
        let l = FileLedger::new();
        l.record("a", b"one", true);
        l.record("a", b"one", false);
        assert!(l.seen("a").unwrap().whole_file);
    }

    #[test]
    fn an_unread_path_is_absent_rather_than_empty() {
        let l = FileLedger::new();
        assert_eq!(l.seen("nope"), None);
    }
}
