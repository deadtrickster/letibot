//! **The notes**: disclosures filed, retired and counted, and the one-line notice.

use super::*;
use letibot_sessionlog::view::{SettledDecision, Warned};

impl App {
    /// Post a transient line. It lives for [`NOTICE_MS`] of **wall time** and then gets
    /// out of the way; it does **not** take the input line's place, which is what the old
    /// one did — after the first prompt of a session there was nowhere to see what you
    /// were typing, for the rest of the session.
    ///
    /// **The one writer of the notice, and it is the one that starts the clock.** Both
    /// halves in one `setf` is the point: a sentence nobody timed is a sentence nobody can
    /// get rid of, and there is now no way to set one without starting its clock.
    pub(crate) fn say(&mut self, text: &str) {
        self.notice = Some(text.to_string());
        self.notice_until = Some(self.now_ms.saturating_add(NOTICE_MS));
    }

    /// File something that happened between rows, at the row it happened at.
    pub(crate) fn note(&mut self, n: Note) {
        // A warning is announced once, so it is noted once: the same `Warning` reaches a head from
        // the snapshot at anchor 0 and from the live arm at the current end, and the second copy
        // reads as pinned under the conversation.
        //
        // Identity is `(code, log ts, detail)`, the same key `/notes dismiss` retires by, and the
        // FIRST anchor wins: a warning belongs where it happened and the later arrival is a
        // redelivery.
        if holds(&self.notes, &n) {
            return;
        }
        self.file_note(n);
    }

    /// Put a note in the conversation, with the bookkeeping and without the identity test.
    ///
    /// The split exists for [`Note::Pane`], the one disclosure that cannot be redelivered: a second
    /// `!term mc` exiting 7 with the same words is a second ending, and [`note_key`] cannot tell it
    /// from the first, so deduplicating would swallow every repeat of a pane that dies at once.
    pub(crate) fn file_note(&mut self, n: Note) {
        let at = self.items.len();
        // A note that joins a burst changes a row that is already drawn: the burst's count lives in
        // the FIRST member's seam, and the walk is incremental. The rewind goes to the row before
        // that seam, because `hist_marks[k]` is the state after seam k was drawn.
        //
        // The note it chains with is the previous one of its KIND, not the previous one filed: the
        // wall interleaves two codes, and `notes.last()` would miss every second member.
        let head = coalescable(&n).and_then(|_| {
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
        // **A note born older than what the reader has read is born hidden** — the
        // watermark's half of filing, and the reason a re-said or re-planted notice cannot
        // rebuild a wall that was cleared. Through `hide_read` rather than by hand so there
        // is one writer of the mark's keys and one place the cap lives; the walk is over the
        // notes this head holds, which is bounded.
        self.hide_read();
        if let Some(head) = head {
            self.invalidate_history_from(head.saturating_sub(1));
        }
        if self.notes.len() > 64 {
            self.notes.remove(0);
            self.note_upto = self.note_upto.saturating_sub(1);
            // Every mark holds a `note_upto`, and dropping the oldest note shifts
            // every index in `notes` down by one. A mark that is not shifted with
            // them rewinds to the wrong note and re-renders it — which is the
            // "duplicates or loses everything after it" failure, one cursor along.
            for m in &mut self.hist_marks {
                m.note_upto = m.note_upto.saturating_sub(1);
            }
        }
    }

    /// Is this note one the reader has retired? A retired note is not rendered and nothing else
    /// changes: it stays in `notes`, [`App::notes_lines`] lists it, [`App::retired_notes`] counts it.
    ///
    /// Two ways to be retired and they are one question: the key set is the reader's act on one
    /// copy, the watermark on a kind. Both mean hidden, counted and listed, so every asker keeps
    /// asking one question.
    pub(crate) fn is_retired(&self, n: &Note) -> bool {
        self.dismissed.contains(&note_key(n)) || already_read(&self.read_marks, n)
    }

    /// **What this reader has read of one kind of notice** — the newest `ts` a `ctrl-n`
    /// covered, or `None` for a kind it has never read.
    #[cfg(test)]
    pub(crate) fn read_upto(&self, kind: &str) -> Option<u64> {
        self.read_marks
            .iter()
            .find(|(k, _)| k == kind)
            .map(|(_, ts)| *ts)
    }

    /// `ctrl-n`'s act: say how far this reader has read, per kind, then hide what that covers.
    ///
    /// Per kind and not per copy: the mark is `(code, newest ts)`, so a copy that arrives after it
    /// is read because it is not newer, whatever identity it comes under. A note with no clock is
    /// not marked, since two of them cannot be ordered and a mark of 0 would hide the second for
    /// ever; the key set handles those. Marks only move forwards.
    ///
    /// The mark is a drawing input, so moving it rebuilds what was drawn: one function does both,
    /// because a door that moved the mark alone would leave the wall on the screen.
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
            self.redraw = true;
        }
    }

