//! One top-level session's connection to the box's room - the `room` tool's verbs behind it, the
//! bell that becomes a wake, and the beat the daemon runs while a turn does. A subagent has no link
//! at all: the room is for top-level sessions, and the refusal that matters is the room's own.

use std::path::Path;
use std::sync::{Arc, Mutex, Weak};

use letibot_room::client::{Beat, Client};
use letibot_room::{Cause, Decision, Marker, Request, Response, Speaker};

use super::supervise::{RestartFrame, RoomSupervisor};
use super::wake;
use crate::plugins::room_feed::{RestartView, RoomFeed, RoomSnapshot};

/// How often the room is read for the pane, and how old a drawn picture may be. One second
/// because that is the finest thing a row says and the feed's clock is quantised to the same second
/// it runs on its own thread, so a wedged coordinator never holds the daemon's idle pass.
pub const PANE_READ_EVERY_MS: u64 = 1_000;

/// What a session's `room` tool talks through: empty until a connection exists, filled when one is
/// made, so a coordinator that starts after the session still reaches it.
#[derive(Default)]
pub struct LateRoom {
    slot: Mutex<Option<Arc<SessionRoom>>>,
}

impl LateRoom {
    pub fn get(&self) -> Option<Arc<SessionRoom>> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set(&self, room: Arc<SessionRoom>) {
        *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(room);
    }
}

impl letibot_tools::builtins::room::RoomLink for LateRoom {
    fn call(&self, verb: &str, args: &serde_json::Value) -> Result<String, String> {
        match self.get() {
            Some(room) => room.call(verb, args),
            None => letibot_tools::builtins::room::NoRoom.call(verb, args),
        }
    }

    fn beats_on_my_behalf(&self) -> bool {
        self.get().is_some()
    }
}

/// Opens a session's connection to the room; kept so a later pass can try again.
pub type Connector = Box<dyn Fn() -> Result<Arc<SessionRoom>, String> + Send>;

/// A session's room. One per top-level session, held by its `Harness` and by its tool.
pub struct SessionRoom {
    client: Client,
    session: String,
    socket: std::path::PathBuf,
    /// Which project's room this connection is in - the half of the room's name that is not the
    /// box's. Resolved once, at the open, because a session does not move between projects.
    project: String,
    /// The tree, from the daemon that can see it - `None` on a link whose caller has no
    /// registry (a fixture, the coordinator's own daemon). A link with none sends no `Members`.
    roster: Option<Arc<dyn crate::room::roster::RosterSource>>,
    /// The beat, while a turn is running. `None` between turns, which is exactly when the lease is
    /// counting - see [`SessionRoom::turn_ended`].
    beat: Mutex<Option<Beat>>,
    /// The room as the PANE last saw it - read on [`SessionRoom::reader`]'s thread, so that
    /// drawing a pane is a clone and never a socket call. `None` until the first read lands.
    room: Mutex<Option<RoomSnapshot>>,
    /// Who may restart the box's room on this session's behalf - `None` on the coordinator's
    /// own daemon, on a box with no launcher, and on a fixture: a pane that offers no key rather
    /// than a key that would refuse.
    supervisor: Option<Arc<dyn RoomSupervisor>>,
    /// The room instance this link last saw - the lease's `started_ms`. A change here that no
    /// marker claims is a crash, which is the one cause of a restart nobody asks for.
    seen: Mutex<Option<u64>>,
    /// The worker's bell, kept as well as handed to the client - see [`SessionRoom::note_gap`]
    /// a gap in the connection is the one thing no event can carry, because the events are exactly
    /// what it did not hear.
    bell: Arc<dyn Fn(&str) + Send + Sync>,
    /// The newest chat this session heard, for the pane and for `room read`'s tail. Bounded.
    recent: Arc<Mutex<std::collections::VecDeque<letibot_room::Message>>>,
    /// The rooms this session sits in besides its own: the ones it was invited into, from the
    /// room's own `rooms` answer. What a wake is allowed to come from.
    invited: Arc<Mutex<std::collections::BTreeSet<String>>>,
    /// The one room its project makes, or the host room when it has no project.
    mine: String,
    /// Each todo row's last seen status, by content. `None` until the first update, which is
    /// the board as it was and not news.
    todo_seen: Mutex<Option<std::collections::BTreeMap<String, String>>>,
    /// Plain sessions of this daemon, so a daemon-level fact is reported through one of them.
    plain: bool,
}

/// Tell the room `session` has taken chat up to `seq`, which is its seen mark: the room retells
/// anything after it on the next join. Off the caller's thread, which is a turn or the reader.
pub(crate) fn ack_taken(session: &str, seq: u64) {
    let Some(room) = links()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(session)
        .and_then(Weak::upgrade)
    else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("room-ack".into())
        .spawn(move || {
            if let Err(e) = room.client.request(&Request::Ack { seq }) {
                eprintln!(
                    "harnessd: `{}` could not ack room #{seq}: {e}",
                    room.session
                );
            }
        });
}

fn links() -> &'static Mutex<std::collections::BTreeMap<String, Weak<SessionRoom>>> {
    static LINKS: std::sync::OnceLock<
        Mutex<std::collections::BTreeMap<String, Weak<SessionRoom>>>,
    > = std::sync::OnceLock::new();
    LINKS.get_or_init(Default::default)
}

/// A merge-queue or review-queue row moved: say so in the room of the row's repository, or the host
/// room when no session of that project is here. Every session hears a move, so one live link
/// reports it and the coordinator drops a repeat of the same request id.
pub fn report_queue_move(queue: &str, id: &str, to: &str, why: &str, repository: Option<&str>) {
    let key = repository
        .filter(|r| !r.trim().is_empty())
        .map(letibot_room::project_key);
    report_queue_move_in(queue, id, to, why, key);
}

/// [`report_queue_move`] for a project already named by its key.
pub fn report_queue_move_in(queue: &str, id: &str, to: &str, why: &str, key: Option<String>) {
    let plain: Vec<Arc<SessionRoom>> = links()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .filter_map(Weak::upgrade)
        .filter(|l| l.plain)
        .collect();
    let (link, room) = match key
        .as_ref()
        .and_then(|k| plain.iter().find(|l| &l.project == k))
    {
        Some(l) => (l.clone(), ""),
        None => match plain.first() {
            Some(l) => (l.clone(), "host"),
            None => return,
        },
    };
    let text = if why.trim().is_empty() {
        format!("{queue} queue: {id} -> {to}")
    } else {
        format!("{queue} queue: {id} -> {to} ({})", why.trim())
    };
    link.notify_in(
        room,
        "queue",
        format!(
            "queue:{}:{queue}:{id}:{to}:{why}",
            key.as_deref().unwrap_or("host")
        ),
        text,
        vec![letibot_room::Ref {
            kind: "queue".into(),
            id: id.to_string(),
        }],
    );
}

/// Chat lines the daemon keeps per session for its feed.
pub const RECENT_CHAT: usize = 100;

impl std::fmt::Debug for SessionRoom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionRoom")
            .field("session", &self.session)
            .field("socket", &self.socket)
            .finish()
    }
}

