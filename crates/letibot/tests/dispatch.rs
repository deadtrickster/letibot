//! **The multicall, EXECUTED rather than merely present.**
//!
//! These spawn the real binary, because the claims are about what a *process* does: its exit
//! code and what it prints. `env!("CARGO_BIN_EXE_…")` is defined for integration tests only,
//! which is why they live here.
//!
//! The distinction between "wired" and "not a role of this binary yet" is the MESSAGE, not the
//! exit code: `Role::RenderQwen` exits 2 with no arguments, the same code an unported stub gave.

/// The sentence a thing that is not a role prints. One producer now - the launcher.
const NOT_A_ROLE: &str = "not a role of this binary yet";

/// **Safe invocations only, and that is a constraint rather than tidiness.** `letibot daemon`
/// with no arguments STARTS A DAEMON that would hang rather than fail, and `letibot tui` attaches
/// a head. So each role is reached by something that EXITS, and this list is the record.
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

/// **No wired role may say it is not a role.** `Role::Render` was once wired and left in a list
/// of unported roles; an older test caught it by accident of which assertion ran first, so this
/// one says it plainly, and it covers all six roles.
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

/// **And the marker still has a producer, so the test above cannot be vacuous.** The launcher is
/// the one thing that is not a role: `letibot` with no arguments is the shell script's job. If it
/// stopped producing the marker, the assertion above would be a statement about the empty set.
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

/// **`--help` names every verb this dispatcher accepts, and its descriptions line up.**
///
/// A usage block nobody reads is one whose columns are broken: the `\` continuation this text
/// used to be written with dropped the indent from every second line, and the verbs that are not
/// roles (`migrate`, `reattach`) were missing from it entirely.
#[test]
fn the_help_lists_every_verb_and_keeps_its_descriptions_indented() {
    let (code, stdout, stderr) = run(&["--help"]);
    assert_eq!(code, Some(0), "`--help` is not a failure: {stderr}");
    for verb in [
        "stats [VERB]",
        "fb --session",
        "migrate",
        "attach github",
        "daemon",
        "tui | head",
    ] {
        assert!(
            stdout.contains(verb),
            "`letibot --help` must name `{verb}`:\n{stdout}"
        );
    }
    assert!(
        stdout.contains("--help`") && stdout.contains("verb"),
        "it must say a verb answers for itself, `letibot <verb> --help`:\n{stdout}"
    );
    // A description column, not a wall: the text after the command starts at column 2.
    for line in stdout.lines() {
        assert!(
            !line.starts_with("turns,") && !line.starts_with("per open pull"),
            "a wrapped description lost its indent: {line:?}"
        );
    }
}

/// **`letibot` answers for itself**, and a typo is refused by name rather than resolving
/// to a role - the dispatcher's own name is not in the table, which is what stops
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

/// **A typo is two lines: the mistake, and what to type next.** The sentence used to end
/// "or see <the path this binary was run from>", which is not a command a person can use.
#[test]
fn a_typo_is_refused_in_two_lines_and_never_echoes_the_binary_path() {
    let (code, _, stderr) = run(&["nonsense"]);
    assert_eq!(code, Some(2), "a typo is not a run: {stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 2, "two lines, not a paragraph: {stderr:?}");
    assert!(
        lines[0].contains("no such role: nonsense"),
        "the first line names the mistake: {stderr:?}"
    );
    assert!(
        lines[1].contains("letibot --help"),
        "the second names the next step: {stderr:?}"
    );
    assert!(
        !stderr.contains(env!("CARGO_BIN_EXE_letibot")),
        "the binary's own path is not something to run: {stderr:?}"
    );
}

/// Every role answers to its own NAME, through a symlink - the shape an install creates, and what
/// `sudo` and the launcher reach by PATH. **Exit 127 is the assertion worth having**: the multicall
/// links `libllama`, so every one of these names needs the rpath, and 127 is the loader refusing.
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
