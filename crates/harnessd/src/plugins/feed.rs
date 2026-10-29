//! **The feeds a pane is handed, built where the data lives** — `docs/plugins-plan.md` §4.9,
//! "Feeds".
//!
//! # Why this module exists
//!
//! A pane never runs anything for its data: it **declares** which of the host's own facts it reads
//! (`plugin.toml`'s `feeds = ["sessions"]`) and is handed exactly those, under their names, in
//! `Input::feeds`. That was true of the head's in-process session list from the day feeds landed
//! (`crates/tui/src/app/sessions_pane.rs` builds one for the picker) and **was not true of a pane
//! the daemon draws**: `input_value` wrote `v`, `state`, `look`, `event` and `size`, so a layer on
//! disk that declared a feed was handed nothing and drew an empty pane. A declaration the host
//! does not honour is worse than no declaration: it reads as a broken plugin.
//!
//! So the daemon builds the feeds too, from the one thing it holds that a head does not — the
//! registry, which knows every session this daemon has opened *and* every row the store holds for
//! one it has not.
//!
//! # What is fed, and what is not
//!
//! Only `sessions` today: it is the feed that exists (`letibot_plugin::pane::Feed::ALL` names four
//! more, and they are still planned). A layer that declares `monitors`, `jobs`, `todos` or `git` is
//! handed **no key at all** rather than an empty list — `[]` would tell it *there are none*, which
//! is a different fact from *this host cannot answer that yet*, and only the layer can draw the
//! difference.
//!
//! # The three fields only a host can fill
//!
//! * `now` — the host's clock. A guest compiled for `wasm32-unknown-unknown` has no `SystemTime`;
//!   asking for one is a panic, not a zero (`.letibot/notes/the-pane-contracts-two-look-keys.md`).
//!   So an age is the host's arithmetic and the layer's drawing.
//! * `moved_ms` on each session — when it last appended a row, off the store's own row. The age of
//!   that number is the answer to *is that one stuck?*, which is what a list of agents is read for.
//! * `repo` on each session — the folder of the workspace's git **common** directory, so a session
//!   in `…/letibot/worktrees/agent-x` is grouped with the checkout it was cut from. A plugin cannot
//!   work this out from a path (a worktree may live anywhere), so the host that can ask git does.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use letibot_sessionlog::registry::Registry;
use serde_json::{Value, json};

/// **Where a pane's session list comes from.** One implementation in the daemon (the registry);
/// a test stands in its own, which is what makes a pane's feed testable without a daemon.
pub trait SessionFeed: Send + Sync {
    /// Every session this host knows, live and on disk, as the `sessions` feed spells one.
    fn rows(&self) -> Vec<Value>;
}

/// **The daemon's own list**: the registry's briefs — which already join the live sessions with
/// the store's rows (`Registry::list`) — plus the repository each workspace is in.
pub struct RegistryFeed {
    registry: Arc<Registry>,
    /// **The repository of each workspace, asked of git once.** A `git rev-parse` per session per
    /// frame would be a process per row every five seconds, and a workspace's repository does not
    /// change while a daemon runs. A workspace git cannot answer for is cached as empty too, so a
    /// folder outside any repository costs one call and not one per frame.
    repos: Mutex<HashMap<PathBuf, String>>,
}

impl RegistryFeed {
    pub fn new(registry: Arc<Registry>) -> RegistryFeed {
        RegistryFeed {
            registry,
            repos: Mutex::new(HashMap::new()),
        }
    }

    /// The repository `workspace` is in, or `""` when it is in none (or git could not say).
    fn repo(&self, workspace: &str) -> String {
        if workspace.trim().is_empty() {
            return String::new();
        }
        let path = PathBuf::from(workspace);
        if let Ok(cache) = self.repos.lock()
            && let Some(found) = cache.get(&path)
        {
            return found.clone();
        }
        let found = repository_of(&path);
        if let Ok(mut cache) = self.repos.lock() {
            cache.insert(path, found.clone());
        }
        found
    }
}

