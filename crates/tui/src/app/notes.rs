//! The notes: disclosures filed, retired and counted, and the one-line notice.

use super::*;
use letibot_sessionlog::view::{SettledDecision, Warned};

/// How many notes this head holds. One list serves two doors - the conversation's own and the
/// edge's - so the cap belongs to the list, and a door that forgot it would grow for the life of the
/// box.
pub(crate) const NOTES_KEPT: usize = 64;

/// What this head keeps quiet: the warning kinds `leticode.toml`'s `[warnings] mute` names, and
/// whether a burst of one kind is one counted row (`[warnings] coalesce`, whose default is
/// today's head). One value because the drawing walk reads the two together, and a kind's mute is
/// answered before its burst is counted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Quiet {
    pub(crate) muted: Vec<String>,
    pub(crate) coalesce: bool,
}

impl Quiet {
    /// The head's own answer: nothing muted, and bursts counted.
    pub(crate) fn shipped() -> Quiet {
        Quiet {
            muted: Vec::new(),
            coalesce: true,
        }
    }
}

impl App {
    /// Post a transient line, which lives for [`NOTICE_MS`] of wall time and then gets out of the way. It does not
    /// take the input line's place, since a head that did left nowhere to see what you were typing for the rest of
    /// the session.
    pub(crate) fn say(&mut self, text: &str) {
        self.notice = Some(text.to_string());
        self.notice_until = Some(self.now_ms.saturating_add(NOTICE_MS));
    }

    /// File something that happened between rows, at the row it happened at.
    pub(crate) fn note(&mut self, n: Note) {
        // A warning is announced once, so it is noted once: the same `Warning` reaches a head from the snapshot at
        // anchor 0 and from the live arm at the current end, and the second copy reads as pinned under the
        // conversation. Identity is `(code, log ts, detail)`, and the first anchor wins.
        if holds(&self.notes, &n) {
            return;
        }
        self.file_note(n);
    }

    /// File a note BESIDE the conversation: `/notes` lists it, `/status` counts it, and no row is drawn for it. The
    /// edge-bound diagnostics are filed here, through the same identity test and cap as [`App::note`], since two
    /// doors into one list drift.
    pub(crate) fn note_beside(&mut self, n: Note) {
        if holds(&self.notes, &n) {
            return;
        }
        self.notes.push((Placed::Before, n));
        // Born older than what the reader has read, or not: the same one writer of the mark's keys.
        self.hide_read();
        self.cap_notes();
    }

    /// Put a note in the conversation, with the bookkeeping and without the identity test. The split exists for
    /// [`Note::Pane`], the one disclosure that cannot be redelivered: a second pane exiting with the same words is a
    /// second ending, and [`note_key`] cannot tell it from the first.
    pub(crate) fn file_note(&mut self, n: Note) {
        let at = self.items.len();
        // A note joining a burst changes a row that is already drawn: the count lives in the first member's seam and
        // the walk is incremental, so the rewind goes to the row before it. The chain is with the previous note of
        // its KIND, since the wall interleaves two codes - and `[warnings] coalesce = false` means nothing joins.
        let head = coalescable(&n)
            .filter(|_| self.quiet.coalesce)
            .and_then(|_| {
                let prev = self
                    .notes
                    .iter()
                    .rposition(|(place, o)| matches!(place, Placed::Seam(_)) && chains(o, &n))?;
                Some(burst_head(&self.notes, prev))
            });
        let head = head.and_then(|i| match self.notes[i].0 {
            Placed::Seam(at) => Some(at),
            Placed::Before => None,
        });
        self.notes.push((Placed::Seam(at), n));
        // A note born older than what the reader has read is born hidden: the watermark's half of filing, and the
        // reason a re-said notice cannot rebuild a wall that was cleared. Through `hide_read`, so there is one writer
        // of the mark's keys and one place the cap lives.
        self.hide_read();
        if let Some(head) = head {
            self.invalidate_history_from(head.saturating_sub(1));
        }
        self.cap_notes();
    }

