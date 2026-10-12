//! **The branch is the deliverable: a worktree child does not finish with its work outside it.**
//!
//! The report that asked for this, from pg-noop's model on 2026-10-12, relayed by the operator:
//!
//! > *"So the child's death cost the work nothing except its own account of it — and that is
//! > only true because the worktree was checked before I spawned a replacement. Seven times
//! > tonight a child has ended with its work outside its branch, and each time the recovery was
//! > the same two commands; the pattern is now so routine that it belongs in the brief rather
//! > than in the salvage."*
//!
//! A `task_start` child works in `<workspace>/.claude/worktrees/agent-<slug>` on branch
//! `agent/<slug>`, and the branch is what the merge queue lands. A child that ends with edits
//! uncommitted, or new files untracked, has handed its parent a branch that does not hold the
//! work — and the parent found out by looking, seven times in one night. Three things here, each
//! one the half the others cannot do:
//!
//! * **The brief** ([`letibot_tools::builtins::task::worktree_brief`]) tells the child the rule
//!   before it starts: commit as you go, finish clean, name the commits.
//! * **The end-of-turn check** ([`EndOfWork`]) reads `git status --porcelain` in the worktree when
//!   the child's turn is about to end, and if it is dirty, tells the child ONCE, in the harness's
//!   voice, what is uncommitted, and lets the turn go on — the same one-nudge budget as the
//!   intent check (`close_the_turn`) beside it in the round loop.
//! * **The completion** ([`with_the_line`]) — what the parent receives, as an answer or as a
//!   failure (a child that crashed, errored or was killed) — carries at its top the plain fact
//!   when the tree is still dirty, with the two salvage commands, so the parent never has to
//!   discover it.
//!
//! A child with no worktree (a plain `task`, or `task_start` with `main_tree: true`) has no
//! [`Deliverable`] and none of this touches it: the main checkout's status is the operator's, not
//! the child's account of its work.

use letibot_tools::builtins::task::WorktreePlacement;

/// How many porcelain entries a nudge or a completion line names before it counts the rest.
const LISTED: usize = 8;

/// **Where a worktree child's work has to end up**: the tree it works in and the branch that
/// tree is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deliverable {
    pub path: String,
    pub branch: String,
}

impl Deliverable {
    /// The deliverable of a `task_start` placement, or `None` for the main-tree exception —
    /// whose status holds whatever the operator had open, not the child's account of its work.
    pub fn of(placement: &WorktreePlacement) -> Option<Deliverable> {
        (!placement.main_tree).then(|| Deliverable {
            path: placement.path.clone(),
            branch: placement.branch.clone(),
        })
    }

    /// **What the worktree holds that the branch does not** — `git status --porcelain`'s lines,
    /// or `None` when the tree is clean.
    ///
    /// Also `None` when git cannot answer (the tree was removed, git is missing): the claim this
    /// feeds is *"the branch does not contain them"*, and a status that could not be read is not
    /// evidence of uncommitted work. Guard the fact, not the proxy.
    pub fn uncommitted(&self) -> Option<Vec<String>> {
        // The daemon's own leftover logs (`.letibot/*.jsonl`, written there by older builds) are
        // not the child's work, and the child is not told to commit them (R92.4).
        let lines =
            crate::mergequeue::git::uncommitted_work(std::path::Path::new(&self.path)).ok()?;
        (!lines.is_empty()).then_some(lines)
    }
}

/// The porcelain lines as a short list: every one up to [`LISTED`], then a count of the rest.
fn listed(files: &[String]) -> String {
    let shown: Vec<&str> = files.iter().take(LISTED).map(String::as_str).collect();
    let mut out = shown.join(", ");
    if files.len() > LISTED {
        out.push_str(&format!(
            ", and {} more ({} in all)",
            files.len() - LISTED,
            files.len()
        ));
    }
    out
}

