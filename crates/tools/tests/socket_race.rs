//! A boundary is built while the sockets in its runtime directory come and go, and none is
//! refused. A mask per socket is a start that depends on the socket still being there when bwrap
//! mounts it — a daemon's short-lived peer socket is not — so the fence is one mount per directory
//! and this churns exactly what a busy box churns.

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

use letibot_tools::exec::confine::{
    Bwrap, ConfinePlan, Confinement, Egress, HomeView, TmpView, ViewSpec,
};

/// The apparatus a wide seat needs: bubblewrap and an unprivileged user namespace. A box without
/// them skips loudly through `apparatus`, which `LETIBOT_REQUIRE_APPARATUS` turns into a failure.
const BOUNDARY: &str = "a usable bubblewrap boundary (bwrap and an unprivileged user namespace)";

/// Sockets churning at once: enough that a per-socket mask is mounted while several of them are
/// being unlinked, which is the window this test exists to measure.
const CHURN: u64 = 24;

/// Boundaries built while they churn. Each is a real bwrap start and read-back, not a dry run.
const ROUNDS: u64 = 12;

/// One seat's fixture: a project, a home to carry (so the view is the wide one), the session's
/// scratch, and a runtime directory of its own.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let root = std::env::temp_dir().join(format!("letibot-sockrace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let f = Fixture { root };
        std::fs::create_dir_all(f.project()).expect("a project");
        std::fs::create_dir_all(f.home()).expect("a home");
        std::fs::create_dir_all(f.root.join("tmp")).expect("a scratch");
        std::fs::create_dir_all(f.run()).expect("a runtime directory");
        f
    }

    fn project(&self) -> PathBuf {
        self.root.join("project")
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn run(&self) -> PathBuf {
        self.root.join("run/letibot")
    }

    /// The view `HostBackend::confined_over` builds for a wide seat: the machine at its own paths,
    /// the runtime directory covered, and the one socket this command was given kept.
    fn view(&self, given: &Path) -> ViewSpec {
        ViewSpec::project_only(self.project())
            .with_home(HomeView::Dir(self.home()))
            .with_tmp(TmpView::Session(self.root.join("tmp")))
            .wide()
            .covering(self.run(), vec![given.to_path_buf()])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Sockets being made and removed as a daemon makes and removes them: bound as real sockets, then
/// unlinked by their owner. `passes` is how a caller waits for churn without a clock.
struct Churn {
    stop: Arc<AtomicBool>,
    passes: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl Churn {
    fn over(dir: &Path) -> Churn {
        let stop = Arc::new(AtomicBool::new(false));
        let passes = Arc::new(AtomicU64::new(0));
        let (s, p, d) = (stop.clone(), passes.clone(), dir.to_path_buf());
        let thread = std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                for i in 0..CHURN {
                    let path = d.join(format!("churn-{i}.sock"));
                    // A real socket, and its owner removes it: the shape of the socket that took
                    // a session's boundary down.
                    if let Ok(listener) = UnixListener::bind(&path) {
                        drop(listener);
                        let _ = std::fs::remove_file(&path);
                    }
                }
                p.fetch_add(1, Ordering::Relaxed);
                std::thread::yield_now();
            }
        });
        Churn {
            stop,
            passes,
            thread: Some(thread),
        }
    }

    fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The boundary, or the loud skip: a box that cannot nest a namespace here measures nothing and
/// says so rather than reporting a pass.
fn probe(f: &Fixture, given: &Path) -> Option<Bwrap> {
    match Bwrap::probe(f.view(given), Egress::Denied) {
        Ok(b) => Some(b),
        Err(e) => {
            letibot_tokencore::apparatus::applies(BOUNDARY, cfg!(target_os = "linux"))?;
            eprintln!("the boundary could not be measured on this box: {e}");
            letibot_tokencore::apparatus::absent(BOUNDARY);
            None
        }
    }
}

/// One command inside the boundary, as its own argv and not through a shell of the harness's.
fn run(b: &Bwrap, f: &Fixture, command: &str) -> (String, i32) {
    let argv = b
        .wrap(&ConfinePlan {
            cwd: &f.project(),
            env: &[],
        })
        .expect("a command line");
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .arg("/bin/sh")
        .arg("-c")
        .arg(command)
        .output()
        .expect("the helper runs");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (said, out.status.code().unwrap_or(-1))
}

#[test]
fn a_boundary_is_built_while_its_runtime_directorys_sockets_come_and_go() {
    let f = Fixture::new();
    // The socket this command was given — `sudo -A` reaches the session through it — and one that
    // belongs to another session's daemon and must stay unreachable.
    let given = f.run().join("abc-peer.sock");
    let heads = f.run().join("other-daemon.sock");
    let (given_up, heads_up) = (
        UnixListener::bind(&given).expect("a socket to be given"),
        UnixListener::bind(&heads).expect("another daemon's socket"),
    );
    let Some(b) = probe(&f, &given) else {
        return;
    };
    let churn = Churn::over(&f.run());
    let mut done = 0;
    while done < ROUNDS {
        // Wait for churn this round has not seen yet, then build a boundary over it: a condition
        // with the crate's `PATIENCE` behind it, never a sleep.
        let want = done + 1;
        letibot_tools::testing::wait_until("the runtime directory to churn", || {
            churn.passes.load(Ordering::Relaxed) >= want
        });
        if let Err(e) = Bwrap::probe(f.view(&given), Egress::Denied) {
            panic!(
                "a boundary was refused while sockets came and went in {}: {e}",
                f.run().display()
            );
        }
        done += 1;
    }
    churn.finish();

    // Nothing was unmasked by the cover: the directory is there and empty but for the door, and
    // the socket the command was given still connects.
    let (out, rc) = run(
        &b,
        &f,
        &format!(
            "test -S {given} && echo door-open; \
             test -e {heads} || echo other-hidden; \
             test -e {dir}/churn-0.sock || echo churn-hidden",
            given = given.display(),
            heads = heads.display(),
            dir = f.run().display(),
        ),
    );
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("door-open"), "the given socket is gone: {out}");
    assert!(
        out.contains("other-hidden"),
        "another daemon's socket is reachable: {out}"
    );
    assert!(
        out.contains("churn-hidden"),
        "a churn socket was left visible: {out}"
    );
    drop(given_up);
    drop(heads_up);
}
