//! **One top-level session's connection to the box's room** — the tool's verbs behind it, the bell
//! that becomes a wake, and the beat the daemon runs on the session's behalf.
//!
//! # The three things this file is
//!
//! 1. **[`SessionRoom`] is the `room` tool's half.** `crates/tools` has no socket and no wire; it
//!    has [`letibot_tools::builtins::room::RoomLink`], and this is the implementation that speaks
//!    [`letibot_room::Request`] over the coordinator's socket and answers in sentences.
//! 2. **The bell.** A [`letibot_room::Event`] arrives on the connection's reader thread, becomes a
//!    sentence ([`super::wake::sentence`]) in the session's queue, and rings the worker's bell —
//!    which is firecode's fourth rule satisfied in code: *a bell is something that can inject a
//!    turn.* A session that only ever reads the room when a person prompts it is a mailbox, and the
//!    room would be read when somebody happened to look.
//! 3. **The beat on the session's behalf.** [`SessionRoom::turn_started`] begins beating and
//!    [`SessionRoom::turn_ended`] stops. This is the difference between a design that works and one
//!    that blames the model: a model that must remember to call `beat` is thinking about its work,
//!    and the ninety-second lease would take its item while it works. The beat is the daemon's
//!    because the turn clock is the daemon's.
//!
//! # A subagent has no link
//!
//! The room is for top-level sessions only, and a subagent speaks through its parent. So a session
//! with a `parent_session_id` is given **no link at all** rather than a link that refuses: the tool
//! it would reach is not on its seat either, and two refusals for one rule is one too many to
//! explain. The refusal that matters is the room's own, at the door.
//!
//! # The restart, and the two notes it leaves
//!
//! The pane's fifth control is not a room verb: it is a **daemon lifetime**, asked of the
//! supervisor (`super::supervise`) and never performed by the coordinator on itself. Two things
//! follow, and both are here because this is the object that holds the socket:
//!
//! * [`SessionRoom::restart`] asks the supervisor and then writes the [`letibot_room::Marker`]
//!   beside the socket — *who pressed it, and when* — so the pane can say why the room is a new one.
//! * The reader thread **watches the room's instance** (the lease's `started_ms`). An instance that
//!   changes with no marker naming it is a room that died and came back, and that is written as
//!   [`letibot_room::Cause::Crashed`]. An observation never overwrites an act: a marker that
//!   already claims the instance is left alone.

use std::path::Path;
use std::sync::{Arc, Mutex, Weak};

use letibot_room::client::{Beat, Client};
use letibot_room::{Cause, Decision, Marker, Request, Response, Speaker};

use super::supervise::{RestartFrame, RoomSupervisor};
use super::wake;
use crate::plugins::room_feed::{RestartView, RoomFeed, RoomSnapshot};

/// **How often the room is read for the pane**, and how old a drawn picture may be.
///
/// One second, because that is the finest thing a row says (an age in `{}s`) and because the feed's
/// clock is quantised to the same second (`room_feed::CLOCK_QUANTUM_MS`) — a faster read would be
/// answered from a cache and drawn identically. It runs on a thread of its own and never on a
/// caller's: a pane is drawn on the daemon's idle pass, and a coordinator that is up but wedged
/// must not hold that pass.
pub const PANE_READ_EVERY_MS: u64 = 1_000;

/// **A session's room.** One per top-level session, held by its `Harness` and by its tool.
pub struct SessionRoom {
    client: Client,
    session: String,
    socket: std::path::PathBuf,
    /// The beat, while a turn is running. `None` between turns, which is exactly when the lease is
    /// counting — see [`SessionRoom::turn_ended`].
    beat: Mutex<Option<Beat>>,
    /// **The room as the PANE last saw it** — read on [`SessionRoom::reader`]'s thread, so that
    /// drawing a pane is a clone and never a socket call. `None` until the first read lands.
    room: Mutex<Option<RoomSnapshot>>,
    /// **Who may restart the box's room on this session's behalf** — `None` on the coordinator's
    /// own daemon, on a box with no launcher, and on a fixture. A session with none is a pane that
    /// offers no restart key rather than a key that would refuse.
    supervisor: Option<Arc<dyn RoomSupervisor>>,
    /// **The room instance this link last saw** — the lease's `started_ms`. A change here that no
    /// marker claims is a crash, which is the one cause of a restart nobody asks for.
    seen: Mutex<Option<u64>>,
}

impl std::fmt::Debug for SessionRoom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionRoom")
            .field("session", &self.session)
            .field("socket", &self.socket)
            .finish()
    }
}

impl SessionRoom {
    /// **Connect as `session`**, with `bell` rung for every event the room sends about it.
    ///
    /// `bell` is a closure rather than the registry so this type needs no daemon: the caller —
    /// `Harness::open` — has the bell, and what it does with it is one `ring_wake`.
    pub fn connect(
        session: &str,
        socket: &Path,
        bell: Arc<dyn Fn(&str) + Send + Sync>,
        supervisor: Option<Arc<dyn RoomSupervisor>>,
    ) -> Result<Arc<SessionRoom>, String> {
        let me = session.to_string();
        // **The arbiter is not woken by the queue's traffic.** The coordinator's own session is the
        // arbiter, so the server routes every event to it — that is how a question reaches it — but
        // a turn per `post` would be a turn per item somebody else made, on the one session whose
        // job is to answer questions. `Raised`, `Asked` and `Ruled` are its business; the rest it
        // reads when it looks (`room queue`, `room questions`).
        let arbiter = session == super::session_id();
        let wake: letibot_room::client::Wake = {
            let bell = Arc::clone(&bell);
            let me = me.clone();
            Arc::new(move |event: &letibot_room::Event| {
                use letibot_room::Event;
                if arbiter
                    && matches!(
                        event,
                        Event::Posted { .. } | Event::Claimed { .. } | Event::Done { .. }
                    )
                {
                    return;
                }
                // **The push, then the ring** — a ring with nothing behind it is a wake that finds
                // nothing and costs a pass over the store.
                if let Some(line) = wake::sentence(event) {
                    wake::push(&me, line);
                    bell(&me);
                }
            })
        };
        let client = Client::connect(socket, Speaker::top_level(session), wake)
            .map_err(|e| e.to_string())?;
        let room = Arc::new(SessionRoom {
            client,
            session: session.to_string(),
            socket: socket.to_path_buf(),
            beat: Mutex::new(None),
            room: Mutex::new(None),
            supervisor,
            // **The first sighting is not a crash.** A daemon that connects to a room which has
            // been up for an hour did not witness anything, so the instance is recorded and no
            // marker is written.
            seen: Mutex::new(letibot_room::room_started_ms(socket)),
        });
        SessionRoom::reader(&room);
        Ok(room)
    }

