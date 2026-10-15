//! **A monitor on a GitHub pull request or issue** — through the operator's own `gh`.
//!
//! The operator, 2026-10-15: *"we should add more waiting criteria such as github issue or pr"* —
//! closed or merged, checks finished, a review, a new comment, and CodeRabbit *"to be watched in
//! particular and as a separate thing"*. Each is a [`Condition`], so a GitHub watch is a monitor
//! like any other: the poller, an owner, the restart rules, the monitors pane, and a todo row may
//! wait on it (`TodoNeed::Monitor`).
//!
//! # How it looks
//!
//! **`gh`, and nothing else**: the operator's authenticated CLI is the one door to GitHub, so a
//! watch sees what they can see and nothing in the daemon holds a token. It is spawned at most
//! once per interval (default [`EVERY`]) and its child is polled with `try_wait`, the way a
//! command monitor's is, so a slow network never stalls the poller. **What it saw first is the
//! baseline**: a review, comment or CodeRabbit pass is news only if it came after the watch was
//! armed — except a state that is already the answer (a pull request already merged, its checks
//! already finished), which fires at once. A `gh` that is missing or not logged in fires with
//! that, because a watch that can never see anything must not read as one still waiting; a
//! network blip is retried at the next interval.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::monitor::Condition;

/// **How often a watch asks GitHub**, unless the call says. A pull request's checks take minutes
/// and a review hours; ninety seconds is prompt for both and a few hundred calls a day.
pub const EVERY: Duration = Duration::from_secs(90);

/// The shortest interval a call may ask for.
pub const EVERY_MIN: Duration = Duration::from_secs(30);

/// **How long a GitHub watch may live** — reviews take days, and a monitor's ordinary hour would
/// expire before most of them. Renewable like any monitor.
pub const GITHUB_MAX_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

/// The life a GitHub watch gets when the call names none.
pub const GITHUB_DEFAULT_TTL: Duration = Duration::from_secs(24 * 3600);

/// What a watch waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Until {
    /// Closed, merged or not.
    Closed,
    /// Merged — a pull request closed without merging fires too, and says so.
    Merged,
    /// Every check on the pull request's head has finished.
    Checks,
    /// A review that approves or asks for changes (CodeRabbit's are [`Until::CodeRabbit`]'s).
    Review,
    /// A comment or a review comment by anyone but the operator.
    Comment,
    /// CodeRabbit has reviewed.
    CodeRabbit,
}

impl Until {
    pub fn parse(s: &str) -> Option<Until> {
        Some(match s {
            "closed" => Until::Closed,
            "merged" => Until::Merged,
            "checks" => Until::Checks,
            "review" => Until::Review,
            "comment" => Until::Comment,
            "coderabbit" => Until::CodeRabbit,
            _ => return None,
        })
    }

    pub fn word(self) -> &'static str {
        match self {
            Until::Closed => "closing",
            Until::Merged => "merging",
            Until::Checks => "its checks finishing",
            Until::Review => "a review",
            Until::Comment => "a new comment",
            Until::CodeRabbit => "CodeRabbit's review",
        }
    }

    /// Only a pull request has these.
    pub fn needs_pr(self) -> bool {
        matches!(
            self,
            Until::Merged | Until::Checks | Until::Review | Until::CodeRabbit
        )
    }
}

/// What a watch is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// `owner/name`.
    pub repo: String,
    pub number: u64,
    pub pr: bool,
}

impl Target {
    /// **A pull request or issue URL** — `https://github.com/o/r/pull/12`, `…/issues/7`.
    pub fn from_url(url: &str) -> Option<Target> {
        let rest = url
            .trim()
            .trim_end_matches('/')
            .strip_prefix("https://github.com/")
            .or_else(|| url.trim().strip_prefix("github.com/"))?;
        let parts: Vec<&str> = rest.split('/').collect();
        let (owner, name, kind, number) =
            (parts.first()?, parts.get(1)?, parts.get(2)?, parts.get(3)?);
        let number = number.split(['#', '?']).next()?.parse().ok()?;
        let pr = match *kind {
            "pull" => true,
            "issues" => false,
            _ => return None,
        };
        Some(Target {
            repo: format!("{owner}/{name}"),
            number,
            pr,
        })
    }

