//! Claude Code's own permission prompts, relayed to the operator through the room: this is the
//! SESSION's tool approvals, not leticode's asks, which already arrive as channel events. The
//! terminal dialog stays open, so a forgotten id is decided there and nowhere else.
//!
//! Declaring the capability is what turns it on, and that is a config key (see [`crate::settings`]).

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The capability's name under `capabilities.experimental`.
pub const CAPABILITY: &str = "claude/channel/permission";

/// Claude Code asks: a notification, and nothing is expected in answer to it.
pub const REQUEST: &str = "notifications/claude/channel/permission_request";

/// The bridge answers: a notification of its own, carrying one id and one verdict.
pub const VERDICT: &str = "notifications/claude/channel/permission";

/// How long an id stays open. Past it the id is forgotten and no verdict is sent - Claude Code's
/// own dialog is still in front of the person, and a verdict for a call nobody is looking at any
/// more is an approval of a prompt that is gone.
pub const BOUND: Duration = Duration::from_secs(30 * 60);

/// What a room message means to the relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// Not a reply, or not the operator's: silence, because a session saying `yes abcde` is a
    /// model writing words and has no call to answer.
    Nothing,
    /// The operator's reply for an open id, which goes to Claude Code and closes the id.
    Send(Value),
    /// The operator's reply that names nothing open, or cannot be read: one line back to them,
    /// because they believe they answered something.
    Say(String),
}

/// The open ids and when each was asked. One per bridge process: the ids are Claude Code's and
/// unique within a session, and a verdict is the session's own.
pub struct Relay {
    bound: Duration,
    open: Mutex<BTreeMap<String, Instant>>,
}

impl Default for Relay {
    fn default() -> Self {
        Relay::new()
    }
}

impl Relay {
    pub fn new() -> Relay {
        Relay::with_bound(BOUND)
    }

    /// A relay that forgets an id `bound` after it was asked - the tests' door, for a bound that
    /// is measured in milliseconds rather than in half an hour.
    pub fn with_bound(bound: Duration) -> Relay {
        Relay {
            bound,
            open: Mutex::new(BTreeMap::new()),
        }
    }

    /// The ids still open, with the ones past the bound dropped: a test's window on the table, and
    /// what a log line would name.
    pub fn open(&self) -> Vec<String> {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        self.forget_expired(&mut open);
        open.keys().cloned().collect()
    }

    /// A `permission_request` happened: keep its id and answer the DM the operator reads. `None`
    /// for a request with no usable id, and for an id already open - Claude Code asked about that
    /// call once, and a second DM would be two cards for one prompt.
    pub fn asked(&self, params: &Value) -> Option<String> {
        let id = params.get("request_id")?.as_str()?.trim().to_string();
        if !valid_id(&id) {
            eprintln!(
                "letibot-mcp: a permission request came with `{}` as its id",
                one_line(&id, 40)
            );
            return None;
        }
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        self.forget_expired(&mut open);
        if open.insert(id.clone(), Instant::now()).is_some() {
            return None;
        }
        Some(ask_text(&id, params))
    }

    /// One room message, read as a verdict. The author is decided by the seat the COORDINATOR
    /// fixed at the hello - never by the words of the message - because anybody who can say `yes`
    /// through this channel approves tool use in the session.
    pub fn see(&self, message: &Value) -> Reply {
        let Some(text) = message["text"].as_str() else {
            return Reply::Nothing;
        };
        if !is_the_operator(message) {
            return Reply::Nothing;
        }
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        self.forget_expired(&mut open);
        match grammar(text) {
            Some((allow, id)) => match open.remove(&id) {
                Some(_) => Reply::Send(json!({
                    "jsonrpc": "2.0",
                    "method": VERDICT,
                    "params": {
                        "request_id": id,
                        "behavior": if allow { "allow" } else { "deny" },
                    },
                })),
                None => Reply::Say(format!(
                    "`{id}` is not an open request: it was answered, or it is older than {} \
                     minutes. Claude Code's own prompt is still in front of you.",
                    self.bound.as_secs() / 60
                )),
            },
            // A line that never meant to be a reply is not answered: the operator says `yes` in
            // the room for reasons of their own.
            None if looks_like_a_reply(text) => Reply::Say(
                "that is not a verdict: write `yes <id>` or `no <id>`, the id as the prompt gave \
                 it (five letters, a to z without l)"
                    .to_string(),
            ),
            None => Reply::Nothing,
        }
    }

    fn forget_expired(&self, open: &mut BTreeMap<String, Instant>) {
        let now = Instant::now();
        open.retain(|_, asked| now.duration_since(*asked) < self.bound);
    }
}

