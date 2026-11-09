//! Signal accounting: for each message, what followed within a window. The ledger is a second
//! append-only file beside the chat, written by its own thread, so `say` never waits on it.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use serde::{Deserialize, Serialize};

/// How long after a message an action still counts as a result of it.
pub const SIGNAL_WINDOW_MS: u64 = 30 * 60 * 1000;
/// Most messages the ledger keeps in memory. The file keeps all; the scorecard says what was dropped.
pub const MAX_OUTCOMES: usize = 20_000;

/// What a message can lead to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Act {
    /// Someone else replied in its thread.
    Reply,
    /// It was an ask and the target answered or declined.
    AskSettled,
    /// A todo row names it.
    Todo,
    /// An ownership claim followed from it (R163 calls this).
    Claim,
    /// Someone else read its thread.
    Read,
}

const ACTS: usize = 5;

impl Act {
    fn ix(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Line {
    Msg {
        seq: u64,
        ts: u64,
        author: String,
        kind: String,
    },
    Act {
        seq: u64,
        act: Act,
        by: String,
        ms: u64,
    },
    Wake {
        seq: u64,
        cheap: bool,
        silent: bool,
        tokens: u64,
    },
    Refused {
        author: String,
        kind: String,
    },
    /// One named counter went up: what the room refused, what it filed, what was opened again.
    Count {
        what: String,
    },
}

/// The measurements the room keeps beside its scorecard. One query reads them all: the `signals`
/// frame's `counts`, one row per word below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Count {
    /// A post refused as an ack with no ref in it.
    AckRefused,
    /// A post refused as a repeat of one of the last few messages.
    EchoRefused,
    /// A post filed as a record, over [`crate::chat::RECORD_CHARS`] characters or 3 lines.
    RecordMade,
    /// A `read` of a message the room filed as a record: did the overflow get opened again?
    RecordOpen,
    /// A `say` into the host room that named nobody and was not an announcement.
    HostRoomRefused,
    /// A project-room post that pointed at nothing: the leak this count is here to measure.
    ProjectPostWithoutRef,
}

impl Count {
    /// The word the count is kept and read by.
    pub fn word(self) -> &'static str {
        match self {
            Count::AckRefused => "ack_refused",
            Count::EchoRefused => "echo_refused",
            Count::RecordMade => "record_made",
            Count::RecordOpen => "record_open",
            Count::HostRoomRefused => "host_room_refused",
            Count::ProjectPostWithoutRef => "project_post_without_ref",
        }
    }
}

#[derive(Debug, Default, Clone)]
struct Outcome {
    author: String,
    kind: String,
    ts: u64,
    count: [u32; ACTS],
    wakes: u32,
    silent: u32,
    silent_tokens: u64,
}

impl Outcome {
    fn acted(&self) -> bool {
        self.count.iter().any(|c| *c > 0)
    }
}

/// One line of the scorecard: a sender or a kind.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalRow {
    pub key: String,
    /// Messages whose window closed or that were acted on already.
    pub msgs: u64,
    /// Of `msgs`, the ones something followed.
    pub acted: u64,
    /// Messages still inside their window with nothing yet.
    pub pending: u64,
    pub replies: u64,
    pub asks_settled: u64,
    pub todos: u64,
    pub claims: u64,
    pub reads: u64,
    pub wakes: u64,
    /// Wakes that ended with nothing said and no tool called.
    pub silent: u64,
    pub silent_tokens: u64,
    /// Null posts the room refused.
    pub refused: u64,
}

