//! The session's own checkout, under every name a placement gives it.
//!
//! A host with `stable_paths` mounts a session's checkout at one fixed name and keeps it somewhere
//! else on that machine, so one tree has two spellings: `/run/letibot/work/...` for the session's
//! own commands and `/letibot-remote/<repo>/<short>/...` on the disk. Both are the session's work,
//! and neither is a widening - a path outside both still asks.

use super::*;

/// The far side's own name for the checkout. Nobody's box holds it, like `/w`: the screen compares
/// spellings, so a fixture that named this box's checkout would measure the box.
const REAL: &str = "/letibot-remote/letibot/1a2b3c4d";

/// What the daemon hands the gate for such a session: the fixed name as the workspace (the backend
/// says so), and the real one as the placement's other name.
fn stable() -> crate::intent::Surroundings {
    let mut env = pinned();
    env.workspace = Some(crate::remote::STABLE_WORK.to_string());
    env
}

fn gate(mode: crate::mode::Mode) -> AdjudicatedGate {
    AdjudicatedGate::new(Box::new(NoAdjudicator))
        .with_surroundings(stable())
        .with_owned_roots(vec![REAL.to_string()])
        .with_mode(mode)
}

/// An `edit` as the tool hands it to the gate, run from the session's own working directory.
fn edit<'a>(args: &'a Value) -> GateCall<'a> {
    GateCall {
        name: "edit",
        access: Access::Write,
        args,
        turn_id: "t1",
        call_id: "c1",
        workspace: crate::remote::STABLE_WORK,
        target_exists: Some(true),
        scripts: &[],
        preview: &[],
    }
}

/// A write's arguments, which is all the gate reads for one.
fn writing(path: &str) -> Value {
    json!({"path": path, "old_string": "a", "new_string": "b"})
}

/// The spelling the session's own tools use is the tree, so an edit there is the session doing the
/// job it was opened for: no card and no model call.
#[test]
fn a_write_to_the_fixed_name_is_the_sessions_own_work() {
    for path in [
        "/run/letibot/work/TODO.md",
        "/run/letibot/work/crates/tokencore/src/store.rs",
        "/run/letibot/work/crates/tui/messages/en.toml",
    ] {
        let args = writing(path);
        let mut g = gate(crate::mode::Mode::AUTO);
        let d = g.admit(&edit(&args));
        assert!(
            matches!(d, GateDecision::Admit),
            "{path} is the session's own checkout: {d:?}"
        );
        assert_eq!(g.log[0].decision.by, "gate:owned", "{path}");
    }
}

/// And so is the checkout's name on the machine: a model reads the real path out of a note as
/// readily as the fixed one, and both are one tree.
#[test]
fn a_write_to_the_placements_real_name_is_the_same_tree() {
    let path = format!("{REAL}/crates/tools/src/intent.rs");
    let args = writing(&path);
    let mut g = gate(crate::mode::Mode::AUTO);
    let d = g.admit(&edit(&args));
    assert!(matches!(d, GateDecision::Admit), "{path}: {d:?}");
    assert_eq!(g.log[0].decision.by, "gate:owned");
}

/// The screen is about THIS tree and not about paths that look like it: a sibling checkout on the
/// same host is another session's, and a write there is not the session's own work.
#[test]
fn a_write_outside_both_names_still_asks() {
    for path in [
        "/letibot-remote/letibot/ffffffff/TODO.md",
        "/letibot-remote/other/1a2b3c4d/TODO.md",
        "/run/letibot/work-other/TODO.md",
        "/etc/letibot.conf",
    ] {
        let args = writing(path);
        let mut g = gate(crate::mode::Mode::AUTO);
        let d = g.admit(&edit(&args));
        assert!(
            !matches!(d, GateDecision::Admit),
            "{path} is not the session's: {d:?}"
        );
    }
}

/// `writes allowed` admits a write without asking, and that holds for both of the tree's names - a
/// point whose whole promise is "a write needs no card" cannot ask about the session's own file.
#[test]
fn writes_allowed_admits_a_write_to_either_name() {
    for path in [
        "/run/letibot/work/crates/tools/src/intent.rs",
        "/letibot-remote/letibot/1a2b3c4d/crates/tools/src/intent.rs",
    ] {
        let args = writing(path);
        let mut g = gate(crate::mode::Mode::WRITES_ALLOWED);
        let d = g.admit(&edit(&args));
        assert!(matches!(d, GateDecision::Admit), "{path}: {d:?}");
    }
}

/// The machine is a fact of the CARD and never part of the path. The runtime used to hand the gate
/// `{workspace} (on {host})` as one string, which is what `path_is_inside` compares - so an
/// absolute argument in a remote session read as outside its own tree, and a relative one resolved
/// against a directory nobody has.
#[test]
fn the_card_names_the_machine_and_the_workspace_stays_a_path() {
    let args = writing("/run/letibot/work/TODO.md");
    let call = edit(&args);
    assert!(
        call.path_is_inside(),
        "the path is under the workspace the backend named"
    );
    let mut g =
        gate(crate::mode::Mode::WRITES_ALLOWED).with_host_label(Some("xmg over ssh".to_string()));
    let req = g.request_for(&call);
    assert!(
        req.boundary_facts
            .iter()
            .any(|f| f.contains("workspace: /run/letibot/work (on xmg over ssh)")),
        "the card has to name the machine: {:?}",
        req.boundary_facts
    );
}

/// A `bash` call runs in the session's own tree, so a command whose meaning nobody could read is
/// admitted as unread-owned: the worktree it runs in is one of the two names.
#[test]
fn a_bash_call_that_runs_in_the_tree_is_placed_there() {
    let args = json!({"command": "$(which cat) TODO.md"});
    let mut g = gate(crate::mode::Mode::AUTO);
    let call = GateCall {
        name: "bash",
        access: Access::Exec,
        args: &args,
        turn_id: "t1",
        call_id: "c1",
        workspace: crate::remote::STABLE_WORK,
        target_exists: None,
        scripts: &[],
        preview: &[],
    };
    let d = g.admit(&call);
    assert!(
        matches!(d, GateDecision::Admit),
        "the command runs in the session's own checkout: {d:?}"
    );
    assert_eq!(g.log[0].ownership.as_deref(), Some("unread_owned_cwd"));
}

/// And a `bash` call that RUNS in the tree is placed there: the checkout is what it reads, so a
/// command whose cwd is the session's own is the session's own work.
///
/// A command that NAMES the checkout's real path is a read of it, and layer A places that path as
/// the host's rather than the workspace's: nothing is admitted on that reading, and it is left as
/// the ask it was rather than widened here.
#[test]
fn a_bash_call_whose_cwd_is_the_tree_is_the_sessions_own_work() {
    for command in ["git status .", "head -1 TODO.md"] {
        let args = json!({"command": command});
        let mut g = gate(crate::mode::Mode::AUTO);
        let d = g.admit(&bash(&args));
        assert!(
            matches!(d, GateDecision::Admit),
            "{command} runs in the session's own checkout: {d:?}"
        );
        assert_eq!(g.log[0].decision.by, "gate:owned", "{command}");
    }
}