impl SessionRoom {
    /// Connect as `session`, with `bell` rung for every event the room sends about it.
    /// `bell` is a closure rather than the registry so this type needs no daemon. `project` is
    /// which room this is - the name on the wire is `room_name(&host_key, project)`.
    pub fn connect(
        session: &str,
        socket: &Path,
        project: &str,
        roster: Option<Arc<dyn crate::room::roster::RosterSource>>,
        bell: Arc<dyn Fn(&str) + Send + Sync>,
        supervisor: Option<Arc<dyn RoomSupervisor>>,
    ) -> Result<Arc<SessionRoom>, String> {
        let me = session.to_string();
        // This box's key, read once. Both questions below derive from it, and `host_key` shells
        // out to `uname -n`: a session's open need not pay for that twice.
        let key = letibot_room::host_key();
        // The arbiter is not woken by the queue's traffic. The server routes every event to it -
        // that is how a question reaches it - but a turn per `post` would be a turn per item
        // somebody else made, on the one session whose job is to answer questions.
        let arbiter = session == letibot_room::coordinator_session(&key);
        // A tag's agent hears its own tag, and a note is not a turn - the operator's cost answer
        // a claim, a healthy beat and a finished item are comparisons the room already made, so they
        // arrive as notes and only `Raised` and `Asked` ring the bell.
        let tag = letibot_room::tag_of_session(&key, session);
        let wake: letibot_room::client::Wake = {
            let bell = Arc::clone(&bell);
            let me = me.clone();
            Arc::new(move |event: &letibot_room::Event| {
                use letibot_room::Event;
                if let Some(tag) = tag.as_deref() {
                    // A `Ruled` carries no tag of its own, so its arrival IS the addressing: the
                    // server only sends one to the sessions the item's tag reaches.
                    let mine = event.tag() == tag || matches!(event, Event::Ruled { .. });
                    if !mine {
                        return;
                    }
                    if let Some(line) = wake::sentence(event) {
                        wake::push(&me, line);
                    }
                    if matches!(event, Event::Raised { .. } | Event::Asked { .. }) {
                        bell(&me);
                    }
                    return;
                }
                if arbiter
                    && matches!(
                        event,
                        Event::Posted { .. } | Event::Claimed { .. } | Event::Done { .. }
                    )
                {
                    return;
                }
                // The push, then the ring - a ring with nothing behind it is a wake that finds
                // nothing and costs a pass over the store.
                if let Some(line) = wake::sentence(event) {
                    wake::push(&me, line);
                    bell(&me);
                }
            })
        };
        // The room's name, derived from the box and the project - never invented, and the
        // coordinator checks a `Hello` against the same two halves.
        let named = letibot_room::room_name(&key, project);
        let client = Client::connect_named(socket, Speaker::top_level(session), named, wake)
            .map_err(|e| e.to_string())?;
        // Chat: kept for the feed, and a message wakes a turn when it names this session, comes
        // from the operator, or is said to a room it sits in. A tag's agent and the arbiter work
        // the queue and are never woken by chat.
        let recent: Arc<Mutex<std::collections::VecDeque<letibot_room::Message>>> = Arc::default();
        let invited: Arc<Mutex<std::collections::BTreeSet<String>>> = Arc::default();
        // The room its project makes - the host room for a session in no project, which is the
        // coordinator's own. Rule: a session is woken by its own rooms, never by host chatter
        // between others.
        let mine = if project.is_empty() {
            letibot_room::chat::HOST_ROOM.to_string()
        } else {
            letibot_room::chat::project_room(project)
        };
        {
            let recent = Arc::clone(&recent);
            let invited = Arc::clone(&invited);
            let bell = Arc::clone(&bell);
            let me = me.clone();
            let sits_in = {
                let invited = Arc::clone(&invited);
                let mine = mine.clone();
                move |room: &str| {
                    room == mine
                        || invited
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .contains(room)
                }
            };
            let plain = letibot_room::tag_of_session(&key, session).is_none() && !arbiter;
            client.on_chat(Arc::new(move |m: &letibot_room::Message, names: bool| {
                {
                    let mut r = recent.lock().unwrap_or_else(|e| e.into_inner());
                    r.push_back(m.clone());
                    while r.len() > RECENT_CHAT {
                        r.pop_front();
                    }
                }
                // The room's own facts (a todo row done, a queue move) are kind `system` and
                // wake only the session they are addressed to; a session woke on its own rows.
                let wakes = m.kind != letibot_room::Seat::System || names;
                if plain && wakes && m.wakes(&me, names, &sits_in) {
                    wake::push_chat(&me, m.seq, wake::chat_sentence(m, names));
                    bell(&me);
                } else if !wake::seen_meanwhile(&me, m.seq) {
                    // Nothing here will take it as a note, so it is seen on arrival.
                    ack_taken(&me, m.seq);
                }
            }));
        }
        let plain = letibot_room::tag_of_session(&key, session).is_none() && !arbiter;
        let room = Arc::new(SessionRoom {
            recent,
            invited,
            mine,
            todo_seen: Mutex::new(None),
            plain,
            client,
            session: session.to_string(),
            socket: socket.to_path_buf(),
            project: project.to_string(),
            roster,
            beat: Mutex::new(None),
            room: Mutex::new(None),
            supervisor,
            // The first sighting is not a crash. A daemon that connects to a room which has
            // been up for an hour did not witness anything, so no marker is written.
            seen: Mutex::new(letibot_room::room_started_ms(socket)),
            bell,
        });
        SessionRoom::reader(&room);
        links()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session.to_string(), Arc::downgrade(&room));
        Ok(room)
    }

    /// Start the thread that keeps the pane's picture of the room. It holds a `Weak`, so a
    /// reader never keeps the connection open - the same rule the client's own reader keeps.
    fn reader(me: &Arc<SessionRoom>) {
        let weak: Weak<SessionRoom> = Arc::downgrade(me);
        let _ = std::thread::Builder::new()
            .name(format!("room-pane-{}", me.session))
            .spawn(move || {
                loop {
                    // The first read happens now, not a second from now: a session's panes are
                    // announced as it opens, and the frame after this one should have a room in it.
                    let Some(me) = weak.upgrade() else {
                        return;
                    };
                    let snap = me.read_room();
                    *me.room.lock().unwrap_or_else(|e| e.into_inner()) = Some(snap);
                    me.note_instance();
                    // The tree, on the same tick as the pane's read. A room keeps no copy of the tree, so a
                    // restarted coordinator knows nobody until a daemon says - and sending it every
                    // poll rather than on a change is what makes that self-healing.
                    me.say_who_is_here();
                    // And a gap in the bell, said once. A connection that was down and is back
                    // was handed a count with its `Welcome`; the events are gone, so the session is
                    // woken to re-read the state rather than left believing it heard everything.
                    me.note_gap();
                    drop(me);
                    std::thread::sleep(std::time::Duration::from_millis(PANE_READ_EVERY_MS));
                    if weak.strong_count() == 0 {
                        // The session is gone. Its `Client` went with it, which closed the socket -
                        // the coordinator's `note_gone` - so there is nothing to do here.
                        return;
                    }
                }
            });
    }

    /// Tell the room who is in it - the project's tree, from the daemon that can see it. A
    /// failure is not said: the read above already draws an unreachable room as `unreadable`.
    fn say_who_is_here(&self) {
        let Some(roster) = self.roster.as_ref() else {
            return;
        };
        let rows = roster.members(&self.project);
        let root = roster.root(&self.project);
        let _ = self.client.request(&Request::Members { rows, root });
    }

    /// Report a fact into this session's project room (or the host's when it has none), off the
    /// caller's thread: a wedged coordinator must not stall a turn.
    fn notify(&self, source: &str, id: String, text: String, refs: Vec<letibot_room::Ref>) {
        self.notify_in("", source, id, text, refs);
    }

    /// As [`SessionRoom::notify`], into `room` (`""` is this session's project room).
    fn notify_in(
        &self,
        room: &str,
        source: &str,
        id: String,
        text: String,
        refs: Vec<letibot_room::Ref>,
    ) {
        let room = room.to_string();
        let client = self.client.clone();
        let source = source.to_string();
        let _ = std::thread::Builder::new()
            .name("room-notify".into())
            .spawn(move || {
                let _ = client.request(&Request::Notify {
                    id,
                    room,
                    source,
                    text,
                    refs,
                });
            });
    }

    /// The todo board changed: a new row is *filed*, a row whose status moved says what it
    /// became. At most five lines per update. The first update only records the board.
    pub fn todos_changed(&self, todos: &[letibot_sessionlog::event::TodoEntry]) {
        use letibot_sessionlog::event::{TodoStatus, todo_title};
        let now: std::collections::BTreeMap<String, String> = todos
            .iter()
            .map(|t| {
                let word = match t.status {
                    TodoStatus::Pending => "pending",
                    TodoStatus::InProgress => "started",
                    TodoStatus::Completed => "done",
                    TodoStatus::Postponed => "postponed",
                    TodoStatus::Cancelled => "cancelled",
                };
                (t.content.clone(), word.to_string())
            })
            .collect();
        let before = self
            .todo_seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(now.clone());
        let Some(before) = before else { return };
        let mut lines = Vec::new();
        for (content, status) in &now {
            let said = match before.get(content) {
                None => "filed",
                Some(was) if was != status => status.as_str(),
                Some(_) => continue,
            };
            let title: String = todo_title(content).chars().take(100).collect();
            lines.push((said, title));
        }
        let extra = lines.len().saturating_sub(5);
        for (said, title) in lines.into_iter().take(5) {
            self.notify(
                "todo",
                format!("todo:{}:{said}:{title}", self.session),
                format!("todo {said}: {title}"),
                vec![letibot_room::Ref {
                    kind: "todo".into(),
                    id: format!("{}/{title}", self.session),
                }],
            );
        }
        if extra > 0 {
            self.notify(
                "todo",
                format!("todo:{}:more:{}", self.session, now.len()),
                format!("{extra} more todo row(s) moved"),
                Vec::new(),
            );
        }
    }

    /// Which project's room this is - the key half of its name.
    pub fn project(&self) -> &str {
        &self.project
    }

    /// The room, in one read - every item, every open question, the arbiter, and the identity.
    /// A room that cannot be read is not an error here: the snapshot's `unreadable` carries the
    /// sentence, because the pane must draw *nobody answered* rather than an empty queue.
    fn read_room(&self) -> RoomSnapshot {
        // Which rooms this session is in, asked on the same tick as the rest: an invitation is the
        // room's to give, and a session woken by a room it was invited to needs to know which.
        if let Ok(Response::Rooms { rows }) = self.client.request(&Request::Rooms) {
            let mut invited = self.invited.lock().unwrap_or_else(|e| e.into_inner());
            invited.clear();
            // Its own room is not an invitation, and neither is the host room: every session may
            // read host, and reading it is not sitting in it (rule 5).
            invited.extend(
                rows.into_iter()
                    .map(|r| r.id)
                    .filter(|id| id != &self.mine && id != letibot_room::chat::HOST_ROOM),
            );
        }
        let welcome = self.client.welcome();
        let base = RoomSnapshot {
            lease_ms: welcome.lease_ms,
            threshold: welcome.threshold,
            // Every read carries the restart note, because the note is about the room the read is
            // of: queues drawn without saying the room had just begun again would be the opaque
            // screen the feature exists to remove.
            restart: self.restart_view(),
            chat: self
                .recent
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .cloned()
                .collect(),
            ..RoomSnapshot::default()
        };
        match self.client.request(&Request::Room) {
            Ok(Response::Room {
                identity,
                arbiter,
                connected,
                items,
                questions,
                agents,
                members,
            }) => RoomSnapshot {
                identity,
                arbiter,
                connected,
                items,
                questions,
                agents,
                members,
                unreadable: None,
                ..base
            },
            Ok(other) => RoomSnapshot {
                unreadable: Some(format!(
                    "the room answered the pane's read with something else: {other:?}"
                )),
                ..base
            },
            Err(e) => RoomSnapshot {
                unreadable: Some(e.to_string()),
                ..base
            },
        }
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Restart the box's room - asked of its supervisor, never performed here. The order is the
    /// design: ask the lease whether there was a room to stop (a restart and a start are different
    /// sentences), ask the supervisor, write the note for the NEW instance, then re-read at once.
    pub fn restart(&self) -> Result<String, String> {
        let supervisor = self.supervisor.as_ref().ok_or_else(|| {
            "this daemon holds no supervisor for the box's room, so it cannot restart it. The \
             launcher is what starts a coordinator (`letibot --room`), and the room's own daemon \
             is never given one — a restart from its own pane would kill the feed that reports it"
                .to_string()
        })?;
        let frame = RestartFrame {
            at_ms: letibot_room::server::now_ms(),
            by: self.session.clone(),
            was_running: letibot_room::room_running(&self.socket),
            why: "pressed restart in the room pane".to_string(),
        };
        let said = supervisor.restart(&frame)?;
        let instance = letibot_room::room_started_ms(&self.socket).unwrap_or(0);
        // The cause is what the act found, not what it hoped - a restart with nothing running
        // is a start, and saying so is the difference between *my key emptied the queue* and *the
        // room had already gone*.
        let cause = if frame.was_running {
            Cause::Asked
        } else {
            Cause::Daemon
        };
        let marker = Marker::new(frame.at_ms, instance, cause).by(self.session.clone());
        if let Err(e) = marker.write(&letibot_room::marker_path(&self.socket)) {
            return Ok(format!(
                "{said}, but the note that says why could not be written ({}): {} — the pane \
                 will read this as a room that came back on its own",
                letibot_room::marker_path(&self.socket).display(),
                e
            ));
        }
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(instance);
        let snap = self.read_room();
        *self.room.lock().unwrap_or_else(|e| e.into_inner()) = Some(snap);
        Ok(format!(
            "{said} — and the queues are read back from the store beside the socket"
        ))
    }

    /// Watch the room's instance, and write the note nobody asked for. An instance that is not
    /// the one this link last saw, and that no marker already claims, is a room that died and came
    /// back: a restart this daemon asked for is not a crash, and an act's note is never overwritten.
    fn note_instance(&self) {
        let Some(instance) = letibot_room::room_started_ms(&self.socket) else {
            // No lease is no room: there is no new instance to name, and the pane's own
            // `unreadable` sentence is the honest report until one appears.
            return;
        };
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        match *seen {
            Some(prev) if prev == instance => return,
            None => {
                *seen = Some(instance);
                return;
            }
            Some(_) => {}
        }
        *seen = Some(instance);
        let path = letibot_room::marker_path(&self.socket);
        if Marker::read(&path).is_some_and(|m| m.claims(instance)) {
            return;
        }
        let says = format!(
            "the room this one replaced began {} ago",
            letibot_room::duration(letibot_room::server::now_ms().saturating_sub(instance))
        );
        let _ = Marker::new(letibot_room::server::now_ms(), instance, Cause::Crashed)
            .saying(says)
            .write(&path);
    }

    /// Report a gap in the bell, once - the room keeps no log, so an event addressed to a
    /// session whose connection was down is gone and the count is all the room can say. A note
    /// nobody rings for is a mailbox; ringing once per gap keeps the tag agents' cost rule.
    fn note_gap(&self) {
        let (count, since_ms) = self.client.take_missed();
        if count == 0 {
            return;
        }
        wake::push(
            &self.session,
            wake::missed_line(count, since_ms, letibot_room::server::now_ms()),
        );
        (self.bell)(&self.session);
    }

    /// What the pane is told about the restart - whether this host may ask for one, why not
    /// when it may not, and the note the last one left.
    fn restart_view(&self) -> RestartView {
        RestartView {
            available: self.supervisor.is_some(),
            why: self.supervisor.is_none().then(|| {
                "this host was not given a supervisor for the box's room, so there is no restart \
                 key here — a coordinator is started by the launcher (`letibot --room`)"
                    .to_string()
            }),
            marker: letibot_room::read_marker(&self.socket),
        }
    }

    /// A turn is running: say so for as long as it does. Idempotent - a second call while a beat
    /// is already running is the same beat, because two would double every message.
    pub fn turn_started(&self) {
        let mut slot = self.beat.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(self.client.start_beating());
        }
    }

    /// The turn is over: stop beating. The lease now counts from the last beat, which is the
    /// fact the room measures - a session whose turn ended is alive and *not working on it*.
    pub fn turn_ended(&self) {
        let mut slot = self.beat.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(beat) = slot.take() {
            beat.stop();
        }
    }

    /// One verb, and the sentence a model reads back.
    /// `claim` acknowledges itself - silence reads as absence, and a model at work will not think
    /// about the queue's bookkeeping. It is `said` and not `waits_on`, which silences the reminder.
    pub fn call(&self, verb: &str, args: &serde_json::Value) -> Result<String, String> {
        let request = request_of(verb, args)?;
        let acknowledges = matches!(verb, "claim" | "claim_item");
        let response = self
            .client
            .request(&request)
            .map_err(|e| format!("the room at {} did not answer: {e}", self.socket.display()))?;
        let said = answer_sentence(&response, &self.session)?;
        if !acknowledges {
            return Ok(said);
        }
        let Response::Claimed { item, .. } = &response else {
            return Ok(said);
        };
        let doing = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
        Ok(match self.ack(item.id, doing) {
            Ok(what) => format!("{said} — and it is on the room, before the work: {what}"),
            Err(why) => format!(
                "{said}, but saying what it is for did not reach the room ({why}) — the claim \
                 itself stands, so the item is not lost, but nobody queued behind you can see what \
                 you are doing with it"
            ),
        })
    }

    /// "Seen, doing X", on the room - the sentence [`SessionRoom::call`] adds to a claim.
    /// It is [`Request::Acknowledge`] and not [`Request::Waiting`]: `waiting` writes
    /// `Claim::waits_on`, which silences every reminder there is.
    fn ack(&self, item: u64, doing: &str) -> Result<String, String> {
        let doing = doing.trim();
        let text = if doing.is_empty() {
            format!("seen #{item}; working on it")
        } else {
            format!("seen #{item}: {doing}")
        };
        self.client
            .request(&Request::Acknowledge {
                id: request_id(&format!("ack-{item}")),
                item,
                text: text.clone(),
            })
            .map(|_| text)
            .map_err(|e| e.to_string())
    }

    /// Every reminder the room owes, as data rather than as a sentence. The `reminders` VERB
    /// answers a model in words, and a daemon that parsed its own sentence back would be reading its
    /// own formatting. It is the WHOLE room's list, so the caller filters by `Reminder::session`.
    pub fn reminders(&self) -> Result<Vec<letibot_room::Reminder>, String> {
        let response = self
            .client
            .request(&Request::Reminders)
            .map_err(|e| format!("the room at {} did not answer: {e}", self.socket.display()))?;
        match response {
            Response::Reminders { rows } => Ok(rows),
            Response::Bye { why } => Err(format!("the room closed the connection: {why}")),
            // A frame that is not the answer to this read - said as a fact rather than guessed
            // at, because a daemon that invented a meaning here would put a wrong row on a board.
            _ => Err(
                "the room answered a frame that is not a `reminders` read — nothing was written on \
                 the board"
                    .to_string(),
            ),
        }
    }
}

