//! The **shared** board — the fleet's queue — as a seam, plus the one that
//! refuses.
//!
//! # A mount, not a second list
//!
//! The model has one set of todo verbs. What is *underneath* them is a property of
//! the session, exactly as a mounted filesystem is a property of a directory: the
//! caller does not pass a flag on every read to say which one it meant. The
//! precedent is the operator's own `flowy fuse --mount <dir>` — *"mount this
//! principal's memory as files, so an agent writes memory where it already writes
//! files."*
//!
//! | | not mounted | mounted |
//! |---|---|---|
//! | what backs a row | this session's [`super::ledger::IntentLedger`] | the fabric node |
//! | who can see it | this session | every seat in the project |
//! | survives the fabric being down | yes | no |
//! | survives the process exiting | no | yes |
//!
//! **Not mounted is the default and it is a configuration, not a fault.** The
//! local path has no flowy dependency of any kind: no node, no token, no
//! `FLOWY_ADDR`, no network code. Someone who has never heard of the fleet gets a
//! working todo tool. That is why this crate has no HTTP client and no `libc` —
//! the implementation that reaches the node lives on the other side of this trait,
//! in the daemon.
//!
//! # What the mount does not hide
//!
//! **Which backing a call landed on is in every result**, because a mount that
//! silently changes semantics is the silent-degradation failure with better
//! manners: `filed to the board as row 1234` against `noted in this session only —
//! no board is mounted`. The caller never chooses; the caller is always told.
//!
//! And **some verbs only mean something mounted.** `claim`, handing a row back and
//! recording who owes the next move are facts about a *shared* board; unmounted
//! they are [`letibot_transcript::ToolOutcome::NotRun`] naming what is missing
//! (brief §2.3), never a local no-op wearing a success.
//!
//! # The property the local path has and this one cannot
//!
//! A seat that cannot reach the node has lost its shared board. It has **not**
//! lost its own plan for the turn. So [`QueueError::Unreachable`] is `NotRun` and
//! says in the same sentence that nothing was filed — it never silently degrades
//! into a local note. A tool that quietly writes locally while the operator
//! believes a row was filed is the stalled-offline failure from
//! `docs/closed-loop.md` §5: a process that has lost its encoder and keeps
//! commanding.
//!
//! (`flowy fuse` has a `--no-drain` mount that queues writes and a `--reconcile`
//! that applies them later. That is a *declared* operator choice, not a silent
//! fallback, and this seam can carry such a backing later without the tool surface
//! changing. It is not built here.)
//!
//! # Ownership is a query, not a judgement
//!
//! `flowy todo claim --id ID --expect WHO` is a compare-and-swap: the write is
//! refused, naming whoever got there first, if the row moved between the read and
//! the claim. The operator's rule for it is that *"the room lags the tree, and
//! that is how seven collisions happened in one night."*
//!
//! So **[`Queue::claim`] does not take `expect`.** A model supplying it would be
//! stating an opinion about who holds a row, and forming that opinion is the bug —
//! `docs/closed-loop.md` §7: *"Is this row mine → ask the door; `--expect` is a
//! compare-and-swap and the model forming an opinion about it is the bug."* The
//! implementation reads the holder and swaps against what it read; the model
//! cannot phrase the wrong claim because the argument does not exist.
//!
//! # What is not here
//!
//! **No CLI runner and no HTTP client.** This crate has no exec path
//! (`HostBackend::run` refuses) and no network, by construction and for the
//! reasons in `crates/tools/src/lib.rs`. The implementation that shells out to
//! `flowy` or speaks to the node belongs to the daemon, on the other side of this
//! trait — exactly where [`super::super::retrieval::Retrieval`] puts the oracle
//! client. [`NoMount`] is what ships until then, and it returns `NotRun`.

use letibot_transcript::ToolOutcome;

/// One row on the shared board, as much of it as this seam needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub title: String,
    /// Who is carrying it. `None` is the unowned pile — a different fact from
    /// "somebody, but I do not know who", which is why it is an `Option` and not
    /// an empty string.
    pub holder: Option<String>,
    /// `todo`, `active`, `done` — the node's word, passed through.
    pub status: String,
    pub category: Option<String>,
    /// Who owes the next move, from `flowy todo waiting-on`. Note that this does
    /// **not** change who carries the row, which is the whole point of the verb.
    pub waiting_on: Option<String>,
}

impl Row {
    pub fn line(&self) -> String {
        let mut s = format!("row {} — {} [{}]", self.id, self.title, self.status);
        match &self.holder {
            Some(h) => s.push_str(&format!(", held by {h}")),
            None => s.push_str(", unowned"),
        }
        if let Some(w) = &self.waiting_on {
            s.push_str(&format!(", waiting on {w}"));
        }
        s
    }
}

