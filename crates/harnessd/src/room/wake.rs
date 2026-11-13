//! The room's side of the delivery door: the reader thread composes a sentence per event, and it
//! waits here - process-wide, keyed by the session id - until the door that says the session's held
//! notices takes it. Nothing here decides when a line is said, and nothing here drops one.

use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use crate::delivery::RoomLine;

/// How many room lines one session's pen keeps as lines. Past it the OLDEST are folded into one
/// digest that names every `seq` and points at the room's own read, so a session that fell far
/// behind loses nothing and can still fetch the text.
pub const HELD_ROOM_MAX: usize = 64;

/// How many sessions have a queue at once. A daemon's sessions are tens, and a closed one gives its
/// entry back; the bound is here so a name that is never closed again cannot grow the map.
pub const MAX_SESSIONS: usize = 256;

/// The lines the bound folded away, oldest first - what the digest line is made of.
#[derive(Debug, Default)]
struct Folded {
    seqs: Vec<u64>,
    /// How many lines were folded, chat or queue event.
    count: usize,
    /// When the oldest of them was queued, so the digest takes that line's place in arrival order.
    first: Option<SystemTime>,
}

impl Folded {
    fn add(&mut self, line: &RoomLine) {
        self.first.get_or_insert(line.arrived);
        self.count += 1;
        if line.seq > 0 {
            self.seqs.push(line.seq);
        }
    }
}

/// One session's lines, and the order in which they were pushed - what the fold needs, without a
/// clock two pushes in the same millisecond would share.
#[derive(Debug, Default)]
struct Queue {
    lines: VecDeque<RoomLine>,
    folded: Folded,
    pushed: u64,
    /// The newest chat `seq` behind a line here, acked once a turn carries it.
    chat_seq: u64,
    /// A line other than a passing-by chat line is queued, so the wake is a full turn.
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

/// Put a sentence in front of a session's next turn - a queue event, a gap, a restart note.
pub fn push(session: &str, line: String) {
    let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
    enqueue(&mut q, session, note(&line, 0, false));
    if let Some(entry) = q.map.get_mut(session) {
        entry.full = true;
    }
}

/// A chat message's line, remembered by its `seq` so the turn that carries it can tell the room.
/// Only a `Passing` line leaves the wake cheap.
pub fn push_chat(session: &str, seq: u64, reach: letibot_room::chat::Reach, line: String) {
    let passing = reach == letibot_room::chat::Reach::Passing;
    let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
    enqueue(&mut q, session, note(&line, seq, passing));
    if let Some(entry) = q.map.get_mut(session) {
        entry.chat_seq = entry.chat_seq.max(seq);
        entry.full |= !passing;
    }
}

fn note(line: &str, seq: u64, passing: bool) -> RoomLine {
    RoomLine {
        seq,
        stands_for: Vec::new(),
        line: line.to_string(),
        arrived: SystemTime::now(),
        passing,
    }
}

/// Note that chat up to `seq` was seen by a session that is not woken for it. With lines still
/// queued the seq rides on them, acked when a turn carries them; `false` when nothing is queued and
/// the caller acks now. Acked now, it would mark seen past a line no turn has taken.
pub fn seen_meanwhile(session: &str, seq: u64) -> bool {
    let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
    match q.map.get_mut(session) {
        Some(entry) if !entry.lines.is_empty() || entry.folded.first.is_some() => {
            entry.chat_seq = entry.chat_seq.max(seq);
            true
        }
        _ => false,
    }
}

/// The push, given the queues it goes into - so a test can prove the bound without filling the
/// process's own map.
fn enqueue(queues: &mut Queues, session: &str, line: RoomLine) {
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
    fold(entry);
}

/// Keep at most [`HELD_ROOM_MAX`] lines: the overflow is summarised, never dropped. The digest
/// names every `seq` it folded, because the session can always fetch the text with the room's read.
fn fold(entry: &mut Queue) {
    while entry.lines.len() > HELD_ROOM_MAX {
        if let Some(gone) = entry.lines.pop_front() {
            entry.folded.add(&gone);
        }
    }
}

/// What the bound folded, as the one line that stands in for it - `None` when nothing was folded.
fn digest(entry: &Queue) -> Option<RoomLine> {
    let first = entry.folded.first?;
    let seqs: Vec<String> = entry.folded.seqs.iter().map(|s| format!("#{s}")).collect();
    let which = match seqs.is_empty() {
        true => String::new(),
        false => format!(": {}", seqs.join(", ")),
    };
    let text = format!(
        "[room] {} earlier room line(s) are held for you{which}. None were dropped: read them all \
         with `room read` (`room_read` from Claude Code), and this turn goes on with the ones below.",
        entry.folded.count,
    );
    Some(RoomLine {
        seq: entry.folded.seqs.first().copied().unwrap_or(0),
        stands_for: entry.folded.seqs.clone(),
        line: text,
        arrived: first,
        passing: true,
    })
}

/// Everything waiting for this session, in the order it arrives - the door's take. It does not ack:
/// a line is acknowledged when a turn has it in front of the model, see [`ack_through`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Taken {
    /// The digest first when something was folded, then the lines themselves.
    pub lines: Vec<RoomLine>,
    /// Nothing here directs the session, so the turn runs cheap and silence is its expected end.
    pub cheap: bool,
    /// The chat messages these lines carry.
    pub seqs: Vec<u64>,
    /// The newest chat `seq` behind them, for the turn that carries them to ack.
    pub ack: u64,
}

