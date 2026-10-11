use letibot_tools::builtins::intent::{Question, QuestionAnswer};

fn a_question() -> Question {
    Question {
        text: "which database should the migration target?".into(),
        options: vec!["postgres".into(), "sqlite".into()],
        because: "the two need different migration files".into(),
    }
}

fn asked(answer: QuestionAnswer) -> crate::answers::Asked {
    crate::answers::Asked {
        question: a_question(),
        answer,
        by: "dead".into(),
    }
}

/// **The answer row carries what the person said, and nothing of ours.**
///
/// The rule the `!` line's own row keeps — *their words, verbatim* — one vocabulary over.
/// A label, an index or a harness sentence glued to the front would be a citable utterance
/// of the wrong thing, and the trail cites this row.
#[test]
fn the_answer_row_is_the_persons_words_and_nothing_else() {
    // A choice is the OPTION'S TEXT — the label they were shown and picked. An index is
    // not something anybody said.
    assert_eq!(
        operator_answer_text(&asked(QuestionAnswer::choosing(1))).as_deref(),
        Some("sqlite")
    );
    // A note is theirs too, and it qualifies the choice, so it rides with it.
    assert_eq!(
        operator_answer_text(&asked(
            QuestionAnswer::choosing(0).with_note("only for the CUDA box")
        ))
        .as_deref(),
        Some("postgres\nonly for the CUDA box")
    );
    // Typed words are the whole answer.
    assert_eq!(
        operator_answer_text(&asked(QuestionAnswer::free("neither — split it in two"))).as_deref(),
        Some("neither — split it in two")
    );
    // And an abstention is the act, in the only word the wire carries for it.
    assert_eq!(
        operator_answer_text(&asked(QuestionAnswer::abstaining())).as_deref(),
        Some("abstain")
    );
    // **An index nobody offered has nothing of theirs in it.** The tool refuses one of
    // those, so this is the belt to that pair of braces rather than a state a session
    // reaches — and returning words here would be inventing a sentence for them.
    assert_eq!(
        operator_answer_text(&asked(QuestionAnswer::choosing(7))),
        None
    );
}

/// **The two rows: the question is the session's, the answer is the operator's.**
///
/// The speakers are the whole of what an `ALLOW <n>` rests on, so they are asserted on the
/// rows the session writes and not on a rendering.
#[test]
fn the_question_row_is_the_sessions_and_the_answer_row_is_the_operators() {
    let rows = asked_rows(&asked(QuestionAnswer::choosing(1)), "sqlite");
    let [question, answer] = rows;
    let text = |item: &TranscriptItem| match item {
        TranscriptItem::User { parts, .. } => parts
            .iter()
            .map(|p| match p {
                UserPart::Text { text } => text.clone(),
                other => format!("{other:?}"),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => panic!("not a user row: {other:?}"),
    };
    assert!(
        matches!(
            question,
            TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Agent,
                ..
            }
        ),
        "the question must never read back as the operator's words: {question:?}"
    );
    let asked_text = text(&question);
    assert!(
        asked_text.contains("which database should the migration target?")
            && asked_text.contains("postgres | sqlite"),
        "the question row must name the question and the options it offered: {asked_text}"
    );
    assert!(
        matches!(
            answer,
            TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Operator,
                ..
            }
        ),
        "the answer row must be the person's: {answer:?}"
    );
    assert_eq!(text(&answer), "sqlite");
}

/// **A job this daemon never ran is answered from the record, not refused.**
///
/// The operator, after a restart: entering a job showed nothing at all — `no job ... here`
/// about a job whose ending was written down. Three claims, and the third is the one a
/// reader acts on: the state word is the row's own, the bytes are admitted LOST rather than
/// silently absent, and a redirect names the file that still holds them (R41).
#[test]
fn a_recorded_job_says_what_happened_and_where_its_bytes_went() {
    let row = |state: &str, redirect: Option<&str>| JobRecord {
        handle: "j7".into(),
        command: "cargo test --release".into(),
        how: String::new(),
        state: state.into(),
        produced: 4_096,
        elapsed_ms: 8_000,
        redirect: redirect.map(str::to_string),
    };
    let said = recorded_job_lines(&row("exited 0", None)).join("\n");
    assert!(said.contains("exited 0"), "{said}");
    assert!(said.contains("8.0s"), "the duration is the row's: {said}");
    assert!(said.contains("4096 bytes"), "{said}");
    assert!(
        said.contains("not kept"),
        "the bytes are lost and the window must say so rather than look empty: {said}"
    );
    // **A word this function does not know is REPEATED, not interpreted.** A job whose
    // state is `not run (could not join its scope)` must not read here as one that ran,
    // which is why the sentence says *not in this daemon's table* and not *it ran*.
    let never = recorded_job_lines(&row("not run (could not join its scope)", None)).join("\n");
    assert!(
        never.contains("not run (could not join its scope)"),
        "{never}"
    );
    // **And the file, when the row knows it** — R41's other half: the pane cannot read the
    // bytes, so the one useful thing it can do is say where they are.
    let redirected = recorded_job_lines(&row("exited 0", Some("/tmp/build.log"))).join("\n");
    assert!(
        redirected.contains("/tmp/build.log"),
        "a redirected job's file is not named: {redirected}"
    );
}

/// **A condition is met by a job that is GONE — including one this daemon never knew** — the
/// operator's restart case, as the words the model is handed.
///
/// Three fates, three sentences, and the split is the operator's own: a handle that ended here
/// can be read with `job_output`, and one this daemon has never heard of cannot — so that line
/// says the result is not here rather than leaving the reader to find out by asking. His
/// words, which are the whole reason the fate is carried and not just the boolean:
/// *"you either follow the job result up manually (which is more robust) if next steps depend
/// on it or just do your things if it was just a timeline"*.
#[test]
fn a_conditions_sentence_says_which_kind_of_gone_it_was() {
    assert_eq!(
        fate_line(
            "j121",
            &JobFate::Ended {
                state: "exited 0".into()
            }
        ),
        "`j121` exited 0",
        "the job's own word, and nothing invented around it"
    );
    let unknown = fate_line("j121", &JobFate::Unknown);
    assert!(
        unknown.contains("not a job this session knows"),
        "the restart case must say the result is not here: {unknown}"
    );
    assert!(
        unknown.contains("read it up yourself"),
        "and say what the reader can do about it, since following it up is the robust half: \
             {unknown}"
    );
    // **A running job is not due, and the sentence exists anyway.** A function that panics on
    // the arm nobody reaches is a function whose contract is a rumour — and this one is
    // reachable the moment anything but `due_todo_lines` asks.
    assert!(
        fate_line("j7", &JobFate::Running).contains("still running"),
        "the running arm must answer rather than be unreachable"
    );
}

/// **THE EVALUATOR: what the world says about a handle, and which rows that makes due.**
///
/// The decision a conditioned row turns on, and it had **no assertion at all** before this:
/// reaching it through a `Harness` needs a backend, a registry, a gate and a store, so the only
/// thing pinned was [`fate_line`]'s wording — the sentence, and not the choice that picks it.
///
/// The operator's acceptance case is the one that decides the design: *"a todo conditioned on
/// job end, and then i restart head and harnessd. once server is back it should fire - job is
/// gone"*. After that restart the process table is empty and the handle is a job this daemon
/// never ran, so **`Unknown` must meet the condition** — a decision that read *no row, no fate,
/// skip it* would leave the row waiting for ever on work that is already done, which is the one
/// failure this whole mechanism exists to prevent.
#[test]
fn the_evaluator_fires_a_row_whose_handle_is_gone() {
    use letibot_tokencore::store::{TodoBy, TodoCondition, TodoItem, TodoStatus};
    /// A stored row for one handle, as a restart would find it — the ending written down
    /// before the daemon died.
    fn record(handle: &str, state: &str) -> JobRecord {
        JobRecord {
            handle: handle.into(),
            command: "cargo test".into(),
            how: "asked".into(),
            state: state.into(),
            produced: 12,
            elapsed_ms: 4_000,
            redirect: None,
        }
    }
    let row = |content: &str, when: Option<TodoCondition>| TodoItem {
        content: content.into(),
        status: TodoStatus::Pending,
        by: TodoBy::Operator,
        when,
        needs: Vec::new(),
    };
    let waiting_on = |handle: &str| {
        Some(TodoCondition::Job {
            handle: handle.into(),
        })
    };
    // **The same row, set aside.** Nothing else about it changes — the words, the author and
    // the handle are `row`'s — so what the filter below is being asked about is the STATE and
    // not some other difference between two fixtures.
    let set_aside = |content: &str, when: Option<TodoCondition>| TodoItem {
        status: TodoStatus::Postponed,
        ..row(content, when)
    };
    // **The same row again, finished.** The fourth skip below, and it is the one this change
    // widens rather than adds — see `due_rows`: the predicate is the idle check's, and a row
    // that is already done is not work it may speak about.
    let finished = |content: &str, when: Option<TodoCondition>| TodoItem {
        status: TodoStatus::Completed,
        ..row(content, when)
    };

    // **The order of the two looks, which is the whole of the honesty.** The live table answers
    // first: only the process host can say a job runs *now*, so a record saying `running` — a
    // job the daemon was watching when it died — must not override a host that says otherwise.
    assert_eq!(
        fate_of(
            Some(letibot_tools::exec::JobState::Running),
            Some(&record("j7", "running")),
        ),
        JobFate::Running,
        "the live table is the first look, so the record cannot say otherwise"
    );
    assert_eq!(
        fate_of(
            Some(letibot_tools::exec::JobState::Exited { code: 0 }),
            Some(&record("j7", "running")),
        ),
        JobFate::Ended {
            state: "exited 0".into()
        },
        "the host's own word for it, and not the stale row's"
    );
    // **The restart, which is why the record is the second look**: no process table at all, and
    // the ending this daemon wrote down before it went away.
    assert_eq!(
        fate_of(None, Some(&record("j121", "exited 3"))),
        JobFate::Ended {
            state: "exited 3".into()
        },
        "the record is what survives a restart"
    );
    // And a handle nobody has run: met, and not dressed up as an ending — the distinction is
    // the operator's, since the result can be read up for one and not for the other.
    assert_eq!(
        fate_of(None, None),
        JobFate::Unknown,
        "a job this daemon never heard of has ended; that is what a restart looks like"
    );

    // **The filter, over a board the world answers for.** Four skips — a row with no
    // condition, a running job, a row the operator has set aside, and a row already done — and
    // two firings, one of each kind of gone.
    let rows = vec![
        row("ship the parity row", None),
        row("push once CI lands", waiting_on("j121")),
        row("wait for the build", waiting_on("j7")),
        row("after the restart", waiting_on("j900")),
        // **A POSTPONED row does not fire, and it keeps its handle.** The handle is the same
        // `j121` the row above is due on, so a firing here could only come from the state —
        // and the absence of one is the whole of what *"persists but without nag"* means for
        // a conditioned row. Nothing in `due_rows` clears the condition: lifting the
        // postponement puts the row back in front of this same filter, which is what makes
        // the state reversible rather than a quiet way to drop a firing.
        set_aside("push once CI lands", waiting_on("j121")),
        // **And a FINISHED row does not fire either**, again on `j121`, which is the same
        // handle the open row above is due on: a firing here could only come from the state.
        // The condition is not cleared by any of this, so the row keeps its handle — what the
        // skip buys is silence about work that is over.
        finished("push once CI lands", waiting_on("j121")),
    ];
    let due = due_rows(
        &rows,
        &letibot_tools::builtins::todo::ChildSessions::none(),
        &letibot_tools::builtins::todo::RunningJobs::none(),
        |handle| match handle {
            "j7" => JobFate::Running,
            "j121" => JobFate::Ended {
                state: "exited 0".into(),
            },
            _ => JobFate::Unknown,
        },
    );
    assert_eq!(
        due,
        vec![
            (
                "push once CI lands".to_string(),
                "j121".to_string(),
                JobFate::Ended {
                    state: "exited 0".into()
                }
            ),
            (
                "after the restart".to_string(),
                "j900".to_string(),
                JobFate::Unknown
            ),
        ],
        "a row with no condition is not due, a running job is not due, a row the operator \
             postponed is not due, a row already done is not due, and both kinds of gone are — with \
             the handle the caller needs to consume the condition: {due:?}"
    );
}

