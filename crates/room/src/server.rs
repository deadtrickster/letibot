//! The coordinator's socket: accept loop, a read loop per connection, the [`Room`] behind one lock, bell routing.
//! A close is the daemon's liveness and a beat is the work's; they are kept apart.
//! The room has no durability: [`Server::identity`] tells "restarted clean" from "restarted empty".

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};

use crate::chat::{self, Chat, Seat};
use crate::item::Speaker;
use crate::lease::{CoordinatorLease, LEASE_BEAT_EVERY_MS};
use crate::room::{Event, Room};
use crate::wire::{Answer, ROOM_PROTOCOL_VERSION, Request, Response};

/// How often the clock runs by default: five seconds, eighteen passes per lease, so a claim whose
/// beat stopped is taken back promptly and a beat that is one pass late is not mistaken for death.
pub const DEFAULT_SWEEP_EVERY_MS: u64 = 5_000;

/// How many frames one connection may have waiting for its writer thread. A frame is at most a
/// chat message's 4 000 bytes plus its envelope, so this is roughly a megabyte of memory per
/// connection - the slack a reader is allowed to fall behind by, and the point at which it is
/// not reading any more.
pub const OUTBOX: usize = 256;

/// How long one frame may take to reach a reader before its connection is ended. A local socket
/// that cannot take 4 KiB in this long has a peer that is not reading, and waiting longer means
/// the room's own writer threads pile up behind it.
pub const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long one frame may be, in bytes: one line of JSON. The frames a client sends are a `hello`,
/// a chat message of at most `MAX_TEXT` bytes and the project's tree, which is a few hundred bytes
/// a session - 1 MiB holds a project of several thousand sessions. Past it the line is a peer
/// holding the room's memory, not a request, and the connection is closed.
pub const MAX_FRAME_BYTES: usize = 1 << 20;

/// One connection's identity - what its `Hello` said, the room it is in, and the queue its writer
/// thread drains. Nothing writes to a peer's socket from another thread: a reader that has stopped
/// reading must not be able to stop the room.
struct Conn {
    who: Speaker,
    seat: Seat,
    /// Which project's room this connection is in - resolved once, at the `Hello`, and the key
    /// every frame from this connection is answered under. A connection that moved rooms would be a
    /// session in two projects; there is no verb for that and there does not need to be one.
    project: String,
    /// Frames for this connection, in the order they were made. Bounded: a full queue is a reader
    /// that has stopped, and it is disconnected rather than waited for.
    outbox: SyncSender<Response>,
    /// The socket, kept for the one act another thread may do to this connection - ending it.
    socket: UnixStream,
    /// Set by the first failure, so one broken connection says its reason once.
    gone: AtomicBool,
}

impl Conn {
    /// Hand a frame to this connection's writer thread. Never blocks: the room's clock, its
    /// router and every author go on to the next connection whatever this one is doing.
    fn send(&self, response: Response) {
        if self.gone.load(Ordering::SeqCst) {
            return;
        }
        match self.outbox.try_send(response) {
            Ok(()) => {}
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                self.give_up(&format!("{OUTBOX} frames were waiting and it took none"))
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                self.give_up("its writer thread had already gone")
            }
        }
    }

    /// End this connection, saying why. The shutdown is what ends the read loop too: it is blocked
    /// on a frame that will never come, and a reader nobody closed is a thread nobody can stop.
    fn give_up(&self, why: &str) {
        if self.gone.swap(true, Ordering::SeqCst) {
            return;
        }
        eprintln!(
            "room: disconnecting `{}` from `{}` - {why}; the other connections are unaffected",
            self.who.session, self.project
        );
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
    }
}

/// The server's own state, shared by the accept loop, the read loops and the clock.
struct Inner {
    /// One room per PROJECT, keyed by [`crate::project_key`]'s answer: the box's coordinator holds
    /// one for each project whose sessions have dialled it. [`crate::DEFAULT_PROJECT`] is always
    /// present - the box's own room, where a one-shot, a fixture and the arbiter live.
    rooms: Mutex<BTreeMap<String, Room>>,
    /// The lease every room created later is given, so two projects' claims go stale on one clock.
    lease_ms: u64,
    conns: Mutex<BTreeMap<u64, Arc<Conn>>>,
    next_conn: AtomicU64,
    /// How many frames one connection may have waiting - [`OUTBOX`], or what a test set. Atomic
    /// because it is read at every accept and written by nothing but a test, before it connects.
    outbox: AtomicUsize,
    path: PathBuf,
    started_ms: u64,
    sweep_every_ms: u64,
    /// The box half of every room's name - `lease::host_key()` by default, or whatever
    /// [`Server::with_name`] was given. A `Hello` that names another box's room is refused with both
    /// names, because obeying it would put its post in another machine's queue.
    name: Mutex<String>,
    /// The session that hears everything - the tie-breaker. `None` on a room whose arbiter is a
    /// person at a head, which is a room where every question waits.
    arbiter: Mutex<Option<String>>,
    /// Who owns each queue, and how many times it had to be brought back - `None` on a room whose
    /// daemon seats no tag agents, which answers `agents: []` rather than a column nobody fills.
    agents: Mutex<Option<Arc<crate::agent::AgentLedger>>>,
    lease: Mutex<Option<CoordinatorLease>>,
    stop: AtomicBool,
    /// How many connections have been served since the start - the pane's *is anybody here*.
    served: AtomicU64,
    /// The events this room addressed to a session that had no connection - the count and the first
    /// of them, per session, taken (and cleared) by that session's next `Hello`. The room keeps no
    /// log, so this is how a session learns it was away rather than reading a plain `Welcome`.
    missed: Mutex<BTreeMap<String, Missed>>,
    /// The chat log, its rings and every principal's inbox cursor. Kept in a store when [`Server::with_chat_store`] was called, memory only otherwise.
    chat: Mutex<Chat>,
    /// Names of externals that have connected, so a message to one that is away is not refused.
    externals: Mutex<BTreeSet<String>>,
    /// The token a connection must present to be the operator, minted at [`Server::with_options`]
    /// and written beside the socket in mode 0600. The seat is a file, not a word in a frame.
    operator_token: String,
}

/// How long a session was out of earshot, and how much it missed while it was - see
/// [`Inner::missed`]. `since_ms` is the FIRST of them, so a client can say how wide the gap was
/// rather than only how deep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Missed {
    count: u64,
    since_ms: u64,
}

/// A handle to a running server - what the coordinator's own threads use: the tie-breaker reads
/// the open questions through it, and the daemon reads the census for its banner.
#[derive(Clone)]
pub struct Server {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("path", &self.inner.path)
            .field("started_ms", &self.inner.started_ms)
            .finish()
    }
}

impl Server {
    /// Bind `path` and start serving `room`, which is the box's own room ([`crate::DEFAULT_PROJECT`]).
    /// The listener is bound before anything is spawned, so a socket another coordinator already
    /// serves is the caller's error and not a thread's: the caller is the one that can say it.
    pub fn serve(path: &Path, room: Room) -> std::io::Result<Server> {
        Self::with_options(path, room, DEFAULT_SWEEP_EVERY_MS)
    }

