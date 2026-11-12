//! The ten verbs, and the map of sessions the bridge holds (`docs/claude-code-bridge.md`).
//! `leticode_start` keeps the head, `leticode_attach` joins and does not start, and
//! `leticode_answer` sends the protocol's `Answer` so the decision is attributed. Nothing here talks to a model.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use letibot_tools::builtins::task::Placement;
use letibot_tools::remote::Hosts;

use crate::daemon;
use crate::diff;
use crate::head::{self, Attention, Head, Turn};

/// The default approval point. `writes-allowed` is the point whose grant scope is
/// *session*, so an approval a person gives once covers that class of call - and exec,
/// the network and the always-ask list still ask, which is what the bridge is for.
pub const DEFAULT_MODE: &str = "writes-allowed";

/// The default page size of `leticode_events`. Fifty is a screenful of progress: a turn
/// that produced more has produced more than a caller reads in one go.
pub const EVENTS_LIMIT: usize = 50;

/// How long `leticode_mode` waits for the confirmation it asked for. Two minutes: long
/// enough for a person to read the card and answer it, short enough that a tool call does not
/// look hung. The daemon keeps the request open past this, so a late answer still applies.
pub const MODE_PATIENCE: Duration = Duration::from_secs(120);

/// How long `leticode_status` waits for the daemon's settings answer before it says null.
const IDLE_EXIT_PATIENCE: Duration = Duration::from_millis(500);

/// What the bridge holds: one attached head per session it started or joined, by session id.
/// A `HashMap` behind a mutex rather than an actor: the tools are called one at a time, and the
/// only concurrency here is each head's own collecting thread.
pub struct Bridge {
    heads: Arc<Mutex<HashMap<String, Arc<Head>>>>,
    /// The `claude-code` seat on the host coordinator, opened on first use.
    rooms: crate::rooms::RoomLink,
    /// Push the room's messages to the client as channel notifications (set by `letibot-mcp`).
    stream_rooms: bool,
    /// The store and folder session names are read from, instead of this process's own. Set by
    /// a test; a running bridge asks `daemon::resolve` for the folder it was started in.
    names_from: Option<(std::path::PathBuf, std::path::PathBuf)>,
    /// Where held sessions' events go once the client can take channel notifications.
    push: Mutex<Option<std::sync::mpsc::Sender<serde_json::Value>>>,
    /// One channel per held session: the records a client has not taken yet, and the cursor it
    /// has been told up to. Kept apart from `push` because a client that connects again is
    /// handed what it missed rather than starting a second watcher that repeats the lot.
    channels: Mutex<HashMap<String, Arc<crate::push::Outbox>>>,
    /// Whether this folder's queues are already being followed: a client that initializes twice
    /// must not leave a second follower saying the same move again.
    queue: Mutex<bool>,
    /// This seat's todo board, opened on first use - by a todo tool, or by the watcher the client
    /// starts when it initializes.
    seat: Mutex<Option<Arc<crate::seat::Seat>>>,
    /// How long the seat sits quiet before the plan check comes due - [`crate::seat::NAG_AFTER`]
    /// in every bridge, and a field for a test that must drive a delivery without the minute.
    seat_window: Duration,
    /// Claude Code's own permission prompts: off unless `[bridge] permission_relay` says so, and
    /// the same flag that declares the capability.
    relay: Arc<crate::relay::Relay>,
    permission_relay: bool,
    /// The relay's observer is on the room link: a client that initializes twice must not leave a
    /// second one reading the same verdicts.
    watching: Mutex<bool>,
    /// This folder's plugin layers (X25), built on the first call that needs one: the `tool_`
    /// layers a client may call and the `push_channel` rule. `Layers` remembers that it tried, so a
    /// folder with nothing to load is not scanned again.
    plugins: Mutex<crate::plugins::Layers>,
    /// The push rule, built once from those layers and shared by every watcher and the room stream.
    rules: Mutex<Option<Arc<crate::push::Rules>>>,
}

impl Default for Bridge {
    fn default() -> Bridge {
        Bridge {
            heads: Arc::default(),
            rooms: crate::rooms::RoomLink::default(),
            stream_rooms: false,
            names_from: None,
            push: Mutex::new(None),
            channels: Mutex::new(HashMap::new()),
            queue: Mutex::new(false),
            seat: Mutex::new(None),
            // Written out rather than derived: a derived `Duration` is zero, and a zero window is
            // a bridge that nags the moment a row is written.
            seat_window: crate::seat::NAG_AFTER,
            relay: Arc::new(crate::relay::Relay::new()),
            permission_relay: false,
            watching: Mutex::new(false),
            plugins: Mutex::new(crate::plugins::Layers::default()),
            rules: Mutex::new(None),
        }
    }
}

impl Bridge {
    pub fn new() -> Bridge {
        Bridge::default()
    }

    /// A bridge whose room seat connects to `socket`.
    pub fn with_room_socket(socket: impl Into<std::path::PathBuf>) -> Bridge {
        Bridge {
            rooms: crate::rooms::RoomLink::at(socket),
            ..Bridge::default()
        }
    }

    /// A bridge that resolves session names in `folder` from `store`.
    pub fn with_names(
        mut self,
        store: impl Into<std::path::PathBuf>,
        folder: impl Into<std::path::PathBuf>,
    ) -> Bridge {
        self.names_from = Some((store.into(), folder.into()));
        self
    }

