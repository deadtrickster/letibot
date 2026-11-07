//! R121 items 4 and 8: the file tools obey the perimeter the shell's view draws. A path the view
//! denies is refused to `read`, `write`, `edit`, `grep` and `glob` in the boundary's own words,
//! naming the `[boundary]` key that would change it - and a session reaches its own
//! `sessions/<id>/` in the letibot store and nothing else of it. Every fixture home is a
//! temporary directory and every secret is a fake.

use std::path::{Path, PathBuf};

use letibot_tools::backend::HostBackend;
use letibot_tools::exec::boundary::{self, BoundaryConfig, Entry, Origin};
use letibot_tools::exec::confine::{HomeView, ViewSpec};
use letibot_tools::exec::fence::FileFence;
use letibot_tools::exec::seat::{self, Level};
use letibot_tools::mode::Mode;
use letibot_tools::testing::{Harness, harness_over};
use letibot_transcript::ToolOutcome;

/// A fake operator home with the real store's shape: `sessions.db` at the top, one folder per
/// session, a fake key, and a project beside it.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root =
            std::env::temp_dir().join(format!("letibot-filefence-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let f = Fixture { root };
        let store = f.store();
        std::fs::create_dir_all(store.join("sessions/s-1/plugins")).expect("own session");
        std::fs::create_dir_all(store.join("sessions/s-2/plugins")).expect("another session");
        std::fs::write(store.join("sessions.db"), "not a real store\n").expect("the store file");
        std::fs::write(store.join("sessions/s-1/plugins/x.toml"), "own plugin\n").expect("plugin");
        std::fs::write(
            store.join("sessions/s-2/plugins/other.toml"),
            "another session's plugin\n",
        )
        .expect("plugin");
        std::fs::create_dir_all(f.home().join(".ssh")).expect("a fake store");
        std::fs::write(f.home().join(".ssh/id_rsa"), "fake-key\n").expect("a fake key");
        std::fs::create_dir_all(f.project()).expect("a project");
        std::fs::write(f.project().join("main.rs"), "fn main() {}\n").expect("a source file");
        f
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn home(&self) -> PathBuf {
        self.root.join("operator")
    }

    /// `$XDG_DATA_HOME`-shaped: the box's store under the fixture home, so the seat's own masked
    /// list and this fence name the same directory.
    fn store(&self) -> PathBuf {
        self.home().join(".local/share/letibot")
    }

    fn project(&self) -> PathBuf {
        self.root.join("project")
    }

    /// The view a wide seat measures: the machine at its own paths, the stores masked.
    fn view(&self) -> ViewSpec {
        ViewSpec::project_only(self.project())
            .with_home(HomeView::Dir(self.home()))
            .wide()
    }

    fn seat(&self) -> seat::SeatView {
        seat::seat_for_level(
            Level::Wide,
            &Mode::WRITES_ALLOWED,
            "this machine",
            &[] as &[&str],
            &self.home(),
            None,
        )
    }

    /// A session over a backend rooted at `/` like a `leticode` seat's, fenced with its own seat.
    fn harness(&self) -> Harness {
        let backend = HostBackend::writable("/")
            .expect("a whole-host backend")
            .with_cwd(self.project())
            .expect("the workspace")
            .with_home(self.home())
            .with_file_fence(
                FileFence::of(&self.seat(), &self.view()).owning(&self.store(), "s-1"),
            );
        harness_over(Box::new(backend), self.project(), self.root.join("notes"))
    }
}

fn refused(r: &letibot_tools::result::ToolResult) -> String {
    match &r.outcome {
        ToolOutcome::Failed { reason } => reason.clone(),
        other => panic!("expected a failure, got {other:?}\n{}", r.render()),
    }
}

/// The one the row was filed for: the store is the daemon's and every session's, so a file tool
/// reaches `sessions/<id>/` and nothing else of it - by the same words a command gets.
#[test]
fn read_write_edit_grep_and_glob_refuse_the_store_outside_the_sessions_own_folder() {
    let f = Fixture::new("store");
    let mut h = f.harness();

    let store = f.store();
    let for_other = store.join("sessions/s-2/plugins/other.toml");
    let db = store.join("sessions.db");

    for tool in [
        format!(r#"{{"path":"{}"}}"#, db.display()),
        format!(r#"{{"path":"{}"}}"#, for_other.display()),
    ] {
        let why = refused(&h.call("read", &tool));
        assert!(why.contains("the letibot store"), "{why}");
        assert!(
            why.contains(&format!("{}", store.join("sessions/s-1").display())),
            "the refusal names the folder the session does have: {why}"
        );
        assert!(why.contains("never-grant"), "and no key grants it: {why}");
    }
    let why = refused(&h.call(
        "write",
        &format!(
            r#"{{"path":"{}","content":"x"}}"#,
            store.join("sessions/s-2/plugins/mine.toml").display()
        ),
    ));
    assert!(why.contains("the letibot store"), "{why}");
    let why = refused(&h.call(
        "edit",
        &format!(
            r#"{{"path":"{}","old_string":"another","new_string":"x"}}"#,
            for_other.display()
        ),
    ));
    assert!(why.contains("the letibot store"), "{why}");
    let why = refused(&h.call(
        "grep",
        &format!(r#"{{"pattern":"plugin","path":"{}"}}"#, store.display()),
    ));
    assert!(why.contains("the letibot store"), "{why}");
    assert!(
        !why.contains("no directory"),
        "a fenced path is denied, never reported absent: {why}"
    );
    let why = refused(&h.call(
        "glob",
        &format!(r#"{{"pattern":"**/*.toml","path":"{}"}}"#, store.display()),
    ));
    assert!(why.contains("the letibot store"), "{why}");

    // And the session's own folder in it is reachable, read and written: that is where its
    // plugins live, and the writer of a session's own layer is the session.
    let own = store.join("sessions/s-1/plugins");
    let r = h.call(
        "read",
        &format!(r#"{{"path":"{}"}}"#, own.join("x.toml").display()),
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.render());
    assert!(r.render().contains("own plugin"), "{}", r.render());
    let r = h.call(
        "write",
        &format!(
            r#"{{"path":"{}","content":"a layer\n"}}"#,
            own.join("new.toml").display()
        ),
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.render());
    assert_eq!(
        std::fs::read_to_string(own.join("new.toml")).unwrap_or_default(),
        "a layer\n",
        "the write landed, read off the disk rather than off the tool's opinion"
    );

    // The workspace is untouched by the fence: a file tool's ordinary work is not a refusal.
    let r = h.call("read", r#"{"path":"main.rs"}"#);
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.render());
    let _ = std::fs::remove_dir_all(f.root());
}

/// Item 4's second half: a tool's refusal names the key that would change it - `[boundary] hide`
/// with the file and line that wrote it, or the fact that no key reaches a credential store.
#[test]
fn a_tool_refusal_names_its_boundary_key() {
    let f = Fixture::new("key");
    let dir = f.root().join("cfg");
    std::fs::create_dir_all(&dir).expect("a config dir");
    let file = dir.join("leticode.toml");
    std::fs::write(
        &file,
        "# the operator's\n[boundary]\nhide = [\"/opt/tools\"]\n",
    )
    .expect("a config");
    let cfg = BoundaryConfig {
        hide: vec![Entry {
            value: "/opt/tools".into(),
            from: Origin {
                file: file.clone(),
                line: 3,
                user: true,
            },
        }],
        ..Default::default()
    };
    let resolved = boundary::resolve(&cfg, &f.home()).expect("resolves");
    let seat = boundary::apply(f.seat(), &resolved);
    let backend = HostBackend::writable("/")
        .expect("a whole-host backend")
        .with_cwd(f.project())
        .expect("the workspace")
        .with_home(f.home())
        .with_file_fence(FileFence::of(&seat, &f.view()));
    let mut h = harness_over(Box::new(backend), f.project(), f.root().join("notes"));

    let why = refused(&h.call("read", r#"{"path":"/opt/tools/run.sh"}"#));
    assert!(
        why.contains("`[boundary] hide`") && why.contains(&format!("{}:3", file.display())),
        "a hidden path names the key and where it was written: {why}"
    );

    // A credential store is the other answer, and it is an answer: no key grants it.
    let key = f.home().join(".ssh/id_rsa");
    let why = refused(&h.call("read", &format!(r#"{{"path":"{}"}}"#, key.display())));
    assert!(why.contains("`~/.ssh` is never granted"), "{why}");
    assert!(!why.contains("grants_ro"), "no key is offered: {why}");
    let _ = std::fs::remove_dir_all(f.root());
}

/// A tight seat's file tools are its view: a path outside it is refused naming `grants_ro`, and a
/// write under a read-only grant names `grants_rw` — the operator's keys, by name.
#[test]
fn a_tight_seat_refuses_outside_its_view_naming_the_key() {
    let f = Fixture::new("tight");
    let grant = letibot_tools::exec::confine::Grant::ReadOnly {
        path: f.home().join(".cargo"),
        why: "the toolchain".into(),
    };
    std::fs::create_dir_all(f.home().join(".cargo/registry")).expect("a toolchain");
    let view = ViewSpec::project_only(f.project())
        .granting(grant.clone())
        .with_home(HomeView::Dir(f.root().join("throwaway")));
    let seat = seat::seat_view(
        &Mode::WRITES_ALLOWED,
        "this machine",
        &[] as &[&str],
        &f.home(),
    );
    let backend = HostBackend::writable("/")
        .expect("a whole-host backend")
        .with_cwd(f.project())
        .expect("the workspace")
        .with_home(f.home())
        .with_file_fence(FileFence::of(&seat, &view));
    let mut h = harness_over(Box::new(backend), f.project(), f.root().join("notes"));

    let why = refused(&h.call(
        "read",
        &format!(r#"{{"path":"{}"}}"#, f.home().join("note.txt").display()),
    ));
    assert!(why.contains("`[boundary] grants_ro"), "{why}");
    let why = refused(&h.call(
        "glob",
        &format!(r#"{{"pattern":"**/*","path":"{}"}}"#, f.home().display()),
    ));
    assert!(why.contains("`[boundary] grants_ro"), "{why}");
    // Inside the view, and inside a grant, nothing is refused: a file under the read-only
    // toolchain grant reads like any other file the session may see.
    let r = h.call("read", r#"{"path":"main.rs"}"#);
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.render());
    std::fs::write(f.home().join(".cargo/registry/a.txt"), "toolchain\n").expect("a file");
    let r = h.call(
        "read",
        &format!(
            r#"{{"path":"{}"}}"#,
            f.home().join(".cargo/registry/a.txt").display()
        ),
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.render());
    let why = refused(&h.call(
        "write",
        &format!(
            r#"{{"path":"{}","content":"x"}}"#,
            f.home().join(".cargo/registry/x").display()
        ),
    ));
    assert!(why.contains("`[boundary] grants_rw"), "{why}");
    let _ = std::fs::remove_dir_all(f.root());
}
