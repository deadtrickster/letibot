//! `letibot attach github <org|owner/repo>`: the reviewer seat, started from a terminal.
//!
//! The seat itself is `letibot_github`'s, and it is a poll: what is open in the scope, what changed,
//! which row is owed a review, which comment mentioned it. The worker is the other half: a review
//! is a brief run in that pull request's own letibot session, and what it answers is the draft a
//! person decides about.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use letibot_bridge::daemon::{self, Daemon};
use letibot_bridge::head::{Attention, Head, create_session_in};
use letibot_github::seat::{Config, Seat, Session};
use letibot_github::worker::{Sessions, Worker};
use letibot_github::{Pull, Scope, Standing};
// The seat is asked the tree's own clock, so a poll's instant and an event's `ts_ms` are one clock.
use letibot_metrics::now_ms;

/// How often a scope is asked. Polling rather than webhooks, because a webhook is an endpoint
/// exposed on this box and a poll is not.
const POLL_MS: u64 = 60_000;

/// The command that has both words, for the second line of a refusal. The one-line version ran to
/// 150 columns, so it wrapped into a paragraph wherever a person read it.
const THE_VERB: &str =
    "         `letibot attach github <org|owner/repo>` — an org, or one repository";

/// **A mistake is two lines**: what is missing, and the command that carries it.
fn needs(what: &str) -> String {
    format!("attach needs {what}\n{THE_VERB}")
}

/// **The scope this invocation names.** `attach github <scope>` and nothing else: a second source
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

/// **What the seat does, what it needs, and one example.** Without it `--help` was read as the
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

/// **Both levels of `leticode.toml`, merged.** `plan` is given this and not the user's file alone:
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

    let sessions = match DaemonSessions::at(&cwd) {
        Ok(sessions) => sessions,
        Err(why) => {
            eprintln!("letibot: {why}");
            return ExitCode::from(2);
        }
    };
    let source = letibot_github::gh::Gh::new(config.token.clone());
    let mut reviewer = Worker::new(sessions);
    let mut seat = Seat::new(vec![scope], config);
    loop {
        for said in seat.poll(&source, &mut reviewer, now_ms()) {
            println!("{said}");
        }
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
    }
}

/// **Nothing runs a pull request's code, and the session it runs in is the daemon's.** The seat asks
/// this for a review and an answer; where the session comes from is the daemon's own business.
pub struct DaemonSessions {
    /// This folder's daemon: the sessions are its own, so the operator attaches to them as they do
    /// to any other, and the trees are worktrees of the checkout it was started for.
    daemon: Daemon,
    /// Where the pull requests' worktrees go: one per pull request, under this folder's key.
    trees: PathBuf,
}

impl DaemonSessions {
    /// The seat's sessions in the daemon of `workspace`. A daemon that is not up is refused by
    /// name: a seat that started one itself would choose the flags of a daemon the operator runs.
    pub fn at(workspace: &Path) -> Result<DaemonSessions, String> {
        let daemon = daemon::resolve(workspace)?;
        if !daemon::is_up(&daemon.socket) {
            return Err(format!(
                "no daemon is listening for {}: a review runs in that daemon's session, so start \
                 it there first (`letibot` in that folder) and attach the seat beside it",
                daemon.workspace.display()
            ));
        }
        let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
            "no $HOME: the pull requests' trees have no home without one".to_string()
        })?;
        let trees = home.join(".local/share/letibot/github").join(&daemon.key);
        Ok(DaemonSessions { daemon, trees })
    }

    /// **One tree per pull request, at its head.** The head is fetched from `origin` as GitHub's
    /// `refs/pull/<n>/head`, which is the same ref for a fork's pull request, and the tree is a
    /// detached worktree of the checkout the seat was started in.
    fn tree_for(&self, pull: &Pull) -> Result<PathBuf, String> {
        let here = origin_slug(&self.daemon.workspace);
        if here.as_deref() != Some(pull.repo.as_str()) {
            return Err(format!(
                "{} is {}'s pull request and this seat runs in {}, so it has no tree for it: \
                 run the seat in a checkout of {}",
                pull.key(),
                pull.repo,
                self.daemon.workspace.display(),
                pull.repo
            ));
        }
        let reference = format!("refs/letibot/pr/{}", pull.number);
        let tree = self.trees.join(tree_name(pull));
        git(
            &self.daemon.workspace,
            &[
                "fetch",
                "--quiet",
                "origin",
                &format!("+refs/pull/{}/head:{reference}", pull.number),
            ],
        )?;
        if tree.is_dir() {
            // A tree a previous review was made in: the head may have moved, and what a reviewer
            // left in it is not work anybody keeps.
            git(
                &tree,
                &["checkout", "--quiet", "--force", "--detach", &reference],
            )?;
        } else {
            std::fs::create_dir_all(&self.trees)
                .map_err(|e| format!("{}: {e}", self.trees.display()))?;
            git(
                &self.daemon.workspace,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "--detach",
                    &tree.display().to_string(),
                    &reference,
                ],
            )?;
        }
        Ok(tree)
    }
}