impl Taken {
    /// The sentences, for a caller that only wants to say them.
    pub fn text(&self) -> Vec<String> {
        self.lines.iter().map(|l| l.line.clone()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// Take everything waiting, without acking it.
pub fn hold(session: &str) -> Taken {
    let taken = {
        let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
        q.map.remove(session)
    };
    match taken {
        Some(entry) => collect(entry),
        None => Taken::default(),
    }
}

/// A queue taken, as the door hands it over: the digest first when something was folded, then the
/// lines, and every chat `seq` any of them carries - so nothing a line names is left unreported.
fn collect(entry: Queue) -> Taken {
    let digest = digest(&entry);
    let mut seqs = entry.folded.seqs.clone();
    seqs.extend(entry.lines.iter().filter(|l| l.seq > 0).map(|l| l.seq));
    // A message can be queued twice by two rooms' readers; the report is of messages, not lines.
    let mut unique: Vec<u64> = Vec::new();
    for seq in seqs {
        if !unique.contains(&seq) {
            unique.push(seq);
        }
    }
    let mut lines: Vec<RoomLine> = digest.into_iter().collect();
    let cheap = !entry.full && !(lines.is_empty() && entry.lines.is_empty());
    lines.extend(entry.lines);
    Taken {
        // A digest is passing-by talk too, so it does not make the turn a full one.
        cheap,
        lines,
        seqs: unique,
        ack: entry.chat_seq,
    }
}

/// Tell the room that a session has seen chat up to `seq` - the one ack, from the door that has a
/// turn carrying those lines, or a session that took none.
pub fn ack_through(session: &str, seq: u64) {
    if seq > 0 {
        super::link::ack_taken(session, seq);
    }
}

/// Hand back lines a turn never opened: they go in front, in the order they were taken, and the
/// bound folds them again if they overflow it. Nothing is acked, so the room still has them unseen.
pub fn put_back(session: &str, lines: Vec<RoomLine>) {
    if lines.is_empty() {
        return;
    }
    let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
    let entry = q.map.entry(session.to_string()).or_default();
    for line in lines.into_iter().rev() {
        // What the take would have said about the batch is restored with it: the newest seq an ack
        // names, and whether a line other than passing-by talk is in it.
        entry.chat_seq = entry.chat_seq.max(line.seq);
        entry.full |= !line.passing;
        entry.lines.push_front(line);
    }
    fold(entry);
}

/// Take everything waiting and tell the room it was taken - the pre-door callers' take, kept for
/// the tests that assert a take is a take. The harness's own doors use [`hold`] and then ack.
pub fn take(session: &str) -> Vec<String> {
    let (lines, seq) = take_with_seq(session);
    ack_through(session, seq);
    lines
}

/// [`take`] without the ack split out: the sentences and the newest chat `seq` among them.
pub fn take_with_seq(session: &str) -> (Vec<String>, u64) {
    let taken = hold(session);
    (taken.text(), taken.ack)
}

/// [`take`] for a caller that also needs to know if the turn may run cheap.
pub fn take_wake(session: &str) -> Taken {
    let taken = hold(session);
    ack_through(session, taken.ack);
    taken
}

/// [`hold`] only when every queued line is passing by, for a cheap turn already running: an
/// addressed line stays queued, and the wake it rang runs a full turn after this one.
pub fn take_if_cheap(session: &str) -> Taken {
    let taken = {
        let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = q.map.get(session) else {
            return Taken::default();
        };
        if entry.full {
            return Taken::default();
        }
        q.map.remove(session)
    };
    let Some(entry) = taken else {
        return Taken::default();
    };
    collect(entry)
}

/// The file a closing session's untaken notes are kept in, under the directory its other logs are
/// in.
pub const KEPT: &str = "room-notes.jsonl";

/// One note as the file holds it: the sentence, the chat message it was - every one of them, since
/// a folded note stands for many - and when it arrived.
#[derive(serde::Serialize, serde::Deserialize)]
struct Kept {
    seq: u64,
    seqs: Vec<u64>,
    arrived_ms: u64,
    line: String,
}

/// Empty a closing session's queue into `dir`, keeping every line it never took. The queue is
/// memory and a closed session takes no turn, so without this the lines go with the memory; they
/// are that session's record, so they are filed under its own logs instead.
pub fn keep(session: &str, dir: &std::path::Path) -> usize {
    let taken = hold(session);
    if taken.is_empty() {
        return 0;
    }
    let mut buf = String::new();
    for line in &taken.lines {
        let kept = Kept {
            seq: line.seq,
            seqs: line.seqs(),
            arrived_ms: line
                .arrived
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            line: line.line.clone(),
        };
        match serde_json::to_string(&kept) {
            Ok(text) => {
                buf.push_str(&text);
                buf.push('\n');
            }
            Err(e) => eprintln!("room: a note of {session} not kept ({e})"),
        }
    }
    let path = dir.join(KEPT);
    let wrote = std::fs::create_dir_all(dir).and_then(|()| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(buf.as_bytes()))
    });
    match wrote {
        Ok(()) => taken.lines.len(),
        Err(e) => {
            // Said, and the memory is already gone: a note nobody can read is worth the line.
            eprintln!(
                "  {session} · closed; {} untaken room note(s) could not be kept ({}: {e})",
                taken.lines.len(),
                path.display()
            );
            0
        }
    }
}

/// The notes a closed session kept under `dir`, oldest first - what a person reads after it went.
pub fn kept(dir: &std::path::Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(dir.join(KEPT)) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|line| serde_json::from_str::<Kept>(line).ok())
        .map(|kept| kept.line)
        .collect()
}

