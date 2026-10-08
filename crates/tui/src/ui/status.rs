//! **The status lines**: `/status`'s report, and the link's and a stop's one-line state.

use crate::app::*;
use crate::ui::render::{dur_human, row, row_strings};
use crate::ui::*;
use rano::agent::status as facts;

impl App {
    /// **The line a head with no daemon draws**, above the composer and under nothing.
    ///
    /// A resident line rather than a note, and the distinction is the requirement: a
    /// note is anchored to a place in the conversation and scrolls away with it, while
    /// this is a *state* — it is true until it is not, and a head that had said it once,
    /// at the top of the scrollback, would be saying nothing about right now. That is
    /// the same argument the stuck line makes, one level up: this is why the turn went
    /// quiet.
    ///
    /// Past [`LINK_IMPATIENT_MS`] it stops saying `reconnecting` and starts saying how
    /// long, how many tries, and what the operator can do — because at that point the
    /// wait is not going to end on its own, and a person staring at a head that says
    /// `reconnecting` has no way to tell a quarter of a second from an afternoon.
    pub(crate) fn link_line(&self, w: usize) -> Vec<String> {
        let Link::Reconnecting {
            since_ms,
            attempts,
            why,
            ..
        } = &self.link
        else {
            return Vec::new();
        };
        let out = self.now_ms.saturating_sub(*since_ms);
        let impatient = out >= LINK_IMPATIENT_MS;
        let said = if impatient {
            format!(
                "the daemon connection is down — trying for {} ({} attempt{}). If the \
                 daemon is gone: `letibot` starts one and `letibot --status` says what is \
                 on the socket. This head keeps trying either way.",
                dur_human(out),
                attempts,
                if *attempts == 1 { "" } else { "s" },
            )
        } else {
            format!("the daemon connection is down — reconnecting. {why}")
        };
        row_strings(&facts::warning(&said, w), self.cfg.palette())
    }

    /// **The line a head draws while it waits for a daemon it asked to stop** (R30).
    ///
    /// A resident line and not a note, for `link_line`'s reason: it is a state, true until
    /// it is not, and the operator is by definition still looking at the screen. The
    /// requirement says *the head says what it is waiting for rather than freezing on a
    /// dead screen*, and this is that sentence — with the elapsed time, the time left, and
    /// what the daemon has done so far, because "waiting" and "waiting and it has answered"
    /// are different facts and the second one means it worked.
    pub(crate) fn stopping_line(&self, w: usize) -> Vec<String> {
        let Some(s) = self.stopping.as_ref() else {
            return Vec::new();
        };
        if s.resolved(self.now_ms) {
            return Vec::new();
        }
        row_strings(
            &facts::warning(&s.waiting_line(self.now_ms), w),
            self.cfg.palette(),
        )
    }

    /// The alarm line for the **unboxed** composer — the degenerate short-screen
    /// path, where there is no border to pin a triangle to. The boxed path says
    /// it with a `⚠` in the bottom edge's right corner and leaves the numbers to
    /// `/status`; this names them, because on a screen this small the triangle
    /// alone would be a fact with no way to read it.
    ///
    /// It used to be all of them, plus the sequence numbers, plus the verbosity,
    /// plus the twenty-one-character session id and the head id, on every frame:
    ///
    /// ```text
    /// ╰─ seq 907 · rendered 900 · filtered 1 (normal) · dropped 0 · scrubbed 0 ·
    ///    resync 0 · s-1789023464202470853 h3 ─╯
    /// ```
    ///
    /// Every one of those was added because something was measured going wrong,
    /// and none of that is an argument for keeping them resident. §13.2b's rule is
    /// that *an absent field and a zero field must not look the same when the
    /// field is the disclosure* — which is a rule about the moment the field is
    /// **read**, not about where it lives the rest of the time. `/status` is where
    /// it is read, it says `dropped 0` explicitly, and it says what the counter
    /// means, which the border never had room to.
    ///
    /// What stays here is the case a person must not have to go looking for: a
    /// counter that has moved. In [`Role::Attention`], not the border's grey,
    /// because a second colour inside a border reads as damage and this *is*
    /// damage — that was the argument for painting it grey and it was the wrong
    /// way round.
    /// The disclosure line: the read mark, what this head suppressed, what the
    /// daemon will never send, and what it stripped on the way.
    ///
    /// Ordered by how likely it is to matter, and truncated from the right, because
    /// on an 80-column terminal the old line lost `dropped`, `scrubbed` and
    /// `resync` to the ellipsis — the three numbers whose whole purpose is to be
    /// impossible to miss. Anything nonzero is promoted to the front.
    pub(crate) fn status_line(&self, w: usize) -> String {
        if !self.alarmed() {
            return String::new();
        }
        let mut said = format!(
            "⚠ dropped {} · scrubbed {} · resync {}",
            self.dropped, self.scrubbed, self.resyncs
        );
        // **Named, not only counted.** The other three are facts about what this head
        // did with what it was given; this one is a fact about the wire, and it is
        // the only one that means "you are running two different builds".
        if self.unreadable > 0 {
            said.push_str(&format!(
                " · unreadable {} (frames this head could not read)",
                self.unreadable
            ));
        }
        // **R17, and the reason it is on the border and not only on `/status`.** A gap
        // is a hole in the conversation in front of the reader: rows are missing from
        // the middle of what they are reading, and nothing else on the screen says so.
        // The two whose absence is *not* an alarm are deliberately absent here —
        // `behind` is an ordinary backlog, and `orphan` is a body for a row that is
        // already gone — and both are on `/status` where the whole set is read.
        if self.gaps > 0 {
            said.push_str(&format!(
                " · gaps {} (events that never arrived; a resync was asked for)",
                self.gaps
            ));
        }
        said.push_str(" · /status");
        row(&facts::alarm(&said, w), self.cfg.palette())
    }