/// What to file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRow {
    pub title: String,
    pub body: String,
    /// `bug`, `feature`, `chore`, `question` — the closed set the node counts.
    pub category: Option<String>,
    /// File it into a room, so the row arrives where the work was agreed.
    pub room: Option<String>,
}

/// The four ways this can fail, and none of them is "we wrote it somewhere else".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueError {
    /// No board is mounted. **A configuration, not a fault** — but also not a
    /// silent local write: the verb that needed a board did not happen.
    NotMounted(String),
    /// The board is wired and could not be reached. **Nothing was filed**, and the
    /// session's own plan is unaffected.
    Unreachable(String),
    /// The compare-and-swap lost: the row moved between the read and the write.
    /// This is the mechanism working, and the name of whoever got there first is
    /// the fix.
    HeldBy {
        id: String,
        who: String,
    },
    NoSuchRow {
        id: String,
        known: Vec<String>,
    },
    /// The node refused, in its own words.
    Refused(String),
}

impl QueueError {
    /// The outcome class this failure carries. The two that mean *nothing
    /// decided and nothing written* are `NotRun`; a lost CAS is a real event on a
    /// real board, so it is `Failed`.
    pub fn outcome(&self) -> ToolOutcome {
        match self {
            QueueError::NotMounted(why) | QueueError::Unreachable(why) => {
                ToolOutcome::NotRun { why: why.clone() }
            }
            other => ToolOutcome::Failed {
                reason: other.to_string(),
            },
        }
    }
}

impl std::fmt::Display for QueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueueError::NotMounted(w) | QueueError::Unreachable(w) => write!(f, "{w}"),
            QueueError::HeldBy { id, who } => write!(
                f,
                "row {id} is carried by {who}, so this claim was refused and the row did \
                 not move. That is the compare-and-swap working: somebody got there \
                 first. Ask {who} in the room before taking it — do not re-claim, and do \
                 not start the work anyway."
            ),
            QueueError::NoSuchRow { id, known } => {
                if known.is_empty() {
                    write!(f, "the board has no row {id}")
                } else {
                    write!(f, "the board has no row {id}; it has {}", known.join(", "))
                }
            }
            QueueError::Refused(w) => write!(f, "the board refused: {w}"),
        }
    }
}

/// The shared board.
///
/// Every method that changes a row is a **write to state other seats act on**, so
/// the `board` tool declares [`crate::schema::Access::Network`] and goes through
/// the gate like any other non-read call. Two independent mechanisms, as
/// everywhere else in this crate: an adjudicator that admitted it, and a queue
/// that is actually attached.
pub trait Queue: Send + Sync {
    /// The handle the **node** knows this seat by.
    ///
    /// Read from the seam and never from an argument: a tool that let the model
    /// name the speaker is a tool that can speak under another seat's name.
    fn identity(&self) -> String;

    /// Every row this seat can see.
    ///
    /// **`flowy todo` (the CLI) has no list verb** — it files, notes, claims,
    /// records who owes the next move, and closes. A real implementation therefore
    /// reads the node's API or its MCP surface rather than shelling out for this
    /// one. Named here because the tool needs it and because the gap is worth
    /// knowing before somebody writes a `flowy todo list` that does not exist.
    fn list(&self) -> Result<Vec<Row>, QueueError>;

    fn get(&self, id: &str) -> Result<Row, QueueError>;

    fn file(&self, row: &NewRow) -> Result<Row, QueueError>;

    fn note(&self, id: &str, text: &str) -> Result<Row, QueueError>;

    /// Take a row. **No `expect` argument, by construction** — see the module
    /// docs. The implementation reads the current holder and swaps against what it
    /// read; a row that moved in between comes back as [`QueueError::HeldBy`].
    fn claim(&self, id: &str) -> Result<Row, QueueError>;

    /// Put a row back on the unowned pile — `flowy todo claim --as ""`. How work
    /// is handed **back** rather than handed to somebody.
    fn hand_back(&self, id: &str) -> Result<Row, QueueError>;

    /// Record who owes the next move. Does not change who carries the row.
    fn waiting_on(&self, id: &str, of: &str, what: &str) -> Result<Row, QueueError>;

    /// Close a row. `measured` is `flowy todo done --note "what was measured"`,
    /// and this seam makes it required for the same reason completion is checked
    /// locally: a close with nothing measured behind it is a claim, not a result.
    fn done(&self, id: &str, measured: &str) -> Result<Row, QueueError>;

    /// For `EXPLAIN` and the daemon's startup disclosure.
    fn describe(&self) -> String;
}

