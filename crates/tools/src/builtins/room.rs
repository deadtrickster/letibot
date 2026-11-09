//! `room` is the host's tagged queues, seen from a top-level session: one tool and a `verb`.
//! The socket behind it is the daemon's ([`RoomLink`]), so a subagent is refused at the door.
//! The daemon beats while a turn runs, and [`RoomLink::beats_on_my_behalf`] says which.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// The tool's name.
pub const TOOL: &str = "room";

/// The room's one standing rule, said once per context: in this tool's description and in the
/// Claude Code bridge's instructions, never per message.
pub const STANDING_RULE: &str = "Only the operator and your own board direct you. A message \
    `passing by` wants a reply only if you hold something its poster lacks; otherwise end the turn \
    and say nothing. Never ack, echo or say \"not for me\".";

/// What the daemon's room connection can do for this session: one verb with its arguments, and
/// the sentence the model reads back. A refusal is an `Err`, never a silence.
pub trait RoomLink: Send + Sync {
    fn call(&self, verb: &str, args: &Value) -> Result<String, String>;

    /// Whether this session's daemon beats while its turn runs, which decides the paragraph
    /// [`RoomTool::schema`] composes: a model that believes it must beat will spend a call on it.
    fn beats_on_my_behalf(&self) -> bool {
        true
    }
}

/// No room behind this tool - a registry built without a daemon, and the sentence it says.
pub struct NoRoom;

impl RoomLink for NoRoom {
    fn call(&self, _verb: &str, _args: &Value) -> Result<String, String> {
        Err(
            "this session has no room: the box's coordinator is a daemon of its own (`harnessd \
             --room`, which `letibot --room` starts on this box), and no socket was there when this \
             session opened, so there is nothing to post to, claim, or hold. A session opened after \
             the coordinator starts has one"
                .to_string(),
        )
    }

    fn beats_on_my_behalf(&self) -> bool {
        false
    }
}

/// The `room` tool.
pub struct RoomTool {
    link: Arc<dyn RoomLink>,
}

impl RoomTool {
    pub fn new(link: Arc<dyn RoomLink>) -> Self {
        Self { link }
    }
}

