//! **The session's end of the socket** — one connection per top-level session, a round trip per
//! verb, the bell delivered to a callback, and a beat the daemon can run on the session's behalf.
//!
//! This is the half a daemon that is NOT the coordinator needs: a project's sessions talk to the
//! box's room over this, and the room has no idea whether the other end is a tool call, a wake or a
//! beat.
//!
//! # One connection, and why it must stay open
//!
//! The room's own rule (`crate::wire`'s header, and `docs/room.md` § *The wire*) is that **the
//! socket is the DAEMON's liveness and the beat is the WORK's**. A connection per call would make
//! every call a close, and every close a [`Room::note_gone`](crate::Room::note_gone) — so a session
//! that posted, claimed and read in three calls would lose its claim between the second and the
//! third. So [`Client`] holds one connection for the session's life, and reconnects only when that
//! connection has actually failed. **A reconnect is a new life**: the old socket's close already
//! gave the claims back, and the honest reading of the next claim is a fresh one.
//!
//! # The bell
//!
//! Frames the client did not ask for are [`Response::Event`], and they go to the callback the
//! client was built with. That callback is the whole of the wake: a client that dropped them would
//! be a mailbox, and firecode's fourth rule (*a bell is something that can inject a turn*) is the
//! criterion this exists to satisfy.
//!
//! # The beat on the session's behalf
//!
//! [`Client::start_beating`] is the difference between a design that works and one that blames the
//! model: a session that must remember to call `beat` will forget — it is thinking about its work —
//! and the ninety-second lease will take its item while it works. So the daemon beats while the
//! session's TURN is running, and stops when the turn ends, which is exactly the fact the lease is
//! measuring. Each tick re-reads what the session holds rather than remembering a list, so an item
//! claimed in the middle of a turn is beaten on the next tick.

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
pub const REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// **The client could not talk to the room.** Always a sentence, because the tool that shows it to
/// a model has nowhere else to put the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientError(pub String);

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ClientError {}

/// What the room said when the connection opened — the numbers a client must not guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Welcome {
    pub lease_ms: u64,
    pub beat_every_ms: u64,
    pub threshold: u32,
}

impl Default for Welcome {
    fn default() -> Self {
        Self {
            lease_ms: crate::room::DEFAULT_LEASE_MS,
            beat_every_ms: crate::room::DEFAULT_BEAT_EVERY_MS,
            threshold: crate::room::ABANDON_THRESHOLD,
        }
    }
}

/// **What a client does when the room rings it** — the wake, as a callback.
pub type Wake = Arc<dyn Fn(&Event) + Send + Sync>;

struct Live {
    writer: UnixStream,
}

struct Shared {
    path: PathBuf,
    speaker: Speaker,
    wake: Wake,
    live: Mutex<Option<Live>>,
    /// One round trip at a time: the reply channel below is a single slot, and two calls in flight
    /// on one connection would have no way to say which reply is whose.
    call: Mutex<()>,
    waiting: Mutex<Option<Sender<Response>>>,
    welcome: Mutex<Welcome>,
    /// Why the last connection failed, for the sentence a caller shows. Cleared on a good connect.
    why: Mutex<Option<String>>,
}

