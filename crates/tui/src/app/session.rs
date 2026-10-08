//! **The head's place in the daemon**: attaching, the link and getting it back, stopping,
//! and the tree of sessions this head can switch between.

use super::*;
use crate::ui::render::dur_human;
use letibot_sessionlog::client::Unreadable;
use letibot_sessionlog::registry::short_id;
use letibot_sessionlog::view::Warned;

impl App {
    /// **Which process this head is attached to.** Called by the caller that opened the
    /// socket, once per connection — including a reconnect, where the answer can be a
    /// different process than the one before.
    ///
    /// **The head is the one holder of this fact.** `/status` reads it, the stop's farewell
    /// names it, and the driver asks the head for it rather than keeping its own copy — so
    /// there is one answer to *which process am I sending the operator to `ps` for*.
    pub fn set_daemon_pid(&mut self, pid: Option<i32>) {
        self.daemon_pid = pid;
    }

    /// The daemon's pid, or `None` when the kernel would not name the peer.
    pub fn daemon_pid(&self) -> Option<i32> {
        self.daemon_pid
    }

    /// **The stop this head is waiting on**, or `None`.
    ///
    /// `Some` from the moment the frame goes out until the head exits. Read by the
    /// renderer for its line, by the driver for the four facts it observes, and at the
    /// end by `main` for the farewell.
    pub fn stopping(&self) -> Option<&Stopping> {
        self.stopping.as_ref()
    }

    /// The same, to write the observations into. The driver is the only writer: it holds
    /// the socket, the clock and the pid, and none of those are the app's.
    pub fn stopping_mut(&mut self) -> Option<&mut Stopping> {
        self.stopping.as_mut()
    }

    /// **The head has asked, and is now waiting.** Called by the driver once, when the
    /// frame has been written (or failed to be), and never before — a head that showed
    /// this line while it had not sent anything would be lying about what it did.
    pub fn stop_began(&mut self, who: &str, sent: bool, pid: Option<i32>, now_ms: u64) {
        self.stopping = Some(Stopping {
            who: who.to_string(),
            since_ms: now_ms,
            deadline_ms: now_ms.saturating_add(STOP_DEADLINE_MS),
            sent,
            acked: false,
            closed: false,
            gone: false,
            pid,
            turn_busy: self.turn_busy(),
        });
        self.redraw = true;
    }

    /// **Whether a plain detach should be sent on the way out.** Not while a stop is in
    /// flight: the daemon's own `Detach` handling would take this head off the session it
    /// is shutting down, and the notice it publishes (`daemon_stopping`) names every head
    /// it reached. The connection is going anyway.
    pub fn wants_detach(&self) -> bool {
        self.stopping.is_none()
    }

    /// **The farewell this head owes, after the screen is gone.** R30's third part: an
    /// operator who chose *stop* and got a running daemon learns it here, once, on stderr,
    /// rather than from `ps` a day later — which is exactly how this one was found.
    ///
    /// `None` when there is nothing to say: no stop was asked for, or the daemon's process
    /// is gone and the question is answered. **A `Bye` is not one of those cases** — see
    /// below. The sentences below are four because the operator's next move differs: it did
    /// not go and a turn was running (legitimate, and it will finish); it did not go and
    /// nothing was running or it could not even be asked (the wedge, and here is the verb);
    /// or the daemon ended the connection itself, which is a different fact with its own
    /// sentence in [`App::farewell`].
    pub fn stop_farewell(&self) -> Option<String> {
        let s = self.stopping.as_ref()?;
        let secs = s.since_ms.max(self.now_ms).saturating_sub(s.since_ms) / 1000;
        // **A `Bye` is not the daemon going, on this path.** The operator was reading this
        // pair on nearly every orderly stop, four lines apart and in this order:
        //
        // ```text
        // letibot: the daemon was asked to stop and had not gone 0s later.
        //   the request was acknowledged and did not stop; the daemon is still there (pid
        //   2291248).
        //   `letibot --stop --force` finishes it — …
        // letibot: the daemon ended this head — daemon shutting down
        // ```
        //
        // The second line is the daemon saying goodbye. The first said it did not go — and
        // recommended `--force`, which *aborts in-flight turns over the protocol*, against a
        // daemon that had just left politely. The fix for that was to treat the `Bye` as the
        // answer — and **that over-corrected into the operator's second report**: *"it
        // reports the server exited within a second — while `harnessd` is in fact hung and
        // has to be killed with `--force`."*
        //
        // **The frame is published before the process ends, and by a thread that is not the
        // one being waited for.** `registry.close()` runs on the connection thread the
        // instant the `Stop` is taken; the **worker** that is running the operator's command
        // is a different thread, is inside that command, and has not ended. Measured on a
        // live daemon, 2026-10-06: `Bye` at 519 µs, the process still in `/proc`, and it
        // stayed there for the rest of the run. So the `Bye` is evidence that the request
        // was **read**, which is a real and useful fact — and it is not evidence about the
        // process, which is the only thing the operator's question is about.
        //
        // **`gone` is the observation that is about the process**, and it is not overloaded
        // to mean anything else: it is read from `waitpid` for this head's own child and
        // from `/proc` — zombie-aware — for anybody else's. It goes on meaning exactly what
        // its docstring says, and it is now the only thing that silences this sentence.
        //
        // The line below used to read `if s.gone || self.bye.is_some() { return None; }`,
        // so the arrival of the goodbye **silenced the farewell** — the head left saying
        // nothing, which a person reads as *it stopped*.
        //
        // The wait is no longer cut short (see [`App::should_quit`]), so this composes its
        // sentence when the question is actually settled: the process is gone, or the
        // deadline passed with it still there. A `Bye` then makes the sentence **stronger**
        // rather than quieter — the daemon did hear the request and did begin shutting
        // down, and its process is still in `/proc` — which is the fact a person needs in
        // order to know that `--force` is the right next move and not a workaround for a
        // lie.
        if s.gone {
            return None;
        }
        let pid = pid_word(s.pid);
        let ask = if self.bye.is_some() {
            "was acknowledged, and the daemon began shutting down"
        } else if s.acked {
            "was acknowledged and did not stop"
        } else if s.sent {
            "was sent and never acknowledged"
        } else {
            "could NOT be sent"
        };
        let because = if s.turn_busy {
            // **And a round can be inside a command.** The daemon has one worker and a
            // round calls its tools on that worker, so a stop that arrives mid-`! sudo apt
            // install mc` waits for that command — whose own deadline is what ends it, and
            // which is two minutes at the default. That is the difference between a stop
            // that is merely slow and one that has to be forced, and the operator is the
            // only one who can tell which they are looking at.
            "A turn was running, and the daemon finishes its round before it stops — and a \
             round can be inside a command, whose own deadline is what ends it. This is a \
             slow stop rather than a refused one."
        } else {
            "No turn was running, so there was nothing for it to finish."
        };
        Some(format!(
            "the daemon was asked to stop and had not gone {secs}s later.\n  \
             the request {ask}; the daemon is still there ({pid}).\n  {because}\n  \
             `letibot --stop --force` finishes it — it aborts in-flight turns over the \
             protocol, then signals, and says `NOT stopped` if the process survives."
        ))
    }

