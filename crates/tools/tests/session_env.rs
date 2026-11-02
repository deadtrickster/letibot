//! A real shell says what it holds, and the next command is read against it.
//!
//! The shell's own tests cover the trailer and the filter, the resolver's cover the order and
//! the card; only this file covers the join, which is the chain below.
//!
//! ```text
//!   ShellSession::run("export LT_DIR=…")   →   the trailer's NAME=value pairs
//!        →  SessionEnv::observe            →    Surroundings::with_session
//!        →  Baseline::of_command("cat $LT_DIR/file")
//! ```
//!
//! No cgroup and no boundary: `ShellConfig::scope` is `None`, so nothing writes `cgroup.procs`
//! and nothing nests `bwrap`, which is what lets this run inside a confined session's own
//! boundary. It needs a pty and a bash with `compgen -e`, and says so where either is absent.

use std::time::Duration;

use letibot_tools::exec::{ShellConfig, ShellSession};
use letibot_tools::intent::env::SessionEnv;
use letibot_tools::intent::{Baseline, Region, ShellTrust, Surroundings};

/// The bash these tests drive, `LETIBOT_TEST_BASH` first: a box whose bash is somewhere else can
/// still run them, and pointing it at nothing is how the skip below is measured on a box that has
/// `/bin/bash`.
fn bash_path() -> std::path::PathBuf {
    std::env::var_os("LETIBOT_TEST_BASH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/bin/bash".into())
}

/// The shell the tests drive: bash with no rc, so an assertion is about this chain and not
/// about whatever a `.bashrc` exports. A bash with `compgen -e` is apparatus, so the skip goes
/// through `apparatus` and `LETIBOT_REQUIRE_APPARATUS=1` refuses it instead of passing quietly.
fn shell() -> Option<Vec<String>> {
    let bash = bash_path();
    letibot_tokencore::apparatus::present(
        "a bash with `compgen -e` (the shell whose trailer is read)",
        bash.exists(),
    )?;
    Some(vec![
        bash.display().to_string(),
        "--norc".to_string(),
        "--noprofile".to_string(),
        "-i".to_string(),
    ])
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("letibot-env-e2e-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("a scratch directory");
    d.canonicalize().expect("a canonical directory")
}

/// `export X=...` in one line, and `cat $X/file` adjudicated after it: the value need not be
/// written into the command, because the session's shell holds it and said so.
#[test]
fn an_export_in_one_line_resolves_a_command_adjudicated_after_it() {
    let Some(shell) = shell() else {
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
    // The shell's own answer, not a value this test remembered.
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
    // The path is placed where the shell said it was: that is the point of reading the answer
    // rather than assuming where a session writes.
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

/// A value the shell stops holding stops resolving: `unset` is a fact the next trailer reports
/// by omission, and a resolver that remembered the first report would place a directory that is
/// no longer in the environment.
#[test]
fn an_unset_on_a_later_line_stops_resolving() {
    let Some(shell) = shell() else {
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

/// A credential the shell holds reaches the card masked and never the reading: the same chain,
/// with the one value that must not be written down.
#[test]
fn a_credential_the_shell_reports_is_masked_and_not_spliced() {
    let Some(shell) = shell() else {
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

/// The fixtures pin the shell, which means something only if `Pinned` is a different value from
/// the default.
#[test]
fn the_fixture_declares_a_pinned_shell() {
    assert_ne!(
        ShellTrust::Pinned { how: "x".into() }.as_str(),
        ShellTrust::default().as_str()
    );
}