    /// The store and folder this bridge resolves names from, when a caller named them: what
    /// `holds` reads its folder's owners from without a daemon on the socket.
    pub fn names_source(&self) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        self.names_from.clone()
    }

    /// The session a tool's `session` argument means: an id as given, else the conversation of
    /// that name (title or slug) in the bridge's folder, the folders of the sessions it holds and
    /// `folder` when the call names one. A name that fits several is refused with them listed.
    pub fn resolve_session(&self, raw: &str, folder: Option<&str>) -> Result<String, String> {
        let raw = raw.trim();
        if self.held(raw).is_some() {
            return Ok(raw.to_string());
        }
        let (store, mut folders) = match &self.names_from {
            Some((store, folder)) => (store.clone(), vec![folder.clone()]),
            None => {
                let mut folders = Vec::new();
                let here = std::env::current_dir().map_err(|e| e.to_string())?;
                let d = daemon::resolve(&here)?;
                folders.push(d.workspace);
                let held: Vec<std::path::PathBuf> = self
                    .heads
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .values()
                    .map(|h| h.worktree.clone())
                    .collect();
                for w in held {
                    if let Ok(w) = daemon::workspace_of(&w) {
                        folders.push(w);
                    }
                }
                (d.store, folders)
            }
        };
        if let Some(f) = folder.map(str::trim).filter(|f| !f.is_empty())
            && let Ok(w) = daemon::workspace_of(Path::new(f))
        {
            folders.push(w);
        }
        folders.sort();
        folders.dedup();
        crate::names::resolve(&store, &folders, raw)
    }

    /// Deliver the room's `to_me` messages to the MCP client once it has initialized.
    pub fn streaming_rooms(mut self) -> Bridge {
        self.stream_rooms = true;
        self
    }

    /// Relay this session's own permission prompts to the operator: the capability is declared at
    /// `initialize`, a request is asked in the room, and the operator's `yes <id>` is the verdict.
    /// Off unless the user's `leticode.toml` says `[bridge] permission_relay = true`.
    pub fn relaying(mut self) -> Bridge {
        self.permission_relay = true;
        self
    }

    /// The relay's open ids, unpruned - what the operator was asked about and has not answered.
    pub fn relay_open(&self) -> Vec<String> {
        self.relay.open()
    }

    /// Does this bridge declare `claude/channel/permission` and take verdicts?
    pub fn relays_permissions(&self) -> bool {
        self.permission_relay
    }

    /// A `permission_request` from Claude Code: keep the id and DM the operator what the call is.
    /// The DM goes out on a thread of its own, because this is a notification and the client's next
    /// line must not wait on the room's round trip.
    pub fn permission_request(&self, params: &Value) {
        if !self.permission_relay {
            return;
        }
        let Some(text) = self.relay.asked(params) else {
            return;
        };
        let link = self.rooms.clone();
        std::thread::spawn(move || {
            if let Err(why) = crate::rooms::dm(&link, letibot_room::OPERATOR, &text) {
                eprintln!("letibot-mcp: the permission prompt did not reach the operator: {why}");
            }
        });
    }

    /// Read every room message for a verdict from now on, and put the ones that are one on `push`:
    /// the client's own writer, so a verdict is written by the thread that owns stdout. Installed
    /// once, at `notifications/initialized`, which is when the client may be told anything at all.
    pub fn watch_permissions(&self, push: std::sync::mpsc::Sender<Value>) {
        if !self.permission_relay {
            return;
        }
        {
            let mut watching = self.watching.lock().unwrap_or_else(|e| e.into_inner());
            if *watching {
                return;
            }
            *watching = true;
        }
        let relay = self.relay.clone();
        let link = self.rooms.clone();
        let hook: crate::rooms::Hook =
            Arc::new(move |m, _names_you| on_message(&relay, &link, &push, m));
        self.rooms.observe(hook);
    }

    pub fn rooms(&self) -> &crate::rooms::RoomLink {
        &self.rooms
    }

    /// A bridge whose seat is already opened - a test's door, and the one that names the handle
    /// and the window instead of taking them from the room's name and the minute.
    pub fn with_seat(mut self, seat: crate::seat::Seat) -> Bridge {
        self.seat = Mutex::new(Some(Arc::new(seat)));
        self
    }

    /// A bridge whose plan check comes due `window` after the seat goes quiet - the tests' door,
    /// for `TodoNagClock::with_window`'s reason.
    pub fn with_nag_window(mut self, window: Duration) -> Bridge {
        self.seat_window = window;
        self
    }

    /// This seat's todo board, opened on first use: the handle the room knows it by (`cc@<project>`)
    /// and the store of the folder this bridge serves.
    pub fn seat(&self) -> Result<Arc<crate::seat::Seat>, String> {
        let mut g = self.seat.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(seat) = g.as_ref() {
            return Ok(seat.clone());
        }
        let store = match &self.names_from {
            Some((store, _)) => store.clone(),
            None => {
                let here = std::env::current_dir().map_err(|e| e.to_string())?;
                daemon::resolve(&here)?.store
            }
        };
        let seat = Arc::new(
            crate::seat::Seat::new(crate::rooms::handle(), store, self.heads.clone())
                .with_window(self.seat_window),
        );
        *g = Some(seat.clone());
        Ok(seat)
    }

    /// Push this seat's nags on `tx` from now on: the channel the client opened at `initialize`,
    /// and the only moment a notification is legal.
    pub fn push_seat(&self, tx: std::sync::mpsc::Sender<serde_json::Value>) {
        if let Ok(seat) = self.seat() {
            crate::seat::Seat::watch(&seat, tx);
        }
    }

    /// A tool call is the seat working: it is not idle, and the plan check waits for its end. Never
    /// opens the seat - a bridge that has never served a todo tool has no board to re-arm.
    pub fn touch_seat(&self) {
        let g = self.seat.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(seat) = g.as_ref() {
            seat.touch();
        }
    }

    pub fn stream_rooms(&self) -> bool {
        self.stream_rooms
    }

    /// The folder whose plugin scopes this bridge reads: the one a test named, else the working
    /// directory the client started the server in - the same folder `leticode_start` resolves.
    pub fn folder(&self) -> std::path::PathBuf {
        match &self.names_from {
            Some((_, folder)) => folder.clone(),
            None => std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        }
    }

    /// This folder's plugin stack, built on first use. `None` when nothing could be loaded, which is
    /// what a box with no engine or an unreadable plugins directory comes to.
    pub fn plugin_stack(&self) -> Option<Arc<letibot_plugins::Stack>> {
        let folder = self.folder();
        let mut g = self.plugins.lock().unwrap_or_else(|e| e.into_inner());
        g.stack(&folder)
    }

    /// The rule every push goes through: this folder's stack, or none when there is no layer to ask.
    pub fn push_rules(&self) -> Arc<crate::push::Rules> {
        let mut g = self.rules.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(rules) = g.as_ref() {
            return rules.clone();
        }
        let rules = Arc::new(match self.plugin_stack() {
            Some(stack) => crate::push::Rules::with(stack),
            None => crate::push::Rules::none(),
        });
        *g = Some(rules.clone());
        rules
    }

    /// A bridge whose layers are `stack`, for a test that must not compile one: its `tool_` layers
    /// are listed and called, and its `push_channel` answer is the one asked.
    pub fn with_plugins(mut self, stack: Option<Arc<letibot_plugins::Stack>>) -> Bridge {
        self.plugins = Mutex::new(crate::plugins::Layers::with(stack));
        self.rules = Mutex::new(None);
        self
    }

    /// This folder's plugin layers, discovered from disk on first use: the server's door, and the
    /// only one that reads the operator's config directory.
    pub fn with_layers_from_disk(mut self) -> Bridge {
        self.plugins = Mutex::new(crate::plugins::Layers::of_disk());
        self.rules = Mutex::new(None);
        self
    }

    /// Start leticode on a worktree. A daemon this call started already holds its first
    /// session, so that is the session the brief runs in: one brief is one conversation. A daemon
    /// already up belongs to somebody else, so a new session is created and attached by id.
    pub fn start(
        &self,
        worktree: &str,
        brief: &str,
        model: Option<&str>,
        mode: &str,
        host: Option<&str>,
        vm: bool,
    ) -> Result<Value, String> {
        if brief.trim().is_empty() {
            return Err("`brief` is empty: a session with no brief runs no turn".into());
        }
        // The host is checked before anything is done: a name this file does not declare is
        // refused by the operator's rule, listing the names it does, and no daemon is started
        // for it.
        let placement = placement_arg(host, vm, &Hosts::load())?;
        let d = daemon::ensure(Path::new(worktree), model, mode, &placement)?;
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
            // Where this session's tools run, from the daemon's own record and not from the
            // argument: the fact is the daemon's, and a head that cannot tell would report a
            // package's work as being somewhere it is not.
            "placement": placement_json(h.placement.as_ref()),
            // The same thing in a sentence, because this is the field a model reads to answer
            // *where is my work happening* - and the half that matters as much is what did NOT
            // move: the model, the store and the gate are this box's.
            "tools_run": tools_run(h.placement.as_ref()),
        });
        // Each session's model is its own, and this is the half that makes it so: the daemon's
        // `--provider` is the DEFAULT, so a session joining a running daemon would otherwise answer
        // every turn on whatever its first session chose. `/models` is per session.
        //
        // A session whose tools run somewhere else is QUEUED rather than waited for: its worker
        // opens the remote tree or boots a guest first, which on a loaded box is minutes, and a
        // start that waited would give up with the model, the mode and the brief all unsent. The
        // three go out in that order because the daemon applies one session's commands in the
        // order they arrived - which is what makes a queued model still the one the brief runs on.
        let placed = placement.is_vm() || placement.host().is_some();
        let mut queued: Vec<&str> = Vec::new();
        if let Some(m) = model.filter(|m| !m.trim().is_empty()) {
            let set = if placed {
                h.queue_model(m)
            } else {
                h.set_model(m).map(|said| {
                    answer["model_said"] = json!(said);
                })
            };
            match set {
                Ok(()) => {
                    answer["model"] = json!(m);
                    if placed {
                        answer["model_queued"] = json!(true);
                        queued.push("model");
                    }
                }
                Err(why) => {
                    // Held: the session exists and a caller that wants to ask why must be able to
                    // reach it by the id it was told, and no brief was sent, so nothing ran on a
                    // model nobody asked for.
                    self.hold(&h);
                    return Err(format!(
                        "`{session}` was opened, and its model could not be set to `{m}`, so the \
                         brief was not sent: {why}. \nThe model is a fact of the SESSION, so this \
                         works over a daemon already serving another provider: pass a provider \
                         this box has a key for (`deepseek`, `glm`, `grok`), or call \
                         `leticode_send` on `{session}` and let the daemon's own model answer"
                    ));
                }
            }
        }
        // Before the brief, so the first tool call already runs at the point the caller
        // asked for: `Mode` is queued like a prompt and takes effect from the next call.
        if let Err(e) = h.set_mode(mode) {
            // A session that cannot be moved is still a session - say so and let the brief
            // run at the daemon's own point rather than losing the session to the failure.
            answer["mode_requested"] = json!(mode);
            answer["mode_error"] = json!(e);
        } else {
            answer["mode"] = json!(mode);
            if placed {
                queued.push("mode");
            }
        }
        let sent = h.prompt(brief);
        // Held even when the send failed: the session exists, and a caller that wants to
        // ask why must be able to reach it by the id it was told.
        self.hold(&h);
        sent?;
        if placed {
            queued.push("brief");
            answer["queued"] = json!(queued);
            // The sentence a caller reads instead of polling: the three went out in the order the
            // daemon applies them, so no further call is needed to send the brief.
            answer["state"] = json!(if placement.is_vm() {
                "queued until the VM is up: the model, the mode and the brief are applied in that \
                 order when the session's worker opens"
            } else {
                "queued until the session is open there: the model, the mode and the brief are \
                 applied in that order when its worker opens"
            });
        }
        Ok(answer)
    }

    fn hold(&self, h: &Arc<Head>) {
        let mut g = self.heads.lock().unwrap_or_else(|e| e.into_inner());
        let new = g.insert(h.session.clone(), h.clone()).is_none();
        drop(g);
        if new && let Some(tx) = self.push.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            self.channel(h).attach(tx);
        }
    }

    /// The channel a session's events are recorded in, started on first use: one per session,
    /// because a second watcher on one head would say every event twice.
    fn channel(&self, h: &Arc<Head>) -> Arc<crate::push::Outbox> {
        let mut g = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(channel) = g.get(&h.session) {
            return channel.clone();
        }
        let channel = crate::push::watch(h, self.push_rules());
        g.insert(h.session.clone(), channel.clone());
        channel
    }

    /// Push every held session's events on `tx` from now on, those held already included.
    pub fn push_sessions(&self, tx: std::sync::mpsc::Sender<serde_json::Value>) {
        *self.push.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx.clone());
        let heads: Vec<Arc<Head>> = self
            .heads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for h in heads {
            // A new client, or the first one: either way what was recorded while nobody was
            // reading goes now, and a channel that never existed starts at the state it sees.
            self.channel(&h).attach(tx.clone());
        }
    }

    /// Push the queues' moves for this bridge's folder on `tx` from now on: the same channel as
    /// the held sessions', because a landing and a verdict are what an orchestrator waits for.
    pub fn push_queue(&self, tx: std::sync::mpsc::Sender<serde_json::Value>) {
        // The store and folder a test names win, exactly as `resolve_session` takes them; a
        // running bridge asks the daemon of the folder it was started in.
        let (store, folder, socket) = match &self.names_from {
            Some((store, folder)) => (store.clone(), folder.clone(), None),
            None => match std::env::current_dir()
                .map_err(|e| e.to_string())
                .and_then(|here| daemon::resolve(&here))
            {
                Ok(d) => (d.store, d.workspace, Some(d.socket)),
                // A folder with no daemon yet has no queue to follow; the tools still work, and
                // `leticode_start` will start one.
                Err(_) => return,
            },
        };
        {
            let mut following = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            if *following {
                return;
            }
            *following = true;
        }
        crate::queue::watch(&store, &folder, socket.as_deref(), tx);
    }

    /// Join a session this bridge did not start - `leticode_attach`. It joins a running
    /// daemon and never starts one, and `watch_only` is the default: the session keeps the head
    /// that holds it, and this one only watches - no prompt, no answer, no interrupt.
    pub fn attach(&self, worktree: &str, session: &str, watch_only: bool) -> Result<Value, String> {
        // The daemon first, because the name `gatekeeper` resolves against ITS workspace key:
        // the review queue's session id is per folder, so a name resolved to a constant would
        // join whichever daemon spelled it rather than the one this call is about.
        let d = daemon::resolve(Path::new(worktree))?;
        let session = session_name(session, &d.key)?;
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
        // A second attach to a session this bridge already holds is refused. The map is keyed
        // by session id, so a second entry would silently replace the first: the socket that
        // answers its cards would shut down and a `since` cursor would read a stream going back.
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
            // A session somebody else opened: where ITS daemon runs the tools is the one fact
            // an orchestrator following it from here has no other way to learn.
            "placement": placement_json(h.placement.as_ref()),
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
    /// own prompt is - the daemon never rejects it.
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
            // What answers this session's turns, as `/models` spells it: read off the last round the
            // daemon sent. Empty is *not known* - a session that has never run a turn under this head -
            // and not `local`, which is a model and not a guess.
            "model": if s.model.is_empty() {
                Value::Null
            } else {
                json!(s.model)
            },
            // The last time the provider itself was in trouble, in the daemon's own words: the kind,
            // the reset time it gave, and where the daemon moved the session if it did. `null` is a
            // session whose provider has not failed since this head attached.
            "last_trouble": s
                .last_trouble
                .as_ref()
                .map(|t| t.to_json())
                .unwrap_or(Value::Null),
            // Where this session's tools run - `host`, or `host:NAME`, or null when the
            // daemon recorded nothing this build can read. A caller reading a diff or a test
            // result needs it: the same words mean one thing here and another on a machine.
            "placement": placement_json(h.placement.as_ref()),
            // Whether this head may act on the session at all: a caller that cannot
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
            // The package so far: every round this head watched, summed. `cost_usd`
            // and `tokens` above are the last round only.
            "spent": {
                "cost_usd": s.spent.cost_micros_usd.map(|m| m as f64 / 1_000_000.0),
                "prompt": s.spent.prompt_tokens,
                "cached": s.spent.cached_tokens,
                "predicted": s.spent.predicted_tokens,
                "rounds": s.rounds,
            },
            // The model's own background jobs still running: each ends in a turn that says so, so an
            // idle session holding one is waiting rather than done, unless the model parked it with
            // `wake: "later"`; the daemon's `activity` answers that and this list does not.
            "jobs_open": s.jobs_open.iter().collect::<Vec<_>>(),
            // And the same jobs with what their cgroups hold, from the kernel: a job whose host pid
            // is invisible from a sandboxed seat still has 75 processes and 17 GiB, and a model that
            // reads only `jobs_open` has to guess. An empty clause is a host with no cgroup.
            "jobs": s
                .jobs_open
                .iter()
                .map(|h| json!({
                    "handle": h,
                    "cgroup": s.jobs_cgroup.get(h).cloned().unwrap_or_default(),
                }))
                .collect::<Vec<_>>(),
            // What the DAEMON says the session is waiting on (`PROTOCOL_VERSION` 52), instead of
            // timed settle windows. `waiting_on` is the list
            // in a reader's words and `activity` is the value itself, so a caller can branch on it.
            "activity": serde_json::to_value(&s.activity).unwrap_or(Value::Null),
            "waiting_on": s.waiting_on(),
            // Whether an unfinished todo needs one of the running jobs: `wait` does not hold on
            // this any more - the daemon's activity does - and the key stays because it is a
            // question a caller already asks, answered now from the daemon's own list.
            "waiting_on_jobs": s.waiting_on_jobs(),
            "pending_asks": s.asks.iter().map(|a| a.req_id.clone()).collect::<Vec<_>>(),
            // What the daemon said to the last frame it refused. An answer that arrives after
            // its card closed is refused rather than applied, and an orchestrator that could not
            // see the refusal would keep believing it answered.
            "last_rejection": s.rejection,
            "seq": s.seq,
            // Non-null when the daemon is another build than this bridge, and says which is older.
            "protocol_skew": s.skew,
            // Lines from the daemon this build could not read and skipped.
            "unreadable_frames": s.unreadable,
            // What keeps the daemon from its idle exit, `nothing holds it`, or null for a daemon
            // that never idles out or did not answer in time.
            "idle_exit": h.idle_exit_within(IDLE_EXIT_PATIENCE),
            // What this session holds on disk, for the operator and for the agent that made the
            // leftovers: `holds: scope 3 processes, 1.2 GiB now of a 32.0 GiB cap · idle 12m`.
            // Null for a session this build never recorded an owner row for.
            "holds": crate::holds::status_line(self, session),
        });
        // What the last ending measured, which is the question: `prefix_check` says whether
        // zero reuse was a slot that did not hold the history or a prompt that diverged, and the
        // dialect's sha says which template produced it. Keys named once, in `State`.
        for (key, value) in s.measured_keys() {
            out[key] = value;
        }
        // The brief's report, when the last message is one. Absent, not empty, when it is
        // not. The newest report rather than the last message, because a run that reports and is
        // then woken by its own jobs ends on *nothing new in them*; `report_is_last` says which.
        if let Some(report) = crate::report::parse(s.last_message()) {
            out["report"] = report.to_json();
            out["report_is_last"] = json!(true);
        } else if let Some(report) = s.last_report.as_deref().and_then(crate::report::parse) {
            out["report"] = report.to_json();
            out["report_is_last"] = json!(false);
        }
        Ok(out)
    }

    /// The progress since a cursor: tool calls, results, jobs, turns, asks, warnings and the
    /// first line of what was said. A cursor older than the ring is answered with what remains
    /// *and* a `dropped` count, because a caller that believes it has the whole story cannot tell.
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

    /// Answer a card. The option must be one the ask offered; anything else is refused
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

    /// The conversation itself, from a cursor, as the daemon serves it to a head. Not the
    /// store: one SQLite file is shared by every daemon on the box, and a reader there sees
    /// sessions it was never told about. A watched session answers this too: reading is not acting.
    pub fn transcript(
        &self,
        session: &str,
        since: Option<u64>,
        limit: Option<u64>,
        reasoning: bool,
    ) -> Result<Value, String> {
        let h = self.head(session)?;
        let limit = limit.unwrap_or(crate::rows::LIMIT as u64).clamp(1, 1000) as usize;
        let snap = h.rows(&h.session, head::PEEK_PATIENCE)?;
        let mut out = crate::rows::page(&snap, since.unwrap_or(0), limit, reasoning);
        out["session"] = json!(session);
        Ok(out)
    }

    /// Move the session's approval point, and ask a PERSON for the one that needs them -
    /// `leticode_mode`. The bridge never consents: a program that sent `consented: true`
    /// for `allow-all` would be exactly the lie the consent exists to prevent.
    pub fn set_mode(&self, session: &str, mode: &str) -> Result<Value, String> {
        if mode.trim().is_empty() {
            return Err("`mode` is empty: name the approval point, e.g. `writes-allowed`".into());
        }
        let h = self.head(session)?;
        // Inside a VM the boundary IS the guest, so `allow-all` there needs nobody - and before the
        // worker has opened there is nobody who could be asked. The point is queued instead of a
        // card being put in front of a person who would be answering a question already settled.
        if in_a_vm(&h) {
            h.set_mode(mode.trim())?;
            return Ok(json!({
                "session": session,
                "mode": mode.trim(),
                "queued": true,
                "answer": "nobody",
                "said": "queued: a firecode VM is the boundary, so `allow-all` inside one needs no \
                         confirmation. It is applied, in order behind the model, when the session's \
                         worker opens",
            }));
        }
        let settled = h.ask_mode(mode.trim(), MODE_PATIENCE)?;
        Ok(json!({
            "session": session,
            "mode": settled.name,
            "applied": settled.applied,
            // Who settled it, in one word: `yes` (a person in a head), `no` (a person in a
            // head), or `nobody` (no person had to be asked).
            "answer": settled.answer,
            "said": settled.said,
        }))
    }

    /// Move the session's model, at the next turn boundary - `leticode_model`. The daemon's
    /// `/models` verb is per session and writes its row, so a restart restores it; a mid-turn switch
    /// takes at the turn's end, the daemon's rule.
    pub fn set_model(&self, session: &str, model: &str) -> Result<Value, String> {
        let model = model.trim();
        if model.is_empty() {
            return Err(
                "`model` is empty: name what should answer, e.g. `deepseek/deepseek-chat`, or \
                 `local` for this box's own server"
                    .into(),
            );
        }
        let h = self.head(session)?;
        // The same rule as the mode: a session whose worker is opening cannot answer `/models`, and
        // giving up on it is how a package's brief came to run on the daemon's own model.
        if in_a_vm(&h) {
            h.queue_model(model)?;
            return Ok(json!({
                "session": session,
                "model": model,
                "queued": true,
                "said": "queued until the VM is up: the switch is applied, in order, when the \
                         session's worker opens. `leticode_status` names the model it is on",
            }));
        }
        let said = h.set_model(model)?;
        Ok(json!({
            "session": session,
            "model": model,
            // The daemon's own sentence, which says when it takes: `turns go to … from the next one
            // on`, or `… when this turn ends`.
            "said": said,
        }))
    }

    /// Interrupt, then close the session here and leave: a placement settles as it closes, so the
    /// branch a host session worked on comes home now. The rows stay on disk, so any head can
    /// reopen it.
    pub fn stop(&self, session: &str) -> Result<Value, String> {
        let h = self.head(session)?;
        h.stop()?;
        let mut g = self.heads.lock().unwrap_or_else(|e| e.into_inner());
        g.remove(session);
        drop(g);
        // The watcher goes with the head, and the records it never delivered stay in the file: a
        // session started again on this folder is the same channel, resumed where it stopped.
        self.channels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session);
        let mut answer = json!({ "stopped": true, "session": session, "closed": true });
        // The daemon's own line carries the commit, and the head cannot read it - so it names the
        // branch the caller must look at, on the one rule the placement itself uses.
        if let Some(host) = h.placement.as_ref().and_then(Placement::host) {
            let branch = letibot_tools::remote::back_branch_for(host, session);
            // The daemon's own fetch on close is not seen from here, so the repo is asked, and a
            // branch that is not there is collected now. Either way the answer says which.
            let vm = matches!(h.placement, Some(Placement::RemoteVm(_)));
            let repo = h.worktree.clone();
            let collect = || {
                let hosts = Hosts::load();
                let declared = hosts
                    .get(host)
                    .ok_or_else(|| hosts.refusal(host).to_string())?;
                letibot_tools::remote::collect(&letibot_tools::remote::CollectSpec {
                    host: declared.clone(),
                    session: session.to_string(),
                    repo: repo.clone(),
                    ssh: None,
                    paths: letibot_tools::remote::Paths::from_env(),
                    vm,
                })
                .map_err(|e| e.to_string())
            };
            settle_branch(&mut answer, &h.worktree, &branch, collect);
        }
        Ok(answer)
    }

    /// Put a session this bridge holds out of sight - `leticode_archive`. The daemon closes it and
    /// writes the row, so nothing of it is left running here or there; the rows stay in the store,
    /// and opening the id is what takes it back into the list. The head goes with the session, which
    /// is why this drops it from the map exactly as `stop` does.
    pub fn archive(&self, session: &str, reason: &str) -> Result<Value, String> {
        let h = self.head(session)?;
        h.archive(reason)?;
        let mut g = self.heads.lock().unwrap_or_else(|e| e.into_inner());
        g.remove(session);
        drop(g);
        self.channels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session);
        Ok(json!({
            "session": session,
            "archived": true,
            "closed": true,
            "reason": reason,
            // The door back, said because it is not this tool: the session is out of SIGHT, not
            // gone, and a caller told only `archived: true` would think it had been deleted.
            "said": format!(
                "{session} is out of the list now and nothing is deleted: `letibot --sessions \
                 --archived` lists it, and opening it by id puts it back"
            ),
        }))
    }

    /// Collect a session that is already stopped. A placement's branch comes home as its session
    /// closes, which `letibot --stop` or a dead daemon skips. Collected by the names the placement
    /// was made from: session id, host and repository.
    pub fn collect(
        &self,
        session: &str,
        host: &str,
        folder: Option<&str>,
        vm: bool,
    ) -> Result<Value, String> {
        let hosts = Hosts::load();
        let name = host.trim();
        let Some(declared) = hosts.get(name) else {
            return Err(format!(
                "{} `leticode_collect` checks a `host` the same way `where: host:{name}` does, \
                 and nothing was fetched.",
                hosts.refusal(name)
            ));
        };
        // The folder is where the branch comes home: this process's own when the caller names none,
        // because an MCP server is started in the project it serves.
        let repo = match folder.map(str::trim).filter(|f| !f.is_empty()) {
            Some(folder) => std::path::PathBuf::from(folder),
            None => std::env::current_dir().map_err(|e| {
                format!(
                    "no `folder` was given and this process has no working directory to collect \
                     into: {e}"
                )
            })?,
        };
        let session = session.trim();
        let said = letibot_tools::remote::collect(&letibot_tools::remote::CollectSpec {
            host: declared.clone(),
            session: session.to_string(),
            repo,
            ssh: None,
            paths: letibot_tools::remote::Paths::from_env(),
            vm,
        })
        .map_err(|e| e.to_string())?;
        Ok(json!({
            "session": session,
            "host": name,
            // The branch, named here because the sentence is the daemon's own words and a caller
            // wants the ref it can diff.
            "branch": letibot_tools::remote::back_branch_for(name, session),
            "said": said,
        }))
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
                    "placement": placement_json(h.placement.as_ref()),
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

    /// One held session, or a refusal naming the ones it does hold - a caller that mistyped
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

/// The session an attach names. `gatekeeper` resolves through the workspace key, because the
/// review queue is one session per daemon (`s-gatekeeper-<workspace key>`) and every daemon on
/// the box shares one store, so two appending to one transcript is silent corruption.
fn session_name(raw: &str, key: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(
            "`session` is empty: an attach has to name the session it joins — an id, or \
                    `gatekeeper` for the review queue's"
                .into(),
        );
    }
    Ok(match raw {
        "gatekeeper" => letibot_sessionlog::gatekeeper_session_for_key(key),
        other => other.to_string(),
    })
}

