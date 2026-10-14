//! Scratch dirs, transcript rows, fleet URLs, doors, and monitor notices.

use super::*;

/// **The scratch is per-user and per-process, never the flat world-readable
/// `/tmp/letibot-scratch-<pid>`** — the operator's ask, 2026-10-05: *"job output
/// should go to your scratch directory."* The path is checked by SHAPE rather than
/// by resolving `$XDG_RUNTIME_DIR`, which a test must not mutate process-wide: the
/// old spelling is refused outright, and the directory component the process's
/// scratch sits in is the per-user one either environment names.
#[test]
fn the_scratch_dir_is_per_user_and_never_the_flat_tmp_spelling() {
    let s = super::scratch_dir();
    let said = s.display().to_string();
    assert!(
        !said.contains("letibot-scratch"),
        "the old world-readable spelling: {said}"
    );
    assert!(
        s.file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("scratch-")),
        "the last component is the process's own: {said}"
    );
    // Under the runtime dir when there is one — both components of it.
    if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
        assert!(
            s.starts_with(std::path::Path::new(&rt).join("letibot")),
            "the runtime dir's letibot, where the socket already lives: {said}"
        );
    }
}

/// **The depth cap refuses by NAME, and at the right boundary** (R58).
///
/// `task` is seated on `m2_coder` at every depth, so the seat table is not where the
/// limit lives — this function is. `depth == max` is the `task` call that would open
/// `max + 1` and is refused; `depth == max - 1` is the last one allowed.
#[test]
fn the_depth_cap_refuses_the_spawn_one_past_it_by_name() {
    // A root (0) with the ruled default (3) may spawn, and so may levels 1 and 2.
    assert!(super::subagent_depth_refusal(0, 3).is_none());
    assert!(super::subagent_depth_refusal(1, 3).is_none());
    assert!(super::subagent_depth_refusal(2, 3).is_none());
    // Depth 3 is the last level; `task` there would open depth 4 and is refused.
    let why = super::subagent_depth_refusal(3, 3).expect("refused at the cap");
    assert!(
        why.contains("depth 3"),
        "the refusal names where it is: {why}"
    );
    assert!(
        why.contains("depth 4"),
        "and the level it would have opened: {why}"
    );
    assert!(
        why.contains("--max-subagent-depth"),
        "and the knob that moves it — a refusal that does not name the knob is how a \
             model manufactures the workaround this seat already paid for once: {why}"
    );
    assert!(
        why.contains("Nothing was started"),
        "a refusal is not a maybe: {why}"
    );
    // A cap of 0 refuses even a root — *no nesting*, read honestly rather than as
    // *unlimited*.
    assert!(super::subagent_depth_refusal(0, 0).is_some());
}

/// A port is not optional and the refusal says so, because the alternative is
/// guessing 8080 — which is the port the three LOCAL units share, so the guess
/// would land on this box while the operator was naming another one.
#[test]
fn a_fleet_url_without_a_port_is_refused_by_name() {
    let e = super::parse_local_url("http://192.168.1.78").unwrap_err();
    assert!(e.contains("no port"), "{e}");
    assert!(super::parse_local_url("http://").is_err());
}

/// The GGUF's file name is the comparison, so two boxes holding one model in
/// different directories agree, and a split file's shard count still has to match.
#[test]
fn the_vocab_comparison_is_the_file_name_not_the_path() {
    use std::path::Path;
    assert_eq!(
        super::vocab_basename(Path::new("/home/dead/models/Qwen3.8-27B-UD-Q6_K_XL.gguf")),
        super::vocab_basename(Path::new("/srv/weights/Qwen3.8-27B-UD-Q6_K_XL.gguf")),
        "one model in two places is one model"
    );
    assert_ne!(
        super::vocab_basename(Path::new("/m/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf")),
        super::vocab_basename(Path::new("/m/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00008.gguf")),
        "a different shard count is a different file"
    );
}

