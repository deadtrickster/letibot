//! **The room: one host's agents, one queue per tag, a claim that is a lease with a beat — and no
//! transport at all.**
//!
//! `docs/room.md` is the long form (the wire, what is idempotent, what the coordinator daemon still
//! owes); this is the map.
//!
//! # What this crate is, and what it deliberately is not
//!
//! The operator's brief: *"we have to be able to let all agents running in letibot communicate on a
//! single host for starters … this chat should come with a tagged priority queue — queue per tag
//! essentially … once agent that claimed the queue item dies (or goes offline) that item is marked
//! as such and queue moves on. Just like with todo top level session get reminds (with ability to
//! mark waits_on) so they dont occupy it forever."*
//!
//! The first answer was a table in `sessions.db`. The operator's second answer replaced it:
//! *"id still do it as a socket tho, we have a dedicated harnessd instance that autostarts and has
//! a tie breaker agent … and then we gain a wasm plugin for that server — raw session of the tie
//! breaker, queues, etc, with the manual controls"*. So:
//!
//! * **[`Room`] is the state machine** — the item lifecycle, the claim as a lease with a beat, the
//!   abandonment with a reason and a count, top-level-sessions-only, the ordering per tag, and the
//!   reminder question. No store, no socket, no clock: every time is handed in, and the whole of
//!   the transport is [`wire`].
//! * **[`wire`] is the protocol** a coordinator's socket would take and send — proposed here, with
//!   the protocol version, and with the idempotency rules a reconnecting client needs. It is types
//!   and a dispatch ([`Room::answer`]), not a server.
//! * **The coordinator daemon is the next slice** — the autostarted harnessd instance, the socket,
//!   the tie-breaker agent in a session, the wake, and the wasm pane. `docs/room.md` § *What the
//!   daemon still owes* is the list, so the next person does not re-derive it.
//!
//! # The four decisions, in one place
//!
//! 1. **A tag is a lock.** One live claim per tag; everybody else is refused by name while it
//!    lives. That is the RAM example's shape, and it is why a "priority" is not needed.
//! 2. **Order is `(posted_ms, id)` — first come, first served.** No priority number: a number
//!    invites every session to claim 0, and urgent work gets its own tag (`ram-urgent`), which is
//!    a name a reader can see. An abandoned item returns to the head, because it was posted first.
//! 3. **A claim is a lease with a beat**, [`DEFAULT_LEASE_MS`] (the merge queue's ninety seconds).
//!    It is lost to a stale beat ([`Room::sweep`]) or to a session the daemon reports gone
//!    ([`Room::note_gone`]), and the abandonment is loud: an [`Event::Abandoned`] with the reason
//!    and the count, and a state that says `abandoned` rather than hiding the item back in the
//!    queue as if nothing had happened.
//! 4. **Three abandonments raise the item** ([`ABANDON_THRESHOLD`]) instead of cycling: it freezes,
//!    its tag's queue moves past it, and a [`Question`] is asked of the tie-breaker, whose three
//!    answers are [`Decision::Retry`], [`Decision::Hand`] and [`Decision::Drop`]. See [`room`]'s
//!    header for the other two situations that need a ruling, and for why two coordinators is not
//!    one of them.
//!
//! # Top-level sessions only
//!
//! A [`Speaker`] carries the parent it speaks through, and every verb refuses a subagent by name
//! ([`Refused::Subagent`]). A subagent's words and claims go through the session that spawned it —
//! the operator's rule and flowy's. The daemon fills `parent` from `session.parent_session_id`;
//! the room never sees the store.

pub mod agent;
pub mod client;
pub mod item;
pub mod lease;
pub mod restart;
pub mod room;
pub mod server;
pub mod wire;

pub use agent::{AgentLedger, TagAgent};
pub use item::{Abandonment, Claim, Item, Speaker, State, Wait, Why, duration};
pub use lease::{
    Acquired, COORDINATOR_LEASE_TTL_MS, CoordinatorLease, Held, Holder, LEASE_BEAT_EVERY_MS,
    Takeover, coordinator_session, host_key, host_name, lease_path, named_session, pid_alive,
    socket_path, tag_of_session, tag_session,
};
pub use restart::{Cause, Marker, marker_path, read_marker, room_running, room_started_ms};
pub use room::{
    ABANDON_THRESHOLD, About, DEFAULT_BEAT_EVERY_MS, DEFAULT_LEASE_MS, Decision, Event, Facts,
    Holding, Outcome, Question, REPLAY_WINDOW, Refused, Reminder, Room, Ruling, Waiting,
    normalise_tag,
};
pub use server::Server;
pub use wire::{Answer, ROOM_PROTOCOL_VERSION, Request, Response};
