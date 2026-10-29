//! The seat's instruction files: the markdown a leticode reads because it says
//! how to work in this project.
//!
//! > *"leticode can support claude.md. but not letibot"*
//! >
//! > *"let it just look for the canonical claude.md if our leticode.md doesnt
//! > exist"*
//! >
//! > *"and on this host they both link to the same claude.md from handoffs"*
//!
//! Three sentences and one rule: a project's instructions may be written under
//! the name this seat gives them ([`INSTRUCTION_FILE`]) or under the canonical
//! name every other tool already reads ([`CANONICAL_INSTRUCTION_FILE`]), and the
//! SEAT is the thing that knows both. The runtime must not — *"i want letibot
//! stay a framework, a runtime"* — so the two names are spelled here and
//! nowhere in `letibot-harnessd`, which asks this crate for a list of paths and
//! reads them. A runtime that knew the names would be the framework deciding
//! what a leticode is; and one that knew a checkout path, as the first cut of
//! this did, would be worse: *"dont you think hardcoding the repo checkout path
//! is a bit too much"*.
//!
//! # Two scopes, and the rule in each
//!
//! * **Workspace** — walk up from the session's workspace. At the first
//!   directory holding either name, the seat's own wins; and a file found nearer
//!   beats one further up, in either name. This is [`crate::LeticodeConfig::discover`]'s
//!   walk with a preference inside each directory rather than one name.
//! * **Box** — the config dir this crate already reads ([`user_path`]'s
//!   directory, `~/.config/letibot/` in the default spelling): its own
//!   `leticode.md` if it is there, else the canonical `~/.claude/CLAUDE.md`. On
//!   the operator's box the two are links to one file — the third sentence above
//!   — so the fallback costs nothing there and is still the whole reason a
//!   `CLAUDE.md` written for another tool reaches a leticode at all.
//!
//! **Neither present is an answer, not a fault.** A seat with no instruction
//! file has none: [`instructions`] answers an empty list, nothing is created,
//! nothing is warned about and nothing is refused. That is what makes this safe
//! to ask on every session open.
//!
//! # Why a `BoxDirs` rather than `$HOME` read where it is needed
//!
//! Because a test that read the box it runs on would pass or fail with
//! `~/.claude/CLAUDE.md` — the operator's own file, which exists on the box this
//! was written on. The two directories are a value the caller supplies
//! ([`BoxDirs::for_this_box`] in production), which is the same shape
//! `standing_notes::section` already takes its box-wide notes directory in.

use std::path::{Path, PathBuf};

// The one function that knows where the config dir is — see [`BoxDirs::for_this_box`].
use crate::user_path;

/// The seat's own name for an instruction file — `leticode.md`, beside
/// `leticode.toml` at a project's root and in the config dir.
pub const INSTRUCTION_FILE: &str = "leticode.md";

/// The canonical name, the one other tools read — `CLAUDE.md`. Looked for in the
/// same directory [`INSTRUCTION_FILE`] would be in, and on the box in
/// `~/.claude/`.
pub const CANONICAL_INSTRUCTION_FILE: &str = "CLAUDE.md";

/// The canonical name's directory under the box's home.
const CANONICAL_DIR: &str = ".claude";

/// **The two directories a box-scope answer comes from, given rather than
/// discovered.**
///
/// A field each, because they are two different facts: the config dir is where
/// this crate's own files live (and [`user_path`] already decides it), and the
/// home is where another tool's canonical file lives. The default is two empty
/// paths — a box with no instruction file of its own, which is what a fixture
/// wants and what an unset `$HOME` degrades to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoxDirs {
    /// The seat's config dir — `~/.config/letibot/` in the default spelling.
    pub config: PathBuf,
    /// The box's home directory — `~/.claude/CLAUDE.md` hangs off it.
    pub home: PathBuf,
}

impl BoxDirs {
    /// This box's: the config dir [`user_path`] answers from, and `$HOME`.
    ///
    /// Derived from [`user_path`] rather than from `$HOME` spelled a second time,
    /// so a `$XDG_CONFIG_HOME` that `providers.toml` honours is honoured here
    /// too — the same argument [`user_path`] itself makes.
    pub fn for_this_box() -> BoxDirs {
        BoxDirs {
            config: user_path()
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default(),
        }
    }
}

