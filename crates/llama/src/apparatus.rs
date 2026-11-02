//! Whether a vocabulary is on this machine AND this build can load it.
//!
//! [`letibot_tokencore::apparatus`] answers the first question and on its own it is the
//! wrong one: a GGUF can exist and be unloadable, a split shard whose siblings are
//! missing or an architecture this llama.cpp does not know, and a test that skipped on
//! `is_file` alone failed inside [`crate::load`] rather than saying the apparatus was absent.
//!
//! So presence here means the load succeeded once, cached by path, length and mtime.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// **One measurement**: the file it was taken on (path, byte length, modification time) and whether
/// this build could load it. Only the answer is kept, never the model: a few megabytes of token
/// table per entry would be a leak with a cache in front of it.
type Measured = (PathBuf, u64, u128);

static MEASURED: Mutex<Vec<(Measured, bool)>> = Mutex::new(Vec::new());

/// The box's vocabulary, or `None` with the skip already announced.
pub fn present_gguf() -> Option<PathBuf> {
    let path = letibot_tokencore::apparatus::gguf_path();
    present_file(&path)?;
    Some(path)
}

/// [`present_gguf`] for a named file, for a test that pins its own rather than the box's.
pub fn present_file(path: &Path) -> Option<()> {
    let loadable = loadable(path);
    letibot_tokencore::apparatus::present(
        &format!(
            "a vocabulary GGUF ({}) this build can load — llama.cpp has to know the file's \
             architecture, and a split model needs its other shards",
            path.display()
        ),
        loadable,
    )
}

/// **The load, once per version of the file** - and the loader's own words the first time it
/// fails, so a reader learns which half was missing rather than only that something was.
fn loadable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let key = (path.to_path_buf(), meta.len(), modified);
    let mut measured = MEASURED.lock().expect("the vocabulary cache");
    if let Some((_, said)) = measured.iter().find(|(k, _)| *k == key) {
        return *said;
    }
    let said = match crate::load(path) {
        Ok(_) => true,
        Err(why) => {
            eprintln!(
                "  the vocabulary at {} cannot be loaded here: {why}",
                path.display()
            );
            false
        }
    };
    measured.push((key, said));
    said
}