    /// **Why the daemon ended this, for after the screen is given back.**
    ///
    /// `Some` only when a [`ServerFrame::Bye`] arrived — an ordinary quit has
    /// nothing to say. The caller prints it once the terminal is restored, because
    /// anything said into the transcript goes down with the alternate screen and
    /// the operator is left with a head that exited for no stated reason. That is
    /// what a protocol skew looked like on 2026-09-20: *"when i went to job with
    /// enter in pfn project leticode just exited"*.
    pub fn farewell(&self) -> Option<&str> {
        self.bye.as_deref()
    }

    pub fn head_id(&self) -> &str {
        &self.head_id
    }

    /// The next door call's number, for the `call_id` the daemon keys its pending set on.
    pub fn next_head_run(&mut self) -> u64 {
        self.head_run_seq += 1;
        self.head_run_seq
    }

    /// Tell the head it is about to ask the daemon, so the frames it draws in the
    /// meantime tell the truth.
    ///
    /// Called once, by a head that has taken the screen and is about to attach. See
    /// [`Self::attaching`] for what it suppresses and why.
    pub fn begin_attach(&mut self) {
        self.begin_attach_at(self.now_ms);
    }

    /// The same, with the clock stated rather than read, for a caller that knows it —
    /// and for a test, which has no clock.
    pub fn begin_attach_at(&mut self, now_ms: u64) {
        self.attaching = true;
        self.attach_started_ms = now_ms;
    }

    /// The head id the daemon just seated this connection with, once.
    ///
    /// The driver hands it to the client. It is `take`n rather than read because a
    /// client that re-applied a stale one would ack into the session it left.
    pub fn take_seated(&mut self) -> Option<String> {
        self.seated.take()
    }

    /// How this session should be named on a screen: its title, or a short id.
    ///
    /// **Never the full id.** `s-1788987496351498881` is twenty-one characters of
    /// which the first thirteen are the same for every session minted on the same
    /// afternoon — it costs a fifth of an eighty-column header to say almost nothing,
    /// and the part that distinguishes two sessions is the part that gets cut when
    /// the header runs out of room. The last eight characters are where they differ,
    /// so that is what is shown.
    ///
    /// The full id is still reachable: it is on its own line under every named row in
    /// the picker, and `letibot --sessions` prints it in full. A label is for
    /// recognising a session; an id is for naming one to a command, and those are
    /// different jobs done in different places.
    /// **The picker's rows, as ONE enumeration — every key and the drawing read this.**
    ///
    /// # Why one function and not a filter at each reader
    ///
    /// Five things index a session list: the picker's arrows and Enter, its seeding, the row a
    /// typed number names, and the header's position. Working out *which row is that* separately
    /// for a nested list is the **two-enumerations defect** and this file already carries the scar:
    /// [`App::todos_stops`], whose docstring is the operator's own two reports — *"arrows dont go
    /// here"* and *"mouse doesnt click"* — two symptoms of one cursor whose position came from one
    /// list and whose row came from another.
    ///
    /// # The shape
    ///
    /// A conversation is depth 0 and keeps the daemon's own order. A sub-session sits directly
    /// under the session that spawned it, **when that session is expanded** — `parent_session_id`
    /// is already on every row, so nothing had to be added to the wire for this.
    ///
    /// **The chain down to the session you are IN is always shown**, whatever the collapse state:
    /// a picker that hides where you are is a picker that cannot answer *where am I*, which is the
    /// one question the header above it exists to answer.
    ///
    /// # What the filter this replaces was protecting against
    ///
    /// Not curiosity — noise. Collapsed-by-default is that concern answered instead of obeyed, and
    /// the operator's own words are the reason the OBEDIENCE was wrong: *"yes subagents are not
    /// even scratch session they are session, just sub sessions"*, and *"why readonly? subagent
    /// session is more like you driving others via tmux"*. A session a head can post to, and get an
    /// answer from, is not a row to be filtered — it is a session to be driven.
    ///
    /// # Inside a subagent the list is its FAMILY
    ///
    /// A head switched into a child used to draw the whole daemon — every conversation, and
    /// every conversation's children — around a row that was one level down. The operator's
    /// ask is the narrow one a tree walk implies: *"make sure sessions list (ctrl-s) is
    /// filtered to the parent and siblings"*, so that is what this is. The parent is the top
    /// row and its children are under it, **shown whatever the collapse state says**, because
    /// the filter and the expansion would otherwise be the same gesture twice — a family list
    /// whose siblings were collapsed into the parent would be a list of one row.
    ///
    /// The chain up to where you are is still always shown, and it is here by construction:
    /// the child you are in *is* one of the listed siblings. `esc` (up) and a row's `enter` walk
    /// that same edge from the outside, so ctrl-s inside a child answers *where am I* and *who is
    /// next to me* with one list — while ctrl-s in a conversation still answers *what does this
    /// daemon hold* under the collapse rule above. Two questions, one enumeration per question.
    pub(crate) fn session_rows(&self) -> Vec<SessionRow> {
        // The chain from the current session up to its root, by id — so the way back to where you
        // are is always on the screen.
        let mut path: Vec<String> = vec![self.session_id.clone()];
        let mut cur = self.session_id.as_str();
        while let Some(s) = self.sessions.iter().find(|s| s.session_id == cur)
            && let Some(p) = s.parent_session_id.as_deref()
        {
            path.push(p.to_string());
            cur = p;
        }
        let mut out: Vec<SessionRow> = Vec::new();
        // **This head is in a subagent**: the parent and its children, and nothing else.
        if let Some(parent) = self.parent_session() {
            if let Some(i) = self.sessions.iter().position(|s| s.session_id == parent) {
                out.push(SessionRow { idx: i, depth: 0 });
                self.push_children(&mut out, &parent, 1, &path, self.family_open(&parent, 0));
            }
            return out;
        }
        for (i, s) in self.sessions.iter().enumerate() {
            if s.parent_session_id.is_some() {
                continue;
            }
            out.push(SessionRow { idx: i, depth: 0 });
            self.push_children(&mut out, &s.session_id, 1, &path, false);
        }
        out
    }

