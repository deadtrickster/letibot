//! **The eight verbs**, and the map of sessions the bridge holds.
//!
//! `docs/claude-code-bridge.md` fixes the table: each tool's input, and what it answers.
//! Two things in it are worth naming here because they are the design rather than a
//! shape:
//!
//! * **`leticode_start` keeps the head.** It creates the session, sends the brief and
//!   stays attached on its own socket, because a session nobody is attached to cannot ask
//!   anybody anything — and the whole point of running leticode from Claude Code is that
//!   the gate has somebody to ask.
//! * **`leticode_answer` sends the protocol's `Answer`**, not a shell command and not a
//!   prompt: the decision lands in the corpus attributed to the head that gave it, which
//!   is what makes "who approved this" answerable afterwards.
//!
//! Nothing here talks to a model. A tool call either drives the head protocol or runs
//! `git` in the worktree; the model is leticode's, on the other side of the socket.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::daemon;
use crate::diff;
use crate::head::{self, Attention, Head, Turn};

/// The default approval point. `writes-allowed` is the point whose grant scope is
/// *session*, so an approval a person gives once covers that class of call — and exec,
/// the network and the always-ask list still ask, which is what the bridge is for.
pub const DEFAULT_MODE: &str = "writes-allowed";

/// What the bridge holds: one attached head per session it started, by session id.
///
/// A `HashMap` behind a mutex rather than an actor: the tools are called one at a time by
/// one MCP client, and the only concurrency here is each head's own collecting thread.
#[derive(Default)]
pub struct Bridge {
    heads: Mutex<HashMap<String, Arc<Head>>>,
}

impl Bridge {
    pub fn new() -> Bridge {
        Bridge::default()
    }

    /// **Start leticode on a worktree.**
    ///
    /// The daemon is this folder's — started if nothing is listening, joined if one is —
    /// because a second daemon for one folder is how two processes come to interleave rows
    /// into one transcript.
    pub fn start(
        &self,
        worktree: &str,
        brief: &str,
        model: Option<&str>,
        mode: &str,
    ) -> Result<Value, String> {
        if brief.trim().is_empty() {
            return Err("`brief` is empty: a session with no brief runs no turn".into());
        }
        let d = daemon::ensure(Path::new(worktree), model, mode)?;
        let session = head::create_session(&d, "")?;
        let h = Arc::new(Head::attach(&d, &session)?);
        // Before the brief, so the first tool call already runs at the point the caller
        // asked for: `Mode` is queued like a prompt and takes effect from the next call.
        if let Err(e) = h.set_mode(mode) {
            // A session that cannot be moved is still a session — say so and let the brief
            // run at the daemon's own point rather than losing the session to the failure.
            let prompt = h.prompt(brief);
            self.hold(&h);
            return match prompt {
                Ok(_) => Ok(json!({
                    "session": session,
                    "worktree": d.workspace.display().to_string(),
                    "socket": d.socket.display().to_string(),
                    "model": model,
                    "mode_requested": mode,
                    "mode_error": e,
                    "daemon_started": d.started,
                })),
                Err(e) => Err(e),
            };
        }
        h.prompt(brief)?;
        self.hold(&h);
        Ok(json!({
            "session": session,
            "worktree": d.workspace.display().to_string(),
            "socket": d.socket.display().to_string(),
            "model": model,
            "mode": mode,
            "daemon_started": d.started,
        }))
    }

    fn hold(&self, h: &Arc<Head>) {
        let mut g = self.heads.lock().unwrap_or_else(|e| e.into_inner());
        g.insert(h.session.clone(), h.clone());
    }

    /// A line into a session the bridge holds. Queued behind a running turn, as a head's
    /// own prompt is — the daemon never rejects it.
    pub fn send(&self, session: &str, text: &str) -> Result<Value, String> {
        if text.trim().is_empty() {
            return Err("`text` is empty: an empty prompt starts no turn".into());
        }
        let h = self.head(session)?;
        h.prompt(text)?;
        Ok(json!({ "queued": true, "session": session }))
    }

    /// What the session is doing, in the words the orchestrator branches on.
    pub fn status(&self, session: &str) -> Result<Value, String> {
        let h = self.head(session)?;
        let s = h.snapshot();
        let turn = match &s.turn {
            Turn::Idle => json!("idle"),
            Turn::Running => json!("running"),
            Turn::Failed(why) => json!({ "state": "failed", "why": why }),
        };
        Ok(json!({
            "session": session,
            "state": s.word(),
            "turn": turn,
            "turn_id": s.turn_id,
            "todos": serde_json::to_value(&s.todos).unwrap_or(Value::Null),
            "last_message": s.last_message(),
            "cost_usd": s.meter.cost_micros_usd.map(|m| m as f64 / 1_000_000.0),
            "tokens": {
                "prompt": s.meter.prompt_tokens,
                "cached": s.meter.cached_tokens,
                "predicted": s.meter.predicted_tokens,
            },
            "pending_asks": s.asks.iter().map(|a| a.req_id.clone()).collect::<Vec<_>>(),
            // **What the daemon said to the last frame it refused.** An answer that arrives
            // after its card closed is refused rather than applied (`docs`: a reply to a
            // question asked ten minutes ago is input from nowhere), and an orchestrator
            // that could not see the refusal would keep believing it answered.
            "last_rejection": s.rejection,
            "seq": s.seq,
        }))
    }

