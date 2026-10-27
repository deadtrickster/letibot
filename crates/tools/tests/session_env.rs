//! **D14/R95 end to end: a real shell says what it holds, and the next command is read
//! against it.**
//!
//! `crates/tools/src/exec/shell.rs`'s own tests cover the trailer and the filter; the
//! resolver's cover the order and the card. What neither can cover is the join — that the
//! bytes a *real* shell reports after one line are the values a command adjudicated after it
//! resolves through. That is what this file is, and it is deliberately the whole chain:
//!
//! ```text
//!   ShellSession::run("export LT_DIR=…")   →   the trailer's NAME=value pairs
//!        →  SessionEnv::observe            →    Surroundings::with_session
//!        →  Baseline::of_command("cat $LT_DIR/file")
//! ```
//!
//! **No cgroup and no boundary**, which is why it runs in a confined session's boundary:
//! `ShellConfig::scope` is `None`, so no `cgroup.procs` is written and nothing nests
//! `bwrap`. What it does need is a pty and a shell that has `compgen -e` — bash — and on a
//! box where neither is there the test says so rather than pretending to pass.

use std::time::Duration;

use letibot_tools::exec::{ShellConfig, ShellSession};
use letibot_tools::intent::env::SessionEnv;
use letibot_tools::intent::{Baseline, Region, ShellTrust, Surroundings};

/// The shell the tests drive: bash with no rc, so an assertion is about this chain and not
/// about whatever the operator's `.bashrc` exports.
fn shell() -> Option<Vec<String>> {
    let bash = std::path::Path::new("/bin/bash");
    bash.exists().then(|| {
        vec![
            "/bin/bash".to_string(),
            "--norc".to_string(),
            "--noprofile".to_string(),
            "-i".to_string(),
        ]
    })
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("letibot-env-e2e-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("a scratch directory");
    d.canonicalize().expect("a canonical directory")
}

/// **`export X=…` in one line, and `cat $X/file` adjudicated after it.**
///
/// The claim the operator's decision makes, in one test: the value does not have to be
/// written into the command, because the session's shell holds it and said so.
#[test]
fn an_export_in_one_line_resolves_a_command_adjudicated_after_it() {
    let Some(shell) = shell() else {
        eprintln!("skipped: no /bin/bash on this box, so there is no compgen -e to report");
        return;
    };
    let dir = tmp("export");
    let target = dir.join("sub");
    std::fs::create_dir_all(&target).expect("a directory");
    let mut s = ShellSession::open(&ShellConfig {
        shell,
        cwd: dir.clone(),
        env: Vec::new(),
        ..ShellConfig::default()
    })
    .expect("a shell session on a pty");

    s.run(
        &format!("export LT_DIR={}", target.display()),
        Duration::from_secs(30),
    )
    .expect("a turn");
    // **The shell's own answer**, not a value this test remembered.
    let report = s.env().expect("every trailer carries the environment");
    let mut env = SessionEnv::seeded([(
        "PATH".to_string(),
        std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
    )]);
    env.observe(report).expect("a report this build believes");

    let surroundings = Surroundings::from_env(dir.display().to_string())
        .with_pinned_shell("a test fixture: the trailer is what is being measured")
        .with_session(env);
    let b = Baseline::of_command("cat $LT_DIR/file", &surroundings);
    assert!(
        b.resolved,
        "the value the shell reported did not resolve: {:?}",
        b.findings
    );
    // The path was placed where the shell said it was — the point of reading the shell's
    // answer rather than an assumption about where a session writes.
    let source = b
        .command
        .as_ref()
        .map(|n| n.source.clone())
        .unwrap_or_default();
    assert!(
        source.contains(&format!("{}/file", target.display())),
        "the value was not written out: {source}"
    );
    assert!(
        b.regions.contains(&Region::Workspace),
        "the file is under the workspace the session was opened in: {:?}",
        b.regions
    );
    // And the card says which name and where it came from.
    let said = b.findings.join(" | ");
    assert!(said.contains("`$LT_DIR` is"), "{said}");
    assert!(said.contains("this session's shell reported"), "{said}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// **A value the shell stops holding stops resolving** — the other half of *observed*.
///
/// `unset` on one line is a fact the next trailer reports by omission, and a resolver that
/// remembered the first report would go on placing a directory that is no longer in the
/// environment.
#[test]
fn an_unset_on_a_later_line_stops_resolving() {
    let Some(shell) = shell() else {
        eprintln!("skipped: no /bin/bash on this box");
        return;
    };
    let dir = tmp("unset");
    let mut s = ShellSession::open(&ShellConfig {
        shell,
        cwd: dir.clone(),
        env: Vec::new(),
        ..ShellConfig::default()
    })
    .expect("a shell session on a pty");
    let mut env = SessionEnv::default();
    s.run(
        &format!("export LT_GONE={}", dir.display()),
        Duration::from_secs(30),
    )
    .expect("a turn");
    env.observe(s.env().expect("a report"))
        .expect("a clean report");
    let sur = Surroundings::from_env(dir.display().to_string())
        .with_pinned_shell("a test fixture")
        .with_session(env.clone());
    assert!(Baseline::of_command("cat $LT_GONE/x", &sur).resolved);

    s.run("unset LT_GONE", Duration::from_secs(30))
        .expect("a turn");
    let mut later = env;
    later
        .observe(s.env().expect("a report"))
        .expect("a clean report");
    let sur = Surroundings::from_env(dir.display().to_string())
        .with_pinned_shell("a test fixture")
        .with_session(later);
    assert!(
        !Baseline::of_command("cat $LT_GONE/x", &sur).resolved,
        "a name the shell no longer holds still resolved"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **A credential the shell holds reaches the card masked and never the reading.** The same
/// chain as above, with the one value that must not be written down.
#[test]
fn a_credential_the_shell_reports_is_masked_and_not_spliced() {
    let Some(shell) = shell() else {
        eprintln!("skipped: no /bin/bash on this box");
        return;
    };
    let dir = tmp("credential");
    let mut s = ShellSession::open(&ShellConfig {
        shell,
        cwd: dir.clone(),
        env: Vec::new(),
        ..ShellConfig::default()
    })
    .expect("a shell session on a pty");
    let token = "ghp_0123456789abcdef4c";
    s.run(
        &format!("export LT_GITHUB_TOKEN={token}"),
        Duration::from_secs(30),
    )
    .expect("a turn");
    let mut env = SessionEnv::default();
    env.observe(s.env().expect("a report"))
        .expect("a clean report");
    let sur = Surroundings::from_env(dir.display().to_string())
        .with_pinned_shell("a test fixture")
        .with_session(env);
    let b = Baseline::of_command(
        "curl -H \"Authorization: Bearer $LT_GITHUB_TOKEN\" https://example.invalid/",
        &sur,
    );
    let said = b.findings.join(" | ");
    assert!(
        said.contains("ghp_…4c"),
        "the card must name it, masked: {said}"
    );
    assert!(!said.contains(token), "the token reached the card: {said}");
    assert!(
        !b.command.as_ref().is_some_and(|n| n.source.contains(token)),
        "the token was written into the reading"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A `ShellTrust` that is not `Pinned` is a different axis; this only keeps the import list
/// honest when the fixtures above change.
#[test]
fn the_fixture_declares_a_pinned_shell() {
    assert_ne!(
        ShellTrust::Pinned { how: "x".into() }.as_str(),
        ShellTrust::default().as_str()
    );
}
