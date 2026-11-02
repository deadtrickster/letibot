//! The shared board - the fleet's queue - as a seam, plus the one that refuses. Not mounted is
//! the default and it is a configuration, not a fault: the local path needs no node, no token
//! and no network. [`Queue::claim`] takes no `expect`: forming that opinion is the bug.

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

/// The shared board. Every method that changes a row is a write to state other seats act on, so
/// the `board` tool declares [`crate::schema::Access::Network`] and goes through the gate like
/// any other non-read call, on top of a queue that is actually attached.
pub trait Queue: Send + Sync {
    /// The handle the node knows this seat by, read from the seam and never from an argument: a
    /// tool that let the model name the speaker could speak under another seat's name.
    fn identity(&self) -> String;

    /// Every row this seat can see. `flowy todo` has no list verb - it files, notes, claims and
    /// closes - so a real implementation reads the node's API or MCP surface rather than
    /// shelling out for this one.
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

/// The default, and it is not an error state: no board is mounted. The verbs that only mean
/// something on a shared board return `NotRun` naming what is missing, and the verbs that work
/// either way never reach here - [`super::board::Todo`] serves them from the session's ledger.
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

/// An in-memory board, for tests: it exists so that no test here files, claims or closes a row
/// on the real fleet queue, where other seats act on what it writes.
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
            // The read half of the compare-and-swap: a real implementation reads the node and
            // passes what it read as `--expect`. The fake models the same window, and
            // `set_holder` is how a test slides another seat into it.
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