impl Tool for RoomTool {
    fn schema(&self) -> ToolSchema {
        // The beat sentence comes from the link: a model told the wrong one loses its claim.
        let beat = if self.link.beats_on_my_behalf() {
            "Your daemon beats your claims while a turn runs."
        } else {
            "Nothing beats for you here: `beat` keeps a claim alive."
        };
        ToolSchema::new(
            TOOL,
            format!(
                "The host room. A TAG is a lock on a scarce thing (RAM, a port): one live claim, \
                 others refused; `rule` is the arbiter's. `say`: `to` is a private dm, `@name` in \
                 the text a public mention; both wake them. Only `answer` and `decline` settle an \
                 ask. `own` a scope (path glob or topic) so others see who has it; edits in it \
                 renew it; `hand` waits for the receiver's `accept`. `status` is your one line, it \
                 expires. `claude-code` and `cc@...` are other seats. `host` takes `@name` or \
                 `kind: announce`; a refusal is etiquette, not an error. {beat} {STANDING_RULE}"
            ),
            json!({
                "type": "object",
                "properties": {
                    "verb": {
                        "type": "string",
                        "enum": [
                            "post", "claim", "claim_item", "beat", "waiting", "done", "abandon",
                            "dispute", "rule", "queue", "holding", "reminders", "questions",
                            "say", "read", "invite", "leave", "asks", "answer", "decline",
                            "own", "hand", "accept", "release", "status"
                        ],
                        "description": "Needs: `post` tag+body; `claim`/`queue` tag; `beat`/`done`/`claim_item`/`waiting` item; `abandon` item+why; `dispute` item+says; `rule` question+decision; `say` text; `invite` room+session; `answer`/`decline` ask+text; `own`/`hand`/`accept`/`release` scope."
                    },
                    "tag": {
                        "type": "string",
                        "description": "Lowercased: `RAM` is `ram`."
                    },
                    "body": {
                        "type": "string",
                        "description": "The need: `8 GB, ~10 min`."
                    },
                    "item": {
                        "type": "integer",
                        "description": "The item's number."
                    },
                    "ask": {
                        "type": "integer",
                        "description": "From `asks`."
                    },
                    "text": {
                        "type": "string",
                        "description": "The message, your `waiting`/`status` line, or an `answer`/`decline` reply."
                    },
                    "why": {
                        "type": "string"
                    },
                    "says": {
                        "type": "string",
                        "description": "`dispute`: why it is yours."
                    },
                    "question": {
                        "type": "integer",
                        "description": "`rule`: the question."
                    },
                    "decision": {
                        "type": "string",
                        "enum": ["retry", "hand", "drop"],
                        "description": "`hand` gives it to `to`."
                    },
                    "to": {
                        "type": "string",
                        "description": "`say`: one principal, PRIVATE, dm only. `rule`/`hand`: the new holder."
                    },
                    "room": {
                        "type": "string",
                        "description": "`project` (default), `host`, `dm` (read only), or `project/<key>`."
                    },
                    "since": {
                        "type": "integer",
                        "description": "`read`: after this seq."
                    },
                    "last": {
                        "type": "integer",
                        "description": "`read`: the newest N (20)."
                    },
                    "thread": {
                        "type": "integer",
                        "description": "`say`/`own`: the seq you answer. `read`: that thread."
                    },
                    "todo": {
                        "type": "string",
                        "description": "`say`: a todo row id."
                    },
                    "queue": {
                        "type": "string",
                        "description": "`say`: a merge-queue row id."
                    },
                    "evidence": {
                        "type": "string",
                        "description": "`say`: a sha or gate/queue row id backing a claim, else it reads `unbacked`."
                    },
                    "check": {
                        "type": "boolean",
                        "description": "`read`: judge claims against the queue."
                    },
                    "session": {
                        "type": "string",
                        "description": "Id or name; default you."
                    },
                    "scope": {
                        "type": "string",
                        "description": "A path glob or a topic."
                    },
                    "ttl": {
                        "type": "integer",
                        "description": "Minutes; `own` 60, `status` 30, max 240."
                    },
                    "attachments": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "`say`: up to 4 paths, 8 MiB each (png, jpeg, gif, webp, text)."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["post", "announce"],
                        "description": "`say`: one line to all."
                    }
                },
                "required": ["verb"]
            }),
            // It changes the host's shared state - who holds what - and nothing on the operator's
            // disk, no process and no network. `Session` is the class for that, and it is
            // unattended: a card per room verb would be a room nobody uses.
            Access::Session,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let workspace = ctx.backend.workspace_path().map(PathBuf::from);
        match dispatch(self.link.as_ref(), args, workspace.as_deref()) {
            Ok(said) => Invocation::ok(said),
            Err(failure) => failure,
        }
    }
}

/// The mime the room takes for a file, by its name: one table, because the daemon that reads the
/// bytes and the bridge that uploads them both ask this question, and two answers would drift.
/// These are the five of `letibot_room::blob::MIMES`; the room still refuses anything else.
/// `None` is a kind the room does not take, so the caller refuses before uploading.
pub fn mime_of(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "txt" | "text" | "log" => "text/plain",
        _ => return None,
    })
}

/// The largest one attachment may be, and the bytes one `put_blob` part carries: the coordinator's
/// own numbers, restated because a bridge names only the protocol number from the room crate.
/// `the_attachment_table_is_the_rooms_allowlist` (harnessd) pins both to `letibot_room::blob`.
pub const MAX_ATTACHMENT: usize = 8 * 1024 * 1024;
pub const PART_BYTES: usize = 512 * 1024;