    /// The cap on the note list, as one function because filing has two doors now: a cap written into
    /// one of them is a list that grows through the other for the life of the box.
    fn cap_notes(&mut self) {
        while self.notes.len() > NOTES_KEPT {
            self.notes.remove(0);
            self.note_upto = self.note_upto.saturating_sub(1);
            // Every mark holds a `note_upto`, and dropping the oldest note shifts every index down by one: a mark
            // that is not shifted rewinds to the wrong note and re-renders it.
            for m in &mut self.hist_marks {
                m.note_upto = m.note_upto.saturating_sub(1);
            }
        }
    }

    /// Take a note out of the list altogether - the model's live line, when the daemon withdraws it:
    /// a recovered retry leaves nothing behind and a spent ladder has a row of its own. Unlike
    /// [`App::retire`] this leaves nothing in `/notes` either, so the walk is rebuilt rather than
    /// patched: a mark holds an index into this list, and the list has just got shorter.
    pub(crate) fn drop_note(&mut self, i: usize) {
        if i >= self.notes.len() {
            return;
        }
        self.notes.remove(i);
        self.invalidate_history();
    }

    /// Is this note one the reader does not see? A retired note is not rendered and nothing else changes: it stays in
    /// `notes`, [`App::notes_lines`] lists it, [`App::retired_notes`] counts it. The key set, the
    /// muted kinds and the watermark are three ways to the one question.
    pub(crate) fn is_retired(&self, n: &Note) -> bool {
        self.dismissed.contains(&note_key(n))
            || muted(&self.quiet, n)
            || already_read(&self.read_marks, n)
    }

    /// What this reader has read of one kind of notice - the newest `ts` a `ctrl-n`
    /// covered, or `None` for a kind it has never read.
    #[cfg(test)]
    pub(crate) fn read_upto(&self, kind: &str) -> Option<u64> {
        self.read_marks
            .iter()
            .find(|(k, _)| k == kind)
            .map(|(_, ts)| *ts)
    }

    /// `ctrl-n`'s act: say how far this reader has read, per kind, then hide what that covers. Per kind and not per
    /// copy, since the mark is `(code, newest ts)` and a copy arriving after it is read because it is not newer. A
    /// note with no clock is never marked.
    pub(crate) fn mark_read(&mut self) {
        let before = self.read_marks.clone();
        for (_, n) in &self.notes {
            let (Some(kind), Some(ts)) = (note_kind(n), note_stamp(n)) else {
                continue;
            };
            if ts == 0 {
                continue;
            }
            match self.read_marks.iter_mut().find(|(k, _)| k == kind) {
                Some(row) => row.1 = row.1.max(ts),
                None => self.read_marks.push((kind.to_string(), ts)),
            }
        }
        let added = self.hide_read();
        if self.read_marks != before || added > 0 {
            self.invalidate_history();
        }
    }

    /// Put every note the mark covers into `dismissed`, and answer how many went in: one writer for the second half
    /// of the mark, called from the three moments the two records can disagree. `dismissed` stays the record
    /// everything else reads.
    pub(crate) fn hide_read(&mut self) -> usize {
        let mut added = 0;
        for (_, n) in &self.notes {
            if !covered_by_mark(&self.read_marks, n) {
                continue;
            }
            let key = note_key(n);
            if !self.dismissed.contains(&key) {
                self.dismissed.push(key);
                added += 1;
            }
        }
        if added > 0 {
            // The same cap `retire` keeps: this is the reader's memory of what they have read, and an unbounded one
            // grows for the life of the box. A key that falls off is a copy whose kind the mark still covers.
            let over = self
                .dismissed
                .len()
                .saturating_sub(crate::prefs::RETIRED_CAP);
            if over > 0 {
                self.dismissed.drain(..over);
            }
        }
        added
    }

