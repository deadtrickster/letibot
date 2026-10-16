//! **A pull request the session opened is watched without being asked** — the operator: *"somehow
//! model and harness already knows pr is created and watched. maybe intercept gh tool calls?"*.
//!
//! When a `bash` call that ran `gh pr create` succeeds and its output names the pull request
//! (`https://github.com/OWNER/REPO/pull/N`, which is what `gh pr create` prints), the harness arms
//! the watches a person would want on it — its checks, its reviews, CodeRabbit's review, new
//! comments, and its merging or closing — as ordinary monitors owned by the session, through the
//! same door the operator's own calls use, and says so in that call's result, so the model reads
//! what is being watched at the moment it opened the pull request. Each firing wakes the session;
//! `monitor` with `action: "retire"` ends one.
//!
//! Read off the call and its RESULT, never guessed from a command alone: a `gh pr create` that
//! failed opened nothing, and one that succeeded printed the URL.

use letibot_transcript::ToolCall;

/// The watches armed on a pull request: a name suffix, the `until`, and whether it repeats.
pub const WATCHES: &[(&str, &str, bool)] = &[
    ("checks", "checks", true),
    ("review", "review", true),
    ("coderabbit", "coderabbit", true),
    ("comment", "comment", true),
    ("closed", "closed", false),
];

/// **The pull request a successful `gh pr create` opened**, as `(url, number)` — `None` for any
/// other call, and for one whose output names no pull request.
pub fn opened(call: &ToolCall, payload: &str) -> Option<(String, u64)> {
    if call.name != "bash" {
        return None;
    }
    let args: serde_json::Value = serde_json::from_str(&call.arguments).ok()?;
    let command = args.get("command")?.as_str()?;
    if !(command.contains("gh pr create") || command.contains("gh pr new")) {
        return None;
    }
    // **The LAST pull request URL in the output**, not the first GitHub URL: a command that pushes
    // before it creates prints git's `set up to track 'https://github.com/o/r.git/…'` first, and
    // stopping there missed stroppy-io/stroppy#212. `gh pr create` prints the pull request last.
    payload
        .match_indices("https://github.com/")
        .filter_map(|(at, _)| {
            let url: String = payload[at..]
                .chars()
                .take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | ')' | '>' | '`'))
                .collect();
            let target = letibot_tools::exec::github::Target::from_url(&url)?;
            target.pr.then_some((url, target.number))
        })
        .last()
}

/// The `monitor` calls that watch pull request `n` at `url`.
pub fn watch_calls(url: &str, n: u64) -> Vec<(String, ToolCall)> {
    WATCHES
        .iter()
        .map(|(suffix, until, repeat)| {
            let name = format!("pr-{n}-{suffix}");
            let args = serde_json::json!({
                "name": name,
                "github": {"url": url, "until": until},
                "repeat": repeat,
            });
            (
                name.clone(),
                ToolCall {
                    id: format!("pr-watch-{name}"),
                    name: "monitor".into(),
                    arguments: args.to_string(),
                },
            )
        })
        .collect()
}

/// **What the model reads, appended to the `gh pr create` result** — what is now watched, and
/// what could not be.
pub fn told(url: &str, armed: &[String], refused: &[(String, String)]) -> String {
    let mut out = String::new();
    if !armed.is_empty() {
        out.push_str(&format!(
            "\n\n[harness] {url} is now watched for you — {} (monitors {}). Each wakes you when \
             it fires; a todo row can wait on one with {{\"kind\": \"monitor\", \"name\": …}}; \
             `monitor` with action \"retire\" ends one.",
            "its checks, reviews, CodeRabbit's review, new comments, and its merging or closing",
            armed
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for (name, why) in refused {
        out.push_str(&format!("\n[harness] `{name}` was not armed: {why}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash(cmd: &str) -> ToolCall {
        ToolCall {
            id: "c".into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": cmd }).to_string(),
        }
    }

    #[test]
    fn a_pr_create_that_printed_its_url_is_the_pr_it_opened() {
        let out = "Creating pull request for agent/fix into main in o/r\n\nhttps://github.com/o/r/pull/412\n";
        assert_eq!(
            opened(&bash("cd x && gh pr create --fill"), out),
            Some(("https://github.com/o/r/pull/412".into(), 412))
        );
        assert_eq!(opened(&bash("gh pr view 412"), out), None, "not a create");
        // **stroppy-io/stroppy#212, as it was created**: a push first, whose tracking line names
        // the repository's URL before the pull request's.
        let pushed = " * [new branch]      fix/x -> fix/x\nbranch 'fix/x' set up to track \
                      'https://github.com/stroppy-io/stroppy.git/fix/x'.\n\
                      https://github.com/stroppy-io/stroppy/pull/212\n";
        assert_eq!(
            opened(
                &bash("git push -u origin fix/x && gh pr create --fill"),
                pushed
            ),
            Some(("https://github.com/stroppy-io/stroppy/pull/212".into(), 212))
        );
        assert_eq!(
            opened(&bash("gh pr create --fill"), "error: no commits"),
            None
        );
        assert_eq!(
            opened(&bash("gh pr create"), "https://github.com/o/r/issues/3"),
            None,
            "an issue is not a pull request"
        );
    }

    #[test]
    fn the_watches_are_monitor_calls_and_the_model_is_told() {
        let calls = watch_calls("https://github.com/o/r/pull/7", 7);
        let names: Vec<&str> = calls.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "pr-7-checks",
                "pr-7-review",
                "pr-7-coderabbit",
                "pr-7-comment",
                "pr-7-closed"
            ]
        );
        let v: serde_json::Value = serde_json::from_str(&calls[0].1.arguments).unwrap();
        assert_eq!(v["github"]["until"], "checks");
        assert_eq!(v["repeat"], true);
        let said = told(
            "https://github.com/o/r/pull/7",
            &["pr-7-checks".into()],
            &[("pr-7-review".into(), "gh is not logged in".into())],
        );
        assert!(
            said.contains("is now watched for you") && said.contains("`pr-7-checks`"),
            "{said}"
        );
        assert!(
            said.contains("`pr-7-review` was not armed: gh is not logged in"),
            "{said}"
        );
    }
}
