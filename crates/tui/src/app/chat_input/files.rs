//! The workspace's file names for `@` completion: one gitignore-aware walk per folder, kept in a
//! cache and refreshed behind the frame. A Tab must answer at once and a walk of a large tree does
//! not, so what a keypress ranks is always a list a walk has already landed.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The most files one walk keeps. A tree larger than this is one whose tail nobody reads, and the
/// cap is what bounds both the walk's work and the memory the cache holds per folder.
pub(crate) const CAP: usize = 20_000;

/// How old a list may be before the next read starts another walk behind the frame. Two seconds is
/// the interval the header's own git reading uses: quick enough that a file just written is
/// offered, slow enough that a burst of `@` words costs one walk.
const STALE: Duration = Duration::from_secs(2);

/// The directories every walk skips, whatever the ignore files say: the same fixed set `grep` and
/// `glob` skip, so a `@` word offers the files a search would find.
const SKIP: [&str; 6] = [
    ".git",
    "target",
    "node_modules",
    ".venv",
    "__pycache__",
    ".cache",
];

/// One file the walk found: the path as it is spelled, and its lowercased copy, so ranking a
/// keypress compares against a string that is already folded rather than folding thousands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Name {
    pub path: String,
    pub lower: String,
}

/// One folder's names: what a walk landed and when, and whether one is running.
#[derive(Default)]
struct Cell {
    /// `None` until the first walk lands, which is not the same fact as an empty folder.
    files: Mutex<Option<(Instant, Arc<Vec<Name>>)>>,
    running: AtomicBool,
}

/// A cache key: the folder, and the directory part a fragment named - `@` walks the workspace and
/// `@src/` walks `src`, so one entry is one directory's own file list.
type Key = (String, String);

fn cells() -> &'static Mutex<HashMap<Key, Arc<Cell>>> {
    static CELLS: OnceLock<Mutex<HashMap<Key, Arc<Cell>>>> = OnceLock::new();
    CELLS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cell(folder: &str, dir: &str) -> Arc<Cell> {
    let key = (folder.to_string(), dir.trim_end_matches('/').to_string());
    let mut cells = cells().lock().unwrap_or_else(|e| e.into_inner());
    cells.entry(key).or_default().clone()
}

/// The files under `folder/dir`, as paths relative to `dir`, from the cache - and a walk started to
/// fill or refresh it. This never walks: a keypress reads what is already there.
pub(crate) fn under(folder: &str, dir: &str) -> Arc<Vec<Name>> {
    let cell = cell(folder, dir);
    let landed = cell.files.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let stale = match &landed {
        Some((at, _)) => at.elapsed() >= STALE,
        None => true,
    };
    if stale {
        start(cell, folder, dir);
    }
    landed.map(|(_, files)| files).unwrap_or_default()
}

/// Fill a folder's list if it has never been filled, and do nothing once it has: the loop calls
/// this a pass, so the first `@` of a session is already warm. Costs one lock and no walk.
pub(crate) fn warm(folder: &str) {
    let cell = cell(folder, "");
    let cold = cell
        .files
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_none();
    if cold {
        start(cell, folder, "");
    }
}

/// Has the walk for this folder landed? The door a test waits on: the walk is a thread's, so what a
/// test can prove is that the answer arrives, not that it was there when the question was asked.
pub(crate) fn landed(folder: &str, dir: &str) -> bool {
    // The same call a keypress makes, so a poll starts the walk it is waiting for.
    let _ = under(folder, dir);
    cell(folder, dir)
        .files
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some()
}

/// A walk in flight, which puts the folder back to unclaimed however the walk ends - a panicking
/// walk would otherwise leave its folder with no list and no way to ask for one again.
struct Running(Arc<Cell>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::Release);
    }
}

