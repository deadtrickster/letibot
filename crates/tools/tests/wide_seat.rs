//! The two Linux seats, measured: the wide one (the machine at its own paths, the stores masked) and
//! the tight one (a view of the checkout, with a store masked inside a granted directory). Every
//! fixture home is a temporary directory, never the operator's, and every secret is a fake.

use std::path::{Path, PathBuf};

use letibot_tools::exec::confine::{
    Bwrap, ConfinePlan, Confinement, Egress, HomeView, TmpView, ViewSpec,
};
use letibot_tools::exec::seat::{Level, seat_for_level, seat_view};

/// The apparatus a wide seat needs: bubblewrap and an unprivileged user namespace. A box without
/// them skips loudly through `apparatus`, which `LETIBOT_REQUIRE_APPARATUS` turns into a failure.
const BOUNDARY: &str = "a usable bubblewrap boundary (bwrap and an unprivileged user namespace)";

const WHO_IS_IT: &str = "letibot-fixture-line-never-a-real-key";

/// A fake crates.io token, shaped like one so a substring match is a real measurement.
const TOKEN: &str = "letibot-fixture-cargo-token-0000";

/// A fake operator home with a fake key in it, a gh config, a note, and one credential file. The
/// repository is beside it, so a write outside the home has somewhere to be refused.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Option<Fixture> {
        let root =
            std::env::temp_dir().join(format!("letibot-wideseat-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let f = Fixture { root };
        std::fs::create_dir_all(f.home().join(".ssh")).expect("a fake store");
        std::fs::create_dir_all(f.home().join(".config/gh")).expect("a fake gh config");
        std::fs::write(f.home().join(".ssh/id_rsa"), WHO_IS_IT).expect("a fake key");
        std::fs::write(f.home().join(".netrc"), WHO_IS_IT).expect("a fake credential file");
        std::fs::write(
            f.home().join(".config/gh/hosts.yml"),
            "github.com:\n    user: fixture\n",
        )
        .expect("a fake gh config");
        std::fs::write(f.home().join("note.txt"), "plain\n").expect("a note");
        std::fs::create_dir_all(f.project()).expect("a project");
        Some(f)
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn home(&self) -> PathBuf {
        self.root.join("operator")
    }

    fn project(&self) -> PathBuf {
        self.root.join("project")
    }

    /// The seat's own view, built by the one builder both platforms use: a top-level session's
    /// home, the fence, and the masks that go with it.
    fn view(&self) -> ViewSpec {
        let seat = seat_for_level(
            Level::Wide,
            &letibot_tools::mode::Mode::WRITES_ALLOWED,
            "this machine",
            &[] as &[&str],
            &self.home(),
            None,
        );
        assert!(
            !seat.masked.is_empty(),
            "a wide seat carries the fence, or there is nothing to measure"
        );
        ViewSpec::project_only(self.project())
            .with_home(HomeView::Dir(self.home()))
            .with_tmp(TmpView::Session(self.root.join("tmp")))
            .wide()
            .masking(seat.masked.iter().cloned())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The boundary, or the loud skip: a box that cannot nest a namespace here measures nothing and
/// says so rather than reporting a pass.
fn probe(f: &Fixture) -> Option<Bwrap> {
    match Bwrap::probe(f.view(), Egress::Denied) {
        Ok(b) => Some(b),
        Err(e) => {
            letibot_tokencore::apparatus::applies(BOUNDARY, cfg!(target_os = "linux"))?;
            eprintln!("the wide seat could not be measured on this box: {e}");
            letibot_tokencore::apparatus::absent(BOUNDARY);
            None
        }
    }
}

/// One command inside the seat, as its own argv and not through a shell of the harness's.
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
fn a_wide_seat_reads_and_writes_its_home_and_the_masks_are_the_fence() {
    let Some(f) = Fixture::new("home") else {
        return;
    };
    let Some(b) = probe(&f) else {
        return;
    };
    let home = f.home().display().to_string();
    // The home is the seat's `$HOME`, its files are there, and a write lands on the host.
    let (out, rc) = run(
        &b,
        &f,
        "echo home=$HOME && cat \"$HOME/note.txt\" && echo new > \"$HOME/written.txt\" \
         && cat /etc/hosts >/dev/null && echo ok",
    );
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains(&format!("home={home}")), "{out}");
    assert!(out.contains("plain"), "{out}");
    assert!(out.contains("ok"), "the machine is visible: {out}");
    assert!(
        f.home().join("written.txt").is_file(),
        "the real home is writable"
    );

    // The key is not readable, and neither is a credential file: one is an empty directory and the
    // other an unreadable `/dev/null`, and neither discloses a byte.
    let (out, rc) = run(&b, &f, "cat \"$HOME/.ssh/id_rsa\"");
    assert_ne!(rc, 0, "a masked store was readable: {out}");
    assert!(!out.contains(WHO_IS_IT), "{out}");
    let (out, _) = run(&b, &f, "cat \"$HOME/.netrc\"");
    assert!(!out.contains(WHO_IS_IT), "a masked file disclosed: {out}");
    // And the mask is not a hole a write can open either.
    let (out, rc) = run(&b, &f, "echo planted > \"$HOME/.ssh/planted\"");
    assert_ne!(rc, 0, "a command wrote into the key store: {out}");
    assert!(!f.home().join(".ssh/planted").exists(), "{out}");

    // gh's config is the one store a top-level seat keeps: the token it injects is for `gh`.
    let (out, rc) = run(&b, &f, "cat \"$HOME/.config/gh/hosts.yml\"");
    assert_eq!(rc, 0, "a top-level seat reads gh's config: {out}");
    assert!(out.contains("user: fixture"), "{out}");
}

#[test]
fn a_wide_seat_keeps_the_machines_tmp_and_puts_the_session_scratch_in_tmpdir() {
    let Some(f) = Fixture::new("tmp") else {
        return;
    };
    let Some(b) = probe(&f) else {
        return;
    };
    // The machine's `/tmp` is the machine's: a wide seat does not empty it, and the session's own
    // scratch reaches the command as `$TMPDIR` instead.
    let (out, rc) = run(
        &b,
        &f,
        "echo scratch=$TMPDIR && echo kept > \"$TMPDIR/probe.txt\" && cat \"$TMPDIR/probe.txt\"",
    );
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("kept"), "{out}");
    assert!(
        f.root().join("tmp/probe.txt").is_file(),
        "a file one call writes is in the directory the daemon made"
    );
}

#[test]
fn a_tight_seat_is_still_a_view_of_the_checkout_and_not_the_machine() {
    // The contrast that makes the wide seat a decision rather than a default: the same helper, the
    // same box, and a child's view that does not hold the machine.
    let Some(f) = Fixture::new("tight") else {
        return;
    };
    let Some(b) = Bwrap::probe(ViewSpec::project_only(f.project()), Egress::Denied).ok() else {
        let Some(_) = letibot_tokencore::apparatus::applies(BOUNDARY, cfg!(target_os = "linux"))
        else {
            return;
        };
        letibot_tokencore::apparatus::absent(BOUNDARY);
        return;
    };
    let (out, rc) = run(&b, &f, "cat \"$HOME/note.txt\"");
    assert_ne!(rc, 0, "a tight seat's $HOME is a tmpfs: {out}");
    let (out, rc) = run(&b, &f, "echo x > inside.txt && cat inside.txt");
    assert_eq!(rc, 0, "the checkout is still the checkout: {out}");

    // A tight seat still carries the fence, because a grant can put a store inside the view: what
    // makes it harmless elsewhere is the renderer, which masks only the paths this view reaches.
    let seat = seat_view(
        &letibot_tools::mode::Mode::WRITES_ALLOWED,
        "x",
        &[f.home().join(".cargo").display().to_string()],
        &f.home(),
    );
    assert!(
        seat.masked
            .iter()
            .any(|p| p.ends_with(".cargo/credentials")),
        "the tight seat's list is the fence a grant can reach into: {}",
        seat.masked.len()
    );
}

/// The tight seat with the operator's toolchain granted read-only, which is where a credential
/// store arrives inside a view that otherwise holds nothing of theirs.
fn tight_with_cargo(f: &Fixture) -> Option<Bwrap> {
    let seat = seat_view(
        &letibot_tools::mode::Mode::WRITES_ALLOWED,
        "this machine",
        &[f.home().join(".cargo").display().to_string()],
        &f.home(),
    );
    let view = ViewSpec::project_only(f.project()).masking(seat.masked.iter().cloned());
    let view = seat.grants.iter().fold(view, |v, g| v.granting(g.clone()));
    match Bwrap::probe(view, Egress::Denied) {
        Ok(b) => Some(b),
        Err(e) => {
            letibot_tokencore::apparatus::applies(BOUNDARY, cfg!(target_os = "linux"))?;
            eprintln!("the tight seat could not be measured on this box: {e}");
            letibot_tokencore::apparatus::absent(BOUNDARY);
            None
        }
    }
}

#[test]
fn a_granted_toolchain_carries_no_credential_store_into_the_view() {
    let Some(f) = Fixture::new("cargo") else {
        return;
    };
    // A fake crates.io token in a fake home, and a config beside it that must stay readable: a
    // boundary that hid both would be a boundary that broke `cargo`.
    let cargo = f.home().join(".cargo");
    std::fs::create_dir_all(&cargo).expect("a fake toolchain");
    std::fs::write(cargo.join("credentials.toml"), TOKEN).expect("a fake token file");
    std::fs::write(
        cargo.join("config.toml"),
        "[build]\nrustflags = [\"-C\", \"opt-level=2\"]\n",
    )
    .expect("a fake config");
    let Some(b) = tight_with_cargo(&f) else {
        return;
    };
    let credentials = cargo.join("credentials.toml");
    assert!(
        std::fs::read_to_string(&credentials)
            .expect("the fixture wrote it")
            .contains(TOKEN),
        "the fixture's own claim is wrong"
    );
    // The measurement: the same path, read from inside the seat, discloses nothing. A tight seat
    // would otherwise read it, because the grant is the whole directory.
    let (out, _) = run(
        &b,
        &f,
        &format!("cat {} ; echo \"rc=$?\"", credentials.display()),
    );
    assert!(
        !out.contains(TOKEN),
        "a granted toolchain leaked its store: {out}"
    );
    assert!(
        out.contains("rc=0"),
        "the name is there and empty, not refused: {out}"
    );
    let (out, rc) = run(
        &b,
        &f,
        &format!("cat {}", cargo.join("config.toml").display()),
    );
    assert_eq!(rc, 0, "the toolchain's own config stays readable: {out}");
    assert!(out.contains("opt-level=2"), "{out}");
}