#[test]
fn the_shell_is_declared_pinned_only_where_an_exec_backend_exists() {
    // R10's wiring. The pin claim rides on the host spawn's env hygiene —
    // `a_bash_env_planted_in_the_parent_never_reaches_the_child` in
    // `letibot-tools` holds that — so a seat with an exec backend declares
    // it, and a seat without one leaves `Unknown`, under which a bare name
    // is `not_run`: the fail-closed direction.
    let mut cfg = Config::for_this_box(std::env::temp_dir());
    cfg.seat = Seat::Runner;
    match surroundings_for(&cfg).shell {
        letibot_tools::ShellTrust::Pinned { how } => {
            assert!(how.contains("cleared before its own pairs"), "{how}");
        }
        other => panic!("a runner seat must declare the pin, got {other:?}"),
    }
    cfg.seat = Seat::Orchestrator;
    assert_eq!(
        surroundings_for(&cfg).shell,
        letibot_tools::ShellTrust::Unknown
    );
}

/// **R18's daemon half: layer A is given the configured workspace.**
///
/// The wrong fact the operator's four cards showed came from the *backend's root*
/// (`/` for an unconfined seat), not from here — `surroundings_for` has always used
/// `cfg.workspace`. This pins it, because two halves of one program answering *where
/// is this session* differently is exactly the shape of that defect, and nothing in
/// either half would have noticed.
#[test]
fn the_gate_is_told_the_configured_workspace_and_not_a_root() {
    let mut cfg = Config::for_this_box("/home/dead/Projects/letibot");
    assert_eq!(
        surroundings_for(&cfg).workspace.as_deref(),
        Some("/home/dead/Projects/letibot")
    );
    // …and it follows `--workspace` rather than a constant, which is the property
    // that makes the configured value the one that reaches the classifier.
    cfg.workspace = std::path::PathBuf::from("/tmp/somewhere-else");
    assert_eq!(
        surroundings_for(&cfg).workspace.as_deref(),
        Some("/tmp/somewhere-else")
    );
}

/// **The stop must not blame the model for working.**
///
/// The sentence this replaced was *"the model called tools 12 times without
/// answering"*. It was answering; it had not finished, and an operator who reads
/// that learns to distrust the model when the harness was at fault — the same
/// defect as `grep` reporting absence when it had opened no files.
#[test]
fn neither_stop_accuses_the_model_of_not_answering() {
    let backstop = HarnessError::LoopBound { rounds: 200 }.to_string();
    assert!(
        !backstop.contains("without answering"),
        "the accusation is the defect: {backstop}"
    );
    assert!(
        backstop.contains("--max-tool-rounds"),
        "errors carry the fix: {backstop}"
    );

    // The progress stop says what it SAW. Built here the way the loop builds it,
    // so the wiring and the sentence cannot drift apart.
    let mut d = crate::progress::ProgressDetector::new(2);
    for _ in 0..3 {
        d.observe(
            &ToolCall {
                id: "c0".into(),
                name: "read".into(),
                arguments: r#"{"path":"src/main.rs"}"#.into(),
            },
            &letibot_transcript::ToolOutcome::Ok,
            "the same body",
        );
        d.end_round();
    }
    assert!(d.exhausted());
    let e = HarnessError::NoProgress {
        evidence: d.evidence(9, 200),
    }
    .to_string();
    assert!(e.contains("src/main.rs"), "it names the evidence: {e}");
    assert!(e.contains("0 of 2"), "with its denominator: {e}");
    assert!(!e.contains("without answering"), "{e}");
}

