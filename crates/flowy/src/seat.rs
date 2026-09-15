//! The seat: a persistent identity, held by the daemon, that sessions attach to.
//!
//! # What a seat is here
//!
//! One name, one token, one inbox reader, one local waiter claim, one presence
//! on the roster. It is minted once by the operator and it outlives every
//! session — which is the whole reason it is not a session's. The daemon opens
//! it at start and holds it until it exits; sessions come and go underneath.
//!
//! # The loop, and the contract each line keeps
//!
//! ```text
//! loop:
//!   token file changed?          → STOP, say so   (polling as somebody the seat no longer is)
//!   poll  GET /api/inbox/wait     at the loosest level any attached session wants
//!   spool the page                (before the ack, so a crash is a duplicate)
//!   ack   POST /api/inbox/ack     (the mark moves over everything READ)
//!   fan out to attached sessions  (each through its own table)
//!   nobody attached?             → backlog, labelled, for the next one
//!   renew the monitors this loop is the reason for
//! ```
//!
//! - **Unreachable is a state, not a retry.** Closed loop §5: the first failed
//!   poll makes the seat `Stalled`, every attached session is told once, `say`
//!   is refused while it lasts, and the loop keeps knocking on flowy's own
//!   backoff (1 s doubling to 30 s — most outages here are a deploy). Coming
//!   back is announced with how long the gap was. Nothing in that ever looks
//!   like a quiet room.
//! - **No reader is a refusal, not a declaration.** flowy's rule: a name that
//!   silently became a new reader is a typo that produces an inbox which is
//!   permanently empty. And the same sentence appears when the token has been
//!   switched. The seat stops and hands the sentence over; declaring is an
//!   explicit act, [`Seat::declare_reader`].
//! - **A re-mint stops the loop.** A waiter that resolves its credential once
//!   and polls for hours keeps succeeding under the old identity after the seat
//!   file changes — six and a half hours of that on 2026-08-18. The file is
//!   re-read every poll; a change is a stop with a reason.
//! - **One waiter per name** is the local claim ([`crate::waiter`]) plus the
//!   pid, start time and host on every poll, so `GET /api/presence` can name
//!   the process behind this listener rather than a command line.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime};

use letibot_tools::exec::monitor::{MAX_TTL, Monitors};

use crate::attention::{Attention, Identity, Level};
use crate::client::{Node, NodeError, WaiterProcess};
use crate::creds::{Credentials, read_token};
use crate::inbox::{Delivery, InboxCondition};
use crate::spool::Spool;
use crate::subs::{EntityChange, EntityWatch, Subscription};
use crate::waiter::{ClaimError, WaiterClaim};

/// Retry pacing for a node that went away — flowy's own numbers.
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// How much arrives while nobody is attached before the oldest is dropped. The
/// spool still has everything; this is what the next session is handed inline.
const BACKLOG_CAP: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeatState {
    /// Opened, not yet polled.
    Opening,
    Listening {
        since: String,
    },
    /// The node cannot be reached. The encoder is gone and the seat knows it.
    Stalled {
        since: String,
        last_error: String,
    },
    /// The loop ended and will not restart on its own.
    Stopped {
        why: String,
    },
}

impl SeatState {
    pub fn word(&self) -> String {
        match self {
            SeatState::Opening => "opening".into(),
            SeatState::Listening { since } => format!("listening since {since}"),
            SeatState::Stalled { since, last_error } => {
                format!("STALLED since {since}: {last_error}")
            }
            SeatState::Stopped { why } => format!("STOPPED: {why}"),
        }
    }

    pub fn is_listening(&self) -> bool {
        matches!(self, SeatState::Listening { .. })
    }
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub polls: u64,
    pub delivered: u64,
    pub server_skipped: i64,
    pub acks: u64,
    pub ack_failures: u64,
    pub last_cursor: i64,
    pub last_poll: Option<String>,
    pub last_node_now: String,
}

#[derive(Debug)]
pub enum SeatError {
    Claim(ClaimError),
    Spool(std::io::Error),
}

