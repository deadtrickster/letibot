//! What a beacon loop does when its own socket stops working: say it once, open a new socket, and
//! tell the operator the box is cut off.
//!
//! A coordinator started before the network is up - launchd's, on a Mac whose Local Network privacy
//! was never granted - keeps a datagram socket the kernel will never route. Every send answers `No
//! route to host` and the socket cannot recover: measured, 4383 lines in two and a half hours with
//! no link at all, and the same binary started by hand linked to six boxes in a second.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use crate::peer::announce::{self, Announcer, Beacon};

/// The socket the beacon loop sends and hears on. A trait because the failure this exists for - a
/// send no network will carry - cannot be arranged on a real one, and the loop's answer to it must
/// be testable where the box has no LAN at all.
pub trait BeaconSocket: Send {
    fn announce(&self, beacon: &Beacon) -> io::Result<usize>;
    fn hear(&self) -> io::Result<Option<(Beacon, SocketAddr)>>;
    /// Drop this socket and bind another. A socket the kernel will not route never recovers, so a
    /// socket made after the network came back is the only cure there is.
    fn reopen(&mut self) -> io::Result<()>;
}

/// Makes one socket: what [`Sockets`] holds, named so the field's type is readable at all.
type Open = dyn Fn(Ipv4Addr, u16) -> io::Result<Box<dyn BeaconSocket>> + Send + Sync;

/// Makes the socket the loop sends and hears on: [`Announcer::open`] on every box, and a test's own.
#[derive(Clone)]
pub struct Sockets(Arc<Open>);

impl Sockets {
    /// A real multicast socket: the port every box on the LAN shares, joined to the group.
    pub fn real() -> Sockets {
        Sockets::new(|group, port| Ok(Box::new(Announcer::open(group, port)?)))
    }

    pub fn new(
        open: impl Fn(Ipv4Addr, u16) -> io::Result<Box<dyn BeaconSocket>> + Send + Sync + 'static,
    ) -> Sockets {
        Sockets(Arc::new(open))
    }

    pub fn open(&self, group: Ipv4Addr, port: u16) -> io::Result<Box<dyn BeaconSocket>> {
        (self.0)(group, port)
    }
}

impl std::fmt::Debug for Sockets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sockets")
    }
}

impl Default for Sockets {
    fn default() -> Sockets {
        Sockets::real()
    }
}

/// How the beacon loop paces itself and when it stops believing its own socket. A value rather than
/// constants so a test watches an outage for a tenth of a second instead of two minutes - `Tick` is
/// a value for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cadence {
    pub every_ms: u64,
    /// Consecutive failures before the socket is thrown away and made again.
    pub reopen_after: u64,
    /// The wait before the next reopen: doubled each time, capped at `backoff_max_ms`.
    pub backoff_ms: u64,
    pub backoff_max_ms: u64,
    /// How long a box that cannot send, and has no link, waits before the operator is told.
    pub cut_after_ms: u64,
    /// The most often a continuing failure is written to the log: the first one is said at once.
    pub summary_ms: u64,
}

impl Default for Cadence {
    fn default() -> Cadence {
        Cadence {
            every_ms: announce::EVERY_MS,
            // Six sends is ten seconds in: long enough that a dropped datagram is not a broken
            // socket, short enough that a box cut off at boot is found in the same minute.
            reopen_after: 6,
            backoff_ms: 30_000,
            backoff_max_ms: 300_000,
            cut_after_ms: 120_000,
            summary_ms: 300_000,
        }
    }
}

/// What one round of the beacon loop asks of the loop that owns the socket.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Asked {
    /// Lines for the log, already rate-limited: never one per tick.
    pub log: Vec<String>,
    /// Throw the socket away and open a new one.
    pub reopen: bool,
    /// What the room tells the operator: this box cannot reach the LAN and has no peer.
    pub cut: Option<String>,
    /// The box can announce itself again, or a link came up, after a `cut` was said.
    pub back: bool,
}