    pub fn describe(&self) -> String {
        format!(
            "{} {}#{}",
            if self.pr { "pull request" } else { "issue" },
            self.repo,
            self.number
        )
    }
}

/// What one look saw.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Seen {
    state: String,
    head: String,
    /// Reviews that approve or ask for changes, not CodeRabbit's: `(author, state)`, oldest first.
    reviews: Vec<(String, String)>,
    /// Comments and review bodies by anyone but the operator: `(when, author, first line)`, in
    /// the order they were written — two lists merged, so "the latest" is the latest.
    comments: Vec<(String, String, String)>,
    /// CodeRabbit's reviews: `(state, first line)`.
    rabbit: Vec<(String, String)>,
    /// `None` while any check is pending or there are none; `Some((passed, failed names))` once all
    /// have finished.
    checks: Option<(usize, Vec<String>)>,
    /// GitHub says the pull request conflicts with its base (`mergeable: CONFLICTING`).
    conflict: bool,
}

#[derive(Debug, Default)]
struct State {
    viewer: Option<String>,
    child: Option<(Step, Child)>,
    last: Option<Instant>,
    base: Option<Seen>,
    latest: Option<Seen>,
    /// The head whose finished checks were already reported.
    checks_told: Option<String>,
    /// The head whose conflict with the base was already reported.
    conflict_told: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Viewer,
    View,
}

/// **A watch on a pull request or issue.** See the module docs.
#[derive(Debug)]
pub struct GithubCondition {
    target: Target,
    until: Until,
    every: Duration,
    gh: String,
    state: Mutex<State>,
}

impl GithubCondition {
    pub fn new(target: Target, until: Until, every: Duration) -> std::sync::Arc<Self> {
        std::sync::Arc::new(GithubCondition {
            target,
            until,
            every: every.max(EVERY_MIN),
            // The operator's `gh`; a test points this at a stub.
            gh: std::env::var("LETIBOT_GH").unwrap_or_else(|_| "gh".into()),
            state: Mutex::new(State::default()),
        })
    }

    #[cfg(test)]
    fn with_gh(target: Target, until: Until, gh: &str) -> std::sync::Arc<Self> {
        std::sync::Arc::new(GithubCondition {
            target,
            until,
            every: Duration::ZERO,
            gh: gh.into(),
            state: Mutex::new(State::default()),
        })
    }

