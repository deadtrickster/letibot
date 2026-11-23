//! A coordinator whose beacon cannot send: the loop says so once, opens a new socket, and tells the
//! operator - with an injected socket, because a LAN that refuses a datagram cannot be arranged
//! here.
//!
//! The shape is the Mac that started this: a coordinator under launchd logged a failed beacon every
//! two seconds for two and a half hours with no link, and the same binary started by hand linked to
//! six boxes in a second. Nothing in the room said it was unreachable.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use letibot_room::client::Client;
use letibot_room::peer::announce::Beacon;
use letibot_room::peer::beacon::{BeaconSocket, Cadence, Sockets};
use letibot_room::peer::{Cookie, Peer, PeerConfig, Tick};
use letibot_room::server::Server;
use letibot_room::{Event, Message, Room, Seat, Speaker, room_name};

mod support;

use support::wait_until;

const COOKIE: &str = "a word only this LAN knows";

/// What a Mac whose Local Network access was never granted answers, and the words the operator has
/// to read to know it.
const WHY: &str = "No route to host (os error 65)";

/// A beacon socket that fails on demand. `reopens` is how many times the loop threw one away - the
/// socket a box cut off at boot needs and never used to get.
#[derive(Clone)]
struct Deaf {
    sends: Arc<AtomicU64>,
    reopens: Arc<AtomicU64>,
    /// Fail every send while this is set. Cleared, the same socket starts working, which is what a
    /// network coming back looks like from inside a loop.
    deaf: Arc<AtomicBool>,
    /// Return an error from every read: a socket the loop must outlive rather than return from.
    unreadable: Arc<AtomicBool>,
}

impl Deaf {
    fn new(deaf: bool, unreadable: bool) -> Deaf {
        Deaf {
            sends: Arc::new(AtomicU64::new(0)),
            reopens: Arc::new(AtomicU64::new(0)),
            deaf: Arc::new(AtomicBool::new(deaf)),
            unreadable: Arc::new(AtomicBool::new(unreadable)),
        }
    }

    fn sends(&self) -> u64 {
        self.sends.load(Ordering::SeqCst)
    }

    fn reopens(&self) -> u64 {
        self.reopens.load(Ordering::SeqCst)
    }

    fn repair(&self) {
        self.deaf.store(false, Ordering::SeqCst);
    }

    /// The factory a [`PeerConfig`] takes: every socket it asks for is this same deaf one, so a
    /// reopen is counted rather than made real.
    fn sockets(&self) -> Sockets {
        let deaf = self.clone();
        Sockets::new(move |_group: Ipv4Addr, _port: u16| {
            Ok(Box::new(deaf.clone()) as std::boxed::Box<dyn BeaconSocket>)
        })
    }
}

