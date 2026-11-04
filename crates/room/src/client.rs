//! The session's end of the socket: one connection per top-level session, a round trip per verb,
//! the bell delivered to a callback, and a beat run on the session's behalf.
//! The connection stays open for the session's life, or each close would be a `note_gone`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

use crate::item::Speaker;
use crate::room::Event;
use crate::server::now_ms;
use crate::wire::{ROOM_PROTOCOL_VERSION, Request, Response};

/// How long one round trip may take before the client calls the connection dead. Generous: the
/// coordinator is a local process whose longest act is a `Mutex` on an in-memory map.
/// The redial thread's first wait and its ceiling while the room is away.
const REDIAL_MIN: std::time::Duration = std::time::Duration::from_millis(250);
const REDIAL_MAX: std::time::Duration = std::time::Duration::from_secs(5);
pub const REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The client could not talk to the room. Always a sentence, because the tool that shows it to a model
/// has nowhere else to put the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientError(pub String);

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ClientError {}

/// What the room said when the connection opened - the numbers a client must not guess, and what the
/// room said it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Welcome {
    pub lease_ms: u64,
    pub beat_every_ms: u64,
    pub threshold: u32,
    /// `Server::identity()`, verbatim, so a caller can print what it connected to. A reader that wants
    /// the age now asks [`Request::Room`], which carries the same sentence taken at the read.
    pub identity: String,
    /// How many events the room addressed to this session while it had no connection, and when the
    /// first of them was routed. The room keeps no log, so this is a COUNT and not a replay: a client
    /// told *you missed four* re-reads the state, which cannot be wrong.
    pub missed: u64,
    pub missed_since_ms: u64,
    /// Which coordinator start this is. A change between two connections is a restart.
    pub instance: u64,
}

impl Default for Welcome {
    fn default() -> Self {
        Self {
            lease_ms: crate::room::DEFAULT_LEASE_MS,
            beat_every_ms: crate::room::DEFAULT_BEAT_EVERY_MS,
            threshold: crate::room::ABANDON_THRESHOLD,
            identity: String::new(),
            missed: 0,
            missed_since_ms: 0,
            instance: 0,
        }
    }
}

/// What a client does with a pushed chat message: the message, and whether it names this reader.
pub type ChatHook = Arc<dyn Fn(&crate::chat::Message, bool) + Send + Sync>;

/// What a client does when the room rings it - the wake, as a callback.
pub type Wake = Arc<dyn Fn(&Event) + Send + Sync>;

struct Live {
    writer: UnixStream,
    /// Which connection this is, so a reader that outlives its connection cannot close a newer one.
    generation: u64,
}

struct Shared {
    path: PathBuf,
    speaker: Speaker,
    /// The room this client believes it is reaching. It rides on every `Hello`, including a
    /// reconnect's: a session moved to another box must not keep talking to the one it opened on.
    room: String,
    wake: Wake,
    seat: crate::chat::Seat,
    /// Called for every chat message pushed to this connection. Runs on the reader thread.
    chat: Mutex<Option<ChatHook>>,
    live: Mutex<Option<Live>>,
    generation: std::sync::atomic::AtomicU64,
    /// One round trip at a time: the reply channel below is a single slot, and two calls in flight
    /// on one connection would have no way to say which reply is whose.
    call: Mutex<()>,
    waiting: Mutex<Option<Sender<Response>>>,
    welcome: Mutex<Welcome>,
    /// Why the last connection failed, for the sentence a caller shows. Cleared on a good connect.
    why: Mutex<Option<String>>,
}

/// A dropped client closes its socket, which is the whole of *the close is a fact*: dropping the writer
/// is not enough, because the reader thread holds a second descriptor and the claim would survive the
/// lease. `shutdown` acts on the socket, so the blocked `read` returns and `note_gone` runs at once.
impl Drop for Shared {
    fn drop(&mut self) {
        if let Ok(live) = self.live.get_mut()
            && let Some(live) = live.take()
        {
            let _ = live.writer.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// A session's connection to the room.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Shared>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("path", &self.inner.path)
            .field("session", &self.inner.speaker.session)
            .finish()
    }
}

impl Client {
    /// Connect and say `hello`; a room that is not there is the error, with the path in it. `wake` runs
    /// on the reader thread, so it must not block - it is what keeps the connection alive.
    pub fn connect(
        path: impl Into<PathBuf>,
        speaker: Speaker,
        wake: Wake,
    ) -> Result<Client, ClientError> {
        Self::connect_named(path, speaker, crate::lease::host_key(), wake)
    }

