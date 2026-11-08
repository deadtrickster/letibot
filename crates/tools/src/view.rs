//! The paths the operator granted into a session's filesystem view, and the one card a path gets.
//!
//! A grant on a directory covers every path under it at that access: a read-write grant covers
//! reads and writes below it, a read-only one covers reads below it. The record belongs to a
//! session TREE rather than to one session, because a grant made for one subagent is the same
//! answer for its siblings and its parent - a card per child for one path is one question asked
//! as many times as there are children. And a path already being asked about gets no second
//! card: the asker that finds one open waits for its answer instead.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How long a second ask for a path already being asked about waits for that card's answer.
/// A card's own deadline is a fifth of an hour, so this is the same order: a sibling is
/// released by the answer and only a wedged asker makes it time out.
pub const SHARED_ASK_WAIT: Duration = Duration::from_secs(300);

/// Which access a view ask is about. A read-only grant covers a read and not a write, and that
/// is the whole difference between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewAccess {
    Read,
    Write,
}

impl ViewAccess {
    /// Whether a grant of `writable` covers this access.
    pub fn covered_by(self, writable: bool) -> bool {
        writable || self == ViewAccess::Read
    }
}

/// One path a call could not see, and what the call wanted to do with it. `bash` finds these in
/// a command's output; the runtime asks about each through the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewAsk {
    pub path: PathBuf,
    pub access: ViewAccess,
}

impl ViewAsk {
    pub fn read(path: impl Into<PathBuf>) -> ViewAsk {
        ViewAsk {
            path: path.into(),
            access: ViewAccess::Read,
        }
    }

    pub fn write(path: impl Into<PathBuf>) -> ViewAsk {
        ViewAsk {
            path: path.into(),
            access: ViewAccess::Write,
        }
    }
}

/// What one entry of the record is: a path, and whether a write is covered by it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Granted {
    path: PathBuf,
    writable: bool,
}

/// The canonical key of a path, for every comparison made here: `..` resolved, `.` and a
/// trailing slash dropped, and a symbol link followed as far as the path exists - so a file a
/// command wanted to CREATE is still compared at its real place. Two spellings of one path share
/// a key, and a path that only STARTS with a granted name - `/build-other` against `/build` -
/// does not.
pub fn key(path: &Path) -> PathBuf {
    let lexical = lexical(path);
    // The deepest ancestor that is really there, so the tail that is not (yet) is joined onto the
    // resolved prefix rather than compared as the literal a caller spelled.
    let mut probe = lexical.clone();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(&probe) {
            return tail.iter().rev().fold(real, |at, seg| at.join(seg));
        }
        match (probe.file_name().map(|n| n.to_os_string()), probe.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                probe = parent.to_path_buf();
            }
            // Nothing of it is there: the lexical key is the honest answer.
            _ => return lexical,
        }
    }
}

/// [`key`] without touching the filesystem: a grant is often made before the path exists.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            // `pop` on the root answers `false` and leaves it, which is what `..` at the top
            // of an absolute path means.
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The grant record: what was granted, and which paths have a card open right now.
#[derive(Debug, Default)]
pub struct ViewGrants {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    granted: Vec<Granted>,
    open: HashMap<PathBuf, Arc<Open>>,
}

/// One card, and the asks that wait on it instead of raising a second.
#[derive(Debug, Default)]
pub struct Open {
    settled: Mutex<Option<Answer>>,
    cv: Condvar,
    /// How many asks have waited on this card. Read back by [`ViewGrants::waiting_on`], which is
    /// how a caller can tell *a sibling is behind this card* from *nobody else has asked*.
    waiting: Mutex<usize>,
}

/// What came of the one card raised for a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Granted {
        writable: bool,
    },
    Refused(String),
    /// Nobody answered: the call has to be told that rather than told no.
    Nobody,
}

impl Open {
    /// The answer to the card this waits on, or `None` if it did not arrive within `budget`.
    pub fn wait(&self, budget: Duration) -> Option<Answer> {
        let deadline = Instant::now() + budget;
        let mut g = self.settled.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(a) = g.as_ref() {
                return Some(a.clone());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            let (next, _) = self
                .cv
                .wait_timeout(g, left)
                .unwrap_or_else(|e| e.into_inner());
            g = next;
        }
    }
}

