//! A running peer: the TCP listener a box's links arrive at, the beacons that find the others, the
//! tick that says a link is still there, and the one door everything else uses.
//!
//! It is an object rather than a thread's private state because three other things reach it - the
//! coordinator's server to ship a message, the announce loop to dial a box that appeared, and the
//! tick to end a link that went quiet - and it tells nobody anything directly: what it learns is
//! pushed into the channel [`Peer::open`] answers with, and the coordinator acts on it.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::peer::announce::{self, Announcer, Beacon};
use crate::peer::cookie::Cookie;
use crate::peer::frame::{MAX_LINE, PeerFrame};
use crate::peer::handshake::{Answerer, Dialer, Outcome, nonce};
use crate::peer::mesh::{Mesh, PeerEvent, Tick};
use crate::peer::profile::{ProjectProfile, SeatProfile};

/// How long a dial may take before the box is treated as gone.
pub const DIAL: Duration = Duration::from_secs(3);
/// How long one write to a link may block. A peer that stopped reading is a peer whose link ends,
/// and it must not stop this box's clock.
pub const WRITE: Duration = Duration::from_secs(5);
/// The most seats one peer may publish. A hostile box that listed thousands is a memory leak, not
/// a roster.
pub const MAX_SEATS: usize = 64;
/// How many events may wait for the coordinator. Past it the oldest is dropped: a mesh whose
/// reader stopped is a link to end, not memory to spend.
pub const QUEUE: usize = 1_024;
/// How many refusals a box keeps to show an operator: a link is refused for one of a few reasons,
/// and a list that grew would be a log with a lock on it.
pub const REFUSALS_KEPT: usize = 8;
/// Why a box says it is going, in the `Bye` of a coordinator that is stopping. The words are the
/// SENDER's box's - the far side renders them as *that box's* (`mac1 said goodbye: its coordinator
/// is stopping`), so they may not say *this box*.
pub const STOPPING: &str = "its coordinator is stopping";

/// What a box needs to peer. The cookie decides whether it peers at all: a coordinator with no
/// cookie file has no peer object and runs exactly as it did before.
#[derive(Debug, Clone)]
pub struct PeerConfig {
    /// This box's host half - the pinned name every one of its seat ids carries.
    pub host: String,
    pub cookie: Cookie,
    /// The port a link arrives at; 0 lets the kernel pick, which a test uses.
    pub port: u16,
    /// Where the listener binds: `0.0.0.0` on a box, loopback in a test.
    pub bind: IpAddr,
    pub group: Ipv4Addr,
    pub group_port: u16,
    /// Say who this box is on the LAN. Off is a box that dials only the peers it was told to.
    pub announce: bool,
    pub tick: Tick,
}

impl PeerConfig {
    pub fn new(host: impl Into<String>, cookie: Cookie) -> PeerConfig {
        PeerConfig {
            host: host.into(),
            cookie,
            port: 0,
            bind: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            group: announce::GROUP.parse().expect("the group is a literal"),
            group_port: announce::PORT,
            announce: true,
            tick: Tick::default(),
        }
    }

    pub fn with_port(mut self, port: u16) -> PeerConfig {
        self.port = port;
        self
    }

    pub fn with_bind(mut self, bind: IpAddr) -> PeerConfig {
        self.bind = bind;
        self
    }

    pub fn at_group(mut self, group: Ipv4Addr, port: u16) -> PeerConfig {
        self.group = group;
        self.group_port = port;
        self
    }

    pub fn without_announce(mut self) -> PeerConfig {
        self.announce = false;
        self
    }

    pub fn with_tick(mut self, tick: Tick) -> PeerConfig {
        self.tick = tick;
        self
    }
}

/// One live link's writing half. The socket is cloned out of the reading thread so a message does
/// not wait for the reader to come round.
type Sender = Arc<Mutex<TcpStream>>;

/// A live link, and which one of them it is. A box that reconnects makes a second link while the
/// first one's reader may still be waking from its socket, and the reader of the OLD socket must not
/// take the new link down: the generation is what tells them apart.
#[derive(Clone)]
struct Live {
    generation: u64,
    stream: Sender,
}

