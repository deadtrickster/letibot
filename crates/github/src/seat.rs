//! The seat: what a scope is doing, which pull requests it has met, and what it did about each.
//! One poll pass asks what is open, what changed, what is owed a review and what is owed an answer.
//! Nothing here runs a command of a pull request's own; a fork is handed the diff read-only.

use serde::{Deserialize, Serialize};

use std::path::{Path, PathBuf};

use crate::offers::{self, Candidate, Candidates, Kind, Offer, Scorer};
use crate::source::{Comment, Pull, Review, Scope, Source, Standing};

/// What the seat may send, and to whom. `post = false` is the shipped state: reviews are kept as
/// drafts until an identity named `letibot` exists, and the pane says so on every draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Send reviews and replies to GitHub. Off until the operator's identity exists.
    pub post: bool,
    /// The token file `[github] token_path` names. Without it nothing can be posted, whatever
    /// `post` says; `gh`'s own store is never the identity.
    pub token: Option<String>,
    /// Reviews one repository may get in an hour. 0 is no cap.
    pub reviews_per_hour: u32,
    /// The account the seat posts as, and so the name a comment must mention to be a question.
    pub identity: String,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            post: false,
            token: None,
            reviews_per_hour: 6,
            identity: "letibot".to_string(),
        }
    }
}

impl Config {
    /// Whether anything leaves this box. Both facts are needed: a scope told to post without a
    /// token is the "token missing" state the pane draws, not an attempt that fails per push.
    pub fn posting(&self) -> bool {
        self.post && self.token.is_some()
    }

    /// The seat's config from what a file may set, with the token file's mode checked here so
    /// the child that reads it later is not the first to notice. `~` is `home`, not the process's.
    pub fn resolved(post: bool, token_path: Option<&str>, home: &Path) -> Result<Config, String> {
        let config = Config::default();
        if !post {
            return Ok(config);
        }
        let Some(given) = token_path else {
            return Err(
                "posting is on and `[github] token_path` is not set: a review posted as nobody \
                 would be a stranger's comment on the operator's repository, so the seat posts \
                 nothing until the token file is named"
                    .into(),
            );
        };
        let path = match given.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(given),
        };
        std::fs::metadata(&path).map_err(|e| {
            format!(
                "the token file {} is not there ({e}) — `[github] token_path` names the file \
                 `gh` reads, and `letibot attach github` posts nothing without it",
                path.display()
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .map(|m| m.permissions().mode() & 0o777)
                .unwrap_or(0);
            if mode & 0o077 != 0 {
                return Err(format!(
                    "the token file {} is {mode:04o} and a token must be 0600 — \
                     `chmod 600 {}`",
                    path.display(),
                    path.display()
                ));
            }
        }
        Ok(Config {
            post: true,
            token: Some(path.to_string_lossy().into_owned()),
            ..config
        })
    }
}

/// What one scope's poll is doing, in the pane's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeState {
    Polling,
    /// The per-repository cap is spent, so the rows behind it stay queued.
    RateLimited,
    /// Posting is on and no token is there: nothing can be sent and nothing is attempted.
    TokenMissing,
    /// The operator paused this scope. Nothing is asked of GitHub while it holds.
    Paused,
    /// The last poll was refused, in `why`.
    Failed,
}

impl ScopeState {
    /// The word a row and the snapshot carry.
    pub fn word(self) -> &'static str {
        match self {
            ScopeState::Polling => "polling",
            ScopeState::RateLimited => "rate-limited",
            ScopeState::TokenMissing => "token missing",
            ScopeState::Paused => "paused",
            ScopeState::Failed => "failed",
        }
    }
}

/// What one pull request's seat is doing, in the pane's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    /// Nothing has been reviewed yet: a new pull request, or one whose review was refused.
    Queued,
    /// A review is running in its session.
    Reviewing,
    /// Reviewed, and the review is a draft because nothing is posted.
    Drafted,
    /// Reviewed and sent.
    Posted,
    /// Its head moved past the last review, so the new commits are owed one.
    ReReviewPending,
    /// A mention is being answered.
    Answering,
}

