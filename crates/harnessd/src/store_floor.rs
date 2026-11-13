//! This daemon's binary against the store it serves: the sentence every head is told when
//! the store was written by a newer build, and the refusal when it cannot be read at all.
//! The two facts are one question asked once, at the one moment the answer exists

use std::path::Path;

use crate::config::Config;

/// What to tell every head, or `Ok(None)` when there is nothing to tell. `Err` is a store
/// this build must not serve; the message names the fix and the caller makes it fatal.
pub fn said(cfg: &Config) -> Result<Option<String>, String> {
    let Some(at) = cfg.store_at() else {
        return Ok(None);
    };
    match at.open_existing() {
        // `newer_store` and not a version comparison of our own: whether an
        // older reader may read a newer file is the store's declaration to make
        // and a second opinion here is a second answer to one question.
        Ok(store) => Ok(store.newer_store().map(|newer| {
            sentence(
                &newer,
                std::env::current_exe().ok().as_deref(),
                crate::exe_stamp::replaced().as_deref(),
            )
        })),
        // A refusal is fatal and says why. Every other store error is left to the paths that
        // already report them: a `database is locked` at startup is a wait, not a sentence about
        // this binary, and making it one would refuse to start because a neighbour was committing.
        Err(e @ letibot_tokencore::store::StoreError::SchemaTooNew { .. }) => {
            Err(format!("store: opening {}: {e}", at.display()))
        }
        Err(_) => Ok(None),
    }
}

/// The store's sentence, the binary, and a restart only when one would run another binary: the
/// file this daemon started from has been replaced since.
fn sentence(
    newer: &letibot_tokencore::store::NewerStore,
    exe: Option<&Path>,
    replaced: Option<&Path>,
) -> String {
    let mut said = format!("{} · running {}", newer.sentence(), build_of(exe));
    if let Some(path) = replaced {
        said.push_str(&format!(
            " · {} has been rebuilt since this daemon started - restart it: letibot --restart",
            path.display()
        ));
    }
    said
}

/// Which binary this is: its path and when it was built, in a person's words. Both halves
/// can fail and neither is worth failing a daemon over, so each is said as itself. `exe` is
/// a parameter so a test can ask about its own path.
fn build_of(exe: Option<&Path>) -> String {
    let Some(exe) = exe else {
        return "a binary the platform would not name".into();
    };
    let shown = exe.display().to_string();
    let built = std::fs::metadata(exe)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| {
            let ms = d.as_millis() as u64;
            crate::nag_pause::clock_at(ms, ms)
        })
        .unwrap_or_else(|| "a build time this filesystem would not give".into());
    format!("{shown}, built {built}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sentence names the versions, the fix and the process, which is the
    /// whole of what a head draws.
    #[test]
    fn the_sentence_names_the_versions_the_fix_and_the_binary() {
        let newer = letibot_tokencore::store::NewerStore {
            found: 34,
            known: 33,
            floor: 30,
        };
        let said = sentence(&newer, None, None);
        assert!(
            said.starts_with(
                "a newer build (schema 34) wrote this store; this one (schema 33) reads it \
                 (floor 30) - install the newer build to clear this"
            ),
            "{said}"
        );
        assert!(
            !said.contains("restart"),
            "nothing newer is installed, so a restart would change nothing: {said}"
        );
        let rebuilt = sentence(&newer, None, Some(Path::new("/x/target/release/harnessd")));
        assert!(
            rebuilt.ends_with(
                "/x/target/release/harnessd has been rebuilt since this daemon started - restart \
                 it: letibot --restart"
            ),
            "{rebuilt}"
        );
        assert!(
            said.contains("a binary the platform would not name"),
            "a nameless binary is said as that, never as a path nobody checked: {said}"
        );
    }

    /// A binary that is not there is not a build time of zero: the path is still
    /// the one that was asked about, and the time says it could not be read.
    #[test]
    fn an_unreadable_build_time_says_so_and_keeps_the_path() {
        let said = build_of(Some(Path::new("/nonexistent/letibot-harnessd")));
        assert!(
            said.starts_with("/nonexistent/letibot-harnessd, built "),
            "{said}"
        );
        assert!(
            said.contains("would not give"),
            "an unreadable mtime must not read as a build time: {said}"
        );
    }
}
