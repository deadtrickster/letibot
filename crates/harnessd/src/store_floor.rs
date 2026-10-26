//! **This daemon's binary against the store it serves** — the sentence every head is told when the
//! file was written by a newer build, and the refusal when it cannot be read at all.
//!
//! # Why there is a module for two functions
//!
//! Because the two facts are one question asked once, at the one moment the answer exists: the
//! store's own record of **what a reader must know** (`letibot_tokencore`'s `store::floor`), and
//! **which binary is asking**. The second is this process's, and no crate below here may know it —
//! a store that formatted `std::env::current_exe` would be a store that knows about executables.
//!
//! # What the operator asked for, and what each half does
//!
//! 2026-10-26, after a schema bump 32 → 33 (one nullable column) refused every daemon built before
//! it: *an older binary should keep working when the newer schema only ADDED things, and must say
//! so in red on the head's usual status line*.
//!
//! * **Keep working** is [`said`]'s `Ok(None)` — that is, nothing to say, because the store is this
//!   build's or older. `Store::open` itself decided it by the floor, so a daemon that gets here has
//!   already been allowed to serve.
//! * **Keep working, and say so** is `Ok(Some(sentence))`: the store is ahead and its floor says
//!   this build reads it. The sentence goes on the registry
//!   (`Registry::set_store_older`), and `sessionlog`'s `seat_in` publishes it to every head that
//!   attaches — as `store_newer`, a `Warning`, which is the wire that already exists.
//! * **Refused** is `Err`. A store whose floor is above this build, or one too old to record a
//!   floor at all, is not something an older daemon should touch, and the message names the fix:
//!   `restart this daemon from a current build: letibot --stop`.
//!
//! # Why it names the binary
//!
//! Because *"this daemon is older"* is a fact about a process on a box that usually has several,
//! and the operator's next question is *which one*. The path is what a person re-execs; the
//! modification time is what tells a rebuilt binary from the one still running
//! (`~/bin/letibot` execs `target/release`, and a stale `target/release` is exactly the trap this
//! whole file exists to make visible). Both are best-effort: a binary the kernel will not name is
//! said to be unnameable rather than guessed at, the rule `tui`'s `pid_word` keeps for a pid.

use std::path::Path;

use crate::config::Config;

/// **What to tell every head, or `Ok(None)` when there is nothing to tell.**
///
/// `Err` is a store this build must not serve — the message is the whole sentence a person needs,
/// including the command that fixes it, and the caller makes it fatal.
pub fn said(cfg: &Config) -> Result<Option<String>, String> {
    let Some(path) = cfg.store.as_ref() else {
        return Ok(None);
    };
    match letibot_tokencore::store::Store::open(path) {
        // **`newer_store()` and not a version comparison of our own**: whether an older reader may
        // read a newer file is the store's declaration to make (`store::floor`), and a second
        // opinion here is a second answer to the one question that has to have one.
        Ok(store) => Ok(store.newer_store().map(|newer| {
            format!(
                "{} · running {}",
                newer.sentence(),
                build_of(std::env::current_exe().ok().as_deref())
            )
        })),
        // **A refusal is fatal and says why.** Every other store error is left to the paths that
        // already report them — a `database is locked` at startup is a wait, not a sentence about
        // this binary, and turning it into one would be a daemon that refuses to start because a
        // neighbour was committing.
        Err(e @ letibot_tokencore::store::StoreError::SchemaTooNew { .. }) => {
            Err(format!("store: opening {path:?}: {e}"))
        }
        Err(_) => Ok(None),
    }
}

/// **Which binary this is** — its path and when it was built, in the words a person reads.
///
/// Both halves can fail and neither is worth failing a daemon over: a `current_exe` the platform
/// will not answer, and a modification time a filesystem will not give, are each said as themselves
/// rather than as a plausible default. `exe` is a parameter rather than a call to
/// `std::env::current_exe` inside, so a test can ask about a path of its own.
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

    /// **The sentence names the versions, the fix, and the process** — the four things the
    /// operator asked for in one line, and the whole of what a head draws.
    #[test]
    fn the_sentence_names_the_versions_the_fix_and_the_binary() {
        let newer = letibot_tokencore::store::NewerStore {
            found: 34,
            known: 33,
        };
        let said = format!("{} · running {}", newer.sentence(), build_of(None));
        assert!(
            said.starts_with(
                "this daemon (schema 33) is older than its store (34) — restart it: letibot --stop"
            ),
            "{said}"
        );
        assert!(
            said.contains("a binary the platform would not name"),
            "a nameless binary is said as that, never as a path nobody checked: {said}"
        );
    }

    /// **A binary that is not there is not a build time of zero** — the same rule as the nameless
    /// case, one branch along: the path is still the one that was asked about (it is what
    /// `current_exe` answered), and the time says it could not be read.
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
