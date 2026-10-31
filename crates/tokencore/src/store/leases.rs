//! **A block of a shared host's ports, held by one session** (schema 35).
//!
//! # Why this is a row and not a variable in a tool
//!
//! The operator's own words for what went wrong on the hosts agents share (`docs/remote-hosts.md`
//! § *What an agent hand-rolled without builtins*, item 6): *ephemeral-port collisions, loopback
//! exhaustion, "address already in use" ×18, "too many open files" ×34, a container left 19 h*.
//! Every one of those is a resource that two sessions picked independently, and the fix the
//! Stroppy session arrived at was exactly this — *"a block of ports outside the ephemeral range
//! (checked against `ss -ltn`), all under a host lock"*.
//!
//! A lock in one process is a lock for half the callers. **One `sessions.db` is shared by every
//! daemon on the box** (the measurement in [`super::floor`]'s header: ~11 daemons and dozens of
//! connections on one file), so a lease has to live where all of them look, and the allocation has
//! to happen inside `BEGIN IMMEDIATE` — that transaction *is* the per-host lock, and it is the only
//! one that spans processes.
//!
//! # What a lease is, and what it is not
//!
//! It is **a range of ports on one host, held by one session, with a name prefix** that the
//! session's containers and compose projects are named under. It is not a reservation of anything
//! in the kernel: a kernel cannot be asked to keep a port free. So the record answers the two
//! questions that can be answered —
//!
//! * **nobody else's lease overlaps this one**, which is what the lock gives; and
//! * **nothing was listening there when it was taken** ([`Ask::busy`], a census the caller takes on
//!   the host), which is what `ss -ltn` gives.
//!
//! A program that binds one of those ports without asking is not stopped by this table and cannot
//! be. That is said here rather than discovered.
//!
//! # Why `released_ms` and not a deleted row
//!
//! A released lease is history: *this session held 61000–61015 on lab2x1 for four minutes* is what
//! a person reads when asking why a port was busy. It is also the only record that a restart can
//! use to tell *held by a session that is gone* from *never held*. So the row stays, and
//! [`Lease::held`] is `released_ms IS NULL`.
//!
//! # And why it is not in `transfer::TABLES`
//!
//! The same reason `process` is not: a lease names ports on ONE host. Copied to a session's new
//! home it would be a claim on a machine the copy cannot see — and worse than the process case,
//! because the ports might be free there and the copy would report them taken by itself.

use std::collections::BTreeSet;

use rusqlite::{OptionalExtension, params};

use super::{Result, Store, StoreError, now_ms};

/// **The table both a fresh store and the v35 migration create.**
pub const LEASES_SQL: &str = r#"
-- **A block of a shared host's ports, held by one session** (schema 35). See
-- `crates/tokencore/src/store/leases.rs` for why a lock in a tool would be half a lock, and for
-- the one thing this table cannot promise (a program that binds without asking).
CREATE TABLE IF NOT EXISTS host_lease (
    id            INTEGER PRIMARY KEY,
    -- NULL = the box this daemon runs on; otherwise a name from hosts.toml
    host          TEXT,
    session_id    TEXT NOT NULL,
    -- the name every compose project and container of this lease starts with
    prefix        TEXT NOT NULL,
    port_first    INTEGER NOT NULL,
    port_last     INTEGER NOT NULL,
    -- the ephemeral range the host reported when this block was picked, "lo-hi": a reader can
    -- tell that the block is outside it without asking the host again
    ephemeral     TEXT NOT NULL,
    taken_ms      INTEGER NOT NULL,
    -- NULL while held
    released_ms   INTEGER,
    -- why it went back, when it did
    released_by   TEXT
);
-- The allocation's own query is "every block still held on this host"; the reaper's is "every
-- block still held at all".
CREATE INDEX IF NOT EXISTS host_lease_open_idx ON host_lease (released_ms);
CREATE INDEX IF NOT EXISTS host_lease_host_idx ON host_lease (host, released_ms);
CREATE INDEX IF NOT EXISTS host_lease_session_idx ON host_lease (session_id, released_ms);
"#;

/// **The lowest port this build will ever hand out.** Below it a bind needs a privilege the remote
/// session's user may not have, so a block there would be a lease nothing could use.
pub const FIRST_PORT: u16 = 1024;