struct Inner {
    host: String,
    cookie: Cookie,
    port: u16,
    tick: Tick,
    mesh: Mutex<Mesh>,
    links: Mutex<BTreeMap<String, Live>>,
    next_generation: AtomicU64,
    dialing: Mutex<BTreeSet<String>>,
    seats: Mutex<Vec<SeatProfile>>,
    /// This box's projects, as the mesh matches a peer's seat against them: the identity is read
    /// from git by the coordinator's owner and handed in here.
    projects: Mutex<Vec<ProjectProfile>>,
    /// What was last sent to each box, so a tick sends a profile list only when it changed.
    sent: Mutex<BTreeMap<String, Vec<SeatProfile>>>,
    events: SyncSender<PeerEvent>,
    stop: AtomicBool,
    dropped: AtomicU64,
    /// The last few refusals, for the person reading the log and for a banner: a box that will not
    /// speak to this one is a thing an operator needs named, not a silence.
    refusals: Mutex<Vec<String>>,
}

impl Inner {
    fn now(&self) -> u64 {
        letibot_metrics::now_ms()
    }

    /// A link this box would not make, said on the log and kept for whoever asks.
    fn refused(&self, why: &str) {
        eprintln!("room: {why}");
        let mut kept = self.refusals.lock().expect("the refusals lock");
        kept.push(why.to_string());
        while kept.len() > REFUSALS_KEPT {
            kept.remove(0);
        }
    }

    fn announce_event(&self, event: PeerEvent) {
        if self.events.try_send(event).is_err() {
            // The coordinator's reader is behind. What is lost here is a roster note; the link
            // itself and the messages on it keep working, and the next `Up` re-sends the seats.
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Write one frame to one box's link. A link that cannot take it ends: a box that stopped
    /// reading a ping will not read a message either.
    fn ship(&self, host: &str, frame: &PeerFrame) -> bool {
        let at = self
            .links
            .lock()
            .expect("the links lock")
            .get(host)
            .cloned();
        let Some(live) = at else {
            return false;
        };
        let Ok(mut stream) = live.stream.lock() else {
            return false;
        };
        let mut line = frame.line();
        line.push('\n');
        match stream
            .write_all(line.as_bytes())
            .and_then(|()| stream.flush())
        {
            Ok(()) => true,
            Err(e) => {
                drop(stream);
                self.unlinked(
                    host,
                    live.generation,
                    &format!("writing to {host} failed ({e})"),
                );
                false
            }
        }
    }

    /// The one place a link becomes live: the socket is filed for writing, the mesh says so, and
    /// this box says what its seats are on.
    fn linked(&self, host: &str, addr: SocketAddr, stream: TcpStream) -> u64 {
        let now = self.now();
        let generation = self.next_generation.fetch_add(1, Ordering::SeqCst);
        self.links.lock().expect("the links lock").insert(
            host.to_string(),
            Live {
                generation,
                stream: Arc::new(Mutex::new(stream)),
            },
        );
        let event = self.mesh.lock().expect("the mesh lock").up(host, addr, now);
        // The seats go out on every new link, before the `Up` is announced: a box that has just
        // heard of this one should be able to introduce the seats in the same breath.
        let seats = self.seats.lock().expect("the seats lock").clone();
        self.sent
            .lock()
            .expect("the sent lock")
            .insert(host.to_string(), seats.clone());
        self.ship(host, &PeerFrame::Profiles { seats });
        if let Some(event) = event {
            self.announce_event(event);
            // And what the FAR box holds, said again: after a `Down` the last list it sent is all
            // this box knows, and a reconnect whose list did not change would otherwise never mark
            // its seats here again.
            let known = self.mesh.lock().expect("the mesh lock").known_seats(host);
            if !known.is_empty() {
                self.announce_event(PeerEvent::Profiles {
                    host: host.to_string(),
                    seats: known,
                });
            }
        }
        generation
    }

    /// A link ended. `generation` is the link that ended: one that has since been replaced by a
    /// reconnect says nothing, because the box is not away - it came back.
    fn unlinked(&self, host: &str, generation: u64, why: &str) {
        {
            let mut links = self.links.lock().expect("the links lock");
            match links.get(host) {
                Some(live) if live.generation == generation => {
                    links.remove(host);
                }
                Some(_) => return,
                None => {}
            }
        }
        self.sent.lock().expect("the sent lock").remove(host);
        let event = self.mesh.lock().expect("the mesh lock").down(host, why);
        if let Some(event) = event {
            self.announce_event(event);
        }
    }
}

/// This box's peering, as everything outside it holds it.
#[derive(Clone)]
pub struct Peer {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Peer")
            .field("host", &self.inner.host)
            .field("port", &self.inner.port)
            .finish()
    }
}

impl Peer {
    /// Bind the link listener, start the three threads - accept, tick, announce - and answer with
    /// the peer and the one channel it says anything on.
    pub fn open(cfg: PeerConfig) -> std::io::Result<(Peer, Receiver<PeerEvent>)> {
        let listener = TcpListener::bind(SocketAddr::new(cfg.bind, cfg.port))?;
        let port = listener.local_addr()?.port();
        let (tx, rx) = sync_channel(QUEUE.max(1));
        let peer = Peer {
            inner: Arc::new(Inner {
                host: cfg.host,
                cookie: cfg.cookie,
                port,
                tick: cfg.tick,
                mesh: Mutex::new(Mesh::new(cfg.tick)),
                links: Mutex::new(BTreeMap::new()),
                next_generation: AtomicU64::new(1),
                dialing: Mutex::new(BTreeSet::new()),
                seats: Mutex::new(Vec::new()),
                projects: Mutex::new(Vec::new()),
                sent: Mutex::new(BTreeMap::new()),
                events: tx,
                stop: AtomicBool::new(false),
                dropped: AtomicU64::new(0),
                refusals: Mutex::new(Vec::new()),
            }),
        };
        peer.spawn_accept(listener);
        peer.spawn_tick();
        if cfg.announce {
            peer.spawn_announce(cfg.group, cfg.group_port);
        }
        Ok((peer, rx))
    }