impl std::fmt::Display for SeatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SeatError::Claim(c) => write!(f, "{c}"),
            SeatError::Spool(e) => write!(f, "cannot open the spool: {e}"),
        }
    }
}

impl std::error::Error for SeatError {}

pub(crate) struct Shared {
    creds: Credentials,
    node: Mutex<Node>,
    me: Mutex<Identity>,
    spool: Spool,
    state: Mutex<SeatState>,
    attached: Mutex<Vec<Weak<InboxCondition>>>,
    backlog: Mutex<VecDeque<Delivery>>,
    backlog_since: Mutex<Option<String>>,
    stats: Mutex<Stats>,
    stop: AtomicBool,
    started: AtomicBool,
    renew: Mutex<Vec<(Weak<Monitors>, String)>>,
    _claim: Mutex<Option<WaiterClaim>>,
    entities: Arc<EntityWatch>,
    proc: WaiterProcess,
    /// The last board reading, for edge detection. See [`NagState`].
    nag: Mutex<NagState>,
}

/// The daemon's handle. Cheap to clone; the loop thread holds a `Weak`, so
/// dropping the last handle ends the loop within one poll window.
#[derive(Clone)]
pub struct Seat {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Seat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Seat({})", self.shared.creds.agent)
    }
}

impl Seat {
    /// Open a seat: take the waiter claim, open the spool. Does not touch the
    /// node — [`Seat::start`] does, and a node that is away at start is a stall,
    /// not a failure to open.
    ///
    /// `claim_dir` and `spool_dir` are for tests; `None` is the usual path.
    pub fn open(
        creds: Credentials,
        claim_dir: Option<&std::path::Path>,
        spool_dir: Option<&std::path::Path>,
    ) -> Result<Seat, SeatError> {
        let claim = match claim_dir {
            Some(d) => WaiterClaim::hold_in(d, &creds.agent),
            None => WaiterClaim::hold(&creds.agent),
        }
        .map_err(SeatError::Claim)?;
        let spool = Spool::for_reader(spool_dir, &creds.agent).map_err(SeatError::Spool)?;
        let node = Node::new(
            creds.endpoint.clone(),
            creds.addr.clone(),
            creds.token.clone(),
        );
        let proc = WaiterProcess {
            pid: std::process::id(),
            since: rfc3339_now(),
            host: hostname(),
        };
        let me = Identity {
            name: creds.agent.clone(),
            ..Default::default()
        };
        Ok(Seat {
            shared: Arc::new(Shared {
                entities: EntityWatch::new(node.clone()),
                nag: Mutex::new(NagState::default()),
                node: Mutex::new(node),
                creds,
                me: Mutex::new(me),
                spool,
                state: Mutex::new(SeatState::Opening),
                attached: Mutex::new(Vec::new()),
                backlog: Mutex::new(VecDeque::new()),
                backlog_since: Mutex::new(None),
                stats: Mutex::new(Stats::default()),
                stop: AtomicBool::new(false),
                started: AtomicBool::new(false),
                renew: Mutex::new(Vec::new()),
                _claim: Mutex::new(Some(claim)),
                proc,
            }),
        })
    }

    pub fn name(&self) -> &str {
        &self.shared.creds.agent
    }

    pub fn credentials(&self) -> &Credentials {
        &self.shared.creds
    }

