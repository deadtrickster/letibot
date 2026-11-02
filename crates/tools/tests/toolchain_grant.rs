//! The grant that makes a confined shell able to build.
//!
//! # Why this file exists
//!
//! `bash` on the coder seat is only worth seating if the shell can run the
//! project's own tests, and the confined view is **hermetic**: `$HOME` inside is a
//! fresh tmpfs, so `~/.cargo` and `~/.rustup` are ABSENT rather than denied. That
//! is the same property that keeps a token out of the transcript, pointed at the
//! toolchain — and it means a shell with no grant is a shell that cannot compile.
//!
//! So the grant is not a convenience. It is the difference between a seated `bash`
//! and a seated `bash` that can do the one thing it was seated for, and these tests
//! assert both halves: absent without it, present with it.
//!
//! # The consequence that is asserted out loud
//!
//! `Grant::ReadOnly` is readable **into the transcript** (`confine.rs`'s doc says
//! so and `describe` prints it). Granting the toolchain is therefore a decision
//! about what a model may read, not only about what it may run, and the last test
//! here holds the boundary description to saying it.

use std::path::PathBuf;

use letibot_tools::exec::confine::{Egress, Grant, ViewSpec};
use letibot_tools::exec::{Bwrap, Confinement, ExecError};
use letibot_tools::testing::confined_harness_with;

/// What this file's skips name, for [`letibot_tokencore::apparatus`].
const BOUNDARY: &str = "a usable confinement boundary (bwrap, an unprivileged user namespace)";

/// **The cargo that runs this test, and the directories it needs to compile**, asked of the
/// environment cargo hands its children, not guessed from `$HOME`: a box whose toolchain is a
/// distro's or mise's has a `$HOME/.cargo` that does not exist, and a grant over it grants
/// nothing.
struct Toolchain {
    /// `CARGO`, which cargo sets for every process it runs: the binary that compiled this test
    /// rather than whichever one `$PATH` offers a shell.
    cargo: PathBuf,
    /// `CARGO_HOME`: the registry, the shims, and the directory a `bin/cargo` lives under.
    cargo_home: PathBuf,
    /// `RUSTUP_HOME`, where the toolchain the shims resolve to is installed. `None` for a
    /// toolchain rustup does not manage, which needs no grant for one.
    rustup_home: Option<PathBuf>,
}

impl Toolchain {
    fn of_this_run() -> Toolchain {
        let cargo = std::env::var_os("CARGO")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("cargo"));
        // The binary is the one cargo started this test with, so it exists: a derivation that names
        // a file nobody has is what `$HOME/.cargo/bin/cargo` was on every box but a rustup one.
        if std::env::var_os("CARGO").is_some() {
            assert!(
                cargo.is_file(),
                "the cargo that runs this test is a file: {}",
                cargo.display()
            );
        }
        let home = || std::env::var_os("HOME").map(PathBuf::from);
        // The registry and the shims. Cargo's own default is `$HOME/.cargo`, and `CARGO` is not a
        // way to find it: it may be the toolchain's own binary, one directory under the toolchain.
        let cargo_home = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".cargo")))
            .unwrap_or_else(|| PathBuf::from(".cargo"));
        let rustup_home = std::env::var_os("RUSTUP_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".rustup")))
            .filter(|p| p.join("toolchains").is_dir());
        Toolchain {
            cargo,
            cargo_home,
            rustup_home,
        }
    }

    /// The toolchain directories that EXIST, as grants. A read-only bind of a path the host has
    /// not got is a boundary that will not start.
    fn grants(&self) -> Vec<Grant> {
        let mut out = Vec::new();
        if self.cargo_home.is_dir() {
            out.push(Grant::ReadOnly {
                path: self.cargo_home.clone(),
                why: "the cargo registry and the rustup shims; a shell that cannot \
                      compile is not worth seating"
                    .into(),
            });
        }
        if let Some(rustup) = self.rustup_home.as_ref().filter(|p| p.is_dir()) {
            out.push(Grant::ReadOnly {
                path: rustup.clone(),
                why: "the toolchain the shims resolve to".into(),
            });
        }
        out
    }

    /// **Whether this box's cargo is somewhere the view can see without a grant**: `/usr`, `/bin`
    /// and the rest of [`letibot_tools::exec::confine::SYSTEM_ROOTS`]. Where it is, a test cannot
    /// prove that a toolchain needs granting, and says so instead of failing.
    fn is_in_the_views_system_roots(&self) -> bool {
        let Ok(real) = std::fs::canonicalize(&self.cargo) else {
            return false;
        };
        let roots = letibot_tools::exec::confine::SYSTEM_ROOTS;
        roots.iter().any(|r| real.starts_with(r))
    }
}

fn no_boundary(e: &ExecError, test: &str) {
    eprintln!("{test}: no usable boundary on this host: {e}");
}