impl ReviewState {
    /// The word a row and the snapshot carry.
    pub fn word(self) -> &'static str {
        match self {
            ReviewState::Queued => "queued",
            ReviewState::Reviewing => "reviewing",
            ReviewState::Drafted => "drafted",
            ReviewState::Posted => "posted",
            ReviewState::ReReviewPending => "re-review pending",
            ReviewState::Answering => "answering a mention",
        }
    }
}

/// A session reviewing one pull request, with the worktree its head is checked out in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub worktree: std::path::PathBuf,
}

/// One review to make: the diff, the session that stands in the pull request's tree, and what that
/// session may do with it.
#[derive(Debug, Clone)]
pub struct Job<'a> {
    pub pull: &'a Pull,
    pub session: &'a Session,
    /// The whole pull request, or only the commits after `since`.
    pub diff: &'a str,
    pub since: Option<&'a str>,
    pub standing: Standing,
    /// What has already been said on it. A review of a change somebody has argued about is a
    /// different review from one of the same diff alone.
    pub comments: &'a [Comment],
}

/// The review machinery behind a trait. The merge queue's reviewer is the real one; a fork's job
/// arrives [`Standing::ReadOnly`] and a reviewer that runs anything in it is the defect this exists
/// to make impossible to write by accident.
pub trait Reviewer {
    /// A session for this pull request, with a worktree at its head.
    fn open(&mut self, pull: &Pull, standing: Standing) -> Result<Session, String>;

    /// Review the diff in that session.
    fn review(&mut self, job: &Job<'_>) -> Result<Review, String>;

    /// Answer a mention in that session.
    fn answer(&mut self, session: &Session, question: &str) -> Result<String, String>;
}

/// What the seat would send, kept because nothing is sent yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Draft {
    /// A review of `for_sha`, which is the head it was made on.
    Review { for_sha: String, review: Review },
    /// An answer to one comment.
    Reply { comment: u64, body: String },
}

/// One pull request's row: everything the seat knows about it, and the one action it last took.
#[derive(Debug, Clone)]
pub struct PullRow {
    pub pull: Pull,
    pub session: Option<Session>,
    pub standing: Standing,
    pub state: ReviewState,
    /// The head the last review was of: what a push is measured against.
    pub reviewed_sha: Option<String>,
    pub last_action: String,
    pub last_ms: u64,
    pub drafts: Vec<Draft>,
    /// The newest comment id read, so a mention is answered once.
    pub seen_comment: u64,
    /// When the row appeared: a mention older than the seat is not a question for it.
    pub since_ms: u64,
    /// Why the last review could not be made, in words - empty when it could.
    pub failure: String,
    /// What this pull request's words are like, best first: an open todo row to watch, a running
    /// session's brief, or a review this seat made before.
    pub offers: Vec<Offer>,
}

impl PullRow {
    /// The drafts this row holds, which is what a pane counts as "not sent".
    pub fn drafts(&self) -> &[Draft] {
        &self.drafts
    }
}

struct ScopeRun {
    scope: Scope,
    state: ScopeState,
    why: String,
}

/// The seat: every scope it was started with, and every pull request it has met.
pub struct Seat {
    scopes: Vec<ScopeRun>,
    rows: Vec<PullRow>,
    config: Config,
    /// When each review was started, for the per-repository cap: `repo`, the clock.
    started: Vec<(String, u64)>,
    /// What a pull request's words may be like - the open todos and the running sessions' briefs,
    /// from whoever holds them ([`Seat::with_offers`]). `None` is a seat that makes no offers.
    candidates: Option<Box<dyn Candidates>>,
    /// How alike two texts are. `None` disables the offers with `candidates`.
    scorer: Option<Box<dyn Scorer>>,
    at_ms: u64,
}

/// One hour, the window the cap is counted over.
const HOUR_MS: u64 = 60 * 60 * 1_000;

