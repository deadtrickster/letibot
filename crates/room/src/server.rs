//! The coordinator's socket: accept loop, a read loop per connection, the [`Room`] behind one
//! lock, bell routing. A close is the daemon's liveness and a beat is the work's, so they are kept
//! apart; durability is the caller's, through [`Server::with_item_store`].

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};

/// The id prefix of a project's review queue session, `s-gatekeeper-<project key>`.
const GATEKEEPER: &str = "s-gatekeeper-";

/// The project key a queue seat (`s-gatekeeper-<key>` and its reviewer children) serves.
fn queue_seat_project(name: &str) -> Option<&str> {
    let key = name.strip_prefix(GATEKEEPER)?;
    let key = key.split("-sub-").next().unwrap_or(key);
    (!key.is_empty()).then_some(key)
}

/// The family an external's `Hello` may declare: a bridge handle whose `-<suffix>` this name is.
/// Anything else is dropped, so a connection cannot read mail it did not lose a race for.
fn family_of(seat: Seat, name: &str, family: Option<&str>) -> Option<String> {
    let f = family?.trim();
    let tail = name.strip_prefix(f)?.strip_prefix('-')?;
    let ok = seat == Seat::External
        && chat::valid_handle(f)
        && chat::bridge_parts(f).is_some()
        && !tail.is_empty()
        && tail.bytes().all(|b| b.is_ascii_alphanumeric());
    ok.then(|| f.to_string())
}

use crate::chat::{self, Chat, Seat};
use crate::item::Speaker;
use crate::lease::{CoordinatorLease, LEASE_BEAT_EVERY_MS};
use crate::peer::{Introductions, Peer, PeerEvent, PeerFrame, SeatProfile};
use crate::room::{Event, Room};
use crate::wire::{Answer, ROOM_PROTOCOL_VERSION, Request, Response};

/// How often the clock runs by default: five seconds, eighteen passes per lease, so a claim whose
/// beat stopped is taken back promptly and a beat that is one pass late is not mistaken for death.
pub const DEFAULT_SWEEP_EVERY_MS: u64 = 5_000;

/// How often a manager-held listener's accept loop looks at the stop flag.
const ACCEPT_TICK_MS: libc::c_int = 200;

/// How long an ask's other end may be gone before the operator is asked to restart it. A restart
/// inside the grace is invisible; a longer wait leaves a plan blocked unannounced. It never
/// settles anything - only the explicit verbs and a deleted session do that.
pub const ASK_GRACE_MS: u64 = 300_000;

/// How many frames one connection may have waiting for its writer thread: roughly a megabyte at a
/// chat message's 4 000 bytes each. That is the slack a reader may fall behind by, and past it the
/// connection is not reading any more.
pub const OUTBOX: usize = 256;

/// How long one frame may take to reach a reader before its connection is ended. A local socket
/// that cannot take 4 KiB in this long has a peer that is not reading, and waiting longer means
/// the room's own writer threads pile up behind it.
pub const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long one frame may be, in bytes: one line of JSON. A project's tree is a few hundred bytes a
/// session, so 1 MiB holds several thousand of them; past it the line is a peer holding the room's
/// memory, not a request, and the connection is closed.
pub const MAX_FRAME_BYTES: usize = 1 << 20;

/// How many principals one coordinator keeps an inbox cursor for. A connection may name itself
/// anything, so a bound is what stops a fresh name each time growing two maps without end; past it
/// the room says so rather than growing.
pub const MAX_PRINCIPALS: usize = 4_096;

/// How many chat messages one member's inbox keeps while it is away. A message is at most
/// `MAX_TEXT` bytes, so this is a quarter of a megabyte per absent member, and the room's own ring
/// still holds every one of them: what is dropped here is the PUSH, never the record.
pub const MAX_PENDING: usize = 64;

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
    /// The name an external asked for before taking a suffixed one: mail to it reaches this
    /// connection too. Checked at the `Hello` to be this name's own prefix.
    family: Option<String>,
    /// The seat's stable id, from the `Hello`'s own field: empty from a client that predates it.
    /// What the seat's own state is keyed by where that state must outlive a reconnect
    /// ([`Server::principal`]), and what a nick is filed under.
    id: String,
    /// The label this connection is drawn by: the `Hello`'s `nick`, else the name it speaks as.
    nick: String,
}

impl Conn {
    /// The seat id this connection was told about, or the one this box would have minted for a
    /// session that sent none - a session's id is its own half, and the host half is the box's.
    /// One spelling for the roster, the nick table and the refusal that says whose a label is.
    fn seat(&self, host: &str) -> String {
        if !self.id.is_empty() {
            return self.id.clone();
        }
        match self.seat {
            Seat::Session => crate::SeatId::of_session(host, &self.who.session).to_string(),
            Seat::External => self.who.session.clone(),
            _ => String::new(),
        }
    }
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
    /// What one sender may post in one room, and when the next post comes back - see
    /// [`crate::budget`]. The operator and the harness never reach it.
    budget: Mutex<crate::budget::Budget>,
    /// What followed each message. Its own lock and file so recording never waits on the chat.
    signals: Mutex<crate::signal::Ledger>,
    signal_window_ms: AtomicU64,
    /// The rooms' items and leases, kept in one store when [`Server::with_item_store`] was called
    /// and memory only otherwise. Shared by every project's room, so all append to one file.
    items: Mutex<Option<Arc<Mutex<crate::store::Store>>>>,
    /// Names of externals that have connected, so a message to one that is away is not refused.
    externals: Mutex<BTreeSet<String>>,
    /// Each project room's invited sessions - a copy of chat's, so a visibility check made
    /// while the chat lock is held does not take it again.
    guests: Mutex<BTreeMap<String, BTreeSet<String>>>,
    /// The principals this room has armed an inbox cursor for - [`MAX_PRINCIPALS`], or what a test
    /// set. A name here is one the chat log keeps a cursor for, and that map is chat's own.
    armed: Mutex<BTreeSet<String>>,
    /// Chat that arrived while a principal had no connection, per principal, in the order it was
    /// said. A disconnect is not a reason to lose a message: what is here is pushed when that
    /// principal next says `hello`, and emptied then.
    pending: Mutex<BTreeMap<String, Pending>>,
    /// The bound on both per-principal maps: [`MAX_PRINCIPALS`], or what a test set.
    principals: AtomicUsize,
    /// How many lines one away box's queue keeps: [`chat::MAX_HELD`], or what a test set.
    held_bound: AtomicUsize,
    /// The token a connection must present to be the operator, minted at [`Server::with_options`]
    /// and written beside the socket in mode 0600. The seat is a file, not a word in a frame.
    operator_token: String,
    /// When each session's last room connection closed, by session: the room's one answer to *is
    /// this session running now*. Set on the last close and cleared by the next `Hello`, because a
    /// connection is the daemon's liveness and a session that closed its own is not a session that
    /// crashed.
    gone_since: Mutex<BTreeMap<String, u64>>,
    /// Each session's last facts from its daemon and when they arrived: what the roster's
    /// presence is derived from. Never stored, a daemon says them again within a second.
    seats: Mutex<BTreeMap<String, (crate::seat::SeatFacts, u64)>>,
    /// Each project's ownership claims, kept in the items store beside the queues' leases.
    claims: Mutex<BTreeMap<String, crate::own::Claims>>,
    /// Open asks whose target had no connection when it was asked: escalated at once while it
    /// is still away. Memory only, a restart gives them the grace again.
    asked_absent: Mutex<BTreeSet<u64>>,
    /// The dm each principal was sent and has not answered — [`DmAwaiting`]. A line that answers
    /// it goes back as a dm however its sender spelled it.
    dms_awaiting: Mutex<BTreeMap<String, DmAwaiting>>,
    /// The open asks that ARE a consent request, by ask seq: `(sender, addressee)`. The answer is
    /// the consent, and it settles here rather than in the sender's hands.
    consent_asks: Mutex<BTreeMap<u64, (String, String)>>,
    /// How long a session may be gone before the operator is asked to restart it -
    /// [`ASK_GRACE_MS`], or what a test set. A liveness fact never settles an ask; it escalates.
    ask_grace_ms: AtomicU64,
    /// The pictures and files a message carries, in `room-<host key>.blobs` beside the socket. Always
    /// present: the directory is made by the first upload, so a room with no pictures touches no disk.
    blobs: Mutex<crate::blob::Blobs>,
    /// The chat store's rewrite count when blobs were last collected, so the collection runs on a
    /// rewrite and not on every message.
    blob_rewrites: AtomicU64,
    /// The listener came from a socket manager, which owns the socket file.
    inherited: bool,
    /// The polled accept thread of an inherited listener, joined by `stop` so nothing accepts
    /// on the manager's socket after it returns.
    accept: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// The label each seat is drawn by, by seat id: what [`Request::Nick`] set. Display only -
    /// nothing is keyed on it, and a seat with no entry is drawn by the name it speaks as.
    nicks: Mutex<BTreeMap<String, String>>,
    /// This box's half of every seat id it hands out, pinned in the data directory by
    /// [`crate::id::seat_host`] - or what a test set. An id is minted nowhere else.
    seat_host: Mutex<String>,
    /// The seat id behind each name that has connected, by that name: what [`Server::principal_of`]
    /// answers for a bridge, whose id nothing in the room could derive. Bounded like the cursors.
    ids_of: Mutex<BTreeMap<String, String>>,
    /// This box's peering, when an operator wrote a cookie: `None` on every box that does not peer,
    /// which is why nothing else in this file asks whether a seat may be on another box.
    peer: Mutex<Option<Peer>>,
    /// The peer seats this box's seats have already been told about: an introduction is said once,
    /// and a seat that reconnects on the same repository is not news.
    introduced: Mutex<Introductions>,
    /// Has the operator been told that this box's beacons cannot get out? One line per outage, and
    /// a link coming up (or a beacon sending again) is what clears it.
    beacon_cut: AtomicBool,
}

/// How long a session was out of earshot, and how much it missed while it was - see
/// [`Inner::missed`]. `since_ms` is the FIRST of them, so a client can say how wide the gap was
/// rather than only how deep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Missed {
    count: u64,
    since_ms: u64,
}

/// The dm a principal is owed an answer to and has not answered: who sent it, and its `seq` - so
/// an answer said anywhere else is still delivered where it belongs. See [`Inner::dms_awaiting`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct DmAwaiting {
    seq: u64,
    peer: String,
}

/// What a `Hello` may do with the name it asks for - see [`Server::hello_name_door`].
enum NameDoor {
    /// The name is free, or this connection is one more of the seat that holds it.
    Admit,
    /// The same seat is already connected and this connection did not ask to share: it is that
    /// seat reconnecting, and the connections the room still holds are its own stale ones.
    Replace,
    /// A name that is taken, by the answer to send. Boxed because a `Response` is far larger than
    /// the two answers beside it, and this enum lives on one stack frame per `Hello`.
    Refuse(Box<Response>),
}

/// One of this box's seats as the mesh needs it: the id it answers to, the project it is in, and
/// the branch and task line its own daemon reported. The repository identity is NOT here - it is
/// read from git by whoever owns the coordinator, and this crate reads no repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatRow {
    /// `<host>/<local>`: the id a peer's message is addressed to.
    pub id: String,
    pub project: String,
    pub branch: String,
    /// The one line the seat is on, as its own task line says it.
    pub task: String,
    /// The label the seat is drawn by here ([`crate::SeatId`]'s neighbour, never an address): a
    /// peer draws it beside the id, and a send still takes the id.
    pub nick: String,
}