    /// Put every note the mark covers into `dismissed`, and answer how many went in. One writer for
    /// the second half of the mark, called from the three moments the two records can disagree: a
    /// note being filed, a mark being set, and the key set being replaced from the file.
    ///
    /// `dismissed` stays the record everything else reads: the walk, `/notes`' marker, the `/status`
    /// count and `one_event` all ask it, and a second place for hidden is a second place to
    /// disagree. This is `<=` where the draw is `<`: the draw asks *is this news*, this asks *was it
    /// on the screen when the key was pressed*, and the mark was taken from the newest line.
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
            // The same cap `retire` keeps, for the same reason: this is the reader's memory of
            // what they have read, and an unbounded one is a list that grows for the life of
            // the box. A key that falls off is a copy whose kind the mark still covers, so the
            // note stays hidden — the cap costs a key here and nothing on the screen.
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

    /// How many of the notes this head holds are hidden right now.
    ///
    /// The number `/status` shows. Deliberately *computed from the notes* rather
    /// than kept as a counter: a counter can disagree with the screen, and the one
    /// thing a count of what is hidden may not do is be wrong.
    pub(crate) fn retired_notes(&self) -> usize {
        self.notes
            .iter()
            .filter(|(_, n)| self.is_retired(n))
            .count()
    }

    /// **How many notes this head holds and is not drawing, because they are older than
    /// the conversation it is showing** (R19).
    ///
    /// The number `/status` shows beside the count, computed from the notes for the same
    /// reason [`App::retired_notes`] is: the two are different reasons for a line not
    /// being on the screen — one the reader chose, one the window decided — and a reader
    /// who cannot tell *"I dismissed it"* from *"it happened before I attached"* will
    /// believe the wrong one.
    pub(crate) fn notes_before(&self) -> usize {
        self.notes
            .iter()
            .filter(|(place, _)| matches!(place, Placed::Before))
            .count()
    }

