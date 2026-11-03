//! The worker's own policy, offline: what a brief carries, and what an answer means.

use super::*;
use crate::fake::FakeSessions;
use crate::source::Comment;

fn a_pull() -> Pull {
    Pull {
        repo: "stroppy-io/letibot".into(),
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

fn a_session() -> Session {
    Session {
        id: "s-stroppy-io-letibot-12".into(),
        worktree: std::path::PathBuf::from("/worktrees/12"),
    }
}

fn a_comment(body: &str) -> Comment {
    Comment {
        id: 4,
        author: "someone".into(),
        body: body.into(),
        created_ms: 500,
        in_review: false,
    }
}

/// **A reviewer that gets only a diff reviews the wrong change.** The brief carries what the author
/// said the pull request is and what has already been said on it, because neither is in the diff.
#[test]
fn the_brief_carries_the_title_the_description_the_diff_and_the_comments() {
    let pull = a_pull();
    let session = a_session();
    let comments = vec![a_comment("why not the other way?")];
    let brief = review_brief(&Job {
        pull: &pull,
        session: &session,
        diff: "-loaded twice\n+loaded once",
        since: None,
        standing: Standing::Trusted,
        comments: &comments,
    });
    for want in [
        "stroppy-io/letibot#12",
        "load the widget once",
        "the widget was loaded twice and the second load won",
        "-loaded twice\n+loaded once",
        "why not the other way?",
    ] {
        assert!(brief.contains(want), "`{want}` is missing from:\n{brief}");
    }
}

/// A re-review is of the new commits only, and the brief says what it was measured against so the
/// session does not review the whole pull request again.
#[test]
fn a_re_review_s_brief_says_what_the_new_commits_are_measured_against() {
    let pull = a_pull();
    let session = a_session();
    let brief = review_brief(&Job {
        pull: &pull,
        session: &session,
        diff: "-second\n+second change",
        since: Some("c1"),
        standing: Standing::Trusted,
        comments: &[],
    });
    assert!(brief.contains("c1"), "{brief}");
    assert!(brief.contains("-second\n+second change"), "{brief}");
    assert!(
        !brief.contains("no description"),
        "the pull request has one: {brief}"
    );
}

/// **A fork's brief says the tree is not ours**, so a session that would run the diff's tests is
/// told not to before it starts rather than after.
#[test]
fn a_fork_s_brief_says_nothing_of_it_runs() {
    let mut pull = a_pull();
    pull.from_fork = true;
    let session = a_session();
    let brief = review_brief(&Job {
        pull: &pull,
        session: &session,
        diff: "-theirs\n+theirs",
        since: None,
        standing: Standing::ReadOnly,
        comments: &[],
    });
    assert!(brief.contains("run nothing"), "{brief}");
}

/// A pull request with no description is still reviewed, and the brief says so rather than leaving
/// a hole where the author's words would be.
#[test]
fn a_pull_request_without_a_description_says_so() {
    let mut pull = a_pull();
    pull.description.clear();
    let session = a_session();
    let brief = review_brief(&Job {
        pull: &pull,
        session: &session,
        diff: "-x\n+y",
        since: None,
        standing: Standing::Trusted,
        comments: &[],
    });
    assert!(brief.contains("no description"), "{brief}");
}

/// **What the session says is the review**: the prose is the summary, and each line spelled
/// `path:LINE: what is wrong` is an inline comment on that line.
#[test]
fn the_session_s_answer_becomes_the_review() {
    let review = review_of(
        "This loads the widget once now.\n\
         \n\
         crates/tui/src/widget.rs:42: this leaks the handle\n\
         Note: the second branch is unreachable.",
    )
    .expect("an answer is a review");
    assert_eq!(
        review.body,
        "This loads the widget once now.\n\nNote: the second branch is unreachable."
    );
    assert_eq!(review.event, Event::Comment, "a reviewer never approves");
    assert_eq!(review.comments.len(), 1, "{review:#?}");
    assert_eq!(review.comments[0].path, "crates/tui/src/widget.rs");
    assert_eq!(review.comments[0].line, 42);
    assert_eq!(review.comments[0].body, "this leaks the handle");
}

/// A session that says nothing has not reviewed: the row goes back to owed rather than keeping an
/// empty review as a draft the operator would post.
#[test]
fn an_empty_answer_is_not_a_review() {
    let why = review_of("   \n\n").expect_err("nothing is not a review");
    assert!(why.contains("nothing"), "{why}");
}

/// The worker asks the session it was handed, and a `review` that a session refuses is a refusal.
#[test]
fn the_review_is_asked_in_the_pull_requests_own_session() {
    let pull = a_pull();
    let session = a_session();
    let sessions = FakeSessions::answering("LGTM.\n\ncrates/x.rs:3: name this");
    let mut worker = Worker::new(sessions);
    let review = worker
        .review(&Job {
            pull: &pull,
            session: &session,
            diff: "-a\n+b",
            since: None,
            standing: Standing::Trusted,
            comments: &[],
        })
        .expect("a review");
    assert_eq!(review.body, "LGTM.");
    assert_eq!(review.comments.len(), 1);
}

/// **A mention is answered by the pull request's session**, and the brief carries the question.
#[test]
fn a_mention_is_asked_in_the_pull_requests_session() {
    let session = a_session();
    let sessions = FakeSessions::answering("Because the second load raced the first.");
    let mut worker = Worker::new(sessions);
    let said = worker
        .answer(&session, "@letibot is this safe to merge?")
        .expect("an answer");
    assert_eq!(said, "Because the second load raced the first.");
    let briefs = worker.sessions().briefs();
    assert_eq!(briefs.len(), 1);
    assert_eq!(briefs[0].0, session.id);
    assert!(briefs[0].1.contains("is this safe to merge?"), "{briefs:?}");
}
