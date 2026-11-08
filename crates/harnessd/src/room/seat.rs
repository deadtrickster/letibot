//! The daemon's half of a seat's presence: the facts the harness writes at its own moments (a turn,
//! a model call, a limit, an edit, the board) and the link sends every tick. Nothing here is the
//! agent's word except the declared status, which expires.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use letibot_room::seat::{Call, Declared, Edit, RECENT_FILES, SeatFacts, WorkingOn, status_ttl};
use letibot_sessionlog::Hub;
use letibot_sessionlog::event::{TodoEntry, TodoStatus, todo_title};

/// One session's facts, shared by its harness (the writer) and its room link (the reader).
pub struct SeatCell {
    workspace: PathBuf,
    hub: Option<Arc<Hub>>,
    facts: Mutex<SeatFacts>,
    /// The rows not yet closed, by title: a declared status set on a row clears when it closes.
    open_rows: Mutex<Vec<String>>,
    /// This seat's live claims as the room last answered: what a passing line is judged against.
    owns: Mutex<Vec<String>>,
}

impl SeatCell {
    pub fn new(workspace: impl Into<PathBuf>, hub: Option<Arc<Hub>>) -> Arc<SeatCell> {
        Arc::new(SeatCell {
            workspace: workspace.into(),
            hub,
            facts: Mutex::new(SeatFacts::default()),
            open_rows: Mutex::new(Vec::new()),
            owns: Mutex::new(Vec::new()),
        })
    }

