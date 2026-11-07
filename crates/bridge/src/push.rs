//! A held session's events as channel notifications: what `leticode-wait` told an orchestrator,
//! pushed, so a Claude Code session needs no waiter of its own.
//!
//! A notification is recorded in a per-session outbox and leaves it only when a client takes it,
//! so an event that happens while nobody is reading - a client between connections, a bridge
//! restarted - is said once when the channel comes back, and never twice.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::head::{Attention, Entry, Head, LimitNews};

/// How long one `wait` blocks before the watcher looks again. It must outlast
/// [`crate::head::START_GRACE`]: a `wait` that starts on a session which is ALREADY quiet answers
/// `Timeout` until that grace is over, so a shorter tick would never report a turn end that did not
/// begin during the wait. Three seconds of wait plus a second's sleep was the old loop's cadence.
const TICK: Duration = Duration::from_secs(4);

/// How many keys the outbox remembers as said. A key is what stops the same fact being said
/// twice; past this many the oldest fall off, and only a client away for longer than this many
/// distinct events can be told one of them a second time.
const SAID_KEEP: usize = 256;

/// Most ring entries one pass reads: a backlog drains over the next passes instead of firing a
/// burst at one turn.
const PER_PASS: usize = 32;

/// Longest turn-end text a notification carries; `leticode_transcript` has the rest.
const MAX_MESSAGE: usize = 1500;

/// One notification waiting for the client: the key that makes it news, and the frame.
#[derive(Debug, Clone, PartialEq)]
struct Record {
    seq: u64,
    key: String,
    note: Value,
}

/// What one session's channel knows: the keys it has said, the records a client has not taken,
/// and how far it has been told. In memory for this run and on disk so a bridge restarted on the
/// same folder resumes instead of repeating itself or falling silent.
#[derive(Debug, Default)]
struct Kept {
    next: u64,
    /// The `seq` of the last record a client took: the cursor this channel is a follower of.
    cursor: u64,
    said: VecDeque<String>,
    pending: VecDeque<Record>,
}

/// A held session's channel: the records, the client they go to, and the place they are kept.
///
/// One per session and not one per client. A watcher records whether or not anybody is reading,
/// which is the whole of how an event that happens during a reconnect survives it - the record
/// waits, and [`Outbox::attach`] is what a client that comes back is handed.
#[derive(Debug)]
pub struct Outbox {
    path: PathBuf,
    kept: Mutex<Kept>,
    sink: Mutex<Option<Sender<Value>>>,
    /// Whether an unkeepable file has been said once, so it is not said every pass.
    told: AtomicBool,
}

impl Outbox {
    /// The channel kept at `path`, picking up whatever a previous run left there.
    pub fn at(path: impl Into<PathBuf>) -> Arc<Outbox> {
        let path = path.into();
        let kept = read(&path);
        Arc::new(Outbox {
            path,
            kept: Mutex::new(kept),
            sink: Mutex::new(None),
            told: AtomicBool::new(false),
        })
    }

    /// Record `note` as this event, `false` when the key has already been said or is waiting.
    /// The identity is the key, not the frame: a state re-read on the next pass is not news.
    pub fn record(&self, key: impl Into<String>, note: Value) -> bool {
        let key = key.into();
        let fresh = {
            let mut g = self.kept.lock().unwrap_or_else(|e| e.into_inner());
            if g.said.contains(&key) {
                false
            } else {
                g.next += 1;
                let seq = g.next;
                g.said.push_back(key.clone());
                while g.said.len() > SAID_KEEP {
                    g.said.pop_front();
                }
                g.pending.push_back(Record { seq, key, note });
                self.keep(&g);
                true
            }
        };
        if fresh {
            self.deliver();
        }
        fresh
    }

    /// Point the channel at a client. Nothing leaves before this, and whatever was recorded
    /// while there was nobody goes now, oldest first.
    pub fn attach(&self, tx: Sender<Value>) {
        *self.sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        self.deliver();
    }

    /// How far the client has been told: the `seq` of the last record it took.
    pub fn cursor(&self) -> u64 {
        self.kept.lock().unwrap_or_else(|e| e.into_inner()).cursor
    }

    /// Records recorded and not yet taken: what a client that reconnects is owed.
    pub fn waiting(&self) -> usize {
        self.kept
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .len()
    }