impl letibot_tools::builtins::room::RoomLink for SessionRoom {
    fn call(&self, verb: &str, args: &serde_json::Value) -> Result<String, String> {
        SessionRoom::call(self, verb, args)
    }
}

/// The pane's half - the room as a feed is built from, and the four controls performed. The
/// LAYER decides which row to draw and which verb it offers; what a `claim` DOES is the room's
/// state machine, done over the session's own connection - a second close gives the claims back.
impl RoomFeed for SessionRoom {
    fn snapshot(&self) -> RoomSnapshot {
        let welcome = self.client.welcome();
        match self.room.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            Some(snap) => snap,
            // The first frame is a promise, and the second is the answer. A pane opened before
            // the first read lands says so rather than drawing an empty room.
            None => RoomSnapshot {
                lease_ms: welcome.lease_ms,
                threshold: welcome.threshold,
                restart: self.restart_view(),
                unreadable: Some(
                    "the room has not been read yet — the first read is a second away".to_string(),
                ),
                ..RoomSnapshot::default()
            },
        }
    }

    /// The pane's fifth control - the restart, asked of the supervisor rather than of the room.
    fn restart(&self) -> Result<String, String> {
        SessionRoom::restart(self)
    }

    fn act(&self, entry: &str, verb: &str) -> Result<String, String> {
        let frame = pane_frame(verb, entry)?;
        let response = self
            .client
            .request(&frame)
            .map_err(|e| format!("the room at {} did not answer: {e}", self.socket.display()))?;
        // A claim made by a key acknowledges itself too - the third rule is about the act, not
        // about who performed it: a person who takes a row and walks off is the same silence.
        if let Response::Claimed { item, .. } = &response
            && verb == "claim"
        {
            let _ = self.ack(item.id, "");
        }
        // And re-read at once, so the frame the person sees next is the state their key made -
        // a second of *nothing happened* is the one thing a key must never look like.
        let snap = self.read_room();
        *self.room.lock().unwrap_or_else(|e| e.into_inner()) = Some(snap);
        answer_sentence(&response, &self.session)
    }
}

