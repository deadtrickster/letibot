//! Subagents: spawning, worktrees, titles, stopping, and what a child hands back.

use super::*;

/// A child that ran out of room says so, and not in token numbers: the wall's own Display is a
/// page about ledgers and compaction that the parent can neither read nor act on.
#[test]
fn a_child_that_hit_the_wall_hands_its_parent_a_move_and_not_its_numbers() {
    let wall = HarnessError::ContextWall {
        rounds: 41,
        resident: 980_112,
        window: 1_000_000,
        unit: "tokens".into(),
        ledger: (980_112, 1_000_000),
    };
    let said = said_failure(&wall);
    assert_eq!(
        said, "ran out of room; resume it with task_message",
        "{said}"
    );
    assert!(!said.contains("980"), "no token number: {said}");
    assert!(
        !said.contains("context wall"),
        "and not the words the check forbids: {said}"
    );

    // Anything else is still its own sentence: the wall is the one that cannot be read.
    let refused = HarnessError::Setup("the store refused the row".into());
    assert_eq!(said_failure(&refused), "the store refused the row");
}

/// A real worktree is arranged the way `task_start` arranges it: the branch is `agent/<slug>`, cut from the base, and
/// the main tree is untouched. It drives the same `git_in` door in a throwaway repo, so the branch name and the main
/// tree's cleanliness are facts on disk rather than a claim in the answer.
#[test]
fn a_worktree_is_arranged_with_its_branch_and_the_main_tree_untouched() {
    // A throwaway repo, the same shape `detect.rs`'s tests use.
    let root = std::env::temp_dir().join(format!(
        "letibot-task-start-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        }
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir");
    let git = |args: &[&str]| {
        let out = letibot_tools::testing::git(&root)
            .args(args)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    letibot_tools::testing::repo_config(&root);
    std::fs::write(root.join("a.txt"), "one\ntwo\n").expect("write");
    git(&["add", "-A"]);
    git(&["commit", "-qm", "first"]);
    let base_sha =
        super::git_in(root.to_str().unwrap(), &["rev-parse", "HEAD"]).expect("the base SHA");

    // The worktree, arranged the way `start_worktree` arranges it.
    let slug = "task-start";
    let path = letibot_tools::builtins::task::worktree_path(root.to_str().unwrap(), slug);
    let branch = letibot_tools::builtins::task::worktree_branch(slug);
    super::git_in(
        root.to_str().unwrap(),
        &["worktree", "add", &path, "-b", &branch, "HEAD"],
    )
    .expect("the worktree is created");

    // The branch is `agent/<slug>`, cut from the base.
    let wt_branch = super::git_in(&path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .expect("the worktree's branch");
    assert_eq!(wt_branch, branch, "the branch is agent/<slug>");
    let wt_base = super::git_in(&path, &["rev-parse", "HEAD"]).expect("the worktree's HEAD");
    assert_eq!(wt_base, base_sha, "the branch is cut from the base");

    // The main tree is untouched: its branch is still `main`, its HEAD is still
    // the base, and its status is clean - the worktree did not move the main tree.
    let main_branch = super::git_in(
        root.to_str().unwrap(),
        &["rev-parse", "--abbrev-ref", "HEAD"],
    )
    .expect("the main tree's branch");
    assert_eq!(main_branch, "main", "the main tree's branch is untouched");
    let main_head = super::git_in(root.to_str().unwrap(), &["rev-parse", "HEAD"])
        .expect("the main tree's HEAD");
    assert_eq!(main_head, base_sha, "the main tree's HEAD is untouched");
    // The `.claude/` directory is where the worktree lives, and it is untracked in
    // the main tree - that is expected, it is the worktree's home. The main tree's
    // *tracked* files are what must be untouched, so the status excludes it.
    let status = super::git_in(
        root.to_str().unwrap(),
        &["status", "--porcelain", "--", ".", ":(exclude).claude"],
    )
    .expect("the main tree's status");
    assert!(
        status.is_empty(),
        "the main tree's tracked files are untouched: {status}"
    );

    // And the worktree is a fact on disk: `git worktree list` shows it.
    let list =
        super::git_in(root.to_str().unwrap(), &["worktree", "list"]).expect("the worktree list");
    assert!(list.contains(&path), "the worktree is listed: {list}");

    // Clean up: remove the worktree and the repo.
    let _ = super::git_in(
        root.to_str().unwrap(),
        &["worktree", "remove", "--force", &path],
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The subagent's title is not the session's name, and the two have different bounds: a session name is an ADDRESS
/// somebody types, held to five words and 48 characters, while a subagent's title is a ROW whose cap is the UI's.
/// Reusing the address rule cut every subagent line to a name's length, so both are pinned here.
#[test]
fn a_subagent_title_is_a_row_and_a_session_name_is_an_address() {
    let long = "audit the session store and say which rows are never read by anything at \
                    all in the whole tree, and then propose a fix for each";
    // The ADDRESS rule: five words as a slug, 48 characters at the outside.
    assert_eq!(
        super::derive_title(long),
        "audit-the-session-store-and",
        "a name is five words, so it stays a name somebody can type"
    );
    assert_eq!(
        super::derive_title("  \n Fix: the parser, now!\nmore"),
        "fix-the-parser-now"
    );
    assert_eq!(super::derive_title("... ---"), "");
    let one_word = "x".repeat(200);
    assert_eq!(super::derive_title(&one_word).chars().count(), 48);
    // The DISPLAY rule: the whole first line, and the six-word rule does not apply.
    let title = super::subagent_title(long);
    assert_eq!(
        title, long,
        "a title long enough to matter must survive whole"
    );
    assert!(
        title.split_whitespace().count() > 6,
        "the naming rule must not reach a title: {title}"
    );
    // Only the first line - a row is one line - and only past the generous bound.
    assert_eq!(
        super::subagent_title("audit the store\nand the reader"),
        "audit the store"
    );
    let huge = "x".repeat(400);
    let cut = super::subagent_title(&huge);
    assert_eq!(cut.chars().count(), super::SUBAGENT_TITLE_MAX);
    assert!(cut.ends_with('…'), "{cut}");
}

/// The child's row says where its point came from, and says when it was clamped: the vocabulary the settings table
/// already uses, asserted rather than trusted, and the clamp's sentence asserted to survive INTO the row. What it does
/// not test is that the spawn reads the parent's live point, which needs a session with a model behind it.
#[test]
fn a_childs_row_says_where_its_point_came_from() {
    let inherited = super::child_mode_source(None);
    assert_eq!(inherited, "inherited from the parent session");

    let clamped = letibot_tools::mode::Mode::ALLOW_ALL.inherited_by(
        &[letibot_tools::mode::Prereq::ReachableAdjudicator],
        letibot_tools::mode::Seats {
            write: false,
            exec: true,
            network: false,
        },
        "runner",
    );
    let why = clamped.why().expect("a confinement is missing here");
    let said = super::child_mode_source(Some(why));
    assert!(said.starts_with(&inherited), "{said}");
    assert!(said.contains("clamped"), "{said}");
    assert!(said.contains("`runner`"), "the seat is named: {said}");
    assert!(said.contains("confinement"), "what was missing: {said}");
    assert!(said.contains("`writes allowed`"), "where it landed: {said}");
}

/// Three children minted at one instant get three ids: the id is the handle `task_result` collects by and it finds the
/// FIRST slot, so the other replies would exist, settle and be unreachable. The defect was a millisecond clock, so this
/// drives the same instant to a nanosecond, and both halves of the guard are exercised because either alone misses it.
#[test]
fn three_children_minted_at_one_instant_get_three_ids() {
    use letibot_sessionlog::registry::{Registry, SessionWiring};

    const NS: u128 = 1_700_000_000_000_000_000;
    let r = Registry::new();
    let mut handed: Vec<String> = Vec::new();
    let mint = |handed: &Vec<String>| {
        super::mint_sub_id_at(&r, "s-parent", NS, |id| handed.iter().any(|h| h == id))
    };

    let a = mint(&handed);
    handed.push(a.clone());
    let b = mint(&handed);
    handed.push(b.clone());
    let c = mint(&handed);
    assert_eq!(
        a,
        format!("s-parent-sub-{NS}"),
        "the first keeps the plain form"
    );
    assert_eq!(b, format!("s-parent-sub-{NS}-2"));
    assert_eq!(c, format!("s-parent-sub-{NS}-3"));
    assert_ne!(
        a, b,
        "two children, one id, is two answers nobody can collect"
    );
    assert_ne!(b, c);
    assert_ne!(a, c);

    // And the registry half: an id whose child is already ADOPTED is refused too, even
    // by a caller that believes nothing has been handed out.
    let adopted = super::mint_sub_id_at(&r, "s-parent", NS, |_| false);
    r.adopt(
        r.new_hub(adopted.clone()),
        "",
        SessionWiring::default(),
        None,
    )
    .expect("the first child adopts");
    let next = super::mint_sub_id_at(&r, "s-parent", NS, |_| false);
    assert_ne!(next, adopted);
}

/// A stored pair is an anchor on ONE conversation, and what came after it is added, never divided. Measured: a store
/// count of 940,211 paired as a RATIO with a rebuilt ledger of 1,486,369 came out 0.63 where the truth was 1.016, so a
/// session 1.46M tokens deep never compacted. As an anchor it grows with the conversation, so it does.
#[test]
fn a_stored_token_pair_is_used_only_for_the_conversation_it_measured() {
    // The pair that matches: both halves of one measurement.
    assert_eq!(
        super::recovered_scale(Some(940_211), Some(940_211), 940_211),
        Some((940_211, 940_211)),
    );
    // The row: an anchor at the conversation it measured, which the roll
    // then adds the later rows to - see the restart test below for the addition.
    assert_eq!(
        super::recovered_scale(Some(940_211), Some(950_000), 1_486_369),
        Some((950_000, 940_211)),
        "the count is anchored where it was taken, not paired with today's ledger"
    );
    // A stored ledger LONGER than the conversation in hand measured something else.
    assert_eq!(
        super::recovered_scale(Some(940_211), Some(1_486_369), 950_000),
        None
    );
    // A row written before v12 has no ledger to anchor to. Unverifiable is not
    // the same as wrong, but it IS the same as unmeasured - and the unscaled
    // window is the honest answer for unmeasured.
    assert_eq!(super::recovered_scale(Some(940_211), None, 940_211), None);
    // No provider count at all: a row whose turn never finished.
    assert_eq!(super::recovered_scale(None, Some(940_211), 940_211), None);
    // Zero is not a measurement of anything, on either side.
    assert_eq!(
        super::recovered_scale(Some(0), Some(940_211), 940_211),
        None
    );
    assert_eq!(super::recovered_scale(Some(940_211), Some(0), 0), None);
}

/// A killed subagent settles as STOPPED, not as having answered: an interrupted turn still returns the text it had
/// written, so a `task_result` calling that the child's last word would hand the parent a truncated answer with nothing
/// saying it was cut off.
#[test]
fn a_killed_subagent_settles_as_stopped_rather_than_as_an_answer() {
    use letibot_tools::builtins::task::TaskStatus;

    // Unkilled: the answer is the answer. This arm is here because the kill flag must
    // not become the relabelling of every completion that happens to follow one.
    let plain = TaskSlot::new("s-parent");
    plain.settle(TaskStatus::Done {
        answer: "half a sentence".into(),
        child: None,
    });
    assert!(matches!(
        &*plain.state.lock().expect("slot"),
        TaskStatus::Done { .. }
    ));

    let killed = TaskSlot::new("s-parent");
    killed.kill();
    killed.settle(TaskStatus::Done {
        answer: "half a sentence".into(),
        child: None,
    });
    match &*killed.state.lock().expect("slot") {
        TaskStatus::Failed { why } => assert!(
            why.contains("job_kill"),
            "the reason names the stop, so a reader can tell it from a crash: {why}"
        ),
        other => panic!("a killed subagent settled as {other:?}"),
    }

    // A real failure is not relabelled. The flag must not overwrite the child's
    // own reason for dying, which is the more useful sentence of the two.
    let failed = TaskSlot::new("s-parent");
    failed.kill();
    failed.settle(TaskStatus::Failed {
        why: "the model went away".into(),
    });
    assert!(matches!(
        &*failed.state.lock().expect("slot"),
        TaskStatus::Failed { why } if why.contains("went away")
    ));
}

/// A child's later answer is written where a restart reads it. The slot holds the newest one, and
/// the daemon that heard it is not the one that answers `task_result` afterwards; the row already said
/// what the first turn said, so the later answer goes beside it rather than over it.
#[test]
fn a_later_turns_answer_is_written_beside_the_first_one() {
    use crate::subagent_record::{RunStore, SubagentRecord};
    use letibot_tokencore::store::{Store, SubagentRun};
    use letibot_tools::builtins::task::TaskStatus;

    let dir = std::env::temp_dir().join(format!(
        "letibot-later-answer-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("sessions.db");
    let store = Store::open(&path).expect("the store");
    store
        .put_subagent_run(&SubagentRun {
            child_id: "s-1-sub-1".into(),
            parent_id: "s-1".into(),
            state: "running".into(),
            prompt: "do the thing".into(),
            spec_json: "{}".into(),
            instance: "i-1".into(),
            resumes: 0,
            outcome: None,
            later_answer: None,
            delivered: false,
            started_ms: 1,
            updated_ms: 1,
        })
        .expect("the child's row");

    let record = SubagentRecord {
        store: RunStore {
            path: path.clone(),
            cell: Default::default(),
        },
        child: "s-1-sub-1".into(),
        registry: letibot_sessionlog::registry::Registry::new(),
        pending: None,
    };
    let slot = TaskSlot::recorded("s-1", Some(record));
    slot.settle(TaskStatus::Done {
        answer: "the first answer".into(),
        child: None,
    });
    store.deliver_subagent_run("s-1-sub-1").expect("told");
    slot.spoke_again("the second answer");

    let row = store
        .subagent_run("s-1-sub-1")
        .expect("the read")
        .expect("the child's row");
    assert_eq!(
        row.later_answer.as_deref(),
        Some("the second answer"),
        "the answer of a turn after the settled one is the parent's last news of it, and it has to \
         outlive the daemon that heard it"
    );
    assert_eq!(
        row.outcome.as_deref(),
        Some("the first answer"),
        "the first answer stays where it was"
    );
    assert!(
        !row.delivered,
        "nobody has been handed this one: it is a settlement of its own"
    );
    assert_eq!(
        slot.newest_again().as_deref(),
        Some("the second answer"),
        "and this daemon still answers from the slot"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A child nobody gave a model runs on its parent's - *"so if no local config, and no
/// model - default to the same"*. `local` only for a parent that is itself local.
#[test]
fn a_child_with_no_model_named_runs_on_its_parents_model() {
    let on = |name: &str, model: Option<&str>| {
        Some(crate::config::ProviderConfig {
            name: name.into(),
            model: model.map(str::to_string),
            api_key: None,
            thinking: false,
        })
    };
    // The parent's configuration as it is - its key with it, which no file holds here.
    let mut parent = on("deepseek", Some("deepseek-chat"));
    parent.as_mut().unwrap().api_key = Some("sk-only-in-memory".into());
    let m =
        inherited_spawn_model(&parent, Some(None)).expect("the parent's own model needs no lookup");
    let pc = m.provider.as_ref().expect("a provider child");
    assert_eq!(pc.name, "deepseek");
    assert_eq!(pc.model.as_deref(), Some("deepseek-chat"));
    assert_eq!(pc.api_key.as_deref(), Some("sk-only-in-memory"));
    assert_eq!(m.label, "deepseek/deepseek-chat");
    assert!(m.local.is_none(), "{}", m.label);

    // A parent on its own server gives `local` - the same model, which is the point.
    let l = inherited_spawn_model(&None, Some(Some(32768))).expect("local");
    assert_eq!(l.label, "local");
    assert_eq!(l.window, Some(32768));
    assert!(l.provider.is_none());
}

/// A child whose thread ends in failure leaves its worktree reaped: the one exit every refused,
/// failed and killed child takes is `finish_child_thread`, and nothing else removed the trees those
/// children left. The branch keeps its commits, and a tree with uncommitted work is kept.
#[test]
fn a_failed_child_leaves_no_worktree_and_keeps_its_branch() {
    let root = std::env::temp_dir().join(format!("letibot-reap-{}-{}", std::process::id(), {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir");
    let git = |args: &[&str]| {
        let out = letibot_tools::testing::git(&root)
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(root.join("a.txt"), "one\n").expect("write");
    git(&["add", "-A"]);
    git(&["commit", "-qm", "first"]);
    // Beside the repository rather than where `worktree_path` would put them: the layout rule keeps
    // the trees of a repository under its PARENT, which for a temp repository is shared by every run.
    let slug = "reap-me";
    let path = root.join(format!("wt-{slug}"));
    let path = path.to_str().expect("utf-8 path").to_string();
    let branch = letibot_tools::builtins::task::worktree_branch(slug);
    super::git_in(
        root.to_str().unwrap(),
        &["worktree", "add", "-q", &path, "-b", &branch, "HEAD"],
    )
    .expect("the worktree is created");
    let placement = letibot_tools::builtins::task::WorktreePlacement {
        path: path.clone(),
        branch: branch.clone(),
        base_sha: super::git_in(root.to_str().unwrap(), &["rev-parse", "HEAD"]).expect("base"),
        main_tree: false,
    };

    // A failure: the tree goes and the branch stays, at the commit the child made.
    std::fs::write(std::path::Path::new(&path).join("a.txt"), "two\n").expect("write");
    // Through the fixture's own git, which carries an identity and no signing: a commit made with
    // the box's own config depends on what the operator's `~/.gitconfig` says.
    let made = letibot_tools::testing::git(std::path::Path::new(&path))
        .args(["-c", "commit.gpgsign=false", "commit", "-qam", "the work"])
        .output()
        .expect("git");
    assert!(
        made.status.success(),
        "the child's commit: {}",
        String::from_utf8_lossy(&made.stderr)
    );
    let head = super::git_in(&path, &["rev-parse", "HEAD"]).expect("its head");
    // A daemon that is not stopping: this child's failure is its own, so its tree is debris.
    let registry = letibot_sessionlog::registry::Registry::new();
    let slot = TaskSlot::new("s-parent");
    slot.set_deliverable(deliverable::Deliverable::of(&placement));
    finish_child_thread(
        &slot,
        Err("the provider refused the request".into()),
        &registry,
    );
    assert!(
        matches!(
            slot.status(),
            letibot_tools::builtins::task::TaskStatus::Failed { .. }
        ),
        "the child failed: {:?}",
        slot.status()
    );
    assert!(
        !std::path::Path::new(&path).exists(),
        "the worktree a failed child left is debris and is reaped"
    );
    assert_eq!(
        super::git_in(root.to_str().unwrap(), &["rev-parse", &branch]).expect("the branch"),
        head,
        "the branch keeps what the child committed"
    );

    // And a tree that still holds uncommitted work is kept: that is what makes reaping safe.
    let path2 = root.join("wt-keep-me");
    let path2 = path2.to_str().expect("utf-8 path").to_string();
    let branch2 = letibot_tools::builtins::task::worktree_branch("keep-me");
    super::git_in(
        root.to_str().unwrap(),
        &["worktree", "add", "-q", &path2, "-b", &branch2, "HEAD"],
    )
    .expect("a second worktree");
    std::fs::write(std::path::Path::new(&path2).join("a.txt"), "three\n").expect("write");
    let kept = TaskSlot::new("s-parent");
    kept.set_deliverable(deliverable::Deliverable::of(
        &letibot_tools::builtins::task::WorktreePlacement {
            path: path2.clone(),
            branch: branch2.clone(),
            base_sha: String::new(),
            main_tree: false,
        },
    ));
    finish_child_thread(&kept, Err("a turn that blew up".into()), &registry);
    assert!(
        std::path::Path::new(&path2).exists(),
        "a tree with uncommitted work is not reaped"
    );

    // A child that ANSWERED and then parked keeps its tree, whatever its last turn did: the
    // parent was told the branch is at this path, and `later_turn` settles the same slot when a
    // `task_message` turn fails. That is not the turn the child was started for.
    let path3 = root.join("wt-answered");
    let path3 = path3.to_str().expect("utf-8 path").to_string();
    let branch3 = letibot_tools::builtins::task::worktree_branch("answered");
    super::git_in(
        root.to_str().unwrap(),
        &["worktree", "add", "-q", &path3, "-b", &branch3, "HEAD"],
    )
    .expect("a third worktree");
    let parked = TaskSlot::new("s-parent");
    parked.set_deliverable(deliverable::Deliverable::of(
        &letibot_tools::builtins::task::WorktreePlacement {
            path: path3.clone(),
            branch: branch3.clone(),
            base_sha: String::new(),
            main_tree: false,
        },
    ));
    parked.settle(letibot_tools::builtins::task::TaskStatus::Done {
        answer: "the branch is here".into(),
        child: None,
    });
    parked.settle(letibot_tools::builtins::task::TaskStatus::Failed {
        why: "the provider refused the request".into(),
    });
    finish_child_thread(&parked, Ok("the branch is here".into()), &registry);
    assert!(
        std::path::Path::new(&path3).exists(),
        "a parked child whose last turn failed still has the tree its answer named"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A child the daemon's orderly stop ended keeps its tree. The stop is the daemon's ending and
/// not the child's: its row stays `running`, and the next daemon resumes it into this worktree, so a
/// reap here would resume a child into a directory that is gone.
#[test]
fn a_child_the_orderly_stop_ended_keeps_its_worktree() {
    let root = std::env::temp_dir().join(format!("letibot-keep-{}-{}", std::process::id(), {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir");
    let git = |args: &[&str]| {
        let out = letibot_tools::testing::git(&root)
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(root.join("a.txt"), "one\n").expect("write");
    git(&["add", "-A"]);
    git(&["commit", "-qm", "first"]);
    let path = root.join("wt-stopped");
    let path = path.to_str().expect("utf-8 path").to_string();
    let branch = letibot_tools::builtins::task::worktree_branch("stopped");
    super::git_in(
        root.to_str().unwrap(),
        &["worktree", "add", "-q", &path, "-b", &branch, "HEAD"],
    )
    .expect("the worktree is created");
    let placement = letibot_tools::builtins::task::WorktreePlacement {
        path: path.clone(),
        branch,
        base_sha: super::git_in(root.to_str().unwrap(), &["rev-parse", "HEAD"]).expect("base"),
        main_tree: false,
    };

    let slot = TaskSlot::new("s-parent");
    slot.set_deliverable(deliverable::Deliverable::of(&placement));
    let registry = letibot_sessionlog::registry::Registry::new();
    // The orderly stop, before the child's turn winds down: the same order `Registry::close` keeps
    // and the one the record's own `settled` reads.
    registry.close();
    finish_child_thread(&slot, Err("the daemon is stopping".into()), &registry);
    assert!(
        matches!(
            slot.status(),
            letibot_tools::builtins::task::TaskStatus::Failed { .. }
        ),
        "the turn was ended by the stop, so the slot is not an answer: {:?}",
        slot.status()
    );
    assert!(
        std::path::Path::new(&path).exists(),
        "a child the next daemon resumes keeps the tree it is resumed into"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A child refused before it opened is a failure `task_result` can read, at once: the refusal returned before
/// `AnswerOnce` existed, the thread dropped the sentence, and the slot read `Running` with nothing behind it. The
/// operator's report of that: it hanged on task_result.
#[test]
fn a_child_refused_before_it_opened_settles_its_slot_with_the_reason() {
    use letibot_tools::builtins::task::TaskStatus;
    let refused = TaskSlot::new("s-parent");
    let registry = letibot_sessionlog::registry::Registry::new();
    finish_child_thread(
        &refused,
        Err("model `nope` is not one this daemon can run".into()),
        &registry,
    );
    match refused.status() {
        TaskStatus::Failed { why } => assert!(why.contains("nope"), "{why}"),
        other => panic!("a refused child still reads {other:?} — task_result would wait"),
    }

    // A slot the child's own path settled keeps that, answer or failure: the thread's
    // fallback is for the case nothing spoke, not a second opinion.
    let answered = TaskSlot::new("s-parent");
    answered.settle(TaskStatus::Done {
        answer: "the child's own answer".into(),
        child: None,
    });
    finish_child_thread(&answered, Err("a later error".into()), &registry);
    assert!(
        matches!(answered.status(), TaskStatus::Done { ref answer, .. } if answer == "the child's own answer"),
        "{:?}",
        answered.status()
    );

    // And an Ok return changes nothing: `AnswerOnce` already said it.
    let ok = TaskSlot::new("s-parent");
    ok.settle(TaskStatus::Failed {
        why: "the child's own failure".into(),
    });
    finish_child_thread(&ok, Ok(String::new()), &registry);
    assert!(
        matches!(ok.status(), TaskStatus::Failed { ref why } if why == "the child's own failure"),
        "{:?}",
        ok.status()
    );
}

/// A stop takes THIS session's children and nobody else's, and not the ones that are gone. The list is the TREE's, so
/// stopping needs the owner back: a root stopping every slot it could see would stop its grandchildren in one go, and a
/// child its siblings' work. And a child that has ANSWERED is still alive, so `exited` is the fact that decides.
#[test]
fn a_stop_takes_this_sessions_children_and_not_the_ones_that_are_gone() {
    // The tree's one list, with four entries: three of mine - one still computing, one
    // parked with an answer, one whose thread has ended - and one of my PARENT's, which is
    // a sibling of mine and must not be in my answer.
    let mine_running = Arc::new(TaskSlot::new("s-child"));
    let mine_answered = Arc::new(TaskSlot::new("s-child"));
    mine_answered.settle(letibot_tools::builtins::task::TaskStatus::Done {
        answer: "done, and still here".into(),
        child: None,
    });
    let mine_and_gone = Arc::new(TaskSlot::new("s-child"));
    mine_and_gone.mark_exited();
    // A slot's handle follows its OWNER (`<owner>-sub-N`), so a parent's child cannot be called `s-child-sub-*`: a name
    // saying `s-child` on a slot the root started is the confusion this test is about, and the owner field settles it.
    let my_parents = Arc::new(TaskSlot::new("s-root"));
    let slots = vec![
        ("s-child-sub-1".to_string(), mine_running),
        ("s-child-sub-2".to_string(), mine_answered),
        ("s-child-sub-3".to_string(), mine_and_gone),
        ("s-root-sub-1".to_string(), my_parents),
    ];

    let stop = stoppable_children(&slots, "s-child");
    assert_eq!(
        stop,
        vec!["s-child-sub-1".to_string(), "s-child-sub-2".to_string()],
        "my own children — the running one AND the one that has answered and is still \
             parked — and no sibling's, no parent's, and none that has left"
    );
    // And from the other seat in the same tree: the root stops ITS children, not this
    // session's. Same list, different answer - which is the whole reason the owner is on
    // the slot.
    assert_eq!(
        stoppable_children(&slots, "s-root"),
        vec!["s-root-sub-1".to_string()],
        "a parent's stop is its own children, not its grandchildren's"
    );
    assert!(
        stoppable_children(&slots, "s-nobody").is_empty(),
        "a session with no children of its own in this tree stops nothing"
    );
}

/// The wait for a child's thread is a wait and not a hope: `wait_until_exited` is what a caller about to let the
/// process exit uses to know no child is inside `libllama` when `exit()` finalizes it. It must not answer gone on a
/// live thread, and it must return as soon as the thread leaves rather than at the deadline.
#[test]
fn the_wait_for_a_childs_thread_ends_when_the_thread_does() {
    let slot = Arc::new(TaskSlot::new("s-parent"));

    // Still running: the wait honours its deadline and says so rather than answering
    // "gone" about a thread that is right there.
    let began = std::time::Instant::now();
    assert!(
        !slot.wait_until_exited(std::time::Duration::from_millis(60)),
        "a wait that returns true on a live child would hide the crash instead of fixing it"
    );
    assert!(
        began.elapsed() >= std::time::Duration::from_millis(50),
        "it must actually wait: it returned in {:?}",
        began.elapsed()
    );

    // The child's thread leaves 50 ms from now, and the waiter must wake on that rather
    // than on its own deadline.
    let leaving = slot.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        leaving.mark_exited();
    });
    let began = std::time::Instant::now();
    assert!(
        slot.wait_until_exited(std::time::Duration::from_secs(30)),
        "the child's own `mark_exited` must be what ends the wait"
    );
    assert!(
        began.elapsed() < std::time::Duration::from_secs(5),
        "the wait ended {:?} after the child left, which is a deadline and not a wakeup",
        began.elapsed()
    );
}

/// The sentence a stopped session leaves names what it stopped and what it could not: a count that swallowed a refusal
/// would be the claim-versus-fact defect this tree keeps paying for, with a reader told stopped two while one is still
/// computing.
#[test]
fn a_stop_says_what_it_stopped_and_what_it_could_not() {
    // No children: no sentence at all, which is the ordinary case - a session with nothing
    // running says nothing on the way out.
    assert_eq!(children_stopped_notice(&[]), None);

    let stopped = children_stopped_notice(&[(
        "s-child-sub-1".into(),
        Ok("the daemon accepted the interrupt".into()),
    )]);
    let said = stopped.expect("one child stopped is one sentence");
    assert!(said.contains("s-child-sub-1"), "{said}");
    assert!(
        said.contains("before stopping itself"),
        "the order is the point of it — children first, then this session: {said}"
    );

    // A refusal is NAMED, with the daemon's own reason, and the sentence does not claim a
    // stop it did not get.
    let mixed = children_stopped_notice(&[
        ("s-a".into(), Ok("accepted".into())),
        (
            "s-b".into(),
            Err("`s-b` has already settled, or its session is gone".into()),
        ),
    ])
    .expect("a partial stop is still a sentence");
    assert!(mixed.contains("s-a"), "{mixed}");
    assert!(mixed.contains("s-b"), "{mixed}");
    assert!(
        mixed.contains("could NOT be stopped") && mixed.contains("already settled"),
        "the refusal is on the record, in the daemon's own words: {mixed}"
    );
}

/// A stop delivered to a running turn takes the session's CHILDREN with it first: the door an interrupt actually
/// reaches a turn through. The instrument is a fake runner, since the claim is about who is told and when, and two
/// levels, because each session's arm stops ITS OWN runner and the walk down is one level per session.
#[test]
fn a_stop_takes_the_sessions_children_before_it_takes_the_session() {
    /// Records that it was asked to stop, and what it was asked for after each stop.
    struct Recording {
        me: String,
        stops: Arc<Mutex<Vec<String>>>,
    }

    impl letibot_tools::builtins::task::TaskRunner for Recording {
        fn start(
            &self,
            _prompt: &str,
            _spec: &letibot_tools::builtins::task::TaskSpec,
        ) -> Result<String, String> {
            Err("this runner starts nothing".into())
        }
        fn collect(
            &self,
            _handle: &str,
            _timeout: std::time::Duration,
        ) -> letibot_tools::builtins::task::TaskStatus {
            letibot_tools::builtins::task::TaskStatus::Unknown
        }
        fn stop_all(&self) -> Vec<(String, Result<String, String>)> {
            self.stops.lock().expect("stops").push(self.me.clone());
            vec![(
                format!("{}-sub-1", self.me),
                Ok("the daemon accepted the interrupt".into()),
            )]
        }
    }

    // Two levels of the same door: a parent's steering source and a child's, each with its
    // OWN runner - which is what "one level per session" means in code.
    let stops = Arc::new(Mutex::new(Vec::new()));
    let parent_hub = Hub::new("s-root");
    let mut parent = HubSteering {
        hub: parent_hub.clone(),
        // No host: these fixtures are about the message's scoping, not about a layer's words.
        host: None,
        trail: None,
        injected: Arc::new(Mutex::new(VecDeque::new())),
        monitors: None,
        monitor_cursor: Arc::new(AtomicUsize::new(0)),
        deliveries: None,
        subagents: Some(Arc::new(Recording {
            me: "s-root".into(),
            stops: stops.clone(),
        })),
        held: Default::default(),
        stopped: Default::default(),
        todos: None,
        // No parent: these fixtures are about the message's scoping, and the sentence
        // falls back to `message from session `<id>`` - see `message_from_a_session`.
        parent: None,
        at_boundary: false,
        cheap: false,
    };
    let child_hub = Hub::new("s-child");
    let mut child = HubSteering {
        hub: child_hub.clone(),
        host: None,
        trail: None,
        injected: Arc::new(Mutex::new(VecDeque::new())),
        monitors: None,
        monitor_cursor: Arc::new(AtomicUsize::new(0)),
        deliveries: None,
        subagents: Some(Arc::new(Recording {
            me: "s-child".into(),
            stops: stops.clone(),
        })),
        held: Default::default(),
        stopped: Default::default(),
        todos: None,
        // No parent: these fixtures are about the message's scoping, and the sentence
        // falls back to `message from session `<id>`` - see `message_from_a_session`.
        parent: None,
        at_boundary: false,
        cheap: false,
    };

    // Nothing has been stopped to begin with, and a prompt is not a stop: it is a turn.
    assert!(stops.lock().expect("stops").is_empty());
    let head = parent_hub.attach(
        "tui",
        "dead@lab2x1",
        letibot_sessionlog::protocol::Caps::default(),
        0,
    );
    parent_hub.submit(
        &head.head_id,
        "c1",
        0,
        letibot_sessionlog::CommandKind::Prompt {
            text: "carry on".into(),
            attachments: Vec::new(),
        },
    );
    assert!(
        matches!(parent.try_next(), Some(m) if !m.urgent),
        "a prompt is ordinary steering"
    );
    assert!(
        stops.lock().expect("stops").is_empty(),
        "and it stops nothing: children are taken by a STOP, not by every message"
    );

    // The stop, on both sessions.
    for (hub, steering) in [(&parent_hub, &mut parent), (&child_hub, &mut child)] {
        let f = hub.submit(
            letibot_sessionlog::hub::DAEMON_SUBMITTER,
            "job_kill-1",
            0,
            letibot_sessionlog::CommandKind::Interrupt {
                reason: "`job_kill` stopped this subagent".into(),
            },
        );
        assert!(
            matches!(f, letibot_sessionlog::ServerFrame::Accepted { .. }),
            "the hub refused the interrupt: {f:?}"
        );
        let got = steering.try_next();
        assert!(
            matches!(&got, Some(m) if m.urgent),
            "the interrupt is handed to the engine as urgent, which is what stops the \
                 turn: {got:?}"
        );
    }
    assert_eq!(
        *stops.lock().expect("stops"),
        vec!["s-root".to_string(), "s-child".to_string()],
        "each session's own arm stopped its own children, in that order"
    );

    // The ordering, which is the requirement and not tidiness: the arm stops the children on the way IN, before the
    // urgent message it returns can stop the session, and the notice is on the stopping session's own log.
    let said = |hub: &Arc<Hub>| {
        hub.retained()
            .iter()
            .filter_map(|e| match &e.event {
                letibot_sessionlog::SessionEvent::Warning { code, detail, .. }
                    if code == "subagents_stopped" =>
                {
                    Some(detail.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let root_said = said(&parent_hub);
    assert_eq!(root_said.len(), 1, "one sentence per stop: {root_said:?}");
    assert!(
        root_said[0].contains("s-root-sub-1") && root_said[0].contains("before stopping itself"),
        "{}",
        root_said[0]
    );
    let child_said = said(&child_hub);
    assert!(
        child_said[0].contains("s-child-sub-1"),
        "and the child's own log names the child's own child — one level, each session \
             answerable for its own: {child_said:?}"
    );
}

/// R7's tool-side half: the sentence says the result comes to you. The text is the deliverable, since it taught the
/// model that waiting was the only way to learn a result, so two things are asserted in it: the job's own identity and
/// the promise that there is nothing to wait for.
#[test]
fn a_completion_notice_names_the_job_and_says_do_not_wait() {
    let text = completion_notice(
        &[JobCompletion {
            arrived: std::time::SystemTime::UNIX_EPOCH,
            later: false,
            kind: BackgroundKind::Job,
            owner: "s-me".into(),
            job: "j7".into(),
            command: "cargo build --release".into(),
            // Nobody named this one; the named case is its own test below.
            slug: None,
            state: "exited 0".into(),
            produced: 4096,
            elapsed_ms: 4_400,
            detail: String::new(),
            memory: None,
        }],
        true,
    );
    assert!(text.starts_with("[job]"), "labelled like a monitor: {text}");
    assert!(text.contains("`j7`"), "{text}");
    assert!(text.contains("exited 0"), "{text}");
    assert!(text.contains("4.4s"), "how long it ran: {text}");
    // no byte count, and the model's own command is not echoed back.
    assert!(!text.contains("cargo build --release"), "{text}");
    assert!(!text.contains("4096"), "{text}");
    // the closing names the door with the real handle - not `job_wait`, not `job="…"`.
    assert!(text.ends_with("Output: `job_output` job=\"j7\"."), "{text}");
    assert!(!text.contains("job_wait") && !text.contains('…'), "{text}");
}

/// A prompt is answered, a parent's message is HEARD, a stop is a STOP for the whole subtree, and everything else a child
/// inherits is left and named. The interrupt arm does not say nothing is running to
/// interrupt about a session whose subtree is, and the message arm starts a turn of its own.
#[test]
fn a_child_answers_a_prompt_and_leaves_the_machinery_its_parent_owns() {
    use letibot_sessionlog::CommandKind as K;
    assert_eq!(
        child_command(&K::Prompt {
            text: "do this next".into(),
            attachments: Vec::new(),
        }),
        ChildCommand::Answer,
        "the operator asking a child something is a turn, not a no-op"
    );
    assert_eq!(
        child_command(&K::Message {
            from: "s-parent".into(),
            text: "change of plan".into(),
        }),
        ChildCommand::Hear,
        "a parent's message is heard: the child's own turn reads it"
    );
    assert_eq!(
        child_command(&K::Interrupt {
            reason: "esc".into()
        }),
        ChildCommand::Stop,
        "an interrupt IS this child's stop between turns — its children go first, then it does"
    );
    for kind in [
        K::Compact,
        K::Mode {
            name: "allow-all".into(),
            consented: true,
        },
    ] {
        assert_eq!(
            child_command(&kind),
            ChildCommand::Leave("a door a child does not own"),
            "{kind:?}"
        );
    }
}

/// What the head attached to a subagent is told about a command the child leaves: the line it typed, named back, and where to send it instead.
#[test]
fn a_command_a_child_leaves_is_named_back_to_its_head() {
    use letibot_sessionlog::CommandKind as K;
    let said = child_leave_said(
        "s-1-sub-2",
        &K::OperatorShell {
            line: "! ls".into(),
            who: "dead".into(),
        },
        "a door a child does not own",
    );
    assert!(said.contains("`! ls`"), "{said}");
    assert!(said.contains("s-1-sub-2"), "{said}");
    assert!(said.contains("parent"), "{said}");
    let said = child_leave_said("s-1-sub-2", &K::Compact, "a door a child does not own");
    assert!(said.contains("`compact`"), "{said}");
}

/// A progress bar is ONE line, and the window must show its last state: `cargo`, `docker` and `npm` redraw a row with
/// `\r` and no newline, so splitting on `\n` alone put every update on one row, which is what the operator was reading.
#[test]
fn a_progress_bar_is_one_line_and_not_every_update_concatenated() {
    // The ordinary case: each redraw is longer, so the last one stands alone.
    assert_eq!(
        progress_lines("  2%\r 23%\r 47%\r100%"),
        vec!["100%"],
        "the row is what a terminal would end up showing"
    );
    // A SHORTER final segment leaves the tail of the longer one standing - that is
    // what the screen showed, and dropping the tail would be inventing a different one.
    // (The naive `after the last \r` rule gets this wrong.)
    assert_eq!(progress_lines("abcdefgh\rXY"), vec!["XYcdefgh"]);
    // Newlines still split; a carriage return at the end of a line ends it rather than
    // being kept as a byte nobody can see.
    assert_eq!(progress_lines("a\nb\rc\nd\r"), vec!["a", "c", "d"]);
    // And it is not a rewrite of ordinary output: no CR, no change.
    assert_eq!(
        progress_lines("$ cargo test\nrunning 3 tests"),
        vec!["$ cargo test", "running 3 tests"]
    );
}

/// A parent's correction reaches a running child as steering, and NOT as the operator's words: the runner submits
/// under the daemon's name, the kind carries the parent's identity, and the child's poll takes it at its next round. It
/// is not `from_operator`, so it cannot authorise, and not `urgent`, so the child finishes the call it is in.
#[test]
fn a_parents_message_is_the_childs_steering_and_not_the_operators_words() {
    let hub = Hub::new("s-child");
    let mut steering = HubSteering {
        hub: hub.clone(),
        // No trail: what is asserted is the message the child reads and how it is
        // scoped, and a trail would only add a second place for the same words.
        host: None,
        trail: None,
        injected: Arc::new(Mutex::new(VecDeque::new())),
        monitors: None,
        monitor_cursor: Arc::new(AtomicUsize::new(0)),
        deliveries: None,
        // No children either: what is asserted here is the message's scoping.
        subagents: None,
        held: Default::default(),
        stopped: Default::default(),
        todos: None,
        // No parent: these fixtures are about the message's scoping, and the sentence
        // falls back to `message from session `<id>`` - see `message_from_a_session`.
        parent: None,
        at_boundary: false,
        cheap: false,
    };

    // What `HarnessTaskRunner::send` does, exactly: the daemon's own name at the
    // door, the parent's id in the kind.
    let f = hub.submit(
        letibot_sessionlog::hub::DAEMON_SUBMITTER,
        "task_message-1",
        0,
        letibot_sessionlog::CommandKind::Message {
            from: "s-parent".into(),
            text: "stop and report what you have".into(),
        },
    );
    assert!(
        matches!(f, letibot_sessionlog::ServerFrame::Accepted { .. }),
        "the child's hub refused the daemon's relay: {f:?}"
    );

    let got = steering
        .try_next()
        .expect("the child's next round boundary takes it");
    assert!(
        !got.from_operator,
        "a parent's correction was recorded as the operator's own words: {got:?}"
    );
    assert!(
        !got.urgent,
        "a correction is context for the next round, not an interrupt: {got:?}"
    );
    assert!(
        got.text.contains("s-parent") && got.text.contains("stop and report what you have"),
        "the child cannot tell which parent spoke, or what it said: {}",
        got.text
    );
    // Taken, not peeked: a correction delivered twice is a child told the same thing
    // at two round boundaries, which reads as insistence.
    assert!(
        steering.try_next().is_none(),
        "the same message was handed over twice"
    );
}

/// A failed turn's words come back out of the hub's steering, still the operator's, and not twice: a message is out of
/// the queue the moment `try_next` dequeues it, and a turn that fails commits nothing, so the words go back to the
/// source rather than being appended, with the flags they had when taken.
#[test]
fn a_handed_back_message_comes_out_of_the_next_poll_still_the_operators() {
    let hub = Hub::new("s-steer");
    let mut steering = HubSteering {
        hub: hub.clone(),
        host: None,
        trail: None,
        injected: Arc::new(Mutex::new(VecDeque::new())),
        monitors: None,
        monitor_cursor: Arc::new(AtomicUsize::new(0)),
        deliveries: None,
        subagents: None,
        held: Default::default(),
        stopped: Default::default(),
        todos: None,
        // No parent: these fixtures are about the message's scoping, and the sentence
        // falls back to `message from session `<id>`` - see `message_from_a_session`.
        parent: None,
        at_boundary: false,
        cheap: false,
    };

    // An interrupt queued behind the failed turn: it must come out AFTER the handed-back words, which were taken before
    // it. The daemon may submit an interrupt and nothing else that speaks as a person, so this is the door the test uses.
    let f = hub.submit(
        letibot_sessionlog::hub::DAEMON_SUBMITTER,
        "interrupt-1",
        0,
        letibot_sessionlog::CommandKind::Interrupt {
            reason: "ABORT".into(),
        },
    );
    assert!(
        matches!(f, letibot_sessionlog::ServerFrame::Accepted { .. }),
        "the hub refused the interrupt: {f:?}"
    );

    // The failed turn hands the operator's words back.
    steering.give_back(vec![SteeringMessage::operator(
        "use web_search and fetch to lookup git commands meaning",
    )]);

    // The next poll takes the handed-back words first, still the operator's.
    let back = steering
        .try_next()
        .expect("the handed-back words come out of the next poll");
    assert_eq!(
        back.text,
        "use web_search and fetch to lookup git commands meaning"
    );
    assert!(
        back.from_operator,
        "the words came out as somebody else's: {back:?}"
    );

    // Not handed over twice: the next poll takes the queued interrupt, then nothing.
    let next = steering
        .try_next()
        .expect("the queued interrupt still comes out");
    assert!(
        next.urgent,
        "the interrupt came out as something else: {next:?}"
    );
    assert!(
        steering.try_next().is_none(),
        "a message was handed over twice"
    );
}

#[test]
fn flowy_is_a_door_for_every_root_session_and_for_no_subagent() {
    let mut cfg = Config::for_this_box(std::env::temp_dir());
    cfg.seat = Seat::Leticode;
    cfg.allow_bash = true;
    // Seat or no seat: the door is there, so `/flowy login` can fill it.
    let without = role_for(&cfg);
    assert!(without.tools.iter().any(|t| t == "flowy"));

    cfg.flowy = Some(crate::config::FlowyConfig::default());
    let root = role_for(&cfg);
    assert!(root.tools.iter().any(|t| t == "flowy"));
    // And the person can be asked. The operator's ruling: *"yeah i
    // want you to be able to ask me for a choice"* - the seat carries it whether or
    // not a room is attached, because asking is not a room's business.
    assert!(
        root.tools.iter().any(|t| t == "ask_user_question"),
        "the leticode seat cannot ask the person a question: {:?}",
        root.tools
    );
    // The whole opencode union plus the room fits the ceiling, which moved to twenty-four when `task_message` joined the
    // delegation trio. A tool added after this takes a seat from something or declares its own number where somebody
    // deciding can read it.
    assert!(
        root.tools.len() <= root.max_tools,
        "{} > {}",
        root.tools.len(),
        root.max_tools
    );

    // A subagent hears through its parent.
    cfg.parent_session_id = Some("parent".into());
    assert!(!role_for(&cfg).tools.iter().any(|t| t == "flowy"));

    // The runner has no spare seat, and the disclosure says NOT SEATED.
    cfg.parent_session_id = None;
    cfg.seat = Seat::Runner;
    let runner = role_for(&cfg);
    assert!(!runner.tools.iter().any(|t| t == "flowy"));
    assert!(runner.tools.len() <= runner.max_tools);
}

/// A child that ends while its own children are still on their task stops them, the edge a hold does not cover: a
/// child whose first turn failed on a store error left them to boot two VM grandchildren, whose VMs held two of four
/// slots for a day. A child with nothing unsettled stops nothing and says nothing.
#[test]
fn a_child_that_ends_with_children_on_their_task_stops_them() {
    struct Children {
        unsettled: usize,
        stops: Arc<Mutex<usize>>,
    }
    impl letibot_tools::builtins::task::TaskRunner for Children {
        fn start(
            &self,
            _prompt: &str,
            _spec: &letibot_tools::builtins::task::TaskSpec,
        ) -> Result<String, String> {
            Err("this runner starts nothing".into())
        }
        fn collect(
            &self,
            _handle: &str,
            _timeout: std::time::Duration,
        ) -> letibot_tools::builtins::task::TaskStatus {
            letibot_tools::builtins::task::TaskStatus::Unknown
        }
        fn unsettled_children(&self) -> usize {
            self.unsettled
        }
        fn stop_all(&self) -> Vec<(String, Result<String, String>)> {
            *self.stops.lock().expect("stops") += 1;
            vec![(
                "s-c-sub-1".into(),
                Ok("the daemon accepted the interrupt".into()),
            )]
        }
    }
    let stops = Arc::new(Mutex::new(0));
    let hub = Hub::new("s-c");
    let working: Arc<dyn letibot_tools::builtins::task::TaskRunner> = Arc::new(Children {
        unsettled: 2,
        stops: stops.clone(),
    });
    let said = a_child_that_ends_stops_its_own(Some(&working), &hub)
        .expect("children on their task are stopped, and it is said");
    assert!(said.contains("s-c-sub-1"), "{said}");
    assert_eq!(*stops.lock().expect("stops"), 1);

    let idle: Arc<dyn letibot_tools::builtins::task::TaskRunner> = Arc::new(Children {
        unsettled: 0,
        stops: stops.clone(),
    });
    assert_eq!(a_child_that_ends_stops_its_own(Some(&idle), &hub), None);
    assert_eq!(a_child_that_ends_stops_its_own(None, &hub), None);
    assert_eq!(
        *stops.lock().expect("stops"),
        1,
        "nothing unsettled, nothing stopped"
    );
}

/// An answer is kept only while it is owed to somebody. What a reader has been handed is in the
/// child's own session, so the slot drops the body: how many bodies the tree still owes must not
/// grow with the children it has run.
#[test]
fn a_delivered_answer_is_not_kept_for_the_daemons_life() {
    use letibot_tools::builtins::task::TaskStatus;
    let body = "one line of a child's answer, and then another\n".repeat(200);
    let mut slots: Vec<(String, Arc<TaskSlot>)> = Vec::new();
    for i in 0..200 {
        let slot = Arc::new(TaskSlot::new("s-parent"));
        slot.settle(TaskStatus::Done {
            answer: body.clone(),
            child: None,
        });
        let handle = format!("s-parent-sub-{i}");
        slots.push((handle.clone(), slot));
        // Undelivered, the body is whole and owed: a reader may still be handed it.
        assert_eq!(
            owed_answers(&slots),
            1,
            "child {i}'s answer is owed until somebody reads it"
        );
        release_delivered(&slots, &handle);
        assert_eq!(
            owed_answers(&slots),
            0,
            "child {i}'s answer was read, so the tree keeps none of it"
        );
    }
    // The state survives the body, and what a recordless child's reader gets in its place is the
    // sentence naming the session that does hold the text.
    let held = slots[123].1.status();
    assert!(
        matches!(&held, TaskStatus::Done { answer, .. } if answer.contains("transcript")),
        "{held:?}"
    );
    // And a child that answers AGAIN is owed again: the debt is per answer, not per child.
    let again = &slots[123].1;
    again.settle(TaskStatus::Done {
        answer: body.clone(),
        child: None,
    });
    assert_eq!(owed_answers(&slots), 1, "a fresh answer is a fresh debt");
    again.hand_over_answer();
    assert_eq!(owed_answers(&slots), 0);
}

/// A later answer is let go when a reader has it and the record keeps it, and never before. The
/// slot holds one only while it is owed; a child whose row never landed has no other copy, so that
/// answer stays where it is.
#[test]
fn a_later_answer_the_record_keeps_is_not_kept_twice() {
    let later = "what the child said when it was asked again\n".repeat(400);
    let slots = recorded_slots("a-later-answer", 20, &later);
    let held = |slots: &[(String, Arc<TaskSlot>)]| -> usize {
        slots.iter().map(|(_, s)| s.body_bytes()).sum()
    };
    assert!(
        held(&slots) >= slots.len() * later.len(),
        "a later answer is owed to whoever asks for it, so every child's is here"
    );
    for (handle, _) in &slots {
        release_delivered(&slots, handle);
    }
    assert_eq!(owed_answers(&slots), 0);
    // What is left is the sentence saying where the text is, not the text: a fixed handful of bytes
    // a child however much it said.
    assert!(
        held(&slots) <= slots.len() * 512,
        "the tree holds {} byte(s) of the later answers it has handed over",
        held(&slots)
    );
    // And the ROW is what a tree keeps per child once the body is gone: a few hundred bytes, so a
    // field that makes a slot thousands is the retention coming back.
    assert!(
        std::mem::size_of::<TaskSlot>() <= 640,
        "one slot is {} bytes before its strings",
        std::mem::size_of::<TaskSlot>()
    );
    let _ = std::fs::remove_dir_all(recorded_store_dir("a-later-answer"));
}

/// The slot is the only copy for a child whose row never landed, so a reader being handed the
/// text does not take it away: the answer a restart would have read is here or nowhere.
#[test]
fn a_later_answer_no_record_kept_stays_the_slots_own() {
    use letibot_tools::builtins::task::TaskStatus;
    let later = "what the child said when it was asked again\n".repeat(400);
    let slot = TaskSlot::new("s-parent");
    slot.settle(TaskStatus::Done {
        answer: "done".into(),
        child: None,
    });
    slot.spoke_again(&later);
    slot.hand_over_answer();
    assert_eq!(
        slot.newest_again().as_deref(),
        Some(later.as_str()),
        "nothing else holds this child's last words, so the slot must"
    );
    assert!(slot.body_bytes() >= later.len());
}

/// `count` slots of a session that ANSWERS, each with a record in a store of its own: the shape a
/// child the daemon wrote down has, and the one whose answer a second reader must still be given.
fn recorded_slots(tag: &str, count: usize, later: &str) -> Vec<(String, Arc<TaskSlot>)> {
    use crate::subagent_record::{RunStore, SubagentRecord};
    use letibot_tokencore::store::SubagentRun;
    use letibot_tools::builtins::task::TaskStatus;
    let dir = recorded_store_dir(tag);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("sessions.db");
    let store = Store::open(&path).expect("the store");
    (0..count)
        .map(|i| {
            let child = format!("s-parent-sub-{i}");
            store
                .put_subagent_run(&SubagentRun {
                    child_id: child.clone(),
                    parent_id: "s-parent".into(),
                    state: "running".into(),
                    prompt: "do the thing".into(),
                    spec_json: "{}".into(),
                    instance: "i-1".into(),
                    resumes: 0,
                    outcome: None,
                    later_answer: None,
                    delivered: false,
                    started_ms: 1,
                    updated_ms: 1,
                })
                .expect("the child's row");
            let record = SubagentRecord {
                store: RunStore {
                    path: path.clone(),
                    cell: Default::default(),
                },
                child: child.clone(),
                registry: letibot_sessionlog::registry::Registry::new(),
                pending: None,
            };
            let slot = Arc::new(TaskSlot::recorded("s-parent", Some(record)));
            slot.settle(TaskStatus::Done {
                answer: "done".into(),
                child: None,
            });
            slot.spoke_again(later);
            (child, slot)
        })
        .collect()
}

fn recorded_store_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{tag}-{}", std::process::id()))
}

fn reply(text: &str) -> Reply {
    Reply {
        text: text.into(),
        metrics: Vec::new(),
        rounds: 1,
        tool_calls: 0,
        truncated: false,
    }
}

/// A `[meanwhile]` turn is somebody's word: the door a child's held result wakes it
/// through is the door a prompt or a message comes in by, so its answer reaches its parent - while
/// the plan-check and the census are the harness talking to itself, and are not news.
#[test]
fn a_childs_wake_turn_is_news_and_its_own_clock_is_not() {
    use crate::harness::children::somebody_spoke;
    use crate::turn_tail::Door;
    for door in [Door::Prompt, Door::Message, Door::Wake] {
        assert!(
            somebody_spoke(&door),
            "{door:?} is somebody's word, so the turn's answer reaches the parent"
        );
    }
    for door in [
        Door::Nag(None),
        Door::Census,
        Door::OperatorShell,
        Door::Reseat,
        Door::Fallback,
    ] {
        assert!(
            !somebody_spoke(&door),
            "{door:?} is the harness's own clock, and its turn is not a settlement"
        );
    }
}

/// A turn the child runs inside its hold ends before the slot has settled: it is part of the
/// answer the hold ends with, and a failure of it must not settle the slot as failed (the tree
/// the answer names would be reaped).
#[test]
fn a_failing_turn_of_the_hold_does_not_settle_the_slot() {
    let out = Some(Err(HarnessError::Setup("provider refused".into())));
    assert!(
        crate::harness::children::later_news(true, false, &out).is_none(),
        "a hold turn's failure settled a Running slot"
    );
    assert!(
        crate::harness::children::later_news(true, true, &out).is_some(),
        "a later turn's failure after the answer is still news"
    );
}

/// The same hold turn answering is not the child finishing: the parent would be told
/// an intermediate answer was the task's.
#[test]
fn an_answering_turn_of_the_hold_is_not_handed_up_as_the_child_finishing() {
    let out = Some(Ok(reply("waiting on two more files")));
    assert!(
        crate::harness::children::later_news(true, false, &out).is_none(),
        "a hold turn's answer was handed up"
    );
    assert!(crate::harness::children::later_news(true, true, &out).is_some());
}

/// A later turn that fails does not rewrite the record: the child answered, and a restart must
/// still hand its parent that answer and not the later turn's error.
#[test]
fn a_later_turn_that_fails_leaves_the_first_outcome_in_the_record() {
    use crate::subagent_record::{RunStore, SubagentRecord};
    use letibot_tokencore::store::{Store, SubagentRun};
    use letibot_tools::builtins::task::TaskStatus;

    let dir = std::env::temp_dir().join(format!(
        "letibot-later-fails-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("sessions.db");
    let store = Store::open(&path).expect("the store");
    store
        .put_subagent_run(&SubagentRun {
            child_id: "s-1-sub-1".into(),
            parent_id: "s-1".into(),
            state: "running".into(),
            prompt: "do the thing".into(),
            spec_json: "{}".into(),
            instance: "i-1".into(),
            resumes: 0,
            outcome: None,
            later_answer: None,
            delivered: false,
            started_ms: 1,
            updated_ms: 1,
        })
        .expect("the child's row");
    let record = SubagentRecord {
        store: RunStore {
            path: path.clone(),
            cell: Default::default(),
        },
        child: "s-1-sub-1".into(),
        registry: letibot_sessionlog::registry::Registry::new(),
        pending: None,
    };
    let slot = TaskSlot::recorded("s-1", Some(record));
    slot.settle(TaskStatus::Done {
        answer: "the first answer".into(),
        child: None,
    });
    slot.spoke_again("the second answer");
    slot.settle_later(TaskStatus::Failed {
        why: "provider refused".into(),
    });

    let row = store
        .subagent_run("s-1-sub-1")
        .expect("the read")
        .expect("the child's row");
    assert_eq!(
        row.state, "done",
        "a later failure became the child's verdict"
    );
    assert_eq!(row.outcome.as_deref(), Some("the first answer"));
    assert_eq!(
        row.later_answer.as_deref(),
        Some("the second answer"),
        "the later answer was cleared"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Only the newest later answer is ever read, so only it is kept.
#[test]
fn a_slot_keeps_the_newest_later_answer_and_not_every_one() {
    let slot = TaskSlot::recorded("s-1", None);
    slot.spoke_again("two");
    slot.spoke_again("three");
    assert_eq!(slot.newest_again().as_deref(), Some("three"));
    assert_eq!(slot.answers_kept(), 1);
}

/// A failed child's tree is removed before its survivors are handed to the parent. A handed-over
/// process keeps the directory it stands in, so the reverse order leaves it writing into a path
/// nothing owns - and the reap is the caller's, which is why the order is a function and not a habit.
#[test]
fn a_failed_childs_tree_goes_before_its_survivors_are_handed_over() {
    let order = std::sync::Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));
    let said = crate::harness::children::a_failed_childs_ending(
        || {
            order.lock().unwrap().push("the tree is removed");
            Ok(())
        },
        || {
            order.lock().unwrap().push("the survivors are handed over");
            None
        },
    );
    let order = order.lock().unwrap().clone();
    assert_eq!(
        order,
        vec!["the tree is removed", "the survivors are handed over"],
        "the survivors were handed into a tree that was about to go"
    );
    assert!(said.is_none(), "a child with nothing left says nothing");
}
