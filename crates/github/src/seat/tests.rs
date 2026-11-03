//! The seat's behaviour, with no network, no token and no `gh`.
//!
//! Every test here is one sentence of the operator's ask: a new pull request gets one draft, a push
//! gets a review of the new commits only, a mention gets an answer, a fork gets a review and runs
//! nothing, and with `post = false` nothing leaves the box.

use std::path::PathBuf;

use super::*;
use crate::fake::{Commit, FakeSessions, FakeSource};
use crate::source::{Event, Inline};
use crate::worker::Worker;

/// One review the seat asked for, as the fake reviewer saw it.
#[derive(Debug, Clone, PartialEq)]
struct Asked {
    key: String,
    session: String,
    diff: String,
    since: Option<String>,
    standing: Standing,
    description: String,
    /// What had already been said on the pull request, in the order GitHub lists it.
    comments: Vec<String>,
}

/// The review machinery, faked: it records what it was asked and answers a fixed review, which is
/// the whole claim "the seat ran a review" can be tested with offline.
#[derive(Debug, Default)]
struct FakeReviewer {
    opened: Vec<(String, Standing)>,
    jobs: Vec<Asked>,
    answers: Vec<(String, String)>,
    /// A refusal for the next `review`, so the retry path has a way to be reached.
    refuse: Option<String>,
}

impl Reviewer for FakeReviewer {
    fn open(&mut self, pull: &Pull, standing: Standing) -> Result<Session, String> {
        self.opened.push((pull.key(), standing));
        Ok(Session {
            id: format!("s-{}-{}", pull.repo.replace('/', "-"), pull.number),
            worktree: PathBuf::from(format!("/worktrees/{}", pull.number)),
        })
    }

    fn review(&mut self, job: &Job<'_>) -> Result<Review, String> {
        if let Some(why) = self.refuse.take() {
            return Err(why);
        }
        self.jobs.push(Asked {
            key: job.pull.key(),
            session: job.session.id.clone(),
            diff: job.diff.to_string(),
            since: job.since.map(str::to_string),
            standing: job.standing,
            description: job.pull.description.clone(),
            comments: job.comments.iter().map(|c| c.body.clone()).collect(),
        });
        Ok(Review {
            body: format!("a review of {} changed lines", job.diff.lines().count()),
            event: Event::Comment,
            comments: vec![Inline {
                path: "src/lib.rs".into(),
                line: 3,
                body: "this leaks".into(),
            }],
        })
    }

    fn answer(&mut self, session: &Session, question: &str) -> Result<String, String> {
        self.answers
            .push((session.id.clone(), question.to_string()));
        Ok(format!("because: {question}"))
    }
}

fn a_pull(number: u64, head: &str) -> Pull {
    Pull {
        repo: "stroppy-io/letibot".into(),
        number,
        head_sha: head.into(),
        base_sha: "b0".into(),
        branch: format!("topic-{number}"),
        from_fork: false,
        title: "a fix".into(),
        description: String::new(),
        author: "someone".into(),
        updated_ms: 1_000,
    }
}

fn a_source() -> FakeSource {
    let fake = FakeSource::new();
    fake.open(
        &a_pull(12, "c1"),
        vec![Commit::new("c1", "-first\n+first change")],
    );
    fake
}

fn a_seat() -> Seat {
    Seat::new(vec![Scope::Org("stroppy-io".into())], Config::default())
}

/// **A new pull request gets one review draft and nothing is sent**: that is the shipped state until
/// the operator's identity exists.
#[test]
fn a_new_pull_request_gets_one_draft_and_nothing_is_sent() {
    let source = a_source();
    let mut reviewer = FakeReviewer::default();
    let mut seat = a_seat();
    seat.poll(&source, &mut reviewer, 1_000);

    assert_eq!(reviewer.jobs.len(), 1, "{reviewer:#?}");
    assert_eq!(reviewer.jobs[0].diff, "-first\n+first change");
    assert_eq!(reviewer.jobs[0].since, None, "the whole pull request");
    assert_eq!(reviewer.jobs[0].key, "stroppy-io/letibot#12");
    assert!(source.reviews().is_empty(), "posting is off by default");

    let rows = seat.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, ReviewState::Drafted);
    assert_eq!(rows[0].reviewed_sha.as_deref(), Some("c1"));
    let drafts = seat.drafts();
    assert_eq!(drafts.len(), 1);
    assert!(matches!(&drafts[0], Draft::Review { for_sha, review }
        if for_sha == "c1" && review.comments.len() == 1));

    // A second poll with no push asks for nothing: one review per push.
    seat.poll(&source, &mut reviewer, 2_000);
    assert_eq!(reviewer.jobs.len(), 1);
    assert_eq!(seat.drafts().len(), 1);
}

