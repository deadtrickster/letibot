//! The daemon's half of the smart `!`: the prompt it builds and the reply it parses.
//!
//! The operator's ask, in their words: *"i want smart ! when a model suggest
//! completions."* The head's history completion is the first answer — the commands this
//! session has actually run, which are on the screen. When the history has no match for
//! the prefix, the head asks the daemon, and the daemon builds a prompt from the
//! conversation and asks the LOCAL model. This module is the two pure halves of that:
//! the prompt (what is asked) and the parse (what is believed of the answer). The model
//! call itself is the daemon's (`harnessd`), because this crate holds no HTTP client and
//! no endpoint — the same reason the oracle's layer B lives there.
//!
//! # Why the prompt is built, not templated
//!
//! A template that says *"suggest a command for this prefix"* with nothing else in it
//! would invent a command from the prefix alone — `! git` becomes `! git push` whether or
//! not the session has a repository. The prompt carries the conversation instead: the
//! last handful of rows condensed, the commands already run, the workspace path, and the
//! prefix. The model then suggests what FITS this session, and the one rule it is told is
//! that **a wrong suggestion is worse than none** — so it may answer with nothing, and an
//! empty answer is a good answer, not a failure.

use letibot_transcript::{TranscriptItem, UserPart};

/// How many of the session's own rows the prompt carries, newest last.
///
/// A handful, not a window: the prompt is a brief for a small local model with a small
/// output cap, and the rows are context, not the conversation. Eight is enough to say
/// what the session is about and small enough that the prompt stays a paragraph.
pub const RECENT_ROWS: usize = 8;

/// How many characters of one condensed row the prompt keeps.
///
/// A row is context, and a `read` row can be 418 KB — carrying it whole would make the
/// prompt the row and the row the prompt. Two hundred characters says what the row was
/// about and nothing it would not say in a sentence.
const ROW_CAP: usize = 200;

/// **The prompt the daemon asks the local model.**
///
/// Built from the conversation, not from a template: the last handful of rows condensed
/// (`recent`), the commands already run in this session (`commands`), the workspace path
/// (`workspace`), and the typed prefix (`prefix`). It asks for up to five shell lines,
/// one per line, no fences, no prose — and it says that **a wrong suggestion is worse
/// than none**, which is the rule that makes an empty answer acceptable rather than a
/// defect.
///
/// The prefix travels `!` first, the same spelling the history completion matches, so
/// the model is asked to complete the line the operator is typing rather than to invent
/// a command from a word.
pub fn suggestion_prompt(
    recent: &[String],
    commands: &[String],
    workspace: &str,
    prefix: &str,
) -> String {
    let mut p = String::new();
    p.push_str("You are suggesting shell commands for an operator working in ");
    p.push_str(workspace);
    p.push_str(".\n\n");
    if !recent.is_empty() {
        p.push_str("The conversation so far, condensed:\n");
        for row in recent {
            p.push_str("- ");
            p.push_str(row);
            p.push('\n');
        }
        p.push('\n');
    }
    if !commands.is_empty() {
        p.push_str("Commands already run in this session:\n");
        for c in commands {
            p.push_str("- ");
            p.push_str(c);
            p.push('\n');
        }
        p.push('\n');
    }
    p.push_str("The operator has typed this line and is asking for completions: ");
    p.push_str(prefix);
    p.push_str("\n\n");
    p.push_str(
        "Suggest up to five shell commands that complete this line, one per line, each \
         starting with `! `. No code fences, no prose, no explanations — the lines and \
         nothing else. If you are not confident a command is right for this session, say \
         nothing: a wrong suggestion is worse than none.",
    );
    p
}

