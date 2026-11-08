//! macOS's boundary, measured on the machine that has it. Unit tests pin the profile text; this
//! asks a real `sandbox-exec` to prevent things, against a fake home whose `.ssh` holds a known
//! string. The operator's `~/.ssh` is never opened. Linux has no `sandbox-exec`.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};

use letibot_tools::exec::confine::{
    Confinement, Egress, HomeView, Mechanism, Prop, PropState, SealKind, TmpView, ViewSpec,
};
use letibot_tools::exec::seatbelt::{CANARY, Seatbelt};

/// A fixture: a project, a fake home with a `.ssh` in it, and a scratch directory, all under
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
        // The fake credential: not a key and not the operator's, but a known string this test
        // can look for, in a directory named the way the deny list names it.
        std::fs::write(root.join("home/ssh-fixture/known_hosts"), SECRET)
            .expect("the fake ssh directory");
        let mut f = Fixture { root };
        // The deny list is spelled with the path the view sees, and this home is not the
        // operator's: the rule is measured on a path that costs nothing to get wrong.
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

    /// The view a session would get, with the fixture's home and scratch and its `ssh-fixture`
    /// added to the deny list beside the real ones.
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

/// Run a shell command inside a measured boundary the way `HostProcesses` does: the
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
    // The three required properties are held, and held by measurement: the probe checks each
    // from inside the boundary.
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
    // A policy boundary must not print the namespace table: seven `NOT ENTERED` lines would read
    // as a boundary that failed.
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
    // The measurement the whole list is for: the home is writable in this profile, as a session
    // needs, and `ssh-fixture` inside it is denied anyway. The proof is an exit status and the
    // absence of a known string, never the content of a real key.
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
    // The canary the probe itself uses is in the same state, and the profile is refused when
    // the deny list does not win.
    let (rc, _) = run_in(&s, &cwd, &format!("cat \"$HOME/{CANARY}\""));
    assert_ne!(rc, 0, "the canary was readable");
}

/// `[boundary] hide` reaches the profile: a path in the view's `masked` list is denied inside a
/// writable home, and a sibling is not.
#[test]
fn a_hidden_path_in_the_view_is_unreadable_and_its_sibling_is_not() {
    let f = Fixture::new("hide");
    let hidden = f.home().join("hidden-by-config");
    let sibling = f.home().join("sibling");
    for d in [&hidden, &sibling] {
        std::fs::create_dir_all(d).expect("a fixture directory");
        std::fs::write(d.join("f"), SECRET).expect("a fixture file");
    }
    let view = f.view().masking([hidden.clone()]);
    let s = match Seatbelt::probe_with(view, Egress::Denied, vec![f.denied()]) {
        Ok(s) => s,
        Err(e) => panic!("the macOS boundary did not measure on this box: {e}"),
    };
    let (rc, out) = run_in(&s, &f.project(), &format!("cat {}/f", hidden.display()));
    assert_ne!(rc, 0, "the hidden path was readable: {out}");
    assert!(
        !out.contains("letibot-fixture-line"),
        "a secret got out: {out}"
    );
    let (rc, out) = run_in(&s, &f.project(), &format!("cat {}/f", sibling.display()));
    assert_eq!(
        rc, 0,
        "a sibling of the hidden path is still in the view: {out}"
    );
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
    // A path outside the view is refused rather than absent, which is the difference the
    // description exists to state.
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
    // A connect that cannot leave, and nothing is sent: TEST-NET-1 is reserved for
    // documentation and routed nowhere, and `nc -z` closes as soon as the answer arrives.
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
    // Every path it denies is one of ours or the operator's own, never a path either of them
    // wrote into the checkout.
    assert!(text.contains(&f.denied().display().to_string()), "{text}");
}

#[test]
fn a_session_tmp_spelled_through_a_symlink_is_writable() {
    // `std::env::temp_dir()` is `/var/folders/..`, a symlink into `/private/var`, and the
    // kernel matches the resolved path. A session tmp is handed over in the spelled form.
    let f = Fixture::new("symtmp");
    let spelled = std::env::temp_dir().join(format!("letibot-symtmp-{}", std::process::id()));
    let view = ViewSpec::project_only(f.project())
        .with_home(HomeView::Dir(f.home()))
        .with_tmp(TmpView::Session(spelled.clone()));
    let s = Seatbelt::probe_with(view, Egress::Denied, vec![f.denied()])
        .unwrap_or_else(|e| panic!("probe: {e}"));
    let (rc, out) = run_in(
        &s,
        &f.project(),
        "mkdir \"$TMPDIR/d\" && echo x > \"$TMPDIR/d/f\" && cat \"$TMPDIR/d/f\"",
    );
    let _ = std::fs::remove_dir_all(&spelled);
    assert_eq!(rc, 0, "the session tmp is writable: {out}");
}