impl Seat {
    pub fn new(scopes: Vec<Scope>, config: Config) -> Seat {
        Seat {
            scopes: scopes
                .into_iter()
                .map(|scope| ScopeRun {
                    scope,
                    state: ScopeState::Polling,
                    why: String::new(),
                })
                .collect(),
            rows: Vec::new(),
            config,
            started: Vec::new(),
            candidates: None,
            scorer: None,
            at_ms: 0,
        }
    }

    /// Make R114's offers: a pull request whose words are like an open todo row or a running
    /// session's brief is offered as one to watch ([`offers`]), and one like a review this seat
    /// made before is offered as context. The candidates are recomputed per poll, so a todo closed
    /// and a session ended stop being offered.
    pub fn with_offers(mut self, candidates: Box<dyn Candidates>, scorer: Box<dyn Scorer>) -> Seat {
        self.candidates = Some(candidates);
        self.scorer = Some(scorer);
        self
    }

    /// What the seat is configured to do, for a caller that must not re-derive it.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Every row, in the order the pane draws them: by repository then number.
    pub fn rows(&self) -> &[PullRow] {
        &self.rows
    }

    /// The scopes this seat holds, in the order they were attached.
    pub fn held(&self) -> Vec<&Scope> {
        self.scopes.iter().map(|run| &run.scope).collect()
    }

    /// Hold one more scope - `letibot attach github <scope>` on a seat that is already watching
    /// something. `false` when this scope is already held, which is not an error: the operator
    /// asked for what is there.
    pub fn attach(&mut self, scope: Scope) -> bool {
        if self.scopes.iter().any(|r| r.scope == scope) {
            return false;
        }
        self.scopes.push(ScopeRun {
            scope,
            state: ScopeState::Polling,
            why: String::new(),
        });
        true
    }

    /// Every draft, oldest first: what the operator would have to paste while nothing is posted.
    pub fn drafts(&self) -> Vec<&Draft> {
        self.rows.iter().flat_map(|r| r.drafts.iter()).collect()
    }

    /// Stop asking GitHub for this scope. The rows stay as they are: a pause is not a reset.
    pub fn pause(&mut self, scope: &str) {
        for run in self.scopes.iter_mut().filter(|r| r.scope.as_str() == scope) {
            run.state = ScopeState::Paused;
            run.why.clear();
        }
    }

    /// Start asking again.
    pub fn resume(&mut self, scope: &str) {
        for run in self.scopes.iter_mut().filter(|r| r.scope.as_str() == scope) {
            run.state = ScopeState::Polling;
            run.why.clear();
        }
    }

    /// Say on a scope why nothing is being asked of GitHub. A scope drawn as `polling` while no pass
    /// can run is a promise nobody keeps, so the reason is the scope's own words.
    pub fn fail_scope(&mut self, scope: &str, why: String) {
        for run in self.scopes.iter_mut().filter(|r| r.scope.as_str() == scope) {
            if run.state == ScopeState::Paused {
                continue;
            }
            run.state = ScopeState::Failed;
            run.why = why.clone();
        }
    }

    /// One pass over every scope. `said` is what it did, in the order it did it, and every
    /// sentence is also written on the row it is about. The source and the reviewer are `?Sized` so
    /// the daemon can hold one of each behind a trait object: a pass is built per tick, not per poll.
    pub fn poll<S: Source + ?Sized, R: Reviewer + ?Sized>(
        &mut self,
        source: &S,
        reviewer: &mut R,
        now_ms: u64,
    ) -> Vec<String> {
        let mut said = Vec::new();
        let mut listed: Vec<Pull> = Vec::new();
        for run in self.scopes.iter_mut() {
            if run.state == ScopeState::Paused {
                continue;
            }
            if self.config.post && self.config.token.is_none() {
                run.state = ScopeState::TokenMissing;
                run.why = "posting is on and no token file is there".into();
                continue;
            }
            match source.list_open(&run.scope) {
                Ok(pulls) => {
                    run.state = ScopeState::Polling;
                    run.why.clear();
                    listed.extend(pulls);
                }
                Err(why) => {
                    run.state = ScopeState::Failed;
                    run.why = why;
                }
            }
        }
        self.reconcile(&listed, now_ms, &mut said);
        self.at_ms = now_ms;

        let keys: Vec<(String, u64)> = self
            .rows
            .iter()
            .map(|r| (r.pull.repo.clone(), r.pull.number))
            .collect();
        for (repo, number) in keys {
            self.act(source, reviewer, &repo, number, now_ms, &mut said);
        }
        said
    }

