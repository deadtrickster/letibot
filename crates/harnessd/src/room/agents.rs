//! **One agent per tag, seated by the daemon that owns the room.**
//!
//! `docs/room.md` § *Slice 5* is the list this answers. The operator's design: *"one subsession per
//! tag, owned by the host"* — the box's owner is `s-room-<host key>` ([`letibot_room::coordinator_session`]),
//! and each tag's queue gets its own agent, named by derivation ([`letibot_room::tag_session`]) so a
//! person can reach it with `letibot --session host:ram`.
//!
//! # Three things this file is careful about
//!
//! **1. It cannot open a turn.** [`ensure_tag_agents`] takes a ledger, a registry and a list of
//! tags — and **no bell**. The only thing that turns a session into a model call is a ring
//! ([`crate::room::wake`]'s note plus `Bell::ring_wake`), and this function has nothing to ring
//! with. That is the operator's cost answer made structural rather than remembered: a claim, a
//! healthy beat and a finished item are comparisons the ROOM already made, and a comparison must
//! not cost a model call. A re-seat is the same: the session is seated, and its first turn happens
//! when something that earns one arrives.
//!
//! **2. It is idempotent, and the count is what makes that checkable.** A pass finds the agent
//! already there and moves nothing; a pass finds it gone and seats it again, and *that* is the one
//! thing [`letibot_room::AgentLedger`] counts. A tag whose agent keeps dying is a fact about that
//! tag — said in the pane, with the number — rather than a silent restart loop.
//!
//! **3. A tag with no live item is not given an agent.** The tags come from the room
//! ([`letibot_room::Server::live_tags`]), which is the only thing that knows which queues exist;
//! a finished tag's agent is left standing (its session is not closed here) but it is not
//! re-seated, because nothing is waiting for it.
//!
//! # What a re-seat is, and what it is not
//!
//! It is [`Registry::create_under`] on the derived id with the owner as the parent — the same door
//! a head's `NewSession` and the merge queue's gatekeeper use — after which the daemon's own worker
//! opens it (`Work::Open`). It is **not** a restart of a process: the tag's agent is a session in
//! the coordinator's daemon, so *dead* means *the registry no longer holds it* (the daemon restarted,
//! the session was stopped, or its open failed), and the remedy is to seat it again.
//!
//! `docs/room.md` § *Slice 5* names the comparison the brief asked for: the review queue re-seats a
//! reviewer when its child is not alive (`mergequeue::review::serve`), and the shape here is the
//! same — a liveness fact, a move, a count, and the count visible rather than swallowed.

use std::sync::Arc;

use letibot_room::{AgentLedger, TagAgent};
use letibot_sessionlog::registry::Registry;

/// **How often the tags are looked at.** Five seconds: a claim posted to a new tag is a session
/// seated within one tick, and the pass is three in-memory reads plus, at most, one `create_under`
/// per new tag. It is not a poll of the model, of a socket or of the store.
pub const UPKEEP_EVERY_MS: u64 = 5_000;

/// **What one pass did** — the sessions it seated, the ones it had to bring back, and the tags it
/// found no derivation for (a tag the room accepted but that has no agent, which must be said rather
/// than dropped on the floor).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pass {
    /// Tags whose agent was seated for the first time.
    pub seated: Vec<TagAgent>,
    /// Tags whose agent was gone and is back — the count in each one is the new total.
    pub reseated: Vec<TagAgent>,
    /// Tags the room has items under and this function could not name an agent for.
    pub unnamed: Vec<String>,
}

impl Pass {
    /// Did this pass change anything a person should be told about? A first seat is not news; a
    /// re-seat is.
    pub fn quiet(&self) -> bool {
        self.reseated.is_empty()
    }
}