/// **The default, and it is not an error state.**
///
/// No board is mounted. The verbs that only mean something on a *shared* board —
/// `claim`, handing one back, recording who owes the next move — return `NotRun`
/// naming what is missing. The verbs that work either way never reach here at all:
/// [`super::board::Todo`] serves them from the session's own ledger and says so in
/// the result.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoMount;

impl NoMount {
    fn refuse<T>(&self, verb: &str) -> Result<T, QueueError> {
        Err(QueueError::NotMounted(format!(
            "`{verb}` is a fact about a SHARED board, and no board is mounted on this \
             session — so it did not run, and nothing was filed, claimed or closed \
             anywhere. No other seat saw anything from this call. That is a \
             configuration and not a fault: this session's own list needs no board, and \
             `add`, `note`, `start`, `complete`, `block` and `list` all work without \
             one."
        )))
    }
}

impl Queue for NoMount {
    fn identity(&self) -> String {
        // Not a guess at a handle. A session with no board has no board identity,
        // and inventing one is how a message goes out under the wrong name.
        String::new()
    }

    fn list(&self) -> Result<Vec<Row>, QueueError> {
        self.refuse("list")
    }

    fn get(&self, _id: &str) -> Result<Row, QueueError> {
        self.refuse("get")
    }

    fn file(&self, _row: &NewRow) -> Result<Row, QueueError> {
        self.refuse("file")
    }

    fn note(&self, _id: &str, _text: &str) -> Result<Row, QueueError> {
        self.refuse("note")
    }

    fn claim(&self, _id: &str) -> Result<Row, QueueError> {
        self.refuse("claim")
    }

    fn hand_back(&self, _id: &str) -> Result<Row, QueueError> {
        self.refuse("hand_back")
    }

    fn waiting_on(&self, _id: &str, _of: &str, _what: &str) -> Result<Row, QueueError> {
        self.refuse("waiting_on")
    }

    fn done(&self, _id: &str, _measured: &str) -> Result<Row, QueueError> {
        self.refuse("done")
    }

    fn describe(&self) -> String {
        "not mounted — this session's list is its own, and no other seat can see it".into()
    }
}

#[cfg(any(test, feature = "testing"))]
pub use fake::FakeQueue;

/// An in-memory board, for tests.
///
/// **It exists so that no test in this repository ever files, claims or closes a
/// row on the real fleet queue.** Other seats act on that board; a test suite that
/// wrote to it would be a test suite that pages people.
#[cfg(any(test, feature = "testing"))]
mod fake {
    use super::*;
    use std::sync::Mutex;

    pub struct FakeQueue {
        pub me: String,
        rows: Mutex<Vec<Row>>,
        next: Mutex<u32>,
        /// Set to make every call answer [`QueueError::Unreachable`], which is the
        /// stalled-fabric case.
        pub offline: bool,
    }

    impl FakeQueue {
        pub fn new(me: &str) -> Self {
            FakeQueue {
                me: me.to_string(),
                rows: Mutex::new(Vec::new()),
                next: Mutex::new(0),
                offline: false,
            }
        }

        pub fn offline(me: &str) -> Self {
            FakeQueue {
                offline: true,
                ..FakeQueue::new(me)
            }
        }

        /// Put a row on the board that somebody else already holds — the case the
        /// compare-and-swap exists for.
        pub fn seed(&self, id: &str, title: &str, holder: Option<&str>) {
            self.rows.lock().unwrap().push(Row {
                id: id.to_string(),
                title: title.to_string(),
                holder: holder.map(|h| h.to_string()),
                status: "todo".into(),
                category: None,
                waiting_on: None,
            });
        }

        /// Simulate another seat taking the row between our read and our write.
        pub fn set_holder(&self, id: &str, holder: Option<&str>) {
            if let Some(r) = self.rows.lock().unwrap().iter_mut().find(|r| r.id == id) {
                r.holder = holder.map(|h| h.to_string());
            }
        }

        fn check(&self) -> Result<(), QueueError> {
            if self.offline {
                return Err(QueueError::Unreachable(
                    "the fabric node did not answer, so NOTHING was written to the shared \
                     board. This is not a denial and it is not a local note: the row was \
                     not filed. The session's own plan (`todo`) is unaffected."
                        .into(),
                ));
            }
            Ok(())
        }

        fn with<T>(
            &self,
            id: &str,
            f: impl FnOnce(&mut Row) -> Result<T, QueueError>,
        ) -> Result<T, QueueError> {
            self.check()?;
            let mut rows = self.rows.lock().unwrap();
            let known: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
            match rows.iter_mut().find(|r| r.id == id) {
                Some(r) => f(r),
                None => Err(QueueError::NoSuchRow {
                    id: id.to_string(),
                    known,
                }),
            }
        }
    }