    fn spawn(&self, step: Step) -> Result<Child, String> {
        let mut c = Command::new(&self.gh);
        match step {
            Step::Viewer => {
                c.args(["api", "user", "--jq", ".login"]);
            }
            Step::View if self.target.pr => {
                c.args(["pr", "view", &self.target.number.to_string(), "--repo"])
                    .arg(&self.target.repo)
                    .args([
                        "--json",
                        "state,headRefOid,mergeable,reviews,comments,statusCheckRollup",
                    ]);
            }
            Step::View => {
                c.args(["issue", "view", &self.target.number.to_string(), "--repo"])
                    .arg(&self.target.repo)
                    .args(["--json", "state,comments"]);
            }
        }
        c.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                format!(
                    "`{}` could not be run ({e}) — is the GitHub CLI installed?",
                    self.gh
                )
            })
    }

    fn evaluate(&self, st: &mut State, seen: Seen) -> Option<String> {
        st.latest = Some(seen.clone());
        let first = st.base.is_none();
        let base = st.base.get_or_insert_with(|| seen.clone()).clone();
        let what = self.target.describe();
        // **A pull request in conflict will not merge and will not be checked** — GitHub runs no
        // CI on a branch that conflicts with its base — so a watch for its merging or its checks
        // is waiting on something that cannot happen. Measured on stroppy-io/stroppy#206: a
        // `merged` watch and a model "waiting for CI" over `mergeable: CONFLICTING`. Said once
        // per head, including at arming: resolving and pushing is the next step either way.
        if matches!(self.until, Until::Merged | Until::Checks)
            && seen.state == "OPEN"
            && seen.conflict
            && st.conflict_told.as_deref() != Some(seen.head.as_str())
        {
            st.conflict_told = Some(seen.head.clone());
            return Some(format!(
                "{what} has merge CONFLICTS with its base — it cannot be merged, and GitHub runs \
                 no checks on it until they are resolved (merge or rebase onto the base, push). \
                 Watching for {} goes on nowhere until then",
                self.until.word()
            ));
        }
        match self.until {
            Until::Closed if seen.state != "OPEN" => {
                Some(format!("{what} is {}", seen.state.to_lowercase()))
            }
            Until::Merged if seen.state == "MERGED" => Some(format!("{what} was merged")),
            Until::Merged if seen.state == "CLOSED" => {
                Some(format!("{what} was closed WITHOUT merging"))
            }
            Until::Checks => {
                let (passed, failed) = seen.checks.clone()?;
                if st.checks_told.as_deref() == Some(seen.head.as_str()) {
                    return None;
                }
                st.checks_told = Some(seen.head.clone());
                let at = &seen.head[..seen.head.len().min(8)];
                Some(if failed.is_empty() {
                    format!("{what}: every check passed on {at} ({passed})")
                } else {
                    format!(
                        "{what}: checks finished on {at} — {passed} passed, {} FAILED ({})",
                        failed.len(),
                        failed.join(", ")
                    )
                })
            }
            Until::Review if !first && seen.reviews.len() > base.reviews.len() => {
                let (who, how) = seen.reviews.last()?;
                Some(match how.as_str() {
                    "APPROVED" => format!("{what} was approved by {who}"),
                    _ => format!("{who} asked for changes on {what}"),
                })
            }
            Until::Comment if !first && seen.comments.len() > base.comments.len() => {
                let new = &seen.comments[base.comments.len()..];
                let (_, who, said) = new.last()?;
                let quoted = if said.is_empty() {
                    String::new()
                } else {
                    format!(": \"{said}\"")
                };
                Some(format!(
                    "{} new comment{} on {what} — the latest by {who}{quoted}",
                    new.len(),
                    if new.len() == 1 { "" } else { "s" }
                ))
            }
            Until::CodeRabbit if !first && seen.rabbit.len() > base.rabbit.len() => {
                let (how, said) = seen.rabbit.last()?;
                let how = how.to_lowercase().replace('_', " ");
                // **A review whose own text is empty has said everything on the diff** — CodeRabbit
                // posts its findings as inline comments and often leaves the review body blank, and
                // quoting `""` read as a review that said nothing (the operator: *"empty line looks
                // suspicious"*). Say where its words are instead.
                Some(if said.is_empty() {
                    format!(
                        "CodeRabbit reviewed {what} ({how}); its review has no summary text, so what \
                         it found is in its inline comments on the diff — `gh pr view {} --repo {} \
                         --comments`, or the review threads on GitHub",
                        self.target.number, self.target.repo
                    )
                } else {
                    format!("CodeRabbit reviewed {what} ({how}): \"{said}\"")
                })
            }
            _ => None,
        }
    }
}

fn first_line(s: &str) -> String {
    let l = s
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let mut out: String = l.chars().take(120).collect();
    if l.chars().count() > 120 {
        out.push('…');
    }
    out
}

fn login(v: &Value) -> String {
    v.get("author")
        .and_then(|a| a.get("login"))
        .and_then(|l| l.as_str())
        .unwrap_or("someone")
        .to_string()
}

fn is_rabbit(who: &str) -> bool {
    who.to_ascii_lowercase().starts_with("coderabbit")
}