    /// The cards still owed an answer, with the ladder each offers.
    pub fn asks(&self, session: &str) -> Result<Value, String> {
        let h = self.head(session)?;
        let asks = h.asks();
        Ok(json!({
            "session": session,
            "asks": asks.iter().map(|a| a.to_json()).collect::<Vec<_>>(),
        }))
    }

    /// **Answer a card.** The option must be one the ask offered; anything else is refused
    /// with the list, because an answer to a question nobody asked is input from nowhere.
    pub fn answer(
        &self,
        session: &str,
        ask: &str,
        option: &str,
        note: Option<&str>,
    ) -> Result<Value, String> {
        let h = self.head(session)?;
        h.answer(ask, option, note)?;
        Ok(json!({ "answered": true, "session": session, "ask": ask, "option": option }))
    }

    /// The worktree's branch against its merge base.
    pub fn diff(&self, session: &str, full: bool) -> Result<Value, String> {
        let h = self.head(session)?;
        let mut out = diff::diff(&h.worktree, full);
        out["session"] = json!(session);
        Ok(out)
    }

    /// Interrupt and leave. The session stays on disk, so any head can reopen it.
    pub fn stop(&self, session: &str) -> Result<Value, String> {
        let h = self.head(session)?;
        h.stop()?;
        let mut g = self.heads.lock().unwrap_or_else(|e| e.into_inner());
        g.remove(session);
        Ok(json!({ "stopped": true, "session": session }))
    }