impl SignalRow {
    /// One compact line: `key acted 3/5 60% rep 0.8 silent 2 (410 tok) null 1`.
    pub fn line(&self) -> String {
        let pct = (self.acted * 100).checked_div(self.msgs).unwrap_or(0);
        let reps = if self.msgs + self.pending == 0 {
            0.0
        } else {
            self.replies as f64 / (self.msgs + self.pending) as f64
        };
        let mut s = format!(
            "{} acted {}/{} {pct}% rep {reps:.1}",
            self.key, self.acted, self.msgs
        );
        if self.pending > 0 {
            s.push_str(&format!(" open {}", self.pending));
        }
        if self.wakes > 0 {
            s.push_str(&format!(
                " silent {}/{} ({} tok)",
                self.silent, self.wakes, self.silent_tokens
            ));
        }
        if self.refused > 0 {
            s.push_str(&format!(" null {}", self.refused));
        }
        s
    }
}

/// The ledger read out: per sender and per kind.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scorecard {
    pub window_ms: u64,
    pub senders: Vec<SignalRow>,
    pub kinds: Vec<SignalRow>,
    /// Outcomes dropped from memory by [`MAX_OUTCOMES`]. They stay in the file.
    pub forgotten: u64,
    /// The room's named counters, by [`Count::word`].
    #[serde(default)]
    pub counts: BTreeMap<String, u64>,
}

enum Cmd {
    Line(String),
    Flush(mpsc::Sender<()>),
}

pub struct Ledger {
    window_ms: u64,
    rows: BTreeMap<u64, Outcome>,
    refused: BTreeMap<(String, String), u64>,
    counts: BTreeMap<String, u64>,
    forgotten: u64,
    tx: Option<mpsc::Sender<Cmd>>,
}

impl Default for Ledger {
    fn default() -> Ledger {
        Ledger::new(SIGNAL_WINDOW_MS)
    }
}

impl Ledger {
    pub fn new(window_ms: u64) -> Ledger {
        Ledger {
            window_ms,
            rows: BTreeMap::new(),
            refused: BTreeMap::new(),
            counts: BTreeMap::new(),
            forgotten: 0,
            tx: None,
        }
    }

    /// A ledger kept in `path`: replay what it holds, then append through a writer thread.
    pub fn open(path: &Path, window_ms: u64) -> Ledger {
        let mut l = Ledger::new(window_ms);
        let text = std::fs::read_to_string(path).unwrap_or_default();
        for (n, line) in text.lines().enumerate() {
            match serde_json::from_str::<Line>(line) {
                Ok(rec) => l.apply(rec),
                Err(e) => eprintln!(
                    "room: signal ledger {} line {} is not a record ({e}); skipped",
                    path.display(),
                    n + 1
                ),
            }
        }
        l.tx = Some(spawn_writer(path.to_path_buf()));
        l
    }

    pub fn set_window(&mut self, ms: u64) {
        self.window_ms = ms;
    }

    pub fn window_ms(&self) -> u64 {
        self.window_ms
    }

    /// Wait until every line sent so far is on disk. For tests and shutdown.
    pub fn flush(&self) {
        if let Some(tx) = &self.tx {
            let (done, wait) = mpsc::channel();
            if tx.send(Cmd::Flush(done)).is_ok() {
                let _ = wait.recv();
            }
        }
    }

    fn put(&self, rec: &Line) {
        if let Some(tx) = &self.tx
            && let Ok(line) = serde_json::to_string(rec)
        {
            let _ = tx.send(Cmd::Line(line));
        }
    }

    fn apply(&mut self, rec: Line) {
        match rec {
            Line::Msg {
                seq,
                ts,
                author,
                kind,
            } => {
                self.rows.entry(seq).or_insert(Outcome {
                    author,
                    kind,
                    ts,
                    ..Outcome::default()
                });
                while self.rows.len() > MAX_OUTCOMES {
                    self.rows.pop_first();
                    self.forgotten += 1;
                }
            }
            Line::Act { seq, act, by, ms } => {
                if let Some(o) = self.rows.get_mut(&seq)
                    && by != o.author
                    && ms.saturating_sub(o.ts) <= self.window_ms
                {
                    o.count[act.ix()] += 1;
                }
            }
            Line::Wake {
                seq,
                cheap: _,
                silent,
                tokens,
            } => {
                if let Some(o) = self.rows.get_mut(&seq) {
                    o.wakes += 1;
                    if silent {
                        o.silent += 1;
                        o.silent_tokens += tokens;
                    }
                }
            }
            Line::Refused { author, kind } => {
                *self.refused.entry((author, kind)).or_insert(0) += 1;
            }
            Line::Count { what } => {
                *self.counts.entry(what).or_insert(0) += 1;
            }
        }
    }

