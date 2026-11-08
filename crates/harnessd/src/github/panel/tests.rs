//! The panel's own policy, offline: a verdict per member, the gatekeeper's rule over them, and the
//! one GitHub review that comes out. The members here are stubs, and the one Claude test drives a
//! stub `claude` script this file writes - no real binary, no network.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use letibot_github::fake::{Commit, FakeSource};
use letibot_github::mirror::Mirrors;
use letibot_github::seat::{Config, Job, Seat, Session};
use letibot_github::source::{Comment, Event, Pull, Review, Scope, Standing};
use letibot_tools::gatekeeper::{Decision, LookedAt, Verdict};

use super::*;

/// A member that answers what the test told it to, and records what it was handed.
struct Stub {
    name: String,
    decision: Decision,
    reasons: Vec<String>,
    /// A refusal instead of a verdict, so the panel's own failure path has a way to be reached.
    refuse: Option<String>,
    /// What the tree and the request looked like from inside the member.
    seen: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Stub {
    fn says(name: &str, decision: Decision, reasons: &[&str]) -> Stub {
        Stub {
            name: name.into(),
            decision,
            reasons: reasons.iter().map(|r| r.to_string()).collect(),
            refuse: None,
            seen: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn refusing(name: &str, why: &str) -> Stub {
        Stub {
            refuse: Some(why.into()),
            ..Stub::says(name, Decision::Accept, &[])
        }
    }
}

impl Member for Stub {
    fn name(&self) -> &str {
        &self.name
    }

    fn judge(&mut self, case: &Case<'_>) -> Result<Verdict, String> {
        if let Some(why) = &self.refuse {
            return Err(why.clone());
        }
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(format!("{} in {}", case.pull.key(), case.tree.display()));
            seen.push(case.request.brief.clone());
        }
        Ok(Verdict::for_request(
            case.request,
            self.decision,
            self.reasons.clone(),
            LookedAt {
                files: vec!["src/lib.rs".into()],
                commands: vec!["git diff base...head".into()],
            },
        ))
    }
}

fn a_pull(repo: &str) -> Pull {
    Pull {
        repo: repo.into(),
        number: 12,
        head_sha: "c1".into(),
        base_sha: "b0".into(),
        branch: "topic".into(),
        from_fork: false,
        title: "load the widget once".into(),
        description: "the widget was loaded twice and the second load won".into(),
        author: "someone".into(),
        updated_ms: 1_000,
    }
}

fn a_session(tree: &Path) -> Session {
    Session {
        id: "panel:stroppy-io/letibot#12".into(),
        worktree: tree.to_path_buf(),
    }
}

/// The job a seat hands the panel: the diff it has read, and the pull request it is of.
fn a_job<'a>(pull: &'a Pull, session: &'a Session, diff: &'a str) -> Job<'a> {
    Job {
        pull,
        session,
        diff,
        since: None,
        standing: Standing::Trusted,
        comments: &[],
    }
}

/// The panel's verdict is the gatekeeper's over every member: one refusal is a refusal, and the one
/// GitHub review carries every member's words with the decision in front of them.
#[test]
fn a_panels_verdicts_become_one_review() {
    let pull = a_pull("stroppy-io/letibot");
    let session = a_session(Path::new("/trees/stroppy-io-letibot-12"));
    let mut panel = Panel::new(
        Mirrors::new("/nowhere"),
        vec![
            Box::new(Stub::says(
                "reads the diff",
                Decision::Accept,
                &["the change is the ask"],
            )),
            Box::new(Stub::says(
                "runs the tests",
                Decision::Reject,
                &["crates/x/src/lib.rs:42: this leaks the handle"],
            )),
        ],
    );
    assert_eq!(panel.members(), vec!["reads the diff", "runs the tests"]);
    let review = panel
        .review(&a_job(&pull, &session, "-twice\n+once"))
        .expect("a review");

    assert_eq!(review.event, Event::Comment, "a reviewer never approves");
    assert!(
        review.body.starts_with("verdict: reject"),
        "the decision comes first: {}",
        review.body
    );
    assert!(
        review
            .body
            .contains("reads the diff: the change is the ask")
    );
    assert!(
        review.body.contains("looked at: src/lib.rs"),
        "{}",
        review.body
    );
    assert!(review.body.contains("ran: git diff base...head"));
    // A reason spelled `path:LINE: what is wrong` is an inline comment on that line.
    assert_eq!(review.comments.len(), 1, "{review:#?}");
    assert_eq!(review.comments[0].path, "crates/x/src/lib.rs");
    assert_eq!(review.comments[0].line, 42);
    assert_eq!(review.comments[0].body, "this leaks the handle");
}