    /// **Whether a row's children are on the list because this head is standing among them.**
    ///
    /// The family view ([`App::session_rows`]) shows the parent's children whatever the collapse
    /// state says — a family whose members were folded away would be a list of one row — so the
    /// first level of that view is open by rule rather than by the operator's `→`. One function,
    /// read by the enumeration **and by the fold glyph the picker draws beside the row**: a `▸`
    /// next to the rows it is hiding is the one thing this list must not say, and the glyph came
    /// from the collapse list alone, so a family view drew `▸` over three visible rows. Same rule,
    /// same reader, which is the `todos_stops` lesson this file already carries.
    pub(crate) fn family_open(&self, id: &str, depth: usize) -> bool {
        depth == 0 && self.parent_session().as_deref() == Some(id)
    }

    /// A session's children, in the daemon's order, one step deeper — the recursive half of
    /// [`App::session_rows`].
    ///
    /// A child is shown when its parent is expanded **or** when it is on the chain to the current
    /// session; anything else is collapsed into its parent. `force` is [`App::family_open`]'s own
    /// answer for the family view's first level — see `session_rows` — and it applies to **one
    /// level only**: a sibling's own children are still folded away until that sibling is
    /// expanded, which is what keeps the family view a family rather than the whole subtree behind
    /// it.
    pub(crate) fn push_children(
        &self,
        out: &mut Vec<SessionRow>,
        parent: &str,
        depth: usize,
        path: &[String],
        force: bool,
    ) {
        let open = force || self.expanded.iter().any(|e| e == parent);
        for (i, s) in self.sessions.iter().enumerate() {
            if s.parent_session_id.as_deref() != Some(parent) {
                continue;
            }
            if !open && !path.iter().any(|p| *p == s.session_id) {
                continue;
            }
            out.push(SessionRow { idx: i, depth });
            self.push_children(out, &s.session_id, depth + 1, path, false);
        }
    }

    /// **The conversation this head's session belongs to** — itself, unless it is a sub-session.
    ///
    /// The header counts *conversations*, and this is the half that keeps a head driving a child
    /// from reading `0/4`: it still says which conversation it is in.
    pub(crate) fn session_root(&self) -> String {
        let mut cur = self.session_id.as_str();
        while let Some(s) = self.sessions.iter().find(|s| s.session_id == cur)
            && let Some(p) = s.parent_session_id.as_deref()
        {
            cur = p;
        }
        cur.to_string()
    }

    pub(crate) fn session_label(&self, id: &str) -> String {
        self.sessions
            .iter()
            .find(|s| s.session_id == id)
            .filter(|s| !s.title.is_empty())
            .map(|s| s.title.clone())
            .unwrap_or_else(|| short_id(id))
    }

    /// Ask the daemon for a session as soon as this head is attached: resume it out
    /// of the store if it is not live, and switch to it either way.
    ///
    /// What `letibot --continue` and `letibot --session ID` turn into. It cannot be
    /// an `Attach` naming the id, because the daemon refuses an attach to a session
    /// it does not hold — correctly, since a typo must not seat you somewhere — and
    /// "not held yet" is exactly the state a resume is for. So the head attaches to
    /// wherever the daemon puts it and then asks, which is the same two steps the
    /// picker takes.
    pub fn request_session(&mut self, id: &str) {
        if id.is_empty() {
            return;
        }
        self.want_new_session = true;
        self.queued.push(Action::ResumeSession(id.to_string()));
    }