/// The first line of a message, for a listing row - a status line is one line, and a
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

/// The `host` argument, as a placement, checked before anything is started. `None` is
/// [`Placement::Host`] - a placement, compared against a running daemon's record like any
/// other. A name is checked against the operator's `hosts.toml` here, with the same refusal.
fn placement_arg(host: Option<&str>, vm: bool, hosts: &Hosts) -> Result<Placement, String> {
    let Some(name) = host.map(str::trim).filter(|n| !n.is_empty()) else {
        // `vm` with no host is a VM on this box, which is the same placement `--vm` asks
        // for and needs no name from the operator's file.
        return Ok(if vm {
            Placement::Firecode
        } else {
            Placement::Host
        });
    };
    if hosts.get(name).is_none() {
        return Err(format!(
            "{} A `host` on `leticode_start` is the same check as `where: host:{name}`, and \
             nothing was started. Call the `hosts` tool (or read that file) for the machines \
             this box has, or leave `host` out to run the session's tools on this box.",
            hosts.refusal(name)
        ));
    }
    Ok(if vm {
        Placement::RemoteVm(name.to_string())
    } else {
        Placement::Remote(name.to_string())
    })
}

/// Whether this session's tools run in a VM. Two facts ride on it: the guest IS a boundary, so
/// `allow-all` inside one needs no person, and a worker that is booting one cannot answer a verb
/// about its own mode or model - so both are queued rather than waited for.
fn in_a_vm(h: &Head) -> bool {
    matches!(
        h.placement,
        Some(Placement::Firecode) | Some(Placement::RemoteVm(_))
    )
}