/// The gatekeeper's rule, one arm at a time: a refusal wins, an undecided member asks a person, and
/// only a panel that all accepted accepts.
#[test]
fn the_strictest_verdict_is_the_panels() {
    assert_eq!(decide(&[Decision::Accept]), Ok(Decision::Accept));
    assert_eq!(
        decide(&[Decision::Accept, Decision::Accept]),
        Ok(Decision::Accept)
    );
    assert_eq!(
        decide(&[Decision::Accept, Decision::NeedsHuman]),
        Ok(Decision::NeedsHuman)
    );
    assert_eq!(
        decide(&[Decision::NeedsHuman, Decision::Reject]),
        Ok(Decision::Reject)
    );
    // A panel with nobody on it has judged nothing, and that is never a pass.
    let why = decide(&[]).expect_err("an empty panel decides nothing");
    assert!(why.contains("no member"), "{why}");
}

/// One member that cannot judge refuses the whole panel: half a panel is a review nobody
/// configured, and accepting on the half that answered would be the defect.
#[test]
fn a_member_that_cannot_judge_refuses_the_panel() {
    let pull = a_pull("stroppy-io/letibot");
    let session = a_session(Path::new("/trees/stroppy-io-letibot-12"));
    let mut panel = Panel::new(
        Mirrors::new("/nowhere"),
        vec![
            Box::new(Stub::says("reads the diff", Decision::Accept, &["fine"])),
            Box::new(Stub::refusing("runs the tests", "the tree has no tests")),
        ],
    );
    let why = panel
        .review(&a_job(&pull, &session, "-twice\n+once"))
        .expect_err("half a panel is not a review");
    assert!(why.contains("runs the tests"), "{why}");
    assert!(why.contains("the tree has no tests"), "{why}");
}

/// The queue's request is what a member judges against: the pull request's own words, what has been
/// said on it, and the fork's warning - a reviewer with only a diff reviews the wrong change.
#[test]
fn the_request_carries_the_ask_the_conversation_and_the_forks_warning() {
    let pull = a_pull("stroppy-io/letibot");
    let session = a_session(Path::new("/trees/stroppy-io-letibot-12"));
    let comments = vec![Comment {
        id: 7,
        author: "someone".into(),
        body: "why not the other way?".into(),
        created_ms: 0,
        in_review: false,
    }];
    let job = Job {
        pull: &pull,
        session: &session,
        diff: "-twice\n+once",
        since: None,
        standing: Standing::ReadOnly,
        comments: &comments,
    };
    let request = request_of(&job);
    assert!(
        request.brief.contains("load the widget once"),
        "{request:#?}"
    );
    assert!(
        request.brief.contains("the second load won"),
        "{request:#?}"
    );
    assert!(
        request.brief.contains("why not the other way?"),
        "{request:#?}"
    );
    assert!(request.brief.contains("run nothing"), "{request:#?}");
    assert_eq!(request.branch, "topic");
    assert_eq!(request.base_sha, "b0");
    assert_eq!(request.head_sha, "c1");
    assert_eq!(request.cwd, "/trees/stroppy-io-letibot-12");
}

// ===== A panel over the seat, a mirror and a real repository =====

/// A directory of this test's own, removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "letibot-panel-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp dir");
        TempDir(std::fs::canonicalize(&dir).expect("a real path"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "letibot fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@letibot.invalid")
        .env("GIT_COMMITTER_NAME", "letibot fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@letibot.invalid")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repository with a pull request on it, and the `file://` URL a mirror clones it from.
fn a_repository(dir: &TempDir, name: &str) -> (PathBuf, String) {
    let repo = dir.0.join(name);
    std::fs::create_dir_all(&repo).expect("the repository's directory");
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("the-change.txt"), "the change\n").expect("a file");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "the change"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["update-ref", "refs/pull/12/head", &head]);
    let url = format!("file://{}", repo.display());
    (repo, url)
}

/// **The target's third sentence**: a review runs the panel, and its verdict is ONE review draft on
/// the row - with the tree the panel cut from the pull request's own mirror handed to the member.
#[test]
fn a_review_runs_the_panel_and_its_verdict_becomes_one_review_draft() {
    let dir = TempDir::new("seat");
    let (_repo, url) = a_repository(&dir, "letibot");
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut member = Stub::says(
        "reads the diff",
        Decision::Reject,
        &["crates/x/src/lib.rs:42: this leaks the handle"],
    );
    member.seen = seen.clone();

    let mut panel = Panel::new(Mirrors::new(dir.0.join("github")), vec![Box::new(member)])
        .with_url(Arc::new(move |_: &str| url.clone()));

    let source = FakeSource::new();
    let pull = a_pull("stroppy-io/letibot");
    source.open(
        &pull,
        vec![Commit::new("c1", "-the widget twice\n+the widget once")],
    );
    let mut seat = Seat::new(vec![Scope::Org("stroppy-io".into())], Config::default());
    seat.poll(&source, &mut panel, 1_000);

    // One draft, and it is the panel's review: the row owes nothing else and nothing is posted.
    let drafts = seat.drafts();
    assert_eq!(drafts.len(), 1, "{drafts:#?}");
    let Review {
        body,
        event,
        comments,
    } = match drafts[0] {
        letibot_github::Draft::Review { review, .. } => review,
        other => panic!("a review is what a judged pull request keeps: {other:#?}"),
    };
    assert_eq!(*event, Event::Comment);
    assert!(body.starts_with("verdict: reject"), "{body}");
    assert!(body.contains("reads the diff:"), "{body}");
    assert_eq!(comments.len(), 1, "{comments:#?}");
    assert_eq!(comments[0].line, 42);
    assert!(source.reviews().is_empty(), "posting is off by default");
    assert_eq!(seat.rows()[0].state, letibot_github::ReviewState::Drafted);

    // And the member was handed a tree of this repository, at the pull request's head.
    let seen = seen.lock().expect("the member's record");
    assert!(seen[0].starts_with("stroppy-io/letibot#12 in "), "{seen:?}");
    let tree = PathBuf::from(seen[0].trim_start_matches("stroppy-io/letibot#12 in "));
    assert_eq!(
        std::fs::read_to_string(tree.join("the-change.txt")).expect("the head's file"),
        "the change\n"
    );
    assert!(seen[1].contains("load the widget once"), "{seen:?}");
}