    /// What this bridge is holding, and what each of them is doing.
    pub fn list(&self) -> Value {
        let heads: Vec<Arc<Head>> = {
            let g = self.heads.lock().unwrap_or_else(|e| e.into_inner());
            g.values().cloned().collect()
        };
        let mut rows: Vec<Value> = heads
            .iter()
            .map(|h| {
                let s = h.snapshot();
                json!({
                    "session": h.session,
                    "worktree": h.worktree.display().to_string(),
                    "socket": h.socket.display().to_string(),
                    "state": s.word(),
                    "pending_asks": s.asks.len(),
                    "last_message": first_line(s.last_message()),
                    "closed": s.closed,
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            a["session"]
                .as_str()
                .unwrap_or_default()
                .cmp(b["session"].as_str().unwrap_or_default())
        });
        json!({ "sessions": rows })
    }

    /// One held session, or a refusal naming the ones it does hold — a caller that mistyped
    /// an id should not have to ask `leticode_list` to find out.
    fn head(&self, session: &str) -> Result<Arc<Head>, String> {
        let g = self.heads.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(h) = g.get(session) {
            return Ok(h.clone());
        }
        let held: Vec<String> = g.keys().cloned().collect();
        Err(format!(
            "no session `{session}` is held by this bridge; it holds {}",
            if held.is_empty() {
                "none — `leticode_start` opens one".to_string()
            } else {
                held.join(", ")
            }
        ))
    }
}

/// The first line of a message, for a listing row — a status line is one line, and a
/// report of forty lines in a table is a table nobody reads.
fn first_line(text: &str) -> String {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .chars()
        .take(160)
        .collect()
}

/// **The eight tools, exactly as the design's table names them.**
///
/// The schemas are JSON Schema objects because MCP requires `inputSchema` to be one, and
/// `required` is stated rather than inferred: a client that sends `leticode_answer` without
/// an option should be told which field is missing, not have an argument defaulted.
pub fn list() -> Value {
    json!({ "tools": [
        tool("leticode_start",
             "Start leticode on a worktree and hold the session. Starts or joins that \
              folder's daemon, opens a session, sets the approval point, sends the brief as \
              the first prompt, and stays attached so the gate's asks reach this bridge. \
              Answers the session id every other tool takes. Run the `wait` verb as a \
              background command afterwards — its exit is the notification that the turn \
              ended or that a decision is pending.",
             json!({
                 "type": "object",
                 "properties": {
                     "worktree": { "type": "string", "description": "Absolute path to the checkout the work happens in." },
                     "brief": { "type": "string", "description": "The task, in full: what to build, the hard rules, and what to report." },
                     "model": { "type": "string", "description": "What answers the turns: a provider name (`deepseek`, `glm`, `grok`), `provider/model`, or a local model alias. Omitted: the daemon's own choice." },
                     "mode": { "type": "string", "description": "The approval point. Default `writes-allowed`: writes go through, commands and the network ask. `allow-all` needs the operator's confirmation and is refused by the daemon without it." }
                 },
                 "required": ["worktree", "brief"]
             })),
        tool("leticode_send",
             "Send an operator line into a running session. Queued behind a running turn as \
              the head's own prompt, so it never loses a race with what the model is doing; \
              the daemon starts a turn for it when the current one ends.",
             json!({
                 "type": "object",
                 "properties": {
                     "session": { "type": "string", "description": "The id `leticode_start` answered." },
                     "text": { "type": "string", "description": "What to say to the session." }
                 },
                 "required": ["session", "text"]
             })),
        tool("leticode_status",
             "What the session is doing: `running`, `idle`, `asking` or `failed`, the last \
              assistant message, the todos, the last turn's tokens and cost, and which asks \
              are pending. The report fields the design's later package adds are not here yet.",
             json!({
                 "type": "object",
                 "properties": {
                     "session": { "type": "string", "description": "The session id." }
                 },
                 "required": ["session"]
             })),
        tool("leticode_asks",
             "The permission asks still owed an answer: the ask id, the call (tool and \
              target), the sentence the gate wrote, and the options the ladder offers. \
              Answer one with `leticode_answer`; a turn stopped on a card does not continue \
              without one.",
             json!({
                 "type": "object",
                 "properties": {
                     "session": { "type": "string", "description": "The session id." }
                 },
                 "required": ["session"]
             })),
        tool("leticode_answer",
             "Answer a permission ask with one of the options the ask offered. Sent as the \
              protocol's Answer frame, so the decision lands in the corpus attributed to \
              this bridge, and the daemon takes it exactly as it takes the operator's own.",
             json!({
                 "type": "object",
                 "properties": {
                     "session": { "type": "string", "description": "The session id." },
                     "ask": { "type": "string", "description": "The ask id from `leticode_asks` (its `ask` field)." },
                     "option": { "type": "string", "description": "One option id the ask offered: `allow_once`, `allow_session`, `allow_always`, `reject_once`, `reject_always` — the ask says which." },
                     "note": { "type": "string", "description": "What the model is told, for a `deny_and_tell` option." }
                 },
                 "required": ["session", "ask", "option"]
             })),
        tool("leticode_diff",
             "The worktree's branch against its merge base: the commits it added \
              (`git log --oneline`) and the shape of the change (`git diff --stat`). The \
              full diff only when `full` is true — it is large and belongs in a review, not \
              in a status check.",
             json!({
                 "type": "object",
                 "properties": {
                     "session": { "type": "string", "description": "The session id; its worktree is where git runs." },
                     "full": { "type": "boolean", "description": "Include the whole diff. Default false." }
                 },
                 "required": ["session"]
             })),
        tool("leticode_stop",
             "Interrupt a running turn and detach this bridge from the session. The session \
              stays on disk, so `letibot --session <id>` or another head can reopen it.",
             json!({
                 "type": "object",
                 "properties": {
                     "session": { "type": "string", "description": "The session id." }
                 },
                 "required": ["session"]
             })),
        tool("leticode_list",
             "The sessions this bridge is holding, with their state and pending ask count. \
              It lists what this process holds, not what the daemon has on disk.",
             json!({ "type": "object", "properties": {} })),
    ]})
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": schema })
}

/// Which tool names this build answers — the list `tools/list` publishes, and the one
/// `tools/call` checks against, so a name can never be in one and not the other.
pub fn names() -> Vec<&'static str> {
    vec![
        "leticode_start",
        "leticode_send",
        "leticode_status",
        "leticode_asks",
        "leticode_answer",
        "leticode_diff",
        "leticode_stop",
        "leticode_list",
    ]
}

/// A string argument, or a refusal naming the field.
fn arg_str(args: &Value, key: &str) -> Result<String, String> {
    match args.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(format!("`{key}` must be a string")),
        None => Err(format!("`{key}` is required")),
    }
}