    /// Hand the client every record it is owed, and stop at the first it cannot take.
    fn deliver(&self) {
        let mut sink = self.sink.lock().unwrap_or_else(|e| e.into_inner());
        let Some(tx) = sink.clone() else { return };
        let mut g = self.kept.lock().unwrap_or_else(|e| e.into_inner());
        while let Some(front) = g.pending.front() {
            if tx.send(front.note.clone()).is_err() {
                // The client is gone. The record stays for the next one and the watcher goes on
                // recording, which is what makes a reconnect say what it missed.
                *sink = None;
                return;
            }
            g.cursor = front.seq;
            g.pending.pop_front();
            self.keep(&g);
        }
    }

    /// Move the place on in the file. A place this process cannot keep costs a restart its
    /// memory of what it said and nothing else, so it is said once and never retried per pass.
    fn keep(&self, g: &Kept) {
        if write(&self.path, g) || self.told.swap(true, Ordering::Relaxed) {
            return;
        }
        eprintln!(
            "  push: {} cannot be written, so a restart of this bridge may repeat what it said",
            self.path.display()
        );
    }
}

/// Watch `head`, returning the outbox its events are recorded in; the caller attaches the client
/// it has. One watcher per session: a client that attaches again calls [`Outbox::attach`], because
/// a second watcher on one head would record every event twice.
pub fn watch(head: &Arc<Head>) -> Arc<Outbox> {
    let outbox = Outbox::at(outbox_path(head));
    let weak = Arc::downgrade(head);
    let recording = outbox.clone();
    let _ = std::thread::Builder::new()
        .name(format!("leticode-push-{}", head.session))
        .spawn(move || run(weak, recording));
    outbox
}

/// Where a session's channel is kept: one file per folder and session, so a bridge restarted on
/// the same folder finds what its client has been told. Named by the folder for `queue`'s
/// reason - a restart of that folder's bridge is the one that has to find it.
pub fn outbox_path(head: &Head) -> PathBuf {
    crate::daemon::rundir().join(format!(
        "{}.push-{}",
        crate::daemon::key(&head.worktree),
        head.session
    ))
}

fn run(weak: Weak<Head>, outbox: Arc<Outbox>) {
    // The ring's head at the first pass: a job that settled before this bridge held the session
    // is history, and `leticode_events` is where history is read.
    let mut job_cursor = match weak.upgrade() {
        Some(head) => head.events(0, crate::head::RING).next,
        None => return,
    };
    let mut last_limit = 0u64;
    loop {
        let Some(head) = weak.upgrade() else {
            return;
        };
        let attention = head.wait(Instant::now() + TICK);
        let word = head.word();
        let (jobs, rows) = head.open_work();
        let page = head.events(job_cursor, PER_PASS);
        // The limit is reported before the attention, and from its own counter: a session that
        // was parked and resumed is quiet, and a `wait` would answer with the turn rather than
        // with the provider's clock - `limited until X`, `switched to Y`, `resumed`.
        let (limits, news) = crate::head::limit_news(&head);
        let limit = news
            .filter(|_| limits != last_limit)
            .map(|n| limit_event(&head, &n));
        last_limit = limits;
        let event = event(&head, &attention);
        let who = format!("leticode `{}` ({})", head.session, head.worktree.display());
        let session = head.session.clone();
        drop(head);
        if let Some((key, note)) = limit {
            outbox.record(key, note);
            continue;
        }
        if let Some((key, note)) = event {
            outbox.record(key, note);
        }
        job_cursor = page.next;
        // A ring entry that fell off before this pass is not said: `leticode_events` is where a
        // reader asks for a hole's shape, and one notification per lost line is a flood.
        for e in page
            .entries
            .iter()
            .filter(|e| e.kind == "job" && e.state == "ended")
        {
            let (key, note) = job_event(&who, &session, e);
            outbox.record(key, note);
        }
        // Between turns with work still open is the state an orchestrator has no other event
        // for: the turn ended, the jobs did not, and nothing will wake `wait` again.
        if word == "idle"
            && let Some((key, note)) = idle_event(&who, &session, &jobs, rows)
        {
            outbox.record(key, note);
        }
    }
}

/// A limit's channel notification: `limited until X`, `switched to Y`, or the daemon's own words
/// for a resume. Keyed by the fact itself, so one park is one message.
pub fn limit_event(head: &Head, news: &LimitNews) -> (String, Value) {
    let who = format!("leticode `{}` ({})", head.session, head.worktree.display());
    let line = news.line();
    let key = format!("limit:{}:{line}", news.kind);
    let content = match news.kind {
        "resumed" => format!("{who} resumed: {line}"),
        _ => format!("{who} {line}"),
    };
    let meta = json!({
        "source": "leticode",
        "session": head.session,
        "event": news.kind,
    });
    (key, notification(content, meta))
}

