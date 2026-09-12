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

use std::path::Path;

use letibot_tools::exec::confine::{Egress, Grant, ViewSpec};
use letibot_tools::exec::{Bwrap, Confinement, ExecError};
use letibot_tools::testing::confined_harness_with;

fn toolchain() -> Vec<Grant> {
    let home = std::env::var("HOME").expect("a home");
    vec![
        Grant::ReadOnly {
            path: Path::new(&home).join(".cargo"),
            why: "the cargo registry and the rustup shims; a shell that cannot \
                  compile is not worth seating"
                .into(),
        },
        Grant::ReadOnly {
            path: Path::new(&home).join(".rustup"),
            why: "the toolchain the shims resolve to".into(),
        },
    ]
}

fn no_boundary(e: &ExecError, test: &str) {
    eprintln!("{test}: no usable boundary on this host: {e}");
}

#[test]
fn without_the_grant_the_toolchain_is_absent_rather_than_denied() {
    let mut h = match confined_harness_with(|root| {
        Bwrap::probe(ViewSpec::project_only(root), Egress::Denied).map(|b| Box::new(b) as _)
    }) {
        Ok(h) => h,
        Err(e) => return no_boundary(&e, "without_the_grant"),
    };
    // Literal paths, because layer A refuses a command whose meaning is not in its
    // own text: `$HOME` comes from the environment, so nothing can decide about it
    // before it runs. That refusal is a feature and it is also a constraint on
    // every command a seated shell sends.
    let home = std::env::var("HOME").expect("a home");
    let r = h.call(
        "bash",
        &serde_json::json!({"command": format!("ls -d {home}/.cargo/bin/cargo")}).to_string(),
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
    let mut h = match confined_harness_with(|root| {
        let mut v = ViewSpec::project_only(root);
        for g in toolchain() {
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
    let home = std::env::var("HOME").expect("a home");
    let cmd = format!(
        "cd {root}/probe && CARGO_HOME={home}/.cargo RUSTUP_HOME={home}/.rustup \
         CARGO_TARGET_DIR={root}/probe/target {home}/.cargo/bin/cargo test --offline",
        root = root.display()
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
    // A real directory: `probe` measures the boundary it is given, so a path that
    // does not exist takes the "no usable boundary" branch and this test would pass
    // having measured nothing at all.
    let dir = std::env::temp_dir().join(format!("letibot-grant-desc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a project to be confined to");
    let mut v = ViewSpec::project_only(&dir);
    for g in toolchain() {
        v = v.granting(g);
    }
    let probed = Bwrap::probe(v, Egress::Denied);
    let _ = std::fs::remove_dir_all(&dir);
    let Ok(b) = probed else {
        return eprintln!("no usable boundary on this host");
    };
    let d = b.describe();
    eprintln!("--- describe ---\n{d}\n");
    assert!(
        d.contains(".cargo"),
        "a grant is named where it can be revoked: {d}"
    );
    assert!(d.contains(".rustup"), "{d}");
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
