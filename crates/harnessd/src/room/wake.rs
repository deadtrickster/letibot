//! The queue between the room's reader thread and the turn that answers it. The event arrives on
//! the thread blocked on the socket and the turn runs on the daemon's worker, so it becomes a
//! sentence that outlives its hearer - process-wide, keyed by the session id, unique in a daemon.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, OnceLock};

/// How many notes one session's queue holds. A note is a sentence of a few hundred bytes, so sixty
/// four of them is the memory one session's listener may hold when nothing is taking them.
pub const MAX_NOTES: usize = 64;

/// How many sessions have a queue at once. A daemon's sessions are tens, and a closed one gives its
/// entry back; the bound is here so a name that is never closed again cannot grow the map.
pub const MAX_SESSIONS: usize = 256;

/// One session's notes, and the order in which they were pushed - what an eviction needs, without a
/// clock two pushes in the same millisecond would share.
#[derive(Debug, Default)]
struct Queue {
    lines: VecDeque<String>,
    pushed: u64,
    /// Notes the bound took: said on the log rather than cut in silence.
    dropped: u64,
}

/// The process-wide queues and the counter that orders them.
#[derive(Debug, Default)]
struct Queues {
    map: BTreeMap<String, Queue>,
    next: u64,
}

fn queues() -> &'static Mutex<Queues> {
    static QUEUES: OnceLock<Mutex<Queues>> = OnceLock::new();
    QUEUES.get_or_init(Default::default)
}

/// Put a sentence in front of a session's next turn.
pub fn push(session: &str, line: String) {
    enqueue(
        &mut queues().lock().unwrap_or_else(|e| e.into_inner()),
        session,
        line,
    );
}

/// The push, given the queues it goes into - so a test can prove the bounds without filling the
/// process's own map.
fn enqueue(queues: &mut Queues, session: &str, line: String) {
    queues.next += 1;
    let pushed = queues.next;
    if !queues.map.contains_key(session)
        && queues.map.len() >= MAX_SESSIONS
        && let Some((gone, was)) = queues
            .map
            .iter()
            .min_by_key(|(_, q)| q.pushed)
            .map(|(k, q)| (k.clone(), q.lines.len()))
    {
        queues.map.remove(&gone);
        eprintln!(
            "harnessd: dropping {was} room note(s) queued for `{gone}`; the wake queue holds \
             {MAX_SESSIONS} sessions"
        );
    }
    let entry = queues.map.entry(session.to_string()).or_default();
    entry.pushed = pushed;
    entry.lines.push_back(line);
    while entry.lines.len() > MAX_NOTES {
        entry.lines.pop_front();
        entry.dropped += 1;
        if entry.dropped == 1 {
            eprintln!(
                "harnessd: `{session}` already has {MAX_NOTES} untaken room note(s); the oldest \
                 are dropped - the room's reads have the state"
            );
        }
    }
}

/// Take everything waiting for this session - what `Harness::wake` composes into its notices.
pub fn take(session: &str) -> Vec<String> {
    let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
    match q.map.remove(session) {
        Some(queue) => {
            if queue.dropped > 0 {
                eprintln!(
                    "harnessd: {} room note(s) for `{session}` were dropped while they waited",
                    queue.dropped
                );
            }
            queue.lines.into_iter().collect()
        }
        None => Vec::new(),
    }
}

/// Forget a session's queue. A session that has closed will take no turn, so its notes are for
/// nobody - and an entry nothing removes is the memory of every session a daemon ever held.
pub fn forget(session: &str) -> usize {
    let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
    q.map
        .remove(session)
        .map(|queue| queue.lines.len())
        .unwrap_or(0)
}

/// How many sentences are waiting for a session - the assertion a test makes instead of sleeping.
pub fn waiting(session: &str) -> usize {
    let q = queues().lock().unwrap_or_else(|e| e.into_inner());
    q.map.get(session).map(|v| v.lines.len()).unwrap_or(0)
}

/// The sentence a chat message that names a session becomes. Bounded: a long message is cut for
/// the turn's notice, and `room read` has the whole.
pub fn chat_sentence(m: &letibot_room::Message) -> String {
    // A digest is already bounded by the room, and says what it left out.
    if m.kind == letibot_room::Seat::System && m.author == letibot_room::chat::DIGEST_AUTHOR {
        return format!("[room] {}", m.text);
    }
    let mut line = m.line();
    if line.len() > 600 {
        let mut end = 600;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        line.truncate(end);
        line.push_str(" ...");
    }
    format!("[chat] {line} - answer with `room say` (room `host` or `project`, optional `to`).")
}

/// The sentence one room event becomes, in the words a model reads. `None` for an event a
/// session does not need to be woken by. A beat is not worth a ring - a bell that rings five
/// times a minute is a bell nobody listens to, and the room sends no beat event anyway.
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

/// The sentence a gap in the bell becomes - the room keeps no log, so it can only say how many
/// events it rang for. The sentence names the count and the READS that cannot be wrong: a client
/// told *you missed four* and left to guess which four would believe it had the picture.
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

    /// One session's queue is bounded, and what the bound takes is the OLDEST note: the newest is
    /// what the next turn needs, and every read the room answers is still current.
    #[test]
    fn one_sessions_queue_is_bounded_from_the_front() {
        let mut queues = Queues::default();
        for i in 0..(MAX_NOTES * 2) {
            enqueue(&mut queues, "s-a", format!("note {i}"));
        }
        let q = queues.map.get("s-a").expect("a queue");
        assert_eq!(q.lines.len(), MAX_NOTES);
        assert_eq!(q.dropped as usize, MAX_NOTES, "the rest were dropped");
        assert_eq!(q.lines.front().map(String::as_str), Some("note 64"));
        assert_eq!(
            q.lines.back().map(String::as_str),
            Some("note 127"),
            "the newest is kept"
        );
        // And another session's queue is nobody else's business.
        assert_eq!(queues.map.get("s-b").map(|q| q.lines.len()), None);
    }

    /// The map is bounded too, and the session evicted is the one pushed longest ago - which is the
    /// one whose listener is not turning anything into a turn.
    #[test]
    fn the_number_of_sessions_queued_is_bounded() {
        let mut queues = Queues::default();
        for i in 0..MAX_SESSIONS {
            enqueue(&mut queues, &format!("s-{i}"), format!("note {i}"));
        }
        assert_eq!(queues.map.len(), MAX_SESSIONS);
        // A push to an existing session refreshes it, so the next eviction picks another.
        enqueue(&mut queues, "s-0", "again".into());
        enqueue(&mut queues, "s-past-the-bound", "new".into());
        assert_eq!(queues.map.len(), MAX_SESSIONS, "the map did not grow");
        assert!(
            queues.map.contains_key("s-0"),
            "the one just pushed is not the one forgotten"
        );
        assert!(
            !queues.map.contains_key("s-1"),
            "the least recently pushed is"
        );
        assert!(queues.map.contains_key("s-past-the-bound"));
    }

    /// A closed session takes no turn, so its queue goes: an entry nothing removes is the memory of
    /// every session the daemon ever held.
    #[test]
    fn forgetting_a_session_drops_what_it_never_took() {
        let (gone, kept) = ("s-room-forget-a", "s-room-forget-b");
        push(gone, "one".into());
        push(gone, "two".into());
        push(kept, "three".into());
        assert_eq!(forget(gone), 2, "the notes nobody will take");
        assert_eq!(waiting(gone), 0);
        assert_eq!(take(gone), Vec::<String>::new());
        assert_eq!(waiting(kept), 1, "somebody else's queue is untouched");
        assert_eq!(forget(kept), 1);
    }
}