    /// [`Server::serve`], with the clock's own period.
    pub fn with_options(path: &Path, room: Room, sweep_every_ms: u64) -> std::io::Result<Server> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // A socket file left by a coordinator that was killed is not a coordinator: binding fails
        // on it, and the honest reading is *nobody is listening* - which is what `UnixListener`
        // tells us by refusing. So it is removed first, and the LEASE is what stops two live ones.
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        // The mode after the bind, not before: `bind` creates the file, and a socket created 0755
        // and tightened a moment later was openable for that moment. Mode and peer uid are two
        // gates on one door - the file says who may open it, the kernel says who did.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        let operator_token = mint_token()?;
        write_private(&crate::operator_token_path(path), operator_token.as_bytes())?;
        let lease_ms = room.lease_ms();
        let server = Server {
            inner: Arc::new(Inner {
                rooms: Mutex::new(BTreeMap::from([(crate::DEFAULT_PROJECT.to_string(), room)])),
                lease_ms,
                conns: Mutex::new(BTreeMap::new()),
                next_conn: AtomicU64::new(0),
                outbox: AtomicUsize::new(OUTBOX),
                path: path.to_path_buf(),
                started_ms: now_ms(),
                sweep_every_ms: sweep_every_ms.max(100),
                name: Mutex::new(crate::lease::host_key()),
                arbiter: Mutex::new(None),
                agents: Mutex::new(None),
                lease: Mutex::new(None),
                missed: Mutex::new(BTreeMap::new()),
                chat: Mutex::new(Chat::new()),
                externals: Mutex::new(BTreeSet::new()),
                operator_token,
                stop: AtomicBool::new(false),
                served: AtomicU64::new(0),
            }),
        };
        server.spawn_accept(listener);
        server.spawn_clock();
        Ok(server)
    }

    /// The room of `project`, made if this is the first time anybody asked. A project's room is
    /// empty at its first sight, which is the same reading as a restarted one - and the identity
    /// sentence says so.
    fn with_room<R>(&self, project: &str, f: impl FnOnce(&mut Room) -> R) -> R {
        let lease_ms = self.inner.lease_ms;
        let mut rooms = self.inner.rooms.lock().expect("the rooms lock");
        f(rooms
            .entry(project.to_string())
            .or_insert_with(|| Room::with_lease_ms(lease_ms)))
    }

    /// Which projects have a room here, sorted - the default project included, because it is a
    /// project and not an absence.
    pub fn projects(&self) -> Vec<String> {
        self.inner
            .rooms
            .lock()
            .expect("the rooms lock")
            .keys()
            .cloned()
            .collect()
    }

    /// The project a `Hello` is asking for, or the sentence refusing it. An empty name is *I do not
    /// say* - a protocol-1 client - and reaches the box's own room, where the version check refuses
    /// it. A name that is not this box's is refused here, before any verb exists to refuse.
    fn project_of_hello(&self, asked: &str) -> Result<String, String> {
        let asked = asked.trim();
        if asked.is_empty() {
            return Ok(crate::DEFAULT_PROJECT.to_string());
        }
        let mine = self.name();
        match crate::room_of_name(&mine, asked) {
            Some(project) => Ok(project),
            None => Err(format!(
                "this is the room of `{mine}` and `{asked}` was asked for — a room is one box's \
                 and one project's (`{mine}/<project key>`), so obeying this connection would put \
                 its items in another project's queues. Nothing was done: check the socket you \
                 dialled ({}), or the room name the client sends.",
                self.inner.path.display()
            )),
        }
    }

    /// Keep chat in `path`: load what it holds and append from now on. Call before the first
    /// connection, so no message is made against the empty chat it replaces.
    pub fn with_chat_store(&self, path: &Path) -> &Self {
        *self.inner.chat.lock().expect("the chat lock") = Chat::open(path);
        self
    }

    /// Name the session that hears every event - the tie-breaker.
    pub fn with_arbiter(&self, session: impl Into<String>) -> &Self {
        *self.inner.arbiter.lock().expect("the arbiter lock") = Some(session.into());
        self
    }

    /// Hand the server the ledger of tag agents - who owns each queue and how many times it had to
    /// be brought back. A room with no ledger answers `agents: []`, which is what a room whose
    /// daemon does not seat tag agents should say.
    pub fn with_agents(&self, ledger: Arc<crate::agent::AgentLedger>) -> &Self {
        *self.inner.agents.lock().expect("the agents lock") = Some(ledger);
        self
    }

    /// The tags that have a live item, sorted and deduplicated - what a daemon's upkeep asks to
    /// decide which agents it owes. Live is [`crate::Item::live`] (a frozen item still counts), and
    /// only the box's own room answers, because an agent's name carries no project.
    pub fn live_tags(&self) -> Vec<String> {
        let room = self.inner.rooms.lock().expect("the rooms lock");
        let mut out: Vec<String> = room
            .get(crate::DEFAULT_PROJECT)
            .map(|r| {
                r.all()
                    .into_iter()
                    .filter(|i| i.live())
                    .map(|i| i.tag.clone())
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out.dedup();
        out
    }

    /// The box half of every room's name - `lease::host_key()` by default, and something else in
    /// a test that needs two boxes on one socket.
    pub fn with_name(&self, name: impl Into<String>) -> &Self {
        *self.inner.name.lock().expect("the name lock") = name.into();
        self
    }

    /// The box half of a room's name - what a `Hello` is held to. A room's full name is
    /// `<this>/<project key>` ([`crate::room_name`]), and this is the half that is the same for every
    /// project here.
    pub fn name(&self) -> String {
        self.inner.name.lock().expect("the name lock").clone()
    }

    /// The full name of a project's room - `<host key>/<project key>`, and the box's own name
    /// for [`crate::DEFAULT_PROJECT`].
    pub fn room_name_of(&self, project: &str) -> String {
        crate::room_name(&self.name(), project)
    }

    /// Beat a lease from the server's own clock. One process, one clock: the lease's beat and
    /// the room's sweep are the same thread's two duties, so they cannot drift apart.
    pub fn with_lease(&self, lease: CoordinatorLease) -> &Self {
        *self.inner.lease.lock().expect("the lease lock") = Some(lease);
        self
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// The token the operator's seat is granted by, as this server minted it - what a test (and a
    /// daemon that wants its own head to speak as the person) reads instead of the file.
    pub fn operator_token(&self) -> String {
        self.inner.operator_token.clone()
    }

    /// Shrink one connection's queue of frames - a test's door onto [`OUTBOX`], so a reader that
    /// stops can be driven to the bound in a few dozen frames instead of a megabyte.
    pub fn with_outbox(&self, frames: usize) -> &Self {
        // Zero is a rendezvous channel, which would make every connection a race; one frame is the
        // smallest bound that still means *a queue*.
        self.inner.outbox.store(frames.max(1), Ordering::SeqCst);
        self
    }

    pub fn started_ms(&self) -> u64 {
        self.inner.started_ms
    }

    /// The session that hears everything - the tie-breaker, or `None` on a room whose arbiter
    /// is a person at a head.
    pub fn arbiter(&self) -> Option<String> {
        self.inner.arbiter.lock().expect("the arbiter lock").clone()
    }

    /// Every session connected right now, sorted - the pane's *who is up*, and the list a `hand`
    /// names. The socket is the daemon's liveness, so this is not the same fact as a beat: a session
    /// whose turn ended is here and not beating. `None` is every room on this box.
    pub fn connected_in(&self, project: Option<&str>) -> Vec<String> {
        let mut out: Vec<String> = self
            .inner
            .conns
            .lock()
            .expect("the connections lock")
            .values()
            .filter(|c| c.seat == Seat::Session)
            .filter(|c| project.is_none_or(|p| c.project == p))
            .map(|c| c.who.session.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Every session connected to this box, whatever project it is in.
    pub fn connected(&self) -> Vec<String> {
        self.connected_in(None)
    }

    /// What this coordinator is, in the words a pane prints - and the sentence that makes
    /// *restarted clean* and *restarted empty* two different readings.
    pub fn identity(&self) -> String {
        self.identity_at(crate::DEFAULT_PROJECT, now_ms())
    }

    /// [`Server::identity`] for one project's room at a given clock, so a test can pin the age rather
    /// than wait for one. The age and the count are both in the sentence: an in-memory room holds
    /// nothing at every start, so *no items* alone cannot tell a restart from an idle hour.
    pub fn identity_at(&self, project: &str, now: u64) -> String {
        let n = self.inner.served.load(Ordering::SeqCst);
        let items = self.with_room(project, |r| r.all().len());
        let up = now.saturating_sub(self.inner.started_ms);
        format!(
            "the room of `{}` — started {} ago and holding {items} item(s) since then; the room \
             keeps no store, so a restart begins with no items at all ({n} connection(s) served \
             since it started)",
            self.room_name_of(project),
            crate::item::duration(up),
        )
    }

    /// One frame, answered: the room's dispatch with this server's own facts filled in, plus the two
    /// refusals that need a fact the state machine has not got - the version, and which room this is.
    /// `project` is the connection's room, resolved once at the `Hello`.
    fn answer_frame(
        &self,
        project: &str,
        seat: Seat,
        who: &Speaker,
        frame: &Request,
        now: u64,
    ) -> Answer {
        // The door, and it is answered before the room sees the frame. A skew or a wrong room
        // must not be half-obeyed: the room would vouch for a session that is not going to be
        // spoken to again, and a `post` after a `Bye` is a post in a queue nobody reads.
        if let Request::Hello {
            protocol,
            room,
            token,
            ..
        } = frame
        {
            if let Some(why) = crate::wire::protocol_refusal(*protocol) {
                return Answer {
                    response: Response::Bye { why },
                    events: Vec::new(),
                };
            }
            // Before the name and before the room: the operator's seat is granted by a token the
            // coordinator minted, so a connection that cannot read the coordinator's own runtime
            // directory is told so here rather than admitted as the person and argued with later.
            if let Some(why) = self.operator_refusal(seat, token.as_deref()) {
                return Answer {
                    response: Response::Refused { id: None, why },
                    events: Vec::new(),
                };
            }
            if let Some(refused) = self.hello_name_refusal(seat, who) {
                return Answer {
                    response: refused,
                    events: Vec::new(),
                };
            }
            if seat.is_wide() {
                return Answer {
                    response: Response::Welcome {
                        protocol: ROOM_PROTOCOL_VERSION,
                        lease_ms: self.inner.lease_ms,
                        beat_every_ms: crate::room::DEFAULT_BEAT_EVERY_MS,
                        threshold: crate::room::ABANDON_THRESHOLD,
                        identity: self.identity_at(crate::DEFAULT_PROJECT, now),
                        missed: 0,
                        missed_since_ms: 0,
                        instance: self.inner.started_ms,
                    },
                    events: Vec::new(),
                };
            }
            if let Err(why) = self.project_of_hello(room) {
                return Answer {
                    response: Response::Bye { why },
                    events: Vec::new(),
                };
            }
        }
        if is_chat_frame(frame) {
            return Answer {
                response: self.chat_frame(project, seat, who, frame, now),
                events: Vec::new(),
            };
        }
        // A `rule` is the one frame that may name another room: two rooms number their questions
        // from 1, so a ruling that names its project is answered there, and one that does not is
        // answered in the room that holds the question.
        let ruling_in = match frame {
            Request::Rule {
                project: Some(named),
                id,
                ..
            } if !named.trim().is_empty() => {
                let named = named.trim();
                // A room is only created for a key somebody derived. A ruling that names a
                // string this tree would never write would otherwise make a room with no
                // membership, no name and no meaning - a room that can never be reached again.
                if !crate::is_project_key(named) {
                    return Answer {
                        response: Response::Refused {
                            id: Some(id.clone()),
                            why: crate::Refused::UnknownRoom {
                                project: named.to_string(),
                            },
                        },
                        events: Vec::new(),
                    };
                }
                named.to_string()
            }
            Request::Rule { question, .. } => self
                .room_holding(*question)
                .unwrap_or_else(|| project.to_string()),
            _ => project.to_string(),
        };
        // The room lock is released before anything else is read. `identity_at` reads the items
        // and the connections, and the room's mutex is not reentrant: filling the answer inside the
        // guard is a deadlock that reads as a room that stopped answering.
        let mut answer = self.with_room(&ruling_in, |room| room.answer_traced(who, frame, now));
        let room = if ruling_in == project {
            project.to_string()
        } else {
            ruling_in
        };
        match &mut answer.response {
            Response::Welcome {
                identity,
                missed,
                missed_since_ms,
                instance,
                ..
            } => {
                *instance = self.inner.started_ms;
                *identity = self.identity_at(&room, now);
                // The gap this session just came back from, taken as the `Hello` is answered:
                // the frame that says *you are connected* is the only one that can say *and here
                // is what happened while you were not*.
                let was = self.take_missed(&who.session);
                *missed = was.count;
                *missed_since_ms = was.since_ms;
            }
            Response::Room {
                identity,
                arbiter,
                connected,
                agents,
                ..
            } => {
                *identity = self.identity_at(&room, now);
                *arbiter = self.arbiter();
                // This room's connections, not the box's: a pane in one project must not draw
                // another project's sessions as *here*.
                *connected = self.connected_in(Some(&room));
                // The ledger is the SERVER's, like the three fields above. The room cannot say
                // who owns a queue: an agent outlives every item under its tag, and it is the
                // daemon's upkeep that seats one.
                *agents = self
                    .inner
                    .agents
                    .lock()
                    .expect("the agents lock")
                    .as_ref()
                    .map(|l| l.all())
                    .unwrap_or_default();
            }
            // A question is stamped with the room that asked it. Two rooms number from 1, so an
            // arbiter that heard both without this could not say which one it was answering - and
            // `Request::Rule`'s `project` is what it says back.
            Response::Questions { rows } => {
                for q in rows.iter_mut() {
                    q.project = room.clone();
                }
            }
            Response::Question { question, .. } | Response::Ruled { question, .. } => {
                question.project = room.clone();
            }
            _ => {}
        }
        answer
    }

    /// The room whose OPEN question is `question`, when exactly one room has one. This is what lets a
    /// ruling name no project and still land, since the arbiter hears every project's questions. An
    /// ambiguity is answered by `None`, so the ruling is refused by name rather than misapplied.
    fn room_holding(&self, question: u64) -> Option<String> {
        let mut holding = None;
        let rooms = self.inner.rooms.lock().expect("the rooms lock");
        for (project, room) in rooms.iter() {
            if room.question(question).is_some_and(|q| q.answer.is_none()) {
                if holding.is_some() {
                    return None;
                }
                holding = Some(project.clone());
            }
        }
        holding
    }

    /// How many sessions are connected now - the pane's *is anybody here*, and the number a
    /// test reads instead of sleeping.
    pub fn connections(&self) -> usize {
        self.inner.conns.lock().expect("the connections lock").len()
    }

    /// Run the clock once, now - every room swept, each room's events routed to its own. A test
    /// drives this instead of waiting.
    pub fn tick(&self, now: u64) -> Vec<Event> {
        let rooms = self.projects();
        let mut all = Vec::new();
        for project in rooms {
            let events = self.with_room(&project, |r| r.sweep(now));
            self.route(&project, &events);
            all.extend(events);
        }
        all
    }

    /// The open questions, oldest first - what the tie-breaker is prompted with, from EVERY room,
    /// each stamped with the project that asked it (`Question::project`), which is what lets the
    /// arbiter answer the right one.
    pub fn open_questions(&self) -> Vec<crate::room::Question> {
        let mut out = Vec::new();
        for project in self.projects() {
            let mut rows = self.with_room(&project, |r| {
                r.open_questions().into_iter().cloned().collect::<Vec<_>>()
            });
            for q in rows.iter_mut() {
                q.project = project.clone();
            }
            out.extend(rows);
        }
        out
    }

    /// Every item in every room, oldest first - the census a test reads.
    pub fn items(&self) -> Vec<crate::item::Item> {
        let mut out = Vec::new();
        for project in self.projects() {
            out.extend(self.with_room(&project, |r| {
                r.all().into_iter().cloned().collect::<Vec<_>>()
            }));
        }
        out
    }

    /// One project's room's items, oldest first - the pane, and the room a session is in.
    pub fn items_in(&self, project: &str) -> Vec<crate::item::Item> {
        self.with_room(project, |r| r.all().into_iter().cloned().collect())
    }

    /// Who is in a project's room - the tree its daemon last described.
    pub fn members_of(&self, project: &str) -> Vec<crate::tree::Member> {
        self.with_room(project, |r| r.members().to_vec())
    }

    /// Stop serving - the socket is unbound and the lease given back. Called on an orderly
    /// stop; a killed coordinator leaves both behind on purpose, and its lease is what tells the
    /// next one that it was killed rather than that it never existed.
    pub fn stop(&self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_file(&self.inner.path);
        // The token dies with the socket it belongs to. A file left behind would be a word a
        // later process could present to the next coordinator, which never minted it.
        let _ = std::fs::remove_file(crate::operator_token_path(&self.inner.path));
        if let Some(lease) = self.inner.lease.lock().expect("the lease lock").as_ref() {
            let _ = lease.release();
        }
        // A connection whose peer is gone is already closed; this wakes the ones still reading so
        // their threads end rather than waiting for a client that will never write again.
        for conn in self
            .inner
            .conns
            .lock()
            .expect("the connections lock")
            .values()
        {
            let _ = conn.socket.shutdown(std::net::Shutdown::Both);
        }
    }

    // ===== Chat =====

    /// Why this `Hello` may not be the operator's, when it asked to be. The comparison is over the
    /// whole token: one that stopped at the first wrong byte would answer a guesser byte by byte.
    fn operator_refusal(&self, seat: Seat, token: Option<&str>) -> Option<crate::Refused> {
        if seat != Seat::Operator {
            return None;
        }
        let path = crate::operator_token_path(&self.inner.path);
        let Some(given) = token.map(str::trim).filter(|t| !t.is_empty()) else {
            return Some(crate::Refused::OperatorSeat {
                why: format!(
                    "this hello presented no token, and the coordinator writes the one it grants \
                     the seat by in {} (mode 0600) at its start",
                    path.display()
                ),
            });
        };
        if same_secret(given, &self.inner.operator_token) {
            return None;
        }
        Some(crate::Refused::OperatorSeat {
            why: format!(
                "the token presented is not the one this coordinator minted - read {} (mode 0600) \
                 and present what is in it",
                path.display()
            ),
        })
    }

    /// Why this `Hello` may not speak under this name in this seat, as the reply to send.
    fn hello_name_refusal(&self, seat: Seat, who: &Speaker) -> Option<Response> {
        let name = who.session.as_str();
        let ok = match seat {
            Seat::Session => name != chat::OPERATOR && !name.is_empty(),
            Seat::Operator => name == chat::OPERATOR && who.is_top_level(),
            Seat::External => chat::valid_handle(name) && who.is_top_level(),
            Seat::System => false,
        };
        (!ok).then(|| Response::Refused {
            id: None,
            why: crate::Refused::ReservedName {
                name: name.to_string(),
            },
        })
    }

    fn conn_list(&self) -> Vec<Arc<Conn>> {
        self.inner
            .conns
            .lock()
            .expect("the connections lock")
            .values()
            .cloned()
            .collect()
    }

    /// May a principal in `seat` and `project` see chat room `room`?
    fn may_see(&self, seat: Seat, project: &str, room: &str) -> bool {
        if room == chat::HOST_ROOM {
            return true;
        }
        let Some(key) = chat::project_of_room(room) else {
            return false;
        };
        if seat.is_wide() {
            self.inner
                .rooms
                .lock()
                .expect("the rooms lock")
                .contains_key(key)
        } else {
            !project.is_empty() && project == key
        }
    }

    /// Is this message one `name` (in `seat`, `project`) may read?
    fn visible(&self, name: &str, seat: Seat, project: &str, m: &chat::Message) -> bool {
        if m.room == chat::DM_ROOM {
            return m.author == name || m.to.as_deref() == Some(name);
        }
        self.may_see(seat, project, &m.room)
    }

    /// Does anybody on this host answer to `name`?
    fn is_known(&self, name: &str) -> bool {
        if name == chat::OPERATOR
            || self
                .inner
                .externals
                .lock()
                .expect("externals")
                .contains(name)
            || self.conn_list().iter().any(|c| c.who.session == name)
        {
            return true;
        }
        let rooms = self.inner.rooms.lock().expect("the rooms lock");
        rooms.values().any(|r| {
            r.members()
                .iter()
                .any(|m| m.group == crate::Group::Top && m.session == name)
        })
    }

    /// The room a message goes to when the sender names none.
    fn default_chat_room(seat: Seat, project: &str) -> String {
        if seat.is_wide() || project.is_empty() {
            chat::HOST_ROOM.to_string()
        } else {
            chat::project_room(project)
        }
    }

    /// Who is in a chat room: the tree's top-level sessions, whoever is connected, and the wide
    /// seats, which are in every room.
    pub fn roster_of(&self, room: &str) -> Vec<chat::Presence> {
        let key = chat::project_of_room(room);
        let mut rows: BTreeMap<String, chat::Presence> = BTreeMap::new();
        {
            let rooms = self.inner.rooms.lock().expect("the rooms lock");
            for (project, r) in rooms.iter() {
                if project.is_empty() || key.is_some_and(|k| k != project) {
                    continue;
                }
                for m in r.members().iter().filter(|m| m.group == crate::Group::Top) {
                    rows.insert(
                        m.session.clone(),
                        chat::Presence {
                            name: m.session.clone(),
                            kind: Seat::Session,
                            connected: false,
                            project: project.clone(),
                            label: m.label.clone(),
                        },
                    );
                }
            }
        }
        for c in self.conn_list() {
            let name = c.who.session.clone();
            let in_room = c.seat.is_wide() || key.is_none_or(|k| k == c.project);
            if !in_room {
                continue;
            }
            let row = rows.entry(name.clone()).or_insert(chat::Presence {
                name,
                kind: c.seat,
                connected: false,
                project: c.project.clone(),
                label: String::new(),
            });
            row.connected = true;
            row.kind = c.seat;
        }
        rows.into_values().collect()
    }

    /// The chat messages in the log, for a test.
    pub fn chat_head(&self) -> u64 {
        self.inner.chat.lock().expect("the chat lock").head()
    }

    fn chat_frame(
        &self,
        project: &str,
        seat: Seat,
        who: &Speaker,
        frame: &Request,
        now: u64,
    ) -> Response {
        let name = who.session.as_str();
        let refuse = |id: Option<&String>, why: crate::Refused| Response::Refused {
            id: id.cloned(),
            why,
        };
        match frame {
            Request::Rooms => {
                let mut ids = vec![chat::HOST_ROOM.to_string()];
                if seat.is_wide() {
                    ids.extend(
                        self.projects()
                            .into_iter()
                            .filter(|p| !p.is_empty())
                            .map(|p| chat::project_room(&p)),
                    );
                } else if !project.is_empty() {
                    ids.push(chat::project_room(project));
                }
                let chat = self.inner.chat.lock().expect("the chat lock");
                let rows = ids
                    .into_iter()
                    .map(|id| {
                        let key = chat::project_of_room(&id).unwrap_or("").to_string();
                        chat::RoomInfo {
                            kind: if key.is_empty() { "host" } else { "project" }.into(),
                            members: self.roster_of(&id).len() as u64,
                            last_seq: chat.last_in(&id),
                            unread: chat.unread(name, &id, |_| true),
                            project: key,
                            id,
                        }
                    })
                    .collect();
                Response::Rooms { rows }
            }
            Request::Say {
                id,
                room,
                text,
                to,
                thread,
                refs,
            } => {
                let room = match room.trim() {
                    "" => Self::default_chat_room(seat, project),
                    r => r.to_string(),
                };
                if !self.may_see(seat, project, &room) {
                    return refuse(Some(id), crate::Refused::UnknownChatRoom { room });
                }
                let to = to
                    .as_deref()
                    .map(|t| t.trim().trim_start_matches('@').to_string())
                    .filter(|t| !t.is_empty());
                if let Some(t) = &to
                    && !self.is_known(t)
                {
                    return refuse(
                        Some(id),
                        crate::Refused::UnknownAddressee { name: t.clone() },
                    );
                }
                self.append_and_push(id, &room, name, seat, to, *thread, refs.clone(), text, now)
            }
            Request::Dm {
                id,
                to,
                text,
                thread,
            } => {
                let to = to.trim().trim_start_matches('@').to_string();
                if to.is_empty() || to == name || !self.is_known(&to) {
                    return refuse(Some(id), crate::Refused::UnknownAddressee { name: to });
                }
                self.append_and_push(
                    id,
                    chat::DM_ROOM,
                    name,
                    seat,
                    Some(to),
                    *thread,
                    Vec::new(),
                    text,
                    now,
                )
            }
            Request::Notify {
                id,
                room,
                source,
                text,
                refs,
            } => {
                // Only a daemon's session connection reports facts, and into rooms it is in.
                let room = match room.trim() {
                    "" => Self::default_chat_room(seat, project),
                    r => r.to_string(),
                };
                if seat != Seat::Session
                    || !matches!(source.as_str(), "todo" | "queue")
                    || room == chat::DM_ROOM
                    || !self.may_see(seat, project, &room)
                {
                    return refuse(
                        Some(id),
                        crate::Refused::ReservedName {
                            name: source.clone(),
                        },
                    );
                }
                self.append_and_push(
                    id,
                    &room,
                    source,
                    Seat::System,
                    None,
                    None,
                    refs.clone(),
                    text,
                    now,
                )
            }
            Request::Read {
                room,
                since,
                last,
                thread,
            } => {
                let room = match room.trim() {
                    "" => Self::default_chat_room(seat, project),
                    r => r.to_string(),
                };
                if room != chat::DM_ROOM && !self.may_see(seat, project, &room) {
                    return refuse(None, crate::Refused::UnknownChatRoom { room });
                }
                let chat = self.inner.chat.lock().expect("the chat lock");
                Response::History(chat.read(
                    &room,
                    *since,
                    last.map(|n| n as usize),
                    *thread,
                    |m| self.visible(name, seat, project, m),
                ))
            }
            Request::Roster { room } => {
                let room = match room.trim() {
                    "" => Self::default_chat_room(seat, project),
                    r => r.to_string(),
                };
                if !self.may_see(seat, project, &room) {
                    return refuse(None, crate::Refused::UnknownChatRoom { room });
                }
                Response::Roster {
                    rows: self.roster_of(&room),
                    room,
                }
            }
            Request::Inbox { attention, limit } => {
                let chat = self.inner.chat.lock().expect("the chat lock");
                let (rows, skipped) = chat.inbox(
                    name,
                    *attention,
                    limit.map_or(chat::MAX_READ, |n| n as usize),
                    |m| self.visible(name, seat, project, m),
                );
                Response::Inbox {
                    rows,
                    skipped,
                    cursor: chat.cursor(name),
                    head: chat.head(),
                }
            }
            Request::Ack { seq } => Response::Acked {
                cursor: self
                    .inner
                    .chat
                    .lock()
                    .expect("the chat lock")
                    .ack(name, *seq),
            },
            _ => refuse(None, crate::Refused::ChatIsTheServers),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn append_and_push(
        &self,
        id: &str,
        room: &str,
        author: &str,
        kind: Seat,
        to: Option<String>,
        thread: Option<u64>,
        refs: Vec<chat::Ref>,
        text: &str,
        now: u64,
    ) -> Response {
        let appended = self
            .inner
            .chat
            .lock()
            .expect("the chat lock")
            .append(now, id, room, author, kind, to, thread, refs, text);
        let message = match appended {
            Ok(m) => m,
            Err(why) => {
                return Response::Refused {
                    id: Some(id.to_string()),
                    why,
                };
            }
        };
        // Pushed after the log lock is released. A repeat of `id` pushes again: a reader that
        // already has the seq drops it, and one that missed it gets it.
        for c in self.conn_list() {
            let name = c.who.session.as_str();
            if name == message.author || !self.visible(name, c.seat, &c.project, &message) {
                continue;
            }
            c.send(Response::Chat {
                names_you: message.names(name),
                message: message.clone(),
            });
        }
        Response::Said {
            id: id.to_string(),
            message,
        }
    }

    // ===== The threads =====

    fn spawn_accept(&self, listener: UnixListener) {
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("room-accept".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if me.inner.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    match stream {
                        Ok(stream) => Server::clone(&me).serve_one(stream),
                        Err(_) => break,
                    }
                }
            });
    }

    /// One connection, one thread - `Hello` first, then a read loop.
    fn serve_one(self, stream: UnixStream) {
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("room-conn".into())
            .spawn(move || {
                // The door, before a byte is read: the socket is this user's own and a peer is
                // checked anyway. It costs one syscall and it refuses a descriptor inherited from
                // another uid, which a file's mode cannot see.
                let mine = unsafe { libc::geteuid() };
                let theirs = peer_uid(&stream);
                if theirs != Some(mine) {
                    let said = theirs
                        .map(|u| u.to_string())
                        .unwrap_or_else(|| "a user the kernel would not name".to_string());
                    eprintln!(
                        "room: refused a connection from uid {said}; this socket is uid {mine}'s"
                    );
                    let mut w = stream;
                    let _ = write_frame(
                        &mut w,
                        &Response::Bye {
                            why: format!(
                                "this room socket belongs to uid {mine} and the process that \
                                 connected is uid {said} - a room takes the same user's \
                                 connections only, and nothing was done"
                            ),
                        },
                    );
                    let _ = w.shutdown(std::net::Shutdown::Both);
                    return;
                }
                let writer = match stream.try_clone() {
                    Ok(w) => w,
                    Err(_) => return,
                };
                // The handshake is written on this thread, before any writer thread exists, so the
                // bound applies to it too: a peer that connects and reads nothing must not hold a
                // thread for ever. Options belong to the socket, so the clone shares it.
                let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
                let mut reader = BufReader::new(stream);
                // The door. The first frame is the `Hello`, and a connection that does not
                // open with one has nothing to say that the room could act on.
                let first = match read_frame(&mut reader) {
                    ReadFrame::Frame(frame) => frame,
                    ReadFrame::OverLong(got) => {
                        let mut w = writer;
                        let _ = write_frame(
                            &mut w,
                            &Response::Bye {
                                why: over_long_sentence(got),
                            },
                        );
                        let _ = w.shutdown(std::net::Shutdown::Both);
                        return;
                    }
                    ReadFrame::End => return,
                    // Not JSON, so not a frame: there is no name to answer and no request to
                    // refuse, and the line is said on the log so it can be found afterwards.
                    ReadFrame::Bad(why) => {
                        eprintln!(
                            "room: a connection sent a line that is not a frame ({why}); it is \
                             closed"
                        );
                        return;
                    }
                };
                let hello = match &first {
                    Request::Hello { .. } => first.clone(),
                    _ => {
                        let mut w = writer;
                        let _ = write_frame(
                            &mut w,
                            &Response::Bye {
                                why: "the first frame on a room connection is `hello` — it names \
                                      the session, so that a claim can be given back when this \
                                      connection closes"
                                    .to_string(),
                            },
                        );
                        return;
                    }
                };
                let (speaker, seat) = match &hello {
                    Request::Hello { speaker, seat, .. } => (speaker.clone(), *seat),
                    _ => return,
                };
                // Which room this connection is in, resolved ONCE - the `Hello`'s name, and
                // every frame from this connection is answered in it. A connection that could move
                // rooms would be one session in two projects.
                let project = match &hello {
                    // A wide seat is in no project's queue: it sees rooms, it does not hold items.
                    _ if seat.is_wide() => Some(crate::DEFAULT_PROJECT.to_string()),
                    Request::Hello { room, .. } => me.project_of_hello(room).ok(),
                    _ => None,
                };
                let Some(project) = project else {
                    // The `Hello` below answers the refusal, with both names in it.
                    let answer =
                        me.answer_frame(crate::DEFAULT_PROJECT, seat, &speaker, &hello, now_ms());
                    let mut w = writer;
                    let _ = write_frame(&mut w, &answer.response);
                    let _ = w.shutdown(std::net::Shutdown::Both);
                    return;
                };
                let answer = me.answer_frame(&project, seat, &speaker, &hello, now_ms());
                let mut w = writer;
                if write_frame(&mut w, &answer.response).is_err() {
                    return;
                }
                // A `Bye` or a refusal is the end of the connection: there is no session here.
                match &answer.response {
                    Response::Welcome { .. } => {}
                    _ => {
                        let _ = w.shutdown(std::net::Shutdown::Both);
                        return;
                    }
                }
                let id = me.inner.next_conn.fetch_add(1, Ordering::SeqCst) + 1;
                // From here on, this connection's frames go through ONE writer thread, and the
                // room's threads only ever enqueue. The handshake above was written directly
                // because there was no other writer then, and it is written before the thread
                // starts so the two cannot interleave a frame.
                let Ok(their_writer) = w.try_clone() else {
                    // A socket that cannot be cloned has no writer thread, so there is nobody to
                    // send a frame and nothing to keep: the close is the only thing left to say.
                    let _ = w.shutdown(std::net::Shutdown::Both);
                    return;
                };
                let (outbox, frames) =
                    std::sync::mpsc::sync_channel(me.inner.outbox.load(Ordering::SeqCst));
                spawn_writer(speaker.session.clone(), their_writer, frames);
                let conn = Arc::new(Conn {
                    who: speaker.clone(),
                    seat,
                    project: project.clone(),
                    outbox,
                    socket: w,
                    gone: AtomicBool::new(false),
                });
                me.inner
                    .conns
                    .lock()
                    .expect("the connections lock")
                    .insert(id, Arc::clone(&conn));
                me.inner.served.fetch_add(1, Ordering::SeqCst);
                me.inner
                    .chat
                    .lock()
                    .expect("the chat lock")
                    .arm(&speaker.session);
                if seat == Seat::External {
                    me.inner
                        .externals
                        .lock()
                        .expect("the externals lock")
                        .insert(speaker.session.clone());
                }

                // The read loop. Every frame is answered, and every event the answer carries
                // is routed to the connections it is about - including this one, because a claim
                // by `s-b` is news to `s-a`.
                loop {
                    if me.inner.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let frame = match read_frame(&mut reader) {
                        ReadFrame::Frame(f) => f,
                        // A line past the bound is answered before the close: the peer is told
                        // what it sent and what the room takes, rather than reading a bare close.
                        ReadFrame::OverLong(got) => {
                            conn.send(Response::Bye {
                                why: over_long_sentence(got),
                            });
                            break;
                        }
                        ReadFrame::End => break,
                        // Not JSON, so not a frame: the room cannot answer it, and the log is the
                        // only place a person could find out what the peer tried to send.
                        ReadFrame::Bad(why) => {
                            eprintln!(
                                "room: `{}` sent a line that is not a frame ({why}); its \
                                 connection is closed",
                                speaker.session
                            );
                            break;
                        }
                    };
                    let answer: Answer =
                        me.answer_frame(&project, seat, &speaker, &frame, now_ms());
                    let parting = matches!(answer.response, Response::Bye { .. });
                    conn.send(answer.response);
                    // A ruling lands in the room it names, so its events go there too.
                    let landed_in = match &frame {
                        Request::Rule {
                            project: Some(named),
                            ..
                        } if !named.trim().is_empty() => named.trim().to_string(),
                        _ => project.clone(),
                    };
                    me.route(&landed_in, &answer.events);
                    if parting {
                        break;
                    }
                }

                // The close is a fact. This session's daemon is not there any more - it
                // exited, restarted, or was stopped - so every claim it holds goes back to the
                // queue NOW, rather than ninety seconds from now.
                me.inner
                    .conns
                    .lock()
                    .expect("the connections lock")
                    .remove(&id);
                if seat.is_wide() {
                    return;
                }
                let events = me.with_room(&project, |room| {
                    room.note_gone(
                        &speaker.session,
                        &format!("`{}`'s room connection closed", speaker.session),
                        now_ms(),
                    )
                });
                me.route(&project, &events);
            });
    }

    fn spawn_clock(&self) {
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("room-clock".into())
            .spawn(move || {
                let mut last_beat = 0u64;
                while !me.inner.stop.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(me.inner.sweep_every_ms));
                    if me.inner.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let now = now_ms();
                    // The lease's beat, on this thread: one process, one clock.
                    if now.saturating_sub(last_beat) >= LEASE_BEAT_EVERY_MS {
                        last_beat = now;
                        if let Some(lease) = me.inner.lease.lock().expect("the lease lock").as_ref()
                        {
                            let _ = lease.beat(now);
                        }
                    }
                    // Every room, on the one clock - a claim is taken back whether its project
                    // is the one this coordinator was started for or another.
                    me.tick(now);
                }
            });
    }

    // ===== Routing =====

    /// Send each event to the connections it is about, in ONE room: the sessions queued under the
    /// tag, the session it names, and the arbiter. A connection in another project never hears it,
    /// and a member with no connection is counted into [`Inner::missed`] rather than lost.
    fn route(&self, project: &str, events: &[Event]) {
        if events.is_empty() {
            return;
        }
        let arbiter = self.inner.arbiter.lock().expect("the arbiter lock").clone();
        let conns: Vec<Arc<Conn>> = self
            .inner
            .conns
            .lock()
            .expect("the connections lock")
            .values()
            .filter(|c| c.project == project)
            .cloned()
            .collect();
        // The arbiter's own connection, which is in the BOX's room and hears every project's
        // events - a question is addressed to it and it is never a poster, so without this the one
        // session that must rule on a project it is not in would be a room it cannot hear.
        let boxed: Vec<Arc<Conn>> = self
            .inner
            .conns
            .lock()
            .expect("the connections lock")
            .values()
            .filter(|c| arbiter.as_deref() == Some(c.who.session.as_str()) && c.project != project)
            .cloned()
            .collect();
        // Who is listening, by session. A set because the audience below is a set too: a
        // session that is both a poster and the holder is one bell, and one count.
        let here: BTreeSet<&str> = conns
            .iter()
            .chain(boxed.iter())
            .map(|c| c.who.session.as_str())
            .collect();
        let now = now_ms();
        // There is no early return for a room nobody is connected to, and that is the bell's
        // whole rule: an audience with no connection is the case the count exists for, so it is
        // derived and counted whether or not there is anybody to write to.
        for event in events {
            let audience: BTreeSet<String> = self.with_room(project, |room| {
                let tag = if event.tag().is_empty() {
                    room.item(event.item()).map(|i| i.tag.clone())
                } else {
                    Some(event.tag().to_string())
                };
                let mut who: BTreeSet<String> = match &tag {
                    Some(tag) => room
                        .queue(tag)
                        .into_iter()
                        .flat_map(|i| {
                            std::iter::once(i.posted_by.clone())
                                .chain(i.holder().map(str::to_string))
                        })
                        .collect(),
                    None => BTreeSet::new(),
                };
                who.extend(event.named().into_iter().map(str::to_string));
                // And the tag's own agent, a member of every queue under that tag and of none other.
                // It is not a poster and not a holder, so without this line the one session whose
                // job is the tag would never hear about it. It is the BOX's agent, in the box's room.
                if project == crate::DEFAULT_PROJECT
                    && let Some(agent) = tag
                        .as_deref()
                        .and_then(|t| crate::tag_session(&crate::lease::host_key(), t))
                {
                    who.insert(agent);
                }
                // The arbiter hears everything, so it is an audience member like any other - and
                // a room whose arbiter is not connected has missed every event, which is exactly
                // the fact a reconnected arbiter should be told.
                if let Some(arbiter) = &arbiter {
                    who.insert(arbiter.clone());
                }
                who
            });
            // The bell rang where nobody was - counted for every audience member with no
            // connection, before anything is written, so a session that comes back is told the
            // size of the gap instead of a `Welcome` a session that was never away also gets.
            for session in &audience {
                if !here.contains(session.as_str()) {
                    self.count_missed(session, now);
                }
            }
            // The arbiter is written to through `boxed`, because its connection is in another
            // room; it is in `audience` like every other member (the closure above inserts it), so
            // this one test covers both lists.
            for conn in conns.iter().chain(boxed.iter()) {
                if !audience.contains(&conn.who.session) {
                    continue;
                }
                conn.send(Response::Event {
                    event: event.clone(),
                });
            }
        }
    }

    /// One event was addressed to a session that is not here - see [`Server::route`].
    fn count_missed(&self, session: &str, now: u64) {
        let mut missed = self.inner.missed.lock().expect("the missed lock");
        let entry = missed.entry(session.to_string()).or_insert(Missed {
            count: 0,
            since_ms: now,
        });
        entry.count += 1;
    }

    /// What this session missed, and forget it - the `Hello`'s half of [`Server::count_missed`].
    /// Taking rather than reading is the design: the number is a statement about the gap that just
    /// ended, and a client told the same gap twice would think the room had gone quiet again.
    fn take_missed(&self, session: &str) -> Missed {
        self.inner
            .missed
            .lock()
            .expect("the missed lock")
            .remove(session)
            .unwrap_or(Missed {
                count: 0,
                since_ms: 0,
            })
    }
}

fn is_chat_frame(frame: &Request) -> bool {
    matches!(
        frame,
        Request::Rooms
            | Request::Say { .. }
            | Request::Dm { .. }
            | Request::Notify { .. }
            | Request::Read { .. }
            | Request::Roster { .. }
            | Request::Inbox { .. }
            | Request::Ack { .. }
    )
}

/// One connection's writer: drain its frames onto its socket, one at a time, and end the
/// connection when the socket will not take one. The timeout is the point - a peer that stopped
/// reading must cost its own thread and nobody else's turn.
fn spawn_writer(
    session: String,
    mut socket: UnixStream,
    frames: std::sync::mpsc::Receiver<Response>,
) {
    let name = format!("room-write-{session}");
    let _ = socket.set_write_timeout(Some(WRITE_TIMEOUT));
    let _ = std::thread::Builder::new().name(name).spawn(move || {
        for frame in frames {
            if let Err(e) = write_frame(&mut socket, &frame) {
                let why = match e.kind() {
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                        format!("it took no frame in {}s", WRITE_TIMEOUT.as_secs())
                    }
                    _ => format!("writing to it failed ({e})"),
                };
                eprintln!("room: closing `{session}`'s connection - {why}");
                break;
            }
        }
        // The read loop is blocked on a peer that will never write again, and the socket is the
        // only thing that can end it.
        let _ = socket.shutdown(std::net::Shutdown::Both);
    });
}

/// One frame, one line of JSON. A line rather than a length prefix because the two ends are this
/// crate on both sides, and a frame a person can read with `nc` is worth more than the bytes it
/// costs.
fn write_frame(w: &mut impl Write, response: &Response) -> std::io::Result<()> {
    let mut line = serde_json::to_string(response)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    w.write_all(line.as_bytes())?;
    w.flush()
}

/// `read_frame`'s answer, as four different facts: a frame, the peer's end, a line past the bound
/// (which is answered with a sentence before the close), and a line that is not a frame.
enum ReadFrame {
    Frame(Request),
    End,
    OverLong(usize),
    Bad(String),
}

fn read_frame(reader: &mut BufReader<UnixStream>) -> ReadFrame {
    use std::io::Read;
    let mut line = Vec::new();
    // One byte past the bound is what tells a long line from a line too long: a frame of exactly
    // the bound ends in its newline inside the window, and anything else does not.
    let window = (MAX_FRAME_BYTES + 1) as u64;
    let n = match (&mut *reader).take(window).read_until(b'\n', &mut line) {
        Ok(n) => n,
        Err(e) => return ReadFrame::Bad(e.to_string()),
    };
    if n == 0 {
        return ReadFrame::End;
    }
    if line.last() != Some(&b'\n') {
        return ReadFrame::OverLong(line.len());
    }
    let text = String::from_utf8_lossy(&line);
    let text = text.trim();
    if text.is_empty() {
        return ReadFrame::End;
    }
    match serde_json::from_str(text) {
        Ok(request) => ReadFrame::Frame(request),
        Err(e) => ReadFrame::Bad(e.to_string()),
    }
}

/// The sentence an over-long line is refused with: what arrived, what the room accepts, and that
/// nothing was done. A refusal that only said *closed* would leave the peer guessing at a bound it
/// cannot see.
fn over_long_sentence(got: usize) -> String {
    format!(
        "this connection sent a line longer than {MAX_FRAME_BYTES} bytes ({got} were read) - a \
         frame is one line of JSON, and a longer line is not a request: nothing was done, and the \
         connection is closed"
    )
}

/// The token an operator's seat is granted by: 32 bytes from the kernel's pool, hex. Read from
/// `/dev/urandom` rather than a crate, because the platform has the entropy and a guess of this
/// string is the whole of the seat's security.
fn mint_token() -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Create `path` at mode 0600 with `bytes` in it. Truncate first, so a token shorter than the one
/// it replaces leaves no tail of the old secret on disk.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush()
}