    pub fn host(&self) -> &str {
        &self.inner.host
    }

    /// The TCP port links arrive at: what a beacon carries, and what a peer must be told.
    pub fn port(&self) -> u16 {
        self.inner.port
    }

    pub fn tick(&self) -> Tick {
        self.inner.tick
    }

    /// Where a peer on this host would dial this one, for a test and for a box with one interface.
    pub fn addr(&self) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), self.inner.port)
    }

    /// What this box's seats are on, which every live link is told when it changes.
    pub fn publish(&self, seats: Vec<SeatProfile>) {
        let mut mine = self.inner.seats.lock().expect("the seats lock");
        if *mine == seats {
            return;
        }
        *mine = seats;
    }

    pub fn seats(&self) -> Vec<SeatProfile> {
        self.inner.seats.lock().expect("the seats lock").clone()
    }

    /// What this box's projects are, by repository identity: what a peer's seat is matched against
    /// before anybody is introduced.
    pub fn publish_projects(&self, projects: Vec<ProjectProfile>) {
        let mut mine = self.inner.projects.lock().expect("the projects lock");
        if *mine == projects {
            return;
        }
        *mine = projects;
    }

    /// The project here that is the repository `identity` names, when this box has one and it has
    /// not opted out of being introduced.
    pub fn project_of(&self, identity: &str) -> Option<ProjectProfile> {
        self.inner
            .projects
            .lock()
            .expect("the projects lock")
            .iter()
            .find(|p| p.introduce && !p.identity.is_empty() && p.identity.as_str() == identity)
            .cloned()
    }

    /// Every seat a live peer holds: the mesh's half of a roster.
    pub fn peer_seats(&self) -> Vec<SeatProfile> {
        self.inner.mesh.lock().expect("the mesh lock").seats()
    }

    /// Every seat of every box this one knows of, each with whether its box is linked now: what a
    /// roster draws, where an away box's seats stay in the list and lose only their being here.
    pub fn seats_with_state(&self) -> Vec<(SeatProfile, bool)> {
        self.inner
            .mesh
            .lock()
            .expect("the mesh lock")
            .seats_with_state()
    }

    /// The seat with this id, on the box that holds it - and only while that box is linked.
    pub fn seat(&self, id: &str) -> Option<SeatProfile> {
        self.inner.mesh.lock().expect("the mesh lock").seat(id)
    }

    pub fn holds(&self, id: &str) -> bool {
        self.seat(id).is_some()
    }

    pub fn alive(&self, host: &str) -> bool {
        self.inner.mesh.lock().expect("the mesh lock").alive(host)
    }

    /// Why `host` is away, in the words the operator reads - the far side's own for a `Bye`.
    pub fn why(&self, host: &str) -> String {
        self.inner
            .mesh
            .lock()
            .expect("the mesh lock")
            .link(host)
            .map(|l| l.why.clone())
            .unwrap_or_default()
    }

    /// The hosts whose links are up, and the ones known and not answering.
    pub fn up(&self) -> Vec<String> {
        self.inner.mesh.lock().expect("the mesh lock").live()
    }

    pub fn away(&self) -> Vec<String> {
        self.inner.mesh.lock().expect("the mesh lock").away()
    }

    /// Send one frame to one box, if its link is up. `false` is *nothing was sent*: no link, or a
    /// link that just died trying.
    pub fn ship(&self, host: &str, frame: PeerFrame) -> bool {
        if !self.alive(host) {
            return false;
        }
        self.inner.ship(host, &frame)
    }

    /// Dial a box this one was told about - by a beacon, or by an operator who named it.
    pub fn connect_at(&self, addr: SocketAddr) {
        if self.inner.stop.load(Ordering::SeqCst) {
            return;
        }
        let inner = Arc::clone(&self.inner);
        std::thread::spawn(move || {
            if let Err(e) = dial(&inner, addr) {
                eprintln!("room: dialling {addr} failed ({e})");
            }
        });
    }

    /// The clock's round, for a test that must not wait twenty seconds for a link to be called
    /// away. The thread does this on its own every tick.
    pub fn tick_once(&self) -> Vec<PeerEvent> {
        let now = self.inner.now();
        let mut out = Vec::new();
        for host in self.up() {
            self.inner.ship(&host, &PeerFrame::Ping { at: now });
            let seats = self.inner.seats.lock().expect("the seats lock").clone();
            let stale = self
                .inner
                .sent
                .lock()
                .expect("the sent lock")
                .get(&host)
                .is_none_or(|sent| *sent != seats);
            if stale {
                self.inner
                    .sent
                    .lock()
                    .expect("the sent lock")
                    .insert(host.clone(), seats.clone());
                self.inner.ship(&host, &PeerFrame::Profiles { seats });
            }
        }
        let down = self
            .inner
            .mesh
            .lock()
            .expect("the mesh lock")
            .tick_round(now);
        for event in down {
            if let PeerEvent::Down { host, .. } = &event {
                // The socket goes too: a reader still blocked on a box the tick called away would
                // hold a descriptor and a thread for as long as the far side is silent.
                let live = self
                    .inner
                    .links
                    .lock()
                    .expect("the links lock")
                    .remove(host);
                if let Some(live) = live
                    && let Ok(stream) = live.stream.lock()
                {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
                self.inner.sent.lock().expect("the sent lock").remove(host);
            }
            out.push(event);
        }
        out
    }

    /// Stop announcing, stop dialling, and close every link. A stopped peer says nothing to
    /// anybody and takes nothing; the coordinator is going away.
    pub fn stop(&self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        let links: Vec<(String, Live)> = self
            .inner
            .links
            .lock()
            .expect("the links lock")
            .iter()
            .map(|(h, live)| (h.clone(), live.clone()))
            .collect();
        for (host, live) in links {
            if let Ok(mut stream) = live.stream.lock() {
                let mut line = PeerFrame::Bye {
                    why: STOPPING.to_string(),
                }
                .line();
                line.push('\n');
                let _ = stream.write_all(line.as_bytes());
                let _ = stream.flush();
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
            self.inner
                .unlinked(&host, live.generation, "this box stopped peering");
        }
    }

    /// How many events were dropped for want of a reader - a number a mesh pane can say.
    pub fn dropped(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// The last few links this box would not make, in the words the operator reads: a wrong
    /// cookie, a protocol neither side speaks. Newest last, oldest dropped.
    pub fn refusals(&self) -> Vec<String> {
        self.inner
            .refusals
            .lock()
            .expect("the refusals lock")
            .clone()
    }

    fn spawn_accept(&self, listener: TcpListener) {
        let inner = Arc::clone(&self.inner);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if inner.stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let inner = Arc::clone(&inner);
                std::thread::spawn(move || {
                    if let Err(e) = run_link(&inner, stream, Dialed::In) {
                        eprintln!("room: a box that dialled this one left the link unread ({e})");
                    }
                });
            }
        });
    }

    fn spawn_tick(&self) {
        let peer = self.clone();
        let every = self.inner.tick.every_ms.max(50);
        std::thread::spawn(move || {
            while !peer.inner.stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(every));
                if peer.inner.stop.load(Ordering::SeqCst) {
                    break;
                }
                for event in peer.tick_once() {
                    peer.inner.announce_event(event);
                }
            }
        });
    }

    fn spawn_announce(&self, group: Ipv4Addr, port: u16) {
        let peer = self.clone();
        std::thread::spawn(move || {
            let announcer = match Announcer::open(group, port) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!(
                        "room: this box cannot announce itself on {group} ({e}) - peering is up, \
                         and only the boxes that were named to it will be dialled"
                    );
                    return;
                }
            };
            let beacon = Beacon::new(peer.host(), peer.port());
            let mut said = 0u64;
            while !peer.inner.stop.load(Ordering::SeqCst) {
                let now = peer.inner.now();
                if now.saturating_sub(said) >= announce::EVERY_MS {
                    said = now;
                    if let Err(e) = announcer.announce(&beacon) {
                        eprintln!("room: the beacon to {group} failed ({e})");
                    }
                }
                let heard = match announcer.hear() {
                    Ok(Some((beacon, from))) => Some((beacon, from)),
                    Ok(None) => None,
                    Err(e) => {
                        eprintln!("room: listening to {group} failed ({e})");
                        return;
                    }
                };
                let Some((beacon, from)) = heard else {
                    continue;
                };
                if beacon.host == peer.host() {
                    continue;
                }
                let addr = beacon.at(from);
                peer.inner.mesh.lock().expect("the mesh lock").touching(
                    &beacon.host,
                    addr,
                    peer.inner.now(),
                );
                if peer.alive(&beacon.host) || !announce::dials(peer.host(), &beacon.host) {
                    continue;
                }
                // One dial at a time per box: beacons arrive every two seconds, and a box that is
                // refusing the cookie would otherwise be dialled for ever.
                if !peer
                    .inner
                    .dialing
                    .lock()
                    .expect("the dialing lock")
                    .insert(beacon.host.clone())
                {
                    continue;
                }
                let inner = Arc::clone(&peer.inner);
                let host = beacon.host.clone();
                std::thread::spawn(move || {
                    if let Err(e) = dial(&inner, addr) {
                        eprintln!("room: dialling {host} at {addr} failed ({e})");
                    }
                    inner
                        .dialing
                        .lock()
                        .expect("the dialing lock")
                        .remove(&host);
                });
            }
        });
    }
}

