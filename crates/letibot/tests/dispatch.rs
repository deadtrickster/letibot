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
//! legitimate usage error — which is the same code an unported stub returned. So
//! `assert_ne!(code, 2)` proves nothing, and an exit-code check cannot tell "wired"
//! from "not a role of this binary yet". The distinction is the MESSAGE.
//!
//! # Every role is wired, so the marker needed a new producer
//!
//! With the `harnessd` role, all six are wired and `roles::not_yet` is deleted. The
//! marker below is now produced by exactly one thing: the **launcher**, which is not a
//! role and says so. That matters — if nothing printed it,
//! `assert!(!contains(marker))` would pass against a binary that printed nothing at
//! all, which is the can-only-pass shape this repository keeps finding.
//! [`the_launcher_is_the_only_thing_that_is_not_a_role`] is what keeps it honest.

/// The sentence a thing that is not a role prints. One producer now — the launcher.
const NOT_A_ROLE: &str = "not a role of this binary yet";

/// **Safe invocations only, and that is a constraint rather than tidiness.**
///
/// `letibot daemon` with no arguments STARTS A DAEMON: a test that runs it leaves one
/// behind, holding a socket and a store, and it would hang rather than fail. `letibot
/// tui` with no arguments attaches a head, which is the same hazard one step down. So
/// each role is reached by something that EXITS — a usage error, a version, or a
/// refusal — and the list is the record of which invocation is safe for which role.
const SAFE: &[(&str, &[&str])] = &[
    ("daemon", &["--bogus"]),
    ("tui", &["--version"]),
    ("head", &["--version"]),
    ("askpass", &[]),
    ("m1", &["--bogus"]),
    ("render", &[]),
    ("render-qwen", &[]),
];

fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_letibot"));
    cmd.args(args)
        // No session, no socket: a role that would attach must not find one.
        .env_remove("LETIBOT_SOCKET")
        .env_remove("LETIBOT_SESSION")
        .env_remove("LETIBOT_COMPLETION_URL");
    let out = cmd.output().expect("the multicall runs");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// **No wired role may say it is not a role.**
///
/// This is the direct statement of the fact that went stale in `54308ff`: `Role::Render`
/// was wired and left in a list of unported roles. CI caught it through an *older* test,
/// which was an accident of which assertion ran first; this one says it plainly, and it
/// now covers all six roles.
#[test]
fn no_wired_role_says_it_is_not_a_role() {
    for (word, args) in SAFE {
        let mut full: Vec<&str> = vec![word];
        full.extend_from_slice(args);
        let (_, _, stderr) = run(&full);
        assert!(
            !stderr.contains(NOT_A_ROLE),
            "`letibot {word}` is wired and must not claim otherwise: {stderr}"
        );
    }
}

/// **And the marker still has a producer, so the test above cannot be vacuous.**
///
/// The launcher is the one thing that is not a role: `letibot` with no arguments is the
/// shell script's job, and the multicall says so rather than guessing which daemon the
/// caller meant. If this stops producing the marker, the assertion above becomes a
/// statement about the empty set — which is why this test exists rather than a comment
/// saying the marker is still used.
#[test]
fn the_launcher_is_the_only_thing_that_is_not_a_role() {
    let (code, _, stderr) = run(&[]);
    assert!(
        stderr.contains(NOT_A_ROLE),
        "the bare command must explain that the launcher is not a role yet: {stderr}"
    );
    assert!(
        stderr.contains("launcher"),
        "and it must say WHICH thing is missing, by name: {stderr}"
    );
    assert_eq!(code, Some(2), "an unwired launcher must exit 2");
}

/// **`letibot` answers for itself**, and a typo is refused by name rather than resolving
/// to a role — the dispatcher's own name is not in the table, which is what stops
/// `letibot` from becoming the daemon by substring.
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

/// Every role answers to its own NAME, through a symlink — the shape an install
/// creates, and what `sudo` (for `letibot-askpass`) and the launcher reach by PATH.
///
/// **Exit 127 is the assertion worth having here.** The multicall links `libllama`
/// through `letibot-harnessd`, so every one of these names needs the rpath to resolve;
/// 127 is the loader saying it could not. That is the arm64 defect and the `m1` defect
/// in one check, and it costs a symlink to ask.
#[test]
fn each_role_is_reachable_through_a_symlink_of_its_own_name() {
    let dir = std::env::temp_dir().join(format!("letibot-dispatch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp dir");
    let exe = env!("CARGO_BIN_EXE_letibot");
    for (name, args) in [
        ("harnessd", vec!["--bogus"]),
        ("letibot-tui", vec!["--version"]),
        ("letibot-askpass", vec![]),
        ("letibot-m1", vec!["--bogus"]),
        ("letibot-render", vec![]),
        ("letibot-render-qwen", vec![]),
    ] {
        let link = dir.join(name);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(exe, &link).expect("a symlink");
        let out = std::process::Command::new(&link)
            .args(&args)
            .env_remove("LETIBOT_SOCKET")
            .env_remove("LETIBOT_SESSION")
            .output()
            .expect("the role runs");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_ne!(
            out.status.code(),
            Some(127),
            "`{name}` must not fail to LOAD — it shares the multicall's libraries, and \
             127 means the loader could not find one: {stderr}"
        );
        assert!(
            !stderr.contains(NOT_A_ROLE),
            "`{name}` is a role reached by its own name, not a refusal: {stderr}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