    pub fn state(&self) -> SeatState {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn stats(&self) -> Stats {
        self.shared
            .stats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn identity(&self) -> Identity {
        self.shared
            .me
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn spool(&self) -> &Spool {
        &self.shared.spool
    }

    fn node(&self) -> Node {
        self.shared
            .node
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Whom the node says this token is. Fills the identity the attention
    /// tables compare against.
    pub fn whoami(&self) -> Result<Identity, NodeError> {
        let w = self.node().whoami()?;
        let me = Identity {
            user_id: w.user,
            agent_id: w.agent,
            name: self.shared.creds.agent.clone(),
        };
        *self.shared.me.lock().unwrap_or_else(|e| e.into_inner()) = me.clone();
        for c in self.attached_conditions() {
            c.set_identity(me.clone());
        }
        Ok(me)
    }

    /// The home project, for the attention focus: what `whoami` reports.
    pub fn home_project(&self) -> Result<String, NodeError> {
        Ok(self.node().whoami()?.project)
    }

    /// Declare the reader at the head of the log. Explicit, on purpose.
    pub fn declare_reader(&self) -> Result<crate::client::Reader, NodeError> {
        self.node().declare_reader(&self.shared.creds.agent)
    }

    /// Whether the reader exists, and where it stands.
    pub fn reader(&self) -> Result<Option<crate::client::Reader>, NodeError> {
        Ok(self
            .node()
            .readers()?
            .into_iter()
            .find(|r| r.reader == self.shared.creds.agent))
    }

    /// Attach a session: its own table, its own condition. What arrived while
    /// nobody was attached is offered first, labelled.
    pub fn attach(&self, session: &str, attention: Attention) -> Arc<InboxCondition> {
        self.attach_as(session, "", attention)
    }

    /// [`Seat::attach`] with an alias — the session's title — so the session can
    /// be addressed as `@seat/title` as well as `@seat/id`.
    pub fn attach_as(
        &self,
        session: &str,
        alias: &str,
        attention: Attention,
    ) -> Arc<InboxCondition> {
        let mut attention = attention;
        if attention.focus.is_none() {
            attention.focus = self
                .node()
                .whoami()
                .ok()
                .map(|w| w.project)
                .filter(|p| !p.is_empty());
        }
        let cond = InboxCondition::new(self.name(), session, self.identity(), attention);
        cond.set_alias(alias);
        {
            let mut att = self
                .shared
                .attached
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            att.retain(|w| w.upgrade().is_some());
            att.push(Arc::downgrade(&cond));
        }
        let backlog: Vec<Delivery> = {
            let mut b = self
                .shared
                .backlog
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            b.drain(..).collect()
        };
        if !backlog.is_empty() {
            let since = self
                .shared
                .backlog_since
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .unwrap_or_default();
            cond.offer(&Delivery::Notice(format!(
                "{} delivery(ies) arrived while no session was attached to `{}` (since {since}); \
                 they follow, oldest first",
                backlog.len(),
                self.name()
            )));
            cond.offer_all(&backlog);
        }
        cond
    }

    pub fn attached_conditions(&self) -> Vec<Arc<InboxCondition>> {
        self.shared
            .attached
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(|w| w.upgrade())
            .collect()
    }

    /// Ask the loop to renew a monitor by name each cycle, for as long as the
    /// registry lives. This is the explicit renewal T24 requires, made by the
    /// declarer — the daemon — which is alive for exactly as long as the seat.
    pub fn keep_renewing(&self, monitors: &Arc<Monitors>, name: &str) {
        self.shared
            .renew
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((Arc::downgrade(monitors), name.to_string()));
    }

    /// Say something, as the seat. Refused while stalled: sending into the void
    /// is the failure closed loop §5 names.
    ///
    /// `to` may name a session as well as a seat — `seat/session` — in which
    /// case the body is prefixed with the `@seat/session` tag the receiving
    /// daemon routes on, and the node is told `to: seat`. When the seat is THIS
    /// seat and the session is attached here, the message is also handed over
    /// **locally**: the node never delivers a seat's own messages back to it
    /// (`wakesFor`'s own-actor rule), so two sessions on one seat cannot hear
    /// each other through the inbox, and the room copy is the record.
    pub fn say(
        &self,
        room: &str,
        body: &str,
        to: Option<&str>,
        thread: Option<&str>,
    ) -> Result<crate::client::Event, NodeError> {
        self.refuse_if_stalled()?;
        let (to_seat, to_session) = match to {
            Some(t) => match t.trim_start_matches('@').split_once('/') {
                Some((seat, sess)) if !sess.is_empty() => {
                    (Some(seat.to_string()), Some(sess.to_string()))
                }
                _ => (Some(t.trim_start_matches('@').to_string()), None),
            },
            None => (None, None),
        };
        let tagged;
        let body = match (&to_seat, &to_session) {
            (Some(seat), Some(sess)) => {
                tagged = format!("@{seat}/{sess} {body}");
                tagged.as_str()
            }
            _ => body,
        };
        let sent = self.node().say(room, body, to_seat.as_deref(), thread)?;
        if let (Some(seat), Some(sess)) = (&to_seat, &to_session)
            && seat.eq_ignore_ascii_case(self.name())
        {
            match self
                .attached_conditions()
                .into_iter()
                .find(|c| c.is_addressed_as(sess))
            {
                Some(c) => {
                    c.offer(&Delivery::Direct(sent.clone()));
                }
                None => {
                    return Err(NodeError::Refused {
                        code: 0,
                        message: format!(
                            "posted to #{room} as the record, but no session called `{sess}` is \
                             attached to `{}` here, so nobody received it. Attached: {}",
                            self.name(),
                            self.attached_names()
                        ),
                    });
                }
            }
        }
        Ok(sent)
    }

    /// `id (alias)` for every attached session, for a refusal or a status line.
    pub fn attached_names(&self) -> String {
        let names: Vec<String> = self
            .attached_conditions()
            .iter()
            .map(|c| {
                let a = c.alias();
                if a.is_empty() {
                    c.session.clone()
                } else {
                    format!("{} ({a})", c.session)
                }
            })
            .collect();
        if names.is_empty() {
            "none".into()
        } else {
            names.join(", ")
        }
    }

    pub fn dm(
        &self,
        to: &str,
        body: &str,
        thread: Option<&str>,
    ) -> Result<crate::client::Event, NodeError> {
        self.refuse_if_stalled()?;
        self.node().dm(to, body, thread)
    }

    /// The fabric block for this seat: live, cached with its age, or
    /// unreachable — see [`crate::context`]. `None` in the first slot means no
    /// copy at all; the source says why.
    pub fn fabric(
        &self,
    ) -> (
        Option<crate::context::FabricContext>,
        crate::context::Source,
    ) {
        let project = self.identity_focus().unwrap_or_default();
        crate::context::FabricContext::read(&self.node(), self.name(), &project, None)
    }

    /// Rows of one kind, through the seat's node and token.
    pub fn node_artifacts_of_kind(
        &self,
        kind: &str,
        limit: usize,
    ) -> Result<Vec<crate::client::Artifact>, NodeError> {
        self.node().artifacts_of_kind(kind, limit)
    }

    pub fn node_artifact(&self, id: &str) -> Result<crate::client::Artifact, NodeError> {
        self.node().artifact(id)
    }

    pub fn room_read(
        &self,
        room: &str,
        limit: usize,
    ) -> Result<Vec<crate::client::Event>, NodeError> {
        self.node().room_read(room, limit)
    }

    fn refuse_if_stalled(&self) -> Result<(), NodeError> {
        match self.state() {
            SeatState::Stalled { since, last_error } => Err(NodeError::Unreachable(format!(
                "the seat has been stalled since {since} ({last_error}); not sending into the \
                 void — the message would look sent and reach nobody"
            ))),
            SeatState::Stopped { why } => Err(NodeError::Refused {
                code: 0,
                message: format!(
                    "the seat's listener has stopped ({why}); a reply from a seat that \
                                  cannot hear the answer is half a conversation"
                ),
            }),
            _ => Ok(()),
        }
    }

    /// Subscribe to an entity on behalf of a session's condition.
    pub fn subscribe(&self, cond: &InboxCondition, sub: Subscription) -> Result<String, NodeError> {
        let summary = self.shared.entities.add(&sub)?;
        cond.subscribe(sub);
        Ok(summary)
    }

    pub fn unsubscribe(&self, cond: &InboxCondition, sub: &Subscription) -> bool {
        let had = cond.unsubscribe(sub);
        if had {
            self.shared.entities.remove(sub);
        }
        had
    }

    pub fn entities(&self) -> &Arc<EntityWatch> {
        &self.shared.entities
    }

    /// Stop the loop. It notices at the end of the current poll window.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.entities.stop();
    }

    /// Start the loop thread. Idempotent.
    pub fn start(&self) {
        if self.shared.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak = Arc::downgrade(&self.shared);
        let sink_seat = Seat {
            shared: self.shared.clone(),
        };
        let sink_weak = Arc::downgrade(&sink_seat.shared);
        self.shared
            .entities
            .set_sink(Arc::new(move |c: EntityChange| {
                if let Some(s) = sink_weak.upgrade() {
                    Seat { shared: s }.fan_out(Delivery::Entity(c));
                }
            }));
        let _ = std::thread::Builder::new()
            .name(format!("letibot-flowy-{}", self.name()))
            .spawn(move || run(weak));
    }

    /// Fan one delivery out to every attached session, or into the backlog.
    fn fan_out(&self, d: Delivery) {
        self.fan_out_all(vec![d]);
    }

    /// Fan a page out: one ping per session for the whole page, so a page is one
    /// firing. Into the backlog when nobody is attached.
    ///
    /// **A message carrying `@seat/session` goes to that session and nobody
    /// else**, through no table — being named is the whole of the decision. The
    /// other sessions count it as gone past. A tag naming a session that is not
    /// attached here falls through to the tables like any addressed message, and
    /// whoever receives it sees the tag in the body.
    fn fan_out_all(&self, ds: Vec<Delivery>) {
        if ds.is_empty() {
            return;
        }
        let conds = self.attached_conditions();
        if conds.is_empty() {
            let mut b = self
                .shared
                .backlog
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut since = self
                .shared
                .backlog_since
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if since.is_none() {
                *since = Some(rfc3339_now());
            }
            for d in ds {
                if b.len() >= BACKLOG_CAP {
                    b.pop_front();
                }
                b.push_back(d);
            }
            return;
        }
        let mut per: Vec<Vec<Delivery>> = vec![Vec::new(); conds.len()];
        for d in ds {
            if let Delivery::Message(e) = &d
                && let Some(frag) = session_tag(&e.body, self.name())
                && let Some(i) = conds.iter().position(|c| c.is_addressed_as(&frag))
            {
                per[i].push(Delivery::Direct(e.clone()));
                for (j, c) in conds.iter().enumerate() {
                    if j != i {
                        c.note_addressed_elsewhere();
                    }
                }
                continue;
            }
            for p in per.iter_mut() {
                p.push(d.clone());
            }
        }
        for (c, list) in conds.iter().zip(per) {
            c.offer_all(&list);
        }
    }

    fn set_state(&self, s: SeatState) {
        *self.shared.state.lock().unwrap_or_else(|e| e.into_inner()) = s;
    }

    /// The loosest level any attached session wants; `addressed` when nobody is
    /// attached, so a backlog holds what named the seat and what people said.
    fn wire_level(&self) -> Level {
        let conds = self.attached_conditions();
        if conds.is_empty() {
            return Level::Addressed;
        }
        conds
            .iter()
            .map(|c| c.attention().loosest())
            .max()
            .unwrap_or(Level::Addressed)
    }

    fn focus(&self) -> Option<String> {
        // Every attached session's focus is the same seat's home project unless
        // one widened it to nothing — in which case the wire is widened too.
        let conds = self.attached_conditions();
        if conds.is_empty() {
            return self.identity_focus();
        }
        let mut focus: Option<String> = None;
        for c in conds {
            focus = Some(c.attention().focus?);
        }
        focus
    }

    fn identity_focus(&self) -> Option<String> {
        self.node()
            .whoami()
            .ok()
            .map(|w| w.project)
            .filter(|p| !p.is_empty())
    }

    /// One poll. Public so a test can drive the loop by hand.
    /// Sample the board and return a line when something CHANGED, else `None`.
    ///
    /// A nag is LEVEL-triggered — `mine_todo` stays 4 for as long as four rows
    /// are yours, and firecode's `board-nag.sh` paid for the rule that it must
    /// not be given a floor, because working is what turns it off. A poll loop
    /// that fanned the level out every window would say the same sentence every
    /// 20 seconds forever; one that suppressed it would be the silence this
    /// whole change exists to remove.
    ///
    /// So the level is kept and the EDGES are delivered: a line when an id
    /// appears in a bucket, and one line when a bucket empties. `None` while
    /// nothing moves, which is most of the time.
    fn sample_nag(&self) -> Option<String> {
        let nag = self.node().nag().ok()?;
        let mut last = self.shared.nag.lock().unwrap_or_else(|e| e.into_inner());
        last.diff(&nag)
    }

    pub fn poll_once(&self) -> PollOutcome {
        // A re-mint: the file changed under us.
        if let Some(file) = &self.shared.creds.token_file
            && let Ok(now) = read_token(file, None)
            && now != self.shared.creds.token
        {
            let why = format!(
                "seat `{}` was re-minted ({} changed); this listener was polling as the \
                 OLD identity and has stopped. Restart the daemon to listen as the new one",
                self.name(),
                file.display()
            );
            self.set_state(SeatState::Stopped { why: why.clone() });
            self.fan_out(Delivery::Notice(why));
            return PollOutcome::Stop;
        }
        let level = self.wire_level();
        let focus = self.focus();
        let page = self.node().inbox_wait(
            self.name(),
            level.server_filter(),
            focus.as_deref(),
            Some(&self.shared.proc),
        );
        let mut stats = self.shared.stats.lock().unwrap_or_else(|e| e.into_inner());
        stats.polls += 1;
        stats.last_poll = Some(rfc3339_now());
        drop(stats);
        match page {
            Ok(page) => {
                if let SeatState::Stalled { since, .. } = self.state() {
                    self.set_state(SeatState::Listening {
                        since: rfc3339_now(),
                    });
                    self.fan_out(Delivery::Notice(format!(
                        "reattached to {}; the node had been unreachable since {since}. \
                         Anything said meanwhile is in this delivery or the next",
                        self.shared.creds.addr
                    )));
                } else if !self.state().is_listening() {
                    self.set_state(SeatState::Listening {
                        since: rfc3339_now(),
                    });
                }
                if self.identity().agent_id.is_empty() {
                    let _ = self.whoami();
                }
                // Spool, THEN ack. A spool that cannot be written is a page that
                // was not delivered, and the node keeps it for the next poll.
                if let Err(e) = self.shared.spool.append(&page.events) {
                    return PollOutcome::Error(format!("spool: {e}; the page was not acked"));
                }
                let delivered = !page.events.is_empty();
                match self.node().inbox_ack(self.name(), page.cursor, delivered) {
                    Ok(_) => {
                        let mut st = self.shared.stats.lock().unwrap_or_else(|e| e.into_inner());
                        st.acks += 1;
                        st.last_cursor = page.cursor;
                    }
                    Err(e) => {
                        // Delivered anyway: the spool has it and the next poll
                        // re-reads from the old mark, which is a duplicate.
                        let mut st = self.shared.stats.lock().unwrap_or_else(|e| e.into_inner());
                        st.ack_failures += 1;
                        drop(st);
                        eprintln!("flowy seat {}: ack failed: {e}", self.name());
                    }
                }
                {
                    let mut st = self.shared.stats.lock().unwrap_or_else(|e| e.into_inner());
                    st.delivered += page.events.len() as u64;
                    st.server_skipped += page.skipped;
                    st.last_node_now = page.now.clone();
                }
                for c in self.attached_conditions() {
                    c.set_now(&page.now);
                    c.add_server_skipped(page.skipped);
                }
                let n = page.events.len();
                self.fan_out_all(page.events.into_iter().map(Delivery::Message).collect());
                // **Board state, in the same loop and the same stream.** Not a
                // second watcher: the operator's rule, 2026-09-15 — *"I don't
                // want that many watchers, or separate watchers for that matter
                // … I don't see any distinction between a chat message and a
                // todo update."* A seat is one poll loop and one queue; a row
                // assigned to you arrives beside what somebody said, through the
                // same `Condition`, and a session that is awake for one is awake
                // for the other. `inbox_wait` has just returned, so this is
                // sampled at most once per window and costs one cheap GET.
                if let Some(notice) = self.sample_nag() {
                    self.fan_out(Delivery::Notice(notice));
                }
                self.renew_monitors();
                PollOutcome::Delivered(n)
            }
            Err(NodeError::Unreachable(m)) => {
                if !matches!(self.state(), SeatState::Stalled { .. }) {
                    let since = rfc3339_now();
                    self.set_state(SeatState::Stalled {
                        since: since.clone(),
                        last_error: m.clone(),
                    });
                    self.fan_out(Delivery::Notice(format!(
                        "STALLED: {} is unreachable ({m}). You are not hearing the room and \
                         `say` is refused until it is back; the listener keeps trying",
                        self.shared.creds.addr
                    )));
                } else {
                    self.set_state(SeatState::Stalled {
                        since: match self.state() {
                            SeatState::Stalled { since, .. } => since,
                            _ => rfc3339_now(),
                        },
                        last_error: m,
                    });
                }
                self.renew_monitors();
                PollOutcome::Unreachable
            }
            Err(e @ NodeError::NoReader { .. }) => {
                let why = e.to_string();
                self.set_state(SeatState::Stopped { why: why.clone() });
                self.fan_out(Delivery::Notice(format!("listener stopped — {why}")));
                PollOutcome::Stop
            }
            Err(NodeError::Refused {
                code: 401 | 403,
                message,
            }) => {
                let why = format!("the node refused this token ({message})");
                self.set_state(SeatState::Stopped { why: why.clone() });
                self.fan_out(Delivery::Notice(format!("listener stopped — {why}")));
                PollOutcome::Stop
            }
            Err(e) => {
                self.renew_monitors();
                PollOutcome::Error(e.to_string())
            }
        }
    }

    fn renew_monitors(&self) {
        let mut list = self.shared.renew.lock().unwrap_or_else(|e| e.into_inner());
        list.retain(|(w, name)| match w.upgrade() {
            Some(m) => m.renew(name, MAX_TTL).is_ok(),
            None => false,
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    Delivered(usize),
    Unreachable,
    Error(String),
    Stop,
}

fn run(weak: Weak<Shared>) {
    let mut backoff = FIRST_BACKOFF;
    loop {
        let Some(shared) = weak.upgrade() else { return };
        if shared.stop.load(Ordering::SeqCst) {
            let seat = Seat { shared };
            seat.set_state(SeatState::Stopped {
                why: "the daemon stopped it".into(),
            });
            return;
        }
        let seat = Seat { shared };
        let started = Instant::now();
        let outcome = seat.poll_once();
        drop(seat);
        match outcome {
            PollOutcome::Delivered(_) => backoff = FIRST_BACKOFF,
            PollOutcome::Unreachable => {
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
            PollOutcome::Error(e) => {
                eprintln!("flowy seat: {e}");
                // A fast failure loop is the thing that looks like traffic.
                if started.elapsed() < Duration::from_secs(1) {
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
            PollOutcome::Stop => return,
        }
    }
}

/// The session fragment of an `@seat/session` tag in `body`, for THIS seat:
/// the bytes after `@seat/` up to the first byte the node's own mention parser
/// would not count as a name byte (letters, digits, `.`, `-`, `_`), with
/// trailing dots trimmed the way the node trims a name. `None` when the body
/// names no session of this seat. The seat name is matched ignoring case, as
/// the node matches a mention.
pub fn session_tag(body: &str, seat: &str) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    let needle = format!("@{}/", seat.to_ascii_lowercase());
    let mut from = 0;
    while let Some(pos) = lower[from..].find(&needle) {
        let at = from + pos;
        // The @ has to start a word, or this is somebody's email address.
        let starts_word = at == 0 || !is_name_byte(body.as_bytes()[at - 1]);
        let start = at + needle.len();
        let end = body[start..]
            .bytes()
            .position(|b| !is_name_byte(b))
            .map(|n| start + n)
            .unwrap_or(body.len());
        let frag = body[start..end].trim_end_matches('.');
        if starts_word && !frag.is_empty() {
            return Some(frag.to_string());
        }
        from = end.max(at + 1);
    }
    None
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_'
}

/// RFC3339 UTC, seconds. No `chrono`: this crate pulls nothing it does not need.
pub fn rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most len bytes into buf.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if rc != 0 {
        return "unknown".into();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_tag_is_read_the_way_the_node_reads_a_mention() {
        assert_eq!(
            session_tag("@seat/planner look at this", "seat"),
            Some("planner".into())
        );
        assert_eq!(session_tag("hey @Seat/s-17.", "seat"), Some("s-17".into()));
        assert_eq!(
            session_tag("@seat/s-1, and @seat/s-2", "seat"),
            Some("s-1".into())
        );
        assert_eq!(session_tag("mail@seat/x", "seat"), None);
        assert_eq!(session_tag("@seat alone", "seat"), None);
        assert_eq!(session_tag("@other/s-1", "seat"), None);
        assert_eq!(session_tag("@seat/", "seat"), None);
    }

    #[test]
    fn rfc3339_is_the_shape_the_node_prints() {
        let s = rfc3339_now();
        assert_eq!(s.len(), 20, "{s}");
        assert!(s.starts_with("20"), "{s}");
        assert!(s.ends_with('Z'));
        assert_eq!(&s[10..11], "T");
    }
}

/// The last board reading, so the poll loop can tell an edge from a level.
///
/// Four buckets, each `(count, ids)` from `GET /api/nag`. The ids are what make
/// this honest: a count going 4 → 4 can still mean one row finished and another
/// arrived, and a seat told "4 rows" twice would never learn about the second.
/// So the comparison is on the id SET, and the count only decorates the line.
///
/// `stale` is included and has `stale_ids` like the rest — checked against the
/// node rather than taken from a note that said it was the exception.
#[derive(Debug, Default)]
pub struct NagState {
    seen: std::collections::BTreeMap<&'static str, std::collections::BTreeSet<String>>,
    /// Nothing has been sampled yet, so the first reading is the baseline and
    /// its contents are reported as a standing total rather than as N arrivals.
    started: bool,
}

/// The buckets worth waking a seat for, and how a line about each one reads.
const BUCKETS: &[(&str, &str)] = &[
    ("mine_todo", "assigned to you"),
    ("answers_owed", "waiting on an answer from you"),
    ("stale", "stale"),
    ("mine_waiting", "yours, blocked"),
];

impl NagState {
    fn bucket<'a>(nag: &'a crate::client::Nag, key: &str) -> (i64, &'a [String]) {
        match key {
            "mine_todo" => (nag.mine_todo, &nag.mine_todo_ids),
            "answers_owed" => (nag.answers_owed, &nag.answers_owed_ids),
            "stale" => (nag.stale, &nag.stale_ids),
            "mine_waiting" => (nag.mine_waiting, &nag.mine_waiting_ids),
            _ => (0, &[]),
        }
    }

    /// The line to deliver, or `None` when nothing moved.
    pub fn diff(&mut self, nag: &crate::client::Nag) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        for (key, label) in BUCKETS {
            let (count, ids) = Self::bucket(nag, key);
            let now: std::collections::BTreeSet<String> = ids.iter().cloned().collect();
            let before = self.seen.entry(key).or_default();
            if !self.started {
                // Baseline: state what is true, once, without pretending it
                // just arrived.
                if count > 0 {
                    parts.push(format!("{count} {label}"));
                }
            } else {
                let new: Vec<&String> = now.difference(before).collect();
                if !new.is_empty() {
                    parts.push(format!(
                        "{} new {label} ({}){}",
                        new.len(),
                        new.iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                        if count as usize > new.len() {
                            format!("; {count} in that bucket now")
                        } else {
                            String::new()
                        }
                    ));
                } else if !before.is_empty() && now.is_empty() {
                    // The clear is an edge too, and the only one that says the
                    // nag is over. Without it a seat's last word on a bucket is
                    // the arrival.
                    parts.push(format!("nothing {label} any more"));
                }
            }
            *before = now;
        }
        let first = !self.started;
        self.started = true;
        if parts.is_empty() {
            return None;
        }
        Some(if first {
            format!("board: {}. `flowy nag` for the detail", parts.join(", "))
        } else {
            format!("board: {}", parts.join(", "))
        })
    }
}