/// **One look, read** — `gh pr view --json` or `gh issue view --json`.
fn parse(json: &str, viewer: Option<&str>) -> Option<Seen> {
    let v: Value = serde_json::from_str(json).ok()?;
    let arr = |k: &str| {
        v.get(k)
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default()
    };
    let mut seen = Seen {
        state: v.get("state")?.as_str()?.to_string(),
        head: v
            .get("headRefOid")
            .and_then(|h| h.as_str())
            .unwrap_or("")
            .to_string(),
        conflict: v.get("mergeable").and_then(|m| m.as_str()) == Some("CONFLICTING"),
        ..Seen::default()
    };
    let mine = |who: &str| viewer.is_some_and(|me| me.eq_ignore_ascii_case(who));
    for c in arr("comments") {
        let who = login(&c);
        // CodeRabbit's summary comment is its own watch's news, not "a new comment".
        if !mine(&who) && !is_rabbit(&who) {
            let body = c.get("body").and_then(|b| b.as_str()).unwrap_or("");
            let at = c.get("createdAt").and_then(|t| t.as_str()).unwrap_or("");
            seen.comments.push((at.to_string(), who, first_line(body)));
        }
    }
    for r in arr("reviews") {
        let who = login(&r);
        let how = r
            .get("state")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let body = r.get("body").and_then(|b| b.as_str()).unwrap_or("");
        if is_rabbit(&who) {
            seen.rabbit.push((how, first_line(body)));
            continue;
        }
        if how == "APPROVED" || how == "CHANGES_REQUESTED" {
            seen.reviews.push((who.clone(), how.clone()));
        }
        if !body.trim().is_empty() && !mine(&who) {
            let at = r.get("submittedAt").and_then(|t| t.as_str()).unwrap_or("");
            seen.comments.push((at.to_string(), who, first_line(body)));
        }
    }
    // RFC 3339 stamps from one API sort as text; a stable sort keeps a missing one in place.
    seen.comments.sort_by(|a, b| a.0.cmp(&b.0));
    let checks = arr("statusCheckRollup");
    if !checks.is_empty() {
        let mut passed = 0;
        let mut failed = Vec::new();
        let mut pending = false;
        for c in &checks {
            let name = c
                .get("name")
                .or_else(|| c.get("context"))
                .and_then(|n| n.as_str())
                .unwrap_or("a check")
                .to_string();
            let s = |k: &str| c.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            let (done, ok) = if c.get("status").is_some() {
                let conclusion = s("conclusion");
                (
                    s("status") == "COMPLETED",
                    matches!(conclusion.as_str(), "SUCCESS" | "NEUTRAL" | "SKIPPED"),
                )
            } else {
                let state = s("state");
                (
                    !matches!(state.as_str(), "PENDING" | "EXPECTED" | ""),
                    state == "SUCCESS",
                )
            };
            if !done {
                pending = true;
            } else if ok {
                passed += 1;
            } else {
                failed.push(name);
            }
        }
        if !pending {
            seen.checks = Some((passed, failed));
        }
    }
    Some(seen)
}

/// Whether `gh` failing is the kind no later look will fix.
fn hopeless(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("gh auth login")
        || s.contains("not logged")
        || s.contains("authentication")
        || s.contains("could not resolve to a")
        || s.contains("not found")
}

impl Condition for GithubCondition {
    fn met(&self) -> Option<String> {
        let mut st = self.state.lock().expect("github watch");
        if let Some((step, child)) = st.child.as_mut() {
            let step = *step;
            match child.try_wait() {
                Ok(None) => return None,
                Err(e) => {
                    st.child = None;
                    return Some(format!("`gh` could not be waited for: {e}"));
                }
                Ok(Some(status)) => {
                    let (mut out, mut err) = (String::new(), String::new());
                    if let Some(mut o) = child.stdout.take() {
                        let _ = o.read_to_string(&mut out);
                    }
                    if let Some(mut e) = child.stderr.take() {
                        let _ = e.read_to_string(&mut err);
                    }
                    st.child = None;
                    st.last = Some(Instant::now());
                    if !status.success() {
                        if hopeless(&err) {
                            return Some(format!(
                                "GitHub cannot be watched here: `gh` says \"{}\"",
                                first_line(&err)
                            ));
                        }
                        return None;
                    }
                    match step {
                        Step::Viewer => {
                            st.viewer = Some(out.trim().to_string());
                            st.last = None;
                        }
                        Step::View => {
                            let seen = parse(&out, st.viewer.as_deref())?;
                            return self.evaluate(&mut st, seen);
                        }
                    }
                }
            }
        }
        if st.child.is_some() || st.last.is_some_and(|t| t.elapsed() < self.every) {
            return None;
        }
        let step = if st.viewer.is_none() && self.until == Until::Comment {
            Step::Viewer
        } else {
            Step::View
        };
        match self.spawn(step) {
            Ok(c) => {
                st.child = Some((step, c));
                None
            }
            Err(e) => Some(e),
        }
    }

    fn rearm(&self) {
        // **The look that fired is the next baseline**, so a repeating watch reports what came
        // after it: the next review, the next comment, the checks of the next push.
        let mut st = self.state.lock().expect("github watch");
        st.base = st.latest.clone();
    }

    fn describe(&self) -> String {
        format!("{} until {}", self.target.describe(), self.until.word())
    }