/// **One held block**, as the store holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub id: i64,
    /// `None` for this box, a `hosts.toml` name otherwise — the same spelling
    /// [`super::ProcessRecord::host`] uses, so a lease and the processes it serves say one thing.
    pub host: Option<String>,
    pub session: String,
    /// The name every compose project and container of this lease starts with.
    pub prefix: String,
    pub port_first: u16,
    pub port_last: u16,
    /// The ephemeral range the host reported when the block was picked. Recorded rather than
    /// re-derived: the kernel's range is a fact of the moment the ports were chosen, and a reader
    /// asking *is this block outside it* should not need a second answer that may have changed.
    pub ephemeral: (u16, u16),
    pub taken_ms: i64,
    pub released_ms: Option<i64>,
    pub released_by: Option<String>,
}

impl Lease {
    /// **Is anybody holding this?** The whole of `released_ms IS NULL`.
    pub fn held(&self) -> bool {
        self.released_ms.is_none()
    }

    /// How many ports it is.
    pub fn ports(&self) -> u16 {
        self.port_last.saturating_sub(self.port_first) + 1
    }

    /// **The data a program needs to use its block**, as environment pairs — the shape a
    /// [`crate::store`]-less tool and a `SpawnRequest` both take.
    ///
    /// Three names and no more: where the block starts, where it ends, and the prefix its containers
    /// must be named under. A service that reads them is a service a teardown can find again.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            (
                "LETIBOT_LEASE_PORT_FIRST".to_string(),
                self.port_first.to_string(),
            ),
            (
                "LETIBOT_LEASE_PORT_LAST".to_string(),
                self.port_last.to_string(),
            ),
            ("LETIBOT_LEASE_PORTS".to_string(), self.ports().to_string()),
            ("LETIBOT_LEASE_PREFIX".to_string(), self.prefix.clone()),
        ]
    }

    /// **The name a person reads**: `lab2x1/7`, or `local/7`.
    pub fn name(&self) -> String {
        format!("{}/{}", self.host.as_deref().unwrap_or("local"), self.id)
    }

    /// The row's one line, as `hosts` and a tool's answer print it.
    pub fn label(&self) -> String {
        format!(
            "{} · ports {}-{} · prefix `{}` · {}",
            self.name(),
            self.port_first,
            self.port_last,
            self.prefix,
            match self.released_ms {
                Some(_) => format!(
                    "released ({})",
                    self.released_by.as_deref().unwrap_or("nobody said")
                ),
                None => "held".to_string(),
            }
        )
    }
}

/// **What is being asked for** — the ports, the prefix, and the two facts about the host that the
/// caller measured rather than guessed.
#[derive(Debug, Clone)]
pub struct Ask<'a> {
    pub session: &'a str,
    /// `None` for this box, a `hosts.toml` name otherwise.
    pub host: Option<&'a str>,
    /// The name prefix this lease's own compose projects and containers are made under.
    pub prefix: &'a str,
    /// How many consecutive ports. At least one; a block is never split.
    pub ports: u16,
    /// The escape hatch of the host's kernel, read **there**: `(low, high)` as
    /// `/proc/sys/net/ipv4/ip_local_port_range` spells it.
    pub ephemeral: (u16, u16),
    /// **Every port something is listening on there right now** — the `ss -ltn` half.
    pub busy: &'a BTreeSet<u16>,
    pub now_ms: i64,
}