/// **Make sure every tag with work in it has an agent** — one pass, no clock of its own, no bell.
///
/// `host_key` is this box's ([`letibot_room::host_key`]), passed rather than read so a test can
/// pin the derivation; `owner` is the box's owner session, which every tag's agent names as its
/// parent.
pub fn ensure_tag_agents(
    ledger: &AgentLedger,
    registry: &Arc<Registry>,
    host_key: &str,
    owner: &str,
    tags: &[String],
    now: u64,
) -> Pass {
    let mut pass = Pass::default();
    for tag in tags {
        let Some(session) = letibot_room::tag_session(host_key, tag) else {
            // The room accepted the tag, so this is a derivation that cannot spell it — said,
            // because a tag with items and no agent is a queue nobody is watching.
            pass.unnamed.push(tag.clone());
            continue;
        };
        // **Is it there?** The registry is the daemon's own answer to *do I hold this session*, and
        // it is the same question the queue asks before it re-seats a reviewer.
        if registry.holds(&session) {
            ledger.seat(tag, &session, now);
            continue;
        }
        // **Gone, or never there.** `of` answers which: a tag the ledger has never seen is being
        // seated, and one it has seen is coming back.
        let coming_back = ledger.of(tag).is_some();
        let wiring = registry.wiring(owner);
        let title = format!("`{tag}` — the room's agent for that queue");
        match registry.create_under(session.clone(), title, wiring, Some(owner.to_string())) {
            Ok(_) => {
                let entry = if coming_back {
                    ledger.reseat(tag, &session, now)
                } else {
                    ledger.seat(tag, &session, now)
                };
                if coming_back {
                    pass.reseated.push(entry);
                } else {
                    pass.seated.push(entry);
                }
            }
            // **Somebody seated it between the check and the create.** That is not a re-seat: the
            // agent never went away.
            Err(letibot_sessionlog::registry::CreateError::Exists(_)) => {
                ledger.seat(tag, &session, now);
            }
            // **A session this daemon cannot make** — said, and not counted as a re-seat, because
            // a number that climbs on a failure is a number that means nothing.
            Err(e) => {
                pass.unnamed.push(format!("{tag} ({e})"));
            }
        }
    }
    pass
}

