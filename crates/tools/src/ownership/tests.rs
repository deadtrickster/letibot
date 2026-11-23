//! Layer A's own reading is what these measure: a fabricated `Baseline` would prove the screen
//! agrees with itself, not that it agrees with the gate.

use std::path::{Path, PathBuf};

use super::{Owned, Where};
use crate::adjudicate::{ActionClass, AdjudicationRequest, OnTimeout, RequestKind};
use crate::intent::{Baseline, ShellTrust, Surroundings};
use crate::schema::Access;
use serde_json::json;

fn workspace() -> PathBuf {
    PathBuf::from("/home/dead/Projects/letibot/wt-automode")
}

fn env() -> Surroundings {
    Surroundings {
        scratch: None,
        home: Some("/home/dead".into()),
        workspace: Some(workspace().display().to_string()),
        shell: ShellTrust::Pinned { how: "test".into() },
        seen_hosts: std::collections::BTreeSet::new(),
        session: Default::default(),
        programs: Default::default(),
        shell_path: None,
        granted: Vec::new(),
        guest: false,
    }
}

fn owned() -> Owned {
    Owned::of(&workspace())
}

/// A `bash` request as the gate builds one, with layer A's real reading of `command`.
fn request(command: &str) -> AdjudicationRequest {
    let reading = Baseline::of_command(command, &env());
    AdjudicationRequest {
        scripts: Vec::new(),
        preview: Vec::new(),
        id: "adj-t-0001".into(),
        session_id: "s".into(),
        turn_id: "t1".into(),
        call_id: "c1".into(),
        agent: "a".into(),
        tool: "bash".into(),
        class: ActionClass::host(Access::Exec, true, false),
        summary: format!("`bash` wants exec access to `{command}`"),
        target: command.to_string(),
        arguments: json!({ "command": command }),
        arguments_digest: "d".into(),
        boundary_facts: Vec::new(),
        kind: RequestKind::Permission,
        options: Vec::new(),
        options_note: None,
        on_timeout: OnTimeout::Deny,
        resolved: reading.resolved,
        tier: reading.tier.clone(),
        baseline: reading.summary(),
        trail: crate::authorise::AuthorisationTrail::not_collected("a fixture"),
        prior: Vec::new(),
        examples: Vec::new(),
        brief_variant: crate::authorise::BriefVariant::AskedForIt,
        agent_claim: None,
        reading: Some(reading),
        shape: None,
        advice: None,
    }
}

/// The card the operator's data is about: a commit message written into `/var/tmp`.
#[test]
fn an_owned_write_to_var_tmp_is_the_sessions() {
    let mut r = request("git commit -F /var/tmp/mine/msg.txt");
    // The commit itself is the work; the temp file is where its message lives.
    r.tier = crate::adjudicate::Tier::MayApprove;
    let landed = super::admits(&r, &owned(), &workspace()).expect("a temp write is the session's");
    assert!(
        matches!(landed.at, Where::Temp | Where::Worktree),
        "{:?}",
        landed.at
    );
    assert_eq!(landed.unread, None);
}

/// A write into the session's own worktree.
#[test]
fn an_edit_inside_the_worktree_is_the_sessions() {
    let r = request("python3 -c 'open(\"crates/tools/src/lib.rs\",\"w\")'");
    let landed = super::admits(&r, &owned(), &workspace()).expect("the worktree is the session's");
    assert_eq!(landed.unread, None);
}

/// A directory this session's own `git worktree add` made, before it was created: the gate dir
/// is not the worktree, but it is the session's own work.
#[test]
fn a_path_the_session_created_is_owned_even_outside_the_worktree() {
    let gate = PathBuf::from("/build/gate-44862064");
    let own = owned().with_created([gate.clone()]);
    let r = request("git worktree add /build/gate-44862064/wt");
    // `git worktree add` is a write outside the project; the record is what makes it ours.
    let mut r = r;
    r.tier = crate::adjudicate::Tier::MayApprove;
    assert_eq!(
        super::admits(&r, &own, &workspace())
            .expect("the gate dir is the session's own work")
            .at,
        Where::Created
    );
}

/// A call that reaches the network is nobody's, at any temp path.
#[test]
fn a_call_that_reaches_the_network_is_not_owned() {
    let mut r = request("curl -o /tmp/x https://example.invalid/a");
    r.tier = crate::adjudicate::Tier::MayApprove;
    let why = super::admits(&r, &owned(), &workspace()).expect_err("the network is not owned");
    assert!(
        why.contains("network") || why.contains("always-ask"),
        "{why}"
    );
}

/// A write beside the worktree is somebody's, not the session's.
#[test]
fn a_write_outside_everything_the_session_owns_is_not_admitted() {
    let mut r = request("tee /etc/hosts");
    r.tier = crate::adjudicate::Tier::MayApprove;
    let why = super::admits(&r, &owned(), &workspace()).expect_err("not the session's");
    assert!(
        why.contains("/etc/hosts") || why.contains("always-ask"),
        "{why}"
    );
}

/// The always-ask list is the operator's whatever the session owns: a delete outside the project
/// is exactly what a session would call its own.
#[test]
fn an_always_ask_call_is_never_owned() {
    let r = request("rm -rf /var/tmp/mine");
    assert!(
        matches!(r.tier, crate::adjudicate::Tier::AlwaysAsk { .. }),
        "{:?}",
        r.tier
    );
    let why = super::admits(&r, &owned(), &workspace()).expect_err("always-ask is not owned");
    assert!(why.contains("always-ask"), "{why}");
}