/// One outage's bookkeeping, and the quiet that keeps it to a few lines instead of thousands.
#[derive(Debug)]
pub struct Watch {
    cadence: Cadence,
    /// Consecutive failures: a send that works resets everything below.
    failed: u64,
    /// Has a line been said in this outage, and when: the window a summary is measured by.
    said: bool,
    said_at: u64,
    since: u64,
    last: String,
    /// When the socket may be opened again, and how many reopens this outage has asked for.
    reopen_at: u64,
    reopens: u64,
    backoff: u64,
    /// Has the room been told? One line per outage, whatever the socket does next.
    told: bool,
}

impl Watch {
    pub fn new(cadence: Cadence) -> Watch {
        let backoff = cadence.backoff_ms;
        Watch {
            cadence,
            failed: 0,
            said: false,
            said_at: 0,
            since: 0,
            last: String::new(),
            reopen_at: 0,
            reopens: 0,
            backoff,
            told: false,
        }
    }

    /// A send that worked. After an outage this is the one "recovered" line and the reset.
    pub fn sent(&mut self, now: u64, group: &str) -> Asked {
        let mut asked = Asked::default();
        if self.failed > 0 {
            asked.log.push(format!(
                "the beacon to {group} recovered after {} failed send(s) over {} (last: {})",
                self.failed,
                crate::item::duration(now.saturating_sub(self.since)),
                self.last
            ));
            asked.back = self.told;
        }
        *self = Watch::new(self.cadence);
        asked
    }

    /// A send that did not work, or a socket that could not be read from. `linked` is how many
    /// boxes this one is linked to now: a box with a link is not the box an operator must be told
    /// about, whatever its beacons are doing.
    pub fn failed(&mut self, now: u64, group: &str, why: &str, linked: usize) -> Asked {
        let mut asked = Asked::default();
        if self.failed == 0 {
            *self = Watch::new(self.cadence);
            self.since = now;
        }
        self.failed += 1;
        self.last = why.to_string();
        // The first failure is said at once - the operator has to read the error, not a count that
        // started ten minutes ago - and after that one summary per window.
        if !self.said || now.saturating_sub(self.said_at) >= self.cadence.summary_ms {
            asked.log.push(self.summary(now, group));
            self.said = true;
            self.said_at = now;
        }
        if self.reopen_due(now) {
            asked.reopen = true;
        }
        if !self.told && linked == 0 && now.saturating_sub(self.since) >= self.cadence.cut_after_ms
        {
            self.told = true;
            let for_ms = now.saturating_sub(self.since);
            asked.cut = Some(cut_line(group, self.failed, for_ms, &self.last));
        }
        asked
    }

    /// One line per window at most, and the two failures it names are the first and the last.
    fn summary(&self, now: u64, group: &str) -> String {
        if self.failed == 1 {
            return format!(
                "the beacon to {group} failed ({}) - a socket opened before the network was up \
                 cannot send; it is opened again after {} in a row",
                self.last, self.cadence.reopen_after
            );
        }
        format!(
            "the beacon to {group} has failed {} times over {} (last: {})",
            self.failed,
            crate::item::duration(now.saturating_sub(self.since)),
            self.last
        )
    }

    /// Is it time to try a socket that is not this one? The first try is at `reopen_after`, and
    /// every one after it waits its backoff - so a box whose LAN never comes back is not opening a
    /// socket every two seconds for the rest of the day.
    fn reopen_due(&mut self, now: u64) -> bool {
        if self.failed < self.cadence.reopen_after {
            return false;
        }
        if self.reopens > 0 && now < self.reopen_at {
            return false;
        }
        self.reopens += 1;
        self.reopen_at = now.saturating_add(self.backoff);
        self.backoff = self
            .backoff
            .saturating_mul(2)
            .min(self.cadence.backoff_max_ms);
        true
    }
}