/// **The nudge, in the harness's voice** — said once, when the child's turn is about to end with
/// the worktree dirty, and the turn continues.
pub fn nudge_text(d: &Deliverable, files: &[String]) -> String {
    format!(
        "[the deliverable] Your worktree `{path}` has uncommitted changes, so the branch \
         `{branch}` does not hold them: {list}. The branch is the deliverable — commit this \
         work to `{branch}` now (or say, for any file that must stay out, why), check that \
         `git status --porcelain` prints nothing, and then finish, naming the commit(s) in \
         your answer.",
        path = d.path,
        branch = d.branch,
        list = listed(files),
    )
}

/// **The line a parent reads first** when the child ended — answered, failed, crashed or killed —
/// with its work outside its branch, and the two commands that put it on the branch.
pub fn completion_line(d: &Deliverable, files: &[String]) -> String {
    format!(
        "the worktree {path} has uncommitted changes: {list} — the branch {branch} does not \
         contain them. To salvage: `git -C {path} add -A` then \
         `git -C {path} commit -m \"salvage: uncommitted work of {branch}\"`.",
        path = d.path,
        branch = d.branch,
        list = listed(files),
    )
}

/// **What the parent is handed, with the dirty-tree line on top when there is one.** `text` is
/// the child's answer or the failure's sentence; a child with no deliverable, or a clean tree,
/// gets it back unchanged.
pub fn with_the_line(d: Option<&Deliverable>, text: &str) -> String {
    match d.and_then(|d| d.uncommitted().map(|f| completion_line(d, &f))) {
        Some(line) => format!("{line}\n\n{text}"),
        None => text.to_string(),
    }
}

/// **The end-of-turn check, with its budget of one.**
///
/// Per turn, like `nudges_left`: a model that answers the nudge by explaining why a file stays
/// out has given a legitimate answer, and a second nudge would be the harness arguing. What it
/// did not commit is then the completion line's job, not another round's.
#[derive(Debug, Default)]
pub struct EndOfWork {
    nudged: bool,
}

impl EndOfWork {
    pub fn new() -> Self {
        Self::default()
    }