/// The words a message carries once its files are on the room: a body names a picture
/// `![alt](blob:ID)`, so a file the sender did not name is referenced on its own line. A reference
/// the text already carries is left alone - the sender's words are not rewritten twice.
pub fn referenced(text: &str, files: &[(String, String)]) -> String {
    let mut out = text.to_string();
    for (id, name) in files {
        if out.contains(&format!("blob:{id}")) {
            continue;
        }
        out.push_str(&format!("\n![{name}](blob:{id})"));
    }
    out
}

/// The files a `say` names, resolved against the workspace this session works in. A path outside it
/// is refused here: the daemon reads these bytes on the session's behalf, and it may read no further
/// than the session itself may.
fn attachments_of(args: &Value, workspace: Option<&Path>) -> Result<Vec<String>, String> {
    let Some(v) = args.get("attachments").filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let paths = v
        .as_array()
        .ok_or("`attachments` must be an array of file paths")?;
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let Some(workspace) = workspace else {
        return Err(
            "this session has no workspace, so there is no directory an attachment may be read \
             from — nothing was uploaded"
                .to_string(),
        );
    };
    let root = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let mut out = Vec::new();
    for p in paths {
        let raw = p
            .as_str()
            .ok_or("`attachments` must be an array of file paths")?;
        if raw.trim().is_empty() {
            return Err("`attachments` holds an empty path — nothing was uploaded".to_string());
        }
        let full = root.join(raw);
        // Canonicalised where the file is there, so a symlink out of the workspace is the path it
        // really names; a path that does not exist fails the `is_file` below either way.
        let full = full.canonicalize().unwrap_or(full);
        if !full.starts_with(&root) {
            return Err(format!(
                "`{raw}` is outside this session's workspace `{}`, so it is not a file this \
                 session may send — nothing was uploaded",
                root.display()
            ));
        }
        if !full.is_file() {
            return Err(format!(
                "`{raw}` is not a file this session can read — nothing was uploaded"
            ));
        }
        if mime_of(raw).is_none() {
            return Err(format!(
                "`{raw}` is not a kind the room carries: a picture (`png`, `jpeg`, `gif`, `webp`) \
                 or a text file — nothing was uploaded"
            ));
        }
        out.push(full.to_string_lossy().into_owned());
    }
    Ok(out)
}

