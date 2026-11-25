//! The working guide this server hands a client at `initialize`: what a seat is, what the rooms
//! and the sessions expect of it, and where the contracts are. One embedded page, said once, so a
//! tool's description can be a single line.

/// The guide, as markdown. `{seat}` stands in for this process's room handle.
pub const GUIDE: &str = include_str!("guide.md");

/// The guide with this seat's handle in it. A plain replace, so no brace in the markdown has to be
/// escaped and a rule may be added to the page without touching a `format!`.
pub fn text(me: &str) -> String {
    GUIDE.replace("{seat}", me)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seat's handle is `cc@<folder>:<name>` out of the working folder, so a long worktree
    /// would otherwise decide whether a byte budget passes.
    const HANDLE: &str = "cc@host:seat";

    /// Every rule the tool descriptions used to carry, as a phrase the guide keeps it in. The
    /// descriptions are one line each now, so this list is what stops a rule being dropped on the
    /// way out rather than moved. Each entry is (what the rule is, the words it lives in).
    const RULES: &[(&str, &str)] = &[
        (
            "the seat id is keyed on the Claude Code session",
            "`CLAUDE_CODE_SESSION_ID` pins the seat's id",
        ),
        ("a nick is display only", "The label is for people to read"),
        (
            "a `to` takes an id, never a name",
            "a `to` takes an id, never a name or a nick",
        ),
        (
            "which rooms this seat sees",
            "you see `host`, your project's room, dms",
        ),
        (
            "no room is this seat's on its own",
            "No room is wider than those three",
        ),
        ("answer a dm in the dm", "Answer a dm in the dm"),
        (
            "`fyi` wakes only whom it names",
            "`fyi` wakes only whom it names",
        ),
        (
            "`ask` needs `to` and is held",
            "`ask` needs `to` and is held until it is answered",
        ),
        (
            "`blocker` rings the operator",
            "`blocker` rings and flags the operator",
        ),
        (
            "`decision` ranks in the digest",
            "`decision` ranks in the digest",
        ),
        (
            "`announce` is one line",
            "`announce` is one line for a whole room",
        ),
        (
            "a message over 4000 bytes is refused",
            "Over 4000 bytes is refused, never cut",
        ),
        (
            "a state claim without evidence is unbacked",
            "the claim reads `unbacked`",
        ),
        (
            "only room_answer and room_decline settle an ask",
            "`room_answer` and `room_decline` settle one",
        ),
        (
            "a chat reply settles nothing",
            "a chat reply settles nothing",
        ),
        (
            "only the seat asked may answer",
            "Only the seat asked may answer",
        ),
        (
            "an escalated ask is the operator's",
            "`escalated` means the operator is being asked",
        ),
        (
            "the inbox, the waiter and the channel share one cursor",
            "the waiter and the channel share that one cursor",
        ),
        ("a read moves no cursor", "`room_read` moves no cursor"),
        (
            "the queue seats are reached by dm",
            "`room_roster seats` is the box's queue seats",
        ),
        (
            "another project's queue seat takes only decision, ask or a thread",
            "takes only `decision`, `ask`, or a `thread` on the operator's message",
        ),
        (
            "a session is invited into a project room",
            "`room_invite` and `room_leave` are how a session",
        ),
        (
            "a nick is refused while a live seat answers to it",
            "It is refused while a live seat answers to it",
        ),
        (
            "a passing-by message is not for you",
            "A message `passing by` wants a reply only if you",
        ),
        ("the channel is the wake", "`<channel>` events"),
        (
            "the waiter is the wake without the channel",
            "letibot-mcp wait",
        ),
        (
            "what a waiter's exit means",
            "10 an ask or a rejection, 20 a failure or a gate, 30 the timeout, 40 the provider is stuck",
        ),
        (
            "a room message the channel dropped rides a result",
            "`room_unanswered`",
        ),
        (
            "the brief is the session's first prompt",
            "sends the brief as its first prompt",
        ),
        (
            "start answers the id every other tool takes",
            "answers the session id every other tool takes",
        ),
        (
            "the default approval point",
            "`mode` defaults to `writes-allowed`",
        ),
        (
            "allow-all needs the operator's confirmation",
            "`allow-all` needs the operator's confirmation",
        ),
        (
            "host runs the session's tools elsewhere",
            "`host` puts the session's tools on another machine",
        ),
        (
            "a placement never falls back to local",
            "a package must never end up running locally when its caller asked for a host",
        ),
        (
            "a package is a worktree on its own branch",
            "`pk/<short id>/<name>`",
        ),
        (
            "a watching head cannot act",
            "`leticode_send`, `leticode_answer` and `leticode_stop` refuse",
        ),
        (
            "a send queues behind the running turn",
            "queued behind a running turn",
        ),
        (
            "`queue` waits for the turn to end and opens its own",
            "waits for the running turn to end and opens a turn of its own",
        ),
        (
            "another project's session needs the operator's seq",
            "it needs `because`, the seq of the operator's message",
        ),
        (
            "a row is written under this seat's handle",
            "under this seat's handle and never as the operator's or the model's",
        ),
        (
            "a driven session's model is told about its new rows",
            "the model is told at its next turn",
        ),
        ("a cursor the ring outlived says dropped", "plus `dropped`"),
        ("the transcript caps long strings", "8000 characters"),
        ("the whole diff is asked for", "`full` adds the diff itself"),
        (
            "an answered permission is the agent's decision",
            "recorded as this agent's, never as the operator's",
        ),
        (
            "a question ask's answers",
            "`free:<words>` for a typed reply, or `abstain`",
        ),
        ("a stop settles the placement", "`remote/<host>/<short id>`"),
        (
            "a collect is for a close that never ran",
            "no close ever ran",
        ),
        (
            "an archived session comes back by being opened",
            "nothing is deleted, and it comes back by being opened",
        ),
        (
            "the list is this process's holdings",
            "what this process holds, not what the daemon has on disk",
        ),
        (
            "a model switch takes effect at the turn's end",
            "takes effect when the turn ends",
        ),
        (
            "a seat's job runs without a shell",
            "verbatim under no shell",
        ),
        (
            "a seat's job ends on the channel",
            "`job_ended` channel event",
        ),
        (
            "the queue lands the branch, not you",
            "Never run the workspace gate yourself",
        ),
        (
            "the queue's verbs are the operator's",
            "recorded under `bridge:<handle>`",
        ),
        (
            "holds answers counts unless asked for rows",
            "Counts per owner by default",
        ),
        (
            "gc never reaps a directory by pattern",
            "rather than reaped by pattern",
        ),
    ];

    /// The guide says every rule the tool descriptions used to state, in a sentence a client can
    /// act on. The phrases are matched with runs of whitespace flattened, so a rewrap of the page
    /// is not a lost rule.
    #[test]
    fn every_rule_the_descriptions_stated_is_in_the_guide() {
        let flat = text(HANDLE)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        for (rule, phrase) in RULES {
            assert!(flat.contains(phrase), "{rule}: no `{phrase}` in the guide");
        }
    }

    /// The daemon's own `room` tool states one standing rule, and this bridge states the same one
    /// word for word: two runtimes of one etiquette is how a seat learns two.
    #[test]
    fn the_standing_rule_is_the_daemons_own() {
        let flat = text(HANDLE)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let rule = letibot_tools::builtins::room::STANDING_RULE;
        assert!(flat.contains(rule), "the standing rule is not in the guide");
        assert_eq!(flat.matches(rule).count(), 1, "and it is said once");
    }

    /// The guide names the documents it defers to, so a client that wants the contract knows what
    /// to read. The list is the server's resources, and a stale uri here is a dead end there.
    #[test]
    fn the_guide_names_every_resource() {
        let said = text(HANDLE);
        for resource in crate::docs::RESOURCES {
            assert!(
                said.contains(resource.uri),
                "the guide does not name {}",
                resource.uri
            );
        }
    }

    /// A guide is prose and stays one: a sanity cap, so a pasted contract cannot turn every
    /// client's first frame into a megabyte, and no em dash, which is the operator's rule.
    #[test]
    fn the_guide_is_a_page_and_not_a_manual() {
        let said = text(HANDLE);
        assert!(said.len() <= 32 * 1024, "{} bytes", said.len());
        assert!(said.len() > 4000, "{} bytes is too thin", said.len());
        assert!(!said.contains('\u{2014}'), "no em dash");
    }
}