    /// **Start the thread that keeps the pane's picture of the room.** It holds a `Weak`, so the
    /// connection is not kept open by a reader — the same rule the client's own reader thread
    /// keeps, and the reason a dropped session's close is seen by the coordinator at once.
    fn reader(me: &Arc<SessionRoom>) {
        let weak: Weak<SessionRoom> = Arc::downgrade(me);
        let _ = std::thread::Builder::new()
            .name(format!("room-pane-{}", me.session))
            .spawn(move || {
                loop {
                    // **The first read happens now**, not a second from now: a session's panes are
                    // announced as it opens, and the frame after this one should have a room in it.
                    let Some(me) = weak.upgrade() else {
                        return;
                    };
                    let snap = me.read_room();
                    *me.room.lock().unwrap_or_else(|e| e.into_inner()) = Some(snap);
                    me.note_instance();
                    drop(me);
                    std::thread::sleep(std::time::Duration::from_millis(PANE_READ_EVERY_MS));
                    if weak.strong_count() == 0 {
                        // The session is gone. Its `Client` went with it, which closed the socket,
                        // which is the coordinator's `note_gone` — so there is nothing to do here
                        // and nothing to say.
                        return;
                    }
                }
            });
    }

    /// **The room, in one read** — every item, every open question, the arbiter, and the identity.
    ///
    /// A room that cannot be read is not an error here: it is a snapshot whose `unreadable` carries
    /// the sentence, because the pane must draw *nobody answered* rather than an empty queue.
    fn read_room(&self) -> RoomSnapshot {
        let welcome = self.client.welcome();
        let base = RoomSnapshot {
            lease_ms: welcome.lease_ms,
            threshold: welcome.threshold,
            // **Every read carries the restart note**, because the note is about the room the
            // read is of: a pane that showed the queues without saying the room had just begun
            // again would be the opaque screen the feature exists to remove.
            restart: self.restart_view(),
            ..RoomSnapshot::default()
        };
        match self.client.request(&Request::Room) {
            Ok(Response::Room {
                identity,
                arbiter,
                connected,
                items,
                questions,
            }) => RoomSnapshot {
                identity,
                arbiter,
                connected,
                items,
                questions,
                unreadable: None,
                ..base
            },
            Ok(other) => RoomSnapshot {
                unreadable: Some(format!(
                    "the room answered the pane's read with something else: {other:?}"
                )),
                ..base
            },
            Err(e) => RoomSnapshot {
                unreadable: Some(e.to_string()),
                ..base
            },
        }
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// **Restart the box's room — asked of its supervisor, never performed here.**
    ///
    /// The act is a [`RestartFrame`] handed to [`RoomSupervisor::restart`], and the order is the
    /// design:
    ///
    /// 1. **Was there a room to stop?** Asked of the lease before the act, because that is the
    ///    difference between a restart and a start — and the difference between *this key emptied
    ///    the queue* and *the queue was already gone*.
    /// 2. **Ask.** A refusal comes back as a sentence and is published by the caller; nothing is
    ///    written to the marker, because a restart that did not happen is not a restart.
    /// 3. **Write the note** — after the act, carrying the NEW room's `started_ms`, so the pane can
    ///    say when and why, and so the reader thread's own observation knows this instance is
    ///    accounted for.
    /// 4. **Re-read at once**, so the frame after the key is the new room rather than the old one's
    ///    last picture. The client re-opens its socket by itself (`Client::request`), which is the
    ///    same reconnect a coordinator that restarted under it would have forced anyway.
    pub fn restart(&self) -> Result<String, String> {
        let supervisor = self.supervisor.as_ref().ok_or_else(|| {
            "this daemon holds no supervisor for the box's room, so it cannot restart it. The \
             launcher is what starts a coordinator (`letibot --room`), and the room's own daemon \
             is never given one — a restart from its own pane would kill the feed that reports it"
                .to_string()
        })?;
        let frame = RestartFrame {
            at_ms: letibot_room::server::now_ms(),
            by: self.session.clone(),
            was_running: letibot_room::room_running(&self.socket),
            why: "pressed restart in the room pane".to_string(),
        };
        let said = supervisor.restart(&frame)?;
        let instance = letibot_room::room_started_ms(&self.socket).unwrap_or(0);
        // **The cause is what the act found, not what it hoped.** A restart with nothing running
        // is a start, and saying so is the difference between a person believing their key threw
        // the queue away and knowing the room had already gone.
        let cause = if frame.was_running {
            Cause::Asked
        } else {
            Cause::Daemon
        };
        let marker = Marker::new(frame.at_ms, instance, cause).by(self.session.clone());
        if let Err(e) = marker.write(&letibot_room::marker_path(&self.socket)) {
            return Ok(format!(
                "{said}, but the note that says why could not be written ({}): {} — the pane \
                 will read this as a room that came back on its own",
                letibot_room::marker_path(&self.socket).display(),
                e
            ));
        }
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(instance);
        let snap = self.read_room();
        *self.room.lock().unwrap_or_else(|e| e.into_inner()) = Some(snap);
        Ok(format!("{said} — and it begins with no items at all"))
    }

    /// **Watch the room's instance, and write the note nobody asked for.**
    ///
    /// An instance that is not the one this link last saw, and that no marker already claims, is a
    /// room that died and came back — the third of the three causes, and the only one that is an
    /// observation rather than an act. The guard matters in both directions: a restart this daemon
    /// asked for is not a crash, and the daemon's own marker must not be overwritten by the reader
    /// thread noticing the same change a moment later.
    fn note_instance(&self) {
        let Some(instance) = letibot_room::room_started_ms(&self.socket) else {
            // No lease is no room: there is no new instance to name, and the pane's own
            // `unreadable` sentence is the honest report until one appears.
            return;
        };
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        match *seen {
            Some(prev) if prev == instance => return,
            None => {
                *seen = Some(instance);
                return;
            }
            Some(_) => {}
        }
        *seen = Some(instance);
        let path = letibot_room::marker_path(&self.socket);
        if Marker::read(&path).is_some_and(|m| m.claims(instance)) {
            return;
        }
        let says = format!(
            "the room this one replaced began {} ago",
            letibot_room::duration(letibot_room::server::now_ms().saturating_sub(instance))
        );
        let _ = Marker::new(letibot_room::server::now_ms(), instance, Cause::Crashed)
            .saying(says)
            .write(&path);
    }

    /// **What the pane is told about the restart** — whether this host may ask for one, why not
    /// when it may not, and the note the last one left.
    fn restart_view(&self) -> RestartView {
        RestartView {
            available: self.supervisor.is_some(),
            why: self.supervisor.is_none().then(|| {
                "this host was not given a supervisor for the box's room, so there is no restart \
                 key here — a coordinator is started by the launcher (`letibot --room`)"
                    .to_string()
            }),
            marker: letibot_room::read_marker(&self.socket),
        }
    }

    /// **A turn is running: say so for as long as it does.** Idempotent — a second call while a beat
    /// is already running is the same beat, because two would double every message.
    pub fn turn_started(&self) {
        let mut slot = self.beat.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(self.client.start_beating());
        }
    }