    /// Make a session and go there, as soon as this head is attached.
    ///
    /// `letibot --new [TITLE]` against a daemon that is already running.
    pub fn request_new_session(&mut self, title: &str) {
        self.want_new_session = true;
        self.queued.push(Action::NewSession(title.to_string()));
    }

    /// **A frame this head could not read: said, and counted.**
    ///
    /// The requirement is *survive AND count*. Dying on an unparseable frame takes the
    /// session down and says nothing; stepping over one in silence is the same failure
    /// more quietly, because *"this daemon is sending me something I do not
    /// understand"* then looks exactly like quiet. So this is the one entry point the
    /// driver has for `Inbound::Unreadable`: it says what it was, in the transcript,
    /// and it moves a counter that `/status` carries and the border names once it has
    /// moved.
    ///
    /// # What it deliberately does not do
    ///
    /// It does not touch `seq`, and nothing about it is acked. No frame was parsed, so
    /// there is no seq to report — and inventing one would rewind this head's read mark
    /// over frames it has already read, which is the one thing a mark must never do.
    /// `Ack`'s `filtered` is "events I chose not to show" and this is not that either,
    /// so it moves neither counter the daemon reads back.
    ///
    /// It is `Control` rather than `Rendered` for the same reason a `Hello` is: the
    /// counter it moves is on a screen this head does not reach for, and the sentence
    /// is filed as a note like every other thing that happened between rows — anchored
    /// where it arrived, so it scrolls away like the rest of the conversation instead
    /// of sitting above the composer for ever.
    pub fn unreadable(&mut self, u: Unreadable) -> Disposition {
        self.unreadable += 1;
        // `ts` is 0: this happened on the socket rather than on the session's log, and
        // the log's clock is not this. A note with no timestamp renders without one,
        // which is the honest shape — see `clock_time`.
        self.note(Note::Warned(Warned {
            code: "unreadable_frame".into(),
            detail: u.said(),
            ts: 0,
        }));
        self.redraw = true;
        Disposition::Control
    }

    /// **The daemon connection has gone.** Called by the driver when a write fails or
    /// when the pump's channel closes, and by the caller when a reconnect attempt
    /// fails.
    ///
    /// Idempotent, and that matters: the two ways it is noticed arrive together — the
    /// socket EOFs and the pump dies, so the reader sees `Disconnected` while a write
    /// in the same pass fails too — and the elapsed time is measured from the *first*
    /// report rather than from the last, so the sentence does not keep starting over.
    ///
    /// A head that is going away does not reconnect, and neither does one that is
    /// already on its way out: a `Bye` and a `/quit` both arrive mid-pass, and turning
    /// either into a two-second retry loop is how leticl made a refusal unescapable.
    pub fn link_down(&mut self, why: &str) {
        if self.quit || self.bye.is_some() {
            return;
        }
        // **No answer is coming, so nothing is asked for.** A `SuggestShell` this head
        // queued is dropped by the driver when the link is down ("nothing leaves a head
        // whose link is down"), and a head that kept the ask would go on saying *asking
        // the model* — for ever, because the id it is waiting on was never sent. Cleared
        // here rather than in the driver's refusal arm because this is the fact: the
        // socket the answer would arrive on is gone. The transcript's own snapshot after
        // a reconnect clears them too, and both are the same rule.
        self.clear_shell_suggestions();
        // **A link that went down because this head asked is not news.** R30: the daemon
        // closing our socket is the answer arriving, and drawing *"the daemon connection
        // is down — reconnecting"* over a shutdown the operator ordered would be this
        // head reporting its own request as a fault. Reconnecting is wrong for the same
        // reason: the loop would open a second socket to a process that is on its way
        // out, and `should_reconnect` would be true the whole time it waited.
        if self.stopping.is_some() {
            return;
        }
        // **And the pane goes with the connection.** See [`App::drop_pane`]: the pty is the
        // daemon's, and a head that cannot reach it can neither feed the screen nor forward
        // the one key that leaves.
        self.drop_pane();
        if self.link.is_down() {
            // Already known: refresh the reason if this report has one and keep the
            // clock. Both reports are true; the first is the more useful clock.
            if let Link::Reconnecting { why: known, .. } = &mut self.link
                && !why.is_empty()
            {
                *known = why.to_string();
            }
            return;
        }
        self.link = Link::Reconnecting {
            since_ms: self.now_ms,
            attempts: 0,
            why: why.to_string(),
            next_try_ms: self.now_ms.saturating_add(RECONNECT_BACKOFF_MS),
        };
        self.redraw = true;
    }

    /// **Whether the caller should try to get back now**: the link is down and the
    /// backoff has passed.
    ///
    /// The timing lives on the head because the head has the clock, and because a
    /// caller that kept its own would be a second copy of a rule about how often to
    /// retry.
    pub fn should_reconnect(&self) -> bool {
        matches!(&self.link, Link::Reconnecting { next_try_ms, .. } if self.now_ms >= *next_try_ms)
    }

    /// **An attempt to get back is out.** Called by the caller the moment it has opened
    /// a socket and sent its `ATTACH`, so the head does not try again while an answer is
    /// in flight.
    ///
    /// This is not an optimisation, it is what stops the retry from eating its own
    /// answer: an `ATTACH` goes out on a *live* socket and the `Hello` comes back a
    /// moment later, and a loop that asked `should_reconnect` between those two would
    /// open a second socket and — to open it — close the first, which is the one about
    /// to be answered. Found by the end-to-end test, which deadlocked on it.
    ///
    /// It pushes the window out without counting an attempt: a socket that opened is not
    /// a failure, and the `Hello` either arrives (the link goes up) or the backoff passes
    /// and this tries again.
    pub fn reconnect_sent(&mut self) {
        if let Link::Reconnecting { next_try_ms, .. } = &mut self.link {
            *next_try_ms = self.now_ms.saturating_add(RECONNECT_BACKOFF_MS);
        }
    }