/// Where a held session's tools run, as an answer's field. `host` for this box and
/// `host:NAME` for a machine - the spellings [`Placement::parse`] reads back. `null` when the
/// daemon recorded nothing this build can read: *not known*, not *here*.
fn placement_json(placement: Option<&Placement>) -> Value {
    match placement {
        Some(p) => json!(p.as_str()),
        None => Value::Null,
    }
}

/// The same fact as a sentence, for the answer a model reads: where the work happens, and what
/// stayed on this box.
fn tools_run(placement: Option<&Placement>) -> String {
    match placement {
        Some(Placement::RemoteVm(host)) => format!(
            "inside a firecode VM on `{host}`: this session's bash, read, write, edit and jobs \
             run in the guest while its model, the store and the gate stay here. What it commits \
             comes back as the branch `remote/{host}/<short id>` when the VM stops"
        ),
        _ => match placement.and_then(Placement::host) {
            Some(host) => format!(
                "on `{host}`: this session's bash, read, write, edit and jobs run there; its \
                 model, the store and the gate stay here. What it commits comes back as the \
                 branch `remote/{host}/<short id>` when the session ends"
            ),
            None => "on this box".to_string(),
        },
    }
}

/// The ten tools, exactly as the design's table names them. The schemas are JSON Schema
/// objects because MCP requires `inputSchema` to be one, and `required` is stated rather than
/// inferred: a client that omits a field should be told which, not have it defaulted.
pub fn list() -> Value {
    let mut tools = vec![
        tool(
            "leticode_start",
            "Start leticode on a worktree and hold the session. Starts or joins that \
              folder's daemon, opens a session, sets the approval point, sends the brief as \
              the first prompt, and stays attached so the gate's asks reach this bridge. \
              Answers the session id every other tool takes. Its turn ends, asks, failures \
              and provider trouble arrive as <channel> events when the client loaded this \
              server's channel; without the channel, run the `wait` verb as a background \
              command afterwards — its exit is the notification. `host` places the session's TOOLS on \
              another machine: a daemon already running for this folder with a different \
              placement is refused, naming where it runs and how to stop it, because a \
              package must never end up running locally when its caller asked for a host.",
            json!({
                "type": "object",
                "properties": {
                    "worktree": { "type": "string", "description": "Absolute path to the checkout the work happens in." },
                    "brief": { "type": "string", "description": "The task, in full: what to build, the hard rules, and what to report." },
                    "model": { "type": "string", "description": "What answers THIS session's turns: a provider name (`deepseek`, `glm`, `grok`), `provider/model`, or a local model alias. Omitted: the daemon's own choice. It is set on the session, so it holds over a daemon already serving another provider, and a name this box cannot serve is refused before the brief is sent." },
                    "mode": { "type": "string", "description": "The approval point. Default `writes-allowed`: writes go through, commands and the network ask. `allow-all` needs the operator's confirmation and is refused by the daemon without it." },
                    "host": { "type": "string", "description": "Where this session's TOOLS run: a machine the operator declared in `~/.config/letibot/hosts.toml` — call the `hosts` tool or read that file for the names. Then bash, read, write, edit and jobs run there while the model, the store and the gate stay here. Omitted: on this box. An undeclared name is refused before anything starts." },
                    "vm": { "type": "boolean", "description": "Run this session's tools inside a firecode VM. With `host`, the VM is booted on THAT machine by its own firecode, over this session's ssh link, on a copy of the checkout; the guest is the boundary, so builds and tests run there without asking. Omitted: false. A host with no firecode, or no `/dev/kvm` this connection's user may open, is refused by name." }
                },
                "required": ["worktree", "brief"]
            }),
        ),
        tool(
            "leticode_attach",
            "Join a session that is already running — this folder's review queue, a \
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
                    "session": { "type": "string", "description": "The session id, or the name `gatekeeper` for THIS folder's review queue — one session per daemon now, so the name resolves through the folder it attaches to (`s-gatekeeper-<workspace key>`)." },
                    "watch_only": { "type": "boolean", "description": "Default true: watch and never act — no prompt, no answer, no interrupt. False drives the session as `leticode_start` would, and takes over answering its asks." }
                },
                "required": ["worktree", "session"]
            }),
        ),
        tool(
            "leticode_send",
            "Send an operator line into a running session. Queued behind a running turn as \
              the head's own prompt, so it never loses a race with what the model is doing; \
              the daemon starts a turn for it when the current one ends.",
            json!({
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "The id `leticode_start` answered, or the session's name (the `/rename` title or its slug) in this folder." },
                    "text": { "type": "string", "description": "What to say to the session." }
                },
                "required": ["session", "text"]
            }),
        ),
        tool(
            "leticode_status",
            "What the session is doing: `running`, `idle`, `asking` or `failed`, the last \
              assistant message, the todos, the last turn's tokens and cost, and which asks \
              are pending. `model` is what answers this session's turns, as `/models` spells it, \
               and `last_trouble` is the last time the provider itself was stuck — a rate \
               limit (with the reset instant it gave), an exhausted account, a timeout or a \
               stall — so a caller can wait it out or move the session instead of guessing. \
               `placement` says where the session's tools run — `host` for this \
              box, `host:NAME` for another machine, null when the daemon recorded nothing. \
              When that message is the brief's closing report, it also comes \
              back parsed: `report.built`, `report.tested`, `report.could_not`, \
              `report.contracts` and the `report.commits` it named — the newest report, \
              with `report_is_last` false when words came after it. `spent` sums every \
              round since the bridge attached; `cost_usd` and `tokens` are the last round.",
            json!({
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "The session id, or its name (the `/rename` title or its slug) in this folder." }
                },
                "required": ["session"]
            }),
        ),
        tool(
            "leticode_events",
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
                    "session": { "type": "string", "description": "The session id, or its name (the `/rename` title or its slug) in this folder." },
                    "since": { "type": "integer", "minimum": 0, "description": "The `next` cursor of the last answer. Omitted: everything the ring still holds." },
                    "limit": { "type": "integer", "minimum": 1, "description": "How many entries to answer with. Default 50; the ring holds 500." }
                },
                "required": ["session"]
            }),
        ),
        tool(
            "leticode_asks",
            "The permission asks still owed an answer: the ask id, the call (tool and \
              target), the sentence the gate wrote, and the options the ladder offers. \
              Answer one with `leticode_answer`; a turn stopped on a card does not continue \
              without one.",
            json!({
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "The session id, or its name (the `/rename` title or its slug) in this folder." }
                },
                "required": ["session"]
            }),
        ),
        tool(
            "leticode_answer",
            "Answer an open ask. A PERMISSION ask takes one of the option ids it offered, sent \
              as the protocol's Answer frame, so the decision lands in the corpus attributed to \
              this bridge exactly as the operator's own. A QUESTION ask (the model's own \
              ask_user_question) takes one of its `choices` by exact text or 1-based number, \
              `free:<words>` for a typed reply, or `abstain`; `note` qualifies a chosen option.",
            json!({
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "The session id, or its name (the `/rename` title or its slug) in this folder." },
                    "ask": { "type": "string", "description": "The ask id from `leticode_asks` (its `ask` field)." },
                    "option": { "type": "string", "description": "For a permission: one option id it offered (`allow_once`, `allow_session`, `allow_always`, `reject_once`, `reject_always` — the ask says which). For a question: a choice's text or number, `free:<words>`, or `abstain`." },
                    "note": { "type": "string", "description": "What the model is told: for a `deny_and_tell` option, or a qualification of a question's chosen option." }
                },
                "required": ["session", "ask", "option"]
            }),
        ),
        tool(
            "leticode_diff",
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
            }),
        ),
        tool(
            "leticode_transcript",
            "The session's CONVERSATION, row by row, as the daemon serves it to a head — the \
              same rows a terminal attached to that session is drawing. Full text: the \
              operator's prompts, the model's answers, every tool call (name and arguments) and \
              every tool result (outcome and payload). Long strings are cut only beyond the cap \
              this answer states (`cap`, 8000 characters), and a row that was cut says so. Pass \
              the `next` of the last answer as `since` and you get only what is new. It reads \
              the daemon, never the session store; it works on a session this bridge holds and \
              on one it only watches.",
            json!({
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "The session id, or its name (the `/rename` title or its slug) in this folder." },
                    "since": { "type": "integer", "minimum": 0, "description": "The `next` cursor of the last answer — how many rows you have already read. Omitted: from the oldest row the daemon still holds (which says `dropped` for the ones it does not)." },
                    "limit": { "type": "integer", "minimum": 1, "description": "How many rows to answer with. Default 100, at most 1000." },
                    "reasoning": { "type": "boolean", "description": "Include the model's own reasoning blocks as rows. Default false: they are the one part of a transcript nothing downstream reads." }
                },
                "required": ["session"]
            }),
        ),
        tool(
            "leticode_mode",
            "Move the session's approval point. A point that needs nobody is applied at once \
              (`writes-allowed`, `read-only`, `allow-all` inside a VM). `allow-all` on a box with \
              no boundary needs the OPERATOR's confirmation: the daemon puts the same card a \
              terminal shows for `/mode allow-all` in front of the heads attached to that \
              session, and the point moves only when a person says yes — this bridge never \
              consents on its own. With nobody attached to ask, nothing is changed and the \
              answer is `needs the operator's confirmation in a head: run leticode --session \
              <id>`.",
            json!({
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "The session id, or its name (the `/rename` title or its slug) in this folder." },
                    "mode": { "type": "string", "description": "The approval point: `writes-allowed`, `read-only`, `allow-all`, … The daemon's own set decides which; a name it does not know is refused by name." }
                },
                "required": ["session", "mode"]
            }),
        ),
        tool(
            "leticode_model",
            "Move a RUNNING session to another model without restarting anything. The switch is the \
              daemon's own `/models` verb: it is per session, it is written to the session's row so a \
              restart restores it, and a switch that arrives mid-turn takes effect when the turn ends \
              - never inside one, because the rest of a half-finished turn belongs to the model that \
              started it. Works on a session this bridge started and on one it joined, including a \
              session of a daemon another session started. The answer's `said` is the daemon's own \
              sentence, which says when the switch takes.",
            json!({
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "The session id, or its name (the `/rename` title or its slug) in this folder." },
                    "model": { "type": "string", "description": "What should answer from now on: a provider name (`deepseek`, `glm`, `grok`), `provider/model`, or `local` for this box's own server. A name this box cannot serve is refused by name and nothing changes." }
                },
                "required": ["session", "model"]
            }),
        ),
        tool(
            "leticode_stop",
            "Interrupt a running turn, close the session here and detach this bridge from it. \
              Closing it is what settles a PLACEMENT: what a host session committed comes home \
              as the branch `remote/<host>/<short id>` (`branch` in the answer) as the session \
              closes, and the daemon's log names the commit it landed at. The rows stay on \
              disk, so `letibot --session <id>` or another head can reopen it.",
            json!({
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "The session id, or its name (the `/rename` title or its slug) in this folder." }
                },
                "required": ["session"]
            }),
        ),
        tool(
            "leticode_archive",
            "Put a session this bridge holds out of sight and let go of it. The daemon closes \
             it and writes the row, so the conversation stops here either way; nothing is \
             deleted, the rows stay in the store, and `letibot --sessions --archived` lists it. \
             It comes back by being opened: `letibot --session <id>` takes it into the list again, \
             and so does attaching to it. Unarchiving a session this bridge does not hold is the \
             CLI's `--unarchive`.",
            json!({
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "The session id, or its name (the `/rename` title or its slug) in this folder." },
                    "reason": { "type": "string", "description": "Why it is being put away, in a short line: it is stored on the row and printed beside the session in an `--archived` listing." }
                },
                "required": ["session"]
            }),
        ),
        tool(
            "leticode_collect",
            "Fetch what a session that is already stopped committed on its host, as the branch \
            `remote/<host>/<short id>`. `leticode_stop` brings the branch home as it closes and answers `branch_home` (true, or false with `branch_said`), \
            but a daemon stopped underneath the session - or one that died - reaches no close, \
            and then the work stays on that machine. Nothing of the daemon is needed: the \
            session id, the host and the repository are the names the placement was made from. \
            Answers `branch` and a sentence saying what arrived, or why not.",
            json!({
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "The session id - the one `leticode_start` answered, or any recorded for a run that is over." },
                    "host": { "type": "string", "description": "The machine it was placed on, as `hosts.toml` names it. Checked here, and an undeclared name is refused with the list." },
                    "folder": { "type": "string", "description": "The repository the branch comes home to. Left out, this process's own working directory is used." },
                    "vm": { "type": "boolean", "description": "The placement ran in a firecode guest, so its commits are in that checkout and not in the mirror (default false)." }
                },
                "required": ["session", "host"]
            }),
        ),
        tool(
            "leticode_list",
            "The sessions this bridge is holding, with their state, pending ask count, \
              where each one's tools run (`placement`) and \
              whether each is only watched (`leticode_attach`'s default) or driven. It lists \
              what this process holds, not what the daemon has on disk.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "holds",
            "What the owners of this folder hold on disk right now - a package's VM, its cgroup \
             scope with its process count and memory, its worktree and target sizes, its firecode \
             copies, its daemon - each with the stamp that proves whose it is, its state and what \
             will happen to it. The facts come from the kernel and the filesystem, so a seat in a \
             sandbox can read its own leftovers: a job's pid it cannot see is not evidence of \
             anything, and this never asks `ps`. Read-only, always. Omit `owner` for every owner \
             this folder's daemon recorded, which is how a seat finds the packages it started.",
            json!({
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "One owner: a session id, `s-…`. Omitted: every owner in this folder's store." }
                }
            }),
        ),
        tool(
            "gc_plan",
            "What `letibot gc` would reap for this folder, and why, and when - the read-only plan. \
             Each holding gets a verdict (`reap now`, `reap at`, `keep`) and one sentence; a \
             directory that looks like letibot's and names no owner the store knows is reported \
             under `looks_like_ours` and is never reaped by pattern. Nothing here removes, stops or \
             writes anything: this build's reaping is G2 (compute), G3 (work), G4 (caches).",
            json!({
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "One owner, `s-…`. Omitted: the whole folder." }
                }
            }),
        ),
    ];
    tools.extend(crate::rooms::tool_list());
    tools.extend(todo_list());
    json!({ "tools": tools })
}

