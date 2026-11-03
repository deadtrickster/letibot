//! `letibot attach github <org|owner/repo>`: the reviewer seat, started from a terminal.
//!
//! The seat itself is `letibot_github`'s, and it is a poll: what is open in the scope, what changed,
//! which row is owed a review, which comment mentioned it.
//! **The reviewer worker is the other half** - it opens a session with a worktree at the head and
//! runs the queue's reviewer in it - so on this build every row says by name what it is waiting for
//! instead of the run looking idle.

use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use letibot_github::seat::{Config, Job, Reviewer, Seat, Session};
use letibot_github::{Pull, Review, Scope, Standing};

/// How often a scope is asked. Polling rather than webhooks, because a webhook is an endpoint
/// exposed on this box and a poll is not.
const POLL_MS: u64 = 60_000;

/// **The scope this invocation names.** `attach github <scope>` and nothing else: a second source
/// is a second seat with its own identity, and a missing scope would poll whatever the default is.
pub fn scope_from(args: &[String]) -> Result<&str, String> {
    let Some(source) = args.first() else {
        return Err("attach takes a source: `letibot attach github <org|owner/repo>`".into());
    };
    if source != "github" {
        return Err(format!(
            "attach knows one source and it is `github`, not `{source}` \
             (`letibot attach github <org|owner/repo>`)"
        ));
    }
    match args.get(1) {
        Some(scope) if !scope.is_empty() => Ok(scope),
        _ => Err("attach takes a scope: `letibot attach github <org|owner/repo>`".into()),
    }
}

/// **What the seat will do and what it is allowed to send**, worked out before anything is polled:
/// the scope as typed, and the config with the token file resolved and its mode checked.
pub fn plan(
    scope: &str,
    user: &leticode_config::LeticodeConfig,
    home: &Path,
) -> Result<(Scope, Config), String> {
    let scope = Scope::parse(scope)?;
    // `post` is what the operator means; the token is what makes it possible. A file that says
    // `post = true` and names no token is refused here rather than drafting in silence.
    let config = Config::resolved(
        user.github.post.unwrap_or(false),
        user.github.token_path.as_deref(),
        home,
    )?;
    Ok((scope, config))
}

/// **Nothing runs a pull request's code, and this build cannot review at all**: a row says so, so
/// `attach github` is honest about what it is instead of looking like a seat with no work.
struct Unwired;

impl Reviewer for Unwired {
    fn open(&mut self, pull: &Pull, standing: Standing) -> Result<Session, String> {
        Err(format!(
            "{} is {} and the reviewer worker does not open sessions yet",
            pull.key(),
            match standing {
                Standing::ReadOnly => "a fork, so it is read only",
                Standing::Trusted => "this repository's",
            }
        ))
    }

    fn review(&mut self, job: &Job<'_>) -> Result<Review, String> {
        Err(format!("{} has no reviewer worker yet", job.pull.key()))
    }

    fn answer(&mut self, session: &Session, _: &str) -> Result<String, String> {
        Err(format!(
            "{} cannot answer: no reviewer worker yet",
            session.id
        ))
    }
}

/// The clock the seat is asked about: milliseconds since the epoch, as `Pull::updated_ms` is.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// `letibot attach github <scope>`, with the scope and the source already named.
pub fn run(args: &[String]) -> ExitCode {
    let scope = match scope_from(args) {
        Ok(scope) => scope,
        Err(why) => {
            eprintln!("letibot: {why}");
            return ExitCode::from(2);
        }
    };
    let user = match leticode_config::load_user() {
        Ok(user) => user,
        Err(why) => {
            eprintln!("letibot: {why}");
            return ExitCode::from(2);
        }
    };
    let home = std::env::var("HOME").unwrap_or_default();
    let (scope, config) = match plan(scope, &user, Path::new(&home)) {
        Ok(planned) => planned,
        Err(why) => {
            eprintln!("letibot: {why}");
            return ExitCode::from(2);
        }
    };
    println!("the reviewer seat on {}", scope.as_str());
    if config.posting() {
        println!(
            "  posting as {} — reviews and answers are sent to GitHub",
            config.identity
        );
    } else {
        println!("  drafting: nothing is sent to GitHub (`[github] post` and a token turn it on)");
    }
    println!("  polling every {}s", POLL_MS / 1_000);

    let source = letibot_github::gh::Gh::new();
    let mut reviewer = Unwired;
    let mut seat = Seat::new(vec![scope], config);
    loop {
        for said in seat.poll(&source, &mut reviewer, now_ms()) {
            println!("{said}");
        }
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// The scope is the third word and nothing else, and each refusal names what is missing.
    #[test]
    fn the_scope_is_the_word_after_github() {
        assert_eq!(scope_from(&v(&["github", "stroppy-io"])), Ok("stroppy-io"));
        assert_eq!(
            scope_from(&v(&["github", "stroppy-io/letibot"])),
            Ok("stroppy-io/letibot")
        );
        let why = scope_from(&v(&[])).unwrap_err();
        assert!(why.contains("attach github"), "{why}");
        let why = scope_from(&v(&["github"])).unwrap_err();
        assert!(
            why.contains("scope") && why.contains("attach github"),
            "{why}"
        );
        let why = scope_from(&v(&["gitlab", "stroppy-io"])).unwrap_err();
        assert!(why.contains("gitlab") && why.contains("github"), "{why}");
    }

    /// **The plan is the scope and the config together**, and both refusals happen before anything
    /// is polled: a scope that is not one, and posting without the token that makes it possible.
    #[test]
    fn the_plan_refuses_a_bad_scope_and_a_missing_token() {
        let user = leticode_config::LeticodeConfig::default();
        let home = Path::new("/home/operator");
        let (scope, config) = plan("stroppy-io", &user, home).expect("a plain scope");
        assert_eq!(scope, Scope::Org("stroppy-io".into()));
        assert!(!config.posting(), "a file that says nothing drafts");

        let why = plan("a/b/c", &user, home).unwrap_err();
        assert!(why.contains("a/b/c"), "{why}");

        let mut posting = leticode_config::LeticodeConfig::default();
        posting.github.post = Some(true);
        let why = plan("stroppy-io", &posting, home).unwrap_err();
        assert!(why.contains("token_path"), "{why}");
    }
}