/// **A dropped client closes its socket**, which is the whole of *the close is a fact*.
///
/// It is not enough to drop the writer: the reader thread holds a second descriptor on the same
/// socket, so the peer would see nothing and a session whose daemon went away would keep its
/// claim for the lease's ninety seconds. `shutdown` acts on the socket rather than the descriptor,
/// so the blocked `read` returns and the coordinator's `note_gone` runs at once.
impl Drop for Shared {
    fn drop(&mut self) {
        if let Ok(live) = self.live.get_mut()
            && let Some(live) = live.take()
        {
            let _ = live.writer.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// **A session's connection to the room.**
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
    /// **Connect and say `hello`.** A room that is not there is the error, with the path in it.
    ///
    /// `wake` is called for every event the room sends about this session, from the reader thread.
    /// It must not block: it is the thread that keeps the connection alive.
    pub fn connect(
        path: impl Into<PathBuf>,
        speaker: Speaker,
        wake: Wake,
    ) -> Result<Client, ClientError> {
        let client = Client {
            inner: Arc::new(Shared {
                path: path.into(),
                speaker,
                wake,
                live: Mutex::new(None),
                call: Mutex::new(()),
                waiting: Mutex::new(None),
                welcome: Mutex::new(Welcome::default()),
                why: Mutex::new(None),
            }),
        };
        client.open()?;
        Ok(client)
    }

    pub fn session(&self) -> &str {
        &self.inner.speaker.session
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn welcome(&self) -> Welcome {
        *self.inner.welcome.lock().expect("the welcome lock")
    }

    /// **Is the connection up right now?** A `false` here is not an error — the next call reconnects.
    pub fn connected(&self) -> bool {
        self.inner.live.lock().expect("the live lock").is_some()
    }

    /// A `Client` over a shared state the reader thread has already upgraded — the reader keeps a
    /// [`std::sync::Weak`], so it is never the owner that keeps a dropped connection open.
    fn from_shared(inner: Arc<Shared>) -> Client {
        Client { inner }
    }

    /// **One verb, one reply.**
    ///
    /// A connection that has failed is re-opened and the frame is sent again — which is safe
    /// precisely because the wire is idempotent: every mutating frame carries the caller's id, and
    /// the room answers a repeat from its replay window instead of acting twice.
    pub fn request(&self, request: &Request) -> Result<Response, ClientError> {
        let _one_at_a_time = self.inner.call.lock().expect("the call lock");
        if !self.connected() {
            self.open()?;
        }
        let (tx, rx): (Sender<Response>, Receiver<Response>) = mpsc::channel();
        *self.inner.waiting.lock().expect("the waiting lock") = Some(tx);
        let written = self.write(request);
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

    /// **Start beating for this session until the returned [`Beat`] is dropped.** The turn's
    /// beginning and end are the caller's — [`crate::client::Client::start_beating`] knows nothing
    /// about turns, only about *while this is held, say I am working*.
    pub fn start_beating(&self) -> Beat {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let me = self.clone();
        let flag = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name(format!("room-beat-{}", self.inner.speaker.session))
            .spawn(move || {
                let welcome = me.welcome();
                // **A third of the lease, or the room's own number, whichever is sooner.** A
                // client that beats every five seconds against a five-second lease is a client
                // that loses its item between two ticks.
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
                    // **Read, then beat.** An item claimed during the turn is on this list; a
                    // remembered list would miss it and the lease would take it back.
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

    // ===== The connection =====

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
        // **A handshake that cannot hang its caller.** `Client::connect` runs inside
        // `Harness::open`, so a coordinator that is up but wedged would otherwise block every
        // session open on this box for ever. Two seconds is generous for a local process whose
        // longest act is a mutex over an in-memory map; past it the session gets its sentence and
        // opens without a room.
        let _ = writer.set_read_timeout(Some(std::time::Duration::from_secs(2)));
        let _ = writer.set_write_timeout(Some(std::time::Duration::from_secs(2)));
        let hello = Request::Hello {
            protocol: ROOM_PROTOCOL_VERSION,
            speaker: self.inner.speaker.clone(),
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
                ..
            } => {
                *self.inner.welcome.lock().expect("the welcome lock") = Welcome {
                    lease_ms,
                    beat_every_ms,
                    threshold,
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
        // **The timeout was the HANDSHAKE's, and it goes back now.** Socket options belong to the
        // socket, not to one descriptor, so leaving it on would have the reader thread wake every
        // two seconds with `WouldBlock` and read that as a closed connection — a client that
        // disconnects itself twice a minute.
        let _ = writer.set_read_timeout(None);
        let _ = writer.set_write_timeout(None);
        let weak = Arc::downgrade(&self.inner);
        let reader_thread = std::thread::Builder::new()
            .name(format!("room-read-{}", self.inner.speaker.session))
            .spawn(move || {
                // **A `Weak`, so the reader is not what keeps the client alive.** The thread that
                // blocks on this socket must not be the last owner of it, or a dropped client
                // would hold its own connection open and the coordinator would never see the
                // close — which is the one fact the whole liveness design rests on.
                //
                // **And the read comes BEFORE the upgrade, which is not a style choice.**
                // MEASURED 2026-10-29: `weak.upgrade()` was the loop's first statement and its
                // `Client` lived across `read_frame`, so the thread blocked on the socket was an
                // OWNER of it. `drop(client)` then could not run `Shared::drop`, the `shutdown`
                // that is supposed to wake this read never happened, and the coordinator's
                // `note_gone` never ran — the claim waited out the ninety-second lease instead of
                // being given back at once, and `tests/socket.rs`'s
                // `a_closed_connection_gives_its_claim_back_at_once_and_says_so` failed about
                // half the time (5.1 s, its own deadline) while passing the other half, depending
                // on whether the drop landed in the few instructions between one frame and the
                // next. Reading first leaves the blocked thread holding nothing but the socket.
                loop {
                    let frame = match read_frame(&mut reader) {
                        Ok(frame) => frame,
                        Err(e) => {
                            // A read that timed out is not a connection that closed — the socket
                            // has no read timeout after the handshake, so this is belt and braces
                            // against one set by something else. Nothing was read, so nothing is
                            // dispatched and the loop re-reads.
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) {
                                // Nothing was read, so nothing is dispatched — but a client that
                                // is GONE is still a reason to stop: without this the loop would
                                // re-read for ever on a socket somebody else put a timeout on,
                                // holding no owner and doing no work.
                                if weak.strong_count() == 0 {
                                    break;
                                }
                                continue;
                            }
                            if let Some(me) = weak.upgrade().map(Client::from_shared) {
                                me.drop_connection(&e.to_string());
                            }
                            break;
                        }
                    };
                    // **Now the strong reference**, for as long as dispatching one frame takes.
                    // Gone means the client was dropped while this thread was blocked, which is
                    // the case the whole design is for: nothing is left to tell.
                    let Some(me) = weak.upgrade().map(Client::from_shared) else {
                        break;
                    };
                    match frame {
                        Some(Response::Event { event }) => (me.inner.wake)(&event),
                        Some(Response::Bye { why }) => {
                            me.drop_connection(&why);
                            break;
                        }
                        Some(other) => {
                            let slot = me.inner.waiting.lock().expect("the waiting lock");
                            if let Some(tx) = slot.as_ref() {
                                let _ = tx.send(other);
                            }
                        }
                        None => {
                            me.drop_connection("the room closed the connection");
                            break;
                        }
                    }
                }
            })
            .map_err(|e| ClientError(e.to_string()))?;
        *self.inner.why.lock().expect("the why lock") = None;
        *self.inner.live.lock().expect("the live lock") = Some(Live { writer });
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

    fn drop_connection(&self, why: &str) {
        let mut live = self.inner.live.lock().expect("the live lock");
        if live.take().is_some() {
            *self.inner.why.lock().expect("the why lock") = Some(why.to_string());
        }
    }

    /// Why the last connection ended, when one has — the sentence a caller adds to a refusal.
    pub fn last_failure(&self) -> Option<String> {
        self.inner.why.lock().expect("the why lock").clone()
    }
}

/// **The beat, for as long as it is held.** Dropping it stops the beating at once: the turn that
/// started it has ended, and the lease should start counting from the last beat.
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
        // **Not joined on purpose.** The thread's next act after waking is to look at the flag and
        // leave; joining it would make a turn's end wait for a socket round trip that the caller
        // has already decided not to care about.
        self.handle.take();
    }
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