    /// An attempt to get back has failed, and this is the last thing it said. Counted,
    /// so the sentence can say how many times the head has tried — a head that has tried
    /// forty times and one that has tried once are in different situations, and "still
    /// reconnecting…" says neither.
    pub fn reconnect_failed(&mut self, why: &str) {
        if let Link::Reconnecting {
            attempts,
            why: known,
            next_try_ms,
            ..
        } = &mut self.link
        {
            *attempts += 1;
            *known = why.to_string();
            *next_try_ms = self.now_ms.saturating_add(RECONNECT_BACKOFF_MS);
            self.redraw = true;
        }
    }

    /// **The daemon is back** — the `Hello` is what says so, because it is the frame
    /// that seats this connection.
    ///
    /// Returns the sentence rather than filing it, and the caller is why: this runs
    /// inside the `Hello` arm, and the very next thing that arm does is fold in the
    /// snapshot — which **replaces `self.notes` wholesale**, a snapshot's warnings being
    /// the head's whole warning history. A note filed here would be thrown away, which is
    /// how the first version of this said nothing at all. Same shape, same reason, as
    /// `letibot_sessionlog::protocol_skew`'s sentence in the same arm.
    ///
    /// The sentence carries the seq the head asked from, so "where it picks up" is a
    /// number and not a promise. `None` when nothing was down, so the `Hello` a `Switch`
    /// produces says nothing.
    pub(crate) fn link_up(&mut self) -> Option<String> {
        let Link::Reconnecting {
            since_ms, attempts, ..
        } = self.link.clone()
        else {
            return None;
        };
        let out = self.now_ms.saturating_sub(since_ms);
        // Only when there *were* failed attempts: the ordinary reconnect succeeds on
        // its first try, and `(...4.4s ().)` with nothing in the brackets is a head
        // saying a number where there is none.
        let tries = if attempts == 0 {
            String::new()
        } else {
            format!(
                " ({} attempt{})",
                attempts,
                if attempts == 1 { "" } else { "s" }
            )
        };
        self.link = Link::Attached;
        self.redraw = true;
        Some(format!(
            "the daemon is back after {}{tries}. Resuming from seq {} — anything the \
             daemon has for me in the gap arrives as events, or as a resync if it is \
             larger than the daemon still holds.",
            dur_human(out),
            self.seq,
        ))
    }

    /// A command the operator (or a frame) produced did not leave, because there is no
    /// daemon to send it to. Said once per batch: the alternative is one sentence per
    /// action, and the batch is usually one action.
    pub fn refused_while_detached(&mut self) {
        self.say("no daemon connection — that did not go out");
        self.redraw = true;
    }

    /// Whether the link to the daemon is down. What the screen asks before it draws the
    /// detached line, and what `submit` asks before it turns a line into a prompt.
    pub fn detached(&self) -> bool {
        self.link.is_down()
    }

    /// **Leave the pane without ending it** — the `ctrl-\` act. See [`TermPane`].
    ///
    /// The rectangle goes and the conversation comes back; **nothing is sent**, so the program
    /// keeps running on the daemon's pty, the daemon keeps its screen, and the pane's slot stays
    /// occupied — which is what makes a later `!term` an attach to the *same* run rather than a
    /// new one.
    ///
    /// **The head keeps its own copy of the screen and keeps feeding it**, deliberately: the
    /// `TermOutput` frames are still arriving (the pane is the session's and this head is still
    /// attached to the session), so a program that exits while the operator is away still leaves
    /// the row they would have seen had they been looking — with its last rows and its status.
    /// Dropping the screen here would make a death while detached a death with nothing to show,
    /// which is the second half of *a detach must not hide anything*.
    ///
    /// **The sentence names both ways on**, because a person who has just made a program
    /// disappear needs to know it is still there and how to end it if they meant to.
    ///
    /// # It is NOT a notice, and that is the correction
    ///
    /// This said *"{line} is still running — `!term` comes back to it, `!term close` ends it"*
    /// through [`App::say`], and it was the same fact twice: [`App::pane_behind`] draws that
    /// sentence **persistently**, one line above the composer, for as long as it is true. Worse,
    /// the notice is the copy that cannot be taken back — a notice lives for `NOTICE_MS` of wall
    /// time and nothing retires it early, so the `TermEnded` a second later left a sentence on
    /// the screen saying a program was still running, directly above the row saying it had
    /// ended. A fact about NOW belongs in the one place that stops drawing it when it stops
    /// being true.
    ///
    /// Returns whether there was a pane to leave, so a caller can say so once rather than per
    /// report — the shape [`App::drop_pane`] uses for the same reason.
    pub fn detach(&mut self) -> bool {
        let Some(p) = self.term.as_mut() else {
            return false;
        };
        p.detached = true;
        self.redraw = true;
        true
    }

