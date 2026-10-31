//! **The queue between the room's reader thread and the turn that answers it.**
//!
//! A room event arrives on the connection's own thread — the one blocked on the socket — and the
//! turn that should act on it runs on the daemon's worker. Those are different threads by
//! construction (a `Harness` is `&mut` in one place), so the event has to become a *sentence* that
//! outlives the thread that heard it, and `Harness::wake` — which already collects a monitor's
//! firing and a job's settlement — has to find it there.
//!
//! That is all this is: a per-session list of sentences, written by whoever heard the bell, taken by
//! the turn that runs. It is process-wide rather than a field on the `Harness` for one reason: the
//! writer is a thread the harness does not own (the socket reader), and giving the harness a
//! `Mutex<Vec<String>>` would still leave that thread with no way to find the harness. The
//! alternative — a channel per session threaded through every constructor — buys nothing: the
//! daemon is one process and the key is the session id, which is unique within it.
//!
//! **A wake with nothing to say is not a wake.** `Harness::wake` returns `Ok(None)` when nothing
//! had settled, so the ring must follow the push: a ring with an empty list is a pass that finds
//! nothing and costs a `Harness::wake`'s reads (the note on `Bell::ring_wake` says the same thing
//! from the other end).

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

fn queues() -> &'static Mutex<BTreeMap<String, Vec<String>>> {
    static QUEUES: OnceLock<Mutex<BTreeMap<String, Vec<String>>>> = OnceLock::new();
    QUEUES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// **Put a sentence in front of a session's next turn.**
pub fn push(session: &str, line: String) {
    let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
    q.entry(session.to_string()).or_default().push(line);
}

/// **Take everything waiting for this session** — what `Harness::wake` composes into its notices.
pub fn take(session: &str) -> Vec<String> {
    let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
    q.remove(session).unwrap_or_default()
}

/// How many sentences are waiting for a session — the assertion a test makes instead of sleeping.
pub fn waiting(session: &str) -> usize {
    let q = queues().lock().unwrap_or_else(|e| e.into_inner());
    q.get(session).map(|v| v.len()).unwrap_or(0)
}

/// **The sentence one room event becomes**, in the words a model reads. `None` for an event a
/// session does not need to be woken by.
///
/// The rule is `docs/room.md`'s: `Posted`, `Claimed`, `Done`, `Abandoned`, `Raised`, `Asked` and
/// `Ruled` are worth ringing about, and **a beat is not** — a bell that rings five times a minute
/// is a bell nobody listens to. The room does not even send a beat (it produces no event for one),
/// so this is the second half of a rule that is already true on the wire.
pub fn sentence(event: &letibot_room::Event) -> Option<String> {
    use letibot_room::Event;
    Some(match event {
        Event::Posted { item, tag, by } => format!(
            "[room] `{by}` posted #{item} under `{tag}` — the queue is `room queue {tag}`, and \
             `room claim {tag}` takes its head if it is free."
        ),
        Event::Claimed { item, tag, by } => format!(
            "[room] `{by}` claimed #{item} under `{tag}` — while that claim lives, `{tag}` is \
             locked and your own claim under it would be refused. Post under another tag if your \
             work does not need the same resource."
        ),
        Event::Done { item, tag, by } => {
            format!("[room] `{by}` finished #{item} under `{tag}` — the tag is free again.")
        }
        Event::Abandoned {
            item,
            tag,
            by,
            why,
            count,
            back_in_the_queue,
        } => format!(
            "[room] #{item} under `{tag}` lost its claimant `{by}`: {why} (that is {count} \
             claimant(s) lost). {}",
            if *back_in_the_queue {
                format!("It is back at the head of `{tag}` — `room claim {tag}` takes it.")
            } else {
                "It has reached the threshold and is frozen until the arbiter rules.".to_string()
            }
        ),
        Event::Raised {
            item,
            tag,
            question,
            count,
        } => format!(
            "[room] #{item} under `{tag}` has now lost {count} claimants and is frozen: question \
             #{question} is yours to rule on — `room questions` has the words and the facts, and \
             `room rule question {question} decision retry|hand|drop` answers it."
        ),
        Event::Asked {
            item,
            tag,
            question,
        } => format!(
            "[room] a dispute about #{item} under `{tag}` is question #{question}, and it is yours \
             to rule on: `room questions`, then `room rule`."
        ),
        Event::Ruled {
            item,
            question,
            decision,
            by,
        } => {
            format!("[room] question #{question} about #{item} was answered by `{by}`: {decision}.")
        }
    })
}

/// **The sentence a gap in the bell becomes** — firecode's second rule, *one read is not
/// membership*, applied to the one case a room can be honest about.
///
/// The room keeps no log, so it cannot say what it rang for while a session was away; what it can
/// say is how many times it rang and when the first was (`wire`'s version-4 note). The sentence
/// therefore names the count, names the READS that cannot be wrong, and says in words that nothing
/// will be replayed — a client told *you missed four* and left to guess which four would believe it
/// had the picture, and the whole point of a bell is that a session does not have to poll to know
/// it does not.
///
/// `since_ms` is the room's clock (`server::now_ms`), and `now` is passed in rather than read so the
/// sentence can be asserted instead of matched loosely.
pub fn missed_line(count: u64, since_ms: u64, now: u64) -> String {
    let gap = if since_ms == 0 || since_ms > now {
        String::new()
    } else {
        format!(
            "the first of them was {} ago; ",
            letibot_room::duration(now.saturating_sub(since_ms))
        )
    };
    format!(
        "[room] this session's connection to the room was down, and {count} event(s) were addressed \
         to it while it was — {gap}they are not replayed, because the room keeps no log. Read the \
         state instead of the bells: `room queue <tag>`, `room holding` and `room questions` answer \
         as things stand, and a claim this session held was given back when its socket closed."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sentence_per_event_and_a_beat_has_none() {
        use letibot_room::{Decision, Event, Why};
        let events = [
            Event::Posted {
                item: 1,
                tag: "ram".into(),
                by: "s-a".into(),
            },
            Event::Claimed {
                item: 1,
                tag: "ram".into(),
                by: "s-a".into(),
            },
            Event::Done {
                item: 1,
                tag: "ram".into(),
                by: "s-a".into(),
            },
            Event::Abandoned {
                item: 1,
                tag: "ram".into(),
                by: "s-a".into(),
                why: Why::BeatStale { age_ms: 90_001 },
                count: 1,
                back_in_the_queue: true,
            },
            Event::Raised {
                item: 1,
                tag: "ram".into(),
                question: 1,
                count: 3,
            },
            Event::Asked {
                item: 1,
                tag: "ram".into(),
                question: 2,
            },
            Event::Ruled {
                item: 1,
                question: 1,
                decision: Decision::Retry,
                by: "s-host".into(),
            },
        ];
        for e in &events {
            let s = sentence(e).unwrap_or_else(|| panic!("no sentence for {e:?}"));
            assert!(s.starts_with("[room] "), "{s}");
        }
    }

    #[test]
    fn a_push_is_taken_once_and_only_by_its_own_session() {
        let (a, b) = ("s-room-test-a", "s-room-test-b");
        push(a, "one".into());
        push(a, "two".into());
        assert_eq!(waiting(a), 2);
        assert_eq!(waiting(b), 0, "a bell is addressed");
        assert_eq!(take(a), vec!["one".to_string(), "two".to_string()]);
        assert_eq!(take(a), Vec::<String>::new(), "taken is taken");
    }
}