/// Which end of a link this is: the box that dialled, or the box that was dialled.
enum Dialed {
    In,
    Out(SocketAddr),
}

/// Dial one box and run the link until it ends.
fn dial(inner: &Arc<Inner>, addr: SocketAddr) -> std::io::Result<()> {
    let stream = TcpStream::connect_timeout(&addr, DIAL)?;
    run_link(inner, stream, Dialed::Out(addr))
}

/// The link's whole life: the handshake, then every frame until the far side goes.
fn run_link(inner: &Arc<Inner>, stream: TcpStream, how: Dialed) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_write_timeout(Some(WRITE))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream.try_clone()?;
    let up = match how {
        Dialed::In => answer_handshake(inner, &mut reader, &mut writer)?,
        Dialed::Out(addr) => dial_handshake(inner, &mut reader, &mut writer, addr)?,
    };
    let Some((far, addr)) = up else {
        return Ok(());
    };
    let generation = inner.linked(&far, addr, stream);
    let why = read_loop(inner, &mut reader, &far);
    inner.unlinked(&far, generation, &why);
    Ok(())
}

fn answer_handshake(
    inner: &Arc<Inner>,
    reader: &mut BufReader<TcpStream>,
    writer: &mut TcpStream,
) -> std::io::Result<Option<(String, SocketAddr)>> {
    let mut state = Answerer::new(&inner.host, inner.port, nonce()?);
    let mut claimed: Option<(String, u16)> = None;
    loop {
        let Some(frame) = read_frame(reader)? else {
            return Ok(None);
        };
        if let PeerFrame::Hello { host, port, .. } = &frame {
            claimed = Some((host.clone(), *port));
        }
        let step = state.on(&frame, &inner.cookie);
        if let Some(say) = &step.say {
            write_frame(writer, say)?;
        }
        match step.outcome {
            Outcome::More => {}
            Outcome::Refused(why) => {
                inner.refused(&why);
                return Ok(None);
            }
            Outcome::Up { .. } => {
                let from = writer.peer_addr()?;
                let Some((host, port)) = claimed.clone() else {
                    return Err(std::io::Error::other("a link came up with no name for it"));
                };
                let addr = SocketAddr::new(from.ip(), port);
                return Ok(Some((host, addr)));
            }
        }
    }
}