// ===== R116's reviewer as one member =====

/// A stub `claude`: it appends its arguments to `argv.txt` in the tree it was started in and then
/// prints the result object the headless docs give.
fn a_claude_stub(dir: &TempDir, structured: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.0.join("claude");
    let script = format!(
        "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> argv.txt; done\n\
         echo '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\
         \"result\":\"I read the change.\",\"session_id\":\"cc-1\",\
         \"structured_output\":{structured}}}'\n"
    );
    std::fs::write(&path, script).expect("the stub is written");
    let mut perms = std::fs::metadata(&path).expect("the stub").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("the stub is executable");
    path
}

/// R116's reviewer is one member of the panel: the run's `structured_output` is the queue's verdict,
/// the prompt is the queue's own told which door to answer through, and the first run carries no
/// `--resume` because there is no conversation yet.
#[test]
fn a_claude_run_is_one_member_of_the_panel() {
    let dir = TempDir::new("claude-member");
    let binary = a_claude_stub(
        &dir,
        r#"{"decision":"reject","reasons":["crates/x/src/lib.rs:42: this leaks"],"files":["crates/x/src/lib.rs"],"commands":["git diff b0...c1"]}"#,
    );
    let runs = Arc::new(claude::Runs::new());
    let settings = claude::Settings::new(binary);
    let claude = Claude::new(runs, settings);
    let pull = a_pull("stroppy-io/letibot");
    // The tree a run works in: the stub records its argv here, which is how the flags are asserted.
    let tree = dir.0.join("tree");
    std::fs::create_dir_all(&tree).expect("the run's tree");
    let session = a_session(&tree);
    let mut panel = Panel::new(Mirrors::new(dir.0.join("github")), vec![Box::new(claude)]);
    let review = panel
        .review(&a_job(&pull, &session, "-twice\n+once"))
        .expect("a review");

    assert!(
        review.body.starts_with("verdict: reject"),
        "{}",
        review.body
    );
    assert_eq!(review.comments.len(), 1, "{review:#?}");
    assert_eq!(review.comments[0].path, "crates/x/src/lib.rs");

    let argv = std::fs::read_to_string(tree.join("argv.txt")).expect("the run's argv");
    assert!(argv.contains("--output-format\njson"), "{argv}");
    assert!(argv.contains("--json-schema"), "{argv}");
    // The queue's prompt, told the door this reviewer has.
    assert!(argv.contains("structured output"), "{argv}");
    assert!(
        !argv.contains("--resume"),
        "the first run resumes nothing: {argv}"
    );
}

/// A run that answers with no `structured_output` has judged nothing, so the member refuses and the
/// panel refuses with it - never a pass on an empty answer.
#[test]
fn a_claude_run_without_a_verdict_refuses_the_panel() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("claude-prose");
    let binary = dir.0.join("claude");
    std::fs::write(
        &binary,
        "#!/bin/sh\necho '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\
         \"result\":\"Looks good to me!\",\"session_id\":\"cc-2\"}'\n",
    )
    .expect("the stub is written");
    let mut perms = std::fs::metadata(&binary).expect("the stub").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&binary, perms).expect("the stub is executable");

    let runs = Arc::new(claude::Runs::new());
    let claude = Claude::new(runs, claude::Settings::new(binary));
    let pull = a_pull("stroppy-io/letibot");
    let session = a_session(&dir.0);
    let mut panel = Panel::new(Mirrors::new(dir.0.join("github")), vec![Box::new(claude)]);
    let why = panel
        .review(&a_job(&pull, &session, "-twice\n+once"))
        .expect_err("prose is not a verdict");
    assert!(why.contains("structured_output"), "{why}");
}

/// A mention with no member that holds a conversation is refused by name: a seat that answered as
/// nobody would be a stranger's comment on the operator's repository.
#[test]
fn a_mention_with_no_correspondent_is_refused() {
    let mut panel = Panel::new(
        Mirrors::new("/nowhere"),
        vec![Box::new(Stub::says(
            "reads the diff",
            Decision::Accept,
            &["fine"],
        ))],
    );
    let why = panel
        .answer(&a_session(Path::new("/trees/x")), "@letibot is this safe?")
        .expect_err("nobody to answer");
    assert!(why.contains("no member"), "{why}");
}
