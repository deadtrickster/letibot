//! **The ten verbs**, and the map of sessions the bridge holds.
//!
//! `docs/claude-code-bridge.md` fixes the table: each tool's input, and what it answers.
//! Three things in it are worth naming here because they are the design rather than a
//! shape:
//!
//! * **`leticode_start` keeps the head.** It creates the session, sends the brief and
//!   stays attached on its own socket, because a session nobody is attached to cannot ask
//!   anybody anything — and the whole point of running leticode from Claude Code is that
//!   the gate has somebody to ask.
//! * **`leticode_attach` joins, and does not start.** The same head, on a session somebody
//!   else opened — the review queue's `s-gatekeeper`, most of the time — and by default
//!   read-only: a watcher never sends, never answers, never interrupts (B6).
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

/// The default page size of `leticode_events`. Fifty is a screenful of progress: a turn
/// that produced more has produced more than a caller reads in one go.
pub const EVENTS_LIMIT: usize = 50;

/// What the bridge holds: one attached head per session it started or joined, by session id.
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
    ///
    /// **Which session the brief runs in is the first thing that has to be right.** When
    /// this call started the daemon, the daemon has already opened its first session —
    /// eagerly, and it is the session its worker is holding — so that is the session to
    /// use: one brief is one conversation, and a second one minted beside it only creates
    /// the possibility of the reply naming one session while the work happens in the other.
    /// When the daemon was already up, its first session is somebody else's conversation,
    /// so a new one is created and the head attaches **to that id** before the brief is
    /// sent; `Head::attach` then refuses to hand back a head the daemon seated elsewhere.
    ///
    /// The first real run got this wrong the other way round: the answer named the session
    /// it had just created and the brief ran in the daemon's own first one, so
    /// `leticode_status` watched an empty session report `idle` while a turn was happening
    /// one session over.
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
        let (session, opened) = if d.started {
            (head::open_default(&d)?, "daemon")
        } else {
            (head::create_session(&d, "")?, "new")
        };
        let h = Arc::new(Head::attach(&d, &session)?);
        let mut answer = json!({
            "session": session,
            "worktree": d.workspace.display().to_string(),
            "socket": d.socket.display().to_string(),
            "model": model,
            "daemon_started": d.started,
            // Which of the two sessions this is, because the two look the same in an id
            // and a caller that expected to have opened one should be able to see that it
            // inherited the daemon's.
            "session_opened": opened,
        });
        // Before the brief, so the first tool call already runs at the point the caller
        // asked for: `Mode` is queued like a prompt and takes effect from the next call.
        if let Err(e) = h.set_mode(mode) {
            // A session that cannot be moved is still a session — say so and let the brief
            // run at the daemon's own point rather than losing the session to the failure.
            answer["mode_requested"] = json!(mode);
            answer["mode_error"] = json!(e);
        } else {
            answer["mode"] = json!(mode);
        }
        let queued = h.prompt(brief);
        // Held even when the send failed: the session exists, and a caller that wants to
        // ask why must be able to reach it by the id it was told.
        self.hold(&h);
        queued?;
        Ok(answer)
    }

    fn hold(&self, h: &Arc<Head>) {
        let mut g = self.heads.lock().unwrap_or_else(|e| e.into_inner());
        g.insert(h.session.clone(), h.clone());
    }

    /// **Join a session this bridge did not start** — `leticode_attach` (B6).
    ///
    /// The operator cannot see who in the merge queue is doing what: the review queue's own
    /// session lives in the operator's daemon and spawns a reviewer per row, and until now
    /// the only way to follow it from Claude Code was to read the store and guess. This
    /// attaches a head to it — or to any other session a folder's daemon holds — and
    /// everything else in the table then works on it: `status`, `events`, `asks`, `list`,
    /// `wait`, `diff`. What it does not do is the brief: a session that already exists has
    /// already been told what to do.
    ///
    /// **It joins a running daemon and never starts one.** A daemon nobody asked for is the
    /// defect Q1 fixed in the other direction — a package's own daemon taking a repository's
    /// merge-queue lease — and here the folder is usually the operator's: `leticode_start` is
    /// the tool that starts a daemon, and this one refuses rather than *helpfully* beginning a
    /// second daemon over a workspace whose daemon is momentarily busy.
    ///
    /// `watch_only` is the default and the whole point (see [`Head::attach_watching`]): the
    /// session keeps the head that holds it, and this one only watches — no prompt, no
    /// answer, no interrupt — so a queue the operator is also watching is not steered by a
    /// second hand. `watch_only: false` exists for the other job, driving a session this
    /// bridge did not open (a resumed package, a session a person left idle).
    pub fn attach(&self, worktree: &str, session: &str, watch_only: bool) -> Result<Value, String> {
        let session = session_name(session)?;
        let d = daemon::resolve(Path::new(worktree))?;
        if !daemon::is_up(&d.socket) {
            return Err(format!(
                "no daemon is up for {}: nothing is accepting on {}. `leticode_attach` joins a \
                 running daemon and never starts one — a second daemon for one folder is how \
                 two processes come to interleave rows into one transcript. `leticode_start` \
                 is the tool that starts a daemon",
                d.workspace.display(),
                d.socket.display()
            ));
        }
        // **A second attach to a session this bridge already holds is refused.** The map is
        // keyed by session id, so a second entry would silently replace the first: the socket
        // that answers its cards would shut down, the ring would start over, and a caller
        // holding a `since` cursor would read a stream that went backwards.
        if let Some(held) = self.held(&session) {
            return Err(format!(
                "this bridge already holds `{session}` ({}); an attach would replace that head \
                 and restart the progress ring `leticode_events` counts from. `leticode_list` \
                 says what is held",
                if held.watch_only {
                    "watching it"
                } else {
                    "driving it"
                }
            ));
        }
        let h = Arc::new(if watch_only {
            Head::attach_watching(&d, &session)?
        } else {
            Head::attach(&d, &session)?
        });
        let s = h.snapshot();
        let answer = json!({
            "session": session,
            "worktree": d.workspace.display().to_string(),
            "socket": d.socket.display().to_string(),
            // `resolve` never starts anything, so this is the field's own answer and not a
            // constant written into a reply: an attach that had started a daemon would be
            // the bug this tool exists to not have.
            "daemon_started": d.started,
            "watch_only": watch_only,
            "can_decide": !watch_only,
            "state": s.word(),
            "pending_asks": s.asks.iter().map(|a| a.req_id.clone()).collect::<Vec<_>>(),
            "last_message": first_line(s.last_message()),
        });
        self.hold(&h);
        Ok(answer)
    }

    /// The head this bridge already holds for a session, if any.
    fn held(&self, session: &str) -> Option<Arc<Head>> {
        self.heads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session)
            .cloned()
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
        let mut out = json!({
            "session": session,
            "state": s.word(),
            // **Whether this head may act on the session at all** (B6): a caller that cannot
            // see the difference asks twice why `leticode_send` refused.
            "watch_only": h.watch_only,
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
            // **The package so far**: every round this head watched, summed. `cost_usd`
            // and `tokens` above are the last round only.
            "spent": {
                "cost_usd": s.spent.cost_micros_usd.map(|m| m as f64 / 1_000_000.0),
                "prompt": s.spent.prompt_tokens,
                "cached": s.spent.cached_tokens,
                "predicted": s.spent.predicted_tokens,
                "rounds": s.rounds,
            },
            // The model's own background jobs still running: each will wake it again, so an
            // idle session with any of these is waiting, not done.
            "jobs_open": s.jobs_open.iter().collect::<Vec<_>>(),
            // Whether an unfinished todo needs one of them: `wait` holds while this is true.
            "waiting_on_jobs": s.waiting_on_jobs(),
            "pending_asks": s.asks.iter().map(|a| a.req_id.clone()).collect::<Vec<_>>(),
            // **What the daemon said to the last frame it refused.** An answer that arrives
            // after its card closed is refused rather than applied (`docs`: a reply to a
            // question asked ten minutes ago is input from nowhere), and an orchestrator
            // that could not see the refusal would keep believing it answered.
            "last_rejection": s.rejection,
            "seq": s.seq,
        });
        // **The brief's report, when the last message is one.** Absent, not empty, when it
        // is not: a status answer with four blank fields would have a reviewer asking
        // whether the run produced nothing or the parser failed, and this build says
        // nothing rather than something untrue.
        //
        // The newest report rather than the last message: a run that reports and is then
        // woken by its own jobs ends on *nothing new in them*, and the report is the one
        // before. `report_is_last` says which, so a reviewer knows words came after it.
        if let Some(report) = crate::report::parse(s.last_message()) {
            out["report"] = report.to_json();
            out["report_is_last"] = json!(true);
        } else if let Some(report) = s.last_report.as_deref().and_then(crate::report::parse) {
            out["report"] = report.to_json();
            out["report_is_last"] = json!(false);
        }
        Ok(out)
    }

    /// **The progress since a cursor**: tool calls, results, jobs, turns, asks, warnings and
    /// the first line of what was said — and never the banner, the memory watcher or a
    /// payload.
    ///
    /// A cursor older than the ring is answered with what remains *and* a `dropped` count:
    /// a caller that believes it has the whole story when it has the tail of it would read
    /// a report of ten calls as a turn that made ten calls.
    pub fn events(
        &self,
        session: &str,
        since: Option<u64>,
        limit: Option<u64>,
    ) -> Result<Value, String> {
        let h = self.head(session)?;
        let limit = limit
            .unwrap_or(EVENTS_LIMIT as u64)
            .clamp(1, head::RING as u64) as usize;
        let page = h.events(since.unwrap_or(0), limit);
        let mut out = json!({
            "session": session,
            "entries": page.entries.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
            "next": page.next,
        });
        if page.dropped > 0 {
            out["dropped"] = json!(page.dropped);
        }
        Ok(out)
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
                    "watch_only": h.watch_only,
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

/// **The session an attach names.**
///
/// Most attaches name an id — `s-gatekeeper` above all — but the review queue's session is
/// the one session in the tree whose id is a *constant*, the same string in every daemon on
/// every box, and a name a person types has to be that constant rather than a second
/// spelling of it. `gatekeeper` is the name; [`letibot_sessionlog::GATEKEEPER_SESSION`] is
/// the id it resolves to, and `harnessd`'s `sessions::gatekeeper::SESSION` is the same value
/// under the daemon's own name.
fn session_name(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(
            "`session` is empty: an attach has to name the session it joins — an id, or \
                    `gatekeeper` for the review queue's"
                .into(),
        );
    }
    Ok(match raw {
        "gatekeeper" => letibot_sessionlog::GATEKEEPER_SESSION.to_string(),
        other => other.to_string(),
    })
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

/// **The ten tools, exactly as the design's table names them.**
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
        tool("leticode_attach",
             "Join a session that is already running — the merge queue's own `s-gatekeeper`, a \
              package a person resumed, any session a folder's daemon holds — and hold it \
              the way `leticode_start` holds one, without a brief. It JOINS a running daemon and \
              never starts one: with nothing listening on that folder's socket it refuses and \
              says so, because a second daemon for one folder is how two processes come to \
              interleave rows into one transcript. `watch_only` (the default) makes this a \
              WATCHING head: it reads the turn, the last message, the todos, the pending asks \
              and the progress ring, and `leticode_send`, `leticode_answer` and `leticode_stop` \
              then refuse, because the head that holds the session is somebody else's. Its \
              reviewers are child sessions and `leticode_events` shows each of them starting \
              and ending; following one into its own turns needs a second `leticode_attach` on \
              that child's id.",
             json!({
                 "type": "object",
                 "properties": {
                     "worktree": { "type": "string", "description": "Absolute path to a folder of the checkout whose daemon holds the session — the daemon is found by the workspace's hash, so a worktree of the same repository is the same daemon." },
                     "session": { "type": "string", "description": "The session id, or the name `gatekeeper` for the review queue's session (`s-gatekeeper`, the one id every daemon spells the same)." },
                     "watch_only": { "type": "boolean", "description": "Default true: watch and never act — no prompt, no answer, no interrupt. False drives the session as `leticode_start` would, and takes over answering its asks." }
                 },
                 "required": ["worktree", "session"]
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
              are pending. When that message is the brief's closing report, it also comes \
              back parsed: `report.built`, `report.tested`, `report.could_not`, \
              `report.contracts` and the `report.commits` it named — the newest report, \
              with `report_is_last` false when words came after it. `spent` sums every \
              round since the bridge attached; `cost_usd` and `tokens` are the last round.",
             json!({
                 "type": "object",
                 "properties": {
                     "session": { "type": "string", "description": "The session id." }
                 },
                 "required": ["session"]
             })),
        tool("leticode_events",
             "The session's progress since a cursor, folded to one line each: tool calls (name \
              and what they act on), their outcomes, background jobs starting and ending, \
              turns, permission asks opening and being answered, the reviewer subagents a \
              session spawns (their slug, the branch they were given, and how they ended — the \
              review queue's children are how its work appears here), warnings and failures, \
              and the first line of each assistant message. Never the banner, the memory \
              watcher or a payload — for those, read the transcript. Pass the `next` of one \
              answer as the `since` of the next; a cursor the ring has outlived is answered \
              with what remains plus `dropped`, the count it can no longer show you.",
             json!({
                 "type": "object",
                 "properties": {
                     "session": { "type": "string", "description": "The session id." },
                     "since": { "type": "integer", "minimum": 0, "description": "The `next` cursor of the last answer. Omitted: everything the ring still holds." },
                     "limit": { "type": "integer", "minimum": 1, "description": "How many entries to answer with. Default 50; the ring holds 500." }
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
             "The sessions this bridge is holding, with their state, pending ask count and \
              whether each is only watched (`leticode_attach`'s default) or driven. It lists \
              what this process holds, not what the daemon has on disk.",
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
        "leticode_attach",
        "leticode_send",
        "leticode_status",
        "leticode_events",
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

/// A whole-number argument that may be absent — a cursor, a limit.
fn arg_count(args: &Value, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        Some(Value::Null) | None => Ok(None),
        Some(Value::Number(n)) => match n.as_u64() {
            Some(v) => Ok(Some(v)),
            None => Err(format!("`{key}` must be a whole number, and `{n}` is not")),
        },
        Some(other) => Err(format!("`{key}` must be a whole number, not `{other}`")),
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
        "leticode_attach" => {
            let worktree = arg_str(args, "worktree")?;
            let session = arg_str(args, "session")?;
            let watch_only = arg_bool(args, "watch_only", true)?;
            bridge.attach(&worktree, &session, watch_only)
        }
        "leticode_send" => bridge.send(&arg_str(args, "session")?, &arg_str(args, "text")?),
        "leticode_status" => bridge.status(&arg_str(args, "session")?),
        "leticode_events" => {
            let session = arg_str(args, "session")?;
            let since = arg_count(args, "since")?;
            let limit = arg_count(args, "limit")?;
            bridge.events(&session, since, limit)
        }
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
/// **The head this verb attaches is a WATCHER** (B6). A `wait` is a process that never
/// answers anything and is usually gone within minutes, so declaring it able to decide is the
/// lie the caps field exists against: `Answers::ask` reads `deciding_heads` before it posts,
/// and a session whose only head was a `wait` would post a card, be watched exit `10` with it,
/// and then sit on that card until its deadline — a timeout reported as nobody having
/// answered a question that nobody was ever able to answer. As a watcher it still SEES the
/// cards (the daemon publishes to every attached head), which is all it ever did with them.
///
/// Codes: `0` the turn ended (the last assistant message is on stdout), `10` an ask is
/// pending (the ask is on stdout), `20` the session failed, `30` the timeout passed.
pub fn wait(worktree: &str, session: &str, deadline: Instant) -> (i32, String) {
    let d = match daemon::resolve(Path::new(worktree)) {
        Ok(d) => d,
        Err(e) => return (20, e),
    };
    let h = match Head::attach_watching(&d, session) {
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

    /// **Ten tools, each with a schema and a required list** — the count and the names the
    /// design's table fixes, with `leticode_events` added by package B2 and `leticode_attach`
    /// by B6. A tool added to `tools/list` and forgotten in `call` (or the other way round)
    /// is the defect this pairs against.
    #[test]
    fn the_ten_tools_are_listed_with_schemas() {
        let listed = list();
        let tools = listed["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 10, "{listed}");
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
        // A cursor is a whole number, and a string or a negative one says which field it was
        // rather than being read as zero — a cursor silently read as zero re-sends a ring.
        for bad in [json!(-1), json!("12"), json!(1.5)] {
            let e = call(
                &b,
                "leticode_events",
                &json!({ "session": "s-1", "since": bad }),
            )
            .expect_err("not a whole number");
            assert!(e.contains("`since`"), "{bad}: {e}");
        }
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

    /// **`gatekeeper` is the review queue's id, not a second spelling of it** (B6): the name
    /// resolves to the constant every daemon spells the same, an id passes through, and an
    /// empty one is refused rather than read as "whatever the daemon calls current" — a
    /// watcher that landed on somebody else's session would report on the wrong conversation.
    #[test]
    fn the_gatekeeper_name_resolves_to_the_queues_id() {
        assert_eq!(
            session_name("gatekeeper").expect("a name"),
            letibot_sessionlog::GATEKEEPER_SESSION
        );
        assert_eq!(session_name(" s-1789 ").expect("an id"), "s-1789");
        let e = session_name("  ").expect_err("an attach names a session");
        assert!(e.contains("`session` is empty"), "{e}");
        assert!(e.contains("gatekeeper"), "{e}");
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