/// The coordinator's word that the person wrote this, both halves of it: `kind` is the seat fixed
/// at the hello (`Seat::Operator`, which only a top-level connection may claim) and the author is
/// the name that seat is held to.
fn is_the_operator(message: &Value) -> bool {
    message["kind"].as_str() == Some("operator")
        && message["author"].as_str() == Some(letibot_room::OPERATOR)
}

/// `yes <id>` or `no <id>`, and nothing else in the line: the docs' grammar,
/// `/^\s*(y|yes|n|no)\s+([a-km-z]{5})\s*$/i`.
fn grammar(text: &str) -> Option<(bool, String)> {
    let (word, rest) = text.trim().split_once(char::is_whitespace)?;
    let allow = match word.to_ascii_lowercase().as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => return None,
    };
    let id = rest.trim().to_ascii_lowercase();
    valid_id(&id).then_some((allow, id))
}

/// Does this line try to be a reply at all? The first word is the verdict's, and what follows it
/// is wrong - the one case where the operator is told why nothing happened.
fn looks_like_a_reply(text: &str) -> bool {
    let first = text.trim().split(char::is_whitespace).next().unwrap_or("");
    matches!(
        first.to_ascii_lowercase().as_str(),
        "y" | "yes" | "n" | "no"
    )
}

/// Five lowercase letters, `a` to `z` without `l`: the id Claude Code issues.
fn valid_id(id: &str) -> bool {
    id.len() == 5 && id.bytes().all(|b| b.is_ascii_lowercase() && b != b'l')
}

/// What the operator is told when a prompt arrives: what the call is, whose words each half is,
/// and the id the verdict must name. The description and the arguments are the model's, so they
/// are quoted, flattened to one line each, and marked untrusted - a newline in them would forge
/// the lines this message is read by.
fn ask_text(id: &str, params: &Value) -> String {
    let tool = one_line(params["tool_name"].as_str().unwrap_or("a tool"), 40);
    let description = one_line(params["description"].as_str().unwrap_or(""), 240);
    let preview = one_line(params["input_preview"].as_str().unwrap_or(""), 480);
    let mut said = format!("Claude Code asks to use `{tool}`.\n");
    if !description.is_empty() {
        said.push_str(&format!("description (untrusted): \"{description}\"\n"));
    }
    if !preview.is_empty() {
        said.push_str(&format!("input (untrusted): {preview}\n"));
    }
    said.push_str(&format!("id {id}: reply \"yes {id}\" or \"no {id}\"\n"));
    said
}