/// **A subagent's ask is addressed to the tree's ROOT, never to its parent** — R58's ask
/// route, and the one sentence in the spawn that has to be right for a card to reach a
/// head at all.
///
/// The GRANDCHILD case is what decides it: at depth ≥ 1 the parent is itself a child, so
/// addressing the parent would post the card on a session no head can be attached to and
/// no worker drives — a card nobody would ever see, which is the defect this whole change
/// is against. The fixture is therefore a root, a child and a grandchild, built the way
/// `jobwatch`'s settlement test builds them.
#[test]
fn a_subagents_ask_is_addressed_to_the_tree_root_and_not_to_its_parent() {
    use letibot_sessionlog::registry::{Registry, SessionWiring};

    let registry = Registry::new();
    // The root is the session a daemon drives; the child and the grandchild are the ones
    // it does not. All three are registered, which is what `adopt` does at a spawn.
    let root_hub = registry
        .create("s-root", "", SessionWiring::default())
        .expect("the root registers");
    let _child_hub = registry
        .create_under(
            "s-child",
            "",
            SessionWiring::default(),
            Some("s-root".into()),
        )
        .expect("the child registers");
    let grandchild_hub = registry
        .create_under(
            "s-child-sub-1",
            "",
            SessionWiring::default(),
            Some("s-child".into()),
        )
        .expect("the grandchild registers");

    // The answers the whole tree reaches: the root's own, handed down through `Parts`.
    let answers = Some(Arc::new(crate::answers::Answers::new()));
    // The grandchild's set is built FROM the root's, which is what gives it the ROOT —
    // the same set `Parts` carries for a grandchild.
    let root_watch = crate::jobwatch::JobWatchers::watching_tasks(&root_hub, None);
    let grandchild_watch = crate::jobwatch::JobWatchers::watching_tasks(&grandchild_hub, None)
        .shares_tree(&root_watch);

    let (root, head) = subagent_ask_target(
        &registry,
        Some(&grandchild_watch),
        "s-child",
        answers.as_ref(),
    );
    assert_eq!(
        root, "s-root",
        "the card was addressed to the parent, not to the tree's root"
    );
    assert!(
        head.is_some(),
        "the root's answers did not reach the child's ask"
    );

    // **A tree with no head-wired adjudicator**: no card is posted, and the child's own
    // refusal names the root instead (see `answers::SubagentAdjudicator`).
    let (root, head) = subagent_ask_target(&registry, Some(&grandchild_watch), "s-child", None);
    assert_eq!(root, "s-root");
    assert!(head.is_none());

    // **A root this daemon does not hold** — a harness with no daemon behind it, or a
    // registry that has lost the session — is the same refusal rather than a card on a
    // hub nothing will read. With no watcher set at all, the parent IS the tree's root.
    let (root, head) = subagent_ask_target(&registry, None, "s-absent", answers.as_ref());
    assert_eq!(root, "s-absent");
    assert!(head.is_none(), "a hub nobody holds cannot answer");
}

/// **A row announced through a capturing sink is filled by the pairing rule, and
/// body-less without it** — the compaction defect at the smallest size there is.
///
/// This is the free half of `Harness::reconcile` (`reconcile_rows`), driven the way
/// the summary-turn helper drives it. The false-first direction is the assertion that
/// matters: a sink announces on `emit` and carries the id, so a caller that drops the
/// sink has announced a row it never filled — which is what the daemon's view then
/// serves to every head and to every `/resync`.
#[test]
fn an_announced_row_is_filled_by_the_pairing_rule_and_body_less_without_it() {
    use super::*;
    let item = || TranscriptItem::System {
        text: "the summary instruction".into(),
        origin: letibot_transcript::SystemOrigin::Update,
    };
    let announce = |sink: &mut CapturingSink| {
        sink.emit(letibot_turn::TurnEvent::TranscriptAppended {
            item_id: "s#t0.9".into(),
            kind: "system",
            ledger_head: "beef".into(),
            tokens: 3,
        });
    };

    // The defect: announced, sink dropped, no body. The view holds the row.
    let hub = Hub::new("s");
    let mut sink = CapturingSink::new(hub.clone());
    announce(&mut sink);
    drop(sink);
    let snap = hub.snapshot();
    assert_eq!(snap.items.len(), 1, "the announcement reached the log");
    assert!(
        snap.items[0].item.is_none(),
        "the premise: nothing filled it"
    );

    // The rule: same announcement, paired. The row carries its body.
    let hub = Hub::new("s");
    let mut sink = CapturingSink::new(hub.clone());
    announce(&mut sink);
    reconcile_rows(&hub, &mut sink, std::slice::from_ref(&item()));
    let snap = hub.snapshot();
    assert_eq!(snap.items.len(), 1);
    assert!(
        snap.items[0].item.is_some(),
        "announced and never filled: {:?}",
        snap.items[0].item_id
    );
}
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

/// A ledger and its items, built row by row with tokens of a given count — the
/// shape `Session::append_items` keeps (one row per item), with no vocabulary.
struct Wall {
    ledger: letibot_tokencore::ledger::TokenLedger,
    items: Vec<TranscriptItem>,
}

impl Wall {
    fn new(prefix: usize) -> Wall {
        Wall {
            ledger: letibot_tokencore::ledger::TokenLedger::new("t-wall", &vec![1; prefix])
                .expect("a ledger"),
            items: Vec::new(),
        }
    }
    fn push(&mut self, item: TranscriptItem, tokens: usize) -> &mut Wall {
        let id = format!("t-wall.{}", self.items.len());
        self.ledger
            .append(&id, &vec![2; tokens])
            .expect("an append");
        self.items.push(item);
        self
    }
    fn sent(&mut self, tokens: usize) -> &mut Wall {
        self.push(
            TranscriptItem::System {
                text: "sent".into(),
                origin: SystemOrigin::Update,
            },
            tokens,
        )
    }
    fn thought(&mut self, tokens: usize) -> &mut Wall {
        self.push(
            TranscriptItem::Reasoning {
                text: "thinking".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            },
            tokens,
        )
    }
    fn len(&self) -> u64 {
        self.ledger.len() as u64
    }
    fn anchor(&self, provider: u64) -> super::WallAnchor {
        super::WallAnchor {
            transcript: "t-wall".into(),
            ledger: self.len(),
            provider,
            measured: true,
        }
    }
}

fn deepseek_window() -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.context_window = Some(1_000_000);
    cfg.auto_compact = true;
    cfg
}