impl BeaconSocket for Deaf {
    fn announce(&self, _beacon: &Beacon) -> io::Result<usize> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        if self.deaf.load(Ordering::SeqCst) {
            return Err(io::Error::new(
                io::ErrorKind::HostUnreachable,
                "No route to host (os error 65)",
            ));
        }
        Ok(1)
    }

    fn hear(&self) -> io::Result<Option<(Beacon, SocketAddr)>> {
        if self.unreadable.load(Ordering::SeqCst) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Bad file descriptor",
            ));
        }
        Ok(None)
    }

    fn reopen(&mut self) -> io::Result<()> {
        self.reopens.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// A pace a test can watch an outage in: milliseconds instead of a two-second tick and two minutes
/// of silence, and a backoff long enough that one outage asks for exactly one socket.
fn cadence() -> Cadence {
    Cadence {
        every_ms: 5,
        reopen_after: 3,
        backoff_ms: 60_000,
        backoff_max_ms: 60_000,
        cut_after_ms: 40,
        summary_ms: 40,
    }
}

fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::AtomicU64 as Next;
    static NEXT: Next = Next::new(0);
    let p = std::env::temp_dir().join(format!(
        "letibot-room-beacon-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&p).expect("a scratch directory");
    p
}

/// One box: its coordinator, its peer with the deaf socket, and the socket's own name.
struct Machine {
    dir: PathBuf,
    server: Server,
    peer: Peer,
}

impl Drop for Machine {
    fn drop(&mut self) {
        self.server.stop();
        self.peer.stop();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Machine {
    fn open(tag: &str, deaf: Deaf) -> Machine {
        let dir = scratch(tag);
        let server = Server::serve(&dir.join("room.sock"), Room::new()).expect("a room");
        server.with_seat_host("lab1");
        let (peer, events) = Peer::open(
            PeerConfig::new("lab1", Cookie::of(COOKIE))
                .with_bind(IpAddr::V4(Ipv4Addr::LOCALHOST))
                .with_tick(Tick::new(60, 4))
                .with_beacon(cadence(), deaf.sockets()),
        )
        .expect("a peer");
        server.with_peer(peer.clone(), events);
        Machine { dir, server, peer }
    }
}

/// The operator's seat: what a person is pushed a blocker on, and the only reader this notice has.
struct Operator {
    _client: Client,
    seen: Arc<Mutex<Vec<Message>>>,
}

impl Operator {
    fn open(server: &Server) -> Operator {
        let seen: Arc<Mutex<Vec<Message>>> = Arc::new(Mutex::new(Vec::new()));
        let into = Arc::clone(&seen);
        let client = Client::connect_as(
            server.path(),
            Speaker::top_level("operator"),
            room_name(&server.name(), ""),
            Seat::Operator,
            Arc::new(|_: &Event| {}),
        )
        .expect("the operator's seat");
        client.on_chat(Arc::new(move |m: &Message, _names: bool| {
            into.lock().expect("the seen lock").push(m.clone());
        }));
        Operator {
            _client: client,
            seen,
        }
    }

    /// How many lines the operator has been pushed that say `needle`.
    fn count(&self, needle: &str) -> usize {
        self.seen
            .lock()
            .expect("the seen lock")
            .iter()
            .filter(|m| m.text.contains(needle))
            .count()
    }

    fn last(&self) -> Option<Message> {
        self.seen.lock().expect("the seen lock").last().cloned()
    }
}

#[test]
fn a_coordinator_that_cannot_send_tells_the_operator_once_and_opens_a_new_socket() {
    let deaf = Deaf::new(true, false);
    let machine = Machine::open("deaf", deaf.clone());
    let op = Operator::open(&machine.server);

    wait_until("the operator is told the beacon cannot send", || {
        op.count("Local Network") == 1
    });
    let said = op.last().expect("a line");
    assert_eq!(
        said.kind,
        Seat::System,
        "the room says it, not a session: {said:?}"
    );
    assert_eq!(said.say_kind(), letibot_room::SayKind::Blocker, "{said:?}");
    assert!(said.text.contains(WHY), "{}", said.text);
    assert!(said.text.contains("224.0.0.0/4"), "{}", said.text);
    // The socket went, and it went for a socket that can be made again: three failures in.
    assert_eq!(deaf.reopens(), 1, "one outage opens one socket");

    // A thousand more failed ticks are one notice, not a thousand: this is the line the Mac logged
    // 4383 times.
    let before = deaf.sends();
    wait_until("the beacon to keep failing", || deaf.sends() > before + 200);
    assert_eq!(op.count("Local Network"), 1);
    assert_eq!(deaf.reopens(), 1, "the backoff holds: no socket per tick");

    // And a network that comes back says so, once.
    deaf.repair();
    wait_until("the operator is told the box can be found again", || {
        op.count("announce itself on the LAN again") == 1
    });
    assert_eq!(op.count("Local Network"), 1, "the notice is not repeated");
    assert_eq!(
        op.count("announce itself on the LAN again"),
        1,
        "the recovery is said once"
    );
}

#[test]
fn a_socket_that_cannot_be_read_is_not_the_end_of_the_beacon_loop() {
    // The loop used to `return` on a read error: the box could then never be found again, and
    // nothing said so either.
    let deaf = Deaf::new(false, true);
    let _machine = Machine::open("unreadable", deaf.clone());
    wait_until("the beacon loop to outlive a read error", || {
        deaf.sends() > 50
    });
}