/// **A review is handed what a reviewer needs**: what the author said the pull request is, and
/// what has already been said on it. A reviewer given a bare diff cannot answer either.
#[test]
fn a_review_is_handed_the_description_and_the_comments_so_far() {
    let source = FakeSource::new();
    let mut pull = a_pull(12, "c1");
    pull.description = "this changes how the widget loads".into();
    source.open(&pull, vec![Commit::new("c1", "-first\n+first change")]);
    source.comment(
        "stroppy-io/letibot",
        12,
        Comment {
            id: 4,
            author: "someone".into(),
            body: "why not the other way?".into(),
            created_ms: 500,
            in_review: false,
        },
    );
    let mut reviewer = FakeReviewer::default();
    let mut seat = a_seat();
    seat.poll(&source, &mut reviewer, 1_000);

    let job = &reviewer.jobs[0];
    assert_eq!(job.description, "this changes how the widget loads");
    assert_eq!(job.comments, vec!["why not the other way?".to_string()]);
    assert_eq!(job.diff, "-first\n+first change");
}

/// **A push is reviewed as the commits it added**, and the review it makes is a second review - not
/// a second review of the whole pull request.
#[test]
fn a_push_is_reviewed_as_the_new_commits_only() {
    let source = a_source();
    let mut reviewer = FakeReviewer::default();
    let mut seat = a_seat();
    seat.poll(&source, &mut reviewer, 1_000);
    source.push(
        "stroppy-io/letibot",
        12,
        Commit::new("c2", "-second\n+second change"),
    );
    seat.poll(&source, &mut reviewer, 2_000);

    assert_eq!(reviewer.jobs.len(), 2, "{reviewer:#?}");
    assert_eq!(reviewer.jobs[1].since.as_deref(), Some("c1"));
    assert_eq!(reviewer.jobs[1].diff, "-second\n+second change");
    assert!(
        !reviewer.jobs[1].diff.contains("first change"),
        "the reviewed commit must not be reviewed again"
    );
    assert_eq!(
        source.diffs_asked(),
        vec![
            ("stroppy-io/letibot".to_string(), 12, None),
            ("stroppy-io/letibot".to_string(), 12, Some("c1".to_string()))
        ]
    );
    assert_eq!(seat.rows()[0].reviewed_sha.as_deref(), Some("c2"));
    assert_eq!(seat.rows()[0].state, ReviewState::Drafted);
    assert_eq!(seat.drafts().len(), 2, "one review per push");
    assert!(
        seat.rows()[0].last_action.contains("new commits"),
        "{}",
        seat.rows()[0].last_action
    );
}

/// **A mention is answered by that pull request's session**, once, and by nobody else.
#[test]
fn a_mention_is_answered_by_the_pull_requests_session() {
    let source = a_source();
    let mut reviewer = FakeReviewer::default();
    let mut seat = a_seat();
    seat.poll(&source, &mut reviewer, 1_000);
    let session = reviewer.jobs[0].session.clone();

    source.comment(
        "stroppy-io/letibot",
        12,
        Comment {
            id: 7,
            author: "someone".into(),
            body: "@letibot is this safe to merge?".into(),
            created_ms: 2_000,
            in_review: false,
        },
    );
    seat.poll(&source, &mut reviewer, 3_000);
    assert_eq!(reviewer.answers.len(), 1, "{reviewer:#?}");
    assert_eq!(reviewer.answers[0].0, session);
    assert!(reviewer.answers[0].1.contains("is this safe to merge?"));
    let drafts = seat.drafts();
    assert_eq!(drafts.len(), 2, "the review and the answer");
    assert!(
        matches!(&drafts[1], Draft::Reply { comment, body } if *comment == 7 && body.contains("because"))
    );
    assert!(source.replies().is_empty(), "posting is off");
    assert_eq!(seat.rows()[0].state, ReviewState::Drafted);

    // The same comment is not answered twice.
    seat.poll(&source, &mut reviewer, 4_000);
    assert_eq!(reviewer.answers.len(), 1);
}