    /// **The turn is over: stop beating.** The lease now counts from the last beat, which is the
    /// fact the room measures — a session whose turn ended is alive and *not working on it*.
    pub fn turn_ended(&self) {
        let mut slot = self.beat.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(beat) = slot.take() {
            beat.stop();
        }
    }

    /// One verb, and the sentence a model reads back.
    pub fn call(&self, verb: &str, args: &serde_json::Value) -> Result<String, String> {
        let request = request_of(verb, args)?;
        let response = self
            .client
            .request(&request)
            .map_err(|e| format!("the room at {} did not answer: {e}", self.socket.display()))?;
        answer_sentence(&response, &self.session)
    }

    /// **Every reminder the room owes, as data rather than as a sentence.**
    ///
    /// The `reminders` VERB answers a model in words (`answer_sentence`), and the daemon's pass
    /// needs the same answer as values: one row per item a session holds, with the wait it declared
    /// and whether its beat has gone quiet. A daemon that parsed the sentence back would be reading
    /// its own formatting, so the request is sent here and the reply read as the struct it is.
    ///
    /// **It is the whole room's list, not this session's** — `reminders` is one read for the room —
    /// so the caller filters by `Reminder::session`. That is deliberate rather than sloppy: the
    /// coordinator answers in one frame, and a per-session read would be a second frame shape for a
    /// question the room already answers.
    pub fn reminders(&self) -> Result<Vec<letibot_room::Reminder>, String> {
        let response = self
            .client
            .request(&Request::Reminders)
            .map_err(|e| format!("the room at {} did not answer: {e}", self.socket.display()))?;
        match response {
            Response::Reminders { rows } => Ok(rows),
            Response::Bye { why } => Err(format!("the room closed the connection: {why}")),
            // **A frame that is not the answer to this read** — said as a fact rather than
            // guessed at, because a daemon that invented a meaning here would put a wrong row on
            // a board. A `Request::Reminders` carries no id, so there is nothing to correlate.
            _ => Err(
                "the room answered a frame that is not a `reminders` read — nothing was written on \
                 the board"
                    .to_string(),
            ),
        }
    }
}

impl letibot_tools::builtins::room::RoomLink for SessionRoom {
    fn call(&self, verb: &str, args: &serde_json::Value) -> Result<String, String> {
        SessionRoom::call(self, verb, args)
    }
}

/// **The pane's half** — the room as a feed is built from, and the four controls performed.
///
/// The split is the point of the slice: the LAYER decides which row to draw and which verb it
/// offers; what a `claim` or a `release` DOES is the room's state machine, and this is the host
/// doing it over the session's own connection — the same connection its `room` tool uses, because a
/// second connection speaking one session would be a second close, and a close gives the claims
/// back.
impl RoomFeed for SessionRoom {
    fn snapshot(&self) -> RoomSnapshot {
        let welcome = self.client.welcome();
        match self.room.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            Some(snap) => snap,
            // **The first frame is a promise, and the second is the answer.** The reader thread
            // starts at connect and reads once a second; a pane opened before the first read lands
            // says so rather than drawing an empty room, which would read as *nothing is queued*.
            None => RoomSnapshot {
                lease_ms: welcome.lease_ms,
                threshold: welcome.threshold,
                restart: self.restart_view(),
                unreadable: Some(
                    "the room has not been read yet — the first read is a second away".to_string(),
                ),
                ..RoomSnapshot::default()
            },
        }
    }

    /// **The pane's fifth control** — the restart, asked of the supervisor rather than of the room.
    fn restart(&self) -> Result<String, String> {
        SessionRoom::restart(self)
    }

    fn act(&self, entry: &str, verb: &str) -> Result<String, String> {
        let frame = pane_frame(verb, entry)?;
        let response = self
            .client
            .request(&frame)
            .map_err(|e| format!("the room at {} did not answer: {e}", self.socket.display()))?;
        // **And re-read at once**, so the frame the person sees next is the state their key made
        // — the reader thread would catch up within a second, and a second of *nothing happened* is
        // the one thing a key must never look like.
        let snap = self.read_room();
        *self.room.lock().unwrap_or_else(|e| e.into_inner()) = Some(snap);
        answer_sentence(&response, &self.session)
    }
}