    fn record(&mut self, rec: Line) {
        self.put(&rec);
        self.apply(rec);
    }

    /// A message was said. A repeat of a `seq` is ignored.
    pub fn message(&mut self, seq: u64, ts: u64, author: &str, kind: &str) {
        if self.rows.contains_key(&seq) {
            return;
        }
        self.record(Line::Msg {
            seq,
            ts,
            author: author.into(),
            kind: kind.into(),
        });
    }

    /// `by` did `act` about message `seq` at `ms`. The author's own acts and acts outside the
    /// window are not recorded.
    pub fn act(&mut self, seq: u64, act: Act, by: &str, ms: u64) {
        let Some(o) = self.rows.get(&seq) else {
            return;
        };
        if by == o.author || ms.saturating_sub(o.ts) > self.window_ms {
            return;
        }
        self.record(Line::Act {
            seq,
            act,
            by: by.into(),
            ms,
        });
    }

    /// A wake caused by these messages ended. `tokens` is the turn's total, split across them.
    pub fn wake(&mut self, seqs: &[u64], cheap: bool, silent: bool, tokens: u64) {
        let n = seqs.len().max(1) as u64;
        for (i, seq) in seqs.iter().enumerate() {
            let share = tokens / n + if i + 1 == seqs.len() { tokens % n } else { 0 };
            self.record(Line::Wake {
                seq: *seq,
                cheap,
                silent,
                tokens: share,
            });
        }
    }

    /// A null post by `author` (seat word `kind`) was refused.
    pub fn refused(&mut self, author: &str, kind: &str) {
        self.record(Line::Refused {
            author: author.into(),
            kind: kind.into(),
        });
    }

    /// `what` happened once more.
    pub fn counted(&mut self, what: Count) {
        self.record(Line::Count {
            what: what.word().into(),
        });
    }

    /// The scorecard as of `now`: a message counts once something followed or its window closed.
    pub fn scorecard(&self, now: u64) -> Scorecard {
        let mut senders: BTreeMap<String, SignalRow> = BTreeMap::new();
        let mut kinds: BTreeMap<String, SignalRow> = BTreeMap::new();
        for o in self.rows.values() {
            let closed = now.saturating_sub(o.ts) >= self.window_ms;
            for (map, key) in [(&mut senders, &o.author), (&mut kinds, &o.kind)] {
                let r = map.entry(key.clone()).or_insert_with(|| SignalRow {
                    key: key.clone(),
                    ..SignalRow::default()
                });
                if o.acted() {
                    r.msgs += 1;
                    r.acted += 1;
                } else if closed {
                    r.msgs += 1;
                } else {
                    r.pending += 1;
                }
                r.replies += o.count[Act::Reply.ix()] as u64;
                r.asks_settled += o.count[Act::AskSettled.ix()] as u64;
                r.todos += o.count[Act::Todo.ix()] as u64;
                r.claims += o.count[Act::Claim.ix()] as u64;
                r.reads += o.count[Act::Read.ix()] as u64;
                r.wakes += o.wakes as u64;
                r.silent += o.silent as u64;
                r.silent_tokens += o.silent_tokens;
            }
        }
        for ((author, kind), n) in &self.refused {
            for (map, key) in [(&mut senders, author), (&mut kinds, kind)] {
                map.entry(key.clone())
                    .or_insert_with(|| SignalRow {
                        key: key.clone(),
                        ..SignalRow::default()
                    })
                    .refused += n;
            }
        }
        let sorted = |m: BTreeMap<String, SignalRow>| {
            let mut v: Vec<SignalRow> = m.into_values().collect();
            v.sort_by(|a, b| (b.msgs + b.pending, &a.key).cmp(&(a.msgs + a.pending, &b.key)));
            v
        };
        Scorecard {
            window_ms: self.window_ms,
            senders: sorted(senders),
            kinds: sorted(kinds),
            forgotten: self.forgotten,
            counts: self.counts.clone(),
        }
    }
}