    fn with<T>(&self, f: impl FnOnce(&mut SeatFacts) -> T) -> T {
        f(&mut self.facts.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn turn_began(&self, at_ms: u64) {
        self.with(|f| f.turn_since_ms = Some(at_ms));
    }

    pub fn turn_ended(&self, at_ms: u64) {
        self.with(|f| {
            f.turn_since_ms = None;
            f.idle_since_ms = Some(at_ms);
        });
    }

    /// A model call came back: `error` is `None` for an answer, the provider's words for a failure.
    pub fn called(&self, at_ms: u64, error: Option<String>) {
        self.with(|f| {
            f.last_call = Some(Call {
                ok: error.is_none(),
                at_ms,
                error: error.unwrap_or_default(),
            })
        });
    }

    /// Parked on a limit until `until_ms`, or `None` when the park ended.
    pub fn limited(&self, until: Option<(u64, String)>) {
        self.with(|f| match until {
            Some((at, words)) => {
                f.limited_until_ms = Some(at);
                f.limit = words;
            }
            None => {
                f.limited_until_ms = None;
                f.limit.clear();
            }
        });
    }

    /// A file this session changed, relative to its root as the edit tools label it.
    pub fn edited(&self, path: &str) {
        self.edited_at(path, letibot_room::server::now_ms());
    }

    pub fn edited_at(&self, path: &str, at_ms: u64) {
        let path = path.trim().to_string();
        if path.is_empty() {
            return;
        }
        self.with(|f| {
            let files = &mut f.working.files;
            files.retain(|p| *p != path);
            files.insert(0, path.clone());
            files.truncate(RECENT_FILES);
            f.edits.retain(|e| e.path != path);
            f.edits.insert(0, Edit { path, at_ms });
            f.edits.truncate(RECENT_FILES);
        });
    }

    pub fn set_owns(&self, owns: Vec<String>) {
        *self.owns.lock().unwrap_or_else(|e| e.into_inner()) = owns;
    }

    pub fn owns(&self) -> Vec<String> {
        self.owns.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The board changed: the in-progress row is what it works on, and a status set on a row that
    /// has closed is cleared.
    pub fn todos(&self, todos: &[TodoEntry]) {
        let title = |t: &TodoEntry| todo_title(&t.content).to_string();
        let doing = todos
            .iter()
            .find(|t| t.status == TodoStatus::InProgress)
            .map(title)
            .unwrap_or_default();
        let open: Vec<String> = todos
            .iter()
            .filter(|t| matches!(t.status, TodoStatus::InProgress | TodoStatus::Pending))
            .map(title)
            .collect();
        self.with(|f| {
            f.working.row = doing;
            if f.working
                .status
                .as_ref()
                .is_some_and(|s| !s.row.is_empty() && !open.contains(&s.row))
            {
                f.working.status = None;
            }
        });
        *self.open_rows.lock().unwrap_or_else(|e| e.into_inner()) = open;
    }

    /// The agent's one-line status, bound to the row in progress now. Empty text clears it.
    pub fn declare(&self, text: &str, ttl_ms: Option<u64>, now: u64) -> String {
        let text: String = text
            .trim()
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect();
        self.with(|f| {
            if text.is_empty() {
                f.working.status = None;
                return "status cleared".to_string();
            }
            let ttl = status_ttl(ttl_ms);
            let row = f.working.row.clone();
            f.working.status = Some(Declared {
                text: text.clone(),
                until_ms: now.saturating_add(ttl),
                row: row.clone(),
            });
            let on = if row.is_empty() {
                String::new()
            } else {
                format!(", cleared when \"{row}\" closes")
            };
            format!(
                "status set for {}{on}; the roster shows your row, branch and files without it",
                letibot_room::duration(ttl)
            )
        })
    }

    /// The facts as of `now`: an expired status dropped, the place read from the workspace's git
    /// files, the operator's cards from the hub.
    pub fn facts(&self, now: u64) -> SeatFacts {
        let (repo, branch, worktree) = git_place(&self.workspace);
        let cards = self
            .hub
            .as_ref()
            .map(|h| h.open_ask_ids().len() as u32)
            .unwrap_or(0);
        self.with(|f| {
            if f.working.status.as_ref().is_some_and(|s| s.until_ms <= now) {
                f.working.status = None;
            }
            f.working.repo = repo;
            f.working.branch = branch;
            f.working.worktree = worktree;
            f.cards = cards;
            f.clone()
        })
    }

    /// Does a passing line touch this seat's work: a scope it owns, a file it edited, its branch
    /// or worktree? `None` when the seat has nothing to judge by, and the caller wakes as before.
    pub fn overlaps(&self, text: &str) -> Option<bool> {
        let owns = self.owns();
        let w = self.with(|f| f.working.clone());
        let (repo_branch, worktree) = {
            let (_, b, t) = git_place(&self.workspace);
            (b, t)
        };
        let branch = Some(repo_branch).filter(|b| !matches!(b.as_str(), "" | "main" | "master"));
        let worktree = Some(worktree).filter(|t| !t.is_empty());
        if owns.is_empty() && w.files.is_empty() && branch.is_none() && worktree.is_none() {
            return None;
        }
        let lower = text.to_lowercase();
        let words: Vec<&str> = text
            .split(|c: char| {
                c.is_whitespace() || matches!(c, '`' | '"' | '\'' | ',' | ';' | '(' | ')')
            })
            .map(|t| t.trim_end_matches(['.', ':', '!', '?']))
            .filter(|t| !t.is_empty())
            .collect();
        let named_path = |scope: &str| {
            words.iter().any(|t| {
                letibot_room::own::is_path(t)
                    && (letibot_room::own::covers(scope, t)
                        || letibot_room::own::overlap(scope, &letibot_room::own::normalise(t)))
            })
        };
        let owned = owns.iter().any(|scope| {
            if letibot_room::own::is_path(scope) {
                named_path(scope)
            } else {
                lower
                    .split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
                    .any(|t| t == scope)
            }
        });
        let edited = w.files.iter().any(|f| {
            let base = f.rsplit('/').next().unwrap_or(f);
            words.iter().any(|t| {
                *t == f
                    || t.ends_with(&format!("/{f}"))
                    || f.ends_with(&format!("/{t}"))
                    || (*t == base && base.contains('.'))
            })
        });
        let place = [branch, worktree]
            .into_iter()
            .flatten()
            .any(|p| words.iter().any(|t| *t == p));
        Some(owned || edited || place)
    }

    /// What the seat works on now, for the ownership renewal and the wake's overlap.
    pub fn working(&self) -> WorkingOn {
        self.with(|f| f.working.clone())
    }
}

/// The repository's folder, the branch and the worktree's folder (empty when it is the main
/// checkout), read from git's own files: a process per tick per seat would be the daemon's idle load.
pub fn git_place(workspace: &Path) -> (String, String, String) {
    let mut at = Some(workspace);
    while let Some(dir) = at {
        let dot = dir.join(".git");
        if dot.is_dir() {
            return (folder(dir), branch_of(&dot), String::new());
        }
        if dot.is_file() {
            let gitdir = std::fs::read_to_string(&dot)
                .ok()
                .and_then(|t| {
                    t.trim()
                        .strip_prefix("gitdir:")
                        .map(|g| g.trim().to_string())
                })
                .map(|g| dir.join(g));
            let Some(gitdir) = gitdir else {
                return (folder(dir), String::new(), String::new());
            };
            // A worktree's gitdir is `<main>/.git/worktrees/<name>`; its `commondir` names `<main>/.git`.
            let common = std::fs::read_to_string(gitdir.join("commondir"))
                .ok()
                .map(|c| gitdir.join(c.trim()));
            let main = common
                .as_deref()
                .and_then(|c| std::fs::canonicalize(c).ok())
                .and_then(|c| c.parent().map(Path::to_path_buf));
            return match main {
                Some(main) => (folder(&main), branch_of(&gitdir), folder(dir)),
                None => (folder(dir), branch_of(&gitdir), String::new()),
            };
        }
        at = dir.parent();
    }
    (String::new(), String::new(), String::new())
}

fn folder(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `HEAD`'s branch, or the short sha of a detached head.
fn branch_of(gitdir: &Path) -> String {
    let head = std::fs::read_to_string(gitdir.join("HEAD")).unwrap_or_default();
    let head = head.trim();
    match head.strip_prefix("ref: refs/heads/") {
        Some(b) => b.to_string(),
        None => head.chars().take(10).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(content: &str, status: TodoStatus) -> TodoEntry {
        serde_json::from_value(serde_json::json!({
            "content": content,
            "status": status,
        }))
        .expect("a todo row")
    }

    #[test]
    fn the_last_three_files_edited_are_kept_newest_first() {
        let c = SeatCell::new("/nowhere", None);
        for p in ["a.rs", "b.rs", "a.rs", "c.rs", "d.rs"] {
            c.edited(p);
        }
        assert_eq!(c.working().files, vec!["d.rs", "c.rs", "a.rs"]);
    }

    #[test]
    fn a_declared_status_expires_and_clears_when_its_row_closes() {
        let c = SeatCell::new("/nowhere", None);
        c.todos(&[row("wire the port", TodoStatus::InProgress)]);
        let said = c.declare("bisecting\nsecond line", Some(60_000), 1_000);
        assert!(
            said.contains("cleared when \"wire the port\" closes"),
            "{said}"
        );
        let f = c.facts(2_000);
        let s = f.working.status.expect("set");
        assert_eq!(s.text, "bisecting", "one line only");
        assert_eq!(s.row, "wire the port");
        assert!(c.facts(61_000).working.status.is_none(), "expired");

        c.declare("again", None, 100_000);
        c.todos(&[
            row("wire the port", TodoStatus::Pending),
            row("other", TodoStatus::InProgress),
        ]);
        let w = c.facts(100_001).working;
        assert!(w.disagrees(), "still open, on another row now: {w:?}");
        c.todos(&[row("wire the port", TodoStatus::Completed)]);
        assert!(c.facts(100_002).working.status.is_none(), "its row closed");
        assert_eq!(c.declare("", None, 0), "status cleared");
    }

    #[test]
    fn a_turn_a_call_and_a_limit_become_facts() {
        let c = SeatCell::new("/nowhere", None);
        c.turn_began(10);
        c.called(11, Some("503 overloaded".into()));
        let f = c.facts(12);
        assert_eq!(f.turn_since_ms, Some(10));
        assert!(!f.last_call.as_ref().unwrap().ok);
        c.turn_ended(20);
        c.limited(Some((99, "paused until 21:02:27 UTC".into())));
        let f = c.facts(21);
        assert_eq!((f.turn_since_ms, f.idle_since_ms), (None, Some(20)));
        assert_eq!(f.limited_until_ms, Some(99));
        c.limited(None);
        assert!(c.facts(22).limit.is_empty());
    }

    /// The link sends the cell's facts on its tick, and `status` is the one verb answered here.
    #[test]
    fn a_links_facts_and_status_reach_the_rooms_roster() {
        let id = "s-seat-link";
        let dir = std::env::temp_dir().join(format!("letibot-seat-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");
        let bell: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_: &str| {});
        let link = crate::room::link::SessionRoom::connect(
            id,
            &path,
            letibot_room::DEFAULT_PROJECT,
            None,
            bell,
            None,
        )
        .expect("connects");
        let cell = SeatCell::new(&dir, None);
        link.set_seat(cell.clone());
        cell.turn_began(letibot_room::server::now_ms());
        cell.todos(&[row("wire the port", TodoStatus::InProgress)]);
        let said = link
            .call(
                "status",
                &serde_json::json!({"text": "bisecting", "ttl": 5}),
            )
            .expect("status is answered by the daemon");
        assert!(said.starts_with("status set for 5m"), "{said}");
        let seat = server
            .roster_of(letibot_room::HOST_ROOM)
            .into_iter()
            .find(|p| p.id == id)
            .and_then(|p| p.seat)
            .expect("the session is in the roster with a seat view");
        assert!(seat.says.starts_with("in turn"), "{}", seat.says);
        assert!(seat.doing.starts_with("\"bisecting\""), "{}", seat.doing);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `own` through the link reaches the room, the tick brings the scope back, and a passing
    /// line rings the owner only when it names the scope; otherwise it waits as a note.
    #[test]
    fn a_passing_line_rings_only_the_seat_whose_scope_it_names() {
        let dir = std::env::temp_dir().join(format!("letibot-seat-ring-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let _server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");
        let rung: Arc<Mutex<Vec<String>>> = Arc::default();
        let bell: Arc<dyn Fn(&str) + Send + Sync> = {
            let rung = Arc::clone(&rung);
            Arc::new(move |s: &str| rung.lock().unwrap().push(s.to_string()))
        };
        let connect = |id: &str| {
            crate::room::link::SessionRoom::connect(
                id,
                &path,
                letibot_room::DEFAULT_PROJECT,
                None,
                bell.clone(),
                None,
            )
            .expect("connects")
        };
        let (me, other) = (connect("s-ring-a"), connect("s-ring-b"));
        let cell = SeatCell::new(&dir, None);
        me.set_seat(cell.clone());
        let said = me
            .call("own", &serde_json::json!({"scope": "crates/room/**"}))
            .expect("own");
        assert!(said.starts_with("you own `crates/room/**`"), "{said}");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while cell.owns().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(cell.owns(), ["crates/room/**"]);

        let say = |text: &str| {
            other
                .call("say", &serde_json::json!({"room": "host", "text": text}))
                .expect("say");
        };
        let rang_for_me = || {
            rung.lock()
                .unwrap()
                .iter()
                .filter(|s| *s == "s-ring-a")
                .count()
        };
        say("lunch anyone?");
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert_eq!(rang_for_me(), 0, "no overlap: no bell");
        say("crates/room/src/own.rs panics on an empty scope");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while rang_for_me() == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(rang_for_me(), 1, "the owner is rung");
        let notes = crate::room::wake::take("s-ring-a");
        assert!(
            notes.iter().any(|n| n.contains("lunch anyone?")),
            "the other line waits as a note: {notes:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_passing_line_overlaps_an_owned_scope_an_edited_file_or_the_branch() {
        let c = SeatCell::new("/nowhere", None);
        assert_eq!(c.overlaps("the gate is red"), None, "nothing to judge by");
        c.set_owns(vec!["crates/room/**".into(), "flaky-gate".into()]);
        c.edited("crates/tui/src/app/rooms.rs");
        assert_eq!(
            c.overlaps("why does `crates/room/src/own.rs` panic?"),
            Some(true)
        );
        assert_eq!(c.overlaps("the flaky-gate is back"), Some(true));
        assert_eq!(
            c.overlaps("rooms.rs has a typo"),
            Some(true),
            "an edited file by name"
        );
        assert_eq!(c.overlaps("lunch?"), Some(false));
        assert_eq!(c.overlaps("crates/http/src/lib.rs is slow"), Some(false));
    }

    #[test]
    fn the_place_is_read_from_git_files_for_a_checkout_and_a_worktree() {
        let root = std::env::temp_dir().join(format!("letibot-seat-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let main = root.join("letibot");
        let wt = root.join("wt-x");
        std::fs::create_dir_all(main.join(".git/worktrees/wt-x")).unwrap();
        std::fs::create_dir_all(wt.join("crates")).unwrap();
        std::fs::write(main.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            main.join(".git/worktrees/wt-x/HEAD"),
            "ref: refs/heads/z-r163\n",
        )
        .unwrap();
        std::fs::write(main.join(".git/worktrees/wt-x/commondir"), "../..\n").unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/wt-x").display()),
        )
        .unwrap();
        assert_eq!(
            git_place(&main),
            ("letibot".into(), "main".into(), String::new())
        );
        assert_eq!(
            git_place(&wt.join("crates")),
            ("letibot".into(), "z-r163".into(), "wt-x".into())
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