/// A principal's chat while it has no connection - see [`Inner::pending`]. `dropped` is what did
/// not fit, said out loud on the log and again when the inbox is delivered, never a silent cut.
#[derive(Debug, Default)]
struct Pending {
    rows: std::collections::VecDeque<chat::Message>,
    since_ms: u64,
    dropped: u64,
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
        Self::build(listener, path, room, sweep_every_ms, false)
    }

    /// Serve on a listener a socket manager (launchd, systemd) already bound at `path`. The file
    /// is the manager's: it is neither removed nor rebound, and a stop leaves connections that
    /// were not accepted yet in the backlog for the next start.
    pub fn with_listener(
        listener: UnixListener,
        path: &Path,
        room: Room,
        sweep_every_ms: u64,
    ) -> std::io::Result<Server> {
        Self::build(listener, path, room, sweep_every_ms, true)
    }

    fn build(
        listener: UnixListener,
        path: &Path,
        room: Room,
        sweep_every_ms: u64,
        inherited: bool,
    ) -> std::io::Result<Server> {
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
                budget: Mutex::new(crate::budget::Budget::default()),
                signals: Mutex::new(crate::signal::Ledger::default()),
                signal_window_ms: AtomicU64::new(crate::signal::SIGNAL_WINDOW_MS),
                items: Mutex::new(None),
                externals: Mutex::new(BTreeSet::new()),
                guests: Mutex::new(BTreeMap::new()),
                armed: Mutex::new(BTreeSet::new()),
                pending: Mutex::new(BTreeMap::new()),
                principals: AtomicUsize::new(MAX_PRINCIPALS),
                held_bound: AtomicUsize::new(chat::MAX_HELD),
                operator_token,
                gone_since: Mutex::new(BTreeMap::new()),
                seats: Mutex::new(BTreeMap::new()),
                claims: Mutex::new(BTreeMap::new()),
                asked_absent: Mutex::new(BTreeSet::new()),
                dms_awaiting: Mutex::new(BTreeMap::new()),
                consent_asks: Mutex::new(BTreeMap::new()),
                ask_grace_ms: AtomicU64::new(ASK_GRACE_MS),
                blobs: Mutex::new(crate::blob::Blobs::open(&path.with_extension("blobs"))),
                blob_rewrites: AtomicU64::new(0),
                stop: AtomicBool::new(false),
                served: AtomicU64::new(0),
                inherited,
                accept: Mutex::new(None),
                nicks: Mutex::new(BTreeMap::new()),
                ids_of: Mutex::new(BTreeMap::new()),
                peer: Mutex::new(None),
                introduced: Mutex::new(Introductions::new()),
                beacon_cut: AtomicBool::new(false),
                // The data directory is the socket's own: `seat_host` pins this box's half of
                // every id it hands out there, once, and reads it back after a rename.
                seat_host: Mutex::new(crate::id::seat_host(
                    path.parent().unwrap_or(std::path::Path::new(".")),
                )),
            }),
        };
        if inherited {
            server.spawn_accept_polled(listener)?;
        } else {
            server.spawn_accept(listener);
        }
        server.spawn_clock();
        Ok(server)
    }

    /// The room of `project`, made if this is the first time anybody asked. A project's room is
    /// empty at its first sight, which is the same reading as a restarted one - and the identity
    /// sentence says so.
    fn with_room<R>(&self, project: &str, f: impl FnOnce(&mut Room) -> R) -> R {
        let lease_ms = self.inner.lease_ms;
        // A room made after the store was attached loads what the store holds for it on first
        // touch, so a project nobody has dialled in again still finds its queue.
        let store = self
            .inner
            .items
            .lock()
            .expect("the items store lock")
            .clone();
        let mut rooms = self.inner.rooms.lock().expect("the rooms lock");
        let room = rooms
            .entry(project.to_string())
            .or_insert_with(|| Room::with_lease_ms(lease_ms));
        if let Some(store) = &store {
            room.ensure_loaded(store, project, now_ms());
        }
        f(room)
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

    /// Where a project's workspace is, as its own daemon last told the room: what a caller turns
    /// into the repository identity its seats are matched by. Empty when no session of that
    /// project has dialled in with a root yet.
    pub fn project_root(&self, project: &str) -> String {
        self.inner
            .rooms
            .lock()
            .expect("the rooms lock")
            .get(project)
            .map(|r| r.root().to_string())
            .unwrap_or_default()
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

    /// Give this coordinator a peer: the mesh it is one node of. `events` is [`Peer::open`]'s own
    /// channel, and what arrives on it is acted on here on a thread of its own, so the peer's
    /// sockets never wait for a room lock.
    ///
    /// A coordinator with no cookie has no peer and never calls this: every seat an id can name is
    /// then on this box, which is exactly the behaviour this server had before peering existed.
    pub fn with_peer(&self, peer: Peer, events: std::sync::mpsc::Receiver<PeerEvent>) -> &Self {
        *self.inner.peer.lock().expect("the peer lock") = Some(peer);
        let server = self.clone();
        std::thread::spawn(move || {
            while !server.inner.stop.load(Ordering::SeqCst) {
                match events.recv_timeout(std::time::Duration::from_millis(500)) {
                    Ok(event) => server.on_peer_event(event),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            // The peer goes with the reader: a stopped coordinator announces nothing and answers
            // nobody, and an object left behind would keep a TCP socket for a room that is gone.
            if let Some(peer) = server.inner.peer.lock().expect("the peer lock").take() {
                peer.stop();
            }
        });
        self
    }

    /// This box's mesh, when an operator wrote a cookie. Cloned rather than borrowed because it is
    /// reached from the append path, which holds no other lock at that point.
    pub fn peer(&self) -> Option<Peer> {
        self.inner.peer.lock().expect("the peer lock").clone()
    }

    /// The boxes this one knows of and their state - what a pane draws for *the mesh*.
    pub fn mesh(&self) -> Vec<(String, bool)> {
        match self.peer() {
            Some(p) => {
                let mut rows: Vec<(String, bool)> = p.up().into_iter().map(|h| (h, true)).collect();
                rows.extend(p.away().into_iter().map(|h| (h, false)));
                rows.sort();
                rows.dedup();
                rows
            }
            None => Vec::new(),
        }
    }

    /// Is `id` a seat of a box this one KNOWS of - linked now, or away with its seats still in the
    /// mesh, or away since before this coordinator started? An away box keeps its seats, so a line
    /// for one is accepted and held rather than refused; a box this one has never heard of, and a
    /// seat such a box never announced, are both still nobody.
    fn peer_holds(&self, id: &crate::SeatId) -> bool {
        let seat = id.to_string();
        // A box with no cookie has no mesh, and nothing of it is elsewhere: an id this one cannot
        // reach is nobody, whatever a store written by an older configuration still says.
        let Some(peer) = self.peer() else {
            return false;
        };
        if peer.seats_with_state().iter().any(|(s, _)| s.id == seat) {
            return true;
        }
        // The mesh lives in memory, so a restart empties it. What each box announced is in the chat
        // store, and that is what a dm to a sleeping laptop's seat is answered from.
        self.inner
            .chat
            .lock()
            .expect("the chat lock")
            .remembers_seat(&seat, now_ms())
    }

    /// What the mesh learned, done to the room. A refusal is said on the log rather than to the far
    /// side: a peer cannot act on this box's refusal, and the person reading the log can.
    fn on_peer_event(&self, event: PeerEvent) {
        match event {
            PeerEvent::Up { host } => {
                eprintln!(
                    "room: {host} is linked; its seats are in this box's roster while it stays"
                );
                // A link is the fact a beacon-cut notice asked about, so it is what clears it.
                self.say_beacon_back(&format!("{host} linked"));
                // And what this box was told about consents while they were apart.
                self.ship_consents(&host);
                // And every line it kept for that box while it was away.
                self.ship_held(&host);
            }
            PeerEvent::Down { why, .. } => eprintln!("room: {why}"),
            PeerEvent::BeaconCut { why } => self.say_beacon_cut(&why),
            PeerEvent::BeaconBack => self.say_beacon_back("the beacon is getting out again"),
            PeerEvent::Profiles { host, seats } => {
                // Kept before it is acted on: an id in this list stays an addressee after a restart.
                self.remember_seats(&host, &seats);
                self.introduce(&seats)
            }
            PeerEvent::Message(message) => self.accept_remote(*message),
            PeerEvent::Consents(rows) => self.learn_consents(rows),
        }
    }

    /// This box cannot announce itself on the LAN: the operator is told once per outage, because
    /// nothing else in this room says that a running coordinator is unreachable. A blocker, so it
    /// arrives on their connection and is not one more line of the host room's talk.
    fn say_beacon_cut(&self, why: &str) {
        if self.inner.beacon_cut.swap(true, Ordering::SeqCst) {
            return;
        }
        let now = now_ms();
        let _ = self.append_and_push(
            &format!("beacon:cut:{now}"),
            chat::HOST_ROOM,
            "room",
            Seat::System,
            Some(chat::OPERATOR.to_string()),
            None,
            chat::with_say_ref(crate::SayKind::Blocker, &[]).unwrap_or_default(),
            Vec::new(),
            why,
            now,
        );
    }

    /// The other half of [`Server::say_beacon_cut`]: a beacon got out, or a box linked, so the box is
    /// not cut off any more. Said once, and never without the notice it answers.
    fn say_beacon_back(&self, why: &str) {
        if !self.inner.beacon_cut.swap(false, Ordering::SeqCst) {
            return;
        }
        let now = now_ms();
        let _ = self.append_and_push(
            &format!("beacon:back:{now}"),
            chat::HOST_ROOM,
            "room",
            Seat::System,
            Some(chat::OPERATOR.to_string()),
            None,
            chat::with_say_ref(crate::SayKind::Fyi, &[]).unwrap_or_default(),
            Vec::new(),
            &format!("this box can announce itself on the LAN again: {why}"),
            now,
        );
    }

    /// Keep what a linked box said its seats are, in the room's own store: the mesh that holds them
    /// is memory only, and the seats are what a dm to that box is accepted by once it is away.
    fn remember_seats(&self, host: &str, seats: &[SeatProfile]) {
        self.inner
            .chat
            .lock()
            .expect("the chat lock")
            .peer_announced(host, seats, now_ms());
    }

    /// A peer seat whose repository is one of this box's lands in that project's room, once: the
    /// line says who it now shares the room with and what it is on, and neither side is asked.
    fn introduce(&self, seats: &[SeatProfile]) {
        let Some(peer) = self.peer() else {
            return;
        };
        let host = self.seat_host();
        let now = now_ms();
        for seat in seats {
            // The far side's list is its own seats; one that claims this box's name is not news,
            // and a seat this box has not been told about is the whole of what is left.
            if seat.host() == host {
                continue;
            }
            // The project first: a seat on a repository this box does not hold is not a match, and
            // it must not be filed as one that was quietly told.
            let Some(project) = peer.project_of(seat.identity()) else {
                continue;
            };
            if !self
                .inner
                .introduced
                .lock()
                .expect("the introduced lock")
                .is_new(seat)
            {
                continue;
            }
            let room = chat::project_room(&project.key);
            let line = seat.line();
            let request = format!("peer-intro:{}:{}", seat.id, seat.identity());
            let _ = self.append_and_push(
                &request,
                &room,
                &seat.id,
                Seat::External,
                None,
                None,
                Vec::new(),
                Vec::new(),
                &line,
                now,
            );
        }
    }

    /// A line that came over a link, said here: the far side numbered it and said it in its own
    /// room, so this box files it under the sender and its seq - a redelivery is the same message,
    /// never a second one - and addresses it to the seat here that holds the id.
    fn accept_remote(&self, message: chat::Message) {
        let Some(id) = message.to.as_deref().and_then(crate::SeatId::parse) else {
            eprintln!(
                "room: `{}` sent a line addressed to `{}`, which is not a seat id - a peer carries \
                 only what is addressed to a seat",
                message.author,
                message.to.unwrap_or_default()
            );
            return;
        };
        if id.host() != self.seat_host() {
            eprintln!(
                "room: `{}` sent a line for {id}, which is another box's seat - this box cannot \
                 deliver it and did not",
                message.author
            );
            return;
        }
        let Some(name) = self.name_of_id(&id.to_string()) else {
            eprintln!(
                "room: a line from `{}` arrived for {id} and no seat on this box answers to it",
                message.author
            );
            return;
        };
        let request = format!("peer:{}:{}", message.author, message.seq);
        let now = now_ms();
        // The attachments stay on the box that holds their bytes: a blob id means nothing here,
        // and a message whose pictures are missing is better than one that cannot be read at all.
        let response = self.append_and_push(
            &request,
            &message.room,
            &message.author,
            message.kind,
            Some(name),
            None,
            message.refs.clone(),
            Vec::new(),
            &message.text,
            now,
        );
        if let Response::Refused { why, .. } = response {
            eprintln!(
                "room: a line from `{}` was refused here ({why})",
                message.author
            );
        }
    }

    /// Does this line address a seat on another box? The one question a record and a link both ask:
    /// a record would be a reference the far box cannot read, and the line's own text is what crosses.
    fn across_boxes(&self, to: Option<&str>) -> bool {
        to.and_then(crate::SeatId::parse)
            .is_some_and(|id| id.host() != self.seat_host())
    }

    /// A line addressed to a seat on another box goes over that box's link. Called from the one
    /// place a message is appended, so every door that can address an id is routed by it.
    fn ship_remote(&self, message: &chat::Message) {
        let Some(id) = message.to.as_deref().and_then(crate::SeatId::parse) else {
            return;
        };
        if id.host() == self.seat_host() {
            return;
        }
        // A message that came over a link is not sent back over one: only a seat on this box
        // speaks here, and a relayed line's author is the far side's.
        if crate::SeatId::parse(&message.author).is_some_and(|a| a.host() != self.seat_host()) {
            return;
        }
        let Some(peer) = self.peer() else {
            return;
        };
        // The far side reads the author by its ID: this box's name for the seat (`s-a1`) means
        // whatever that box's own `s-a1` is, and a line signed with it would name the wrong seat
        // there - the one thing lead 70 says an id exists to prevent.
        let mut shipped = message.clone();
        let author = self.principal_of(&message.author);
        if !author.is_empty() {
            shipped.author = author;
        }
        let host = id.host().to_string();
        if !peer.ship(
            &host,
            PeerFrame::Message {
                message: shipped.clone(),
            },
        ) {
            // A box that is away is one this box still owes the line to: kept, in order, and
            // shipped when the link comes back - the promise `pending` makes for a principal.
            let bound = self.inner.held_bound.load(Ordering::SeqCst);
            let dropped = self
                .inner
                .chat
                .lock()
                .expect("the chat lock")
                .hold_for(&host, shipped, bound);
            eprintln!(
                "room: {id} is on {host} and its link is down - the line is kept for it and \
                 goes over when the box is back"
            );
            if let Some(gone) = dropped {
                eprintln!(
                    "room: {host}'s queue holds {bound} already, so the oldest went unshipped: \
                     #{} {}",
                    gone.seq,
                    gone.text.lines().next().unwrap_or_default()
                );
            }
        }
    }

    /// This box's seats as the mesh needs them: the id of every live seat, the project it is in,
    /// and what its daemon last said it was on. `project` is the key a caller turns into a
    /// repository identity; nothing here reads git.
    ///
    /// One row per SEAT, however many connections hold it: a bridge and its folder's waiter are two
    /// connections of one id, and a mesh that carried both would offer a peer one seat twice.
    pub fn seat_rows(&self) -> Vec<SeatRow> {
        let host = self.seat_host();
        let facts: BTreeMap<String, crate::seat::SeatFacts> = self
            .inner
            .seats
            .lock()
            .expect("the seats lock")
            .iter()
            .map(|(name, (facts, _))| (name.clone(), facts.clone()))
            .collect();
        let mut rows: BTreeMap<String, SeatRow> = BTreeMap::new();
        for c in self
            .conn_list()
            .into_iter()
            .filter(|c| matches!(c.seat, Seat::Session | Seat::External))
        {
            let working = facts.get(&c.who.session).map(|f| f.working.clone());
            let id = c.seat(&host);
            rows.insert(
                id.clone(),
                SeatRow {
                    id,
                    project: c.project.clone(),
                    branch: working
                        .as_ref()
                        .map(|w| w.branch.clone())
                        .unwrap_or_default(),
                    task: working
                        .as_ref()
                        .map(|w| w.row.trim().to_string())
                        .unwrap_or_default(),
                    nick: if c.nick.is_empty() {
                        self.inner
                            .nicks
                            .lock()
                            .expect("the nicks lock")
                            .get(&c.id)
                            .cloned()
                            .unwrap_or_default()
                    } else {
                        c.nick.clone()
                    },
                },
            );
        }
        rows.into_values().collect()
    }

    /// Keep chat in `path`: load what it holds and append from now on. Call before the first
    /// connection, so no message is made against the empty chat it replaces.
    pub fn with_chat_store(&self, path: &Path) -> &Self {
        let chat = Chat::open(path);
        *self.inner.guests.lock().expect("the guests lock") = chat.guests();
        *self.inner.chat.lock().expect("the chat lock") = chat;
        let mut ledger = path.as_os_str().to_owned();
        ledger.push(".signals");
        *self.inner.signals.lock().expect("the signals lock") = crate::signal::Ledger::open(
            Path::new(&ledger),
            self.inner.signal_window_ms.load(Ordering::SeqCst),
        );
        self
    }

    /// How long after a message an action still counts as its result. Call before
    /// [`Server::with_chat_store`], which opens the ledger with it.
    pub fn with_signal_window(&self, ms: u64) -> &Self {
        self.inner.signal_window_ms.store(ms, Ordering::SeqCst);
        self.inner
            .signals
            .lock()
            .expect("the signals lock")
            .set_window(ms);
        self
    }

    /// The signal scorecard now.
    pub fn scorecard(&self) -> crate::signal::Scorecard {
        self.inner
            .signals
            .lock()
            .expect("the signals lock")
            .scorecard(now_ms())
    }

    /// Something followed message `seq`: the hook for parts that learn of an action the room does
    /// not see itself, such as an ownership claim.
    pub fn note_act(&self, seq: u64, act: crate::signal::Act, by: &str) {
        self.inner
            .signals
            .lock()
            .expect("the signals lock")
            .act(seq, act, by, now_ms());
    }

    /// Wait until the signal ledger's file holds everything recorded. For tests.
    pub fn flush_signals(&self) {
        self.inner.signals.lock().expect("the signals lock").flush();
    }

    /// Keep the blobs messages carry in `dir`, instead of the `room-<host key>.blobs` beside the
    /// socket. A test points it at its own directory; the coordinator never needs to.
    pub fn with_blob_store(&self, dir: &Path) -> &Self {
        *self.inner.blobs.lock().expect("the blob lock") = crate::blob::Blobs::open(dir);
        self
    }

    /// Keep every room's items and leases in `path`: load what it holds - one room per project it
    /// knows - and append from now on. Call before the first connection, so no verb is answered
    /// against the empty rooms it replaces.
    pub fn with_item_store(&self, path: &Path) -> &Self {
        self.with_item_store_at(path, now_ms())
    }

    /// [`Server::with_item_store`] at a clock the caller names, so a test can make a lease stale
    /// without sleeping one out. A claim whose beat is older than the lease is taken back here,
    /// by the room's own sweep, and the take-back is recorded.
    pub fn with_item_store_at(&self, path: &Path, now: u64) -> &Self {
        let store = Arc::new(Mutex::new(crate::store::Store::open(path)));
        *self.inner.items.lock().expect("the items store lock") = Some(Arc::clone(&store));
        // Every project the file holds is rebuilt before the first connection: the tie-breaker
        // reads open questions at start, so a raised item must not wait for its project to dial
        // in again. The box's own room is loaded too, empty or not, so its verbs persist.
        let mut projects = store.lock().unwrap_or_else(|e| e.into_inner()).projects();
        if !projects.iter().any(|p| p == crate::DEFAULT_PROJECT) {
            projects.push(crate::DEFAULT_PROJECT.to_string());
        }
        let claims = store.lock().unwrap_or_else(|e| e.into_inner()).claims();
        *self.inner.claims.lock().expect("the claims lock") = claims
            .into_iter()
            .map(|(p, rows)| (p, crate::own::Claims::from_rows(rows)))
            .collect();
        for project in projects {
            let mut rooms = self.inner.rooms.lock().expect("the rooms lock");
            let room = rooms
                .entry(project.clone())
                .or_insert_with(|| Room::with_lease_ms(self.inner.lease_ms));
            room.ensure_loaded(&store, &project, now);
        }
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

    /// Shrink the bound on what one away box's queue keeps - a test's door onto
    /// [`chat::MAX_HELD`], which is sixty-four because a box holds a handful of seats.
    pub fn with_held(&self, held: usize) -> &Self {
        self.inner.held_bound.store(held.max(1), Ordering::SeqCst);
        self
    }

    /// What this box still owes a box that is away: one row per host, with how many lines, in the
    /// order they will go. It survives a restart, because the queue is in the chat store.
    pub fn held_away(&self) -> Vec<(String, usize)> {
        self.inner.chat.lock().expect("the chat lock").held()
    }

    /// Whether the store has kept what `host` said its seats are - the fact a restart reads, which
    /// lands a moment after the mesh holds the seat, because the store is written off the link.
    pub fn remembers_seats_of(&self, host: &str) -> bool {
        self.inner
            .chat
            .lock()
            .expect("the chat lock")
            .peer_seats(host)
            .is_some()
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

    /// Shrink the bound on the principals the room tracks state for - a test's door onto
    /// [`MAX_PRINCIPALS`], which is four thousand because a box has tens of sessions.
    pub fn with_principals(&self, principals: usize) -> &Self {
        self.inner
            .principals
            .store(principals.max(1), Ordering::SeqCst);
        self
    }

    /// Set how long a gone session's ask may stand before the operator is asked to restart it -
    /// [`ASK_GRACE_MS`], or a test's seconds. The clock that reads it is the room's own.
    pub fn with_ask_grace(&self, ms: u64) -> &Self {
        self.inner.ask_grace_ms.store(ms, Ordering::SeqCst);
        self
    }

    /// Set the posts one sender may make in one room - [`crate::budget::BUDGET_POSTS`], or a
    /// test's smaller number. The refill stays the module's own.
    pub fn with_budget(&self, posts: u32) -> &Self {
        *self.inner.budget.lock().expect("the budget lock") =
            crate::budget::Budget::new(posts, crate::budget::BUDGET_REFILL_MS);
        self
    }

    /// The budget's size and refill, as the room holds them.
    pub fn budget(&self) -> (u32, u64) {
        let b = self.inner.budget.lock().expect("the budget lock");
        (b.posts(), b.refill_ms())
    }

    /// The grace an ask's other end has before the operator is owed a restart.
    pub fn ask_grace_ms(&self) -> u64 {
        self.inner.ask_grace_ms.load(Ordering::SeqCst)
    }

    /// When `session`'s last connection closed, `None` while it is connected or was never gone -
    /// the one liveness fact an ask escalates on.
    pub fn gone_since(&self, session: &str) -> Option<u64> {
        self.inner
            .gone_since
            .lock()
            .expect("the gone-since lock")
            .get(session)
            .copied()
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
        // Read before the rooms lock is taken: the items lock is a leaf, and nesting it under
        // `with_room` would hold two guards for one sentence.
        let kept = if self
            .inner
            .items
            .lock()
            .expect("the items store lock")
            .is_some()
        {
            "its queues are kept in a store beside the socket, so a restart brings them back"
        } else {
            "this room has no item store, so a restart begins with no items at all"
        };
        let chat_kept = match self.inner.chat.lock().expect("the chat lock").store_path() {
            Some(p) => format!("chat is kept in {}", p.display()),
            None => "chat is in memory only, so a restart empties it".to_string(),
        };
        let items = self.with_room(project, |r| r.all().len());
        let up = now.saturating_sub(self.inner.started_ms);
        format!(
            "the room of `{}` — started {} ago and holding {items} item(s) since then; {kept}; \
             {chat_kept} ({n} connection(s) served since it started)",
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
            share,
            id,
            ..
        } = frame
        {
            if let Some(why) = crate::wire::protocol_refusal(*protocol) {
                return Answer {
                    response: Response::Bye { why },
                    events: Vec::new(),
                };
            }
            // A seat id is minted once, by the box that holds the seat, and nothing here can
            // repair a malformed one: a `Hello` that carries something other than an id is refused
            // before it is admitted, so a typo does not become a second seat of the same name.
            let id = id.trim();
            if !id.is_empty() && crate::SeatId::parse(id).is_none() {
                return Answer {
                    response: Response::Refused {
                        id: None,
                        why: crate::Refused::BadSeatId { id: id.to_string() },
                    },
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
            // The same seat reconnecting without asking to share: the room's list is what a name is
            // held by, and a socket whose coordinator is gone cannot be noticed from the client's
            // side. Ended only once this Hello has passed every other refusal, so a misaddressed
            // frame cannot knock out the connection that is working.
            let mut replaces = false;
            match self.hello_name_door(seat, who, *share, id) {
                NameDoor::Admit => {}
                NameDoor::Replace => replaces = true,
                NameDoor::Refuse(response) => {
                    return Answer {
                        response: *response,
                        events: Vec::new(),
                    };
                }
            }
            // The room keeps a cursor and a missed-count per principal, and a connection may name
            // itself anything: a fresh name every time would grow both maps without end. Past the
            // bound the connection is told so, with the number, rather than the maps growing.
            if let Some(why) = self.principal_refusal(&self.principal_of(&who.session)) {
                return Answer {
                    response: Response::Bye { why },
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
            if replaces {
                self.end_name(&who.session, "the same seat reconnected on its own name");
            }
        }
        // A seat's facts, kept under the connection's own name: a daemon reports only its own, and
        // a bridge reports the seat it holds - it is a seat with no daemon. The root a seat names is
        // the room's, so a project whose only seat is a bridge still has somewhere to be found.
        if let Request::Seat { facts, root } = frame {
            let mut owns = Vec::new();
            if seat == Seat::Session {
                self.inner
                    .seats
                    .lock()
                    .expect("the seats lock")
                    .insert(who.session.clone(), (facts.clone(), now));
                owns = self.renew_claims(project, &who.session, facts, now);
            } else if seat == Seat::External {
                self.inner
                    .seats
                    .lock()
                    .expect("the seats lock")
                    .insert(who.session.clone(), (facts.clone(), now));
            }
            if !root.trim().is_empty()
                && let Some(room) = self
                    .inner
                    .rooms
                    .lock()
                    .expect("the rooms lock")
                    .get_mut(project)
            {
                room.set_root(root.clone());
            }
            return Answer {
                response: Response::Seated { owns },
                events: Vec::new(),
            };
        }
        if let Some(response) = self.ownership_frame(project, seat, &who.session, frame, now) {
            return Answer {
                response,
                events: Vec::new(),
            };
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
                let was = self.take_missed(&self.principal_of(&who.session));
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

    /// The newest chat `seq` `name` is marked as having seen, `None` for a member never seen. The
    /// cursor is the SEAT's, so a name is resolved first: a session's mark is kept under the id that
    /// outlives a rename, and a caller holding the name it speaks as must still find it.
    pub fn seen(&self, name: &str) -> Option<u64> {
        let id = self.principal_of(name);
        self.inner.chat.lock().expect("the chat lock").seen(&id)
    }

    /// How many sessions are connected now - the pane's *is anybody here*, and the number a test
    /// reads instead of sleeping.
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
        // The asks' own clock: a session that is gone starts its grace here rather than only on
        // the `Ask` that asked it, so a room restarted under an open ask counts from its start,
        // and a settle that has been kept long enough goes.
        self.sweep_asks(now);
        self.sweep_claims(now);
        all
    }

    /// The ask half of the clock: note every absent end of an open ask, and drop settled asks past
    /// their keep window. Nothing is settled here - liveness only escalates.
    fn sweep_asks(&self, now: u64) {
        let here: BTreeSet<String> = self
            .conn_list()
            .iter()
            .map(|c| c.who.session.clone())
            .collect();
        let (absent, pruned) = {
            let mut chat = self.inner.chat.lock().expect("the chat lock");
            let peers: BTreeSet<String> = chat
                .asks_by_any()
                .into_iter()
                .filter(|a| a.open())
                .flat_map(|a| [a.asker, a.target])
                .filter(|p| !here.contains(p))
                .collect();
            let pruned = chat.prune_asks(now);
            (peers, pruned)
        };
        if !absent.is_empty() {
            let mut gone = self.inner.gone_since.lock().expect("the gone-since lock");
            for peer in absent {
                gone.entry(peer).or_insert(now);
            }
        }
        if pruned > 0 {
            eprintln!("room: dropped {pruned} settled ask(s) older than the keep window");
        }
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
        let accept = self.inner.accept.lock().expect("the accept lock").take();
        if let Some(h) = accept
            && h.thread().id() != std::thread::current().id()
        {
            let _ = h.join();
        }
        if !self.inner.inherited {
            let _ = std::fs::remove_file(&self.inner.path);
        }
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
        // Peering goes with the room: a box that is stopping says goodbye on every link and hears
        // no more beacons, rather than staying a name on the LAN that answers nothing.
        if let Some(peer) = self.inner.peer.lock().expect("the peer lock").take() {
            peer.stop();
        }
    }

    // ===== Chat =====

    /// Why this principal may not be tracked, when the room already tracks the most it will. A name
    /// it has seen before is free: the cursor and the count are already there.
    fn principal_refusal(&self, name: &str) -> Option<String> {
        let max = self.inner.principals.load(Ordering::SeqCst);
        let armed = self.inner.armed.lock().expect("the armed lock");
        if armed.contains(name) || armed.len() < max {
            return None;
        }
        Some(format!(
            "the room is already tracking inbox state for {max} sessions, which is as many as it \
             keeps, and `{name}` is not one of them - nothing was done; a coordinator restart \
             begins with none of them"
        ))
    }

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

    /// What this `Hello` may do with the name it asks for. `id` is the seat it claims to be: an id
    /// is one seat, so a second connection under one name is admitted only when it IS that seat -
    /// sharing it (a bridge and its waiter) or replacing it (a reconnect). A different seat under a
    /// name already speaking is what a suffixed twin is for.
    fn hello_name_door(&self, seat: Seat, who: &Speaker, share: bool, id: &str) -> NameDoor {
        let name = who.session.as_str();
        let ok = match seat {
            Seat::Session => name != chat::OPERATOR && !name.is_empty(),
            Seat::Operator => name == chat::OPERATOR && who.is_top_level(),
            Seat::External => chat::valid_handle(name) && who.is_top_level(),
            Seat::System => false,
        };
        if !ok {
            return NameDoor::Refuse(Box::new(Response::Refused {
                id: None,
                why: crate::Refused::ReservedName {
                    name: name.to_string(),
                },
            }));
        }
        let id = id.trim();
        // One id is one seat, whatever name it is spoken under: a second connection claiming an id
        // the room already keys a cursor and a wait on is refused - unless it says it is one more
        // connection of that seat (a bridge and its waiter do).
        if !share
            && !id.is_empty()
            && self
                .conn_list()
                .iter()
                .any(|c| c.id == id && c.who.session != name)
        {
            return NameDoor::Refuse(Box::new(Response::Refused {
                id: None,
                why: crate::Refused::NameInUse {
                    name: id.to_string(),
                },
            }));
        }
        if seat != Seat::External {
            return NameDoor::Admit;
        }
        let held = self.conn_list();
        let held: Vec<&Arc<Conn>> = held.iter().filter(|c| c.who.session == name).collect();
        if held.is_empty() {
            return NameDoor::Admit;
        }
        // A SEAT is an id, and a connection that names none is a reader of the name rather than a
        // seat: a folder's waiter reads what its bridge reads and holds nothing, and the fact the two
        // processes share is the folder, not the id each of them minted from its own environment.
        // Two seats still differ by their ids, so a second Claude Code session takes a twin.
        if share && id.is_empty() {
            return NameDoor::Admit;
        }
        // A connection that named no id predates ids, and the name is all there is to tell it by.
        let is = |c: &Conn| c.id.is_empty() || (!id.is_empty() && c.id == id);
        if share {
            return if held.iter().all(|c| is(c)) {
                NameDoor::Admit
            } else {
                NameDoor::Refuse(Box::new(Response::Refused {
                    id: None,
                    why: crate::Refused::NameInUse {
                        name: name.to_string(),
                    },
                }))
            };
        }
        if held.iter().any(|c| !id.is_empty() && c.id == id) && held.iter().all(|c| is(c)) {
            return NameDoor::Replace;
        }
        NameDoor::Refuse(Box::new(Response::Refused {
            id: None,
            why: crate::Refused::NameInUse {
                name: name.to_string(),
            },
        }))
    }

    /// End every connection speaking as `name`: a name taken by a socket the room cannot use again.
    /// Each ends the same way a stuck one does, so its reader wakes and its thread goes.
    fn end_name(&self, name: &str, why: &str) {
        for c in self.conn_list() {
            if c.who.session == name {
                c.give_up(why);
            }
        }
    }

    /// The seat id another connection under `name` speaks as, or empty when none does: what a
    /// connection that names no seat of its own joins, so a reader is drawn as the seat it reads
    /// for rather than as an unnamed thing beside it.
    fn id_held_for(&self, name: &str) -> String {
        self.conn_list()
            .iter()
            .find(|c| c.who.session == name && !c.id.is_empty())
            .map(|c| c.id.clone())
            .unwrap_or_default()
    }

    /// The family a connected external `name` declared at its `Hello`.
    fn family(&self, name: &str) -> Option<String> {
        self.conn_list()
            .iter()
            .find(|c| c.who.session == name && c.family.is_some())
            .and_then(|c| c.family.clone())
    }

    /// Does a connection read `name`'s mail as its family?
    fn family_connected(&self, name: &str) -> bool {
        self.conn_list()
            .iter()
            .any(|c| c.family.as_deref() == Some(name))
    }

    /// The seat ids a written word is the LABEL of, when it names a seat by something other than the
    /// name that seat speaks as: the ids a send must carry instead. Empty for an address - an id, a
    /// `cc@` handle, a `s-…` session, the operator, the harness, a queue seat - and for a word no
    /// seat answers to. More than one is a name that fits several, which is the room's `Ambiguous`.
    ///
    /// Precedence is `resolve_handle`'s: a word that IS a handle is that member's, and only when no
    /// handle answers does a title count. Both are refused; only the count differs.
    fn seats_behind_label(&self, written: &str) -> Vec<String> {
        let word = written.trim().trim_start_matches('@').trim();
        if word.is_empty()
            || crate::SeatId::parse(word).is_some()
            || word.starts_with("s-")
            || chat::bridge_parts(word).is_some()
            || chat::BRIDGE_ALIASES.contains(&word)
        {
            return Vec::new();
        }
        // A label is written the way a person reads it, so `Rano Session` and `rano-session` are one
        // name: the exact spelling decides first, the folded one after it.
        let key = name_key(word);
        let want = word.to_lowercase();
        let host = self.seat_host();
        let mut exact: Vec<String> = Vec::new();
        let mut folded: Vec<String> = Vec::new();
        {
            let rooms = self.inner.rooms.lock().expect("the rooms lock");
            for m in rooms.values().flat_map(|r| r.members().iter()) {
                if m.group != crate::Group::Top || m.session == word {
                    continue;
                }
                let id = crate::SeatId::of_session(&host, &m.session).to_string();
                if !m.handle.is_empty() && m.handle == want {
                    exact.push(id);
                } else if (!m.handle.is_empty() && (!key.is_empty() && name_key(&m.handle) == key))
                    || (!key.is_empty() && name_key(&m.label) == key)
                {
                    folded.push(id);
                }
            }
        }
        // A bridge's label is the nick the ROOM holds for it, by seat id - a nick is taken with a
        // `Nick` frame and kept there, so the connection's own field says nothing about it.
        for (seat, (label, name)) in self.live_nicks() {
            if label == name || crate::SeatId::parse(&seat).is_none() {
                continue;
            }
            if label == want {
                exact.push(seat);
            } else if !key.is_empty() && name_key(&label) == key {
                folded.push(seat);
            }
        }
        let mut ids = if exact.is_empty() { folded } else { exact };
        ids.sort();
        ids.dedup();
        ids
    }

    /// The external names holding a connection now.
    fn external_names(&self) -> Vec<String> {
        self.conn_list()
            .iter()
            .filter(|c| c.seat == Seat::External)
            .map(|c| c.who.session.clone())
            .collect()
    }

    /// The principal a seat id names: the connection that says it, else the map a bridge's id was
    /// greeted into, else - for a session - the local half, which IS its name. `None` for an id no
    /// connection here speaks as, another box's included: an id is minted where its seat is.
    fn name_of_id(&self, id: &str) -> Option<String> {
        let parsed = crate::SeatId::parse(id)?;
        let host = self.seat_host();
        if let Some(c) = self
            .conn_list()
            .into_iter()
            .find(|c| c.seat(&host) == parsed.to_string())
        {
            return Some(c.who.session.clone());
        }
        if let Some(name) = self
            .inner
            .ids_of
            .lock()
            .expect("the ids lock")
            .iter()
            .find(|(_, at)| **at == parsed.to_string())
            .map(|(name, _)| name.clone())
        {
            return Some(name);
        }
        // A seat on another box is an ADDRESS this box can post to and nothing more: it is named
        // by its id, no connection here answers to it, and what it is sent goes over the link.
        if parsed.host() != host {
            return self.peer_holds(&parsed).then(|| parsed.to_string());
        }
        // The local half is this box's own assumption: a session whose id this box would have
        // minted. A host that is not this box's is never spelled as a local name - that is how a
        // dm to another box's `s-1` would have reached this box's own `s-1`.
        (parsed.is_session() && !parsed.host().is_empty()).then(|| parsed.local().to_string())
    }

    /// What a name a sender wrote means: itself when somebody answers to it, else the one
    /// connected bridge a short name (`cc`, `claude-code`) stands for, else the session whose
    /// handle it is. `Err` says why the name named nobody, and what it could have been.
    ///
    /// A seat ID is the address itself and is resolved first; a word that is a seat's LABEL rather
    /// than its address is refused with the id it would have meant when `labels` is
    /// [`Labels::Refuse`], because routing on a label is what a rename breaks.
    fn resolve_name(
        &self,
        project: &str,
        name: &str,
        reach: Reach,
        labels: Labels,
    ) -> Result<String, Miss> {
        if crate::SeatId::parse(name).is_some() {
            return self.name_of_id(name).ok_or(Miss::Unknown);
        }
        if labels == Labels::Refuse {
            match self.seats_behind_label(name).as_slice() {
                [] => {}
                [id] => {
                    return Err(Miss::Label {
                        name: name.trim().trim_start_matches('@').to_string(),
                        id: id.clone(),
                    });
                }
                ids => return Err(Miss::Ambiguous(ids.to_vec())),
            }
        }
        let collapsed = Self::collapse_bridge(name);
        if collapsed != name && self.is_known(&collapsed) {
            return Ok(collapsed);
        }
        let name: &str = &collapsed;
        // A family with only suffixed members connected is still somebody: they read its mail.
        if self.family_connected(name) {
            return Ok(name.to_string());
        }
        match self.bridge_lookup(name) {
            Some(Lookup::One(full)) => return Ok(full),
            Some(Lookup::Many(c) | Lookup::Group(_, c)) => return Err(Miss::Ambiguous(c)),
            None => {}
        }
        if self.is_known(name) {
            return Ok(name.to_string());
        }
        // `gatekeeper@<label>` is that project's review queue; a bare `gatekeeper` is the sender's.
        if let Some(label) = name.strip_prefix("gatekeeper@") {
            if let Some((key, _)) = self
                .project_label_map()
                .into_iter()
                .find(|(_, l)| l == label)
            {
                return Ok(format!("{GATEKEEPER}{key}"));
            }
        } else if name == "gatekeeper" && !project.is_empty() {
            return Ok(format!("{GATEKEEPER}{project}"));
        }
        // A bridge-shaped name is a bridge or nobody. Resolving `cc@x` on through the member
        // handles is how a dm meant for `cc@serenedb` was stored against a session of another
        // project: the handle, not the name, decided the addressee.
        if chat::bridge_parts(name).is_some() {
            return Err(Miss::Unknown);
        }
        if !chat::BRIDGE_ALIASES.contains(&name) {
            return self.resolve_handle(project, name, reach);
        }
        let mut found: Vec<String> = self
            .external_names()
            .into_iter()
            .filter(|n| n.split('@').next().is_some_and(|h| h == "cc") && n.contains('@'))
            .collect();
        found.sort();
        found.dedup();
        match found.len() {
            1 => Ok(found.remove(0)),
            0 => Err(Miss::Unknown),
            _ => Err(Miss::Ambiguous(found)),
        }
    }

    /// `cc@folder:folder` is the handle `cc@folder`.
    fn collapse_bridge(name: &str) -> String {
        match chat::bridge_parts(name) {
            Some((f, n)) if f == n && name.contains(':') => format!("cc@{f}"),
            _ => name.to_string(),
        }
    }

    /// What a `cc@x` a sender wrote means among the connected bridges: the one whose full handle
    /// it is (unless others share its folder), else the one whose name part it is, else every
    /// bridge of folder `x`, else the several whose name part it is. `None` when no bridge fits.
    fn bridge_lookup(&self, name: &str) -> Option<Lookup> {
        if name.contains(':') {
            return None;
        }
        let x = name.strip_prefix("cc@").filter(|x| !x.is_empty())?;
        let mut bridges: Vec<String> = self
            .external_names()
            .into_iter()
            .filter(|n| chat::bridge_parts(n).is_some())
            .collect();
        bridges.sort();
        bridges.dedup();
        let in_folder: Vec<String> = bridges
            .iter()
            .filter(|b| chat::bridge_parts(b).is_some_and(|(f, _)| f == x))
            .cloned()
            .collect();
        if bridges.iter().any(|b| b == name) && in_folder.len() <= 1 {
            return Some(Lookup::One(name.to_string()));
        }
        let named: Vec<String> = bridges
            .iter()
            .filter(|b| chat::bridge_parts(b).is_some_and(|(_, n)| n == x))
            .cloned()
            .collect();
        if in_folder.len() > 1 {
            return Some(Lookup::Group(x.to_string(), in_folder));
        }
        match (named.len(), in_folder.is_empty()) {
            (1, _) => Some(Lookup::One(named[0].clone())),
            (_, false) => Some(Lookup::Group(x.to_string(), in_folder)),
            (0, true) => None,
            _ => Some(Lookup::Many(named)),
        }
    }

    /// How each connected bridge reads to others: `cc@name` when its name is unique among the
    /// connected bridges, else the qualified `cc@folder:name`.
    fn bridge_display(&self) -> BTreeMap<String, String> {
        // One name, one bridge: a waiter sharing the name is not a second bridge of it.
        let mut bridges: Vec<String> = self.external_names();
        bridges.sort();
        bridges.dedup();
        let mut count: BTreeMap<&str, usize> = BTreeMap::new();
        for b in &bridges {
            if let Some((_, n)) = chat::bridge_parts(b) {
                *count.entry(n).or_default() += 1;
            }
        }
        bridges
            .iter()
            .filter_map(|b| {
                let (f, n) = chat::bridge_parts(b)?;
                let shown = if count[n] == 1 {
                    format!("cc@{n}")
                } else {
                    format!("cc@{f}:{n}")
                };
                Some((b.clone(), shown))
            })
            .collect()
    }

    /// The session id a member handle means: the one in the sender's project if that is
    /// unambiguous, else - for a seat with no project, or a door that may reach the host - the
    /// only one that has it. `Miss` says which ids answer when it is not one name.
    fn resolve_handle(&self, project: &str, name: &str, reach: Reach) -> Result<String, Miss> {
        let want = name.to_lowercase();
        let key = name_key(name);
        let (by_handle, by_title) = {
            let rooms = self.inner.rooms.lock().expect("the rooms lock");
            let mut by_handle: Vec<(String, String)> = Vec::new();
            let mut by_title: Vec<(String, String)> = Vec::new();
            for (p, r) in rooms.iter() {
                for m in r.members().iter().filter(|m| m.group == crate::Group::Top) {
                    let row = (p.clone(), m.session.clone());
                    if !m.handle.is_empty() && m.handle == want {
                        by_handle.push(row);
                    } else if !key.is_empty()
                        && (name_key(&m.handle) == key || name_key(&m.label) == key)
                    {
                        by_title.push(row);
                    }
                }
            }
            (by_handle, by_title)
        };
        // A handle is exact and wins; a title or label is the same name written the way a person
        // reads it, so it counts only when no handle answers.
        let mut found = if by_handle.is_empty() {
            by_title
        } else {
            by_handle
        };
        found.sort();
        found.dedup();
        let here: Vec<&(String, String)> = found.iter().filter(|(p, _)| p == project).collect();
        let mut ids: Vec<String> = found.iter().map(|(_, id)| id.clone()).collect();
        ids.sort();
        ids.dedup();
        match (here.as_slice(), found.as_slice()) {
            ([one], _) => Ok(one.1.clone()),
            (_, []) => Err(Miss::Unknown),
            (_, [_]) if project.is_empty() || reach == Reach::Host => Ok(found[0].1.clone()),
            // A session's dm keeps to its own project's names: a handle that only another
            // project answers to is that session's, and saying so is not the same as sending to
            // it. An ask, an invite and a seat in no project may reach across.
            _ if here.is_empty() && !project.is_empty() && reach == Reach::OwnProject => {
                Err(Miss::Elsewhere(ids))
            }
            _ => Err(Miss::Ambiguous(ids)),
        }
    }

    /// Every named top-level member's handle, by session id.
    fn handles(&self) -> BTreeMap<String, String> {
        let mut out: BTreeMap<String, String> = {
            let rooms = self.inner.rooms.lock().expect("the rooms lock");
            rooms
                .values()
                .flat_map(|r| r.members().to_vec())
                .filter(|m| m.group == crate::Group::Top && !m.handle.is_empty())
                .map(|m| (m.session, m.handle))
                .collect()
        };
        out.extend(self.bridge_display());
        for (id, shown) in self.gatekeeper_display() {
            out.entry(id).or_insert(shown);
        }
        out
    }

    /// Each project's review queue as `gatekeeper@<label>`, the way a bridge reads `cc@<folder>`:
    /// its id is `s-gatekeeper-<project key>`, which says nothing to a reader.
    fn gatekeeper_display(&self) -> BTreeMap<String, String> {
        self.project_label_map()
            .into_iter()
            .map(|(key, label)| (format!("{GATEKEEPER}{key}"), format!("gatekeeper@{label}")))
            .collect()
    }

    /// `message` as a reader sees it: a named session's id in the author, the addressee and the
    /// `@` mentions becomes its handle. The log keeps ids, so a rename changes how old lines read.
    fn shown(
        &self,
        mut message: chat::Message,
        handles: &BTreeMap<String, String>,
    ) -> chat::Message {
        if handles.is_empty() {
            return message;
        }
        let swap = |name: &mut String| {
            if let Some(h) = handles.get(name.as_str()) {
                *name = h.clone();
            }
        };
        swap(&mut message.author);
        if let Some(to) = message.to.as_mut() {
            swap(to);
        }
        for m in chat::mentions(&message.text) {
            if let Some(h) = handles.get(&m) {
                message.text = replace_mention(&message.text, &m, h);
            }
        }
        message
    }

    /// `text` with each short bridge name and each member handle in an `@mention` made the full
    /// name or id, or the refusal for one that fits several.
    fn resolve_mentions(&self, project: &str, text: &str) -> Result<String, crate::Refused> {
        let mut out = text.to_string();
        for m in chat::mentions(text) {
            if let Some(Lookup::Group(folder, _)) = self.bridge_lookup(&m) {
                out = replace_mention(&out, &m, &format!("cc@{folder}:*"));
                continue;
            }
            match self.resolve_name(project, &m, Reach::Host, Labels::Rewrite) {
                Ok(full) if full != m => {
                    out = replace_mention(&out, &m, &full);
                }
                Ok(_) => {}
                // A mention of nobody stays as it was written: the sentence is the sender's, and
                // only a name that fits several is a refusal, because only that one is not. A label
                // is rewritten above, so `Label` is a `Refuse`-only answer and cannot come back here.
                Err(miss @ Miss::Ambiguous(_)) => return Err(miss.refused(&m, self)),
                Err(Miss::Unknown | Miss::Elsewhere(_) | Miss::Label { .. }) => {}
            }
        }
        Ok(out)
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

    /// Keep a message for every member of its room that has no connection - the ones a push cannot
    /// reach. `here` is who has one, so this and the push before it never disagree about a member.
    fn remember_for_absent(&self, message: &chat::Message, here: &BTreeSet<String>) {
        // Who could be told: the room's members, the principal it is addressed to, and whoever it
        // names. The last two matter most - a session that is away is very often exactly the one
        // the message was for, and a room that only counted its connected members would lose it.
        let project = chat::project_of_room(&message.room)
            .unwrap_or("")
            .to_string();
        let mut audience: BTreeMap<String, (Seat, String)> = BTreeMap::new();
        for p in self.roster_rows(&message.room) {
            audience.insert(p.id, (p.kind, p.project));
        }
        let named: Vec<String> = message
            .to
            .iter()
            .cloned()
            .chain(chat::mentions(&message.text))
            .collect();
        // A name is worth keeping for if the room can still deliver to it: somebody who answers now,
        // or a session the room has a cursor for that has simply gone quiet. Without the second
        // half, the message naming a session while it restarts would be the one it never hears.
        let armed: BTreeSet<String> = self
            .inner
            .armed
            .lock()
            .expect("the armed lock")
            .iter()
            .map(|name| self.principal_of(name))
            .collect();
        for who in named {
            if who == message.author || !(self.is_known(&who) || armed.contains(&who)) {
                continue;
            }
            audience
                .entry(who)
                .or_insert((Seat::Session, project.clone()));
        }
        let max_principals = self.inner.principals.load(Ordering::SeqCst);
        // Who is actually kept for, decided BEFORE the inbox lock: `visible` reads the rooms, and a
        // lock held across another lock is how two of these threads find each other.
        let absent: Vec<String> = audience
            .into_iter()
            .filter(|(name, (kind, project))| {
                name != &message.author
                    && !here.contains(name)
                    && self.visible(name, *kind, project, message)
            })
            .map(|(name, _)| name)
            .collect();
        let mut pending = self.inner.pending.lock().expect("the pending lock");
        for name in absent {
            if !pending.contains_key(&name)
                && pending.len() >= max_principals
                && let Some((gone, was)) = pending
                    .iter()
                    .min_by_key(|(_, p)| p.since_ms)
                    .map(|(k, p)| (k.clone(), p.rows.len() + p.dropped as usize))
            {
                pending.remove(&gone);
                eprintln!(
                    "room: forgetting {was} unread message(s) for `{gone}`; the room tracks \
                     {max_principals} sessions"
                );
            }
            let inbox = pending.entry(name.clone()).or_insert_with(|| Pending {
                rows: std::collections::VecDeque::new(),
                since_ms: message.ts,
                dropped: 0,
            });
            inbox.rows.push_back(message.clone());
            while inbox.rows.len() > MAX_PENDING {
                inbox.rows.pop_front();
                inbox.dropped += 1;
                // Said once per member rather than once per message: a flood of drops is one
                // sentence, and the ring still has every one of them for an `inbox` read.
                if inbox.dropped == 1 {
                    eprintln!(
                        "room: `{name}` already has {MAX_PENDING} undelivered message(s); the \
                         oldest are not pushed - `inbox` still has them"
                    );
                }
            }
        }
    }

    /// Tell a connection what its principal missed, once: what names it verbatim and one digest per
    /// room for the rest. The log is the record, so the per-member backlog is dropped here, and a
    /// member seen before is told only what followed its last mark.
    fn deliver_pending(&self, conn: &Conn, session: &str) {
        let id = self.principal_of(session);
        self.inner
            .pending
            .lock()
            .expect("the pending lock")
            .remove(&id);
        // The operator reads the whole log with `read`; a digest is for an agent's context.
        if conn.seat == Seat::Operator {
            return;
        }
        let now = now_ms();
        let (floor, head, rows, asks_to_me) = {
            let chat = self.inner.chat.lock().expect("the chat lock");
            let seen = chat.seen(&id);
            // An external's reader is the channel it writes to, so where that channel stopped is
            // its CURSOR - and a window of wall-clock is not a read position: it is what wrote a
            // dozen old dms to one bridge on every restart. A session's reader is its model, which
            // has taken nothing until it acks, so a mark it never made falls back to the window.
            let floor = match (seen, conn.seat) {
                (Some(seq), _) => Some(seq),
                (None, Seat::External) => Some(chat.cursor(&id)),
                (None, _) => None,
            };
            let rows = chat.missed(
                session,
                floor,
                now.saturating_sub(chat::DIGEST_WINDOW_MS),
                |_| true,
            );
            let asks: Vec<chat::AskView> = chat.asks_of(session);
            (floor, chat.head(), rows, asks)
        };
        // Visibility reads the rooms, so it runs after the chat lock is released.
        let rows: Vec<chat::Message> = rows
            .into_iter()
            .filter(|m| !m.is_queue_line())
            .filter(|m| self.visible(session, conn.seat, &conn.project, m))
            .collect();
        let (addressed, rest): (Vec<_>, Vec<_>) = rows
            .into_iter()
            .partition(|m| m.attends(session, chat::Attention::ToMe));
        let skipped = addressed.len().saturating_sub(MAX_PENDING);
        let handles = self.handles();
        for message in addressed.into_iter().skip(skipped) {
            conn.send(Response::Chat {
                names_you: true,
                message: self.shown(message, &handles),
            });
        }
        let since = match floor {
            Some(s) => format!("#{s}"),
            None => "the last hour".to_string(),
        };
        let mut by_room: BTreeMap<String, Vec<chat::Message>> = BTreeMap::new();
        for m in rest {
            by_room
                .entry(m.room.clone())
                .or_default()
                .push(self.shown(m, &handles));
        }
        let mut notes: Vec<(String, String)> = Vec::new();
        if skipped > 0 {
            notes.push((
                chat::HOST_ROOM.to_string(),
                format!(
                    "{skipped} older message(s) addressed to you were left out, `room inbox` has them"
                ),
            ));
        }
        for (room, rows) in by_room {
            let items = self.open_item_lines(&room);
            let asks: Vec<chat::AskView> = asks_to_me
                .iter()
                .filter(|a| a.room == room)
                .cloned()
                .collect();
            if let Some(text) = chat::summarise(&room, &since, &rows, &items, &asks) {
                notes.push((room, text));
            }
        }
        for (room, text) in notes {
            conn.send(Response::Chat {
                names_you: true,
                message: chat::Message {
                    seq: head,
                    ts: now,
                    room,
                    author: chat::DIGEST_AUTHOR.to_string(),
                    kind: Seat::System,
                    to: Some(session.to_string()),
                    thread: None,
                    refs: Vec::new(),
                    attachments: Vec::new(),
                    text,
                },
            });
        }
        // A session acks the digest when its model takes it; until then a reconnect says it again.
        if conn.seat != Seat::Session {
            let mut chat = self.inner.chat.lock().expect("the chat lock");
            chat.mark_seen(&id, head);
            chat.keep_seen(&id);
        }
    }

    /// The unfinished items of the project a chat room belongs to, one short line each.
    fn open_item_lines(&self, room: &str) -> Vec<String> {
        let key = chat::project_of_room(room).unwrap_or("");
        let rooms = self.inner.rooms.lock().expect("the rooms lock");
        let Some(r) = rooms.get(key) else {
            return Vec::new();
        };
        r.all()
            .into_iter()
            .filter_map(|i| match &i.state {
                crate::item::State::Open => Some(format!("#{} `{}` open", i.id, i.tag)),
                crate::item::State::Claimed { claim } => {
                    Some(format!("#{} `{}` held by {}", i.id, i.tag, claim.by))
                }
                crate::item::State::Abandoned { .. } => {
                    Some(format!("#{} `{}` abandoned", i.id, i.tag))
                }
                _ => None,
            })
            .collect()
    }

    /// May a principal in `seat` and `project` see chat room `room`?
    fn may_see(&self, name: &str, seat: Seat, project: &str, room: &str) -> bool {
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
            (!project.is_empty() && project == key) || self.is_guest(room, name)
        }
    }

    fn is_guest(&self, room: &str, name: &str) -> bool {
        self.inner
            .guests
            .lock()
            .expect("the guests lock")
            .get(room)
            .is_some_and(|g| g.contains(name))
    }

    /// The refusal a seat gets for a room it may not see. A project room that is not its own is said
    /// so with the room it IS in: `unknown_chat_room` would read as a typo it should fix.
    fn room_refusal(project: &str, room: &str) -> crate::Refused {
        match chat::project_of_room(room) {
            Some(_) => crate::Refused::NotYourRoom {
                room: room.to_string(),
                own: if project.is_empty() {
                    String::new()
                } else {
                    chat::project_room(project)
                },
            },
            None => crate::Refused::UnknownChatRoom {
                room: room.to_string(),
            },
        }
    }

    /// The project room `named` means - `project/<key>` or a room's label - when it exists.
    fn project_room_named(&self, named: &str) -> Option<String> {
        let named = named.trim();
        let keys: Vec<String> = self
            .inner
            .rooms
            .lock()
            .expect("the rooms lock")
            .keys()
            .filter(|k| !k.is_empty())
            .cloned()
            .collect();
        if let Some(key) = chat::project_of_room(named) {
            return keys.iter().any(|k| k == key).then(|| named.to_string());
        }
        self.project_label_map()
            .into_iter()
            .find(|(_, label)| label == named)
            .map(|(key, _)| chat::project_room(&key))
    }

    /// `room` as named, with empty and `project` meaning the caller's own project room.
    fn own_room_or(&self, room: &str, project: &str) -> String {
        match room.trim() {
            "" | "project" if !project.is_empty() => chat::project_room(project),
            r => r.to_string(),
        }
    }

    /// Invite `who` into `room` or take it out, in the store and in the copy `may_see` reads.
    fn set_guest(&self, room: &str, who: &str, on: bool) {
        self.inner
            .chat
            .lock()
            .expect("the chat lock")
            .set_guest(room, who, on);
        let mut g = self.inner.guests.lock().expect("the guests lock");
        if on {
            g.entry(room.to_string())
                .or_default()
                .insert(who.to_string());
        } else if let Some(names) = g.get_mut(room) {
            names.remove(who);
            if names.is_empty() {
                g.remove(room);
            }
        }
    }

    /// Is this message one `name` (in `seat`, `project`) may read?
    fn visible(&self, name: &str, seat: Seat, project: &str, m: &chat::Message) -> bool {
        if queue_seat_project(name).is_some() && m.author != name {
            return self.queue_seat_hears(name, m);
        }
        if m.room == chat::DM_ROOM {
            return m.author == name
                || m.to.as_deref() == Some(name)
                || self
                    .family(name)
                    .is_some_and(|f| m.to.as_deref() == Some(&f));
        }
        self.may_see(name, seat, project, &m.room)
    }

    /// A queue seat hears no broadcast and no `@` mention, only the one-to-one doors: a dm or an
    /// ask. From another project only what [`Server::queue_seat_gate`] let through.
    fn queue_seat_hears(&self, name: &str, m: &chat::Message) -> bool {
        if m.to.as_deref() != Some(name) {
            return false;
        }
        if m.kind == Seat::Operator {
            return true;
        }
        let asked = m.refs.iter().any(|r| r.kind == "ask");
        if m.room != chat::DM_ROOM && !asked {
            return false;
        }
        asked
            || self.queue_seat_kin(name, &m.author)
            || m.say_kind() == crate::SayKind::Decision
            || m.thread.is_some()
            || m.refs.iter().any(|r| r.kind == chat::RELAYS)
    }

    /// The refs a dm to `to` is stored with, or the refusal when `to` is a queue seat that takes no
    /// such line from `name`. Another project may send a `decision` or an `ask`, or relay an
    /// operator's message it names by `thread`; plain chatter stays out.
    #[allow(clippy::too_many_arguments)]
    fn queue_seat_gate(
        &self,
        to: &str,
        name: &str,
        seat: Seat,
        project: &str,
        kind: crate::SayKind,
        thread: Option<u64>,
        mut refs: Vec<chat::Ref>,
    ) -> Result<Vec<chat::Ref>, crate::Refused> {
        refs.retain(|r| r.kind != chat::RELAYS);
        if queue_seat_project(to).is_none()
            || seat == Seat::Operator
            || self.queue_seat_kin(to, name)
            || matches!(kind, crate::SayKind::Decision | crate::SayKind::Ask)
        {
            return Ok(refs);
        }
        match thread.filter(|t| self.operator_said(name, seat, project, *t)) {
            Some(t) if refs.len() < chat::MAX_REFS => {
                refs.push(chat::Ref {
                    kind: chat::RELAYS.to_string(),
                    id: t.to_string(),
                });
                Ok(refs)
            }
            _ => Err(crate::Refused::QueueSeatOwnProject {
                session: to.to_string(),
            }),
        }
    }

    /// Is `seq` the operator's message, one `name` can read? Acting on it is acting for them.
    fn operator_said(&self, name: &str, seat: Seat, project: &str, seq: u64) -> bool {
        let found: Option<chat::Message> = {
            let chat = self.inner.chat.lock().expect("the chat lock");
            chat.thread_root(seq, |_| true)
        };
        found.is_some_and(|m| m.kind == Seat::Operator && self.visible(name, seat, project, &m))
    }

    /// The consent a prompt into a session this seat does not hold needs: none within its own
    /// project, the operator's word across projects - theirs, or their message cited by seq.
    fn may_prompt(
        &self,
        project: &str,
        seat: Seat,
        name: &str,
        to: &str,
        hint: &str,
        because: Option<u64>,
    ) -> Result<String, crate::Refused> {
        let to = to.trim().trim_start_matches('@');
        let session = match self.resolve_name(project, to, Reach::OwnProject, Labels::Refuse) {
            Ok(s) => s,
            // A label is answered with the id it means, which is the only useful thing to say here:
            // the session it names is one this prompt would reach.
            Err(miss @ Miss::Label { .. }) => return Err(miss.refused(to, self)),
            Err(_) => to.to_string(),
        };
        if session.is_empty() || session == name {
            return Err(crate::Refused::UnknownAddressee {
                name: session,
                known: Vec::new(),
            });
        }
        let known = self.seat_and_project_of(&session).1;
        let target = match queue_seat_project(&session) {
            Some(key) => key.to_string(),
            None if !known.is_empty() => known,
            None => hint.trim().to_string(),
        };
        if seat == Seat::Operator || (!target.is_empty() && target == project) {
            return Ok(session);
        }
        match because {
            None => Err(crate::Refused::PromptNeedsOperator { session }),
            Some(b) if self.operator_said(name, seat, project, b) => Ok(session),
            Some(b) => Err(crate::Refused::PromptCitesNotOperator {
                session,
                because: b,
            }),
        }
    }

    /// The box's queue seats: in no room, so no roster lists them, and this is how one is found.
    /// Listed is not seated - room talk still does not wake them.
    fn queue_seats(&self) -> Vec<chat::Presence> {
        let mut rows: BTreeMap<String, chat::Presence> = BTreeMap::new();
        let seat_row = |id: &str, connected: bool| chat::Presence {
            name: id.to_string(),
            id: id.to_string(),
            seat_id: crate::SeatId::of_session(&self.seat_host(), id).to_string(),
            kind: Seat::Session,
            connected,
            project: queue_seat_project(id).unwrap_or_default().to_string(),
            label: "queue seat".to_string(),
            seat: None,
        };
        {
            let rooms = self.inner.rooms.lock().expect("the rooms lock");
            for m in rooms.values().flat_map(|r| r.members().to_vec()) {
                if queue_seat_project(&m.session).is_some() && !m.session.contains("-sub-") {
                    rows.entry(m.session.clone())
                        .or_insert_with(|| seat_row(&m.session, false));
                }
            }
        }
        for c in self.conn_list() {
            let id = &c.who.session;
            if queue_seat_project(id).is_some() && !id.contains("-sub-") {
                rows.entry(id.clone())
                    .or_insert_with(|| seat_row(id, true))
                    .connected = true;
            }
        }
        let shown = self.gatekeeper_display();
        let now = now_ms();
        rows.into_values()
            .map(|mut r| {
                if let Some(g) = shown.get(&r.id) {
                    r.name = g.clone();
                }
                r.seat = Some(self.seat_view(&r.id, r.connected, now));
                r
            })
            .collect()
    }

    /// Is `other` a session of the project the queue seat `name` serves?
    fn queue_seat_kin(&self, name: &str, other: &str) -> bool {
        let Some(key) = queue_seat_project(name) else {
            return false;
        };
        other == chat::OPERATOR || self.seat_and_project_of(other).1 == key
    }

    /// This box's half of every seat id it hands out: the pinned answer, or what a test set. A
    /// session that sends no id of its own is minted one from this, which is why it is a fact of
    /// the box rather than of a connection.
    pub fn seat_host(&self) -> String {
        self.inner
            .seat_host
            .lock()
            .expect("the seat host lock")
            .clone()
    }

    /// Pin the seat host - a test that must not read this machine's name, and the one place a
    /// coordinator's own half of an id can be said.
    pub fn with_seat_host(&self, host: impl Into<String>) -> &Self {
        *self.inner.seat_host.lock().expect("the seat host lock") = host.into();
        self
    }

    /// The seat id a name is a connection of: what the roster draws beside it. `None` for a name
    /// no connection speaks as - a member with no connection is minted one by [`Server::live_nicks`].
    pub fn seat_of(&self, name: &str) -> Option<String> {
        self.conn_list()
            .into_iter()
            .find(|c| c.who.session == name)
            .map(|c| c.seat(&self.seat_host()))
    }

    /// The key a seat's own durable state is filed under: its seat id, which outlives a rename.
    /// A session's is derivable from its name; a bridge's is what its `Hello` said, remembered by
    /// the name it spoke as, because a name on its own says nothing about the seat behind it.
    pub fn principal_of(&self, name: &str) -> String {
        if let Some(id) = self
            .inner
            .ids_of
            .lock()
            .expect("the ids lock")
            .get(name)
            .filter(|id| !id.is_empty())
        {
            return id.clone();
        }
        if name.starts_with("s-") {
            return crate::SeatId::of_session(&self.seat_host(), name).to_string();
        }
        name.to_string()
    }

    /// Do two names mean one seat? An ask, a wait and a refusal belong to the seat, and a seat
    /// keeps its id across a rename, so the answer is the ids rather than the spellings.
    pub fn same_seat(&self, a: &str, b: &str) -> bool {
        a == b || (!a.is_empty() && !b.is_empty() && self.principal_of(a) == self.principal_of(b))
    }

    /// The label the room holds for a seat: the nick it was given, else the name it is drawn by.
    pub fn nick_of(&self, seat: &str) -> Option<String> {
        self.live_nicks().remove(seat).map(|(nick, _)| nick)
    }

    /// The label the room draws `author` by, when it is a live seat that is drawn by something
    /// other than the name it speaks as. `None` for the operator, the harness and any name no
    /// seat holds: those are drawn exactly as they post, so there is nothing to record.
    fn label_of(&self, author: &str) -> Option<String> {
        let seat = self.seat_of(author).filter(|s| !s.is_empty())?;
        self.nick_of(&seat).filter(|label| label != author)
    }

    /// Every live seat's label, by seat id, with the name it is drawn by when it has no nick: a
    /// connection's own, and a live roster member's handle (the slug of its title). A seat with no
    /// connection is a seat with a name - the tree lists it - so both halves are here.
    fn live_nicks(&self) -> BTreeMap<String, (String, String)> {
        let host = self.seat_host();
        let mut out: BTreeMap<String, (String, String)> = BTreeMap::new();
        // A member's own handle first: it is the label a session's title gives it, and a session's
        // connection name is an id rather than a name anybody reads.
        {
            let rooms = self.inner.rooms.lock().expect("the rooms lock");
            for r in rooms.values() {
                for m in r
                    .members()
                    .iter()
                    .filter(|m| m.group == crate::Group::Top && m.live)
                {
                    let seat = crate::SeatId::of_session(&host, &m.session).to_string();
                    let drawn = if m.handle.is_empty() {
                        m.session.clone()
                    } else {
                        m.handle.clone()
                    };
                    out.insert(seat, (drawn, m.session.clone()));
                }
            }
        }
        for c in self.conn_list() {
            let seat = c.seat(&host);
            if seat.is_empty() {
                continue;
            }
            let drawn = if c.nick.is_empty() {
                c.who.session.clone()
            } else {
                c.nick.clone()
            };
            match out.entry(seat) {
                // A connection that brought its own label keeps it; another is drawn by the
                // member's handle, which is what the tree calls it.
                std::collections::btree_map::Entry::Occupied(mut e) => {
                    if !c.nick.is_empty() {
                        e.insert((drawn, c.who.session.clone()));
                    }
                }
                std::collections::btree_map::Entry::Vacant(v) => {
                    v.insert((drawn, c.who.session.clone()));
                }
            }
        }
        // A nick the room was told beats the one derived from a title: it is the operator's word.
        for (seat, nick) in self.inner.nicks.lock().expect("the nicks lock").iter() {
            if let Some(row) = out.get_mut(seat) {
                row.0 = nick.clone();
            }
        }
        out
    }

    /// Who is drawn as `nick` among the live seats, other than `except`: the holder's own name, so
    /// a refusal can say whose the label is. A bare name (a handle, a session id) is held by
    /// whoever speaks as it, so this answers for both spellings a person types.
    fn nick_holder(&self, nick: &str, except: &str) -> Option<String> {
        self.live_nicks()
            .into_iter()
            .filter(|(seat, _)| seat != except)
            .find(|(_, (label, name))| label == nick || name == nick)
            .map(|(_, (_, name))| name)
    }

    /// The rooms a seat is in: the host room and its own project's, every room for the operator (who
    /// is in all), and the host room alone for a seat with no project or one that only serves it.
    fn rooms_of(&self, seat: Seat, project: &str) -> Vec<String> {
        if seat == Seat::Operator {
            let mut all = vec![chat::HOST_ROOM.to_string()];
            all.extend(self.projects().into_iter().map(|p| chat::project_room(&p)));
            return all;
        }
        let mut rooms = vec![chat::HOST_ROOM.to_string()];
        if !project.is_empty() {
            rooms.push(chat::project_room(project));
        }
        rooms
    }

    /// Take `nick` for the caller's seat, or say who holds it. The seat keeps its id either way: a
    /// nick is a label, and the one thing a rename must not do is make the seat somebody else.
    fn take_nick(
        &self,
        project: &str,
        seat: Seat,
        name: &str,
        asked: &str,
        nick: &str,
        now: u64,
    ) -> Response {
        let refuse = |why: crate::Refused| Response::Refused { id: None, why };
        let mine = self.seat_of(name).unwrap_or_else(|| name.to_string());
        let asked = asked.trim();
        if !asked.is_empty() && asked != mine && asked != name {
            return refuse(crate::Refused::NotYourSeat {
                id: asked.to_string(),
                yours: mine,
            });
        }
        let nick = nick.trim().trim_start_matches('@').to_lowercase();
        if !chat::valid_nick(&nick) {
            return refuse(crate::Refused::BadNick { nick });
        }
        if let Some(held_by) = self.nick_holder(&nick, &mine) {
            let next = chat::next_nick(&nick, |c| self.nick_holder(c, &mine).is_some());
            return refuse(crate::Refused::NickTaken {
                nick,
                held_by,
                next,
            });
        }
        let previous = self
            .inner
            .nicks
            .lock()
            .expect("the nicks lock")
            .get(&mine)
            .cloned()
            .unwrap_or_default();
        if previous == nick {
            return Response::Nicked {
                id: mine,
                nick,
                previous,
            };
        }
        self.inner
            .nicks
            .lock()
            .expect("the nicks lock")
            .insert(mine.clone(), nick.clone());
        // Said once in each room the seat is in, because a reader who saw the old label has no
        // other way to learn it moved: the log is what history is read from.
        for room in self.rooms_of(seat, project) {
            let text = if previous.is_empty() {
                format!("{name} is now {nick}")
            } else {
                format!("{name} is now {nick}, and was {previous}")
            };
            let _ = self.append_and_push(
                &format!("nick:{mine}:{nick}:{now}"),
                &room,
                chat::DIGEST_AUTHOR,
                Seat::System,
                None,
                None,
                Vec::new(),
                Vec::new(),
                &text,
                now,
            );
        }
        Response::Nicked {
            id: mine,
            nick,
            previous,
        }
    }

    /// Does anybody answer to `name` - a seat of this box, or an id a box this one knows of
    /// announced, whose line is then shipped or kept? A session the room has merely seen before is
    /// NOT here - that would accept a message to nobody - and [`Server::remember_for_absent`] is what
    /// keeps such a session's chat for it.
    fn is_known(&self, name: &str) -> bool {
        if name == chat::OPERATOR
            || self
                .inner
                .externals
                .lock()
                .expect("externals")
                .contains(name)
            || self
                .conn_list()
                .iter()
                .any(|c| c.who.session == name || c.family.as_deref() == Some(name))
        {
            return true;
        }
        // A seat a linked box holds answers to its id, and a message to it goes over that link; an
        // away box's seat answers too, and its line waits in this box's queue for the link.
        if crate::SeatId::parse(name).is_some_and(|id| self.peer_holds(&id)) {
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

    /// Every project room's label, told apart from its namesakes.
    fn project_label_map(&self) -> BTreeMap<String, String> {
        let rows: Vec<(String, String)> = self
            .inner
            .rooms
            .lock()
            .expect("the rooms lock")
            .iter()
            .filter(|(k, _)| !k.is_empty())
            .map(|(k, r)| (k.clone(), r.root().to_string()))
            .collect();
        crate::project_labels(&rows, &std::env::var("HOME").unwrap_or_default())
    }

    /// Who is in a chat room: its tree's top-level sessions, whoever is connected to it, and the
    /// wide seats, which are in every room. The host room is every top-level seat on this host -
    /// every project's - so its rows carry the project each one is in; a project room is its own.
    pub fn roster_rows(&self, room: &str) -> Vec<chat::Presence> {
        let key = chat::project_of_room(room);
        let host_room = room == chat::HOST_ROOM;
        let host = self.seat_host();
        let mut rows: BTreeMap<String, chat::Presence> = BTreeMap::new();
        {
            let rooms = self.inner.rooms.lock().expect("the rooms lock");
            for (project, r) in rooms.iter() {
                if !host_room && project.as_str() != key.unwrap_or(crate::DEFAULT_PROJECT) {
                    continue;
                }
                for m in r.members().iter().filter(|m| m.group == crate::Group::Top) {
                    rows.insert(
                        m.session.clone(),
                        chat::Presence {
                            name: if m.handle.is_empty() {
                                m.session.clone()
                            } else {
                                m.handle.clone()
                            },
                            id: m.session.clone(),
                            seat_id: crate::SeatId::of_session(&host, &m.session).to_string(),
                            kind: Seat::Session,
                            connected: false,
                            project: project.clone(),
                            label: m.label.clone(),
                            seat: None,
                        },
                    );
                }
            }
        }
        let guests: BTreeSet<String> = self
            .inner
            .guests
            .lock()
            .expect("the guests lock")
            .get(room)
            .cloned()
            .unwrap_or_default();
        for g in &guests {
            rows.entry(g.clone()).or_insert(chat::Presence {
                name: g.clone(),
                id: g.clone(),
                seat_id: crate::SeatId::of_session(&host, g).to_string(),
                kind: Seat::Session,
                connected: false,
                project: key.unwrap_or("").to_string(),
                label: "guest".to_string(),
                seat: None,
            });
        }
        for c in self.conn_list() {
            let name = c.who.session.clone();
            // A queue seat is one project's queue and not a member of the host room. Every other
            // seat is in the host room and in its own project's room; the operator is in all.
            let in_room = c.seat.is_wide()
                || if host_room {
                    queue_seat_project(&name).is_none()
                } else {
                    key == Some(c.project.as_str())
                }
                || guests.contains(&name);
            if !in_room {
                continue;
            }
            let row = rows.entry(name.clone()).or_insert(chat::Presence {
                id: name.clone(),
                name,
                seat_id: c.seat(&host),
                kind: c.seat,
                connected: false,
                project: c.project.clone(),
                label: String::new(),
                seat: None,
            });
            row.connected = true;
            row.kind = c.seat;
            // The row is drawn by the label the seat holds - a nick, not the name it speaks as.
            if !c.nick.is_empty() {
                row.name = c.nick.clone();
            } else if let Some(n) = self
                .inner
                .nicks
                .lock()
                .expect("the nicks lock")
                .get(&row.seat_id)
            {
                row.name = n.clone();
            }
        }
        let display = self.bridge_display();
        let gatekeepers = self.gatekeeper_display();
        let nicks = self.inner.nicks.lock().expect("the nicks lock").clone();
        // A linked box's seats are in this room's roster and are not here: their own box holds
        // them, and this one draws the id it would send to. A box that is away keeps its seats in
        // the list, away - a roster that dropped them would be a room that forgot who it shares.
        if let Some(peer) = self.peer() {
            for (seat, live) in peer.seats_with_state() {
                if seat.host() == host {
                    continue;
                }
                let here = peer.project_of(seat.identity()).map(|p| p.key);
                let mine = key.map(str::to_string).or(here.clone());
                if !host_room && mine.as_deref() != here.as_deref() {
                    continue;
                }
                rows.insert(
                    seat.id.clone(),
                    chat::Presence {
                        // The id is the address a send takes; a `nick@host` is what a reader sees.
                        name: chat::drawn(
                            crate::SeatId::parse(&seat.id),
                            Some(&seat.nick),
                            &seat.id,
                        ),
                        id: seat.id.clone(),
                        seat_id: seat.id.clone(),
                        kind: Seat::External,
                        connected: live,
                        project: here.unwrap_or_default(),
                        label: seat.line(),
                        seat: None,
                    },
                );
            }
        }
        for row in rows.values_mut() {
            // A label the seat was given is what it is drawn by, whatever its handle derives to:
            // the room was told, and a reader addresses nothing with it.
            if let Some(n) = nicks.get(&row.seat_id) {
                row.name = n.clone();
                continue;
            }
            if row.kind == Seat::External
                && let Some(d) = display.get(&row.id)
            {
                row.name = d.clone();
            } else if row.name == row.id
                && let Some(g) = gatekeepers.get(&row.id)
            {
                row.name = g.clone();
            }
        }
        rows.into_values().collect()
    }

    /// The roster with each session's derived presence. Takes the chat and rooms locks, so a caller
    /// holding either reads [`Server::roster_rows`] instead.
    pub fn roster_of(&self, room: &str) -> Vec<chat::Presence> {
        let now = now_ms();
        let mut rows = self.roster_rows(room);
        for row in rows.iter_mut().filter(|r| r.kind == Seat::Session) {
            row.seat = Some(self.seat_view(&row.id, row.connected, now));
        }
        rows
    }

    /// One session's derived presence: its daemon's facts joined with what only the room sees, the
    /// connection and the waits it holds here (open asks it made, queue waits it declared).
    pub fn seat_view(&self, session: &str, connected: bool, now: u64) -> crate::seat::SeatView {
        let (facts, heard) = match self
            .inner
            .seats
            .lock()
            .expect("the seats lock")
            .get(session)
        {
            Some((f, at)) => (Some(f.clone()), Some(*at)),
            None => (None, None),
        };
        use crate::seat::{Pending, PendingKind};
        let mut waits = Vec::new();
        let mut pending = Vec::new();
        for a in self
            .inner
            .chat
            .lock()
            .expect("the chat lock")
            .asks_by(session)
            .into_iter()
            .filter(|a| a.open())
        {
            waits.push(format!("ask #{} of {}", a.seq, a.target));
            pending.push(Pending::new(
                PendingKind::Question,
                session,
                a.seq.to_string(),
            ));
        }
        for project in self.projects() {
            let held = self.with_room(&project, |r| r.holding(session, now));
            for h in held {
                if let Some(w) = h.waits_on {
                    waits.push(format!("item #{} ({}): {}", h.item, h.tag, w.text));
                    pending.push(Pending::new(
                        PendingKind::Other,
                        session,
                        format!("i{}", h.item),
                    ));
                }
            }
        }
        let side = crate::seat::RoomSide {
            connected,
            gone_since_ms: self.gone_since(session),
            heard_ms: heard,
            waits,
            pending,
        };
        let mut view = crate::seat::derive(facts.as_ref(), &side, now);
        view.owns = self
            .inner
            .claims
            .lock()
            .expect("the claims lock")
            .values()
            .flat_map(|c| c.live_of(session, now))
            .map(|c| c.line(now))
            .collect();
        view
    }

    // ===== Ownership =====

    /// Run `f` on one project's claims, then keep what it changed in the items store.
    fn with_claims<R>(
        &self,
        project: &str,
        f: impl FnOnce(&mut crate::own::Claims) -> (R, Vec<crate::own::Claim>),
    ) -> R {
        let (out, changed) = {
            let mut all = self.inner.claims.lock().expect("the claims lock");
            f(all.entry(project.to_string()).or_default())
        };
        if !changed.is_empty()
            && let Some(store) = self
                .inner
                .items
                .lock()
                .expect("the items store lock")
                .clone()
        {
            let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
            for c in changed {
                store.put_claim(project, c);
            }
        }
        out
    }

    /// A seat's own work renews its claims, and its being back clears a stopped-owner flag.
    /// Returns its live scopes.
    fn renew_claims(
        &self,
        project: &str,
        session: &str,
        facts: &crate::seat::SeatFacts,
        now: u64,
    ) -> Vec<String> {
        let edits: Vec<(String, u64)> = facts
            .edits
            .iter()
            .map(|e| (e.path.clone(), e.at_ms))
            .collect();
        self.with_claims(project, |claims| {
            let mut changed = claims.renew(session, &edits, &facts.working.row, now);
            let flagged: Vec<u64> = claims
                .live_of(session, now)
                .iter()
                .filter(|c| c.flagged)
                .map(|c| c.id)
                .collect();
            changed.extend(flagged.into_iter().filter_map(|id| claims.flag(id, false)));
            let owns = claims
                .live_of(session, now)
                .iter()
                .map(|c| c.scope.clone())
                .collect();
            (owns, changed)
        })
    }

    /// `own`, `hand`, `accept`, `release`; `None` for any other frame.
    fn ownership_frame(
        &self,
        project: &str,
        seat: Seat,
        name: &str,
        frame: &Request,
        now: u64,
    ) -> Option<Response> {
        let refuse = |id: &str, why: String| Response::Refused {
            id: Some(id.to_string()),
            why: crate::Refused::Ownership { why },
        };
        let session_only = |id: &str| {
            (seat != Seat::Session).then(|| {
                refuse(
                    id,
                    "only a session owns a scope: a claim is renewed by the owner's own edits"
                        .into(),
                )
            })
        };
        let owned = |id: &str, claim: crate::own::Claim, said: String| Response::Owned {
            id: id.to_string(),
            claim,
            said,
        };
        Some(match frame {
            Request::Own {
                id,
                scope,
                ttl_ms,
                thread,
            } => {
                if let Some(r) = session_only(id) {
                    return Some(r);
                }
                let got = self.with_claims(project, |c| match c.own(name, scope, *ttl_ms, now) {
                    Ok(claim) => (Ok(claim.clone()), vec![claim]),
                    Err(why) => (Err(why), Vec::new()),
                });
                match got {
                    Ok(claim) => {
                        if let Some(seq) = thread {
                            self.note_act(*seq, crate::signal::Act::Claim, name);
                        }
                        let said = format!(
                            "you own `{}` for {}, renewed by your edits in it",
                            claim.scope,
                            crate::item::duration(claim.ttl_ms)
                        );
                        owned(id, claim, said)
                    }
                    Err(why) => refuse(id, why),
                }
            }
            Request::Hand {
                id,
                scope,
                to,
                note,
            } => {
                if let Some(r) = session_only(id) {
                    return Some(r);
                }
                let asked = to.trim().trim_start_matches('@');
                // A claim is handed within the project, like a dm's `to`.
                let target =
                    match self.resolve_name(project, asked, Reach::OwnProject, Labels::Refuse) {
                        Ok(t) => t,
                        Err(miss) => {
                            return Some(Response::Refused {
                                id: Some(id.clone()),
                                why: miss.refused(asked, self),
                            });
                        }
                    };
                let got =
                    self.with_claims(project, |c| match c.hand(name, scope, &target, note, now) {
                        Ok(claim) => (Ok(claim.clone()), vec![claim]),
                        Err(why) => (Err(why), Vec::new()),
                    });
                let claim = match got {
                    Ok(claim) => claim,
                    Err(why) => return Some(refuse(id, why)),
                };
                let mut text = format!(
                    "`{name}` hands you `{}`; `room accept` it to own it, `release` to decline",
                    claim.scope
                );
                if !note.trim().is_empty() {
                    text.push_str(&format!(": {}", note.trim()));
                }
                let told = self.append_and_push(
                    &format!("own:{}:hand:{now}", claim.id),
                    &Self::default_chat_room(Seat::Session, project),
                    "room",
                    Seat::System,
                    Some(target.clone()),
                    None,
                    Vec::new(),
                    Vec::new(),
                    &text,
                    now,
                );
                let claim = match told {
                    Response::Said { message, .. } => {
                        self.with_claims(project, |c| match c.hand_told(claim.id, message.seq) {
                            Some(k) => (k.clone(), vec![k]),
                            None => (claim, Vec::new()),
                        })
                    }
                    _ => claim,
                };
                let said = format!(
                    "`{}` is offered to `{target}`; it stays yours until they accept",
                    claim.scope
                );
                owned(id, claim, said)
            }
            Request::Accept { id, scope } => {
                if let Some(r) = session_only(id) {
                    return Some(r);
                }
                let got = self.with_claims(project, |c| match c.accept(name, scope, now) {
                    Ok((claim, was, seq)) => (Ok((claim.clone(), was, seq)), vec![claim]),
                    Err(why) => (Err(why), Vec::new()),
                });
                match got {
                    Ok((claim, was, seq)) => {
                        if let Some(seq) = seq {
                            self.note_act(seq, crate::signal::Act::Claim, name);
                        }
                        let said = format!("you own `{}`, handed over by `{was}`", claim.scope);
                        owned(id, claim, said)
                    }
                    Err(why) => refuse(id, why),
                }
            }
            Request::Release { id, scope } => {
                // The operator's seat is in no project: it releases wherever the scope is held.
                let project = if seat.is_wide() {
                    let want = crate::own::normalise(scope);
                    self.inner
                        .claims
                        .lock()
                        .expect("the claims lock")
                        .iter()
                        .find(|(_, c)| c.live(now).iter().any(|k| k.scope == want))
                        .map(|(p, _)| p.clone())
                        .unwrap_or_else(|| project.to_string())
                } else {
                    project.to_string()
                };
                let project = project.as_str();
                let got = self.with_claims(project, |c| {
                    match c.release(name, scope, seat.is_wide(), now) {
                        Ok(claim) => (Ok(claim.clone()), vec![claim]),
                        Err(why) => (Err(why), Vec::new()),
                    }
                });
                match got {
                    Ok(claim) if claim.ended.is_none() => {
                        let said = format!("you declined `{}`", claim.scope);
                        owned(id, claim, said)
                    }
                    Ok(claim) => {
                        let said = format!("`{}` is nobody's now", claim.scope);
                        owned(id, claim, said)
                    }
                    Err(why) => refuse(id, why),
                }
            }
            _ => return None,
        })
    }

    /// The claims' clock: end the ones past their TTL, and tell the operator once about a live
    /// claim whose owner has been gone past the ask grace, so a stopped owner's scope is not orphaned.
    fn sweep_claims(&self, now: u64) {
        let here: BTreeSet<String> = self
            .conn_list()
            .iter()
            .map(|c| c.who.session.clone())
            .collect();
        let projects: Vec<String> = self
            .inner
            .claims
            .lock()
            .expect("the claims lock")
            .keys()
            .cloned()
            .collect();
        let grace = self.ask_grace_ms();
        for project in projects {
            let owners: Vec<String> = self.with_claims(&project, |c| {
                let owners = c
                    .live(now)
                    .iter()
                    .map(|k| k.owner.clone())
                    .filter(|o| !here.contains(o))
                    .collect();
                (owners, c.sweep(now))
            });
            if !owners.is_empty() {
                let mut gone = self.inner.gone_since.lock().expect("the gone-since lock");
                for o in &owners {
                    gone.entry(o.clone()).or_insert(now);
                }
            }
            let due: Vec<String> = owners
                .into_iter()
                .filter(|o| {
                    self.gone_since(o)
                        .is_some_and(|g| now.saturating_sub(g) >= grace)
                })
                .collect();
            let flagged = self.with_claims(&project, |c| {
                let ids: Vec<u64> = c
                    .live(now)
                    .iter()
                    .filter(|k| !k.flagged && due.contains(&k.owner))
                    .map(|k| k.id)
                    .collect();
                let moved: Vec<crate::own::Claim> =
                    ids.into_iter().filter_map(|id| c.flag(id, true)).collect();
                (moved.clone(), moved)
            });
            for claim in flagged {
                let text = format!(
                    "`{}` stopped and still owns `{}` ({} left): restart it, or `release` the \
                     scope from the operator's seat",
                    claim.owner,
                    claim.scope,
                    crate::item::duration(claim.left_ms(now))
                );
                let _ = self.append_and_push(
                    &format!("own:{}:stopped:{now}", claim.id),
                    &Self::default_chat_room(Seat::Session, &project),
                    "room",
                    Seat::System,
                    Some(chat::OPERATOR.to_string()),
                    None,
                    Vec::new(),
                    Vec::new(),
                    &text,
                    now,
                );
            }
        }
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
                // And the rooms this session was invited into.
                for (room, names) in self.inner.guests.lock().expect("the guests lock").iter() {
                    if names.contains(name) && !ids.contains(room) {
                        ids.push(room.clone());
                    }
                }
                let labels = self.project_label_map();
                let chat = self.inner.chat.lock().expect("the chat lock");
                let rows = ids
                    .into_iter()
                    .map(|id| {
                        let key = chat::project_of_room(&id).unwrap_or("").to_string();
                        chat::RoomInfo {
                            label: labels.get(&key).cloned().unwrap_or_default(),
                            kind: if key.is_empty() { "host" } else { "project" }.into(),
                            members: self.roster_rows(&id).len() as u64,
                            last_seq: chat.last_in(&id),
                            unread: chat.unread(&self.principal_of(name), name, &id, |_| true),
                            archived_ms: chat.archived_ms(&id),
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
                attachments,
                kind,
            } => self.route_say(
                project,
                seat,
                name,
                id,
                room,
                text,
                to.as_deref(),
                *thread,
                refs,
                attachments,
                *kind,
                now,
            ),
            Request::Dm {
                id,
                to,
                text,
                thread,
                attachments,
            } => {
                if seat != Seat::Operator && attachments.is_empty() && chat::is_bare_ack(text) {
                    self.note_null_post(name, seat);
                    return refuse(Some(id), crate::Refused::NullPost);
                }
                let to = to.trim().trim_start_matches('@').to_string();
                let to = match self.resolve_name(project, &to, Reach::OwnProject, Labels::Refuse) {
                    Ok(t) => t,
                    Err(miss) => return refuse(Some(id), miss.refused(&to, self)),
                };
                let text = &match self.resolve_mentions(project, text) {
                    Ok(t) => t,
                    Err(why) => return refuse(Some(id), why),
                };
                if to.is_empty() || to == name || !self.is_known(&to) {
                    return refuse(
                        Some(id),
                        crate::Refused::UnknownAddressee {
                            known: self.closest(&to),
                            name: to,
                        },
                    );
                }
                // A dm to another PROJECT asks that seat first - the same rule `say ... to` keeps,
                // because a second door to one addressee is how the id form crossed unasked.
                if let Some(asked) = self.dm_needs_consent(project, seat, name, id, "", &to, now) {
                    return asked;
                }
                let refs = match self.queue_seat_gate(
                    &to,
                    name,
                    seat,
                    project,
                    crate::SayKind::Post,
                    *thread,
                    Vec::new(),
                ) {
                    Ok(r) => r,
                    Err(why) => return refuse(Some(id), why),
                };
                let attachments = match self.attach_blobs(attachments) {
                    Ok(a) => a,
                    Err(why) => return refuse(Some(id), why),
                };
                // A dm hangs only from a dm. One answering a public message is still private: it
                // goes to the dm channel and names the thread it answers, so the reader can find it.
                let mut text = text.clone();
                let mut thread = *thread;
                if let Some(root) = thread.and_then(|t| {
                    let found: Vec<chat::Message> = {
                        let chat = self.inner.chat.lock().expect("the chat lock");
                        chat.thread_root(t, |_| true).into_iter().collect()
                    };
                    found
                        .into_iter()
                        .next()
                        .filter(|m| self.visible(name, seat, project, m))
                }) {
                    if root.room != chat::DM_ROOM {
                        text = Self::linked_to(&root, &text);
                        thread = None;
                    }
                } else if let Some(t) = thread {
                    return refuse(Some(id), crate::Refused::UnknownThread { thread: t });
                }
                let text = &text;
                let thread = &thread;
                self.say_and_push(
                    id,
                    chat::DM_ROOM,
                    name,
                    seat,
                    Some(to),
                    *thread,
                    refs,
                    attachments,
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
                    || !self.may_see(name, seat, project, &room)
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
                    Vec::new(),
                    text,
                    now,
                )
            }
            Request::Ask {
                id,
                room,
                to,
                path,
                topic,
                question,
            } => match to.trim() {
                "" => self.ask_by_scope(project, seat, name, id, room, path, topic, question, now),
                target => self.ask_asked(project, seat, name, id, room, target, question, now),
            },
            Request::Reply { id, ask, text } | Request::Decline { id, ask, text } => {
                let answer = matches!(frame, Request::Reply { .. });
                self.ask_settled(name, seat, id, *ask, text, answer, now)
            }
            Request::Withdraw { id, ask } => self.ask_withdrawn(name, seat, id, *ask, now),
            Request::Retire { id, session } => self.ask_retired(name, id, session, now),
            Request::Asks => {
                let (by_me, of_me) = self.asks_for(name, now);
                Response::Asks { by_me, of_me }
            }
            Request::Read {
                room,
                since,
                last,
                thread,
                before,
            } => {
                let room = match room.trim() {
                    "" => Self::default_chat_room(seat, project),
                    r => r.to_string(),
                };
                if room != chat::DM_ROOM && !self.may_see(name, seat, project, &room) {
                    return refuse(None, Self::room_refusal(project, &room));
                }
                if let Some(t) = thread {
                    self.inner.signals.lock().expect("the signals lock").act(
                        *t,
                        crate::signal::Act::Read,
                        name,
                        now,
                    );
                }
                let handles = self.handles();
                let chat = self.inner.chat.lock().expect("the chat lock");
                let mut history = chat.read(
                    &room,
                    *since,
                    last.map(|n| n as usize),
                    *thread,
                    *before,
                    |m| self.visible(name, seat, project, m),
                );
                let opened = history
                    .rows
                    .iter()
                    .filter(|m| chat.is_record(m.seq))
                    .count();
                drop(chat);
                // A record filed is only worth filing if it is opened, so opens are counted.
                if opened > 0 {
                    let mut ledger = self.inner.signals.lock().expect("the signals lock");
                    for _ in 0..opened {
                        ledger.counted(crate::signal::Count::RecordOpen);
                    }
                }
                history.rows = history
                    .rows
                    .into_iter()
                    .map(|m| self.shown(m, &handles))
                    .collect();
                Response::History(history)
            }
            // A search is a read: it moves nothing and answers what a read answers, so a head
            // draws its rows the same way. The log is asked, not the ring, so a hit the window has
            // long dropped is still found.
            Request::Search { room, text, last } => {
                let room = match room.trim() {
                    "" => Self::default_chat_room(seat, project),
                    r => r.to_string(),
                };
                if room != chat::DM_ROOM && !self.may_see(name, seat, project, &room) {
                    return refuse(None, Self::room_refusal(project, &room));
                }
                let handles = self.handles();
                let chat = self.inner.chat.lock().expect("the chat lock");
                let last_seq = chat.last_in(&room);
                let rows = chat.search(
                    &room,
                    text,
                    last.map_or(chat::MAX_READ, |n| n as usize),
                    |m| self.visible(name, seat, project, m),
                );
                drop(chat);
                let rows = rows.into_iter().map(|m| self.shown(m, &handles)).collect();
                Response::History(chat::History {
                    room,
                    rows,
                    last_seq,
                    trimmed: false,
                })
            }
            // The operator's own reading of the box: every dm, of every pair. Only the person's
            // seat may ask - a session asking is refused rather than shown another pair's talk -
            // and nothing here moves a cursor or a mark, so a party's unread count is untouched.
            Request::Dms { last } => {
                if seat != Seat::Operator {
                    return refuse(None, crate::Refused::NotTheOperator);
                }
                let handles = self.handles();
                let chat = self.inner.chat.lock().expect("the chat lock");
                let mut history = chat.read(
                    chat::DM_ROOM,
                    0,
                    last.map(|n| n as usize),
                    None,
                    None,
                    |_| true,
                );
                drop(chat);
                history.rows = history
                    .rows
                    .into_iter()
                    .map(|m| self.shown(m, &handles))
                    .collect();
                Response::History(history)
            }
            // The blob store is the room's, not a room's: an upload is a file with an id and no
            // audience until a message names it, so it is not gated on which room this seat is in.
            Request::PutBlob {
                upload,
                name: blob_name,
                mime,
                part,
                last,
                data,
            } => {
                let bytes = crate::blob::decode_base64(data);
                let out = self
                    .inner
                    .blobs
                    .lock()
                    .expect("the blob lock")
                    .put_part(upload, blob_name, mime, *part, &bytes, *last, now);
                match out {
                    Ok(attachment) => Response::Blob { attachment },
                    Err(why) => refuse(None, why),
                }
            }
            Request::GetBlob { id, offset } => {
                let part = self
                    .inner
                    .blobs
                    .lock()
                    .expect("the blob lock")
                    .read(id, *offset);
                match part {
                    Ok(p) => Response::BlobPart {
                        attachment: p.attachment,
                        offset: p.offset,
                        data: crate::blob::encode_base64(&p.data),
                        last: p.last,
                    },
                    Err(why) => refuse(None, why),
                }
            }
            Request::Roster { room } if room.trim() == chat::SEATS => Response::Roster {
                rows: self.queue_seats(),
                room: chat::SEATS.to_string(),
            },
            Request::MayPrompt {
                id,
                to,
                project: hint,
                because,
            } => match self.may_prompt(project, seat, name, to, hint, *because) {
                Ok(session) => Response::MayPrompt {
                    id: id.clone(),
                    session,
                },
                Err(why) => refuse(Some(id), why),
            },
            Request::Roster { room } => {
                let room = match room.trim() {
                    "" => Self::default_chat_room(seat, project),
                    r => r.to_string(),
                };
                if !self.may_see(name, seat, project, &room) {
                    return refuse(None, Self::room_refusal(project, &room));
                }
                Response::Roster {
                    rows: self.roster_of(&room),
                    room,
                }
            }
            Request::Inbox {
                attention,
                limit,
                after,
            } => {
                let handles = self.handles();
                let id = self.principal_of(name);
                let chat = self.inner.chat.lock().expect("the chat lock");
                let (rows, skipped) = chat.inbox(
                    &id,
                    name,
                    self.family(name).as_deref(),
                    *attention,
                    limit.map_or(chat::MAX_READ, |n| n as usize),
                    |m| after.is_none_or(|a| m.seq > a) && self.visible(name, seat, project, m),
                );
                Response::Inbox {
                    rows: rows.into_iter().map(|m| self.shown(m, &handles)).collect(),
                    skipped,
                    cursor: chat.cursor(&id),
                    head: chat.head(),
                }
            }
            Request::Invite { room, session } => {
                let room = self.own_room_or(room, project);
                let Some(room) = self.project_room_named(&room) else {
                    return refuse(None, crate::Refused::UnknownChatRoom { room: room.clone() });
                };
                let key = chat::project_of_room(&room).unwrap_or("");
                // The room's own members and the operator invite; a guest does not.
                if !seat.is_wide() && project != key {
                    return refuse(None, Self::room_refusal(project, &room));
                }
                let asked = session.trim().trim_start_matches('@');
                let who = match self.resolve_name(project, asked, Reach::Host, Labels::Refuse) {
                    Ok(who) => who,
                    Err(miss) => return refuse(None, miss.refused(asked, self)),
                };
                if !self.is_known(&who) {
                    return refuse(
                        None,
                        crate::Refused::UnknownAddressee {
                            known: self.closest(&who),
                            name: who,
                        },
                    );
                }
                self.set_guest(&room, &who, true);
                Response::Roster {
                    rows: self.roster_of(&room),
                    room,
                }
            }
            Request::Leave { room, session } => {
                let room = self.own_room_or(room, project);
                let Some(room) = self.project_room_named(&room) else {
                    return refuse(None, crate::Refused::UnknownChatRoom { room: room.clone() });
                };
                let key = chat::project_of_room(&room).unwrap_or("");
                let who = match session.as_deref().map(|s| s.trim().trim_start_matches('@')) {
                    None | Some("") => name.to_string(),
                    Some(asked) => self
                        .resolve_name(project, asked, Reach::Host, Labels::Refuse)
                        .unwrap_or_else(|_| asked.to_string()),
                };
                // A guest leaves by itself; the room's members and the operator show one out.
                if who != name && !seat.is_wide() && project != key {
                    return refuse(None, Self::room_refusal(project, &room));
                }
                self.set_guest(&room, &who, false);
                Response::Roster {
                    rows: self.roster_of(&room),
                    room,
                }
            }
            Request::ArchiveRoom { room } => self.archive_room(project, seat, room, true, now),
            Request::UnarchiveRoom { room } => self.archive_room(project, seat, room, false, now),
            Request::Ack { seq } => {
                let id = self.principal_of(name);
                let mut chat = self.inner.chat.lock().expect("the chat lock");
                // Every seat's ack is its seen mark, because an ack says the reader HAS the row: a
                // session's model took it, an external's channel was written to. Written, not held,
                // so a coordinator that stops mid-session resumes the digest where the seat is.
                chat.mark_seen(&id, *seq);
                chat.keep_seen(&id);
                Response::Acked {
                    cursor: chat.ack(&id, *seq),
                }
            }
            Request::Nick { id, nick } => self.take_nick(project, seat, name, id, nick, now),
            Request::Signals => Response::Signals {
                card: self.scorecard(),
            },
            Request::Acted { seq, act } => {
                self.inner
                    .signals
                    .lock()
                    .expect("the signals lock")
                    .act(*seq, *act, name, now);
                Response::Noted
            }
            Request::WakeEnded {
                seqs,
                cheap,
                silent,
                tokens,
            } => {
                self.inner
                    .signals
                    .lock()
                    .expect("the signals lock")
                    .wake(seqs, *cheap, *silent, *tokens);
                Response::Noted
            }
            _ => refuse(None, crate::Refused::ChatIsTheServers),
        }
    }

    /// A project room put out of sight, or brought back. The host room is refused: it is the one
    /// room every seat on the box is in, and putting it away would hide the box from its seats.
    /// An archived room keeps every message - only the lists a person picks from lose it.
    fn archive_room(
        &self,
        project: &str,
        seat: Seat,
        room: &str,
        archived: bool,
        now: u64,
    ) -> Response {
        let named = self.own_room_or(room, project);
        // The host room is answered before any lookup: it is never put away, by anybody.
        if named == chat::HOST_ROOM {
            return Response::Refused {
                id: None,
                why: crate::Refused::HostRoomKept,
            };
        }
        let Some(room) = self.project_room_named(&named) else {
            return Response::Refused {
                id: None,
                why: crate::Refused::UnknownChatRoom { room: named },
            };
        };
        // The room's own members and the operator put a room away; a guest it was merely invited
        // into comes and goes in it, and does not decide whether anybody else finds it.
        let key = chat::project_of_room(&room).unwrap_or("");
        if !seat.is_wide() && project != key {
            return Response::Refused {
                id: None,
                why: Self::room_refusal(project, &room),
            };
        }
        let mut chat = self.inner.chat.lock().expect("the chat lock");
        if archived {
            chat.archive(&room, now);
        } else {
            chat.unarchive(&room);
        }
        Response::Archived {
            archived_ms: chat.archived_ms(&room),
            room,
        }
    }

    /// The message's attachments, as the store holds them. An id the store does not have is
    /// refused: a message naming bytes nobody can fetch is a line a reader cannot draw.
    fn attach_blobs(&self, ids: &[String]) -> Result<Vec<chat::Attachment>, crate::Refused> {
        if ids.len() > chat::MAX_ATTACHMENTS {
            return Err(crate::Refused::TooManyAttachments {
                max: chat::MAX_ATTACHMENTS as u64,
                got: ids.len() as u64,
            });
        }
        let blobs = self.inner.blobs.lock().expect("the blob lock");
        ids.iter()
            .map(|id| {
                blobs
                    .get(id)
                    .ok_or(crate::Refused::UnknownBlob { id: id.clone() })
            })
            .collect()
    }

    /// Drop the blobs no message names, once the state has been written whole: that rewrite is the
    /// one moment the answer is taken, and it is taken from the whole log, not the ring - a message
    /// paged back out of a segment is still readable.
    fn gc_blobs(&self, now: u64) {
        let rewrites = self.inner.chat.lock().expect("the chat lock").rewrites();
        if rewrites == self.inner.blob_rewrites.load(Ordering::SeqCst) {
            return;
        }
        self.inner.blob_rewrites.store(rewrites, Ordering::SeqCst);
        let kept = self
            .inner
            .chat
            .lock()
            .expect("the chat lock")
            .kept_attachments();
        let dropped = self
            .inner
            .blobs
            .lock()
            .expect("the blob lock")
            .gc(&kept, now);
        if dropped > 0 {
            eprintln!("room: blob store dropped {dropped} blob(s) no message keeps");
        }
    }

    /// The project a seat belongs to: the room that holds it as a top-level member. `None` for
    /// the operator, a bridge-shaped handle and any name the rooms do not place.
    fn project_of(&self, name: &str) -> Option<String> {
        let rooms = self.inner.rooms.lock().expect("the rooms lock");
        rooms.iter().find_map(|(project, r)| {
            r.members()
                .iter()
                .any(|m| m.group == crate::Group::Top && m.session == name)
                .then(|| project.clone())
        })
    }

    /// Has `to` allowed `from` direct messages? A grant for the SESSION stays; a one-message
    /// grant is spent by the check that reads it. Read from the room's own state, so a restart is
    /// not a room that forgets, and a row a linked box gave is read the same way as this one's own.
    fn consent_given(&self, from: &str, to: &str) -> bool {
        self.inner
            .chat
            .lock()
            .expect("the chat lock")
            .consent_given(from, to)
    }

    /// Tell every linked box about one pair: a consent, or the pair being taken back. The table is
    /// room state on each box and the two boxes judge the same dm, so both have to hold it.
    fn say_consent(&self, from: &str, to: &str, session: bool, on: bool) {
        let Some(peer) = self.peer() else {
            return;
        };
        let frame = crate::peer::PeerFrame::Consents {
            rows: vec![crate::peer::ConsentRow {
                from: from.to_string(),
                to: to.to_string(),
                session,
                on,
            }],
        };
        for host in peer.up() {
            peer.ship(&host, frame.clone());
        }
    }

    /// The whole table, to one box: a link that has just come up knows nothing of what this box was
    /// told while it was away.
    fn ship_consents(&self, host: &str) {
        let Some(peer) = self.peer() else {
            return;
        };
        let rows: Vec<crate::peer::ConsentRow> = self
            .inner
            .chat
            .lock()
            .expect("the chat lock")
            .consents()
            .into_iter()
            .map(|(from, to, session)| crate::peer::ConsentRow {
                from,
                to,
                session,
                on: true,
            })
            .collect();
        peer.ship(host, crate::peer::PeerFrame::Consents { rows });
    }

    /// Every line this box kept for a box that was away, in the order it was said. A ship that
    /// fails leaves the rest queued: the far side's own `peer:<author>:<seq>` request id is what
    /// stops a line that did cross before the link died from being filed twice.
    fn ship_held(&self, host: &str) {
        let Some(peer) = self.peer() else {
            return;
        };
        let rows = self
            .inner
            .chat
            .lock()
            .expect("the chat lock")
            .held_for(host);
        let mut went = 0;
        let mut short = 0;
        for message in &rows {
            if !peer.ship(
                host,
                crate::peer::PeerFrame::Message {
                    message: message.clone(),
                },
            ) {
                short = rows.len() - went;
                break;
            }
            went += 1;
        }
        if let Some(last) = rows.get(went.saturating_sub(1)).filter(|_| went > 0) {
            self.inner
                .chat
                .lock()
                .expect("the chat lock")
                .shipped_to(host, last.seq);
        }
        if short > 0 {
            eprintln!("room: {host} went away again with {short} line(s) still kept for it");
        }
    }

    /// A peer's table, or the row that changed: kept, so this box honours the pair after the link
    /// goes down and after this coordinator restarts. Not sent on: the far side already has it, and
    /// a mesh that echoed would never stop.
    fn learn_consents(&self, rows: Vec<crate::peer::ConsentRow>) {
        let mut chat = self.inner.chat.lock().expect("the chat lock");
        for row in rows {
            chat.learn_consent(&row.from, &row.to, row.session, row.on);
        }
    }

    /// A dm whose addressee is another PROJECT asks that seat first. It is an ask, so the answer
    /// is a row on the addressee's own board and the rule crosses boxes as far as asks do; the
    /// message is NOT delivered, and the sender is told to say it again once it is allowed.
    #[allow(clippy::too_many_arguments)]
    fn dm_needs_consent(
        &self,
        project: &str,
        seat: Seat,
        name: &str,
        id: &str,
        room: &str,
        to: &str,
        now: u64,
    ) -> Option<Response> {
        let other = self.project_of(to)?;
        if other == project || self.consent_given(name, to) {
            return None;
        }
        let question = format!(
            "`{name}` of `{project}` asks to send you direct messages. Answer `allow once`,              `allow for the session`, or anything else to refuse. Your answer is the consent;              the message it was sent with was not delivered."
        );
        // The HOST room, because a consent ask crosses projects and the two seats see no shared
        // project room: the host room is the one both read.
        let _ = room;
        let asked = self.ask_to(project, seat, name, id, chat::HOST_ROOM, to, &question, now);
        if let Response::Asked { ask, .. } = &asked {
            self.inner
                .consent_asks
                .lock()
                .expect("the consent lock")
                .insert(ask.seq, (name.to_string(), to.to_string()));
            return Some(Response::Refused {
                id: Some(id.to_string()),
                why: crate::Refused::DmNeedsConsent {
                    to: to.to_string(),
                    ask: ask.seq,
                },
            });
        }
        // A target the ask itself could not reach is that refusal, in the ask's own words.
        Some(asked)
    }

    /// Route a `say`: a `to` is a private message, a reply stays in its root's room, and anything
    /// else is said in the room named - where an `@` of someone outside it invites them in, so a
    /// mention reaches them where it was said, with the thread it sits in.
    #[allow(clippy::too_many_arguments)]
    fn route_say(
        &self,
        project: &str,
        seat: Seat,
        name: &str,
        id: &str,
        room: &str,
        text: &str,
        to: Option<&str>,
        thread: Option<u64>,
        refs: &[chat::Ref],
        attachments: &[String],
        kind: crate::SayKind,
        now: u64,
    ) -> Response {
        let refuse = |why: crate::Refused| Response::Refused {
            id: Some(id.to_string()),
            why,
        };
        // `kind: ask` IS the room's ask: one held question, so the target's own board carries it
        // and only `answer` releases it. It has to name somebody, or nothing holds it.
        if kind.needs_target() {
            let asked = to.map(str::trim).filter(|t| !t.is_empty());
            let Some(asked) = asked else {
                return refuse(crate::Refused::AskNamesNobody);
            };
            return self.ask_asked(project, seat, name, id, room, asked, text, now);
        }
        if seat != Seat::Operator && attachments.is_empty() && chat::is_bare_ack(text) {
            self.note_null_post(name, seat);
            return refuse(crate::Refused::NullPost);
        }
        // A status line is the roster's, not the room's: the same reason the ack is folded, and the
        // same exemption - the operator may say what they like, in any words.
        if seat != Seat::Operator && attachments.is_empty() && chat::is_status_post(text) {
            self.note_count(crate::signal::Count::StatusFolded);
            return refuse(crate::Refused::StatusPost);
        }
        // The host room is an announcement channel, and an announcement is one line: a long one is
        // refused here rather than filed, because the room's first line would be the whole of it.
        if kind == crate::SayKind::Announce && !chat::one_line(text) {
            return refuse(crate::Refused::AnnounceNotOneLine);
        }
        // What the line asks of its readers rides the refs, so the frame and the store keep their
        // shape and an old row still reads as a post.
        let refs = match chat::with_say_ref(kind, refs) {
            Ok(refs) => refs,
            Err(why) => return refuse(why),
        };
        let text = match self.resolve_mentions(project, text) {
            Ok(t) => t,
            Err(why) => return refuse(why),
        };
        let attachments = match self.attach_blobs(attachments) {
            Ok(a) => a,
            Err(why) => return refuse(why),
        };
        let to = match to
            .map(|t| t.trim().trim_start_matches('@'))
            .filter(|t| !t.is_empty())
        {
            None => None,
            Some(t) => match self.resolve_name(project, t, Reach::OwnProject, Labels::Refuse) {
                Ok(full) => Some(full),
                Err(miss) => return refuse(miss.refused(t, self)),
            },
        };
        if let Some(t) = &to
            && (t == name || !self.is_known(t))
        {
            return refuse(crate::Refused::UnknownAddressee {
                name: t.clone(),
                known: self.closest(t),
            });
        }
        // A dm to another PROJECT asks that seat first: same project is free, and the rule reads
        // the addressee's project rather than host membership, so it holds between boxes.
        if let Some(t) = &to
            && let Some(asked) = self.dm_needs_consent(project, seat, name, id, room, t, now)
        {
            return asked;
        }
        let refs = match &to {
            Some(t) => match self.queue_seat_gate(t, name, seat, project, kind, thread, refs) {
                Ok(r) => r,
                Err(why) => return refuse(why),
            },
            None => refs,
        };
        // Looked up before the room: a reply stays in its root's room, so the frame's room does
        // not decide where it lands. The chat lock is released first - a lock held across another
        // lock is how two threads find each other.
        let root = thread.and_then(|t| {
            let found: Vec<chat::Message> = {
                let chat = self.inner.chat.lock().expect("the chat lock");
                chat.thread_root(t, |_| true).into_iter().collect()
            };
            found
                .into_iter()
                .next()
                .filter(|m| self.visible(name, seat, project, m))
        });
        if let Some(root) = root {
            // A private answer to a public message goes to the dm channel, naming the thread.
            if let Some(to) = to.clone()
                && root.room != chat::DM_ROOM
            {
                let text = Self::linked_to(&root, &text);
                return self.say_and_push(
                    id,
                    chat::DM_ROOM,
                    name,
                    seat,
                    Some(to),
                    None,
                    refs.to_vec(),
                    attachments,
                    &text,
                    now,
                );
            }
            // A reply in a dm thread is still a dm: the other party is the root's addressee, or
            // its author when somebody answers this session's own message.
            let other = match root.room == chat::DM_ROOM {
                false => None,
                true => {
                    let other = if root.author == name {
                        root.to.clone()
                    } else {
                        Some(root.author.clone())
                    };
                    match other.filter(|t| t != name) {
                        Some(t) => Some(t),
                        None => {
                            return refuse(crate::Refused::UnknownAddressee {
                                name: root.room,
                                known: Vec::new(),
                            });
                        }
                    }
                }
            };
            // A dm thread is the two people in it. A message that names a third cannot answer
            // it: handing it to the thread's other party instead is how a dm to a bridge was
            // stored, and delivered, against an unrelated session.
            if let (Some(named), Some(party)) = (to.as_deref(), other.as_deref())
                && named != party
            {
                return refuse(crate::Refused::ThreadIsAnotherDm {
                    thread: root.seq,
                    with: party.to_string(),
                });
            }
            let room = root.room;
            // An agent-only thread past the depth cap is a conversation nothing reads: the answer
            // belongs in a row the operator can see, not six replies down a room.
            if seat != Seat::Operator
                && room != chat::DM_ROOM
                && let Some(shape) = self
                    .inner
                    .chat
                    .lock()
                    .expect("the chat lock")
                    .thread_shape(&room, root.seq)
                && !shape.operator
                && shape.depth > chat::MAX_AGENT_DEPTH
            {
                self.note_count(crate::signal::Count::DepthCapped);
                return refuse(crate::Refused::ThreadTooDeep {
                    depth: shape.depth as u64,
                    max: chat::MAX_AGENT_DEPTH as u64,
                    root: shape.root,
                });
            }
            // A mention in a reply brings the one it names in, exactly as one in a room line does.
            if room != chat::DM_ROOM {
                self.invite_the_named(&room, project, &text);
            }
            self.say_and_push(
                id,
                &room,
                name,
                seat,
                other,
                Some(root.seq),
                refs,
                attachments,
                &text,
                now,
            )
        } else if let Some(thread) = thread {
            refuse(crate::Refused::UnknownThread { thread })
        } else if let Some(to) = to {
            // `to` is a private message: it goes to the dm channel between the two, and the room
            // the frame named is not written to at all.
            self.say_and_push(
                id,
                chat::DM_ROOM,
                name,
                seat,
                Some(to),
                None,
                refs.to_vec(),
                attachments,
                &text,
                now,
            )
        } else if room.trim().is_empty()
            && answers_the_dm(kind)
            && let Some(dm) = self.dm_awaited(name, &text)
        {
            // The answer to a dm goes to the dm. Said in a room it is a line the one it is for
            // will not be woken by, which is how a private question was answered in public.
            let note = format!(
                "this answered the dm from `{}` (#{}), so it went there, not to the room{} — \
                 name a room (`host`, `project/<key>`) to say a line there instead",
                dm.peer,
                dm.seq,
                if crate::SayKind::Fyi == kind {
                    " as an `fyi`"
                } else {
                    ""
                },
            );
            let mut said = self.say_and_push(
                id,
                chat::DM_ROOM,
                name,
                seat,
                Some(dm.peer),
                Some(dm.seq),
                refs.to_vec(),
                attachments,
                &text,
                now,
            );
            if let Response::Said { note: out, .. } = &mut said {
                *out = note;
            }
            said
        } else {
            let room = match room.trim() {
                "" => Self::default_chat_room(seat, project),
                r => r.to_string(),
            };
            if !self.may_see(name, seat, project, &room) {
                return refuse(Self::room_refusal(project, &room));
            }
            // The box-wide room carries announcements, blockers and mentions, not talk: a line
            // there is for somebody, for everybody, or it stops the box - anything else belongs to
            // a project room.
            if room == chat::HOST_ROOM
                && seat != Seat::Operator
                && !matches!(kind, crate::SayKind::Announce | crate::SayKind::Blocker)
                && chat::mentions(&text).is_empty()
            {
                self.note_count(crate::signal::Count::HostRoomRefused);
                return refuse(crate::Refused::HostRoomUnaddressed);
            }
            // A project room's line that points at nothing is the leak to measure, not to refuse.
            // The kind's own ref names no row, so it does not make a line point at something.
            if seat != Seat::Operator
                && chat::project_of_room(&room).is_some()
                && refs.iter().all(|r| r.kind == chat::SAY)
                && !chat::has_ref(&text)
            {
                self.note_count(crate::signal::Count::ProjectPostWithoutRef);
            }
            self.invite_the_named(&room, project, &text);
            // A question the room could have put to an owner is nudged, never refused: the line
            // lands either way, and the nudge is what makes the next question an `ask`.
            let nudge = self.ask_nudge(project, name, seat, &room, &text, now);
            let mut said = self.say_and_push(
                id,
                &room,
                name,
                seat,
                None,
                None,
                refs.to_vec(),
                attachments,
                &text,
                now,
            );
            if let Some(nudge) = nudge
                && let Response::Said { note, .. } = &mut said
            {
                *note = nudge;
            }
            said
        }
    }

    /// The one-line nudge a broadcast question earns when the room can name an owner for what it
    /// names: *`s-a` owns this, ask it*. `None` for a line that is not a question of the project
    /// room, or for a question nothing owns - that one is `ask_by_room`'s measure, not this one.
    fn ask_nudge(
        &self,
        project: &str,
        name: &str,
        seat: Seat,
        room: &str,
        text: &str,
        now: u64,
    ) -> Option<String> {
        if seat == Seat::Operator
            || chat::project_of_room(room).is_none()
            || !text.trim_end().ends_with('?')
            || !chat::mentions(text).is_empty()
        {
            return None;
        }
        // A path is a word with a slash in it; a topic is one of the room's own words.
        let mut scopes: Vec<crate::scope::Scope> = chat::path_words(text)
            .into_iter()
            .map(crate::scope::Scope::Path)
            .collect();
        let words = chat::echo_words(text);
        for claim in self.claims_in(project) {
            if !crate::own::is_path(&claim.scope) && words.contains(&claim.scope) {
                scopes.push(crate::scope::Scope::Topic(claim.scope.clone()));
            }
        }
        for scope in scopes {
            let placed = self.place(project, &scope, name, now);
            let Some(target) = placed.target().map(str::to_string) else {
                continue;
            };
            self.note_count(crate::signal::Count::BroadcastQuestion);
            return Some(format!(
                "`{target}` owns this ({}) — ask it: `room ask {}={}`",
                placed.short(),
                scope.word(),
                scope.text()
            ));
        }
        None
    }

    /// This project's ownership claims, live or ended: the caller reads which of them are live.
    fn claims_in(&self, project: &str) -> Vec<crate::own::Claim> {
        let claims = self.inner.claims.lock().expect("the claims lock");
        claims
            .get(project)
            .map(|c| c.all().cloned().collect())
            .unwrap_or_default()
    }

    /// A dm answering a public message, with the thread it answers in front: `re #12 in host: …`.
    fn linked_to(root: &chat::Message, text: &str) -> String {
        format!("re #{} in {}: {text}", root.seq, root.room)
    }

    /// Invite into `room` everyone its text names who could not see it: a mention stays public
    /// where it was said, and the one it names is brought in rather than left outside looking in.
    fn invite_the_named(&self, room: &str, project: &str, text: &str) {
        for m in chat::mentions(text) {
            let Ok(who) = self.resolve_name(project, &m, Reach::Host, Labels::Rewrite) else {
                continue;
            };
            if who == chat::OPERATOR || !self.is_known(&who) {
                continue;
            }
            let (seat, theirs) = self.seat_and_project_of(&who);
            if !self.may_see(&who, seat, &theirs, room) {
                self.set_guest(room, &who, true);
            }
        }
    }

    /// Who a principal is as a room sees it: the seat and room its connection says, else a session
    /// of the project the tree puts it in, else a session in no project at all.
    fn seat_and_project_of(&self, name: &str) -> (Seat, String) {
        if let Some(c) = self.conn_list().into_iter().find(|c| c.who.session == name) {
            return (c.seat, c.project.clone());
        }
        let rooms = self.inner.rooms.lock().expect("the rooms lock");
        let key = rooms
            .iter()
            .find(|(k, r)| !k.is_empty() && r.members().iter().any(|m| m.session == name))
            .map(|(k, _)| k.clone())
            .unwrap_or_default();
        (Seat::Session, key)
    }

    /// The refusal a repeat of one of the room's or thread's last messages earns, naming it. The
    /// operator is exempt - saying a thing twice is how a person underlines it - and so is a
    /// repeat of a request the room has already answered.
    fn echo(
        &self,
        seat: Seat,
        who: &str,
        room: &str,
        thread: Option<u64>,
        request: &str,
        text: &str,
    ) -> Option<u64> {
        if seat == Seat::Operator {
            return None;
        }
        let chat = self.inner.chat.lock().expect("the chat lock");
        if chat.repeated(room, who, request).is_some() {
            return None;
        }
        let recent = chat.recent(room, thread, who, chat::ECHO_WINDOW);
        chat::echo_of(text, &recent)
    }

    /// A post, after the room's two etiquette checks: a repeat of a recent message is refused, and
    /// a long one is filed as a record the room names (see [`chat::Chat::append`]).
    #[allow(clippy::too_many_arguments)]
    fn say_and_push(
        &self,
        id: &str,
        room: &str,
        author: &str,
        kind: Seat,
        to: Option<String>,
        thread: Option<u64>,
        refs: Vec<chat::Ref>,
        attachments: Vec<chat::Attachment>,
        text: &str,
        now: u64,
    ) -> Response {
        if let Some(seq) = self.echo(kind, author, room, thread, id, text) {
            self.note_count(crate::signal::Count::EchoRefused);
            return Response::Refused {
                id: Some(id.to_string()),
                why: crate::Refused::EchoPost { seq },
            };
        }
        self.append_and_push(
            id,
            room,
            author,
            kind,
            to,
            thread,
            refs,
            attachments,
            text,
            now,
        )
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
        attachments: Vec<chat::Attachment>,
        text: &str,
        now: u64,
    ) -> Response {
        // The budget is the room's answer to one session's chatter, and it is charged before the
        // append so a refused post is never stored: the operator and the harness are never charged.
        if !matches!(kind, Seat::Operator | Seat::System) {
            let allowed = self
                .inner
                .budget
                .lock()
                .expect("the budget lock")
                .allowed(author, room, now);
            if let Err(why) = allowed {
                self.note_count(crate::signal::Count::BudgetRefused);
                return Response::Refused {
                    id: Some(id.to_string()),
                    why,
                };
            }
        }
        // The operator's long line is carried whole: a person is not filing a record for the room.
        // And neither is a line that CROSSES a link, either way: a record's text stays on the box
        // that filed it, so what crossed would be a reference the other box cannot read. The text
        // cap is far under a peer frame's, so the whole line fits the link.
        let crossed = self.across_boxes(to.as_deref()) || self.across_boxes(Some(author));
        let file = kind != Seat::Operator && !crossed;
        // What the author is drawn by rides the refs, so a reader who knew the seat under another
        // label is told which, and a line written before this has no ref and reads unchanged.
        let refs = match self.label_of(author) {
            Some(label) => chat::with_nick_ref(&label, &refs),
            None => refs,
        };
        let appended = self.inner.chat.lock().expect("the chat lock").append_post(
            now,
            id,
            room,
            author,
            kind,
            to,
            thread,
            refs,
            attachments,
            text,
            file,
        );
        let message = match appended {
            Ok(m) => m,
            Err(why) => {
                return Response::Refused {
                    id: Some(id.to_string()),
                    why,
                };
            }
        };
        if !matches!(kind, Seat::Operator | Seat::System) {
            let _ = self
                .inner
                .budget
                .lock()
                .expect("the budget lock")
                .spend(author, room, now);
        }
        // Pushed after the log lock is released, and a repeat of `id` pushes again. A reply answers
        // its root's author, who is the one the room names: the root may be in another room, so
        // nothing in the text says so.
        let filed = self
            .inner
            .chat
            .lock()
            .expect("the chat lock")
            .is_record(message.seq);
        if filed {
            self.note_count(crate::signal::Count::RecordMade);
        }
        // A blocker is the one line that flags the operator: it is counted here, and `deliver`
        // pushes it to the person's own connection as if it named them.
        if message.say_kind() == crate::SayKind::Blocker {
            self.note_count(crate::signal::Count::BlockerFlagged);
        }
        // A line addressed to a seat on another box goes over that box's link. Here rather than in
        // each door, because a dm, a `say` with a `to` and a reply that answers one all arrive.
        self.ship_remote(&message);
        self.deliver(&message, now);
        Response::Said {
            id: id.to_string(),
            message: self.shown(message, &self.handles()),
            note: String::new(),
        }
    }

    fn note_null_post(&self, name: &str, seat: Seat) {
        self.inner
            .signals
            .lock()
            .expect("the signals lock")
            .refused(name, seat.word());
        self.note_count(crate::signal::Count::AckRefused);
    }

    /// One of the room's named counters, up by one - what the `signals` frame's `counts` reads.
    fn note_count(&self, what: crate::signal::Count) {
        self.inner
            .signals
            .lock()
            .expect("the signals lock")
            .counted(what);
    }

    /// Enter a message in the signal ledger, and credit the thread it answers. The row's `kind` is
    /// what the line asked of its readers when it asked anything - an `ask`, a `blocker`, a
    /// `decision`, an `fyi` - and the seat it came from otherwise, so `signals` counts wakes per
    /// message by the kind that rang them.
    fn note_message(&self, message: &chat::Message, now: u64) {
        if message.kind == Seat::System {
            return;
        }
        let say = message.say_kind();
        let asked = self
            .inner
            .chat
            .lock()
            .expect("the chat lock")
            .ask(message.seq)
            .is_some();
        let kind = if say.recorded() {
            say.word()
        } else if asked {
            "ask"
        } else {
            message.kind.word()
        };
        let mut ledger = self.inner.signals.lock().expect("the signals lock");
        ledger.message(message.seq, message.ts, &message.author, kind);
        if let Some(t) = message.thread {
            ledger.act(t, crate::signal::Act::Reply, &message.author, now);
            if message.refs.iter().any(|r| r.kind == "todo") {
                ledger.act(t, crate::signal::Act::Todo, &message.author, now);
            }
        }
    }

    /// The dm each principal was SENT and has not answered, kept for [`Server::dm_awaited`]. A dm
    /// sent pays the sender's own debt to that peer: answering in the dm is how a dm's answer ends.
    fn note_dm(&self, message: &chat::Message) {
        if message.room != chat::DM_ROOM {
            return;
        }
        let Some(to) = &message.to else { return };
        let mut dms = self
            .inner
            .dms_awaiting
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        dms.insert(
            to.clone(),
            DmAwaiting {
                seq: message.seq,
                peer: message.author.clone(),
            },
        );
        let paid = dms.get(&message.author).is_some_and(|d| {
            message.to.as_deref() == Some(d.peer.as_str()) || message.thread == Some(d.seq)
        });
        if paid {
            dms.remove(&message.author);
        }
    }

    /// The dm `who` was sent and has not answered, when `text` answers it - the `seq` it hangs from
    /// and the peer it goes to. `None` once the dm was answered, or when the line names somebody
    /// else, so a session's later lines about other things are left where their sender put them.
    fn dm_awaited(&self, who: &str, text: &str) -> Option<DmAwaiting> {
        let dms = self
            .inner
            .dms_awaiting
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dm = dms.get(who)?;
        chat::names_in_text(text, &dm.peer).then(|| dm.clone())
    }

    /// Everything a message does once it is in the log: pushed to whoever may see it, kept for
    /// whoever may not and will be here later, and the blobs nothing names any more collected.
    fn deliver(&self, message: &chat::Message, now: u64) {
        self.note_message(message, now);
        self.note_dm(message);
        let answered = message.thread.and_then(|t| {
            self.inner
                .chat
                .lock()
                .expect("the chat lock")
                .author_of(&message.room, t)
        });
        let mut here: BTreeSet<String> = BTreeSet::new();
        let handles = self.handles();
        // A blocker is the operator's business whoever it names: a session that is stopped is the
        // person's to unblock, so it arrives on their connection as if it were addressed to them.
        let blocked = message.say_kind() == crate::SayKind::Blocker;
        for c in self.conn_list() {
            let name = c.who.session.as_str();
            here.insert(name.to_string());
            if name == message.author || !self.visible(name, c.seat, &c.project, message) {
                continue;
            }
            let family = c.family.as_deref();
            c.send(Response::Chat {
                names_you: message.names(name)
                    || family.is_some_and(|f| message.names(f) || answered.as_deref() == Some(f))
                    || answered.as_deref() == Some(name)
                    || (blocked && c.seat == Seat::Operator),
                message: self.shown(message.clone(), &handles),
            });
            // A session's daemon marks what its model took, with `Ack`: a push it dropped, or held
            // across its own restart, was not seen, and the join digest must still tell it. An
            // external's reader is the channel the row went to, so the push IS the delivery - and
            // the mark is the SEAT's, the key `Ack` and `deliver_pending` read it under.
            if c.seat != Seat::Session {
                let id = self.principal_of(name);
                self.inner
                    .chat
                    .lock()
                    .expect("the chat lock")
                    .mark_seen(&id, message.seq);
            }
        }
        // And a member with no connection is told nothing by a push. It is not lost: the message is
        // kept for it and delivered when it next connects, and its inbox cursor counts it unread.
        self.remember_for_absent(message, &here);
        self.gc_blobs(now);
    }

    // ===== Asks =====

    /// The asks `me` made and the asks `me` was asked, each view carrying what the room knows about
    /// the OTHER end: gone since when, and whether that is past the grace. That is the whole of the
    /// escalation, and it is why a reconnect clears it without any bookkeeping beyond `gone_since`.
    fn asks_for(&self, me: &str, now: u64) -> (Vec<chat::AskView>, Vec<chat::AskView>) {
        let (by_me, of_me): (Vec<chat::AskView>, Vec<chat::AskView>) = {
            let chat = self.inner.chat.lock().expect("the chat lock");
            let all = chat.asks_by_any();
            let mine: Vec<chat::AskView> = all
                .iter()
                .filter(|a| self.same_seat(&a.asker, me))
                .cloned()
                .collect();
            let theirs: Vec<chat::AskView> = all
                .iter()
                .filter(|a| self.same_seat(&a.target, me))
                .cloned()
                .collect();
            (mine, theirs)
        };
        (
            by_me.iter().map(|a| self.ask_view(a, me, now)).collect(),
            of_me.iter().map(|a| self.ask_view(a, me, now)).collect(),
        )
    }

    /// One ask as `me` reads it: the peer's absence, and whether it has gone on too long.
    fn ask_view(&self, ask: &chat::AskView, me: &str, now: u64) -> chat::AskView {
        let peer = ask.peer_of(me);
        let gone = self.gone_since(&peer);
        // A target that cannot act escalates at once; one that stops after the ask keeps the grace.
        let cannot = if ask.open() && peer == ask.target {
            self.cannot_act(&peer, gone, ask.seq, now)
        } else {
            String::new()
        };
        chat::AskView {
            gone_since_ms: gone,
            escalated: ask.open()
                && (!cannot.is_empty()
                    || gone.is_some_and(|g| now.saturating_sub(g) >= self.ask_grace_ms())),
            cannot,
            ..ask.clone()
        }
    }

    /// Why `target` cannot act on an ask now, empty when it can or its state is unknown.
    fn cannot_act(&self, target: &str, gone: Option<u64>, ask: u64, now: u64) -> String {
        if gone.is_some() {
            let absent = self
                .inner
                .asked_absent
                .lock()
                .expect("the asked-absent lock")
                .contains(&ask);
            return if absent {
                "was stopped when asked".into()
            } else {
                String::new()
            };
        }
        match self.seat_view(target, true, now).state {
            crate::seat::SeatState::RateLimited { .. } => "is rate-limited".into(),
            crate::seat::SeatState::ProviderError { .. } => "has a provider error".into(),
            _ => String::new(),
        }
    }

    /// The names closest to one nobody answers to, a few at most: the refusal points at `roster`
    /// for the rest, so the list stays short enough to read.
    fn closest(&self, written: &str) -> Vec<String> {
        let want = name_key(written);
        let mut known: Vec<(usize, String)> = self
            .top_handles()
            .into_iter()
            .map(|n| (name_distance(&want, &name_key(&n)), n))
            .collect();
        known.sort();
        known.into_iter().take(CLOSEST).map(|(_, n)| n).collect()
    }

    /// A candidate as a reader picks it: `` `handle` (id, "title") `` for a session the tree names,
    /// else the name itself. A candidate may be a SEAT id, whose local half is the session the tree
    /// knows, so both spellings find the row.
    fn who_is(&self, id: &str) -> String {
        let local = crate::SeatId::parse(id)
            .map(|s| s.local().to_string())
            .unwrap_or_else(|| id.to_string());
        let rooms = self.inner.rooms.lock().expect("the rooms lock");
        let member = rooms
            .values()
            .flat_map(|r| r.members().iter())
            .find(|m| m.session == local && m.group == crate::Group::Top);
        match member {
            Some(m) if !m.handle.is_empty() => format!("`{}` ({id}, \"{}\")", m.handle, m.label),
            Some(m) if !m.label.is_empty() => format!("{id} (\"{}\")", m.label),
            _ => id.to_string(),
        }
    }

    /// The top-level sessions this box has, as a reader names them - the list a refusal about an
    /// unknown handle hands back.
    fn top_handles(&self) -> Vec<String> {
        let mut known: Vec<String> = self.handles().into_values().collect();
        known.sort();
        known.dedup();
        known
    }

    /// The parent of `session` when it is a subagent: a subagent has no room connection, so nothing
    /// asked of it could ever be delivered.
    fn subagent_parent(&self, session: &str) -> Option<String> {
        let rooms = self.inner.rooms.lock().expect("the rooms lock");
        rooms.values().find_map(|r| {
            r.members()
                .iter()
                .find(|m| m.session == session && m.group == crate::Group::Sub)
                .map(|m| m.parent.clone().unwrap_or_default())
        })
    }

    /// `ask`: hold a question between two sessions. Every refusal the design names is decided here,
    /// before anything is written, because the asking session must learn it in the same call.
    #[allow(clippy::too_many_arguments)]
    fn ask_asked(
        &self,
        project: &str,
        seat: Seat,
        name: &str,
        id: &str,
        room: &str,
        to: &str,
        question: &str,
        now: u64,
    ) -> Response {
        // An ask is held by the room and answered to the asker as a dm, so a `cc@` seat may make
        // one whether or not it has a board: the bridge's session reads the answer with its inbox.
        if !matches!(seat, Seat::Session | Seat::External) {
            return Response::Refused {
                id: Some(id.to_string()),
                why: crate::Refused::AskNotASession,
            };
        }
        self.ask_to(project, seat, name, id, room, to, question, now)
    }

    /// [`Server::ask_asked`] once the seat has been judged: the ask itself, whichever door opened it.
    #[allow(clippy::too_many_arguments)]
    fn ask_to(
        &self,
        project: &str,
        seat: Seat,
        name: &str,
        id: &str,
        room: &str,
        to: &str,
        question: &str,
        now: u64,
    ) -> Response {
        let refuse = |why: crate::Refused| Response::Refused {
            id: Some(id.to_string()),
            why,
        };
        let asked = to.trim().trim_start_matches('@');
        let target = match self.resolve_name(project, asked, Reach::Host, Labels::Refuse) {
            Ok(t) => t,
            // A name nobody answers to is left to the block below, which tells another box's
            // `x@host` from a typo: both are the same question about the same name.
            Err(Miss::Unknown) => asked.to_string(),
            Err(miss @ Miss::Ambiguous(_)) => return refuse(miss.refused(asked, self)),
            Err(Miss::Elsewhere(sessions)) => {
                return refuse(crate::Refused::HandleInAnotherProject {
                    name: asked.to_string(),
                    sessions,
                });
            }
            Err(miss @ Miss::Label { .. }) => return refuse(miss.refused(asked, self)),
        };
        if target == name {
            return refuse(crate::Refused::AskSelf);
        }
        if target == chat::OPERATOR {
            return refuse(crate::Refused::AskOperator);
        }
        if let Some(parent) = self.subagent_parent(&target) {
            return refuse(crate::Refused::AskSubagent {
                session: target,
                parent,
            });
        }
        if !self.is_known(&target) {
            // A name of the shape `<session>@<host>` is another box's, and an ask is this box's
            // room: say which of the two refusals this is instead of calling it unknown.
            if asked.contains('@') && chat::bridge_parts(asked).is_none() {
                return refuse(crate::Refused::AskOtherHost {
                    name: asked.to_string(),
                });
            }
            return refuse(crate::Refused::AskUnknown {
                name: asked.to_string(),
                known: self.closest(asked),
            });
        }
        let question = question.trim();
        if question.is_empty() {
            return refuse(crate::Refused::ChatEmpty);
        }
        // Another project's queue seat sees no room of the asker's, so its question is held in
        // the host room, which both read.
        let room = match room.trim() {
            "" if queue_seat_project(&target).is_some_and(|k| k != project) => {
                chat::HOST_ROOM.to_string()
            }
            "" => Self::default_chat_room(seat, project),
            r => r.to_string(),
        };
        if room == chat::DM_ROOM || !self.may_see(name, seat, project, &room) {
            return refuse(Self::room_refusal(project, &room));
        }
        let (tseat, tproject) = self.seat_and_project_of(&target);
        if !self.may_see(&target, tseat, &tproject, &room) {
            return refuse(crate::Refused::AskCannotSee {
                session: target,
                room,
            });
        }
        // The same question already open is THAT ask: several rows carry one id and one settle
        // releases them all, so a second copy would be a second nag for one question.
        let existing = {
            let chat = self.inner.chat.lock().expect("the chat lock");
            chat.open_ask(name, &target, question)
        };
        let fresh = existing.is_none();
        let ask = match existing {
            Some(ask) => ask,
            None => {
                let appended = self
                    .inner
                    .chat
                    .lock()
                    .expect("the chat lock")
                    .append_ask(now, id, &room, name, &target, question);
                match appended {
                    Ok(ask) => ask,
                    Err(why) => return refuse(why),
                }
            }
        };
        // A push is a wake, not the record: the target's own pass reads the ask from `Asks`. So
        // the message goes out even when it repeats an open ask, and the target learns the ask
        // exists here rather than only in the ring.
        if fresh {
            let carried = self.inner.chat.lock().expect("the chat lock").read(
                &room,
                ask.seq.saturating_sub(1),
                None,
                None,
                None,
                |m| m.seq == ask.seq,
            );
            if let Some(message) = carried.rows.first() {
                self.deliver(message, now);
            }
        }
        // The grace starts at the ask when the target is not connected now, so the operator is
        // told at the same distance from the asking either way.
        if !self.conn_list().iter().any(|c| c.who.session == target) {
            self.inner
                .asked_absent
                .lock()
                .expect("the asked-absent lock")
                .insert(ask.seq);
            self.inner
                .gone_since
                .lock()
                .expect("the gone-since lock")
                .entry(target.clone())
                .or_insert(now);
        }
        Response::Asked {
            id: id.to_string(),
            ask: self.ask_view(&ask, name, now),
        }
    }

    /// `ask` by SCOPE: the owner the room resolves for a path or a topic, and the rule that
    /// resolved it. Nothing owning it is not a refusal - the question goes to the project room,
    /// where whoever knows it answers.
    #[allow(clippy::too_many_arguments)]
    fn ask_by_scope(
        &self,
        project: &str,
        seat: Seat,
        name: &str,
        id: &str,
        room: &str,
        path: &str,
        topic: &str,
        question: &str,
        now: u64,
    ) -> Response {
        let refuse = |why: crate::Refused| Response::Refused {
            id: Some(id.to_string()),
            why,
        };
        // A scope ask is the room's own routing, so a `cc@` seat may make one: the ask is held
        // here and answered to it, and no board of its own is needed to receive one.
        if !matches!(seat, Seat::Session | Seat::External) {
            return refuse(crate::Refused::AskNotASession);
        }
        // The room the ask lands in: the asker's own, unless it names one. A scope is resolved in
        // THAT room's project, and a question about a project's file is about that project's owners.
        let room = match room.trim() {
            "" => Self::default_chat_room(seat, project),
            r => r.to_string(),
        };
        if room == chat::DM_ROOM || !self.may_see(name, seat, project, &room) {
            return refuse(Self::room_refusal(project, &room));
        }
        let project = chat::project_of_room(&room).unwrap_or(project);
        let scope = match crate::scope::Scope::of(path, topic) {
            Ok(Some(scope)) => scope,
            Ok(None) => {
                return refuse(crate::Refused::AskBadScope {
                    why: "an `ask` names a session (`to`), a `path` or a `topic` - this one named \
                          nothing, so no question was held"
                        .to_string(),
                });
            }
            Err(why) => return refuse(crate::Refused::AskBadScope { why }),
        };
        let placed = self.place(project, &scope, name, now);
        self.note_count(match placed.rule() {
            crate::scope::Rule::Lease => crate::signal::Count::AskByLease,
            crate::scope::Rule::Edit => crate::signal::Count::AskByEdit,
            crate::scope::Rule::Owners => crate::signal::Count::AskByOwners,
            crate::scope::Rule::Room => crate::signal::Count::AskByRoom,
        });
        let why = placed.why(&scope);
        let Some(target) = placed.target().map(str::to_string) else {
            let mut answered = self.say_and_push(
                id,
                &room,
                name,
                seat,
                None,
                None,
                Vec::new(),
                Vec::new(),
                question,
                now,
            );
            if let Response::Said { note, .. } = &mut answered {
                *note = format!("{why} - the question is in `{room}` for whoever knows");
            }
            return answered;
        };
        match self.ask_to(project, seat, name, id, &room, &target, question, now) {
            Response::Asked { id, ask } => {
                let ask = self
                    .inner
                    .chat
                    .lock()
                    .expect("the chat lock")
                    .ask_held(ask.seq, &why)
                    .unwrap_or(ask);
                Response::Asked { id, ask }
            }
            other => other,
        }
    }

    /// Where an ask by scope goes, from the room's own facts: this project's live leases, its
    /// sessions' edits, the repo's `OWNERS` file - and never the asker itself.
    fn place(
        &self,
        project: &str,
        scope: &crate::scope::Scope,
        asker: &str,
        now: u64,
    ) -> crate::scope::Placement {
        let claims = self.claims_in(project);
        let edits = self.edits_in(project);
        let owners = self.owners_file(project);
        crate::scope::place(scope, &claims, &edits, &owners, now, |who| {
            who != asker
                && who != chat::OPERATOR
                && self.is_known(who)
                && self.subagent_parent(who).is_none()
        })
    }

    /// The edits this project's sessions reported, newest last: the last-edit rule's facts, which
    /// the room already keeps to renew a path claim.
    fn edits_in(&self, project: &str) -> Vec<(String, String, u64)> {
        let here: Vec<String> = self
            .conn_list()
            .iter()
            .filter(|c| c.project == project)
            .map(|c| c.who.session.clone())
            .collect();
        let seats = self.inner.seats.lock().expect("the seats lock");
        let mut edits = Vec::new();
        for session in here {
            let Some((facts, _)) = seats.get(&session) else {
                continue;
            };
            for edit in &facts.edits {
                edits.push((session.clone(), edit.path.clone(), edit.at_ms));
            }
        }
        edits
    }

    /// The repo's `OWNERS` file, from the project's own folder. Empty when the folder is not known
    /// or the file is not there: the rule is skipped, never guessed.
    fn owners_file(&self, project: &str) -> String {
        let root = {
            let rooms = self.inner.rooms.lock().expect("the rooms lock");
            rooms
                .get(project)
                .map(|r| r.root().to_string())
                .unwrap_or_default()
        };
        if root.is_empty() {
            return String::new();
        }
        match std::fs::read_to_string(std::path::Path::new(&root).join("OWNERS")) {
            Ok(text) if text.len() <= crate::scope::OWNERS_MAX => text,
            _ => String::new(),
        }
    }

    /// `reply` and `decline`: the target's answer. Posted as a thread reply in the ask's room,
    /// addressed to the asker, and only then does the ask settle - a plain thread reply does not.
    #[allow(clippy::too_many_arguments)]
    fn ask_settled(
        &self,
        name: &str,
        seat: Seat,
        id: &str,
        ask: u64,
        text: &str,
        answered: bool,
        now: u64,
    ) -> Response {
        let refuse = |why: crate::Refused| Response::Refused {
            id: Some(id.to_string()),
            why,
        };
        let Some(found) = self.inner.chat.lock().expect("the chat lock").ask(ask) else {
            return refuse(crate::Refused::UnknownAsk { ask });
        };
        if !self.same_seat(&found.target, name) {
            return refuse(crate::Refused::AskNotTheTarget {
                ask,
                target: self.handles().remove(&found.target).unwrap_or(found.target),
                state: Some(found.state),
            });
        }
        if !found.open() {
            return refuse(crate::Refused::AskSettled {
                ask,
                state: found.state.line(),
            });
        }
        let text = text.trim();
        if text.is_empty() {
            return refuse(crate::Refused::ChatEmpty);
        }
        let state = match answered {
            true => chat::AskState::Answered {
                text: text.to_string(),
                ms: now,
            },
            false => chat::AskState::Declined {
                text: text.to_string(),
                ms: now,
            },
        };
        // The reply says the ask is settled in its own words: the asker may have had plain replies
        // in the thread already, and only this one ends the wait.
        let who = self
            .handles()
            .remove(name)
            .unwrap_or_else(|| name.to_string());
        let said = match answered {
            true => format!("`{who}` answered your ask #{ask}: {text}"),
            false => format!("`{who}` declined your ask #{ask}: {text}"),
        };
        let appended = self.inner.chat.lock().expect("the chat lock").append(
            now,
            id,
            &found.room,
            name,
            seat,
            Some(found.asker.clone()),
            Some(found.seq),
            Vec::new(),
            Vec::new(),
            &said,
        );
        let message = match appended {
            Ok(m) => m,
            Err(why) => return refuse(why),
        };
        self.inner
            .chat
            .lock()
            .expect("the chat lock")
            .settle_ask(ask, state);
        self.deliver(&message, now);
        // A consent ask's answer IS the consent: `allow once` is one message, `allow for the
        // session` stays. Anything else, or a decline, leaves the pair where it was.
        if let Some((from, to)) = self
            .inner
            .consent_asks
            .lock()
            .expect("the consent lock")
            .remove(&ask)
        {
            let word = text.to_lowercase();
            let allowed = answered
                && (word.contains("allow once")
                    || word.contains("allow for the session")
                    || word == "allow");
            if allowed {
                let session = word.contains("session");
                self.inner
                    .chat
                    .lock()
                    .expect("the chat lock")
                    .allow(&from, &to, session);
                self.say_consent(&from, &to, session, true);
            }
        }
        self.inner.signals.lock().expect("the signals lock").act(
            ask,
            crate::signal::Act::AskSettled,
            name,
            now,
        );
        let settled = self
            .inner
            .chat
            .lock()
            .expect("the chat lock")
            .ask(ask)
            .unwrap_or(found);
        Response::Asked {
            id: id.to_string(),
            ask: self.ask_view(&settled, name, now),
        }
    }

    /// `withdraw`: the asker or the operator takes the question back, and the thread says so.
    fn ask_withdrawn(&self, name: &str, seat: Seat, id: &str, ask: u64, now: u64) -> Response {
        let refuse = |why: crate::Refused| Response::Refused {
            id: Some(id.to_string()),
            why,
        };
        let Some(found) = self.inner.chat.lock().expect("the chat lock").ask(ask) else {
            return refuse(crate::Refused::UnknownAsk { ask });
        };
        if !self.same_seat(&found.asker, name) && seat != Seat::Operator {
            return refuse(crate::Refused::AskNotYours {
                ask,
                asker: found.asker,
            });
        }
        if !found.open() {
            return refuse(crate::Refused::AskSettled {
                ask,
                state: found.state.line(),
            });
        }
        let by = if found.asker == name {
            name.to_string()
        } else {
            chat::OPERATOR.to_string()
        };
        self.inner.chat.lock().expect("the chat lock").settle_ask(
            ask,
            chat::AskState::Withdrawn {
                by: by.clone(),
                ms: now,
            },
        );
        // A system note in the thread, so a question the target was nagged about does not simply
        // vanish from under it.
        let note = self.inner.chat.lock().expect("the chat lock").append(
            now,
            id,
            &found.room,
            chat::DIGEST_AUTHOR,
            Seat::System,
            None,
            Some(found.seq),
            Vec::new(),
            Vec::new(),
            &format!("ask #{ask} was withdrawn by {by}"),
        );
        if let Ok(message) = note {
            self.deliver(&message, now);
        }
        let settled = self
            .inner
            .chat
            .lock()
            .expect("the chat lock")
            .ask(ask)
            .unwrap_or(found);
        Response::Asked {
            id: id.to_string(),
            ask: self.ask_view(&settled, name, now),
        }
    }

    /// `retire`: a session is deleted, so nothing asked of it or by it can ever be answered. The one
    /// `DOWN`, and the only ending a liveness fact may cause - a crash is a restart, not a goodbye.
    fn ask_retired(&self, name: &str, id: &str, session: &str, now: u64) -> Response {
        let session = session.trim();
        if session != name {
            return Response::Refused {
                id: Some(id.to_string()),
                why: crate::Refused::NotTheOwners {
                    session: session.to_string(),
                },
            };
        }
        let (of_me, by_me) = {
            let chat = self.inner.chat.lock().expect("the chat lock");
            (chat.asks_of(session), chat.asks_by(session))
        };
        for ask in of_me.iter().filter(|a| a.open()) {
            self.inner
                .chat
                .lock()
                .expect("the chat lock")
                .settle_ask(ask.seq, chat::AskState::Down { ms: now });
        }
        // A deleted asker withdraws its asks: its rows are going with it, so nothing is left to
        // wait, and the target's nag ends with the note that it was taken back.
        for ask in by_me.iter().filter(|a| a.open()) {
            self.ask_withdrawn(session, Seat::Session, id, ask.seq, now);
        }
        // And a consent is between two seats, so it ends with either of them: "allow for the
        // session" is a sentence about a session that is now gone.
        let gone = self
            .inner
            .chat
            .lock()
            .expect("the chat lock")
            .consents_ending_with(session);
        for (from, to) in gone {
            self.say_consent(&from, &to, false, false);
        }
        let (by_me, of_me) = self.asks_for(name, now);
        Response::Asks { by_me, of_me }
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

    /// The accept loop on a manager's listener: polled, so a stop is seen within a tick and a
    /// connection that arrives after it stays queued in the kernel for the next coordinator.
    fn spawn_accept_polled(&self, listener: UnixListener) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        listener.set_nonblocking(true)?;
        let me = self.clone();
        let h = std::thread::Builder::new()
            .name("room-accept".into())
            .spawn(move || {
                let mut p = libc::pollfd {
                    fd: listener.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                while !me.inner.stop.load(Ordering::SeqCst) {
                    p.revents = 0;
                    // SAFETY: one live pollfd.
                    let n = unsafe { libc::poll(&mut p, 1, ACCEPT_TICK_MS) };
                    if n <= 0 || me.inner.stop.load(Ordering::SeqCst) {
                        continue;
                    }
                    while let Ok((stream, _)) = listener.accept() {
                        let _ = stream.set_nonblocking(false);
                        Server::clone(&me).serve_one(stream);
                    }
                }
            })?;
        *self.inner.accept.lock().expect("the accept lock") = Some(h);
        Ok(())
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
                    ReadFrame::Frame(frame) => *frame,
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
                    // A frame this coordinator cannot decode is answered before the close: the
                    // client would otherwise see a dead socket where a refusal belongs.
                    ReadFrame::Bad { word, why } => {
                        let mut w = writer;
                        let _ = write_frame(
                            &mut w,
                            &Response::Bye {
                                why: unreadable_sentence(&word, &why),
                            },
                        );
                        let _ = w.shutdown(std::net::Shutdown::Both);
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
                let (speaker, seat, family, seat_id, seat_nick) = match &hello {
                    Request::Hello {
                        speaker,
                        seat,
                        family,
                        id,
                        nick,
                        ..
                    } => (
                        speaker.clone(),
                        *seat,
                        family_of(*seat, &speaker.session, family.as_deref()),
                        id.trim().to_string(),
                        nick.trim().to_string(),
                    ),
                    _ => return,
                };
                // A reader that named no seat of its own is the seat this name already speaks as:
                // the roster draws it as that seat and the cursor is the seat's. A name nothing
                // holds is the reader's alone until a seat arrives.
                let seat_id = match seat_id.is_empty() {
                    true => me.id_held_for(&speaker.session),
                    false => seat_id,
                };
                // A nick that is not a label is no label: the name it speaks as is drawn instead.
                // And a label **another** live seat already answers to is dropped rather than taken:
                // the hello is not the place to argue about a name, and its own `nick` frame is.
                let mine = crate::SeatId::parse(&seat_id)
                    .map(|i| i.to_string())
                    .unwrap_or_default();
                let seat_nick = if crate::chat::valid_nick(&seat_nick)
                    && me.nick_holder(&seat_nick, &mine).is_none()
                {
                    seat_nick
                } else {
                    String::new()
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
                // A `Bye` or a refusal is the end of the connection: there is no session here.
                if !matches!(&answer.response, Response::Welcome { .. }) {
                    let _ = write_frame(&mut w, &answer.response);
                    let _ = w.shutdown(std::net::Shutdown::Both);
                    return;
                }
                let id = me.inner.next_conn.fetch_add(1, Ordering::SeqCst) + 1;
                // From here on, this connection's frames go through ONE writer thread, and the
                // room's threads only ever enqueue.
                let Ok(their_writer) = w.try_clone() else {
                    // A socket that cannot be cloned has no writer thread, so there is nobody to
                    // send a frame and nothing to keep: the close is the only thing left to say.
                    let _ = w.shutdown(std::net::Shutdown::Both);
                    return;
                };
                let (outbox, frames) =
                    std::sync::mpsc::sync_channel(me.inner.outbox.load(Ordering::SeqCst));
                // The `Welcome` is the first queued frame and is written only once the connection
                // is registered below: a client that has read it may say to this name at once.
                let _ = outbox.try_send(answer.response.clone());
                let conn = Arc::new(Conn {
                    who: speaker.clone(),
                    seat,
                    project: project.clone(),
                    outbox,
                    socket: w,
                    gone: AtomicBool::new(false),
                    family,
                    id: seat_id.clone(),
                    nick: seat_nick.clone(),
                });
                me.inner
                    .conns
                    .lock()
                    .expect("the connections lock")
                    .insert(id, Arc::clone(&conn));
                me.inner.served.fetch_add(1, Ordering::SeqCst);
                // A connection IS the session's liveness: one just arrived, so it is not gone and
                // every ask on it stops escalating now.
                me.inner
                    .gone_since
                    .lock()
                    .expect("the gone-since lock")
                    .remove(&speaker.session);
                // A seat's cursor is keyed by its id, and its id is what its `Hello` said: a
                // reconnect under another label comes back to the cursor it left.
                if !seat_id.is_empty() {
                    me.inner
                        .ids_of
                        .lock()
                        .expect("the ids lock")
                        .insert(speaker.session.clone(), seat_id.clone());
                }
                let principal = me.principal_of(&speaker.session);
                {
                    let mut chat = me.inner.chat.lock().expect("the chat lock");
                    chat.arm(&principal);
                    // A store written before cursors were keyed by the seat holds this bridge's
                    // handle: the handle is the seat that is connecting now, so its cursor moves.
                    chat.adopt(&speaker.session, &principal);
                }
                // What the bound in `principal_refusal` is counted over: the names this room has a
                // cursor for. A name is added once and never twice, so the count is principals.
                me.inner
                    .armed
                    .lock()
                    .expect("the armed lock")
                    .insert(principal.clone());
                if seat == Seat::External {
                    me.inner
                        .externals
                        .lock()
                        .expect("the externals lock")
                        .insert(speaker.session.clone());
                }
                // What was said while this principal had no connection, pushed now that it has
                // one. After the `Welcome`, and after the connection is registered, so a message
                // said during the delivery reaches it by one door or the other and not neither.
                spawn_writer(speaker.session.clone(), their_writer, frames);
                me.deliver_pending(&conn, &speaker.session);

                // The read loop. Every frame is answered, and every event the answer carries
                // is routed to the connections it is about - including this one, because a claim
                // by `s-b` is news to `s-a`.
                loop {
                    if me.inner.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let frame = match read_frame(&mut reader) {
                        ReadFrame::Frame(f) => *f,
                        // A line past the bound is answered before the close: the peer is told
                        // what it sent and what the room takes, rather than reading a bare close.
                        ReadFrame::OverLong(got) => {
                            conn.send(Response::Bye {
                                why: over_long_sentence(got),
                            });
                            break;
                        }
                        ReadFrame::End => break,
                        // Same as the door's: a line that is not a frame still gets a sentence
                        // naming it, because a client reads a bare close as a dead coordinator.
                        ReadFrame::Bad { word, why } => {
                            conn.send(Response::Bye {
                                why: unreadable_sentence(&word, &why),
                            });
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

                // The close is a fact - the daemon exited, restarted or was stopped - so every claim
                // goes back to the queue now, not ninety seconds from now. One close is not that fact:
                // a stopping coordinator shut this socket itself, so the lease decides.
                me.inner
                    .chat
                    .lock()
                    .expect("the chat lock")
                    .keep_seen(&me.principal_of(&speaker.session));
                me.inner
                    .conns
                    .lock()
                    .expect("the connections lock")
                    .remove(&id);
                // The last connection closing is what *gone* means, and it is the only liveness
                // fact an ask escalates on. A name with another connection left (a bridge and its
                // waiter) is still here.
                if !me
                    .conn_list()
                    .iter()
                    .any(|c| c.who.session == speaker.session)
                {
                    me.inner
                        .gone_since
                        .lock()
                        .expect("the gone-since lock")
                        .insert(speaker.session.clone(), now_ms());
                }
                // An external that has gone is not known any more: the set is what keeps `say to`
                // from refusing a name that can answer, and a name with no connection and no tree
                // row answers nothing. Its next `Hello` adds it back.
                if seat == Seat::External
                    && !me
                        .conn_list()
                        .iter()
                        .any(|c| c.who.session == speaker.session)
                {
                    me.inner
                        .externals
                        .lock()
                        .expect("the externals lock")
                        .remove(&speaker.session);
                }
                if seat.is_wide() || me.inner.stop.load(Ordering::SeqCst) {
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
        let session = self.principal_of(session);
        let session = session.as_str();
        let mut missed = self.inner.missed.lock().expect("the missed lock");
        if !missed.contains_key(session) {
            let max = self.inner.principals.load(Ordering::SeqCst);
            if missed.len() >= max
                && let Some((gone, was)) = missed
                    .iter()
                    .min_by_key(|(_, m)| m.since_ms)
                    .map(|(k, m)| (k.clone(), *m))
            {
                // The map is per session and a session may be named anything, so the oldest gap
                // goes rather than the map growing: a count nobody came back for is worth less
                // than the room's memory, and the number is said on the log, not dropped silently.
                missed.remove(&gone);
                eprintln!(
                    "room: forgetting that `{gone}` missed {} event(s); the room tracks {max} \
                     sessions",
                    was.count
                );
            }
        }
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

/// `text` with the whole-word `@from` mentions made `@to`.
fn replace_mention(text: &str, from: &str, to: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let needle = format!("@{from}");
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut at = 0;
    while let Some(i) = lower
        .get(at..)
        .and_then(|r| r.find(&needle))
        .map(|i| i + at)
    {
        let end = i + needle.len();
        let before_ok = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        let after_ok = !bytes.get(end).is_some_and(|b| {
            b.is_ascii_alphanumeric() || matches!(*b, b'-' | b'_' | b'@' | b':' | b'*')
        });
        out.push_str(&text[at..i]);
        if before_ok && after_ok {
            out.push('@');
            out.push_str(to);
        } else {
            out.push_str(&text[i..end]);
        }
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

/// What a short bridge name stands for.
enum Lookup {
    /// The one bridge, by its full handle.
    One(String),
    /// Every connected bridge of one folder.
    Group(String, Vec<String>),
    /// Several bridges that share the name.
    Many(Vec<String>),
}

/// How far a written name may reach. A dm is one session talking to one other, so it keeps to
/// its own project's names; an ask, an invite and every seat outside a project may name one on the
/// host.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    OwnProject,
    Host,
}

/// What a word that turns out to be a seat's LABEL means. A `to`, a dm and an ask are addresses, and
/// an address is an id: the label is refused with the id it would have meant, so routing never
/// depends on a name a rename can change. A mention inside a sentence is REWRITTEN instead - that line
/// is the sender's, and throwing it back over a label would lose what they said.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Labels {
    Refuse,
    Rewrite,
}

/// How many near names an unknown-name refusal lists.
const CLOSEST: usize = 5;

/// A name as a person writes it, folded: lowercase, every run of other characters one `-`. A
/// title `Rano Session` and the handle `rano-session` fold to the same key.
fn name_key(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// How far apart two folded names are: 0 for one inside the other, else one more than the edit
/// distance to the nearest of `b`, its words and its head, so a typo of one word still ranks.
fn name_distance(a: &str, b: &str) -> usize {
    if !a.is_empty() && !b.is_empty() && (a.contains(b) || b.contains(a)) {
        return 0;
    }
    let head: String = b.chars().take(a.chars().count()).collect();
    std::iter::once(b)
        .chain(b.split('-'))
        .chain(std::iter::once(head.as_str()))
        .map(|part| edit_distance(a, part) + 1)
        .min()
        .unwrap_or(usize::MAX)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (prev + usize::from(ca != *cb))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            prev = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// Why a written name named nobody, or more than one, and what would have named somebody.
enum Miss {
    /// Several principals fit it: these are the names to choose between.
    Ambiguous(Vec<String>),
    /// Nobody the sender may address answers to it.
    Unknown,
    /// It is a handle, but another project's: these are the session ids that go by it.
    Elsewhere(Vec<String>),
    /// It is the LABEL of `id`: a name the room draws, which is not an address.
    Label { name: String, id: String },
}

impl Miss {
    /// The refusal a door answers with. One place, so a door that forgets to say what it *could*
    /// have been told is a door that does not compile.
    fn refused(self, written: &str, server: &Server) -> crate::Refused {
        match self {
            Miss::Ambiguous(candidates) => crate::Refused::Ambiguous {
                name: written.to_string(),
                shown: candidates.iter().map(|c| server.who_is(c)).collect(),
                candidates,
            },
            Miss::Unknown => crate::Refused::UnknownAddressee {
                name: written.to_string(),
                known: server.closest(written),
            },
            Miss::Elsewhere(sessions) => crate::Refused::HandleInAnotherProject {
                name: written.to_string(),
                sessions,
            },
            Miss::Label { name, id } => crate::Refused::NameIsLabel { name, id },
        }
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
            | Request::Search { .. }
            | Request::Dms { .. }
            | Request::Roster { .. }
            | Request::MayPrompt { .. }
            | Request::Inbox { .. }
            | Request::Ack { .. }
            | Request::Nick { .. }
            | Request::Signals
            | Request::Acted { .. }
            | Request::WakeEnded { .. }
            | Request::Invite { .. }
            | Request::PutBlob { .. }
            | Request::GetBlob { .. }
            | Request::Ask { .. }
            | Request::Reply { .. }
            | Request::Decline { .. }
            | Request::Withdraw { .. }
            | Request::Retire { .. }
            | Request::Asks
            | Request::Leave { .. }
            | Request::ArchiveRoom { .. }
            | Request::UnarchiveRoom { .. }
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
/// (which is answered with a sentence before the close), and a line that is not a frame. The frame
/// is boxed because a `Request` is the one large variant, and every connection carries this enum
/// through a match per line it reads.
enum ReadFrame {
    Frame(Box<Request>),
    End,
    OverLong(usize),
    /// A line that is not a frame: the `type` word when the line was JSON with one, and what the
    /// decoder said.
    Bad {
        word: String,
        why: String,
    },
}

fn read_frame(reader: &mut BufReader<UnixStream>) -> ReadFrame {
    use std::io::Read;
    let mut line = Vec::new();
    // One byte past the bound is what tells a long line from a line too long: a frame of exactly
    // the bound ends in its newline inside the window, and anything else does not.
    let window = (MAX_FRAME_BYTES + 1) as u64;
    let n = match (&mut *reader).take(window).read_until(b'\n', &mut line) {
        Ok(n) => n,
        Err(e) => {
            return ReadFrame::Bad {
                word: String::new(),
                why: e.to_string(),
            };
        }
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
        Ok(request) => ReadFrame::Frame(Box::new(request)),
        Err(e) => {
            // The word is what lets the refusal say *which* frame was unreadable; a line that is
            // not JSON at all has none, and the decoder's own words are all there is.
            let word = serde_json::from_str::<serde_json::Value>(text)
                .ok()
                .and_then(|v| v["type"].as_str().map(str::to_string))
                .unwrap_or_default();
            ReadFrame::Bad {
                word,
                why: e.to_string(),
            }
        }
    }
}

/// The sentence a line this coordinator cannot decode is refused with. A client that only saw its
/// connection close cannot tell a dead coordinator from a frame it was too old to read.
fn unreadable_sentence(word: &str, why: &str) -> String {
    let named = match word.is_empty() {
        true => "a line that is not a frame".to_string(),
        false => format!("a `{word}` frame"),
    };
    format!(
        "this room speaks protocol {ROOM_PROTOCOL_VERSION} and could not read {named} ({why}): \
         nothing was done and the connection is closed. A frame this build does not know belongs \
         to a newer client, so the two halves are different builds"
    )
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

/// May a line of `kind` be moved into the dm it answers? A `blocker` is the operator's to see
/// whatever else it is, and an `announce` is one line for a whole room by construction; every
/// other kind is the sender's answer to say privately.
fn answers_the_dm(kind: crate::SayKind) -> bool {
    !matches!(kind, crate::SayKind::Blocker | crate::SayKind::Announce)
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
        assert!(said.contains("no item store"), "{said}");
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
            share: false,
            family: None,
            id: String::new(),
            nick: String::new(),
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
            assert!(said.contains("no item store"), "{said}");
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
                share: false,
                family: None,
                id: String::new(),
                nick: String::new(),
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
                share: false,
                family: None,
                id: String::new(),
                nick: String::new(),
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
                share: false,
                family: None,
                id: String::new(),
                nick: String::new(),
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
                assert!(identity.contains("no item store"), "{identity}");
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
    fn the_room_speaks_protocol_fourteen() {
        assert_eq!(ROOM_PROTOCOL_VERSION, 14);
        // And the refusal names which side is older, in both directions.
        assert_eq!(
            crate::wire::protocol_refusal(11),
            None,
            "11 is still served"
        );
        let old = crate::wire::protocol_refusal(10).expect("10 is too old");
        assert!(old.contains("the client is the older"), "{old}");
        let new = crate::wire::protocol_refusal(15).expect("15 is not 14");
        assert!(new.contains("this room is the older"), "{new}");
        assert_eq!(crate::wire::protocol_refusal(12), None, "12 is served too");
        assert!(
            new.contains("letibot --room --stop && letibot --room"),
            "{new}"
        );
        assert!(old.contains("letibot --stop"), "{old}");
    }

    /// An older coordinator's own sentence still gets the exact command from the client.
    #[test]
    fn a_refusal_from_an_older_coordinator_gets_the_command() {
        let theirs = "this room speaks protocol 1 and the client speaks 8 — this room is the older of the two, so nothing was done: upgrade this room";
        assert_eq!(
            crate::wire::upgrade_hint(theirs),
            Some(crate::wire::ROOM_UPGRADE)
        );
        let newer = "this room speaks protocol 9 and the client speaks 8 — x";
        assert_eq!(
            crate::wire::upgrade_hint(newer),
            Some(crate::wire::CLIENT_UPGRADE)
        );
        assert_eq!(crate::wire::upgrade_hint("some other refusal"), None);
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
        // The bell arrives on the poster's connection thread, so wait for it rather than guess: a
        // loaded box must not trip this, and a room that never routed still ends here.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
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
            family: None,
            id: String::new(),
            nick: String::new(),
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

    // ===== The blob store on the wire =====

    /// A server whose blobs live in a directory of their own, so a test never writes beside a
    /// socket another test may be using.
    fn with_blobs(tag: &str) -> (Server, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("letibot-room-blob-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = Server::serve(&dir.join("room.sock"), Room::new()).expect("a socket");
        server.with_blob_store(&dir.join("room-test.blobs"));
        (server, dir)
    }

    /// One PNG of the given size, uploaded through the frames a head sends: `PutBlob` in parts and
    /// the last one finishes it.
    fn upload(
        server: &Server,
        me: &Speaker,
        upload: &str,
        png: &[u8],
        now: u64,
    ) -> chat::Attachment {
        let chunks: Vec<&[u8]> = png.chunks(crate::blob::MAX_PART).collect();
        let mut attachment = None;
        for (i, chunk) in chunks.iter().enumerate() {
            let answer = server.answer_frame(
                crate::DEFAULT_PROJECT,
                Seat::Session,
                me,
                &Request::PutBlob {
                    upload: upload.into(),
                    name: "shot.png".into(),
                    mime: "image/png".into(),
                    part: i as u32,
                    last: i + 1 == chunks.len(),
                    data: crate::blob::encode_base64(chunk),
                },
                now,
            );
            match answer.response {
                Response::Blob { attachment: a } => attachment = a,
                other => panic!("part {i} is answered with a blob: {other:?}"),
            }
        }
        attachment.expect("the last part finished the upload")
    }

    /// A message names a blob by id and carries what the store holds for it: the picture travels
    /// with the row, so a reader that has the history has the bytes' description too.
    #[test]
    fn a_say_with_a_blob_carries_its_attachment() {
        let (server, dir) = with_blobs("say");
        let me = Speaker::top_level("s-a");
        let png: Vec<u8> = {
            // A PNG's first chunk is IHDR: the signature, its length, its type, then the size.
            let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
            v.extend_from_slice(&13u32.to_be_bytes());
            v.extend_from_slice(b"IHDR");
            v.extend_from_slice(&40u32.to_be_bytes());
            v.extend_from_slice(&20u32.to_be_bytes());
            v.extend_from_slice(b"\x08\x02\x00\x00\x00");
            v
        };
        let blob = upload(&server, &me, "up1", &png, 1_000);
        assert_eq!(blob.w, Some(40), "the header is read, not guessed");
        assert_eq!(blob.h, Some(20));
        let said = server.answer_frame(
            crate::DEFAULT_PROJECT,
            Seat::Session,
            &me,
            &Request::Say {
                id: "1".into(),
                room: chat::HOST_ROOM.into(),
                text: "@s-a look: ![shot](blob:ID)".into(),
                to: None,
                thread: None,
                refs: Vec::new(),
                attachments: vec![blob.id.clone()],
                kind: Default::default(),
            },
            1_001,
        );
        match said.response {
            Response::Said { message, .. } => {
                assert_eq!(message.attachments.len(), 1);
                assert_eq!(message.attachments[0], blob, "the store's own record");
                assert_eq!(message.attachments[0].bytes, png.len() as u64);
            }
            other => panic!("a say is answered with the message: {other:?}"),
        }
        // And a read carries it too: the attachment is on the row, not a second fetch.
        let read = server.answer_frame(
            crate::DEFAULT_PROJECT,
            Seat::Session,
            &me,
            &Request::Read {
                room: chat::HOST_ROOM.into(),
                since: 0,
                last: None,
                thread: None,
                before: None,
            },
            1_002,
        );
        match read.response {
            Response::History(h) => assert_eq!(h.rows[0].attachments, vec![blob]),
            other => panic!("a read is answered with history: {other:?}"),
        }
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An id the store does not hold is refused rather than dropped: a message that names bytes
    /// nobody can fetch is a line a reader would draw as a broken picture for ever.
    #[test]
    fn a_say_naming_an_unknown_blob_is_refused() {
        let (server, dir) = with_blobs("unknown");
        let me = Speaker::top_level("s-a");
        let answer = server.answer_frame(
            crate::DEFAULT_PROJECT,
            Seat::Session,
            &me,
            &Request::Say {
                id: "1".into(),
                room: chat::HOST_ROOM.into(),
                text: "here it is".into(),
                to: None,
                thread: None,
                refs: Vec::new(),
                attachments: vec!["a".repeat(64)],
                kind: Default::default(),
            },
            1_000,
        );
        match answer.response {
            Response::Refused { why, .. } => {
                assert!(
                    matches!(&why, crate::Refused::UnknownBlob { id } if *id == "a".repeat(64)),
                    "{why}"
                );
                assert!(why.to_string().contains(&"a".repeat(64)), "{why}");
            }
            other => panic!("an unknown blob is refused: {other:?}"),
        }
        // Nothing was appended: a refused say leaves the room as it was.
        assert_eq!(server.chat_head(), 0);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What `put_blob` stored is what `get_blob` answers, part by part, and the last part says so -
    /// the reader stops asking because the store told it to, not because the bytes ran out.
    #[test]
    fn get_blob_returns_what_put_blob_stored() {
        let (server, dir) = with_blobs("get");
        let me = Speaker::top_level("s-a");
        // Two parts, so the second read is the one that carries `last`.
        let bytes: Vec<u8> = (0..(crate::blob::MAX_PART + 100))
            .map(|i| (i % 251) as u8)
            .collect();
        let blob = upload(&server, &me, "up2", &bytes, 1_000);
        let mut got: Vec<u8> = Vec::new();
        let mut offset = 0u64;
        loop {
            let answer = server.answer_frame(
                crate::DEFAULT_PROJECT,
                Seat::Session,
                &me,
                &Request::GetBlob {
                    id: blob.id.clone(),
                    offset,
                },
                1_001,
            );
            match answer.response {
                Response::BlobPart {
                    attachment,
                    offset: at,
                    data,
                    last,
                } => {
                    assert_eq!(attachment.id, blob.id);
                    assert_eq!(at, offset, "the store answers from where it was asked");
                    let part = crate::blob::decode_base64(&data);
                    assert!(part.len() <= crate::blob::MAX_PART, "one frame, one part");
                    got.extend_from_slice(&part);
                    offset = at + part.len() as u64;
                    if last {
                        break;
                    }
                }
                other => panic!("a get is answered with bytes: {other:?}"),
            }
        }
        assert_eq!(got, bytes, "the same bytes, in the same order");
        // An id nobody uploaded is a refusal, not an empty part.
        let answer = server.answer_frame(
            crate::DEFAULT_PROJECT,
            Seat::Session,
            &me,
            &Request::GetBlob {
                id: "b".repeat(64),
                offset: 0,
            },
            1_002,
        );
        assert!(
            matches!(answer.response, Response::Refused { .. }),
            "{:?}",
            answer.response
        );
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The store keeps pictures and plain text, and says which of those it keeps. A frame that
    /// offers anything else is told the list rather than quietly stored.
    #[test]
    fn a_wrong_mime_is_refused() {
        let (server, dir) = with_blobs("mime");
        let me = Speaker::top_level("s-a");
        let answer = server.answer_frame(
            crate::DEFAULT_PROJECT,
            Seat::Session,
            &me,
            &Request::PutBlob {
                upload: "up3".into(),
                name: "run.sh".into(),
                mime: "application/x-sh".into(),
                part: 0,
                last: true,
                data: crate::blob::encode_base64(b"#!/bin/sh\n"),
            },
            1_000,
        );
        match answer.response {
            Response::Refused { why, .. } => {
                assert!(
                    matches!(&why, crate::Refused::BadMime { mime } if mime == "application/x-sh"),
                    "{why}"
                );
                let sentence = why.to_string();
                assert!(sentence.contains("image/png"), "{sentence}");
                assert!(sentence.contains("text/plain"), "{sentence}");
            }
            other => panic!("a wrong mime is refused: {other:?}"),
        }
        // A picture is what the list is for, and it goes through.
        let png = upload(&server, &me, "up4", b"\x89PNG\r\n\x1a\n", 1_001);
        assert_eq!(png.mime, "image/png");
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Sleep until the room's millisecond clock has moved, so a second `now` is a second `now`.
    /// Not a pause: the longest this can wait is the next tick of a clock that is always running.
    fn next_millisecond() {
        let at = now_ms();
        while now_ms() == at {
            std::hint::spin_loop();
        }
    }

    /// Everything the box's own room holds, oldest first - what the operator was told.
    fn host_room(server: &Server) -> Vec<chat::Message> {
        server
            .inner
            .chat
            .lock()
            .expect("the chat lock")
            .read(chat::HOST_ROOM, 0, None, None, None, |_| true)
            .rows
    }

    #[test]
    fn a_beacon_notice_is_raised_once_and_a_link_clears_it_once() {
        let dir = std::env::temp_dir().join(format!("letibot-room-beacon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let server = Server::serve(&dir.join("room.sock"), Room::new()).expect("a socket");
        let cut = |why: &str| PeerEvent::BeaconCut {
            why: why.to_string(),
        };
        // A peer's loop says this once per outage, and the room holds the same rule: the line is
        // the operator's, and a second one says nothing new.
        server.on_peer_event(cut("no route to host"));
        next_millisecond();
        server.on_peer_event(cut("no route to host"));
        let said = host_room(&server);
        assert_eq!(said.len(), 1, "{said:?}");
        assert_eq!(said[0].kind, Seat::System);
        assert_eq!(said[0].say_kind(), crate::SayKind::Blocker);
        assert_eq!(said[0].to.as_deref(), Some(chat::OPERATOR));
        assert!(said[0].text.contains("no route to host"), "{:?}", said[0]);
        // A box that links is the fact the notice asked about, so it is what clears it.
        server.on_peer_event(PeerEvent::Up {
            host: "mac1".into(),
        });
        let said = host_room(&server);
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(
            said[1].text.contains("announce itself on the LAN again"),
            "{:?}",
            said[1]
        );
        assert!(said[1].text.contains("mac1 linked"), "{:?}", said[1]);
        // And a second link, with no notice standing, says nothing.
        server.on_peer_event(PeerEvent::Up {
            host: "lab2x1".into(),
        });
        assert_eq!(host_room(&server).len(), 2);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
