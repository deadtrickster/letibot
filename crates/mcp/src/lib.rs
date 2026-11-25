//! An MCP client for `letibot-tools`' catalogue seam: stdio and streamable-HTTP JSON-RPC, blocking,
//! one lock per server. A start failure is kept as a [`Failure`] while the rest serve, a server
//! that dies is restarted with backoff, and [`Catalog::reconcile`] applies a new config live.

mod wire;

/// The test server, for another crate's tests to run as a child.
#[cfg(feature = "fake")]
#[doc(hidden)]
pub mod fake;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use letibot_tools::builtins::external::mcp::{
    McpCallResult, McpCatalog, McpError, McpServer, McpToolSpec,
};
use serde_json::{Value, json};

use wire::{TOOLS_CHANGED, Wire, WireError};

/// The protocol revision this client speaks first; a server answering an older one is accepted.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// A call's default deadline, dsh's 60 s.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// How one server is reached.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Transport {
    /// A child process speaking newline-delimited JSON-RPC on stdin and stdout.
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        /// The directory it runs in; `None` is the daemon's own.
        cwd: Option<PathBuf>,
    },
    /// Streamable HTTP over `http://` or `https://`.
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

/// One `[mcp.NAME]` table, resolved.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServerSpec {
    pub name: String,
    pub transport: Transport,
    /// Per request: start, listing and each call.
    pub timeout: Duration,
}

/// A server that did not start, and why, in words for the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub server: String,
    pub why: String,
}

/// The reconnect ladder: the first retry is at once, then 500 ms doubling to 30 s.
fn backoff(failed: u32) -> Duration {
    let ms = 500u64.saturating_mul(1 << failed.saturating_sub(1).min(16));
    Duration::from_millis(ms.min(30_000))
}

/// One live connection.
struct Client {
    wire: Box<dyn Wire>,
    next_id: u64,
    timeout: Duration,
    /// Set once the server is gone, so later calls say so without writing to a dead pipe.
    dead: Option<String>,
}