/// **The reply, read as candidate lines — defensively.**
///
/// The model is told to answer with lines and nothing else, and a small local model told
/// that will mostly do so. But the parse must not TRUST the telling, because a reply that
/// slips past it becomes a candidate the operator might run:
///
/// - **Markdown fences are stripped.** A model that wraps the lines in ` ```bash ` is
///   answering the question and decorating the answer; the fences are the decoration.
/// - **Prose lines are dropped.** A line that is not a `!` line is the model talking, not
///   suggesting — *"Here are some commands:"* is not a command, and offering it as one is
///   the same class of lie as an unattributed quote.
/// - **Duplicates are dropped**, keeping the first, so a model that says a command twice
///   does not offer it twice.
/// - **Empty lines are dropped**, because an empty candidate is nothing to complete to.
///
/// The result is at most five lines, `!` first, in the order the model offered them.
pub fn parse_suggestions(reply: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in reply.lines() {
        let line = raw.trim();
        // A fence line, opening or closing, or a fence-tagged line: not a command.
        if line.starts_with("```") {
            continue;
        }
        // A candidate is a `!` line and nothing else. Prose, a bare command without the
        // bang, and an empty line all fall out here.
        let Some(cmd) = crate::protocol::operator_shell_command(line) else {
            continue;
        };
        let candidate = format!("! {cmd}");
        if out.len() >= 5 {
            break;
        }
        if seen.insert(candidate.clone()) {
            out.push(candidate);
        }
    }
    out
}

/// **The last handful of the session's rows, condensed to one line each.**
///
/// The prompt's context half. Each row is read the way a reader would read it — the
/// operator's words, the model's prose, the tool that ran — and cut to [`ROW_CAP`]
/// characters, so a row that is a megabyte of file contents becomes a sentence about
/// what it was. Reasoning rows are skipped: a draft the model abandoned is not
/// conversation, and replaying it into a prompt is how the model reads its own dead
/// ends as history.
///
/// Returned oldest-first, so the prompt reads the conversation in the order it happened.
pub fn recent_rows(items: &[TranscriptItem], n: usize) -> Vec<String> {
    let tail: Vec<String> = items
        .iter()
        .rev()
        .take(n)
        .map(condense_row)
        .filter(|l| !l.is_empty())
        .collect();
    tail.into_iter().rev().collect()
}