/// **The seat's instruction files for a session at `workspace`, nearest first**
/// — at most two, and none when the seat has no instruction file anywhere.
///
/// The workspace's comes first because it is the more specific statement: a
/// project that says how to build itself outranks a box that says how to work.
/// The caller reads what this hands it; which name answered is not its business,
/// and that is the whole point of the split.
pub fn instructions(workspace: &Path, box_dirs: &BoxDirs) -> Vec<PathBuf> {
    let mut out = Vec::with_capacity(2);
    for found in [workspace_scope(workspace), box_scope(box_dirs)]
        .into_iter()
        .flatten()
    {
        // One file twice would be one instruction given twice. A workspace that
        // IS the config dir is the only way to reach it, and two lines not to
        // spend the prompt on a duplicate is a fair trade.
        if !out.contains(&found) {
            out.push(found);
        }
    }
    out
}

/// The workspace scope: the nearest directory at or above `workspace` holding
/// either name, and inside it the seat's own before the canonical one.
///
/// `pop` returns `false` at the filesystem root, which is where the walk stops —
/// a box with no instruction file anywhere is a no-file, not a loop.
fn workspace_scope(workspace: &Path) -> Option<PathBuf> {
    let mut dir = workspace.to_path_buf();
    loop {
        if let Some(found) = in_dir(&dir) {
            return Some(found);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// The box scope: the config dir's own file, else the canonical one under the
/// box's home.
fn box_scope(box_dirs: &BoxDirs) -> Option<PathBuf> {
    in_dir(&box_dirs.config).or_else(|| {
        let canonical = box_dirs
            .home
            .join(CANONICAL_DIR)
            .join(CANONICAL_INSTRUCTION_FILE);
        // `is_file` rather than `exists`: a directory named `CLAUDE.md` is not a
        // file anything can read, and a dangling symlink is the same nothing.
        canonical.is_file().then_some(canonical)
    })
}

/// One directory's answer: the seat's own name, else the canonical one.
fn in_dir(dir: &Path) -> Option<PathBuf> {
    [INSTRUCTION_FILE, CANONICAL_INSTRUCTION_FILE]
        .into_iter()
        .map(|name| dir.join(name))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch dir per CALL, not per name — twenty tests may ask for `"ws"` and
    /// a shared path would have them racing each other's writes. Temp files only:
    /// nothing is written inside the tree, and nothing is removed.
    fn dir(name: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "leticode-instructions-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("a scratch dir");
        d
    }

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("the file's directory");
        }
        std::fs::write(&path, text).expect("a fixture file");
        path
    }

    /// **A box with no instruction file of its own.** Every test that is not about
    /// the box scope points here, so the box the test RUNS on cannot answer for
    /// it — the difference between a rule and a fact about this machine.
    fn no_box() -> BoxDirs {
        BoxDirs {
            config: dir("box-config"),
            home: dir("box-home"),
        }
    }

    /// The seat's own name is what a project writes, and it is what answers.
    #[test]
    fn a_workspace_with_the_seats_own_name_uses_it() {
        let ws = dir("own-name");
        let own = write(&ws, INSTRUCTION_FILE, "build with cargo test -p\n");
        assert_eq!(instructions(&ws, &no_box()), vec![own]);
    }

    /// **A project written for another tool still reaches this seat** — the
    /// canonical name is the fallback, not a second-class citizen.
    #[test]
    fn a_workspace_with_only_the_canonical_name_uses_that() {
        let ws = dir("canonical-only");
        let canonical = write(&ws, CANONICAL_INSTRUCTION_FILE, "run the gate\n");
        assert_eq!(instructions(&ws, &no_box()), vec![canonical]);
    }

    /// Both names in one directory: the seat's own wins, and the canonical file is
    /// not read as well — one directory answers with one file.
    #[test]
    fn both_names_in_one_directory_prefer_the_seats_own() {
        let ws = dir("both-names");
        let own = write(&ws, INSTRUCTION_FILE, "the seat's own words\n");
        write(&ws, CANONICAL_INSTRUCTION_FILE, "somebody else's\n");
        assert_eq!(instructions(&ws, &no_box()), vec![own]);
    }

    /// **A nearer file wins over a further one, in EITHER name** — the walk stops
    /// at the first directory that answers, so a project's `CLAUDE.md` is not
    /// outranked by a parent's `leticode.md`.
    #[test]
    fn a_nearer_directory_wins_over_a_further_one() {
        // The near directory holds only the canonical name; the far one holds the
        // seat's own. Near still wins.
        let root = dir("near-wins");
        write(&root, INSTRUCTION_FILE, "the far, seat-named file\n");
        let near = root.join("crates").join("harnessd");
        std::fs::create_dir_all(&near).expect("the near directory");
        let near_file = write(&near, CANONICAL_INSTRUCTION_FILE, "the near file\n");
        assert_eq!(
            instructions(&near, &no_box()),
            vec![near_file],
            "the nearest directory answers, whatever it is named"
        );

        // And the other way round: the near directory's own name wins over the
        // far one's.
        let root2 = dir("near-own");
        write(
            &root2,
            CANONICAL_INSTRUCTION_FILE,
            "the far, canonical file\n",
        );
        let near2 = root2.join("crates");
        std::fs::create_dir_all(&near2).expect("the near directory");
        let near2_file = write(&near2, INSTRUCTION_FILE, "the near file\n");
        assert_eq!(instructions(&near2, &no_box()), vec![near2_file]);
    }

    /// **Neither name anywhere is no files and no error** — the normal answer for
    /// a project that wrote neither, which is most of them.
    #[test]
    fn neither_name_is_no_file_and_no_error() {
        let ws = dir("nothing");
        write(
            &ws,
            "AGENTS.md",
            "not an instruction file this rule knows\n",
        );
        write(&ws, "leticode.toml", "main_model = \"local\"\n");
        assert!(
            instructions(&ws, &no_box()).is_empty(),
            "a config file and a notes file are not instruction files"
        );
    }

    /// The box scope's own name, in the config dir the seat already reads.
    #[test]
    fn the_box_scope_prefers_the_seats_own_name() {
        let ws = dir("box-own-ws");
        let box_dirs = no_box();
        let own = write(&box_dirs.config, INSTRUCTION_FILE, "the box's own words\n");
        write(&box_dirs.home, ".claude/CLAUDE.md", "the canonical file\n");
        assert_eq!(instructions(&ws, &box_dirs), vec![own]);
    }

    /// **The box's canonical file is the fallback** — `~/.claude/CLAUDE.md`, the
    /// one other tools write, which on the operator's box is a link to the same
    /// file as the config dir's.
    #[test]
    fn the_box_scope_falls_back_to_the_canonical_name() {
        let ws = dir("box-canonical-ws");
        let box_dirs = no_box();
        let canonical = write(&box_dirs.home, ".claude/CLAUDE.md", "the canonical file\n");
        assert_eq!(instructions(&ws, &box_dirs), vec![canonical]);
    }

    /// A box with neither is nothing added — not a heading, not a warning.
    #[test]
    fn a_box_with_neither_name_adds_nothing() {
        let ws = dir("box-nothing-ws");
        assert!(instructions(&ws, &no_box()).is_empty());
    }

    /// **The workspace's file comes first**, because it is the more specific
    /// statement, and both are handed over when both exist.
    #[test]
    fn the_workspace_comes_before_the_box() {
        let ws = dir("order-ws");
        let box_dirs = no_box();
        let own = write(&ws, INSTRUCTION_FILE, "the project's words\n");
        let box_file = write(&box_dirs.config, INSTRUCTION_FILE, "the box's words\n");
        assert_eq!(instructions(&ws, &box_dirs), vec![own, box_file]);
    }

    /// One file reached by both scopes is listed once — a workspace that is the
    /// config dir is the only way to get here, and one instruction given twice is
    /// worse than one given once.
    #[test]
    fn one_file_reached_twice_is_listed_once() {
        let box_dirs = no_box();
        let own = write(&box_dirs.config, INSTRUCTION_FILE, "the box's words\n");
        assert_eq!(instructions(&box_dirs.config, &box_dirs), vec![own]);
    }

    /// **The names this crate owns are the two the seat answers to**, asserted
    /// through the API rather than by reading a file: a rename here is a rename
    /// everywhere, and the runtime holds no copy to forget.
    #[test]
    fn the_two_names_are_the_ones_the_seat_answers_to() {
        assert_eq!(INSTRUCTION_FILE, "leticode.md");
        assert_eq!(CANONICAL_INSTRUCTION_FILE, "CLAUDE.md");
        let ws = dir("names");
        for name in [INSTRUCTION_FILE, CANONICAL_INSTRUCTION_FILE] {
            let path = write(&ws, name, "x\n");
            assert_eq!(
                instructions(&ws, &no_box()),
                vec![path],
                "{name} is one of the two names this rule looks for"
            );
            std::fs::remove_file(ws.join(name)).expect("clean up for the next name");
        }
    }
}
