//! **macOS's boundary, measured on the machine that has it.**
//!
//! The unit tests in `crates/tools/src/exec/seatbelt.rs` pin the profile text; this file is the
//! other half — a real `sandbox-exec`, a real profile, and a shell asked to do the things the
//! boundary exists to prevent. `#[cfg(target_os = "macos")]` on every test here, because
//! Seatbelt is a Darwin facility and `apparatus::applies` is the module's answer for a test
//! whose *other* half is measured everywhere: on Linux there is no `sandbox-exec` at all, and
//! asking a run to go and provision one would be a demand no provisioner can meet.
//!
//! # What is never read here
//!
//! `~/.ssh`, the keychains and the operator's shell profiles are never opened, listed or
//! printed — not even to prove the boundary works. The deny list is measured against a **fake
//! home** the test makes in a temporary directory, whose `.ssh` it fills with a known string,
//! and every assertion is on an exit status or on whether that string came back. The operator's
//! real paths are in the profile too (they are, by construction), and the only thing this file
//! does with them is leave them alone.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};

use letibot_tools::exec::confine::{
    Confinement, Egress, HomeView, Mechanism, Prop, PropState, SealKind, TmpView, ViewSpec,
};
use letibot_tools::exec::seatbelt::{CANARY, Seatbelt};