fn dial_handshake(
    inner: &Arc<Inner>,
    reader: &mut BufReader<TcpStream>,
    writer: &mut TcpStream,
    addr: SocketAddr,
) -> std::io::Result<Option<(String, SocketAddr)>> {
    let mut state = Dialer::new(&inner.host, inner.port, nonce()?);
    write_frame(writer, &state.hello())?;
    loop {
        let Some(frame) = read_frame(reader)? else {
            return Ok(None);
        };
        let step = state.on(&frame, &inner.cookie);
        if let Some(say) = &step.say {
            write_frame(writer, say)?;
        }
        match step.outcome {
            Outcome::More => {}
            Outcome::Refused(why) => {
                inner.refused(&why);
                return Ok(None);
            }
            Outcome::Up { host, .. } => return Ok(Some((host, addr))),
        }
    }
}

/// Everything after the handshake: the tick, a box's seats, and the messages addressed here.
fn read_loop(inner: &Arc<Inner>, reader: &mut BufReader<TcpStream>, far: &str) -> String {
    loop {
        let frame = match read_frame(reader) {
            Ok(Some(frame)) => frame,
            Ok(None) => return format!("{far} closed the link"),
            Err(e) => return format!("reading from {far} failed ({e})"),
        };
        inner
            .mesh
            .lock()
            .expect("the mesh lock")
            .heard(far, inner.now());
        match frame {
            PeerFrame::Ping { .. } => {}
            PeerFrame::Profiles { mut seats } => {
                if seats.len() > MAX_SEATS {
                    seats.truncate(MAX_SEATS);
                }
                let event = inner
                    .mesh
                    .lock()
                    .expect("the mesh lock")
                    .seats_of(far, seats);
                if let Some(event) = event {
                    inner.announce_event(event);
                }
            }
            PeerFrame::Message { message } => {
                inner.announce_event(PeerEvent::Message(Box::new(message)));
            }
            PeerFrame::Bye { why } => return format!("{far} said goodbye: {why}"),
            PeerFrame::Hello { .. }
            | PeerFrame::Challenge { .. }
            | PeerFrame::Proved { .. }
            | PeerFrame::Ready { .. }
            | PeerFrame::Refused { .. } => {
                return format!("{far} sent a handshake frame on a link that was up");
            }
        }
    }
}