/// The seat's board, as three tools: the session's `todo_write` - its own schema and its own
/// description, so the two runtimes cannot answer the same call differently - and the two the
/// bridge needs beside it, a read and the operator's answer.
fn todo_list() -> Vec<Value> {
    let session_tool = letibot_tools::builtins::todo::TodoWriteTool::new(std::sync::Arc::new(
        letibot_tools::builtins::todo::TodoBoard::new(Vec::new()),
    ));
    let schema = letibot_tools::runtime::Tool::schema(&session_tool);
    vec![
        tool("todo_write", &schema.description, schema.parameters),
        tool(
            "todos",
            "This seat's todo board as it stands: every row, its status, its waits and the \
             version they last moved at. The board is the store's, under this seat's handle, so \
             it survives a bridge restart - read it back with this rather than from memory.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "todo_answer",
            "The operator's answer to a question a row waits on them for: `todo_write`'s \
             `wait_on {operator: {question, options}}` puts the question on the board, you ask \
             it in the conversation, and this records their reply on the row so the row stops \
             waiting. The words are theirs, kept as they said them.",
            json!({
                "type": "object",
                "properties": {
                    "content": {"type": "string", "description": "The row, quoted exactly."},
                    "question": {"type": "string", "description": "The question it waits on."},
                    "reply": {"type": "string", "description": "What the operator answered, in their words."}
                },
                "required": ["content", "question", "reply"]
            }),
        ),
        tool(
            "seat_job_start",
            "Run a command as a job of YOURS - the seat's own, not a package's - and answer the \
             handle. A seat is not a session, so nothing else here can background a command for \
             it: this is the local gate, the long build, the thing a row waits on while the \
             package turn it was waiting for is over. `argv` is run verbatim (no shell), its \
             output goes to a file under this bridge's rundir, and the row's `todo_write \
             {wait_on: [{content, job: \"<handle>\"}]}` is quiet until it ends. Its ending arrives \
             as a <channel> event `job_ended`, the same frame a package's jobs use.",
            json!({
                "type": "object",
                "properties": {
                    "argv": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "The command and its arguments, each already a word: `[\"scripts/correctness_gate.sh\", \"--ratchet\", \"0\"]`. No shell, so a pipe or a `&&` is a word the program gets, not something this runs."
                    },
                    "cwd": {"type": "string", "description": "Where to run it. Omitted: the bridge's own working directory."},
                    "name": {"type": "string", "description": "A short name for the listing, so `seat_jobs` reads as work rather than as handles."}
                },
                "required": ["argv"]
            }),
        ),
        tool(
            "seat_jobs",
            "Every job this seat has started, oldest first, with the state each ended in - `running` \
             only while its process is really there. The `log` is the file its output went to, and \
             `pid`/`start` are what `wait_on {pid}` names.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "seat_job_kill",
            "End one of this seat's own jobs: SIGKILL to that exact process. Its ending - the row's \
             release and the `job_ended` event - follows on the seat's next pass.",
            json!({
                "type": "object",
                "properties": {
                    "job": {"type": "string", "description": "The handle `seat_job_start` answered, `j…`."}
                },
                "required": ["job"]
            }),
        ),
    ]
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": schema })
}