/// **R72: a reasoning-heavy history is judged at the window's REAL edge.**
///
/// The provider counted 880k at its last round, over a ledger ~3.4× that (the
/// reasoning it is not sent). Then a round fails inside its own reasoning — R7's
/// *"the turn spent its whole output on reasoning"* — which commits 200k ledger
/// tokens of thought and measures nothing, and a 12k tool result lands. The
/// provider will be sent 892k of a 1M window whose wall is at 937.5k: room.
///
/// The ratio reading — `ledger ÷ (ledger, provider)` as learned at the round —
/// counted the 200k of unsent thought at nearly a third of its size and the
/// session past the wall (941.7k against 937.5k), compacting a conversation with
/// 108k of its window free.
#[test]
fn a_reasoning_heavy_history_meets_the_wall_at_the_providers_count() {
    let mut w = Wall::new(20_000);
    // ~3.4 ledger tokens per provider token: 880k sent, ~2.1M of thought.
    for _ in 0..100 {
        w.sent(8_600).thought(21_400);
    }
    let anchor = w.anchor(880_000);
    let at_round = w.len();
    w.thought(200_000).sent(12_000);

    let (_, now) = super::roll_resident(&anchor, "t-wall", &w.ledger, &w.items, true);
    assert_eq!(
        now.tokens, 892_000,
        "the provider's 880k + the 12k it will be sent"
    );
    assert_eq!(now.measured, Some(880_000));
    assert_eq!(now.since, 12_000);

    let mut cfg = deepseek_window();
    cfg.ledger_scale = Some((w.len(), now.tokens));
    assert!(
        !cfg.should_compact(w.len()),
        "892k of 1M is not the wall (937.5k); compacting here is 27% early"
    );
    assert_eq!(
        cfg.shown_tokens(w.len()),
        892_000,
        "the line prints the same count"
    );
    // The old reading, for the record: the round's ratio applied to today's ledger.
    let old = (w.len() as u128 * 880_000 / at_round as u128) as u64;
    assert!(
        old > 937_500,
        "the ratio reading put it past the wall: {old}"
    );

    // And at the real edge it fires: a 50k tool result takes it to 942k.
    w.sent(50_000);
    let (_, now) = super::roll_resident(&anchor, "t-wall", &w.ledger, &w.items, true);
    assert_eq!(now.tokens, 942_000);
    cfg.ledger_scale = Some((w.len(), now.tokens));
    assert!(
        cfg.should_compact(w.len()),
        "942k + 62.5k reserve is past 1M"
    );
}

/// **R72: a restart whose last append was a notice comes back at the provider's
/// count, and does not compact.**
///
/// MEASURED 2026-10-11, leticl's `s-1791151788256943467` after a daemon restart:
/// `compacting: 3454266 of 1000000 tokens resident` → `compacted: 75408` — a
/// session the provider counted at ~740k, because the stored pair's ledger no
/// longer EQUALLED the rebuilt one (something was appended after the last round),
/// so the window went unscaled and the raw ledger was compared with it.
#[test]
fn a_restart_after_a_notice_recovers_the_providers_count_and_does_not_compact() {
    let mut w = Wall::new(20_000);
    for _ in 0..100 {
        w.sent(7_200).thought(26_900);
    }
    // The last round: its prompt was this long, and the provider counted 740k.
    let (stored_ledger, stored_provider) = (w.len(), 740_000u64);
    // Then the round's reply (thought and a short answer) and a notice.
    w.thought(40_000).sent(1_500).sent(600);

    let anchor =
        super::recovered_scale(Some(stored_provider), Some(stored_ledger), w.len() as usize)
            .expect("a later append does not throw the count away");
    let anchor = super::WallAnchor {
        transcript: "t-wall".into(),
        ledger: anchor.0,
        provider: anchor.1,
        measured: true,
    };
    let (_, now) = super::roll_resident(&anchor, "t-wall", &w.ledger, &w.items, true);
    assert_eq!(now.tokens, 742_100, "740k + the answer and the notice");

    let mut cfg = deepseek_window();
    cfg.provider = Some(crate::config::ProviderConfig {
        name: "deepseek".into(),
        ..Default::default()
    });
    cfg.ledger_scale = Some((w.len(), now.tokens));
    assert!(
        w.len() > 3_400_000,
        "the ledger is the 3.45M the log printed"
    );
    assert!(
        !cfg.should_compact(w.len()),
        "742k of 1M must not compact on the first idle pass after a restart"
    );
    assert!(!cfg.tokens_are_unscaled());
}

/// **A fork re-bases the count on the new base**, rather than applying the
/// history's ratio to a summary whose reasoning share is nothing like it; and a
/// local session counts every row, because its prompt IS the ledger.
#[test]
fn a_fork_rebases_the_count_and_a_local_session_counts_the_ledger() {
    let mut w = Wall::new(5_000);
    w.sent(1_000).thought(4_000).sent(2_000);
    let stale = super::WallAnchor {
        transcript: "t-before-the-fork".into(),
        ledger: 3_000_000,
        provider: 900_000,
        measured: true,
    };
    let (rebased, now) = super::roll_resident(&stale, "t-wall", &w.ledger, &w.items, true);
    assert_eq!(
        now.tokens, 8_000,
        "prefix + what is sent, the thought left out"
    );
    assert_eq!(now.measured, None, "nothing measured the new base yet");
    assert_eq!(rebased.ledger, w.len());
    assert!(!rebased.measured);
    // The next roll adds only what came after the re-base.
    w.sent(500);
    let (_, next) = super::roll_resident(&rebased, "t-wall", &w.ledger, &w.items, true);
    assert_eq!(next.tokens, 8_500);

    // Local: the ledger is the prompt, reasoning and all.
    let anchor = super::WallAnchor {
        transcript: "t-wall".into(),
        ledger: u64::from(w.ledger.prefix_len()),
        provider: 5_000,
        measured: true,
    };
    let (_, local) = super::roll_resident(&anchor, "t-wall", &w.ledger, &w.items, false);
    assert_eq!(local.tokens, w.len(), "ratio 1 by construction");
}

/// The closing line names its unit and says when the after-count is an estimate.
#[test]
fn the_compacted_line_is_one_unit_said_once() {
    let base = super::Resident {
        tokens: 75_408,
        measured: None,
        since: 0,
    };
    let said = super::compacted_line(937_686, base, "deepseek tokens", true);
    assert!(
        said.starts_with("compacted: 937686 → 75408 deepseek tokens resident"),
        "{said}"
    );
    assert!(said.contains("the next round measures it"), "{said}");
    assert_eq!(
        super::compacted_line(240_000, base, "tokens", false),
        "compacted: 240000 → 75408 tokens resident."
    );
}

/// **The wall's turn failure is the provider's count with its unit** (R72 follow-up,
/// defect A). MEASURED on a scratch deepseek session at a 40,000-token window: the
/// head drew *"── FAILED — stopped after 1 round(s) at the context wall: 56466 of
/// 41843 tokens resident"* while the log for the same wall said *"53978 of 40000
/// deepseek tokens (56466 of 41843 ledger tokens)"* — the ledger's pair, no unit,
/// and a window nobody configured.
#[test]
fn the_wall_failure_says_the_providers_count_and_names_its_unit() {
    let said = super::HarnessError::ContextWall {
        rounds: 1,
        resident: 53_978,
        window: 40_000,
        unit: "deepseek tokens".into(),
        ledger: (56_466, 41_843),
    }
    .to_string();
    assert!(
        said.starts_with(
            "stopped after 1 round(s) at the context wall: 53978 of 40000 deepseek \
                 tokens resident (56466 of 41843 ledger tokens) and the next round"
        ),
        "{said}"
    );
    // A local session: the ledger IS the count, so it is said once.
    let local = super::HarnessError::ContextWall {
        rounds: 2,
        resident: 31_000,
        window: 32_768,
        unit: "tokens".into(),
        ledger: (31_000, 32_768),
    }
    .to_string();
    assert!(
        local.starts_with(
            "stopped after 2 round(s) at the context wall: 31000 of 32768 tokens \
                 resident and the next round"
        ),
        "{local}"
    );
}

/// Round 0 stops only past the window; later rounds inside the reserve (defect D).
#[test]
fn round_zero_stops_past_the_window_and_later_rounds_inside_the_reserve() {
    assert!(!super::wall_stops_round(0, 39_000, 5_000, 2_500, 40_000));
    assert!(super::wall_stops_round(0, 40_001, 5_000, 2_500, 40_000));
    assert!(super::wall_stops_round(1, 37_500, 5_000, 2_500, 40_000));
    assert!(!super::wall_stops_round(1, 37_499, 5_000, 2_500, 40_000));
    // Message zero alone past the window: nothing a compaction can fix, so round 0
    // is left to the server rather than stopped into a compaction known to fail.
    assert!(!super::wall_stops_round(0, 5_600, 5_443, 128, 512));
}

/// **Infinity is `0`, and it is one line, so it gets one test.**
///
/// The operator: *"make 200 tool calls limit configurable and set it to
/// infinity"*. `0` reaching the loop as `0` would mean a turn that may take no
/// rounds at all — the exact opposite — and it is the kind of inversion that
/// looks right in a diff.
#[test]
fn a_zero_backstop_is_unbounded_and_every_other_number_is_itself() {
    assert_eq!(super::round_backstop(0), usize::MAX, "0 means no bound");
    assert_eq!(super::round_backstop(1), 1);
    assert_eq!(super::round_backstop(200), 200);
    // And the default is the unbounded one, which is the half of this the
    // operator actually asked for.
    assert_eq!(
        super::round_backstop(Config::for_this_box("/tmp").max_tool_rounds),
        usize::MAX
    );
}

/// **`next` is the offset to ask at, not a count, and it stops at the end.**
///
/// A pane pages by handing `next` straight back as the next request's offset,
/// so it has to be the offset the window ended at — and it has to be `None`
/// exactly when the ring holds nothing further, or the pane offers a page that
/// comes back empty and reads as the job's end.
#[test]
fn a_job_window_names_the_next_offset_until_the_ring_runs_out() {
    let slice = |from, to, produced, dropped, retained| letibot_tools::exec::OutputSlice {
        bytes: Vec::new(),
        from,
        to,
        produced,
        dropped,
        retained,
    };
    // A window in the middle of a long job: there is more, and `next` is where
    // this one ended.
    assert_eq!(next_job_offset(&slice(0, 16, 40, 0, 40)), Some(16));
    // The window has taken everything still held: no further page.
    assert_eq!(next_job_offset(&slice(0, 40, 40, 0, 40)), None);
    // A ring that dropped its front: the end of what is *held* is the end, even
    // though `produced` is past `to` — that part is gone, not waiting.
    assert_eq!(next_job_offset(&slice(992, 1000, 1000, 992, 8)), None);
    // And a window that stopped short of the retained tail still has a page.
    assert_eq!(next_job_offset(&slice(992, 996, 1000, 992, 8)), Some(996));
}

