//! What a session's environment resolves to at the resolver: what a command says wins over
//! what the session holds, a name the session holds is written out with its source, and a
//! credential is held and never written into the reading.

use std::path::PathBuf;

use super::env::{From, SessionEnv};
use super::*;
use crate::adjudicate::ActionClass;
use crate::schema::Access;

/// The surroundings most of these tests want: a pinned shell, this box's own view of the
/// filesystem, and a session seeded the way the daemon seeds one - `PATH` included, because
/// it is what the host puts on every child after `env_clear()`.
fn sur() -> Surroundings {
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    Surroundings {
        home: Some("/home/dead".into()),
        workspace: Some("/home/dead/Projects/letibot".into()),
        scratch: None,
        shell: ShellTrust::Pinned {
            how: "test fixture: the host pins PATH and clears the rest".into(),
        },
        seen_hosts: BTreeSet::new(),
        session: SessionEnv::seeded([("PATH".to_string(), path.clone())]),
        programs: ProgramLookup::Host,
        shell_path: Some(path),
    }
}

/// A session that holds one name besides the harness's own pairs, from the pairs every spawn
/// is given.
fn holding(name: &str, value: &str) -> Surroundings {
    let mut s = sur();
    s.session = SessionEnv::seeded([
        (
            "PATH".to_string(),
            s.shell_path
                .clone()
                .unwrap_or_else(|| "/usr/bin:/bin".into()),
        ),
        (name.to_string(), value.to_string()),
    ]);
    s
}

/// A scratch directory per test and per process, so no two tests share one.
fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("letibot-session-env-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("a scratch directory");
    d
}

/// A name the session holds is written out, and the card says where it came from.
/// The finding names both the value and its source: the harness's pairs and the shell's own
/// report are different claims, and the card must not blur them.
#[test]
fn a_name_the_session_holds_is_read_and_the_card_says_where_it_came_from() {
    let b = Baseline::of_command(
        "cd $p && ls -la",
        &holding("p", "/home/dead/Projects/letibot"),
    );
    assert!(b.resolved, "{:?} {:?}", b.verdict, b.findings);
    assert!(
        b.scoped
            .iter()
            .any(|s| s.target == "/home/dead/Projects/letibot")
            || b.command
                .as_ref()
                .is_some_and(|n| n.source.contains("/home/dead/Projects/letibot")),
        "the value was not placed: {:?}",
        b.command.as_ref().map(|n| n.source.clone())
    );
    let said = b.findings.join(" | ");
    assert!(
        said.contains("`$p` is `/home/dead/Projects/letibot`"),
        "{said}"
    );
    assert!(
        said.contains(From::Harness.as_phrase()),
        "the card does not say where the value came from: {said}"
    );
}

/// What the command says wins, including `unset` - and the second half is the one a careless
/// implementation gets wrong: `unset p; cat $p` must not fall through to a value the session
/// holds, because the shell would not have one either.
#[test]
fn what_the_command_says_wins_and_unset_does_not_fall_through() {
    let s = holding("p", "/home/dead/session");
    let set = Baseline::of_command("p=/home/dead/command; cat $p/x", &s);
    assert!(
        set.command
            .as_ref()
            .is_some_and(|n| n.source.contains("/home/dead/command")),
        "the command's own assignment lost: {:?}",
        set.command.as_ref().map(|n| n.source.clone())
    );
    let unset = Baseline::of_command("unset p; cat $p/x", &s);
    assert!(
        !unset.resolved,
        "`unset p` fell through to the session's value: {:?}",
        unset.command.as_ref().map(|n| n.source.clone())
    );
}

/// A dead shell's report dies with it: the values it reported are gone and the command that
/// needed one stays as unread as it was, while the pairs every spawn gets are untouched.
#[test]
fn a_dead_shell_resolves_nothing_it_reported() {
    let mut s = sur();
    s.session = SessionEnv::seeded([
        (
            "PATH".to_string(),
            s.shell_path
                .clone()
                .unwrap_or_else(|| "/usr/bin:/bin".into()),
        ),
        ("CARGO_TARGET_DIR".to_string(), "/w/target".to_string()),
    ]);
    s.session
        .observe("p=/home/dead/from-the-shell;")
        .expect("a clean report");
    assert!(Baseline::of_command("cat $p/x", &s).resolved);
    s.session.mark_dead();
    assert!(
        !Baseline::of_command("cat $p/x", &s).resolved,
        "a dead shell's value still resolved"
    );
    // And the harness's own pair still does: it is not the shell's claim.
    assert!(
        Baseline::of_command("cat $CARGO_TARGET_DIR/x", &s).resolved,
        "{:?}",
        Baseline::of_command("cat $CARGO_TARGET_DIR/x", &s).findings
    );
}