    /// The listed pull requests as rows: a new one queued, a moved head owed a re-review, and a row
    /// whose pull request is no longer open dropped.
    fn reconcile(&mut self, listed: &[Pull], now_ms: u64, said: &mut Vec<String>) {
        // Worked out before the rows are borrowed: the offers read this seat's own drafts.
        let offered: Vec<Vec<Offer>> = listed.iter().map(|pull| self.offers_for(pull)).collect();
        self.rows.retain(|row| {
            listed
                .iter()
                .any(|p| p.repo == row.pull.repo && p.number == row.pull.number)
        });
        for (i, pull) in listed.iter().enumerate() {
            match self
                .rows
                .iter_mut()
                .find(|r| r.pull.repo == pull.repo && r.pull.number == pull.number)
            {
                Some(row) => {
                    let moved = row.reviewed_sha.as_deref() != Some(pull.head_sha.as_str());
                    row.pull = pull.clone();
                    row.offers = offered[i].clone();
                    if moved && !matches!(row.state, ReviewState::Queued | ReviewState::Reviewing) {
                        row.state = ReviewState::ReReviewPending;
                        row.last_action =
                            format!("a push moved the head to {}", short(&pull.head_sha));
                        row.last_ms = now_ms;
                        said.push(format!("{} has a new head", pull.key()));
                    }
                }
                None => {
                    self.rows.push(PullRow {
                        pull: pull.clone(),
                        session: None,
                        standing: Standing::of(pull),
                        state: ReviewState::Queued,
                        reviewed_sha: None,
                        last_action: "a new pull request".to_string(),
                        last_ms: now_ms,
                        drafts: Vec::new(),
                        seen_comment: 0,
                        since_ms: now_ms,
                        failure: String::new(),
                        offers: offered[i].clone(),
                    });
                    said.push(format!("{} is new", pull.key()));
                }
            }
        }
        self.rows
            .sort_by(|a, b| (&a.pull.repo, a.pull.number).cmp(&(&b.pull.repo, b.pull.number)));
    }

    /// What a pull request's words are like, best first - [`offers::offers_of`] over the candidates
    /// a holder supplied plus the reviews this seat has already drafted. A row's own draft is not
    /// offered back to it, which would be the seat agreeing with itself.
    fn offers_for(&self, pull: &Pull) -> Vec<Offer> {
        let (Some(candidates), Some(scorer)) = (&self.candidates, &self.scorer) else {
            return Vec::new();
        };
        let mut all: Vec<Candidate> = candidates.candidates();
        for row in &self.rows {
            if row.pull.repo == pull.repo && row.pull.number == pull.number {
                continue;
            }
            for draft in &row.drafts {
                if let Draft::Review { review, .. } = draft {
                    all.push(Candidate {
                        kind: Kind::Context,
                        label: format!("review of {}", row.pull.key()),
                        text: review.body.clone(),
                    });
                }
            }
        }
        let subject = format!("{}\n{}", pull.title, pull.description);
        offers::offers_of(&subject, &all, &**scorer)
    }

    /// What one row is owed: a review, then an answer.
    fn act<S: Source + ?Sized, R: Reviewer + ?Sized>(
        &mut self,
        source: &S,
        reviewer: &mut R,
        repo: &str,
        number: u64,
        now_ms: u64,
        said: &mut Vec<String>,
    ) {
        let Some(index) = self
            .rows
            .iter()
            .position(|r| r.pull.repo == repo && r.pull.number == number)
        else {
            return;
        };
        let owed_review = matches!(
            self.rows[index].state,
            ReviewState::Queued | ReviewState::ReReviewPending
        );
        if owed_review {
            if self.capped(repo, now_ms) {
                for run in self.scopes.iter_mut() {
                    if run.scope.known_repo() == Some(repo)
                        || matches!(&run.scope, Scope::Org(org) if repo.starts_with(&format!("{org}/")))
                    {
                        run.state = ScopeState::RateLimited;
                        run.why = format!(
                            "{} reviews of {repo} in the last hour, which is the cap",
                            self.config.reviews_per_hour
                        );
                    }
                }
                said.push(format!("{repo}#{number} waits: the hourly cap is spent"));
                return;
            }
            if !self.review(source, reviewer, index, now_ms, said) {
                return;
            }
        }
        self.answer_mentions(source, reviewer, index, now_ms, said);
        let _ = &self.at_ms;
    }