#[test]
fn a_command_inside_the_profile_cannot_connect_to_a_hidden_unix_socket() {
    // A listener stands in for the coordinator; the profile must refuse the connect even with
    // egress on. It lives in /tmp because a macOS socket path is capped at 104 bytes.
    let f = Fixture::new("hidden-socket");
    let sock = std::path::PathBuf::from(format!("/tmp/lb-hs-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock);
    let _listener = std::os::unix::net::UnixListener::bind(&sock).expect("a stand-in");
    let view = f.view().hiding(&sock);
    let s = Seatbelt::probe_with(
        view,
        Egress::Host {
            why: "the test needs the network rule to be the only thing in the way".into(),
        },
        Vec::new(),
    )
    .expect("the macOS boundary measures");
    let (_, out) = run_in(
        &s,
        &f.project(),
        &format!(
            "/usr/bin/python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); \
             s.connect(sys.argv[1]); print(\"CONNECTED\")' {} 2>&1",
            sock.display()
        ),
    );
    let _ = std::fs::remove_file(&sock);
    assert!(!out.contains("CONNECTED"), "{out}");
}

#[test]
fn a_command_inside_the_profile_can_stop_its_own_child() {
    // A test runner's timeout kills the child it started; a profile that lets a process signal
    // only itself turns every such deadline into the child's whole lifetime.
    let f = Fixture::new("own-child");
    let s = match f.probe(Egress::Denied) {
        Ok(s) => s,
        Err(e) => panic!("the macOS boundary did not measure on this box: {e}"),
    };
    let (rc, out) = run_in(
        &s,
        &f.project(),
        "sleep 30 & c=$!; if kill -9 $c 2>&1; then echo KILLED; else echo REFUSED; fi; wait $c",
    );
    assert!(out.contains("KILLED"), "rc={rc}: {out}");
}

/// Resolve a name and connect to a loopback listener from inside; prints `DNS=ok|fail` and
/// `TCP=ok|fail`. No packet leaves the machine.
fn net_probe(s: &Seatbelt, f: &Fixture, port: u16) -> String {
    let script = format!(
        "python3 -c 'import socket\n\
try:\n socket.gethostbyname(\"localhost\"); print(\"DNS=ok\")\n\
except Exception as e:\n print(\"DNS=fail\", e)\n\
try:\n socket.create_connection((\"127.0.0.1\", {port}), 2).close(); print(\"TCP=ok\")\n\
except Exception as e:\n print(\"TCP=fail\", e)'"
    );
    run_in(s, &f.project(), &script).1
}

#[test]
fn a_host_egress_profile_resolves_names_and_connects_and_a_denied_one_does_neither() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let port = l.local_addr().expect("addr").port();
    let f = Fixture::new("host-net");
    let host = f
        .probe(Egress::Host { why: "test".into() })
        .unwrap_or_else(|e| panic!("the macOS boundary did not measure on this box: {e}"));
    let out = net_probe(&host, &f, port);
    assert!(out.contains("DNS=ok"), "{out}");
    assert!(out.contains("TCP=ok"), "{out}");
    let denied = f
        .probe(Egress::Denied)
        .unwrap_or_else(|e| panic!("the macOS boundary did not measure on this box: {e}"));
    let out = net_probe(&denied, &f, port);
    assert!(out.contains("TCP=fail"), "{out}");
}

#[test]
fn git_inside_the_profile_reads_the_operators_gitconfig_read_only() {
    let f = Fixture::new("gitconfig");
    let op = f.root.join("operator");
    std::fs::create_dir_all(&op).expect("a fake operator home");
    std::fs::write(
        op.join(".gitconfig"),
        "[user]\n\temail = fixture@example.test\n",
    )
    .expect("a fixture gitconfig");
    let s = Seatbelt::probe_as(f.view(), Egress::Denied, vec![f.denied()], op.clone())
        .unwrap_or_else(|e| panic!("the macOS boundary did not measure on this box: {e}"));
    let (_, out) = run_in(&s, &f.project(), "git config user.email 2>/dev/null");
    assert_eq!(out.trim(), "fixture@example.test", "{out}");
    let (rc, out) = run_in(
        &s,
        &f.project(),
        &format!("echo x >> '{}/.gitconfig'", op.display()),
    );
    assert_ne!(rc, 0, "the grant is read-only: {out}");
}

#[test]
fn the_xdg_git_config_is_the_identity_when_there_is_no_dotfile() {
    let f = Fixture::new("xdg-gitconfig");
    let op = f.root.join("operator");
    std::fs::create_dir_all(op.join(".config/git")).expect("a fake operator home");
    std::fs::write(
        op.join(".config/git/config"),
        "[user]\n\temail = xdg@example.test\n",
    )
    .expect("a fixture gitconfig");
    let s = Seatbelt::probe_as(f.view(), Egress::Denied, vec![f.denied()], op)
        .unwrap_or_else(|e| panic!("the macOS boundary did not measure on this box: {e}"));
    let (_, out) = run_in(&s, &f.project(), "git config user.email 2>/dev/null");
    assert_eq!(out.trim(), "xdg@example.test", "{out}");
}

// ------------------------------------------------------ the two levels

/// The wide seat over a fake operator home: `$HOME` is that directory, with a fixture `.ssh` and
/// Chrome profile in it, and nothing real is opened.
fn wide(f: &Fixture) -> (Seatbelt, PathBuf) {
    let op = f.root.join("operator");
    std::fs::create_dir_all(op.join(".ssh")).expect("fake .ssh");
    std::fs::create_dir_all(op.join("Library/Application Support/Google/Chrome/Default"))
        .expect("fake chrome");
    std::fs::write(op.join(".ssh/id_rsa"), SECRET).expect("fake key");
    std::fs::write(
        op.join("Library/Application Support/Google/Chrome/Default/Cookies"),
        SECRET,
    )
    .expect("fake cookies");
    std::fs::write(op.join("note.txt"), "plain\n").expect("a note");
    let view = ViewSpec::project_only(f.project())
        .with_home(HomeView::Dir(op.clone()))
        .with_tmp(TmpView::Session(f.root.join("tmp")));
    let s = Seatbelt::probe_as(view, Egress::Denied, vec![], op.clone())
        .unwrap_or_else(|e| panic!("the macOS boundary did not measure on this box: {e}"));
    (s, op)
}

#[test]
fn a_wide_seat_sees_and_writes_the_real_home_and_the_deny_list_is_the_fence() {
    let f = Fixture::new("wide");
    let (s, op) = wide(&f);
    let cwd = f.project();
    let (rc, out) = run_in(
        &s,
        &cwd,
        "echo \"home=$HOME\" && cat \"$HOME/note.txt\" && cat /etc/hosts >/dev/null && \
         echo new > \"$HOME/written.txt\"",
    );
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains(&format!("home={}", op.display())), "{out}");
    assert!(out.contains("plain"), "{out}");
    assert!(
        op.join("written.txt").is_file(),
        "the real home is writable"
    );
    assert!(
        !op.join(CANARY).exists(),
        "the probe left no marker in the home"
    );
    let (rc, out) = run_in(&s, &cwd, "cat \"$HOME/.ssh/id_rsa\"");
    assert_ne!(rc, 0, "{out}");
    assert!(!out.contains("letibot-fixture-line"), "{out}");
    let (rc, out) = run_in(
        &s,
        &cwd,
        "cat \"$HOME/Library/Application Support/Google/Chrome/Default/Cookies\"",
    );
    assert_ne!(rc, 0, "{out}");
    assert!(!out.contains("letibot-fixture-line"), "{out}");
    let outside = f.root.join("outside/x");
    let (rc, _) = run_in(&s, &cwd, &format!("echo x > {}", outside.display()));
    assert_ne!(rc, 0, "a write outside home and checkout");
}