/// **THE WINDOW OF A REDIRECTED JOB IS THE FILE'S TAIL** — R41's other half, and the
/// operator's own next ask after the pane learned to name the file: *"job pane can tail
/// the file"*.
///
/// `offset == 0` is the TAIL, because that is what a fresh read of an output pane means;
/// any other offset is a forward window from there, so `→` still walks to the end of a
/// long log the way it does for the ring.
#[test]
fn a_redirected_jobs_window_is_the_tail_of_its_file() {
    let dir = std::env::temp_dir().join(format!("letibot-tail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch");
    let path = dir.join("build.log");
    let body: String = (0..40).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&path, &body).expect("write");
    let path = path.to_str().expect("utf-8 path");

    // **A fresh read is the tail**: the last `limit` bytes, and the pane is told the
    // FILE's length so its `of N` is the file and not the window.
    let w = tail_file(path, 0, 20).expect("a window");
    assert_eq!(w.produced, body.len() as u64);
    assert_eq!(
        w.to,
        body.len() as u64,
        "the tail ends at the end of the file"
    );
    assert_eq!(w.from, body.len() as u64 - 20);
    assert_eq!(w.text.len(), 20);
    assert!(w.text.ends_with("line 39\n"), "{:?}", w.text);

    // **An explicit offset is a forward window**, which is what `next` hands to `→`.
    let w2 = tail_file(path, w.from, 20).expect("a window");
    assert_eq!(w2.from, w.from);
    assert_eq!(w2.to, w.from + 20);

    // A limit larger than the file is the whole file, not an error and not padding.
    let all = tail_file(path, 0, 4096).expect("a window");
    assert_eq!(all.from, 0);
    assert_eq!(all.to, body.len() as u64);

    // **A file that is not there is `None`** — the job has not created it yet — which is
    // what the caller answers with the sentence that names the path.
    assert!(tail_file(&dir.join("nope.log").to_string_lossy(), 0, 10).is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

// **The fixtures for the window tests below: real jobs, really run.** The decision
// those tests hold to is `job_window_for`, which `Harness::job_output_window` hands
// its two reads; the honest way to exercise it is with a command a real `/bin/sh`
// ran, a capture ring that really holds what the preamble wrote, and a file the
// work's stage really created — the same substrate `jobwatch`'s tests spawn on,
// and the only one that can prove the fixture is the operator's measurement.
mod job_tail {
    use super::*;
    use letibot_tools::ProcessHost as _;
    use letibot_tools::exec::{HostProcesses, ScopeKind, SpawnRequest};
    use std::time::Duration;

    /// Run `command` to its end and hand back the three things the window is
    /// decided on: the command, its state, and its capture. A job that does not
    /// exit 0 fails here, so a broken fixture never reaches the assertions as a
    /// mystery.
    fn a_settled_job(
        host: &HostProcesses,
        command: &str,
    ) -> (
        String,
        letibot_tools::exec::JobState,
        letibot_tools::exec::OutputSlice,
    ) {
        let id = host
            .spawn(&SpawnRequest {
                command: command.to_string(),
                // Nobody named this run: the test's own fixture.
                slug: None,
                cwd: "/".into(),
                scope: ScopeKind::Session,
                scope_name: None,
                background: true,
                env: vec![],
                tty: false,
                memory: None,
            })
            .expect("spawn");
        match host.wait_job(&id, Duration::from_secs(30)) {
            Ok(letibot_tools::exec::Waited::Happened {
                state: Some(letibot_tools::exec::JobState::Exited { code: 0 }),
                ..
            }) => {}
            other => panic!("the fixture did not run clean: {other:?}"),
        }
        let view = host.job(&id).expect("the job is still in the table");
        let slice = host
            .output(&id, 0, JOB_OUTPUT_WINDOW)
            .expect("the capture reads");
        (view.command, view.state, slice)
    }

    /// **A host over a temp root — or nothing, when this box cannot delegate a
    /// cgroup v2 subtree.**
    ///
    /// A job needs a process-lifetime tree, and a box without one is a real thing:
    /// CI's runner is one, MEASURED on run 37925350568 — *"mkdir
    /// /sys/fs/cgroup/system.slice/hosted-compute-agent.service/letibot.7443.2:
    /// Permission denied (os error 13) — a delegated cgroup v2 subtree is needed;
    /// …/hosted-compute-agent.service is the harness's own cgroup and it is not
    /// writable"*. A `#[ignore]` here would be a green suite that measured nothing,
    /// so the absence is SAID and `LETIBOT_REQUIRE_APPARATUS=1` refuses it — the same
    /// gate over the same predicate this crate's other host-spawning tests take (see
    /// `jobwatch.rs`).
    fn a_host(name: &str) -> Option<(std::path::PathBuf, std::sync::Arc<HostProcesses>)> {
        let Some(_) = letibot_tokencore::apparatus::present(
            "a process-lifetime tree (cgroup v2; process groups on macOS)",
            letibot_tools::host_tree().is_ok(),
        ) else {
            return None;
        };
        let root = std::env::temp_dir().join(format!(
            "letibot-jobtail-{name}-{pid}",
            pid = std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("scratch");
        let host = HostProcesses::new(&root)
            .expect("the tree probed writable above, so this root builds one");
        Some((root, std::sync::Arc::new(host)))
    }

    /// **THE MEASURED DEFECT, j87's own shape**: a compound command whose preamble
    /// writes to the capture and whose work redirects to a file. Read from the
    /// operator's store — `exited 0`, produced 2426, redirect `/tmp/tpcc-trace2.log`,
    /// the file NOT read — with j70 (`produced 0`, same redirect, worked) proving
    /// the empty capture was the only case the pane tailed.
    ///
    /// The window must answer with the FILE's tail whatever the capture holds, and
    /// the capture's bytes must be counted, not hidden: the fresh read says how many
    /// are outside the window and which file the window is.
    #[test]
    fn a_preamble_in_the_capture_does_not_stop_the_window_being_the_file() {
        let Some((root, host)) = a_host("compound") else {
            return;
        };
        let log = root.join("work.log");
        let command = format!(
            "sleep 0.1; echo '=== MIX (load + validate + mix) ==='; \
                 printf 'work-a\\nwork-b\\n' > {} 2>&1",
            log.display()
        );
        let (command, state, slice) = a_settled_job(&host, &command);

        // **The fixture is the measurement or it is nothing**: the capture holds the
        // preamble — bytes, produced > 0 — and not one byte of the work.
        assert!(slice.produced > 0, "j87's shape needs a non-empty capture");
        assert!(slice.text().contains("=== MIX"));
        assert!(
            !slice.text().contains("work-a"),
            "the work must be in the file, not the capture: {:?}",
            slice.text()
        );

        let w = job_window_for(&command, &state, &slice, 0, JOB_OUTPUT_WINDOW);
        let file_len = std::fs::metadata(&log).expect("the work ran").len();
        assert_eq!(w.produced, file_len, "the denominator is the FILE's length");
        assert_eq!(w.from, 0);
        assert_eq!(w.to, file_len);
        let drawn = w.lines.join("\n");
        assert!(
            drawn.contains("work-b"),
            "the file's tail is the window: {drawn}"
        );
        assert!(
            !drawn.contains("=== MIX"),
            "the preamble is not the window any more: {drawn}"
        );
        // **The preamble is disclosed, not silently dropped.** The line names the
        // count that stayed in the capture and the file the window is over.
        assert!(
            drawn.contains(&slice.produced.to_string()) && drawn.contains("work.log"),
            "the fresh read must count what it is not showing: {drawn}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **j70's case must not regress**: a redirect whose capture is empty — the
    /// `produced == 0` the old condition required — still tails the file, and the
    /// window gains no phantom disclosure about bytes that were never there.
    #[test]
    fn a_redirect_with_an_empty_capture_still_tails_and_claims_no_preamble() {
        let Some((root, host)) = a_host("empty-capture") else {
            return;
        };
        let log = root.join("solo.log");
        let command = format!("printf 'solo\\n' > {} 2>&1", log.display());
        let (command, state, slice) = a_settled_job(&host, &command);

        assert_eq!(slice.produced, 0, "the fixture needs the empty capture");
        let w = job_window_for(&command, &state, &slice, 0, JOB_OUTPUT_WINDOW);
        assert_eq!(w.produced, 5);
        // The file's own lines, and nothing bolted on: no phantom disclosure
        // about a preamble that was never there.
        let body = std::fs::read_to_string(&log).expect("the work ran");
        assert_eq!(
            w.lines,
            progress_lines(&body),
            "the file, and only the file"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A `2>&1` merge is not a redirect** — it is what makes the capture complete,
    /// so the window is the capture. R41's parser guarantees it; this holds the
    /// window path to the same guarantee end to end.
    #[test]
    fn a_merged_descriptor_keeps_the_capture_as_the_window() {
        let Some((root, host)) = a_host("merge") else {
            return;
        };
        let (command, state, slice) = a_settled_job(&host, "echo merged 2>&1");
        assert!(slice.produced > 0);

        let w = job_window_for(&command, &state, &slice, 0, JOB_OUTPUT_WINDOW);
        assert_eq!(
            w.produced, slice.produced,
            "the denominator is the capture's"
        );
        assert_eq!(w.lines, progress_lines(&slice.text()));
        assert!(w.lines.join("\n").contains("merged"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A job with no redirect is unaffected**: the capture is the window, exactly
    /// as before — the redirect branch must not have widened into this case.
    #[test]
    fn a_job_with_no_redirect_keeps_the_capture_as_the_window() {
        let Some((root, host)) = a_host("plain") else {
            return;
        };
        let (command, state, slice) = a_settled_job(&host, "echo plain");
        assert!(slice.produced > 0);

        let w = job_window_for(&command, &state, &slice, 0, JOB_OUTPUT_WINDOW);
        assert_eq!(w.produced, slice.produced);
        assert_eq!(w.lines, progress_lines(&slice.text()));
        assert!(w.lines.join("\n").contains("plain"));
        let _ = std::fs::remove_dir_all(&root);
    }
}

use super::*;
use letibot_tools::authorise::TrailProvenance;

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

/// **The name reaches the transcript, and a job nobody named reads exactly as it did.**
///
/// The operator's own correction, verbatim: *"yes, change rano, and i was look at the
/// transcript actually, not jobs pane"* — their complaint (*"j57 doesnt ringe the bell for
/// me"*) was about THIS line, the one a settlement puts in the conversation, and a counter
/// is not a name. So the bullet carries [`letibot_tools::exec::job_label`]'s spelling: the
/// same string `bash`'s backgrounded result printed when the job was started, and the same
/// one `/job` draws.
///
/// **And the unnamed half is a regression test rather than a courtesy.** A job nobody named
/// must read byte for byte as it did before a name existed — the tree's rule for a defaulted
/// field — and it does, because the label's fallback IS the id. The two notices below differ
/// in the name and nowhere else, which is what makes that a property instead of a hope.
#[test]
fn a_settlement_carries_the_jobs_name_and_an_unnamed_one_is_unchanged() {
    let settled = |job: &str, slug: Option<&str>| JobCompletion {
        kind: BackgroundKind::Job,
        owner: "s-me".into(),
        job: job.into(),
        command: "cargo test".into(),
        slug: slug.map(str::to_string),
        state: "exited 0".into(),
        produced: 1_030,
        elapsed_ms: 54_900,
        detail: String::new(),
        memory: None,
    };

    let named = completion_notice(&[settled("j65", Some("release-build"))], true);
    assert!(
        named.contains(
            "  - `release-build · j65` exited 0 after 54.9s, wrote 1030 bytes: cargo test\n"
        ),
        "the settlement must name the job the way the bash result did: {named}"
    );

    // **The handed-up variant carries the same name.** One bullet builder, so this is the
    // same sentence under the other heading — and the owner is still named beside it.
    let handed = completion_notice(&[settled("j65", Some("release-build"))], false);
    assert!(
        handed.contains("`release-build · j65` exited 0") && handed.contains("started by `s-me`"),
        "{handed}"
    );

    let bare = completion_notice(&[settled("j65", None)], true);
    assert!(
        bare.contains("  - `j65` exited 0 after 54.9s, wrote 1030 bytes: cargo test\n"),
        "{bare}"
    );
    // **The name is the only difference.** Strip it from the named notice and what is left
    // is the unnamed one, byte for byte — so "nothing regresses for those who do not use it"
    // is asserted rather than argued.
    assert_eq!(named.replace("release-build · ", ""), bare);
}

/// A job whose view was already reaped still produces a usable notice: the command
/// is the one field the durable settlement does not carry, so it is the one that can
/// be missing, and the notice says so rather than inventing one.
#[test]
fn a_reaped_jobs_completion_says_the_command_was_not_recorded() {
    let text = completion_notice(
        &[JobCompletion {
            kind: BackgroundKind::Job,
            owner: "s-me".into(),
            job: "j9".into(),
            command: String::new(),
            // **And the name is gone with it.** A reaped job has no view left to read a
            // slug off, so this is the other field that can be missing — and the notice
            // draws the id, which is the label's own fallback rather than a second rule.
            slug: None,
            state: "gone".into(),
            produced: 0,
            elapsed_ms: 0,
            detail: String::new(),
            memory: None,
        }],
        true,
    );
    assert!(text.contains("command not recorded"), "{text}");
    assert!(!text.contains("``"), "an empty pair of backticks: {text}");
}
/// **The child's sentence, which had no test, and it is the one the model reads.**
///
/// `completion_notice`'s twin on the subagent side: same channel, same wake, and the text is
/// again the deliverable. It is the *whole* of what the parent is handed when a child finishes
/// between turns, so what it names and what it promises are asserted rather than left to the
/// docstring — which is exactly the gap that let *"nothing fired"* be reported without anything
/// in CI disagreeing about whether the notice could fire at all.
///
/// **Two functions rather than a flag on one**, and the first assertion is why: the verb a child
/// is read with is `task_result`, not `job_output`, so a model that gets the wrong one is being
/// sent to a door that does not open. The second is that a child's completion carries an ANSWER
/// where a job's carries a command's output, and the parent that is told only *it stopped* has to
/// go and ask — which is the poll this whole path exists to end.
#[test]
fn a_childs_completion_names_the_child_and_points_at_task_result() {
    let one = JobCompletion {
        kind: BackgroundKind::Subagent,
        owner: "s-me".into(),
        job: "s-1-sub-1".into(),
        // A subagent runs no command, and the field must not quietly hold an answer.
        command: String::new(),
        // Nor does it have a name: a `task` child's handle is its identity.
        slug: None,
        state: "done".into(),
        produced: 0,
        elapsed_ms: 0,
        detail: "two facts off a fixture".into(),
        memory: None,
    };
    let text = subagent_notice(&[one.clone()], true);
    // The label, and it is what tells this row from a job's: one channel, two nouns.
    assert!(
        text.starts_with("[task]"),
        "labelled like a monitor: {text}"
    );
    assert!(
        text.contains("a subagent you started has finished"),
        "{text}"
    );
    assert!(text.contains("`s-1-sub-1`"), "{text}");
    assert!(text.contains("done"), "{text}");
    // **What the child SAID.** Without this the notice is a doorbell with no answer behind it:
    // the parent is told something happened and must still go and read.
    assert!(text.contains("two facts off a fixture"), "{text}");
    // The half that is the fix and not the footnote, in the job side's own words.
    assert!(text.contains("do not need to wait"), "{text}");
    assert!(text.contains("task_result"), "{text}");
    assert!(
        !text.contains("job_output"),
        "a subagent is read with `task_result`; sending the parent to `job_output` is a door \
             that does not open: {text}"
    );

    // **A child that said nothing gets no empty field.** The job side guards an empty command
    // (`` `` ``); the field that can be empty here is `detail`, and the shape it would leave is
    // a line ending in a colon with nothing after it.
    let bare = subagent_notice(
        &[JobCompletion {
            detail: String::new(),
            ..one.clone()
        }],
        true,
    );
    assert!(
        !bare.contains("done:"),
        "an empty detail drew a colon and nothing after it: {bare}"
    );
    assert!(bare.contains("`s-1-sub-1` done"), "{bare}");

    // **Two children are one notice, two lines, and the plural sentence** — a round that spawned
    // a pair reports a pair rather than making the reader count the list themselves.
    let pair = subagent_notice(
        &[
            one.clone(),
            JobCompletion {
                job: "s-1-sub-2".into(),
                ..one.clone()
            },
        ],
        true,
    );
    assert!(
        pair.contains("2 subagents you started have finished"),
        "{pair}"
    );
    assert!(
        pair.contains("s-1-sub-1") && pair.contains("s-1-sub-2"),
        "{pair}"
    );
}
/// **The wake's one decision, asserted: which sentence for which kind, and none for nothing.**
///
/// The partition is the whole of what [`Harness::wake`] decides about a settlement — everything
/// else it does is take the queue and submit the text — and it is asserted here because `wake()`
/// itself cannot be: it needs a live `Harness`, which this crate's tests build only where a
/// vocabulary and a store are present. See [`completion_notices`] for why that is not a reason to
/// leave the decision unasserted.
#[test]
fn a_settlement_is_two_nouns_out_of_one_channel() {
    let job = |id: &str| JobCompletion {
        kind: BackgroundKind::Job,
        owner: "s-me".into(),
        job: id.into(),
        command: "cargo test".into(),
        slug: None,
        state: "exited 0".into(),
        produced: 12,
        elapsed_ms: 900,
        detail: String::new(),
        memory: None,
    };
    let child = |id: &str| JobCompletion {
        kind: BackgroundKind::Subagent,
        owner: "s-me".into(),
        job: id.into(),
        command: String::new(),
        slug: None,
        state: "done".into(),
        produced: 0,
        elapsed_ms: 0,
        detail: "off a fixture".into(),
        memory: None,
    };

    // **Nothing settled says nothing**, which is what makes a spurious wake cost no generation.
    assert!(completion_notices(Vec::new(), "s-me").is_empty());

    // **The two nouns are the point.** A reader told `[job]` about a child goes looking for it
    // with `job_output` — which is the wrong door, and is what a hand measurement had to find
    // because nothing asserted this.
    let only_child = completion_notices(vec![child("s-1-sub-1")], "s-me");
    assert_eq!(only_child.len(), 1, "{only_child:?}");
    assert!(only_child[0].starts_with("[task]"), "{:?}", only_child[0]);
    assert!(only_child[0].contains("task_result"), "{:?}", only_child[0]);
    let only_job = completion_notices(vec![job("j7")], "s-me");
    assert_eq!(only_job.len(), 1, "{only_job:?}");
    assert!(only_job[0].starts_with("[job]"), "{:?}", only_job[0]);
    assert!(only_job[0].contains("job_output"), "{:?}", only_job[0]);

    // **Both kinds are one wake and two sentences**, job first — the order the old inline code
    // produced, kept so the record of a session reads the same way it did.
    let both = completion_notices(vec![job("j7"), child("s-1-sub-1")], "s-me");
    assert_eq!(both.len(), 2, "{both:?}");
    assert!(both[0].starts_with("[job]"), "{:?}", both[0]);
    assert!(both[1].starts_with("[task]"), "{:?}", both[1]);

    // **Two children are one sentence, not two** — the plural arm of the child's own wording,
    // asserted through the partition so the two functions cannot drift apart.
    let pair = completion_notices(vec![child("s-1-sub-1"), child("s-1-sub-2")], "s-me");
    assert_eq!(pair.len(), 1, "{pair:?}");
    assert!(
        pair[0].contains("2 subagents you started have finished"),
        "{:?}",
        pair[0]
    );
}

/// **A settlement handed up says WHOSE it was, and points at no door.**
///
/// The other half of the ownership split, and the sentence the measured defect of
/// 2026-10-05 needed: [`JobWatchers::stop`] hands a stopped session's undrained
/// settlements to the level above, and this is what keeps that hand-over honest.
///
/// The measurement, from the operator's own conversation: a subagent backgrounded
/// `sleep 25; echo done`, and the notice arrived in the MAIN session's transcript
/// reading *"a job you backgrounded has ended"* — while that session's own
/// `job_output` answered *no job called `j17`*. A notice that says *you backgrounded*
/// about a job you cannot read is not misplaced, it is false twice; and a notice that
/// promised `job_output` to the session that cannot open it sends a model to a door
/// that does not open, which is the rule this file keeps for `task_result` vs
/// `job_output` one layer down.
#[test]
fn a_handed_up_settlement_names_its_owner_and_sends_nobody_to_a_shut_door() {
    let job = JobCompletion {
        kind: BackgroundKind::Job,
        owner: "s-child".into(),
        job: "j17".into(),
        command: "sleep 25; echo done".into(),
        slug: None,
        state: "exited 0".into(),
        produced: 5,
        elapsed_ms: 25_000,
        detail: String::new(),
        memory: None,
    };

    // The level it was handed up to — the ROOT — is not the session that started it.
    let theirs = completion_notices(vec![job.clone()], "s-root");
    assert_eq!(theirs.len(), 1, "{theirs:?}");
    assert!(
        !theirs[0].contains("you backgrounded"),
        "the one sentence that must never be said about somebody else's job: {:?}",
        theirs[0]
    );
    assert!(
        theirs[0].contains("a job a session below this one started has ended"),
        "{:?}",
        theirs[0]
    );
    assert!(
        theirs[0].contains("started by `s-child`"),
        "the owner is named on the fact line: {:?}",
        theirs[0]
    );
    assert!(
        !theirs[0].contains("job_output"),
        "its host went with the session that started it, so `job_output` here is a door \
             that does not open: {:?}",
        theirs[0]
    );

    // **The same settlement, read by the session that started it** — which is the
    // sentence that was right all along and must not have moved.
    let mine = completion_notices(vec![job.clone()], "s-child");
    assert!(
        mine[0].contains("a job you backgrounded has ended"),
        "{:?}",
        mine[0]
    );
    assert!(mine[0].contains("job_output"), "{:?}", mine[0]);
    assert!(!mine[0].contains("started by"), "{:?}", mine[0]);

    // **Its own come first**, so one drain reads as this session's own news before what
    // it is being told on somebody else's behalf — two owners, two notices, that order.
    let mine_job = JobCompletion {
        owner: "s-root".into(),
        job: "j3".into(),
        ..job.clone()
    };
    let both = completion_notices(vec![job, mine_job], "s-root");
    assert_eq!(both.len(), 2, "{both:?}");
    assert!(
        both[0].contains("a job you backgrounded has ended") && both[0].contains("`j3`"),
        "{:?}",
        both[0]
    );
    assert!(
        both[1].contains("started by `s-child`") && both[1].contains("`j17`"),
        "{:?}",
        both[1]
    );

    // A handed-up GRANDCHILD keeps its own door: `task_result` reads the tree's handle
    // list (`Parts::tree_slots`), so unlike a dead job's output the answer is still there.
    let child = JobCompletion {
        kind: BackgroundKind::Subagent,
        owner: "s-child".into(),
        job: "s-child-sub-1".into(),
        command: String::new(),
        slug: None,
        state: "done".into(),
        produced: 0,
        elapsed_ms: 0,
        detail: "the grandchild answered".into(),
        memory: None,
    };
    let handed = completion_notices(vec![child], "s-root");
    assert_eq!(handed.len(), 1, "{handed:?}");
    assert!(
        handed[0].contains("a subagent a session below this one started has finished"),
        "{:?}",
        handed[0]
    );
    assert!(
        handed[0].contains("started by `s-child`"),
        "{:?}",
        handed[0]
    );
    assert!(
        handed[0].contains("the grandchild answered"),
        "{:?}",
        handed[0]
    );
    assert!(
        handed[0].contains("task_result"),
        "the handle is in this tree, so this door does open: {:?}",
        handed[0]
    );
}

/// **A finished foreground command is not a job anybody acts on.**
///
/// The operator ran `/job` after a turn and got hundreds of lines — one per
/// `grep`, `cat` and `sed` the model had run all session — because the host
/// gives every command a job id and the listing showed all of them. Measured
/// in their own store the same day: of 1,210 tool results in that session,
/// exactly **2** were `backgrounded`.
#[test]
fn only_background_and_still_running_jobs_are_listed() {
    use letibot_tools::exec::{JobId, JobState, JobView};
    use letibot_tools::exec::{ScopeId, ScopeKind};
    let scope = || ScopeId {
        kind: ScopeKind::Session,
        name: "s".into(),
        path: std::path::PathBuf::from("/sys/fs/cgroup/x"),
    };
    let job = |background, state| JobView {
        id: JobId("j1".into()),
        command: "grep -n x y".into(),
        // Nobody named this one — which is the premise of every case below.
        slug: None,
        scope: scope(),
        owner: scope(),
        cwd: "/".into(),
        pid: 1,
        background,
        state,
        elapsed: std::time::Duration::from_secs(1),
        ran_for: Some(std::time::Duration::from_secs(1)),
        produced: 91,
        since_last_output: None,
        memory_cap: letibot_tools::exec::MemoryCap::NotAsked,
        memory: None,
    };

    // The case that flooded the screen: finished, never backgrounded.
    assert!(!Harness::worth_listing(&job(
        None,
        JobState::Exited { code: 0 }
    )));
    // A failure is still not a job — the tool result already carried it.
    assert!(!Harness::worth_listing(&job(
        None,
        JobState::Exited { code: 1 }
    )));
    // Still running, never backgrounded: ctrl-o moves this one, so it stays.
    assert!(Harness::worth_listing(&job(None, JobState::Running)));
    // Backgrounded, however it got there, finished or not.
    for how in [
        letibot_transcript::Backgrounding::Asked,
        letibot_transcript::Backgrounding::Promoted,
    ] {
        assert!(Harness::worth_listing(&job(
            Some(how.clone()),
            JobState::Running
        )));
        assert!(Harness::worth_listing(&job(
            Some(how),
            JobState::Exited { code: 0 }
        )));
    }
}

/// **A row is one line, whatever the model wrote.**
///
/// A heredoc carries newlines inside the command text, and the listing cut it
/// to 60 *characters* — keeping the newline, so one row drew as two and a
/// screen of them was unreadable. The operator, 2026-09-20, looking at it:
/// *"some fuckery with jobs"*.
#[test]
fn a_job_row_is_one_line_however_many_the_command_had() {
    let heredoc = "cd /home/dead/Projects/leticl && python3 - <<'PY'\ns = open(\"x\")\nPY";
    let line = Harness::one_line(heredoc, 60);
    assert!(!line.contains('\n'), "a row that draws as two: {line:?}");
    // 60 characters plus the ellipsis that says something was cut.
    assert_eq!(line.chars().count(), 61, "{line:?}");
    assert!(line.ends_with('…'), "{line:?}");
    assert!(
        line.starts_with("cd /home/dead/Projects/leticl && python3"),
        "{line:?}"
    );

    // A short command is left exactly as it is — no ellipsis promising more.
    assert_eq!(Harness::one_line("git status", 60), "git status");
    // Internal runs of whitespace collapse, so the width asked for is the
    // width drawn.
    assert_eq!(Harness::one_line("ls   -la\t-h", 60), "ls -la -h");
}

/// **A named job draws its name beside its id, and an unnamed one draws what it drew
/// before.**
///
/// `j57` is a counter: the operator, watching the pane, could not tell which running job
/// was the release build and which was the fold's tests — *"do we want to give jobs
/// slugs? j57 doesnt ringe the bell for me"*. The name is the agent's own stated intent,
/// so the row has to carry it and the absence of one has to be exactly the old row.
#[test]
fn a_named_job_draws_its_name_beside_its_id_and_an_unnamed_one_does_not() {
    use letibot_tools::exec::{JobId, JobState, JobView, ScopeId, ScopeKind};
    let scope = || ScopeId {
        kind: ScopeKind::Session,
        name: "s".into(),
        path: std::path::PathBuf::from("/sys/fs/cgroup/x"),
    };
    let view = |slug: Option<&str>| JobView {
        id: JobId("j65".into()),
        command: "cargo build --release".into(),
        slug: slug.map(|s| s.to_string()),
        scope: scope(),
        owner: scope(),
        cwd: "/".into(),
        pid: 1,
        background: Some(letibot_transcript::Backgrounding::Asked),
        state: JobState::Running,
        elapsed: std::time::Duration::from_secs(2),
        ran_for: None,
        produced: 12_288,
        since_last_output: None,
        memory_cap: letibot_tools::exec::MemoryCap::NotAsked,
        memory: None,
    };

    let named = Harness::job_row(&view(Some("release-build")));
    assert_eq!(
        named,
        "  release-build · j65  running  12288 bytes  cargo build --release"
    );
    // **The name is flattened like everything else on the row** — a name is one line
    // however it was typed, and the listing is a listing.
    assert_eq!(
        Harness::job_name(&view(Some("release\nbuild"))),
        "release build · j65",
        "a row that draws as two"
    );
    // **No name, no decoration**: byte-for-byte what this listing printed before slugs
    // existed, because that is what "absent" has to mean.
    let unnamed = Harness::job_row(&view(None));
    assert_eq!(
        unnamed,
        "  j65  running  12288 bytes  cargo build --release"
    );
    assert!(
        !unnamed.contains('·'),
        "a separator with nothing before it: {unnamed:?}"
    );
    // And an empty string is the same claim as no name, for the same reason the wire
    // spells absence as empty: *nobody named this* is one fact with one rendering.
    assert_eq!(Harness::job_name(&view(Some(""))), "j65");
}

/// **The name reaches the wire, and a job nobody named sends an empty one.**
///
/// The head draws `JobEntry`, so this is the half that makes the pane possible at all;
/// the empty half is the one an older daemon's row takes, and it must be the row that
/// existed before the field did.
#[test]
fn a_job_entry_carries_the_name_it_was_given_and_nothing_when_it_was_not() {
    use letibot_tools::exec::{JobId, JobState, JobView, ScopeId, ScopeKind};
    let scope = || ScopeId {
        kind: ScopeKind::Session,
        name: "s".into(),
        path: std::path::PathBuf::from("/sys/fs/cgroup/x"),
    };
    let view = |slug: Option<&str>| JobView {
        id: JobId("j65".into()),
        command: "cargo test -p letibot-tools".into(),
        slug: slug.map(|s| s.to_string()),
        scope: scope(),
        owner: scope(),
        cwd: "/".into(),
        pid: 1,
        background: Some(letibot_transcript::Backgrounding::Asked),
        state: JobState::Running,
        elapsed: std::time::Duration::from_secs(2),
        ran_for: None,
        produced: 0,
        since_last_output: None,
        memory_cap: letibot_tools::exec::MemoryCap::NotAsked,
        memory: None,
    };

    assert_eq!(
        Harness::job_entry(&view(Some("fold-tests"))).slug,
        "fold-tests"
    );
    assert_eq!(Harness::job_entry(&view(None)).slug, "");
    assert_eq!(Harness::job_entry(&view(Some(""))).slug, "");
    // The command is untouched by any of it: the name is not the command's doing.
    assert_eq!(
        Harness::job_entry(&view(Some("fold-tests"))).command,
        "cargo test -p letibot-tools"
    );
}

/// **The url is written the way it is curled.** A scheme and the chat-completions
/// path both appear in what the operator pastes, and both have to come off: the
/// client appends its own route, so a kept path would post to
/// `/v1/chat/completions/completion`.
#[test]
fn a_fleet_url_is_read_the_way_the_operator_writes_it() {
    let e = super::parse_local_url("http://192.168.1.78:8082/v1/chat/completions")
        .expect("a curl's url");
    assert_eq!(e.authority(), "192.168.1.78:8082");
    assert_eq!(
        super::parse_local_url("192.168.1.78:8082")
            .unwrap()
            .authority(),
        "192.168.1.78:8082",
        "a bare authority is the same address"
    );
    assert_eq!(
        super::parse_local_url("https://box.lan:443/")
            .unwrap()
            .authority(),
        "box.lan:443"
    );
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

fn user(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

/// A `User` row **this session appended** — a job settlement, a salvage notice,
/// an intent check. The same variant on the wire and never the operator's words.
fn agent_row(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: letibot_transcript::Speaker::Agent,
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

/// A ledger with an encoder attached, which is what every session has.
///
/// Constructing the decorator is the *only* way to say so — the flag is
/// `pub(crate)` in `letibot-tools` precisely so that "an encoder was declared"
/// and "an encoder was wired" cannot be two different facts. The sink is
/// dropped; the attachment is not.
fn encoded() -> IntentLedger {
    let l = IntentLedger::new();
    let l = Arc::new(l);
    drop(IntentSink::new(l.clone(), letibot_tools::NullToolSink));
    Arc::try_unwrap(l).expect("the sink was just dropped")
}

#[test]
fn a_downgrade_denies_the_seated_tools_of_its_classes_and_the_well_known_names() {
    use letibot_tools::permission::{Action, Rule};
    use letibot_tools::schema::{Access, Downgrade, ToolSchema};
    let seated = vec![
        ToolSchema::new("read", "r", serde_json::json!({}), Access::Read),
        ToolSchema::new("edit", "e", serde_json::json!({}), Access::Write),
        ToolSchema::new("bash", "b", serde_json::json!({}), Access::Exec),
        ToolSchema::new("flowy", "f", serde_json::json!({}), Access::Network),
        ToolSchema::new("odd_writer", "o", serde_json::json!({}), Access::Write),
    ];
    let parent = vec![Rule::new("edit", "*", Action::Allow)];
    let rules = downgraded_ruleset(&parent, &Downgrade::parse("no-write").unwrap(), &seated);
    let denied: Vec<&str> = rules
        .iter()
        .filter(|r| r.action == Action::Deny)
        .map(|r| r.permission.as_str())
        .collect();
    assert!(
        denied.contains(&"edit") && denied.contains(&"write") && denied.contains(&"odd_writer"),
        "{denied:?}"
    );
    // `notes` denies with the write class: a standing note outlives the
    // subagent that wrote it, so it is not a capability a `no-write`
    // child keeps.
    assert!(denied.contains(&"notes"), "{denied:?}");
    assert!(
        !denied.contains(&"read") && !denied.contains(&"bash") && !denied.contains(&"flowy"),
        "{denied:?}"
    );
    // Last rule wins: the parent's allow for `edit` is overridden.
    let r = letibot_tools::permission::evaluate("edit", "anything", &[&rules]);
    assert_eq!(r.action, Action::Deny);
    // No downgrade, no change.
    assert_eq!(
        downgraded_ruleset(&parent, &Downgrade::none(), &seated),
        parent
    );
}

/// **A child's answer is settled once, and a path out with no answer still settles.**
///
/// Two failures this guard exists for, and both are silent without it: a settlement that
/// never fires leaves the parent's `task_result` waiting for ever, and a second one lets a
/// path out of the call overwrite the answer the child actually gave.
#[test]
fn a_childs_answer_is_settled_once_and_never_overwritten() {
    use letibot_tools::builtins::task::TaskStatus;

    // The ordinary ending.
    let slot = TaskSlot::new("s-parent");
    {
        let answer = AnswerOnce::new(&slot);
        answer.say(TaskStatus::Done {
            answer: "the child's answer".into(),
            child: None,
        });
    }
    assert_eq!(
        slot.status(),
        TaskStatus::Done {
            answer: "the child's answer".into(),
            child: None,
        }
    );

    // **A path out with no answer still settles** — the one that would hang a parent: a
    // thread ending with the slot still `Running` is a `task_result` that never returns.
    let slot = TaskSlot::new("s-parent");
    {
        let _answer = AnswerOnce::new(&slot);
    }
    assert!(
        matches!(slot.status(), TaskStatus::Failed { .. }),
        "an unsettled child stayed `running` for ever: {:?}",
        slot.status()
    );

    // **And the first answer is the answer.**
    let slot = TaskSlot::new("s-parent");
    {
        let answer = AnswerOnce::new(&slot);
        answer.say(TaskStatus::Done {
            answer: "said".into(),
            child: None,
        });
        answer.say(TaskStatus::Failed {
            why: "the park ended oddly".into(),
        });
    }
    assert_eq!(
        slot.status(),
        TaskStatus::Done {
            answer: "said".into(),
            child: None,
        },
        "a later path out overwrote what the child said"
    );
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

/// **The turn boundary carries the intent finding and NOTHING about the plan.**
///
/// This test used to assert the opposite — that the plan's finding was joined in at the turn
/// boundary because *"joining is what keeps this inside the one-nudge budget"*. That budget was
/// the thing the operator rejected: one nudge per user turn means a session in constant
/// conversation is checked after every exchange (*"looks like our todo nag is overkill"*) while
/// a quiet one is checked once and never again. The plan is now delivered by the IDLE clock
/// (`Sessions::rearm_todo_nag`, `Harness::nag_turn`), so what is left here is the finding that
/// really is about the turn that just ended.
///
/// The plan half has its own tests where it now lives: `unfinished_plan` in `letibot-tools` for
/// the text, and `nag_should_arm` below for the schedule.
#[test]
fn the_turn_boundary_says_what_the_turn_promised_and_not_what_the_plan_is() {
    // A session with nothing declared has nothing to say at the boundary — the common case,
    // and the one that must stay silent.
    assert!(
        steer_for_turn(&encoded(), false, "t1", &[]).is_none(),
        "an encoded ledger with nothing declared has no boundary finding"
    );
    // And the finding that IS the boundary's own: a turn that declared something and did
    // nothing to show for it. It carries no plan text at all, which is the whole claim of
    // this test — the two checks are no longer one message.
    let bare = IntentLedger::new();
    let steer = steer_for_turn(&bare, false, "t1", &[])
        .expect("no encoder is itself a finding, on every turn");
    assert!(!steer.contains("[todo check]"), "{steer}");
}

/// **An unattached encoder is loud, not silent.** The state this is asserting
/// is the one a session must never be in quietly: a diff computed against
/// nothing would report every completion as verified, which is exactly the
/// announced-but-not-done failure wearing the check's own clothes.
#[test]
fn a_ledger_with_no_encoder_says_so_in_the_diff_itself() {
    let bare = IntentLedger::new();
    let steer = steer_for_turn(&bare, false, "t1", &[])
        .expect("no encoder is itself a finding, on every turn");
    assert!(steer.contains("effect log"), "{steer}");
    assert!(
        steer_for_turn(&encoded(), false, "t1", &[]).is_none(),
        "an encoded ledger with nothing declared has nothing to say"
    );
}

// -- the authorisation trail --------------------------------------------

/// **The harness must not be able to authorise itself.**
///
/// Everything a session appends as a `User` item looks identical in the
/// transcript: the operator's prompt, a head's steering, §5.7's salvage notice
/// and the intent check's own *"you said you would X"*. A trail walked over the
/// transcript would read all four as the operator's words, and an agent whose
/// own text can authorise an action is the shape a prompt injection would most
/// like to take. This is the assertion that keeps the four apart.
#[test]
fn an_injected_notice_is_never_read_back_as_the_operator() {
    let m = TrailMirror::default();
    m.begin_turn();
    m.say(
        Speaker::Operator,
        "ssh to lubuntu2 and check the build",
        None,
    );
    m.say(
        Speaker::Agent,
        "[intent check] you said you would ssh",
        None,
    );

    let t = m.trail();
    let words: Vec<&str> = t.operator_words().iter().map(|u| u.text.as_str()).collect();
    assert_eq!(words, vec!["ssh to lubuntu2 and check the build"]);
    assert_eq!(
        t.utterances.len(),
        2,
        "both are carried, with their speakers"
    );
    assert!(
        t.utterances.iter().any(|u| u.speaker == Speaker::Agent),
        "the agent's own line is carried as context and never as authority"
    );
}

/// The denominator is what was **looked at**, and it is not derived from what
/// came back.
///
/// `docs/tool-design-brief.md` §2.2 applied to a security input: an adjudicator
/// told *"the operator said nothing"* when nobody looked would deny the thing
/// that was asked for. `0 of 41` is a measurement; `0` is not.
#[test]
fn a_trail_with_no_operator_words_still_carries_its_denominator() {
    let m = TrailMirror::default();
    m.note_items(41);
    let t = m.trail();
    assert!(
        t.was_collected(),
        "somebody looked; this is not `NotCollected`"
    );
    assert!(t.operator_words().is_empty());
    match t.provenance {
        TrailProvenance::Scanned {
            messages_scanned,
            operator_messages,
        } => {
            assert_eq!(operator_messages, 0);
            assert!(
                messages_scanned > 0,
                "a scan of 41 items reported a denominator of {messages_scanned}"
            );
        }
        TrailProvenance::NotCollected { .. } => panic!("something looked"),
    }
}

/// A gate with no trail source installed reports `NotCollected` — **not** an
/// empty trail. This is the state the whole seam exists to leave behind, and it
/// is asserted here so that "the default is safe" is a fact rather than a
/// recollection.
#[test]
fn an_uninstalled_trail_is_not_collected_rather_than_empty() {
    let t = AuthorisationTrail::default();
    assert!(!t.was_collected());
    assert!(t.render().contains("NOT COLLECTED"), "{}", t.render());
}

/// Distance is half the evidence: a *"yeah restart"* from six turns ago is not
/// the same fact as one from this turn, and a trail that dropped the distance
/// would present them as identical.
#[test]
fn recency_is_carried_and_the_clock_is_absent_rather_than_invented() {
    let m = TrailMirror::default();
    m.begin_turn();
    m.say(Speaker::Operator, "old", None);
    for _ in 0..5 {
        m.begin_turn();
    }
    m.say(Speaker::Operator, "recent", Some(Instant::now()));

    let t = m.trail();
    let by = |s: &str| {
        t.utterances
            .iter()
            .find(|u| u.text == s)
            .unwrap_or_else(|| panic!("{s} is missing"))
    };
    assert_eq!(by("recent").turns_ago, 0);
    assert_eq!(by("old").turns_ago, 5);
    // `None` reads as *not recorded*, never as *just now* — which is why the
    // rebuilt-from-store rows keep it.
    assert_eq!(by("old").seconds_ago, None);
    assert!(by("recent").seconds_ago.is_some());
}

/// A rebuilt transcript seeds the trail, and the clock does not come back with it.
/// Asserted so that the resume note beside it is describing something true.
#[test]
fn a_resumed_trail_is_seeded_with_no_clock() {
    let m = TrailMirror::default();
    m.seed(&[
        user("first"),
        TranscriptItem::Assistant {
            text: "an answer".into(),
            tool_calls: vec![],
            truncated: false,
        },
        user("second"),
    ]);
    let t = m.trail();
    assert_eq!(t.operator_words().len(), 2);
    assert!(
        t.utterances.iter().all(|u| u.seconds_ago.is_none()),
        "nothing recorded when a stored row was said; a reconstructed clock \
             would be a guess presented as a measurement"
    );
}

/// **A resumed session does not promote its own words to the operator's.**
///
/// The regression this closes was live and measurable: `seed` destructured
/// `TranscriptItem::User { parts, .. }` and stamped `Speaker::Operator` on every
/// row, so a job settlement came back as an operator utterance and
/// [`AuthorisationTrail::cited_operator_words`] — which exists precisely to refuse
/// an ALLOW citing agent text — had nothing left to refuse. 280 of 715 briefs the
/// oracle answered carried one.
///
/// Both halves are asserted, because only one of them is the bug: the agent row is
/// still **carried** (it is context, and `Speaker::Agent` says what it is), and it
/// is no longer **counted** as something that could authorise anything.
#[test]
fn a_resumed_trail_does_not_call_the_harnesss_own_rows_the_operators() {
    let m = TrailMirror::default();
    m.seed(&[
        user("fix the compaction"),
        agent_row("[job] a job you backgrounded has ended:\n  - `j6` exited 0"),
        user("restarted it"),
    ]);
    let t = m.trail();

    assert_eq!(
        t.utterances.len(),
        3,
        "the agent row is carried — it is context the operator's reply may lean on"
    );
    assert_eq!(
        t.operator_words().len(),
        2,
        "but it is not one of the operator's words"
    );

    // The citation check is the thing that was defeated, so assert it directly
    // rather than trusting the count above to stand in for it.
    let job = t
        .utterances
        .iter()
        .position(|u| u.text.starts_with("[job]"))
        .expect("the job row is on the page");
    assert_eq!(
        t.cited_operator_words(&[job]),
        Vec::<usize>::new(),
        "an ALLOW citing the harness's own job notice authorises nothing"
    );
    assert_eq!(
        t.cited_operator_words(&[0, job, 2]),
        vec![0, 2],
        "the operator's own lines still survive beside it"
    );
}

// -- the intent diff -----------------------------------------------------

/// **T21.3, closed.** An item declared in a turn that then ran nothing is the
/// acceptance case, and the steering text is what the model is handed.
#[test]
fn an_intent_declared_and_not_acted_on_produces_steering() {
    let l = encoded();
    l.declare("t1", "restart the model server", intent_tools::Source::Todo);
    let steer = steer_for_turn(&l, false, "t1", &[]).expect("a declared item and no effect");
    assert!(steer.contains("intent check"), "{steer}");
    assert!(steer.contains("restart the model server"), "{steer}");
}

/// **The default does not fire on prose**, which is the whole reason the prose
/// half is a flag. A turn that says "I'll check the file" and calls nothing is
/// the *normal* shape of a plain answer, and nudging it would put the check into
/// conversations that were working.
#[test]
fn the_prose_heuristic_is_off_unless_it_is_asked_for() {
    let l = encoded();
    let items = [TranscriptItem::Assistant {
        text: "I'll check the file and get back to you.".into(),
        tool_calls: vec![],
        truncated: false,
    }];
    assert_eq!(
        steer_for_turn(&l, false, "t1", &items),
        None,
        "the deterministic half must not read prose"
    );
    let with_prose =
        steer_for_turn(&l, true, "t2", &items).expect("the prose half is what --intent-prose buys");
    assert!(with_prose.contains("check the file"), "{with_prose}");
}

/// The diff is idempotent per turn. A turn boundary that fires twice is a thing
/// that happens, and a diff that grows each time it is read is not a
/// measurement.
#[test]
fn reading_the_same_turn_twice_says_the_same_thing() {
    let l = encoded();
    l.declare("t1", "write the report", intent_tools::Source::Todo);
    let a = steer_for_turn(&l, false, "t1", &[]);
    let b = steer_for_turn(&l, false, "t1", &[]);
    assert_eq!(a, b);
}

// -- the monitor notice --------------------------------------------------

/// A monitor that expired is **not** a monitor that fired, and the sentence the
/// model gets has to keep them apart — reporting a deadline as a completion is
/// F5 one primitive over.
#[test]
fn a_monitor_notice_says_which_of_the_four_endings_it_was() {
    let monitors = Monitors::new();
    assert_eq!(monitors.settled_count(), 0);
    // Nothing declared, so nothing settles: the notice builder is exercised
    // through the empty case here and through the real one in `background.rs`,
    // which owns the four endings. What matters at this layer is that the
    // sentence carries `word()` rather than a boolean.
    let text = monitor_notice(&[]);
    assert!(text.contains("FIRED"), "{text}");
    assert!(
        text.contains("expiry") || text.contains("stopped existing"),
        "the notice must distinguish a firing from a watch that merely ended: {text}"
    );
}