/// Start one walk for this folder, unless one is already running there.
fn start(cell: Arc<Cell>, folder: &str, dir: &str) {
    if cell.running.swap(true, Ordering::AcqRel) {
        return;
    }
    let root = Path::new(folder).join(dir.trim_end_matches('/'));
    let guard = Running(cell.clone());
    let spawned = std::thread::Builder::new().name("at-files".into()).spawn({
        let cell = cell.clone();
        move || {
            let files = Arc::new(walk(&root, CAP));
            {
                let mut slot = cell.files.lock().unwrap_or_else(|e| e.into_inner());
                *slot = Some((Instant::now(), files));
            }
            drop(guard);
        }
    });
    // No thread, no list: the next read asks again rather than keeping a folder that never fills.
    if spawned.is_err() {
        cell.running.store(false, Ordering::Release);
    }
}

/// Every file under `root`, by git's own rules: `.gitignore` files, `.git/info/exclude` and the
/// global excludes file - the set `grep` and `glob` read. `.ignore` and `.rgignore` are another
/// tool's rules and are not read there either. Hidden files are listed, as a search lists them.
pub(crate) fn walk(root: &Path, cap: usize) -> Vec<Name> {
    let mut out = Vec::new();
    let walk = ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .ignore(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .filter_entry(|e| {
            !e.file_type().is_some_and(|t| t.is_dir())
                || !SKIP.contains(&e.file_name().to_str().unwrap_or(""))
        })
        .build();
    for entry in walk.flatten() {
        if out.len() >= cap {
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let path = rel.to_string_lossy().to_string();
        out.push(Name {
            lower: path.to_lowercase(),
            path,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch tree, named for the test that asked: the walk's own answers do not depend on any
    /// other test's files.
    fn tree(what: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "letibot-at-walk-{what}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        for (p, body) in files {
            let f = d.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, body).unwrap();
        }
        d
    }

    fn paths(root: &Path, cap: usize) -> Vec<String> {
        let mut got: Vec<String> = walk(root, cap).into_iter().map(|n| n.path).collect();
        got.sort();
        got
    }

    /// The cap is what keeps a huge tree affordable: the walk stops at it rather than reading on.
    #[test]
    fn a_walk_keeps_at_most_its_cap() {
        let d = tree(
            "cap",
            &[("a.rs", ""), ("b.rs", ""), ("c.rs", ""), ("d.rs", "")],
        );
        assert_eq!(paths(&d, 2).len(), 2, "the walk stopped at its cap");
        assert_eq!(paths(&d, CAP).len(), 4, "and keeps everything under it");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A `.gitignore`, a nested one and git's own exclude file are all honoured, and the fixed skip
    /// set besides - the rules a search applies, since a `@` word offers what a search would find.
    #[test]
    fn a_walk_applies_gitignore_and_gits_exclude_file() {
        let d = tree(
            "rules",
            &[
                (".gitignore", "gen/\n*.tmp\n"),
                (".git/info/exclude", "notes.txt\n"),
                ("src/main.rs", ""),
                ("src/a.tmp", ""),
                ("gen/out.rs", ""),
                ("notes.txt", ""),
                ("target/debug/x.rs", ""),
            ],
        );
        assert_eq!(
            paths(&d, CAP),
            vec![".gitignore".to_string(), "src/main.rs".to_string()],
            "an ignored tree, an ignored file, the repository's exclude and the skip set"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The list a keypress reads is the walk's, never the caller's: a read answers with what the
    /// cache holds, and a file that appeared after the walk lands only when another read asks for
    /// a fresh one - which happens behind the frame, as the poll below shows.
    #[test]
    fn a_read_answers_from_the_cache_and_a_stale_one_asks_for_another_walk() {
        let d = tree("cold", &[("src/main.rs", "")]);
        let folder = d.to_string_lossy().to_string();
        letibot_sessionlog::testing::wait_until("the walk to land", || landed(&folder, ""));
        let names = under(&folder, "");
        assert_eq!(names.len(), 1, "the walk's answer: {names:?}");
        assert_eq!(names[0].path, "src/main.rs");
        assert_eq!(names[0].lower, "src/main.rs");

        std::fs::write(d.join("src/after.rs"), "").unwrap();
        assert_eq!(
            under(&folder, "").len(),
            1,
            "the read walked the folder itself"
        );
        letibot_sessionlog::testing::wait_until("the list to be refreshed", || {
            under(&folder, "").len() == 2
        });
        let _ = std::fs::remove_dir_all(&d);
    }
}
