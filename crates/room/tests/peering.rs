//! Two coordinators in one process, on loopback, with two fake host names: the whole of lead 71's
//! first slice, from the cookie to the introduction.
//!
//! The boxes dial each other by address rather than by beacon - a test may not depend on a LAN, and
//! what discovery adds to a link is an address, which [`letibot_room::peer::announce`]'s own test
//! covers on the box's real multicast stack.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_room::client::Client;
use letibot_room::peer::{
    Cookie, CookieError, Peer, PeerConfig, RepoIdentity, SeatProfile, Tick, cookie,
    handshake::{Dialer, nonce},
};
use letibot_room::server::Server;
use letibot_room::{
    Event, Group, Member, Message, Refused, Request, Response, Room, Seat, Speaker,
};

mod support;

use support::{PATIENCE, TICK, wait_until};

const COOKIE: &str = "a word only this LAN knows";
const A: &str = "aaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbb";
/// The one line a peer seat is introduced with.
const INTRO_A: &str = "lab1/s-a1 (letibot, main) - landing stacks";
const INTRO_B: &str = "lab2/s-b1 (letibot, main) - provider fixes";

fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let p = std::env::temp_dir().join(format!(
        "letibot-room-peering-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&p).expect("a scratch directory");
    p
}

/// One box of the mesh: its coordinator, its peer, and the project it has.
struct Box {
    dir: PathBuf,
    server: Server,
    peer: Peer,
    project: String,
}

impl Drop for Box {
    fn drop(&mut self) {
        self.server.stop();
        self.peer.stop();
        // The peer's threads notice a stop flag; unlinking the directory under them is safe, and a
        // test that wants the socket gone waits for it rather than sleeping here.
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Box {
    /// A coordinator on `host`, with a peer on loopback and nothing announced: the test dials, and
    /// the beacon is [`letibot_room::peer::announce`]'s own business.
    fn open(tag: &str, host: &str, project: &str) -> Box {
        let dir = scratch(tag);
        let server = Server::serve(&dir.join("room.sock"), Room::new()).expect("a room");
        server.with_seat_host(host);
        let (peer, events) = Peer::open(
            PeerConfig::new(host, Cookie::of(COOKIE))
                .with_bind(IpAddr::V4(Ipv4Addr::LOCALHOST))
                .with_tick(Tick::new(60, 4))
                .without_announce(),
        )
        .expect("a peer");
        server.with_peer(peer.clone(), events);
        Box {
            dir,
            server,
            peer,
            project: project.to_string(),
        }
    }

    /// What a project here knows about the repository it is: the identity a peer's seat is matched
    /// against, and whether this project wants to be introduced at all.
    fn project_identity(&self, identity: &str, introduce: bool) {
        self.peer
            .publish_projects(vec![letibot_room::peer::ProjectProfile {
                key: self.project.clone(),
                identity: RepoIdentity::named(identity),
                introduce,
            }]);
    }

    fn publishes(&self, seat: SeatProfile) {
        self.peer.publish(vec![seat]);
    }

    fn seat(&self, name: &str) -> Seated {
        seat(&self.server, name, &self.project)
    }

    fn wait_link(&self, host: &str) {
        let deadline = Instant::now() + PATIENCE;
        while !self.peer.alive(host) {
            assert!(
                Instant::now() < deadline,
                "{} never linked to {host}: refusals {:?}, away {:?}",
                self.peer.host(),
                self.peer.refusals(),
                self.peer.away()
            );
            std::thread::sleep(TICK);
        }
    }
}

struct Seated {
    client: Client,
    heard: Receiver<(Message, bool)>,
}

fn seat(server: &Server, name: &str, project: &str) -> Seated {
    let (tx, heard) = mpsc::channel();
    let wake = Arc::new(|_: &Event| {});
    let client = Client::connect_as(
        server.path(),
        Speaker::top_level(name),
        letibot_room::room_name(&server.name(), project),
        Seat::Session,
        wake,
    )
    .unwrap_or_else(|e| panic!("`{name}` must connect: {e}"));
    let tx = Mutex::new(tx);
    client.on_chat(Arc::new(move |m, names| {
        let _ = tx.lock().unwrap().send((m.clone(), names));
    }));
    Seated { client, heard }
}

impl Seated {
    fn call(&self, r: Request) -> Response {
        self.client.request(&r).expect("an answer")
    }

    fn dm(&self, id: &str, to: &str, text: &str) -> Response {
        self.call(Request::Dm {
            id: id.into(),
            to: to.into(),
            text: text.into(),
            thread: None,
            attachments: Vec::new(),
        })
    }

    /// Everything the seat was pushed within `ms`, in order.
    fn pushed(&self, ms: u64) -> Vec<Message> {
        let deadline = Instant::now() + Duration::from_millis(ms);
        let mut out = Vec::new();
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.heard.recv_timeout(left) {
                Ok((m, _)) => out.push(m),
                Err(_) => break,
            }
        }
        out
    }
}

fn an_identity(root: &str) -> String {
    RepoIdentity::of(root, "git@github.com:deadtrickster/letibot.git")
        .as_str()
        .to_string()
}

/// A far box that is nobody's coordinator: a loopback socket that speaks the peer protocol and
/// then does whatever the test wants, including nothing at all. That is how a link is made to go
/// silent without stopping a process - the case a real box's killed coordinator leaves behind.
struct FakeBox {
    reader: std::io::BufReader<std::net::TcpStream>,
    writer: std::net::TcpStream,
    /// Bytes read but not yet a whole line: a read's own deadline can land mid-frame.
    buffered: String,
}

impl FakeBox {
    /// Dial `at` as `host` and complete the handshake.
    fn up(at: std::net::SocketAddr, host: &str, cookie: &Cookie) -> FakeBox {
        use std::io::{BufRead, BufReader, Write};
        let stream = std::net::TcpStream::connect_timeout(&at, Duration::from_secs(3))
            .expect("the far box's listener");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("a read timeout");
        let mut reader = BufReader::new(stream.try_clone().expect("a reader"));
        let mut writer = stream;
        let mut state = Dialer::new(host, 0, nonce().expect("a nonce"));
        let mut say = |frame: &letibot_room::peer::PeerFrame| {
            let mut line = frame.line();
            line.push('\n');
            writer.write_all(line.as_bytes()).expect("a frame");
            writer.flush().expect("a flush");
        };
        say(&state.hello());
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("an answer");
            let frame = letibot_room::peer::PeerFrame::of(line.trim_end())
                .unwrap_or_else(|| panic!("a frame this build reads: {line:?}"));
            let step = state.on(&frame, cookie);
            if let Some(answer) = &step.say {
                let mut line = answer.line();
                line.push('\n');
                writer.write_all(line.as_bytes()).expect("a frame");
                writer.flush().expect("a flush");
            }
            if step.is_up() {
                return FakeBox {
                    reader,
                    writer,
                    buffered: String::new(),
                };
            }
            assert!(step.why().is_none(), "the handshake was refused: {step:?}");
        }
    }

    fn say(&mut self, frame: &letibot_room::peer::PeerFrame) {
        use std::io::Write;
        let mut line = frame.line();
        line.push('\n');
        let _ = self.writer.write_all(line.as_bytes());
        let _ = self.writer.flush();
    }

    /// Read frames until one the far side sent is seen, ignoring pings: the far box pings whether
    /// or not anybody listens.
    fn wait_for(&mut self, want: &str) {
        use std::io::BufRead;
        for _ in 0..50 {
            let mut line = String::new();
            if self.reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            if let Some(frame) = letibot_room::peer::PeerFrame::of(line.trim_end())
                && frame.word() == want
            {
                return;
            }
        }
    }

    /// The next `message` frame this box is sent within `ms`, other frames skipped: `None` when the
    /// far side said no line in that time, which is how *nothing more arrived* is checked without a
    /// sleep. A line cut off by the read's own deadline stays in the buffer for the next call.
    fn next_message(&mut self, ms: u64) -> Option<letibot_room::Message> {
        use letibot_room::peer::PeerFrame;
        use std::io::BufRead;
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            if let Some(at) = self.buffered.find('\n') {
                let line: String = self.buffered.drain(..=at).collect();
                if let Some(PeerFrame::Message { message }) = PeerFrame::of(line.trim_end()) {
                    return Some(message);
                }
                continue;
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            self.reader.get_ref().set_read_timeout(Some(left)).ok();
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => return None,
                Ok(_) => self.buffered.push_str(&line),
                Err(_) => self.buffered.push_str(&line),
            }
        }
    }
}