/// One row, read as a line of context.
fn condense_row(item: &TranscriptItem) -> String {
    let text = match item {
        TranscriptItem::User { parts, .. } => parts
            .iter()
            .filter_map(|p| match p {
                UserPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        TranscriptItem::Assistant {
            text, tool_calls, ..
        } => {
            let mut s = text.clone();
            if !tool_calls.is_empty() {
                let names: Vec<&str> = tool_calls.iter().map(|c| c.name.as_str()).collect();
                let clause = format!("[calls {}]", names.join(", "));
                if s.is_empty() {
                    s = clause;
                } else {
                    s.push(' ');
                    s.push_str(&clause);
                }
            }
            s
        }
        TranscriptItem::ToolResult { name, .. } => format!("[tool {name}]"),
        TranscriptItem::System { text, .. } => text.clone(),
        // A draft and a mark are not conversation: the one is abandoned and the other
        // is a pointer, and neither is context a suggestion should be built on.
        TranscriptItem::Reasoning { .. } | TranscriptItem::SegmentMark { .. } => String::new(),
    };
    cut(&text)
}

/// Cut a line to [`ROW_CAP`] characters, marking the cut so the reader knows it is one.
fn cut(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= ROW_CAP {
        return s.to_string();
    }
    let kept: String = chars[..ROW_CAP].iter().collect();
    format!("{kept}…")
}

/// **The commands this session has already run**, newest first, deduped.
///
/// The prompt's second context half, and the same walk the head's history completion
/// makes over its own rows: the operator's own `!` rows verbatim, and the model's `bash`
/// calls as `! ` plus the command they ran. Carrying them in the prompt is what tells the
/// model what is already known — a command the session ran a moment ago is not a
/// suggestion, it is a fact.
///
/// A `bash` call whose arguments do not parse, or that carries no `command`, is skipped:
/// a command that cannot be re-run is not a command the session ran.
pub fn commands_run(items: &[TranscriptItem]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items.iter().rev() {
        match item {
            TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Operator,
                parts,
                ..
            } => {
                let text = parts
                    .iter()
                    .filter_map(|p| match p {
                        UserPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                if text.starts_with('!') {
                    out.push(text);
                }
            }
            TranscriptItem::Assistant { tool_calls, .. } => {
                for c in tool_calls {
                    if c.name != "bash" {
                        continue;
                    }
                    let Ok(v) = serde_json::from_str::<serde_json::Value>(&c.arguments) else {
                        continue;
                    };
                    let Some(cmd) = v.get("command").and_then(|c| c.as_str()) else {
                        continue;
                    };
                    out.push(format!("! {cmd}"));
                }
            }
            _ => {}
        }
    }
    // Dedupe keeping the newest (first) occurrence.
    let mut seen = std::collections::HashSet::new();
    out.retain(|line| seen.insert(line.clone()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> TranscriptItem {
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Operator,
            parts: vec![UserPart::Text { text: text.into() }],
        }
    }

    fn assistant_bash(cmd: &str) -> TranscriptItem {
        TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: vec![letibot_transcript::ToolCall {
                id: "c1".into(),
                name: "bash".into(),
                arguments: format!(r#"{{"command": {cmd:?}}}"#),
            }],
            truncated: false,
        }
    }

    /// **The prompt carries the context and the prefix, and says the rule.**
    ///
    /// The whole of the feature's honesty is in this: the model is asked about THIS
    /// session (the workspace, the rows, the commands already run) and THIS prefix, and
    /// it is told that a wrong suggestion is worse than none. A prompt that dropped any
    /// of those would be a template, and a template invents.
    #[test]
    fn the_prompt_carries_the_context_and_the_prefix() {
        let recent = vec!["fix the login bug".to_string(), "[tool bash]".to_string()];
        let commands = vec!["! cargo test".to_string(), "! git status".to_string()];
        let p = suggestion_prompt(&recent, &commands, "/home/dead/Projects/letibot", "! git");
        // The workspace, the rows, the commands and the prefix are all in it.
        assert!(
            p.contains("/home/dead/Projects/letibot"),
            "the workspace: {p}"
        );
        assert!(p.contains("fix the login bug"), "a recent row: {p}");
        assert!(p.contains("[tool bash]"), "a recent tool row: {p}");
        assert!(p.contains("! cargo test"), "a command already run: {p}");
        assert!(p.contains("! git status"), "a command already run: {p}");
        assert!(p.contains("! git"), "the typed prefix: {p}");
        // The shape of the answer it asks for.
        assert!(p.contains("up to five"), "the cap: {p}");
        assert!(p.contains("one per line"), "one per line: {p}");
        assert!(p.contains("No code fences"), "no fences: {p}");
        // The rule that makes an empty answer acceptable.
        assert!(
            p.contains("a wrong suggestion is worse than none"),
            "the rule: {p}"
        );
    }

    /// **An empty context is still a prompt, and it says so rather than lying.**
    ///
    /// A fresh session has no rows and no commands, and the prompt must not carry an
    /// empty section that reads as if the session had something and the model missed it.
    #[test]
    fn an_empty_context_is_a_prompt_without_the_sections() {
        let p = suggestion_prompt(&[], &[], "/tmp/empty", "! ls");
        assert!(
            !p.contains("The conversation so far"),
            "no rows section: {p}"
        );
        assert!(
            !p.contains("Commands already run"),
            "no commands section: {p}"
        );
        assert!(p.contains("/tmp/empty"), "the workspace: {p}");
        assert!(p.contains("! ls"), "the prefix: {p}");
    }

    /// **The parse strips fences and prose, dedupes, and drops the empty.**
    ///
    /// A reply the model was told to keep to lines will mostly be lines, and the parse
    /// must take the lines and refuse the rest: a fence is decoration, a prose line is
    /// the model talking, a duplicate is said twice, and an empty line is nothing.
    #[test]
    fn the_parse_strips_fences_and_prose() {
        let reply = "\
```bash
! git status
! git log
Here are some commands:
! git status

! git diff
```
";
        let lines = parse_suggestions(reply);
        assert_eq!(
            lines,
            vec![
                "! git status".to_string(),
                "! git log".to_string(),
                "! git diff".to_string(),
            ],
            "fences, prose, the duplicate and the blank are all gone: {lines:?}"
        );
    }

    /// **A reply that is all prose is nothing, and nothing is the answer.**
    ///
    /// The model said something and suggested nothing. That is the "a wrong suggestion
    /// is worse than none" rule working: the parse drops the prose and the daemon
    /// answers with an empty list, which the head reads as *no suggestion* rather than
    /// waiting on one that is not coming.
    #[test]
    fn a_prose_only_reply_is_nothing() {
        let lines = parse_suggestions("I'm not sure what you need here.\nLet me know more.");
        assert!(lines.is_empty(), "prose is not a suggestion: {lines:?}");
    }

    /// **The parse keeps at most five, in the order offered.**
    #[test]
    fn the_parse_keeps_at_most_five_in_order() {
        let reply: String = (1..=7)
            .map(|i| format!("! cmd{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = parse_suggestions(&reply);
        assert_eq!(
            lines,
            vec![
                "! cmd1".to_string(),
                "! cmd2".to_string(),
                "! cmd3".to_string(),
                "! cmd4".to_string(),
                "! cmd5".to_string(),
            ],
            "five, in order: {lines:?}"
        );
    }

    /// **A line the prompt did not ask for is not a candidate.**
    ///
    /// The two that look most like candidates are the ones worth pinning, because both
    /// are things a model told to answer with `! ` lines writes anyway: **a bare command
    /// without the sigil** — which the parse drops rather than prefixing, because
    /// `!`-first is the one recogniser for *this is a command* everywhere else in the
    /// tree and a parser that guessed would be a second one — and **a bang with nothing
    /// after it**, which is not a command here any more than it is at the send. The one
    /// line that IS a candidate comes through, and a comment inside a fence does not.
    #[test]
    fn a_line_the_prompt_did_not_ask_for_is_not_a_candidate() {
        let reply = "```bash\nls -la\ngit status\n# run the tests\n! \n!   \n! cargo test\n```";
        assert_eq!(
            parse_suggestions(reply),
            vec!["! cargo test".to_string()],
            "a bare command, a comment and a bare bang are all not candidates"
        );
    }

    /// **A `bash` call that cannot be re-run is not a command the session ran.**
    ///
    /// The prompt's second context half is what tells the model which commands are already
    /// facts, so a line it cannot stand behind would be a fact nobody ran: unparseable
    /// arguments, no `command` at all, and a call that is not `bash` are all skipped.
    #[test]
    fn a_bash_call_that_cannot_be_re_run_is_not_a_command_the_session_ran() {
        let items = vec![
            TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![
                    letibot_transcript::ToolCall {
                        id: "c1".into(),
                        name: "bash".into(),
                        arguments: "not json at all".into(),
                    },
                    letibot_transcript::ToolCall {
                        id: "c2".into(),
                        name: "bash".into(),
                        arguments: r#"{"cwd": "/tmp"}"#.into(),
                    },
                    letibot_transcript::ToolCall {
                        id: "c3".into(),
                        name: "read".into(),
                        arguments: r#"{"command": "cat x"}"#.into(),
                    },
                ],
                truncated: false,
            },
            assistant_bash("cargo test"),
        ];
        assert_eq!(
            commands_run(&items),
            vec!["! cargo test".to_string()],
            "only the call that is a command"
        );
    }

    /// **The rows are condensed, newest last, and a long row is cut.**
    #[test]
    fn the_rows_are_condensed_and_cut() {
        let long = "x".repeat(ROW_CAP + 50);
        let items = vec![user("first"), user(&long), assistant_bash("cargo test")];
        let rows = recent_rows(&items, 8);
        assert_eq!(rows.len(), 3, "every row is carried: {rows:?}");
        assert_eq!(rows[0], "first", "oldest first: {rows:?}");
        assert!(rows[1].ends_with('…'), "the long row is cut: {rows:?}");
        assert!(
            rows[1].chars().count() <= ROW_CAP + 1,
            "the cut is bounded: {rows:?}"
        );
        assert_eq!(rows[2], "[calls bash]", "a call row is named: {rows:?}");
    }

    /// **The commands already run are the operator's `!` rows and the model's `bash`
    /// calls, newest first, deduped.**
    #[test]
    fn the_commands_already_run_are_walked_newest_first() {
        let items = vec![
            user("! ls -la"),
            assistant_bash("git status"),
            user("! ls -la"),
        ];
        let cmds = commands_run(&items);
        assert_eq!(
            cmds,
            vec!["! ls -la".to_string(), "! git status".to_string()],
            "newest first, deduped: {cmds:?}"
        );
    }
}