    /// **`!term close` — the ending, and the head asks first.**
    ///
    /// # The four answers, and why the head cannot give three of them
    ///
    /// * **a pane of this head's own** — drawn or detached. The line the operator typed at
    ///   `TermOpen` is in [`TermPane::line`], so the card can name what is about to end;
    /// * **no pane here, but a live one the daemon named** — [`App::term_fact`] is
    ///   [`PaneFact::Running`], and the card names `!term <command>`. This is the case the
    ///   read exists for: the program's screen may be in another head entirely, and the
    ///   operator still means it;
    /// * **no pane at all** — a sentence, and **nothing sent**. `TermClose` is quiet about
    ///   there being nothing to end, so a head that sent it would look like it had done
    ///   something. The sentence names the way to start one;
    /// * **and *not asked yet*** — [`PaneFact::Unasked`] is the state between an attach and the
    ///   daemon's answer to the read, and **the line is held**: the read goes out, and its
    ///   answer runs this same decision (see the `TermStatus` arm). A head that guessed *no
    ///   pane* here would be wrong on the one case that matters — a person who attached a
    ///   moment ago and means the program the daemon is holding.
    ///
    /// **Nothing here ends anything.** Every arm either raises [`App::term_ask`] or says why it
    /// cannot; the frame leaves when the card is answered with a yes, and never before.
    pub(crate) fn begin_close(&mut self) -> Option<Action> {
        match (&self.term, &self.term_fact) {
            (Some(p), _) => {
                let line = p.line.clone();
                self.ask_close(&line);
                None
            }
            (None, PaneFact::Running(command)) => {
                let line = format!("!term {command}");
                self.ask_close(&line);
                None
            }
            (None, PaneFact::None) => {
                self.say(
                    "this session has no pane to end — `!term COMMAND` starts one, and \
                     `!term` attaches to one that is already running",
                );
                self.redraw = true;
                None
            }
            (None, PaneFact::Unasked) => {
                self.close_pending = true;
                self.say("asking the daemon what this session is running…");
                self.redraw = true;
                Some(Action::TermStatus)
            }
        }
    }

    /// **Raise the confirmation card**, and it is the only place [`App::term_ask`] is set — so
    /// there is one place a pane can be ended from, and it is the one that asks.
    pub(crate) fn ask_close(&mut self, line: &str) {
        self.term_ask = Some(TermAsk {
            line: line.to_string(),
        });
        self.redraw = true;
    }

    pub(crate) fn switch_to(&mut self, id: String) -> Option<Action> {
        if id == self.session_id {
            self.picker = false;
            self.redraw = true;
            self.say("already here");
            return None;
        }
        // A session that is on disk and not in this daemon has to be brought in
        // before it can be switched to. Two frames, and the head sends the second
        // one when the daemon answers the first — the same two steps `/new` takes,
        // reusing `want_new_session` because "go to the session the daemon just told
        // me about" is one behaviour and a second flag for it would be a second
        // behaviour that drifts.
        if self.sessions.iter().any(|b| b.session_id == id && !b.live) {
            self.want_new_session = true;
            self.say(&format!(
                "resuming {} from the store…",
                self.session_label(&id)
            ));
            return Some(Action::ResumeSession(id));
        }
        // `since_seq` is not sent: this head has no state for the session it is
        // going to, so a snapshot is the only honest ask. Coming *back* to a
        // session it was watching would be a resume, and this head does not keep
        // per-session marks — it would be a cache with no invalidation rule.
        Some(Action::Switch(id))
    }

    /// **Everything a seated head must ASK FOR, in one place — attach, re-attach, and the
    /// return from a switch.**
    ///
    /// # Why an ask at all, when the `Hello` already carries a list
    ///
    /// The operator's report is the whole argument: the composer's `N subagents running`
    /// segment **disappeared and came back on its own** while a subagent ran throughout —
    /// *"so the counter is gone"*, then, minutes later, *"yep and now it is back. wtf"*.
    /// Nothing was restarted between the two. A drawn state that comes back by itself is a
    /// state whose restore is **opportunistic**, and the thing that was restoring it was
    /// some later list reply happening to arrive — the head was not asking for one.
    ///
    /// So every read here is a question the head puts to the daemon at the one moment it
    /// knows it needs the answer, rather than a frame it hopes will land:
    ///
    /// * **`ListSessions`** — the subagent rows and the composer's count are folded from
    ///   the daemon's own list ([`App::fold_subagents`]), and a `Hello` carries a copy of it
    ///   cut at the instant the daemon answered. A copy is not an ask: the fold's source was
    ///   whatever list the last frame happened to hold, and nothing re-asked. This does.
    /// * **`ListJobs`** — the same shape, and the reason is the brief's own: *"for a job
    ///   already running when this head attached, which no event announces."*
    /// * **`Settings`** — the header reads the live `model` row, and the daemon never sends
    ///   the rows unprompted. The operator, on a session answered by deepseek: *"restarted
    ///   the letibot - still qwen"*.
    ///
    /// # Which three moments this is
    ///
    /// There is one call site, in the `Hello` arm, because the daemon answers all three with
    /// the same frame: an **attach**, a **re-attach** (the driver's reconnect is an `ATTACH`
    /// and the daemon answers it with a `Hello` like any other), and the **return from a
    /// switch** — a `Switch` is answered with a second `Hello` on purpose, so that the
    /// late-join path is the only seating path. The return from a *pane* is the same moment:
    /// whether this session has a pane is asked on the same seating, by the same arm (see the
    /// `TermStatus` push above, which is `Action::TermStatus`).
    ///
    /// **The asks are reads and they are cheap.** `ListSessions` and `ListJobs` are answered
    /// off the registry on the connection's own thread, not through the command queue (see
    /// the server's arms), so none of them waits behind a running turn — which is the whole
    /// reason the daemon answers them here rather than as commands.
    ///
    /// **And every answer re-folds.** A re-ask whose reply is not applied is worse than no
    /// re-ask, because it looks like one: the `Sessions` arm calls [`App::fold_subagents`]
    /// and the `Jobs` arm replaces the table, for exactly this reason.
    pub(crate) fn refetch_session_facts(&mut self) {
        self.queued.push(Action::Settings);
        self.queued.push(Action::ListJobs);
        self.queued.push(Action::ListSessions);
    }