/// **The block to take, or `None` when the host has nothing left outside its ephemeral range.**
///
/// Pure, and that is the point: the rule that two sessions never share a port is a few lines of
/// arithmetic and is tested as arithmetic. [`Store::allocate_lease`] adds the lock.
///
/// The first fit is the first run of `ports` consecutive free ports **above `ephemeral.1`**, which
/// is the whole of "outside the ephemeral range" — a port in the kernel's own range is one the
/// kernel will hand to somebody else's outgoing connection the moment it feels like it. `busy` is a
/// census taken on the host and `held` is what other sessions already have, and a run that touches
/// either is skipped rather than trimmed: a block is contiguous, because a service that takes
/// `ports` of them wants to know `first + n` is its own.
pub fn pick(
    held: &[(u16, u16)],
    busy: &BTreeSet<u16>,
    ephemeral: (u16, u16),
    ports: u16,
) -> Option<(u16, u16)> {
    let want = ports.max(1) as u32;
    let fits = |first: u32, end: u32| -> Option<(u16, u16)> {
        let mut first = first;
        while first + want - 1 <= end {
            let last = (first + want - 1) as u16;
            let port = first as u16;
            let touches_held = held.iter().any(|(a, b)| port <= *b && last >= *a);
            let touches_busy = (port..=last).any(|p| busy.contains(&p));
            if !touches_held && !touches_busy {
                return Some((port, last));
            }
            first += 1;
        }
        None
    };
    // **Above the range first**, where nothing else on the box expects to find a listener.
    // `+ 1` in `u32`, never saturating: a saturating add clamps a host whose range ends at 65535
    // back INTO its own range and hands out the kernel's last ephemeral port — the one thing this
    // function exists to prevent.
    let above = (ephemeral.1 as u32 + 1).max(FIRST_PORT as u32);
    // **Then below it**, from [`LOW_FIRST`] to the port before the range starts. A range that ends
    // at 65535 leaves nothing above it — macOS's default (49152-65535) and some Linux tunings —
    // and a lease engine that only looked above refused every request there. MEASURED 2026-10-27:
    // every lease test failed in the Mac gate with *no free block … above 49152-65535*.
    fits(above, u16::MAX as u32).or_else(|| {
        let start = ephemeral.0 as u32;
        (start > LOW_FIRST as u32)
            .then(|| fits(LOW_FIRST as u32, start - 1))
            .flatten()
    })
}

/// **Where a block below the ephemeral range may start.** Under it sit the registered ports that
/// well-known services listen on (databases, caches, dev servers), and a lease is a promise that
/// nothing else is there.
pub const LOW_FIRST: u16 = 10000;

const COLUMNS: &str = "id, host, session_id, prefix, port_first, port_last, ephemeral, taken_ms, released_ms, released_by";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Lease> {
    let ephemeral: String = r.get(6)?;
    let (lo, hi) = ephemeral.split_once('-').unwrap_or(("0", "0"));
    Ok(Lease {
        id: r.get(0)?,
        host: r.get(1)?,
        session: r.get(2)?,
        prefix: r.get(3)?,
        port_first: r.get::<_, i64>(4)? as u16,
        port_last: r.get::<_, i64>(5)? as u16,
        ephemeral: (
            lo.trim().parse().unwrap_or(0),
            hi.trim().parse().unwrap_or(0),
        ),
        taken_ms: r.get(7)?,
        released_ms: r.get(8)?,
        released_by: r.get(9)?,
    })
}

impl Store {
    /// **Take a block on one host, or refuse by naming what is left.**
    ///
    /// `BEGIN IMMEDIATE` is the lock, and it is the only one that spans processes: two daemons on
    /// this box share one `sessions.db`, so a mutex in a tool would serialise half the callers.
    /// Everything between the `BEGIN` and the `COMMIT` is this host's, which is why the census is
    /// taken *before* it — a probe over ssh inside the write lock would hold every other session's
    /// lease for up to a round trip.
    pub fn allocate_lease(&self, ask: &Ask<'_>) -> Result<Lease> {
        self.write_tx(|tx| {
            let mut held: Vec<(u16, u16)> = Vec::new();
            let mut held_text: Vec<String> = Vec::new();
            {
                let mut st = tx.prepare(
                    "SELECT port_first, port_last FROM host_lease
                  WHERE released_ms IS NULL AND host IS ?1",
                )?;
                let rows = st.query_map(params![ask.host], |r| {
                    Ok((r.get::<_, i64>(0)? as u16, r.get::<_, i64>(1)? as u16))
                })?;
                for r in rows {
                    let (a, b) = r?;
                    held.push((a, b));
                    held_text.push(format!("{a}-{b}"));
                }
            }
            let Some((first, last)) = pick(&held, ask.busy, ask.ephemeral, ask.ports) else {
                return Err(StoreError::Refused(format!(
                    "there is no free block of {} port(s) on {} left outside its ephemeral range \
                 {}-{}, and this build does not hand out ports inside that range — the kernel \
                 gives those away to anybody's outgoing connection. Already held there: {}. \
                 Release a lease (`hosts` shows them) or ask for fewer ports.",
                    ask.ports.max(1),
                    ask.host.unwrap_or("this box"),
                    ask.ephemeral.0,
                    ask.ephemeral.1,
                    if held_text.is_empty() {
                        "nothing".to_string()
                    } else {
                        held_text.join(", ")
                    }
                )));
            };
            tx.execute(
                "INSERT INTO host_lease
               (host, session_id, prefix, port_first, port_last, ephemeral, taken_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    ask.host,
                    ask.session,
                    ask.prefix,
                    first as i64,
                    last as i64,
                    format!("{}-{}", ask.ephemeral.0, ask.ephemeral.1),
                    ask.now_ms,
                ],
            )?;
            let id = tx.last_insert_rowid();
            Ok(Lease {
                id,
                host: ask.host.map(str::to_string),
                session: ask.session.to_string(),
                prefix: ask.prefix.to_string(),
                port_first: first,
                port_last: last,
                ephemeral: ask.ephemeral,
                taken_ms: ask.now_ms,
                released_ms: None,
                released_by: None,
            })
        })
    }

