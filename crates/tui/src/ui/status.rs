//! The status lines: `/status`'s report, and the link's and a stop's one-line state.

use crate::app::*;
use crate::ui::render::{dur_human, row, row_strings};
use crate::ui::*;
use rano::agent::status as facts;

impl App {
    /// The line a head with no daemon draws, above the composer. A resident line and not a note: it
    /// is a state, true until it is not, so it cannot scroll away with the conversation. Under
    /// `LINK_IMPATIENT_MS` it draws nothing here; past it, how long and what the operator can do.
    pub(crate) fn link_line(&self, w: usize) -> Vec<String> {
        let Link::Reconnecting {
            since_ms, attempts, ..
        } = &self.link
        else {
            return Vec::new();
        };
        let out = self.now_ms.saturating_sub(*since_ms);
        // Under the threshold the drop is a blip and is drawn in the status row, which never changes
        // height: as lines of its own here it moved the conversation down and back up again.
        if self.link_blip().is_some() {
            return Vec::new();
        }
        let said = format!(
            "the daemon connection is down — trying for {} ({} attempt{}). If the \
             daemon is gone: `letibot` starts one and `letibot --status` says what is \
             on the socket. This head keeps trying either way.",
            dur_human(out),
            attempts,
            if *attempts == 1 { "" } else { "s" },
        );
        row_strings(&facts::warning(&said, w), self.cfg.palette())
    }

    /// The line a read-only attach draws, above the composer. A resident line for `link_line`'s
    /// reason, and it carries the chord as well as both versions, the direction and what still
    /// works: that is the one fact which must not be off-screen while the lock is on.
    pub(crate) fn lock_line(&self, w: usize) -> Vec<String> {
        if !self.skew_locked() {
            return Vec::new();
        }
        let said = format!(
            "read-only — this daemon speaks protocol {} and this head speaks {}: an OLDER \
             build. Reading and scrolling work and nothing is lost; nothing is sent, because \
             a command it has never heard of ends the connection. {ATTACH_ANYWAY_CHORD} \
             attaches anyway — the whole sentence is in the note at the handshake.",
            self.daemon_protocol.unwrap_or_default(),
            letibot_sessionlog::protocol::PROTOCOL_VERSION,
        );
        row_strings(&facts::warning(&said, w), self.cfg.palette())
    }

    /// The line a stood-down auto-compaction draws. A resident line for `link_line`'s reason: the
    /// warning that announced it scrolls away. It keeps the two doors apart - pre-emptive tidying is
    /// off, a turn refused for length is still compacted, `/compact` is by hand.
    pub(crate) fn compact_off_line(&self, w: usize) -> Vec<String> {
        let Some(said) = &self.auto_compact_off else {
            return Vec::new();
        };
        let said = format!(
            "{said}. The session is no longer tidied pre-emptively at the wall; a \
             turn the provider refuses for length is still compacted, and `/compact` \
             does it by hand. A fresh session, or a larger --context-window, is the \
             way past a summary that cannot shrink."
        );
        row_strings(&facts::warning(&said, w), self.cfg.palette())
    }

    /// The line a paused plan-check draws, above the composer. A resident line for
    /// `compact_off_line`'s reason, and in the warning register because a session whose board stopped
    /// asking must never be mistaken for one whose clock broke. It names the end.
    pub(crate) fn nag_pause_line(&self, w: usize) -> Vec<String> {
        let Some(until) = &self.nags_paused else {
            return Vec::new();
        };
        let said = if until.is_empty() {
            "nags PAUSED — no plan-check reaches this session; `/nag resume` turns them back on"
                .to_string()
        } else {
            format!(
                "nags PAUSED {until} — no plan-check reaches this session; `/nag resume` turns \
                 them back on"
            )
        };
        row_strings(&facts::warning(&said, w), self.cfg.palette())
    }

