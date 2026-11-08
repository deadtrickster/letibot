//! The reviewer worker: one brief into the pull request's own session, one review out.
//! The seat decides what is owed; this decides what to ask and what the answer means.
//! Session placement arrives through [`Sessions`], which tests fake.

use crate::seat::{Job, Reviewer, Session};
use crate::source::{Event, Inline, Pull, Review, Standing};

/// Where a pull request's session comes from. One session per pull request, opened once and
/// reused for every push on it - so the second review of a change knows what the first one said.
pub trait Sessions {
    /// A session standing in this pull request's tree, or a refusal in words.
    fn open(&mut self, pull: &Pull, standing: Standing) -> Result<Session, String>;

    /// Run `brief` in that session and answer what it said. One turn: the answer is the review.
    fn ask(&mut self, session: &Session, brief: &str) -> Result<String, String>;
}

/// The worker, over whichever sessions a host hands it.
pub struct Worker<S: Sessions> {
    sessions: S,
}

impl<S: Sessions> Worker<S> {
    pub fn new(sessions: S) -> Worker<S> {
        Worker { sessions }
    }

    /// The sessions behind this worker, for a caller that has to read what they were asked.
    pub fn sessions(&self) -> &S {
        &self.sessions
    }
}

impl<S: Sessions> Reviewer for Worker<S> {
    fn open(&mut self, pull: &Pull, standing: Standing) -> Result<Session, String> {
        self.sessions.open(pull, standing)
    }

    fn review(&mut self, job: &Job<'_>) -> Result<Review, String> {
        let said = self.sessions.ask(job.session, &review_brief(job))?;
        review_of(&said)
    }

    fn answer(&mut self, session: &Session, question: &str) -> Result<String, String> {
        let said = self.sessions.ask(session, &answer_brief(question))?;
        let said = said.trim().to_string();
        if said.is_empty() {
            return Err(format!(
                "{} was asked about a comment and answered nothing",
                session.id
            ));
        }
        Ok(said)
    }
}

/// The brief a review is asked with. Everything a reviewer needs is in it and nothing is left to
/// a session's own look around: the diff, what the author said, and what has been said since.
pub fn review_brief(job: &Job<'_>) -> String {
    let pull = job.pull;
    let mut out = String::new();
    if job.standing.may_run() {
        out.push_str("Review this pull request. You may run what its tree has to prove a claim.");
    } else {
        out.push_str(
            "Review this pull request. Its tree is somebody else's: run nothing in it, and read \
             the diff below instead.",
        );
    }
    out.push_str(&format!("\n\n{}: {}", pull.key(), pull.title.trim()));

    out.push_str("\n\nWhat the author says it is:\n");
    let description = pull.description.trim();
    out.push_str(if description.is_empty() {
        "no description was written\n"
    } else {
        description
    });
    if !description.is_empty() {
        out.push('\n');
    }

    if !job.comments.is_empty() {
        out.push_str("\nWhat has been said on it so far:\n");
        for comment in job.comments {
            out.push_str(&format!("- {}: {}\n", comment.author, comment.body.trim()));
        }
    }

    match job.since {
        Some(sha) => out.push_str(&format!(
            "\nThe new commits only, since {sha}:\n{}",
            job.diff.trim_end()
        )),
        None => out.push_str(&format!("\nThe whole diff:\n{}", job.diff.trim_end())),
    }
    out.push_str(
        "\n\nAnswer with the review itself: the summary first, then one line for each thing that \
         belongs on a line of the diff, spelled `path:LINE: what is wrong`. Nothing else.\n",
    );
    out
}

/// The brief a mention is asked with: somebody's words, and the one thing the answer is.
pub fn answer_brief(question: &str) -> String {
    format!(
        "Somebody wrote this on the pull request you reviewed:\n\n{}\n\nAnswer it in the thread: \
         the answer itself and nothing else.\n",
        question.trim()
    )
}

/// The review a session's answer is. The prose is the summary and each line spelled
/// `path:LINE: what is wrong` is a comment on that line; the event is always a comment, because an
/// unattended reviewer's verdict is not a gate.
pub fn review_of(said: &str) -> Result<Review, String> {
    let mut body: Vec<&str> = Vec::new();
    let mut comments: Vec<Inline> = Vec::new();
    for line in said.lines() {
        match inline_in(line) {
            Some(on_line) => comments.push(on_line),
            None => body.push(line),
        }
    }
    let body = body.join("\n").trim().to_string();
    if body.is_empty() && comments.is_empty() {
        return Err("the session answered nothing, so there is no review to keep".into());
    }
    Ok(Review {
        body,
        event: Event::Comment,
        comments,
    })
}

/// One line of an answer as an inline comment: `path:LINE: what is wrong`, where the path has to
/// look like one - a sentence that opens with a word and a colon is prose.
/// The spelling may sit after a member's name (`reads the diff: src/x.rs:3: leaks`), so the scan is
/// of the whole line and not of its first word.
pub fn inline_in(line: &str) -> Option<Inline> {
    let line = line.trim();
    for (at, _) in line.match_indices(':') {
        let before = line[..at].trim_end();
        let path = match before.rsplit_once(char::is_whitespace) {
            Some((_, last)) => last,
            None => before,
        };
        if path.is_empty() || path.contains(char::is_whitespace) {
            continue;
        }
        if !path.contains('/') && !path.contains('.') {
            continue;
        }
        let rest = line[at + 1..].trim_start();
        let Some(digits) = rest.find(':') else {
            continue;
        };
        let Ok(line_no) = rest[..digits].trim().parse::<u64>() else {
            continue;
        };
        let body = rest[digits + 1..].trim();
        if body.is_empty() {
            continue;
        }
        return Some(Inline {
            path: path.to_string(),
            line: line_no,
            body: body.to_string(),
        });
    }
    None
}

#[cfg(test)]
mod tests;