/// **Start the pass on its own thread** — the coordinator's upkeep, and nothing else's.
///
/// Its first tick is a second away rather than immediate: a coordinator's first act is its lease
/// and its socket, and a tag's agent seated before the room can be read would be a session opened
/// for a queue that does not exist yet.
pub fn spawn(
    server: letibot_room::Server,
    registry: Arc<Registry>,
    host_key: String,
    owner: String,
    ledger: Arc<AgentLedger>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) {
    let _ = std::thread::Builder::new()
        .name("room-tag-agents".into())
        .spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(UPKEEP_EVERY_MS));
                if stop.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                let now = letibot_room::server::now_ms();
                let pass = ensure_tag_agents(
                    &ledger,
                    &registry,
                    &host_key,
                    &owner,
                    &server.live_tags(),
                    now,
                );
                for agent in &pass.reseated {
                    // **A re-seat is a notice, not a turn.** `eprintln!` is the daemon's log; the
                    // pane is where a person reads it (`room_feed`'s per-tag `agent` block), and
                    // the coordinator's own session is not woken by its own upkeep.
                    eprintln!(
                        "harnessd: re-seated the agent for `{}` ({}) — that is {} time(s) now",
                        agent.tag, agent.session, agent.reseats
                    );
                }
                for why in &pass.unnamed {
                    eprintln!("harnessd: no agent for {why}");
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_sessionlog::registry::SessionWiring;

    /// A registry and no daemon: `create_under` is the whole of what a seat is, and the worker's
    /// `Work::Open` — which is what actually opens the harness — is not this function's business.
    fn a_registry() -> Arc<Registry> {
        let r = Registry::new();
        r.create_under(
            "s-room-box-1a2b3c4d",
            "the box",
            SessionWiring::default(),
            None,
        )
        .expect("the owner");
        r
    }

    const KEY: &str = "box-1a2b3c4d";
    const OWNER: &str = "s-room-box-1a2b3c4d";

    #[test]
    fn a_tag_with_work_gets_one_agent_named_by_the_derivation() {
        let r = a_registry();
        let ledger = AgentLedger::new();
        let pass = ensure_tag_agents(&ledger, &r, KEY, OWNER, &["ram".to_string()], 1_000);
        assert_eq!(pass.seated.len(), 1);
        assert_eq!(pass.seated[0].session, "s-room-box-1a2b3c4d-ram");
        assert_eq!(pass.seated[0].reseats, 0);
        assert!(pass.quiet(), "a first seat is not a re-seat");
        // **It is a session, with the owner as its parent** — the *subsession* the operator asked
        // for, and the tree a picker draws.
        assert!(r.holds("s-room-box-1a2b3c4d-ram"));
        assert_eq!(r.parent_of("s-room-box-1a2b3c4d-ram").as_deref(), Some(OWNER));
    }

    #[test]
    fn a_second_pass_moves_nothing_and_the_count_stays_at_zero() {
        let r = a_registry();
        let ledger = AgentLedger::new();
        ensure_tag_agents(&ledger, &r, KEY, OWNER, &["ram".to_string()], 1_000);
        let pass = ensure_tag_agents(&ledger, &r, KEY, OWNER, &["ram".to_string()], 2_000);
        assert_eq!(pass, Pass::default(), "the agent was there");
        assert_eq!(ledger.of("ram").map(|a| a.reseats), Some(0));
        assert_eq!(
            ledger.of("ram").map(|a| a.since_ms),
            Some(1_000),
            "the clock is when it was seated, not when it was last looked at"
        );
    }

    #[test]
    fn a_tag_whose_agent_died_is_brought_back_and_the_count_says_so() {
        let r = a_registry();
        let ledger = AgentLedger::new();
        ensure_tag_agents(&ledger, &r, KEY, OWNER, &["ram".to_string()], 1_000);
        // The agent goes: the daemon restarted, the session was stopped, its open failed. The
        // registry is where the daemon reads that, and it is the only place that knows.
        r.remove("s-room-box-1a2b3c4d-ram");
        let pass = ensure_tag_agents(&ledger, &r, KEY, OWNER, &["ram".to_string()], 9_000);
        assert_eq!(pass.reseated.len(), 1, "a re-seat is news");
        assert!(!pass.quiet());
        assert_eq!(pass.reseated[0].reseats, 1);
        assert_eq!(pass.reseated[0].since_ms, 9_000);
        assert!(r.holds("s-room-box-1a2b3c4d-ram"), "and it is back");
        // A third death is counted as a third, because a tag something keeps killing is a fact
        // about the tag and not a loop to hide.
        r.remove("s-room-box-1a2b3c4d-ram");
        let pass = ensure_tag_agents(&ledger, &r, KEY, OWNER, &["ram".to_string()], 10_000);
        assert_eq!(pass.reseated[0].reseats, 2);
    }

    #[test]
    fn two_tags_are_two_agents_and_one_tag_is_never_two() {
        let r = a_registry();
        let ledger = AgentLedger::new();
        let tags = vec!["ram".to_string(), "disk".to_string(), "RAM".to_string()];
        ensure_tag_agents(&ledger, &r, KEY, OWNER, &tags, 1_000);
        assert!(r.holds("s-room-box-1a2b3c4d-ram"));
        assert!(r.holds("s-room-box-1a2b3c4d-disk"));
        assert_eq!(
            ledger.all().iter().map(|a| a.tag.as_str()).collect::<Vec<_>>(),
            vec!["disk", "ram"],
            "`RAM` and `ram` are one queue, so they are one agent"
        );
        // And the owner is not a tag's agent, however the list is spelled.
        assert_eq!(
            letibot_room::tag_of_session(KEY, OWNER),
            None,
            "the box's own session is nobody's subsession"
        );
    }

    #[test]
    fn a_tag_the_derivation_cannot_spell_is_said_rather_than_dropped() {
        let r = a_registry();
        let ledger = AgentLedger::new();
        // The room refuses an empty tag, so the only way here is a list from somewhere else —
        // and a tag with items and no agent must be said.
        let pass = ensure_tag_agents(&ledger, &r, KEY, OWNER, &["  ".to_string()], 1_000);
        assert_eq!(pass.unnamed, vec!["  ".to_string()]);
        assert!(pass.seated.is_empty());
    }
}