    fn max_ttl(&self) -> Option<Duration> {
        Some(GITHUB_MAX_TTL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_names_its_pull_request_or_issue() {
        assert_eq!(
            Target::from_url("https://github.com/stroppy-io/stroppy/pull/412"),
            Some(Target {
                repo: "stroppy-io/stroppy".into(),
                number: 412,
                pr: true
            })
        );
        assert_eq!(
            Target::from_url("https://github.com/o/r/issues/7#issuecomment-1")
                .map(|t| (t.number, t.pr)),
            Some((7, false))
        );
        assert_eq!(Target::from_url("https://example.com/o/r/pull/1"), None);
        assert_eq!(Target::from_url("https://github.com/o/r/tree/main"), None);
    }

    fn pr(state: &str, reviews: &str, comments: &str, checks: &str) -> String {
        format!(
            r#"{{"state":"{state}","headRefOid":"abcdef1234","reviews":[{reviews}],"comments":[{comments}],"statusCheckRollup":[{checks}]}}"#
        )
    }

    #[test]
    fn checks_finish_once_per_head_and_name_what_failed() {
        let c = GithubCondition::with_gh(
            Target::from_url("https://github.com/o/r/pull/1").unwrap(),
            Until::Checks,
            "gh",
        );
        let mut st = State::default();
        let pending = parse(
            &pr(
                "OPEN",
                "",
                "",
                r#"{"name":"build","status":"IN_PROGRESS","conclusion":""}"#,
            ),
            None,
        )
        .unwrap();
        assert_eq!(c.evaluate(&mut st, pending), None, "still running");
        let done = parse(
            &pr(
                "OPEN",
                "",
                "",
                r#"{"name":"build","status":"COMPLETED","conclusion":"SUCCESS"},{"name":"lint","status":"COMPLETED","conclusion":"FAILURE"},{"context":"ci/legacy","state":"SUCCESS"}"#,
            ),
            None,
        )
        .unwrap();
        let said = c.evaluate(&mut st, done.clone()).unwrap();
        assert_eq!(
            said,
            "pull request o/r#1: checks finished on abcdef12 — 2 passed, 1 FAILED (lint)"
        );
        assert_eq!(c.evaluate(&mut st, done), None, "told once for this head");
    }

    #[test]
    fn a_review_comment_or_rabbit_is_news_only_after_the_baseline() {
        let t = Target::from_url("https://github.com/o/r/pull/1").unwrap();
        let ann = r#"{"author":{"login":"ann"},"state":"COMMENTED","body":"hm","submittedAt":"2026-10-15T08:00:00Z"}"#;
        let before = pr("OPEN", ann, "", "");
        let after = pr(
            "OPEN",
            &format!(
                r#"{ann},{{"author":{{"login":"bob"}},"state":"APPROVED","body":"","submittedAt":"2026-10-15T09:00:00Z"}},{{"author":{{"login":"coderabbitai"}},"state":"COMMENTED","body":"**Actionable comments posted: 3**\n...","submittedAt":"2026-10-15T09:01:00Z"}}"#
            ),
            r#"{"author":{"login":"me"},"body":"mine","createdAt":"2026-10-15T09:02:00Z"},{"author":{"login":"carl"},"body":"looks off\nmore","createdAt":"2026-10-15T09:03:00Z"}"#,
            "",
        );
        for (until, want) in [
            (Until::Review, "pull request o/r#1 was approved by bob"),
            (
                Until::Comment,
                "1 new comment on pull request o/r#1 — the latest by carl: \"looks off\"",
            ),
            (
                Until::CodeRabbit,
                "CodeRabbit reviewed pull request o/r#1 (commented): \"**Actionable comments posted: 3**\"",
            ),
        ] {
            let c = GithubCondition::with_gh(t.clone(), until, "gh");
            let mut st = State::default();
            assert_eq!(
                c.evaluate(&mut st, parse(&before, Some("me")).unwrap()),
                None,
                "{until:?}"
            );
            assert_eq!(
                c.evaluate(&mut st, parse(&after, Some("me")).unwrap())
                    .as_deref(),
                Some(want),
                "{until:?}"
            );
        }
    }

    /// **A CodeRabbit review with no text of its own says where its findings are** — the
    /// operator, on `pr206-coderabbit` firing with `: ""`: *"empty line looks suspicious"*.
    #[test]
    fn an_empty_rabbit_review_points_at_its_inline_comments() {
        let t = Target::from_url("https://github.com/stroppy-io/stroppy/pull/206").unwrap();
        let c = GithubCondition::with_gh(t, Until::CodeRabbit, "gh");
        let mut st = State::default();
        assert_eq!(
            c.evaluate(&mut st, parse(&pr("OPEN", "", "", ""), None).unwrap()),
            None
        );
        let said = c
            .evaluate(
                &mut st,
                parse(
                    &pr(
                        "OPEN",
                        r#"{"author":{"login":"coderabbitai"},"state":"COMMENTED","body":"","submittedAt":"2026-10-16T08:00:00Z"}"#,
                        "",
                        "",
                    ),
                    None,
                )
                .unwrap(),
            )
            .unwrap();
        assert!(!said.contains("\"\""), "nothing empty is quoted: {said}");
        assert!(
            said.contains("no summary text")
                && said.contains("gh pr view 206 --repo stroppy-io/stroppy --comments"),
            "{said}"
        );
    }

    /// **A pull request in conflict says so to a watch for its merging or its checks** — once per
    /// head, at arming too — and a watch for reviews is not told.
    #[test]
    fn a_conflicting_pull_request_is_said_once_per_head() {
        let t = Target::from_url("https://github.com/stroppy-io/stroppy/pull/206").unwrap();
        let dirty = r#"{"state":"OPEN","headRefOid":"abc","mergeable":"CONFLICTING","reviews":[],"comments":[],"statusCheckRollup":[]}"#;
        for until in [Until::Merged, Until::Checks] {
            let c = GithubCondition::with_gh(t.clone(), until, "gh");
            let mut st = State::default();
            let said = c.evaluate(&mut st, parse(dirty, None).unwrap()).unwrap();
            assert!(
                said.contains("merge CONFLICTS") && said.contains("no checks"),
                "{said}"
            );
            assert_eq!(
                c.evaluate(&mut st, parse(dirty, None).unwrap()),
                None,
                "once per head"
            );
        }
        let c = GithubCondition::with_gh(t, Until::Review, "gh");
        let mut st = State::default();
        assert_eq!(c.evaluate(&mut st, parse(dirty, None).unwrap()), None);
    }

    #[test]
    fn a_state_that_is_already_the_answer_fires_at_once() {
        let t = Target::from_url("https://github.com/o/r/pull/1").unwrap();
        let c = GithubCondition::with_gh(t.clone(), Until::Merged, "gh");
        let mut st = State::default();
        assert_eq!(
            c.evaluate(&mut st, parse(&pr("CLOSED", "", "", ""), None).unwrap())
                .as_deref(),
            Some("pull request o/r#1 was closed WITHOUT merging")
        );
    }

    /// **Through a stub `gh`, end to end** — spawned, polled, read: the watch fires on the look
    /// that shows the change, and a `gh` that is not logged in fires with that.
    #[test]
    fn a_stub_gh_is_asked_and_answers() {
        let dir = std::env::temp_dir().join(format!("letibot-gh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let gh = dir.join("gh");
        std::fs::write(
            &gh,
            "#!/bin/sh\necho '{\"state\":\"MERGED\",\"headRefOid\":\"x\",\"reviews\":[],\"comments\":[],\"statusCheckRollup\":[]}'\n",
        )
        .unwrap();
        let bad = dir.join("gh-bad");
        std::fs::write(&bad, "#!/bin/sh\necho 'To get started with GitHub CLI, please run:  gh auth login' >&2\nexit 4\n").unwrap();
        for p in [&gh, &bad] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let t = Target::from_url("https://github.com/o/r/pull/9").unwrap();
        let run = |c: &GithubCondition| {
            for _ in 0..200 {
                if let Some(why) = c.met() {
                    return why;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("never fired");
        };
        let c = GithubCondition::with_gh(t.clone(), Until::Closed, gh.to_str().unwrap());
        assert_eq!(run(&c), "pull request o/r#9 is merged");
        let c = GithubCondition::with_gh(t, Until::Closed, bad.to_str().unwrap());
        assert!(run(&c).starts_with("GitHub cannot be watched here: `gh` says"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