/// A session between turns with work still open: the jobs the daemon still lists and how many
/// todo rows nobody has closed. Keyed by that work, so the same work is said once and a change
/// to it - a job ending, a row written - is said again, which is the whole of "once per change".
pub fn idle_event(
    who: &str,
    session: &str,
    jobs: &[String],
    rows: usize,
) -> Option<(String, Value)> {
    if jobs.is_empty() && rows == 0 {
        return None;
    }
    let mut said = String::new();
    if !jobs.is_empty() {
        said.push_str(&format!(
            "job{} {}",
            if jobs.len() == 1 { "" } else { "s" },
            jobs.join(", ")
        ));
    }
    if rows > 0 {
        if !said.is_empty() {
            said.push_str("; ");
        }
        said.push_str(&format!(
            "{rows} open todo row{}",
            if rows == 1 { "" } else { "s" }
        ));
    }
    let key = format!("idle:{}:{rows}", jobs.join(","));
    let meta = json!({
        "source": "leticode",
        "session": session,
        "event": "idle_with_work",
        "jobs": jobs,
        "rows": rows,
    });
    Some((
        key,
        notification(format!("{who} is idle with work open: {said}"), meta),
    ))
}

/// A held job's exit: the state the daemon settled it with and how long it ran. Keyed by the job
/// and its state, so one settlement is one message however often the ring is re-read.
pub fn job_event(who: &str, session: &str, e: &Entry) -> (String, Value) {
    let state = if e.text.is_empty() {
        "settled".to_string()
    } else {
        e.text.clone()
    };
    let ran = match e.ms {
        Some(ms) => format!(" after {}", span(ms)),
        None => String::new(),
    };
    let key = format!("job-end:{}:{state}", e.name);
    let meta = json!({
        "source": "leticode",
        "session": session,
        "event": "job_ended",
        "job": e.name,
        "state": state,
    });
    (
        key,
        notification(
            format!("{who}'s job `{}` ended: {state}{ran}", e.name),
            meta,
        ),
    )
}

/// A SEAT's own job ending (`seat_job_start`): the same frame a held session's job change uses,
/// with `source: seat` so one client reads one kind of event and `meta.event` says which. Keyed by
/// the job and its state, so one settlement is one message.
pub fn seat_job_event(seat: &str, job: &letibot_tokencore::store::SeatJob) -> (String, Value) {
    let state = if job.state.is_empty() {
        "settled".to_string()
    } else {
        job.state.clone()
    };
    let ran = match job.elapsed_ms {
        0 => String::new(),
        ms => format!(" after {}", span(ms)),
    };
    let key = format!("seat-job-end:{}:{state}", job.handle);
    let meta = json!({
        "source": "seat",
        "event": "job_ended",
        "seat": seat,
        "job": job.handle,
        "state": state,
        "log": job.log,
    });
    (
        key,
        notification(
            format!("your job `{}` ended: {state}{ran}", job.handle),
            meta,
        ),
    )
}

