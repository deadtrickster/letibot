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
    /// The newest chat `seq` behind a queued note, acked to the room when a turn takes it.
    chat_seq: u64,
    /// Every chat `seq` behind a queued note, reported to the room when the turn they woke ends.
    seqs: Vec<u64>,
    /// A note other than a passing-by chat line is queued, so the wake is a full turn.
    full: bool,
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
    let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
    enqueue(&mut q, session, line);
    if let Some(entry) = q.map.get_mut(session) {
        entry.full = true;
    }
}

/// A chat message's note, remembered by its `seq` so the turn that takes it can tell the room.
/// Only a `Passing` note leaves the wake cheap.
pub fn push_chat(session: &str, seq: u64, reach: letibot_room::chat::Reach, line: String) {
    let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
    enqueue(&mut q, session, line);
    if let Some(entry) = q.map.get_mut(session) {
        entry.chat_seq = entry.chat_seq.max(seq);
        if seq > 0 && !entry.seqs.contains(&seq) && entry.seqs.len() < MAX_NOTES {
            entry.seqs.push(seq);
        }
        entry.full |= reach != letibot_room::chat::Reach::Passing;
    }
}

/// Note that chat up to `seq` was seen by a session that is not woken for it. With notes still
/// queued the seq rides on them, acked when a turn takes them; `false` when nothing is queued and
/// the caller acks now. Acked now, it would mark seen past a note no turn has taken.
pub fn seen_meanwhile(session: &str, seq: u64) -> bool {
    let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
    match q.map.get_mut(session) {
        Some(entry) if !entry.lines.is_empty() => {
            entry.chat_seq = entry.chat_seq.max(seq);
            true
        }
        _ => false,
    }
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
    let (lines, seq) = take_with_seq(session);
    if seq > 0 {
        super::link::ack_taken(session, seq);
    }
    lines
}

/// What a between-turns wake takes: the notes, and whether every one was passing by.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Taken {
    pub lines: Vec<String>,
    /// Nothing here directs the session, so the turn runs cheap and silence is its expected end.
    pub cheap: bool,
    /// The chat messages these notes carry.
    pub seqs: Vec<u64>,
}

/// [`take`] for `Harness::wake`, which also needs to know if the turn may run cheap.
pub fn take_wake(session: &str) -> Taken {
    let (taken, seq) = take_full(session);
    if seq > 0 {
        super::link::ack_taken(session, seq);
    }
    taken
}

/// [`take`] only when every queued note is passing by, for a cheap turn already running: an
/// addressed note stays queued, and the wake it rang runs a full turn after this one.
pub fn take_if_cheap(session: &str) -> Vec<String> {
    let taken = {
        let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
        if q.map.get(session).is_none_or(|e| e.full) {
            return Vec::new();
        }
        q.map.remove(session)
    };
    let Some(queue) = taken else {
        return Vec::new();
    };
    if queue.chat_seq > 0 {
        super::link::ack_taken(session, queue.chat_seq);
    }
    queue.lines.into_iter().collect()
}

/// [`take`] without the ack: the notes, and the newest chat `seq` among them (0 for none).
pub fn take_with_seq(session: &str) -> (Vec<String>, u64) {
    let (taken, seq) = take_full(session);
    (taken.lines, seq)
}

