//! The built-in model, pinned: a commit on Hugging Face, three files, and a hash for each.
//!
//! `mode = "internal"` with no `model` is this checkpoint, and none of it is compiled in. The
//! pin is a commit rather than a branch and a hash rather than a length, so the bytes a box
//! fetches can be checked against the bytes somebody reviewed, and any other answer is refused.

use std::path::{Path, PathBuf};

use crate::model::BUILTIN_MODEL;

/// The commit the three files are pinned at. A branch would move under a box that fetched
/// before it moved, so this is a full hash, and every URL below is built under it.
///
/// **This is a placeholder.** It is `0000…` until the real revision is recorded, and
/// [`Pin::is_filled`] refuses a fetch until it is: a box must not embed with bytes nobody
/// pinned.
pub const REVISION: &str = "0000000000000000000000000000000000000000";

/// The repository's `resolve` root, with `/<revision>/<file>` under it.
pub const BASE: &str = "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve";

/// One file of the checkpoint: what it is called, what it must hash to, and what it weighs -
/// the last only so the one sentence before a fetch can say how much is coming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Source {
    pub file: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
}

/// **The three files, and the trust in them.** Hashes are placeholders in the same way
/// [`REVISION`] is, and a fetch refuses to run until all four are the real ones.
pub const SOURCES: [Source; 3] = [
    Source {
        file: "config.json",
        sha256: "PIN-config.json",
        bytes: 0,
    },
    Source {
        file: "tokenizer.json",
        sha256: "PIN-tokenizer.json",
        bytes: 0,
    },
    Source {
        file: "model.safetensors",
        sha256: "PIN-model.safetensors",
        bytes: 0,
    },
];

/// A pin a fetch runs against: the shipped table, or a test's own bytes, which is the only
/// way to exercise a hash check without the network.
#[derive(Debug, Clone)]
pub struct Pin {
    base: String,
    revision: String,
    sources: Vec<Source>,
}

impl Pin {
    /// The table this build ships.
    pub fn shipped() -> Pin {
        Pin {
            base: BASE.to_string(),
            revision: REVISION.to_string(),
            sources: SOURCES.to_vec(),
        }
    }

    /// A pin over files a caller names, which is what a test serves from its own socket.
    pub fn new(base: impl Into<String>, revision: impl Into<String>, sources: Vec<Source>) -> Pin {
        Pin {
            base: base.into(),
            revision: revision.into(),
            sources,
        }
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    /// Where this pin keeps its files: one directory per revision, so a moved pin is a new
    /// directory and never a half-replaced one.
    pub fn dir(&self, data_dir: &Path) -> PathBuf {
        data_dir
            .join("models")
            .join(BUILTIN_MODEL)
            .join(&self.revision)
    }

    /// The URL one file is served at.
    pub fn url(&self, file: &str) -> String {
        format!("{}/{}/{}", self.base, self.revision, file)
    }

    /// **Is the pin a real one?** A commit hash and a SHA-256 are fixed lengths, so a
    /// placeholder is a string of the wrong length or one that is not hexadecimal, and a fetch
    /// that ran on it would trust bytes nobody chose.
    pub fn is_filled(&self) -> bool {
        is_hex(&self.revision, 40) && self.sources.iter().all(|s| is_hex(s.sha256, 64))
    }

    /// Are all three files already here? Checked before anything is fetched, so a box that
    /// paid for the model once starts without reaching the network at all.
    pub fn present(&self, dir: &Path) -> bool {
        self.sources.iter().all(|s| dir.join(s.file).is_file())
    }
}

fn is_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The pin is the whole trust story.** A box that fetched whatever a branch held would
    /// embed with bytes nobody reviewed, so this fails until the table holds the real commit
    /// and the three real hashes.
    #[test]
    fn the_builtin_pin_names_a_commit_and_a_hash_for_every_file() {
        let pin = Pin::shipped();
        assert!(
            pin.is_filled(),
            "the built-in model is not pinned yet: REVISION is {:?} and the files are {}. Fill \
             all four in `crates/embeddings/src/builtin.rs` from the reviewed checkpoint.",
            pin.revision(),
            pin.sources()
                .iter()
                .map(|s| format!("{} = {:?}", s.file, s.sha256))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    /// One directory per revision, so a moved pin fetches into a new place instead of mixing
    /// its files with the ones already there.
    #[test]
    fn the_builtin_model_is_one_directory_per_revision() {
        let dir = Pin::shipped().dir(Path::new("/data"));
        assert_eq!(
            dir,
            Path::new("/data")
                .join("models")
                .join(BUILTIN_MODEL)
                .join(REVISION)
        );
        assert!(Pin::shipped()
            .url("config.json")
            .ends_with(&format!("/{REVISION}/config.json")));
    }
}