    /// Whether this repository's cap is spent.
    fn capped(&mut self, repo: &str, now_ms: u64) -> bool {
        if self.config.reviews_per_hour == 0 {
            return false;
        }
        self.started
            .retain(|(_, at)| now_ms.saturating_sub(*at) < HOUR_MS);
        let count = self.started.iter().filter(|(r, _)| r == repo).count();
        count >= self.config.reviews_per_hour as usize
    }

    /// Open, review, then keep or send. `false` when the review was not made, so the answer pass
    /// waits for a poll that has a review behind it.
    fn review<S: Source + ?Sized, R: Reviewer + ?Sized>(
        &mut self,
        source: &S,
        reviewer: &mut R,
        index: usize,
        now_ms: u64,
        said: &mut Vec<String>,
    ) -> bool {
        let pull = self.rows[index].pull.clone();
        let standing = self.rows[index].standing;
        let since = self.rows[index].reviewed_sha.clone();
        let session = match self.rows[index].session.clone() {
            Some(session) => session,
            None => match reviewer.open(&pull, standing) {
                Ok(session) => {
                    self.rows[index].session = Some(session.clone());
                    said.push(format!("opened {} for {}", session.id, pull.key()));
                    session
                }
                Err(why) => return self.refused(index, why, said),
            },
        };
        let diff = match source.diff(&pull, since.as_deref()) {
            Ok(diff) => diff,
            Err(why) => return self.refused(index, why, said),
        };
        // The conversation is what the review is handed, so a poll that cannot read it cannot
        // review either: the row is owed the review rather than reviewed without it.
        let comments = match source.comments(&pull) {
            Ok(comments) => comments,
            Err(why) => return self.refused(index, why, said),
        };
        self.rows[index].state = ReviewState::Reviewing;
        let job = Job {
            pull: &pull,
            session: &session,
            diff: &diff,
            since: since.as_deref(),
            standing,
            comments: &comments,
        };
        let review = match reviewer.review(&job) {
            Ok(review) => review,
            Err(why) => return self.refused(index, why, said),
        };
        self.started.push((pull.repo.clone(), now_ms));
        let re = since.is_some();
        let row = &mut self.rows[index];
        row.reviewed_sha = Some(pull.head_sha.clone());
        row.failure.clear();
        row.last_ms = now_ms;
        if self.config.posting() {
            match source.post_review(&pull, &review) {
                Ok(id) => {
                    row.state = ReviewState::Posted;
                    row.last_action = format!("posted review {id} on {}", pull.key());
                }
                Err(why) => return self.refused(index, why, said),
            }
        } else {
            row.drafts.push(Draft::Review {
                for_sha: pull.head_sha.clone(),
                review,
            });
            row.state = ReviewState::Drafted;
            row.last_action = if re {
                format!("drafted a re-review of {}'s new commits", pull.key())
            } else {
                format!("drafted a review of {}", pull.key())
            };
        }
        said.push(self.rows[index].last_action.clone());
        true
    }

    /// A review that could not be made stays owed: the row goes back to `queued` with the reason on
    /// it, so the next poll tries again rather than losing the push.
    fn refused(&mut self, index: usize, why: String, said: &mut Vec<String>) -> bool {
        let row = &mut self.rows[index];
        row.state = ReviewState::Queued;
        row.failure = why.clone();
        row.last_action = format!("could not review: {why}");
        said.push(format!("{} was not reviewed: {why}", row.pull.key()));
        false
    }

