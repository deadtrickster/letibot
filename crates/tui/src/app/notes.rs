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
        // **A warning is announced once, so it is noted once.**
        //
        // The same `Warning` can reach a head twice: `adopt` plants everything the
        // snapshot carries at anchor 0, and the live arm anchors at the CURRENT
        // end of the transcript. Two copies of one announcement, in two places —
        // and the second one sits under the conversation, so new rows arrive
        // beneath it and it reads as pinned to the bottom. The operator, on three
        // of them: *"sometimes new messages come under those three but then those
        // three again pinned to the bottom, sometimes they just stay pinned"*.
        //
        // Identity is `(code, detail, ts)`: `ts` is the log's own clock for the
        // envelope that carried it, so the same announcement has the same one by
        // whichever route it arrives, and two genuinely separate warnings that
        // agree on all three are the same sentence at the same instant — which a
        // reader cannot tell apart either, and should not be shown twice.
        //
        // The FIRST anchor wins. A warning belongs where it happened, and the
        // later arrival is a redelivery rather than a new event.
        //
        // The identity is [`note_key`]'s, and it is one rule for the whole head: the
        // same three facts (code, the log's `ts`, and the detail) that `/notes dismiss`
        // retires a note by and that [`holds`] asks a snapshot against. A note that a
        // *snapshot* already carries is not filed a second time at a live seam either —
        // the snapshot's copy is the older statement of the same fact (R19).
        if holds(&self.notes, &n) {
            return;
        }
        self.file_note(n);
    }

    /// **Put a note in the conversation**, with the bookkeeping and without the identity test.
    ///
    /// The split exists for [`Note::Pane`], which is the one disclosure that cannot be
    /// *redelivered* and so must not be deduplicated: a pane's ending is filed by the head that
    /// took the pane, the pane is taken on the first `TermEnded`, and the second `!term mc`
    /// that exits 7 with the same sentence on its screen is a **second ending** and not the same
    /// one announced twice. [`note_key`] cannot tell those two apart — they are the same line,
    /// the same reason and the same rows — so the rule that protects a warning from a snapshot
    /// would silently swallow every repeat of a pane that dies at once, which is the defect
    /// this variant exists for.
    pub(crate) fn file_note(&mut self, n: Note) {
        let at = self.items.len();
        self.notes.push((Placed::Seam(at), n));
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

    /// **Is this note one the reader has retired?**
    ///
    /// A retired note is not rendered, and nothing else about it changes: it stays
    /// in `notes`, [`App::notes_lines`] lists it with its text, and
    /// [`App::retired_notes`] counts it. See [`App::dismissed`].
    pub(crate) fn is_retired(&self, n: &Note) -> bool {
        self.dismissed.contains(&note_key(n))
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

    /// **Take back what the file says is retired, not only what this head wrote.**
    ///
    /// `load_prefs` runs once, at startup. A head that has been up for hours has a
    /// `dismissed` that only ever grew from its own presses — so a dismissal another head
    /// recorded since is invisible, and `/notes` shows a note that is retired on disk as
    /// live on the screen. The file is the durable record (`load_prefs`' own comment says
    /// so), and reading it is what makes that true rather than a claim.
    ///
    /// Called where the operator is looking or acting — the listing and the chord — rather
    /// than on a timer: nothing here needs to notice a change nobody has asked about, and a
    /// stat-and-read on a keypress is free while a poll loop is a poll loop.
    pub(crate) fn refresh_retired(&mut self) {
        let Some(path) = self.prefs_path.clone() else {
            return;
        };
        let (p, _) = crate::prefs::load(&path);
        if p.retired != self.dismissed {
            self.dismissed = p.retired;
            self.invalidate_history();
            self.redraw = true;
        }
    }

    /// True when a §13.2b disclosure counter has moved **past what this reader has been shown**.
    ///
    /// R51 item 17: the triangle is a POINTER at `/status`, and reading that screen acknowledges
    /// it. Without the second half the mark is permanent — the counters are cumulative and start
    /// at zero with the process, so a head that took two resyncs carried `⚠` for the rest of its
    /// life while saying nothing new, and the operator asked the only question available:
    /// *"how to hide that resync counter arrow?"*
    ///
    /// **Up to the value that was READ, and not a switch.** A resync *after* the one that was
    /// acknowledged is a new fact about this head, so the mark comes back — which is what makes
    /// acknowledging safe rather than a way to turn the alarm off and forget it.
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
}

/// **How a save treats the retired set** — because one write cannot express both verbs.
///
/// A dismissal asserts that a key **is** retired. A restore asserts that the set is **not**,
/// which is a removal. A union adds and never removes, so it can express the first and cannot
/// express the second; a replacement can express both, but applying it to a dismissal would
/// discard every key another head had retired since this one loaded — which is the operator's
/// original report (*"i dismissed letibot notes but they stay"*).
///
/// So the verb decides the write, and the call site says which it is rather than a bool that
/// could be passed by accident.
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

/// **Where a note sits, or whether it sits in the conversation at all.**
///
/// `head-parity-2026-09-21.md` **R19**, the operator's ruling of 2026-09-22. A warning is
/// *how a head shows a fact once*; a head that has just attached has shown nothing, so a
/// snapshot's warnings were being replayed as though they had just happened — at position
/// 0, above a conversation they did not precede. The operator restarted a head and was met
/// by twelve red lines: *"i dont want to see that on restart."*
///
/// The distinction is not age — a snapshot's warnings are not old, they are **prior**: the
/// head was not there. So a note filed live is anchored at a seam of this conversation and
/// a note that arrived with a snapshot has no seam to be drawn at, and the two are told
/// apart by this type rather than by a sentinel position that would be a lie the walk
/// would have to undo.
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
    Decided(SettledDecision),
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
            return Note::Decided(d);
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