    /// **Retire a note, or all of them.** Returns how many were newly hidden.
    ///
    /// Written through one function so the two callers (`/notes dismiss` and
    /// `/dismiss`) cannot disagree about the three things that have to happen
    /// together: the key list, the rendered history, and the file.
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
        self.redraw = true;
        added
    }

    /// Take back what the file says is retired, not only what this head wrote: `load_prefs` runs
    /// once at startup, so a dismissal another head recorded since is otherwise invisible and
    /// `/notes` shows a retired note as live.
    ///
    /// Called where the person is looking or acting rather than on a timer: nothing needs to notice
    /// a change nobody asked about, and a read on a keypress is free while a poll loop is a poll loop.
    pub(crate) fn refresh_retired(&mut self) {
        let Some(path) = self.prefs_path.clone() else {
            return;
        };
        let (p, _) = crate::prefs::load(&path);
        if p.retired != self.dismissed {
            self.dismissed = p.retired;
            // **And what the watermark covers goes straight back in.** This replaces the
            // whole set from a file another head may have written, so a key this head added
            // in memory — one the mark, not a press, put there — would be dropped by the
            // reload and the wall it cleared would come back on the next draw. The mark is
            // the newer statement of the two, so it is re-applied rather than trusted to a
            // file that cannot hold it.
            self.hide_read();
            self.invalidate_history();
            self.redraw = true;
        }
    }

    /// True when a disclosure counter has moved past what this reader has been shown. The triangle
    /// is a pointer at `/status` and reading that screen acknowledges it; without the second half
    /// the mark is permanent, since the counters are cumulative and a head that took two resyncs
    /// carried `⚠` for the rest of its life while saying nothing new.
    ///
    /// Up to the value that was read and not a switch, so a resync after the acknowledged one is a
    /// new fact and the mark comes back: acknowledging is safe, not a way to turn the alarm off.
    pub(crate) fn alarmed(&self) -> bool {
        self.counters().exceeds(self.acked)
    }

    /// **This head's six disclosure counters, as one value** — the shape the alarm and its
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

    /// **The reader has read the numbers; stop pointing at them.**
    ///
    /// Called when `/status` opens, and only then: the screen is where the counters are read, so
    /// the act of reading it is the acknowledgement. Nothing is reset — the screen keeps showing
    /// the raw values, `/status` still lists them, and a counter that moves again starts the
    /// conversation over.
    pub(crate) fn acknowledge_counters(&mut self) {
        self.acked = self.counters();
    }

    /// **Move the counter that belongs to an edge-bound code**, or say this head has none.
    ///
    /// One place, so the arm that handles a `Warning` and the test that checks every
    /// `ALARM_ONLY` row is registered both read the same table. `false` is the case worth
    /// having: the tree says a code belongs on the triangle and this head has nowhere to put
    /// it, which the arm above then *says* rather than swallowing — a note that reaches
    /// neither the record nor a counter is a note nobody has.
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

    /// Take down the notes whose sentence has been on the screen long enough, on this head's clock:
    /// the codes in [`letibot_sessionlog::warning::FLEETING`]. See [`FLEETING_MS`] for the duration.
    ///
    /// It returns whether anything went, so the caller rebuilds once: a note is a seam and the walk
    /// is incremental, so un-drawing a line means rebuilding, and without the answer this would
    /// rebuild on every frame after the deadline.
    ///
    /// Spent, not retired: a retired key is written to `head.toml` as the reader's memory and a
    /// timer may not write that file, so this leaves [`App::notes`] rather than joining
    /// [`App::dismissed`]. The standings on the bottom edge are showing the fact now.
    pub(crate) fn fade_fleeting_notes(&mut self) -> bool {
        let before = self.notes.len();
        self.notes.retain(|(_, n)| !past_its_time(n, self.now_ms));
        before != self.notes.len()
    }
}

/// How a save treats the retired set, because one write cannot express both verbs: a dismissal
/// asserts a key is retired and a restore asserts it is not, which is a removal.
///
/// A union adds and never removes, so it cannot restore; a replacement can do both but would
/// discard keys another head retired since this one loaded. The verb decides the write, and the
/// call site says which it is rather than passing a bool that could be passed by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetiredWrite {
    /// Add to the file's set. A dismissal is an assertion no other head's save may contradict.
    Union,
    /// Take this head's set as the whole truth. A restore is an assertion of removal.
    Replace,
}

/// **The six disclosure counters, as one comparable value** — R51 item 17.
///
/// A struct rather than six arguments, because the alarm and its acknowledgement have to agree
/// about WHICH numbers count, and a list spelled out at two call sites is a list that grows at one.
/// The doc that used to sit on `alarmed`'s six-term sum already says what each one is; this is the
/// same six, named once so `exceeds` can be the single definition of *has anything moved*.
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
    /// **Has any counter moved past what was acknowledged** — the alarm's whole question.
    ///
    /// Per counter, so acknowledging is *I have seen `2 resyncs`* and not *stop telling me about
    /// resyncs*: the third one exceeds the second and the mark returns. A counter that somehow went
    /// BACKWARDS (a resync that cleared the state) is not news, and an alarm that fired on a
    /// decrease would be a mark nobody could ever clear.
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

/// Where a note sits, or whether it sits in the conversation at all.
///
/// A warning is how a head shows a fact once, and a head that has just attached has shown nothing,
/// so replaying a snapshot's warnings drew a wall of red above a conversation they did not
/// precede. The distinction is not age: a snapshot's warnings are prior, and the head was not
/// there, so they have no seam to be drawn at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placed {
    /// At this many rows: the seam the walk puts it back into.
    Seam(usize),
    /// **Before this window.** Reachable — `/notes` lists it, `/status` counts it, and a
    /// reader who wants it is one verb away — and not drawn, because it is not news.
    Before,
}