fn take_full(session: &str) -> (Taken, u64) {
    let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
    match q.map.remove(session) {
        Some(queue) => {
            if queue.dropped > 0 {
                eprintln!(
                    "harnessd: {} room note(s) for `{session}` were dropped while they waited",
                    queue.dropped
                );
            }
            let cheap = !queue.full && !queue.lines.is_empty();
            let lines = queue.lines.into_iter().collect();
            let seqs = queue.seqs;
            (Taken { lines, cheap, seqs }, queue.chat_seq)
        }
        None => (Taken::default(), 0),
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

/// Why a message woke this session. A digest summarises peers' talk, so it is passing by.
pub fn reach(m: &letibot_room::Message, names_you: bool) -> letibot_room::chat::Reach {
    if m.kind == letibot_room::Seat::System && m.author == letibot_room::chat::DIGEST_AUTHOR {
        return letibot_room::chat::Reach::Passing;
    }
    m.reach(names_you)
}

/// How many bytes of a passing or room-wide message's text a wake carries. `room read` has the
/// whole.
pub const CHAT_TEXT_MAX: usize = 600;

/// A DM between agents is meant whole for its reader, so it carries more, and still stops: one
/// agent cannot fill another's context. Its sender is warned when it posts over this.
pub const DM_TEXT_MAX: usize = 2_000;

/// What a wake carries of a message, by why it reached the reader: the operator's whole (the room
/// already caps it), a DM up to [`DM_TEXT_MAX`], anything else up to [`CHAT_TEXT_MAX`].
pub fn text_max(m: &letibot_room::Message, names_you: bool) -> usize {
    match m.reach(names_you) {
        letibot_room::chat::Reach::Operator => letibot_room::chat::MAX_TEXT,
        letibot_room::chat::Reach::Dm => DM_TEXT_MAX,
        _ => CHAT_TEXT_MAX,
    }
}

/// The note a chat message becomes for a session it wakes: a header saying why it reached you,
/// then the text quoted. `names_you` is the room's answer, not a reading of the text: the room
/// shows names, so a message for this session can name it by a handle its id does not match.
pub fn chat_sentence(m: &letibot_room::Message, names_you: bool) -> String {
    // A digest is the room's own summary, already bounded, so it is not quoted as a peer's text.
    if m.kind == letibot_room::Seat::System && m.author == letibot_room::chat::DIGEST_AUTHOR {
        return format!("#{} [{}] digest\n{}", m.seq, m.room, m.text);
    }
    m.delivered(names_you, text_max(m, names_you))
}

/// The note one room event becomes. `None` for an event a session is not woken by.
pub fn sentence(event: &letibot_room::Event) -> Option<String> {
    use letibot_room::Event;
    Some(match event {
        Event::Posted { item, tag, by } => format!(
            "[room] `{by}` posted #{item} under `{tag}`. `room claim {tag}` takes the head if free."
        ),
        Event::Claimed { item, tag, by } => {
            format!("[room] `{by}` claimed #{item}. `{tag}` is locked while the claim lives.")
        }
        Event::Done { item, tag, by } => {
            format!("[room] `{by}` finished #{item}. `{tag}` is free.")
        }
        Event::Abandoned {
            item,
            tag,
            by,
            why,
            count,
            back_in_the_queue,
        } => format!(
            "[room] #{item} under `{tag}` lost `{by}`: {why} ({count} lost). {}",
            if *back_in_the_queue {
                format!("Back at the head: `room claim {tag}`.")
            } else {
                "Frozen until the arbiter rules.".to_string()
            }
        ),
        Event::Raised {
            item,
            tag,
            question,
            count,
        } => format!(
            "[room] #{item} under `{tag}` lost {count} claimants and is frozen. Question \
             #{question} is yours: `room questions`, then `room rule question {question} decision \
             retry|hand|drop`."
        ),
        Event::Asked {
            item,
            tag,
            question,
        } => format!(
            "[room] a dispute on #{item} under `{tag}` is question #{question}, yours to rule: \
             `room questions`, then `room rule`."
        ),
        Event::Ruled {
            item,
            question,
            decision,
            by,
        } => {
            format!("[room] question #{question} on #{item} ruled by `{by}`: {decision}.")
        }
    })
}

/// The note a gap in the bell becomes. The room keeps no log, so it names the count and the
/// reads that answer as things stand.
pub fn missed_line(count: u64, since_ms: u64, now: u64) -> String {
    let gap = if since_ms == 0 || since_ms > now {
        String::new()
    } else {
        format!(
            ", first {} ago",
            letibot_room::duration(now.saturating_sub(since_ms))
        )
    };
    format!(
        "[room] the room connection was down: {count} event(s) for this session, not replayed{gap}. \
         Read the state: `room queue <tag>`, `room holding`, `room questions`. A claim held then \
         was given back."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unwoken message that arrives while a note waits is not acked past that note: its seq
    /// rides on the queue, and the turn that takes the note acks both.
    #[test]
    fn a_message_seen_meanwhile_waits_for_the_queued_note() {
        let me = "s-seen-meanwhile";
        assert!(
            !seen_meanwhile(me, 5),
            "nothing queued: the caller acks at once"
        );
        push_chat(me, 7, letibot_room::chat::Reach::Addressed, "a note".into());
        assert!(seen_meanwhile(me, 9), "a note waits: the seq rides on it");
        let (lines, seq) = take_with_seq(me);
        assert_eq!(lines.len(), 1);
        assert_eq!(seq, 9, "the take acks past both");
    }

    fn said(to: Option<&str>, text: &str) -> letibot_room::Message {
        letibot_room::Message {
            seq: 15,
            ts: 0,
            room: "host".into(),
            author: "operator".into(),
            kind: letibot_room::Seat::Operator,
            to: to.map(str::to_string),
            thread: None,
            refs: Vec::new(),
            attachments: Vec::new(),
            text: text.into(),
        }
    }

    /// The operator's text is never cut, an agent's DM stops at [`DM_TEXT_MAX`], and a line that
    /// only passes by stops at [`CHAT_TEXT_MAX`].
    #[test]
    fn what_a_wake_carries_depends_on_why_it_reached_you() {
        let long = "x".repeat(3_000);
        let op = chat_sentence(&said(Some("arango"), &long), true);
        assert!(!op.contains("bytes]"), "the operator is never cut");
        let mut dm = said(Some("arango"), &long);
        dm.kind = letibot_room::Seat::Session;
        dm.author = "s-peer".into();
        dm.room = letibot_room::chat::DM_ROOM.into();
        let got = chat_sentence(&dm, true);
        assert!(
            got.ends_with(" [+1000 bytes]"),
            "an agent's dm stops at 2000: {}",
            &got[got.len() - 40..]
        );
        let mut passing = said(None, &long);
        passing.kind = letibot_room::Seat::Session;
        passing.author = "s-peer".into();
        let got = chat_sentence(&passing, false);
        assert!(
            got.ends_with(" [+2400 bytes]"),
            "passing by stops at 600: {}",
            &got[got.len() - 40..]
        );
    }

    /// The room's `names_you` decides "you", not the text: it shows a session its own handle,
    /// and matching the id against it read a message for arango as someone else's.
    #[test]
    fn a_chat_note_says_who_it_is_for_from_the_rooms_answer() {
        let mine = chat_sentence(&said(Some("arango"), "two questions"), true);
        assert_eq!(
            mine,
            "#15 [host] from operator (operator) to you\n> two questions"
        );
        let mut peer = said(Some("s-b"), "status?");
        peer.kind = letibot_room::Seat::Session;
        peer.author = "s-c".into();
        let other = chat_sentence(&peer, false);
        assert!(other.ends_with("to @s-b, passing by\n> status?"), "{other}");
        let mut mention = said(None, "@cc@letibot see this");
        mention.kind = letibot_room::Seat::Session;
        let mention = chat_sentence(&mention, false);
        assert!(mention.contains("to @cc@letibot, passing by"), "{mention}");
        for line in [&mine, &other, &mention] {
            assert!(!line.contains("not you"), "{line}");
            assert!(
                !line.contains("end the turn"),
                "the rule is said once, not per note"
            );
        }
    }

    /// A wake is cheap only when every note it takes was passing by: one addressed note, one
    /// operator line or one queue event makes it a full turn.
    #[test]
    fn a_wake_is_cheap_only_when_every_note_was_passing_by() {
        use letibot_room::chat::Reach;
        let s = "s-cheap-wake";
        push_chat(s, 3, Reach::Passing, "a".into());
        push_chat(s, 4, Reach::Passing, "b".into());
        let (taken, seq) = take_full(s);
        assert!(taken.cheap, "{taken:?}");
        assert_eq!((taken.lines.len(), seq), (2, 4));
        assert_eq!(
            taken.seqs,
            vec![3, 4],
            "the turn reports every message it answers"
        );
        for reach in [Reach::Operator, Reach::Dm, Reach::Addressed] {
            push_chat(s, 5, Reach::Passing, "a".into());
            push_chat(s, 6, reach, "b".into());
            assert!(!take_full(s).0.cheap, "{reach:?} is a full turn");
        }
        push_chat(s, 7, Reach::Passing, "a".into());
        push(s, "[room] an event".into());
        assert!(!take_full(s).0.cheap, "a queue event is a full turn");
        assert!(!take_full(s).0.cheap, "nothing queued is no turn at all");
        // A cheap turn already running takes passing-by notes and leaves an addressed one.
        push_chat(s, 0, Reach::Passing, "a".into());
        assert_eq!(take_if_cheap(s), vec!["a".to_string()]);
        push_chat(s, 0, Reach::Addressed, "b".into());
        assert!(take_if_cheap(s).is_empty());
        assert_eq!(waiting(s), 1, "it waits for the full turn after");
        forget(s);
    }

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
