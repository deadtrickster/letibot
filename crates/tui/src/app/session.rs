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
            forced: None,
        });
        self.redraw = true;
    }

    /// **The head is stopping the daemon the forced way from the start**, because the
    /// daemon is an older build. No stop frame went out — see [`ForceWhy::OlderDaemon`] —
    /// so `sent` is false and the farewell says why rather than *could NOT be sent*.
    pub fn force_stop_began(&mut self, who: &str, pid: Option<i32>, why: ForceWhy, now_ms: u64) {
        self.stopping = Some(Stopping {
            who: who.to_string(),
            since_ms: now_ms,
            deadline_ms: now_ms,
            sent: false,
            acked: false,
            closed: false,
            gone: false,
            pid,
            turn_busy: self.turn_busy(),
            forced: Some(Forcing {
                why,
                since_ms: now_ms,
                outcome: None,
            }),
        });
        self.redraw = true;
    }

    /// **The ordinary stop did not finish, and the head is escalating — once.** Called by
    /// the driver when the deadline passes with the daemon's process still there. A second
    /// call is ignored: the escalation is the one extra thing the head does, and if that
    /// fails too the operator is told what to run.
    pub fn force_stop_escalated(&mut self, why: ForceWhy, now_ms: u64) {
        if let Some(s) = self.stopping.as_mut()
            && s.forced.is_none()
        {
            s.forced = Some(Forcing {
                why,
                since_ms: now_ms,
                outcome: None,
            });
            self.redraw = true;
        }
    }

    /// The forced stop has an answer. `gone` is the force path's own observation of the
    /// process; it is folded into [`Stopping::gone`] so every reader sees one fact.
    pub fn force_stop_finished(&mut self, outcome: ForceOutcome) {
        if let Some(s) = self.stopping.as_mut()
            && let Some(f) = s.forced.as_mut()
        {
            if outcome.gone {
                s.gone = true;
            }
            f.outcome = Some(outcome);
            self.redraw = true;
        }
    }

    /// The protocol the attached daemon said it speaks in its `Hello`, or `None` before one.
    pub fn daemon_protocol(&self) -> Option<u32> {
        self.daemon_protocol
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
        // **A forced stop is always said**, whether or not it worked. The operator chose
        // *stop*, and the head did something sharper than they asked for — interrupted the
        // turns and signalled the process — so the farewell says that it did, and why, even
        // when the daemon is gone: *"yes"* was the answer to *should the head take the force
        // path itself, with a line saying why*.
        let forced = s.forced.as_ref().map(|f| {
            let done = match &f.outcome {
                Some(o) if o.gone => format!("done: {}.", o.said),
                Some(o) => format!("the forced way did not finish it either: {}.", o.said),
                None => "the forced way had not finished when this head left.".to_string(),
            };
            format!("{}\n  {done}", f.why.line())
        });
        if s.gone {
            return forced;
        }
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
        // instant the `Stop` is taken; the run's own thread is inside that command and has
        // not ended (`agent/run-off-worker`: it used to be the WORKER that was in there).
        // Measured on a
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
        let pid = pid_word(s.pid);
        let ask = if matches!(
            s.forced,
            Some(Forcing {
                why: ForceWhy::OlderDaemon { .. },
                ..
            })
        ) {
            "was not sent as a stop frame, because the daemon is an older build"
        } else if self.bye.is_some() {
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
            //
            // **The TURN is not that wait any more.** The daemon interrupts the turns it
            // holds on the way out — the same frame an Esc-Esc sends — so what a head sees
            // here is the round a command is inside, not a generation finishing. See
            // `Registry::close`'s `STOPPING_TURN_REASON`.
            "A turn was running; the daemon interrupts it on the way out, so this is the \
             round a COMMAND is inside — and its own deadline is what ends that. This is a \
             slow stop rather than a refused one."
        } else {
            "No turn was running, so there was nothing for it to finish."
        };
        let forced = forced.map(|f| format!("{f}\n  ")).unwrap_or_default();
        Some(format!(
            "the daemon was asked to stop and had not gone {secs}s later.\n  \
             the request {ask}; the daemon is still there ({pid}).\n  {because}\n  \
             {forced}`letibot --stop --force` finishes it — it aborts in-flight turns over the \
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
    /// **Two questions, two lists.** A head standing in a CONVERSATION draws the whole daemon,
    /// in groups (§ below); a head standing in a subagent draws its family (the last section).
    ///
    /// # The whole daemon, in groups
    ///
    /// The operator, 2026-10-26: *"in session view the automatic sessions must be grouped
    /// somehow, so my vs auto. otherwise im lost in subagents."* A daemon holds one conversation
    /// of theirs and thirty rows of work it did for that conversation, and one list ordered by
    /// *when something last happened* put a reviewer above the thing they were doing.
    ///
    /// So the list is **`mine` first, then one line per automatic kind** — `subagents (14)`,
    /// `review queue (3)`, `packages (6)`, `one-shots (2)` — each folded until it is opened, and
    /// each row drawn under exactly one of them. [`SessionGroup`] and
    /// `letibot_sessionlog::group_of` are the rule; this function is where it becomes rows.
    ///
    /// **`mine` is never folded**: the operator's own conversations are what the list is for.
    /// An automatic group opens itself when this head is standing in one of its rows, because a
    /// picker that hides where you are cannot answer *where am I*.
    ///
    /// **A group with no rows draws no line.** A list whose every fold says `(0)` teaches a
    /// reader to skip folds, which is the opposite of what the counts are for.
    ///
    /// # Inside a group the tree still nests
    ///
    /// A child sits under the session that spawned it **when that session is in the same group
    /// and is expanded** — `parent_session_id` is already on every row, so nothing had to be
    /// added to the wire for this. A child whose parent is in ANOTHER group (a `task` child of
    /// the operator's own conversation) is a root of its group's tree, because its parent is not
    /// on that list — and the count on the group's line is what accounts for it.
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
        // **This head is in a subagent**: the parent and its children, and nothing else. The
        // family view stays ungrouped on purpose — it is already a list of two or three rows
        // around the one the operator is standing in, and folds over it would be a second way to
        // hide the answer to *where am I* (see this function's docstring).
        if let Some(parent) = self.parent_session() {
            if let Some(i) = self.sessions.iter().position(|s| s.session_id == parent) {
                out.push(SessionRow {
                    idx: Some(i),
                    depth: 0,
                    group: SessionGroup::Mine,
                });
                self.push_children(
                    &mut out,
                    &parent,
                    1,
                    &path,
                    self.family_open(&parent, 0),
                    InGroup {
                        group: None,
                        of: &[],
                    },
                );
            }
            return out;
        }
        // **The whole daemon, in groups** — see [`crate::app::session_rows`]'s docstring for the
        // rule and `letibot_sessionlog::origin` for what each group is.
        let of = self.session_groups();
        // **A group line exists to SEPARATE, so one group draws none.** A daemon whose every
        // session is the operator's own — a fresh folder, which is most of them — would otherwise
        // open on a `mine` header over the only rows there are: chrome with nothing to tell apart,
        // and one keystroke of scrolling bought with nothing.
        let kinds = SessionGroup::ALL.iter().filter(|g| of.contains(g)).count();
        for group in SessionGroup::ALL {
            let members: Vec<usize> = (0..self.sessions.len())
                .filter(|&i| of[i] == group)
                .collect();
            // **No empty fold.** A group the daemon has nothing for is a line saying zero, and a
            // list whose every fold says `(0)` is a list that teaches a reader to skip them.
            if members.is_empty() {
                continue;
            }
            // **With no header there is no fold to open**, so the one group's rows are simply
            // the list — a lone closed group would draw nothing at all.
            let open = kinds == 1 || self.group_open(group, &of);
            if kinds > 1 {
                out.push(SessionRow {
                    idx: None,
                    depth: 0,
                    group,
                });
            }
            if !open {
                continue;
            }
            // With no header the rows are the whole list, so they sit at depth 0.
            let depth = usize::from(kinds > 1);
            for &i in &members {
                // **A session is drawn ONCE**, under the group it is in. Inside a group its own
                // tree still nests — a child of a child sits under its parent — but a child whose
                // parent is in ANOTHER group (a `task` child of the operator's conversation) is a
                // root of this group's tree, because its parent is not on this list.
                let under = self.sessions[i]
                    .parent_session_id
                    .as_deref()
                    .and_then(|p| self.sessions.iter().position(|s| s.session_id == p))
                    .filter(|&p| of[p] == group);
                if under.is_some() {
                    continue;
                }
                out.push(SessionRow {
                    idx: Some(i),
                    depth,
                    group,
                });
                self.push_children(
                    &mut out,
                    &self.sessions[i].session_id,
                    depth + 1,
                    &path,
                    false,
                    InGroup {
                        group: Some(group),
                        of: &of,
                    },
                );
            }
        }
        out
    }

    /// **The session a typed number names — the n-th SESSION, not the n-th row.**
    ///
    /// The list's numbers are its sessions': a group's line takes none, because a number is
    /// something a reader types to go somewhere and there is nowhere to go on a group's line.
    /// That is rano's own rule (`agent::picker` numbers the [`SessionItem::Session`] rows and
    /// nothing else), and it is read here so the drawn number and the typed one are one
    /// number — the two-enumerations defect this list has paid for twice.
    ///
    /// `None` past the last session, and `None` for `0`: the list is numbered from 1.
    pub(crate) fn session_at_number(&self, n: usize) -> Option<usize> {
        self.session_rows()
            .iter()
            .filter_map(|r| r.idx)
            .nth(n.checked_sub(1)?)
    }

    /// **Which group each session in the daemon's list is in** — one answer per draw, from the
    /// one rule (`letibot_sessionlog::group_of`), so the folds, the counts, the rows and the keys
    /// cannot disagree about a session.
    ///
    /// A `Vec` parallel to `self.sessions` rather than a map: every reader indexes the other one
    /// anyway, and a map keyed by id would be a second way to spell *which session is this row*.
    pub(crate) fn session_groups(&self) -> Vec<SessionGroup> {
        let rows: Vec<GroupRow<'_>> = self
            .sessions
            .iter()
            .map(|s| GroupRow {
                id: &s.session_id,
                parent: s.parent_session_id.as_deref(),
                origin: s.origin,
                // **A brief carries no seat**, so a reviewer is known here by the brief its title
                // was cut from — the same gap `App::fold_subagents` fills the same way.
                role: None,
                title: &s.title,
            })
            .collect();
        self.sessions
            .iter()
            .map(|s| letibot_sessionlog::group_of(&rows, &s.session_id))
            .collect()
    }

    /// **Whether a group's rows are on the list** — `mine` always, the four automatic kinds only
    /// when the operator opened them or when this head is standing in one.
    ///
    /// The second half is the rule that outranks the fold: a picker that hides where you are is a
    /// picker that cannot answer *where am I*, which is the one question the header above it
    /// exists to answer — and it is why a group this head is in opens itself.
    pub(crate) fn group_open(&self, group: SessionGroup, of: &[SessionGroup]) -> bool {
        if group.is_mine() || self.open_groups.contains(&group) {
            return true;
        }
        of.iter()
            .enumerate()
            .any(|(i, g)| *g == group && self.sessions[i].session_id == self.session_id)
    }

    /// **Open or close an automatic group**, and answer whether anything moved — so a key on a
    /// `mine` line (which is never folded) says so rather than redrawing nothing.
    pub(crate) fn toggle_group(&mut self, group: SessionGroup) -> bool {
        if group.is_mine() {
            return false;
        }
        match self.open_groups.iter().position(|g| *g == group) {
            Some(at) => {
                self.open_groups.remove(at);
            }
            None => self.open_groups.push(group),
        }
        self.redraw = true;
        true
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
        in_group: InGroup<'_>,
    ) {
        let open = force || self.expanded.iter().any(|e| e == parent);
        for (i, s) in self.sessions.iter().enumerate() {
            if s.parent_session_id.as_deref() != Some(parent) {
                continue;
            }
            // **A row belongs to exactly one group**, so a child of another group's session is
            // not drawn under it — it is a root where it is drawn (see `session_rows`).
            if let Some(group) = in_group.group
                && in_group.of.get(i) != Some(&group)
            {
                continue;
            }
            if !open && !path.iter().any(|p| *p == s.session_id) {
                continue;
            }
            out.push(SessionRow {
                idx: Some(i),
                depth,
                group: in_group.group.unwrap_or(SessionGroup::Mine),
            });
            self.push_children(out, &s.session_id, depth + 1, path, false, in_group);
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

    /// **How many rows this session's store row holds, while this head holds none of them**
    /// — the fact [`App::restoring`] is drawn from, asked at the one moment the two sources
    /// are both in hand (a `Hello`, whose list carries every session's `stored_items`).
    ///
    /// The head opened the session it asked for and was handed an empty snapshot; the same
    /// frame said the store holds 166 rows for it. The daemon restores on its worker, behind
    /// whatever turn it is running, so for however long that takes the two disagree and the
    /// screen must not believe the emptier one.
    ///
    /// **Only while the head holds NOTHING**, and that is the rule rather than caution. A live
    /// session in a daemon with a store has `stored_items` too, and it equals what the view
    /// holds — but a COMPACTED session's view is trimmed to the window, so `items.len() <
    /// stored_items` is the ordinary state of a long conversation and would have this head
    /// claim a restore that finished an hour ago. `is_empty` is the state that has one
    /// meaning: the head is holding no conversation at all and has been told there is one.
    pub(crate) fn rows_the_store_holds(&self) -> Option<u32> {
        if !self.items.is_empty() {
            return None;
        }
        self.sessions
            .iter()
            .find(|s| s.session_id == self.session_id)
            .map(|b| b.stored_items)
            .filter(|n| *n > 0)
    }

    pub(crate) fn session_label(&self, id: &str) -> String {
        let brief = self.sessions.iter().find(|s| s.session_id == id);
        // **A subagent is named by its slug** — `fix-login`, the name its parent was handed and
        // the one its branch carries — where its title is the first line of a brief that can run
        // to 256 characters. A session nobody named keeps its title, or its short id.
        if let Some(b) = brief.filter(|b| !b.slug.trim().is_empty()) {
            return b.slug.clone();
        }
        brief
            .filter(|s| !s.title.is_empty())
            .map(|s| s.title.clone())
            .unwrap_or_else(|| short_id(id))
    }

    /// **A child as a person reads it** — `fix-login · …14688401`, or its short id — from what
    /// this head knows of it: the subagents pane's row, then the daemon's list. See
    /// [`crate::app::child_name`].
    pub(crate) fn child_label(&self, id: &str) -> String {
        let slug = self
            .subagents
            .iter()
            .find(|s| s.session_id == id)
            .map(|s| s.slug.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.sessions
                    .iter()
                    .find(|s| s.session_id == id)
                    .map(|s| s.slug.clone())
            })
            .unwrap_or_default();
        crate::app::child_name(&slug, id)
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
        self.fetching = Some(id.to_string());
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

    /// **Is this head holding everything it would send — the daemon speaks an OLDER protocol
    /// and nobody has said "attach anyway" for it?**
    ///
    /// The daemon (since the acceptance change) seats whatever version arrives and states its
    /// own in the `Hello`, so *"connect, look around and make informed decision"* lands here:
    /// the connection is up, the conversation is readable and scrollable, and the one thing the
    /// head withholds is its own writes. **"Look around" is the default; "decide" is a key** —
    /// [`App::attach_anyway`], the `ctrl-^` chord spelled on the resident line and in every
    /// refusal.
    ///
    /// # Why OLDER is the locked direction and NEWER is not
    ///
    /// The asymmetry is [`letibot_sessionlog::protocol_skew`]'s, and it is the load-bearing
    /// part: a NEWER daemon's unknown frames are a *reading* problem this head already survives
    /// (`App::unreadable` counts them, the line is kept, nothing is lost but what the frame
    /// said), so there is nothing to withhold. An OLDER daemon is a *writing* problem — the
    /// first `ClientFrame` it has no arm for fails its deserialiser, and its read loop answers
    /// with a `Bye` and closes the socket. **A head that sent as usual would be spending the
    /// session on the first command the two do not share**, which is exactly the quiet-until-fatal
    /// case the sentence exists for. Holding sends is what makes the attach safe by default;
    /// the override is the person's, spelled and deliberate.
    ///
    /// The comparison is against [`App::daemon_seat`], not a boolean — see
    /// [`App::skew_override_for`] for why an override belongs to the daemon it was given on.
    pub(crate) fn skew_locked(&self) -> bool {
        self.daemon_protocol
            .is_some_and(|d| d < letibot_sessionlog::protocol::PROTOCOL_VERSION)
            && self
                .daemon_seat
                .is_some_and(|seat| self.skew_override_for != Some(seat))
    }

    /// [`letibot_sessionlog::protocol_skew`]'s sentence for the daemon this head is seated
    /// with — the informed sentence, verbatim, or `None` when the builds match. Every refusal
    /// and the resident line draw from here so the argument cannot be said two ways.
    pub(crate) fn skew_sentence(&self) -> Option<String> {
        self.daemon_protocol.and_then(|d| {
            letibot_sessionlog::protocol_skew(d, letibot_sessionlog::protocol::PROTOCOL_VERSION)
        })
    }

    /// **`ctrl-^` — "attach anyway, I know what this is."** The other half of the ruling: a
    /// head that only knew how to refuse would have kept the closed door and moved it one
    /// screen down.
    ///
    /// **Nothing is re-sent.** The override does not reach back for whatever was refused while
    /// locked — a refused line was held in the composer, not queued, and re-sending a person's
    /// words after they have moved on is an act nobody asked for. What it does send are the
    /// reads the seating itself owes ([`App::refetch_session_facts`] and the `TermStatus` ask),
    /// which the lock skipped on arrival: those are the head's own obligations, not the
    /// operator's, and lifting the lock is the moment they come due.
    ///
    /// **Silent when nothing is locked**, by the rule a chord is only named where it acts: the
    /// chord is spelled on the lock line and in the refusals, never in the hint bar, so a
    /// press that finds no lock has no promise to keep.
    pub(crate) fn attach_anyway(&mut self) {
        if !self.skew_locked() {
            return;
        }
        self.skew_override_for = self.daemon_seat;
        self.queued.push(Action::TermStatus);
        self.refetch_session_facts();
        let older = self
            .daemon_protocol
            .unwrap_or(letibot_sessionlog::protocol::PROTOCOL_VERSION);
        self.say(&format!(
            "attached anyway — this head now sends to a daemon speaking protocol {older} \
             against its own {}. The first frame the two do not share still ends the \
             connection, with a `Bye` naming both builds; restarting the daemon is the fix.",
            letibot_sessionlog::protocol::PROTOCOL_VERSION
        ));
        self.redraw = true;
    }

    /// A command the operator (or a key, or a frame) produced did not leave, because this
    /// head is holding its sends against an older daemon. [`App::refused_while_detached`]'s
    /// shape, once per batch, for the same reason: one sentence, not one per action.
    ///
    /// The sentence is [`App::skew_sentence`]'s — the informed one — with the act and the
    /// chord appended, because the reader here is a person who just pressed a key and needs
    /// to know both why nothing left and what to do about it.
    pub(crate) fn refused_while_locked(&mut self) {
        if let Some(said) = self.skew_sentence() {
            self.say(&format!(
                "{said} Nothing was sent — this attach is read-only by default, and \
                 {ATTACH_ANYWAY_CHORD} attaches anyway.",
            ));
        }
        self.redraw = true;
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
    ///
    /// **`which` is the number the operator typed** (`!term close 2`, protocol 44), and it is
    /// checked against what the head holds and what the daemon said, never sent on trust.
    /// Without one the pane meant is the one this head holds, or the session's only one — and
    /// with several and no number the line is refused with their numbers, because guessing which
    /// program to kill is the one guess this card exists to prevent.
    pub(crate) fn begin_close(&mut self, which: Option<u32>) -> Option<Action> {
        // This head's own pane, when it is the one meant.
        if let Some(p) = &self.term
            && (which.is_none() || which == p.pane)
        {
            let Some(pane) = p.pane else {
                self.say(
                    "the pane has not started yet — the daemon has not numbered it. Nothing was \
                     ended; try again in a moment.",
                );
                self.redraw = true;
                return None;
            };
            let line = p.line.clone();
            self.ask_close(&line, pane);
            return None;
        }
        match (self.term_fact.clone(), which) {
            (PaneFact::Running(list), Some(n)) => match list.iter().find(|b| b.pane == n) {
                Some(b) => {
                    let line = format!("!term {}", b.command);
                    self.ask_close(&line, n);
                }
                None => {
                    self.say(&format!(
                        "there is no pane {n} running in this session — {}. Nothing was ended.",
                        panes_said(&list)
                    ));
                    self.redraw = true;
                }
            },
            (PaneFact::Running(list), None) if list.len() == 1 => {
                let line = format!("!term {}", list[0].command);
                self.ask_close(&line, list[0].pane);
            }
            (PaneFact::Running(list), None) => {
                self.say(&format!(
                    "this session has {} panes running — {}. `!term close N` says which; \
                     nothing was ended.",
                    list.len(),
                    panes_said(&list)
                ));
                self.redraw = true;
            }
            (PaneFact::None, _) => {
                self.say(
                    "this session has no pane to end — `!term COMMAND` starts one, and \
                     `!term` attaches to one that is already running",
                );
                self.redraw = true;
            }
            (PaneFact::Unasked, _) => {
                self.close_pending = true;
                self.close_which = which;
                self.say("asking the daemon what this session is running…");
                self.redraw = true;
                return Some(Action::TermStatus);
            }
        }
        None
    }

    /// **Raise the confirmation card**, and it is the only place [`App::term_ask`] is set — so
    /// there is one place a pane can be ended from, and it is the one that asks.
    pub(crate) fn ask_close(&mut self, line: &str, pane: u32) {
        // A card that names one of several says which by number; one that is the only pane
        // reads as it always did.
        let several = matches!(&self.term_fact, PaneFact::Running(l) if l.len() > 1);
        let line = if several {
            format!("pane {pane}, {line}")
        } else {
            line.to_string()
        };
        let line = line.as_str();
        self.term_ask = Some(TermAsk {
            line: line.to_string(),
            pane,
        });
        self.redraw = true;
    }

    pub(crate) fn switch_to(&mut self, id: String) -> Option<Action> {
        if id == self.session_id {
            self.close_surface(Surface::Picker);
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
            // The cat, until the session is here: a long one is seconds of restoring, and
            // the screen it would otherwise hold is the session being left — which, for a
            // head that just started, is an empty one that invites typing.
            self.fetching = Some(id.clone());
            self.begin_attach();
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
        // **The queues, for the edge** (R92 item 5). The composer's bottom edge draws this
        // session's repository's standing, and before this the only ask for the snapshot was
        // OPENING THE PANE: a head that never opened `/queue` drew no standing at all — the
        // operator's *"not all heads still show the queue"*. The answer also names this
        // session's repository, which a switch changes.
        if !self.queued.contains(&Action::ListMergeQueue) {
            self.queued.push(Action::ListMergeQueue);
        }
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
    /// **Whether the model was still working when the head gave up waiting.** The daemon
    /// interrupts the turns it holds on the way out, so this is not *a turn finishing its
    /// round* — it is what tells *a stop that is inside a command* from *a daemon that did not
    /// go*, and it is the head's to know because it is the head that was watching the turn.
    ///
    /// **`turn_busy` and not the state name**: the round a daemon finishes before it stops can be
    /// one whose tool call is executing, and a head that asked only whether a round was generating
    /// would call that stop *a refused one* when it was merely slow.
    pub turn_busy: bool,
    /// **The force path, when the head took it** — directly for an older daemon, or once,
    /// automatically, when the ordinary stop's deadline passed with the process still there.
    /// `None` for a stop that never needed it.
    pub forced: Option<Forcing>,
}

/// **Why the head stopped the daemon the forced way.** The operator, after an exit-and-stop
/// that did not take against an older daemon: *"yeah when i exited letidb and chose to stop
/// daemon too - it couldnt. had to --stop --force"* — and asked whether the head should
/// detect that and take the force path itself, with a line saying why: *"yes"*. Each variant
/// is one of those lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForceWhy {
    /// The daemon's `Hello` named an older protocol than this head speaks. Such a daemon may
    /// not read this head's stop frame at all — and this head holds its writes against it
    /// anyway (`App::skew_locked`) — so the head does not ask and wait: it goes straight to
    /// the force path, whose interrupt frame every daemon this head can attach to reads.
    OlderDaemon { daemon: u32, head: u32 },
    /// The ordinary stop was asked for and the process was still there at the deadline.
    DidNotStop { waited_ms: u64 },
}

impl ForceWhy {
    /// The sentence, said on the screen while the head waits and again in the farewell.
    pub fn line(&self) -> String {
        let how = format!(
            "stopping it the forced way: in-flight turns are interrupted first, then it is \
             sent SIGTERM, and SIGKILL if it is still there {}s later",
            FORCE_TERM_GRACE_MS / 1000
        );
        match self {
            ForceWhy::OlderDaemon { daemon, head } => format!(
                "the daemon is an older build (protocol {daemon}, this head speaks {head}) — {how}"
            ),
            ForceWhy::DidNotStop { waited_ms } => format!(
                "the daemon did not stop within {}s — {how}",
                waited_ms / 1000
            ),
        }
    }
}

/// The force path in flight, and its answer once it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forcing {
    pub why: ForceWhy,
    /// The head's own clock when the force path started.
    pub since_ms: u64,
    pub outcome: Option<ForceOutcome>,
}

/// **What the force path did** — `crate::head::force_stop`'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForceOutcome {
    /// The daemon's process is gone, observed (zombie-aware, as `watch_stop` observes it).
    pub gone: bool,
    /// What was done, in order, as the operator reads it: the interrupt's answer, each
    /// signal and what followed it, or why no signal was sent.
    pub said: String,
}

/// **How long the force path gives SIGTERM before SIGKILL**: five seconds, the figure
/// `scripts/letibot --stop --force` waits (`wait_gone`) between the two, so the head's force
/// path and the wrapper's are one path with one number.
pub const FORCE_TERM_GRACE_MS: u64 = 5_000;

impl Stopping {
    /// **Has the question been answered?** Either the daemon's process is gone — the
    /// operator's choice, carried out — or the deadline has passed and the head can say
    /// what it observed.
    ///
    /// **A force path in flight holds the head**, gone or not: it ends on its own
    /// observation of the process, within its own bounds, and leaving before it answers
    /// would drop the line saying what it did.
    pub fn resolved(&self, now_ms: u64) -> bool {
        match &self.forced {
            Some(f) => f.outcome.is_some(),
            None => self.gone || now_ms >= self.deadline_ms,
        }
    }

    /// The line the head draws while it waits, so the screen is never a freeze.
    pub fn waiting_line(&self, now_ms: u64) -> String {
        if let Some(f) = &self.forced {
            return format!(
                "stopping the daemon: {} — {} so far.",
                f.why.line(),
                dur_human(now_ms.saturating_sub(f.since_ms)),
            );
        }
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
            "stopping the daemon: {seen} — {} waiting, {} before this head stops it \
             the forced way. A turn already generating is interrupted on the \
             way out, not waited for.",
            dur_human(out),
            dur_human(left),
        )
    }
}

/// **How long a head waits for a daemon it has asked to stop** — and, since the head escalates
/// on its own, **how long an orderly stop is given before it is forced**.
///
/// It was five seconds, the wrapper's figure for *SIGTERM → SIGKILL* (`wait_gone` in
/// `scripts/letibot`), and that was fine while the deadline only decided when to print a
/// sentence. It now decides when to interrupt and signal, so it has to cover what an orderly
/// stop legitimately does after the request is read (`harnessd`'s `cli.rs`, after the worker
/// loop returns):
///
/// * the merge queue is joined, and a gate in flight is killed with its process group —
///   *SIGTERM, five seconds, SIGKILL* — **up to 5 s**;
/// * every subagent's VM is brought down (`firecode::down_all`);
/// * `daemon.shutdown()` joins the accept loop and unlinks the socket;
/// * `drain_subagents(SUBAGENT_EXIT_GRACE)` waits for the children it told to stop —
///   **up to 5 s**;
/// * `lifeline.stopped()` writes the stop time the next daemon measures its down time from
///   (one store open), and the heartbeat thread dies with the process.
///
/// Two bounded five-second waits back to back, plus the VMs and a store write: **fifteen
/// seconds** covers the slowest orderly stop with room, and a daemon that is still there
/// after it is not draining — it is wedged. Forcing one sooner would SIGKILL a daemon in
/// the middle of recording its own stop, which turns a clean restart into a crash the next
/// daemon reports. An orderly stop with nothing to drain takes milliseconds, so this costs
/// nothing in the ordinary case; the screen says what it is waiting for the whole time.
pub const STOP_DEADLINE_MS: u64 = 15_000;

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

/// **The group a tree is being drawn in** — `group: None` in the family view, which groups nothing.
///
/// One argument rather than two, and one type rather than a pair: `of` answers *which group is
/// each session in* for the list being drawn, and it is only meaningful together with the group
/// whose tree is being walked — passing the wrong slice with the wrong group is a mistake a caller
/// cannot make when they travel as one value.
#[derive(Debug, Clone, Copy)]
pub(crate) struct InGroup<'a> {
    pub(crate) group: Option<SessionGroup>,
    pub(crate) of: &'a [SessionGroup],
}

/// **One row of the session picker** — see [`App::session_rows`], which is the only thing that
/// builds one.
///
/// Three facts rather than a bare `usize`, because the list is NESTED **and GROUPED**: which
/// session it is (or that it is a group's own line), how deep it sits, and which group it was
/// drawn in. Deriving any of them at the drawing site instead is exactly how the drawing and the
/// keys become two enumerations again — the defect `todos_stops` exists to record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionRow {
    /// Index into `App::sessions`, or **`None` for a group's own line** — a stop with an act of
    /// its own (`enter` folds it) and no session to switch to.
    pub(crate) idx: Option<usize>,
    /// 0 for a group's line and for the top of a tree; deeper inside a tree.
    pub(crate) depth: usize,
    /// The group this row was drawn under — for a session row, the group it is IN; for a group's
    /// line, the group it heads.
    pub(crate) group: SessionGroup,
}

/// **The live panes, as a person reads them**: `1 `!term mc`, 2 `!term nano``.
pub(crate) fn panes_said(list: &[letibot_sessionlog::PaneBrief]) -> String {
    list.iter()
        .map(|b| format!("{} `!term {}`", b.pane, b.command))
        .collect::<Vec<_>>()
        .join(", ")
}