    /// How many of the notes this head holds are hidden right now, which is the number `/status` shows. Computed
    /// from the notes rather than kept as a counter, since a counter can disagree with the screen.
    pub(crate) fn retired_notes(&self) -> usize {
        self.notes
            .iter()
            .filter(|(_, n)| self.is_retired(n))
            .count()
    }

    /// How many notes this head holds and is not drawing: they are older than the conversation it shows, or they were
    /// filed beside it on purpose. The number `/status` shows beside the count, and the three are different reasons
    /// for a line being absent: one the reader chose, one the window decided, one the placement decided.
    ///
    /// A seam below `hist_floor` counts too: the tail walk steps over it just as it does `Before`.
    pub(crate) fn notes_before(&self) -> usize {
        self.notes
            .iter()
            .filter(|(place, _)| match place {
                Placed::Before => true,
                Placed::Seam(at) => *at < self.hist_floor,
            })
            .count()
    }

    /// Retire a note, or all of them, and answer how many were newly hidden. Written through one function so the two
    /// callers cannot disagree about the three things that happen together: the key list, the rendered history, and
    /// the file.
    pub(crate) fn retire(&mut self, keys: Vec<String>) -> usize {
        let mut added = 0;
        for k in keys {
            if self.dismissed.contains(&k) {
                continue;
            }
            self.dismissed.push(k);
            added += 1;
        }
        if added == 0 {
            return 0;
        }
        // Oldest out, `prefs::RETIRED_CAP` deep: the list is this reader's memory,
        // and an unbounded one is a file that grows for the life of the box.
        let over = self
            .dismissed
            .len()
            .saturating_sub(crate::prefs::RETIRED_CAP);
        if over > 0 {
            self.dismissed.drain(..over);
        }
        // A note that was in `hist_lines` has to come out of it, and the walk is
        // incremental: the only honest way to un-draw a line is to rebuild.
        self.invalidate_history();
        added
    }

    /// Take back what the file says is retired, not only what this head wrote: `load_prefs` runs once at startup, so
    /// a dismissal another head recorded since is otherwise invisible. Called where the person is looking or acting,
    /// not on a timer.
    pub(crate) fn refresh_retired(&mut self) {
        let Some(path) = self.prefs_path.clone() else {
            return;
        };
        let (p, _) = crate::prefs::load(&path);
        if p.retired != self.dismissed {
            self.dismissed = p.retired;
            // And what the watermark covers goes straight back in: this replaces the whole set from a file another
            // head may have written, so a key the mark added in memory would be dropped and the wall would come back
            // on the next draw.
            self.hide_read();
            self.invalidate_history();
        }
    }

    /// True when a disclosure counter has moved past what this reader has been shown: the triangle is a pointer at
    /// `/status` and reading that screen acknowledges it, without which the mark is permanent.
    pub(crate) fn alarmed(&self) -> bool {
        self.counters().exceeds(self.acked)
    }

    /// This head's six disclosure counters, as one value - the shape the alarm and its
    /// acknowledgement both compare, so *"has anything moved"* has one definition.
    pub(crate) fn counters(&self) -> Counters {
        Counters {
            dropped: self.dropped,
            scrubbed: self.scrubbed,
            resyncs: self.resyncs,
            unreadable: self.unreadable,
            gaps: self.gaps,
            orphan_bodies: self.orphan_bodies,
            slow_first_byte: self.slow_first_byte,
        }
    }

    /// The reader has read the numbers, so stop pointing at them: called when `/status` opens, and only then.
    /// Nothing is reset, since the screen keeps showing the raw values and a counter that moves again starts the
    /// conversation over.
    pub(crate) fn acknowledge_counters(&mut self) {
        self.acked = self.counters();
    }

    /// Move the counter that belongs to an edge-bound code, or say this head has none: one place, so the `Warning`
    /// arm and the test that checks every `ALARM_ONLY` row read the same table. `false` is the case worth having,
    /// since a code with nowhere to go is then said rather than swallowed.
    pub(crate) fn count_edge_note(&mut self, code: &str) -> bool {
        match code {
            "model_slow_first_byte" => {
                self.slow_first_byte += 1;
                true
            }
            _ => false,
        }
    }

