//! Which boxes this one is linked to, and whether each is still there.
//!
//! The liveness is the gate's tick (`harnessd/src/mergequeue/gate_tick.rs`) in the other
//! direction: a box pokes, the far side counts what it hears, and a link that misses its ticks is
//! down. Lead 66's numbers are this file's defaults - every five seconds, and down after four
//! missed - because a single late tick is a busy machine and not a gone one.
//!
//! The state is a value the socket threads feed and the clock drives, so what a link's loss does
//! can be tested without a network at all: [`Mesh::tick`] takes the time.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use crate::peer::profile::SeatProfile;

/// How often a link is poked, and how many pokes it may miss. A value rather than two constants so
/// a test watches a link for a tenth of a second instead of twenty seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tick {
    pub every_ms: u64,
    pub miss: u32,
}

impl Tick {
    pub const fn new(every_ms: u64, miss: u32) -> Tick {
        Tick { every_ms, miss }
    }

    /// The window a silent box is given before its link is called down.
    pub fn window_ms(&self) -> u64 {
        self.every_ms * u64::from(self.miss)
    }
}

impl Default for Tick {
    /// The gate's own tick: `TICK` five seconds, `MISSES` four.
    fn default() -> Tick {
        Tick {
            every_ms: 5_000,
            miss: 4,
        }
    }
}

/// What changed in the mesh, for the coordinator that has to act on it: a box arriving or leaving,
/// a new list of its seats, or a message for one of this box's seats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerEvent {
    /// A link came up: `host` is reachable now, and its seats may be introduced.
    Up { host: String },
    /// A link is down, for this reason: its seats are away, and nothing is shipped to it.
    Down { host: String, why: String },
    /// What a peer's seats are on now.
    Profiles {
        host: String,
        seats: Vec<SeatProfile>,
    },
    /// A chat message for a seat on this box.
    Message(Box<crate::chat::Message>),
}

/// One link, as this box reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub host: String,
    /// Where its coordinator listens: from the beacon for one this box dialled, and from the
    /// handshake's own `Hello` for one that dialled here.
    pub addr: SocketAddr,
    /// Is the handshake done and the tick still being heard?
    pub up: bool,
    /// When anything last arrived from it, on this box's clock.
    pub last_heard: u64,
    /// The ticks it has missed since then.
    pub missed: u32,
    /// What its seats are on, as its last `Profiles` said.
    pub seats: Vec<SeatProfile>,
    /// Why it is down, once it is: the sentence the operator reads.
    pub why: String,
}

impl Link {
    pub fn live(&self) -> bool {
        self.up && self.why.is_empty()
    }
}

/// Every box this coordinator knows of: linked, dialling, or remembered as away.
#[derive(Debug, Clone)]
pub struct Mesh {
    tick: Tick,
    links: BTreeMap<String, Link>,
}

impl Mesh {
    pub fn new(tick: Tick) -> Mesh {
        Mesh {
            tick,
            links: BTreeMap::new(),
        }
    }

    pub fn tick(&self) -> Tick {
        self.tick
    }

    /// A box this one has heard of and has no link to: it is in the mesh as away from the first
    /// moment, so a roster says *that box is not answering* rather than omitting it silently.
    pub fn touching(&mut self, host: &str, addr: SocketAddr, now: u64) -> &Link {
        let entry = self.links.entry(host.to_string()).or_insert(Link {
            host: host.to_string(),
            addr,
            up: false,
            last_heard: now,
            missed: 0,
            seats: Vec::new(),
            why: String::new(),
        });
        entry.addr = addr;
        entry
    }

    /// The line for `host`, made if this is the first this box has heard of it: a box that dialled
    /// here is one nobody announced, and a link that arrived is still a link.
    fn entry(&mut self, host: &str, addr: SocketAddr, now: u64) -> &mut Link {
        let link = self.links.entry(host.to_string()).or_insert(Link {
            host: host.to_string(),
            addr,
            up: false,
            last_heard: now,
            missed: 0,
            seats: Vec::new(),
            why: String::new(),
        });
        if !addr.ip().is_unspecified() {
            link.addr = addr;
        }
        link
    }