    /// The line a board waiting on the operator draws, for as long as any open row waits on a
    /// question: it quotes the first, counts the rest and names the key. A question card put away with
    /// Esc is said first, because it holds a running call that lapses at its deadline.
    pub(crate) fn waiting_line(&self, w: usize) -> Vec<String> {
        // Folded away with alt-h - the bottom edge counts them instead (`app/waiting.rs`).
        if self.asks_folded() {
            return Vec::new();
        }
        // The operator's own command, its card put away - the run is blocked on a
        // line and nothing on the screen said so once Esc had taken the card down.
        if let Some(p) = self.prompt.as_ref().filter(|_| self.prompt_away) {
            let asks = p
                .question
                .as_deref()
                .map(|q| format!(" asks \"{}\"", q.trim()))
                .unwrap_or_default();
            let said = format!(
                "YOUR COMMAND IS WAITING: `{}`{asks} — alt-a brings the card back, or `!send LINE`",
                p.command.trim()
            );
            return row_strings(&facts::warning(&said, w), self.cfg.palette());
        }
        let asks = self.operator_asks();
        if let Some(a) = self
            .away
            .iter()
            .find(|a| !a.is_todo() && a.session_id == self.session_id)
        {
            use letibot_sessionlog::event::OnTimeout;
            let d = &a.decision;
            let question = d.kind == "question";
            let clock = match d.deadline {
                Some(at) => format!(
                    " — {} left, then {}",
                    time_left(at.saturating_sub(self.now_ms)),
                    match d.on_timeout {
                        OnTimeout::Deny if question => "the model goes on without your answer",
                        OnTimeout::Deny => "it is denied",
                        OnTimeout::Allow => "it runs anyway",
                        OnTimeout::Ask => "the guard model decides",
                    }
                ),
                None => String::new(),
            };
            // Every other card put away here is counted too - the line named one
            // and counted only the todo questions.
            let others = self
                .away
                .iter()
                .filter(|o| !o.is_todo() && o.session_id == self.session_id)
                .count()
                - 1;
            let mut more: Vec<String> = Vec::new();
            match others {
                0 => {}
                1 => more.push("+1 more put away".into()),
                n => more.push(format!("+{n} more put away")),
            }
            match asks.len() {
                0 => {}
                1 => more.push("+1 todo question".into()),
                n => more.push(format!("+{n} todo questions")),
            }
            let more = if more.is_empty() {
                String::new()
            } else {
                format!(" ({})", more.join(", "))
            };
            // `·` before the key, so the key hint is its own segment and a button.
            let said = format!(
                "{} PUT AWAY: \"{}\"{clock}{more} · alt-a brings it back · alt-h hides",
                if question { "QUESTION" } else { "PERMISSION" },
                d.summary.trim()
            );
            return row_strings(&facts::warning(&said, w), self.cfg.palette());
        }
        // A board read and answered from here is the truth over the `asks` settings row, which was
        // written before the answer and which nothing refreshes: a question the model took back would
        // otherwise be quoted from it for ever.
        let setting = if self.todo_answered.is_empty() && !self.board_read_here() {
            self.asks_setting.as_ref()
        } else {
            None
        };
        // The row whose card is on the screen is not waiting to be opened: the line
        // said "alt-a to answer" about the card already up, and alt-a then did nothing.
        let front = self
            .open
            .first()
            .and_then(|d| d.req_id.strip_prefix(TODO_ASK))
            .map(str::to_string);
        let asks: Vec<_> = asks
            .into_iter()
            .filter(|a| front.as_deref() != Some(a.content.as_str()))
            .collect();
        if front.is_some() && asks.is_empty() {
            return Vec::new();
        }
        let said = match (asks.first(), setting) {
            (Some(first), _) => {
                let more = match asks.len() {
                    1 => String::new(),
                    n => format!(" (+{} more)", n - 1),
                };
                // `·` before the key, so the key hint is its own segment and a button.
                format!(
                    "WAITING ON YOU: \"{}\" on `{}`{more} · alt-a to answer · alt-h hides",
                    first.question.trim(),
                    letibot_sessionlog::event::todo_title(&first.content)
                )
            }
            (None, Some(setting)) => {
                format!("WAITING ON YOU: {setting} · alt-a to answer · alt-h hides")
            }
            (None, None) => return Vec::new(),
        };
        row_strings(&facts::warning(&said, w), self.cfg.palette())
    }

    /// The line a head draws while it waits for a daemon it asked to stop. A resident line for
    /// `link_line`'s reason, and it carries the elapsed time, the time left and what the daemon has
    /// answered so far: waiting and waiting-with-an-answer are different facts.
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

    /// The alarm line for the unboxed composer, the short-screen path with no border to pin a triangle to. The boxed path says it with
    /// a `⚠` in the bottom edge's corner and leaves the numbers to `/status`; here the triangle alone would be unreadable.
    ///
    /// Not all of them: no sequence numbers, verbosity,
    /// twenty-one-character session id or head id, on every frame:
    ///
    /// ```text
    /// ╰─ seq 907 · rendered 900 · filtered 1 (normal) · dropped 0 · scrubbed 0 ·
    ///    resync 0 · s-1789023464202470853 h3 ─╯
    /// ```
    ///
    /// Each was added because something was measured going wrong, which does not argue for keeping them resident. The rule is that
    /// *an absent field and a zero field must not look the same when the field is the disclosure* is about when the field is read:
    /// `/status` reads it, says `dropped 0` explicitly and says what the counter means. What stays here is a counter that has moved,
    /// in [`Role::Attention`], not the border's grey: a second colour inside a border reads as damage and this is damage. The
    /// disclosure line is ordered by how likely it is to matter and truncated from the right, with anything nonzero promoted to the front.
    pub(crate) fn status_line(&self, w: usize) -> String {
        if !self.alarmed() {
            return String::new();
        }
        let mut said = format!(
            "⚠ dropped {} · scrubbed {} · resync {}",
            self.dropped, self.scrubbed, self.resyncs
        );
        // Named, not only counted. The other three are facts about what this head
        // did with what it was given; this one is a fact about the wire, and it is
        // the only one that means "you are running two different builds".
        if self.unreadable > 0 {
            said.push_str(&format!(
                " · unreadable {} (frames this head could not read)",
                self.unreadable
            ));
        }
        // A gap is a hole in the conversation in front of the reader, so it belongs on the border and
        // not only on `/status`. `behind` and `orphan` are deliberately not here - an ordinary backlog,
        // and a body for a row already gone - and both are on `/status`.
        if self.gaps > 0 {
            said.push_str(&format!(
                " · gaps {} (events that never arrived; a resync was asked for)",
                self.gaps
            ));
        }
        said.push_str(" · /status");
        row(&facts::alarm(&said, w), self.cfg.palette())
    }