/// Two boxes, linked, each with one seat on `identity`. A's seat is `s-a1`, B's is `s-b1`.
fn two_boxes(tag: &str, identity_a: &str, identity_b: &str, introduce: (bool, bool)) -> (Box, Box) {
    let a = Box::open(&format!("{tag}-a"), "lab1", A);
    let b = Box::open(&format!("{tag}-b"), "lab2", B);
    a.project_identity(identity_a, introduce.0);
    b.project_identity(identity_b, introduce.1);
    a.publishes(SeatProfile::new(
        "lab1/s-a1",
        &RepoIdentity::named(identity_a),
        "main",
        "landing stacks",
    ));
    b.publishes(SeatProfile::new(
        "lab2/s-b1",
        &RepoIdentity::named(identity_b),
        "main",
        "provider fixes",
    ));
    // The smaller name dials, which is the rule two boxes use to make one link.
    a.peer.connect_at(b.peer.addr());
    a.wait_link("lab2");
    b.wait_link("lab1");
    (a, b)
}

#[test]
fn a_box_that_does_not_know_the_cookie_is_refused_by_its_name() {
    // Two coordinators, two cookies: nothing links, and neither box is up on the other's mesh.
    let small = scratch("wrong-cookie");
    let (peer_a, _a) = Peer::open(
        PeerConfig::new("lab1", Cookie::of("a word only this LAN knows"))
            .with_bind(IpAddr::V4(Ipv4Addr::LOCALHOST))
            .with_tick(Tick::new(60, 4))
            .without_announce(),
    )
    .expect("a peer");
    let (peer_b, _b) = Peer::open(
        PeerConfig::new("lab2", Cookie::of("ANOTHER lan's word entirely"))
            .with_bind(IpAddr::V4(Ipv4Addr::LOCALHOST))
            .with_tick(Tick::new(60, 4))
            .without_announce(),
    )
    .expect("a peer");
    peer_a.connect_at(peer_b.addr());
    // A handshake that would succeed at once is answered by the refusal: waiting for the refusal
    // is the later fact, and the two links are what must NOT exist.
    wait_until("the wrong cookie's refusal to be recorded", || {
        !peer_a.refusals().is_empty()
    });
    assert!(!peer_a.alive("lab2"), "a wrong cookie linked anyway");
    assert!(!peer_b.alive("lab1"), "the answerer went up anyway");
    // The refusal names the box that claimed to be a peer, and says nothing of the cookie.
    let said = peer_a.refusals();
    assert!(
        said.iter()
            .any(|s| s.contains("lab2") && s.contains("cookie")),
        "{said:?}"
    );
    assert!(
        !said.iter().any(|s| s.contains("ANOTHER")),
        "a refusal never quotes the secret: {said:?}"
    );
    peer_a.stop();
    peer_b.stop();
    let _ = std::fs::remove_dir_all(small);
}