/// **A fork is reviewed and nothing of it is run**: its session and its job are read-only.
#[test]
fn a_fork_is_reviewed_read_only_and_nothing_runs() {
    let source = FakeSource::new();
    let mut pull = a_pull(31, "f1");
    pull.from_fork = true;
    pull.branch = "patch-1".into();
    source.open(&pull, vec![Commit::new("f1", "-theirs\n+theirs")]);
    let mut reviewer = FakeReviewer::default();
    let mut seat = a_seat();
    seat.poll(&source, &mut reviewer, 1_000);

    assert_eq!(reviewer.opened[0].1, Standing::ReadOnly, "{reviewer:#?}");
    assert_eq!(reviewer.jobs.len(), 1, "it is still reviewed");
    assert_eq!(reviewer.jobs[0].standing, Standing::ReadOnly);
    assert!(!reviewer.jobs[0].standing.may_run());
    assert_eq!(seat.rows()[0].standing, Standing::ReadOnly);
    assert_eq!(seat.drafts().len(), 1);
    assert!(seat.snapshot().scopes[0].pulls[0].fork);
}

/// Posting is on only when a token is there, and with it the review is sent and no draft is kept.
#[test]
fn posting_needs_a_token_and_then_the_review_is_sent() {
    let source = a_source();
    let mut reviewer = FakeReviewer::default();
    let mut seat = Seat::new(
        vec![Scope::Org("stroppy-io".into())],
        Config {
            post: true,
            ..Config::default()
        },
    );
    seat.poll(&source, &mut reviewer, 1_000);
    assert_eq!(
        reviewer.jobs.len(),
        0,
        "nothing is attempted without a token"
    );
    assert_eq!(seat.snapshot().scopes[0].state, "token missing");
    assert!(source.reviews().is_empty());
    assert_eq!(seat.drafts().len(), 0);

    let mut posting = Seat::new(
        vec![Scope::Org("stroppy-io".into())],
        Config {
            post: true,
            token: Some("/run/secrets/github.token".into()),
            ..Config::default()
        },
    );
    posting.poll(&source, &mut reviewer, 2_000);
    assert_eq!(source.reviews().len(), 1);
    assert_eq!(posting.rows()[0].state, ReviewState::Posted);
    assert!(posting.drafts().is_empty(), "a sent review is not a draft");
    assert!(
        posting.rows()[0].last_action.contains("posted review"),
        "{}",
        posting.rows()[0].last_action
    );
}

/// The cap is per repository per hour: past it a row waits, and the scope says why.
#[test]
fn a_spent_cap_leaves_the_rest_queued() {
    let source = a_source();
    source.open(
        &a_pull(13, "d1"),
        vec![Commit::new("d1", "-another\n+another change")],
    );
    let mut reviewer = FakeReviewer::default();
    let mut seat = Seat::new(
        vec![Scope::Org("stroppy-io".into())],
        Config {
            reviews_per_hour: 1,
            ..Config::default()
        },
    );
    seat.poll(&source, &mut reviewer, 1_000);
    assert_eq!(reviewer.jobs.len(), 1, "{reviewer:#?}");
    assert_eq!(seat.rows()[0].state, ReviewState::Drafted);
    assert_eq!(seat.rows()[1].state, ReviewState::Queued);
    assert_eq!(seat.snapshot().scopes[0].state, "rate-limited");
}