/// Which tool names this build answers - the list `tools/list` publishes, and the one
/// `tools/call` checks against, so a name can never be in one and not the other.
pub fn names() -> Vec<&'static str> {
    let mut all = leticode_names();
    all.extend(LIFECYCLE_TOOLS);
    all.extend(crate::rooms::TOOLS);
    all.extend(TODO_TOOLS);
    all
}

/// The lifecycle verbs: what this folder holds, and what would be reaped. Served to every agent
/// that loads this server, because a package's leftovers are the package's to see first.
pub const LIFECYCLE_TOOLS: [&str; 2] = ["holds", "gc_plan"];

/// The same list with this folder's `tool_` layers after it (X25): what `tools/list` publishes and
/// `tools/call` checks, so a name a layer declares is in both or neither.
pub fn names_with(bridge: &Bridge) -> Vec<String> {
    let native: Vec<String> = names().iter().map(|n| n.to_string()).collect();
    let mut all = native.clone();
    all.extend(crate::plugins::names(&bridge.folder(), &native));
    all
}

/// The tool list, native first and the folder's plugin tools after them.
pub fn list_with(bridge: &Bridge) -> Value {
    let mut list = list();
    let native: Vec<String> = names().iter().map(|n| n.to_string()).collect();
    let declared = crate::plugins::tools(&bridge.folder(), &native);
    if declared.is_empty() {
        return list;
    }
    if let Some(tools) = list["tools"].as_array_mut() {
        tools.extend(declared);
    }
    list
}

/// The seat's own verbs, registered here and served by `crate::seat` and `crate::seat_jobs`.
pub const TODO_TOOLS: [&str; 6] = [
    "todo_write",
    "todos",
    "todo_answer",
    "seat_job_start",
    "seat_jobs",
    "seat_job_kill",
];