    /// Drop the transient notice, once the operator has had a frame to see it, and stop
    /// whatever clock it started.
    pub fn clear_notice(&mut self) {
        self.notice = None;
        self.notice_until = None;
    }

    /// Take down the notes whose sentence has been on the screen long enough, on this head's clock. It returns
    /// whether anything went so the caller rebuilds once, since a note is a seam and the walk is incremental, and a
    /// timer may not write the retired key.
    pub(crate) fn fade_fleeting_notes(&mut self) -> bool {
        let before = self.notes.len();
        self.notes.retain(|(_, n)| !past_its_time(n, self.now_ms));
        before != self.notes.len()
    }
}

/// How a save treats the retired set, since one write cannot express both verbs: a dismissal asserts a key is
/// retired and a restore asserts it is not. A union cannot restore; a replacement can, but would discard keys
/// another head retired since this one loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetiredWrite {
    /// Add to the file's set. A dismissal is an assertion no other head's save may contradict.
    Union,
    /// Take this head's set as the whole truth. A restore is an assertion of removal.
    Replace,
}

/// The six disclosure counters as one comparable value: a struct rather than six arguments, because the alarm and
/// its acknowledgement have to agree about which numbers count, and a list spelled out at two call sites grows at
/// one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Counters {
    pub(crate) dropped: u64,
    pub(crate) scrubbed: u64,
    pub(crate) resyncs: u64,
    pub(crate) unreadable: u64,
    pub(crate) gaps: u64,
    pub(crate) orphan_bodies: u64,
    pub(crate) slow_first_byte: u64,
}

impl Counters {
    /// Has any counter moved past what was acknowledged: per counter, so acknowledging is *I have seen two resyncs*
    /// and not *stop telling me about resyncs*. A counter that went backwards is not news, since an alarm that fired
    /// on a decrease would be a mark nobody could clear.
    pub(crate) fn exceeds(self, seen: Counters) -> bool {
        self.dropped > seen.dropped
            || self.scrubbed > seen.scrubbed
            || self.resyncs > seen.resyncs
            || self.unreadable > seen.unreadable
            || self.gaps > seen.gaps
            || self.orphan_bodies > seen.orphan_bodies
            || self.slow_first_byte > seen.slow_first_byte
    }
}

/// Where a note sits, or whether it sits in the conversation at all. A warning is how a head shows a fact once, and
/// a head that has just attached has shown nothing, so replaying a snapshot's warnings drew a wall above a
/// conversation they did not precede. They are prior: the head was not there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placed {
    /// At this many rows: the seam the walk puts it back into.
    Seam(usize),
    /// Before this window. Reachable - `/notes` lists it, `/status` counts it, and a
    /// reader who wants it is one verb away - and not drawn, because it is not news.
    Before,
}

/// Something that happened between two transcript rows.
#[derive(Debug, Clone)]
pub(crate) enum Note {
    /// The post-flight assertions and the guards land here, and a guard
    /// nobody notices is a guard nobody wrote.
    Warned(Warned),
    /// A refusal nobody made: the harness itself could not read the call, so there is nothing for the person to
    /// answer, grant or lift, and the thing that has to change is the model's next attempt. Hence its own variant.
    NotRun(Warned),
    /// A settled decision renders as its outcome, not as an open prompt, and not as nothing either: a tool that was
    /// refused has to look refused. Boxed because a decision is the widest thing a note carries.
    Decided(Box<SettledDecision>),
    /// A question the person answered: a faint line and not a verdict. `Decided`'s four words are a permission's,
    /// and a question has no ladder, so none of them is true of it.
    Answered {
        /// The daemon's id for the ask, so a redelivery of the same answer is not a
        /// second row.
        req_id: String,
        summary: String,
        /// The person, as `kind identity`.
        by: String,
        /// What they said, rendered by the tool's own renderer.
        said: String,
    },
    /// A pane's ending, as a row that is still there a minute later. A notice lives for
    /// `NOTICE_MS`, so a program that died at once flashed and was gone.
    Pane {
        /// The line that was run, verb included - *what ended* is half the fact.
        line: String,
        /// The last rows the program left on the screen, in order and with the blank
        /// ones dropped. See [`TermPane::last_rows`] for why the screen and not the bytes,
        /// and why the tail.
        said: Vec<String>,
        /// The daemon's own sentence: the exit status, or the operator's act. Never
        /// guessed here - see [`ServerFrame::TermEnded`].
        reason: String,
        /// This head asked for the end and the person confirmed it: its own record that it sent
        /// [`Action::TermClose`], and not a reading of the reason's wording. `closed` and not `left`, since leaving
        /// is a detach now.
        closed: bool,
    },
    /// One model call being retried, as ONE row. The wire sends the row's whole value on every
    /// failed attempt, so a head that holds the row rewrites it - which is what makes five attempts
    /// one line rather than five, and what lets that line settle to how the call ended.
    Retrying(Retry),
}