fn real_home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME"))
}

fn gh_logged_in_outside() -> bool {
    std::process::Command::new("/opt/homebrew/bin/gh")
        .args(["auth", "status"])
        .stdin(std::process::Stdio::null())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `run_in` with extra environment, passed the way a seat's env reaches the wrapped command.
fn run_env(s: &Seatbelt, cwd: &Path, env: &[(String, String)], command: &str) -> (i32, String) {
    let argv = s
        .wrap(&letibot_tools::exec::confine::ConfinePlan { cwd, env })
        .expect("a measured boundary wraps");
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .arg("/bin/sh")
        .arg("-c")
        .arg(command)
        .envs(env.iter().cloned())
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

const FIXTURE_TOKEN: &str = "gho_fixtureFIXTUREfixture0000000000000000";

#[test]
fn a_wide_seat_with_the_injected_token_authenticates_gh_and_git_for_github_only() {
    let f = Fixture::new("wide-token");
    let (s, _) = wide(&f);
    let env = letibot_tools::exec::seat::github_env(FIXTURE_TOKEN).0;
    // gh answers from `GH_TOKEN`, offline.
    let (rc, out) = run_env(
        &s,
        &f.project(),
        &env,
        "/opt/homebrew/bin/gh auth token 2>&1",
    );
    assert_eq!(rc, 0, "{out}");
    assert_eq!(out.trim(), FIXTURE_TOKEN);
    // git's credential fill for github.com gets the token, and for another host gets nothing.
    let fill = |host: &str| {
        run_env(
            &s,
            &f.project(),
            &env,
            &format!(
                "printf 'protocol=https\\nhost={host}\\n\\n' | \
                 GIT_TERMINAL_PROMPT=0 git credential fill 2>&1"
            ),
        )
    };
    let (rc, out) = fill("github.com");
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("username=x-access-token"), "{out}");
    assert!(out.contains(&format!("password={FIXTURE_TOKEN}")), "{out}");
    let (rc, out) = fill("evil.example");
    assert_ne!(rc, 0, "{out}");
    assert!(!out.contains(FIXTURE_TOKEN), "{out}");
}

#[test]
fn gh_is_not_logged_in_in_a_tight_seat() {
    let Some(_) = letibot_tokencore::apparatus::present(
        "a logged-in gh outside the seat (the credential the seat must not see)",
        gh_logged_in_outside(),
    ) else {
        return;
    };
    let f = Fixture::new("tight-gh");
    let s = Seatbelt::probe_as(f.view(), Egress::Denied, vec![f.denied()], real_home())
        .unwrap_or_else(|e| panic!("the macOS boundary did not measure on this box: {e}"));
    let (rc, out) = run_in(&s, &f.project(), "/opt/homebrew/bin/gh auth status 2>&1");
    assert_ne!(rc, 0, "a child is logged in to GitHub: {out}");
    let (rc, _) = run_in(&s, &f.project(), "cat \"$HOME/.config/gh/hosts.yml\"");
    assert_ne!(rc, 0);
}

/// The keychain is one mach service, and gh keeps its token in it: a child denied that service
/// cannot ask for the token its parent pushes with, whatever it spells. The two seats are measured
/// on the same call, so what the difference proves is the profile and not the machine.
#[test]
fn a_tight_seat_cannot_ask_the_keychain_its_parent_uses() {
    let security = ["/usr/bin/security"]
        .into_iter()
        .find(|p| Path::new(p).is_file());
    let Some(security) = letibot_tokencore::apparatus::applies(
        "the `security` tool, which is how a keychain item is asked for",
        security.is_some(),
    )
    .map(|_| security.unwrap_or("/usr/bin/security")) else {
        return;
    };
    // An item that is not there: the wide seat's answer is the keychain's own "not found", and the
    // tight seat's is a refusal before the keychain is reached. Nothing of the operator's is read.
    let ask = format!("{security} find-generic-password -s letibot-boundary-probe -w 2>&1");

    let f = Fixture::new("keychain");
    let tight = Seatbelt::probe_as(f.view(), Egress::Denied, vec![f.denied()], real_home())
        .unwrap_or_else(|e| panic!("the macOS boundary did not measure on this box: {e}"));
    let (tight_rc, tight_out) = run_in(&tight, &f.project(), &ask);
    assert_ne!(tight_rc, 0, "a child asked the keychain: {tight_out}");
    // Without SecurityServer the search cannot be built (errSecParam): the keychain was never
    // reached, which is the refusal on this OS, as opposed to the wide seat's plain not-found.
    let refused = [
        "not permitted",
        "denied",
        "Sandbox",
        "sandbox",
        "were not valid",
    ]
    .iter()
    .any(|w| tight_out.contains(w));
    assert!(
        refused,
        "a child's refusal has to be the boundary's, not the keychain's own answer about a \
         missing item: {tight_out}"
    );

    // And the top-level seat does reach it, so the refusal above is the profile and not a machine
    // with no keychain at all.
    let (wide_seat, op) = wide(&f);
    let (wide_rc, wide_out) = run_in(&wide_seat, &f.project(), &ask);
    assert!(
        (wide_rc != tight_rc || tight_out.contains("were not valid"))
            && !wide_out.contains("were not valid")
            && !wide_out.contains("not permitted"),
        "the top-level seat asks the keychain, so the two answers differ: tight {tight_rc} \
         {tight_out}; wide {wide_rc} {wide_out} (home {})",
        op.display()
    );
}

#[test]
fn a_child_commits_in_a_worktree_with_the_operators_identity_and_has_no_credentials() {
    let f = Fixture::new("child");
    let git = |dir: &Path, args: &[&str]| {
        let o = std::process::Command::new("/usr/bin/git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "f")
            .env("GIT_AUTHOR_EMAIL", "f@x.test")
            .env("GIT_COMMITTER_NAME", "f")
            .env("GIT_COMMITTER_EMAIL", "f@x.test")
            .output()
            .expect("git");
        assert!(o.status.success(), "{args:?}: {o:?}");
    };
    let repo = f.root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "."]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "root"]);
    let wt = f.root.join("wt");
    git(&repo, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    let op = f.root.join("operator");
    std::fs::create_dir_all(&op).unwrap();
    std::fs::write(
        op.join(".gitconfig"),
        "[user]\n\tname = Fixture Op\n\temail = op@example.test\n",
    )
    .unwrap();
    let mut view = ViewSpec::project_only(&wt)
        .with_home(HomeView::Dir(f.home()))
        .with_tmp(TmpView::Session(f.root.join("tmp")));
    for g in letibot_tools::exec::seat::worktree_git_grants(&wt) {
        for p in letibot_tools::exec::seat::git_dir_protected(&wt) {
            view = view.protecting(p);
        }
        view = view.granting(g);
    }
    let s = Seatbelt::probe_as(view, Egress::Denied, vec![f.denied()], op)
        .unwrap_or_else(|e| panic!("the macOS boundary did not measure on this box: {e}"));
    let (rc, out) = run_in(
        &s,
        &wt,
        "git commit -q --allow-empty -m child && git log -1 --format=%an,%ae",
    );
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("Fixture Op,op@example.test"), "{out}");
    // Hooks and config run later outside the boundary, so a child cannot write them.
    let common = repo.join(".git").canonicalize().unwrap();
    let (rc, out) = run_in(
        &s,
        &wt,
        &format!("echo '#!/bin/sh' > {}/hooks/pre-commit", common.display()),
    );
    assert_ne!(rc, 0, "a child planted a hook: {out}");
    let (rc, out) = run_in(
        &s,
        &wt,
        &format!("echo '[core]' >> {}/config", common.display()),
    );
    assert_ne!(rc, 0, "a child edited the repository config: {out}");
    // And the commit still works, with the protected paths in place.
    let (rc, out) = run_in(&s, &wt, "git commit -q --allow-empty -m again");
    assert_eq!(rc, 0, "{out}");
    // No helper answers for GitHub, and prompting is off.
    let (rc, out) = run_in(
        &s,
        &wt,
        "printf 'protocol=https\\nhost=github.com\\n\\n' | \
         GIT_TERMINAL_PROMPT=0 GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=credential.helper \
         GIT_CONFIG_VALUE_0= git credential fill 2>&1",
    );
    assert_ne!(rc, 0, "a child got a credential: {out}");
    assert!(!out.contains("password="), "{out}");
}

