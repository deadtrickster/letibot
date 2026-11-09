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
    /// The harness's presence facts, sent on every tick. `None` on a fixture's link.
    seat: Arc<Mutex<Option<Arc<super::seat::SeatCell>>>>,
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

/// Tell the room how the wake `seqs` rang ended, for its signal ledger. Off the turn's thread, and
/// dropped when the session has no room: accounting must never delay or fail a turn.
pub(crate) fn wake_ended(session: &str, seqs: Vec<u64>, cheap: bool, silent: bool, tokens: u64) {
    if seqs.is_empty() {
        return;
    }
    let Some(room) = links()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(session)
        .and_then(Weak::upgrade)
    else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("room-wake-ended".into())
        .spawn(move || {
            let _ = room.client.request(&Request::WakeEnded {
                seqs,
                cheap,
                silent,
                tokens,
            });
        });
}

fn links() -> &'static Mutex<std::collections::BTreeMap<String, Weak<SessionRoom>>> {
    static LINKS: std::sync::OnceLock<
        Mutex<std::collections::BTreeMap<String, Weak<SessionRoom>>>,
    > = std::sync::OnceLock::new();
    LINKS.get_or_init(Default::default)
}

/// A session was DELETED, not restarted: tell the room, so every open ask of it ends `DOWN` and
/// every ask it made is withdrawn. Best effort and said aloud - a box with no coordinator holds no
/// ask, and a room that did not hear it leaves the asks open until one side's grace escalates.
pub fn retire(session: &str) -> Result<String, String> {
    let socket = super::socket_path().filter(|p| p.exists()).ok_or_else(|| {
        "there is no coordinator on this box, so it holds no ask of this session".to_string()
    })?;
    let wake: letibot_room::client::Wake = Arc::new(|_: &letibot_room::Event| {});
    let client = Client::connect(socket, Speaker::top_level(session), wake)
        .map_err(|e| format!("the coordinator did not take the connection: {e}"))?;
    let response = client
        .request(&Request::Retire {
            id: request_id("retire"),
            session: session.to_string(),
        })
        .map_err(|e| format!("the coordinator did not answer: {e}"))?;
    answer_sentence(&response, session)
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
        let seat: Arc<Mutex<Option<Arc<super::seat::SeatCell>>>> = Arc::default();
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
            let judge = Arc::clone(&seat);
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
                // A peer's passing line rings only a seat whose work it touches (an owned scope, an
                // edited file, its branch); the rest take it as a note on their next turn. A seat
                // with nothing to judge by is rung as before.
                if plain && wakes && m.wakes(&me, names, &sits_in) {
                    let reach = wake::reach(m, names);
                    let ring = reach != letibot_room::chat::Reach::Passing
                        || m.kind == letibot_room::Seat::System
                        || judge
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .as_ref()
                            .and_then(|s| s.overlaps(&m.text))
                            .unwrap_or(true);
                    wake::push_chat(&me, m.seq, reach, wake::chat_sentence(m, names));
                    if ring {
                        bell(&me);
                    }
                } else if !wake::seen_meanwhile(&me, m.seq) {
                    // Nothing here will take it as a note, so it is seen on arrival.
                    ack_taken(&me, m.seq);
                }
            }));
        }
        let room = Arc::new(SessionRoom {
            recent,
            invited,
            mine,
            todo_seen: Mutex::new(None),
            seat,
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
                    me.say_how_i_am();
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

    /// The cell this link reads the session's presence from.
    pub fn set_seat(&self, seat: Arc<super::seat::SeatCell>) {
        *self.seat.lock().unwrap_or_else(|e| e.into_inner()) = Some(seat);
    }

    pub fn seat(&self) -> Option<Arc<super::seat::SeatCell>> {
        self.seat.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Tell the room this session's facts, on the same tick as the tree: a restarted coordinator
    /// knows them again within a second, and a daemon that stops sending reads as unknown.
    fn say_how_i_am(&self) {
        let Some(seat) = self.seat() else {
            return;
        };
        let facts = seat.facts(letibot_room::server::now_ms());
        if let Ok(Response::Seated { owns }) = self.client.request(&Request::Seat { facts }) {
            seat.set_owns(owns);
        }
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
        if verb == "status" {
            let seat = self
                .seat()
                .ok_or("this session reports no presence, so it has no status to set")?;
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let ttl = args
                .get("ttl")
                .and_then(|v| v.as_u64())
                .map(|m| m.saturating_mul(60_000));
            let said = seat.declare(text, ttl, letibot_room::server::now_ms());
            self.say_how_i_am();
            return Ok(said);
        }
        let files = self.upload(verb, args)?;
        let mut request = request_of(verb, args)?;
        if !files.is_empty() {
            let said = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let text = letibot_tools::builtins::room::referenced(said.trim(), &files);
            if text.len() > letibot_room::MAX_TEXT {
                return Err(format!(
                    "the words and the {} file reference(s) come to {} bytes, and the room takes at \
                     most {} — say less, or send the file in a message of its own",
                    files.len(),
                    text.len(),
                    letibot_room::MAX_TEXT
                ));
            }
            let Request::Say {
                text: words,
                attachments,
                ..
            } = &mut request
            else {
                return Err(format!("`{verb}` carries no files"));
            };
            *attachments = files.iter().map(|(id, _)| id.clone()).collect();
            *words = text;
        }
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

    /// The files a session's `say` carries, uploaded before the message that names them. The tool
    /// resolved these paths against the session's workspace, so an absolute path is all this half
    /// may ask for: the daemon reads no directory of its own choosing.
    fn upload(
        &self,
        verb: &str,
        args: &serde_json::Value,
    ) -> Result<Vec<(String, String)>, String> {
        let Some(list) = args.get("attachments").filter(|v| !v.is_null()) else {
            return Ok(Vec::new());
        };
        let paths = list
            .as_array()
            .ok_or("`attachments` must be an array of file paths")?;
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        if verb != "say" {
            return Err(format!(
                "`{verb}` carries no files: only `say` takes `attachments`, and nothing was sent"
            ));
        }
        let mut out = Vec::new();
        for p in paths {
            let path = p
                .as_str()
                .ok_or("`attachments` must be an array of file paths")?;
            out.push(self.put_blob(path)?);
        }
        Ok(out)
    }

    /// One file, in the room's 512 KiB parts, answered by the id its bytes hash to.
    fn put_blob(&self, path: &str) -> Result<(String, String), String> {
        let file = std::path::Path::new(path);
        if !file.is_absolute() {
            return Err(format!(
                "`{path}` is not an absolute path: an attachment reaches the daemon resolved \
                 against the session's workspace, and this one did not"
            ));
        }
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment")
            .to_string();
        let Some(mime) = letibot_tools::builtins::room::mime_of(&name) else {
            return Err(format!(
                "`{name}` is not a kind the room carries: a picture (`png`, `jpeg`, `gif`, `webp`) \
                 or a text file — nothing was sent"
            ));
        };
        let bytes = std::fs::read(file).map_err(|e| format!("`{path}` could not be read: {e}"))?;
        if bytes.len() > letibot_room::blob::MAX_BLOB {
            return Err(format!(
                "`{name}` is {} byte(s), and the room takes at most {} per file — nothing was sent",
                bytes.len(),
                letibot_room::blob::MAX_BLOB
            ));
        }
        let mut parts: Vec<&[u8]> = bytes.chunks(letibot_room::blob::MAX_PART).collect();
        if parts.is_empty() {
            parts.push(&[]);
        }
        let upload = request_id("blob");
        let last = parts.len() - 1;
        let mut id = None;
        for (i, chunk) in parts.iter().enumerate() {
            let request = Request::PutBlob {
                upload: upload.clone(),
                name: name.clone(),
                mime: mime.to_string(),
                part: i as u32,
                last: i == last,
                data: letibot_room::blob::encode_base64(chunk),
            };
            let answer = self.client.request(&request).map_err(|e| {
                format!("the room at {} did not answer: {e}", self.socket.display())
            })?;
            match answer {
                Response::Blob { attachment } => {
                    if let Some(a) = attachment {
                        id = Some(a.id);
                    }
                }
                Response::Refused { why, .. } => {
                    return Err(format!("the room refused `{name}`: {why}"));
                }
                other => {
                    return Err(format!(
                        "the room answered `{name}` with a frame that is not a blob: {other:?}"
                    ));
                }
            }
        }
        let id = id.ok_or_else(|| {
            format!("the room took `{name}` and named no blob: nothing was sent with it")
        })?;
        Ok((id, name))
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

    /// What this session made and what it was asked, as data: the room pass's one read. The `asks`
    /// VERB is the same read in words, and a daemon that parsed those words back would be reading
    /// its own formatting.
    pub fn asks(&self) -> Result<(Vec<letibot_room::AskView>, Vec<letibot_room::AskView>), String> {
        let response = self
            .client
            .request(&Request::Asks)
            .map_err(|e| format!("the room at {} did not answer: {e}", self.socket.display()))?;
        match response {
            Response::Asks { by_me, of_me } => Ok((by_me, of_me)),
            Response::Bye { why } => Err(format!("the room closed the connection: {why}")),
            other => Err(format!(
                "the room answered a frame that is not an `asks` read ({other:?}) — nothing was \
                 written on the board"
            )),
        }
    }

    /// Take one of this session's asks back: its row was done, cancelled, detached or re-worded, so
    /// the target must stop being nagged by a question nobody waits for. The room pass sends this;
    /// no model verb does.
    pub fn withdraw(&self, ask: u64) -> Result<(), String> {
        let response = self
            .client
            .request(&Request::Withdraw {
                id: request_id("withdraw"),
                ask,
            })
            .map_err(|e| format!("the room at {} did not answer: {e}", self.socket.display()))?;
        match response {
            Response::Asked { .. } => Ok(()),
            other => {
                answer_sentence(&other, &self.session)?;
                Err(format!(
                    "the room answered a frame that is not an ask ({other:?})"
                ))
            }
        }
    }

    /// Ask `of` a question, synchronously, so every refusal the room makes reaches the model in the
    /// call that asked. `AskMade`'s note is the room's own fact about the target - not connected
    /// now, or gone past the grace - said verbatim and never interpreted.
    pub fn ask_of(
        &self,
        of: &str,
        room: Option<&str>,
        question: &str,
    ) -> Result<letibot_tools::builtins::todo::AskMade, String> {
        let request = Request::Ask {
            id: request_id("ask"),
            room: chat_room(room.unwrap_or("")),
            to: of.trim().to_string(),
            question: question.trim().to_string(),
        };
        let response = self
            .client
            .request(&request)
            .map_err(|e| format!("the room at {} did not answer: {e}", self.socket.display()))?;
        let Response::Asked { ask, .. } = &response else {
            // A refusal or a `Bye` is the room's own sentence, and it is what the model must read.
            answer_sentence(&response, &self.session)?;
            return Err(
                "the room answered a frame that is not an ask — nothing was recorded, so no row \
                 waits on it"
                    .to_string(),
            );
        };
        let note = if ask.escalated {
            Some(format!(
                "`{of}` has been gone long enough that the operator is being asked to restart it; \
                 the ask stays open and its answer waits in the room"
            ))
        } else if ask.gone_since_ms.is_some() {
            Some(format!("`{of}` is not connected now"))
        } else {
            None
        };
        Ok(letibot_tools::builtins::todo::AskMade {
            id: ask.seq,
            room: ask.room.clone(),
            note,
        })
    }
}

impl letibot_tools::builtins::room::RoomLink for SessionRoom {
    fn call(&self, verb: &str, args: &serde_json::Value) -> Result<String, String> {
        SessionRoom::call(self, verb, args)
    }
}

impl letibot_tools::builtins::todo::AskLink for LateRoom {
    fn ask(
        &self,
        of: &str,
        room: Option<&str>,
        question: &str,
    ) -> Result<letibot_tools::builtins::todo::AskMade, String> {
        match self.get() {
            Some(link) => link.ask_of(of, room, question),
            // A session opened before the coordinator did has a link that arrives later; until it
            // does, the sentence `NoAsks` writes is the true one.
            None => letibot_tools::builtins::todo::NoAsks.ask(of, room, question),
        }
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
        "own" => Request::Own {
            id,
            scope: s("scope"),
            ttl_ms: n("ttl").map(|m| m.saturating_mul(60_000)),
            thread: n("thread"),
        },
        "hand" => Request::Hand {
            id,
            scope: s("scope"),
            to: s("to"),
            note: s("text"),
        },
        "accept" => Request::Accept {
            id,
            scope: s("scope"),
        },
        "release" => Request::Release {
            id,
            scope: s("scope"),
        },
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
            // What backs a claim about the world. It rides `refs`, so the wire is unchanged and a
            // message from before it reads as one that named nothing.
            let evidence = s("evidence");
            if !evidence.is_empty() {
                refs.push(letibot_room::Ref {
                    kind: letibot_room::EVIDENCE.to_string(),
                    id: evidence,
                });
            }
            Request::Say {
                id,
                room: chat_room(&s("room")),
                text: s("text"),
                to: Some(s("to")).filter(|x| !x.is_empty()),
                thread: n("thread"),
                refs,
                // Filled by `SessionRoom::call` once the files are uploaded: the ids exist only
                // after the room has hashed the bytes.
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
        "answer" => Request::Reply {
            id,
            ask: n("ask").ok_or("`answer` needs `ask`: the number a row or `room asks` gave")?,
            text: s("text"),
        },
        "decline" => Request::Decline {
            id,
            ask: n("ask").ok_or("`decline` needs `ask`: the number a row or `room asks` gave")?,
            text: s("text"),
        },
        "asks" => Request::Asks,
        "signals" => Request::Signals,
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
                 invite, leave, answer, decline, asks and signals"
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
        Response::Seated { .. } => "the room has this session's presence".to_string(),
        Response::Owned { said, .. } => said.clone(),
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
        Response::Said { message, .. } if message.room == letibot_room::chat::DM_ROOM => {
            let mut said = format!(
                "said #{} in dm to `{}`: only the two of you read it.",
                message.seq,
                message.to.as_deref().unwrap_or("its addressee"),
            );
            said.push_str(&over_dm_cap(message.text.len()));
            said
        }
        // A model that wrote "arango session: ..." is told it asked nobody.
        Response::Said { message, .. }
            if message.to.is_none() && letibot_room::chat::mentions(&message.text).is_empty() =>
        {
            format!(
                "said #{} in [{}] to all: nobody is asked to reply. `@name` asks one.",
                message.seq, message.room
            )
        }
        Response::Said { message, .. } => {
            format!("said #{} in [{}]", message.seq, message.room)
        }
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
        Response::Signals { card } => {
            let mut out = format!("signals, {}m window", card.window_ms / 60_000);
            for (title, rows) in [("senders", &card.senders), ("kinds", &card.kinds)] {
                out.push_str(&format!("\n{title}"));
                for r in rows {
                    out.push_str(&format!("\n  {}", r.line()));
                }
            }
            if card.forgotten > 0 {
                out.push_str(&format!("\n{} older messages not counted", card.forgotten));
            }
            out
        }
        Response::Noted => "noted".into(),
        // An ask, as the room holds it: who was asked of what, and whether it is still open - with
        // the two facts a model can act on, a peer that is not connected now and one gone past the
        // grace.
        Response::Asked { ask, .. } => {
            let peer = if ask.asker == me {
                &ask.target
            } else {
                &ask.asker
            };
            let what = format!(
                "ask #{} (`{peer}`: \"{}\") - {}",
                ask.seq,
                ask.question.trim(),
                ask.state.line()
            );
            if !ask.open() {
                what
            } else if ask.escalated {
                format!(
                    "{what}; `{peer}` has been gone past the grace, so the operator is being asked \
                     to restart it - the ask stays open and its answer waits in the room"
                )
            } else if ask.gone_since_ms.is_some() {
                format!(
                    "{what}; `{peer}` is not connected now, so the question waits in the room for \
                     its next pass - the ask is not lost, and its own board nags it when it runs"
                )
            } else {
                what
            }
        }
        Response::Asks { by_me, of_me } => {
            let open = by_me.iter().filter(|a| a.open()).count();
            let owed = of_me.iter().filter(|a| a.open()).count();
            if by_me.is_empty() && of_me.is_empty() {
                "no asks: this session asked nobody and was asked nothing".to_string()
            } else {
                let mut s = format!("{open} ask(s) you made, {owed} owed to you:");
                for ask in by_me {
                    s.push_str(&format!("\n  by you: {}", ask.line()));
                }
                for ask in of_me {
                    s.push_str(&format!("\n  owed:   {}", ask.line()));
                }
                s
            }
        }
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

/// The warning a DM over [`super::wake::DM_TEXT_MAX`] earns its sender: the reader is woken with
/// the head of it, and reasoning that long belongs in a filed record.
fn over_dm_cap(len: usize) -> String {
    let cap = super::wake::DM_TEXT_MAX;
    if len <= cap {
        return String::new();
    }
    format!(
        " WARNING: it is {len} bytes and the reader is woken with the first {cap}; the rest only \
         by `room read`. Say the measurement and the decision; put the reasoning in a filed row."
    )
}

#[cfg(test)]
mod dm_cap_tests {
    /// A DM over the cap tells its sender so, and one under it says nothing more.
    #[test]
    fn a_long_dm_warns_its_sender() {
        assert_eq!(super::over_dm_cap(200), "");
        let w = super::over_dm_cap(2_500);
        assert!(w.contains("WARNING") && w.contains("first 2000"), "{w}");
    }
}

#[cfg(test)]
#[cfg(test)]
mod tests;