/// An old coordinator answers a newer box's handshake with a frame this build cannot read. That is
/// a protocol the two do not share, so it is refused ONCE - with what to upgrade in the sentence -
/// and the same word is not said again. Without this the link was made, refused, and made again
/// every two seconds, and nobody was told.
#[test]
fn a_box_speaking_another_protocol_is_refused_once_and_says_so_once() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    let dir = scratch("old-protocol");
    // An old build: it reads a line and answers with a word this one never wrote.
    let listener = TcpListener::bind("127.0.0.1:0").expect("the old box listens");
    let old = listener.local_addr().expect("an address");
    let answers = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&answers);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut writer = stream.try_clone().expect("a writer");
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            // The old side's own kind of answer: a frame the newer box does not know.
            let _ = writeln!(
                writer,
                "{{\"type\":\"nope\",\"why\":\"a frame this build does not read\"}}"
            );
        }
    });

    let (peer, _events) = Peer::open(
        PeerConfig::new("lab1", Cookie::of(COOKIE))
            .with_bind(IpAddr::V4(Ipv4Addr::LOCALHOST))
            .with_tick(Tick::new(60, 4))
            .without_announce(),
    )
    .expect("a peer");
    peer.connect_at(old);
    wait_until("the old box's refusal to be recorded", || {
        !peer.refusals().is_empty()
    });
    // The sentence says which protocol this box speaks and what a person can do about it.
    let said = peer.refusals();
    let one = &said[0];
    assert!(one.contains("peering protocol"), "{said:?}");
    assert!(one.contains("upgrade"), "{said:?}");
    assert!(!peer.alive("lab1"), "the old box linked anyway");

    // A second dial is a second connection and NOT a second sentence: the refusal is one fact.
    let first = answers.load(std::sync::atomic::Ordering::SeqCst);
    peer.connect_at(old);
    wait_until("the second dial to be answered", || {
        answers.load(std::sync::atomic::Ordering::SeqCst) > first
    });
    assert_eq!(
        peer.refusals().len(),
        1,
        "the same box at the same protocol was said twice: {:?}",
        peer.refusals()
    );
    peer.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_cookie_at_another_mode_is_refused_by_name_and_no_peer_is_made() {
    let dir = scratch("cookie-mode");
    let path = cookie::path_in(&dir);
    std::fs::write(&path, COOKIE).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    // This is the door the coordinator opens with: no cookie, no peer object, no peering.
    let refused = Cookie::load(&path).expect_err("0644 is refused");
    assert!(matches!(refused, CookieError::Mode { mode: 0o644, .. }));
    assert!(refused.sentence().contains("mode 0600"));
    // The mode is the operator's to fix, and then the same file is read.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        Cookie::load(&path).expect("a private cookie").len(),
        COOKIE.len()
    );
    // And a directory with no cookie file is not an error: it is a box that does not peer.
    let empty = scratch("no-cookie");
    assert_eq!(
        Cookie::load(&cookie::path_in(&empty)),
        Err(CookieError::Absent)
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

#[test]
fn a_link_that_misses_its_ticks_marks_that_box_away_and_hearing_it_again_brings_it_back() {
    let (a, b) = two_boxes(
        "ticks",
        &an_identity("1111aaaa2222"),
        &an_identity("1111aaaa2222"),
        (true, true),
    );
    assert!(a.peer.alive("lab2"));
    assert_eq!(a.peer.away(), Vec::<String>::new());
    assert!(a.peer.holds("lab2/s-b1"), "its seats are in the mesh");

    // The far box goes: its links close, and the ticks it owed nobody answers.
    b.peer.stop();
    drop(b);
    wait_until("a silent box's link to end", || !a.peer.alive("lab2"));
    assert!(!a.peer.alive("lab2"), "a silent box stayed linked");
    assert_eq!(a.peer.away(), vec!["lab2".to_string()]);
    assert!(
        !a.peer.holds("lab2/s-b1"),
        "an away box holds nothing that can be delivered"
    );
    assert_eq!(a.server.mesh(), vec![("lab2".to_string(), false)]);
}

#[test]
fn a_dm_to_seat_on_another_box_goes_over_the_link_and_reaches_that_seat() {
    let (a, b) = two_boxes(
        "dm",
        &an_identity("2222bbbb3333"),
        &an_identity("999988887777"),
        (true, true),
    );
    let sender = a.seat("s-a1");
    let target = b.seat("s-b1");
    // The room this box keeps a copy of the line in, from the sender's side.
    let said = sender.dm(
        "peer-dm-1",
        "lab2/s-b1",
        "the cache key moves with the branch",
    );
    match &said {
        Response::Said { message, .. } => {
            assert_eq!(message.to.as_deref(), Some("lab2/s-b1"));
            assert_eq!(message.author, "s-a1");
        }
        other => panic!("a dm to another box's seat was refused: {other:?}"),
    }
    // And the seat that holds the id is pushed it, addressed to the name it speaks as here.
    let pushed = target.pushed(3_000);
    let got = pushed
        .iter()
        .find(|m| m.text.contains("the cache key moves with the branch"))
        .unwrap_or_else(|| panic!("the far seat was pushed nothing: {pushed:?}"));
    assert_eq!(
        got.to.as_deref(),
        Some("s-b1"),
        "the addressee is this box's name"
    );
    assert_eq!(
        got.author, "lab1/s-a1",
        "the author is the id it was sent from"
    );
}

/// A dm long enough to be filed as a record on the box that sent it still arrives WHOLE on the
/// other box: a record's text stays where it was filed, so a reference to one would be a reference
/// the far box cannot read - it printed `[record #N, N bytes: room read #N]` and the rest was gone.
#[test]
fn a_long_dm_carries_its_text_across_the_link_instead_of_a_record_reference() {
    let (a, b) = two_boxes(
        "long-dm",
        &an_identity("6666dddd7777"),
        &an_identity("111122223333"),
        (true, true),
    );
    let sender = a.seat("s-a1");
    let target = b.seat("s-b1");
    // Longer than a record's three lines and 400 characters, and well under the line's own cap.
    let text: String = (0..12)
        .map(|i| format!("line {i}: the gate is green and the stack is landed"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        letibot_room::chat::is_record_post(&text),
        "the fixture is a record"
    );
    sender.dm("peer-dm-long", "lab2/s-b1", &text);
    let pushed = target.pushed(3_000);
    let got = pushed
        .iter()
        .find(|m| m.author == "lab1/s-a1")
        .unwrap_or_else(|| panic!("the far seat was pushed nothing: {pushed:?}"));
    assert_eq!(
        got.text, text,
        "the whole line crossed, not a record's head"
    );
    assert!(
        !got.text.contains("[record #"),
        "the far box got a reference it cannot read: {}",
        got.text
    );
}

/// A peer's seat is drawn in the roster as `nick@host`: a nick is unique on the box that minted it
/// and no further, and the seat's id is the address a send takes.
#[test]
fn a_nicked_peer_seat_is_drawn_in_the_roster_by_its_nick_and_its_host() {
    let identity = an_identity("8888eeee9999");
    let (a, b) = two_boxes("nicked", &identity, &identity, (true, true));
    // The far box names its seat, and the name crosses with the profile.
    b.publishes(
        SeatProfile::new(
            "lab2/s-b1",
            &RepoIdentity::named(&identity),
            "main",
            "the gate",
        )
        .with_nick("f36"),
    );
    wait_until("the peer's label to reach this box", || {
        a.peer.seat("lab2/s-b1").map(|s| s.nick).as_deref() == Some("f36")
    });
    let roster = a.server.roster_rows(&letibot_room::chat::project_room(A));
    let row = roster
        .iter()
        .find(|r| r.seat_id == "lab2/s-b1")
        .unwrap_or_else(|| panic!("no peer row in {roster:?}"));
    assert_eq!(row.name, "f36@lab2", "the drawn name carries the host");
    assert_eq!(row.id, "lab2/s-b1", "and the id is still the address");
}

#[test]
fn seats_on_one_repository_are_introduced_once_and_shown_in_the_roster() {
    let identity = an_identity("3333cccc4444");
    let (a, b) = two_boxes("intro", &identity, &identity, (true, true));
    let mine = a.seat("s-a1");
    let lines = mine.pushed(3_000);
    let told = lines
        .iter()
        .map(|m| m.text.matches(INTRO_B).count())
        .sum::<usize>();
    assert_eq!(told, 1, "one line per new peer seat: {lines:?}");
    // The peer's seat is in this project's roster, marked here while its box's link is up - the
    // other box holds it, and this one draws the id it would send to.
    let roster = a.server.roster_rows(&letibot_room::chat::project_room(A));
    let row = roster
        .iter()
        .find(|r| r.seat_id == "lab2/s-b1")
        .unwrap_or_else(|| panic!("no peer row in {roster:?}"));
    assert!(
        row.connected,
        "a linked box's seat is not drawn away: {row:?}"
    );

    // The same seat again is not news: a new task line changes its profile, and nothing is said.
    b.publishes(SeatProfile::new(
        "lab2/s-b1",
        &RepoIdentity::named(&identity),
        "main",
        "the queue's regate",
    ));
    // The later fact is the profile itself arriving; an introduction said about it would be pushed
    // before that, so what the seat was told after it is the whole of the check.
    wait_until("the new task line to reach this box's mesh", || {
        a.peer.seat("lab2/s-b1").map(|s| s.task).as_deref() == Some("the queue's regate")
    });
    let again = mine.pushed(400);
    assert!(
        !again.iter().any(|m| m.text.contains(INTRO_B)),
        "an introduction was said twice: {again:?}"
    );
}

#[test]
fn seats_on_different_repositories_are_not_introduced() {
    let (a, _b) = two_boxes(
        "unrelated",
        &an_identity("4444dddd5555"),
        &an_identity("5555eeee6666"),
        (true, true),
    );
    let mine = a.seat("s-a1");
    let lines = mine.pushed(1_000);
    assert!(
        !lines.iter().any(|m| m.text.contains(INTRO_B)),
        "two repositories were introduced: {lines:?}"
    );
    // The seat is in the mesh - a message to it is routable - and it is not in this room.
    assert!(a.peer.holds("lab2/s-b1"));
    let roster = a.server.roster_rows(&letibot_room::chat::project_room(A));
    assert!(!roster.iter().any(|r| r.seat_id == "lab2/s-b1"));
}

#[test]
fn a_project_that_opts_out_is_not_introduced() {
    let identity = an_identity("6666ffff7777");
    // A's project said `[peering] introduce = false`; B's did not, so B is told about A's seat.
    let (a, b) = two_boxes("opt-out", &identity, &identity, (false, true));
    let mine = a.seat("s-a1");
    let lines = mine.pushed(1_000);
    assert!(
        !lines.iter().any(|m| m.text.contains(INTRO_B)),
        "a project that opted out was introduced: {lines:?}"
    );
    let theirs = b.seat("s-b1");
    let told = theirs.pushed(1_000);
    assert!(
        told.iter().any(|m| m.text.contains(INTRO_A)),
        "the box that did not opt out was not told: {told:?}"
    );
}

#[test]
fn a_box_with_no_cookie_has_no_mesh_and_a_seat_on_another_box_is_nobody() {
    // The shipped case: no cookie, no peer, and the server behaves as it did before peering.
    let dir = scratch("no-peer");
    let server = Server::serve(&dir.join("room.sock"), Room::new()).expect("a room");
    server.with_seat_host("lab1");
    assert!(server.peer().is_none());
    let local = seat(&server, "s-1", A);
    let refusal = local.dm("no-peer-1", "lab2/s-1", "for another box's s-1");
    match refusal {
        Response::Refused { why, .. } => {
            assert!(
                matches!(why, Refused::UnknownAddressee { ref name, .. } if name == "lab2/s-1"),
                "{why:?}"
            );
        }
        other => panic!("a cross-box id was not refused: {other:?}"),
    }
    // Nothing was misdelivered to this box's own `s-1`, which is what the refusal is for.
    assert!(
        !local
            .pushed(200)
            .iter()
            .any(|m| m.to.as_deref() == Some("s-1")),
        "a dm to another box reached this box's seat"
    );
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The report's first bug: a link came back with the very same seat list, so nothing was said, and
/// the far box's seats stayed marked away in the roster for ever.
#[test]
fn a_box_that_comes_back_with_an_unchanged_seat_list_has_its_seats_marked_here_again() {
    use letibot_room::peer::PeerFrame;
    let identity = an_identity("7777aaaa8888");
    let a = Box::open("back-a", "lab1", A);
    a.project_identity(&identity, true);
    a.publishes(SeatProfile::new(
        "lab1/s-a1",
        &RepoIdentity::named(&identity),
        "main",
        "landing stacks",
    ));
    let mine = a.seat("s-a1");
    let theirs = SeatProfile::new(
        "lab2/s-b1",
        &RepoIdentity::named(&identity),
        "main",
        "provider fixes",
    );

    // A far box links, says what it holds, and then goes silent the way a killed coordinator does:
    // no `Bye`, so only the tick can notice. Nothing is sent while it is away.
    let mut first = FakeBox::up(a.peer.addr(), "lab2", &Cookie::of(COOKIE));
    first.wait_for("profiles");
    first.say(&PeerFrame::Profiles {
        seats: vec![theirs.clone()],
    });
    wait_until("the fake box to link", || a.peer.alive("lab2"));
    assert!(a.peer.alive("lab2"), "the fake box never linked");
    wait_until("the seat list to arrive", || a.peer.holds("lab2/s-b1"));
    // Silent from here: four tick intervals of nothing and the link is away, which is the only
    // thing that can notice a box that never said goodbye.
    wait_until("a silent link to end", || !a.peer.alive("lab2"));
    assert!(!a.peer.alive("lab2"), "a silent link stayed up");
    assert!(
        a.peer.why("lab2").contains("missed"),
        "the tick is what ends a link that never said goodbye: {}",
        a.peer.why("lab2")
    );
    // It keeps its seats, away - a roster that dropped them would forget who it shares with.
    let away = a.server.roster_rows(&letibot_room::chat::project_room(A));
    let row = away
        .iter()
        .find(|r| r.seat_id == "lab2/s-b1")
        .unwrap_or_else(|| panic!("the away box's seat left the roster: {away:?}"));
    assert!(!row.connected, "an away box's seat is drawn away");

    // The same box comes back on a new connection with the SAME list. The seats are marked here
    // again - that is what a roster needs, and it is said once, not once per reconnect.
    let _second = FakeBox::up(a.peer.addr(), "lab2", &Cookie::of(COOKIE));
    wait_until("the box that came back to link", || a.peer.alive("lab2"));
    assert!(a.peer.alive("lab2"), "the box that came back is not linked");
    // The list it sent the first time is what a reconnect re-announces: no second Profiles frame
    // is needed for this, and none was sent.
    let back = a.server.roster_rows(&letibot_room::chat::project_room(A));
    let row = back
        .iter()
        .find(|r| r.seat_id == "lab2/s-b1")
        .unwrap_or_else(|| panic!("the box that came back has no seat row: {back:?}"));
    assert!(row.connected, "a box that came back is drawn away: {row:?}");
    assert!(a.peer.holds("lab2/s-b1"));
    // And the introduction was said once in all of this: a reconnect is not news.
    let lines = mine.pushed(600);
    let told = lines.iter().filter(|m| m.text.contains(INTRO_B)).count();
    assert_eq!(told, 1, "an introduction was said per reconnect: {lines:?}");
}

/// The report's second bug: the far box's goodbye was rendered as this box's own.
#[test]
fn a_goodbye_is_worded_as_the_far_boxs_reason() {
    use letibot_room::peer::{PeerFrame, STOPPING};
    let a = Box::open("bye", "lab1", A);
    let mut far = FakeBox::up(a.peer.addr(), "lab2", &Cookie::of(COOKIE));
    wait_until("the fake box to link", || a.peer.alive("lab2"));
    assert!(a.peer.alive("lab2"));
    // Exactly the words this build's own `stop` sends: the sender's box, not the reader's.
    far.say(&PeerFrame::Bye {
        why: STOPPING.to_string(),
    });
    wait_until("the goodbye to end the link", || !a.peer.alive("lab2"));
    assert_eq!(
        a.peer.why("lab2"),
        "lab2 said goodbye: its coordinator is stopping",
        "the reason is the far box's, and the sentence has to read as that box's"
    );
}

/// A seat as a project room knows it: a top-level member, which is what gives it a project.
fn top(session: &str, handle: &str) -> Member {
    Member {
        session: session.into(),
        label: handle.into(),
        handle: handle.into(),
        group: Group::Top,
        parent: None,
        depth: 0,
        live: true,
    }
}

fn tell(s: &Seated, rows: Vec<Member>) {
    let said = s.call(Request::Members {
        rows,
        root: String::new(),
    });
    assert!(
        !matches!(said, Response::Refused { .. }),
        "a member list is taken: {said:?}"
    );
}

/// A consent is room STATE and it travels: a pair allowed on one box is allowed on the other, so
/// the same two seats are not asked twice. The pair is the two NAMES, which is why the rule holds
/// for a dm that would be judged by either box.
#[test]
fn a_consent_given_on_one_box_is_honoured_by_the_other() {
    let (a, b) = two_boxes("consent", "id-a", "id-b", (false, false));

    // Two projects on each box, and the same pair of names in both: what a consent is about. A
    // project key is twelve hex characters, which is what a room name may carry.
    let ax = seat(&a.server, "s-x", "111111111111");
    let ay = seat(&a.server, "s-y", "222222222222");
    tell(&ax, vec![top("s-x", "x")]);
    tell(&ay, vec![top("s-y", "y")]);
    let bx = seat(&b.server, "s-x", "333333333333");
    let by = seat(&b.server, "s-y", "444444444444");
    tell(&bx, vec![top("s-x", "x")]);
    tell(&by, vec![top("s-y", "y")]);

    // Before anything is allowed, THIS box asks too: the pair is nobody's to assume.
    let first = bx.dm("before", "s-y", "across");
    assert!(
        matches!(
            first,
            Response::Refused {
                why: Refused::DmNeedsConsent { .. },
                ..
            }
        ),
        "lab2 has its own reason to ask: {first:?}"
    );

    // The consent is given on the OTHER box, where its addressee's word is enough for this one.
    let asked = ax.dm("a-1", "s-y", "may I write to you?");
    let Response::Refused {
        why: Refused::DmNeedsConsent { ask, .. },
        ..
    } = asked
    else {
        panic!("a dm across projects asks first: {asked:?}");
    };
    let answered = ay.call(Request::Reply {
        id: "consent".into(),
        ask,
        text: "allow for the session".into(),
    });
    assert!(
        matches!(answered, Response::Asked { .. }),
        "the consent settles: {answered:?}"
    );

    // And lab2 stops asking: the row travelled over the link and is read there as its own.
    let mut tries = 0;
    wait_until("the consent to reach lab2", || {
        tries += 1;
        match bx.dm(&format!("b-{tries}"), "s-y", "across") {
            Response::Said { .. } => true,
            Response::Refused {
                why: Refused::DmNeedsConsent { .. },
                ..
            } => false,
            other => panic!("lab2 answered a dm with something else: {other:?}"),
        }
    });
}

/// A's box with a far box `lab2` that linked, announced one seat, and then went away: the seat is
/// still in this box's mesh and its box is not answering. This is what a killed coordinator on the
/// other side of the LAN leaves behind, and the case a dm to that seat used to be refused for.
fn a_box_with_an_away_peer(tag: &str) -> Box {
    let identity = an_identity("aaaa1111bbbb");
    let a = Box::open(&format!("{tag}-a"), "lab1", A);
    a.project_identity(&identity, true);
    a.publishes(SeatProfile::new(
        "lab1/s-a1",
        &RepoIdentity::named(&identity),
        "main",
        "landing stacks",
    ));
    let mut far = FakeBox::up(a.peer.addr(), "lab2", &Cookie::of(COOKIE));
    wait_until("the far box to link", || a.peer.alive("lab2"));
    far.say(&letibot_room::peer::PeerFrame::Profiles {
        seats: vec![SeatProfile::new(
            "lab2/s-b1",
            &RepoIdentity::named(&identity),
            "main",
            "provider fixes",
        )],
    });
    wait_until("the far box's seat to arrive", || a.peer.holds("lab2/s-b1"));
    // The box goes: its link closes, and its seat stays in the mesh while its box is away.
    drop(far);
    wait_until("the far box to go away", || !a.peer.alive("lab2"));
    assert_eq!(a.server.mesh(), vec![("lab2".to_string(), false)]);
    a
}

/// The bug: a dm to a seat on an AWAY box was refused at the sender and lost. An away box keeps its
/// seats, so the line is taken and kept for it.
#[test]
fn a_dm_to_a_seat_on_an_away_box_is_taken_and_kept_for_it() {
    let a = a_box_with_an_away_peer("away-take");
    let sender = a.seat("s-a1");
    let said = sender.dm("away-1", "lab2/s-b1", "the stack is green, land it");
    match &said {
        Response::Said { message, .. } => {
            assert_eq!(message.to.as_deref(), Some("lab2/s-b1"));
        }
        other => panic!("a dm to an away box's seat was refused: {other:?}"),
    }
    assert_eq!(
        a.server.held_away(),
        vec![("lab2".to_string(), 1)],
        "the line is kept for the box that is away"
    );
}

/// An away box is not nobody: a seat it never announced, and a box this one has never heard of, are
/// still refused - a held line must not become a queue for any spelling at all.
#[test]
fn a_seat_no_box_announced_is_refused_while_its_box_is_away() {
    let a = a_box_with_an_away_peer("away-stranger");
    let sender = a.seat("s-a1");
    for id in ["lab2/s-b9", "lab3/s-b1"] {
        match sender.dm(&format!("no-{id}"), id, "for somebody who is not there") {
            Response::Refused {
                why: Refused::UnknownAddressee { name, .. },
                ..
            } => assert_eq!(name, id),
            other => panic!("{id} was answered with {other:?}"),
        }
    }
    assert!(
        a.server.held_away().is_empty(),
        "a refused line was queued anyway: {:?}",
        a.server.held_away()
    );
}

/// And when the box comes back, what was kept for it goes in the order it was said - once, and
/// then nothing more.
#[test]
fn a_box_that_comes_back_is_sent_the_lines_kept_for_it_in_order() {
    let a = a_box_with_an_away_peer("away-ship");
    let sender = a.seat("s-a1");
    let said = [
        "first: the queue is clear",
        "second: the gate is running",
        "third: it is green",
    ];
    for (n, text) in said.iter().enumerate() {
        match sender.dm(&format!("ship-{n}"), "lab2/s-b1", text) {
            Response::Said { .. } => {}
            other => panic!("line {n} was refused: {other:?}"),
        }
    }
    assert_eq!(a.server.held_away(), vec![("lab2".to_string(), 3)]);

    // The box dials in again: the link comes up and what was kept goes over it.
    let mut back = FakeBox::up(a.peer.addr(), "lab2", &Cookie::of(COOKIE));
    wait_until("the box that came back to link", || a.peer.alive("lab2"));
    let mut got: Vec<letibot_room::Message> = Vec::new();
    while got.len() < said.len() {
        let line = back
            .next_message(10_000)
            .unwrap_or_else(|| panic!("the queue stopped after {}: {got:?}", got.len()));
        got.push(line);
    }
    let texts: Vec<&str> = got.iter().map(|m| m.text.as_str()).collect();
    assert_eq!(texts, said, "the queue crosses in the order it was said");
    for line in &got {
        assert_eq!(line.to.as_deref(), Some("lab2/s-b1"));
        assert_eq!(
            line.author, "lab1/s-a1",
            "the author is the id it was sent from"
        );
    }
    assert!(
        back.next_message(300).is_none(),
        "a line kept for the box was sent twice"
    );
    assert!(
        a.server.held_away().is_empty(),
        "what went over the link is still queued: {:?}",
        a.server.held_away()
    );
}

/// The queue is bounded: past the bound the OLDEST goes, stated on the log, so a box that is away
/// for a week does not grow this one's heap without end.
#[test]
fn an_away_boxs_queue_drops_the_oldest_past_its_bound() {
    let a = a_box_with_an_away_peer("away-bound");
    a.server.with_held(2);
    let sender = a.seat("s-a1");
    for n in 0..4 {
        match sender.dm(&format!("bound-{n}"), "lab2/s-b1", &format!("line {n}")) {
            Response::Said { .. } => {}
            other => panic!("line {n} was refused: {other:?}"),
        }
    }
    assert_eq!(
        a.server.held_away(),
        vec![("lab2".to_string(), 2)],
        "the queue is the bound and no wider"
    );
    // What is left is the newest two, which is what a box that comes back is sent.
    let mut back = FakeBox::up(a.peer.addr(), "lab2", &Cookie::of(COOKIE));
    wait_until("the box that came back to link", || a.peer.alive("lab2"));
    let first = back.next_message(10_000).expect("the first held line");
    let second = back.next_message(10_000).expect("the second held line");
    assert_eq!(
        (first.text.as_str(), second.text.as_str()),
        ("line 2", "line 3")
    );
    assert!(
        back.next_message(300).is_none(),
        "an overflowed line was sent too"
    );
}

/// One box's coordinator in `dir`, with a chat store beside its socket and a peer on loopback: what
/// a RESTART of that box reads, and what a peer's announced seats must outlive.
fn a_box_in(dir: &std::path::Path, host: &str) -> (Server, Peer) {
    let server = Server::serve(&dir.join("room.sock"), Room::new()).expect("a room");
    server.with_seat_host(host);
    server.with_chat_store(&dir.join("room.chat"));
    let (peer, events) = Peer::open(
        PeerConfig::new(host, Cookie::of(COOKIE))
            .with_bind(IpAddr::V4(Ipv4Addr::LOCALHOST))
            .with_tick(Tick::new(60, 4))
            .without_announce(),
    )
    .expect("a peer");
    server.with_peer(peer.clone(), events);
    (server, peer)
}

/// A far box that announced one seat and then stopped answering, and a coordinator that restarted
/// after it did. The mesh that held the seat was in memory, so only the chat store can say who the
/// box's seats are - which is the case seen on lab2x1: mac1 linked for a few seconds and stopped.
#[test]
fn a_seat_announced_before_a_restart_is_still_held_with_its_box_away() {
    let dir = scratch("away-restart");
    let (first, first_peer) = a_box_in(&dir, "lab1");
    let mut far = FakeBox::up(first_peer.addr(), "lab2", &Cookie::of(COOKIE));
    wait_until("the far box to link", || first_peer.alive("lab2"));
    far.say(&letibot_room::peer::PeerFrame::Profiles {
        seats: vec![SeatProfile::new(
            "lab2/s-b1",
            &RepoIdentity::named(&an_identity("aaaa1111bbbb")),
            "main",
            "provider fixes",
        )],
    });
    wait_until("the far box's seat to arrive", || {
        first_peer.holds("lab2/s-b1")
    });
    // The mesh holds it first; the store, which is what the restart reads, a moment later.
    wait_until("the store to keep the far box's seat", || {
        first.remembers_seats_of("lab2")
    });
    drop(far);
    wait_until("the far box to go away", || !first_peer.alive("lab2"));
    first.stop();
    drop(first_peer);
    wait_until("the first coordinator to end", || first.connections() == 0);

    let (second, second_peer) = a_box_in(&dir, "lab1");
    assert!(
        second.mesh().is_empty(),
        "the restart kept a mesh in memory: {:?}",
        second.mesh()
    );
    let sender = seat(&second, "s-a1", A);
    match sender.dm("restart-1", "lab2/s-b1", "the queue is clear, land it") {
        Response::Said { message, .. } => assert_eq!(message.to.as_deref(), Some("lab2/s-b1")),
        other => panic!("a seat announced before the restart was refused: {other:?}"),
    }
    assert_eq!(
        second.held_away(),
        vec![("lab2".to_string(), 1)],
        "the line is kept for the box the restart has no link to"
    );
    // And a box this one never heard of is still nobody, restart or not.
    match sender.dm("restart-2", "lab3/s-c1", "for somebody who was never here") {
        Response::Refused {
            why: Refused::UnknownAddressee { name, .. },
            ..
        } => assert_eq!(name, "lab3/s-c1"),
        other => panic!("an unknown box was answered with {other:?}"),
    }

    // The box comes back: what was kept for it goes over the link.
    let mut back = FakeBox::up(second_peer.addr(), "lab2", &Cookie::of(COOKIE));
    wait_until("the box that came back to link", || {
        second_peer.alive("lab2")
    });
    let line = back.next_message(10_000).expect("the held line");
    assert_eq!(line.text, "the queue is clear, land it");
    assert_eq!(
        line.author, "lab1/s-a1",
        "the author is the id it was sent from"
    );
    assert!(
        back.next_message(300).is_none(),
        "a held line was sent twice"
    );
    assert!(
        second.held_away().is_empty(),
        "what went over the link is still queued: {:?}",
        second.held_away()
    );
    second.stop();
    drop(second_peer);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A box's announcement is the whole truth about its seats: one it stops naming stops being an
/// addressee, so an id a closed session left behind is not a place to send lines for ever.
#[test]
fn a_seat_dropped_from_a_boxs_next_announcement_is_refused() {
    let a = Box::open("away-drop", "lab1", A);
    let identity = RepoIdentity::named(&an_identity("aaaa1111bbbb"));
    let seat_of = |id: &str| SeatProfile::new(id, &identity, "main", "provider fixes");
    let mut far = FakeBox::up(a.peer.addr(), "lab2", &Cookie::of(COOKIE));
    wait_until("the far box to link", || a.peer.alive("lab2"));
    far.say(&letibot_room::peer::PeerFrame::Profiles {
        seats: vec![seat_of("lab2/s-b1"), seat_of("lab2/s-b2")],
    });
    wait_until("both of the far box's seats to arrive", || {
        a.peer.holds("lab2/s-b1") && a.peer.holds("lab2/s-b2")
    });
    far.say(&letibot_room::peer::PeerFrame::Profiles {
        seats: vec![seat_of("lab2/s-b1")],
    });
    wait_until("the dropped seat to go", || !a.peer.holds("lab2/s-b2"));

    let sender = a.seat("s-a1");
    match sender.dm("drop-1", "lab2/s-b1", "still there?") {
        Response::Said { .. } => {}
        other => panic!("the seat the box still names was refused: {other:?}"),
    }
    match sender.dm("drop-2", "lab2/s-b2", "and you?") {
        Response::Refused {
            why: Refused::UnknownAddressee { name, .. },
            ..
        } => assert_eq!(name, "lab2/s-b2"),
        other => panic!("a seat the box stopped announcing was answered with {other:?}"),
    }
    assert!(
        a.server.held_away().is_empty(),
        "a live box's line was queued: {:?}",
        a.server.held_away()
    );
}