    /// Every mention since the row appeared and since the last one answered, answered once.
    fn answer_mentions<S: Source + ?Sized, R: Reviewer + ?Sized>(
        &mut self,
        source: &S,
        reviewer: &mut R,
        index: usize,
        now_ms: u64,
        said: &mut Vec<String>,
    ) {
        let pull = self.rows[index].pull.clone();
        let comments = match source.comments(&pull) {
            Ok(comments) => comments,
            Err(why) => {
                self.rows[index].failure = why;
                return;
            }
        };
        let seen = self.rows[index].seen_comment;
        let since_ms = self.rows[index].since_ms;
        let identity = self.config.identity.clone();
        let due: Option<Comment> = comments
            .iter()
            .filter(|c| c.id > seen && c.created_ms >= since_ms && c.author != identity)
            .filter(|c| c.mentions(&identity))
            .min_by_key(|c| c.id)
            .cloned();
        let newest = comments.iter().map(|c| c.id).max().unwrap_or(seen);
        self.rows[index].seen_comment = self.rows[index].seen_comment.max(newest);
        let Some(comment) = due else {
            return;
        };
        let session = match self.rows[index].session.clone() {
            Some(session) => session,
            None => match reviewer.open(&pull, self.rows[index].standing) {
                Ok(session) => {
                    self.rows[index].session = Some(session.clone());
                    session
                }
                Err(why) => {
                    self.rows[index].failure = why;
                    return;
                }
            },
        };
        self.rows[index].state = ReviewState::Answering;
        let body = match reviewer.answer(&session, &comment.body) {
            Ok(body) => body,
            Err(why) => {
                self.rows[index].failure = why;
                self.rows[index].state = before_answer(ReviewState::Answering);
                return;
            }
        };
        // What was sent, worked out before the row is borrowed: a refusal writes the row too.
        let sent = if self.config.posting() {
            source.reply(&pull, &comment, &body)
        } else {
            Ok(0)
        };
        let posting = self.config.posting();
        let row = &mut self.rows[index];
        match sent {
            Ok(0) if !posting => {
                row.drafts.push(Draft::Reply {
                    comment: comment.id,
                    body,
                });
                row.last_action = format!(
                    "drafted an answer to comment {} on {}",
                    comment.id,
                    pull.key()
                );
            }
            Ok(id) => row.last_action = format!("answered comment {id} on {}", pull.key()),
            Err(why) => {
                row.failure = why.clone();
                row.last_action = format!("could not answer: {why}");
                row.state = before_answer(ReviewState::Answering);
                return;
            }
        }
        row.failure.clear();
        row.last_ms = now_ms;
        row.state = match (posting, row.reviewed_sha.is_some()) {
            (true, _) => ReviewState::Posted,
            (false, true) => ReviewState::Drafted,
            // An answer with no review behind it leaves the review still owed.
            (false, false) => ReviewState::Queued,
        };
        said.push(self.rows[index].last_action.clone());
    }