/// The claim a caller made on a path: it raises the card and must [`ViewGrants::settle`] it.
#[derive(Debug)]
pub struct Raised {
    pub path: PathBuf,
    key: PathBuf,
    open: Arc<Open>,
}

/// What an asker must do about a path.
#[derive(Debug)]
pub enum Claim {
    /// A grant already covers it at this access: no card.
    Covered { writable: bool },
    /// A card for this path is already up: wait on it rather than raise a second.
    Shares(Arc<Open>),
    /// Nobody is asking: raise the card, then [`ViewGrants::settle`] it.
    Raises(Raised),
}

impl ViewGrants {
    pub fn new() -> ViewGrants {
        ViewGrants::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The widest access a recorded grant gives `path` at `access`: `Some(true)` when a write is
    /// covered, `Some(false)` when only a read is, `None` when nothing covers it.
    pub fn covers(&self, path: &Path, access: ViewAccess) -> Option<bool> {
        covering(&self.lock(), &key(path), access)
    }

    /// Every path the record holds, as `(path, writable)`, oldest first.
    pub fn entries(&self) -> Vec<(PathBuf, bool)> {
        self.lock()
            .granted
            .iter()
            .map(|e| (e.path.clone(), e.writable))
            .collect()
    }

    /// How many asks have taken the card up for `path` instead of raising another. A path with
    /// no card reads 0, which is the same number as a card nobody else has asked about yet.
    pub fn waiting_on(&self, path: &Path) -> usize {
        let g = self.lock();
        match g.open.get(&key(path)) {
            Some(open) => *open.waiting.lock().unwrap_or_else(|e| e.into_inner()),
            None => 0,
        }
    }

    /// Write down that `path` is in the view at this access - the operator's answer to the card,
    /// or a grant handed in from elsewhere. A later wider answer widens the entry rather than
    /// adding a second one for the same path.
    pub fn record(&self, path: &Path, writable: bool) {
        let key = key(path);
        let mut g = self.lock();
        match g.granted.iter_mut().find(|e| e.path == key) {
            Some(e) => e.writable |= writable,
            None => g.granted.push(Granted {
                path: key,
                writable,
            }),
        }
    }

    /// Claim the one card for this path, or take the answer somebody else already has.
    pub fn claim(&self, path: &Path, access: ViewAccess) -> Claim {
        let key = key(path);
        let mut g = self.lock();
        if let Some(writable) = covering(&g, &key, access) {
            return Claim::Covered { writable };
        }
        if let Some(open) = g.open.get(&key) {
            *open.waiting.lock().unwrap_or_else(|e| e.into_inner()) += 1;
            return Claim::Shares(open.clone());
        }
        let open = Arc::new(Open::default());
        g.open.insert(key.clone(), open.clone());
        Claim::Raises(Raised {
            path: path.to_path_buf(),
            key,
            open,
        })
    }

    /// Answer the card a claim raised. The grant is recorded BEFORE any waiting asker is woken,
    /// so a sibling that reads the answer also finds the grant that came with it.
    pub fn settle(&self, raised: Raised, answer: Answer) {
        if let Answer::Granted { writable } = &answer {
            self.record(&raised.path, *writable);
        }
        *raised
            .open
            .settled
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(answer);
        raised.open.cv.notify_all();
        let mut g = self.lock();
        // Only this card's entry: settling twice must not take a later card's place.
        if g.open
            .get(&raised.key)
            .is_some_and(|o| Arc::ptr_eq(o, &raised.open))
        {
            g.open.remove(&raised.key);
        }
    }
}

/// Whether the record covers `path` at `access`. A write is covered only by a read-write grant;
/// a read by either.
fn covering(g: &Inner, path: &Path, access: ViewAccess) -> Option<bool> {
    let mut read_only = false;
    for e in &g.granted {
        if !path.starts_with(&e.path) {
            continue;
        }
        if e.writable {
            return Some(true);
        }
        read_only = true;
    }
    (read_only && access.covered_by(false)).then_some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_of_one_path_is_one_key() {
        let one = key(Path::new("/build/letibot-r114/review/"));
        assert_eq!(one, key(Path::new("/build/letibot-r114/review")));
        assert_eq!(one, key(Path::new("/build/./letibot-r114//review")));
        assert_eq!(one, key(Path::new("/build/x/../letibot-r114/review")));
        assert_eq!(key(Path::new("/build/")), key(Path::new("/build")));
    }

    #[test]
    fn a_name_that_only_starts_the_same_way_is_not_under_a_grant() {
        let g = ViewGrants::new();
        g.record(Path::new("/build"), true);
        assert_eq!(
            g.covers(Path::new("/build-other/x"), ViewAccess::Read),
            None
        );
        assert_eq!(
            g.covers(
                Path::new("/build/letibot-r114/review/chunk.jsonl"),
                ViewAccess::Read
            ),
            Some(true)
        );
    }

    #[test]
    fn a_read_only_grant_covers_a_read_below_it_and_not_a_write() {
        let g = ViewGrants::new();
        g.record(Path::new("/home/dead/skills"), false);
        assert_eq!(
            g.covers(
                Path::new("/home/dead/skills/briefs/full.md"),
                ViewAccess::Read
            ),
            Some(false)
        );
        assert_eq!(
            g.covers(
                Path::new("/home/dead/skills/briefs/full.md"),
                ViewAccess::Write
            ),
            None,
            "a write is the wider ask, and a read-only grant is not an answer to it"
        );
    }

    #[test]
    fn a_wider_grant_at_a_parent_covers_what_a_narrower_one_does_not() {
        let g = ViewGrants::new();
        g.record(Path::new("/build/review"), false);
        g.record(Path::new("/build"), true);
        assert_eq!(
            g.covers(Path::new("/build/review/chunk.jsonl"), ViewAccess::Write),
            Some(true)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_does_not_exist_yet_is_compared_at_its_real_place() {
        let dir = crate::backend::tempdir::TempDir::new();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).expect("a directory to grant");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("a symbol link to grant through");
        let g = ViewGrants::new();
        g.record(&link, true);
        let not_yet = link.join("sub/file.jsonl");
        assert!(!not_yet.exists());
        assert_eq!(g.covers(&not_yet, ViewAccess::Write), Some(true));
        // And the real name is the same grant, whichever way it was spelled.
        assert_eq!(
            g.covers(&real.join("sub/file.jsonl"), ViewAccess::Write),
            Some(true)
        );
    }

    #[test]
    fn a_second_claim_for_an_open_path_shares_the_card_and_its_answer() {
        let g = ViewGrants::new();
        let raised = match g.claim(Path::new("/build/x/"), ViewAccess::Read) {
            Claim::Raises(r) => r,
            other => panic!("the first ask raises the card: {other:?}"),
        };
        // The second ask arrives while the first card is up: it shares the card instead of
        // raising another, whatever spelling of the path it was given.
        let open = match g.claim(Path::new("/build/x"), ViewAccess::Write) {
            Claim::Shares(open) => open,
            other => panic!("the second ask waits on the first card: {other:?}"),
        };
        assert_eq!(g.waiting_on(Path::new("/build/x")), 1);
        g.settle(raised, Answer::Granted { writable: true });
        assert_eq!(
            open.wait(SHARED_ASK_WAIT),
            Some(Answer::Granted { writable: true })
        );
        assert_eq!(
            g.covers(Path::new("/build/x/y"), ViewAccess::Write),
            Some(true)
        );
    }

    #[test]
    fn a_denied_card_does_not_become_a_standing_denial() {
        let g = ViewGrants::new();
        let raised = match g.claim(Path::new("/build/x"), ViewAccess::Read) {
            Claim::Raises(r) => r,
            other => panic!("the first ask raises the card: {other:?}"),
        };
        g.settle(raised, Answer::Refused("no".into()));
        assert_eq!(g.covers(Path::new("/build/x"), ViewAccess::Read), None);
        assert!(matches!(
            g.claim(Path::new("/build/x"), ViewAccess::Read),
            Claim::Raises(_)
        ));
    }
}