/// The top-level seat: persistence paths and history are denied, the global git config is read
/// and not written.
#[test]
fn a_wide_seat_cannot_persist_or_read_history_and_only_reads_the_git_config() {
    let f = Fixture::new("persist");
    let op = f.root.join("operator");
    std::fs::create_dir_all(op.join("Library/LaunchAgents")).unwrap();
    std::fs::write(op.join(".zsh_history"), SECRET).unwrap();
    std::fs::write(op.join(".gitconfig"), "[user]\n\tname = Op\n").unwrap();
    let mut view = ViewSpec::project_only(f.project())
        .with_home(HomeView::Dir(op.clone()))
        .with_tmp(TmpView::Session(f.root.join("tmp")));
    for p in letibot_tools::exec::seat::wide_protected(&op) {
        view = view.protecting(p);
    }
    let s = Seatbelt::probe_as(view, Egress::Denied, vec![], op.clone())
        .unwrap_or_else(|e| panic!("the macOS boundary did not measure on this box: {e}"));
    let cwd = f.project();
    for write in [
        "echo x > \"$HOME/Library/LaunchAgents/a.plist\"",
        "echo x > \"$HOME/.zlogin\"",
        "echo x > \"$HOME/.zprofile\"",
        "echo x >> \"$HOME/.gitconfig\"",
    ] {
        let (rc, out) = run_in(&s, &cwd, write);
        assert_ne!(rc, 0, "{write} succeeded: {out}");
    }
    let (rc, out) = run_in(&s, &cwd, "cat \"$HOME/.zsh_history\"");
    assert_ne!(rc, 0, "{out}");
    assert!(!out.contains(SECRET), "history was read: {out}");
    let (rc, out) = run_in(&s, &cwd, "cat \"$HOME/.gitconfig\"");
    assert_eq!(rc, 0, "a commit needs the identity: {out}");
    assert!(out.contains("name = Op"), "{out}");
}

