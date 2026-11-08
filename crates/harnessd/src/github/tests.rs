//! The daemon's GitHub seat, end to end and offline: `letibot attach github <scope>` reaching the
//! daemon, the poll a pass makes, the `github` feed, and `/pane github/github` drawn from it.
//!
//! No network and no `gh`: the source is `letibot_github::fake::FakeSource` and the reviewer is the
//! same crate's `Worker` over `FakeSessions`, so every fact here is one this test wrote.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use letibot_github::fake::{Commit, FakeSessions, FakeSource};
use letibot_github::seat::Config;
use letibot_github::source::{Comment, Pull, Review, Scope as Scope_, Source, Standing};
use letibot_github::worker::Worker;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::PaneDriver;
use serde_json::{Value, json};

use super::*;

/// A `FakeSource` one pass at a time: the seat's `Parts` are built per pass, and the pull requests a
/// test opened must be the same ones every pass sees.
struct Shared(Arc<FakeSource>);

impl Source for Shared {
    fn list_open(&self, scope: &Scope_) -> Result<Vec<Pull>, String> {
        self.0.list_open(scope)
    }
    fn diff(&self, pull: &Pull, since: Option<&str>) -> Result<String, String> {
        self.0.diff(pull, since)
    }
    fn comments(&self, pull: &Pull) -> Result<Vec<Comment>, String> {
        self.0.comments(pull)
    }
    fn post_review(&self, pull: &Pull, review: &Review) -> Result<u64, String> {
        self.0.post_review(pull, review)
    }
    fn reply(&self, pull: &Pull, comment: &Comment, body: &str) -> Result<u64, String> {
        self.0.reply(pull, comment, body)
    }
}

/// The `FakeSessions` worker, shared for the same reason: the seat's reviewer is built per pass.
struct SharedReviewer(Arc<Mutex<Worker<FakeSessions>>>);

impl Reviewer for SharedReviewer {
    fn open(
        &mut self,
        pull: &Pull,
        standing: Standing,
    ) -> Result<letibot_github::seat::Session, String> {
        self.0.lock().expect("the reviewer").open(pull, standing)
    }
    fn review(&mut self, job: &letibot_github::seat::Job<'_>) -> Result<Review, String> {
        self.0.lock().expect("the reviewer").review(job)
    }
    fn answer(
        &mut self,
        session: &letibot_github::seat::Session,
        question: &str,
    ) -> Result<String, String> {
        self.0
            .lock()
            .expect("the reviewer")
            .answer(session, question)
    }
}

/// A source with nothing in it, for a test about what is NOT offered.
struct NoCandidates;

impl Candidates for NoCandidates {
    fn candidates(&self) -> Vec<Candidate> {
        Vec::new()
    }
}

/// A list of candidates this test wrote: the open todos and the running sessions are the daemon's,
/// and this is what that list looks like without a registry.
struct Some2(Vec<&'static str>);

impl Candidates for Some2 {
    fn candidates(&self) -> Vec<Candidate> {
        self.0
            .iter()
            .map(|label| Candidate {
                kind: Kind::Watch,
                label: (*label).to_string(),
                text: (*label).to_string(),
            })
            .collect()
    }
}

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

const A_REVIEW: &str = "This loads the widget once now.\n\ncrates/tui/src/widget.rs:42: this leaks";

/// A seat over a fake source and a fake reviewer, with the pull request this file's tests use.
fn a_seat(
    source: &Arc<FakeSource>,
    candidates: Box<dyn Candidates>,
    config: Config,
) -> Arc<Github> {
    let shared = Shared(source.clone());
    let sessions = FakeSessions::answering(A_REVIEW);
    let reviewer = Arc::new(Mutex::new(Worker::new(sessions)));
    let pass: Maker = Box::new(move || {
        Ok(Parts {
            source: Box::new(Shared(shared.0.clone())),
            reviewer: Box::new(SharedReviewer(reviewer.clone())),
        })
    });
    Github::new(config)
        .with_pass(pass, candidates, Box::new(Lexical))
        .install()
}

fn a_source() -> Arc<FakeSource> {
    let source = Arc::new(FakeSource::new());
    source.open(&a_pull(), vec![Commit::new("c1", "-twice\n+once")]);
    source
}

fn tempdir(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "letibot-github-feed-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

/// Every `PluginPane` the hub has published, as the pane's name and the view it answered.
fn views(hub: &Arc<Hub>) -> Vec<(String, Value)> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            letibot_sessionlog::SessionEvent::PluginPane { pane, view } => {
                Some((pane.clone(), view.clone()))
            }
            _ => None,
        })
        .collect()
}