/// A string argument that may be absent.
fn arg_opt_str(args: &Value, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        Some(Value::Null) | None => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn arg_bool(args: &Value, key: &str, default: bool) -> Result<bool, String> {
    match args.get(key) {
        Some(Value::Bool(b)) => Ok(*b),
        Some(Value::Null) | None => Ok(default),
        Some(_) => Err(format!("`{key}` must be a boolean")),
    }
}

/// **Run one tool.** `Err` is a caller's mistake (an argument, a session this bridge does
/// not hold); a tool that ran and failed says so in its answer with `isError`.
pub fn call(bridge: &Bridge, name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "leticode_start" => {
            let worktree = arg_str(args, "worktree")?;
            let brief = arg_str(args, "brief")?;
            let model = arg_opt_str(args, "model")?;
            let mode = arg_opt_str(args, "mode")?.unwrap_or_else(|| DEFAULT_MODE.to_string());
            bridge.start(&worktree, &brief, model.as_deref(), &mode)
        }
        "leticode_send" => bridge.send(&arg_str(args, "session")?, &arg_str(args, "text")?),
        "leticode_status" => bridge.status(&arg_str(args, "session")?),
        "leticode_asks" => bridge.asks(&arg_str(args, "session")?),
        "leticode_answer" => {
            let session = arg_str(args, "session")?;
            let ask = arg_str(args, "ask")?;
            let option = arg_str(args, "option")?;
            let note = arg_opt_str(args, "note")?;
            bridge.answer(&session, &ask, &option, note.as_deref())
        }
        "leticode_diff" => {
            let full = arg_bool(args, "full", false)?;
            bridge.diff(&arg_str(args, "session")?, full)
        }
        "leticode_stop" => bridge.stop(&arg_str(args, "session")?),
        "leticode_list" => Ok(bridge.list()),
        other => Err(format!(
            "unknown tool `{other}`; this build serves {}",
            names().join(", ")
        )),
    }
}

/// How long the `wait` verb will wait when the caller named no `--timeout`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

/// **The wake-up.** Attach, wait for the first thing that needs a person, and say which it
/// was — the exit code is the notification, and the line on stdout is the payload.
///
/// Codes: `0` the turn ended (the last assistant message is on stdout), `10` an ask is
/// pending (the ask is on stdout), `20` the session failed, `30` the timeout passed.
pub fn wait(worktree: &str, session: &str, deadline: Instant) -> (i32, String) {
    let d = match daemon::resolve(Path::new(worktree)) {
        Ok(d) => d,
        Err(e) => return (20, e),
    };
    let h = match Head::attach(&d, session) {
        Ok(h) => h,
        Err(e) => return (20, e),
    };
    match h.wait(deadline) {
        Attention::TurnEnded { message } => (0, message),
        Attention::Ask(ask) => (10, ask.to_json().to_string()),
        Attention::Failed { why } => (20, why),
        Attention::Timeout => (30, String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Eight tools, each with a schema and a required list** — the count and the names the
    /// design's table fixes. A tool added to `tools/list` and forgotten in `call` (or the
    /// other way round) is the defect this pairs against.
    #[test]
    fn the_eight_tools_are_listed_with_schemas() {
        let listed = list();
        let tools = listed["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 8, "{listed}");
        let mut named: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        named.sort_unstable();
        let mut want = names();
        want.sort_unstable();
        assert_eq!(named, want);
        for t in tools {
            let name = t["name"].as_str().expect("a name");
            let schema = &t["inputSchema"];
            assert_eq!(schema["type"], "object", "{name}: {schema}");
            assert!(
                schema["properties"].is_object(),
                "{name} has no properties: {schema}"
            );
            assert!(
                !t["description"].as_str().unwrap_or_default().is_empty(),
                "{name} has no description"
            );
        }
        // And `call` knows every name it publishes: an unknown tool is a refusal that lists
        // them, which is what a client sees when it asks for one that is not here.
        let e = call(&Bridge::new(), "leticode_nope", &json!({})).expect_err("unknown");
        assert!(e.contains("leticode_start"), "{e}");
    }

    /// A missing or mistyped argument is refused by field name, before anything is sent.
    #[test]
    fn a_missing_argument_names_the_field() {
        let b = Bridge::new();
        let e = call(&b, "leticode_start", &json!({ "worktree": "/tmp" })).expect_err("no brief");
        assert!(e.contains("`brief`"), "{e}");
        let e = call(
            &b,
            "leticode_diff",
            &json!({ "session": "s-1", "full": "yes" }),
        )
        .expect_err("not a boolean");
        assert!(e.contains("`full`"), "{e}");
    }

    /// A session this bridge does not hold is refused by name, and the refusal says what it
    /// does hold — the caller's next move is one call, not a listing.
    #[test]
    fn an_unknown_session_is_refused_with_what_is_held() {
        let b = Bridge::new();
        let e = b.status("s-nope").expect_err("not held");
        assert!(e.contains("s-nope"), "{e}");
        assert!(e.contains("leticode_start"), "{e}");
        // And an empty bridge lists empty, rather than failing: a listing is a question.
        assert_eq!(b.list()["sessions"].as_array().map(|v| v.len()), Some(0));
    }

    /// The listing row is one line: a status table with a report inside it is a table nobody
    /// reads.
    #[test]
    fn a_listing_row_is_one_line() {
        assert_eq!(
            first_line("\n\n  hello there\nsecond line\n"),
            "hello there"
        );
        assert_eq!(first_line(""), "");
    }
}