    /// **The session that spawned this one, or `None` when this head is not in a subagent.**
    ///
    /// Read off the daemon's list and **not** inferred from the id: ids are minted by the
    /// daemon (`s-…-sub-…`) and a head that string-matched them would be inventing a fact
    /// the registry already states — `SessionBrief::parent_session_id`, which is `Some`
    /// exactly for a child. See [`App::fold_subagents`] for the same field used the other
    /// way round.
    pub(crate) fn parent_session(&self) -> Option<String> {
        self.sessions
            .iter()
            .find(|s| s.session_id == self.session_id)
            .and_then(|s| s.parent_session_id.clone())
    }
}

/// **The daemon connection, as far as this head can tell.**
///
/// A head's socket to its daemon goes away for ordinary reasons — the daemon is
/// restarted, the box is shut down for a moment, a `--stop` lands — and the head's job
/// is to keep drawing the session it already has, say that the link is down, and keep
/// trying to get it back. It is **not** to exit: exiting takes the operator's view of a
/// conversation that is still on disk and could still be served.
///
/// Two states, and the second is the whole point: `Reconnecting` is a *state the screen
/// shows* rather than a moment between frames. It carries when the link went down, so
/// the head can say how long it has been trying rather than "reconnecting…" for ever,
/// and it carries the last thing known about why.
///
/// It is deliberately **not** the same thing as a `Bye`. A `Bye` is the daemon saying it
/// is finished with this connection — a refusal, a version skew, a shutdown — and the
/// head leaves with the reason on the screen. That is leticl's rule, and it was learned
/// there the expensive way: it dropped only its `connected` flag on a `Bye` and
/// re-attached two seconds later, for ever, so a refusal the daemon meant as the end of
/// the conversation became a two-second loop under a head that never attached and never
/// exited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Attached,
    /// Down, and the caller is retrying. `attempts` counts the tries made since it went
    /// down; `since_ms` is this head's own clock, so `now - since_ms` is how long the
    /// operator has been without a daemon.
    Reconnecting {
        since_ms: u64,
        attempts: u32,
        /// The last thing known about why: the io error, the pump going away, or the
        /// refusal from the last attempt to connect.
        why: String,
        /// No attempt before this, on the head's own clock. The backoff lives here
        /// rather than in the caller so a test can drive it with `clock`.
        next_try_ms: u64,
    },
}

impl Link {
    pub fn is_down(&self) -> bool {
        matches!(self, Link::Reconnecting { .. })
    }
}

/// **A head that has asked the daemon to stop, and is waiting to find out.** (R30)
///
/// The operator chose *exit, and stop the daemon too*, and on 2026-09-23 they got the exit
/// without the stop: the head was gone and `harnessd` was still there at `PPID 1`, idle,
/// its socket bound. Nothing ever asked it, or nothing checked. The old code was two
/// discarded results and a return:
///
/// ```text
/// let _ = self.client.stop(app.seq, &who);
/// let _ = self.client.detach();
/// ```
///
/// — whether the frame reached the socket was a race against the head's own shutdown, and
/// **a request is not an outcome**. This is the state that makes the difference: while it
/// is unresolved the head does not leave, and the three facts it observes are kept apart,
/// because each one is a different answer for the operator:
///
/// * `sent` — the write returned `Ok`. The frame is in the kernel's buffer for this socket,
///   which is the most a writer can ever know.
/// * `acked` — the daemon answered `Accepted { note: "stopping" }`. **This is the one that
///   says the request was read**, and the daemon sends it before it closes anything.
/// * `closed` — the daemon's socket file is gone. Its `shutdown` unlinks that file after
///   joining the accept loop, which is what the wrapper's *"the record is removed only
///   after the process is gone"* is the same shape of.
/// * `gone` — **the daemon's process has exited and been collected.** Observed by reaping it
///   when it is this head's own child (`waitpid(WNOHANG)`) and by its `/proc` entry otherwise,
///   **a zombie counting as gone** — because a child that has exited stays in `/proc` until its
///   parent waits for it, and this head is that parent. The strongest observation available,
///   and the only one that is *the daemon has actually gone*.
///
///   **It was `fs::metadata("/proc/{pid}")`, and that is the defect the operator reported
///   twice**: *"they always tell me daemon not stopped after waiting for 5 sec, then `letibot
///   --stop` tells nothing runs."* Both true — the daemon had stopped, and the test said
///   otherwise, because a zombie keeps its directory. The wrapper's test is right where the
///   wrapper runs and wrong here, and the difference is the relationship. See
///   [`crate::driver::Parentage`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopping {
    /// The identity that asked, as the daemon will have announced it.
    pub who: String,
    /// The head's own clock when the frame went out.
    pub since_ms: u64,
    /// When the head stops waiting and says what it saw.
    pub deadline_ms: u64,
    pub sent: bool,
    pub acked: bool,
    pub closed: bool,
    pub gone: bool,
    /// The daemon's pid, from `SO_PEERCRED` on this very connection — the process at the
    /// other end of the socket, not a number read out of a file that may be stale. `None`
    /// when the kernel would not say, which is a fact the farewell states rather than
    /// fills in.
    pub pid: Option<i32>,
    /// **Whether the model was still working when the head gave up waiting.** A daemon mid-turn
    /// legitimately finishes its round first, so this is the difference between *a slow stop* and
    /// *a daemon that did not go* — and it is the head's to know because it is the head that was
    /// watching the turn.
    ///
    /// **`turn_busy` and not the state name**: the round a daemon finishes before it stops can be
    /// one whose tool call is executing, and a head that asked only whether a round was generating
    /// would call that stop *a refused one* when it was merely slow.
    pub turn_busy: bool,
}

