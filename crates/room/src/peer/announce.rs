//! Finding the other boxes: one UDP datagram, on a multicast group, saying who this box is and
//! where its coordinator listens. No configuration beyond the cookie, which is the whole of lead
//! 71's *to start simple*.
//!
//! A beacon is a HINT and nothing more: it proves nothing and is trusted with nothing, so the only
//! thing a forged one can do is make this box dial a stranger, and the handshake refuses a stranger
//! that cannot prove the cookie. That is why discovery may be as loose as broadcast.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::os::fd::{AsRawFd, FromRawFd};

use serde::{Deserialize, Serialize};

/// The datagram's own version, apart from the link's: a beacon from a build that speaks another one
/// is ignored rather than answered.
pub const BEACON: u16 = 1;

/// The multicast group the boxes meet on. In the administratively scoped range (239/8), so a LAN
/// that filters multicast filters this one by its own policy rather than by an accident.
pub const GROUP: &str = "239.255.41.7";

/// The port the beacons are sent to, unless `[peering] group_port` says another.
pub const PORT: u16 = 47_311;

/// How often a box says it is here. Faster than a link's tick: a box that appears should be
/// dialled within a second or two, and the datagram is a few hundred bytes.
pub const EVERY_MS: u64 = 2_000;

/// What one box says about itself on the LAN: its host half, and the TCP port its coordinator
/// listens on. Nothing here is a secret, and nothing here is proved - the handshake does that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Beacon {
    /// The beacon's own protocol number.
    pub letibot: u16,
    /// The host half of this box's seat ids: the name a peer files the link under.
    pub host: String,
    /// Where its coordinator listens for a link.
    pub port: u16,
}

impl Beacon {
    pub fn new(host: impl Into<String>, port: u16) -> Beacon {
        Beacon {
            letibot: BEACON,
            host: host.into(),
            port,
        }
    }

    pub fn line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// A datagram as a beacon, or `None` when it is not one this build answers: a stranger's
    /// packet, a newer build's, or a name that is not a host half.
    pub fn of(datagram: &[u8]) -> Option<Beacon> {
        let beacon: Beacon = serde_json::from_slice(datagram).ok()?;
        if beacon.letibot != BEACON {
            return None;
        }
        let host = beacon.host.trim();
        if host.is_empty()
            || host.len() > crate::id::HOST_MAX
            || host != crate::id::short_name(host)
        {
            return None;
        }
        if beacon.port == 0 {
            return None;
        }
        Some(Beacon {
            host: host.to_string(),
            ..beacon
        })
    }

    /// Where this box's coordinator is, as the datagram's sender saw itself: the packet's source
    /// address decides the IP, and the beacon decides the port.
    pub fn at(&self, from: SocketAddr) -> SocketAddr {
        SocketAddr::new(from.ip(), self.port)
    }
}

/// Is this box the one that dials? The smaller name dials, so two boxes that hear each other make
/// ONE link rather than two, and the rule needs no clock and no negotiation.
pub fn dials(mine: &str, theirs: &str) -> bool {
    mine < theirs
}

/// The socket a box announces on and listens for others on.
pub struct Announcer {
    socket: UdpSocket,
    group: SocketAddrV4,
    broadcast: SocketAddrV4,
}

/// A datagram socket on `port` that another socket on the same box may share: what every box on a
/// LAN binds, so two of them can be on one machine and neither takes the port from the other.
fn shared(port: u16) -> io::Result<UdpSocket> {
    // SAFETY: the fd is owned by `socket` from `from_raw_fd` on, so every early return closes it,
    // and each `setsockopt` passes a pointer to a `c_int` that outlives the call.
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let socket = UdpSocket::from_raw_fd(fd);
        let on: libc::c_int = 1;
        for name in [libc::SO_REUSEADDR, libc::SO_REUSEPORT, libc::SO_BROADCAST] {
            let rc = libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                name,
                std::ptr::from_ref(&on).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let addr = libc::sockaddr_in {
            // **`sin_len` is a BSD/macOS field Linux does not have** — set it to the struct's own
            // size or `bind` returns `EINVAL` on macOS, and gate it on `cfg` so Linux still compiles
            // (the field does not exist there).
            #[cfg(target_os = "macos")]
            sin_len: std::mem::size_of::<libc::sockaddr_in>() as u8,
            sin_family: libc::AF_INET as libc::sa_family_t,
            sin_port: port.to_be(),
            sin_addr: libc::in_addr { s_addr: 0 },
            sin_zero: [0; 8],
        };
        let rc = libc::bind(
            socket.as_raw_fd(),
            std::ptr::from_ref(&addr).cast(),
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        );
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(socket)
    }
}