#[test]
fn without_the_grant_the_toolchain_is_absent_rather_than_denied() {
    let tc = Toolchain::of_this_run();
    // **A box whose cargo is under `/usr` cannot be asked this question**: the view binds the
    // system roots, so the toolchain is there with or without a grant, and the test would be
    // asserting a path it chose rather than the boundary.
    let Some(()) = letibot_tokencore::apparatus::applies(
        &format!(
            "a toolchain outside the view's system roots (this box's cargo is {})",
            tc.cargo.display()
        ),
        !tc.is_in_the_views_system_roots(),
    ) else {
        return;
    };
    let mut h = match confined_harness_with(|root| {
        Bwrap::probe(ViewSpec::project_only(root), Egress::Denied).map(|b| Box::new(b) as _)
    }) {
        Ok(h) => h,
        Err(e) => return no_boundary(&e, "without_the_grant"),
    };
    // Literal paths, because layer A refuses a command whose meaning is not in its
    // own text: `$CARGO` comes from the environment, so nothing can decide about it
    // before it runs. That refusal is a feature and it is also a constraint on
    // every command a seated shell sends.
    let cargo = tc.cargo.display();
    let r = h.call(
        "bash",
        &serde_json::json!({"command": format!("ls -d {cargo}")}).to_string(),
    );
    let text = r.render();
    eprintln!("--- without grant ---\n{text}\n");
    assert!(
        text.contains("No such file") || text.contains("cannot access"),
        "a hermetic view has no toolchain, and it is ABSENT rather than denied: {text}"
    );
}

#[test]
fn with_the_grant_a_confined_shell_compiles_and_tests() {
    let tc = Toolchain::of_this_run();
    let mut h = match confined_harness_with(|root| {
        let mut v = ViewSpec::project_only(root);
        for g in tc.grants() {
            v = v.granting(g);
        }
        Bwrap::probe(v, Egress::Denied).map(|b| Box::new(b) as _)
    }) {
        Ok(h) => h,
        Err(e) => return no_boundary(&e, "with_the_grant"),
    };
    let root = h.root().canonicalize().expect("fixture root");
    // A crate small enough that the measurement is the boundary and not rustc.
    std::fs::create_dir_all(root.join("probe/src")).expect("probe tree");
    std::fs::write(
        root.join("probe/Cargo.toml"),
        "[package]\nname=\"probe\"\nversion=\"0.0.0\"\nedition=\"2021\"\n",
    )
    .expect("manifest");
    std::fs::write(
        root.join("probe/src/lib.rs"),
        "#[test]\nfn the_seal_can_build() { assert_eq!(2 + 2, 4); }\n",
    )
    .expect("lib");

    // Every value literal, for the reason the sibling test names.
    let rustup = match &tc.rustup_home {
        Some(p) => format!("RUSTUP_HOME={} ", p.display()),
        None => String::new(),
    };
    let cmd = format!(
        "cd {root}/probe && CARGO_HOME={cargo_home} {rustup}CARGO_TARGET_DIR={root}/probe/target \
         {cargo} test --offline",
        root = root.display(),
        cargo_home = tc.cargo_home.display(),
        cargo = tc.cargo.display(),
    );
    let r = h.call("bash", &serde_json::json!({"command": cmd}).to_string());
    let text = r.render();
    eprintln!("--- with grant ---\n{text}\n");
    assert!(
        text.contains("test result: ok"),
        "a granted toolchain must actually build and run the test: {text}"
    );
    assert!(text.contains("the_seal_can_build"), "{text}");
}

#[test]
fn the_boundary_says_what_a_grant_costs() {
    let tc = Toolchain::of_this_run();
    // A real directory: `probe` measures the boundary it is given, so a path that
    // does not exist takes the "no usable boundary" branch and this test would pass
    // having measured nothing at all.
    let dir = std::env::temp_dir().join(format!("letibot-grant-desc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a project to be confined to");
    let mut v = ViewSpec::project_only(&dir);
    let granted = tc.grants();
    for g in granted.clone() {
        v = v.granting(g);
    }
    let Some(()) = letibot_tokencore::apparatus::present(
        "a toolchain directory this run's cargo names (CARGO_HOME or the binary's own parent)",
        !granted.is_empty(),
    ) else {
        return;
    };
    let probed = Bwrap::probe(v, Egress::Denied);
    let _ = std::fs::remove_dir_all(&dir);
    let Ok(b) = probed else {
        // Through `apparatus`, so `LETIBOT_REQUIRE_APPARATUS=1` can refuse the skip (R67 item 18)
        // — but only where a boundary COULD be. macOS has no namespaces to enter and no bwrap
        // to install, so a skip refused there is a requirement no provisioning can meet, and it
        // made the gate's test step red on the operator's Mac for a reason in no diff.
        let Some(_) = letibot_tokencore::apparatus::applies(BOUNDARY, cfg!(target_os = "linux"))
        else {
            return;
        };
        return letibot_tokencore::apparatus::absent(BOUNDARY);
    };
    let d = b.describe();
    eprintln!("--- describe ---\n{d}\n");
    // **Each granted directory is named**, which is the claim: a grant is a decision the
    // operator reads, and one that cannot be named cannot be revoked. The paths are the ones
    // this run's toolchain is at, not a spelling a box may not have.
    for g in &granted {
        assert!(
            d.contains(&g.path().display().to_string()),
            "a grant is named where it can be revoked: {} is not in {d}",
            g.path().display()
        );
    }
    // The why, because a grant nobody can explain is a grant nobody can revoke.
    assert!(
        d.contains("not worth seating") || d.contains("toolchain"),
        "{d}"
    );
    // And the consequence, which is the half a reader skips: read-only in the view
    // still means readable INTO THE TRANSCRIPT.
    assert!(
        !d.contains("no grants"),
        "a granted view must not describe itself as ungranted: {d}"
    );
}