impl Stopping {
    /// **Has the question been answered?** Either the daemon's process is gone — the
    /// operator's choice, carried out — or the deadline has passed and the head can say
    /// what it observed.
    pub fn resolved(&self, now_ms: u64) -> bool {
        self.gone || now_ms >= self.deadline_ms
    }

    /// The line the head draws while it waits, so the screen is never a freeze.
    pub fn waiting_line(&self, now_ms: u64) -> String {
        let out = now_ms.saturating_sub(self.since_ms);
        let left = self.deadline_ms.saturating_sub(now_ms);
        let seen = if self.acked {
            "the daemon answered and is shutting down"
        } else if self.sent {
            "the request went out and the daemon has not answered yet"
        } else {
            "the request could not be sent — the socket is already gone"
        };
        format!(
            "stopping the daemon: {seen} — {} waiting, {} before this head gives up \
             and tells you what it saw. A turn already generating finishes its round.",
            dur_human(out),
            dur_human(left),
        )
    }
}

/// **How long a head waits for a daemon it has asked to stop.**
///
/// **Five seconds, and it is not this head's number** — it is the figure
/// `~/bin/letibot` settled on for the same question, at the site of the same incident:
///
/// ```bash
/// for _ in 1 2 3 4 5 6 7 8 9 10; do [ -d "/proc/$p" ] || break; sleep 0.5; done
/// if [ -d "/proc/$p" ]; then
///   echo "NOT stopped: $line (pid $p) is ignoring SIGTERM after 5s." >&2
/// ```
///
/// Its comment carries the measurement: *"'stopped' is said AFTER the process is gone, not
/// after the signal is sent … Measured 2026-09-16: a daemon wedged on a llama-server that
/// had gone away swallowed SIGTERM, this printed 'stopped', deleted the record, and left an
/// orphan holding the store and the GPU that `--daemons` could no longer see."* Two halves
/// of one program, one figure, and the head never read it.
pub const STOP_DEADLINE_MS: u64 = 5_000;

/// How long between attempts to get back, in milliseconds.
///
/// **Flat, and the same number leticl uses.** A daemon that is coming back is back in
/// well under a second, and one that is gone costs one connect to a socket path — one
/// syscall — every two seconds. Growing the interval would be an optimisation of
/// nothing, and it would make the thing the operator watches move less often than the
/// thing they are waiting for.
pub const RECONNECT_BACKOFF_MS: u64 = 2_000;

/// When a head with no daemon stops saying `reconnecting` and starts saying how long,
/// and what the operator can do about it.
///
/// Under it a drop is usually over before the sentence is read; over it the wait is not
/// a moment, and a head that has been saying the same word at the operator for a minute
/// has told them nothing they could not see for themselves.
pub const LINK_IMPATIENT_MS: u64 = 15_000;

/// **A pid as a sentence fragment, or the honest absence** — R30's rule, in one place because
/// two writers say it: the stop's farewell and the replaced-daemon note.
///
/// `None` is *the kernel would not name the peer*, which is a different statement from a pid of
/// zero; a head that printed a number it did not have would send the operator to `ps` for a
/// process that is not there.
pub(crate) fn pid_word(pid: Option<i32>) -> String {
    match pid {
        Some(p) => format!("pid {p}"),
        None => "a pid the kernel did not name".to_string(),
    }
}

/// **Which daemon a head is drawing the picture of** — the two facts that answer *is this the
/// one I attached to*.
///
/// A pair rather than the pid alone because a pid is reused by the kernel: a daemon restarted
/// and handed the same number is a different daemon with the same identity, and the build's
/// protocol version is the other half of the answer — a replaced daemon is usually a rebuilt
/// one, and a rebuild that moved the wire is the case a head most needs to be told about.
///
/// **What it deliberately is not: a daemon instance id.** The strongest available fact would be
/// a boot-time stamp the daemon mints once and sends on every `Hello`, which is a protocol
/// field and a `PROTOCOL_VERSION` bump; `SO_PEERCRED` is already on the wire (R30) and already
/// re-read on every connection, so this is the honest reading of what the head has. The residue
/// — a replaced daemon that happens to get the same pid and speaks the same protocol — is
/// named here rather than papered over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DaemonSeat {
    /// The process at the other end of this socket, from `SO_PEERCRED`. `None` when the kernel
    /// would not say, which compares equal to another `None` — the one case this cannot tell
    /// apart, and the reason the protocol version is beside it.
    pub(crate) pid: Option<i32>,
    /// The version the daemon claims, from the `Hello` that seated this connection.
    pub(crate) protocol: u32,
}

/// **One row of the session picker** — see [`App::session_rows`], which is the only thing that
/// builds one.
///
/// A pair of facts rather than a bare `usize`, because the list is NESTED: which session it is, and
/// how deep it sits. Deriving the depth at the drawing site instead is exactly how the drawing and
/// the keys become two enumerations again — the defect `todos_stops` exists to record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionRow {
    /// Index into `App::sessions`.
    pub(crate) idx: usize,
    /// 0 for a conversation; 1 for a sub-session under it; deeper for a tree.
    pub(crate) depth: usize,
}