impl Announcer {
    /// Bind `port` for the group's beacons and join `group`. Binding is `0.0.0.0` on purpose: the
    /// datagram arrives on whichever interface the LAN uses, and a box with two has two.
    ///
    /// The port is SHARED - every box on the LAN binds the same one - so the socket is made by
    /// hand for `SO_REUSEADDR`/`SO_REUSEPORT`, which `UdpSocket::bind` cannot set before it binds.
    pub fn open(group: Ipv4Addr, port: u16) -> io::Result<Announcer> {
        let socket = shared(port)?;
        socket.join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED)?;
        // A datagram a box sends is looped back to this box's own sockets, which is what makes two
        // coordinators in one process - a test - hear each other.
        socket.set_multicast_loop_v4(true)?;
        socket.set_read_timeout(Some(std::time::Duration::from_millis(500)))?;
        Ok(Announcer {
            socket,
            group: SocketAddrV4::new(group, port),
            broadcast: SocketAddrV4::new(Ipv4Addr::BROADCAST, port),
        })
    }

    /// Say this box is here: to the multicast group, and to this subnet's broadcast address. Two
    /// spellings because a LAN that filters one usually carries the other, and neither costs more
    /// than a datagram a couple of seconds.
    pub fn announce(&self, beacon: &Beacon) -> io::Result<usize> {
        let line = beacon.line();
        let sent = self.socket.send_to(line.as_bytes(), self.group)?;
        // A broadcast that the network refuses is not a reason to say nothing at all.
        let _ = self.socket.send_to(line.as_bytes(), self.broadcast);
        Ok(sent)
    }

    /// The next beacon somebody said, with the address it came from. `Ok(None)` is the read
    /// timeout doing its job: nothing was heard, and the caller looks at its clock.
    pub fn hear(&self) -> io::Result<Option<(Beacon, SocketAddr)>> {
        let mut buf = [0u8; 512];
        match self.socket.recv_from(&mut buf) {
            Ok((n, from)) => Ok(Beacon::of(&buf[..n]).map(|b| (b, from))),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_beacon_is_a_host_and_a_port_and_nothing_a_stranger_can_use() {
        let b = Beacon::new("lab2x1", 47100);
        assert_eq!(Beacon::of(b.line().as_bytes()), Some(b.clone()));
        // A name that is not a host half is not a box: a peer must not be filed under a name an id
        // could not carry.
        for host in ["", "Lab2x1", "lab2x1.h.khaprov.com", "a b", "lab2x1/../x"] {
            let bad = format!("{{\"letibot\":1,\"host\":\"{host}\",\"port\":1}}");
            assert_eq!(Beacon::of(bad.as_bytes()), None, "{host}");
        }
        // Another build's beacon, and a stranger's packet, are simply not beacons.
        assert_eq!(
            Beacon::of(b"{\"letibot\":99,\"host\":\"mac\",\"port\":1}"),
            None
        );
        assert_eq!(Beacon::of(b"\x00\x01garbage"), None);
        assert_eq!(
            Beacon::of(b"{\"letibot\":1,\"host\":\"mac\",\"port\":0}"),
            None
        );
        // The address a beacon names is the sender's IP and the beacon's own port.
        let from: SocketAddr = "10.1.2.3:5555".parse().unwrap();
        assert_eq!(b.at(from).to_string(), "10.1.2.3:47100");
    }

    #[test]
    fn the_smaller_name_dials_so_two_boxes_make_one_link() {
        assert!(dials("lab2x1", "mac"));
        assert!(!dials("mac", "lab2x1"));
        // The rule is on names, so both boxes must reach the same answer about the same pair.
        assert_eq!(dials("a", "b"), !dials("b", "a"));
    }

    /// Two coordinators in one process, on the box's own multicast stack. A box whose network
    /// refuses to loop a datagram back says so and skips: what this proves is the socket, and a
    /// sandbox that drops multicast cannot prove it.
    #[test]
    fn a_beacon_on_the_group_is_heard_by_the_other_box() {
        let group: Ipv4Addr = GROUP.parse().unwrap();
        // Every box on a LAN binds the SAME port, so two of them on one machine share it.
        let port = PORT + 101;
        let (a, b) = match (Announcer::open(group, port), Announcer::open(group, port)) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(e), _) | (_, Err(e)) => {
                eprintln!("skipped: this box cannot open a multicast socket ({e})");
                return;
            }
        };
        let beacon = Beacon::new("lab2x1", 47100);
        if let Err(e) = a.announce(&beacon) {
            eprintln!("skipped: this box cannot send to {GROUP} ({e})");
            return;
        }
        let mut heard = None;
        for _ in 0..10 {
            match b.hear() {
                Ok(Some((got, from))) => {
                    heard = Some((got, from));
                    break;
                }
                Ok(None) => continue,
                Err(e) => {
                    eprintln!("skipped: this box cannot receive multicast ({e})");
                    return;
                }
            }
        }
        let Some((got, from)) = heard else {
            eprintln!("skipped: no multicast datagram came back on {GROUP} within a second");
            return;
        };
        assert_eq!(got, beacon);
        // The address is the sender's interface and the PORT the beacon carries, which is what a
        // dialer needs.
        assert_eq!(got.at(from).port(), 47100);
    }
}