/// One series of retries, as the row that draws it. The values are the daemon's: the words of the
/// line, the attempts behind the popup, and the line the row settles to once the series stops.
#[derive(Debug, Clone)]
pub(crate) struct Retry {
    /// Which series this row is. A second outage is a second row, and a row that moved is not.
    pub(crate) series: u64,
    /// The endpoint being retried, as the daemon spells a host.
    pub(crate) host: String,
    /// How many attempts have failed so far.
    pub(crate) attempts: u32,
    /// The row's line while the ladder is still trying.
    pub(crate) said: String,
    /// Every failed attempt, oldest first: the popup's list.
    pub(crate) log: Vec<String>,
    /// The line the row settles to, `None` while the series is still going.
    pub(crate) ended: Option<String>,
    /// When this head filed the row: the clock a watermark compares, as every note has.
    pub(crate) ts: u64,
}

impl Retry {
    /// The row's line as it stands: the ending once there is one, because a settled row that still
    /// said `retrying` would be a row about a call nothing is doing anything about.
    pub(crate) fn line(&self) -> &str {
        self.ended.as_deref().unwrap_or(&self.said)
    }

    /// Is the ladder still trying? A settled row is history and must not be rewritten by the next
    /// series, which is the whole difference between a value moving and a second outage.
    pub(crate) fn live(&self) -> bool {
        self.ended.is_none()
    }
}

/// The kind a retry row is filed under: one event, never a burst of itself, so a series counts its
/// own attempts inside its line rather than in a count of rows.
pub(crate) const MODEL_RETRY: &str = "model_retry";

impl Note {
    /// A settled decision, as the note it deserves. One function because the live arm and the snapshot arm both
    /// file this, and a snapshot that made a different note than the live path is the drift this closes.
    pub(crate) fn settled(d: SettledDecision) -> Note {
        if d.kind != "question" {
            return Note::Decided(Box::new(d));
        }
        // `kind identity`, or the kind alone: `gate:timeout` has an identity and a
        // bare `subagent` does not, and an empty quoted name is worse than no name.
        let by = if d.by.identity.is_empty() {
            d.by.kind.clone()
        } else {
            format!("{} {}", d.by.kind, d.by.identity)
        };
        Note::Answered {
            req_id: d.req_id,
            summary: d.summary,
            by,
            said: d.basis,
        }
    }
}

/// The kind of notice a note is, for the watermark: its warning code, or `None` for the disclosures that are not a
/// stream of the same thing happening again. `Warned` and `NotRun` are the two that arrive by the hundred and carry
/// a clock.
pub(crate) fn note_kind(n: &Note) -> Option<&str> {
    match n {
        // The FAMILY and not the code: a refusal's code carries its request id, which is the notice's identity, so
        // what kind of thing it is has to be read off the prefix.
        Note::Warned(w) | Note::NotRun(w) => Some(letibot_sessionlog::warning::family(&w.code)),
        // A series is a kind of its own, so `ctrl-n` retires a settled one and [`LIVE`] keeps it
        // out of a burst: its attempts are counted in its line, not in a count of rows.
        Note::Retrying(_) => Some(MODEL_RETRY),
        Note::Decided(_) | Note::Answered { .. } | Note::Pane { .. } => None,
    }
}