/// The tool's whole decision with no context, which is what the tests drive: the rule that a
/// verb without its field never reaches the room cannot be asserted through an `InvokeCtx`.
#[allow(clippy::result_large_err)] // Invocation is the failed row the caller shows; boxing it reshapes the tool API
fn dispatch(
    link: &dyn RoomLink,
    args: &Value,
    workspace: Option<&Path>,
) -> Result<String, Invocation> {
    let verb = args
        .get("verb")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if verb.is_empty() {
        return Err(Invocation::failed(
            "no verb",
            "the `room` tool needs a `verb`: post, claim, beat, waiting, done, abandon, queue \
             or holding (see the schema).",
        ));
    }
    // The required field of each verb, checked here so a model that forgot one is told
    // which, in the room's own words, rather than by a refusal from three layers down.
    let missing = match verb.as_str() {
        "post" if !nonempty(args, "tag") => Some("tag"),
        "post" if !nonempty(args, "body") => Some("body"),
        "claim" | "queue" if !nonempty(args, "tag") => Some("tag"),
        "claim_item" | "beat" | "done" => {
            if item_of(args).is_none() {
                Some("item")
            } else {
                None
            }
        }
        "waiting" => {
            if item_of(args).is_none() {
                Some("item")
            } else {
                None
            }
        }
        "abandon" => {
            if item_of(args).is_none() {
                Some("item")
            } else if !nonempty(args, "why") {
                Some("why")
            } else {
                None
            }
        }
        "dispute" => {
            if item_of(args).is_none() {
                Some("item")
            } else if !nonempty(args, "says") {
                Some("says")
            } else {
                None
            }
        }
        "say" if !nonempty(args, "text") => Some("text"),
        "own" | "accept" | "release" | "hand" if !nonempty(args, "scope") => Some("scope"),
        "hand" if !nonempty(args, "to") => Some("to"),
        "answer" | "decline" => {
            if args.get("ask").and_then(|v| v.as_u64()).is_none() {
                Some("ask")
            } else if !nonempty(args, "text") {
                Some("text")
            } else {
                None
            }
        }
        "invite" if !nonempty(args, "room") => Some("room"),
        "invite" if !nonempty(args, "session") => Some("session"),
        "leave" if !nonempty(args, "room") => Some("room"),
        "rule" => {
            if args.get("question").and_then(|v| v.as_u64()).is_none() {
                Some("question")
            } else if !nonempty(args, "decision") {
                Some("decision")
            } else {
                None
            }
        }
        _ => None,
    };
    if let Some(field) = missing {
        return Err(Invocation::failed(
            format!("`{verb}` needs `{field}`"),
            format!(
                "the `room` verb `{verb}` cannot be run without `{field}` — nothing was sent to \
                 the room."
            ),
        ));
    }
    if verb != "say" && args.get("attachments").is_some_and(|v| !v.is_null()) {
        return Err(Invocation::failed(
            "`attachments` is a `say` thing",
            format!(
                "`{verb}` carries no files: only `say` takes `attachments`, and nothing was sent to \
                 the room."
            ),
        ));
    }
    if verb != "say" && nonempty(args, "evidence") {
        return Err(Invocation::failed(
            "`evidence` is a `say` thing",
            format!(
                "`{verb}` takes no `evidence`: only a `say` makes a claim about the world, and \
                 nothing was sent to the room."
            ),
        ));
    }
    if verb != "read" && args.get("check").is_some_and(|v| !v.is_null()) {
        return Err(Invocation::failed(
            "`check` is a `read` thing",
            format!(
                "`{verb}` takes no `check`: only `read` hands the messages back for judging, and \
                 nothing was sent to the room."
            ),
        ));
    }
    let files = match attachments_of(args, workspace) {
        Ok(files) => files,
        Err(why) => return Err(Invocation::failed(why.clone(), why)),
    };
    // The daemon reads the bytes, so it is given paths resolved against this workspace and nothing
    // it has to guess a directory from.
    let with;
    let args = if files.is_empty() {
        args
    } else {
        with = {
            let mut a = args.clone();
            a["attachments"] = json!(files);
            a
        };
        &with
    };
    link.call(&verb, args)
        .map_err(|why| Invocation::failed(why.clone(), why))
}