fn leticode_names() -> Vec<&'static str> {
    vec![
        "leticode_start",
        "leticode_attach",
        "leticode_send",
        "leticode_status",
        "leticode_events",
        "leticode_transcript",
        "leticode_mode",
        "leticode_model",
        "leticode_asks",
        "leticode_answer",
        "leticode_diff",
        "leticode_stop",
        "leticode_archive",
        "leticode_collect",
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

/// A whole-number argument that may be absent - a cursor, a limit.
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

/// Run one tool. `Err` is a caller's mistake (an argument, a session this bridge does
/// not hold); a tool that ran and failed says so in its answer with `isError`.
pub fn call(bridge: &Bridge, name: &str, args: &Value) -> Result<Value, String> {
    // A tool call is the seat working: the plan check waits for its end, exactly as a session's
    // waits for the end of a turn.
    bridge.touch_seat();
    match name {
        "leticode_start" => {
            let worktree = arg_str(args, "worktree")?;
            let brief = arg_str(args, "brief")?;
            let model = arg_opt_str(args, "model")?;
            let mode = arg_opt_str(args, "mode")?.unwrap_or_else(|| DEFAULT_MODE.to_string());
            let host = arg_opt_str(args, "host")?;
            let vm = arg_bool(args, "vm", false)?;
            bridge.start(
                &worktree,
                &brief,
                model.as_deref(),
                &mode,
                host.as_deref(),
                vm,
            )
        }
        "leticode_attach" => {
            let worktree = arg_str(args, "worktree")?;
            let session = bridge.resolve_session(&arg_str(args, "session")?, Some(&worktree))?;
            let watch_only = arg_bool(args, "watch_only", true)?;
            bridge.attach(&worktree, &session, watch_only)
        }
        "leticode_send" => bridge.send(&session_arg(bridge, args)?, &arg_str(args, "text")?),
        "leticode_status" => bridge.status(&session_arg(bridge, args)?),
        "leticode_events" => {
            let session = session_arg(bridge, args)?;
            let since = arg_count(args, "since")?;
            let limit = arg_count(args, "limit")?;
            bridge.events(&session, since, limit)
        }
        "leticode_transcript" => {
            let session = session_arg(bridge, args)?;
            let since = arg_count(args, "since")?;
            let limit = arg_count(args, "limit")?;
            let reasoning = arg_bool(args, "reasoning", false)?;
            bridge.transcript(&session, since, limit, reasoning)
        }
        "leticode_mode" => {
            let session = session_arg(bridge, args)?;
            let mode = arg_str(args, "mode")?;
            bridge.set_mode(&session, &mode)
        }
        "leticode_model" => {
            let session = session_arg(bridge, args)?;
            let model = arg_str(args, "model")?;
            bridge.set_model(&session, &model)
        }
        "leticode_asks" => bridge.asks(&session_arg(bridge, args)?),
        "leticode_answer" => {
            let session = session_arg(bridge, args)?;
            let ask = arg_str(args, "ask")?;
            let option = arg_str(args, "option")?;
            let note = arg_opt_str(args, "note")?;
            bridge.answer(&session, &ask, &option, note.as_deref())
        }
        "leticode_diff" => {
            let full = arg_bool(args, "full", false)?;
            bridge.diff(&session_arg(bridge, args)?, full)
        }
        "leticode_stop" => bridge.stop(&session_arg(bridge, args)?),
        "leticode_archive" => {
            let session = session_arg(bridge, args)?;
            let reason = arg_opt_str(args, "reason")?;
            bridge.archive(&session, reason.as_deref().unwrap_or(""))
        }
        "leticode_collect" => {
            let host = arg_str(args, "host")?;
            let folder = arg_opt_str(args, "folder")?;
            let session = bridge.resolve_session(&arg_str(args, "session")?, folder.as_deref())?;
            let vm = arg_bool(args, "vm", false)?;
            bridge.collect(&session, &host, folder.as_deref(), vm)
        }
        "leticode_list" => Ok(bridge.list()),
        "holds" => crate::holds::holds(bridge, arg_opt_str(args, "owner")?.as_deref()),
        "gc_plan" => crate::holds::gc_plan(bridge, arg_opt_str(args, "owner")?.as_deref()),
        "todo_write" => bridge.seat()?.write(args),
        "todos" => Ok(bridge.seat()?.board_json()),
        "todo_answer" => bridge.seat()?.answer(args),
        "seat_job_start" => {
            let Some(argv) = args.get("argv").and_then(|a| a.as_array()) else {
                return Err(
                    "`argv` is required: the command and its arguments, as a list of \
                            words"
                        .into(),
                );
            };
            let cwd = arg_opt_str(args, "cwd")?;
            let name = arg_opt_str(args, "name")?;
            bridge
                .seat()?
                .jobs()
                .start(argv, cwd.as_deref(), name.as_deref())
        }
        "seat_jobs" => Ok(json!({ "jobs": bridge.seat()?.jobs().listing() })),
        "seat_job_kill" => {
            let job = arg_str(args, "job")?;
            bridge.seat()?.jobs().kill(&job)
        }
        room if crate::rooms::TOOLS.contains(&room) => {
            crate::rooms::call(bridge.rooms(), room, args)
        }
        // A `tool_` layer of this folder's stack (X25): listed after the native tools, and run
        // through the same hook the daemon's runtime calls.
        declared if names_with(bridge).iter().any(|n| n == declared) => {
            let stack = bridge.plugin_stack().ok_or_else(|| {
                format!(
                    "`{declared}` is declared by a plugin layer, and this bridge could not build \
                     the stack it answers from"
                )
            })?;
            crate::plugins::call(&stack, declared, args, &bridge.folder())
        }
        other => Err(format!(
            "unknown tool `{other}`; this build serves {}",
            names_with(bridge).join(", ")
        )),
    }
}

/// The `session` argument of a call, a name resolved to its id.
fn session_arg(bridge: &Bridge, args: &Value) -> Result<String, String> {
    bridge.resolve_session(&arg_str(args, "session")?, None)
}

/// How long the `wait` verb will wait when the caller named no `--timeout`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

/// The wake-up. Attach, wait for the first thing that needs a person, say which: the exit code
/// is the notification. The head is a watcher, so a session never waits on a card nobody could
/// answer. Codes: `0` ended, `10` ask, `20` failed, `30` deadline, `40` provider in trouble.
pub fn wait(worktree: &str, session: &str, deadline: Instant) -> (i32, String) {
    wait_with(worktree, session, deadline, false)
}

/// [`wait`], where `exit_on_stall` makes a provider stall end it with `40` instead of being ridden out.
pub fn wait_with(
    worktree: &str,
    session: &str,
    deadline: Instant,
    exit_on_stall: bool,
) -> (i32, String) {
    let d = match daemon::resolve(Path::new(worktree)) {
        Ok(d) => d,
        Err(e) => return (20, e),
    };
    let h = match Head::attach_watching(&d, session) {
        Ok(h) => h,
        Err(e) => return (20, e),
    };
    match h.wait_with(deadline, exit_on_stall) {
        Attention::TurnEnded { message } => (0, message),
        Attention::Ask(ask) => (10, ask.to_json().to_string()),
        Attention::Failed { why } => (20, why),
        // The typed event itself, so a caller's next move - sleep until `reset_ms`, or switch the
        // session's model - needs no second call to ask what happened.
        Attention::Trouble(t) => (40, t.to_json().to_string()),
        Attention::Timeout => (30, String::new()),
    }
}

/// Put `branch_home` (and why not) into a stop answer: true when `branch` is in `repo`, either
/// already or after `collect` fetched it; false with the reason when neither holds.
fn settle_branch(
    answer: &mut Value,
    repo: &Path,
    branch: &str,
    collect: impl FnOnce() -> Result<String, String>,
) {
    answer["branch"] = json!(branch);
    if branch_exists(repo, branch) {
        answer["branch_home"] = json!(true);
        return;
    }
    match collect() {
        Ok(said) if branch_exists(repo, branch) => {
            answer["branch_home"] = json!(true);
            answer["branch_said"] = json!(said);
        }
        Ok(said) => {
            answer["branch_home"] = json!(false);
            answer["branch_said"] = json!(format!(
                "the collect said `{said}` but `{branch}` is not in {}",
                repo.display()
            ));
        }
        Err(why) => {
            answer["branch_home"] = json!(false);
            answer["branch_said"] = json!(format!(
                "`{branch}` is not in {} and fetching it failed: {why}",
                repo.display()
            ));
        }
    }
}

/// One room message as the relay reads it: a verdict goes to the client's writer, which is the only
/// thread that speaks to it, and the one line that corrects the operator is a DM on its own thread.
fn on_message(
    relay: &crate::relay::Relay,
    link: &crate::rooms::RoomLink,
    push: &std::sync::mpsc::Sender<Value>,
    message: &Value,
) {
    match relay.see(message) {
        crate::relay::Reply::Nothing => {}
        crate::relay::Reply::Send(frame) => {
            let _ = push.send(frame);
        }
        crate::relay::Reply::Say(line) => {
            let link = link.clone();
            std::thread::spawn(move || {
                if let Err(why) = crate::rooms::dm(&link, letibot_room::OPERATOR, &line) {
                    eprintln!("letibot-mcp: the reply did not reach the operator: {why}");
                }
            });
        }
    }
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_repo(tag: &str) -> std::path::PathBuf {
        let dir = letibot_sessionlog::testing::scratch_root()
            .join(format!("stop-branch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            vec!["init", "-q"],
            vec![
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "x",
            ],
        ] {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(&dir)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        dir
    }

    #[test]
    fn stop_fetches_a_branch_that_is_not_home_and_says_so_when_it_cannot() {
        let repo = a_repo("settle");
        let branch = "remote/lab/abc12345";
        let make = |repo: std::path::PathBuf| {
            move || {
                let ok = std::process::Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(["branch", "remote/lab/abc12345"])
                    .status()
                    .unwrap()
                    .success();
                assert!(ok);
                Ok("fetched".to_string())
            }
        };
        // Absent: the collect runs and the branch is then home.
        let mut a = json!({});
        settle_branch(&mut a, &repo, branch, make(repo.clone()));
        assert_eq!(a["branch_home"], true, "{a}");
        assert_eq!(a["branch_said"], "fetched", "{a}");
        // Present: the collect is not run.
        let mut b = json!({});
        settle_branch(&mut b, &repo, branch, || panic!("already home"));
        assert_eq!(b["branch_home"], true, "{b}");
        // Absent and the fetch fails: said plainly.
        let mut c = json!({});
        settle_branch(&mut c, &repo, "remote/lab/none0000", || {
            Err("ssh refused".into())
        });
        assert_eq!(c["branch_home"], false, "{c}");
        let said = c["branch_said"].as_str().unwrap();
        assert!(
            said.contains("ssh refused") && said.contains("remote/lab/none0000"),
            "{said}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Thirty-four tools (the leticode verbs, the room verbs, the todo and seat jobs), each with a
    /// schema and a required list - the count and the names the design's table fixes. A tool added
    /// to `tools/list` and forgotten in `call` (or the other way round) is the defect this pairs
    /// against.
    #[test]
    fn the_thirty_six_tools_are_listed_with_schemas() {
        let listed = list();
        let tools = listed["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 36, "{listed}");
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

    /// A collect refuses a host the operator's file does not declare, before anything is
    /// fetched - the same check `where: host:NAME` makes, with the list of names that would work.
    /// A name nobody declared cannot reach a host, so this asks nothing of the network.
    #[test]
    fn a_collect_refuses_an_undeclared_host_by_name() {
        let name = format!("no-such-host-{}", std::process::id());
        let e = Bridge::new()
            .collect("s-01abcdef", &name, Some("."), false)
            .expect_err("an undeclared host is refused");
        assert!(
            e.contains(&name),
            "the refusal names the host asked for: {e}"
        );
        assert!(
            e.contains("nothing was fetched"),
            "and it says nothing happened: {e}"
        );
    }

    /// `leticode_start` publishes `host`, and does not require it. A field a client cannot
    /// see is a feature nobody uses; a field in `required` is a call every existing caller
    /// would have to change to keep working on this box.
    #[test]
    fn the_start_schema_names_host_without_requiring_it() {
        let listed = list();
        let start = listed["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|t| t["name"] == "leticode_start")
            .expect("leticode_start");
        let host = &start["inputSchema"]["properties"]["host"];
        assert_eq!(host["type"], "string", "{host}");
        let described = host["description"].as_str().unwrap_or_default();
        // The two halves a caller needs: where the names come from, and what moves.
        assert!(described.contains("hosts.toml"), "{described}");
        assert!(described.contains("`hosts` tool"), "{described}");
        assert!(
            start["inputSchema"]["required"]
                .as_array()
                .expect("required")
                .iter()
                .all(|v| v != "host"),
            "omitting `host` is a session on this box, and every earlier caller relies on it"
        );
    }

    /// A host name is checked before anything is started, with the tree's own refusal. The
    /// `Hosts` is built from text rather than from `$LETIBOT_HOSTS`, because a unit test that set
    /// a process-wide variable would race every other test in this binary.
    #[test]
    fn an_undeclared_host_is_refused_with_the_names_this_box_has() {
        let hosts = Hosts::parse("[host.lab2x1]\nhostname = \"lab2x1\"\n").expect("a hosts file");
        assert_eq!(
            placement_arg(None, false, &hosts).expect("no host asked for"),
            Placement::Host,
            "asking for nothing is a placement, and it is this box"
        );
        assert_eq!(
            placement_arg(Some("  "), false, &hosts).expect("an empty host is none"),
            Placement::Host
        );
        assert_eq!(
            placement_arg(Some("lab2x1"), false, &hosts).expect("a declared host"),
            Placement::Remote("lab2x1".into())
        );
        // The typo: `lab2` is not `lab2x1`, and the refusal says so with the list.
        let e = placement_arg(Some("lab2"), false, &hosts).expect_err("undeclared");
        assert!(e.contains("there is no host `lab2`"), "{e}");
        assert!(e.contains("it knows: lab2x1"), "{e}");
        assert!(
            e.contains("nothing was started"),
            "the caller has to know no daemon was begun on its behalf: {e}"
        );
        assert!(e.contains("hosts.toml"), "{e}");
    }

    /// `vm` is the second ask, and each of its three shapes is a different placement. A VM
    /// on a host is neither a session on that host nor a VM here, so the flag has to reach the
    /// daemon as both.
    #[test]
    fn the_vm_flag_turns_a_host_into_a_vm_on_that_host() {
        let hosts = Hosts::parse("[host.lab2x1]\nhostname = \"lab2x1\"\n").expect("a hosts file");
        assert_eq!(
            placement_arg(Some("lab2x1"), true, &hosts).expect("a declared host"),
            Placement::RemoteVm("lab2x1".into())
        );
        assert_eq!(
            placement_arg(None, true, &hosts).expect("a VM here"),
            Placement::Firecode,
            "`vm` without a host is a VM on this box"
        );
        // A name nobody declared is still refused, whatever `vm` says.
        assert!(placement_arg(Some("lab2"), true, &hosts).is_err());
    }

    /// The placement an answer carries is the spelling a caller can ask for again, and
    /// *nothing recorded* is `null` rather than the word for *here*.
    #[test]
    fn a_placement_is_answered_in_the_words_it_is_asked_for() {
        assert_eq!(placement_json(Some(&Placement::Host)), json!("host"));
        assert_eq!(
            placement_json(Some(&Placement::Remote("lab2x1".into()))),
            json!("host:lab2x1")
        );
        assert_eq!(placement_json(None), Value::Null);
        // And the sentence says what moved AND what did not, because the second half is what
        // decides whether a result from that session can be read here.
        let remote = tools_run(Some(&Placement::Remote("lab2x1".into())));
        assert!(remote.contains("on `lab2x1`"), "{remote}");
        assert!(remote.contains("stay here"), "{remote}");
        assert_eq!(tools_run(Some(&Placement::Host)), "on this box");
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
        // rather than being read as zero - a cursor silently read as zero re-sends a ring.
        for bad in [json!(-1), json!("12"), json!(1.5)] {
            let e = call(
                &b,
                "leticode_events",
                &json!({ "session": "s-1", "since": bad }),
            )
            .expect_err("not a whole number");
            assert!(e.contains("`since`"), "{bad}: {e}");
        }
        // The transcript's cursor is the same kind of number, and its `reasoning` flag is a
        // boolean: a client that sent `"true"` would otherwise get the default and be told
        // nothing about it.
        let e = call(
            &b,
            "leticode_transcript",
            &json!({ "session": "s-1", "since": "2" }),
        )
        .expect_err("not a whole number");
        assert!(e.contains("`since`"), "{e}");
        let e = call(
            &b,
            "leticode_transcript",
            &json!({ "session": "s-1", "reasoning": "yes" }),
        )
        .expect_err("not a boolean");
        assert!(e.contains("`reasoning`"), "{e}");
        // And `leticode_mode` names both of its fields, because neither can be defaulted: a
        // point nobody asked for and a session nobody named are two different refusals.
        let e = call(&b, "leticode_mode", &json!({ "session": "s-1" })).expect_err("no mode");
        assert!(e.contains("`mode`"), "{e}");
        let e = call(&b, "leticode_mode", &json!({ "mode": "read-only" })).expect_err("no session");
        assert!(e.contains("`session`"), "{e}");
        // An empty point is refused before anything is sent: it names no point at all.
        let e = b
            .set_mode("s-1", "  ")
            .expect_err("an empty point is not a point");
        assert!(e.contains("`mode` is empty"), "{e}");
    }

    /// A session this bridge does not hold is refused by name, and the refusal says what it
    /// does hold - the caller's next move is one call, not a listing.
    #[test]
    fn an_unknown_session_is_refused_with_what_is_held() {
        let b = Bridge::new();
        let e = b.status("s-nope").expect_err("not held");
        assert!(e.contains("s-nope"), "{e}");
        assert!(e.contains("leticode_start"), "{e}");
        // And an empty bridge lists empty, rather than failing: a listing is a question.
        assert_eq!(b.list()["sessions"].as_array().map(|v| v.len()), Some(0));
    }

    /// `leticode_archive` is published with its one required field, and a session this bridge does
    /// not hold is refused by the same door every other verb uses - the map, not a silence.
    #[test]
    fn archiving_a_session_the_bridge_does_not_hold_is_refused_by_name() {
        let tools = list();
        let archive = tools["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .find(|t| t["name"] == "leticode_archive")
            .expect("leticode_archive is published");
        assert_eq!(
            archive["inputSchema"]["required"],
            json!(["session"]),
            "a session is the one field with no default"
        );
        assert!(
            names().contains(&"leticode_archive"),
            "a name in the schema and not in the call table is a tool that fails on every call"
        );
        let b = Bridge::new();
        let e = b.archive("s-nope", "done with it").expect_err("not held");
        assert!(e.contains("s-nope"), "{e}");
        assert!(e.contains("leticode_start"), "{e}");
    }

    /// `gatekeeper` is the review queue's id, not a second spelling of it: the name
    /// resolves to THIS daemon's queue - one session per workspace. An id passes through, and an
    /// empty one is refused rather than read as "whatever the daemon calls current".
    #[test]
    fn the_gatekeeper_name_resolves_to_this_daemons_queue() {
        // The workspace key the caller resolved to find the daemon: `abc123def456` stands for the
        // twelve hex characters `launcher::key` derives from a folder.
        assert_eq!(
            session_name("gatekeeper", "abc123def456").expect("a name"),
            letibot_sessionlog::gatekeeper_session_for_key("abc123def456")
        );
        // And two folders' daemons get two ids, so a review queue is not one string on the box:
        // two daemons appending to one transcript is the corruption the store's append-only trigger then refuses.
        assert_ne!(
            session_name("gatekeeper", "abc123def456").expect("a name"),
            session_name("gatekeeper", "789abc012def").expect("a name")
        );
        assert_eq!(
            session_name(" s-1789 ", "abc123def456").expect("an id"),
            "s-1789"
        );
        let e = session_name("  ", "abc123def456").expect_err("an attach names a session");
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