/// When a note happened, by the session log's clock: the value a watermark compares. `0` is a note that happened on
/// the socket rather than on the log, and it is returned as itself, since two notes with no clock cannot be ordered.
pub(crate) fn note_stamp(n: &Note) -> Option<u64> {
    match n {
        Note::Warned(w) | Note::NotRun(w) => Some(w.ts),
        Note::Retrying(r) => Some(r.ts),
        _ => None,
    }
}

/// Has this reader read this note's kind past it? The watermark's whole question, as a free function so the drawing
/// walk, which holds the head's fields destructured, asks exactly what [`App::is_retired`] asks.
pub(crate) fn already_read(marks: &[(String, u64)], n: &Note) -> bool {
    read_against(marks, n, false)
}

/// Is this note at or before its kind's mark? The same question for the copies this head holds, and `<=` where
/// [`already_read`] is `<`: a note at the mark's own instant was on the screen when the mark was taken, so the copy
/// is read.
pub(crate) fn covered_by_mark(marks: &[(String, u64)], n: &Note) -> bool {
    read_against(marks, n, true)
}

fn read_against(marks: &[(String, u64)], n: &Note, inclusive: bool) -> bool {
    let (Some(kind), Some(ts)) = (note_kind(n), note_stamp(n)) else {
        return false;
    };
    if ts == 0 {
        return false;
    }
    marks
        .iter()
        .any(|(k, mark)| k == kind && if inclusive { ts <= *mark } else { ts < *mark })
}

/// What a note is called when a reader wants to retire it: one identity per disclosure, built from the note's own
/// facts and from nothing about where it is on the screen, so it survives a resync and a restart.
pub(crate) fn note_key(n: &Note) -> String {
    match n {
        Note::Warned(w) => format!("w|{}|{}|{:016x}", w.code, w.ts, fnv1a(&w.detail)),
        Note::NotRun(w) => format!("n|{}|{}|{:016x}", w.code, w.ts, fnv1a(&w.detail)),
        Note::Decided(d) => format!("d|{}", d.req_id),
        Note::Answered { req_id, .. } => format!("a|{req_id}"),
        Note::Pane {
            line, said, reason, ..
        } => format!("t|{line}|{reason}|{:016x}", fnv1a(&said.join("\n"))),
        // By the series and not by the text: the row's whole point is that the text moves while
        // the row stays, so a key built from it would be a hidden dismissal on every attempt.
        Note::Retrying(r) => format!("r|{}", r.series),
    }
}

/// Is this announcement already one this head holds? One identity for a disclosure, and it is [`note_key`]'s rather
/// than a second rule, since the same announcement has to mean the same thing to the walk, the listing and the file.
pub(crate) fn holds(notes: &[(Placed, Note)], n: &Note) -> bool {
    let k = note_key(n);
    notes.iter().any(|(_, o)| note_key(o) == k)
}

/// FNV-1a, 64-bit. Hand-rolled rather than `std`'s hasher, which is not stable across releases, and a key that
/// changed when the head was rebuilt would resurrect every note the person had retired.
pub(crate) fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// How long a status notice stays on the screen, in wall-clock milliseconds. 1600 is the number already in effect:
/// the old countdown was 60 frames, which was 1.6 s on a live head.
pub const NOTICE_MS: u64 = 1_600;

/// How long a fleeting note stays in the conversation: the codes in
/// [`letibot_sessionlog::warning::FLEETING`] are the rule about which ones and why. Thirty seconds, and the number is
/// the operator's.
pub const FLEETING_MS: u64 = 30_000;

