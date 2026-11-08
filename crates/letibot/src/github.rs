//! `letibot attach github <org|owner/repo>`: ask the folder's daemon to hold the reviewer seat on a
//! scope, and print what it holds. The seat - the poll, the rows, the drafts - is the daemon's
//! (`crates/harnessd/src/github.rs`), so the pane and this verb read one snapshot.

use std::path::Path;
use std::process::ExitCode;

use letibot_bridge::daemon::{self, Daemon};
use letibot_github::seat::Config;
use letibot_github::source::Scope;
use letibot_sessionlog::client::HeadClient;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use serde_json::json;

/// How often a scope is asked. Polling rather than webhooks, because a webhook is an endpoint
/// exposed on this box and a poll is not.
const POLL_MS: u64 = 60_000;

/// The command that has both words, for the second line of a refusal. The one-line version ran to
/// 150 columns, so it wrapped into a paragraph wherever a person read it.
const THE_VERB: &str =
    "         `letibot attach github <org|owner/repo>` — an org, or one repository";

/// A mistake is two lines: what is missing, and the command that carries it.
fn needs(what: &str) -> String {
    format!("attach needs {what}\n{THE_VERB}")
}

/// The scope this invocation names. `attach github <scope>` and nothing else: a second source
/// is a second seat with its own identity, and a missing scope would poll whatever the default is.
pub fn scope_from(args: &[String]) -> Result<&str, String> {
    let Some(source) = args.first() else {
        return Err(needs("a source: github"));
    };
    if source != "github" {
        return Err(format!(
            "attach knows one source and it is `github`, not `{source}`\n{THE_VERB}"
        ));
    }
    match args.get(1) {
        Some(scope) if !scope.is_empty() => Ok(scope),
        _ => Err(needs("a scope")),
    }
}

/// What the seat does, what it needs, and one example. Without it `--help` was read as the
/// scope and the seat polled a repository named `--help`.
const USAGE: &str = "\
letibot attach github <org|owner/repo>
  The GitHub reviewer seat: poll the scope's open pull requests, one letibot session per
  pull request and one review per push.

  <org|owner/repo>        the scope to watch: `stroppy-io` for every repository in the org,
                          or `stroppy-io/letibot` for one

  Drafting is the default: nothing is posted to GitHub until `[github] post = true` and
  `[github] token_path` (a file at mode 0600) are set in ~/.config/letibot/leticode.toml.

  example:
    letibot attach github stroppy-io/letibot";

/// Whether this invocation asked for help, wherever the word sits on its line.
fn wants_help(args: &[String]) -> bool {
    args.iter().any(|a| a == "--help" || a == "-h")
}

/// Both levels of `leticode.toml`, merged. `plan` is given this and not the user's file alone:
/// a repository whose own file says `post = false` must be able to stop a seat the user file
/// started, and a project file that will not parse may be the one carrying that `false`.
fn merged(
    user: leticode_config::LeticodeConfig,
    cwd: &Path,
) -> Result<leticode_config::LeticodeConfig, String> {
    let Some(path) = leticode_config::LeticodeConfig::discover(cwd) else {
        return Ok(user);
    };
    let project = leticode_config::LeticodeConfig::load(&path)?;
    Ok(user.overridden_by(&project))
}

/// What the seat will do and what it is allowed to send, worked out before anything is polled:
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

