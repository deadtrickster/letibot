//! `queue`: the merge queue from the model's side. `status` reads one repository, `all` reads
//! every one on the box and `repository` one in full; `regate`, `restart` and `withdraw` move a
//! row; `propose_target` raises a target change to the operator and writes no config.

use std::sync::Arc;

use serde_json::Value;

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// The tool's name, in one place.
pub const TOOL: &str = "queue";

/// The daemon's half - the store and the repository the session is in. Every method answers
/// the sentence the model reads, or a refusal in words.
pub trait QueueDesk: Send + Sync {
    /// The queues, scoped and said so: this session's repository unless `all` widens it to
    /// every one, or `repository` names one (a path, a path's tail or a directory name) - which is
    /// also what asks for it in full. The first line of every answer says what it printed.
    fn status(&self, all: bool, repository: Option<&str>) -> Result<String, String>;
    /// Gate a target's tip again. `None`s are the session's repository and its project's target.
    fn regate(&self, repository: Option<&str>, target: Option<&str>) -> Result<String, String>;
    /// Put a pushed branch of this session's repository up for review. `None` target is the
    /// project's; `why` is the ask or work the branch is for.
    fn enqueue(
        &self,
        branch: &str,
        target: Option<&str>,
        why: &str,
        repository: Option<&str>,
    ) -> Result<String, String>;
    /// A parked row back in line.
    fn restart(&self, id: &str) -> Result<String, String>;
    /// A row out of its queue, with the words why.
    fn withdraw(&self, id: &str, why: &str) -> Result<String, String>;
    /// Put `[queue] target = "<target>"` to the operator. Writes nothing.
    fn propose_target(&self, target: &str, why: &str) -> Result<String, String>;
}

