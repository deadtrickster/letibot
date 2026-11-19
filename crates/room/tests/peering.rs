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
};
use letibot_room::server::Server;
use letibot_room::{Event, Message, Refused, Request, Response, Room, Seat, Speaker};

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
        // The peer's threads notice a stop flag; the socket file and the directory go with the
        // test rather than with a later one.
        std::thread::sleep(Duration::from_millis(20));
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
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if self.peer.alive(host) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "{} never linked to {host}: refusals {:?}, away {:?}",
            self.peer.host(),
            self.peer.refusals(),
            self.peer.away()
        );
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
    // Long enough for a handshake that would succeed at once, twice over.
    std::thread::sleep(Duration::from_millis(400));
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
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && a.peer.alive("lab2") {
        std::thread::sleep(Duration::from_millis(20));
    }
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
    // The peer's seat is in this project's roster, and it is not connected here - its box holds it.
    let roster = a.server.roster_rows(&letibot_room::chat::project_room(A));
    let row = roster
        .iter()
        .find(|r| r.seat_id == "lab2/s-b1")
        .unwrap_or_else(|| panic!("no peer row in {roster:?}"));
    assert!(!row.connected);

    // The same seat again is not news: a new task line changes its profile, and nothing is said.
    b.publishes(SeatProfile::new(
        "lab2/s-b1",
        &RepoIdentity::named(&identity),
        "main",
        "the queue's regate",
    ));
    std::thread::sleep(Duration::from_millis(400));
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