/// **The repository's landing tree is in its sessions' confined view, read-only.** The firecode
/// session, 2026-10-11: *"`.git` is on the never-write list — I can't inspect the landing
/// worktree, and no grant reaches it."* The daemon adds the grant itself, beside `--grant-ro`'s,
/// for the repository the workspace is — and only that one.
#[test]
fn a_confined_session_reads_its_repositorys_landing_tree_and_cannot_write_it() {
    use letibot_tools::exec::confine::Grant;
    let dir = crate::mergequeue::serve::tests::TempDir::new("landing-grant");
    let repo = crate::mergequeue::serve::tests::repo(&dir.0);
    let root = dir.0.join("landing");
    let mut cfg = Config::for_this_box(&repo);
    cfg.grants_ro = vec![std::path::PathBuf::from("/opt/toolchain")];
    let grants = confined_grants(&cfg, Some(&root));
    let landing = crate::mergequeue::git::landing_dir_in(&root, &repo);
    assert!(
        landing.is_dir(),
        "made, empty, so the bind has something to bind"
    );
    assert!(
        matches!(&grants[0], Grant::ReadOnly { path, .. } if path == std::path::Path::new("/opt/toolchain")),
        "the operator's own grants first: {grants:?}"
    );
    assert!(
        grants
            .iter()
            .any(|g| matches!(g, Grant::ReadOnly { path, .. } if *path == landing)),
        "{grants:?}"
    );
    assert!(
        !grants.iter().any(|g| matches!(g, Grant::ReadWrite { .. })),
        "never writable: {grants:?}"
    );
    // A workspace that is no repository gets no landing grant.
    let plain = crate::mergequeue::serve::tests::TempDir::new("landing-grant-none");
    let cfg = Config::for_this_box(&plain.0);
    assert!(confined_grants(&cfg, Some(&root)).is_empty());
}

/// **A VM holds one repository** — the operator: *"VMs are per repo anyway — at least must be"*.
/// In a folder that is one project of several repositories a VM child with nothing named is
/// refused naming them; a named one is copied; a name that is not a repository is refused; and a
/// child on the host, or of a session of one repository, is left as it was.
#[test]
fn a_vm_child_of_a_folder_of_repositories_copies_the_one_it_names() {
    use crate::harness::vm_copy_for;
    let root = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-vm-copy-{}-{}",
        std::process::id(),
        letibot_sessionlog::registry::now_ms()
    ));
    let org = root.join("stroppy-io");
    for (name, url) in [
        ("stroppy", "git@github.com:stroppy-io/stroppy.git"),
        (
            "website",
            "https://github.com/stroppy-io/stroppy-io.github.io",
        ),
    ] {
        let d = org.join(name);
        std::fs::create_dir_all(&d).unwrap();
        crate::mergequeue::git::git(&d, &["init", "-q"]).unwrap();
        crate::mergequeue::git::git(&d, &["remote", "add", "origin", url]).unwrap();
    }
    let refused = vm_copy_for(true, false, None, &org).unwrap_err();
    assert!(
        refused.contains("`stroppy`") && refused.contains("`website`") && refused.contains("repo"),
        "{refused}"
    );
    let named = vm_copy_for(true, false, Some(std::path::Path::new("stroppy")), &org)
        .unwrap()
        .unwrap();
    assert!(named.ends_with("stroppy"), "{}", named.display());
    assert!(vm_copy_for(true, false, Some(std::path::Path::new("nope")), &org).is_err());
    assert_eq!(
        vm_copy_for(false, false, None, &org).unwrap(),
        None,
        "the host"
    );
    assert_eq!(
        vm_copy_for(true, true, None, &org).unwrap(),
        None,
        "a task_start child copies its own worktree"
    );
    assert_eq!(
        vm_copy_for(true, false, None, &org.join("stroppy")).unwrap(),
        None,
        "a session of one repository copies it, as before"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// **What a held answer waits on: this session's children still on their task** — not one that
/// has answered and sits idle (it would hold for ever), not one whose thread has ended, and not
/// a child of another session.
#[test]
fn the_hold_waits_on_children_still_on_their_task_and_no_others() {
    use crate::harness::{TaskSlot, unsettled_children};
    use letibot_tools::builtins::task::TaskStatus;
    let working = Arc::new(TaskSlot::new("me"));
    let answered = Arc::new(TaskSlot::new("me"));
    answered.settle(TaskStatus::Done {
        answer: "done".into(),
        child: None,
    });
    let gone = Arc::new(TaskSlot::new("me"));
    gone.mark_exited();
    let theirs = Arc::new(TaskSlot::new("someone-else"));
    let slots = vec![
        ("a".to_string(), working),
        ("b".to_string(), answered),
        ("c".to_string(), gone),
        ("d".to_string(), theirs),
    ];
    assert_eq!(unsettled_children(&slots, "me"), vec!["a".to_string()]);
}