/// Something that happened between two transcript rows.
#[derive(Debug, Clone)]
pub(crate) enum Note {
    /// §18's post-flight assertions and §8.5's guards land here, and a guard
    /// nobody notices is a guard nobody wrote.
    Warned(Warned),
    /// **A refusal nobody made.** The harness itself could not read the call — the
    /// normaliser could not resolve the command, the host boundary refused it — so
    /// there is nothing for the operator to answer, grant or lift, and the thing
    /// that has to change is the model's next attempt.
    ///
    /// Its own variant rather than a `Warned` because the register is the message:
    /// red with a `!` says *look at this now*, and an operator who is shown that
    /// for something they cannot act on learns to stop reading the red lines. The
    /// operator's report was exactly that — *"i get what it tries to do, but it
    /// just throws up on my chat"* — about a 20-line refusal that was also already
    /// on the screen as the tool's own result, one row up.
    NotRun(Warned),
    /// §13.2b: a settled decision *"renders as its outcome, not as an open
    /// prompt"* — and not as nothing either, which is what it rendered as before.
    /// A tool that was refused has to look refused.
    ///
    /// Boxed because a decision is the widest thing a note carries and a session keeps every
    /// note it has ever filed.
    Decided(Box<SettledDecision>),
    /// **A question the person answered** — a faint line, and not a verdict.
    ///
    /// [`Note::Decided`] is rano's decision note, and its four words are a
    /// *permission's*: `allowed`, `REFUSED`, `cancelled`, `NOT ANSWERED — the deadline
    /// decided it`. A question has no ladder and no verdict, so none of the four fits
    /// it — and the daemon's settled event says `Cancelled` for one, because the wire's
    /// `DecisionOutcome` is a permission vocabulary and the alternative was inventing
    /// an option id nothing chose (`letibot-harnessd`'s `Answers::ask_question`).
    ///
    /// So a person who chose `sqlite` would be shown `REFUSED (sqlite)` for the answer
    /// they gave, and `cancelled` if the daemon had its way. What this head knows and
    /// the wire does not is **which kind it was** (`SettledDecision::kind`, which the
    /// view now carries), so a question gets its own row: what was asked, who
    /// answered, and what they said — in the faint register, because nothing here is
    /// waiting on anybody.
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
    /// **A pane's ending, as a row that is still there a minute later.**
    ///
    /// The defect this exists for, in the operator's words: *"i typed `!term mc`, it
    /// flashed and was gone"*. An ending used to be said as a **notice** — [`App::say`],
    /// which lives for `NOTICE_MS` of wall time — and a program that dies at once is over
    /// before the eye reaches the rectangle: the conversation came back, one sentence
    /// appeared and faded, and what the program had printed went with the pane. A person
    /// who looked a minute later saw nothing at all, and a person who came back to the
    /// session saw less than that.
    ///
    /// So an ending is a disclosure like every other one this head has, and this head's
    /// disclosures are notes: a row in the conversation, at the seam where it happened,
    /// retirable with `ctrl-n`, listed by `/notes`.
    Pane {
        /// The line that was run, verb included — *what ended* is half the fact.
        line: String,
        /// **The last rows the program left on the screen**, in order and with the blank
        /// ones dropped. See [`TermPane::last_rows`] for why the screen and not the bytes,
        /// and why the tail.
        said: Vec<String>,
        /// The daemon's own sentence: the exit status, or the operator's act. Never
        /// guessed here — see [`ServerFrame::TermEnded`].
        reason: String,
        /// **This head asked for the end and the operator confirmed it** — this head's own
        /// record that it sent [`Action::TermClose`], and not a reading of the reason's wording.
        ///
        /// **`closed` and not `left`.** Leaving is now a detach — `ctrl-\` sends nothing at all
        /// — so a row that said *left* about an ending would name the act that does not end
        /// anything. The three endings this can be true of are the deliberate one (this flag),
        /// the program's own exit (false), and a refusal to start (false, and it is a `×` for
        /// the same reason: it is the answer to what the operator just typed).
        closed: bool,
    },
}