fn nonempty(args: &Value, key: &str) -> bool {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

fn item_of(args: &Value) -> Option<u64> {
    args.get("item").and_then(|v| v.as_u64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A link that records what it was asked and answers in its own words - the tool's contract,
    /// with no daemon and no socket.
    struct Recorder {
        seen: Mutex<Vec<(String, Value)>>,
        answer: Result<String, String>,
    }

    impl Recorder {
        fn new(answer: Result<&str, &str>) -> Arc<Recorder> {
            Arc::new(Recorder {
                seen: Mutex::new(Vec::new()),
                answer: answer.map(|s| s.to_string()).map_err(|s| s.to_string()),
            })
        }
    }

    impl RoomLink for Recorder {
        fn call(&self, verb: &str, args: &Value) -> Result<String, String> {
            self.seen
                .lock()
                .unwrap()
                .push((verb.to_string(), args.clone()));
            self.answer.clone()
        }
    }

    fn invoke(link: Arc<dyn RoomLink>, args: Value) -> (bool, String) {
        invoke_in(link, args, None)
    }

    /// The same, with the workspace a session works in: an attachment is resolved against it.
    fn invoke_in(
        link: Arc<dyn RoomLink>,
        args: Value,
        workspace: Option<&std::path::Path>,
    ) -> (bool, String) {
        match dispatch(link.as_ref(), &args, workspace) {
            Ok(said) => (true, said),
            Err(failure) => (
                false,
                match &failure.outcome {
                    letibot_transcript::ToolOutcome::Failed { reason } => {
                        format!("{} — {}", reason, failure.payload)
                    }
                    other => format!("{other:?} — {}", failure.payload),
                },
            ),
        }
    }

    /// A session sends a picture it can read, and no other file on the disk: the daemon reads these
    /// bytes on the session's behalf, so the directory it may read from is the one it works in.
    #[test]
    fn an_attachment_outside_the_workspace_is_refused() {
        let root =
            std::env::temp_dir().join(format!("letibot-room-attach-tool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ws = root.join("ws");
        std::fs::create_dir_all(ws.join("shots")).expect("a workspace");
        let shot = ws.join("shots/shot.png");
        std::fs::write(&shot, b"\x89PNG\r\n\x1a\n-ish").expect("a picture");
        let outside = root.join("outside.png");
        std::fs::write(&outside, b"x").expect("a file outside");

        // A path inside the workspace is resolved and handed on as the absolute file it names.
        let link = Recorder::new(Ok("said #1"));
        let (ok, text) = invoke_in(
            link.clone(),
            json!({"verb": "say", "text": "look", "attachments": ["shots/shot.png"]}),
            Some(&ws),
        );
        assert!(ok, "{text}");
        let seen = link.seen.lock().unwrap();
        let sent = seen[0].1["attachments"][0].as_str().unwrap_or("");
        assert_eq!(
            sent,
            shot.to_string_lossy(),
            "resolved against the workspace"
        );
        drop(seen);

        for (named, want) in [
            (outside.to_string_lossy().into_owned(), "outside"),
            ("../outside.png".to_string(), "outside"),
            ("/etc/passwd".to_string(), "outside"),
        ] {
            let link = Recorder::new(Err("never"));
            let (ok, text) = invoke_in(
                link.clone(),
                json!({"verb": "say", "text": "look", "attachments": [named]}),
                Some(&ws),
            );
            assert!(!ok, "{named} must be refused: {text}");
            assert!(
                text.contains(want) || text.contains("outside this session's workspace"),
                "{named}: {text}"
            );
            assert!(
                link.seen.lock().unwrap().is_empty(),
                "nothing is uploaded: {text}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// What backs a claim about the world is a `say`'s, and it reaches the room as it was written.
    /// Any other verb that carries one is refused before the room is asked.
    #[test]
    fn evidence_is_a_say_thing_and_reaches_the_room() {
        let link = Recorder::new(Ok("said #1"));
        let (ok, text) = invoke(
            link.clone(),
            json!({"verb": "say", "text": "landed", "evidence": "m-7"}),
        );
        assert!(ok, "{text}");
        assert_eq!(link.seen.lock().unwrap()[0].1["evidence"], "m-7");
        let link = Recorder::new(Err("never"));
        let (ok, text) = invoke(
            link.clone(),
            json!({"verb": "post", "tag": "ram", "body": "8 GB", "evidence": "m-7"}),
        );
        assert!(
            !ok && text.contains("`evidence` is a `say` thing"),
            "{text}"
        );
        assert!(
            link.seen.lock().unwrap().is_empty(),
            "the room was not asked"
        );
    }

    /// `check` is a `read`'s: it reaches the link with the read, and any other verb that carries
    /// one is refused before the room is asked.
    #[test]
    fn check_is_a_read_thing_and_reaches_the_room() {
        let link = Recorder::new(Ok("#4 ...\ncheck: nothing to check"));
        let (ok, text) = invoke(
            link.clone(),
            json!({"verb": "read", "room": "host", "check": true}),
        );
        assert!(ok, "{text}");
        assert_eq!(link.seen.lock().unwrap()[0].1["check"], true);
        let link = Recorder::new(Err("never"));
        let (ok, text) = invoke(
            link.clone(),
            json!({"verb": "say", "text": "landed", "check": true}),
        );
        assert!(!ok && text.contains("`check` is a `read` thing"), "{text}");
        assert!(
            link.seen.lock().unwrap().is_empty(),
            "the room was not asked"
        );
    }

    #[test]
    fn the_description_is_clean_and_fits_the_prompt() {
        let tool = RoomTool::new(Arc::new(NoRoom));
        let s = tool.schema();
        assert_eq!(s.name, TOOL);
        assert_eq!(crate::schema::lint_description(&s.description), vec![]);
        assert_eq!(s.access, Access::Session);
        // The budgets the trim set: the whole schema is what a context pays for, every turn.
        let params = s.parameters.to_string().len();
        assert!(s.description.len() <= 800, "{} bytes", s.description.len());
        assert!(params <= 2450, "{params} bytes of parameters");
        assert!(!s.description.contains('\u{2014}'), "no em dash");
    }

    /// The ownership verbs need their scope, and a hand its receiver, before the room is asked.
    #[test]
    fn ownership_verbs_need_a_scope_and_a_hand_its_receiver() {
        let link = Recorder::new(Ok("you own `docs/**`"));
        for verb in ["own", "accept", "release", "hand"] {
            let (ok, text) = invoke(link.clone(), json!({"verb": verb}));
            assert!(!ok && text.contains("`scope`"), "{verb}: {text}");
        }
        let (ok, text) = invoke(link.clone(), json!({"verb": "hand", "scope": "docs/**"}));
        assert!(!ok && text.contains("`to`"), "{text}");
        let (ok, _) = invoke(
            link.clone(),
            json!({"verb": "own", "scope": "docs/**", "ttl": 30}),
        );
        assert!(ok);
        let seen = link.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "only the complete call reached the room");
        assert_eq!(seen[0].0, "own");
        let s = RoomTool::new(Arc::new(NoRoom)).schema();
        let verbs = s.parameters["properties"]["verb"]["enum"].to_string();
        for v in ["own", "hand", "accept", "release", "status"] {
            assert!(verbs.contains(&format!("\"{v}\"")), "{v} is in the schema");
        }
    }

    /// The standing rule is said once per context, here, and nowhere else in the schema.
    #[test]
    fn the_standing_rule_is_in_the_description_once() {
        let s = RoomTool::new(Arc::new(NoRoom)).schema();
        assert_eq!(s.description.matches(STANDING_RULE).count(), 1);
        assert!(!s.parameters.to_string().contains("end the turn"));
        assert!(STANDING_RULE.len() <= 220, "{} bytes", STANDING_RULE.len());
    }

    /// An ask is settled by its own verb and by nothing else: `answer` and `decline` name the ask and
    /// the words, and a call missing either never reaches the room.
    #[test]
    fn answering_needs_the_ask_and_the_words_and_is_named_in_the_schema() {
        let link = Recorder::new(Ok("ask #4211 settled"));
        let (ok, text) = invoke(
            link.clone(),
            json!({"verb": "answer", "ask": 4211, "text": "5433"}),
        );
        assert!(ok, "{text}");
        assert_eq!(text, "ask #4211 settled");
        {
            let seen = link.seen.lock().unwrap();
            assert_eq!(seen[0].0, "answer");
            assert_eq!(seen[0].1["ask"], json!(4211));
        }
        for (args, field) in [
            (json!({"verb": "answer", "text": "5433"}), "ask"),
            (json!({"verb": "decline", "ask": 4211}), "text"),
        ] {
            let link = Recorder::new(Ok("never"));
            let (ok, text) = invoke(link.clone(), args);
            assert!(!ok, "{text}");
            assert!(text.contains(field), "{text}");
            assert!(
                link.seen.lock().unwrap().is_empty(),
                "nothing is sent to the room"
            );
        }
        // And the schema names them, so a model reading the tool knows the door exists.
        let schema = RoomTool::new(Arc::new(NoRoom)).schema();
        let verbs: Vec<&str> = schema.parameters["properties"]["verb"]["enum"]
            .as_array()
            .expect("an enum of verbs")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        for want in ["asks", "answer", "decline"] {
            assert!(verbs.contains(&want), "`{want}` is not a verb: {verbs:?}");
        }
        assert!(schema.parameters["properties"]["ask"].is_object());
    }

    /// The beat paragraph is the link's, and the two links say opposite things: the sentence a
    /// model reads is chosen by the answer, so both answers are reachable and pinned here.
    #[test]
    fn the_description_says_whether_the_daemon_beats_on_my_behalf() {
        let none = RoomTool::new(Arc::new(NoRoom)).schema().description;
        assert!(
            none.contains("Nothing beats for you"),
            "a registry with no room must not promise a beat: {none}"
        );
        assert!(!none.contains("Your daemon beats"), "{none}");

        let real = RoomTool::new(Recorder::new(Ok("posted #1")) as Arc<dyn RoomLink>)
            .schema()
            .description;
        assert!(
            real.contains("Your daemon beats your claims while a turn runs"),
            "a real link is the daemon beating, and the model has to be told: {real}"
        );
        assert!(!real.contains("Nothing beats for you"), "{real}");
    }

    #[test]
    fn a_verb_is_handed_to_the_daemon_with_its_arguments() {
        let link = Recorder::new(Ok("posted #1 under `ram`"));
        let (ok, text) = invoke(
            link.clone(),
            json!({"verb": "post", "tag": "ram", "body": "8 GB"}),
        );
        assert!(ok, "{text}");
        assert_eq!(text, "posted #1 under `ram`");
        let seen = link.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "post");
        assert_eq!(seen[0].1["tag"], "ram");
    }

    #[test]
    fn a_verb_missing_its_field_is_refused_here_and_nothing_is_sent() {
        for (args, field) in [
            (json!({"verb": "post", "tag": "ram"}), "body"),
            (json!({"verb": "claim"}), "tag"),
            (json!({"verb": "beat"}), "item"),
            (json!({"verb": "abandon", "item": 1}), "why"),
            (json!({"verb": "rule", "question": 1}), "decision"),
            (json!({"verb": ""}), "verb"),
        ] {
            let link = Recorder::new(Ok("never"));
            let (ok, text) = invoke(link.clone(), args);
            assert!(!ok, "a verb without its field must fail: {text}");
            assert!(text.contains(field), "it must name `{field}`: {text}");
            assert!(
                link.seen.lock().unwrap().is_empty(),
                "nothing reaches the room: {text}"
            );
        }
    }

    #[test]
    fn a_refusal_from_the_room_is_a_refusal_and_not_a_failure_of_the_tool() {
        let link = Recorder::new(Err("tag `ram` is held by `s-b` on #2 for 40s"));
        let (ok, text) = invoke(link, json!({"verb": "claim", "tag": "ram"}));
        assert!(!ok);
        assert!(text.contains("held by `s-b`"), "{text}");
    }

    #[test]
    fn a_registry_with_no_room_says_so_in_words() {
        let (ok, text) = invoke(Arc::new(NoRoom), json!({"verb": "claim", "tag": "ram"}));
        assert!(!ok);
        assert!(text.contains("harnessd --room"), "{text}");
        assert!(!NoRoom.beats_on_my_behalf());
    }

    /// A session with no room is told how the box gets one, because the launcher starts a
    /// coordinator when none serves and a refusal that stopped naming it would send the model
    /// looking for a socket it cannot start.
    #[test]
    fn a_session_with_no_room_is_told_which_verb_starts_one() {
        let (ok, text) = invoke(Arc::new(NoRoom), json!({"verb": "queue", "tag": "ram"}));
        assert!(!ok);
        assert!(text.contains("letibot --room"), "{text}");
    }
}
