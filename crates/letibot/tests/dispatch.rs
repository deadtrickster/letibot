//! **The multicall, EXECUTED rather than merely present.**
//!
//! These spawn the real binary, because the claims here are about what a *process*
//! does: its exit code, and what it prints on stderr. `env!("CARGO_BIN_EXE_…")` is why
//! they live in `tests/` rather than beside `roles::run` — Cargo defines it for
//! integration tests, not for unit tests inside the binary, and the first version of
//! these assertions was placed in the wrong module and failed to compile there.
//!
//! # Why the MESSAGE and not the exit code
//!
//! MEASURED while wiring `Role::RenderQwen`: it exits **2** with no arguments — a
//! legitimate usage error — which is the same code the unported stub returns. So
//! `assert_ne!(code, 2)` proves nothing about the renderers, and the unit test that
//! asserted it was vacuous for exactly the roles it was written to cover.
//!
//! A role's own refusals are its business. What this file asserts is the one sentence
//! no other role prints, in both directions, so the two cannot both hold.

/// The sentence only the unported branch prints. Both tests key on it, so rewriting
/// that message fails here rather than silently making `is_wired` vacuous.
const STUB_MARKER: &str = "is not a role of this binary yet";

fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_letibot"))
        .args(args)
        .output()
        .expect("the multicall runs");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// **A wired role must not say it is unported.**
///
/// This is the direct statement of the fact that went stale in `54308ff`: `Role::Render`
/// was wired and left in a list of unported roles. CI caught it through the *other*
/// test, which is an accident of which assertion ran first; this one says it plainly.
#[test]
fn wired_roles_do_not_say_they_are_unported() {
    for word in ["render", "render-qwen"] {
        let (_, _, stderr) = run(&[word]);
        assert!(
            !stderr.contains(STUB_MARKER),
            "`letibot {word}` is wired and must not claim otherwise: {stderr}"
        );
    }
}

/// And the roles that are NOT wired still say so, with the exit code the dispatcher
/// promises. Without this, the test above would pass against a binary that printed
/// nothing at all — and a check that cannot fail is the shape this whole repository
/// keeps finding.
#[test]
fn unwired_roles_still_say_they_are_unported() {
    for word in ["daemon", "tui", "m1"] {
        let (code, _, stderr) = run(&[word]);
        assert!(
            stderr.contains(STUB_MARKER),
            "`letibot {word}` is not wired and must say so, not: {stderr}"
        );
        assert_eq!(code, Some(2), "`letibot {word}` must exit 2");
    }
}

/// **The launcher role is not wired either**, and it is the one a person hits by
/// typing the bare command. Asserted separately because its message is different —
/// it names `scripts/letibot` rather than a source file — and because it is the role
/// whose absence a user is most likely to meet.
#[test]
fn the_bare_command_says_the_launcher_is_not_a_role_yet() {
    let (code, _, stderr) = run(&[]);
    assert!(
        stderr.contains("launcher is not a role of this binary yet"),
        "`letibot` with no arguments must explain itself: {stderr}"
    );
    assert_eq!(code, Some(2), "an unwired launcher must exit 2");
}

/// **`letibot` answers for itself**, and a typo is refused by name rather than
/// resolving to a role — the dispatcher's own name is not in the table, which is
/// what stops `letibot` from becoming the daemon by substring.
#[test]
fn the_dispatcher_answers_for_itself_and_refuses_a_typo() {
    let (code, stdout, _) = run(&["--version"]);
    assert_eq!(code, Some(0), "--version must succeed");
    assert!(
        stdout.starts_with("letibot "),
        "--version printed: {stdout:?}"
    );

    let (code, _, stderr) = run(&["nonsense"]);
    assert_eq!(code, Some(2), "an unknown role must exit 2");
    assert!(
        stderr.contains("no such role"),
        "an unknown role must say so: {stderr}"
    );
}