/// A credential is held and never written into the reading.
/// The spliced text is what `shape_of` renders into the approval key every row stores and
/// what the oracle is shown, so a token written there is a token in the store.
#[test]
fn a_credential_is_named_on_the_card_and_never_written_into_the_reading() {
    let token = "ghp_0123456789abcdef4c";
    let s = holding("GITHUB_TOKEN", token);
    let b = Baseline::of_command(
        "curl -H \"Authorization: Bearer $GITHUB_TOKEN\" https://x.io/",
        &s,
    );
    let said = b.findings.join(" | ");
    assert!(
        said.contains("ghp_…4c"),
        "the card must name it, masked: {said}"
    );
    assert!(
        !said.contains(token),
        "the value itself reached the card: {said}"
    );
    let source = b
        .command
        .as_ref()
        .map(|n| n.source.clone())
        .unwrap_or_default();
    assert!(
        !source.contains(token),
        "the credential was written into the reading, which is what a shape and a stored row \
         keep: {source}"
    );
    // And the expansion is still unresolved, which is the fail-closed direction: nothing
    // claims to know what this command means.
    assert!(
        !b.resolved,
        "an unresolved credential made the command look read"
    );
}

/// A `PATH` this session set names the file each stage will run: `export PATH=<dir>:$PATH;`
/// followed by `cargo build`, where `<dir>/cargo` exists, is not a `cargo build` with the
/// box's `cargo` - the stage becomes the file, and the shape becomes that file's.
#[test]
fn a_path_the_command_sets_names_the_file_a_bare_stage_will_run() {
    let dir = tmp("names-the-file");
    let stub = dir.join("cargo");
    crate::testing::write_stub(&stub, "#!/bin/sh\n[ $# -eq 0 ] && exit 0\nexit 0\n");
    let s = sur();
    let b = Baseline::of_command(
        &format!("export PATH={}:$PATH && cargo build", dir.display()),
        &s,
    );
    assert!(b.resolved, "{:?}", b.findings);
    let file = stub.display().to_string();
    let source = b
        .command
        .as_ref()
        .map(|n| n.source.clone())
        .unwrap_or_default();
    assert!(
        !source.contains("$PATH"),
        "the PATH was not resolved at all: {source}"
    );
    let said = b.findings.join(" | ");
    assert!(
        said.contains("found along a `PATH` this command set"),
        "the card does not say which PATH put it there: {said}"
    );
    // Three names, and all three are the FILE: the stage's program word (what the shape and
    // the card read), the approval key `grant_program`, and the shape itself.
    let stage = b
        .command
        .as_ref()
        .and_then(|n| n.stages.last())
        .and_then(|s| s.program.text())
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        stage, file,
        "the stage's program is not the file it will run"
    );
    assert_eq!(
        b.grant_program.as_deref(),
        Some(file.as_str()),
        "the approval key still says `cargo`"
    );
    let shape = crate::adjudicate::shape_of(&b).expect("a parse");
    assert!(
        shape.contains(&file),
        "the shape still says `cargo`: {shape}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A grant given for `cargo` does not settle a stage that runs another file: `Grant::covers`
/// compares the program the reading produced, so a different file is a different program.
#[test]
fn a_grant_for_the_bare_name_does_not_cover_a_stage_that_runs_another_binary() {
    let dir = tmp("grant");
    let stub = dir.join("cargo");
    crate::testing::write_stub(&stub, "#!/bin/sh\n[ $# -eq 0 ] && exit 0\nexit 0\n");
    let s = sur();
    // The grant a person would have taken over `cargo build`: the program, its class and the
    // intents the reading produced.
    let class = ActionClass::host(Access::Exec, true, false);
    let ordinary = Baseline::of_command("cargo build", &s);
    let granted = crate::grant::Grant {
        written: crate::grant::Written::Enumerated {
            patterns: vec![format!(
                "{} ({class})",
                ordinary.grant_program.clone().unwrap_or_default()
            )],
        },
        coverage: vec![crate::grant::Coverage {
            program: ordinary.grant_program.clone().expect("a program"),
            class,
            intents: ordinary.intents.clone(),
        }],
        why: "test: the operator answered this once".into(),
    };
    let covers = |b: &Baseline| {
        granted.covers(
            &b.tier,
            b.resolved,
            b.grant_program.as_deref().unwrap_or_default(),
            class,
            &b.intents,
        )
    };
    // It covers the bare command...
    assert!(
        covers(&ordinary).is_ok(),
        "the fixture grant covers nothing"
    );
    // ...and NOT the same command once a PATH the command set moves the binary.
    let moved = Baseline::of_command(
        &format!("export PATH={}:$PATH && cargo build", dir.display()),
        &s,
    );
    assert!(moved.resolved, "{:?}", moved.findings);
    assert_ne!(
        moved.grant_program, ordinary.grant_program,
        "the moved stage is keyed on the same program as the bare name, so the grant settles it"
    );
    assert!(
        covers(&moved).is_err(),
        "a grant for `cargo` covered a stage that runs {:?}",
        moved.grant_program
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The same file is not a rename: a `PATH` that finds the same binary changes nothing - no
/// new shape, no re-asking, no churn in the approval key.
#[test]
fn a_path_that_finds_the_same_file_changes_nothing() {
    let s = sur();
    let path = s.shell_path.clone().expect("the fixture's PATH");
    let bare = Baseline::of_command("cargo build", &s);
    let same = Baseline::of_command(&format!("export PATH=$PATH:{path} && cargo build"), &s);
    assert!(same.resolved, "{:?}", same.findings);
    assert_eq!(
        same.grant_program, bare.grant_program,
        "the same binary was renamed: {same:?}"
    );
    assert!(
        !same.findings.join(" | ").contains("found along a `PATH`"),
        "a same-file PATH put a note on the card: {:?}",
        same.findings
    );
}

/// Where the file cannot be established, the `PATH` is not resolved at all, which is what
/// keeps the feature monotone: a command that needed a `PATH` read was unread before and it
/// stays unread. `ProgramLookup::None` is a remote placement, a VM, or a corpus replay.
#[test]
fn a_path_this_gate_cannot_follow_is_left_unread_rather_than_placed() {
    let dir = tmp("cannot-follow");
    let stub = dir.join("cargo");
    crate::testing::write_stub(&stub, "#!/bin/sh\n[ $# -eq 0 ] && exit 0\nexit 0\n");
    let mut s = sur();
    s.programs = ProgramLookup::None;
    let b = Baseline::of_command(
        &format!("export PATH={}:$PATH && cargo build", dir.display()),
        &s,
    );
    assert!(
        !b.resolved,
        "an unplaceable PATH made the command look read"
    );
    let said = b.findings.join(" | ");
    assert!(
        said.contains("is left unread rather than placed"),
        "the card does not say why the PATH stayed unread: {said}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `PATH` with no lookable stage after it still resolves: `export PATH=<dir>:$PATH` on its
/// own is the commonest shape there is, and it must not need a filesystem to be read.
#[test]
fn a_path_with_no_bare_stage_after_it_resolves_without_a_lookup() {
    let mut s = sur();
    s.programs = ProgramLookup::None;
    let b = Baseline::of_command("export PATH=\"/home/dead/.local/bin:$PATH\"", &s);
    assert!(b.resolved, "{:?}", b.findings);
    assert!(
        b.command
            .as_ref()
            .is_some_and(|n| n.source.contains("/home/dead/.local/bin:")),
        "{:?}",
        b.command.as_ref().map(|n| n.source.clone())
    );
}

/// A shell builtin is not looked up on `PATH`: `cd`, `export`, `echo` and `test` are answered
/// by the shell, so a resolved `PATH` must not be judged unknowable because there is no file
/// called `export`.
#[test]
fn a_builtin_after_a_resolved_path_is_not_a_file_to_look_for() {
    let mut s = sur();
    // No lookup at all: if a builtin were treated as a program on PATH, this would fail.
    s.programs = ProgramLookup::None;
    for cmd in [
        "export PATH=$HOME/.local/bin:$PATH && cd /tmp && echo done",
        "export PATH=/tmp/x:$PATH && test -d /tmp",
    ] {
        let b = Baseline::of_command(cmd, &s);
        assert!(b.resolved, "{cmd}: {:?}", b.findings);
    }
}