/// A request id that cannot repeat - the verb, the entry, the clock, and a counter. The clock
/// alone is not enough, and the failure is silent: the room answers a repeated `(session, id)`
/// from its replay window, so two acts inside one millisecond lose the second -
fn request_id(what: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{what}-{}-{}",
        letibot_room::server::now_ms(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A row's key, as a frame - the PANE's grammar, which is not the tool's: a model names its
/// arguments, while a row names an ENTRY (`i12`, `q3:hand:s-a`). `release` and `abandon` are one
/// verb and two sentences, and the sentence is the host's to write.
fn pane_frame(verb: &str, entry: &str) -> Result<Request, String> {
    let id = request_id(&format!("{verb}-{entry}"));
    let item_of = |entry: &str| -> Result<u64, String> {
        entry
            .strip_prefix('i')
            .and_then(|n| n.parse::<u64>().ok())
            .ok_or_else(|| {
                format!("`{entry}` names no item — a row's entry is `i<number>`, and a question's is `q<number>:<decision>`")
            })
    };
    match verb {
        "claim" => Ok(Request::ClaimItem {
            id,
            item: item_of(entry)?,
        }),
        "release" => Ok(Request::Abandon {
            id,
            item: item_of(entry)?,
            why: "released from the room pane".to_string(),
        }),
        "abandon" => Ok(Request::Abandon {
            id,
            item: item_of(entry)?,
            why: "marked abandoned from the room pane".to_string(),
        }),
        "rule" => {
            let (question, rest) = entry.split_once(':').ok_or_else(|| {
                format!("`{entry}` names no decision — a question's entry is `q<number>:retry|drop|hand:<session>`")
            })?;
            let question: u64 = question
                .strip_prefix('q')
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| {
                    format!("`{entry}` names no question — a question's entry is `q<number>:…`")
                })?;
            let (decision, to) = match rest.split_once(':') {
                Some((d, t)) => (d, Some(t)),
                None => (rest, None),
            };
            let decision = match (decision, to) {
                ("retry", None) => Decision::Retry,
                ("drop", None) => Decision::Drop {
                    why: "dropped from the room pane".to_string(),
                },
                ("hand", Some(to)) if !to.is_empty() => Decision::Hand { to: to.to_string() },
                ("hand", _) => {
                    return Err(format!(
                        "`{entry}` names no session — `hand` needs one, and the pane draws a row \
                         per session the room has met (`q3:hand:s-a`)"
                    ));
                }
                (other, _) => {
                    return Err(format!(
                        "`{other}` is not a decision — say `retry`, `drop` or `hand:<session>`"
                    ));
                }
            };
            Ok(Request::Rule {
                id,
                question,
                decision,
                project: None,
            })
        }
        other => Err(format!(
            "`{other}` is not one of the room pane's four controls — they are claim, release, \
             abandon and rule"
        )),
    }
}

/// The verb and its arguments, as a frame. `Err` is the tool's own refusal, in its words.
fn request_of(verb: &str, args: &serde_json::Value) -> Result<Request, String> {
    let s = |k: &str| {
        args.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let n = |k: &str| args.get(k).and_then(|v| v.as_u64());
    // The id is minted here, per call, and it is what makes a reconnect safe: a repeat after a
    // dropped socket is answered from the room's replay window instead of acting twice.
    let id = request_id(verb);
    Ok(match verb {
        "post" => Request::Post {
            id,
            tag: s("tag"),
            body: s("body"),
        },
        "claim" => Request::Claim { id, tag: s("tag") },
        "claim_item" => Request::ClaimItem {
            id,
            item: n("item").ok_or("`claim_item` needs `item`")?,
        },
        "beat" => Request::Beat {
            id,
            item: n("item").ok_or("`beat` needs `item`")?,
        },
        "waiting" => Request::Waiting {
            id,
            item: n("item").ok_or("`waiting` needs `item`")?,
            text: s("text"),
        },
        "done" => Request::Done {
            id,
            item: n("item").ok_or("`done` needs `item`")?,
        },
        "abandon" => Request::Abandon {
            id,
            item: n("item").ok_or("`abandon` needs `item`")?,
            why: s("why"),
        },
        "dispute" => Request::Dispute {
            id,
            item: n("item").ok_or("`dispute` needs `item`")?,
            says: s("says"),
        },
        "rule" => {
            let decision = match s("decision").as_str() {
                "retry" => Decision::Retry,
                "hand" => {
                    let to = s("to");
                    if to.is_empty() {
                        return Err(
                            "`decision: \"hand\"` needs `to`: the session that gets the claim"
                                .to_string(),
                        );
                    }
                    Decision::Hand { to }
                }
                "drop" => Decision::Drop { why: s("why") },
                other => {
                    return Err(format!(
                        "`{other}` is not a decision — say `retry`, `hand` (with `to`) or `drop` \
                         (with `why`)"
                    ));
                }
            };
            Request::Rule {
                id,
                question: n("question").ok_or("`rule` needs `question`")?,
                decision,
                // The session's own room, which is the room its connection is in: a model
                // ruling a question it was handed names no project.
                project: None,
            }
        }
        "say" => {
            let mut refs = Vec::new();
            for kind in ["todo", "queue"] {
                let id = s(kind);
                if !id.is_empty() {
                    refs.push(letibot_room::Ref {
                        kind: kind.to_string(),
                        id,
                    });
                }
            }
            Request::Say {
                id,
                room: chat_room(&s("room")),
                text: s("text"),
                to: Some(s("to")).filter(|x| !x.is_empty()),
                thread: n("thread"),
                refs,
                // P1 gives the session tool an `attachments: [path]` and uploads the files.
                attachments: Vec::new(),
            }
        }
        "read" => Request::Read {
            room: chat_room(&s("room")),
            since: n("since").unwrap_or(0),
            // A bare read is the tail, not the whole ring.
            last: Some(n("last").unwrap_or(20).min(200) as u32),
            thread: n("thread"),
            before: n("before"),
        },
        "queue" => Request::Queue { tag: s("tag") },
        "holding" => Request::Holding {
            session: Some(s("session")).filter(|x| !x.is_empty()),
        },
        "reminders" => Request::Reminders,
        "questions" => Request::Questions,
        "invite" => Request::Invite {
            room: s("room"),
            session: s("session"),
        },
        "leave" => Request::Leave {
            room: s("room"),
            session: Some(s("session")).filter(|x| !x.is_empty()),
        },
        other => {
            return Err(format!(
                "`{other}` is not a room verb — the verbs are post, claim, claim_item, beat, \
                 waiting, done, abandon, dispute, rule, queue, holding, reminders, questions, say, read, \
                 invite and leave"
            ));
        }
    })
}

/// The room a model names: `host` is the host's, empty or `project` is its own project's.
fn chat_room(named: &str) -> String {
    match named {
        "project" => String::new(),
        other => other.to_string(),
    }
}

/// What the model reads back. Every arm names the item and the state, because a reply a model
/// cannot check is a reply it will ask about again.
fn answer_sentence(response: &Response, me: &str) -> Result<String, String> {
    Ok(match response {
        Response::Posted { item, .. } => format!(
            "posted #{id} under `{tag}` — {}",
            describe(item),
            id = item.id,
            tag = item.tag
        ),
        Response::Claimed { item, .. } => format!(
            "you hold #{id} under `{tag}` — {}; your daemon beats for you while your turn runs",
            describe(item),
            id = item.id,
            tag = item.tag
        ),
        Response::Beaten { item, .. } => format!("#{} — your beat is recorded", item.id),
        Response::Waited { item, .. } => format!(
            "#{} — the room will remind you of this, and whoever is queued behind you can see it",
            item.id
        ),
        Response::Acknowledged { item, .. } => format!(
            "#{} — the room has it: whoever is queued behind you can see what it is for",
            item.id
        ),
        Response::Finished { item, .. } => {
            format!("#{} is done and `{}` is free", item.id, item.tag)
        }
        Response::Abandoned { item, .. } => format!(
            "#{id} went back to `{tag}` with {n} claimant(s) lost — {state}",
            id = item.id,
            tag = item.tag,
            n = item.abandonments(),
            state = describe(item)
        ),
        Response::Queue { tag, rows } => {
            if rows.is_empty() {
                format!("`{tag}` is empty")
            } else {
                let mut s = format!("`{tag}`, first come first served:");
                for row in rows {
                    s.push_str(&format!(
                        "\n  #{id} {body} — {state} (posted by `{by}`)",
                        id = row.id,
                        body = row.body,
                        state = describe(row),
                        by = row.posted_by
                    ));
                }
                s
            }
        }
        Response::Holding { session, rows } => {
            if rows.is_empty() {
                format!("`{session}` holds nothing in the room")
            } else {
                let mut s = format!("`{session}` holds:");
                for row in rows {
                    s.push_str(&format!(
                        "\n  #{id} under `{tag}` — {body} — held {held}, last beat {beat}{doing}{waiting}",
                        id = row.item,
                        tag = row.tag,
                        body = row.body,
                        held = letibot_room::duration(row.held_ms),
                        beat = row.beat_ms,
                        waiting = match &row.waits_on {
                            Some(w) => format!(", waiting on `{}`", w.text),
                            None => String::new(),
                        },
                        doing = match &row.said {
                            Some(w) => format!(", doing `{}`", w.text),
                            None => String::new(),
                        }
                    ));
                }
                s
            }
        }
        Response::Reminders { rows } => {
            if rows.is_empty() {
                "nobody holds anything in the room".to_string()
            } else {
                rows.iter()
                    .map(|r| format!("`{}`: {}", r.session, r.sentence()))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        Response::Questions { rows } => {
            if rows.is_empty() {
                "the room is not asking anything".to_string()
            } else {
                rows.iter()
                    .map(|q| {
                        format!(
                            "question #{id}: {text}\n  facts: {facts}",
                            id = q.id,
                            text = q.text,
                            facts = serde_json::to_string(&q.facts).unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n")
            }
        }
        Response::Question { question, .. } => format!(
            "question #{id} is open: {text}",
            id = question.id,
            text = question.text
        ),
        Response::Ruled { question, .. } => match &question.answer {
            Some(a) => format!("question #{} is answered: {}", question.id, a.decision),
            None => format!("question #{} is still open", question.id),
        },
        Response::Refused { why, .. } => return Err(why.to_string()),
        // The tree, acknowledged - a membership is a set, so the sentence says how big the one
        // the room now holds is.
        Response::Members { count } => {
            format!("the room now holds {count} session(s) of this project")
        }
        Response::Bye { why } => return Err(format!("the room closed the connection: {why}")),
        Response::Welcome { .. } => format!(
            "the room is open to `{me}` (protocol {})",
            letibot_room::ROOM_PROTOCOL_VERSION
        ),
        // The pane's read, in a sentence - a model never asks it, so this is an honest summary
        // rather than a drawing: what the room is, how much is in it, how many questions are open.
        Response::Room {
            identity,
            items,
            questions,
            ..
        } => {
            let mut tags: Vec<&str> = items.iter().map(|i| i.tag.as_str()).collect();
            tags.sort_unstable();
            tags.dedup();
            format!(
                "{identity}\n{} item(s) under {} tag(s), {} question(s) open",
                items.len(),
                tags.len(),
                questions.len()
            )
        }
        Response::Event { event } => format!("the room said: {event:?}"),
        // A `to` is a private message, and the answer has to say so: a model that believed it had
        // spoken in the room would wait for an audience that never read a word of it.
        Response::Said { message, .. } if message.room == letibot_room::chat::DM_ROOM => format!(
            "said in dm: {} — a private message to `{}`: only the two of you read it, and nothing \
             was said in a room. To ask the same thing where everyone can see it, `say` it to the \
             room and `@name` them in the text.",
            message.line(),
            message.to.as_deref().unwrap_or("its addressee"),
        ),
        // Every member is woken, but only an addressed message asks for a reply, so a model that
        // wrote "arango session: ..." is told it asked nobody.
        Response::Said { message, .. }
            if message.to.is_none() && letibot_room::chat::mentions(&message.text).is_empty() =>
        {
            format!(
                "said: {}\nit names no session: everyone in the room reads it and nobody is \
                 asked to reply. To ask one session, `say` again with an `@<session id>` in the \
                 text for a public ask, or its id in `to` for a private one.",
                message.line()
            )
        }
        Response::Said { message, .. } => format!("said: {}", message.line()),
        Response::History(h) => {
            if h.rows.is_empty() {
                format!(
                    "nothing in `{}` after that point (newest is #{})",
                    h.room, h.last_seq
                )
            } else {
                let mut s = h
                    .rows
                    .iter()
                    .map(|m| m.line())
                    .collect::<Vec<_>>()
                    .join("\n");
                if h.trimmed {
                    s.push_str("\n(older messages were dropped from the room's history)");
                }
                s
            }
        }
        Response::Chat { message, .. } => message.line(),
        Response::Rooms { rows } => rows
            .iter()
            .map(|r| {
                format!(
                    "{} - {} here, newest #{}, {} unread",
                    r.id, r.members, r.last_seq, r.unread
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Response::Roster { room, rows } => format!(
            "in `{room}`: {}",
            rows.iter()
                .map(|p| if p.label == "guest" {
                    format!("{} (guest)", p.name)
                } else {
                    p.name.clone()
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Response::Inbox { rows, skipped, .. } => {
            format!("{} message(s), {skipped} left out", rows.len())
        }
        Response::Acked { cursor } => format!("read up to #{cursor}"),
        // The blob frames exist for a HEAD to draw a picture. A model's verbs do not upload or
        // fetch one yet, so this says what the room answered rather than inventing bytes for it.
        Response::Blob { attachment } => match attachment {
            Some(a) => format!("blob `{}` stored: {} of {} byte(s)", a.id, a.mime, a.bytes),
            None => "blob part stored, and the upload is not finished".to_string(),
        },
        Response::BlobPart {
            attachment,
            offset,
            data,
            last,
        } => format!(
            "blob `{}` from byte {offset}: {} byte(s) of base64{}",
            attachment.id,
            data.len(),
            if *last { ", and that is all of it" } else { "" }
        ),
    })
}

fn describe(item: &letibot_room::Item) -> String {
    match &item.state {
        letibot_room::State::Open => "open".to_string(),
        letibot_room::State::Claimed { claim } => {
            format!("held by `{}` for {}", claim.by, claim.beat_ms)
        }
        letibot_room::State::Done { by, .. } => format!("done by `{by}`"),
        letibot_room::State::Abandoned { last } => format!("abandoned — {}", last.why),
        letibot_room::State::Dropped { by, why, .. } => format!("dropped by `{by}`: {why}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A scratch directory that removes itself however the test ends. The lease and the restart
    /// note in it are read as history, so a copy left by a panic is a room this run already had.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let p = std::env::temp_dir().join(format!(
                "letibot-room-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).expect("a scratch directory");
            Scratch(p)
        }

        fn join(&self, name: &str) -> std::path::PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn every_verb_becomes_a_frame_with_a_request_id() {
        let cases = [
            json!({"verb": "post", "tag": "ram", "body": "8 GB"}),
            json!({"verb": "claim", "tag": "ram"}),
            json!({"verb": "claim_item", "item": 1}),
            json!({"verb": "beat", "item": 1}),
            json!({"verb": "waiting", "item": 1, "text": "the suite"}),
            json!({"verb": "done", "item": 1}),
            json!({"verb": "abandon", "item": 1, "why": "no"}),
            json!({"verb": "dispute", "item": 1, "says": "still on it"}),
            json!({"verb": "rule", "question": 1, "decision": "retry"}),
            json!({"verb": "queue", "tag": "ram"}),
            json!({"verb": "holding"}),
            json!({"verb": "reminders"}),
            json!({"verb": "questions"}),
        ];
        for args in cases {
            let verb = args["verb"].as_str().unwrap();
            let frame = request_of(verb, &args).unwrap_or_else(|e| panic!("{verb}: {e}"));
            // Every MUTATING frame carries an id - that is what makes a reconnect's repeat
            // answerable. A read has none by design (`Request::request_id` says so): there is
            // nothing to act on twice, and a re-read is cheap and cannot be wrong.
            let mutates = !matches!(
                frame,
                Request::Queue { .. }
                    | Request::Holding { .. }
                    | Request::Reminders
                    | Request::Questions
            );
            assert!(
                frame.request_id().is_some() == mutates,
                "{verb}: a mutating frame needs a request id and a read must not have one: {frame:?}"
            );
        }
    }

    #[test]
    fn a_hand_without_a_target_is_refused_here_and_never_sent() {
        let e = request_of("rule", &json!({"question": 1, "decision": "hand"})).unwrap_err();
        assert!(e.contains("needs `to`"), "{e}");
        let e = request_of("rule", &json!({"question": 1, "decision": "maybe"})).unwrap_err();
        assert!(e.contains("not a decision"), "{e}");
        let e = request_of("nonsense", &json!({})).unwrap_err();
        assert!(e.contains("not a room verb"), "{e}");
    }

    /// A session wrote "arango session: ..." with no `to`, which asks nobody for a reply. The
    /// reply says so, and an addressed message does not.
    #[test]
    fn a_say_that_names_nobody_says_it_asked_nobody() {
        let said = |to: Option<&str>, text: &str| Response::Said {
            id: "c1".into(),
            message: letibot_room::Message {
                seq: 79,
                ts: 0,
                room: "host".into(),
                author: "s-a".into(),
                kind: letibot_room::Seat::Session,
                to: to.map(str::to_string),
                thread: None,
                refs: Vec::new(),
                attachments: Vec::new(),
                text: text.into(),
            },
        };
        let plain = answer_sentence(&said(None, "arango session: two questions"), "s-a").unwrap();
        assert!(plain.contains("nobody is asked to reply"), "{plain}");
        for (to, text) in [(Some("s-b"), "two questions"), (None, "@s-b two questions")] {
            let named = answer_sentence(&said(to, text), "s-a").unwrap();
            assert!(!named.contains("nobody is asked"), "{named}");
        }
    }

    #[test]
    fn a_refusal_from_the_room_is_a_sentence_and_not_a_panic() {
        let refused = Response::Refused {
            id: Some("c1".into()),
            why: letibot_room::Refused::Busy {
                tag: "ram".into(),
                item: 1,
                by: "s-b".into(),
                held_for: "40s".into(),
            },
        };
        let e = answer_sentence(&refused, "s-a").unwrap_err();
        assert!(e.contains("held by `s-b`"), "{e}");
        assert!(e.contains("a tag is a lock"), "{e}");
    }

    /// Another session's unaddressed chat wakes this one, told that no reply is asked. A session
    /// wrote "arango session: ..." with no `to`, and arango never heard it.
    #[test]
    fn a_sessions_unaddressed_chat_wakes_every_other_session() {
        let me_id = "s-a-chatwake";
        let other_id = "s-b-chatwake";
        let dir =
            std::env::temp_dir().join(format!("letibot-room-chatwake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");
        let rung: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let bell: Arc<dyn Fn(&str) + Send + Sync> = {
            let rung = Arc::clone(&rung);
            Arc::new(move |session: &str| rung.lock().unwrap().push(session.to_string()))
        };
        let _me = SessionRoom::connect(
            me_id,
            &path,
            letibot_room::DEFAULT_PROJECT,
            None,
            bell.clone(),
            None,
        )
        .expect("s-a connects");
        let other = SessionRoom::connect(
            other_id,
            &path,
            letibot_room::DEFAULT_PROJECT,
            None,
            bell,
            None,
        )
        .expect("s-b connects");
        other
            .call(
                "say",
                &serde_json::json!({"room": "host", "text": "arango session: two questions"}),
            )
            .expect("a say");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut said: Vec<String> = Vec::new();
        while std::time::Instant::now() < deadline {
            said.extend(wake::take(me_id));
            if said.iter().any(|s| s.contains("two questions")) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // The room's own fact about another session's row wakes nobody it does not name.
        other.notify_in(
            "host",
            "todo",
            format!("todo:{other_id}:done:a row"),
            "a row is done".to_string(),
            Vec::new(),
        );
        std::thread::sleep(std::time::Duration::from_millis(500));
        said.extend(wake::take(me_id));
        assert!(
            !said.iter().any(|s| s.contains("a row is done")),
            "a todo fact woke `{me_id}`: {said:?}"
        );
        let line = said
            .iter()
            .find(|s| s.contains("two questions"))
            .unwrap_or_else(|| panic!("the chat never reached `{me_id}`: {said:?}"));
        assert!(line.contains("to everyone - no reply asked"), "{line}");
        assert!(
            rung.lock().unwrap().iter().any(|s| s == me_id),
            "the bell did not ring for `{me_id}`: {:?}",
            rung.lock().unwrap()
        );
        // Taking the note is what marks it seen: a push alone left a restarted daemon's model
        // without it, and the join digest skipped it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while server.seen(me_id).unwrap_or(0) == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            server.seen(me_id).unwrap_or(0) > 0,
            "the taken note never reached the room as a seen mark"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Rule 5, end to end: a session is woken by its own room, by a dm to it and by the operator's
    /// own words - and not by the host room's talk between two others.
    #[test]
    fn a_host_line_between_others_does_not_wake_a_project_session() {
        let me_id = "s-a-routing";
        let same_id = "s-a2-routing";
        let other_id = "s-b-routing";
        let (key_a, key_b) = ("aaaaaaaaaaaa", "bbbbbbbbbbbb");
        let dir = std::env::temp_dir().join(format!("letibot-room-routing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");
        let bell: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_: &str| {});
        let me = SessionRoom::connect(me_id, &path, key_a, None, bell.clone(), None)
            .expect("my own room's session");
        let same = SessionRoom::connect(same_id, &path, key_a, None, bell.clone(), None)
            .expect("the other session of my project");
        let other = SessionRoom::connect(other_id, &path, key_b, None, bell, None)
            .expect("a session of another project");
        let operator = letibot_room::client::Client::connect_as(
            &path,
            Speaker::top_level(letibot_room::OPERATOR),
            server.name(),
            letibot_room::Seat::Operator,
            Arc::new(|_: &letibot_room::Event| {}),
        )
        .expect("the operator's seat");

        // My own project's room, said to the room: it wakes me.
        same.call(
            "say",
            &serde_json::json!({"room": "project", "text": "the build is red"}),
        )
        .expect("a say");
        let line = until_note(me_id, "the build is red");
        assert!(line.contains("to everyone - no reply asked"), "{line}");

        // The host room's chatter between two others: a broadcast, and a mention of the third
        // session. Neither is mine, and neither is a room I sit in.
        other
            .call(
                "say",
                &serde_json::json!({"room": "host", "text": "anyone seen the fleet key?"}),
            )
            .expect("a say");
        other
            .call(
                "say",
                &serde_json::json!({"room": "host", "text": format!("@{same_id} did you see that?")}),
            )
            .expect("a say");
        // The one it names is woken where it was named; the bystander of another project is not.
        let named = until_note(same_id, "did you see that?");
        assert!(named.contains("for you"), "{named}");
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert_eq!(
            wake::waiting(me_id),
            0,
            "host chatter between others woke `{me_id}`: {:?}",
            wake::take(me_id)
        );

        // A `to` is a private message: the tool answer says where it went, and it wakes the one.
        let answer = other
            .call(
                "say",
                &serde_json::json!({"to": me_id, "text": "just between us"}),
            )
            .expect("a say");
        assert!(answer.contains("said in dm"), "{answer}");
        assert!(answer.contains("only the two of you read it"), "{answer}");
        let line = until_note(me_id, "just between us");
        assert!(
            line.contains("a private message for you"),
            "a dm does not read as private: {line}"
        );

        // And the operator's own words reach a session wherever they are said.
        let said = operator
            .request(&letibot_room::Request::Say {
                id: "op1".into(),
                room: letibot_room::chat::HOST_ROOM.into(),
                text: "everyone: the box restarts at six".into(),
                to: None,
                thread: None,
                refs: Vec::new(),
                attachments: Vec::new(),
            })
            .expect("the operator says");
        assert!(
            matches!(said, letibot_room::Response::Said { .. }),
            "{said:?}"
        );
        let line = until_note(me_id, "restarts at six");
        assert!(line.contains("to everyone"), "{line}");

        drop((me, same, other));
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The sentence `session` is woken by that carries `needle`, or a panic naming what it got.
    fn until_note(session: &str, needle: &str) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut said: Vec<String> = Vec::new();
        while std::time::Instant::now() < deadline {
            said.extend(wake::take(session));
            if let Some(line) = said.iter().find(|s| s.contains(needle)) {
                return line.clone();
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("`{session}` was never woken by `{needle}`: {said:?}");
    }

    /// The wake, end to end minus the harness: a real server, two real connections, and the
    /// bell the daemon rings - an event about `s-a` becomes a sentence and rings, while a session
    /// with nothing under the tag is not rung. `Harness::wake` composing it into a turn is `wake`'s.
    #[test]
    fn an_event_about_a_session_rings_its_bell_and_a_reader_is_not_rung() {
        // Ids of this test's own, because `wake` is process-wide and keyed by session id. Four
        // other tests in this module connect as `s-a` and post or claim through it, and their
        // sentences landed in THIS test's queue.
        let me_id = "s-a-wake";
        let other_id = "s-b-wake";
        let dir = std::env::temp_dir().join(format!("letibot-room-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");

        let rung: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let bell: Arc<dyn Fn(&str) + Send + Sync> = {
            let rung = Arc::clone(&rung);
            Arc::new(move |session: &str| {
                rung.lock().unwrap().push(session.to_string());
            })
        };
        let me = SessionRoom::connect(
            me_id,
            &path,
            letibot_room::DEFAULT_PROJECT,
            None,
            bell.clone(),
            None,
        )
        .expect("s-a connects");
        let other = Client::connect(
            &path,
            Speaker::top_level(other_id),
            Arc::new(|_: &letibot_room::Event| {}),
        )
        .expect("s-b connects");

        // `s-a-wake` posts, so it is a MEMBER of `ram` - a reader is not.
        me.call("post", &serde_json::json!({"tag": "ram", "body": "8 GB"}))
            .expect("a post");
        let _ = other.request(&Request::Post {
            id: "p2".into(),
            tag: "ram".into(),
            body: "2 GB".into(),
        });
        // And somebody else's claim under that tag is news this session must be woken by.
        let claimed = other
            .request(&Request::Claim {
                id: "c1".into(),
                tag: "ram".into(),
            })
            .expect("a claim");
        assert!(matches!(claimed, Response::Claimed { .. }), "{claimed:?}");

        // Drained until the sentence this test is about arrives, not until the queue is not
        // empty: the first sentence `me` is woken by is its OWN post, and the claim lands a
        // moment later.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut said: Vec<String> = Vec::new();
        while std::time::Instant::now() < deadline {
            said.extend(wake::take(me_id));
            if said
                .iter()
                .any(|s| s.contains("[room]") && s.contains(other_id))
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            rung.lock().unwrap().iter().any(|s| s == me_id),
            "the bell must ring for the session the event is about: {:?}",
            rung.lock().unwrap()
        );
        assert!(
            !said.is_empty(),
            "a ring with nothing behind it is not a wake"
        );
        assert!(
            said.iter()
                .any(|s| s.contains("[room]") && s.contains(other_id)),
            "the sentence names who did it: {said:?}"
        );

        // The arbiter is not woken by the queue's traffic. It hears every event on its
        // connection - that is how a question reaches it - but a `post` is not a turn.
        let arbiter = super::super::session_id();
        let rung_arbiter: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let bell_arbiter: Arc<dyn Fn(&str) + Send + Sync> = {
            let n = Arc::clone(&rung_arbiter);
            Arc::new(move |_: &str| *n.lock().unwrap() += 1)
        };
        let host = SessionRoom::connect(
            &arbiter,
            &path,
            letibot_room::DEFAULT_PROJECT,
            None,
            bell_arbiter,
            None,
        )
        .expect("the arbiter");
        let _ = other.request(&Request::Post {
            id: "p3".into(),
            tag: "ram".into(),
            body: "1 GB".into(),
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(
            *rung_arbiter.lock().unwrap(),
            0,
            "a post is not a question: the arbiter is woken by a raise, not by the queue moving"
        );
        assert_eq!(wake::waiting(&arbiter), 0);
        drop(host);
        drop(me);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A tag's agent hears its own tag, and only a dispute rings it. The operator's cost answer
    /// made structural: a claim, a healthy beat and a finished item are comparisons and reach it as
    /// NOTES; only the question rings. And another tag's traffic does not reach it at all.
    #[test]
    fn a_tags_agent_is_note_woken_by_its_queue_and_rung_only_by_a_dispute() {
        let dir = std::env::temp_dir().join(format!("letibot-room-tag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");

        let rung: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let bell: Arc<dyn Fn(&str) + Send + Sync> = {
            let rung = Arc::clone(&rung);
            Arc::new(move |session: &str| rung.lock().unwrap().push(session.to_string()))
        };
        // The tag's agent, by the derivation - not a hand-written id, because the router
        // addresses it by the same function and a test that spelled its own would prove nothing.
        let key = letibot_room::host_key();
        let agent = letibot_room::tag_session(&key, "ram").expect("an agent for ram");
        let mine = SessionRoom::connect(
            &agent,
            &path,
            letibot_room::DEFAULT_PROJECT,
            None,
            bell,
            None,
        )
        .expect("the tag's agent");
        let other = Client::connect(
            &path,
            Speaker::top_level("s-b"),
            Arc::new(|_: &letibot_room::Event| {}),
        )
        .expect("s-b connects");

        // A post, a claim, and an abandonment - the queue moving, and none of it a judgement.
        let posted = other
            .request(&Request::Post {
                id: "p1".into(),
                tag: "ram".into(),
                body: "8 GB".into(),
            })
            .expect("a post");
        assert!(matches!(posted, Response::Posted { .. }), "{posted:?}");
        let _ = other.request(&Request::Claim {
            id: "c1".into(),
            tag: "ram".into(),
        });
        let _ = other.request(&Request::Abandon {
            id: "a1".into(),
            item: 1,
            why: "the suite blew up".into(),
        });
        // And another tag's post, which must not reach it at all.
        let _ = other.request(&Request::Post {
            id: "p2".into(),
            tag: "disk".into(),
            body: "2 TB".into(),
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && wake::waiting(&agent) < 3 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            rung.lock().unwrap().len(),
            0,
            "a claim and an abandonment are comparisons: no turn is opened for them"
        );
        let notes = wake::take(&agent);
        assert!(
            notes.iter().any(|n| n.contains("claimed #1")),
            "the claim reaches it as a note: {notes:?}"
        );
        assert!(
            notes.iter().any(|n| n.contains("lost its claimant")),
            "and so does the stale beat: {notes:?}"
        );
        assert!(
            !notes.iter().any(|n| n.contains("disk")),
            "and nothing about another tag's item: {notes:?}"
        );

        // A dispute rings it. That is the one thing a tag's agent exists to rule on - and the
        // disputing session must NOT be the holder, which is the case here: `s-b` lost the item to a
        // stale beat and says *I am still on it* after `s-c` has taken it.
        let c = Client::connect(
            &path,
            Speaker::top_level("s-c"),
            Arc::new(|_: &letibot_room::Event| {}),
        )
        .expect("s-c connects");
        let _ = c.request(&Request::Claim {
            id: "c2".into(),
            tag: "ram".into(),
        });
        let asked = other
            .request(&Request::Dispute {
                id: "d1".into(),
                item: 1,
                says: "still on it".into(),
            })
            .expect("a dispute");
        assert!(matches!(asked, Response::Question { .. }), "{asked:?}");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && rung.lock().unwrap().is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            rung.lock().unwrap().as_slice(),
            std::slice::from_ref(&agent),
            "exactly one ring, and it is this tag's agent"
        );
        let said = wake::take(&agent);
        assert!(
            said.iter().any(|s| s.contains("question")),
            "the ring has the question behind it: {said:?}"
        );
        drop(mine);
        drop(other);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A row's entry and verb become the frame the room acts on, and a word the pane does not
    /// have is refused here rather than sent.
    #[test]
    fn a_panes_entry_and_verb_become_a_frame() {
        let id = |frame: &Request| frame.request_id().map(str::to_string);
        // `claim` on an item is `claim_item` - the operator's own control, and it must still be the
        // head (the room refuses `NotTheHead`, which is the whole ordering rule).
        let claim = pane_frame("claim", "i12").expect("claim");
        assert!(matches!(claim, Request::ClaimItem { item: 12, .. }));
        assert!(id(&claim).is_some(), "a mutating frame carries an id");

        // `release` and `abandon` are the room's one verb and two sentences.
        let release = pane_frame("release", "i12").expect("release");
        let abandon = pane_frame("abandon", "i12").expect("abandon");
        match (&release, &abandon) {
            (
                Request::Abandon {
                    item: 12, why: one, ..
                },
                Request::Abandon {
                    item: 12, why: two, ..
                },
            ) => {
                assert!(one.contains("released"), "{one}");
                assert!(two.contains("abandoned"), "{two}");
                assert_ne!(one, two, "the difference the person gets is the sentence");
            }
            other => panic!("both are abandonments of the same item: {other:?}"),
        }

        // `rule` names the decision in the entry, because the decision is what was picked.
        assert!(matches!(
            pane_frame("rule", "q3:retry"),
            Ok(Request::Rule {
                question: 3,
                decision: Decision::Retry,
                ..
            })
        ));
        assert!(matches!(
            pane_frame("rule", "q3:drop"),
            Ok(Request::Rule {
                question: 3,
                decision: Decision::Drop { .. },
                ..
            })
        ));
        assert!(matches!(
            pane_frame("rule", "q3:hand:s-a"),
            Ok(Request::Rule {
                question: 3,
                decision: Decision::Hand { to },
                ..
            }) if to == "s-a"
        ));

        // And the refusals are this door's own, in words a person can act on.
        let hand = pane_frame("rule", "q3:hand").unwrap_err();
        assert!(hand.contains("names no session"), "{hand}");
        let decision = pane_frame("rule", "q3:maybe").unwrap_err();
        assert!(decision.contains("not a decision"), "{decision}");
        let no_question = pane_frame("rule", "q3").unwrap_err();
        assert!(no_question.contains("names no decision"), "{no_question}");
        let no_item = pane_frame("claim", "q3:retry").unwrap_err();
        assert!(no_item.contains("names no item"), "{no_item}");
        let other = pane_frame("post", "i1").unwrap_err();
        assert!(other.contains("four controls"), "{other}");
    }

    /// The four controls, round trip: a row's key, the room's state machine, the state it made.
    /// The layer names a verb and an entry, the HOST performs it over the session's own connection
    /// and the room is what decides.
    #[test]
    fn the_four_controls_reach_the_room_and_change_it() {
        let dir = std::env::temp_dir().join(format!("letibot-room-acts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");
        let me = SessionRoom::connect(
            "s-a",
            &path,
            letibot_room::DEFAULT_PROJECT,
            None,
            Arc::new(|_: &str| {}),
            None,
        )
        .expect("s-a connects");

        // Two items under one tag, posted by another session so `s-a` can claim the head.
        let other = Client::connect(&path, Speaker::top_level("s-b"), Arc::new(|_| {}))
            .expect("s-b connects");
        for (id, body) in [("p1", "8 GB"), ("p2", "2 GB")] {
            other
                .request(&Request::Post {
                    id: id.into(),
                    tag: "ram".into(),
                    body: body.into(),
                })
                .expect("a post");
        }

        // `claim` - on this session's behalf, and the room says so.
        let said = me.act("i1", "claim").expect("a claim");
        assert!(said.contains("you hold #1"), "{said}");
        assert_eq!(
            server.items()[0].holder(),
            Some("s-a"),
            "the item is held by the session whose pane it is"
        );

        // `release` - the same session gives it up, and it goes back to the head of `ram`.
        let said = me.act("i1", "release").expect("a release");
        assert!(said.contains("went back to `ram`"), "{said}");
        let item = &server.items()[0];
        assert_eq!(item.holder(), None);
        assert_eq!(item.abandonments(), 1, "a voluntary release is counted");
        assert!(
            matches!(&item.abandonments[0].why, letibot_room::Why::Released { says } if says.contains("released from the room pane")),
            "the sentence the host wrote is on the item: {:?}",
            item.abandonments[0].why
        );

        // `abandon` - the same verb, the harder sentence, and it is still one abandonment.
        me.act("i1", "claim").expect("a claim");
        me.act("i1", "abandon").expect("an abandonment");
        let item = &server.items()[0];
        assert_eq!(item.abandonments(), 2);
        assert!(
            matches!(&item.abandonments[1].why, letibot_room::Why::Released { says } if says.contains("marked abandoned")),
            "{:?}",
            item.abandonments[1].why
        );

        // And the third raises a question, which the last control answers.
        me.act("i1", "claim").expect("a claim");
        me.act("i1", "abandon").expect("the third abandonment");
        let questions = server.open_questions();
        assert_eq!(questions.len(), 1, "three lost claimants freeze the item");
        let entry = format!("q{}:retry", questions[0].id);
        let said = me.act(&entry, "rule").expect("a ruling");
        assert!(said.contains("is answered"), "{said}");
        assert!(
            server.open_questions().is_empty(),
            "and the question is closed"
        );
        assert!(
            server.items()[0].claimable(),
            "`retry` puts it back at the head with its count kept"
        );

        // A key the room will not take is refused in the room's own words - the person is told
        // what happened to it rather than left with a key that did nothing.
        let refused = me
            .act("i2", "release")
            .expect_err("`s-a` holds nothing on #2");
        assert!(refused.contains("does not hold"), "{refused}");

        drop(other);
        drop(me);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The pane's picture of the room is read off the socket, and the identity comes with it.
    #[test]
    fn the_feed_snapshot_carries_the_rooms_identity_and_its_items() {
        let dir = std::env::temp_dir().join(format!("letibot-room-snap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");
        server.with_arbiter("s-arbiter");
        let me = SessionRoom::connect(
            "s-a",
            &path,
            letibot_room::DEFAULT_PROJECT,
            None,
            Arc::new(|_: &str| {}),
            None,
        )
        .expect("connects");
        me.call("post", &json!({"tag": "ram", "body": "8 GB"}))
            .expect("a post");

        // The picture arrives from a thread of its own, so the test waits for it rather than
        // sleeping a fixed second: a pane opened before the first read lands says *not read yet*.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut snap = me.snapshot();
        // The first read is a promise and the second is the answer - the frame it lands may be
        // the one taken BEFORE this test's post, so wait for the item as well as for the read.
        while (snap.unreadable.is_some() || snap.items.is_empty())
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
            snap = me.snapshot();
        }
        assert_eq!(snap.unreadable, None, "{:?}", snap.unreadable);
        assert!(snap.identity.contains("no item store"), "{}", snap.identity);
        assert_eq!(snap.arbiter.as_deref(), Some("s-arbiter"));
        assert!(
            snap.connected.iter().any(|c| c == "s-a"),
            "who is up: {:?}",
            snap.connected
        );
        assert_eq!(snap.items.len(), 1);
        assert_eq!(snap.items[0].tag, "ram");
        assert_eq!(snap.lease_ms, letibot_room::DEFAULT_LEASE_MS);

        drop(me);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The restart is a FRAME to the supervisor, and what it found decides the note's cause.
    /// The first act has no lease on the socket - nothing was running, so the key is a start; the
    /// second has a live lease, which is a restart and names the session whose pane it was.
    #[test]
    fn the_restart_is_a_frame_to_the_supervisor_and_the_note_says_what_it_found() {
        use std::sync::Mutex as StdMutex;

        struct Recorder(StdMutex<Vec<RestartFrame>>);
        impl RoomSupervisor for Recorder {
            fn restart(&self, frame: &RestartFrame) -> Result<String, String> {
                self.0.lock().unwrap().push(frame.clone());
                Ok("the room was stopped and started again by a fixture".to_string())
            }
        }

        let scratch = Scratch::new("restart");
        let path = scratch.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");
        let rec = Arc::new(Recorder(StdMutex::new(Vec::new())));
        let me = SessionRoom::connect(
            "s-a",
            &path,
            letibot_room::DEFAULT_PROJECT,
            None,
            Arc::new(|_: &str| {}),
            Some(rec.clone()),
        )
        .expect("s-a connects");

        // No lease: nothing was running. The act is a start, and the note says so - the key did
        // not empty the queue, the room had already gone.
        let said = me.restart().expect("the supervisor answers");
        assert!(
            said.contains("read back from the store"),
            "the act says where the queues come back from: {said}"
        );
        let frames = rec.0.lock().unwrap().clone();
        assert_eq!(frames.len(), 1, "one act, one frame");
        assert_eq!(frames[0].by, "s-a");
        assert!(!frames[0].was_running, "there was nothing to stop");
        assert!(
            frames[0].why.contains("pressed restart"),
            "{}",
            frames[0].why
        );
        let note = letibot_room::read_marker(&path).expect("the note is written");
        assert_eq!(note.cause, Cause::Daemon);
        assert_eq!(note.by, "s-a");
        assert_eq!(note.room_started_ms, 0, "there is no lease to read");

        // A live lease: this IS a restart, and the note names the session and the room it made.
        // The pane's reader polls once a second and reads a room it has not seen as a crash, so
        // this link is told the instance first - the act must not race a note about itself.
        *me.seen.lock().unwrap() = Some(7000);
        let lease =
            letibot_room::CoordinatorLease::owned_by(path.with_extension("lease"), "box/1/1");
        std::fs::write(
            lease.path(),
            format!("box/1/1\t{}\t5000\t7000\n", std::process::id()),
        )
        .unwrap();
        me.restart().expect("the supervisor answers again");
        let frames = rec.0.lock().unwrap().clone();
        assert_eq!(frames.len(), 2);
        assert!(frames[1].was_running, "there was a room to stop");
        let note = letibot_room::read_marker(&path).expect("the note is rewritten");
        assert_eq!(note.cause, Cause::Asked);
        assert_eq!(note.room_started_ms, 7000, "the room this note is about");
        assert!(
            note.sentence(0).contains("pressed restart"),
            "{}",
            note.sentence(0)
        );

        drop(me);
        server.stop();
    }

    /// A supervisor that refuses leaves the room alone and says why - and writes no note
    /// because a restart that did not happen is not a restart.
    #[test]
    fn a_refused_restart_is_a_sentence_and_leaves_no_note() {
        struct Refuse;
        impl RoomSupervisor for Refuse {
            fn restart(&self, _frame: &RestartFrame) -> Result<String, String> {
                Err("`letibot --room --stop` answered exit status: 1: no room on this box".into())
            }
        }

        let dir =
            std::env::temp_dir().join(format!("letibot-room-norestart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");
        let me = SessionRoom::connect(
            "s-a",
            &path,
            letibot_room::DEFAULT_PROJECT,
            None,
            Arc::new(|_: &str| {}),
            Some(Arc::new(Refuse)),
        )
        .expect("s-a connects");

        let why = me.restart().expect_err("the supervisor refused");
        assert!(why.contains("no room on this box"), "{why}");
        assert_eq!(
            letibot_room::read_marker(&path),
            None,
            "a restart that did not happen is not a restart"
        );

        drop(me);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A host with no supervisor refuses in words, and the sentence says which door is which
    /// the launcher starts coordinators, and the room's own daemon is never given one.
    #[test]
    fn a_host_with_no_supervisor_refuses_the_restart_in_words() {
        let dir = std::env::temp_dir().join(format!("letibot-room-nosup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");
        let me = SessionRoom::connect(
            "s-a",
            &path,
            letibot_room::DEFAULT_PROJECT,
            None,
            Arc::new(|_: &str| {}),
            None,
        )
        .expect("s-a connects");
        let why = me.restart().expect_err("no supervisor, no restart");
        assert!(why.contains("no supervisor"), "{why}");
        assert!(why.contains("letibot --room"), "{why}");
        assert!(why.contains("kill the feed that reports it"), "{why}");
        drop(me);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A room that came back with nobody asking is written as a crash - once, and never over an
    /// act's own note. The instance is the lease's `started_ms`, which is the only trace a room
    /// leaves when it is not answering.
    #[test]
    fn an_instance_that_changes_with_nobody_asking_is_a_crash_and_an_act_is_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("letibot-room-crash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");
        let lease = path.with_extension("lease");
        std::fs::write(
            &lease,
            format!("box/1/1\t{}\t5000\t7000\n", std::process::id()),
        )
        .unwrap();
        let me = SessionRoom::connect(
            "s-a",
            &path,
            letibot_room::DEFAULT_PROJECT,
            None,
            Arc::new(|_: &str| {}),
            None,
        )
        .expect("s-a connects");
        assert_eq!(letibot_room::read_marker(&path), None, "nothing yet");

        // The room is a new one: a different `started_ms`, and no note accounting for it.
        std::fs::write(
            &lease,
            format!("box/1/1\t{}\t9000\t9001\n", std::process::id()),
        )
        .unwrap();
        me.note_instance();
        let note = letibot_room::read_marker(&path).expect("the crash is written");
        assert_eq!(note.cause, Cause::Crashed);
        assert_eq!(note.room_started_ms, 9001);
        assert!(
            note.sentence(9_500).contains("nobody asked"),
            "{}",
            note.sentence(9_500)
        );

        // An act's note is not overwritten by the observation - a marker that already claims
        // the room the reader is looking at is left alone.
        let asked = Marker::new(9_100, 9001, Cause::Asked).by("s-a");
        asked.write(&letibot_room::marker_path(&path)).unwrap();
        *me.seen.lock().unwrap() = Some(7000);
        me.note_instance();
        assert_eq!(
            letibot_room::read_marker(&path),
            Some(asked),
            "the act's note stands"
        );

        drop(me);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