/// `main` kept only in `packed-refs` has no ref file; a child must not be able to create the loose
/// one, and must still commit on its own branch.
#[test]
fn a_child_cannot_shadow_a_packed_only_branch_and_still_commits() {
    let f = Fixture::new("packed");
    let git = |dir: &Path, args: &[&str]| {
        let o = std::process::Command::new("/usr/bin/git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "f")
            .env("GIT_AUTHOR_EMAIL", "f@x.test")
            .env("GIT_COMMITTER_NAME", "f")
            .env("GIT_COMMITTER_EMAIL", "f@x.test")
            .output()
            .expect("git");
        assert!(o.status.success(), "{args:?}: {o:?}");
    };
    let repo = f.root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "root"]);
    let wt = f.root.join("wt");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/child",
            wt.to_str().unwrap(),
        ],
    );
    git(&repo, &["pack-refs", "--all", "--prune"]);
    let op = f.root.join("operator");
    std::fs::create_dir_all(&op).unwrap();
    let mut view = ViewSpec::project_only(&wt)
        .with_home(HomeView::Dir(f.home()))
        .with_tmp(TmpView::Session(f.root.join("tmp")));
    for g in letibot_tools::exec::seat::worktree_git_grants(&wt) {
        for p in letibot_tools::exec::seat::git_dir_protected(&wt) {
            view = view.protecting(p);
        }
        view = view.granting(g);
    }
    let s = Seatbelt::probe_as(view, Egress::Denied, vec![f.denied()], op)
        .unwrap_or_else(|e| panic!("the macOS boundary did not measure on this box: {e}"));
    let common = repo.join(".git").canonicalize().unwrap();
    assert!(
        !common.join("refs/heads/main").exists(),
        "main must be packed only"
    );
    let (rc, out) = run_in(
        &s,
        &wt,
        &format!(
            "git rev-parse HEAD > {c}/refs/heads/main",
            c = common.display()
        ),
    );
    assert_ne!(rc, 0, "a child shadowed main with a loose ref: {out}");
    assert!(!common.join("refs/heads/main").exists(), "{out}");
    let (rc, out) = run_in(
        &s,
        &wt,
        "GIT_AUTHOR_NAME=c GIT_AUTHOR_EMAIL=c@x.test GIT_COMMITTER_NAME=c \
         GIT_COMMITTER_EMAIL=c@x.test git commit -q --allow-empty -m child",
    );
    assert_eq!(rc, 0, "{out}");
}