/// Is this note past the time it is drawn for? The `FLEETING` codes only, and the class table is what keeps the set
/// honest: a `Failure` or a `Refused` is never here, so a fault can never be on a timer.
pub(crate) fn past_its_time(n: &Note, now_ms: u64) -> bool {
    let w = match n {
        Note::Warned(w) | Note::NotRun(w) => w,
        _ => return false,
    };
    letibot_sessionlog::warning::is_fleeting(&w.code) && now_ms >= w.ts.saturating_add(FLEETING_MS)
}

/// How long a burst lasts: the window in which notices of one kind become one counted row. A minute, which is the
/// daemon's own tick, so two consecutive passes of one incident are one burst and the third is not.
pub const COALESCE_MS: u64 = 60_000;

/// Codes that are re-said while they still stand, and so are never coalesced: a burst counts things that happened,
/// and a line saying *this is still true* is not one of them.
pub const LIVE: &[&str] = &["store_busy_wait", "model_retry"];

/// The plural a burst of this code is counted in - `5 reviews could not run` rather than
/// `5 × queue_blocked`. A code with no row here is counted by its code, which is the head's own
/// vocabulary everywhere else (`notice_card::warning_notice` leads with it too).
pub const COUNTED_AS: &[(&str, &str)] = &[
    ("queue_blocked", "reviews could not run"),
    ("queue_needs_human", "reviews need you"),
    ("queue_branch_kept", "trees kept"),
    ("queue_waiting_for_vm", "reviews waiting for a VM"),
    ("queue_vm_slots_exhausted", "VM-slot waits"),
    ("target_red", "targets RED"),
    ("target_green", "targets green again"),
    ("target_unjudged", "targets not judged"),
    ("target_timed_out", "targets whose gate timed out"),
    ("gate_setup_failed", "targets the gate could not judge"),
    ("gate_killed", "targets whose gate was killed"),
    ("merge_failed", "landings that failed their gate"),
    ("merge_conflict", "branches that would not rebase"),
    ("merge_queued", "branches queued"),
    ("queue_push_failed", "pushes that failed"),
    ("queue_install_failed", "installs that failed"),
    ("refusal_undelivered", "refusals that reached nobody"),
    ("plugin_layer", "plugin layers that failed"),
    ("plugin_refused", "plugin layers refused"),
    ("plugin_view_refused", "panes a layer refused to draw"),
    ("restart_restored", "restarts that brought work back"),
    ("restart_reminded", "restarts that reminded the model"),
    ("workspace_gone", "sessions whose workspace is gone"),
    ("boundary_absent", "sessions refused for want of a boundary"),
    // A refusal's code carries its request id (`denied:<id>`); the family is what a count of them
    // is counted in, and each member's own id is on its line in the popup.
    ("denied", "calls the gate refused"),
];

/// The phrase a burst of `code` is counted in - its own, or the code itself.
pub(crate) fn counted_as(code: &str) -> &str {
    COUNTED_AS
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, words)| *words)
        .unwrap_or(code)
}

/// May this note join a burst, and under which kind and clock? `Warned` and `NotRun` only, since a settled
/// decision, an answer and a pane's ending are each about one request or one program, so there is no more of the
/// same to count.
pub(crate) fn coalescable(n: &Note) -> Option<(&str, u64)> {
    let kind = note_kind(n)?;
    if LIVE.contains(&kind) {
        return None;
    }
    let ts = note_stamp(n)?;
    (ts > 0).then_some((kind, ts))
}

/// Is `b` the next member of `a`'s burst: the same kind and inside [`COALESCE_MS`]. A clock that went backwards is
/// a member and not a new burst, since two notices that cannot be ordered are one incident as far as a count goes.
pub(crate) fn chains(a: &Note, b: &Note) -> bool {
    match (coalescable(a), coalescable(b)) {
        (Some((ka, ta)), Some((kb, tb))) => ka == kb && tb.saturating_sub(ta) <= COALESCE_MS,
        _ => false,
    }
}