impl Note {
    /// **A settled decision, as the note it deserves.**
    ///
    /// One function because the live arm ([`crate::app::events::asks`]) and the snapshot
    /// arm ([`crate::app::events::apply`]) both file this, and the tree has been bitten
    /// before by those two disagreeing: a snapshot that made a different note than the
    /// head's own would put a different row on a resumed screen for the same fact.
    ///
    /// A **question** gets [`Note::Answered`] — no ladder, no verdict, and the daemon's
    /// own event for one says `Cancelled` because `DecisionOutcome` is a permission's
    /// vocabulary. Everything else is rano's decision note, which is where the four
    /// permission words live.
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

/// **The kind of notice a note is**, for the watermark — its warning code, or `None` for the
/// disclosures that are not a stream of the same thing happening again.
///
/// `Warned` and `NotRun` are the two that arrive by the hundred (a retry ladder, a store that
/// will not lock), and they are the two that carry a clock. A settled decision, an answer and a
/// pane's ending are each about one request or one program — there is no *more of the same* to
/// be older than — so they are left to the key set, which is the right instrument for them.
pub(crate) fn note_kind(n: &Note) -> Option<&str> {
    match n {
        // **The FAMILY, not the code** — a refusal's code carries its request id
        // (`denied:<request id>`, `app::events::asks`), because that id is the notice's identity:
        // the row leads with it and `/gate` finds the request by it. What kind of thing it is —
        // *a call somebody decided to refuse* — is `denied`, and that is what a count counts and
        // what `ctrl-n` means by *I have read refusals up to here*. The rule is
        // `letibot_sessionlog::warning::family`'s, which `class` already reads.
        Note::Warned(w) | Note::NotRun(w) => Some(letibot_sessionlog::warning::family(&w.code)),
        Note::Decided(_) | Note::Answered { .. } | Note::Pane { .. } => None,
    }
}

/// **When a note happened, by the session log's clock** — the value a watermark compares.
///
/// `0` is a note that happened on the socket rather than on the log (`App::unreadable`,
/// `log_gap`), and it is deliberately returned as itself: the caller must not treat two notes
/// with no clock as ordered. See [`App::mark_read`].
pub(crate) fn note_stamp(n: &Note) -> Option<u64> {
    match n {
        Note::Warned(w) | Note::NotRun(w) => Some(w.ts),
        _ => None,
    }
}

/// **Has this reader read this note's kind past it?**
///
/// The watermark's whole question, as a free function so that the drawing walk — which holds
/// the head's fields destructured and cannot call a method — asks exactly the question
/// [`App::is_retired`] asks.
///
/// **Older than the newest one read, and not merely as old.** A notice born in the same
/// millisecond as the mark is NEWS, and the choice is not a coin: the log's clock is
/// milliseconds, the head cannot order two events inside one of them, and drawing loses
/// nothing while hiding loses a notice that will never come back. The copies that *were* on
/// the screen at the press are covered by the key set and by [`covered_by_mark`]'s inclusive
/// rule — `ctrl-n` writes both — so the equality case costs nothing it was meant to catch.
///
/// **`ts == 0` is never read**, for the same reason one step further: a note with no clock
/// happened on the socket rather than on the log (`App::unreadable`, `log_gap`), two of them
/// cannot be ordered at all, and a mark could only hide the second one for ever. A kind this
/// reader has never read has no row here, and then nothing is older than it — which is what
/// keeps a warning kind that did not exist when the mark was set visible.
pub(crate) fn already_read(marks: &[(String, u64)], n: &Note) -> bool {
    read_against(marks, n, false)
}

/// **Is this note at or before its kind's mark?** — the same question for the COPIES this head
/// holds, and `<=` where [`already_read`] is `<`.
///
/// [`App::hide_read`]'s docstring is the argument for the asymmetry: a note at the mark's own
/// instant *was* on the screen when the mark was taken, so the copy is read; a note that
/// arrives at that instant afterwards is news, so the draw draws it.
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

/// **What a note is called when a reader wants to retire it.**
///
/// One identity per disclosure, and it has to survive the two things that used to
/// replant the wall — a resync and a restart. So it is built from the note's own
/// facts and from nothing about where it is on the screen:
///
/// * a warning is `(code, ts)` with the detail hashed. `ts` is the log's clock for
///   the envelope that carried it, which is what [`App::note`] already uses to tell
///   one announcement from a redelivery of the same one — the same identity, for
///   the same reason.
/// * a settled decision is its `req_id`, which is the id the daemon recorded the
///   decision under and the one `/gate` takes.
///
/// **The detail is hashed**, and that is not decoration: a warning's detail can be
/// paragraphs long, and the key is written into `head.toml` as one comma-separated
/// value. `("code|ts|hash", …)` is a line a person can still read and edit. The
/// hash is FNV-1a, which is not a security boundary here — it distinguishes an
/// incident from its neighbours, and two notes that collide on code, second and
/// hash are the same sentence at the same instant.
pub(crate) fn note_key(n: &Note) -> String {
    match n {
        Note::Warned(w) => format!("w|{}|{}|{:016x}", w.code, w.ts, fnv1a(&w.detail)),
        Note::NotRun(w) => format!("n|{}|{}|{:016x}", w.code, w.ts, fnv1a(&w.detail)),
        Note::Decided(d) => format!("d|{}", d.req_id),
        Note::Answered { req_id, .. } => format!("a|{req_id}"),
        Note::Pane {
            line, said, reason, ..
        } => format!("t|{line}|{reason}|{:016x}", fnv1a(&said.join("\n"))),
    }
}

/// **Is this announcement already one this head holds?**
///
/// One identity for a disclosure, and it is [`note_key`]'s rather than a second rule: the
/// same announcement has to mean the same thing to the walk that draws it, the listing
/// that numbers it and the file that remembers it was retired. Asked in two places —
/// [`App::note`], so a redelivery is not filed twice, and [`App::load`], so a note the
/// snapshot carries is not planted a second time beside the head's own copy of it (R19).
pub(crate) fn holds(notes: &[(Placed, Note)], n: &Note) -> bool {
    let k = note_key(n);
    notes.iter().any(|(_, o)| note_key(o) == k)
}

/// FNV-1a, 64-bit: the offset basis and prime, and nothing else.
///
/// Hand-rolled rather than taken from `std`'s hasher, which is **not** stable
/// across releases — and a key that changes when the head is rebuilt would resurrect
/// every note the operator had retired, which is the exact defect this exists for.
pub(crate) fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// **How long a status notice stays on the screen** — in wall-clock milliseconds.
///
/// **1600 is the number that was already in effect, measured rather than chosen.** The
/// old countdown was 60 *frames*, and on the live head that was 1.6 s of wall time (about
/// 38 loop passes a second on an idle screen). So this changes the **unit** and not the
/// behaviour: the same sentence stays for the same second and a half, on a busy screen as
/// on a quiet one.
///
/// **Why the unit matters.** A TTL counted in frames is a timer that stops when the
/// frames stop — which is exactly when a notice is left standing longest. It was six
/// seconds on a head woken ten times a second, instant under `--replay`, and — because
/// the old body was guarded on a positive count — *permanent* for a notice whose count
/// had already reached zero. That is the shape R13 already fixed once for the elapsed
/// time of a running call: the clock belongs to the wall, not to the render loop.
///
/// leticl's `+notice-ttl-ms+` is the same 1600, reached the same way from the same 60
/// frames; see its `chrome.lisp` for the measurement. The two heads must not drift here,
/// because the operator reads the same sentence for the same length of time on both.
pub const NOTICE_MS: u64 = 1_600;

/// **How long a fleeting note stays in the conversation** — the codes in
/// [`letibot_sessionlog::warning::FLEETING`], which is the rule about *which* ones and why.
///
/// **Thirty seconds, and the number is the operator's**: *"hide it to near triangle with the
/// current queue stats — how many in review how many being merged etc, do it after time, say
/// 30 seconds"*. A duration and not a frame count, for [`NOTICE_MS`]'s reason exactly: a
/// countdown of frames stops when the frames stop, which on a quiet screen is the moment a
/// sentence is left standing longest.
///
/// # Why a timer is allowed here at all
///
/// **Because the fact does not go with the sentence.** `merge_queued` says *your child's
/// branch is in the merge queue*, and the same frame that takes the row down carries the
/// queue's standings on the bottom edge — how many entries are in review, how many are being
/// merged, how many are stuck, how many landed today — and the queue pane names every entry and
/// its branch. So the row is the news and the edge is the standing fact; a head with no second
/// place to read it must not put a code in that table, which is the argument written on it in
/// `letibot_sessionlog::warning`.
///
/// **Measured from the announcement's own `ts`, not from the frame that drew it** — the log's
/// clock for the envelope that carried it, which is the same wall clock this head's
/// `now_ms` is read from (`letibot_sessionlog::event::now_ms`, and the daemon is on this box
/// behind a unix socket). It is the same field [`note_key`] already treats as the
/// announcement's identity, and it is the same comparison the decision card's ladder makes
/// against `asked_ts`. The alternative — stamping each note as it is filed — would need a
/// second clock kept beside every note and would let a redelivery restart a sentence that has
/// already been read.
///
/// One constant, in one place, so that changing the decision is changing this line.
pub const FLEETING_MS: u64 = 30_000;

/// **Is this note past the time it is drawn for?** The `FLEETING` codes only, and the class
/// table is what keeps the set honest (see its guard): a `Failure` or a `Refused` is never
/// here, so a fault can never be on a timer.
///
/// Both `Warned` and `NotRun` carry a warning — the second is a refusal's register, and a
/// code that is in the table is answered the same way whichever register it was drawn in.
pub(crate) fn past_its_time(n: &Note, now_ms: u64) -> bool {
    let w = match n {
        Note::Warned(w) | Note::NotRun(w) => w,
        _ => return false,
    };
    letibot_sessionlog::warning::is_fleeting(&w.code) && now_ms >= w.ts.saturating_add(FLEETING_MS)
}

/// **How long a burst lasts** — the window in which notices of one kind become one counted row.
///
/// **A minute, and the number is the daemon's own tick.** The queue's worker passes every
/// `REVIEW_PASS_SECS` (30 s), so two consecutive passes of one incident are one burst and the
/// third is not; a daemon restart re-settles everything it restored in one wake; and a host that
/// dies takes a whole review queue down inside one pass. What the window must NOT be is long
/// enough to fold a morning's work into a count — twenty children in an afternoon are twenty
/// rows, and they are history, not a wall.
///
/// **Chained, not anchored.** A member joins the burst when it is within the window of the
/// member before it, so a slow drip keeps extending one row rather than starting a new one every
/// minute. The chain is per KIND and ignores the notes of other kinds between its members,
/// because the wall the operator photographs is exactly the interleaved case: a `queue_blocked`
/// and a `queue_branch_kept` alternate and both are one row each.
pub const COALESCE_MS: u64 = 60_000;

/// **Codes that are re-said while they still stand, and so are never coalesced.**
///
/// The brief's own rule: *do not coalesce a notice that is still live and changing with ones
/// that are done.* A burst is a count of things that HAPPENED; a line that says *this is still
/// true* is not one of them, and folding it into a count of finished events would turn a
/// standing condition into a number nobody can act on.
///
/// `store_busy_wait` is the one code in this tree that is re-said on a clock while its condition
/// holds — `Harness::persist` says it at most every 120 s, with the seconds waited in the
/// sentence (`crates/harnessd/src/harness.rs`). Every other code that repeats does so once per
/// event and is guarded at its raise site (see `burst_at`'s tests and the measurement in the
/// branch's note).
pub const LIVE: &[&str] = &["store_busy_wait"];

/// **The plural a burst of this code is counted in** — `5 reviews could not run` rather than
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
    ("boundary_absent", "sessions that opened unconfined"),
    // A refusal's code carries its request id (`denied:<id>`); the family is what a count of them
    // is counted in, and each member's own id is on its line in the popup.
    ("denied", "calls the gate refused"),
];