/// `letibot attach github <scope>`, with the scope and the source already named.
pub fn run(args: &[String]) -> ExitCode {
    if wants_help(args) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
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
    let cwd = std::env::current_dir().unwrap_or_default();
    let config_file = match merged(user, &cwd) {
        Ok(file) => file,
        Err(why) => {
            eprintln!("letibot: {why}");
            return ExitCode::from(2);
        }
    };
    let home = std::env::var("HOME").unwrap_or_default();
    let (scope, config) = match plan(scope, &config_file, Path::new(&home)) {
        Ok(planned) => planned,
        Err(why) => {
            eprintln!("letibot: {why}");
            return ExitCode::from(2);
        }
    };
    // The seat is the DAEMON's now (R118): `attach` asks the folder's daemon to hold the scope, and
    // what the operator reads is the daemon's own snapshot. A daemon that is not listening is
    // refused by name: a scope nobody polls is the one thing this verb exists to prevent.
    let daemon = match daemon::resolve(&cwd) {
        Ok(daemon) => daemon,
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
    println!(
        "  held by the daemon for {} — polled every {}s",
        daemon.workspace.display(),
        POLL_MS / 1_000
    );

    match ask(
        &daemon,
        json!({ "verb": "attach", "scope": scope.as_str() }),
    ) {
        Ok(reply) => {
            if let Some(why) = reply.get("error").and_then(serde_json::Value::as_str) {
                eprintln!("letibot: the daemon refused the scope: {why}");
                return ExitCode::from(2);
            }
            print(&reply);
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("letibot: {why}");
            ExitCode::from(2)
        }
    }
}

/// One request to the folder's daemon, and the answer to it: an attach, a pause, a read. The head
/// is the same one every verb that asks a daemon something uses, with no session to be in - the
/// seat is the daemon's, and this connection only asks.
fn ask(daemon: &Daemon, op: serde_json::Value) -> Result<serde_json::Value, String> {
    let (mut client, _hello, mut reader) = HeadClient::attach(
        &daemon.socket,
        "",
        0,
        "github-attach",
        "letibot attach github",
        Caps {
            can_decide: false,
            ..Caps::default()
        },
    )
    .map_err(|e| {
        format!(
            "no daemon is listening on {} ({e}) — start `letibot` in that folder first, because \
             the seat is the daemon's",
            daemon.socket.display()
        )
    })?;
    let id = client.github(op).map_err(|e| e.to_string())?;
    loop {
        // The reader blocks until a frame arrives, and a daemon that stopped closes the socket:
        // a verb that hangs on a dead daemon is the one wait this has, and it ends.
        let frame = reader
            .read()
            .map_err(|e| format!("the daemon's answer could not be read: {e}"))?;
        match frame {
            ServerFrame::Github {
                client_request_id,
                reply,
            } if client_request_id == id => {
                let _ = client.detach();
                return Ok(reply);
            }
            ServerFrame::Bye { reason } => {
                return Err(format!("the daemon closed the connection: {reason}"));
            }
            _ => continue,
        }
    }
}

/// The scope's line and every row under it, as the daemon's snapshot spells them - the same words
/// the pane draws, so a terminal and a pane never disagree about what is held.
fn print(reply: &serde_json::Value) {
    let snapshot = reply.get("snapshot").unwrap_or(reply);
    for scope in snapshot
        .get("scopes")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = scope.get("scope").and_then(|v| v.as_str()).unwrap_or("");
        let state = scope
            .get("state_words")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        println!("  {name}: {state}");
        for pull in scope
            .get("pulls")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let number = pull.get("number").and_then(|v| v.as_u64()).unwrap_or(0);
            let title = pull.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let review = pull.get("review").and_then(|v| v.as_str()).unwrap_or("");
            println!("    #{number} {title} — {review}");
            for draft in pull
                .get("drafts")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
            {
                let said = draft.get("body").and_then(|v| v.as_str()).unwrap_or("");
                println!("      draft: {}", said.lines().next().unwrap_or(""));
            }
            for offer in pull
                .get("offers")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
            {
                let what = offer.get("what").and_then(|v| v.as_str()).unwrap_or("");
                let label = offer.get("label").and_then(|v| v.as_str()).unwrap_or("");
                println!("      {what}: {label}");
            }
        }
    }
}

/// Nothing runs a pull request's code, and the session it ran in was the daemon's. The seat itself
/// is the daemon's now (`crates/harnessd/src/github.rs`), so this verb asks and prints; what is left
/// here is the naming of a scope and the two files that decide what a seat may send.
#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// The three spellings of one GitHub remote are one repository. This is the one comparison
    /// that decides whether a pull request's repository is the one this folder is a checkout of.
    #[test]
    fn a_remote_url_names_its_github_repository() {
        for (url, want) in [
            (
                "https://github.com/stroppy-io/letibot.git",
                Some("stroppy-io/letibot"),
            ),
            (
                "https://github.com/stroppy-io/letibot",
                Some("stroppy-io/letibot"),
            ),
            (
                "git@github.com:stroppy-io/letibot.git",
                Some("stroppy-io/letibot"),
            ),
            (
                "git@github.com:stroppy-io/letibot",
                Some("stroppy-io/letibot"),
            ),
            (
                "ssh://git@github.com/stroppy-io/letibot.git",
                Some("stroppy-io/letibot"),
            ),
            ("https://gitlab.com/stroppy-io/letibot.git", None),
            ("", None),
        ] {
            assert_eq!(
                letibot_github::mirror::slug_of_url(url).as_deref(),
                want,
                "{url}"
            );
        }
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

    /// The plan is the scope and the config together, and both refusals happen before anything
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

    /// A project file may stop the seat, so this verb reads it: a repository whose own file says
    /// `post = false` must not be posted to because the user file says `true`. A project file that
    /// will not parse is a refusal, since it may carry that `false`.
    #[test]
    fn a_projects_file_stops_the_seat_and_its_syntax_is_not_ignored() {
        let dir =
            std::env::temp_dir().join(format!("letibot-attach-github-{}", std::process::id()));
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join("crates/x")).unwrap();
        std::fs::write(repo.join("leticode.toml"), "[github]\npost = false\n").unwrap();

        let mut user = leticode_config::LeticodeConfig::default();
        user.github.post = Some(true);
        user.github.token_path = Some("/run/secrets/github.token".into());
        assert!(user.github.posting(), "the user level alone would post");

        let stopped = merged(user.clone(), &repo.join("crates/x")).expect("the project parses");
        assert!(
            !stopped.github.posting(),
            "the project's own stop is the one direction that must always hold"
        );

        // A file that will not parse is refused by name: a seat that guessed would post.
        std::fs::write(repo.join("leticode.toml"), "[github]\npost = \n").unwrap();
        let why = merged(user, &repo).expect_err("a file that does not parse is not a guess");
        assert!(why.contains("leticode.toml"), "{why}");
    }
}
