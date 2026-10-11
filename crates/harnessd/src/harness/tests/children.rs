//! Subagents: spawning, worktrees, titles, stopping, and what a child hands back.

use super::*;

/// **A real worktree is arranged the way `task_start` arranges it**: the branch is
/// `agent/<slug>`, cut from the base, and the main tree is untouched.
///
/// This drives the same `git_in` door `HarnessTaskRunner::start_worktree` uses, in a
/// throwaway repo, so the branch name and the main tree's cleanliness are facts on
/// disk rather than a claim in the answer. The operator's ask — *"we will need a new
/// tool - task_start or what that will arrange worktree, firecode and subagent"* — is
/// that the placement is arranged before the child is spawned, and this is the half of
/// that that can be checked without a live subagent.
#[test]
fn a_worktree_is_arranged_with_its_branch_and_the_main_tree_untouched() {
    use std::process::Command;
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
        let out = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(root.join("a.txt"), "one\ntwo\n").expect("write");
    git(&["add", "-A"]);
    git(&["commit", "-qm", "first"]);
    let base_sha =
        super::git_in(root.to_str().unwrap(), &["rev-parse", "HEAD"]).expect("the base SHA");

    // **The worktree, arranged the way `start_worktree` arranges it.**
    let slug = "task-start";
    let path = letibot_tools::builtins::task::worktree_path(root.to_str().unwrap(), slug);
    let branch = letibot_tools::builtins::task::worktree_branch(slug);
    super::git_in(
        root.to_str().unwrap(),
        &["worktree", "add", &path, "-b", &branch, "HEAD"],
    )
    .expect("the worktree is created");

    // **The branch is `agent/<slug>`, cut from the base.**
    let wt_branch = super::git_in(&path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .expect("the worktree's branch");
    assert_eq!(wt_branch, branch, "the branch is agent/<slug>");
    let wt_base = super::git_in(&path, &["rev-parse", "HEAD"]).expect("the worktree's HEAD");
    assert_eq!(wt_base, base_sha, "the branch is cut from the base");

    // **The main tree is untouched**: its branch is still `main`, its HEAD is still
    // the base, and its status is clean — the worktree did not move the main tree.
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
    // the main tree — that is expected, it is the worktree's home. The main tree's
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

    // **And the worktree is a fact on disk**: `git worktree list` shows it.
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

/// **The subagent's title is not the session's name, and the two have different bounds.**
///
/// A session name is an **address** — `@seat/title`, which somebody types — so
/// `derive_title` holds it to six words and 48 characters. A subagent's title is a **row**,
/// and the operator's ruling is that a row's cap is the UI's: *"subagents list has
/// everything capped at 48 chars. but this cap is a ui concern — for example my terminal is
/// 160 cells wide, make the cap 256."* Reusing the address rule for the row cut every
/// subagent line to a name's length. This pins both, so a later reader cannot quietly merge
/// them again.
#[test]
fn a_subagent_title_is_a_row_and_a_session_name_is_an_address() {
    let long = "audit the session store and say which rows are never read by anything at \
                    all in the whole tree, and then propose a fix for each";
    // The ADDRESS rule: six words and no more, 48 characters at the outside.
    assert_eq!(
        super::derive_title(long),
        "audit the session store and say",
        "a name is six words, so it stays a name somebody can type"
    );
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
    // Only the first line — a row is one line — and only past the generous bound.
    assert_eq!(
        super::subagent_title("audit the store\nand the reader"),
        "audit the store"
    );
    let huge = "x".repeat(400);
    let cut = super::subagent_title(&huge);
    assert_eq!(cut.chars().count(), super::SUBAGENT_TITLE_MAX);
    assert!(cut.ends_with('…'), "{cut}");
}

/// **The child's row says where its point came from, and says when it was clamped.**
///
/// The row's vocabulary is the one the settings table already uses for a mode's source, so
/// the phrase `inherited from the parent session` is asserted here rather than trusted, and
/// the clamp's sentence is asserted to survive INTO the row — a child silently at a
/// different point than its parent is the state that made this a bug hunt.
///
/// **What this does NOT test**, and it is worth naming rather than leaving to be assumed:
/// that the spawn reads the parent's live point. That is `HarnessTaskRunner::point` reading
/// a cell `Harness::set_mode_consented` writes, and reaching either needs a session with a
/// model behind it — no test in this crate opens a `Harness` (they all need a vocabulary
/// GGUF and a server), so the wiring is read rather than driven. The DECISION is tested
/// where it lives: `mode::tests::a_child_inherits_the_point_of_the_session_that_spawned_it`
/// and its neighbours.
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

/// **Three children minted at one instant get three ids.**
///
/// leticl found this on the wire and could not read this tree: three `task` calls in a
/// loop, one id — *"one pane row, no subagent counter, one `[task]` notice — and two of
/// the three answers never reach me at all"*. The id is the handle `task_result`
/// collects by, and it finds the FIRST slot under a handle, so the other replies exist,
/// settle, and are unreachable.
///
/// The DEFECT was a millisecond clock, which three calls in one round share trivially;
/// this drives the same instant down to a nanosecond, because the guard has to hold at
/// the resolution the mint now uses and not merely at the one it was found at.
///
/// **Both halves of the guard are exercised, because either alone misses the real
/// case.** The handed-out check is the one that catches the ordinary shape: `adopt`
/// happens inside the child's thread, after a whole harness has been opened, while the
/// mint returns at once — so three calls in one round all mint before any of them
/// adopts, and a check against the registry alone finds nothing.
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

/// **A stored pair is an anchor on ONE conversation, and what came after it is
/// added, never divided.** See [`super::recovered_scale`].
///
/// The defect the old equality test closed, measured 2026-10-02: a store holding
/// 940,211 — the provider's count for a ~950k-token conversation — was paired
/// with a ledger of 1,486,369 that the daemon had just rebuilt, as a RATIO. It came
/// out 0.63 where the truth was 1.016, and the window 1,580,888 instead of about
/// 1,015,628 — so a session 1.46M tokens deep planned against a window it was
/// already far past, never compacted, and died at the provider's 400 on every
/// attempt. As an anchor the same row is 940,211 PLUS the rows after it — a count
/// that grows with the conversation, so it compacts.
#[test]
fn a_stored_token_pair_is_used_only_for_the_conversation_it_measured() {
    // The pair that matches: both halves of one measurement.
    assert_eq!(
        super::recovered_scale(Some(940_211), Some(940_211), 940_211),
        Some((940_211, 940_211)),
    );
    // The 2026-10-02 row: an anchor at the conversation it measured, which the roll
    // then adds the later rows to — see the restart test below for the addition.
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
    // the same as wrong, but it IS the same as unmeasured — and the unscaled
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

/// **A killed subagent settles as STOPPED, not as having answered.**
///
/// An interrupted turn still returns the text it had written when the interrupt
/// landed, so `run_to_completion` comes back `Ok` — and a `task_result` that called
/// that the child's last word would hand the parent a truncated answer with nothing on
/// it to say it had been cut off. The same defect as a truncated summary presented as
/// whole, one layer down.
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

    // **A real failure is not relabelled.** The flag must not overwrite the child's
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

/// **A child nobody gave a model runs on its parent's** — *"so if no local config, and no
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
    // The parent's configuration as it is — its key with it, which no file holds here.
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

    // A parent on its own server gives `local` — the same model, which is the point.
    let l = inherited_spawn_model(&None, Some(Some(32768))).expect("local");
    assert_eq!(l.label, "local");
    assert_eq!(l.window, Some(32768));
    assert!(l.provider.is_none());
}

/// **A child refused before it opened is a failure `task_result` can read, at once.**
///
/// The operator, on a stroppy session whose `task` (`where: firecode`) came back "started"
/// and whose `task_result` then sat out its whole 240 s: *"it hanged on task_result wtf"*.
/// The refusal — whichever check at the top of `run_to_completion` it was — returned before
/// `AnswerOnce` existed, the thread dropped the sentence, and the slot read `Running` with
/// nothing behind it. No session row, no `opening` record, no thread: only the job row
/// saying `running`.
#[test]
fn a_child_refused_before_it_opened_settles_its_slot_with_the_reason() {
    use letibot_tools::builtins::task::TaskStatus;
    let refused = TaskSlot::new("s-parent");
    finish_child_thread(
        &refused,
        Err("model `nope` is not one this daemon can run".into()),
    );
    match refused.status() {
        TaskStatus::Failed { why } => assert!(why.contains("nope"), "{why}"),
        other => panic!("a refused child still reads {other:?} — task_result would wait"),
    }

    // **A slot the child's own path settled keeps that**, answer or failure: the thread's
    // fallback is for the case nothing spoke, not a second opinion.
    let answered = TaskSlot::new("s-parent");
    answered.settle(TaskStatus::Done {
        answer: "the child's own answer".into(),
        child: None,
    });
    finish_child_thread(&answered, Err("a later error".into()));
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
    finish_child_thread(&ok, Ok(String::new()));
    assert!(
        matches!(ok.status(), TaskStatus::Failed { ref why } if why == "the child's own failure"),
        "{:?}",
        ok.status()
    );
}

/// **A stop takes THIS session's children and nobody else's, and not the ones that are
/// gone** — the downward edge of the tree, at the size it can be asserted at.
///
/// The list is the TREE's (`Parts::tree_slots`: one list, so any ancestor can `task_result`
/// any handle in it), which is exactly why stopping needs the owner back: a root that
/// stopped every slot it could see would stop its own children AND their children in one
/// go, and a child would stop its siblings' work. The operator's design says otherwise —
/// *"we talk to parents and they own lifecycle"* — and one level per session is what makes
/// the walk down finite and each session answerable for exactly its own.
///
/// The second filter is the one that was measured into existence: a child that has ANSWERED
/// is still alive (a host child parks in `serve_child` and can be asked more) while a child
/// whose thread has ended has no reader for an interrupt at all. So "has an answer" cannot
/// stand for "is gone", and `TaskSlot::exited` is the fact that can.
#[test]
fn a_stop_takes_this_sessions_children_and_not_the_ones_that_are_gone() {
    // The tree's one list, with four entries: three of mine — one still computing, one
    // parked with an answer, one whose thread has ended — and one of my PARENT's, which is
    // a sibling of mine and must not be in my answer.
    let mine_running = Arc::new(TaskSlot::new("s-child"));
    let mine_answered = Arc::new(TaskSlot::new("s-child"));
    mine_answered.settle(letibot_tools::builtins::task::TaskStatus::Done {
        answer: "done, and still here".into(),
        child: None,
    });
    let mine_and_gone = Arc::new(TaskSlot::new("s-child"));
    mine_and_gone.mark_exited();
    // **A slot's handle follows its OWNER** (`<owner>-sub-N`, the shape
    // `HarnessTaskRunner::start` mints), so the parent's child cannot be called
    // `s-child-sub-*`: a name that says `s-child` on a slot the root started is the
    // confusion this whole test is about, and the owner field is what settles it.
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
    // session's. Same list, different answer — which is the whole reason the owner is on
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

/// **The wait for a child's thread is a wait and not a hope.**
///
/// `TaskSlot::wait_until_exited` is what a caller about to let the process exit uses to
/// know no child is still inside `libllama` when `exit()` finalizes it — the SIGSEGV this
/// was written for (`message_between_turns`, after every test had printed `ok`). Two
/// things have to be true of it and neither is free:
///
/// * **It does NOT return true for a child that is still here.** A wait that answered
///   "gone" on a live thread would make the crash invisible rather than fixed, which is
///   the worse of the two failures.
/// * **It returns as soon as the thread leaves, not at the deadline.** The caller gives a
///   deadline in seconds; a wait that spent it would make every shutdown as slow as its
///   own patience. The child here exits after 50 ms and the deadline is 30 s, so a return
///   within seconds is the child's own departure and not the clock.
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

/// **The sentence a stopped session leaves names what it stopped, and what it could not.**
///
/// The pure half of `stop_children_first`, and the reason it is asserted rather than the
/// `Warning` around it: a count that swallowed a refusal would be the claim-versus-fact
/// defect this tree keeps paying for — a reader told *stopped 2* while one of them is still
/// computing, and nothing anywhere saying so.
#[test]
fn a_stop_says_what_it_stopped_and_what_it_could_not() {
    // No children: no sentence at all, which is the ordinary case — a session with nothing
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

/// **A stop delivered to a running turn takes the session's CHILDREN with it first** — the
/// operator's design in one line, *"we talk to parents and they own lifecycle"*, and the
/// door an interrupt actually reaches a turn through.
///
/// The instrument is a fake runner, because the claim is about WHO IS TOLD and WHEN rather
/// than about anything a child does: the parent is stopped by the very message the arm
/// returns, and the children have to be told before it — a stop that waited for its subtree
/// would hold the turn it is stopping open.
///
/// **Two levels, because the recursion is the shape.** Each session's arm stops ITS OWN
/// runner and no other; the walk down is then one level per session, which is what makes it
/// finite and what makes every session answerable for exactly its own children.
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
    // OWN runner — which is what "one level per session" means in code.
    let stops = Arc::new(Mutex::new(Vec::new()));
    let parent_hub = Hub::new("s-root");
    let mut parent = HubSteering {
        hub: parent_hub.clone(),
        trail: None,
        injected: Arc::new(Mutex::new(VecDeque::new())),
        monitors: None,
        monitor_cursor: Arc::new(AtomicUsize::new(0)),
        job_watch: None,
        subagents: Some(Arc::new(Recording {
            me: "s-root".into(),
            stops: stops.clone(),
        })),
        held: VecDeque::new(),
    };
    let child_hub = Hub::new("s-child");
    let mut child = HubSteering {
        hub: child_hub.clone(),
        trail: None,
        injected: Arc::new(Mutex::new(VecDeque::new())),
        monitors: None,
        monitor_cursor: Arc::new(AtomicUsize::new(0)),
        job_watch: None,
        subagents: Some(Arc::new(Recording {
            me: "s-child".into(),
            stops: stops.clone(),
        })),
        held: VecDeque::new(),
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

    // **The ordering, which is the requirement and not tidiness.** The arm stops the
    // children on the way IN, before the urgent message it returns can stop the session —
    // and the notice is on the stopping session's own log, where the person who pressed
    // Esc is looking.
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

/// **R7's tool-side half: the sentence says the result comes to you.**
///
/// The text is the deliverable — it is what taught the model that waiting was the
/// only way to learn a result — so what it says is asserted rather than assumed. Two
/// things must be in it: the job's own identity (id, command, how it ended), and the
/// promise that there is nothing to wait for.
#[test]
fn a_completion_notice_names_the_job_and_says_do_not_wait() {
    let text = completion_notice(
        &[JobCompletion {
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
    assert!(text.contains("cargo build --release"), "{text}");
    assert!(text.contains("4.4s"), "how long it ran: {text}");
    assert!(text.contains("4096"), "how much it wrote: {text}");
    // The half that is the fix, not the footnote.
    assert!(text.contains("do not need to wait"), "{text}");
    // And it must not inline the output — only say where it is.
    assert!(text.contains("job_output"), "{text}");
}

/// **A prompt is answered, a parent's message is HEARD, a stop is a STOP for the whole
/// subtree, and everything else a child inherits is left, and named.**
///
/// The operator's ruling is what makes the first arm right — *"subagent is just another
/// agent… mid turn, post turn whatever"* — and the other arms are what keeps this door from
/// running the parent's machinery in the parent's name.
///
/// **The interrupt arm is the correction, and it is the measured one.** It used to say
/// *"nothing is running to interrupt"*, which was true of the session and false of its
/// subtree: a child between turns, with children of its own still computing, was told
/// nothing and left them running. See the supervision invariant on [`HarnessTaskRunner`].
///
/// **The message arm was the second correction** — the operator's ruling, verbatim: *"fix
/// task_message - it should enqueue"*. It used to `Leave` a message here on the grounds
/// that *"a message steers a turn in flight"*, which was true of the door it was written
/// for and false of a child whose queue is drained between turns: the parent was told
/// *nothing drains a child's queue between turns*, and what was actually missing was a
/// meaning for the entry that arrived. It has one now — a turn of its own, spoken as the
/// parent's (see [`Harness::submit_a_parents_message`]).
#[test]
fn a_child_answers_a_prompt_and_leaves_the_machinery_its_parent_owns() {
    use letibot_sessionlog::CommandKind as K;
    assert_eq!(
        child_command(&K::Prompt {
            text: "do this next".into()
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

/// **What the head attached to a subagent is told about a command the child leaves** — R67
/// item 15: the line it typed, named back, and where to send it instead.
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

/// **A progress bar is ONE line, and the window must show its last state.**
///
/// The operator's report, 2026-10-05: *"more often than not i'd like to enter the job and
/// hope to tail its out with ascii codes correctly processed"*. `cargo`, `docker` and `npm`
/// redraw a single row with `\r` and no newline in it, so splitting on `\n` alone put every
/// update on one row — `2%23%47%100%` — which is what they were reading.
#[test]
fn a_progress_bar_is_one_line_and_not_every_update_concatenated() {
    // The ordinary case: each redraw is longer, so the last one stands alone.
    assert_eq!(
        progress_lines("  2%\r 23%\r 47%\r100%"),
        vec!["100%"],
        "the row is what a terminal would end up showing"
    );
    // **A SHORTER final segment leaves the tail of the longer one standing** — that is
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

/// **A parent's correction reaches a running child as steering, and NOT as the
/// operator's words** — the operator's ruling, 2026-10-06: *"in the tree all subagents
/// must be addressable by their parents. that is how live corrections delivered."*
///
/// The path, end to end and without a daemon: the runner submits under the **daemon's**
/// name (the parent is seated in another session's hub), the kind carries the parent's
/// identity, and the child's own steering poll takes it at its next round boundary. What
/// this pins is the pair of facts a correction cannot afford to get wrong — it is not
/// `from_operator`, so it cannot authorise the act it races, coalesce with the operator's
/// text, or be taken back by a recall; and it is not `urgent`, so a child told to change
/// course finishes the call it is in rather than abandoning it.
///
/// The parent is named in the text because the trail cannot name it: `say` takes a
/// speaker kind, not an identity, and *which* parent spoke is the whole of what
/// addressability means.
#[test]
fn a_parents_message_is_the_childs_steering_and_not_the_operators_words() {
    let hub = Hub::new("s-child");
    let mut steering = HubSteering {
        hub: hub.clone(),
        // No trail: what is asserted is the message the child reads and how it is
        // scoped, and a trail would only add a second place for the same words.
        trail: None,
        injected: Arc::new(Mutex::new(VecDeque::new())),
        monitors: None,
        monitor_cursor: Arc::new(AtomicUsize::new(0)),
        job_watch: None,
        // No children either: what is asserted here is the message's scoping.
        subagents: None,
        held: VecDeque::new(),
    };

    // **What `HarnessTaskRunner::send` does**, exactly: the daemon's own name at the
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

/// **A failed turn's words come back out of the hub's steering, still the
/// operator's, and not twice.**
///
/// The hub half of the fix for the measured loss: a message is out of the hub's
/// queue the moment `try_next` dequeues it, and a turn that fails commits nothing
/// (§5.7), so the words go back to the source rather than being appended.
/// `give_back` holds them, and the next `try_next` drains them before the hub's
/// queue — with the flags they had when they were taken, because it is the same
/// `SteeringMessage` coming out again.
#[test]
fn a_handed_back_message_comes_out_of_the_next_poll_still_the_operators() {
    let hub = Hub::new("s-steer");
    let mut steering = HubSteering {
        hub: hub.clone(),
        trail: None,
        injected: Arc::new(Mutex::new(VecDeque::new())),
        monitors: None,
        monitor_cursor: Arc::new(AtomicUsize::new(0)),
        job_watch: None,
        subagents: None,
        held: VecDeque::new(),
    };

    // An interrupt queued behind the failed turn: it must come out AFTER the
    // handed-back words, which were taken before it. (The daemon may submit an
    // interrupt and nothing else that speaks as a person, so this is the door
    // the test goes through.)
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
    // **And the person can be asked.** The operator's ruling, 2026-10-09: *"yeah i
    // want you to be able to ask me for a choice"* — the seat carries it whether or
    // not a room is attached, because asking is not a room's business.
    assert!(
        root.tools.iter().any(|t| t == "ask_user_question"),
        "the leticode seat cannot ask the person a question: {:?}",
        root.tools
    );
    // The whole opencode union plus the room fits the ceiling, which moved to
    // twenty-four when `task_message` joined the delegation trio — see leticode's own
    // note on `max_tools` for why it took a seat rather than trading for one. A tool
    // added after this has to do the same: take a seat from something, or declare its
    // own number where somebody deciding can read it.
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
