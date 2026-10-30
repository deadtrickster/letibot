//! `queue` — **the merge queue, from the model's side** (R76 item 2).
//!
//! The operator, on pg-noop's red `main` (2026-10-11): *"looks like the main agent doesnt get
//! these main emergencies. also, it should be able to get out of it itself obviously"*. The
//! `/queue` verbs were a head's; a model told its target was red could do nothing but tell the
//! operator to type them. This is the door it holds:
//!
//! * `status` — **this session's repository, and a screenful of it**: the target's health, what
//!   each queue holds by state, the rows somebody must act on first. `all` is one line per
//!   repository on the box and `repository` is one repository in full. Every answer's first line
//!   says which of the three it printed. A read.
//! * `regate` — gate the target's tip again even when it was already judged (the cause was the
//!   machine, a flake, a gate fixed outside the target). Changes the queue's own state: `Session`.
//! * `restart` — a parked review back to waiting, a parked landing back in line. `Session`.
//! * `withdraw` — a review or landing row taken out (history kept; the branch stays). The one
//!   verb that drops somebody's work from the queue, so it is the schema's `Write` and goes to the
//!   gate like any write.
//! * `propose_target` — the project lands on another branch than the queue gates. **Setting
//!   `[queue] target` is the operator's decision**: this raises it to them, naming the exact
//!   `leticode.toml` line, and writes no config. `Session`.
//!
//! What it acts on is the daemon's ([`QueueDesk`]); the caller's session is bound at open and
//! recorded on every move, never read from the call.

use std::sync::Arc;

use serde_json::Value;

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// The tool's name, in one place.
pub const TOOL: &str = "queue";

/// **The daemon's half** — the store and the repository the session is in. Every method answers
/// the sentence the model reads, or a refusal in words.
pub trait QueueDesk: Send + Sync {
    /// **The queues, scoped and said so**: this session's repository unless `all` widens it to
    /// every one, or `repository` names one (a path, a path's tail or a directory name) — which is
    /// also what asks for it in full. The first line of every answer says what it printed.
    fn status(&self, all: bool, repository: Option<&str>) -> Result<String, String>;
    /// Gate a target's tip again. `None`s are the session's repository and its project's target.
    fn regate(&self, repository: Option<&str>, target: Option<&str>) -> Result<String, String>;
    /// A parked row back in line.
    fn restart(&self, id: &str) -> Result<String, String>;
    /// A row out of its queue, with the words why.
    fn withdraw(&self, id: &str, why: &str) -> Result<String, String>;
    /// Put `[queue] target = "<target>"` to the operator. Writes nothing.
    fn propose_target(&self, target: &str, why: &str) -> Result<String, String>;
}

/// **No queue behind the tool** — a daemon with no `--store`, or a test registry. Every verb says
/// so by name rather than the tool being absent, which would read as a capability this build
/// lacks.
pub struct NoQueue;

impl QueueDesk for NoQueue {
    fn status(&self, _all: bool, _r: Option<&str>) -> Result<String, String> {
        Err(Self::WHY.into())
    }
    fn regate(&self, _r: Option<&str>, _t: Option<&str>) -> Result<String, String> {
        Err(Self::WHY.into())
    }
    fn restart(&self, _id: &str) -> Result<String, String> {
        Err(Self::WHY.into())
    }
    fn withdraw(&self, _id: &str, _why: &str) -> Result<String, String> {
        Err(Self::WHY.into())
    }
    fn propose_target(&self, _t: &str, _why: &str) -> Result<String, String> {
        Err(Self::WHY.into())
    }
}

impl NoQueue {
    const WHY: &'static str = "this session's daemon has no store, so it serves no merge queue";
}

/// **The tool.**
pub struct QueueTool {
    desk: Arc<dyn QueueDesk>,
}

impl QueueTool {
    pub fn new(desk: Arc<dyn QueueDesk>) -> Self {
        QueueTool { desk }
    }
}

const ACTIONS: &str = "\"status\", \"regate\", \"restart\", \"withdraw\" or \"propose_target\"";

fn text<'a>(args: &'a Value, field: &str) -> Option<&'a str> {
    args.get(field)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