/// The room's line when a box has been cut off past the bound: what failed, and what to do about it
/// on the two boxes this runs on - the fix is a person's, and it is on their screen.
fn cut_line(group: &str, failed: u64, for_ms: u64, why: &str) -> String {
    format!(
        "this box has not been able to announce itself on the LAN for {}: {} beacon send(s) to \
         {group} failed, the last ({why}), and it has no link to another box. On macOS, allow this \
         binary Local Network access (System Settings > Privacy & Security > Local Network) or \
         restart the coordinator; elsewhere, check the route for 224.0.0.0/4.",
        crate::item::duration(for_ms),
        failed
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHY: &str = "No route to host (os error 65)";
    const GROUP: &str = "239.255.41.7";

    /// Two and a half hours of a two-second tick: what a Mac started by launchd logged as 4383
    /// lines, one per tick, with no link in all that time.
    fn an_outage(mut watch: Watch, ticks: u64, linked: usize) -> (Vec<String>, Vec<u64>) {
        let mut lines = Vec::new();
        let mut reopens = Vec::new();
        for i in 0..ticks {
            let asked = watch.failed(i * 2_000, GROUP, WHY, linked);
            lines.extend(asked.log);
            if asked.reopen {
                reopens.push(i * 2_000);
            }
        }
        (lines, reopens)
    }

    #[test]
    fn a_failing_beacon_is_said_at_once_and_then_once_a_window() {
        let (lines, _) = an_outage(Watch::new(Cadence::default()), 4_500, 0);
        // 9 000 seconds over a five-minute window is thirty lines, not four thousand five hundred.
        assert_eq!(lines.len(), 30, "{lines:#?}");
        assert!(lines[0].contains(WHY), "{}", lines[0]);
        assert!(lines[0].contains("opened again"), "{}", lines[0]);
        // The last one names the count and the error, which is what a summary is for.
        let last = lines.last().expect("a summary");
        assert!(last.contains("has failed"), "{last}");
        assert!(last.contains(WHY), "{last}");
    }

    #[test]
    fn the_socket_is_opened_again_after_six_failures_and_then_on_a_backoff() {
        let (_, reopens) = an_outage(Watch::new(Cadence::default()), 1_000, 0);
        // The sixth failure of a two-second tick is ten seconds in, and a dropped datagram is not
        // enough to throw a socket away.
        assert_eq!(reopens[0], 10_000);
        let waits: Vec<u64> = reopens.windows(2).map(|w| w[1] - w[0]).collect();
        assert_eq!(&waits[..5], &[30_000, 60_000, 120_000, 240_000, 300_000]);
        // And a socket is not opened every two seconds for the rest of the day.
        assert!(waits.iter().all(|w| *w <= 300_000), "{waits:?}");
    }

    #[test]
    fn a_box_with_no_link_is_told_once_after_the_bound_and_a_link_is_never_told() {
        let mut watch = Watch::new(Cadence::default());
        let mut cuts = Vec::new();
        for i in 0..600u64 {
            if let Some(line) = watch.failed(i * 2_000, GROUP, WHY, 0).cut {
                cuts.push((i * 2_000, line));
            }
        }
        // One line, and not before the bound: twenty minutes of failures is one notice.
        assert_eq!(cuts.len(), 1, "{cuts:#?}");
        assert_eq!(cuts[0].0, 120_000);
        let line = &cuts[0].1;
        assert!(line.contains(WHY), "{line}");
        assert!(line.contains("224.0.0.0/4"), "{line}");
        assert!(line.contains("Local Network"), "{line}");
        // A box that has a link is not the box an operator must be told about.
        let mut linked = Watch::new(Cadence::default());
        let told = (0..600u64).any(|i| linked.failed(i * 2_000, GROUP, WHY, 1).cut.is_some());
        assert!(!told);
    }

    #[test]
    fn a_send_that_works_says_it_once_and_a_new_outage_is_told_again() {
        let mut watch = Watch::new(Cadence::default());
        for i in 0..600u64 {
            watch.failed(i * 2_000, GROUP, WHY, 0);
        }
        let back = watch.sent(1_200_000, GROUP);
        assert!(back.back, "the room was told and is owed the other half");
        assert_eq!(back.log.len(), 1);
        assert!(back.log[0].contains("recovered"), "{}", back.log[0]);
        // The reset is a reset: a send that works says nothing, and asks for nothing.
        assert_eq!(watch.sent(1_202_000, GROUP), Asked::default());
        // A second outage is a second outage, and it is said again.
        let mut cut = None;
        for i in 0..600u64 {
            cut = cut.or(watch.failed(1_400_000 + i * 2_000, GROUP, WHY, 0).cut);
        }
        assert!(cut.is_some());
    }
}
