//! **The coordinator's socket** — the accept loop, one read loop per connection, the [`Room`]
//! behind a single `&mut` in one place, the clock, and the routing of a bell to the connections it
//! is about.
//!
//! `docs/room.md` § *What the daemon still owes* items 1–3, in one file:
//!
//! 1. **The socket and the daemon** — a unix socket, `Hello` first, [`Room::answer_traced`] behind
//!    it. The room is `Mutex<Room>` and every mutating path takes that one lock, so *one writer* is
//!    a fact of the type rather than a promise about the threads.
//! 2. **The clock** — [`Server::sweep_every_ms`], well inside the ninety-second lease, and
//!    [`Room::note_gone`] on every connection close. A close is the DAEMON's liveness and a beat is
//!    the WORK's; the two are different facts and this module keeps them apart: a session whose
//!    turn ended keeps its socket and loses its claim to the beat.
//! 3. **Routing** — every event a verb (or the sweep, or a close) produces goes to the connections
//!    it is about: the sessions queued under that tag, the session the event names, and the
//!    arbiter, who is a member of every tag because a question is addressed to it.
//!
//! # One connection per top-level session, and the door
//!
//! The first frame on a connection must be [`Request::Hello`], and a subagent is refused there
//! ([`Refused::Subagent`]) before any verb exists to refuse. A connection whose `Hello` is refused
//! is answered and closed: there is nothing else it could say.
//!
//! # The arbiter is a member of every tag
//!
//! [`Server::with_arbiter`] names one session that hears every event, whether or not it has an item
//! under the tag. That is the tie-breaker: it must be told about a raise, and it is never a poster.
//! It is a fact about the SERVER and not about the room — the room has no idea who arbitrates it —
//! which is why it lives here.
//!
//! # What a restart means, in words
//!
//! The room has **no durability**: [`Server::started_ms`] and the `Welcome`'s own words are the
//! only memory a restarted coordinator has. [`Server::identity`] is what a pane prints — *this
//! coordinator started at T and the queues it holds are the ones since then* — so *restarted clean*
//! and *restarted empty* cannot be read as the same thing. There is no store to be empty: an
//! in-memory room is empty at every start, and it says so rather than looking quiet.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::item::Speaker;
use crate::lease::{CoordinatorLease, LEASE_BEAT_EVERY_MS};
use crate::room::{Event, Room};
use crate::wire::{Answer, ROOM_PROTOCOL_VERSION, Request, Response};

/// How often the clock runs by default: five seconds, eighteen passes per lease, so a claim whose
/// beat stopped is taken back promptly and a beat that is one pass late is not mistaken for death.
pub const DEFAULT_SWEEP_EVERY_MS: u64 = 5_000;

/// **One connection's identity** — what its `Hello` said, and the writer a push goes out on.
struct Conn {
    who: Speaker,
    writer: Mutex<UnixStream>,
}

/// **The server's own state**, shared by the accept loop, the read loops and the clock.
struct Inner {
    room: Mutex<Room>,
    conns: Mutex<BTreeMap<u64, Arc<Conn>>>,
    next_conn: AtomicU64,
    path: PathBuf,
    started_ms: u64,
    sweep_every_ms: u64,
    /// **Which room this is** — the box's name (`crate::host_name`), or whatever
    /// [`Server::with_name`] was given. A `Hello` that names another room is refused with both
    /// names in the sentence: a room is per box, and obeying a client that dialled the wrong one
    /// would put its post in somebody else's queue.
    name: Mutex<String>,
    /// **The session that hears everything** — the tie-breaker. `None` on a room whose arbiter is a
    /// person at a head, which is a room where every question waits.
    arbiter: Mutex<Option<String>>,
    lease: Mutex<Option<CoordinatorLease>>,
    stop: AtomicBool,
    /// How many connections have been served since the start — the pane's *is anybody here*.
    served: AtomicU64,
}