    /// **Every block still held**, across every host and session — the reaper's work list and what
    /// `hosts` prints.
    pub fn held_leases(&self) -> Result<Vec<Lease>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM host_lease WHERE released_ms IS NULL
              ORDER BY host IS NOT NULL, host, id"
        ))?;
        let rows = st.query_map([], row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Every block this session ever held, newest last — what a close releases and a test asserts
    /// against.
    pub fn leases_of_session(&self, session: &str) -> Result<Vec<Lease>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM host_lease WHERE session_id = ?1 ORDER BY id"
        ))?;
        let rows = st.query_map(params![session], row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// One lease by its number. `None` for a lease this store never wrote.
    pub fn lease(&self, id: i64) -> Result<Option<Lease>> {
        Ok(self
            .conn()
            .prepare(&format!("SELECT {COLUMNS} FROM host_lease WHERE id = ?1"))?
            .query_row(params![id], row)
            .optional()?)
    }

    /// **Give one block back.** `false` when it was not held — a release is idempotent because a
    /// session's close, a scenario's end and the reaper can all reach the same lease, and two of
    /// them arriving is a fact about the code rather than a fault.
    pub fn release_lease(&self, id: i64, by: &str, now_ms: i64) -> Result<bool> {
        let n = self.exec(
            "UPDATE host_lease SET released_ms = ?2, released_by = ?3
              WHERE id = ?1 AND released_ms IS NULL",
            params![id, now_ms, by],
        )?;
        Ok(n == 1)
    }

    /// **Give back every block a session still holds** — what its close does, and what the reaper
    /// does for a session that is gone. Returns the ones that were actually held, so the caller can
    /// say what it released and tear down the prefixes of exactly those.
    pub fn release_session_leases(
        &self,
        session: &str,
        by: &str,
        now_ms: i64,
    ) -> Result<Vec<Lease>> {
        self.write_tx(|tx| {
            let mut out = Vec::new();
            {
                let mut st = tx.prepare(&format!(
                    "SELECT {COLUMNS} FROM host_lease
                  WHERE session_id = ?1 AND released_ms IS NULL ORDER BY id"
                ))?;
                let rows = st.query_map(params![session], row)?;
                for r in rows {
                    out.push(r?);
                }
            }
            for lease in &out {
                tx.execute(
                    "UPDATE host_lease SET released_ms = ?2, released_by = ?3 WHERE id = ?1",
                    params![lease.id, now_ms, by],
                )?;
            }
            Ok(out)
        })
    }

    /// The clock this module stamps with — one spelling, so a release and a take dated by two
    /// callers cannot disagree.
    pub fn lease_now_ms() -> i64 {
        now_ms()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn busy(ports: &[u16]) -> BTreeSet<u16> {
        ports.iter().copied().collect()
    }

    /// **Two sessions asking one host get disjoint blocks** — the rule the whole table exists for,
    /// and the one a lock in a tool could only half keep.
    #[test]
    fn two_sessions_on_one_host_get_disjoint_blocks() {
        let store = Store::open_in_memory().expect("a store");
        let none = busy(&[]);
        let a = store
            .allocate_lease(&Ask {
                session: "s-a",
                host: Some("lab2x1"),
                prefix: "letibot-a",
                ports: 4,
                ephemeral: (32768, 60999),
                busy: &none,
                now_ms: 1,
            })
            .expect("the first block");
        let b = store
            .allocate_lease(&Ask {
                session: "s-b",
                host: Some("lab2x1"),
                prefix: "letibot-b",
                ports: 4,
                ephemeral: (32768, 60999),
                busy: &none,
                now_ms: 2,
            })
            .expect("the second block");
        assert_eq!((a.port_first, a.port_last), (61000, 61003));
        assert_eq!((b.port_first, b.port_last), (61004, 61007));
        // Both above the host's own range, which is the point of the census.
        assert!(a.port_first > 60999 && b.port_first > 60999);
        // And a release hands the first one back for the next caller.
        assert!(store.release_lease(a.id, "a test", 3).expect("release"));
        let c = store
            .allocate_lease(&Ask {
                session: "s-c",
                host: Some("lab2x1"),
                prefix: "letibot-c",
                ports: 4,
                ephemeral: (32768, 60999),
                busy: &none,
                now_ms: 4,
            })
            .expect("the third block");
        assert_eq!((c.port_first, c.port_last), (61000, 61003));
    }

    /// **A lease is per HOST**: the same ports on another machine are a different question, and
    /// this box (`host IS NULL`) is a third.
    #[test]
    fn a_block_is_per_host() {
        let store = Store::open_in_memory().expect("a store");
        let none = busy(&[]);
        let mut firsts = Vec::new();
        for host in [Some("lab2x1"), Some("lubuntu1"), None] {
            let l = store
                .allocate_lease(&Ask {
                    session: "s-a",
                    host,
                    prefix: "letibot-a",
                    ports: 2,
                    ephemeral: (32768, 60999),
                    busy: &none,
                    now_ms: 1,
                })
                .expect("a block");
            firsts.push(l.port_first);
        }
        assert_eq!(firsts, vec![61000, 61000, 61000]);
    }

    /// **A port the census found in use is skipped**, and a release is idempotent because three
    /// callers can reach one lease.
    #[test]
    fn a_busy_port_is_skipped_and_a_release_is_idempotent() {
        let store = Store::open_in_memory().expect("a store");
        let taken = busy(&[61000, 61001]);
        let l = store
            .allocate_lease(&Ask {
                session: "s-a",
                host: None,
                prefix: "letibot-a",
                ports: 2,
                ephemeral: (32768, 60999),
                busy: &taken,
                now_ms: 1,
            })
            .expect("a block");
        assert_eq!((l.port_first, l.port_last), (61002, 61003));
        assert!(store.release_lease(l.id, "one", 2).expect("release"));
        assert!(!store.release_lease(l.id, "two", 3).expect("release"));
        assert!(store.held_leases().expect("held").is_empty());
        // The row stays: it is the history of what held what, and only `released_ms` changed.
        let after = store.lease(l.id).expect("a query").expect("a row");
        assert_eq!(after.released_by.as_deref(), Some("one"));
        assert_eq!(after.released_ms, Some(2));
    }

    /// **A restart keeps the leases**, because the row is the lease: a second connection to the
    /// same file finds the block held and allocates past it.
    #[test]
    fn a_second_connection_sees_the_block_as_held() {
        let dir = std::env::temp_dir().join(format!("letibot-lease-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("sessions.db");
        let none = busy(&[]);
        {
            let first = Store::open(&path).expect("a store");
            first
                .allocate_lease(&Ask {
                    session: "s-a",
                    host: Some("lab2x1"),
                    prefix: "letibot-a",
                    ports: 3,
                    ephemeral: (32768, 60999),
                    busy: &none,
                    now_ms: 1,
                })
                .expect("a block");
        }
        // The daemon that comes back later: a new connection, and the block is still taken.
        let second = Store::open(&path).expect("the store again");
        let held = second.held_leases().expect("held");
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].session, "s-a");
        let b = second
            .allocate_lease(&Ask {
                session: "s-b",
                host: Some("lab2x1"),
                prefix: "letibot-b",
                ports: 3,
                ephemeral: (32768, 60999),
                busy: &none,
                now_ms: 2,
            })
            .expect("a second block");
        assert_eq!((b.port_first, b.port_last), (61003, 61005));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A host with nothing left is a REFUSAL naming the ranges already held, not an empty block.
    #[test]
    fn an_exhausted_host_is_refused_in_words() {
        let store = Store::open_in_memory().expect("a store");
        let none = busy(&[]);
        // 65534 would fit above, 65535 is the last port there is, and the range starts at the
        // lowest port a block below it may use — so three fit nowhere.
        let e = store
            .allocate_lease(&Ask {
                session: "s-a",
                host: Some("lab2x1"),
                prefix: "letibot-a",
                ports: 3,
                ephemeral: (LOW_FIRST, 65533),
                busy: &none,
                now_ms: 1,
            })
            .expect_err("a refusal");
        let said = e.to_string();
        assert!(said.contains("no free block of 3"), "{said}");
        assert!(said.contains("lab2x1"), "{said}");
        assert!(said.contains("10000-65533"), "{said}");
    }

    /// **The block, as the environment a session's containers read.**
    #[test]
    fn a_lease_says_its_block_and_its_prefix() {
        let lease = Lease {
            id: 7,
            host: Some("lab2x1".to_string()),
            session: "s-a".to_string(),
            prefix: "letibot-a-1".to_string(),
            port_first: 61000,
            port_last: 61003,
            ephemeral: (32768, 60999),
            taken_ms: 1,
            released_ms: None,
            released_by: None,
        };
        assert_eq!(lease.ports(), 4);
        assert_eq!(lease.name(), "lab2x1/7");
        let env: std::collections::BTreeMap<String, String> = lease.env().into_iter().collect();
        assert_eq!(env["LETIBOT_LEASE_PORT_FIRST"], "61000");
        assert_eq!(env["LETIBOT_LEASE_PORT_LAST"], "61003");
        assert_eq!(env["LETIBOT_LEASE_PREFIX"], "letibot-a-1");
        assert!(lease.held());
    }

    /// **The rule, as arithmetic**: the first run of `ports` consecutive free ports above the
    /// host's ephemeral range.
    #[test]
    fn a_block_starts_above_the_ephemeral_range() {
        assert_eq!(
            pick(&[], &busy(&[]), (32768, 60999), 4),
            Some((61000, 61003))
        );
    }

    /// **A held block is skipped, not trimmed** — a block is contiguous because the service that
    /// takes four ports wants `first + 3` to be its own.
    #[test]
    fn a_held_block_is_stepped_over_whole() {
        assert_eq!(
            pick(&[(61000, 61003)], &busy(&[]), (32768, 60999), 4),
            Some((61004, 61007))
        );
        // And a hold in the MIDDLE of the would-be block moves the whole block past it.
        assert_eq!(
            pick(&[(61002, 61005)], &busy(&[]), (32768, 60999), 4),
            Some((61006, 61009))
        );
    }

    /// A port something else is listening on is skipped for the same reason: it is not ours.
    #[test]
    fn a_port_someone_is_listening_on_is_skipped() {
        assert_eq!(
            pick(&[], &busy(&[61000]), (32768, 60999), 2),
            Some((61001, 61002))
        );
        // A busy port early in the window moves the block, and the held block after it moves it
        // again: 61000-61002 holds the busy 61001, 61002-61004 runs into 61004, and so on.
        assert_eq!(
            pick(&[(61004, 61009)], &busy(&[61001]), (32768, 60999), 3),
            Some((61010, 61012))
        );
    }

    /// **Nothing inside the ephemeral range is ever handed out**, whatever the run would fit.
    #[test]
    fn nothing_inside_the_range_is_offered() {
        // A range that ends at the top leaves only below it — never inside it.
        assert_eq!(
            pick(&[], &busy(&[]), (32768, 65535), 1),
            Some((LOW_FIRST, LOW_FIRST))
        );
        // A range that covers everything from LOW_FIRST up leaves nothing at all.
        assert_eq!(pick(&[], &busy(&[]), (LOW_FIRST, 65535), 1), None);
        // And one that only just fits at the top does.
        assert_eq!(
            pick(&[], &busy(&[]), (32768, 65533), 2),
            Some((65534, 65535))
        );
    }

    /// Exhaustion is `None`, which the store turns into a refusal naming what is left.
    #[test]
    fn an_exhausted_host_has_no_block() {
        let all: Vec<(u16, u16)> = vec![(61000, 65535), (LOW_FIRST, 32767)];
        assert_eq!(pick(&all, &busy(&[]), (32768, 60999), 1), None);
    }

    /// **Below [`FIRST_PORT`] nothing is ever offered** — even when the ephemeral range says the
    /// whole space is free, a block a session's user may not bind is a lease nothing can use.
    #[test]
    fn the_floor_is_an_unprivileged_port() {
        assert_eq!(
            pick(&[], &busy(&[]), (0, 0), 1),
            Some((FIRST_PORT, FIRST_PORT))
        );
    }
}