/// **The target's first sentence.** A scope `letibot attach github` asked the daemon to hold is in
/// the feed, and `/pane github/github` draws that scope - not `no scope is attached`.
#[test]
fn the_daemon_answers_the_github_feed_for_an_attached_scope() {
    let source = a_source();
    let seat = a_seat(&source, Box::new(NoCandidates), Config::default());

    // Before anything is attached the pane is handed NO key, so it says so rather than drawing an
    // empty seat.
    assert!(
        GithubSeat::feed(&*seat).is_none(),
        "a daemon holding no scope is no feed at all"
    );

    let attached = seat.attach("stroppy-io");
    assert!(attached.get("error").is_none(), "{attached}");
    assert!(seat.poll(1_000).is_some(), "one pass");

    let feed = GithubSeat::feed(&*seat).expect("an attached scope is a feed");
    assert_eq!(feed["posting"], json!(false), "posting is off by default");
    assert_eq!(feed["scopes"][0]["scope"], json!("stroppy-io"));
    assert_eq!(
        feed["scopes"][0]["pulls"][0]["repo"],
        json!("stroppy-io/letibot")
    );
    assert_eq!(feed["scopes"][0]["pulls"][0]["review"], json!("drafted"));

    // And the pane draws it, through the host the daemon gives a session.
    let dir = tempdir("pane");
    let host = crate::plugins::PluginHost::production("s-github-feed", &dir)
        .with_github(Arc::new(crate::plugins::feed::SeatFeed(seat.clone())));
    let hub = Hub::new("s-github-feed");
    PaneDriver::open(&host, "s-github-feed", &hub, "github/github", (90, 30));
    let drawn = views(&hub);
    let (pane, view) = drawn
        .iter()
        .find(|(pane, _)| pane == "github/github")
        .unwrap_or_else(|| panic!("the github pane was not published: {drawn:?}"));
    assert_eq!(pane, "github/github");
    let text = serde_json::to_string(view).expect("a view");
    assert!(
        text.contains("scope:stroppy-io"),
        "the scope is a drawn row: {text}"
    );
    assert!(
        text.contains("1 open"),
        "and it holds a pull request: {text}"
    );
    assert!(
        text.contains("pr:stroppy-io/letibot#12"),
        "the pull request is drawn under it: {text}"
    );
    assert!(
        text.contains("draft review of c1"),
        "and its draft's words, which is what the operator decides about: {text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The target's last sentence.** With `post` off, a review is a draft on the row and not one
/// review leaves the box - whatever else the seat did.
#[test]
fn nothing_posts_with_post_off() {
    let source = a_source();
    let seat = a_seat(
        &source,
        Box::new(NoCandidates),
        Config {
            post: false,
            // A token file in hand changes nothing: `post` is the gate.
            token: Some("/run/secrets/github.token".into()),
            ..Config::default()
        },
    );
    seat.attach("stroppy-io");
    seat.poll(1_000);

    assert!(source.reviews().is_empty(), "no review was posted");
    assert!(source.replies().is_empty(), "nothing was answered");
    let feed = GithubSeat::feed(&*seat).expect("a feed");
    let drafts = feed["scopes"][0]["pulls"][0]["drafts"]
        .as_array()
        .expect("the drafts");
    assert_eq!(drafts.len(), 1, "{drafts:#?}");
    assert!(
        drafts[0]["body"]
            .as_str()
            .is_some_and(|b| b.contains("loads the widget once")),
        "{drafts:#?}"
    );
    assert_eq!(feed["posting"], json!(false));
}

/// **The target's fourth sentence.** A pull request whose words are like an open todo row or a
/// running session's brief is offered as one to watch - through the daemon's own lexical scorer.
#[test]
fn a_pull_request_like_an_open_todo_is_offered() {
    let source = a_source();
    let seat = a_seat(
        &source,
        Box::new(Some2(vec![
            "the widget is loaded once and the second load wins",
            "the deploy script waits for a port that never opens",
            "the notes pane loses its scroll position when a note is edited",
        ])),
        Config::default(),
    );
    seat.attach("stroppy-io");
    seat.poll(1_000);

    let feed = GithubSeat::feed(&*seat).expect("a feed");
    let offers = feed["scopes"][0]["pulls"][0]["offers"]
        .as_array()
        .unwrap_or_else(|| panic!("the row carries offers: {feed}"));
    assert!(!offers.is_empty(), "{feed}");
    assert_eq!(offers[0]["what"], json!("watch this"), "{offers:#?}");
    assert!(
        offers[0]["label"]
            .as_str()
            .is_some_and(|l| l.contains("widget is loaded once")),
        "the todo the pull request is the same work as: {offers:#?}"
    );
}

/// The tree's own lexical scorer, on real sentences: a pull request and the todo that are the same
/// work score over the floor, and two unrelated ones score zero - the floor is only honest if the
/// arithmetic behind it separates them.
#[test]
fn the_lexical_score_separates_the_same_work_from_nothing() {
    let subject = "the widget is loaded once and the second load wins";
    let texts = [
        "the widget is loaded once and the handle is released",
        "the deploy script waits for a port that never opens",
        "the notes pane loses its scroll position when a note is edited",
    ];
    let scores = Lexical.scores(subject, &texts);
    assert_eq!(scores.len(), texts.len());
    assert!(
        scores[0] >= letibot_github::offers::OFFER_FLOOR,
        "the same work scores {} and the floor is {}",
        scores[0],
        letibot_github::offers::OFFER_FLOOR
    );
    assert!(
        scores[1] < letibot_github::offers::OFFER_FLOOR
            && scores[2] < letibot_github::offers::OFFER_FLOOR,
        "unrelated work is under the floor: {scores:?}"
    );
}

/// A daemon that cannot ask GitHub says so on the scope rather than on every row: one fact, and a
/// scope drawn as `polling` while no pass can run is a promise nobody keeps.
#[test]
fn a_seat_with_no_way_to_poll_says_so_on_the_scope() {
    let seat = Github::new(Config::default()).install();
    seat.attach("stroppy-io/letibot");
    assert!(seat.poll(1_000).is_some());
    let feed = GithubSeat::feed(&*seat).expect("a feed");
    assert_eq!(feed["scopes"][0]["state"], json!("failed"), "{feed}");
    let why = feed["scopes"][0]["why"].as_str().unwrap_or_default();
    assert!(why.contains("no way to ask GitHub"), "{why}");
}

/// The verbs the daemon answers, and the ones it refuses: an attach with no scope, and a word that
/// is not one of the five.
#[test]
fn the_verbs_are_the_five_and_every_other_word_is_refused() {
    let source = a_source();
    let seat = a_seat(&source, Box::new(NoCandidates), Config::default());
    let why = GithubSeat::request(&*seat, &json!({"verb": "attach"}));
    assert!(
        why["error"]
            .as_str()
            .is_some_and(|w| w.contains("takes a scope")),
        "{why}"
    );
    let why = GithubSeat::request(&*seat, &json!({"verb": "review"}));
    assert!(
        why["error"].as_str().is_some_and(|w| w.contains("no verb")),
        "{why}"
    );
    let out = GithubSeat::request(
        &*seat,
        &json!({"verb": "attach", "scope": "stroppy-io/letibot"}),
    );
    assert_eq!(out["ok"], json!(true), "{out}");
    assert_eq!(seat.scopes(), vec!["stroppy-io/letibot".to_string()]);
    // The same scope twice is not an error: the operator asked for what is there.
    let again = GithubSeat::request(
        &*seat,
        &json!({"verb": "attach", "scope": "stroppy-io/letibot"}),
    );
    assert_eq!(again["fresh"], json!(false), "{again}");
    // And a pause holds the scope without losing its rows.
    let paused = GithubSeat::request(
        &*seat,
        &json!({"verb": "pause", "scope": "stroppy-io/letibot"}),
    );
    assert_eq!(paused["scopes"][0]["state"], json!("paused"), "{paused}");
}

/// A path where a mirror would go: the daemon's seat reads it back, so a caller that reported the
/// wrong root would find its trees somewhere it did not look.
#[test]
fn the_seat_remembers_where_its_mirrors_go() {
    let root = Path::new("/tmp/letibot-mirrors");
    let seat = Github::new(Config::default()).production(
        root.to_path_buf(),
        None,
        letibot_sessionlog::registry::Registry::new(),
    );
    assert_eq!(seat.root(), Some(&root.to_path_buf()));
}