/// Security.framework trust evaluation needs `trustd`, which the profile now allows in both seats
/// (Go's `x509: OSStatus -26276` was this). `security verify-cert` runs SecTrust on a system root,
/// and the keychain item lookup stays refused in the tight seat.
#[test]
fn certificate_trust_evaluates_in_both_seats_and_the_tight_seat_still_has_no_keychain() {
    let security = ["/usr/bin/security"]
        .into_iter()
        .find(|p| Path::new(p).is_file());
    let Some(security) = letibot_tokencore::apparatus::applies(
        "the `security` tool, which runs a SecTrust evaluation",
        security.is_some(),
    )
    .map(|_| security.unwrap_or("/usr/bin/security")) else {
        return;
    };
    let f = Fixture::new("trust");
    let cert = f.project().join("root.pem");
    // A bundled public root, exported out of the system store outside any seat.
    let out = std::process::Command::new(security)
        .args([
            "find-certificate",
            "-a",
            "-c",
            "ISRG Root X1",
            "-p",
            "/System/Library/Keychains/SystemRootCertificates.keychain",
        ])
        .output()
        .expect("security runs outside a seat");
    let Some(_) = letibot_tokencore::apparatus::applies(
        "a bundled public root named ISRG Root X1",
        out.status.success() && !out.stdout.is_empty(),
    ) else {
        return;
    };
    std::fs::write(&cert, &out.stdout).expect("the exported root");
    let verify = format!(
        "{security} verify-cert -c '{}' -p basic 2>&1; echo rc=$?",
        cert.display()
    );

    let tight = Seatbelt::probe_as(f.view(), Egress::Denied, vec![f.denied()], real_home())
        .unwrap_or_else(|e| panic!("the macOS boundary did not measure on this box: {e}"));
    let (_, t_out) = run_in(&tight, &f.project(), &verify);
    assert!(
        t_out.contains("successful") && t_out.contains("rc=0"),
        "trust evaluation in the tight seat: {t_out}"
    );
    let (wide_seat, _op) = wide(&f);
    let (_, w_out) = run_in(&wide_seat, &f.project(), &verify);
    assert!(
        w_out.contains("successful") && w_out.contains("rc=0"),
        "trust evaluation in the wide seat: {w_out}"
    );

    let ask = format!("{security} find-generic-password -s letibot-boundary-probe -w 2>&1");
    let (rc, out) = run_in(&tight, &f.project(), &ask);
    assert_ne!(rc, 0, "the tight seat reached the keychain: {out}");
    assert!(
        [
            "not permitted",
            "denied",
            "Sandbox",
            "sandbox",
            "were not valid"
        ]
        .iter()
        .any(|w| out.contains(w)),
        "the refusal has to be the boundary's: {out}"
    );
}