/// Are these the same string, in constant time? A comparison that stopped at the first wrong byte
/// would answer a guesser one byte at a time, and the token is the whole of the operator's proof.
fn same_secret(given: &str, mine: &str) -> bool {
    let (a, b) = (given.as_bytes(), mine.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= (a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0)) as usize;
    }
    diff == 0
}

/// The user on the other end of `stream`, when the kernel will say. A mode is a fact about a file
/// and a descriptor can be inherited, so the socket is not trusted on its mode alone.
#[cfg(target_os = "macos")]
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    let (mut uid, mut gid): (libc::uid_t, libc::gid_t) = (0, 0);
    // SAFETY: both out-parameters outlive the call.
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    (rc == 0).then_some(uid)
}

/// [`peer_uid`], from `SO_PEERCRED` - the same answer, and the only spelling Linux has.
#[cfg(not(target_os = "macos"))]
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `cred` is plain data, and zero is a valid value for it.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` and `len` outlive the call, and `len` is the size of `cred`.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    (rc == 0).then_some(cred.uid)
}

/// The clock every part of the room reads, from the tree's one, re-exported so a beat and a
/// sweep cannot disagree about what *now* is.
pub use letibot_metrics::now_ms;

/// The protocol number this build speaks - re-exported so a client does not link the wire.
pub fn protocol() -> u16 {
    ROOM_PROTOCOL_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_says_a_restart_is_empty_and_not_merely_clean() {
        let dir = std::env::temp_dir().join(format!("letibot-room-id-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = Server::serve(&path, Room::new()).expect("a socket");
        let said = server.identity();
        assert!(said.contains("no store"), "{said}");
        assert!(said.contains("no items at all"), "{said}");
        // The name is the ROOM's - `<host key>/<project key>`, the name a client is held to  -
        // rather than the socket's path, which is one per box and not one per room.
        assert!(
            said.contains(&server.room_name_of(crate::DEFAULT_PROJECT)),
            "{said}"
        );
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_frame_round_trips_through_one_line_of_json() {
        let dir = std::env::temp_dir().join(format!("letibot-room-frame-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = Server::serve(&path, Room::new()).expect("a socket");
        // The client end is written by hand here rather than through `crate::client`, so the
        // framing is proved on its own: one line of JSON in, one line of JSON out.
        let mut a = UnixStream::connect(&path).expect("connect");
        let mut ra = BufReader::new(a.try_clone().unwrap());
        let hello = serde_json::to_string(&Request::Hello {
            protocol: ROOM_PROTOCOL_VERSION,
            speaker: Speaker::top_level("s-a"),
            room: server.name(),
            seat: Default::default(),
            token: None,
        })
        .unwrap();
        a.write_all(format!("{hello}\n").as_bytes()).unwrap();
        a.flush().unwrap();
        let welcome = read_line(&mut ra);
        assert!(welcome.contains("\"welcome\""), "{welcome}");
        assert!(welcome.ends_with('\n'), "a frame is one line: {welcome:?}");
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn read_line(r: &mut BufReader<UnixStream>) -> String {
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        line
    }

    /// A room that has been up for a while and holds nothing is not a room that has just restarted,
    /// and the only thing that says so is the age. An in-memory room holds nothing at every start,
    /// so the sentence carries how long it has been up and how much has happened since.
    #[test]
    fn the_identity_tells_a_long_up_empty_room_from_a_just_restarted_one() {
        let dir = std::env::temp_dir().join(format!("letibot-room-ident-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = Server::serve(&path, Room::new()).expect("a socket");
        let started = server.started_ms();
        let fresh = server.identity_at(crate::DEFAULT_PROJECT, started + 4_000);
        let settled = server.identity_at(crate::DEFAULT_PROJECT, started + 3_600_000);
        assert!(fresh.contains("started 4s ago"), "{fresh}");
        assert!(settled.contains("started 1h ago"), "{settled}");
        assert_ne!(fresh, settled, "two histories, two sentences");
        // Both are empty, and both say so - and both say that emptiness is what a start IS.
        for said in [&fresh, &settled] {
            assert!(said.contains("holding 0 item(s)"), "{said}");
            assert!(said.contains("no store"), "{said}");
            assert!(
                said.contains("a restart begins with no items at all"),
                "{said}"
            );
        }
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A client that reached the wrong coordinator is TOLD, with both names in the sentence. A room
    /// is per box and two boxes can both call themselves `localhost`, so before the name was on the
    /// wire a post landed in another machine's queue.
    #[test]
    fn a_hello_that_names_another_room_is_refused_with_both_names() {
        let dir = std::env::temp_dir().join(format!("letibot-room-name-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = Server::serve(&path, Room::new()).expect("a socket");
        server.with_name("lab2x1");
        let me = Speaker::top_level("s-a");
        let answer = server.answer_frame(
            crate::DEFAULT_PROJECT,
            Seat::Session,
            &me,
            &Request::Hello {
                protocol: ROOM_PROTOCOL_VERSION,
                speaker: me.clone(),
                room: "the-other-box".to_string(),
                seat: Default::default(),
                token: None,
            },
            now_ms(),
        );
        match answer.response {
            Response::Bye { why } => {
                assert!(why.contains("lab2x1"), "the room's own name: {why}");
                assert!(
                    why.contains("the-other-box"),
                    "and the one asked for: {why}"
                );
                assert!(why.contains("Nothing was done"), "{why}");
            }
            other => panic!("a wrong room is a clean Bye: {other:?}"),
        }
        // And a session that names no room is let in - that is a protocol-1 client, which the
        // version check refuses by name instead of pretending it said something it did not.
        let quiet = server.answer_frame(
            crate::DEFAULT_PROJECT,
            Seat::Session,
            &me,
            &Request::Hello {
                protocol: ROOM_PROTOCOL_VERSION,
                speaker: me.clone(),
                room: String::new(),
                seat: Default::default(),
                token: None,
            },
            now_ms(),
        );
        assert!(
            matches!(quiet.response, Response::Welcome { .. }),
            "{:?}",
            quiet.response
        );
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The welcome carries the identity verbatim - the same sentence `Server::identity` prints,
    /// so a client is told what it connected to rather than inferring it from a socket path it chose
    /// itself.
    #[test]
    fn a_welcome_carries_the_identity_verbatim() {
        let dir = std::env::temp_dir().join(format!("letibot-room-hi-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = Server::serve(&path, Room::new()).expect("a socket");
        let me = Speaker::top_level("s-a");
        let now = now_ms();
        let answer = server.answer_frame(
            crate::DEFAULT_PROJECT,
            Seat::Session,
            &me,
            &Request::Hello {
                protocol: ROOM_PROTOCOL_VERSION,
                speaker: me.clone(),
                room: String::new(),
                seat: Default::default(),
                token: None,
            },
            now,
        );
        match answer.response {
            Response::Welcome { identity, .. } => {
                assert_eq!(
                    identity,
                    server.identity_at(crate::DEFAULT_PROJECT, now),
                    "verbatim, not reworded"
                );
                assert!(identity.contains("no store"), "{identity}");
            }
            other => panic!("a hello is answered with a welcome: {other:?}"),
        }
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A ruling that names no project lands in the room that holds the question, and only when
    /// exactly one room has it open: two rooms number theirs from 1, so an ambiguity is refused
    /// rather than applied to the wrong item.
    #[test]
    fn a_ruling_with_no_project_lands_where_the_question_is() {
        let dir = std::env::temp_dir().join(format!("letibot-room-cross-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = Server::serve(&path, Room::new()).expect("a socket");
        let project = "aaaaaaaaaaaa";
        let me = Speaker::top_level("s-a");
        let now = now_ms();
        let mut at = now;
        let frame = |f: Request, at: &mut u64| {
            *at += 1;
            server
                .answer_frame(project, Seat::Session, &me, &f, *at)
                .response
        };
        // A tag claimed and abandoned three times raises question #1 in project `a`.
        frame(
            Request::Post {
                id: "p".into(),
                tag: "ram".into(),
                body: "8 GB".into(),
            },
            &mut at,
        );
        for k in 1..=3 {
            frame(
                Request::Claim {
                    id: format!("c{k}"),
                    tag: "ram".into(),
                },
                &mut at,
            );
            frame(
                Request::Abandon {
                    id: format!("a{k}"),
                    item: 1,
                    why: "no".into(),
                },
                &mut at,
            );
        }
        assert_eq!(
            server.open_questions().len(),
            1,
            "one question, in project a"
        );
        assert_eq!(server.open_questions()[0].project, project);

        // The arbiter rules from ANOTHER room, naming no project.
        let arbiter = Speaker::top_level("s-arbiter");
        let answer = server.answer_frame(
            crate::DEFAULT_PROJECT,
            Seat::Session,
            &arbiter,
            &Request::Rule {
                id: "r1".into(),
                question: 1,
                decision: crate::Decision::Drop {
                    why: "three claimants died on it".into(),
                },
                project: None,
            },
            at + 1,
        );
        match answer.response {
            Response::Ruled { question, .. } => {
                assert_eq!(question.project, project, "the room that asked it");
                assert!(question.answer.is_some());
            }
            other => panic!("the ruling lands where the question is: {other:?}"),
        }
        assert_eq!(server.items_in(project)[0].state_name(), "dropped");
        assert!(
            server.items_in(crate::DEFAULT_PROJECT).is_empty(),
            "nothing landed in the room the arbiter is in"
        );
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A ruling that names a room nobody derived is refused, not obeyed - a room created for a
    /// string the tree would never write has no membership and can never be reached again.
    #[test]
    fn a_ruling_that_names_a_room_nobody_derived_is_refused() {
        let dir = std::env::temp_dir().join(format!("letibot-room-badkey-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = Server::serve(&path, Room::new()).expect("a socket");
        let answer = server.answer_frame(
            crate::DEFAULT_PROJECT,
            Seat::Session,
            &Speaker::top_level("s-a"),
            &Request::Rule {
                id: "r1".into(),
                question: 1,
                decision: crate::Decision::Retry,
                project: Some("../../etc".into()),
            },
            now_ms(),
        );
        match answer.response {
            Response::Refused { why, .. } => {
                assert!(matches!(why, crate::Refused::UnknownRoom { .. }), "{why}");
            }
            other => panic!("a key nobody derived is refused by name: {other:?}"),
        }
        assert_eq!(
            server.projects(),
            vec![crate::DEFAULT_PROJECT.to_string()],
            "and no room was made for it"
        );
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The protocol number is pinned once, here, as a literal: the only place in the tree that
    /// spells the number rather than the constant, so a wire change that misses a pin is a quiet lie
    /// about what two binaries share.
    #[test]
    fn the_room_speaks_protocol_seven() {
        assert_eq!(ROOM_PROTOCOL_VERSION, 7);
        // And the refusal names which side is older, in both directions.
        let old = crate::wire::protocol_refusal(6).expect("6 is not 7");
        assert!(old.contains("the client is the older"), "{old}");
        let new = crate::wire::protocol_refusal(8).expect("8 is not 7");
        assert!(new.contains("this room is the older"), "{new}");
        assert_eq!(crate::wire::protocol_refusal(7), None);
    }

    /// The pane's read: the room's half from the room, the server's three from the server. What is
    /// asserted is that the completion happens, that it is the server's own answer (`identity`, the
    /// arbiter, who is connected), and that a second caller gets the same three.
    #[test]
    fn the_rooms_read_carries_the_servers_own_three_facts() {
        let dir = std::env::temp_dir().join(format!("letibot-room-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = Server::serve(&path, Room::new()).expect("a socket");
        server.with_arbiter("s-arbiter");
        let me = Speaker::top_level("s-a");
        let answer = server.answer_frame(
            crate::DEFAULT_PROJECT,
            Seat::Session,
            &me,
            &Request::Room,
            now_ms(),
        );
        let Response::Room {
            identity,
            arbiter,
            connected,
            items,
            questions,
            agents,
            members,
        } = answer.response
        else {
            panic!("a room read answers a room: {:?}", answer.response);
        };
        assert!(identity.contains("started"), "{identity}");
        assert_eq!(arbiter.as_deref(), Some("s-arbiter"));
        // Nobody is connected through a socket here, so the list is empty and says so - the pane
        // draws *the arbiter is not up* from exactly this.
        assert!(connected.is_empty(), "{connected:?}");
        assert!(items.is_empty());
        assert!(questions.is_empty());
        // A room whose daemon seats no tag agents says `[]` - not an empty column a pane would
        // draw as *nothing to report*, but the fact that no tag has an agent here.
        assert!(agents.is_empty(), "{agents:?}");
        // And the tree, which is empty until a daemon describes it - a room nobody has
        // described holds nobody, and says so rather than inventing a membership.
        assert!(members.is_empty(), "{members:?}");
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A tag's agent is on the pane's read, with its re-seat count - the one fact only the
    /// coordinator has, carried out on the frame a pane is drawn from.
    #[test]
    fn the_rooms_read_carries_the_tag_agents_and_their_counts() {
        let dir = std::env::temp_dir().join(format!("letibot-room-agents-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = Server::serve(&path, Room::new()).expect("a socket");
        let ledger = crate::agent::AgentLedger::new();
        ledger.seat("ram", "s-room-k-ram", 1_000);
        ledger.reseat("ram", "s-room-k-ram", 2_000);
        server.with_agents(Arc::clone(&ledger));
        let answer = server.answer_frame(
            crate::DEFAULT_PROJECT,
            Seat::Session,
            &Speaker::top_level("s-a"),
            &Request::Room,
            now_ms(),
        );
        let Response::Room { agents, .. } = answer.response else {
            panic!("a room read answers a room");
        };
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].tag, "ram");
        assert_eq!(agents[0].session, "s-room-k-ram");
        assert_eq!(agents[0].reseats, 1);
        assert_eq!(agents[0].since_ms, 2_000);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The audience of a tag's event includes that tag's agent, and no other tag's. An agent is
    /// neither a poster nor a holder, so without it the one session whose job is the tag would never
    /// hear about its own queue.
    #[test]
    fn a_tags_events_reach_that_tags_agent_and_no_other() {
        let dir = std::env::temp_dir().join(format!("letibot-room-route-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = Server::serve(&path, Room::new()).expect("a socket");
        let key = crate::lease::host_key();
        let ram = crate::tag_session(&key, "ram").expect("a derivation");
        let disk = crate::tag_session(&key, "disk").expect("a derivation");
        let heard: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        // Kept alive for the whole test: a dropped client closes its socket, which is the
        // coordinator's `note_gone` and would take the connection out of the audience.
        let mut _clients = Vec::new();
        for who in [ram.clone(), disk.clone()] {
            let heard = Arc::clone(&heard);
            let mine = who.clone();
            let wake: crate::client::Wake = Arc::new(move |e: &crate::room::Event| {
                heard
                    .lock()
                    .unwrap()
                    .push((mine.clone(), e.tag().to_string()));
            });
            _clients.push(
                crate::client::Client::connect(&path, Speaker::top_level(who), wake)
                    .expect("a client"),
            );
        }
        let poster = crate::client::Client::connect(
            &path,
            Speaker::top_level("s-b"),
            Arc::new(|_: &crate::room::Event| {}),
        )
        .expect("the poster");
        let _ = poster.request(&Request::Post {
            id: "p1".into(),
            tag: "ram".into(),
            body: "8 GB".into(),
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && heard.lock().unwrap().is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let rows = heard.lock().unwrap().clone();
        assert!(
            rows.iter().any(|(who, tag)| who == &ram && tag == "ram"),
            "the ram agent hears its own queue: {rows:?}"
        );
        assert!(
            !rows.iter().any(|(who, _)| who == &disk),
            "and the disk agent hears nothing about ram: {rows:?}"
        );
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The peer's user is what the socket's mode cannot see: the kernel is asked, and it answers
    /// the uid on the other end. A test can prove the positive on this machine only - a second uid
    /// is another user's process, and asking for one would be a test that needs root.
    #[test]
    fn the_peer_is_named_by_the_kernel() {
        let (a, _b) = UnixStream::pair().expect("a socket pair");
        let mine = unsafe { libc::geteuid() };
        assert_eq!(
            peer_uid(&a),
            Some(mine),
            "a socket of this process is this user's"
        );
        drop(_b);
        // And the token is not guessable from itself: two mints differ, and neither is empty.
        let (one, two) = (mint_token().expect("a mint"), mint_token().expect("a mint"));
        assert_eq!(one.len(), 64, "32 bytes, hex");
        assert_ne!(one, two, "a token is minted, never derived");
        assert!(same_secret(&one, &one));
        assert!(!same_secret(&one, &two));
        assert!(!same_secret(&one[..63], &one), "a prefix is not the token");
    }

    /// A reader that takes nothing is disconnected at the bound, and the queue in front of it
    /// stays bounded - which is the whole reason a push is a `try_send` and not a write.
    #[test]
    fn a_reader_that_takes_nothing_is_disconnected_at_the_bound() {
        use std::io::Read;
        let (mine, mut theirs) = UnixStream::pair().expect("a socket pair");
        // The receiver is held and never drained: dropping it would make the channel
        // `Disconnected` rather than `Full`, which is a different failure.
        let (outbox, _frames) = std::sync::mpsc::sync_channel(2);
        let conn = Conn {
            who: Speaker::top_level("s-slow"),
            seat: Seat::Session,
            project: crate::DEFAULT_PROJECT.to_string(),
            outbox,
            socket: mine.try_clone().expect("a clone"),
            gone: AtomicBool::new(false),
        };
        let bye = |why: &str| Response::Bye { why: why.into() };
        conn.send(bye("one"));
        conn.send(bye("two"));
        // The channel had room for two and nothing drained it, so a third is the bound: the
        // connection ends, and its socket is closed under the reader that was not reading.
        conn.send(bye("three"));
        assert!(conn.gone.load(Ordering::SeqCst), "the bound was reached");
        let mut sink = Vec::new();
        assert_eq!(
            theirs.read_to_end(&mut sink).expect("the peer's read ends"),
            0,
            "a shut-down socket gives the reader end-of-file, not a frame"
        );
        // And a send after the end is a no-op rather than a second disconnection.
        conn.send(bye("four"));
    }
}