    /// The seat as the pane draws it: one line per scope, one row per pull request, and ages counted
    /// from the poll that is being asked about.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            at_ms: self.at_ms,
            posting: self.config.posting(),
            identity: self.config.identity.clone(),
            scopes: self
                .scopes
                .iter()
                .map(|run| ScopeView {
                    scope: run.scope.as_str().to_string(),
                    state: run.state.word().to_string(),
                    state_words: state_words(run),
                    why: run.why.clone(),
                    pulls: self
                        .rows
                        .iter()
                        .filter(|row| in_scope(&run.scope, &row.pull.repo))
                        .map(|row| PullView {
                            repo: row.pull.repo.clone(),
                            number: row.pull.number,
                            title: row.pull.title.clone(),
                            head_sha: row.pull.head_sha.clone(),
                            review: row.state.word().to_string(),
                            fork: row.pull.from_fork,
                            last_action: row.last_action.clone(),
                            age_ms: self.at_ms.saturating_sub(row.last_ms),
                            session: row
                                .session
                                .as_ref()
                                .map(|s| s.id.clone())
                                .unwrap_or_default(),
                            drafts: row.drafts.iter().map(DraftView::of).collect(),
                            offers: row.offers.iter().map(OfferView::of).collect(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

/// The review state an answer that failed goes back to: the row was owed an answer, not a review.
fn before_answer(state: ReviewState) -> ReviewState {
    match state {
        ReviewState::Answering => ReviewState::Drafted,
        other => other,
    }
}

/// Whether a repository belongs to a scope.
fn in_scope(scope: &Scope, repo: &str) -> bool {
    match scope {
        Scope::Org(org) => repo.starts_with(&format!("{org}/")),
        Scope::Repo(full) => full == repo,
    }
}

/// The scope's state in a sentence, for the pane's line.
fn state_words(run: &ScopeRun) -> String {
    match run.state {
        ScopeState::Polling => "polling GitHub for open pull requests".to_string(),
        ScopeState::RateLimited => "the hourly review cap is spent, so rows wait".to_string(),
        ScopeState::TokenMissing => "posting is on and no token file is there".to_string(),
        ScopeState::Paused => "paused by the operator; GitHub is not asked".to_string(),
        ScopeState::Failed => format!("the last poll was refused: {}", run.why),
    }
}

/// A sha as a person reads it.
fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

// ---- the wire the pane reads ----------------------------------------------------

/// The seat, as a pane or a one-shot print draws it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The clock the poll was made at, so an age is the daemon's and not the head's.
    #[serde(default)]
    pub at_ms: u64,
    /// Whether reviews are being sent: false means every review is a draft.
    #[serde(default)]
    pub posting: bool,
    #[serde(default)]
    pub identity: String,
    #[serde(default)]
    pub scopes: Vec<ScopeView>,
}

/// One scope's line: what it is doing, and the rows under it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScopeView {
    pub scope: String,
    /// `polling`, `rate-limited`, `token missing`, `paused`, `failed`.
    pub state: String,
    pub state_words: String,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub pulls: Vec<PullView>,
}

/// One pull request's row.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PullView {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub head_sha: String,
    /// The review's state, in the pane's words.
    pub review: String,
    /// A fork's diff is reviewed and nothing of it is run.
    #[serde(default)]
    pub fork: bool,
    pub last_action: String,
    pub age_ms: u64,
    #[serde(default)]
    pub session: String,
    /// What would be sent, in full. The operator's decision is about these words, so a pane
    /// that only counted them would be asking for a decision it does not show.
    #[serde(default)]
    pub drafts: Vec<DraftView>,
    /// What this pull request's words are like, best first - a todo row to watch, a running
    /// session, a review made before. Empty when nothing was close enough to be worth a line.
    #[serde(default)]
    pub offers: Vec<OfferView>,
}

/// One offer as the pane draws it: what it is, what it points at, and the score that earned it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OfferView {
    /// `watch this` or `similar to an earlier review`.
    pub what: String,
    pub label: String,
    /// The score, in `0..=1`: the one number behind the line, for a detail and never for a title.
    pub score: f64,
}

impl OfferView {
    /// The wire's spelling of one offer.
    fn of(offer: &Offer) -> OfferView {
        OfferView {
            what: offer.kind.word().to_string(),
            label: offer.label.clone(),
            score: offer.score,
        }
    }
}

/// One draft as the pane draws it: what it is, and the words that would go out.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DraftView {
    /// `review` or `reply`.
    pub kind: String,
    /// The head a review was made on; empty for a reply.
    #[serde(default)]
    pub for_sha: String,
    /// The comment a reply answers; 0 for a review.
    #[serde(default)]
    pub comment: u64,
    pub body: String,
}

impl DraftView {
    /// The wire's spelling of one draft.
    fn of(draft: &Draft) -> DraftView {
        match draft {
            Draft::Review { for_sha, review } => DraftView {
                kind: "review".into(),
                for_sha: for_sha.clone(),
                comment: 0,
                body: review.body.clone(),
            },
            Draft::Reply { comment, body } => DraftView {
                kind: "reply".into(),
                for_sha: String::new(),
                comment: *comment,
                body: body.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests;
