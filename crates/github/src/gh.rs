//! The real source: `gh api`, as a subprocess.
//!
//! Nothing here runs `gh` in a test. [`Gh`] builds the command line and hands it to a [`Run`], so a
//! test asserts the command and the body it would send. The token file `[github] token_path` names
//! is handed to `gh` as `GH_TOKEN`, so the identity is that file's and not `gh`'s own store.

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use crate::source::{Comment, Inline, Pull, Review, Scope, Source};

/// The pull-list question. One JSON array per line: a positional array rather than nine names, so a
/// `--jq` filter and its reader cannot drift apart by a key.
const PULLS: &str = ".[] | [.number, .head.sha, .base.sha, .head.ref, .head.repo.full_name, \
                     .base.repo.full_name, .title, .user.login, (.updated_at | fromdateiso8601), \
                     (.body // \"\")]";

/// The same shape for a comment: id, author, when, and what it says.
const COMMENTS: &str = ".[] | [.id, .user.login, (.created_at | fromdateiso8601), .body]";

/// The header that asks GitHub for a diff rather than for JSON.
const DIFF: &str = "Accept: application/vnd.github.v3.diff";

/// **The one thing this crate cannot test**: a program that answers `argv` on its standard output.
/// Every test hands in a recorder instead, which is why no test here needs a network or a token.
pub trait Run: Send + Sync {
    /// Run `argv`, feeding `stdin` when there is any, and answer its stdout.
    fn run(&self, argv: &[String], stdin: Option<&str>) -> Result<String, String>;
}

/// The operator's `gh`, as `LETIBOT_GH` names it or as `PATH` finds it.
pub struct Process {
    gh: String,
    /// The token file whose contents become `GH_TOKEN`: without it `gh` posts as whatever account
    /// its own store is logged in to, which is the account the file exists to keep out.
    token: Option<std::path::PathBuf>,
}

/// The `gh` this box runs: `LETIBOT_GH` is the override every fixture needs.
fn program() -> String {
    std::env::var("LETIBOT_GH").unwrap_or_else(|_| "gh".into())
}

impl Process {
    /// A `gh` at `program`, reading the token out of `token` when there is one.
    pub fn at(program: impl Into<String>, token: Option<&Path>) -> Process {
        Process {
            gh: program.into(),
            token: token.map(Path::to_path_buf),
        }
    }
}

impl Default for Process {
    fn default() -> Process {
        Process::at(program(), None)
    }
}

