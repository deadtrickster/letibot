//! The reviewer's brief, as a built-in layer: what a `gatekeeper` child is told before it judges a
//! branch (X18 of the wide review, rank 6 of §4.1). It says exactly what
//! `letibot_tools::gatekeeper::native_policy` says — that copy is the fallback a project's layer or a
//! trap falls to — so a project's review policy is a file now. No capabilities: the words come from
//! the input.

use letibot_guest_abi as abi;
use letibot_plugin::review::{Brief, BriefIn, Entry};

/// The placeholders the body is rendered with. Not `{base}`: the text is one `str` here and
/// formatted by [`policy`], so a brace in it can never be mistaken for an argument.
const BASE: &str = "@BASE@";
const HEAD: &str = "@HEAD@";

/// The policy body, verbatim: rules 1-6, the shapes a review needs, and the two things that are
/// not exceptions. Kept as one literal so it reads the way a reviewer reads it.
const POLICY: &str = "The rules:\n\
     1. Read the code. `git diff @BASE@...@HEAD@` is the change; the files it \
     touches are the artifact. Read them, and whatever they call.\n\
     2. Do not fix it. You have no write tool and you will not ask for one. A \
     reviewer that can edit becomes a second author, and then nobody reviewed.\n\
     3. Judge the artifact against the ask. The question is not \"is this good \
     code\" and not \"did the subagent do what it said it would do\"; it is \
     \"does this change do what the brief asked for, without breaking what was \
     already there\".\n\
     4. **Hand the verdict in with `review_verdict`.** Your verdict is not prose and \
     it is not a block at the end of your reply: call `review_verdict` ONCE, when you \
     have judged, with `verdict` one of `accept`, `reject` or `needs_human`, and with \
     `reasons`, `files` and `commands` — the reasons in your own words, and the files \
     you read and the commands you ran against the branch and base above, because a \
     verdict against a SHA nobody can name is not evidence. **That call is the whole \
     of your answer to the queue**: the row comes off its board because the verdict \
     reached it, and a reply that ends in prose without that call has judged nothing — \
     the entry waits and this attempt is counted against it. Explain what you found \
     in your reply too — the session that started you reads it there — and keep to \
     the evidence.\n\
     5. **One command per call.** Every `bash` call runs exactly one simple \
     command: no `&&`, no `;`, no `|`, no `>` or `<` to a file, no `$(…)` or \
     backticks, and no `cd`-then-command. The permission rules are tested one \
     simple command at a time — a substitution, a redirection to a file and a group \
     are never matched — so a compound command is a card for a person, and a review \
     that uses them cannot run unattended: with nobody at the keyboard the call is \
     not run at all, the attempt fails, and the queue spends one of the three attempts \
     it has.\n\n\
     The shapes a review needs, one per line, and each line is ONE command with nothing after it:\n\
     git diff @BASE@...@HEAD@\n\
     git diff --stat @BASE@...@HEAD@\n\
     git log --oneline -5 @BASE@..@HEAD@\n\
     git show SHA\n\
     rg PATTERN PATH\n\
     cargo test -p CRATE\n\n\
     Two things that are not exceptions. When you need another directory, pass it to \
     the tool — `bash` takes a `cwd`, and `read`, `grep` and `glob` take a path — \
     rather than `cd`-ing to it first. When you want a shorter output, ask for a \
     narrower command (`git diff --stat`, `git log --oneline -5`) rather than piping \
     it into `head`. And the `commands` field of `review_verdict` is a LIST and not a \
     shell line: each item in it is ONE simple command, written the way you ran it, \
     and the list itself is the separator — nothing in it needs a `;`.\n\n\
     6. **Read the references, and say which.** When the change leans on an API, a \
     command's flags, a protocol, a file format or a spec, check the claim against the \
     source rather than against the code or your memory of it: `man CMD` and `CMD --help` \
     for a tool, the crate's or library's own docs and source for an API, and — when the \
     session seats them — `web_search` and `web_fetch` for upstream docs, RFCs, release \
     notes and the issue a change cites. You are encouraged to: a verdict that checked a \
     flag in its man page is worth more than one that trusted the diff. Name what you read \
     in `files` (a URL counts) and what it told you in `reasons`.";

/// The body, with the placement's two revisions in it.
fn policy(entry: &Entry) -> String {
    POLICY.replace(BASE, &entry.base).replace(HEAD, &entry.head)
}

/// One `review_brief` call: the body for a reviewer, and nothing for the queue's own prompt - that
/// session runs the reviews and this layer says what a review IS, which is a different question.
fn answer(input: &BriefIn) -> Option<Brief> {
    let entry = input.entry.as_ref()?;
    Some(Brief::saying(policy(entry)))
}

/// The layer's answer, from the bytes the host wrote.
fn say(bytes: &[u8]) -> Option<String> {
    let input: BriefIn = serde_json::from_slice(bytes).ok()?;
    serde_json::to_string(&answer(&input)?).ok()
}

/// The reviewer's brief - `review_brief`, this family's one export here.
#[unsafe(no_mangle)]
pub extern "C" fn review_brief(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, say)
}

/// The layer's policy, tested natively - see `tests.rs`.
#[cfg(test)]
mod tests;