impl Sessions for DaemonSessions {
    fn open(&mut self, pull: &Pull, standing: Standing) -> Result<Session, String> {
        let tree = self.tree_for(pull)?;
        let title = match standing {
            Standing::ReadOnly => format!("review {} (a fork: nothing of it runs)", pull.key()),
            Standing::Trusted => format!("review {}", pull.key()),
        };
        let id = create_session_in(&self.daemon, &title, &tree.display().to_string())?;
        Ok(Session { id, worktree: tree })
    }

    fn ask(&mut self, session: &Session, brief: &str) -> Result<String, String> {
        let head = Head::attach(&self.daemon, &session.id)?;
        head.prompt(brief)?;
        match head.wait(Instant::now() + REVIEW_PATIENCE) {
            Attention::TurnEnded { message } => Ok(message),
            Attention::Failed { why } => Err(why),
            Attention::Trouble(t) => Err(format!("the provider is {}: {}", t.kind, t.detail)),
            Attention::Ask(card) => Err(format!(
                "{} asked `{}`, and a seat answers no cards: the review is owed again",
                session.id, card.summary
            )),
            Attention::Timeout => Err(format!(
                "{} was still reviewing after {}s",
                session.id,
                REVIEW_PATIENCE.as_secs()
            )),
        }
    }
}

/// One review may take this long before its row is owed again.
const REVIEW_PATIENCE: Duration = Duration::from_secs(20 * 60);

/// **The `owner/name` a remote URL names**, or `None` for a host this seat does not review. The
/// three spellings are one repository, and a checkout that cannot say is a tree the seat refuses.
fn slug_of_url(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let (owner, name) = rest.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

/// The `owner/name` of a checkout's `origin`, when it has one.
fn origin_slug(workspace: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["remote", "get-url", "origin"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    slug_of_url(&String::from_utf8_lossy(&out.stdout))
}

/// One pull request's tree's name: its repository and number, which is one path per pull request.
fn tree_name(pull: &Pull) -> String {
    format!("{}-{}", pull.repo.replace('/', "-"), pull.number)
}

/// `git`, in `dir`, with a refusal that carries git's own words.
fn git(dir: &Path, args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("`git {}` could not be run ({e})", args.join(" ")))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "`git {}` in {} failed: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn a_pull(repo: &str, number: u64) -> Pull {
        Pull {
            repo: repo.into(),
            number,
            head_sha: "c1".into(),
            base_sha: "b0".into(),
            branch: "topic".into(),
            from_fork: false,
            title: "a fix".into(),
            description: String::new(),
            author: "someone".into(),
            updated_ms: 0,
        }
    }

    /// The sessions a seat would hold, over a folder with no daemon: what a test needs to reach the
    /// refusals that happen before anything is asked of one.
    fn sessions_over(dir: &Path) -> DaemonSessions {
        DaemonSessions {
            daemon: daemon::resolve(dir).expect("a folder resolves to a daemon"),
            trees: dir.join("trees"),
        }
    }

    /// **The three spellings of one GitHub remote are one repository.** This is the one comparison
    /// that decides whether this checkout can produce a pull request's tree at all.
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
            assert_eq!(slug_of_url(url).as_deref(), want, "{url}");
        }
    }

    /// A pull request's tree is one path per pull request, so a re-review lands in the tree the
    /// last one was made in rather than in a second copy of it.
    #[test]
    fn a_pull_requests_tree_is_named_by_its_repository_and_number() {
        assert_eq!(
            tree_name(&a_pull("stroppy-io/letibot", 12)),
            "stroppy-io-letibot-12"
        );
    }

    /// **A pull request of another repository is refused by name.** The trees are worktrees of the
    /// checkout the seat was started in, so a stranger's repository has none.
    #[test]
    fn a_pull_request_of_another_repository_is_refused_by_name() {
        let dir = std::env::temp_dir().join(format!("letibot-seat-trees-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut sessions = sessions_over(&dir);
        let why = sessions
            .open(&a_pull("somebody/else", 12), Standing::Trusted)
            .expect_err("no tree for a stranger's repository");
        assert!(why.contains("somebody/else#12"), "{why}");
        assert!(why.contains("somebody/else"), "{why}");
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

    /// **A project file may stop the seat, so this verb reads it.** A repository whose own file
    /// says `post = false` - a client's pull requests, say - must not be posted to because the
    /// user file said `post = true`; and a project file that will not parse is a refusal, because
    /// it may be the one carrying that `false`.
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