/// **A request id that cannot repeat** — the verb, the entry, the clock, and a counter.
///
/// **The clock alone is not enough, and the failure is silent.** The room answers a repeated
/// `(session, id)` from its replay window instead of acting twice — which is exactly what a
/// reconnecting client needs, and exactly what swallows a second act in the same millisecond. Two
/// round trips on a unix socket are tens of microseconds, so `claim` and `release` on one row, or
/// the same key pressed twice, really can share a millisecond; the second would be answered *the
/// same* rather than performed. MEASURED: the pane's own round-trip test lost an abandonment that
/// way, and the item sat at two instead of three.
fn request_id(what: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{what}-{}-{}",
        letibot_room::server::now_ms(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// **A row's key, as a frame** — the PANE's grammar, which is not the tool's.
///
/// A model's call names its arguments (`{"verb":"claim_item","item":12}`); a row names an ENTRY,
/// which is one string that has to say both what it is about and, for a ruling, what the decision
/// is. Two grammars, two doors, and this is the one the four controls come through:
///
/// | entry | verb | frame |
/// |---|---|---|
/// | `i12` | `claim` | `claim_item` 12 |
/// | `i12` | `release` | `abandon` 12, *released from the room pane* |
/// | `i12` | `abandon` | `abandon` 12, *marked abandoned from the room pane* |
/// | `q3:retry` | `rule` | `rule` 3, `Retry` |
/// | `q3:drop` | `rule` | `rule` 3, `Drop` |
/// | `q3:hand:s-a` | `rule` | `rule` 3, `Hand { to: "s-a" }` |
///
/// **`release` and `abandon` are one verb in the room and two intentions here.** A voluntary
/// release is still an abandonment and still counted (`docs/room.md` § *A claim is a lease with a
/// beat*), so the difference the person gets is the sentence that lands on the item's history — and
/// the sentence is the host's to write, not the layer's.
fn pane_frame(verb: &str, entry: &str) -> Result<Request, String> {
    let id = request_id(&format!("{verb}-{entry}"));
    let item_of = |entry: &str| -> Result<u64, String> {
        entry
            .strip_prefix('i')
            .and_then(|n| n.parse::<u64>().ok())
            .ok_or_else(|| {
                format!("`{entry}` names no item — a row's entry is `i<number>`, and a question's is `q<number>:<decision>`")
            })
    };
    match verb {
        "claim" => Ok(Request::ClaimItem {
            id,
            item: item_of(entry)?,
        }),
        "release" => Ok(Request::Abandon {
            id,
            item: item_of(entry)?,
            why: "released from the room pane".to_string(),
        }),
        "abandon" => Ok(Request::Abandon {
            id,
            item: item_of(entry)?,
            why: "marked abandoned from the room pane".to_string(),
        }),
        "rule" => {
            let (question, rest) = entry.split_once(':').ok_or_else(|| {
                format!("`{entry}` names no decision — a question's entry is `q<number>:retry|drop|hand:<session>`")
            })?;
            let question: u64 = question
                .strip_prefix('q')
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| {
                    format!("`{entry}` names no question — a question's entry is `q<number>:…`")
                })?;
            let (decision, to) = match rest.split_once(':') {
                Some((d, t)) => (d, Some(t)),
                None => (rest, None),
            };
            let decision = match (decision, to) {
                ("retry", None) => Decision::Retry,
                ("drop", None) => Decision::Drop {
                    why: "dropped from the room pane".to_string(),
                },
                ("hand", Some(to)) if !to.is_empty() => Decision::Hand { to: to.to_string() },
                ("hand", _) => {
                    return Err(format!(
                        "`{entry}` names no session — `hand` needs one, and the pane draws a row \
                         per session the room has met (`q3:hand:s-a`)"
                    ));
                }
                (other, _) => {
                    return Err(format!(
                        "`{other}` is not a decision — say `retry`, `drop` or `hand:<session>`"
                    ));
                }
            };
            Ok(Request::Rule {
                id,
                question,
                decision,
            })
        }
        other => Err(format!(
            "`{other}` is not one of the room pane's four controls — they are claim, release, \
             abandon and rule"
        )),
    }
}

/// **The verb and its arguments, as a frame.** `Err` is the tool's own refusal, in its words.
fn request_of(verb: &str, args: &serde_json::Value) -> Result<Request, String> {
    let s = |k: &str| {
        args.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let n = |k: &str| args.get(k).and_then(|v| v.as_u64());
    // **The id is minted here, per call**, and it is what makes a reconnect safe: a repeat of the
    // same frame after a dropped socket is answered from the room's replay window instead of acting
    // twice. See [`request_id`] for why the clock alone will not do.
    let id = request_id(verb);
    Ok(match verb {
        "post" => Request::Post {
            id,
            tag: s("tag"),
            body: s("body"),
        },
        "claim" => Request::Claim { id, tag: s("tag") },
        "claim_item" => Request::ClaimItem {
            id,
            item: n("item").ok_or("`claim_item` needs `item`")?,
        },
        "beat" => Request::Beat {
            id,
            item: n("item").ok_or("`beat` needs `item`")?,
        },
        "waiting" => Request::Waiting {
            id,
            item: n("item").ok_or("`waiting` needs `item`")?,
            text: s("text"),
        },
        "done" => Request::Done {
            id,
            item: n("item").ok_or("`done` needs `item`")?,
        },
        "abandon" => Request::Abandon {
            id,
            item: n("item").ok_or("`abandon` needs `item`")?,
            why: s("why"),
        },
        "dispute" => Request::Dispute {
            id,
            item: n("item").ok_or("`dispute` needs `item`")?,
            says: s("says"),
        },
        "rule" => {
            let decision = match s("decision").as_str() {
                "retry" => Decision::Retry,
                "hand" => {
                    let to = s("to");
                    if to.is_empty() {
                        return Err(
                            "`decision: \"hand\"` needs `to`: the session that gets the claim"
                                .to_string(),
                        );
                    }
                    Decision::Hand { to }
                }
                "drop" => Decision::Drop { why: s("why") },
                other => {
                    return Err(format!(
                        "`{other}` is not a decision — say `retry`, `hand` (with `to`) or `drop` \
                         (with `why`)"
                    ));
                }
            };
            Request::Rule {
                id,
                question: n("question").ok_or("`rule` needs `question`")?,
                decision,
            }
        }
        "queue" => Request::Queue { tag: s("tag") },
        "holding" => Request::Holding {
            session: Some(s("session")).filter(|x| !x.is_empty()),
        },
        "reminders" => Request::Reminders,
        "questions" => Request::Questions,
        other => {
            return Err(format!(
                "`{other}` is not a room verb — the verbs are post, claim, claim_item, beat, \
                 waiting, done, abandon, dispute, rule, queue, holding, reminders and questions"
            ));
        }
    })
}

/// **What the model reads back.** Every arm names the item and the state, because a reply a model
/// cannot check is a reply it will ask about again.
fn answer_sentence(response: &Response, me: &str) -> Result<String, String> {
    Ok(match response {
        Response::Posted { item, .. } => format!(
            "posted #{id} under `{tag}` — {}",
            describe(item),
            id = item.id,
            tag = item.tag
        ),
        Response::Claimed { item, .. } => format!(
            "you hold #{id} under `{tag}` — {}; your daemon beats for you while your turn runs",
            describe(item),
            id = item.id,
            tag = item.tag
        ),
        Response::Beaten { item, .. } => format!("#{} — your beat is recorded", item.id),
        Response::Waited { item, .. } => format!(
            "#{} — the room will remind you of this, and whoever is queued behind you can see it",
            item.id
        ),
        Response::Finished { item, .. } => {
            format!("#{} is done and `{}` is free", item.id, item.tag)
        }
        Response::Abandoned { item, .. } => format!(
            "#{id} went back to `{tag}` with {n} claimant(s) lost — {state}",
            id = item.id,
            tag = item.tag,
            n = item.abandonments(),
            state = describe(item)
        ),
        Response::Queue { tag, rows } => {
            if rows.is_empty() {
                format!("`{tag}` is empty")
            } else {
                let mut s = format!("`{tag}`, first come first served:");
                for row in rows {
                    s.push_str(&format!(
                        "\n  #{id} {body} — {state} (posted by `{by}`)",
                        id = row.id,
                        body = row.body,
                        state = describe(row),
                        by = row.posted_by
                    ));
                }
                s
            }
        }
        Response::Holding { session, rows } => {
            if rows.is_empty() {
                format!("`{session}` holds nothing in the room")
            } else {
                let mut s = format!("`{session}` holds:");
                for row in rows {
                    s.push_str(&format!(
                        "\n  #{id} under `{tag}` — {body} — held {held}, last beat {beat}{waiting}",
                        id = row.item,
                        tag = row.tag,
                        body = row.body,
                        held = letibot_room::duration(row.held_ms),
                        beat = row.beat_ms,
                        waiting = match &row.waits_on {
                            Some(w) => format!(", waiting on `{}`", w.text),
                            None => String::new(),
                        }
                    ));
                }
                s
            }
        }
        Response::Reminders { rows } => {
            if rows.is_empty() {
                "nobody holds anything in the room".to_string()
            } else {
                rows.iter()
                    .map(|r| format!("`{}`: {}", r.session, r.sentence()))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        Response::Questions { rows } => {
            if rows.is_empty() {
                "the room is not asking anything".to_string()
            } else {
                rows.iter()
                    .map(|q| {
                        format!(
                            "question #{id}: {text}\n  facts: {facts}",
                            id = q.id,
                            text = q.text,
                            facts = serde_json::to_string(&q.facts).unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n")
            }
        }
        Response::Question { question, .. } => format!(
            "question #{id} is open: {text}",
            id = question.id,
            text = question.text
        ),
        Response::Ruled { question, .. } => match &question.answer {
            Some(a) => format!("question #{} is answered: {}", question.id, a.decision),
            None => format!("question #{} is still open", question.id),
        },
        Response::Refused { why, .. } => return Err(why.to_string()),
        Response::Bye { why } => return Err(format!("the room closed the connection: {why}")),
        Response::Welcome { .. } => format!(
            "the room is open to `{me}` (protocol {})",
            letibot_room::ROOM_PROTOCOL_VERSION
        ),
        // **The pane's read, in a sentence** — a model never asks it (the tool's verbs have no
        // `room` read), so this is the honest summary rather than a drawing: what the room is, how
        // much is in it, and how many questions are open.
        Response::Room {
            identity,
            items,
            questions,
            ..
        } => {
            let mut tags: Vec<&str> = items.iter().map(|i| i.tag.as_str()).collect();
            tags.sort_unstable();
            tags.dedup();
            format!(
                "{identity}\n{} item(s) under {} tag(s), {} question(s) open",
                items.len(),
                tags.len(),
                questions.len()
            )
        }
        Response::Event { event } => format!("the room said: {event:?}"),
    })
}

fn describe(item: &letibot_room::Item) -> String {
    match &item.state {
        letibot_room::State::Open => "open".to_string(),
        letibot_room::State::Claimed { claim } => {
            format!("held by `{}` for {}", claim.by, claim.beat_ms)
        }
        letibot_room::State::Done { by, .. } => format!("done by `{by}`"),
        letibot_room::State::Abandoned { last } => format!("abandoned — {}", last.why),
        letibot_room::State::Dropped { by, why, .. } => format!("dropped by `{by}`: {why}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_verb_becomes_a_frame_with_a_request_id() {
        let cases = [
            json!({"verb": "post", "tag": "ram", "body": "8 GB"}),
            json!({"verb": "claim", "tag": "ram"}),
            json!({"verb": "claim_item", "item": 1}),
            json!({"verb": "beat", "item": 1}),
            json!({"verb": "waiting", "item": 1, "text": "the suite"}),
            json!({"verb": "done", "item": 1}),
            json!({"verb": "abandon", "item": 1, "why": "no"}),
            json!({"verb": "dispute", "item": 1, "says": "still on it"}),
            json!({"verb": "rule", "question": 1, "decision": "retry"}),
            json!({"verb": "queue", "tag": "ram"}),
            json!({"verb": "holding"}),
            json!({"verb": "reminders"}),
            json!({"verb": "questions"}),
        ];
        for args in cases {
            let verb = args["verb"].as_str().unwrap();
            let frame = request_of(verb, &args).unwrap_or_else(|e| panic!("{verb}: {e}"));
            // **Every MUTATING frame carries an id** — that is what makes a reconnect's repeat
            // answerable. A read has none by design (`Request::request_id` says so): there is
            // nothing to act on twice, and a re-read is cheap and cannot be wrong.
            let mutates = !matches!(
                frame,
                Request::Queue { .. }
                    | Request::Holding { .. }
                    | Request::Reminders
                    | Request::Questions
            );
            assert!(
                frame.request_id().is_some() == mutates,
                "{verb}: a mutating frame needs a request id and a read must not have one: {frame:?}"
            );
        }
    }

    #[test]
    fn a_hand_without_a_target_is_refused_here_and_never_sent() {
        let e = request_of("rule", &json!({"question": 1, "decision": "hand"})).unwrap_err();
        assert!(e.contains("needs `to`"), "{e}");
        let e = request_of("rule", &json!({"question": 1, "decision": "maybe"})).unwrap_err();
        assert!(e.contains("not a decision"), "{e}");
        let e = request_of("nonsense", &json!({})).unwrap_err();
        assert!(e.contains("not a room verb"), "{e}");
    }

    #[test]
    fn a_refusal_from_the_room_is_a_sentence_and_not_a_panic() {
        let refused = Response::Refused {
            id: Some("c1".into()),
            why: letibot_room::Refused::Busy {
                tag: "ram".into(),
                item: 1,
                by: "s-b".into(),
                held_for: "40s".into(),
            },
        };
        let e = answer_sentence(&refused, "s-a").unwrap_err();
        assert!(e.contains("held by `s-b`"), "{e}");
        assert!(e.contains("a tag is a lock"), "{e}");
    }

    /// **The wake, end to end minus the harness**: a real server, two real connections, and the
    /// bell the daemon rings.
    ///
    /// What this proves is the half that could be wrong: an event about `s-a` reaches `s-a`'s
    /// connection, becomes a sentence in `wake`, and rings the bell — while a session with nothing
    /// under the tag is not rung at all. What it does not prove is `Harness::wake` composing the
    /// sentence into a turn, which is one `take()` (`wake::tests` covers the queue, and the
    /// harness's own wake is where every notice source meets).
    #[test]
    fn an_event_about_a_session_rings_its_bell_and_a_reader_is_not_rung() {
        let dir = std::env::temp_dir().join(format!("letibot-room-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");

        let rung: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let bell: Arc<dyn Fn(&str) + Send + Sync> = {
            let rung = Arc::clone(&rung);
            Arc::new(move |session: &str| {
                rung.lock().unwrap().push(session.to_string());
            })
        };
        let me = SessionRoom::connect("s-a", &path, bell.clone(), None).expect("s-a connects");
        let other = Client::connect(
            &path,
            Speaker::top_level("s-b"),
            Arc::new(|_: &letibot_room::Event| {}),
        )
        .expect("s-b connects");

        // `s-a` posts, so it is a MEMBER of `ram` — a reader is not.
        me.call("post", &serde_json::json!({"tag": "ram", "body": "8 GB"}))
            .expect("a post");
        let _ = other.request(&Request::Post {
            id: "p2".into(),
            tag: "ram".into(),
            body: "2 GB".into(),
        });
        // And somebody else's claim under that tag is news `s-a` must be woken by.
        let claimed = other
            .request(&Request::Claim {
                id: "c1".into(),
                tag: "ram".into(),
            })
            .expect("a claim");
        assert!(matches!(claimed, Response::Claimed { .. }), "{claimed:?}");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && wake::waiting("s-a") == 0 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            rung.lock().unwrap().iter().any(|s| s == "s-a"),
            "the bell must ring for the session the event is about: {:?}",
            rung.lock().unwrap()
        );
        let said = wake::take("s-a");
        assert!(
            !said.is_empty(),
            "a ring with nothing behind it is not a wake"
        );
        assert!(
            said.iter()
                .any(|s| s.contains("[room]") && s.contains("s-b")),
            "the sentence names who did it: {said:?}"
        );

        // **The arbiter is not woken by the queue's traffic.** It hears every event on its
        // connection — that is how a question reaches it — but a `post` is not a turn.
        let arbiter = super::super::session_id();
        let rung_arbiter: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let bell_arbiter: Arc<dyn Fn(&str) + Send + Sync> = {
            let n = Arc::clone(&rung_arbiter);
            Arc::new(move |_: &str| *n.lock().unwrap() += 1)
        };
        let host = SessionRoom::connect(&arbiter, &path, bell_arbiter, None).expect("the arbiter");
        let _ = other.request(&Request::Post {
            id: "p3".into(),
            tag: "ram".into(),
            body: "1 GB".into(),
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(
            *rung_arbiter.lock().unwrap(),
            0,
            "a post is not a question: the arbiter is woken by a raise, not by the queue moving"
        );
        assert_eq!(wake::waiting(&arbiter), 0);
        drop(host);
        drop(me);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A row's entry and verb become the frame the room acts on**, and a word the pane does not
    /// have is refused here rather than sent.
    #[test]
    fn a_panes_entry_and_verb_become_a_frame() {
        let id = |frame: &Request| frame.request_id().map(str::to_string);
        // `claim` on an item is `claim_item` — the operator's own control, and it must still be the
        // head (the room refuses `NotTheHead`, which is the whole ordering rule).
        let claim = pane_frame("claim", "i12").expect("claim");
        assert!(matches!(claim, Request::ClaimItem { item: 12, .. }));
        assert!(id(&claim).is_some(), "a mutating frame carries an id");

        // **`release` and `abandon` are the room's one verb and two sentences.**
        let release = pane_frame("release", "i12").expect("release");
        let abandon = pane_frame("abandon", "i12").expect("abandon");
        match (&release, &abandon) {
            (
                Request::Abandon {
                    item: 12, why: one, ..
                },
                Request::Abandon {
                    item: 12, why: two, ..
                },
            ) => {
                assert!(one.contains("released"), "{one}");
                assert!(two.contains("abandoned"), "{two}");
                assert_ne!(one, two, "the difference the person gets is the sentence");
            }
            other => panic!("both are abandonments of the same item: {other:?}"),
        }

        // **`rule` names the decision in the entry**, because the decision is what was picked.
        assert!(matches!(
            pane_frame("rule", "q3:retry"),
            Ok(Request::Rule {
                question: 3,
                decision: Decision::Retry,
                ..
            })
        ));
        assert!(matches!(
            pane_frame("rule", "q3:drop"),
            Ok(Request::Rule {
                question: 3,
                decision: Decision::Drop { .. },
                ..
            })
        ));
        assert!(matches!(
            pane_frame("rule", "q3:hand:s-a"),
            Ok(Request::Rule {
                question: 3,
                decision: Decision::Hand { to },
                ..
            }) if to == "s-a"
        ));

        // **And the refusals are this door's own**, in words a person can act on.
        let hand = pane_frame("rule", "q3:hand").unwrap_err();
        assert!(hand.contains("names no session"), "{hand}");
        let decision = pane_frame("rule", "q3:maybe").unwrap_err();
        assert!(decision.contains("not a decision"), "{decision}");
        let no_question = pane_frame("rule", "q3").unwrap_err();
        assert!(no_question.contains("names no decision"), "{no_question}");
        let no_item = pane_frame("claim", "q3:retry").unwrap_err();
        assert!(no_item.contains("names no item"), "{no_item}");
        let other = pane_frame("post", "i1").unwrap_err();
        assert!(other.contains("four controls"), "{other}");
    }

    /// **The four controls, round trip: a row's key, the room's state machine, the state it made.**
    ///
    /// This is the whole slice's contract in one test — the layer names a verb and an entry, the
    /// HOST performs it over the session's own connection, and the room is what decides.
    #[test]
    fn the_four_controls_reach_the_room_and_change_it() {
        let dir = std::env::temp_dir().join(format!("letibot-room-acts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");
        let me =
            SessionRoom::connect("s-a", &path, Arc::new(|_: &str| {}), None).expect("s-a connects");

        // Two items under one tag, posted by another session so `s-a` can claim the head.
        let other = Client::connect(&path, Speaker::top_level("s-b"), Arc::new(|_| {}))
            .expect("s-b connects");
        for (id, body) in [("p1", "8 GB"), ("p2", "2 GB")] {
            other
                .request(&Request::Post {
                    id: id.into(),
                    tag: "ram".into(),
                    body: body.into(),
                })
                .expect("a post");
        }

        // **`claim`** — on this session's behalf, and the room says so.
        let said = me.act("i1", "claim").expect("a claim");
        assert!(said.contains("you hold #1"), "{said}");
        assert_eq!(
            server.items()[0].holder(),
            Some("s-a"),
            "the item is held by the session whose pane it is"
        );

        // **`release`** — the same session gives it up, and it goes back to the head of `ram`.
        let said = me.act("i1", "release").expect("a release");
        assert!(said.contains("went back to `ram`"), "{said}");
        let item = &server.items()[0];
        assert_eq!(item.holder(), None);
        assert_eq!(item.abandonments(), 1, "a voluntary release is counted");
        assert!(
            matches!(&item.abandonments[0].why, letibot_room::Why::Released { says } if says.contains("released from the room pane")),
            "the sentence the host wrote is on the item: {:?}",
            item.abandonments[0].why
        );

        // **`abandon`** — the same verb, the harder sentence, and it is still one abandonment.
        me.act("i1", "claim").expect("a claim");
        me.act("i1", "abandon").expect("an abandonment");
        let item = &server.items()[0];
        assert_eq!(item.abandonments(), 2);
        assert!(
            matches!(&item.abandonments[1].why, letibot_room::Why::Released { says } if says.contains("marked abandoned")),
            "{:?}",
            item.abandonments[1].why
        );

        // **And the third raises a question**, which the last control answers.
        me.act("i1", "claim").expect("a claim");
        me.act("i1", "abandon").expect("the third abandonment");
        let questions = server.open_questions();
        assert_eq!(questions.len(), 1, "three lost claimants freeze the item");
        let entry = format!("q{}:retry", questions[0].id);
        let said = me.act(&entry, "rule").expect("a ruling");
        assert!(said.contains("is answered"), "{said}");
        assert!(
            server.open_questions().is_empty(),
            "and the question is closed"
        );
        assert!(
            server.items()[0].claimable(),
            "`retry` puts it back at the head with its count kept"
        );

        // **A key the room will not take is refused in the room's own words** — the person is told
        // what happened to it rather than left with a key that did nothing.
        let refused = me
            .act("i2", "release")
            .expect_err("`s-a` holds nothing on #2");
        assert!(refused.contains("does not hold"), "{refused}");

        drop(other);
        drop(me);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The pane's picture of the room is read off the socket, and the identity comes with it.**
    #[test]
    fn the_feed_snapshot_carries_the_rooms_identity_and_its_items() {
        let dir = std::env::temp_dir().join(format!("letibot-room-snap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");
        server.with_arbiter("s-arbiter");
        let me =
            SessionRoom::connect("s-a", &path, Arc::new(|_: &str| {}), None).expect("connects");
        me.call("post", &json!({"tag": "ram", "body": "8 GB"}))
            .expect("a post");

        // **The picture arrives from a thread of its own**, so the test waits for it rather than
        // sleeping a fixed second and hoping: a pane opened before the first read lands says *not
        // read yet*, which is the honest first frame and a poor assertion.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut snap = me.snapshot();
        // **The first read is a promise and the second is the answer** — the reader thread starts
        // at connect, so the frame it lands may be the one taken BEFORE this test's post. Waiting
        // for the item as well as for the read is the difference between a test that measures the
        // feed and one that measures the scheduler.
        while (snap.unreadable.is_some() || snap.items.is_empty())
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
            snap = me.snapshot();
        }
        assert_eq!(snap.unreadable, None, "{:?}", snap.unreadable);
        assert!(snap.identity.contains("no store"), "{}", snap.identity);
        assert_eq!(snap.arbiter.as_deref(), Some("s-arbiter"));
        assert!(
            snap.connected.iter().any(|c| c == "s-a"),
            "who is up: {:?}",
            snap.connected
        );
        assert_eq!(snap.items.len(), 1);
        assert_eq!(snap.items[0].tag, "ram");
        assert_eq!(snap.lease_ms, letibot_room::DEFAULT_LEASE_MS);

        drop(me);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The restart is a FRAME to the supervisor, and what it found decides the note's cause.**
    ///
    /// Two acts, one fixture supervisor: the first with no lease on the socket — nothing was
    /// running, so the key is a start and the note says the queue was already gone — and the second
    /// with a live lease, which is a restart and names the session whose pane it was.
    #[test]
    fn the_restart_is_a_frame_to_the_supervisor_and_the_note_says_what_it_found() {
        use std::sync::Mutex as StdMutex;

        struct Recorder(StdMutex<Vec<RestartFrame>>);
        impl RoomSupervisor for Recorder {
            fn restart(&self, frame: &RestartFrame) -> Result<String, String> {
                self.0.lock().unwrap().push(frame.clone());
                Ok("the room was stopped and started again by a fixture".to_string())
            }
        }

        let dir = std::env::temp_dir().join(format!("letibot-room-restart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");
        let rec = Arc::new(Recorder(StdMutex::new(Vec::new())));
        let me = SessionRoom::connect("s-a", &path, Arc::new(|_: &str| {}), Some(rec.clone()))
            .expect("s-a connects");

        // **No lease: nothing was running.** The act is a start, and the note says so — the key did
        // not empty the queue, the room had already gone.
        let said = me.restart().expect("the supervisor answers");
        assert!(said.contains("no items at all"), "{said}");
        let frames = rec.0.lock().unwrap().clone();
        assert_eq!(frames.len(), 1, "one act, one frame");
        assert_eq!(frames[0].by, "s-a");
        assert!(!frames[0].was_running, "there was nothing to stop");
        assert!(
            frames[0].why.contains("pressed restart"),
            "{}",
            frames[0].why
        );
        let note = letibot_room::read_marker(&path).expect("the note is written");
        assert_eq!(note.cause, Cause::Daemon);
        assert_eq!(note.by, "s-a");
        assert_eq!(note.room_started_ms, 0, "there is no lease to read");

        // **A live lease: this IS a restart**, and the note names the session and the room it made.
        let lease =
            letibot_room::CoordinatorLease::owned_by(path.with_extension("lease"), "box/1/1");
        std::fs::write(
            lease.path(),
            format!("box/1/1\t{}\t5000\t7000\n", std::process::id()),
        )
        .unwrap();
        me.restart().expect("the supervisor answers again");
        let frames = rec.0.lock().unwrap().clone();
        assert_eq!(frames.len(), 2);
        assert!(frames[1].was_running, "there was a room to stop");
        let note = letibot_room::read_marker(&path).expect("the note is rewritten");
        assert_eq!(note.cause, Cause::Asked);
        assert_eq!(note.room_started_ms, 7000, "the room this note is about");
        assert!(
            note.sentence(0).contains("pressed restart"),
            "{}",
            note.sentence(0)
        );

        drop(me);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A supervisor that refuses leaves the room alone and says why** — and writes no note,
    /// because a restart that did not happen is not a restart.
    #[test]
    fn a_refused_restart_is_a_sentence_and_leaves_no_note() {
        struct Refuse;
        impl RoomSupervisor for Refuse {
            fn restart(&self, _frame: &RestartFrame) -> Result<String, String> {
                Err("`letibot --room --stop` answered exit status: 1: no room on this box".into())
            }
        }

        let dir =
            std::env::temp_dir().join(format!("letibot-room-norestart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");
        let me = SessionRoom::connect("s-a", &path, Arc::new(|_: &str| {}), Some(Arc::new(Refuse)))
            .expect("s-a connects");

        let why = me.restart().expect_err("the supervisor refused");
        assert!(why.contains("no room on this box"), "{why}");
        assert_eq!(
            letibot_room::read_marker(&path),
            None,
            "a restart that did not happen is not a restart"
        );

        drop(me);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A host with no supervisor refuses in words**, and the sentence says which door is which:
    /// the launcher starts coordinators, and the room's own daemon is never given one.
    #[test]
    fn a_host_with_no_supervisor_refuses_the_restart_in_words() {
        let dir = std::env::temp_dir().join(format!("letibot-room-nosup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");
        let me =
            SessionRoom::connect("s-a", &path, Arc::new(|_: &str| {}), None).expect("s-a connects");
        let why = me.restart().expect_err("no supervisor, no restart");
        assert!(why.contains("no supervisor"), "{why}");
        assert!(why.contains("letibot --room"), "{why}");
        assert!(why.contains("kill the feed that reports it"), "{why}");
        drop(me);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A room that came back with nobody asking is written as a crash — once, and never over an
    /// act's own note.** The instance is the lease's `started_ms`, which is the only trace a room
    /// leaves when it is not answering.
    #[test]
    fn an_instance_that_changes_with_nobody_asking_is_a_crash_and_an_act_is_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("letibot-room-crash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("room.sock");
        let server = letibot_room::Server::serve(&path, letibot_room::Room::new())
            .expect("a room to talk to");
        let lease = path.with_extension("lease");
        std::fs::write(
            &lease,
            format!("box/1/1\t{}\t5000\t7000\n", std::process::id()),
        )
        .unwrap();
        let me =
            SessionRoom::connect("s-a", &path, Arc::new(|_: &str| {}), None).expect("s-a connects");
        assert_eq!(letibot_room::read_marker(&path), None, "nothing yet");

        // The room is a new one: a different `started_ms`, and no note accounting for it.
        std::fs::write(
            &lease,
            format!("box/1/1\t{}\t9000\t9001\n", std::process::id()),
        )
        .unwrap();
        me.note_instance();
        let note = letibot_room::read_marker(&path).expect("the crash is written");
        assert_eq!(note.cause, Cause::Crashed);
        assert_eq!(note.room_started_ms, 9001);
        assert!(
            note.sentence(9_500).contains("nobody asked"),
            "{}",
            note.sentence(9_500)
        );

        // **An act's note is not overwritten by the observation.** A marker that already claims the
        // room the reader is looking at is left alone — the daemon asked for this restart, and
        // saying *nobody asked* would be a lie about the last minute.
        let asked = Marker::new(9_100, 9001, Cause::Asked).by("s-a");
        asked.write(&letibot_room::marker_path(&path)).unwrap();
        *me.seen.lock().unwrap() = Some(7000);
        me.note_instance();
        assert_eq!(
            letibot_room::read_marker(&path),
            Some(asked),
            "the act's note stands"
        );

        drop(me);
        server.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