    /// Connect to a room by NAME. The server refuses a `Hello` that names another room with both names
    /// in the sentence, so a test (or a client moved between boxes) can say the wrong one and be told.
    pub fn connect_named(
        path: impl Into<PathBuf>,
        speaker: Speaker,
        room: impl Into<String>,
        wake: Wake,
    ) -> Result<Client, ClientError> {
        Self::connect_as(path, speaker, room, crate::chat::Seat::Session, wake)
    }

    /// [`Client::connect_named`] in a given seat: the operator's (speaker `operator`) or an
    /// external's, which see every room.
    pub fn connect_as(
        path: impl Into<PathBuf>,
        speaker: Speaker,
        room: impl Into<String>,
        seat: crate::chat::Seat,
        wake: Wake,
    ) -> Result<Client, ClientError> {
        let client = Client {
            inner: Arc::new(Shared {
                path: path.into(),
                speaker,
                room: room.into(),
                wake,
                seat,
                chat: Mutex::new(None),
                live: Mutex::new(None),
                generation: std::sync::atomic::AtomicU64::new(0),
                call: Mutex::new(()),
                waiting: Mutex::new(None),
                welcome: Mutex::new(Welcome::default()),
                why: Mutex::new(None),
            }),
        };
        client.open()?;
        client.spawn_redial();
        Ok(client)
    }

    /// Reopen a dropped connection in the background, backing off while the room is away, so pushed
    /// chat and events resume after a coordinator restart without waiting for the next request. The
    /// thread holds a `Weak` and ends with the client.
    fn spawn_redial(&self) {
        let weak = Arc::downgrade(&self.inner);
        let spawned = std::thread::Builder::new()
            .name(format!("room-redial-{}", self.inner.speaker.session))
            .spawn(move || {
                let mut delay = REDIAL_MIN;
                loop {
                    std::thread::sleep(delay);
                    let Some(me) = weak.upgrade().map(Client::from_shared) else {
                        return;
                    };
                    let _one = me.inner.call.lock().expect("the call lock");
                    if me.connected() || me.open().is_ok() {
                        delay = REDIAL_MIN;
                    } else {
                        delay = (delay * 2).min(REDIAL_MAX);
                    }
                }
            });
        if let Err(e) = spawned {
            eprintln!("room: the redial thread did not start: {e}");
        }
    }

    /// Hear pushed chat. Replaces any earlier hook.
    pub fn on_chat(&self, hook: ChatHook) {
        *self.inner.chat.lock().expect("the chat lock") = Some(hook);
    }