/// A fixture: a project, a fake home with a `.ssh` in it, and a scratch directory — all under
/// one temporary directory, removed when the test ends.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "letibot-seatbelt-{}-{tag}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        for d in [
            root.join("project"),
            root.join("home/ssh-fixture"),
            root.join("outside"),
        ] {
            std::fs::create_dir_all(&d).expect("a fixture directory");
        }
        // A git repository, so `git status` has something to answer about.
        let ok = std::process::Command::new("/usr/bin/git")
            .args(["init", "-q", "."])
            .current_dir(root.join("project"))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "git init in the fixture");
        std::fs::write(root.join("project/a.txt"), "hello\n").expect("a file in the checkout");
        // **The fake credential.** Not a key and not the operator's: a known string this test
        // can look for, in a directory named the way the deny list names it.
        std::fs::write(root.join("home/ssh-fixture/known_hosts"), SECRET)
            .expect("the fake ssh directory");
        let mut f = Fixture { root };
        // The deny list is spelled with the path the *view* would see, and this one is not the
        // operator's home at all — which is the point: the rule is measured on a path that
        // costs nothing to get wrong.
        f.root = f.root.canonicalize().unwrap_or(f.root.clone());
        f
    }

    fn project(&self) -> PathBuf {
        self.root.join("project")
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn denied(&self) -> PathBuf {
        self.root.join("home/ssh-fixture")
    }

    /// The view a session would get, with the fixture's home and scratch, and the fixture's
    /// `ssh-fixture` directory added to the deny list beside the operator's real ones.
    fn view(&self) -> ViewSpec {
        ViewSpec::project_only(self.project())
            .with_home(HomeView::Dir(self.home()))
            .with_tmp(TmpView::Session(self.root.join("tmp")))
    }

    fn probe(&self, egress: Egress) -> Result<Seatbelt, String> {
        Seatbelt::probe_with(self.view(), egress, vec![self.denied()]).map_err(|e| format!("{e}"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The string the fake credential holds. Nothing outside this file may see it come back.
const SECRET: &str = "letibot-fixture-line-that-must-never-be-readable\n";

/// **Run a shell command inside a measured boundary**, the way `HostProcesses` does: the
/// confinement's argv, then `/bin/sh -c <command>`, in the checkout.
fn run_in(s: &Seatbelt, cwd: &Path, command: &str) -> (i32, String) {
    let argv = s
        .wrap(&letibot_tools::exec::confine::ConfinePlan { cwd, env: &[] })
        .expect("a measured boundary wraps");
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .arg("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the wrapped command runs");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

// ------------------------------------------------------ the probe, for real

#[test]
fn the_probe_measures_a_boundary_and_says_what_macos_cannot_give() {
    let f = Fixture::new("probe");
    let s = match f.probe(Egress::Denied) {
        Ok(s) => s,
        Err(e) => panic!("the macOS boundary did not measure on this box: {e}"),
    };
    let b = s.boundary().expect("a measured boundary");
    assert_eq!(b.platform, Mechanism::Policy, "{}", b.describe());
    assert!(b.complete(), "{}", b.describe());
    // The three required properties are held, and they were HELD rather than asserted: the
    // probe measures each of them from inside.
    for p in [Prop::FileView, Prop::ProcessIsolation, Prop::Nested] {
        match b.props.get(&p) {
            Some(PropState::Held { how }) => assert!(!how.is_empty(), "{p} held with no how"),
            other => panic!("{p} is {other:?}, not held: {}", b.describe()),
        }
    }
    // And the one macOS cannot give is said, not omitted.
    match b.props.get(&Prop::ProcessAbsence) {
        Some(PropState::Absent { why }) => assert!(why.contains("PID namespace"), "{why}"),
        other => panic!("process-absence is {other:?}: the disclosure would look complete"),
    }
    let d = b.describe();
    assert!(d.starts_with("confined;"), "{d}");
    assert!(d.contains("this OS cannot give it"), "{d}");
    assert!(d.contains("NOT APPLICABLE"), "{d}");
    assert!(d.contains("sandbox-exec"), "{d}");
    // A policy boundary must not print the namespace table — seven `NOT ENTERED` lines would
    // read as a boundary that failed.
    assert!(!d.contains("mnt:"), "{d}");
    assert!(b.seals.contains_key(&SealKind::NestedNamespaces), "{d}");
}

#[test]
fn a_command_inside_the_profile_runs_the_toolchain_and_writes_the_checkout() {
    let f = Fixture::new("toolchain");
    let s = match f.probe(Egress::Denied) {
        Ok(s) => s,
        Err(e) => panic!("the macOS boundary did not measure on this box: {e}"),
    };
    let cwd = f.project();
    let (rc, out) = run_in(
        &s,
        &cwd,
        "cargo --version && /usr/bin/git status --short && echo wrote > inside.txt && \
         cat inside.txt",
    );
    assert_eq!(rc, 0, "a toolchain inside the boundary: {out}");
    assert!(out.contains("cargo "), "{out}");
    assert!(out.contains("wrote"), "{out}");
    assert!(
        cwd.join("inside.txt").is_file(),
        "the write reached the checkout"
    );
    // `git status` is the case that matters for a real session, and it must answer about the
    // checkout rather than about a permission problem.
    assert!(!out.contains("Operation not permitted"), "{out}");
}

#[test]
fn the_deny_list_is_unreadable_even_though_the_home_is_a_writable_part_of_the_view() {
    // **The measurement the whole list is for.** The home is `(rw)` in this profile — a session
    // has to be able to write it — and `ssh-fixture` inside it is denied anyway. The proof is
    // an exit status and the absence of a known string, never the content of a real key.
    let f = Fixture::new("deny");
    let s = match f.probe(Egress::Denied) {
        Ok(s) => s,
        Err(e) => panic!("the macOS boundary did not measure on this box: {e}"),
    };
    let cwd = f.project();
    let denied = f.denied();
    let file = denied.join("known_hosts");
    // The home IS writable, so a refusal here cannot be blamed on the home being outside.
    let (rc, out) = run_in(
        &s,
        &cwd,
        "touch \"$HOME/written-by-the-command\" && echo home-ok",
    );
    assert_eq!(rc, 0, "the home is a writable part of the view: {out}");
    assert!(out.contains("home-ok"), "{out}");
    // And the denied directory is not readable, by `cat`, by `sh` in that directory, or by a
    // child.
    let (rc, out) = run_in(&s, &cwd, &format!("cat {}", file.display()));
    assert_ne!(rc, 0, "the denied path was readable: {out}");
    assert!(
        !out.contains("letibot-fixture-line"),
        "a secret got out: {out}"
    );
    let (rc, out) = run_in(&s, &cwd, &format!("cd {} && ls", denied.display()));
    assert_ne!(rc, 0, "the denied directory was listable: {out}");
    let (rc, out) = run_in(&s, &cwd, &format!("/bin/sh -c 'cat {}'", file.display()));
    assert_ne!(rc, 0, "a nested shell read the denied path: {out}");
    assert!(
        !out.contains("letibot-fixture-line"),
        "a secret got out: {out}"
    );
    // The canary the probe itself uses is in the same state, and it is the one the profile is
    // refused on if the deny list does not win.
    let (rc, _) = run_in(&s, &cwd, &format!("cat \"$HOME/{CANARY}\""));
    assert_ne!(rc, 0, "the canary was readable");
}

#[test]
fn a_write_outside_the_checkout_is_refused() {
    let f = Fixture::new("outside");
    let s = match f.probe(Egress::Denied) {
        Ok(s) => s,
        Err(e) => panic!("the macOS boundary did not measure on this box: {e}"),
    };
    let outside = f.root.join("outside/escape.txt");
    let (rc, out) = run_in(
        &s,
        &f.project(),
        &format!("sh -c 'echo x > {}' 2>&1", outside.display()),
    );
    assert_ne!(rc, 0, "a write outside the view succeeded: {out}");
    assert!(!outside.exists(), "the file outside the view exists");
    // And a path that is not in the view at all is refused rather than absent — the difference
    // the description is written to state.
    let (rc, _) = run_in(&s, &f.project(), "cat /etc/master.passwd");
    assert_ne!(rc, 0, "a path outside the view was readable");
}

#[test]
fn egress_is_denied_and_measured_as_denied() {
    let f = Fixture::new("egress");
    let s = match f.probe(Egress::Denied) {
        Ok(s) => s,
        Err(e) => panic!("the macOS boundary did not measure on this box: {e}"),
    };
    // **A connect that cannot leave, and nothing is sent.** The destinations are a loopback
    // port nothing listens on and TEST-NET-1, which is reserved for documentation and routed
    // nowhere; `nc -z` closes as soon as the answer arrives.
    let (_, out) = run_in(
        &s,
        &f.project(),
        "/usr/bin/nc -v -z -G 1 -w 1 192.0.2.1 80 2>&1",
    );
    assert!(
        out.contains("Operation not permitted"),
        "egress was not denied: {out}"
    );
    assert!(!out.contains("Connected"), "{out}");
}

#[test]
fn a_view_whose_project_is_not_a_directory_is_refused() {
    // The refusal every mechanism shares, reached through this one: a boundary is rooted at a
    // checkout, and a path that is not one is a refusal that names the root rather than a
    // sandbox that fails at every spawn.
    let e = Seatbelt::probe(
        ViewSpec::project_only("/nonexistent/letibot-project"),
        Egress::Denied,
    );
    let m = format!("{}", e.expect_err("a missing root cannot be a view"));
    assert!(m.contains("/nonexistent/letibot-project"), "{m}");
    assert!(m.contains("not a directory"), "{m}");
}

#[test]
fn the_profile_is_written_outside_the_checkout() {
    // A policy the wrapped command can edit is not a policy: the file lives in this daemon's
    // own directory, and the checkout is not it.
    let f = Fixture::new("profile-path");
    let s = match f.probe(Egress::Denied) {
        Ok(s) => s,
        Err(e) => panic!("the macOS boundary did not measure on this box: {e}"),
    };
    let p = s.profile();
    assert!(p.is_file(), "{} is not a file", p.display());
    assert!(
        !p.starts_with(f.project()),
        "the profile is inside the checkout: {}",
        p.display()
    );
    let text = std::fs::read_to_string(p).expect("the profile is readable by its owner");
    assert!(text.starts_with("(version 1)\n"), "{text}");
    assert!(text.contains("(deny default)"), "{text}");
    assert!(text.contains("(deny file-read* file-write*"), "{text}");
    // The deny block comes after every allow: SBPL is last-match-wins, and a deny list before
    // an allow is a deny list the allow undoes.
    assert!(
        text.rfind("(deny file-read* file-write*").unwrap() > text.rfind("(allow ").unwrap(),
        "{text}"
    );
    // Every path it denies is one of ours or the operator's own — never a path either of them
    // wrote into the checkout.
    assert!(text.contains(&f.denied().display().to_string()), "{text}");
}