/// A source that cannot be listed says so on the scope, and one the operator paused is not asked.
#[test]
fn a_refused_list_is_a_state_and_a_pause_stops_the_asking() {
    struct Refusing;
    impl Source for Refusing {
        fn list_open(&self, _: &Scope) -> Result<Vec<Pull>, String> {
            Err("`gh` is not installed".into())
        }
        fn diff(&self, _: &Pull, _: Option<&str>) -> Result<String, String> {
            unreachable!()
        }
        fn comments(&self, _: &Pull) -> Result<Vec<Comment>, String> {
            unreachable!()
        }
        fn post_review(&self, _: &Pull, _: &Review) -> Result<u64, String> {
            unreachable!()
        }
        fn reply(&self, _: &Pull, _: &Comment, _: &str) -> Result<u64, String> {
            unreachable!()
        }
    }
    let mut reviewer = FakeReviewer::default();
    let mut seat = a_seat();
    seat.poll(&Refusing, &mut reviewer, 1_000);
    let scope = &seat.snapshot().scopes[0];
    assert_eq!(scope.state, "failed");
    assert!(scope.why.contains("gh` is not installed"), "{scope:#?}");

    let source = a_source();
    let mut seat = a_seat();
    seat.pause("stroppy-io");
    seat.poll(&source, &mut reviewer, 1_000);
    assert_eq!(reviewer.jobs.len(), 0);
    assert_eq!(seat.snapshot().scopes[0].state, "paused");
    seat.resume("stroppy-io");
    seat.poll(&source, &mut reviewer, 2_000);
    assert_eq!(reviewer.jobs.len(), 1);
}

/// A review that could not be made stays owed, with the reason on the row, and the next poll tries
/// again rather than losing the push.
#[test]
fn a_review_that_failed_is_retried() {
    let source = a_source();
    let mut reviewer = FakeReviewer {
        refuse: Some("the model is down".into()),
        ..FakeReviewer::default()
    };
    let mut seat = a_seat();
    seat.poll(&source, &mut reviewer, 1_000);
    let row = &seat.rows()[0];
    assert_eq!(row.state, ReviewState::Queued);
    assert_eq!(row.failure, "the model is down");
    assert!(
        row.last_action.contains("could not review"),
        "{}",
        row.last_action
    );
    assert!(seat.drafts().is_empty());

    seat.poll(&source, &mut reviewer, 2_000);
    assert_eq!(seat.rows()[0].state, ReviewState::Drafted);
    assert_eq!(seat.rows()[0].failure, "");
    assert_eq!(seat.drafts().len(), 1);
}

/// The snapshot is what the pane draws: one line per scope, one row per pull request, with the
/// review's state, the last action, the age, and the session Enter attaches to.
#[test]
fn the_snapshot_is_the_panes_rows() {
    let source = a_source();
    let mut reviewer = FakeReviewer::default();
    let mut seat = a_seat();
    seat.poll(&source, &mut reviewer, 1_000);
    seat.poll(&source, &mut reviewer, 61_000);
    let snapshot = seat.snapshot();
    assert!(!snapshot.posting);
    assert_eq!(snapshot.identity, "letibot");
    assert_eq!(snapshot.scopes.len(), 1);
    let scope = &snapshot.scopes[0];
    assert_eq!(scope.scope, "stroppy-io");
    assert_eq!(scope.state, "polling");
    assert_eq!(scope.pulls.len(), 1);
    let pull = &scope.pulls[0];
    assert_eq!(pull.repo, "stroppy-io/letibot");
    assert_eq!(pull.number, 12);
    assert_eq!(pull.review, "drafted");
    assert!(
        pull.last_action.contains("drafted a review"),
        "{}",
        pull.last_action
    );
    assert_eq!(pull.age_ms, 60_000);
    assert_eq!(pull.session, "s-stroppy-io-letibot-12");
    assert_eq!(pull.drafts, 1);
    // The wire a pane reads round-trips, so a renamed field breaks a compilation and not a pane.
    let wire = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(serde_json::from_value::<Snapshot>(wire).unwrap(), snapshot);
}

/// A pull request that is closed and no longer listed is dropped: the pane draws what is open.
#[test]
fn a_closed_pull_request_is_dropped() {
    let source = a_source();
    let mut reviewer = FakeReviewer::default();
    let mut seat = Seat::new(
        vec![Scope::Repo("stroppy-io/letibot".into())],
        Config::default(),
    );
    seat.poll(&source, &mut reviewer, 1_000);
    assert_eq!(seat.rows().len(), 1);
    let other = Seat::new(
        vec![Scope::Repo("stroppy-io/nothing".into())],
        Config::default(),
    );
    assert!(other.rows().is_empty());
    // A scope that lists nothing keeps no row, even though the seat has met one.
    let mut seat = a_seat();
    seat.poll(&source, &mut reviewer, 1_000);
    source.close("stroppy-io/letibot", 12);
    seat.poll(&source, &mut reviewer, 2_000);
    assert!(seat.rows().is_empty());
}

