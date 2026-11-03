//! The source in memory: what the seat's tests read and write, with no network and no `gh`.
//!
//! A pull request is its commits here, because that is what a push is: a re-review must not see the
//! commits the last review already read, and the only way to test that is for the fake to know them.

use std::sync::Mutex;

use crate::source::{Comment, Pull, Review, Scope, Source};

/// One commit of a pull request and the diff it carries.
#[derive(Debug, Clone)]
pub struct Commit {
    pub sha: String,
    pub diff: String,
}

impl Commit {
    pub fn new(sha: impl Into<String>, diff: impl Into<String>) -> Commit {
        Commit {
            sha: sha.into(),
            diff: diff.into(),
        }
    }
}

#[derive(Default)]
struct Inner {
    pulls: Vec<(Pull, Vec<Commit>)>,
    /// `repo`, number, the comment.
    comments: Vec<(String, u64, Comment)>,
    reviews: Vec<(String, u64, Review)>,
    /// `repo`, number, the comment answered, the body.
    replies: Vec<(String, u64, u64, String)>,
    /// `repo`, number, and the `since` each diff was asked with.
    diffs: Vec<(String, u64, Option<String>)>,
    minted: u64,
}

/// The source in memory. Every door takes `&self`: a test opens a pull request and pushes to it
/// while an earlier borrow of the fake is still the seat's.
#[derive(Default)]
pub struct FakeSource {
    inner: Mutex<Inner>,
}

impl FakeSource {
    pub fn new() -> FakeSource {
        FakeSource::default()
    }

    /// A pull request appears, with every commit it is on now.
    pub fn open(&self, pull: &Pull, commits: Vec<Commit>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.pulls.push((pull.clone(), commits));
    }

    /// A push: one more commit, and the head moves to it.
    pub fn push(&self, repo: &str, number: u64, commit: Commit) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some((pull, commits)) = inner
            .pulls
            .iter_mut()
            .find(|(p, _)| p.repo == repo && p.number == number)
        else {
            panic!("no pull request {repo}#{number}")
        };
        pull.head_sha = commit.sha.clone();
        pull.updated_ms += 1_000;
        commits.push(commit);
    }

    /// A comment on the conversation, or on a review thread.
    pub fn comment(&self, repo: &str, number: u64, comment: Comment) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.comments.push((repo.to_string(), number, comment));
    }

    /// The pull request is closed, so it is no longer listed - which is how the seat learns it is
    /// gone: it draws what is open, and a row nobody lists any more is dropped.
    pub fn close(&self, repo: &str, number: u64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .pulls
            .retain(|(p, _)| !(p.repo == repo && p.number == number));
    }

    /// Every review posted, oldest first.
    pub fn reviews(&self) -> Vec<(String, u64, Review)> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reviews
            .clone()
    }

    /// Every reply posted, oldest first: `repo`, number, the comment answered, the body.
    pub fn replies(&self) -> Vec<(String, u64, u64, String)> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replies
            .clone()
    }

    /// **What each diff was asked with**, which is how a test says a re-review is of the new
    /// commits only.
    pub fn diffs_asked(&self) -> Vec<(String, u64, Option<String>)> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .diffs
            .clone()
    }
}