/// `40m 12s`, `9s`, `4h 3m`: how long a job ran, in the two units a reader cares about.
fn span(ms: u64) -> String {
    let secs = ms / 1000;
    let (h, m, s) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// The event's identity (what makes it news) and its notification. `None` for a timeout.
pub fn event(head: &Head, attention: &Attention) -> Option<(String, Value)> {
    // `leticode_stop` closes the session through the daemon, and the goodbye that arrives behind
    // its ack is that close: the bridge's own act, not a failure of the session's to report.
    if head.stopped_by_bridge()
        && let Attention::Failed { why } = attention
        && why.starts_with("the daemon closed this connection")
    {
        return None;
    }
    let who = format!("leticode `{}` ({})", head.session, head.worktree.display());
    let (kind, key, content) = match attention {
        Attention::Timeout => return None,
        Attention::TurnEnded { message } => (
            "turn_ended",
            // The turn's own id, so two turns that ended with the same words are two events and a
            // head that re-attached re-derives one. A daemon that minted none falls back to the
            // words, which is what the channel said before there was an id to read.
            match head.turn_id() {
                id if id.is_empty() => format!("end:{message}"),
                id => format!("end:{id}"),
            },
            format!(
                "{who} ended its turn:\n{}",
                cut(message.trim(), MAX_MESSAGE)
            ),
        ),
        Attention::Ask(a) => (
            "ask",
            format!("ask:{}", a.req_id),
            format!(
                "{who} asks ({}): {} {} - answer with `leticode_answer`; `leticode_asks` lists \
                 the options.",
                a.kind, a.tool, a.summary
            ),
        ),
        Attention::Failed { why } => (
            "failed",
            format!("failed:{why}"),
            format!("{who} failed: {why}"),
        ),
        Attention::Trouble(t) => (
            "trouble",
            format!("trouble:{}:{}:{:?}", t.kind, t.count, t.reset_ms),
            // A limit with an instant on it is not a failure and not weather: the session acts
            // again at that time, or has already moved to another model until it.
            match (t.reset_ms, t.switched_to.as_deref()) {
                (Some(at), Some(to)) => format!(
                    "{who} is limited on {} until {}, and switched to {to}: {}",
                    t.model,
                    crate::head::utc_clock(at),
                    t.detail
                ),
                (Some(at), None) => format!(
                    "{who} is limited on {} until {}, and resumes by itself: {}",
                    t.model,
                    crate::head::utc_clock(at),
                    t.detail
                ),
                (None, _) => format!(
                    "{who} is stuck on its provider ({} on {}, {} in a row): {}",
                    t.kind, t.model, t.count, t.detail
                ),
            },
        ),
    };
    let meta = json!({
        "source": "leticode",
        "session": head.session,
        "event": kind,
    });
    Some((key, notification(content, meta)))
}

/// One channel notification: the sentence a client reads, and the meta it sorts by.
fn notification(content: String, meta: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/claude/channel",
        "params": { "content": content, "meta": meta },
    })
}

/// `s` cut to at most `max` bytes on a char boundary, saying that it was cut.
fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{} ... (cut; `leticode_transcript` has the rest)",
        &s[..end]
    )
}

/// What a previous run of this bridge left at `path`, `Kept::default()` when there is nothing
/// readable there: a channel with no memory says what it sees and repeats nothing it cannot
/// know about.
fn read(path: &Path) -> Kept {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Kept::default();
    };
    let Ok(doc) = serde_json::from_str::<Value>(&text) else {
        return Kept::default();
    };
    let mut kept = Kept {
        next: doc["next"].as_u64().unwrap_or(0),
        cursor: doc["cursor"].as_u64().unwrap_or(0),
        ..Kept::default()
    };
    for key in doc["said"].as_array().into_iter().flatten() {
        if let Some(key) = key.as_str() {
            kept.said.push_back(key.to_string());
        }
    }
    for r in doc["pending"].as_array().into_iter().flatten() {
        let seq = r["seq"].as_u64().unwrap_or(0);
        // A record at or below the cursor was taken before whatever killed the last run: the
        // file was written after the send, and a replay of it would be the duplicate to avoid.
        if seq <= kept.cursor {
            continue;
        }
        kept.next = kept.next.max(seq);
        kept.pending.push_back(Record {
            seq,
            key: r["key"].as_str().unwrap_or_default().to_string(),
            note: r["note"].clone(),
        });
    }
    kept
}