    /// The link is up: the handshake's two proofs held. Said twice for one link, it changes
    /// nothing and returns nothing, so a reconnecting socket is not a second arrival.
    pub fn up(&mut self, host: &str, addr: SocketAddr, now: u64) -> Option<PeerEvent> {
        let link = self.entry(host, addr, now);
        link.addr = addr;
        link.last_heard = now;
        link.missed = 0;
        link.why.clear();
        if link.up {
            return None;
        }
        link.up = true;
        Some(PeerEvent::Up {
            host: host.to_string(),
        })
    }

    /// Anything at all arrived from `host`: the tick, a message, a profile list. Only a live link
    /// counts - a box is made live by a handshake, never by a frame arriving on a socket the tick
    /// already ended.
    pub fn heard(&mut self, host: &str, now: u64) -> Option<PeerEvent> {
        let link = self.links.get_mut(host)?;
        if !link.up {
            return None;
        }
        link.last_heard = now;
        link.missed = 0;
        None
    }

    /// A box's list of seats as its last frame said them.
    pub fn seats_of(&mut self, host: &str, seats: Vec<SeatProfile>) -> Option<PeerEvent> {
        let link = self.links.get_mut(host)?;
        if link.seats == seats {
            return None;
        }
        link.seats = seats.clone();
        Some(PeerEvent::Profiles {
            host: host.to_string(),
            seats,
        })
    }

    /// The link is down, by a reason the operator reads. A link already down says nothing twice.
    pub fn down(&mut self, host: &str, why: &str) -> Option<PeerEvent> {
        let link = self.links.get_mut(host)?;
        if !link.up {
            link.why = why.to_string();
            return None;
        }
        link.up = false;
        link.missed = self.tick.miss;
        link.why = why.to_string();
        Some(PeerEvent::Down {
            host: host.to_string(),
            why: why.to_string(),
        })
    }

    /// The clock's round: count the ticks each live link has missed, and end the ones that have
    /// missed too many. `now` is this box's clock in milliseconds.
    pub fn tick_round(&mut self, now: u64) -> Vec<PeerEvent> {
        let tick = self.tick;
        let mut out = Vec::new();
        for link in self.links.values_mut() {
            if !link.up {
                continue;
            }
            let late = now.saturating_sub(link.last_heard) / tick.every_ms.max(1);
            link.missed = u32::try_from(late).unwrap_or(u32::MAX);
            if link.missed >= tick.miss {
                link.up = false;
                let why = format!(
                    "{} missed {} ticks of {}ms and is away; it is back when it is heard again",
                    link.host, link.missed, tick.every_ms
                );
                link.why = why.clone();
                out.push(PeerEvent::Down {
                    host: link.host.clone(),
                    why,
                });
            }
        }
        out
    }

    /// Every box this coordinator has a live link to, in name order.
    pub fn live(&self) -> Vec<String> {
        self.links
            .values()
            .filter(|l| l.live())
            .map(|l| l.host.clone())
            .collect()
    }

    /// Every box it knows of whose link is not live: *that box's seats are away*.
    pub fn away(&self) -> Vec<String> {
        self.links
            .values()
            .filter(|l| !l.live())
            .map(|l| l.host.clone())
            .collect()
    }

    pub fn alive(&self, host: &str) -> bool {
        self.links.get(host).is_some_and(Link::live)
    }

    pub fn link(&self, host: &str) -> Option<&Link> {
        self.links.get(host)
    }

    pub fn hosts(&self) -> Vec<String> {
        self.links.keys().cloned().collect()
    }

    /// Every seat a live peer holds, by its id: the mesh's half of the roster.
    pub fn seats(&self) -> Vec<SeatProfile> {
        self.links
            .values()
            .filter(|l| l.live())
            .flat_map(|l| l.seats.iter().cloned())
            .collect()
    }

    /// The seat with this id, when a live peer holds it: what a message's `to` is routed by.
    pub fn seat(&self, id: &str) -> Option<SeatProfile> {
        self.links
            .values()
            .filter(|l| l.live())
            .flat_map(|l| l.seats.iter())
            .find(|s| s.id == id)
            .cloned()
    }

    /// Does a live peer hold this seat? The host half of an id is what a message goes by, and a
    /// box that is away holds nothing that can be delivered.
    pub fn holds(&self, id: &str) -> bool {
        self.seat(id).is_some()
    }