    /// `/status`: this head's own instrumentation, with what each number means.
    ///
    /// The gloss is the part the border could never carry, and it is the reason
    /// the counters are worth keeping at all — `scrubbed 4` is not actionable
    /// unless you know that scrubbing is what a *late* head does to an
    /// interactive-only frame, at which point it is the answer to "why is this
    /// head quieter than the one next to it".
    pub(crate) fn status_lines(&self, w: usize) -> Vec<String> {
        // **The screen says what it just did** (R51 item 17). Opening it acknowledged the alarm,
        // and a mark that vanishes with nothing said is a mark the reader cannot tell from a bug —
        // the numbers below are unchanged, which is precisely why the sentence is owed.
        //
        // Only when there was something to acknowledge, and only while it is true: on a first read
        // of a clean head there is nothing to say, and a permanent sentence about a mark that is not
        // there is the furniture this file keeps deleting.
        let note = (self.acked != Counters::default()).then(|| {
            "the alarm is acknowledged up to the values below — the ⚠ is gone, and any \
             counter that moves again brings it back"
                .to_string()
        });
        let mut rows: Vec<facts::Fact> = Vec::new();
        let mut row = |k: &str, v: String, why: &str| {
            rows.push(facts::Fact {
                key: k.to_string(),
                value: v,
                why: why.to_string(),
            });
        };

        if !self.session_id.is_empty() {
            row(
                "session",
                self.session_id.clone(),
                "In full, because this is the form a command takes. \
                 The header shows the last eight characters, which is the part \
                 two sessions differ in.",
            );
        }
        if !self.head_id.is_empty() {
            row(
                "head",
                format!("{} · {} attached", self.head_id, self.heads.max(1)),
                "Every head on this session sees the same stream from its own \
                 read mark. Closing one does not stop the turn.",
            );
        }
        row(
            "seq",
            format!("{} · {} rendered", self.seq, self.rendered),
            "The log's monotonic, gap-free position, and how many of those events \
             reached the screen. Both counted by this head, not by the daemon.",
        );
        row(
            "filtered",
            format!("{} ({})", self.filtered, self.visibility.as_str()),
            "Events this head chose not to show at the current filter. \
             /verbosity with nothing after it shows every profile and what each gives you.",
        );
        // **R10: the notes this head holds, and how many the reader has retired.**
        //
        // Present and at zero, like every other counter here (§13.2b): "the reader
        // has retired nothing" and "this head does not count what it retired" must
        // not look the same, and the second is what every head did before this.
        // It is beside `filtered` because it is the same kind of number — a fact
        // about what was chosen NOT to be shown — and a different one from
        // `dropped`, which is a fact about what is gone.
        row(
            "notes",
            format!(
                "{} · {} retired{}",
                self.notes.len(),
                self.retired_notes(),
                // **A third number, because there is a third reason a note is not on the
                // screen** (R19): the reader retired it, or it is from before this window
                // and was never planted. Both are facts about what was chosen not to be
                // shown; they are not the same choice, and the zero case says nothing
                // rather than "0 from before this window" on every attach.
                match self.notes_before() {
                    0 => String::new(),
                    n => format!(" · {n} from before this window"),
                }
            ),
            "What this head is holding: a guard that fired, a decision that settled, a \
             sentence the daemon interrupted with. A retired note is HIDDEN, and still \
             here — `/notes` lists every one with its text and `/notes restore` puts the \
             retired ones back, which is the difference between a disclosure and a \
             deletion. The session log holds them either way; a note is how a head shows \
             a durable fact once. **A note from before this window** is one that arrived \
             with a snapshot: it happened before this head attached, so it is listed and \
             counted rather than planted in a conversation it did not precede (R19).",
        );
        row(
            "dropped",
            self.dropped.to_string(),
            "Events the daemon's bounded scrollback threw away before this head \
             asked for them. Not a rendering choice: they are gone.",
        );
        row(
            "scrubbed",
            self.scrubbed.to_string(),
            "Interactive-only frames withheld from a head that attached late — \
             partial tool output and the like, which has no durable form.",
        );
        row(
            "resync",
            self.resyncs.to_string(),
            "Times this head threw its state away and took a fresh snapshot, \
             because the gap since its read mark was past the daemon's bound.",
        );
        // **R17: the three numbers that tell a lost row from a late one** (R17).
        //
        // They are three rows rather than one because they are three different
        // facts about three different places, and the whole reason this defect
        // survived a night of measurement is that they looked the same:
        //
        // * `gaps` — the wire lost them.
        // * `behind` — the daemon still has them; they are in a queue, on a socket,
        //   or in this head's channel. Not lost, just not here. A head with a
        //   non-zero `behind` and an empty frame queue is *correct* and *not
        //   current*, which is the state nobody could name from outside.
        // * `orphan` — the wire delivered them and there is no row to put them on.
        row(
            "gaps",
            self.gaps.to_string(),
            "Times the log's seq jumped, which means events the daemon sent never \
             reached this head. Counted and said because a gap repaired in silence \
             looks exactly like a session that never had one — and then nobody learns \
             that a socket, a queue or a compaction is losing rows.",
        );
        row(
            "behind",
            self.behind.to_string(),
            "How far the daemon last said it was ahead of this head, in events. A head \
             that is behind has drawn everything it was given and has nothing to draw \
             — the same screen as a head that is current. This is the number that tells \
             the two apart, from the seq the daemon states on an `Accepted` or a \
             `Rejected`.",
        );
        // **Present and zero, like every counter here.**
        row(
            "orphan",
            self.orphan_bodies.to_string(),
            "Bodies that arrived for rows this head is not holding. The words cannot be \
             drawn — a snapshot replaced the rows and this one was not in it — so the \
             count is the only trace they leave.",
        );
        // **A diagnostic that is not an event in the conversation.** `model_slow_first_byte`
        // is the provider being slow to start answering; nothing about the turn is wrong
        // and nothing about the conversation changed, so it is a number to look at when the
        // triangle is up rather than a sentence between two messages — the operator's
        // ruling, and the rule is written down in `warning::ALARM_ONLY`.
        //
        // **The code is in the gloss, and that is not decoration.** Moving the note off the
        // screen took the one place the daemon's own name for this fact appeared — and the code
        // is the word a reader greps the session log for, so the row that replaced it owes
        // them the spelling. Every other row here is named by its code already
        // (`unreadable`, `orphan`, `gaps`); this is the one that had to move, so this is the
        // one that has to say where it went.
        row(
            "first byte",
            self.slow_first_byte.to_string(),
            "Times the provider took longer than this head's patience to send the first \
             byte of an answer — the daemon's code for it is `model_slow_first_byte`. \
             Nothing is wrong with the turn, which is why this is a count here and not a row \
             in the conversation: the latency is a fact about the provider now, and the \
             conversation is not different because of it.",
        );

        // **And the NAMES, which is the pair `orphan` was missing.**
        //
        // The operator, 2026-09-22, looking at `2 row(s) announced and never filled in` for a
        // day: *"tell me if i need to restart anything"*, and *"WHICH two rows, named — an
        // ordinal or an id, not a count, because a count is the least useful form of this
        // fact."* `orphan` counts bodies with no row; this is the other direction — rows with
        // no body — and it printed only a count for the same reason the line does.
        //
        // On `/status` rather than on the line because the line is trimmed to the frame and a
        // name is long: 80 columns hold a count and about three ids, and this pane holds all of
        // them, which is the form a reader can act on.
        let unfilled: Vec<&str> = {
            let mut ids: Vec<&str> = self
                .bulk
                .as_ref()
                .map(|b| b.ids.iter().map(String::as_str).collect())
                .unwrap_or_default();
            ids.sort_unstable();
            ids
        };
        row(
            "unfilled",
            unfilled.len().to_string(),
            &format!(
                "Rows a SNAPSHOT announced whose content never arrived. Named, because a \
                 count is not checkable: {}",
                if unfilled.is_empty() {
                    "none".to_string()
                } else {
                    unfilled.join(", ")
                }
            ),
        );
        // **Present and zero, like every other counter here.** §13.2b: an absent
        // field and a zero field must not look the same. A head that has never met a
        // frame it could not read says `0`, which is a different statement from a
        // head that does not count them at all — and the second is what every head
        // did before this bucket existed.
        row(
            "unreadable",
            self.unreadable.to_string(),
            "Frames that arrived and could not be parsed. Almost always a daemon \
             newer than this head: the frames the two share read fine, and the first \
             one they do not is this. The head stays attached and says so on the \
             border once it has happened; nothing is acked for one, because nothing \
             was read.",
        );
        // **The version, always present.** §13.2b in the other direction from the
        // counters: the question "which build is on the other end of this socket" has
        // no answer anywhere else on the screen, and its answer is the first thing to
        // check when a head behaves strangely. `None` reads as "not told yet", which is
        // a different statement from a version number — the same distinction the empty
        // transcript banner draws.
        row(
            "protocol",
            match self.daemon_protocol {
                None => "not told yet".to_string(),
                Some(d) if d == letibot_sessionlog::protocol::PROTOCOL_VERSION => format!(
                    "{d} · the same build as this head (protocol {})",
                    letibot_sessionlog::protocol::PROTOCOL_VERSION
                ),
                Some(d) => format!(
                    "{d} · this head speaks {} — {} build",
                    letibot_sessionlog::protocol::PROTOCOL_VERSION,
                    if d > letibot_sessionlog::protocol::PROTOCOL_VERSION {
                        "NEWER"
                    } else {
                        "OLDER"
                    }
                ),
            },
            "The protocol both halves were built against, compared at the handshake. \
             A NEWER daemon sends frames this build may not know: they are reported as \
             they arrive and skipped. An OLDER one cannot read a command it has never \
             heard of, and answers that by closing the connection — so a session with \
             an older daemon can end on the next thing you type, and a restart of the \
             daemon is the fix either way.",
        );
        // **Which PROCESS is on the other end of this socket** (R30). The protocol row
        // answers *which build*; this answers *which daemon*, and it is the fact the
        // operator reached for with `ps` a day after a stop that did not happen — which is
        // exactly how that orphan was found. `SO_PEERCRED` on this head's own connection,
        // so it is this daemon and not a pid out of a file that may be a predecessor's.
        //
        // A head does not signal it. Knowing which process is at the other end and
        // reaching around the protocol to signal it are different acts, and R30 keeps the
        // second out while making the first available.
        row(
            "daemon",
            match self.daemon_pid {
                Some(p) => p.to_string(),
                None => "not told (the kernel did not name the peer)".to_string(),
            },
            "The process serving this connection, from SO_PEERCRED — the same number `ps` \
             shows. `letibot --stop` asks it to stop over the protocol, and this head asks \
             with the quit card's second row: that one WAITS until the daemon has gone and \
             says so on stderr if it has not, so a stop that did not happen is not \
             something you find out a day later.",
        );
        row(
            "verbosity",
            self.visibility.as_str(),
            "What reaches the transcript at the current filter — a SET of switches, and the \
             profile is the name of one. `/verbosity` with nothing after it shows every \
             profile and what each one gives you — conversation, read-edits, terse, normal, \
             loud — and `/verbosity NAME` sets one; a switch moved off a profile reads as \
             `custom …`. It used to sit on the composer's border, which was a row of \
             attention paid for ever for a fact read once.",
        );
        if !self.wiring.workspace.is_empty() {
            row(
                "workspace",
                tilde(&self.wiring.workspace),
                "Where the daemon is standing. Tools resolve relative paths here.",
            );
        }
        let pane = facts::FactsPane {
            title: "this head".into(),
            note,
            facts: rows,
            footer: "/status or esc closes this".into(),
        };
        row_strings(&pane.lines(w), self.cfg.palette())
    }
}
