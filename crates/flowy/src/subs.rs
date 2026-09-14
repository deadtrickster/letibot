//! Subscriptions to entities: a todo somebody decided to watch for comments, a
//! diagram for a change, a thread for its replies.
//!
//! The inbox delivers what is said TO a seat. A subscription is the other
//! direction — the seat deciding what it wants to hear about — and the three
//! kinds ride three different transports, because that is what the node has:
//!
//! | kind | how it is heard | what the node offers |
//! |---|---|---|
//! | thread | the inbox, with the thread in the attention table's override set | nothing extra — chat is already delivered, the table decides |
//! | todo | `GET /api/stream?topics=todos`, the node's own SSE, an envelope per move | push; the envelope says THAT a row moved, and the row is re-read |
//! | artifact | `GET /api/artifact/{id}`, compared to a baseline every [`ARTIFACT_TICK`] | a body edit writes no event, so there is nothing to subscribe to; polling is honest about that |
//!
//! Envelopes, not deltas — the stream's own rule, kept: on a todo envelope the
//! watcher re-reads the events above the envelope's hlc and hands over the one
//! about the watched row. A duplicate is a wasted read, an out-of-order one is
//! invisible, and nothing partial is ever applied.
//!
//! The baseline for an artifact is read **at subscription**, not on the first
//! tick — a baseline read half a minute late has already missed the change it
//! exists to detect (`exec/monitor.rs`, on path monitors, says the same).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::client::{Artifact, Node, NodeError};

/// How often a watched artifact is re-read. Unhurried: a diagram changes on a
/// person's timescale, and a subscription exists because nobody is waiting on
/// it this instant.
pub const ARTIFACT_TICK: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "lowercase")]
pub enum Subscription {
    Thread(String),
    Todo(String),
    Artifact(String),
}

impl Subscription {
    pub fn parse(kind: &str, id: &str) -> Option<Subscription> {
        let id = id.trim();
        if id.is_empty() {
            return None;
        }
        match kind.trim().to_ascii_lowercase().as_str() {
            "thread" => Some(Subscription::Thread(id.into())),
            "todo" | "row" => Some(Subscription::Todo(id.into())),
            "artifact" | "diagram" | "memory" | "note" => Some(Subscription::Artifact(id.into())),
            _ => None,
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Subscription::Thread(s) | Subscription::Todo(s) | Subscription::Artifact(s) => s,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Subscription::Thread(_) => "thread",
            Subscription::Todo(_) => "todo",
            Subscription::Artifact(_) => "artifact",
        }
    }
}

impl std::fmt::Display for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.kind(), self.id())
    }
}

/// Where a change goes: the seat installs one that fans out to the sessions.
pub type ChangeSink = Arc<dyn Fn(EntityChange) + Send + Sync>;

/// A change on a watched entity, as one sentence and the subscription it is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityChange {
    pub sub: Subscription,
    pub what: String,
}

/// The seat-wide watcher: the union of every attached session's todo and
/// artifact subscriptions, the transports behind them, and a sink.
pub struct EntityWatch {
    node: Mutex<Node>,
    artifacts: Mutex<BTreeMap<String, Artifact>>,
    todos: Mutex<BTreeMap<String, usize>>,
    /// Where a change goes. The seat installs a closure that fans out to the
    /// attached sessions.
    sink: Mutex<Option<ChangeSink>>,
    artifact_thread: AtomicBool,
    todo_thread: AtomicBool,
    stop: AtomicBool,
}

impl EntityWatch {
    pub fn new(node: Node) -> Arc<EntityWatch> {
        Arc::new(EntityWatch {
            node: Mutex::new(node),
            artifacts: Mutex::new(BTreeMap::new()),
            todos: Mutex::new(BTreeMap::new()),
            sink: Mutex::new(None),
            artifact_thread: AtomicBool::new(false),
            todo_thread: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        })
    }