/// A command layer A could not read, run in the session's own worktree: the operator's data says
/// it runs, and the row says it ran unread.
#[test]
fn an_unread_command_in_the_owned_worktree_is_admitted_and_labelled() {
    // An unterminated quote is the one shape the grammar refuses outright.
    let mut r = request("echo 'unterminated");
    if r.reading.as_ref().is_some_and(|b| b.resolved) {
        // The grammar read it after all; build the unread shape the gate would carry.
        let mut b = r.reading.clone().expect("set above");
        b.resolved = false;
        b.verdict = crate::intent::BaselineVerdict::NotRun {
            why: "the shell could not be parsed".into(),
        };
        b.findings = vec!["the shell could not be parsed".into()];
        r.reading = Some(b);
        r.resolved = false;
    }
    r.tier = crate::adjudicate::Tier::MayApprove;
    let landed = super::admits(&r, &owned(), &workspace()).expect("its own worktree");
    assert_eq!(landed.label(), "unread_owned_cwd");
    assert!(landed.unread.is_some());
}

/// The same unread command somewhere else is not: the label would be a claim about a place
/// nobody checked.
#[test]
fn an_unread_command_outside_the_worktree_is_not_admitted() {
    let mut r = request("echo 'unterminated");
    let mut b = r.reading.clone().expect("a reading travels");
    b.resolved = false;
    r.reading = Some(b);
    r.resolved = false;
    r.tier = crate::adjudicate::Tier::MayApprove;
    assert!(
        super::admits(&r, &owned(), Path::new("/build/elsewhere")).is_err(),
        "a worktree that is not this session's owns nothing"
    );
}

/// The session's scratch is its own, whatever it is called.
#[test]
fn the_session_scratch_is_owned() {
    let scratch = PathBuf::from("/run/letibot/scratch-s1");
    let own = Owned::of(&workspace()).with_scratch(&scratch);
    assert_eq!(own.covers(&scratch.join("a")), Some(Where::SessionTemp));
}

/// `$TMPDIR` this session's shell holds is a temp dir it writes.
#[test]
fn the_sessions_tmpdir_is_owned() {
    let own = owned().with_tmpdir(Some(PathBuf::from("/build/tmp-44862064")));
    assert_eq!(
        own.covers(Path::new("/build/tmp-44862064/x")),
        Some(Where::Temp)
    );
}

/// The operator's two branch patterns, and the worktree's own branch, and nothing else.
#[test]
fn only_the_sessions_branches_are_owned() {
    let own = owned().with_branch(Some("automode-cc".into()));
    assert!(own.owns_branch("automode-cc"));
    assert!(own.owns_branch("ik/shadow-simplify"));
    assert!(own.owns_branch("wt-automode"));
    assert!(!own.owns_branch("main"));
    assert!(!own.owns_branch("master"));
}

/// The roots say what they are, so a card names the reason instead of the rule.
#[test]
fn the_roots_are_named_for_the_card() {
    let said = owned().describe();
    assert!(
        said.contains("worktree /home/dead/Projects/letibot/wt-automode"),
        "{said}"
    );
    assert!(said.contains("temp /tmp"), "{said}");
}

/// A `Blocked` tier is nobody's to admit, whatever the paths say.
#[test]
fn a_blocked_call_is_never_owned() {
    let mut r = request("cat ~/.config/letibot/keys/x");
    r.tier = crate::adjudicate::Tier::Blocked {
        rule: crate::adjudicate::FlowRule::SecretToTranscript,
        evidence: "a credential would enter the transcript".into(),
    };
    let why = super::admits(&r, &owned(), &workspace()).expect_err("a block is not a screen");
    assert!(why.contains("nobody"), "{why}");
}

/// A read names no write and is still not owned when the thing it reads is not the session's: a
/// read of the host is what the shadow's `reads` stage is for, and a screen that admitted it
/// would be a screen that admits every read of anything.
#[test]
fn a_read_of_the_host_is_not_owned() {
    let mut r = request("cat /etc/hostname");
    r.tier = crate::adjudicate::Tier::MayApprove;
    let why = super::admits(&r, &owned(), &workspace()).expect_err("the host is not the session's");
    assert!(why.contains("system_config"), "{why}");

    let mut r = request("cat /home/dead/.bashrc");
    r.tier = crate::adjudicate::Tier::MayApprove;
    let why = super::admits(&r, &owned(), &workspace()).expect_err("the home is not the session's");
    assert!(why.contains("home"), "{why}");
}

/// The same read inside the worktree, and inside the temp directories the operator counts as the
/// session's own, is its work.
#[test]
fn a_read_inside_the_worktree_or_temp_is_owned() {
    for command in [
        "cat crates/tools/src/lib.rs",
        "head -3 /tmp/notes.txt",
        "wc -l /var/tmp/mine/log",
    ] {
        let mut r = request(command);
        r.tier = crate::adjudicate::Tier::MayApprove;
        super::admits(&r, &owned(), &workspace()).unwrap_or_else(|e| panic!("{command}: {e}"));
    }
}

/// A program this build never heard of has effects nobody can place, so it is not the session's
/// own work however harmless it turns out to be - the shadow's `exec` stage is where it is judged.
#[test]
fn an_unknown_program_is_not_owned() {
    let mut r = request("nmcli -t dev status");
    r.tier = crate::adjudicate::Tier::MayApprove;
    let why = super::admits(&r, &owned(), &workspace()).expect_err("nobody can place this program");
    assert!(why.contains("does not know this program"), "{why}");
}