    pub fn session(&self) -> &str {
        &self.inner.speaker.session
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn welcome(&self) -> Welcome {
        self.inner.welcome.lock().expect("the welcome lock").clone()
    }

    /// Is the connection up right now? A `false` here is not an error - the next call reconnects.
    pub fn connected(&self) -> bool {
        self.inner.live.lock().expect("the live lock").is_some()
    }

    /// A `Client` over a shared state the reader thread has already upgraded - the reader keeps a
    /// [`std::sync::Weak`], so it is never the owner that keeps a dropped connection open.
    fn from_shared(inner: Arc<Shared>) -> Client {
        Client { inner }
    }

    /// What the last connection missed, and forget it: `(count, first_ms)` from the `Welcome`. Taking
    /// rather than reading makes one gap one sentence - a caller reporting the same number on every poll
    /// would say the room had gone quiet again every second.
    pub fn take_missed(&self) -> (u64, u64) {
        let mut w = self.inner.welcome.lock().expect("the welcome lock");
        let out = (w.missed, w.missed_since_ms);
        w.missed = 0;
        w.missed_since_ms = 0;
        out
    }

    /// One verb, one reply. A failed connection is re-opened and the frame sent again, which is safe
    /// because the wire is idempotent: a repeat is answered from the replay window, not acted twice.
    pub fn request(&self, request: &Request) -> Result<Response, ClientError> {
        let _one_at_a_time = self.inner.call.lock().expect("the call lock");
        if !self.connected() {
            self.open()?;
        }
        let (tx, rx): (Sender<Response>, Receiver<Response>) = mpsc::channel();
        *self.inner.waiting.lock().expect("the waiting lock") = Some(tx);
        let mut written = self.write(request);
        if written.is_err() {
            // The connection died under us: open a fresh one and send once more. The wire's replay
            // window makes a repeated frame safe.
            self.drop_connection("a write failed");
            written = self.open().and_then(|()| self.write(request));
        }
        let reply = match written {
            Ok(()) => rx.recv_timeout(REPLY_TIMEOUT).map_err(|_| {
                ClientError(format!(
                    "the room at {} did not answer in {}s",
                    self.inner.path.display(),
                    REPLY_TIMEOUT.as_secs()
                ))
            }),
            Err(e) => Err(e),
        };
        *self.inner.waiting.lock().expect("the waiting lock") = None;
        match reply {
            Ok(r) => Ok(r),
            Err(e) => {
                self.drop_connection(&e.0);
                Err(e)
            }
        }
    }

    /// Start beating for this session until the returned [`Beat`] is dropped. It knows nothing about
    /// turns, only about *while this is held, say I am working*.
    pub fn start_beating(&self) -> Beat {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let me = self.clone();
        let flag = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name(format!("room-beat-{}", self.inner.speaker.session))
            .spawn(move || {
                let welcome = me.welcome();
                // A third of the lease, or the room's own number, whichever is sooner: a client that
                // beats every five seconds against a five-second lease loses its item between ticks.
                let every = welcome.beat_every_ms.min(welcome.lease_ms / 3).max(250);
                loop {
                    // Sleep first: a turn that begins by claiming an item must not beat before it
                    // holds anything, and the claim's own `beat_ms` is already `now`.
                    let (lock, cv) = &*flag;
                    let guard = lock.lock().expect("the beat flag");
                    let (guard, _) = cv
                        .wait_timeout(guard, std::time::Duration::from_millis(every))
                        .expect("the beat flag");
                    if *guard {
                        break;
                    }
                    drop(guard);
                    // Read, then beat. An item claimed during the turn is on this list; a remembered
                    // list would miss it and the lease would take it back.
                    let Ok(Response::Holding { rows, .. }) =
                        me.request(&Request::Holding { session: None })
                    else {
                        continue;
                    };
                    for row in rows {
                        let _ = me.request(&Request::Beat {
                            id: format!("beat-{}-{}", row.item, now_ms()),
                            item: row.item,
                        });
                    }
                }
            })
            .ok();
        Beat { stop, handle }
    }

    fn open(&self) -> Result<(), ClientError> {
        let stream = UnixStream::connect(&self.inner.path).map_err(|e| {
            ClientError(format!(
                "there is no room coordinator at {} ({e}) — the room is a daemon of its own, \
                 started with `harnessd --room`",
                self.inner.path.display()
            ))
        })?;
        let mut reader =
            BufReader::new(stream.try_clone().map_err(|e| ClientError(e.to_string()))?);
        let mut writer = stream;
        // A handshake that cannot hang its caller: `connect` runs inside `Harness::open`, so a
        // coordinator that is up but wedged would block every session open on this box for ever.
        // Past two seconds the session gets its sentence and opens without a room.
        let _ = writer.set_read_timeout(Some(std::time::Duration::from_secs(2)));
        let _ = writer.set_write_timeout(Some(std::time::Duration::from_secs(2)));
        let hello = Request::Hello {
            protocol: ROOM_PROTOCOL_VERSION,
            speaker: self.inner.speaker.clone(),
            room: self.inner.room.clone(),
            seat: self.inner.seat,
            token: operator_token(&self.inner.path),
        };
        write_frame(&mut writer, &hello)?;
        let first = read_frame(&mut reader)
            .map_err(|e| ClientError(format!("reading from the room: {e}")))?
            .ok_or_else(|| {
                ClientError(format!(
                    "the room at {} closed the connection before saying `hello`",
                    self.inner.path.display()
                ))
            })?;
        match first {
            Response::Welcome {
                lease_ms,
                beat_every_ms,
                threshold,
                identity,
                missed,
                missed_since_ms,
                instance,
                ..
            } => {
                *self.inner.welcome.lock().expect("the welcome lock") = Welcome {
                    lease_ms,
                    beat_every_ms,
                    threshold,
                    identity,
                    missed,
                    missed_since_ms,
                    instance,
                };
            }
            Response::Refused { why, .. } => {
                return Err(ClientError(format!(
                    "the room refused `{}`: {why}",
                    self.inner.speaker
                )));
            }
            Response::Bye { why } => {
                return Err(ClientError(format!(
                    "the room closed the connection: {why}"
                )));
            }
            other => {
                return Err(ClientError(format!(
                    "the room answered `hello` with {other:?}"
                )));
            }
        }
        // The timeout was the HANDSHAKE's and it goes back now: socket options belong to the socket,
        // so leaving it on would make the reader wake with `WouldBlock` every two seconds and read
        // that as a closed connection - a client that disconnects itself twice a minute.
        let _ = writer.set_read_timeout(None);
        let _ = writer.set_write_timeout(None);
        let generation = self
            .inner
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let weak = Arc::downgrade(&self.inner);
        let reader_thread = std::thread::Builder::new()
            .name(format!("room-read-{}", self.inner.speaker.session))
            .spawn(move || {
                // A `Weak`, and the READ comes before the upgrade: a strong reference held across
                // `read_frame` makes the blocked thread an owner, so `drop(client)` cannot run
                // `Shared::drop` and the claim waits out the lease instead of being given back.
                loop {
                    let frame = match read_frame(&mut reader) {
                        Ok(frame) => frame,
                        Err(e) => {
                            // A read that timed out is not a connection that closed: the socket has no
                            // read timeout after the handshake, so this is belt and braces.
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) {
                                // Nothing was read, so nothing is dispatched - but a client that is
                                // GONE is still a reason to stop, or this re-reads for ever on a
                                // socket somebody else put a timeout on.
                                if weak.strong_count() == 0 {
                                    break;
                                }
                                continue;
                            }
                            if let Some(me) = weak.upgrade().map(Client::from_shared) {
                                me.drop_generation(generation, &e.to_string());
                            }
                            break;
                        }
                    };
                    // Now the strong reference, for as long as dispatching one frame takes. Gone means
                    // the client was dropped while this thread was blocked, which is the case the
                    // whole design is for: nothing is left to tell.
                    let Some(me) = weak.upgrade().map(Client::from_shared) else {
                        break;
                    };
                    match frame {
                        Some(Response::Event { event }) => (me.inner.wake)(&event),
                        Some(Response::Chat { message, names_you }) => {
                            let hook = me.inner.chat.lock().expect("the chat lock").clone();
                            if let Some(hook) = hook {
                                hook(&message, names_you);
                            }
                        }
                        Some(Response::Bye { why }) => {
                            me.drop_generation(generation, &why);
                            break;
                        }
                        Some(other) => {
                            let slot = me.inner.waiting.lock().expect("the waiting lock");
                            if let Some(tx) = slot.as_ref() {
                                let _ = tx.send(other);
                            }
                        }
                        None => {
                            me.drop_generation(generation, "the room closed the connection");
                            break;
                        }
                    }
                }
            })
            .map_err(|e| ClientError(e.to_string()))?;
        *self.inner.why.lock().expect("the why lock") = None;
        *self.inner.live.lock().expect("the live lock") = Some(Live { writer, generation });
        // The reader's handle is dropped on purpose: the thread ends when the socket does, and a
        // join here would be a turn's end waiting for a connection the caller has given up on.
        let _ = reader_thread;
        Ok(())
    }