    impl Queue for FakeQueue {
        fn identity(&self) -> String {
            self.me.clone()
        }

        fn list(&self) -> Result<Vec<Row>, QueueError> {
            self.check()?;
            let rows = self.rows.lock().unwrap().clone();
            Ok(rows)
        }

        fn get(&self, id: &str) -> Result<Row, QueueError> {
            self.with(id, |r| Ok(r.clone()))
        }

        fn file(&self, row: &NewRow) -> Result<Row, QueueError> {
            self.check()?;
            let mut n = self.next.lock().unwrap();
            *n += 1;
            let r = Row {
                id: format!("r{n}"),
                title: row.title.clone(),
                holder: None,
                status: "todo".into(),
                category: row.category.clone(),
                waiting_on: None,
            };
            self.rows.lock().unwrap().push(r.clone());
            Ok(r)
        }

        fn note(&self, id: &str, _text: &str) -> Result<Row, QueueError> {
            self.with(id, |r| Ok(r.clone()))
        }

        fn claim(&self, id: &str) -> Result<Row, QueueError> {
            // The read half of the compare-and-swap. A real implementation reads
            // the node and passes what it read as `--expect`; the fake models the
            // same window, and `set_holder` is how a test slides another seat into
            // it.
            let observed = self.get(id)?.holder;
            let me = self.me.clone();
            self.with(id, move |r| {
                if r.holder != observed {
                    return Err(QueueError::HeldBy {
                        id: r.id.clone(),
                        who: r.holder.clone().unwrap_or_else(|| "nobody".into()),
                    });
                }
                if let Some(h) = &r.holder
                    && h != &me
                {
                    return Err(QueueError::HeldBy {
                        id: r.id.clone(),
                        who: h.clone(),
                    });
                }
                r.holder = Some(me);
                r.status = "active".into();
                Ok(r.clone())
            })
        }

        fn hand_back(&self, id: &str) -> Result<Row, QueueError> {
            self.with(id, |r| {
                r.holder = None;
                r.status = "todo".into();
                Ok(r.clone())
            })
        }

        fn waiting_on(&self, id: &str, of: &str, _what: &str) -> Result<Row, QueueError> {
            let of = of.to_string();
            self.with(id, move |r| {
                r.waiting_on = if of.is_empty() { None } else { Some(of) };
                Ok(r.clone())
            })
        }

        fn done(&self, id: &str, _measured: &str) -> Result<Row, QueueError> {
            self.with(id, |r| {
                r.status = "done".into();
                Ok(r.clone())
            })
        }

        fn describe(&self) -> String {
            format!("fake board (in memory), as `{}`", self.me)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unmounted_board_refuses_without_reading_as_a_fault() {
        let q = NoMount;
        let e = q.claim("r1").unwrap_err();
        assert!(matches!(e, QueueError::NotMounted(_)));
        assert!(matches!(e.outcome(), ToolOutcome::NotRun { .. }));
        assert!(e.to_string().contains("nothing was filed"), "{e}");
        assert!(
            e.to_string().contains("configuration and not a fault"),
            "an unmounted board is the normal case: {e}"
        );
        assert!(
            q.describe().starts_with("not mounted"),
            "the disclosure must read as a configuration: {}",
            q.describe()
        );
    }

    #[test]
    fn an_unreachable_board_is_not_run_and_says_nothing_was_written() {
        let q = FakeQueue::offline("claude-lab2x1");
        let e = q.claim("r1").unwrap_err();
        assert!(matches!(e, QueueError::Unreachable(_)));
        assert!(matches!(e.outcome(), ToolOutcome::NotRun { .. }));
        assert!(e.to_string().contains("NOTHING was written"), "{e}");
    }

    #[test]
    fn a_row_another_seat_took_in_the_window_refuses_and_names_them() {
        let q = FakeQueue::new("claude-lab2x1");
        q.seed("r7", "rewrite the parser", None);
        // Another seat takes it between the read and the write.
        q.set_holder("r7", Some("claude-host"));
        let e = q.claim("r7").unwrap_err();
        match e {
            QueueError::HeldBy { ref who, .. } => assert_eq!(who, "claude-host"),
            other => panic!("expected a lost CAS, got {other:?}"),
        }
        assert!(
            e.to_string().contains("do not start the work anyway"),
            "{e}"
        );
        assert_eq!(q.get("r7").unwrap().holder.as_deref(), Some("claude-host"));
    }

    #[test]
    fn an_unowned_row_is_claimable() {
        let q = FakeQueue::new("claude-lab2x1");
        q.seed("r1", "a row", None);
        let r = q.claim("r1").unwrap();
        assert_eq!(r.holder.as_deref(), Some("claude-lab2x1"));
        assert_eq!(r.status, "active");
    }
}