impl Run for Process {
    fn run(&self, argv: &[String], stdin: Option<&str>) -> Result<String, String> {
        let mut c = Command::new(&self.gh);
        c.args(argv);
        if let Some(path) = &self.token {
            let token = std::fs::read_to_string(path).map_err(|e| {
                format!(
                    "the token file {} cannot be read ({e}), so nothing is posted",
                    path.display()
                )
            })?;
            c.env("GH_TOKEN", token.trim());
        }
        if stdin.is_some() {
            c.stdin(Stdio::piped());
        } else {
            c.stdin(Stdio::null());
        }
        let mut child = c
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("`{}` could not be run ({e})", self.gh))?;
        if let Some(body) = stdin {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .ok_or("the child has no stdin")?
                .write_all(body.as_bytes())
                .map_err(|e| format!("could not write the request body: {e}"))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("`{}` did not finish: {e}", self.gh))?;
        if !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr);
            return Err(format!("`{} {argv:?}` failed: {}", self.gh, why.trim()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// GitHub's API, as this seat needs it.
pub struct Gh {
    run: Box<dyn Run>,
}

impl Default for Gh {
    fn default() -> Gh {
        Gh::new(None)
    }
}

impl Gh {
    /// The real `gh`, posting as the identity `token_path` names when a path is given.
    pub fn new(token_path: Option<String>) -> Gh {
        let token = token_path.map(std::path::PathBuf::from);
        Gh {
            run: Box::new(Process::at(program(), token.as_deref())),
        }
    }

    /// A `gh` that is not the operator's: what every test here uses.
    pub fn with(run: Box<dyn Run>) -> Gh {
        Gh { run }
    }

    fn call(&self, argv: Vec<String>, stdin: Option<&str>) -> Result<String, String> {
        self.run.run(&argv, stdin)
    }

    /// One repository's open pull requests.
    fn pulls_of(&self, repo: &str) -> Result<Vec<Pull>, String> {
        let out = self.call(
            vec![
                "api".into(),
                "--paginate".into(),
                format!("repos/{repo}/pulls?state=open&per_page=100"),
                "--jq".into(),
                PULLS.into(),
            ],
            None,
        )?;
        let mut pulls = Vec::new();
        for line in out.lines().filter(|l| !l.trim().is_empty()) {
            pulls.push(read_pull(repo, line)?);
        }
        Ok(pulls)
    }
}

/// One line of [`PULLS`], as a pull request.
fn read_pull(repo: &str, line: &str) -> Result<Pull, String> {
    let bad = || format!("`gh` answered a pull request this cannot read: {line}");
    let Value::Array(at) = serde_json::from_str::<Value>(line).map_err(|_| bad())? else {
        return Err(bad());
    };
    let text = |i: usize| {
        at.get(i)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let number = at.first().and_then(Value::as_u64).ok_or_else(bad)?;
    let updated = at.get(8).and_then(Value::as_u64).unwrap_or_default();
    let head_repo = text(4);
    Ok(Pull {
        repo: repo.to_string(),
        number,
        head_sha: text(1),
        base_sha: text(2),
        branch: text(3),
        // Where the head lives is the fact, not a flag: a fork is another repository's head.
        from_fork: !head_repo.is_empty() && head_repo != repo,
        title: text(6),
        description: text(9),
        author: text(7),
        updated_ms: updated * 1_000,
    })
}

/// One line of [`COMMENTS`], as a comment.
fn read_comment(line: &str, in_review: bool) -> Result<Comment, String> {
    let bad = || format!("`gh` answered a comment this cannot read: {line}");
    let Value::Array(at) = serde_json::from_str::<Value>(line).map_err(|_| bad())? else {
        return Err(bad());
    };
    Ok(Comment {
        id: at.first().and_then(Value::as_u64).ok_or_else(bad)?,
        author: at.get(1).and_then(Value::as_str).unwrap_or_default().into(),
        created_ms: at.get(2).and_then(Value::as_u64).unwrap_or_default() * 1_000,
        body: at.get(3).and_then(Value::as_str).unwrap_or_default().into(),
        in_review,
    })
}

/// The id GitHub answered for a review or a comment.
fn read_id(out: &str) -> Result<u64, String> {
    serde_json::from_str::<Value>(out.trim())
        .ok()
        .and_then(|v| v.get("id").and_then(Value::as_u64))
        .ok_or_else(|| format!("`gh` answered no id: {}", out.trim()))
}

impl Source for Gh {
    fn list_open(&self, scope: &Scope) -> Result<Vec<Pull>, String> {
        if let Some(repo) = scope.known_repo() {
            return self.pulls_of(repo);
        }
        let Scope::Org(org) = scope else {
            return Err("a scope is an org or a repository".into());
        };
        let out = self.call(
            vec![
                "api".into(),
                "--paginate".into(),
                format!("orgs/{org}/repos?per_page=100"),
                "--jq".into(),
                ".[].full_name".into(),
            ],
            None,
        )?;
        let mut pulls = Vec::new();
        for repo in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
            pulls.extend(self.pulls_of(repo)?);
        }
        Ok(pulls)
    }

    fn diff(&self, pull: &Pull, since: Option<&str>) -> Result<String, String> {
        // A push is the commits between the reviewed head and this one: GitHub's compare endpoint
        // answers exactly that diff, and the whole pull request is the other question.
        let path = match since {
            None => format!("repos/{}/pulls/{}", pull.repo, pull.number),
            Some(sha) => format!("repos/{}/compare/{sha}...{}", pull.repo, pull.head_sha),
        };
        self.call(vec!["api".into(), path, "-H".into(), DIFF.into()], None)
    }

    fn comments(&self, pull: &Pull) -> Result<Vec<Comment>, String> {
        let mut all = Vec::new();
        for (path, in_review) in [
            (
                format!(
                    "repos/{}/issues/{}/comments?per_page=100",
                    pull.repo, pull.number
                ),
                false,
            ),
            (
                format!(
                    "repos/{}/pulls/{}/comments?per_page=100",
                    pull.repo, pull.number
                ),
                true,
            ),
        ] {
            let out = self.call(
                vec![
                    "api".into(),
                    "--paginate".into(),
                    path,
                    "--jq".into(),
                    COMMENTS.into(),
                ],
                None,
            )?;
            for line in out.lines().filter(|l| !l.trim().is_empty()) {
                all.push(read_comment(line, in_review)?);
            }
        }
        Ok(all)
    }

    fn post_review(&self, pull: &Pull, review: &Review) -> Result<u64, String> {
        let comments: Vec<Value> = review
            .comments
            .iter()
            .map(|Inline { path, line, body }| {
                json!({"path": path, "line": line, "side": "RIGHT", "body": body})
            })
            .collect();
        let body = json!({
            "body": review.body,
            "event": review.event.as_str(),
            "comments": comments,
        });
        let out = self.call(
            vec![
                "api".into(),
                "-X".into(),
                "POST".into(),
                format!("repos/{}/pulls/{}/reviews", pull.repo, pull.number),
                "--input".into(),
                "-".into(),
            ],
            Some(&body.to_string()),
        )?;
        read_id(&out)
    }

    fn reply(&self, pull: &Pull, comment: &Comment, body: &str) -> Result<u64, String> {
        // A review thread is answered inside it; the conversation is answered with a comment. The
        // two endpoints are not interchangeable, so which one is asked is the comment's own fact.
        let path = if comment.in_review {
            format!(
                "repos/{}/pulls/{}/comments/{}/replies",
                pull.repo, pull.number, comment.id
            )
        } else {
            format!("repos/{}/issues/{}/comments", pull.repo, pull.number)
        };
        let out = self.call(
            vec![
                "api".into(),
                "-X".into(),
                "POST".into(),
                path,
                "--input".into(),
                "-".into(),
            ],
            Some(&json!({ "body": body }).to_string()),
        )?;
        read_id(&out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Event;
    use std::sync::{Arc, Mutex};

    /// What was asked, and what the fake `gh` answers: argv and stdin in, stdout out. A handle
    /// rather than the `Run` itself, because the test reads the questions after `Gh` owns it.
    #[derive(Default, Clone)]
    struct Recorder {
        inner: Arc<Inner>,
    }

    #[derive(Default)]
    struct Inner {
        seen: Mutex<Vec<(Vec<String>, Option<String>)>>,
        answers: Mutex<Vec<String>>,
    }

    impl Recorder {
        fn with(answers: &[&str]) -> Recorder {
            Recorder {
                inner: Arc::new(Inner {
                    seen: Mutex::new(Vec::new()),
                    answers: Mutex::new(answers.iter().map(|s| (*s).to_string()).collect()),
                }),
            }
        }

        fn seen(&self) -> Vec<(Vec<String>, Option<String>)> {
            self.inner.seen.lock().unwrap().clone()
        }
    }

    impl Run for Recorder {
        fn run(&self, argv: &[String], stdin: Option<&str>) -> Result<String, String> {
            self.inner
                .seen
                .lock()
                .unwrap()
                .push((argv.to_vec(), stdin.map(str::to_string)));
            let mut answers = self.inner.answers.lock().unwrap();
            if answers.is_empty() {
                return Err("nothing left to answer".into());
            }
            Ok(answers.remove(0))
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
            title: "a fix".into(),
            description: String::new(),
            author: "someone".into(),
            updated_ms: 0,
        }
    }

    /// **The list question is a `gh api` on the org and then one per repository**, and a fork is
    /// read from where the head lives - not from anything GitHub calls a flag.
    #[test]
    fn the_org_list_is_asked_repository_by_repository() {
        let rec = Recorder::with(&[
            "stroppy-io/letibot\nstroppy-io/other\n",
            "[12,\"c1\",\"b0\",\"topic\",\"stroppy-io/letibot\",\"stroppy-io/letibot\",\"a fix\",\"someone\",1700000000,\"the why\"]\n",
            "[13,\"d1\",\"b0\",\"patch-1\",\"stranger/letibot\",\"stroppy-io/other\",\"a patch\",\"stranger\",1700000001]\n",
        ]);
        let gh = Gh::with(Box::new(rec.clone()));
        let pulls = gh.list_open(&Scope::Org("stroppy-io".into())).unwrap();
        assert_eq!(pulls.len(), 2);
        assert_eq!(pulls[0].key(), "stroppy-io/letibot#12");
        assert!(!pulls[0].from_fork);
        assert_eq!(
            pulls[0].description, "the why",
            "the author's own sentence is read"
        );
        assert_eq!(
            pulls[1].description, "",
            "a body GitHub did not send is empty, not missing"
        );
        assert_eq!(pulls[0].updated_ms, 1_700_000_000_000);
        assert_eq!(pulls[1].key(), "stroppy-io/other#13");
        assert!(pulls[1].from_fork, "the head is another repository's");
    }

    /// A repository scope is one call, and the command is the one that answers it.
    #[test]
    fn a_repository_scope_is_one_pull_question() {
        let rec = Recorder::with(&[
            "[12,\"c1\",\"b0\",\"topic\",\"stroppy-io/letibot\",\"stroppy-io/letibot\",\"a fix\",\"someone\",1700000000]\n",
        ]);
        let gh = Gh::with(Box::new(rec.clone()));
        let pulls = gh
            .list_open(&Scope::Repo("stroppy-io/letibot".into()))
            .unwrap();
        assert_eq!(pulls.len(), 1);
        let seen = rec.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].0,
            vec![
                "api",
                "--paginate",
                "repos/stroppy-io/letibot/pulls?state=open&per_page=100",
                "--jq",
                PULLS
            ]
        );
        assert!(seen[0].1.is_none(), "a read sends no body");
    }

    /// The diff is a diff: the request asks for one, and a re-review is the compare between the
    /// reviewed head and this one.
    #[test]
    fn a_push_is_reviewed_as_the_compare_to_the_reviewed_head() {
        let rec = Recorder::with(&["diff of everything", "diff of the new commit"]);
        let gh = Gh::with(Box::new(rec.clone()));
        let pull = a_pull();
        assert_eq!(gh.diff(&pull, None).unwrap(), "diff of everything");
        assert_eq!(
            gh.diff(&pull, Some("c0")).unwrap(),
            "diff of the new commit"
        );
        let seen = rec.seen();
        assert_eq!(seen[0].0[1], "repos/stroppy-io/letibot/pulls/12");
        assert_eq!(seen[1].0[1], "repos/stroppy-io/letibot/compare/c0...c1");
        for (argv, _) in seen.iter() {
            assert!(argv.contains(&DIFF.to_string()), "{argv:?}");
        }
    }

    /// Comments come from the two endpoints, and which one a comment came from is what its reply
    /// is addressed with.
    #[test]
    fn the_conversations_and_the_review_threads_are_two_questions() {
        let rec = Recorder::with(&[
            "[7,\"someone\",1700000000,\"@letibot why?\"]\n",
            "[8,\"someone\",1700000001,\"this line\"]\n",
        ]);
        let gh = Gh::with(Box::new(rec.clone()));
        let comments = gh.comments(&a_pull()).unwrap();
        assert_eq!(comments.len(), 2);
        assert!(!comments[0].in_review);
        assert!(comments[0].mentions("letibot"));
        assert!(comments[1].in_review);
        let seen = rec.seen();
        assert_eq!(
            seen[0].0[2],
            "repos/stroppy-io/letibot/issues/12/comments?per_page=100"
        );
        assert_eq!(
            seen[1].0[2],
            "repos/stroppy-io/letibot/pulls/12/comments?per_page=100"
        );
    }

    /// The review is one POST whose body is the summary and its inline comments - and the id it
    /// answers is GitHub's.
    #[test]
    fn a_review_is_posted_as_one_body_with_its_inline_comments() {
        let rec = Recorder::with(&[r#"{"id": 4242}"#]);
        let gh = Gh::with(Box::new(rec.clone()));
        let review = Review {
            body: "two things".into(),
            event: Event::Comment,
            comments: vec![Inline {
                path: "src/lib.rs".into(),
                line: 7,
                body: "this leaks".into(),
            }],
        };
        assert_eq!(gh.post_review(&a_pull(), &review).unwrap(), 4242);
        let seen = rec.seen();
        assert_eq!(
            seen[0].0,
            vec![
                "api",
                "-X",
                "POST",
                "repos/stroppy-io/letibot/pulls/12/reviews",
                "--input",
                "-"
            ]
        );
        let body: Value = serde_json::from_str(seen[0].1.as_deref().unwrap()).unwrap();
        assert_eq!(body["event"], json!("COMMENT"));
        assert_eq!(body["body"], json!("two things"));
        assert_eq!(body["comments"][0]["path"], json!("src/lib.rs"));
        assert_eq!(body["comments"][0]["line"], json!(7));
        assert_eq!(body["comments"][0]["side"], json!("RIGHT"));
    }

    /// A mention is answered where it was asked: inside the thread for a review comment, as a new
    /// comment for the conversation.
    #[test]
    fn a_reply_goes_to_the_endpoint_the_comment_came_from() {
        let rec = Recorder::with(&[r#"{"id": 501}"#, r#"{"id": 502}"#]);
        let gh = Gh::with(Box::new(rec.clone()));
        let thread = Comment {
            id: 8,
            author: "someone".into(),
            body: "@letibot why?".into(),
            created_ms: 0,
            in_review: true,
        };
        let talk = Comment {
            in_review: false,
            ..thread.clone()
        };
        assert_eq!(gh.reply(&a_pull(), &thread, "because").unwrap(), 501);
        assert_eq!(gh.reply(&a_pull(), &talk, "because").unwrap(), 502);
        let seen = rec.seen();
        assert_eq!(
            seen[0].0[3],
            "repos/stroppy-io/letibot/pulls/12/comments/8/replies"
        );
        assert_eq!(seen[1].0[3], "repos/stroppy-io/letibot/issues/12/comments");
        assert_eq!(
            serde_json::from_str::<Value>(seen[1].1.as_deref().unwrap()).unwrap(),
            json!({"body": "because"})
        );
    }

    /// A refusal from `gh` is a refusal in words and never a silent empty answer.
    #[test]
    fn a_gh_that_answers_nothing_is_a_refusal() {
        let rec = Recorder::with(&[]);
        let gh = Gh::with(Box::new(rec.clone()));
        let why = gh.list_open(&Scope::Repo("a/b".into())).unwrap_err();
        assert_eq!(why, "nothing left to answer");
    }

    /// **The token file decides the identity.** Without `GH_TOKEN` a post goes out under whatever
    /// account `gh` is logged in as, which is the one the operator's key file exists to keep out.
    #[test]
    fn the_token_file_is_handed_to_gh_as_gh_token() {
        let dir = std::env::temp_dir().join(format!("letibot-gh-token-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let token = dir.join("github.token");
        std::fs::write(&token, "ghp_x\n").unwrap();

        // `/bin/sh` stands in for `gh`: all a test can watch is what the child was handed.
        let seen = Process::at("/bin/sh", Some(&token))
            .run(&["-c".into(), "printenv GH_TOKEN".into()], None);
        assert_eq!(seen.unwrap().trim(), "ghp_x");

        // A file that has gone is refused by name, and nothing is posted.
        let gone = dir.join("gone.token");
        let why = Process::at("/bin/sh", Some(&gone))
            .run(&["-c".into(), "true".into()], None)
            .unwrap_err();
        assert!(why.contains("gone.token"), "{why}");
    }
}
