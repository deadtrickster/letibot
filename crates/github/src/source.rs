//! One pull request's facts, and the one trait the seat reads and writes them through.
//!
//! Two implementations: [`crate::gh`] shells out to `gh api`, and [`crate::fake`] is memory. The
//! seat is written against this trait only, so every test of it is offline.

/// A scope: one org's repositories, or one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every open pull request in every repository the org has.
    Org(String),
    /// One repository, as `owner/name`.
    Repo(String),
}

impl Scope {
    /// **What the operator typed**, `letibot attach github <scope>`: `stroppy-io` is an org and
    /// `stroppy-io/letibot` is one repository. Anything else is refused by name, because a scope
    /// guessed wrong is a seat that reviews somebody else's repositories.
    pub fn parse(text: &str) -> Result<Scope, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err(
                "a scope is an org (`stroppy-io`) or a repository (`stroppy-io/letibot`)".into(),
            );
        }
        let parts: Vec<&str> = text.split('/').collect();
        let bad = |part: &str| part.is_empty() || part.chars().any(|c| c.is_whitespace());
        match parts.as_slice() {
            [org] if bad(org) => Err(format!("`{text}` is not an org name")),
            [org] => Ok(Scope::Org((*org).to_string())),
            [owner, repo] if !bad(owner) && !bad(repo) => {
                Ok(Scope::Repo(format!("{owner}/{repo}")))
            }
            _ => Err(format!(
                "`{text}` is not a scope: name one org (`stroppy-io`) or one repository \
                 (`stroppy-io/letibot`), not {n} parts",
                n = parts.len()
            )),
        }
    }

    /// The scope as the operator typed it, for the pane's line and for a message.
    pub fn as_str(&self) -> &str {
        match self {
            Scope::Org(name) => name,
            Scope::Repo(full) => full,
        }
    }

    /// **The repositories this poll has to ask about.** An org's list comes from GitHub, so this
    /// is `None` for one - the source answers it - and `Some` for the repository the operator named.
    pub fn known_repo(&self) -> Option<&str> {
        match self {
            Scope::Org(_) => None,
            Scope::Repo(full) => Some(full),
        }
    }
}

/// One open pull request, as the seat knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pull {
    /// `owner/name`.
    pub repo: String,
    pub number: u64,
    pub head_sha: String,
    pub base_sha: String,
    /// The head branch's name, for the worktree.
    pub branch: String,
    /// **A pull request from a fork is not this repository's code**: its diff is reviewed and
    /// nothing in it is ever run ([`Standing::ReadOnly`]).
    pub from_fork: bool,
    pub title: String,
    pub author: String,
    /// When GitHub last saw the pull request change, in milliseconds.
    pub updated_ms: u64,
}

impl Pull {
    /// `owner/name#12`, the one spelling a message and a row use.
    pub fn key(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }
}

/// One comment on a pull request: the conversation's, or one review thread's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// GitHub's id, which is what a reply is addressed to.
    pub id: u64,
    pub author: String,
    pub body: String,
    pub created_ms: u64,
    /// A review thread's comment is answered through `pulls/…/comments/…/replies`; the
    /// conversation's is a new issue comment. Two endpoints, so the fact travels with the comment.
    pub in_review: bool,
}

impl Comment {
    /// Whether this comment asks the seat a question - what the seat answers, and nothing else.
    /// The name has to end the run: `@letibot2` is somebody else's mention.
    pub fn mentions(&self, name: &str) -> bool {
        let at = format!("@{name}");
        let mut from = 0;
        while let Some(hit) = self.body[from..].find(&at) {
            let next = self.body[from + hit + at.len()..].chars().next();
            let ends = next.is_none_or(|c| !(c.is_alphanumeric() || c == '-' || c == '_'));
            if ends {
                return true;
            }
            from += hit + at.len();
        }
        false
    }
}

/// The review's verdict, in GitHub's three words. A review that approves or blocks is a statement
/// about the code; `Comment` is what an unattended reviewer says, so it is never a gate on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Comment,
    RequestChanges,
    Approve,
}

impl Event {
    /// The word GitHub's API takes.
    pub fn as_str(self) -> &'static str {
        match self {
            Event::Comment => "COMMENT",
            Event::RequestChanges => "REQUEST_CHANGES",
            Event::Approve => "APPROVE",
        }
    }
}

/// One inline comment: a path the diff has, the line in the new file, and what is wrong with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inline {
    pub path: String,
    pub line: u64,
    pub body: String,
}

/// One review: a summary and its inline comments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub body: String,
    pub event: Event,
    pub comments: Vec<Inline>,
}