    /// The nudge to append when the turn is about to end, or `None` — a clean tree, no
    /// deliverable, or the one nudge already spent.
    pub fn at_turn_end(&mut self, d: Option<&Deliverable>) -> Option<String> {
        if self.nudged {
            return None;
        }
        let d = d?;
        let files = d.uncommitted()?;
        self.nudged = true;
        Some(nudge_text(d, &files))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// A temp repository with one commit and a `task_start`-shaped worktree cut from it,
    /// removed on drop.
    struct Repo {
        root: PathBuf,
        wt: Deliverable,
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn repo(tag: &str) -> Repo {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "letibot-deliverable-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let main = root.join("main");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        std::fs::write(main.join("lib.rs"), "fn a() {}\n").unwrap();
        git(&main, &["add", "-A"]);
        git(&main, &["commit", "-q", "-m", "base"]);
        let slug = "child";
        let path = letibot_tools::builtins::task::worktree_path(&main.display().to_string(), slug);
        let branch = letibot_tools::builtins::task::worktree_branch(slug);
        git(
            &main,
            &["worktree", "add", "-q", &path, "-b", &branch, "main"],
        );
        // **Exactly the tree a real child is handed**: the runner writes nothing into it (the
        // build cache is shared through the child's environment, `worktree_build_env`).
        Repo {
            root,
            wt: Deliverable { path, branch },
        }
    }

    fn placement(d: &Deliverable, main_tree: bool) -> WorktreePlacement {
        WorktreePlacement {
            path: d.path.clone(),
            branch: d.branch.clone(),
            base_sha: "0".into(),
            main_tree,
        }
    }

    #[test]
    fn a_clean_child_is_not_nudged_and_its_answer_is_untouched() {
        let r = repo("clean");
        assert_eq!(r.wt.uncommitted(), None);
        let mut check = EndOfWork::new();
        assert_eq!(check.at_turn_end(Some(&r.wt)), None);
        assert_eq!(with_the_line(Some(&r.wt), "done: abc123"), "done: abc123");
    }

    #[test]
    fn a_dirty_child_is_nudged_once_commits_and_completes_clean() {
        let r = repo("dirty");
        let wt = Path::new(&r.wt.path);
        std::fs::write(wt.join("lib.rs"), "fn a() { b() }\n").unwrap();
        std::fs::write(wt.join("new.rs"), "fn b() {}\n").unwrap();

        let mut check = EndOfWork::new();
        let nudge = check
            .at_turn_end(Some(&r.wt))
            .expect("a dirty tree is nudged");
        assert!(nudge.starts_with("[the deliverable]"), "{nudge}");
        assert!(
            nudge.contains("lib.rs") && nudge.contains("new.rs"),
            "{nudge}"
        );
        assert!(nudge.contains(&r.wt.branch), "{nudge}");

        // The child does what it was told: commits the files it was shown, and the tree reads
        // clean.
        git(wt, &["add", "lib.rs", "new.rs"]);
        git(wt, &["commit", "-q", "-m", "the work"]);
        assert_eq!(r.wt.uncommitted(), None);
        assert_eq!(check.at_turn_end(Some(&r.wt)), None);
        assert_eq!(with_the_line(Some(&r.wt), "committed abc"), "committed abc");
    }

    #[test]
    fn the_nudge_is_said_once_and_a_child_still_dirty_completes_with_the_line() {
        let r = repo("still");
        std::fs::write(Path::new(&r.wt.path).join("lib.rs"), "changed\n").unwrap();
        let mut check = EndOfWork::new();
        assert!(check.at_turn_end(Some(&r.wt)).is_some());
        assert_eq!(
            check.at_turn_end(Some(&r.wt)),
            None,
            "one nudge, then it may finish"
        );

        let handed = with_the_line(Some(&r.wt), "all done");
        let first = handed.lines().next().unwrap();
        assert!(
            first.starts_with(&format!(
                "the worktree {} has uncommitted changes: M lib.rs — the branch {} does not \
                 contain them.",
                r.wt.path, r.wt.branch
            )),
            "{first}"
        );
        assert!(
            first.contains(&format!("git -C {} add -A", r.wt.path)),
            "{first}"
        );
        assert!(
            first.contains(&format!("git -C {} commit -m", r.wt.path)),
            "{first}"
        );

        // **And the two commands do what the line says**: run as printed, the branch holds the
        // work, the tree is clean, and the commit holds the child's file and nothing else.
        let wt = Path::new(&r.wt.path);
        let sh = |cmd: &str| {
            let out = Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .env("GIT_CONFIG_COUNT", "1")
                .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
                .env("GIT_CONFIG_VALUE_0", "false")
                .output()
                .expect("sh runs");
            assert!(
                out.status.success(),
                "{cmd}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        let commands: Vec<&str> = first.split('`').skip(1).step_by(2).collect();
        assert_eq!(commands.len(), 2, "{first}");
        for c in &commands {
            sh(c);
        }
        assert_eq!(r.wt.uncommitted(), None);
        let committed = sh(&format!(
            "git -C {} show --name-only --format= HEAD",
            wt.display()
        ));
        assert_eq!(committed.trim(), "lib.rs", "{committed}");
        assert!(handed.ends_with("\n\nall done"), "{handed}");
    }

    #[test]
    fn a_dead_childs_failure_carries_the_line() {
        let r = repo("dead");
        std::fs::write(Path::new(&r.wt.path).join("half.rs"), "fn half(").unwrap();
        let why = with_the_line(Some(&r.wt), "the provider refused the request");
        assert!(why.starts_with("the worktree "), "{why}");
        assert!(why.contains("?? half.rs"), "{why}");
        assert!(why.ends_with("the provider refused the request"), "{why}");
    }

    /// **The settlement the parent reads**, through the guard every ending of a child goes
    /// through (`AnswerOnce`): a failure, a kill and a turn that ended with nothing to say all
    /// carry the line, and the kill rule still replaces the truncated answer.
    #[test]
    fn every_ending_of_a_dirty_child_reaches_the_parent_with_the_line() {
        use super::super::{AnswerOnce, TaskSlot};
        use letibot_tools::builtins::task::TaskStatus;
        let r = repo("endings");
        std::fs::write(Path::new(&r.wt.path).join("lost.rs"), "x").unwrap();
        let why_of = |s: TaskStatus| match s {
            TaskStatus::Failed { why } => why,
            other => panic!("expected a failure, got {other:?}"),
        };

        // Died: the provider refused, the context wall, a crash — a `fail` with a reason.
        let slot = TaskSlot::new("s-parent");
        AnswerOnce::new(&slot)
            .with_deliverable(Some(r.wt.clone()))
            .say(TaskStatus::Failed {
                why: "the provider refused".into(),
            });
        let why = why_of(slot.status());
        assert!(
            why.starts_with(&format!("the worktree {} has", r.wt.path)),
            "{why}"
        );
        assert!(why.ends_with("the provider refused"), "{why}");

        // Killed: the answer it had is not an answer, and the line survives the replacement.
        let slot = TaskSlot::new("s-parent");
        slot.kill();
        AnswerOnce::new(&slot)
            .with_deliverable(Some(r.wt.clone()))
            .say(TaskStatus::Done {
                answer: "half a summ".into(),
                child: None,
            });
        let why = why_of(slot.status());
        assert!(why.contains("?? lost.rs"), "{why}");
        assert!(why.contains("stopped by `job_kill`"), "{why}");
        assert!(!why.contains("half a summ"), "{why}");

        // Ended with nothing said: the guard's own failure.
        let slot = TaskSlot::new("s-parent");
        drop(AnswerOnce::new(&slot).with_deliverable(Some(r.wt.clone())));
        let why = why_of(slot.status());
        assert!(why.starts_with("the worktree "), "{why}");

        // Answered: the line is on top of the answer.
        let slot = TaskSlot::new("s-parent");
        AnswerOnce::new(&slot)
            .with_deliverable(Some(r.wt.clone()))
            .say(TaskStatus::Done {
                answer: "done".into(),
                child: None,
            });
        match slot.status() {
            TaskStatus::Done { answer, .. } => {
                assert!(answer.starts_with("the worktree "), "{answer}");
                assert!(answer.ends_with("\n\ndone"), "{answer}");
            }
            other => panic!("expected an answer, got {other:?}"),
        }

        // And a child with no deliverable is handed exactly what it said.
        let slot = TaskSlot::new("s-parent");
        AnswerOnce::new(&slot).say(TaskStatus::Failed {
            why: "plain".into(),
        });
        assert_eq!(why_of(slot.status()), "plain");
    }

    #[test]
    fn a_child_without_a_worktree_is_untouched() {
        let r = repo("main-tree");
        std::fs::write(Path::new(&r.wt.path).join("lib.rs"), "changed\n").unwrap();
        // The main-tree exception has no deliverable, and a plain `task` has none at all.
        assert_eq!(Deliverable::of(&placement(&r.wt, true)), None);
        assert_eq!(with_the_line(None, "answer"), "answer");
        assert_eq!(EndOfWork::new().at_turn_end(None), None);
        // A worktree placement does have one.
        assert_eq!(
            Deliverable::of(&placement(&r.wt, false)),
            Some(r.wt.clone())
        );
    }

    #[test]
    fn an_unreadable_tree_is_not_called_dirty() {
        let gone = Deliverable {
            path: std::env::temp_dir()
                .join("letibot-deliverable-no-such-tree")
                .display()
                .to_string(),
            branch: "agent/gone".into(),
        };
        assert_eq!(gone.uncommitted(), None);
        assert_eq!(with_the_line(Some(&gone), "x"), "x");
    }

    #[test]
    fn a_long_list_is_counted_not_printed() {
        let files: Vec<String> = (0..20).map(|i| format!("?? f{i}.rs")).collect();
        let d = Deliverable {
            path: "/w".into(),
            branch: "agent/w".into(),
        };
        let line = completion_line(&d, &files);
        assert!(line.contains("?? f7.rs, and 12 more (20 in all)"), "{line}");
        assert!(!line.contains("f8.rs"), "{line}");
    }
}