impl SessionFeed for RegistryFeed {
    fn rows(&self) -> Vec<Value> {
        self.registry
            .list()
            .into_iter()
            .map(|b| {
                let workspace = b.wiring.workspace.clone();
                let repo = self.repo(&workspace);
                json!({
                    "id": b.session_id,
                    "title": b.title,
                    "slug": b.slug,
                    "parent": b.parent_session_id,
                    // The origin's wire word — the same one the store holds.
                    "origin": b.origin.as_str(),
                    "live": b.live,
                    "running": b.status.running,
                    "heads": b.status.heads,
                    "items": b.status.items,
                    "stored": b.stored_items,
                    "model": b.wiring.model,
                    "workspace": workspace,
                    "waits_on": b.waits_on_operator,
                    "nags_paused": b.nags_paused.is_some(),
                    "moved_ms": b.moved_ms,
                    "repo": repo,
                })
            })
            .collect()
    }
}

/// **The `sessions` feed as a pane is handed it**: the rows, the session the pane belongs to (the
/// one whose pane this is — that is what `here` means to a pane the daemon draws), the host's home
/// and the host's clock.
pub fn sessions(source: &dyn SessionFeed, here: &str) -> Value {
    json!({
        "sessions": source.rows(),
        "here": here,
        "home": home(),
        "now": now_ms(),
    })
}

/// The host's home, so a layer can draw `~/…` — the same shortening the head's own list does.
fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// The host's clock, in unix millis. See this module's note: a guest has none.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// **The folder of `workspace`'s git common directory** — the checkout a worktree was cut from,
/// which is what a person means by *this project*.
///
/// `--git-common-dir` rather than `--show-toplevel` on purpose: inside a worktree the top level IS
/// the worktree (`…/letibot/worktrees/agent-x`), and grouping by it would put every worktree in a
/// project of its own. `--path-format=absolute` because a plain repository answers `.git`, which
/// would otherwise be resolved against whatever directory the daemon happens to be in.
fn repository_of(workspace: &Path) -> String {
    if !workspace.is_dir() {
        return String::new();
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        // A repository with no network access must fail rather than prompt: the daemon is not a
        // terminal, and a git waiting on a username is a pane that never answers.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output();
    let Ok(out) = out else {
        return String::new();
    };
    if !out.status.success() {
        return String::new();
    }
    let common = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if common.is_empty() {
        return String::new();
    }
    let common = PathBuf::from(common);
    match common.file_name().and_then(|n| n.to_str()) {
        Some(".git") => common
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        // A bare repository, or a layout whose common dir is not called `.git`: the directory
        // itself is the closest honest answer.
        _ => common.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A feed says `0` and `""` rather than guessing** — the two facts only the host can answer,
    /// and the two a plugin must draw as *not told* rather than as *now* and *its own folder*.
    #[test]
    fn a_workspace_outside_a_repository_is_an_empty_answer() {
        assert_eq!(repository_of(Path::new("/definitely/not/here")), "");
        let dir = std::env::temp_dir().join(format!("letibot-feed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        assert_eq!(
            repository_of(&dir),
            "",
            "a plain directory is in no repository, and an empty answer says so"
        );
        let _ = std::fs::remove_dir(&dir);
    }

    /// **A worktree's repository is the checkout it was cut from**, not the worktree — the whole
    /// reason `--git-common-dir` is asked instead of `--show-toplevel`.
    #[test]
    fn a_worktrees_repository_is_the_checkout_not_the_worktree() {
        let root = std::env::temp_dir().join(format!("letibot-feed-git-{}", std::process::id()));
        let repo = root.join("proj");
        let tree = root.join("proj").join("worktrees").join("agent-x");
        std::fs::create_dir_all(&tree).expect("the tree");
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.com")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !git(&["init", "-q"]) {
            // A box with no git: the sentence a reader needs, not a red test (AGENTS.md).
            eprintln!("skipped: this box has no `git` to ask");
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let made = git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent-x",
            tree.to_str().unwrap(),
        ]);
        assert!(made, "the worktree");
        // **git answers a RESOLVED path**, and on this box `/var` is a symlink to `/private/var`
        // — so the expectation is canonicalised rather than compared against the spelling this
        // test made the directory with.
        let want = std::fs::canonicalize(&repo)
            .expect("the checkout")
            .display()
            .to_string();
        assert_eq!(
            repository_of(&tree),
            want,
            "a worktree's repository is the checkout"
        );
        assert_eq!(repository_of(&repo), want, "and the checkout's is itself");
        let _ = std::fs::remove_dir_all(&root);
    }
}