    /// `/status`: this head's own instrumentation, with what each number means. The gloss is the part
    /// the border could not carry and the reason the counters are worth keeping: `scrubbed 4` is not
    /// actionable unless a reader knows what scrubbing is.
    pub(crate) fn status_lines(&self, w: usize) -> Vec<String> {
        // The screen says what it just did: opening this acknowledged the alarm, and a mark that
        // vanishes with nothing said cannot be told from a bug. Only when there was something to
        // acknowledge - a permanent sentence about an absent mark is the furniture this file deletes.
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
        // A stood-down auto-compaction is a state, not an event, so it sits beside `session` and
        // `head` rather than among the diagnostics: a person diagnosing a session that stopped
        // compacting needs this first. Absent means the guard has not fired, which is not drawn.
        if let Some(said) = &self.auto_compact_off {
            row("auto-compact", "off".into(), said);
        }
        // A paused plan-check is the same kind of state, and the same place: the fact a
        // person diagnosing *why has nothing asked about my plan* needs first.
        if let Some(until) = &self.nags_paused {
            row(
                "nags",
                if until.is_empty() {
                    "PAUSED".into()
                } else {
                    format!("PAUSED {until}")
                },
                "The operator paused this session's idle plan-check (`/nag pause`): nothing \
                 is armed or sent while it holds, and the board still takes edits. `/nag \
                 resume` ends it.",
            );
        }
        let unread = self.rooms.unread_total();
        if unread > 0 {
            row(
                "rooms",
                format!("{unread} unread"),
                "Messages in the chat rooms after this head's last look, from the room list it \
                 asks for every ten seconds while /rooms is closed.",
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
        // The notes this head holds and how many the reader retired. Present and at zero, because
        // "retired nothing" and "does not count what it retired" must not look alike. Beside
        // `filtered` because it is the same kind of number, not a fact about what is gone.
        row(
            "notes",
            format!(
                "{} · {} retired{}",
                self.notes.len(),
                self.retired_notes(),
                // A third number, because a note may be off the screen for a third reason: the reader retired it, or it is not
                // a row's news at all (a snapshot from before this window, or an edge diagnostic). The zero case says nothing,
                // not "0 not drawn" on every attach.
                match self.notes_before() {
                    0 => String::new(),
                    n => format!(" · {n} not drawn"),
                }
            ),
            "What this head is holding: a guard that fired, a decision that settled, a \
             sentence the daemon interrupted with. A retired note is HIDDEN, and still \
             here — `/notes` lists every one with its text and `/notes restore` puts the \
             retired ones back, which is the difference between a disclosure and a \
             deletion. The session log holds them either way; a note is how a head shows \
             a durable fact once. **A note that is not drawn** is one that arrived with a \
             snapshot — it happened before this head attached, so it is listed and counted \
             rather than planted in a conversation it did not precede (R19) — or one filed \
             beside the conversation on purpose, which is where every warning's sentence goes: \
             `/notes` lists it and the conversation never draws it.",
        );
        // The two numbers the edge points at. A fault filed only in `/notes` is a fault nobody
        // read, so every refusal and every failure the daemon raises is counted here and the `⚠`
        // on the composer's edge is drawn while either has moved since the last read.
        row(
            "refused",
            self.refused.to_string(),
            "Warnings this head filed beside the conversation whose register is a refusal: the \
             answer to a line somebody typed, in the daemon's words. `/notes` lists each one; \
             this is the number the edge points at, so a refusal is never only in a list nobody \
             opened.",
        );
        row(
            "failed",
            self.failed.to_string(),
            "Warnings this head filed beside the conversation whose register is a failure: \
             something did not work and the model was not told. `/notes` lists each sentence \
             with its code, and this is the number the edge points at.",
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
        // Three rows and not one, because they are three facts about three places and a single number
        // cannot tell them apart: `gaps` is the wire losing them, `behind` is the daemon still holding
        // them, `orphan` is the wire delivering them with no row to put them on.
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
        // Present and zero, like every counter here.
        row(
            "orphan",
            self.orphan_bodies.to_string(),
            "Bodies that arrived for rows this head is not holding. The words cannot be \
             drawn — a snapshot replaced the rows and this one was not in it — so the \
             count is the only trace they leave.",
        );
        // A diagnostic and not an event in the conversation: nothing about the turn is wrong, so this
        // is a number to read when the triangle is up. The daemon's code is in the gloss because
        // leaving the note out took the only place that spelling appeared, and it is what greps work on.
        row(
            "first byte",
            self.slow_first_byte.to_string(),
            "Times the provider took longer than this head's patience to send the first \
             byte of an answer — the daemon's code for it is `model_slow_first_byte`. \
             Nothing is wrong with the turn, which is why this is a count here and not a row \
             in the conversation: the latency is a fact about the provider now, and the \
             conversation is not different because of it.",
        );

        // What the last turn's ending measured. The trap is that zero reuse is one number for
        // a slot that did not hold the history and for a prompt of ours that diverged; the check is
        // the daemon's verdict on which, and the dialect names the template it was judged under.
        row(
            "prefix",
            self.prefix_check_line(),
            "The post-flight check on the last turn's ending, as the daemon sent it. `held` with \
             a shortfall is the SERVER's cache - the prompts are proven identical over that span - \
             while `DIVERGED` is a prompt of ours that did not extend the last one, which is the \
             only verdict here that is a defect in us. `not run` deliberately does not look like \
             a pass: a check nobody ran is not a check that held. Nothing said is a daemon older \
             than the field, or a session with no ending since this head attached.",
        );
        if !self.dialect_template_sha.is_empty() {
            row(
                "dialect",
                self.dialect_template_sha.clone(),
                "The dialect's `template_sha` on the last turn's ending: the prompt's own \
                 identity. A model update that changes the template shows up here as a \
                 different dialect rather than as an unexplained cache miss.",
            );
        }

        // The names, which is the pair `orphan` was missing: that counts bodies with no row, this
        // counts rows with no body. Named and not counted because a count is not checkable, and on
        // `/status` rather than the line because a name is long and the line is trimmed to the frame.
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
        // Present and zero, like every other counter here: an absent field and a zero field must not
        // look the same, and a head that does not count these at all is a different statement.
        row(
            "unreadable",
            self.unreadable.to_string(),
            "Frames that arrived and could not be parsed. Almost always a daemon \
             newer than this head: the frames the two share read fine, and the first \
             one they do not is this. The head stays attached and says so on the \
             border once it has happened; nothing is acked for one, because nothing \
             was read.",
        );
        // The version, always present: which build is on the other end of this socket is answered
        // nowhere else on the screen and is the first thing to check when a head behaves strangely.
        // `None` reads as not told yet, which is a different statement from a version.
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
             heard of, and answers that by closing the connection — so this head holds \
             its sends read-only until `ctrl-^` says otherwise, and a restart of the \
             daemon is the fix either way.",
        );
        // Which PROCESS is on the other end, from `SO_PEERCRED` on this head's own connection - the
        // fact a `ps` was reached for after a stop that did not happen. A head does not signal it:
        // knowing which process it is and signalling it are different acts.
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

    /// The last ending's prefix verdict as one value, in the words a reader acts on: `held` with
    /// its shortfall, `DIVERGED` with the engine's sentence, `not run` with the backend's reason.
    pub(crate) fn prefix_check_line(&self) -> String {
        use letibot_sessionlog::event::PrefixCheck as P;
        match &self.prefix_check {
            None => "not told".to_string(),
            Some(P::FirstTurn) => "first turn: nothing to compare against".to_string(),
            Some(P::Held {
                expected_cached_min,
                cached,
                shortfall: 0,
            }) => format!("held: {cached} of {expected_cached_min} token(s) reused"),
            Some(P::Held {
                expected_cached_min,
                cached,
                shortfall,
            }) => format!(
                "held: short by {shortfall}, {cached} of the {expected_cached_min} token(s) it \
                 could have reused"
            ),
            Some(P::Violated { detail }) => format!("DIVERGED: {detail}"),
            Some(P::Skipped { reason }) => format!("not run: {reason}"),
        }
    }
}

/// Time left on a question's clock, in whole seconds - `45s`, `4m12s`, `1h05m`. Whole seconds
/// rather than [`dur_human`]'s tenths, because this is a countdown a person reads at a glance and
/// a figure that changes ten times a second is one nobody can read.
pub(crate) fn time_left(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}