/// No queue behind the tool - a daemon with no `--store`, or a test registry. Every verb says
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
    fn enqueue(
        &self,
        _b: &str,
        _t: Option<&str>,
        _w: &str,
        _r: Option<&str>,
    ) -> Result<String, String> {
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

/// The tool.
pub struct QueueTool {
    desk: Arc<dyn QueueDesk>,
}

impl QueueTool {
    pub fn new(desk: Arc<dyn QueueDesk>) -> Self {
        QueueTool { desk }
    }
}

const ACTIONS: &str = "\"status\", \"explain\", \"enqueue\", \"regate\", \"restart\", \"withdraw\" or \"propose_target\"";

/// How the queue works, for a session that must not have to ask a model. `queue` `explain` prints it.
pub const EXPLAIN: &str = "\
The merge queue, in two queues and a lease.\n\
\n\
ENTRY. One branch headed for a target. It has a REVIEW row (the first queue) and, once approved, a \
LANDING row (the second). `status` lists both with the reason each is where it is.\n\
\n\
REVIEW ROW states: waiting (for a reviewer slot), reviewing (a reviewer child holds it until a \
deadline), approved (handed to the landing queue), rejected (back to the author), cannot_decide \
(a person rules), blocked (the review could not run). A VERDICT is the reviewer's own call \
(`review_verdict`: accept, reject or needs_human) bound to the head it judged. A branch that moves \
after a verdict goes back to waiting. A red target halts review of ordinary branches.\n\
\n\
LANDING ROW states: queued (held until the target is gated green at its current tip), gating (the \
queue rebased it and runs the gate, one at a time per repository), landed, landed_local (push \
failed), failed (gate red), timed_out, conflict (rebase), blocked. The gate is the target's \
`## Merge gate` in AGENTS.md. A tip that is red lands nothing but its fix.\n\
\n\
LEASES. One per repository: the daemon that holds it serves both queues. It beats every few \
seconds and is replaced after ninety seconds of silence. A review claim is a lease too (owner and \
deadline); a landing claim guards the one gate.\n\
\n\
WHO MOVES WHAT. The queue's clock moves rows on its own. You: `enqueue` a pushed branch (needs \
`branch` and `why`, optional `target`), `regate` a target once a red cause is gone, `restart` a \
parked row, `withdraw` a stale one, `propose_target`. Only the operator approves over a reviewer \
(`/queue approve`), vetoes, or changes the target. A reviewer's verdict is the reviewer's alone. \
Nothing you write edits a row's state.";

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
            "The merge queue of this project, SCOPED TO IT. `explain` says how entries, rows, \
             leases and verdicts work and who moves each. `enqueue` puts a pushed `branch` up \
             for review onto `target` (default: the project's), `why` naming the work. `status` \
             prints this session's repository — its target, each entry with why it is where it \
             is, the leases, and what happens next; `all: true` prints one line per repository \
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
                        "enum": ["status", "explain", "enqueue", "regate", "restart", "withdraw", "propose_target"],
                        "description": "What to do."
                    },
                    "id": {"type": "string", "description": "restart / withdraw: the row's id, as `status` shows it (a unique prefix will do)."},
                    "branch": {"type": "string", "description": "enqueue: the pushed local branch to put up for review."},
                    "target": {"type": "string", "description": "enqueue / regate: the target branch (default: the project's). propose_target: the branch the project should land on."},
                    "repository": {"type": "string", "description": "status: the repository to print in full — a path, a path's tail or its directory name (default: this session's, briefly). regate: the repository's path, when it is not this session's."},
                    "why": {"type": "string", "description": "enqueue: the ask or work the branch is for. withdraw / propose_target: your reason, in a sentence the operator reads."},
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
            Some("status") | Some("explain") => Some(Access::Read),
            Some("enqueue") | Some("regate") | Some("restart") | Some("propose_target") => {
                Some(Access::Session)
            }
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
            "explain" => Ok(EXPLAIN.to_string()),
            "enqueue" => match (need("branch"), need("why")) {
                (Ok(branch), Ok(why)) => {
                    self.desk
                        .enqueue(branch, text(args, "target"), why, text(args, "repository"))
                }
                (Err(refused), _) | (_, Err(refused)) => return *refused,
            },
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
        fn enqueue(
            &self,
            b: &str,
            t: Option<&str>,
            w: &str,
            _r: Option<&str>,
        ) -> Result<String, String> {
            self.said
                .lock()
                .unwrap()
                .push(format!("enqueue {b} {t:?} {w}"));
            Ok("queued".into())
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

    /// Only `withdraw` reaches the gate as a write; a read is a read and the queue's own
    /// state is the session's.
    #[test]
    fn only_withdraw_is_a_write_and_status_is_a_read() {
        let t = QueueTool::new(Arc::new(NoQueue));
        assert_eq!(t.schema().access, Access::Write);
        let a = |action: &str| t.access_for(&serde_json::json!({ "action": action }));
        assert_eq!(a("status"), Some(Access::Read));
        assert_eq!(a("explain"), Some(Access::Read));
        assert_eq!(a("enqueue"), Some(Access::Session));
        assert_eq!(a("regate"), Some(Access::Session));
        assert_eq!(a("restart"), Some(Access::Session));
        assert_eq!(a("propose_target"), Some(Access::Session));
        assert_eq!(a("withdraw"), None, "the schema's Write stands");
    }

    /// The model of the queue is in the tool, and says who may move what.
    #[test]
    fn explain_states_the_model_and_the_description_fits() {
        for word in ["ENTRY", "REVIEW ROW", "LANDING ROW", "VERDICT", "LEASES", "WHO MOVES WHAT"] {
            assert!(EXPLAIN.contains(word), "{word}");
        }
        let d = QueueTool::new(Arc::new(NoQueue)).schema().description;
        assert_eq!(crate::schema::lint_description(&d), vec![]);
        assert!(d.len() < 1200, "{} bytes", d.len());
    }

    /// Each verb reaches the desk; a missing argument is refused by name; and `withdraw` goes
    /// to the gate like any write - here the default boundary, which decides nothing and so
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
        assert!(matches!(call(r#"{"action": "explain"}"#), ToolOutcome::Ok));
        assert!(matches!(
            call(r#"{"action": "enqueue", "branch": "feat/x", "why": "the ask"}"#),
            ToolOutcome::Ok
        ));
        assert!(matches!(
            call(r#"{"action": "enqueue", "branch": "feat/x"}"#),
            ToolOutcome::Failed { .. }
        ));
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
                "enqueue feat/x None the ask".into(),
                "regate None Some(\"main\")".into(),
                "restart r-1".into(),
                "propose feat/x it lands there".into(),
            ]
        );
    }

    /// `status` carries the scope the caller asked for, and asks for none when it asked for
    /// none - `None` is what the desk reads as *this session's own repository*.
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