impl Client {
    fn connect(
        spec: &ServerSpec,
        spawn: &dyn letibot_tools::exec::Spawn,
    ) -> Result<(Client, Vec<McpToolSpec>), String> {
        let wire = wire::open(spec, spawn)?;
        let mut client = Client {
            wire,
            next_id: 1,
            timeout: spec.timeout,
            dead: None,
        };
        let init = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "letibot", "version": env!("CARGO_PKG_VERSION")},
                }),
            )
            .map_err(|e| format!("initialize: {}", err_text(&e)))?;
        if init.get("protocolVersion").is_none() {
            return Err(format!(
                "initialize: the answer names no protocolVersion: {init}"
            ));
        }
        client
            .wire
            .notify(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .map_err(|e| format!("notifications/initialized: {e}"))?;
        let tools = client
            .list_tools()
            .map_err(|e| format!("tools/list: {}", err_text(&e)))?;
        Ok((client, tools))
    }

    fn list_tools(&mut self) -> Result<Vec<McpToolSpec>, McpError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        // A bound on pages, so a server that always returns a cursor cannot hold the open.
        for _ in 0..64 {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let page = self.request("tools/list", params)?;
            for t in page
                .get("tools")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(name) = t.get("name").and_then(Value::as_str) else {
                    continue;
                };
                tools.push(McpToolSpec {
                    name: name.to_string(),
                    description: t
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    parameters: t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"})),
                });
            }
            cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        if let Some(why) = &self.dead {
            return Err(McpError::Transport(why.clone()));
        }
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let answer = match self.wire.request(id, &msg, self.timeout) {
            Ok(a) => a,
            Err(WireError::Gone(why)) => {
                self.dead = Some(why.clone());
                return Err(McpError::Transport(why));
            }
            // The request is out and unanswered. It may have run, so this says so rather than
            // *nothing happened* - and the server is told to stop in the protocol's own words,
            // because a client that walks away leaves it working on a call nobody will read.
            Err(WireError::Timeout(timeout)) => {
                let _ = self.wire.notify(&json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/cancelled",
                    "params": {"requestId": id, "reason": format!("no answer within {}s", timeout.as_secs_f32())},
                }));
                return Err(McpError::MayHaveRun {
                    why: format!(
                        "request {id} (`{method}`) got no answer within {}s, and cancellation is a \
                         request the server may not have read",
                        timeout.as_secs_f32()
                    ),
                });
            }
            Err(e) => return Err(McpError::Transport(e.to_string())),
        };
        if let Some(err) = answer.get("error") {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
            let message = err.get("message").and_then(Value::as_str).unwrap_or("");
            return Err(McpError::Protocol(format!("{code}: {message}")));
        }
        Ok(answer.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Whether the server said its tool list changed since this was last asked.
    fn tools_changed(&mut self) -> bool {
        if self.dead.is_some() {
            return false;
        }
        self.wire.take_notices().iter().any(|m| m == TOOLS_CHANGED)
    }

    fn call(&mut self, tool: &str, args: &Value) -> Result<McpCallResult, McpError> {
        let result = self.request("tools/call", json!({"name": tool, "arguments": args}))?;
        Ok(McpCallResult {
            text: render_content(&result),
            is_error: result
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

/// A `tools/call` result as text. Non-text content is named, not inlined: a base64 image is
/// bytes the model cannot read and the cap would only spill.
fn render_content(result: &Value) -> String {
    let mut parts = Vec::new();
    for item in result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
        let part = match kind {
            "text" => item
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            "resource" => match item.pointer("/resource/text").and_then(Value::as_str) {
                Some(t) => t.to_string(),
                None => format!(
                    "[resource {}]",
                    item.pointer("/resource/uri")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                ),
            },
            "resource_link" => format!(
                "[resource_link {}]",
                item.get("uri").and_then(Value::as_str).unwrap_or("?")
            ),
            other => format!(
                "[{other} content, {}, {} chars not shown]",
                item.get("mimeType").and_then(Value::as_str).unwrap_or("?"),
                item.get("data").and_then(Value::as_str).map_or(0, str::len)
            ),
        };
        parts.push(part);
    }
    if parts.is_empty()
        && let Some(s) = result.get("structuredContent")
    {
        return s.to_string();
    }
    parts.join("\n")
}

fn err_text(e: &McpError) -> String {
    match e {
        McpError::Transport(s) | McpError::Protocol(s) => s.clone(),
        McpError::MayHaveRun { why } => why.clone(),
        McpError::NotAttached(_) => "not attached".into(),
    }
}

/// A server's connection, and when it may be tried again after it went down.
struct Link {
    client: Option<Client>,
    /// Why it went down, failed reconnects since, and the earliest next try.
    down: Option<(String, u32, Instant)>,
}

/// A stdio server's resolved binary and its mtime when it was started.
type Stamp = (PathBuf, Option<SystemTime>);

/// What a status reports, apart from the link so a status never waits on a call in flight.
#[derive(Default)]
struct Info {
    pid: Option<u32>,
    binary: Option<Stamp>,
    /// Why the last start failed. Such a server offers no tools until it is restarted.
    failed: Option<String>,
}

struct Entry {
    spec: ServerSpec,
    /// How this server's process is started: the session's boundary, or [`letibot_tools::exec::Literal`].
    spawn: Arc<dyn letibot_tools::exec::Spawn>,
    /// Behind its own lock so a listing never waits on a call in flight.
    tools: RwLock<Vec<McpToolSpec>>,
    link: Mutex<Link>,
    info: Mutex<Info>,
    /// Catalogues holding this entry. A pool prune drops an entry none holds.
    users: AtomicUsize,
}

impl Entry {
    fn open(spec: &ServerSpec, spawn: Arc<dyn letibot_tools::exec::Spawn>) -> Entry {
        let e = Entry::refused(spec, spawn, None);
        {
            let mut link = lock(&e.link);
            let _ = e.connect(&mut link);
        }
        e
    }

    fn refused(
        spec: &ServerSpec,
        spawn: Arc<dyn letibot_tools::exec::Spawn>,
        why: Option<String>,
    ) -> Entry {
        Entry {
            spec: spec.clone(),
            spawn,
            tools: RwLock::new(Vec::new()),
            // A failure already known is a down entry like any other, so the next call is the
            // one that retries it rather than a `/mcp` verb.
            link: Mutex::new(Link {
                client: None,
                down: why.clone().map(|why| (why, 0, Instant::now())),
            }),
            info: Mutex::new(Info {
                failed: why,
                ..Info::default()
            }),
            users: AtomicUsize::new(0),
        }
    }

    fn set_tools(&self, tools: Vec<McpToolSpec>) {
        *self.tools.write().unwrap_or_else(|e| e.into_inner()) = tools;
    }

    fn failed(&self) -> Option<String> {
        lock(&self.info).failed.clone()
    }

    /// Start the server on `link`, which holds no client.
    fn connect(&self, link: &mut Link) -> Result<(), String> {
        let binary = binary_stamp(&self.spec);
        match Client::connect(&self.spec, &*self.spawn) {
            Ok((client, tools)) => {
                self.adopt(link, client, tools, binary);
                Ok(())
            }
            Err(why) => {
                link.client = None;
                // The next call retries at once, then on the ladder: a start failure is the same
                // kind of trouble as a death, and waiting for a daemon restart is the one answer
                // that leaves the tools missing for hours.
                link.down = Some((why.clone(), 0, Instant::now()));
                self.set_tools(Vec::new());
                *lock(&self.info) = Info {
                    pid: None,
                    binary,
                    failed: Some(why.clone()),
                };
                Err(why)
            }
        }
    }

    fn adopt(
        &self,
        link: &mut Link,
        client: Client,
        tools: Vec<McpToolSpec>,
        binary: Option<Stamp>,
    ) {
        self.set_tools(tools);
        *lock(&self.info) = Info {
            pid: client.wire.pid(),
            binary,
            failed: None,
        };
        link.client = Some(client);
        link.down = None;
    }

    /// A new process for this spec. Waits for a call in flight, which finishes on the old one.
    fn restart(&self) -> Result<(), String> {
        let mut link = lock(&self.link);
        link.client = None;
        link.down = None;
        self.connect(&mut link)
    }

    /// Whether the stdio binary on disk is not the one this server was started from.
    fn stale(&self) -> bool {
        matches!(self.spec.transport, Transport::Stdio { .. })
            && binary_stamp(&self.spec) != lock(&self.info).binary
    }

    /// List again if the server said its tools changed.
    fn refresh(&self, link: &mut Link) {
        if let Some(c) = link.client.as_mut()
            && c.tools_changed()
            && let Ok(tools) = c.list_tools()
        {
            self.set_tools(tools);
        }
    }

    /// A live client, reconnecting one that went down or never came up once its backoff passed.
    fn up<'a>(&self, link: &'a mut Link) -> Result<&'a mut Client, McpError> {
        if let Some((why, failed, next)) = link.down.clone() {
            let now = Instant::now();
            // Both go through this ladder; only the sentence differs, so an operator knows whether
            // a process that used to work has gone or one never got started.
            let what = if self.failed().is_some() {
                "the server did not start"
            } else {
                "the server is down"
            };
            if now < next {
                return Err(McpError::Transport(format!(
                    "{what} ({why}); the next restart is in {:.1}s",
                    (next - now).as_secs_f64()
                )));
            }
            let binary = binary_stamp(&self.spec);
            match Client::connect(&self.spec, &*self.spawn) {
                Ok((client, tools)) => self.adopt(link, client, tools, binary),
                Err(e) => {
                    let failed = failed + 1;
                    let wait = backoff(failed);
                    link.client = None;
                    link.down = Some((why.clone(), failed, Instant::now() + wait));
                    // `/mcp` and the start failures read this: the latest words, not the first.
                    {
                        let mut info = lock(&self.info);
                        if info.failed.is_some() {
                            info.failed = Some(e.clone());
                        }
                    }
                    return Err(McpError::Transport(format!(
                        "{what} ({why}) and did not restart: {e}; the next restart is in {:.1}s",
                        wait.as_secs_f64()
                    )));
                }
            }
        }
        link.client
            .as_mut()
            .ok_or_else(|| McpError::Transport("the server is not connected".into()))
    }

    fn call(&self, tool: &str, args: &Value) -> Result<McpCallResult, McpError> {
        let mut link = lock(&self.link);
        self.refresh(&mut link);
        let client = self.up(&mut link)?;
        let out = client.call(tool, args);
        if let Some(why) = client.dead.clone() {
            // The call in flight is not retried: it may have acted before the server died.
            link.client = None;
            link.down = Some((why.clone(), 0, Instant::now()));
            return Err(McpError::Transport(format!(
                "{why}; it is restarted on the next call"
            )));
        }
        self.refresh(&mut link);
        out
    }

    fn status(&self) -> Status {
        let info = lock(&self.info);
        let state = match (&info.failed, self.link.try_lock()) {
            (Some(why), _) => State::Failed(why.clone()),
            (None, Ok(link)) => match &link.down {
                Some((why, _, _)) => State::Down(why.clone()),
                None if link.client.is_none() => State::Down("not connected".into()),
                None => State::Running,
            },
            // Busy with a call: it is up.
            (None, Err(_)) => State::Running,
        };
        Status {
            name: self.spec.name.clone(),
            tools: self.tools.read().unwrap_or_else(|e| e.into_inner()).len(),
            pid: info.pid.filter(|_| state == State::Running),
            url: match &self.spec.transport {
                Transport::Http { url, .. } => Some(url.clone()),
                Transport::Stdio { .. } => None,
            },
            state,
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The file a stdio command runs, found the way `Command` finds it, and its mtime.
fn binary_stamp(spec: &ServerSpec) -> Option<Stamp> {
    let Transport::Stdio {
        command, env, cwd, ..
    } = &spec.transport
    else {
        return None;
    };
    let path = if command.contains('/') {
        let p = std::path::Path::new(command);
        match cwd {
            Some(d) if p.is_relative() => d.join(p),
            _ => p.to_path_buf(),
        }
    } else {
        let search = env
            .get("PATH")
            .cloned()
            .or_else(|| std::env::var("PATH").ok())?;
        std::env::split_paths(&search)
            .map(|d| d.join(command))
            .find(|c| c.is_file())?
    };
    let path = std::fs::canonicalize(&path).ok()?;
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    Some((path, mtime))
}

/// One server as `/mcp` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub name: String,
    pub state: State,
    pub tools: usize,
    /// A running stdio server's process.
    pub pid: Option<u32>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Running,
    /// Started, then went away. It is restarted on its next call.
    Down(String),
    /// Did not start, or its table was refused. Restarted only by a reload or a restart.
    Failed(String),
}

/// What a reconcile or a restart did to one server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub name: String,
    pub did: Did,
    /// Set when the new process did not start.
    pub failed: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Did {
    Added,
    Removed,
    /// Restarted, and why: `config changed`, `binary updated`, `retried`, `by hand`.
    Restarted(&'static str),
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.did {
            Did::Added => write!(f, "{} added", self.name)?,
            Did::Removed => write!(f, "{} stopped (removed)", self.name)?,
            Did::Restarted(why) => write!(f, "{} restarted ({why})", self.name)?,
        }
        if let Some(why) = &self.failed {
            write!(f, " - failed: {why}")?;
        }
        Ok(())
    }
}

/// What one server is, for sharing: the spec and the boundary it runs in. Two sessions whose
/// keys differ get two processes, because one started under one view is the wrong process for the
/// other; two that agree share one, which is what makes this a pool.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    spec: ServerSpec,
    sandbox: String,
}

/// The servers a daemon runs, shared by every catalogue whose spec names the same process.
#[derive(Default)]
pub struct Pool {
    live: Mutex<HashMap<Key, Arc<Entry>>>,
}

impl Pool {
    pub fn new() -> Arc<Pool> {
        Arc::new(Pool::default())
    }

    /// The entry for each spec, held once more, and whether this call started it. Missing
    /// ones start in parallel, each waited for up to its own timeout.
    fn take(
        &self,
        specs: &[ServerSpec],
        spawn: &Arc<dyn letibot_tools::exec::Spawn>,
    ) -> Vec<(Arc<Entry>, bool)> {
        let keys: Vec<Key> = specs
            .iter()
            .map(|spec| Key {
                spec: spec.clone(),
                sandbox: spawn.key(),
            })
            .collect();
        let mut live = lock(&self.live);
        let missing: Vec<&Key> = keys.iter().filter(|k| !live.contains_key(*k)).collect();
        let started: Vec<Entry> = std::thread::scope(|s| {
            let handles: Vec<_> = missing
                .iter()
                .map(|key| {
                    let spawn = spawn.clone();
                    (
                        key.spec.clone(),
                        s.spawn(move || Entry::open(&key.spec, spawn)),
                    )
                })
                .collect();
            handles
                .into_iter()
                .map(|(spec, h)| {
                    h.join().unwrap_or_else(|_| {
                        Entry::refused(
                            &spec,
                            spawn.clone(),
                            Some("the start thread panicked".into()),
                        )
                    })
                })
                .collect()
        });
        for e in started {
            live.insert(
                Key {
                    spec: e.spec.clone(),
                    sandbox: e.spawn.key(),
                },
                Arc::new(e),
            );
        }
        keys.iter()
            .map(|k| {
                let e = live[k].clone();
                e.users.fetch_add(1, Ordering::SeqCst);
                (e, missing.contains(&k))
            })
            .collect()
    }

    /// Stop the servers no catalogue holds. A call in flight keeps its process until it ends.
    fn prune(&self) {
        lock(&self.live).retain(|_, e| e.users.load(Ordering::SeqCst) > 0);
    }

    /// A catalogue of `specs`, and the start failures of the servers this call started.
    pub fn catalog(
        self: &Arc<Self>,
        specs: &[ServerSpec],
        bad: Vec<Failure>,
    ) -> (Catalog, Vec<Failure>) {
        self.catalog_with(Arc::new(letibot_tools::exec::Literal), specs, bad)
    }

    /// [`Pool::catalog`] with the boundary each server's process is started in.
    pub fn catalog_with(
        self: &Arc<Self>,
        spawn: Arc<dyn letibot_tools::exec::Spawn>,
        specs: &[ServerSpec],
        bad: Vec<Failure>,
    ) -> (Catalog, Vec<Failure>) {
        let taken = self.take(specs, &spawn);
        let failed = taken
            .iter()
            .filter(|(_, started)| *started)
            .filter_map(|(e, _)| {
                e.failed().map(|why| Failure {
                    server: e.spec.name.clone(),
                    why,
                })
            })
            .collect();
        let catalog = Catalog {
            pool: self.clone(),
            spawn,
            entries: RwLock::new(taken.into_iter().map(|(e, _)| e).collect()),
            bad: RwLock::new(bad),
        };
        (catalog, failed)
    }
}

/// One session's servers, connected or failed. Reconciled in place, so the tool doors that
/// hold it see the new set.
pub struct Catalog {
    pool: Arc<Pool>,
    /// The boundary every server of this catalogue's is started in.
    spawn: Arc<dyn letibot_tools::exec::Spawn>,
    entries: RwLock<Vec<Arc<Entry>>>,
    /// Tables refused before any start, such as one naming no transport.
    bad: RwLock<Vec<Failure>>,
}

impl Drop for Catalog {
    fn drop(&mut self) {
        for e in self
            .entries
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
        {
            e.users.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Catalog {
    /// Start every server at once and wait for each up to its own timeout, in a pool of its own.
    pub fn start(specs: &[ServerSpec]) -> Catalog {
        Pool::new().catalog(specs, Vec::new()).0
    }

    /// [`Catalog::start`] with the boundary each server's process is started in: the session that
    /// owns them hands its own, so a server runs where that session's commands run.
    pub fn start_with(specs: &[ServerSpec], spawn: Arc<dyn letibot_tools::exec::Spawn>) -> Catalog {
        Pool::new().catalog_with(spawn, specs, Vec::new()).0
    }

    /// Add servers refused before any start, such as a table naming no transport.
    pub fn with_failures(self, failures: Vec<Failure>) -> Catalog {
        *self.bad.write().unwrap_or_else(|e| e.into_inner()) = failures;
        self
    }

    fn entries(&self) -> Vec<Arc<Entry>> {
        self.entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn entry(&self, name: &str) -> Option<Arc<Entry>> {
        self.entries().into_iter().find(|e| e.spec.name == name)
    }

    /// The servers that did not start, refused tables first.
    pub fn failures(&self) -> Vec<Failure> {
        let mut out = self.bad.read().unwrap_or_else(|e| e.into_inner()).clone();
        out.extend(self.entries().iter().filter_map(|e| {
            e.failed().map(|why| Failure {
                server: e.spec.name.clone(),
                why,
            })
        }));
        out
    }

    /// Every server, refused tables included, sorted by name.
    pub fn status(&self) -> Vec<Status> {
        let mut out: Vec<Status> = self.entries().iter().map(|e| e.status()).collect();
        out.extend(
            self.bad
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .map(|f| Status {
                    name: f.server.clone(),
                    state: State::Failed(f.why.clone()),
                    tools: 0,
                    pid: None,
                    url: None,
                }),
        );
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Make the running set `specs`: start new servers, stop removed ones, replace changed ones
    /// and retry failed ones. An unchanged server keeps its process. A call in flight to a
    /// replaced server finishes on the old process.
    pub fn reconcile(&self, specs: &[ServerSpec], bad: Vec<Failure>) -> Vec<Change> {
        let old = self.entries();
        let by_name = |n: &str| old.iter().find(|e| e.spec.name == n);
        let fresh: Vec<ServerSpec> = specs
            .iter()
            .filter(|s| by_name(&s.name).is_none_or(|e| e.spec != **s))
            .cloned()
            .collect();
        let mut taken = self.pool.take(&fresh, &self.spawn).into_iter();
        let mut changes = Vec::new();
        let mut next = Vec::new();
        for spec in specs {
            match by_name(&spec.name) {
                Some(e) if e.spec == *spec => {
                    if e.failed().is_some() {
                        let failed = e.restart().err();
                        changes.push(Change {
                            name: spec.name.clone(),
                            did: Did::Restarted("retried"),
                            failed,
                        });
                    }
                    next.push(e.clone());
                }
                was => {
                    let (e, started) = taken.next().expect("one taken entry per fresh spec");
                    // A pooled entry another session left failed is tried again.
                    let mut failed = e.failed();
                    if !started && failed.is_some() {
                        failed = e.restart().err();
                    }
                    changes.push(Change {
                        name: spec.name.clone(),
                        did: if was.is_some() {
                            Did::Restarted("config changed")
                        } else {
                            Did::Added
                        },
                        failed,
                    });
                    next.push(e);
                }
            }
        }
        let old_bad = self.bad.read().unwrap_or_else(|e| e.into_inner()).clone();
        for e in &old {
            if !next.iter().any(|n| Arc::ptr_eq(n, e)) {
                e.users.fetch_sub(1, Ordering::SeqCst);
                let now_bad = bad.iter().any(|f| f.server == e.spec.name);
                if !specs.iter().any(|s| s.name == e.spec.name) && !now_bad {
                    changes.push(Change {
                        name: e.spec.name.clone(),
                        did: Did::Removed,
                        failed: None,
                    });
                }
            }
        }
        for f in &bad {
            if !old_bad.contains(f) {
                changes.push(Change {
                    name: f.server.clone(),
                    did: if old.iter().any(|e| e.spec.name == f.server)
                        || old_bad.iter().any(|o| o.server == f.server)
                    {
                        Did::Restarted("config changed")
                    } else {
                        Did::Added
                    },
                    failed: Some(f.why.clone()),
                });
            }
        }
        for f in &old_bad {
            if !bad.iter().any(|b| b.server == f.server)
                && !specs.iter().any(|s| s.name == f.server)
            {
                changes.push(Change {
                    name: f.server.clone(),
                    did: Did::Removed,
                    failed: None,
                });
            }
        }
        *self.entries.write().unwrap_or_else(|e| e.into_inner()) = next;
        *self.bad.write().unwrap_or_else(|e| e.into_inner()) = bad;
        self.pool.prune();
        changes.sort_by(|a, b| a.name.cmp(&b.name));
        changes
    }

    /// Restart one server in place, for every session sharing it. `None` when none is named so.
    pub fn restart(&self, name: &str, why: &'static str) -> Option<Change> {
        let e = self.entry(name)?;
        Some(Change {
            name: name.to_string(),
            did: Did::Restarted(why),
            failed: e.restart().err(),
        })
    }

    /// The stdio servers whose binary on disk changed since they started.
    pub fn stale(&self) -> Vec<String> {
        self.entries()
            .iter()
            .filter(|e| e.stale())
            .map(|e| e.spec.name.clone())
            .collect()
    }
}

impl McpCatalog for Catalog {
    fn servers(&self) -> Vec<McpServer> {
        self.entries()
            .iter()
            .filter(|e| e.failed().is_none())
            .map(|e| {
                // A server busy with a call keeps its last list; the call refreshes it after.
                if let Ok(mut link) = e.link.try_lock() {
                    e.refresh(&mut link);
                }
                McpServer {
                    name: e.spec.name.clone(),
                    // A stdio server is a process this box started, so its calls are `Exec`; an
                    // HTTP server is reached and never run here.
                    access: match &e.spec.transport {
                        Transport::Stdio { .. } => letibot_tools::schema::Access::Exec,
                        Transport::Http { .. } => letibot_tools::schema::Access::Network,
                    },
                    tools: e.tools.read().unwrap_or_else(|e| e.into_inner()).clone(),
                }
            })
            .collect()
    }

    fn call(&self, server: &str, tool: &str, args: &Value) -> Result<McpCallResult, McpError> {
        // Held by this call, so a reload that replaces it leaves this call its process.
        let Some(entry) = self.entry(server) else {
            return Err(McpError::Transport(format!(
                "no MCP server `{server}` is connected"
            )));
        };
        entry.call(tool, args)
    }

    fn describe(&self) -> String {
        let up: Vec<String> = self
            .entries()
            .iter()
            .filter(|e| e.failed().is_none())
            .map(|e| {
                let n = e.tools.read().unwrap_or_else(|e| e.into_inner()).len();
                format!("{} ({n} tools)", e.spec.name)
            })
            .collect();
        let mut s = if up.is_empty() {
            "none connected".to_string()
        } else {
            format!("MCP servers {}", up.join(", "))
        };
        let failures = self.failures();
        if !failures.is_empty() {
            let down: Vec<&str> = failures.iter().map(|f| f.server.as_str()).collect();
            s.push_str(&format!("; failed to start: {}", down.join(", ")));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ladder_starts_at_once_then_doubles_from_half_a_second_to_thirty() {
        let ms: Vec<u128> = (1..=9).map(|n| backoff(n).as_millis()).collect();
        assert_eq!(
            ms,
            [500, 1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000]
        );
        assert_eq!(backoff(u32::MAX).as_millis(), 30000);
    }

    #[test]
    fn content_renders_text_and_names_the_rest() {
        let r = json!({"content": [
            {"type": "text", "text": "a"},
            {"type": "image", "mimeType": "image/png", "data": "AAAA"},
            {"type": "resource", "resource": {"uri": "file:///x", "text": "b"}},
        ]});
        assert_eq!(
            render_content(&r),
            "a\n[image content, image/png, 4 chars not shown]\nb"
        );
    }

    #[test]
    fn structured_content_stands_in_for_no_content() {
        let r = json!({"content": [], "structuredContent": {"n": 1}});
        assert_eq!(render_content(&r), r#"{"n":1}"#);
    }
}