fn spawn_writer(path: PathBuf) -> mpsc::Sender<Cmd> {
    let (tx, rx) = mpsc::channel::<Cmd>();
    let _ = std::thread::Builder::new()
        .name("room-signals".into())
        .spawn(move || {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| {
                    eprintln!(
                        "room: signal ledger {} not opened ({e}); not recorded",
                        path.display()
                    )
                })
                .ok();
            for cmd in rx {
                match cmd {
                    Cmd::Line(line) => {
                        if let Some(f) = file.as_mut()
                            && let Err(e) = writeln!(f, "{line}")
                        {
                            eprintln!("room: signal ledger {} not written ({e})", path.display());
                        }
                    }
                    Cmd::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u64 = 1000;

    fn row<'a>(c: &'a Scorecard, who: &str) -> &'a SignalRow {
        c.senders.iter().find(|r| r.key == who).expect("a row")
    }

    #[test]
    fn a_reply_counts_as_acted_on_and_an_ignored_post_does_not_after_the_window() {
        let mut l = Ledger::new(W);
        l.message(1, 0, "a", "session");
        l.message(2, 0, "a", "session");
        l.act(1, Act::Reply, "b", 10);
        let early = l.scorecard(100);
        assert_eq!(row(&early, "a").acted, 1);
        assert_eq!(row(&early, "a").pending, 1, "#2 may still be answered");
        let late = l.scorecard(W + 1);
        let a = row(&late, "a");
        assert_eq!((a.msgs, a.acted, a.pending), (2, 1, 0));
        assert_eq!(a.replies, 1);
    }

    #[test]
    fn an_act_outside_the_window_or_by_the_author_does_not_count() {
        let mut l = Ledger::new(W);
        l.message(1, 0, "a", "session");
        l.act(1, Act::Reply, "a", 5);
        l.act(1, Act::Reply, "b", W + 1);
        assert_eq!(row(&l.scorecard(W + 2), "a").acted, 0);
    }

    #[test]
    fn a_settled_ask_counts() {
        let mut l = Ledger::new(W);
        l.message(1, 0, "a", "ask");
        l.act(1, Act::AskSettled, "b", 5);
        let c = l.scorecard(6);
        assert_eq!(c.kinds[0].key, "ask");
        assert_eq!((c.kinds[0].acted, c.kinds[0].asks_settled), (1, 1));
    }

    #[test]
    fn a_silent_cheap_wake_records_its_tokens() {
        let mut l = Ledger::new(W);
        l.message(1, 0, "a", "session");
        l.message(2, 0, "a", "session");
        l.wake(&[1, 2], true, true, 101);
        l.refused("a", "session");
        let c = l.scorecard(1);
        let a = row(&c, "a");
        assert_eq!(
            (a.wakes, a.silent, a.silent_tokens, a.refused),
            (2, 2, 101, 1)
        );
        assert!(a.line().contains("silent 2/2 (101 tok)"), "{}", a.line());
    }

    #[test]
    fn the_ledger_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("letibot-signals-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("chat.signals");
        let _ = std::fs::remove_file(&path);
        {
            let mut l = Ledger::open(&path, W);
            l.message(1, 0, "a", "session");
            l.act(1, Act::Read, "b", 3);
            l.flush();
        }
        let l = Ledger::open(&path, W);
        let c = l.scorecard(4);
        assert_eq!((row(&c, "a").acted, row(&c, "a").reads), (1, 1));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