/// Write the place through a temp name and a rename, so a bridge killed mid-write leaves the old
/// one rather than half of a new one. `false` is a place this process could not keep.
fn write(path: &Path, g: &Kept) -> bool {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let doc = json!({
        "next": g.next,
        "cursor": g.cursor,
        "said": g.said.iter().collect::<Vec<_>>(),
        "pending": g.pending.iter().map(|r| json!({
            "seq": r.seq,
            "key": r.key,
            "note": r.note,
        })).collect::<Vec<_>>(),
    });
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = path.with_file_name(format!("{name}.tmp{}", std::process::id()));
    if std::fs::write(&tmp, doc.to_string()).is_err() {
        return false;
    }
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::mpsc::{Receiver, channel};

    /// A directory of its own per test, removed when it ends.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let p = std::env::temp_dir().join(format!("letibot-push-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        fn file(&self) -> PathBuf {
            self.0.join("s-test.push")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn note(text: &str) -> Value {
        json!({ "params": { "content": text } })
    }

    /// Everything the channel has for the client, and nothing after a quiet second.
    fn took(rx: &Receiver<Value>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(v) = rx.recv_timeout(Duration::from_millis(300)) {
            out.push(v["params"]["content"].as_str().unwrap_or("").to_string());
        }
        out
    }

    #[test]
    fn one_event_is_one_message_and_the_same_key_again_is_silent() {
        let dir = TempDir::new("once");
        let out = Outbox::at(dir.file());
        let (tx, rx) = channel();
        out.attach(tx);
        assert!(out.record("end:a", note("first")));
        assert!(!out.record("end:a", note("first")));
        assert_eq!(took(&rx), vec!["first".to_string()]);
        assert_eq!(out.cursor(), 1, "the client took the one record");
        assert_eq!(out.waiting(), 0);
    }

    #[test]
    fn what_happens_while_no_client_reads_is_said_once_when_one_comes_back() {
        let dir = TempDir::new("down");
        let out = Outbox::at(dir.file());
        assert!(out.record("end:a", note("first")));
        assert!(out.record("idle::1", note("second")));
        assert_eq!(out.waiting(), 2, "nothing was handed to anybody yet");
        let (tx, rx) = channel();
        out.attach(tx);
        assert_eq!(
            took(&rx),
            vec!["first".to_string(), "second".to_string()],
            "the backlog goes oldest first"
        );
        assert_eq!(out.waiting(), 0);
    }

    #[test]
    fn a_client_that_went_away_keeps_what_it_did_not_take() {
        let dir = TempDir::new("gone");
        let out = Outbox::at(dir.file());
        let (tx, rx) = channel();
        out.attach(tx);
        drop(rx);
        assert!(out.record("end:a", note("first")));
        assert_eq!(out.waiting(), 1, "a send into a closed channel keeps it");
        let (tx, rx) = channel();
        out.attach(tx);
        assert_eq!(took(&rx), vec!["first".to_string()]);
        assert_eq!(out.waiting(), 0);
    }

    #[test]
    fn a_restart_says_nothing_it_already_said() {
        let dir = TempDir::new("restart");
        let first = Outbox::at(dir.file());
        let (tx, rx) = channel();
        first.attach(tx);
        assert!(first.record("end:a", note("first")));
        assert_eq!(took(&rx), vec!["first".to_string()]);
        drop(first);

        let second = Outbox::at(dir.file());
        assert!(
            !second.record("end:a", note("first")),
            "the file remembers the key"
        );
        let (tx, rx) = channel();
        second.attach(tx);
        assert!(took(&rx).is_empty(), "a restart re-pushed what it said");
        assert_eq!(second.cursor(), 1, "and it resumes where it stopped");
    }

    #[test]
    fn a_restart_owes_what_it_never_delivered() {
        let dir = TempDir::new("owed");
        let first = Outbox::at(dir.file());
        assert!(first.record("end:a", note("first")));
        drop(first);

        let second = Outbox::at(dir.file());
        let (tx, rx) = channel();
        second.attach(tx);
        assert_eq!(
            took(&rx),
            vec!["first".to_string()],
            "the record a killed bridge never handed over is still owed"
        );
    }

    #[test]
    fn work_open_is_named_and_its_change_is_a_second_event() {
        let jobs = vec!["j1".to_string(), "j2".to_string()];
        let (key, note) = idle_event("leticode `s` (/w)", "s", &jobs, 3).expect("work open");
        assert_eq!(key, "idle:j1,j2:3");
        let content = note["params"]["content"].as_str().unwrap_or("");
        assert!(
            content.contains("j1, j2") && content.contains("3 open todo rows"),
            "{content}"
        );
        assert_eq!(note["params"]["meta"]["event"], "idle_with_work");
        assert_eq!(note["params"]["meta"]["jobs"][1], "j2");
        assert!(
            idle_event("w", "s", &[], 0).is_none(),
            "a session with nothing open is not news"
        );
        let (changed, _) = idle_event("w", "s", &jobs[..1], 3).expect("one job left");
        assert_ne!(changed, key, "a job ending changes what is open");
    }

    #[test]
    fn a_job_end_names_the_job_its_state_and_when() {
        let e = Entry {
            cursor: 7,
            kind: "job",
            state: "ended",
            name: "j9".into(),
            code: String::new(),
            text: "exited 0".into(),
            ms: Some(41_000),
            reset_ms: None,
        };
        let (key, note) = job_event("leticode `s` (/w)", "s", &e);
        assert_eq!(key, "job-end:j9:exited 0");
        assert_eq!(note["params"]["meta"]["event"], "job_ended");
        assert_eq!(note["params"]["meta"]["session"], "s");
        assert_eq!(note["params"]["meta"]["job"], "j9");
        assert_eq!(note["params"]["meta"]["state"], "exited 0");
        let content = note["params"]["content"].as_str().unwrap_or("");
        assert!(
            content.contains("j9") && content.contains("41s"),
            "{content}"
        );
    }
}