/// The phrase a burst of `code` is counted in — its own, or the code itself.
pub(crate) fn counted_as(code: &str) -> &str {
    COUNTED_AS
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, words)| *words)
        .unwrap_or(code)
}

/// **May this note join a burst, and under which kind and clock?**
///
/// `Warned` and `NotRun` only, and `note_kind` is the same test `/notes` and the watermark use —
/// a settled decision, an answer and a pane's ending are each about one request or one program,
/// so there is no *more of the same* for them to be counted with.
///
/// **`ts == 0` is never coalesced.** Those are the notes that happened on the SOCKET rather than
/// on the session's log (`unreadable_frame`, `log_gap`), and [`already_read`]'s docstring has the
/// argument for why two of them cannot be ordered — which is also why they cannot be put in a
/// window. A burst is a claim about when things happened, and these have no when.
pub(crate) fn coalescable(n: &Note) -> Option<(&str, u64)> {
    let kind = note_kind(n)?;
    if LIVE.contains(&kind) {
        return None;
    }
    let ts = note_stamp(n)?;
    (ts > 0).then_some((kind, ts))
}

/// **Is `b` the next member of `a`'s burst?** — the same kind, and inside [`COALESCE_MS`].
///
/// A clock that went BACKWARDS is a member, not a new burst: `saturating_sub` answers `0`, and
/// two notices that cannot be ordered are one incident as far as a count is concerned.
pub(crate) fn chains(a: &Note, b: &Note) -> bool {
    match (coalescable(a), coalescable(b)) {
        (Some((ka, ta)), Some((kb, tb))) => ka == kb && tb.saturating_sub(ta) <= COALESCE_MS,
        _ => false,
    }
}