    pub fn forget(&mut self, host: &str) {
        self.links.remove(host);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::profile::RepoIdentity;
    use std::net::{IpAddr, Ipv4Addr};

    fn an_addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    fn a_seat(id: &str) -> SeatProfile {
        SeatProfile::new(
            id,
            &RepoIdentity::of("1a2b3c4d5e6f", "git@github.com:deadtrickster/letibot.git"),
            "main",
            "landing",
        )
    }

    #[test]
    fn four_missed_ticks_make_a_link_away_and_hearing_it_brings_it_back() {
        let tick = Tick::new(100, 4);
        let mut mesh = Mesh::new(tick);
        mesh.touching("mac", an_addr(47101), 0);
        // Nothing is up before the handshake, and a box that was never up is away, not unknown.
        assert_eq!(mesh.away(), vec!["mac".to_string()]);
        assert_eq!(
            mesh.up("mac", an_addr(47101), 0),
            Some(PeerEvent::Up {
                host: "mac".to_string()
            })
        );
        assert!(mesh.alive("mac") && mesh.away().is_empty());
        // A second `up` for one link is not a second arrival.
        assert_eq!(mesh.up("mac", an_addr(47101), 0), None);

        // One tick late, then two, then three: still up. A busy box is not a gone one.
        for now in [100u64, 200, 300] {
            assert!(mesh.tick_round(now).is_empty(), "at {now}");
            assert!(mesh.alive("mac"));
        }
        // The fourth interval it said nothing is the one that marks it away - and says so once.
        let events = mesh.tick_round(400);
        assert_eq!(events.len(), 1);
        let PeerEvent::Down { host, why } = &events[0] else {
            panic!("a down event");
        };
        assert_eq!(host, "mac");
        assert!(why.contains("4 ticks"), "{why}");
        assert!(!mesh.alive("mac"));
        assert_eq!(mesh.away(), vec!["mac".to_string()]);
        assert!(
            mesh.tick_round(500).is_empty(),
            "a link already down says nothing"
        );

        // A frame on a link the tick ended does not resurrect it: only a handshake does.
        mesh.seats_of("mac", vec![a_seat("mac/s-1")]);
        mesh.heard("mac", 600);
        assert!(!mesh.alive("mac"), "a frame is not a link");
        assert!(mesh.seat("mac/s-1").is_none(), "an away box holds nothing");
        // A new handshake makes it live again, and its seats are back in the roster.
        mesh.up("mac", an_addr(47101), 600);
        assert!(mesh.alive("mac"));
        assert_eq!(mesh.seat("mac/s-1"), Some(a_seat("mac/s-1")));
    }

    #[test]
    fn a_live_boxs_seats_are_the_meshs_and_nothing_of_an_away_boxs() {
        let mut mesh = Mesh::new(Tick::new(100, 4));
        let (a, b) = (an_addr(1), an_addr(2));
        mesh.touching("mac", b, 0);
        mesh.touching("lab3", a, 0);
        mesh.up("mac", b, 0);
        mesh.up("lab3", a, 0);
        assert_eq!(mesh.live(), vec!["lab3".to_string(), "mac".to_string()]);
        // Same repository on both boxes: two seats, one identity - that is what an introduction
        // matches on.
        mesh.seats_of("mac", vec![a_seat("mac/s-1")]);
        mesh.seats_of("lab3", vec![a_seat("lab3/cc-9")]);
        let seats = mesh.seats();
        assert_eq!(seats.len(), 2);
        assert!(mesh.holds("mac/s-1") && mesh.holds("lab3/cc-9"));
        assert!(!mesh.holds("lab3/cc-nope"));
        assert_eq!(seats[0].identity(), seats[1].identity());
        // The same list again is not a change, so nothing is re-sent or re-introduced.
        assert_eq!(mesh.seats_of("mac", vec![a_seat("mac/s-1")]), None);
        // A box that is forgotten is not in the roster and holds nothing.
        mesh.forget("lab3");
        assert_eq!(mesh.hosts(), vec!["mac".to_string()]);
        assert!(!mesh.holds("lab3/cc-9"));
    }
}