/// How many sentences are waiting for a session - the assertion a test makes instead of sleeping.
pub fn waiting(session: &str) -> usize {
    let q = queues().lock().unwrap_or_else(|e| e.into_inner());
    q.map
        .get(session)
        .map(|v| v.lines.len() + usize::from(v.folded.first.is_some()))
        .unwrap_or(0)
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

    /// A session's queue taken and dropped, for a test that wants it gone and not said: a CLOSED
    /// session's notes are kept, see [`keep`], so nothing in the daemon reaches for this.
    fn discard(session: &str) -> usize {
        let mut q = queues().lock().unwrap_or_else(|e| e.into_inner());
        q.map
            .remove(session)
            .map(|entry| entry.lines.len() + entry.folded.count)
            .unwrap_or(0)
    }

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

    /// A wake is cheap only when every line it takes was passing by: one addressed line, one
    /// operator line or one queue event makes it a full turn.
    #[test]
    fn a_wake_is_cheap_only_when_every_note_was_passing_by() {
        use letibot_room::chat::Reach;
        let s = "s-cheap-wake";
        push_chat(s, 3, Reach::Passing, "a".into());
        push_chat(s, 4, Reach::Passing, "b".into());
        let taken = hold(s);
        assert!(taken.cheap, "{taken:?}");
        assert_eq!((taken.lines.len(), taken.ack), (2, 4));
        assert_eq!(
            taken.seqs,
            vec![3, 4],
            "the turn reports every message it answers"
        );
        for reach in [Reach::Operator, Reach::Dm, Reach::Addressed] {
            push_chat(s, 5, Reach::Passing, "a".into());
            push_chat(s, 6, reach, "b".into());
            assert!(!hold(s).cheap, "{reach:?} is a full turn");
        }
        push_chat(s, 7, Reach::Passing, "a".into());
        push(s, "[room] an event".into());
        assert!(!hold(s).cheap, "a queue event is a full turn");
        assert!(hold(s).is_empty(), "nothing queued is no take at all");
        // A cheap turn already running takes passing-by lines and leaves an addressed one.
        push_chat(s, 0, Reach::Passing, "a".into());
        assert_eq!(take_if_cheap(s).text(), vec!["a".to_string()]);
        push_chat(s, 0, Reach::Addressed, "b".into());
        assert!(take_if_cheap(s).is_empty());
        assert_eq!(waiting(s), 1, "it waits for the full turn after");
        discard(s);
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

    /// The old bound dropped the oldest sixty-four; the room's lines are never dropped now: past
    /// [`HELD_ROOM_MAX`] they are folded into ONE digest that names every `seq`, oldest first, and
    /// points at the read that has the text. Two hundred lines in, two hundred `seq`s out.
    #[test]
    fn two_hundred_queued_room_lines_lose_none_and_the_digest_names_every_seq() {
        let s = "s-room-digest";
        for seq in 1..=200u64 {
            push_chat(
                s,
                seq,
                letibot_room::chat::Reach::Passing,
                format!("line {seq}"),
            );
        }
        let taken = hold(s);
        assert_eq!(
            taken.lines.len(),
            HELD_ROOM_MAX + 1,
            "the digest and the newest {HELD_ROOM_MAX} lines"
        );
        let digest = &taken.lines[0];
        for seq in 1..=136u64 {
            assert!(digest.line.contains(&format!("#{seq}")), "{}", digest.line);
        }
        assert!(
            digest.line.contains("`room read`"),
            "the digest says where the text is: {}",
            digest.line
        );
        assert!(
            !digest.line.contains("#137"),
            "the lines kept are not also called folded: {}",
            digest.line
        );
        assert_eq!(
            taken.lines.last().map(|l| l.line.as_str()),
            Some("line 200"),
            "the newest line is the last of the take"
        );
        // Nothing is lost: every seq rides out, in order, and the ack is past all of them.
        assert_eq!(taken.seqs, (1..=200).collect::<Vec<u64>>());
        // The digest carries the seqs it stands for, so the door's facts and the wake's report count
        // the 136 lines it took the place of rather than the one line it is.
        assert_eq!(digest.seqs(), (1..=136).collect::<Vec<u64>>());
        assert_eq!(taken.lines[1].seqs(), vec![137]);
        assert_eq!(taken.ack, 200);
        assert!(
            taken.cheap,
            "a folded digest is peers' talk, not instructions"
        );
        assert_eq!(waiting(s), 0, "nothing is left queued");
    }

    /// A turn that never opened hands its lines back, in order, and nothing was acked: the room
    /// still has them unseen and the next door takes them again.
    #[test]
    fn a_failed_turn_hands_its_lines_back_unacked() {
        let s = "s-room-put-back";
        push_chat(s, 1, letibot_room::chat::Reach::Passing, "one".into());
        push_chat(s, 2, letibot_room::chat::Reach::Addressed, "two".into());
        let taken = hold(s);
        assert_eq!(taken.text(), vec!["one".to_string(), "two".to_string()]);
        put_back(s, taken.lines.clone());
        assert_eq!(waiting(s), 2);
        let again = hold(s);
        assert_eq!(
            again.text(),
            vec!["one".to_string(), "two".to_string()],
            "the order they were taken in"
        );
        assert_eq!(again.ack, 2);
        discard(s);
    }

    /// The map is bounded too, and the session evicted is the one pushed longest ago - which is the
    /// one whose listener is not turning anything into a turn.
    #[test]
    fn the_number_of_sessions_queued_is_bounded() {
        let mut queues = Queues::default();
        for i in 0..MAX_SESSIONS {
            enqueue(
                &mut queues,
                &format!("s-{i}"),
                note(&format!("note {i}"), 0, false),
            );
        }
        assert_eq!(queues.map.len(), MAX_SESSIONS);
        // A push to an existing session refreshes it, so the next eviction picks another.
        enqueue(&mut queues, "s-0", note("again", 0, false));
        enqueue(&mut queues, "s-past-the-bound", note("new", 0, false));
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

    /// A session's queue goes when it is dropped, and one session's going leaves another's alone:
    /// the broom a test has, not a rule the daemon keeps.
    #[test]
    fn a_discarded_queue_is_gone_and_another_sessions_is_untouched() {
        let (gone, kept) = ("s-room-discard-a", "s-room-discard-b");
        push(gone, "one".into());
        push(gone, "two".into());
        push(kept, "three".into());
        assert_eq!(discard(gone), 2, "both of its notes");
        assert_eq!(waiting(gone), 0);
        assert_eq!(take(gone), Vec::<String>::new());
        assert_eq!(waiting(kept), 1, "somebody else's queue is untouched");
        assert_eq!(discard(kept), 1);
    }

    /// The bar: a session closes with two notes it never took, and they are readable under its own
    /// logs afterwards - the queue is memory, the file is the record.
    #[test]
    fn a_closing_session_keeps_the_notes_it_never_took() {
        let s = "s-room-keep";
        let dir = std::env::temp_dir().join(format!("letibot-room-kept-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        push(s, "one".into());
        push_chat(s, 12, letibot_room::chat::Reach::Addressed, "two".into());
        assert_eq!(keep(s, &dir), 2, "both notes are filed");
        assert_eq!(waiting(s), 0, "and the queue is empty");
        assert_eq!(
            kept(&dir),
            vec!["one".to_string(), "two".to_string()],
            "readable, in the order they arrived"
        );
        // A session that took its notes keeps nothing, and a closed one's file is appended to, not
        // rewritten: the second closing leaves the first record in place.
        assert_eq!(keep("s-room-keep-none", &dir), 0);
        push(s, "three".into());
        assert_eq!(keep(s, &dir), 1);
        assert_eq!(kept(&dir).len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