/// **A token that anyone can read is not a token.** Posting needs the file, and the file's mode is
/// checked here so the child that reads it later is not the first to notice.
#[test]
fn posting_needs_a_token_file_only_the_operator_can_read() {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    // Posting off: the path is not even looked at.
    let cfg = Config::resolved(false, Some("/nowhere/token"), Path::new("/nowhere")).unwrap();
    assert!(!cfg.posting());

    let home = Path::new("/home/operator");
    let why = Config::resolved(true, None, home).unwrap_err();
    assert!(why.contains("token_path"), "{why}");
    let why = Config::resolved(true, Some("~/.config/github.token"), home).unwrap_err();
    assert!(
        why.contains("github.token") && why.contains("not there"),
        "{why}"
    );

    let dir = std::env::temp_dir().join(format!("letibot-github-token-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("home/.config/letibot")).unwrap();
    let token = dir.join("home/.config/letibot/github.token");
    std::fs::write(&token, "ghp_x\n").unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
    let why = Config::resolved(true, Some(token.to_str().unwrap()), &dir.join("home")).unwrap_err();
    assert!(why.contains("0644") && why.contains("0600"), "{why}");

    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let cfg = Config::resolved(true, Some(token.to_str().unwrap()), &dir.join("home")).unwrap();
    assert!(cfg.posting());
    assert_eq!(cfg.token.as_deref(), Some(token.to_str().unwrap()));

    // `~/` is the operator's home, which is what a config file means by it.
    let cfg = Config::resolved(
        true,
        Some("~/.config/letibot/github.token"),
        &dir.join("home"),
    )
    .unwrap();
    assert_eq!(cfg.token.as_deref(), Some(token.to_str().unwrap()));
}

// ---- the worker's path: the brief, the session, the draft, and the gate ----------

/// A seat whose reviewer is the real one over faked sessions: what the operator's four claims are
/// about, since the fake reviewer elsewhere answers a review without a session at all.
fn a_worker_seat(sessions: FakeSessions, config: Config) -> (Seat, Worker<FakeSessions>) {
    (
        Seat::new(vec![Scope::Org("stroppy-io".into())], config),
        Worker::new(sessions),
    )
}

/// A session's answer that is a review: a summary and one comment on a line.
const A_REVIEW: &str = "This loads the widget once now.\n\ncrates/tui/src/widget.rs:42: this leaks";

/// **A push is one more draft, and its brief is the new commits only.** A second review of the same
/// commits is the defect this measures: the row is owed one review per push and no more.
#[test]
fn a_new_push_is_one_more_draft_and_its_brief_is_the_new_commits() {
    let source = a_source();
    let sessions = FakeSessions::answering(A_REVIEW);
    let (mut seat, mut worker) = a_worker_seat(sessions, Config::default());
    seat.poll(&source, &mut worker, 1_000);
    assert_eq!(seat.drafts().len(), 1);
    assert!(
        matches!(&seat.drafts()[0], Draft::Review { for_sha, review }
        if for_sha == "c1" && review.comments[0].path == "crates/tui/src/widget.rs")
    );

    source.push(
        "stroppy-io/letibot",
        12,
        Commit::new("c2", "-second\n+second change"),
    );
    seat.poll(&source, &mut worker, 2_000);
    let drafts = seat.drafts();
    assert_eq!(drafts.len(), 2, "one draft per push");
    assert!(
        matches!(&drafts[1], Draft::Review { for_sha, .. } if for_sha == "c2"),
        "{drafts:#?}"
    );

    // Both reviews ran in the same session, and the second one was told what it is measured
    // against so it does not review the whole pull request again.
    let briefs = worker.sessions().briefs();
    assert_eq!(briefs.len(), 2, "{briefs:#?}");
    assert_eq!(briefs[0].0, briefs[1].0, "one session per pull request");
    assert!(
        briefs[0].1.contains("-first\n+first change"),
        "{}",
        briefs[0].1
    );
    assert!(
        briefs[1].1.contains("-second\n+second change"),
        "{}",
        briefs[1].1
    );
    assert!(
        briefs[1].1.contains("since c1"),
        "the re-review's own commits: {}",
        briefs[1].1
    );
    assert_eq!(worker.sessions().opened().len(), 1, "the session is reused");

    // And a poll with nothing new asks nothing: one review per push.
    seat.poll(&source, &mut worker, 3_000);
    assert_eq!(seat.drafts().len(), 2);
    assert_eq!(worker.sessions().briefs().len(), 2);
}

/// **A mention is answered by that pull request's session** - the one the review ran in - and the
/// answer is kept as a draft while nothing is posted.
#[test]
fn a_mention_is_answered_by_that_pull_request_s_session() {
    let source = a_source();
    let sessions = FakeSessions::new();
    sessions.say(A_REVIEW);
    sessions.say("Because the second load raced the first.");
    let (mut seat, mut worker) = a_worker_seat(sessions, Config::default());
    seat.poll(&source, &mut worker, 1_000);
    let session = seat.rows()[0]
        .session
        .as_ref()
        .expect("a session to answer in")
        .id
        .clone();

    source.comment(
        "stroppy-io/letibot",
        12,
        Comment {
            id: 7,
            author: "someone".into(),
            body: "@letibot is this safe to merge?".into(),
            created_ms: 2_000,
            in_review: false,
        },
    );
    seat.poll(&source, &mut worker, 3_000);
    let drafts = seat.drafts();
    assert_eq!(drafts.len(), 2, "the review and the answer");
    assert!(
        matches!(&drafts[1], Draft::Reply { comment: 7, body } if body.contains("raced the first")),
        "{drafts:#?}"
    );
    let briefs = worker.sessions().briefs();
    assert_eq!(briefs.len(), 2, "{briefs:#?}");
    assert_eq!(briefs[1].0, session, "the pull request's own session");
    assert!(
        briefs[1].1.contains("is this safe to merge?"),
        "{}",
        briefs[1].1
    );
    assert!(source.replies().is_empty(), "nothing is posted");
}

/// **`post = false` is a gate, not a default**: with a token file in hand and everything reviewed,
/// not one review or answer leaves the box.
#[test]
fn posting_is_refused_while_post_is_false() {
    let source = a_source();
    let sessions = FakeSessions::answering(A_REVIEW);
    let (mut seat, mut worker) = a_worker_seat(
        sessions,
        Config {
            post: false,
            token: Some("/run/secrets/github.token".into()),
            ..Config::default()
        },
    );
    seat.poll(&source, &mut worker, 1_000);
    source.comment(
        "stroppy-io/letibot",
        12,
        Comment {
            id: 7,
            author: "someone".into(),
            body: "@letibot why?".into(),
            created_ms: 2_000,
            in_review: false,
        },
    );
    seat.poll(&source, &mut worker, 3_000);

    assert_eq!(seat.drafts().len(), 2, "both are kept, neither is sent");
    assert!(source.reviews().is_empty(), "no review was posted");
    assert!(source.replies().is_empty(), "no answer was posted");
    assert_eq!(seat.rows()[0].state, ReviewState::Drafted);
    assert!(!seat.snapshot().posting);
}

/// **Six reviews an hour per repository, and the seventh waits** - then it is owed again once the
/// hour has passed, rather than lost.
#[test]
fn the_hourly_cap_holds_at_six_and_frees_the_seventh_an_hour_later() {
    let source = FakeSource::new();
    for number in 1..=7u64 {
        source.open(
            &a_pull(number, &format!("c{number}")),
            vec![Commit::new(format!("c{number}"), "-a\n+b")],
        );
    }
    let sessions = FakeSessions::answering(A_REVIEW);
    let (mut seat, mut worker) = a_worker_seat(sessions, Config::default());
    assert_eq!(seat.config().reviews_per_hour, 6, "the fixed cap");

    seat.poll(&source, &mut worker, 1_000);
    assert_eq!(seat.drafts().len(), 6, "six of seven reviewed");
    assert_eq!(seat.rows()[6].state, ReviewState::Queued);
    assert_eq!(seat.snapshot().scopes[0].state, "rate-limited");
    assert!(
        seat.rows()[6].last_action.contains("hourly cap")
            || seat.snapshot().scopes[0].why.contains("cap"),
        "the row or the scope says why: {}",
        seat.rows()[6].last_action
    );

    // An hour later the window has moved, so the seventh is owed a review and gets one.
    seat.poll(&source, &mut worker, 1_000 + 3_600_001);
    assert_eq!(seat.drafts().len(), 7);
    assert_eq!(seat.rows()[6].state, ReviewState::Drafted);
}