/// A fake letibot store under `op`: data and secrets side by side.
fn fake_store(op: &Path) -> PathBuf {
    let store = op.join(".local/share/letibot");
    for d in ["logs/2026", "panes", "keys", "sessions/s-1"] {
        std::fs::create_dir_all(store.join(d)).expect("a store directory");
    }
    std::fs::create_dir_all(op.join(".config/letibot/keys")).expect("the config keys");
    std::fs::write(op.join(".config/letibot/keys/k"), SECRET).expect("a key");
    for (name, body) in [
        ("sessions.db", "sessions-data"),
        ("metrics.db", "metrics-data"),
        ("logs/2026/a.jsonl", "log-data"),
        ("panes/p", "pane-data"),
        ("room-h.chat", "chat-data"),
        ("room-h.token", SECRET),
        ("room-h.lease", SECRET),
        ("daemon-room-h.placement", SECRET),
        ("logs/x.token", SECRET),
        ("keys/k", SECRET),
        ("sessions/s-1/f", SECRET),
    ] {
        std::fs::write(store.join(name), body).expect("a store file");
    }
    store
}

#[test]
fn every_seat_reads_the_stores_data_and_never_its_secrets_or_writes_it() {
    for tight in [false, true] {
        let f = Fixture::new("store");
        let op = f.root.join("operator");
        let store = fake_store(&op);
        let view = if tight {
            f.view()
        } else {
            ViewSpec::project_only(f.project())
                .with_home(HomeView::Dir(op.clone()))
                .with_tmp(TmpView::Session(f.root.join("tmp")))
        };
        let s = Seatbelt::probe_as(view, Egress::Denied, vec![], op.clone())
            .unwrap_or_else(|e| panic!("the macOS boundary did not measure on this box: {e}"));
        let cwd = f.project();
        let st = store.display();
        for (file, want) in [
            ("sessions.db", "sessions-data"),
            ("metrics.db", "metrics-data"),
            ("logs/2026/a.jsonl", "log-data"),
            ("panes/p", "pane-data"),
            ("room-h.chat", "chat-data"),
        ] {
            let (rc, out) = run_in(&s, &cwd, &format!("cat '{st}/{file}'"));
            assert_eq!(
                (rc, out.contains(want)),
                (0, true),
                "tight={tight} {file}: {out}"
            );
            let (rc, _) = run_in(&s, &cwd, &format!("echo x >> '{st}/{file}'"));
            assert_ne!(rc, 0, "tight={tight}: {file} is not writable");
        }
        for file in [
            "room-h.token",
            "room-h.lease",
            "daemon-room-h.placement",
            "logs/x.token",
            "keys/k",
            "sessions/s-1/f",
        ] {
            let (rc, out) = run_in(&s, &cwd, &format!("cat '{st}/{file}'"));
            assert_ne!(rc, 0, "tight={tight}: {file} is read: {out}");
            assert!(!out.contains("letibot-fixture-line"), "{out}");
        }
        let (rc, _) = run_in(&s, &cwd, &format!("echo x > '{st}/new.db'"));
        assert_ne!(rc, 0, "nothing new can be created in the store");
        let (rc, out) = run_in(
            &s,
            &cwd,
            &format!("cat '{}/.config/letibot/keys/k'", op.display()),
        );
        assert_ne!(rc, 0, "{out}");
        assert!(!out.contains("letibot-fixture-line"), "{out}");
    }
}