impl Tool for QueueTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            TOOL,
            "The merge queue of this project, SCOPED TO IT. `status` prints this session's \
             repository — its target and the gate's evidence, what each queue holds by state, and \
             the ten rows somebody must act on first; `all: true` prints one line per repository \
             on the box; `repository: \"<path, path's tail or directory name>\"` prints one \
             repository in full. Every answer's first line says which scope it printed. Then the \
             verbs: gate the target's tip again once the cause of a red or timed-out gate is gone \
             (`regate`); put a parked review or landing back in line (`restart`); take a stale row \
             out (`withdraw`, history kept, the branch stays); and propose the branch the project \
             lands on (`propose_target`) — which is the operator's decision: it is put to them \
             and no config is written.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["status", "regate", "restart", "withdraw", "propose_target"],
                        "description": "What to do."
                    },
                    "id": {"type": "string", "description": "restart / withdraw: the row's id, as `status` shows it (a unique prefix will do)."},
                    "target": {"type": "string", "description": "regate: the target branch (default: the project's). propose_target: the branch the project should land on."},
                    "repository": {"type": "string", "description": "status: the repository to print in full — a path, a path's tail or its directory name (default: this session's, briefly). regate: the repository's path, when it is not this session's."},
                    "why": {"type": "string", "description": "withdraw / propose_target: your reason, in a sentence the operator reads."},
                    "all": {"type": "boolean", "description": "status: one line per repository on the box, instead of this session's repository in detail."}
                },
                "required": ["action"]
            }),
            Access::Write,
        )
    }

    /// Only `withdraw` drops somebody's row; the rest read, or change the queue's own state and
    /// nothing the operator owns. Narrowing only, per the trait.
    fn access_for(&self, args: &Value) -> Option<Access> {
        match args.get("action").and_then(|v| v.as_str()) {
            Some("status") => Some(Access::Read),
            Some("regate") | Some("restart") | Some("propose_target") => Some(Access::Session),
            _ => None,
        }
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(action) = text(args, "action") else {
            return Invocation::failed(
                "`queue` needs an action",
                format!("call `queue` with `action` = {ACTIONS}."),
            );
        };
        // The refusal is boxed: an `Invocation` is large, and it is the rare arm.
        let need = |field: &str| {
            text(args, field).ok_or_else(|| {
                Box::new(Invocation::failed(
                    format!("`{action}` needs `{field}`"),
                    match field {
                        "id" => "name the row by the id `queue` with `action: \"status\"` shows."
                            .to_string(),
                        "why" => "say why in a sentence: it goes on the row's history, where \
                                  the operator reads it."
                            .to_string(),
                        _ => format!("give `{field}`."),
                    },
                ))
            })
        };
        let said = match action {
            "status" => self.desk.status(
                args.get("all").and_then(|v| v.as_bool()).unwrap_or(false),
                text(args, "repository"),
            ),
            "regate" => self
                .desk
                .regate(text(args, "repository"), text(args, "target")),
            "restart" => match need("id") {
                Ok(id) => self.desk.restart(id),
                Err(refused) => return *refused,
            },
            "withdraw" => match (need("id"), need("why")) {
                (Ok(id), Ok(why)) => self.desk.withdraw(id, why),
                (Err(refused), _) | (_, Err(refused)) => return *refused,
            },
            "propose_target" => match (need("target"), need("why")) {
                (Ok(t), Ok(why)) => self.desk.propose_target(t, why),
                (Err(refused), _) | (_, Err(refused)) => return *refused,
            },
            other => {
                return Invocation::failed(
                    format!("`{other}` is not an action of `queue`"),
                    format!("the actions are {ACTIONS}."),
                );
            }
        };
        match said {
            Ok(line) => Invocation::ok(line),
            Err(why) => Invocation::failed(
                format!("the queue did not {action}: {why}"),
                "nothing moved. `queue` with `action: \"status\"` shows what is there.",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Desk {
        said: Mutex<Vec<String>>,
    }

    impl QueueDesk for Desk {
        fn status(&self, all: bool, r: Option<&str>) -> Result<String, String> {
            self.said
                .lock()
                .unwrap()
                .push(format!("status {all} {r:?}"));
            Ok("both queues".into())
        }
        fn regate(&self, r: Option<&str>, t: Option<&str>) -> Result<String, String> {
            self.said
                .lock()
                .unwrap()
                .push(format!("regate {r:?} {t:?}"));
            Ok("gated again".into())
        }
        fn restart(&self, id: &str) -> Result<String, String> {
            self.said.lock().unwrap().push(format!("restart {id}"));
            Ok("back in line".into())
        }
        fn withdraw(&self, id: &str, why: &str) -> Result<String, String> {
            self.said
                .lock()
                .unwrap()
                .push(format!("withdraw {id} {why}"));
            Ok("withdrawn".into())
        }
        fn propose_target(&self, t: &str, why: &str) -> Result<String, String> {
            self.said.lock().unwrap().push(format!("propose {t} {why}"));
            Ok("put to the operator".into())
        }
    }

    /// **Only `withdraw` reaches the gate as a write**; a read is a read and the queue's own
    /// state is the session's.
    #[test]
    fn only_withdraw_is_a_write_and_status_is_a_read() {
        let t = QueueTool::new(Arc::new(NoQueue));
        assert_eq!(t.schema().access, Access::Write);
        let a = |action: &str| t.access_for(&serde_json::json!({ "action": action }));
        assert_eq!(a("status"), Some(Access::Read));
        assert_eq!(a("regate"), Some(Access::Session));
        assert_eq!(a("restart"), Some(Access::Session));
        assert_eq!(a("propose_target"), Some(Access::Session));
        assert_eq!(a("withdraw"), None, "the schema's Write stands");
    }

    /// **Each verb reaches the desk; a missing argument is refused by name; and `withdraw` goes
    /// to the gate like any write** — here the default boundary, which decides nothing and so
    /// refuses: the desk never hears it.
    #[test]
    fn each_action_reaches_the_desk_and_withdraw_is_gated_like_any_write() {
        use crate::backend::HostBackend;
        use crate::backend::tempdir::TempDir;
        use crate::events::RecordingToolSink;
        use crate::runtime::{Registry, ToolRuntime};
        use letibot_transcript::{ToolCall, ToolOutcome};
        let desk = Arc::new(Desk::default());
        let dir = TempDir::new();
        let backend = HostBackend::new(dir.path()).expect("a backend");
        let mut reg = Registry::new();
        reg.register(Box::new(QueueTool::new(desk.clone())))
            .expect("registers");
        let rt = ToolRuntime::new(reg, Box::new(backend));
        let call = |args: &str| {
            let mut sink = RecordingToolSink::new();
            rt.invoke(
                "t1",
                &ToolCall {
                    id: "c0".into(),
                    name: TOOL.into(),
                    arguments: args.into(),
                },
                &mut sink,
            )
            .outcome
        };
        assert!(matches!(call(r#"{"action": "status"}"#), ToolOutcome::Ok));
        assert!(matches!(
            call(r#"{"action": "regate", "target": "main"}"#),
            ToolOutcome::Ok
        ));
        assert!(matches!(
            call(r#"{"action": "restart", "id": "r-1"}"#),
            ToolOutcome::Ok
        ));
        assert!(matches!(
            call(r#"{"action": "restart"}"#),
            ToolOutcome::Failed { .. }
        ));
        assert!(matches!(
            call(r#"{"action": "propose_target", "target": "feat/x", "why": "it lands there"}"#),
            ToolOutcome::Ok
        ));
        assert!(
            !matches!(
                call(r#"{"action": "withdraw", "id": "r-1", "why": "stale"}"#),
                ToolOutcome::Ok
            ),
            "a write nobody admitted"
        );
        assert_eq!(
            *desk.said.lock().unwrap(),
            vec![
                "status false None".to_string(),
                "regate None Some(\"main\")".into(),
                "restart r-1".into(),
                "propose feat/x it lands there".into(),
            ]
        );
    }

    /// **`status` carries the scope the caller asked for, and asks for none when it asked for
    /// none** — `None` is what the desk reads as *this session's own repository*.
    #[test]
    fn status_passes_the_scope_through_and_asks_for_none_by_default() {
        use crate::backend::HostBackend;
        use crate::backend::tempdir::TempDir;
        use crate::events::RecordingToolSink;
        use crate::runtime::{Registry, ToolRuntime};
        use letibot_transcript::{ToolCall, ToolOutcome};
        let desk = Arc::new(Desk::default());
        let dir = TempDir::new();
        let backend = HostBackend::new(dir.path()).expect("a backend");
        let mut reg = Registry::new();
        reg.register(Box::new(QueueTool::new(desk.clone())))
            .expect("registers");
        let rt = ToolRuntime::new(reg, Box::new(backend));
        let call = |args: &str| {
            let mut sink = RecordingToolSink::new();
            rt.invoke(
                "t1",
                &ToolCall {
                    id: "c0".into(),
                    name: TOOL.into(),
                    arguments: args.into(),
                },
                &mut sink,
            )
            .outcome
        };
        for args in [
            r#"{"action": "status"}"#,
            r#"{"action": "status", "all": true}"#,
            r#"{"action": "status", "repository": "letibot"}"#,
            r#"{"action": "status", "repository": "/a/letim"}"#,
        ] {
            assert!(matches!(call(args), ToolOutcome::Ok), "{args}");
        }
        assert_eq!(
            *desk.said.lock().unwrap(),
            vec![
                "status false None".to_string(),
                "status true None".into(),
                "status false Some(\"letibot\")".into(),
                "status false Some(\"/a/letim\")".into(),
            ]
        );
    }
}