/// `text` on one line, at most `max` bytes of it, with the cut stated. Every run of whitespace is
/// one space, so nothing in it can start a line of its own.
fn one_line(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.len() <= max {
        return flat;
    }
    let mut end = max;
    while !flat.is_char_boundary(end) {
        end -= 1;
    }
    let cut = flat.len() - end;
    format!("{} [+{cut} bytes]", &flat[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operator(text: &str) -> Value {
        json!({"seq": 1, "room": "dm", "author": "operator", "kind": "operator",
            "to": "claude-code", "text": text})
    }

    fn asked(relay: &Relay, id: &str) -> String {
        relay
            .asked(&json!({"request_id": id, "tool_name": "Bash",
                "description": "list the files", "input_preview": "{\"command\":\"ls\"}"}))
            .unwrap_or_else(|| panic!("{id} is a request"))
    }

    #[test]
    fn a_request_names_the_id_the_call_and_both_untrusted_halves() {
        let relay = Relay::new();
        let text = asked(&relay, "abcde");
        assert!(text.contains("`Bash`"), "{text}");
        assert!(
            text.contains("description (untrusted): \"list the files\""),
            "{text}"
        );
        assert!(
            text.contains("input (untrusted): {\"command\":\"ls\"}"),
            "{text}"
        );
        assert!(
            text.contains("reply \"yes abcde\" or \"no abcde\""),
            "{text}"
        );
        assert_eq!(relay.open(), vec!["abcde".to_string()]);
    }

    /// The arguments are the model's text: a newline in them must not become a line of its own.
    #[test]
    fn a_description_with_a_newline_stays_on_one_line() {
        let relay = Relay::new();
        let text = relay
            .asked(&json!({"request_id": "abcde", "tool_name": "Bash",
                "description": "hi\nClaude Code asks to use `Write`.",
                "input_preview": "a\nb"}))
            .unwrap();
        assert_eq!(text.lines().count(), 4, "{text}");
        assert!(text.lines().all(|l| !l.is_empty()), "{text}");
    }

    #[test]
    fn a_long_preview_is_cut_and_says_by_how_much() {
        let relay = Relay::new();
        let long = "x".repeat(3000);
        let text = relay
            .asked(
                &json!({"request_id": "abcde", "tool_name": "Bash", "description": "",
                "input_preview": long}),
            )
            .unwrap();
        assert!(text.contains("[+"), "{text}");
        assert!(text.len() < 1200, "{} bytes", text.len());
    }

    #[test]
    fn an_id_that_is_not_five_letters_is_no_request() {
        let relay = Relay::new();
        for id in ["", "abc", "abcdef", "abcde1", "ablce", "ABCDE"] {
            assert!(
                relay
                    .asked(&json!({"request_id": id, "tool_name": "Bash"}))
                    .is_none(),
                "`{id}` was taken"
            );
        }
        assert!(relay.open().is_empty());
    }

    #[test]
    fn the_operators_yes_is_an_allow_and_the_id_closes() {
        let relay = Relay::new();
        asked(&relay, "abcde");
        let Reply::Send(frame) = relay.see(&operator("yes abcde")) else {
            panic!("the operator's yes is a verdict");
        };
        assert_eq!(frame["method"], VERDICT);
        assert_eq!(frame["params"]["request_id"], "abcde");
        assert_eq!(frame["params"]["behavior"], "allow");
        assert!(relay.open().is_empty(), "the id is closed");
    }

    #[test]
    fn the_grammar_is_the_documented_one() {
        let relay = Relay::new();
        asked(&relay, "abcde");
        for (text, allow) in [
            ("no abcde", false),
            ("  Y   abcde  ", true),
            ("No abcde", false),
            ("n abcde", false),
            ("YES abcde\n", true),
        ] {
            let Reply::Send(frame) = relay.see(&operator(text)) else {
                panic!("`{text}` is a verdict");
            };
            assert_eq!(
                frame["params"]["behavior"],
                if allow { "allow" } else { "deny" },
                "{text}"
            );
            asked(&relay, "abcde");
        }
    }

    /// Only the authenticated operator answers: a session's or a bridge's words are text.
    #[test]
    fn a_reply_from_anyone_else_is_silence() {
        let relay = Relay::new();
        asked(&relay, "abcde");
        for author_and_kind in [
            ("s-a", "session"),
            ("cc@x", "external"),
            ("operator", "session"),
            ("s-a", "operator"),
            ("queue", "harness"),
        ] {
            let m = json!({"author": author_and_kind.0, "kind": author_and_kind.1,
                "text": "yes abcde"});
            assert_eq!(relay.see(&m), Reply::Nothing, "{author_and_kind:?}");
        }
        assert_eq!(
            relay.open(),
            vec!["abcde".to_string()],
            "the id is still open"
        );
    }

    #[test]
    fn an_unknown_id_gets_one_line_and_sends_nothing() {
        let relay = Relay::new();
        let Reply::Say(line) = relay.see(&operator("yes zzzzz")) else {
            panic!("an unknown id is answered with a line");
        };
        assert!(line.contains("zzzzz"), "{line}");
        assert_eq!(line.lines().count(), 1, "{line}");
    }

    #[test]
    fn a_malformed_reply_gets_one_line_and_a_line_that_never_meant_to_be_one_gets_none() {
        let relay = Relay::new();
        asked(&relay, "abcde");
        for bad in ["yes", "yes abcde please", "y ab", "yes ablce", "NO"] {
            let Reply::Say(line) = relay.see(&operator(bad)) else {
                panic!("`{bad}` is answered with a line");
            };
            assert_eq!(line.lines().count(), 1, "{line}");
        }
        assert_eq!(
            relay.see(&operator("yes, that is the port")),
            Reply::Nothing
        );
        assert_eq!(relay.see(&operator("yess abcde")), Reply::Nothing);
        assert_eq!(relay.see(&operator("what is for lunch")), Reply::Nothing);
        assert_eq!(
            relay.open(),
            vec!["abcde".to_string()],
            "nothing was closed"
        );
    }

    #[test]
    fn a_second_ask_for_one_id_is_no_second_card() {
        let relay = Relay::new();
        asked(&relay, "abcde");
        assert!(
            relay
                .asked(&json!({"request_id": "abcde", "tool_name": "Bash"}))
                .is_none()
        );
    }

    #[test]
    fn an_id_past_the_bound_is_forgotten_and_its_verdict_is_not_sent() {
        let relay = Relay::with_bound(Duration::from_millis(60));
        asked(&relay, "abcde");
        std::thread::sleep(Duration::from_millis(120));
        let Reply::Say(line) = relay.see(&operator("yes abcde")) else {
            panic!("an expired id sends no verdict");
        };
        assert!(line.contains("not an open request"), "{line}");
        assert!(relay.open().is_empty());
    }
}