/// The point of the store rule: `sqlite3 -readonly` answers on a live WAL database that a daemon
/// holds open, in a wide and a tight seat.
#[test]
fn sqlite_readonly_answers_on_a_live_wal_store_in_both_seats() {
    use std::io::Write;
    for tight in [false, true] {
        let f = Fixture::new("sqlite");
        let op = f.root.join("operator");
        let store = fake_store(&op);
        let db = store.join("sessions.db");
        let _ = std::fs::remove_file(&db);
        let mut writer = std::process::Command::new("/usr/bin/sqlite3")
            .arg(&db)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("the writer starts");
        writeln!(
            writer.stdin.as_mut().expect("stdin"),
            "PRAGMA journal_mode=wal; create table t(x); insert into t values (1),(2),(3);"
        )
        .expect("the writer is fed");
        let shm = store.join("sessions.db-shm");
        for _ in 0..100 {
            if shm.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(shm.exists(), "the writer holds a -shm");
        assert!(store.join("sessions.db-wal").exists(), "and a -wal");
        let view = if tight {
            f.view()
        } else {
            ViewSpec::project_only(f.project())
                .with_home(HomeView::Dir(op.clone()))
                .with_tmp(TmpView::Session(f.root.join("tmp")))
        };
        let s = Seatbelt::probe_as(view, Egress::Denied, vec![], op.clone())
            .unwrap_or_else(|e| panic!("the macOS boundary did not measure on this box: {e}"));
        let (rc, out) = run_in(
            &s,
            &f.project(),
            &format!(
                "/usr/bin/sqlite3 -readonly '{}' 'select count(*) from t'",
                db.display()
            ),
        );
        drop(writer.stdin.take());
        let _ = writer.wait();
        assert_eq!((rc, out.trim()), (0, "3"), "tight={tight}: {out}");
    }
}

// ------------------------------------------------------ firecode from the seat

/// A VM boots from the top-level seat (`firecode exec`) and not from a child's: Virtualization
/// needs its XPC services and `ps`, which only the wide profile grants.
#[test]
fn firecode_boots_a_vm_from_the_wide_seat_and_the_tight_seat_cannot() {
    let firecode = ["/opt/homebrew/bin/firecode"]
        .into_iter()
        .find(|p| Path::new(p).exists());
    let Some(_) = letibot_tokencore::apparatus::present(
        "firecode, which boots a guest through Virtualization.framework",
        firecode.is_some(),
    ) else {
        return;
    };
    let f = Fixture::new("firecode");
    let cmd = "/opt/homebrew/bin/firecode exec echo firecode-ok 2>&1";
    let host = || Egress::Host { why: "test".into() };
    // The project sits in the writable scratch: firecode copies the VM's result out beside it.
    let proj = f.root.join("tmp/project");
    std::fs::create_dir_all(&proj).expect("a project in the scratch");
    std::fs::write(proj.join("a.txt"), "hello\n").expect("a file in it");
    let view = ViewSpec::project_only(proj.clone())
        .with_home(HomeView::Dir(real_home()))
        .with_tmp(TmpView::Session(f.root.join("tmp")));
    let wide_seat = Seatbelt::probe_as(view, host(), vec![], real_home())
        .unwrap_or_else(|e| panic!("the macOS boundary did not measure on this box: {e}"));
    let (rc, out) = run_in(&wide_seat, &proj, cmd);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("firecode-ok"), "{out}");

    let tight = Seatbelt::probe_as(f.view(), host(), vec![f.denied()], real_home())
        .unwrap_or_else(|e| panic!("the macOS boundary did not measure on this box: {e}"));
    let (rc, out) = run_in(&tight, &f.project(), cmd);
    assert_ne!(rc, 0, "a child's seat booted a VM: {out}");
    assert!(!out.contains("firecode-ok"), "{out}");
}

// ------------------------------------------- `[boundary] firecode_only`, for real

/// A stand-in for a JavaScript runtime that leaves a mark when it runs.
fn fake_runtime(dir: &Path, name: &str, mark: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).expect("a tools directory");
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\necho ran >> '{}'\n", mark.display()))
        .expect("a fake runtime");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("executable");
    p
}

#[test]
fn a_denied_runtime_cannot_be_run_renamed_copied_or_reached_through_make_or_a_script() {
    let f = Fixture::new("firecode-only");
    let tools = f.project().join("tools");
    let mark = f.project().join("MARK");
    let node = fake_runtime(&tools, "node", &mark);
    let other = fake_runtime(&tools, "other", &mark);
    std::fs::write(f.project().join("Makefile"), "all:\n\t./tools/node\n").unwrap();
    std::fs::write(f.project().join("build.sh"), "#!/bin/sh\n./tools/node\n").unwrap();
    // The same list `boundary::firecode_paths` resolves, through the same mask the seat uses.
    let view = f.view().masking([node.clone()]);
    let s = Seatbelt::probe_with(view, Egress::Denied, Vec::new())
        .unwrap_or_else(|e| panic!("the macOS boundary did not measure on this box: {e}"));
    let cwd = f.project();

    // The control: an allowed program of the same shape runs and leaves its mark.
    let (rc, out) = run_in(&s, &cwd, "./tools/other");
    assert_eq!(rc, 0, "{out}");
    assert!(
        mark.is_file(),
        "the harness cannot run a fake runtime: {out}"
    );
    std::fs::remove_file(&mark).unwrap();
    let _ = other;

    let attempts = [
        "./tools/node",
        "/bin/sh ./tools/node",
        "ln -s tools/node renamed && ./renamed",
        "cp tools/node copied && ./copied",
        "cat tools/node > catted && chmod +x catted && ./catted",
        "/usr/bin/make",
        "sh build.sh",
        "env -i ./tools/node",
        "exec ./tools/node",
        "/bin/sh -c 'cd tools && ./node'",
    ];
    for a in attempts {
        let (rc, out) = run_in(&s, &cwd, a);
        assert!(
            !mark.exists(),
            "`{a}` reached the denied runtime (exit {rc}): {out}"
        );
        assert_ne!(rc, 0, "`{a}` succeeded: {out}");
    }
}