/// **Is this note one the reader has retired?** — [`App::is_retired`] as a free function, for the
/// walk and for [`burst_at`], which hold the head's fields destructured.
pub(crate) fn note_retired(dismissed: &[String], marks: &[(String, u64)], n: &Note) -> bool {
    dismissed.contains(&note_key(n)) || already_read(marks, n)
}

/// The `ts` of note `j` when it may chain with `kind` — drawn, coalescable, the same kind.
fn chained_ts(notes: &[(Placed, Note)], j: usize, kind: &str) -> Option<u64> {
    if !matches!(notes[j].0, Placed::Seam(_)) {
        return None;
    }
    match coalescable(&notes[j].1) {
        Some((k, ts)) if k == kind => Some(ts),
        _ => None,
    }
}

/// **The first member of the burst `i` belongs to** — the chain walked backwards.
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

/// The last member of the burst `i` belongs to — the chain walked forwards.
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

/// **What the walk does with one note**, as far as a burst is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Burst {
    /// A member of a burst whose counted row has already been drawn — draw nothing.
    Drawn,
    /// Draw the note as it always was: it is not coalescable, or it is alone.
    Single,
    /// **The head of a burst of two or more** — the indices of the live members, oldest first.
    Counted(Vec<usize>),
}

/// **Is this note the one that draws, and how many does it stand for?**
///
/// The row belongs to the **first live member** of the chain, not to the chain's first member:
/// the reader can retire one notice with `/notes dismiss N` and the others must still be on the
/// screen. A chain whose head is retired is drawn at its next member, and a chain of one live
/// member is not a burst at all — it is the notice's own card, byte for byte what this head drew
/// before there were bursts.
pub(crate) fn burst_at(
    notes: &[(Placed, Note)],
    dismissed: &[String],
    marks: &[(String, u64)],
    i: usize,
) -> Burst {
    let Some((kind, _)) = coalescable(&notes[i].1) else {
        return Burst::Single;
    };
    // **The chain's own members, and not every index between its ends** — the notes of other
    // kinds are interleaved with them (that is the wall the operator photographs: a
    // `queue_blocked` and a `queue_branch_kept` alternating), and an index range would count a
    // neighbour as a member.
    let live: Vec<usize> = (burst_head(notes, i)..=burst_end(notes, i))
        .filter(|&j| chained_ts(notes, j, kind).is_some())
        .filter(|&j| !note_retired(dismissed, marks, &notes[j].1))
        .collect();
    if live.first() != Some(&i) {
        return Burst::Drawn;
    }
    if live.len() < 2 {
        return Burst::Single;
    }
    Burst::Counted(live)
}