/// **A handle to a running server** — what the coordinator's own threads use: the tie-breaker reads
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
    /// **Bind `path` and start serving `room`.**
    ///
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
        // on it, and the honest reading is *nobody is listening* — which is what `UnixListener`
        // tells us by refusing. So it is removed first, and the LEASE is what stops two live ones.
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        let server = Server {
            inner: Arc::new(Inner {
                room: Mutex::new(room),
                conns: Mutex::new(BTreeMap::new()),
                next_conn: AtomicU64::new(0),
                path: path.to_path_buf(),
                started_ms: now_ms(),
                sweep_every_ms: sweep_every_ms.max(100),
                name: Mutex::new(crate::lease::host_name()),
                arbiter: Mutex::new(None),
                lease: Mutex::new(None),
                stop: AtomicBool::new(false),
                served: AtomicU64::new(0),
            }),
        };
        server.spawn_accept(listener);
        server.spawn_clock();
        Ok(server)
    }

    /// **Name the session that hears every event** — the tie-breaker.
    pub fn with_arbiter(&self, session: impl Into<String>) -> &Self {
        *self.inner.arbiter.lock().expect("the arbiter lock") = Some(session.into());
        self
    }

    /// **Name this room** — the box's name by default, and something else in a test that needs two
    /// rooms on one box.
    pub fn with_name(&self, name: impl Into<String>) -> &Self {
        *self.inner.name.lock().expect("the name lock") = name.into();
        self
    }

    /// **Which room this is** — what a `Hello` is held to, and what the pane prints.
    pub fn name(&self) -> String {
        self.inner.name.lock().expect("the name lock").clone()
    }

    /// **Beat a lease from the server's own clock.** One process, one clock: the lease's beat and
    /// the room's sweep are the same thread's two duties, so they cannot drift apart.
    pub fn with_lease(&self, lease: CoordinatorLease) -> &Self {
        *self.inner.lease.lock().expect("the lease lock") = Some(lease);
        self
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn started_ms(&self) -> u64 {
        self.inner.started_ms
    }

    /// **The session that hears everything** — the tie-breaker, or `None` on a room whose arbiter
    /// is a person at a head.
    pub fn arbiter(&self) -> Option<String> {
        self.inner.arbiter.lock().expect("the arbiter lock").clone()
    }

    /// **Every session connected right now**, sorted — the pane's *who is up*, and the list a
    /// `hand` names. The socket is the daemon's liveness, so this is not the same fact as a beat:
    /// a session whose turn ended is here and not beating.
    pub fn connected(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .inner
            .conns
            .lock()
            .expect("the connections lock")
            .values()
            .map(|c| c.who.session.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// **What this coordinator is, in the words a pane prints** — and the sentence that makes
    /// *restarted clean* and *restarted empty* two different readings.
    pub fn identity(&self) -> String {
        self.identity_at(now_ms())
    }

    /// [`Server::identity`] at a given clock, so a test can pin the age rather than wait for one.
    ///
    /// **The age and the count are both in the sentence, and neither is decoration.** An in-memory
    /// room holds nothing at every start, so *no items* is the same reading for a room that has been
    /// up for an hour and one that has just restarted — unless the sentence says how long it has
    /// been up and how much has happened since.
    pub fn identity_at(&self, now: u64) -> String {
        let n = self.inner.served.load(Ordering::SeqCst);
        let items = self.items().len();
        let up = now.saturating_sub(self.inner.started_ms);
        format!(
            "the room on {} — started {} ago and holding {items} item(s) since then; the room keeps \
             no store, so a restart begins with no items at all ({n} connection(s) served since it \
             started)",
            self.inner.path.display(),
            crate::item::duration(up),
        )
    }

    /// **One frame, answered** — the room's dispatch with this server's own facts filled in, and
    /// with the two refusals that need a fact the state machine has not got: the version, and which
    /// room this is.
    ///
    /// Every other frame is the room's. [`Request::Room`] is the pane's read, and three of its
    /// fields are facts about this PROCESS rather than about the state machine, so the room answers
    /// its half and this fills the rest — here, once, so that a pane and a client cannot be told two
    /// different things about who arbitrates or who is up.
    fn answer_frame(&self, who: &Speaker, frame: &Request, now: u64) -> Answer {
        // **The door, and it is answered before the room sees the frame.** A skew or a wrong room
        // must not be half-obeyed: the room would vouch for a session that is not going to be
        // spoken to again, and a `post` after a `Bye` is a post in a queue nobody reads.
        if let Request::Hello { protocol, room, .. } = frame {
            if let Some(why) = crate::wire::protocol_refusal(*protocol) {
                return Answer {
                    response: Response::Bye { why },
                    events: Vec::new(),
                };
            }
            let asked = room.trim();
            let mine = self.name();
            if !asked.is_empty() && asked != mine {
                return Answer {
                    response: Response::Bye {
                        why: format!(
                            "this is the room of `{mine}` and `{asked}` was asked for — a room is \
                             per box, so obeying this connection would put its items in another \
                             machine's queues. Nothing was done: check the socket you dialled \
                             ({}), or the room name the client sends.",
                            self.inner.path.display()
                        ),
                    },
                    events: Vec::new(),
                };
            }
        }
        // **The room lock is released before anything else is read.** `identity_at` reads the items
        // and the connections, and the room's mutex is not reentrant: filling the answer inside the
        // guard is a deadlock that reads as a room that stopped answering.
        let mut answer = self
            .inner
            .room
            .lock()
            .expect("the room lock")
            .answer_traced(who, frame, now);
        match &mut answer.response {
            Response::Welcome { identity, .. } => *identity = self.identity_at(now),
            Response::Room {
                identity,
                arbiter,
                connected,
                ..
            } => {
                *identity = self.identity_at(now);
                *arbiter = self.arbiter();
                *connected = self.connected();
            }
            _ => {}
        }
        answer
    }

    /// **How many sessions are connected now** — the pane's *is anybody here*, and the number a
    /// test reads instead of sleeping.
    pub fn connections(&self) -> usize {
        self.inner.conns.lock().expect("the connections lock").len()
    }

    /// **Run the clock once, now** — the sweep, routed. A test drives this instead of waiting.
    pub fn tick(&self, now: u64) -> Vec<Event> {
        let events = self.inner.room.lock().expect("the room lock").sweep(now);
        self.route(&events);
        events
    }

    /// **The open questions**, oldest first — what the tie-breaker is prompted with.
    pub fn open_questions(&self) -> Vec<crate::room::Question> {
        self.inner
            .room
            .lock()
            .expect("the room lock")
            .open_questions()
            .into_iter()
            .cloned()
            .collect()
    }

    /// **Every item in the room**, oldest first — the pane, and the census a test reads.
    pub fn items(&self) -> Vec<crate::item::Item> {
        self.inner
            .room
            .lock()
            .expect("the room lock")
            .all()
            .into_iter()
            .cloned()
            .collect()
    }

    /// **Stop serving** — the socket is unbound and the lease given back. Called on an orderly
    /// stop; a killed coordinator leaves both behind on purpose, and its lease is what tells the
    /// next one that it was killed rather than that it never existed.
    pub fn stop(&self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_file(&self.inner.path);
        if let Some(lease) = self.inner.lease.lock().expect("the lease lock").as_ref() {
            let _ = lease.release();
        }
        // A connection whose peer is gone is already closed; this wakes the ones still reading so
        // their threads end rather than waiting for a client that will never write again.
        for (_, conn) in self
            .inner
            .conns
            .lock()
            .expect("the connections lock")
            .iter()
        {
            let _ = conn
                .writer
                .lock()
                .expect("a writer")
                .shutdown(std::net::Shutdown::Both);
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

    /// **One connection, one thread** — `Hello` first, then a read loop.
    fn serve_one(self, stream: UnixStream) {
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("room-conn".into())
            .spawn(move || {
                let writer = match stream.try_clone() {
                    Ok(w) => w,
                    Err(_) => return,
                };
                let mut reader = BufReader::new(stream);
                // **The door.** The first frame is the `Hello`, and a connection that does not
                // open with one has nothing to say that the room could act on.
                let first = match read_frame(&mut reader) {
                    Ok(Some(frame)) => frame,
                    _ => return,
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
                let speaker = match &hello {
                    Request::Hello { speaker, .. } => speaker.clone(),
                    _ => return,
                };
                let answer = me.answer_frame(&speaker, &hello, now_ms());
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
                let conn = Arc::new(Conn {
                    who: speaker.clone(),
                    writer: Mutex::new(w),
                });
                me.inner
                    .conns
                    .lock()
                    .expect("the connections lock")
                    .insert(id, Arc::clone(&conn));
                me.inner.served.fetch_add(1, Ordering::SeqCst);

                // **The read loop.** Every frame is answered, and every event the answer carries
                // is routed to the connections it is about — including this one, because a claim
                // by `s-b` is news to `s-a`.
                loop {
                    if me.inner.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let frame = match read_frame(&mut reader) {
                        Ok(Some(f)) => f,
                        _ => break,
                    };
                    let answer: Answer = me.answer_frame(&speaker, &frame, now_ms());
                    if write_to(&conn, &answer.response).is_err() {
                        break;
                    }
                    me.route(&answer.events);
                    if matches!(answer.response, Response::Bye { .. }) {
                        break;
                    }
                }

                // **The close is a fact.** This session's daemon is not there any more — it
                // exited, restarted, or was stopped — so every claim it holds goes back to the
                // queue NOW, rather than ninety seconds from now.
                me.inner
                    .conns
                    .lock()
                    .expect("the connections lock")
                    .remove(&id);
                let events = me.inner.room.lock().expect("the room lock").note_gone(
                    &speaker.session,
                    &format!("`{}`'s room connection closed", speaker.session),
                    now_ms(),
                );
                me.route(&events);
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
                    let events = me.inner.room.lock().expect("the room lock").sweep(now);
                    me.route(&events);
                }
            });
    }

    // ===== Routing =====

    /// **Send each event to the connections it is about.**
    ///
    /// Three audiences, and they are the design rather than a convenience:
    ///
    /// * **the sessions queued under the tag** — everybody with a live item there, which is who a
    ///   claim, a finish or an abandonment changes the world for. Derived from the room at the
    ///   moment of routing, so a session that has just posted is a member and a session that only
    ///   ever READ the queue is not: one read is not membership.
    /// * **the session the event names** — the one who did it, and the one a ruling hands an item
    ///   to, who may have nothing under that tag.
    /// * **the arbiter**, for every event, because a question is addressed to it and it is never a
    ///   poster.
    fn route(&self, events: &[Event]) {
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
            .cloned()
            .collect();
        if conns.is_empty() {
            return;
        }
        for event in events {
            let audience = {
                let room = self.inner.room.lock().expect("the room lock");
                let tag = if event.tag().is_empty() {
                    room.item(event.item()).map(|i| i.tag.clone())
                } else {
                    Some(event.tag().to_string())
                };
                let mut who: Vec<String> = match &tag {
                    Some(tag) => room
                        .queue(tag)
                        .into_iter()
                        .flat_map(|i| {
                            std::iter::once(i.posted_by.clone())
                                .chain(i.holder().map(str::to_string))
                        })
                        .collect(),
                    None => Vec::new(),
                };
                who.extend(event.named().into_iter().map(str::to_string));
                who
            };
            for conn in &conns {
                let mine = audience.iter().any(|s| *s == conn.who.session)
                    || arbiter.as_deref() == Some(conn.who.session.as_str());
                if !mine {
                    continue;
                }
                let _ = write_to(
                    conn,
                    &Response::Event {
                        event: event.clone(),
                    },
                );
            }
        }
    }
}

fn write_to(conn: &Conn, response: &Response) -> std::io::Result<()> {
    let mut w = conn.writer.lock().expect("a writer");
    write_frame(&mut *w, response)
}

/// **One frame, one line of JSON.**
///
/// A line rather than a length prefix because the two ends are this crate on both sides and a
/// frame that a person can read with `nc` is worth more than the bytes it costs.
fn write_frame(w: &mut impl Write, response: &Response) -> std::io::Result<()> {
    let mut line = serde_json::to_string(response)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    w.write_all(line.as_bytes())?;
    w.flush()
}

fn read_frame(reader: &mut BufReader<UnixStream>) -> std::io::Result<Option<Request>> {
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(line)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// The clock every part of the room reads: milliseconds since the epoch, and it is the process's
/// own, so a beat and a sweep cannot disagree about what *now* is.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// **The protocol number this build speaks** — re-exported so a client does not link the wire.
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
        assert!(said.contains("room.sock"), "{said}");
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

    /// **A room that has been up for a while and holds nothing is not a room that has just
    /// restarted**, and the only thing that says so is the age.
    ///
    /// An in-memory room holds nothing at every start (`docs/room.md` § *What the state machine
    /// assumes*), so *no items* is the same reading for both — which is exactly why the sentence
    /// carries how long it has been up and how much has happened since.
    #[test]
    fn the_identity_tells_a_long_up_empty_room_from_a_just_restarted_one() {
        let dir = std::env::temp_dir().join(format!("letibot-room-ident-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = Server::serve(&path, Room::new()).expect("a socket");
        let started = server.started_ms();
        let fresh = server.identity_at(started + 4_000);
        let settled = server.identity_at(started + 3_600_000);
        assert!(fresh.contains("started 4s ago"), "{fresh}");
        assert!(settled.contains("started 1h ago"), "{settled}");
        assert_ne!(fresh, settled, "two histories, two sentences");
        // Both are empty, and both say so — and both say that emptiness is what a start IS.
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

    /// **A client that reached the wrong coordinator is TOLD, with both names in the sentence.**
    ///
    /// A room is per box, and two boxes can both call themselves `localhost`. Before the name was on
    /// the wire, a session that dialled the wrong socket was obeyed: its post landed in another
    /// machine's queue and its claim locked a tag nobody there was waiting for.
    #[test]
    fn a_hello_that_names_another_room_is_refused_with_both_names() {
        let dir = std::env::temp_dir().join(format!("letibot-room-name-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = Server::serve(&path, Room::new()).expect("a socket");
        server.with_name("lab2x1");
        let me = Speaker::top_level("s-a");
        let answer = server.answer_frame(
            &me,
            &Request::Hello {
                protocol: ROOM_PROTOCOL_VERSION,
                speaker: me.clone(),
                room: "the-other-box".to_string(),
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
        // **And a session that names no room is let in** — that is a protocol-1 client, which the
        // version check refuses by name instead of pretending it said something it did not.
        let quiet = server.answer_frame(
            &me,
            &Request::Hello {
                protocol: ROOM_PROTOCOL_VERSION,
                speaker: me.clone(),
                room: String::new(),
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

    /// **The welcome carries the identity verbatim** — the same sentence `Server::identity` prints,
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
            &me,
            &Request::Hello {
                protocol: ROOM_PROTOCOL_VERSION,
                speaker: me.clone(),
                room: String::new(),
            },
            now,
        );
        match answer.response {
            Response::Welcome { identity, .. } => {
                assert_eq!(identity, server.identity_at(now), "verbatim, not reworded");
                assert!(identity.contains("no store"), "{identity}");
            }
            other => panic!("a hello is answered with a welcome: {other:?}"),
        }
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The protocol number is pinned once**, here, as a literal.
    ///
    /// It is the only place in the tree that spells the number rather than the constant, and that is
    /// the point: a wire change that misses a pin is a quiet lie about what two binaries share, so
    /// the pin is a literal a reviewer sees in a diff and every other use is the symbol.
    #[test]
    fn the_room_speaks_protocol_two() {
        assert_eq!(ROOM_PROTOCOL_VERSION, 2);
        // And the refusal names which side is older, in both directions.
        let old = crate::wire::protocol_refusal(1).expect("1 is not 2");
        assert!(old.contains("the client is the older"), "{old}");
        let new = crate::wire::protocol_refusal(3).expect("3 is not 2");
        assert!(new.contains("this room is the older"), "{new}");
        assert_eq!(crate::wire::protocol_refusal(2), None);
    }

    /// **The pane's read: the room's half from the room, the server's three from the server.**
    /// This is the frame a pane is drawn from, and it is the only one whose answer the server
    /// completes — so what is asserted here is that the completion happens at all, that it is the
    /// server's own answer (`identity`, the arbiter, who is connected) and that a second caller
    /// gets the same three.
    #[test]
    fn the_rooms_read_carries_the_servers_own_three_facts() {
        let dir = std::env::temp_dir().join(format!("letibot-room-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = Server::serve(&path, Room::new()).expect("a socket");
        server.with_arbiter("s-arbiter");
        let me = Speaker::top_level("s-a");
        let answer = server.answer_frame(&me, &Request::Room, now_ms());
        let Response::Room {
            identity,
            arbiter,
            connected,
            items,
            questions,
        } = answer.response
        else {
            panic!("a room read answers a room: {:?}", answer.response);
        };
        assert!(identity.contains("started"), "{identity}");
        assert_eq!(arbiter.as_deref(), Some("s-arbiter"));
        // Nobody is connected through a socket here, so the list is empty and says so — the pane
        // draws *the arbiter is not up* from exactly this.
        assert!(connected.is_empty(), "{connected:?}");
        assert!(items.is_empty());
        assert!(questions.is_empty());
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