    fn write(&self, request: &Request) -> Result<(), ClientError> {
        let mut live = self.inner.live.lock().expect("the live lock");
        match live.as_mut() {
            Some(l) => write_frame(&mut l.writer, request),
            None => Err(ClientError("the room connection is not open".to_string())),
        }
    }

    fn drop_generation(&self, generation: u64, why: &str) {
        let mut live = self.inner.live.lock().expect("the live lock");
        if live.as_ref().is_some_and(|l| l.generation == generation) {
            live.take();
            *self.inner.why.lock().expect("the why lock") = Some(why.to_string());
        }
    }

    fn drop_connection(&self, why: &str) {
        let mut live = self.inner.live.lock().expect("the live lock");
        if live.take().is_some() {
            *self.inner.why.lock().expect("the why lock") = Some(why.to_string());
        }
    }

    /// Why the last connection ended, when one has - the sentence a caller adds to a refusal.
    pub fn last_failure(&self) -> Option<String> {
        self.inner.why.lock().expect("the why lock").clone()
    }
}

/// The beat, for as long as it is held. Dropping it stops the beating at once: the turn that started
/// it has ended, and the lease should start counting from the last beat.
#[derive(Debug)]
pub struct Beat {
    stop: Arc<(Mutex<bool>, Condvar)>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Beat {
    /// Stop beating without waiting for the thread.
    pub fn stop(self) {
        drop(self);
    }
}

impl Drop for Beat {
    fn drop(&mut self) {
        let (lock, cv) = &*self.stop;
        *lock.lock().expect("the beat flag") = true;
        cv.notify_all();
        // Not joined on purpose: the thread's next act after waking is to look at the flag and leave,
        // and joining would make a turn's end wait for a socket round trip nobody cares about.
        self.handle.take();
    }
}

/// The operator's token, read from beside the socket it belongs to. `None` when there is no file:
/// the room then refuses the seat with a sentence naming the path, which is a better answer than
/// this end guessing. A confined tool cannot read it - the boundary hides the coordinator's files.
fn operator_token(socket: &Path) -> Option<String> {
    let path = crate::operator_token_path(socket);
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn write_frame(w: &mut impl Write, request: &Request) -> Result<(), ClientError> {
    let mut line = serde_json::to_string(request).map_err(|e| ClientError(e.to_string()))?;
    line.push('\n');
    w.write_all(line.as_bytes())
        .and_then(|()| w.flush())
        .map_err(|e| ClientError(format!("writing to the room: {e}")))
}

fn read_frame(reader: &mut BufReader<UnixStream>) -> std::io::Result<Option<Response>> {
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