fn write_frame(writer: &mut TcpStream, frame: &PeerFrame) -> std::io::Result<()> {
    let mut line = frame.line();
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()
}

/// One line as a frame. `Ok(None)` is a clean end; an unreadable line is an error, because a link
/// whose frames do not parse is not a link that should keep reading.
fn read_frame(reader: &mut BufReader<TcpStream>) -> std::io::Result<Option<PeerFrame>> {
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    if line.len() > MAX_LINE {
        return Err(std::io::Error::other(
            "a frame longer than any this build sends",
        ));
    }
    PeerFrame::of(line.trim_end())
        .map(Some)
        .ok_or_else(|| std::io::Error::other("a frame this build does not read"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_is_a_host_a_cookie_and_the_gates_own_tick() {
        let cfg = PeerConfig::new("lab2x1", Cookie::of("a word for this lan"));
        assert_eq!(cfg.host, "lab2x1");
        assert!(cfg.announce);
        assert_eq!(cfg.group.to_string(), announce::GROUP);
        assert_eq!(cfg.group_port, announce::PORT);
        assert_eq!(cfg.tick, Tick::new(5_000, 4));
        assert_eq!(cfg.tick.window_ms(), 20_000);
        let quiet = cfg.clone().without_announce().with_port(47100);
        assert!(!quiet.announce);
        assert_eq!(quiet.port, 47100);
    }
}