    pub fn set_sink(&self, f: ChangeSink) {
        *self.sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    pub fn set_node(&self, node: Node) {
        *self.node.lock().unwrap_or_else(|e| e.into_inner()) = node;
    }

    fn node(&self) -> Node {
        self.node.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    fn emit(&self, c: EntityChange) {
        let sink = self.sink.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(f) = sink {
            f(c);
        }
    }

    /// Start watching. For an artifact the baseline is read NOW and the read's
    /// outcome is returned, so a subscription to a row that cannot be read is a
    /// refusal and not a watch that fires never.
    pub fn add(self: &Arc<Self>, sub: &Subscription) -> Result<String, NodeError> {
        match sub {
            Subscription::Thread(_) => Ok("a thread is watched through the attention table".into()),
            Subscription::Artifact(id) => {
                let a = self.node().artifact(id)?;
                let summary = format!(
                    "watching artifact {id} `{}` ({}), updated {}, body {} bytes",
                    a.title,
                    if a.kind.is_empty() {
                        a.type_.clone()
                    } else {
                        format!("{}/{}", a.type_, a.kind)
                    },
                    a.updated,
                    a.body.len()
                );
                self.artifacts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id.clone(), a);
                self.ensure_artifact_thread();
                Ok(summary)
            }
            Subscription::Todo(id) => {
                // Read once so an unreadable row refuses here rather than being
                // watched forever in silence.
                let a = self.node().artifact(id)?;
                let summary = format!(
                    "watching todo {id} `{}` (status {}, assignee {}) for notes and moves",
                    a.title,
                    if a.status.is_empty() { "?" } else { &a.status },
                    if a.assignee.is_empty() {
                        "nobody"
                    } else {
                        &a.assignee
                    }
                );
                *self
                    .todos
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .entry(id.clone())
                    .or_insert(0) += 1;
                self.ensure_todo_thread();
                Ok(summary)
            }
        }
    }

    pub fn remove(&self, sub: &Subscription) {
        match sub {
            Subscription::Thread(_) => {}
            Subscription::Artifact(id) => {
                self.artifacts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(id);
            }
            Subscription::Todo(id) => {
                let mut t = self.todos.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(n) = t.get_mut(id) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        t.remove(id);
                    }
                }
            }
        }
    }

    pub fn watched(&self) -> Vec<Subscription> {
        let mut out: Vec<Subscription> = self
            .artifacts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .map(|k| Subscription::Artifact(k.clone()))
            .collect();
        out.extend(
            self.todos
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .keys()
                .map(|k| Subscription::Todo(k.clone())),
        );
        out
    }

    /// Look at every watched artifact once. Public so a test can drive it
    /// without waiting a tick.
    pub fn look_at_artifacts(&self) {
        let ids: Vec<String> = self
            .artifacts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        let node = self.node();
        for id in ids {
            let Ok(now) = node.artifact(&id) else {
                continue;
            };
            let mut map = self.artifacts.lock().unwrap_or_else(|e| e.into_inner());
            let Some(was) = map.get(&id) else { continue };
            if *was == now {
                continue;
            }
            let mut what = format!("artifact {id} `{}` changed", now.title);
            if was.title != now.title {
                what.push_str(&format!("; title was `{}`", was.title));
            }
            if was.status != now.status {
                what.push_str(&format!("; status {} → {}", was.status, now.status));
            }
            if was.assignee != now.assignee {
                what.push_str(&format!("; assignee {} → {}", was.assignee, now.assignee));
            }
            if was.body != now.body {
                what.push_str(&format!(
                    "; body {} → {} bytes",
                    was.body.len(),
                    now.body.len()
                ));
            }
            if was.updated != now.updated {
                what.push_str(&format!("; updated {}", now.updated));
            }
            what.push_str(". Re-read it: this is an envelope, not the change.");
            map.insert(id.clone(), now);
            drop(map);
            self.emit(EntityChange {
                sub: Subscription::Artifact(id),
                what,
            });
        }
    }

    fn ensure_artifact_thread(self: &Arc<Self>) {
        if self.artifact_thread.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak = Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name("letibot-flowy-artifacts".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(ARTIFACT_TICK);
                    let Some(me) = weak.upgrade() else { return };
                    if me.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    if me
                        .artifacts
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .is_empty()
                    {
                        me.artifact_thread.store(false, Ordering::SeqCst);
                        return;
                    }
                    me.look_at_artifacts();
                }
            });
    }

    /// One todo envelope from the stream. Public so a test can feed one.
    pub fn on_todo_envelope(&self, data: &str) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
            return;
        };
        let artifact = v.get("artifact").and_then(|a| a.as_str()).unwrap_or("");
        if artifact.is_empty()
            || !self
                .todos
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(artifact)
        {
            return;
        }
        let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let hlc = v.get("hlc").and_then(|h| h.as_i64()).unwrap_or(0);
        // Re-read, from just under the envelope: the event about this row.
        let found = self
            .node()
            .events_since(hlc.saturating_sub(1), Some(kind), None, 50)
            .ok()
            .and_then(|evs| evs.into_iter().find(|e| e.artifact == artifact));
        let what = match found {
            Some(e) => {
                let who = if e.actor_name().is_empty() {
                    e.actor.clone()
                } else {
                    e.actor_name().to_string()
                };
                let mut w = format!("todo {artifact}: {} by {who}", e.kind);
                if !e.created.is_empty() {
                    w.push_str(&format!(" at {}", e.created));
                }
                if !e.body.is_empty() {
                    w.push_str(&format!("\n  {}", e.body.replace('\n', "\n  ")));
                }
                w
            }
            None => format!(
                "todo {artifact}: {kind} (hlc {hlc}); the event itself was not readable \
                 from here — re-read the row"
            ),
        };
        self.emit(EntityChange {
            sub: Subscription::Todo(artifact.to_string()),
            what,
        });
    }

    fn ensure_todo_thread(self: &Arc<Self>) {
        if self.todo_thread.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak = Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name("letibot-flowy-todos".into())
            .spawn(move || {
                let mut last_id: Option<String> = None;
                let mut backoff = Duration::from_secs(1);
                loop {
                    let Some(me) = weak.upgrade() else { return };
                    if me.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    if me
                        .todos
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .is_empty()
                    {
                        me.todo_thread.store(false, Ordering::SeqCst);
                        return;
                    }
                    let node = me.node();
                    let connected = Instant::now();
                    let weak2 = Arc::downgrade(&me);
                    let mut seen: Option<String> = last_id.clone();
                    let r = node.stream("todos", last_id.as_deref(), |frame| {
                        let Some(me) = weak2.upgrade() else {
                            return Ok(letibot_http::Flow::Stop);
                        };
                        // On every frame — heartbeats included — ask whether
                        // anybody still wants this. The stream only returns on
                        // a close, so this is where "no subscriptions" ends it.
                        if me.stop.load(Ordering::SeqCst)
                            || me
                                .todos
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .is_empty()
                        {
                            return Ok(letibot_http::Flow::Stop);
                        }
                        if let Some(id) = &frame.id {
                            seen = Some(id.clone());
                        }
                        for d in &frame.data {
                            me.on_todo_envelope(d);
                        }
                        Ok(letibot_http::Flow::Continue)
                    });
                    last_id = seen;
                    drop(me);
                    match r {
                        Ok(()) => backoff = Duration::from_secs(1),
                        Err(_) => {
                            // A connection that lived a while was a real one; a
                            // failure straight away is the node being away.
                            if connected.elapsed() > Duration::from_secs(30) {
                                backoff = Duration::from_secs(1);
                            }
                            std::thread::sleep(backoff);
                            backoff = (backoff * 2).min(Duration::from_secs(30));
                        }
                    }
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_kinds_and_refuses_an_empty_id() {
        assert_eq!(
            Subscription::parse("todo", "01M"),
            Some(Subscription::Todo("01M".into()))
        );
        assert_eq!(
            Subscription::parse("diagram", "01M"),
            Some(Subscription::Artifact("01M".into()))
        );
        assert_eq!(
            Subscription::parse("thread", "T"),
            Some(Subscription::Thread("T".into()))
        );
        assert_eq!(Subscription::parse("todo", " "), None);
        assert_eq!(Subscription::parse("room", "x"), None);
    }
}