impl Source for FakeSource {
    fn list_open(&self, scope: &Scope) -> Result<Vec<Pull>, String> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        Ok(inner
            .pulls
            .iter()
            .filter(|(p, _)| match scope {
                Scope::Org(org) => p.repo.starts_with(&format!("{org}/")),
                Scope::Repo(full) => &p.repo == full,
            })
            .map(|(p, _)| p.clone())
            .collect())
    }

    fn diff(&self, pull: &Pull, since: Option<&str>) -> Result<String, String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .diffs
            .push((pull.repo.clone(), pull.number, since.map(str::to_string)));
        let Some((_, commits)) = inner
            .pulls
            .iter()
            .find(|(p, _)| p.repo == pull.repo && p.number == pull.number)
        else {
            return Err(format!("no pull request {}", pull.key()));
        };
        let from = match since {
            None => 0,
            Some(sha) => {
                commits
                    .iter()
                    .position(|c| c.sha == sha)
                    .ok_or_else(|| format!("{} has no commit {sha}", pull.key()))?
                    + 1
            }
        };
        Ok(commits[from..]
            .iter()
            .map(|c| c.diff.clone())
            .collect::<Vec<String>>()
            .join("\n"))
    }

    fn comments(&self, pull: &Pull) -> Result<Vec<Comment>, String> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        Ok(inner
            .comments
            .iter()
            .filter(|(repo, number, _)| repo == &pull.repo && *number == pull.number)
            .map(|(_, _, c)| c.clone())
            .collect())
    }

    fn post_review(&self, pull: &Pull, review: &Review) -> Result<u64, String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.minted += 1;
        let id = 9_000 + inner.minted;
        inner
            .reviews
            .push((pull.repo.clone(), pull.number, review.clone()));
        Ok(id)
    }

    fn reply(&self, pull: &Pull, comment: &Comment, body: &str) -> Result<u64, String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.minted += 1;
        let id = 5_000 + inner.minted;
        inner
            .replies
            .push((pull.repo.clone(), pull.number, comment.id, body.to_string()));
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_pull() -> Pull {
        Pull {
            repo: "stroppy-io/letibot".into(),
            number: 12,
            head_sha: "c1".into(),
            base_sha: "b0".into(),
            branch: "topic".into(),
            from_fork: false,
            title: "a fix".into(),
            author: "someone".into(),
            updated_ms: 1_000,
        }
    }

    /// **A re-review is of the new commits only**, because the fake knows the commits - and that is
    /// the only way the seat's claim about a push can be tested at all.
    #[test]
    fn a_diff_since_a_sha_is_only_the_commits_after_it() {
        let fake = FakeSource::new();
        fake.open(
            &a_pull(),
            vec![
                Commit::new("c1", "-old\n+new"),
                Commit::new("c2", "-second\n+second new"),
            ],
        );
        let pull = a_pull();
        assert_eq!(
            fake.diff(&pull, None).unwrap(),
            "-old\n+new\n-second\n+second new"
        );
        assert_eq!(
            fake.diff(&pull, Some("c1")).unwrap(),
            "-second\n+second new"
        );
        assert_eq!(
            fake.diff(&pull, Some("c3")).unwrap_err(),
            "stroppy-io/letibot#12 has no commit c3"
        );
        assert_eq!(fake.diffs_asked().len(), 3);
    }

    #[test]
    fn a_scope_lists_its_own_pull_requests_and_no_others() {
        let fake = FakeSource::new();
        let mut other = a_pull();
        other.repo = "someone-else/thing".into();
        other.number = 3;
        fake.open(&a_pull(), vec![Commit::new("c1", "")]);
        fake.open(&other, vec![Commit::new("z1", "")]);
        let mine = fake.list_open(&Scope::Org("stroppy-io".into())).unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].key(), "stroppy-io/letibot#12");
        assert_eq!(
            fake.list_open(&Scope::Repo("someone-else/thing".into()))
                .unwrap()[0]
                .key(),
            "someone-else/thing#3"
        );
        assert!(
            fake.list_open(&Scope::Repo("stroppy-io/other".into()))
                .unwrap()
                .is_empty()
        );
    }

    /// A review and a reply are recorded with the pull request they are about, and the id a row
    /// records is the source's.
    #[test]
    fn a_review_and_a_reply_are_recorded_against_the_pull_request() {
        let fake = FakeSource::new();
        fake.open(&a_pull(), vec![Commit::new("c1", "")]);
        let review = Review::summary("looks fine");
        let id = fake.post_review(&a_pull(), &review).unwrap();
        assert!(id > 9_000);
        assert_eq!(
            fake.reviews(),
            vec![("stroppy-io/letibot".into(), 12, review)]
        );
        let asker = Comment {
            id: 77,
            author: "someone".into(),
            body: "@letibot why?".into(),
            created_ms: 2_000,
            in_review: true,
        };
        fake.comment("stroppy-io/letibot", 12, asker.clone());
        assert_eq!(fake.comments(&a_pull()).unwrap(), vec![asker.clone()]);
        fake.reply(&a_pull(), &asker, "because").unwrap();
        assert_eq!(
            fake.replies(),
            vec![(
                "stroppy-io/letibot".to_string(),
                12,
                77,
                "because".to_string()
            )]
        );
    }
}