impl Review {
    /// A review with prose and no inline comment - what a reviewer that found nothing in the diff
    /// itself hands in.
    pub fn summary(body: impl Into<String>) -> Review {
        Review {
            body: body.into(),
            event: Event::Comment,
            comments: Vec::new(),
        }
    }
}

/// What the seat reads and writes: one hub's API, as this seat needs it.
///
/// Every method answers a refusal in words rather than a typed error, because every one of them
/// ends up in a row's `last_action` and on a person's screen.
pub trait Source {
    /// Every open pull request in the scope, with the head it is on now.
    fn list_open(&self, scope: &Scope) -> Result<Vec<Pull>, String>;

    /// **The diff to review.** `since` is the head the last review was of, so a push is reviewed
    /// as the commits it added and not as the whole pull request again; `None` is the whole thing.
    fn diff(&self, pull: &Pull, since: Option<&str>) -> Result<String, String>;

    /// Every comment on the pull request, conversation and review threads alike.
    fn comments(&self, pull: &Pull) -> Result<Vec<Comment>, String>;

    /// Post one review. The id it answers is GitHub's, and is what a row records as `posted`.
    fn post_review(&self, pull: &Pull, review: &Review) -> Result<u64, String>;

    /// Answer one comment in its own thread.
    fn reply(&self, pull: &Pull, comment: &Comment, body: &str) -> Result<u64, String>;
}

/// **Whether a pull request's code may be run.** A fork's may not: the seat reviews the diff and
/// starts nothing, because the diff is a stranger's and this box's boundary is not the stranger's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// This repository's own branch, in a session that may run it.
    Trusted,
    /// A fork's: read-only, and no command of it is run.
    ReadOnly,
}

impl Standing {
    /// What a pull request gets, from where it came.
    pub fn of(pull: &Pull) -> Standing {
        if pull.from_fork {
            Standing::ReadOnly
        } else {
            Standing::Trusted
        }
    }

    /// Whether a session at this standing may run the pull request's own commands.
    pub fn may_run(self) -> bool {
        matches!(self, Standing::Trusted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scope_is_an_org_or_one_repository() {
        assert_eq!(
            Scope::parse("stroppy-io"),
            Ok(Scope::Org("stroppy-io".into()))
        );
        assert_eq!(
            Scope::parse(" stroppy-io/letibot "),
            Ok(Scope::Repo("stroppy-io/letibot".into()))
        );
        assert_eq!(Scope::parse("stroppy-io").unwrap().as_str(), "stroppy-io");
        assert_eq!(
            Scope::parse("stroppy-io/letibot").unwrap().as_str(),
            "stroppy-io/letibot"
        );
        assert_eq!(
            Scope::parse("stroppy-io/letibot").unwrap().known_repo(),
            Some("stroppy-io/letibot")
        );
        assert_eq!(Scope::parse("stroppy-io").unwrap().known_repo(), None);
    }

    /// A scope is what the operator typed or it is refused - a seat that guessed would review
    /// somebody else's repositories.
    #[test]
    fn a_scope_that_is_not_one_is_refused_by_name() {
        for bad in [
            "",
            "  ",
            "a/b/c",
            "https://github.com/a/b",
            "a/",
            "/b",
            "a b",
        ] {
            let why = Scope::parse(bad).expect_err(bad);
            assert!(
                why.contains(bad.trim()) || bad.trim().is_empty(),
                "{bad}: {why}"
            );
        }
        assert!(Scope::parse("a/b/c").unwrap_err().contains("not a scope"));
    }

    #[test]
    fn a_fork_is_reviewed_read_only_and_runs_nothing() {
        let mut pull = Pull {
            repo: "stroppy-io/letibot".into(),
            number: 12,
            head_sha: "abc".into(),
            base_sha: "def".into(),
            branch: "patch-1".into(),
            from_fork: false,
            title: "a fix".into(),
            author: "someone".into(),
            updated_ms: 0,
        };
        assert_eq!(Standing::of(&pull), Standing::Trusted);
        assert!(Standing::of(&pull).may_run());
        pull.from_fork = true;
        assert_eq!(Standing::of(&pull), Standing::ReadOnly);
        assert!(!Standing::of(&pull).may_run());
        assert_eq!(pull.key(), "stroppy-io/letibot#12");
    }

    /// A mention is what the seat answers: the name it posts as, and nothing else.
    #[test]
    fn only_a_mention_is_a_question() {
        let comment = |body: &str| Comment {
            id: 1,
            author: "someone".into(),
            body: body.into(),
            created_ms: 0,
            in_review: false,
        };
        assert!(comment("hey @letibot is this safe?").mentions("letibot"));
        assert!(!comment("hey letibot is this safe?").mentions("letibot"));
        assert!(!comment("@letibot2 look").mentions("letibot"));
    }
}