/// Is this note one the reader has retired? - [`App::is_retired`] as a free function, for the
/// walk and for [`burst_at`], which hold the head's fields destructured.
pub(crate) fn note_retired(
    dismissed: &[String],
    quiet: &Quiet,
    marks: &[(String, u64)],
    n: &Note,
) -> bool {
    dismissed.contains(&note_key(n)) || muted(quiet, n) || already_read(marks, n)
}

/// Is this note's KIND one `[warnings] mute` names? The kind is what `ctrl-n` retires by - the
/// warning code, or `family`'s answer for one that carries an id - so a mute says it once per
/// kind instead of once per copy.
pub(crate) fn muted(quiet: &Quiet, n: &Note) -> bool {
    note_kind(n).is_some_and(|kind| quiet.muted.iter().any(|m| m == kind))
}

/// The `ts` of note `j` when it may chain with `kind` - drawn, coalescable, the same kind.
fn chained_ts(notes: &[(Placed, Note)], j: usize, kind: &str) -> Option<u64> {
    if !matches!(notes[j].0, Placed::Seam(_)) {
        return None;
    }
    match coalescable(&notes[j].1) {
        Some((k, ts)) if k == kind => Some(ts),
        _ => None,
    }
}

/// The first member of the burst `i` belongs to - the chain walked backwards.
pub(crate) fn burst_head(notes: &[(Placed, Note)], i: usize) -> usize {
    let Some((kind, ts)) = coalescable(&notes[i].1) else {
        return i;
    };
    let mut head = i;
    let mut next_ts = ts;
    for j in (0..i).rev() {
        let Some(t) = chained_ts(notes, j, kind) else {
            continue;
        };
        if next_ts.saturating_sub(t) > COALESCE_MS {
            break;
        }
        head = j;
        next_ts = t;
    }
    head
}

/// The last member of the burst `i` belongs to - the chain walked forwards.
fn burst_end(notes: &[(Placed, Note)], i: usize) -> usize {
    let Some((kind, ts)) = coalescable(&notes[i].1) else {
        return i;
    };
    let mut end = i;
    let mut prev_ts = ts;
    for j in (i + 1)..notes.len() {
        let Some(t) = chained_ts(notes, j, kind) else {
            continue;
        };
        if t.saturating_sub(prev_ts) > COALESCE_MS {
            break;
        }
        end = j;
        prev_ts = t;
    }
    end
}

/// What the walk does with one note, as far as a burst is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Burst {
    /// A member of a burst whose counted row has already been drawn - draw nothing.
    Drawn,
    /// Draw the note as it always was: it is not coalescable, or it is alone.
    Single,
    /// The head of a burst of two or more - the indices of the live members, oldest first.
    Counted(Vec<usize>),
}

/// Is this note the one that draws, and how many does it stand for? The row belongs to the first LIVE member of the
/// chain, since the reader can retire one notice and the others must still be on the screen.
pub(crate) fn burst_at(
    notes: &[(Placed, Note)],
    dismissed: &[String],
    quiet: &Quiet,
    marks: &[(String, u64)],
    i: usize,
) -> Burst {
    // `[warnings] coalesce = false` is one row per notice and no counting: the reader asked for
    // every copy, and a burst would draw one of them.
    if !quiet.coalesce {
        return Burst::Single;
    }
    let Some((kind, _)) = coalescable(&notes[i].1) else {
        return Burst::Single;
    };
    // The chain's own members and not every index between its ends: the notes of other kinds are interleaved with
    // them, and an index range would count a neighbour as a member.
    let live: Vec<usize> = (burst_head(notes, i)..=burst_end(notes, i))
        .filter(|&j| chained_ts(notes, j, kind).is_some())
        .filter(|&j| !note_retired(dismissed, quiet, marks, &notes[j].1))
        .collect();
    if live.first() != Some(&i) {
        return Burst::Drawn;
    }
    if live.len() < 2 {
        return Burst::Single;
    }
    Burst::Counted(live)
}
