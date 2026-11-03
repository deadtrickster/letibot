//! **The daemon's other frames**: the lists it answers with (sessions, jobs, the merge queue,
//! shell suggestions, todos, settings), its replies (accepted, rejected, a resync, a row fetched,
//! a peek, a diagnostic, a secret, bye), and the frame that carries an event.

use super::*;
use letibot_sessionlog::protocol::ServerFrame;
use letibot_sessionlog::view::Warned;

impl App {
    /// A confirmation of `allow-all` that another head asked for, and how it ended. The daemon posts
    /// it to every head that may decide except the asker, and it raises the same card the operator's
    /// own `/mode allow-all` raises; `ModeSettled` is that card going away, whoever answered.
    pub(crate) fn on_mode_card(&mut self, frame: ServerFrame) -> Disposition {
        match frame {
            ServerFrame::ModeConfirm {
                session_id,
                req_id,
                name,
                asked_by,
            } => {
                if session_id == self.session_id {
                    self.mode_confirm = Some(name.clone());
                    self.mode_asked = Some(AskedMode {
                        req_id,
                        by: asked_by,
                        name,
                    });
                    self.redraw = true;
                }
                Disposition::Control
            }
            ServerFrame::ModeSettled {
                session_id,
                req_id,
                name,
                applied,
                answer,
                said,
                ..
            } => {
                let mine = self.mode_asked.as_ref().is_some_and(|a| a.req_id == req_id);
                if mine {
                    // Taken on every path: the question is over whether or not this head was
                    // the one that answered it, and a card left up would own the keyboard for
                    // a request nobody can settle any more.
                    self.mode_asked = None;
                    self.mode_confirm = None;
                    self.redraw = true;
                }
                if session_id == self.session_id && mine {
                    let who = if answer == "yes" {
                        "confirmed"
                    } else {
                        "declined"
                    };
                    self.say(&format!(
                        "`{name}` {who}{}: {said}",
                        if applied { "" } else { " — not applied" }
                    ));
                }
                Disposition::Control
            }
            _ => Disposition::Control,
        }
    }

    /// **A list the daemon answered with**: sessions, jobs, the merge queue, shell suggestions, todos, settings. One family of [`App::apply`]'s arms, moved here
    /// verbatim; `apply` hands it only these frames.
    pub(crate) fn on_lists(&mut self, frame: ServerFrame) -> Disposition {
        match frame {
            ServerFrame::Sessions {
                sessions,
                current,
                created,
            } => {
                // Kept whole, like the `Hello` arm above — see it for why the filter is gone.
                self.sessions = sessions;
                // A LIST DOES NOT SEAT A HEAD: `current` is where the daemon's socket is and
                // `self.session_id` is the session this head is DRAWING. When they disagree the ROWS
                // are what is wrong, so this asks for a resync - a `Switch` would answer `already here`.
                if current != self.session_id {
                    self.queued.push(Action::Resync);
                }
                // And the rows the list is the source of are re-derived from it here: a `Sessions`
                // frame REPLACES `self.sessions`, which is what [`App::fold_subagents`] folds the
                // subagent rows and the composer's count from, so a stale pane would outlive the list.
                self.fold_subagents();
                match created {
                    // A session was made *because this head asked*. Going there is
                    // what was meant — `/new` that leaves you where you were is a
                    // command whose effect is invisible.
                    Some(id) if self.want_new_session => {
                        self.want_new_session = false;
                        self.queued.push(Action::Switch(id));
                    }
                    Some(id) => self.say(&format!("session {id} created")),
                    // NOT `self.picker = true`. A `Sessions` frame answers three questions - a list, a
                    // rename and a switch to the session you are already in - and only the first wants a
                    // picker; opening it here put the list over the header `/rename` had just changed.
                    None => {}
                }
                self.redraw = true;
                Disposition::Control
            }
            // The daemon's job table, whole: which jobs are listed, what the command reads as and what
            // the state word is are all its answers, so the head no longer folds them out of the event
            // stream and loses the name of a job that outlived its turn.
            ServerFrame::Jobs {
                session_id,
                jobs,
                monitors,
            } => {
                if session_id == self.session_id {
                    self.jobs = jobs;
                    self.jobs_sel = self.jobs_sel.min(self.job_stops().len().saturating_sub(1));
                    // **And the monitors, from the same answer** — what this session is watching.
                    self.monitors = monitors;
                    self.monitors_sel = self
                        .monitors_sel
                        .min(self.monitor_stops().len().saturating_sub(1));
                    self.redraw = true;
                }
                Disposition::Control
            }
            // The standing notes, whole - the pane's bootstrap read, and the rows are the daemon's own
            // reading of the corpus, so the head draws what the model was given rather than a second
            // opinion. Only the file's size, age and presence are read at DRAW time.
            ServerFrame::StandingNotes { session_id, notes } => {
                if session_id == self.session_id {
                    self.standing = notes;
                    self.standing_sel =
                        self.standing_sel.min(self.standing.len().saturating_sub(1));
                    self.redraw = true;
                }
                Disposition::Control
            }
            // **The skills pane's rows.** Names, one line each and where each was found — the
            // index the prompt carries, which is what makes the pane and the prompt one list.
            ServerFrame::Skills { session_id, skills } => {
                if session_id == self.session_id {
                    self.skills = skills;
                    self.skills_sel = self.skills_sel.min(self.skills.len().saturating_sub(1));
                    self.redraw = true;
                }
                Disposition::Control
            }
            // **Both queues, whole** (R61) — the daemon's one snapshot, kept as it came, and
            // cut to the pane's scope (R92 item 5): the daemon names the asking session's
            // repository, and the pane and the edge are about that one unless told otherwise.
            ServerFrame::Queues { snapshot } => {
                self.queues_all = snapshot;
                self.queues_got_ms = self.now_ms.max(1);
                self.rescope_queues();
                // **The one thing a snapshot cannot say about itself** — whether a running step's
                // output has moved since the last one (see `App::track_gate_tails`).
                self.track_gate_tails();
                self.redraw = true;
                Disposition::Control
            }
            // The model's proposed `!` completions, answered: correlated by the id this head minted, so
            // a suggestion is cached under the prefix it was asked for, and an id no longer held is
            // stale and dropped. Nothing here fills the composer - only a Tab does, and Enter is yours.
            ServerFrame::ShellSuggestions {
                client_request_id,
                prefix: _,
                lines,
            } => {
                if let Some((prefix, position)) = self.shell_ask.remove(&client_request_id) {
                    self.shell_suggestions.insert((prefix, position), lines);
                    self.redraw = true;
                }
                Disposition::Control
            }
            ServerFrame::Todos { session_id, todos } => {
                if session_id == self.session_id {
                    self.todos = todos;
                    // The board has been read HERE, and from now on it is the truth about who waits on
                    // the operator (see [`App::asks_board`]): the settings row's copy is a sentence from
                    // attach time that nothing refreshes.
                    self.asks_board = Some(self.session_id.clone());
                    // The board is the truth once it arrives: a row answered from here is either
                    // off it or — the answer refused — still asking, and asked again.
                    self.todo_answered.clear();
                    self.prune_todo_asks();
                    // A waiting seed runs here too: this is the REPLY to the `ListTodos` the attach
                    // queues when a seed is due, and it reads the board as whole as the event does. One
                    // seed, two arrivals, because the wire has two.
                    if self.todo_seed_pending {
                        self.todo_seed_pending = false;
                        self.seed_todos();
                    }
                    self.redraw = true;
                }
                Disposition::Control
            }
            ServerFrame::Settings { rows } => {
                self.settings = rows;
                // The stood-down compaction, for a head that attached AFTER the guard fired: the warning
                // arm is the live half, this is the durable one, read whenever settings arrive. Keyed on
                // the `off -` spelling, because a `--no-auto-compact` the operator typed is not news.
                self.auto_compact_off = self.settings.iter().find_map(|r| {
                    (r.key == "auto-compact").then(|| {
                        r.value
                            .strip_prefix("off —")
                            .map(|why| format!("auto-compaction is off: {}", why.trim()))
                    })?
                });
                // The operator's `/nag pause`, for a head that attached after it - the durable half, as
                // `auto_compact_off` reads its row above. Keyed on the `paused` spelling the daemon
                // writes only for a pause; `on`, or a daemon too old to send the row, clears it.
                self.nags_paused = self.settings.iter().find_map(|r| {
                    (r.key == "nags").then(|| {
                        r.value
                            .strip_prefix("paused")
                            .map(|until| until.trim().to_string())
                    })?
                });
                // Whether the board waits on the operator, for a head that attached after the question
                // was asked - the durable half of the `waiting on you` line. `none`, or a daemon too old
                // to send the row, clears it; with no board of its own the head reads one.
                self.asks_setting = self
                    .settings
                    .iter()
                    .find_map(|r| (r.key == "asks" && r.value != "none").then(|| r.value.clone()));
                if self.asks_setting.is_some()
                    && self.operator_asks().is_empty()
                    && !self.queued.contains(&Action::ListTodos)
                {
                    self.queued.push(Action::ListTodos);
                }
                // Stamped, so the header can tell whether a turn has named a model
                // since. See `model_from_settings_at`.
                self.model_from_settings_at = self.seq;
                // A PICKER THAT OPENED BEFORE ITS OWN LIST ARRIVED SEEDS NOW (see [`App::pick_unseeded`]):
                // `/mode` and `/models` send the ask and draw the card in the same breath, so an unseeded
                // cursor sits on row 0 with the mark further down. Only while the cursor is untouched.
                if self.pick.is_some() && self.pick_unseeded {
                    self.seed_pick();
                }
                self.redraw = true;
                Disposition::Control
            }
            _ => unreachable!("on_lists was handed a variant it does not handle"),
        }
    }

    /// **A reply**: a diagnostic, a fetched row, a peek, a secret, a resync, accepted, rejected, bye. One family of [`App::apply`]'s arms, moved here
    /// verbatim; `apply` hands it only these frames.
    pub(crate) fn on_replies(&mut self, frame: ServerFrame) -> Disposition {
        match frame {
            // R11's locator, answered: the bytes go to the pane that asked, on the channel `Peeked` and
            // `JobOutput` use - a pane, not the conversation, because a brief scrolled past in the chat
            // is one nobody finds again. `None` reads as NOT RECORDED, never as empty.
            ServerFrame::Diagnostic {
                request_id,
                kind,
                body,
                total,
            } => {
                let what = match kind {
                    letibot_sessionlog::protocol::DiagnosticKind::Brief => "the brief it was shown",
                    letibot_sessionlog::protocol::DiagnosticKind::Reply => "the reply it gave",
                };
                let mut lines: Vec<String> = vec![format!("{request_id} — {what}")];
                match &body {
                    Some(b) if b.is_empty() => {
                        lines.push(String::new());
                        lines.push("(recorded, and zero bytes)".into());
                    }
                    Some(b) => {
                        lines.push(String::new());
                        lines.extend(b.lines().map(str::to_string));
                        lines.push(String::new());
                        lines.push(format!("  {total} bytes"));
                    }
                    None => {
                        lines.push(String::new());
                        lines.push(
                            "not recorded. A row written before the exchange was kept has no \
                             brief and no reply, and an oracle that never answered has no \
                             reply either — this says which case it is not."
                                .into(),
                        );
                    }
                }
                self.slash_out = Some(("diagnostic".to_string(), lines));
                self.push_surface(Surface::Listing);
                Disposition::Control
            }
            // The head does not send `FetchRow`: its paging works over the rows the snapshot gave
            // it, so this arm stays explicit rather than a wildcard, and an answer that arrives one
            // day is visible here instead of swallowed.
            ServerFrame::RowFetched { .. } => Disposition::Control,
            // The rows above the window, or the ones filling the gap to the tail: a page names the
            // ordinal it starts at, so the window stays one contiguous run of the session, and the
            // direction is decided by that ordinal rather than by a second field to keep in step.
            ServerFrame::RowsFetched { from, rows, .. } => {
                // The ask is spent whatever came back, and an empty page is the daemon's answer
                // that nothing is there - above the window or below it - so asking again is a loop.
                // Which ask it was is the ordinal it names, the same rule `apply_rows` places by.
                if from >= self.items_base + self.items.len() {
                    self.fetch_below = None;
                } else {
                    self.fetch_asked = None;
                }
                self.apply_rows(from, rows);
                Disposition::Rendered
            }
            ServerFrame::Peeked {
                session_id,
                dropped,
                events,
                snapshot,
            } => {
                // **A read nobody is waiting for any more is not opened** (R90.17): the surface
                // that asked was closed meanwhile (`App::forget_surface`), and the output would
                // land over whatever the operator went on to.
                if self.sub_out_pending.take().is_none() {
                    return Disposition::Control;
                }
                // The session's rows, when the daemon sent them - and that is the ORDINARY path now: a
                // child is a session, so its rows are drawn by the one renderer that draws rows, and the
                // plain-string path below is what an older daemon falls back to.
                let (lines, degraded) = match &snapshot {
                    Some(s) => (self.sub_out_from_rows(&s.items), false),
                    None => (subagent_out_lines(&events), true),
                };
                let mut lines = lines;
                if lines.is_empty() {
                    lines.push(match snapshot {
                        // A session with nothing in it is a different statement from a session
                        // whose rows could not be read, and the two must not look alike.
                        Some(_) => "    this session has no rows yet.".to_string(),
                        None => "    this subagent's scrollback has neither an answer nor tool \
                                  output. It may still be running, or its rows may have fallen \
                                  off the daemon's ring."
                            .to_string(),
                    });
                }
                let spill = spill_sub_out(&session_id, &lines);
                self.sub_out = Some(SubOut {
                    session_id,
                    lines,
                    degraded,
                    scroll: 0,
                    spill,
                    dropped,
                });
                // **On top of whatever is drawn** — the tree it was read from, or the
                // conversation for a `/peek` typed with no pane open — so Esc goes back to it.
                self.push_surface(Surface::SubOut);
                Disposition::Rendered
            }
            // Only ever written to an `askpass` head; a TUI that sees one has a
            // daemon confused about who it is talking to.
            ServerFrame::Secret { .. } => Disposition::Control,
            ServerFrame::Resync {
                reason,
                dropped,
                snapshot,
                scrubbed,
            } => {
                self.resyncs += 1;
                self.dropped += dropped;
                self.scrubbed += scrubbed.total();
                // Through `say`, so this line has a clock like every other: the `resyncs` counter holds
                // it, the alarm triangle repeats it and `/status` spells it out, so it is news like the
                // rest, and `say` is the one writer that starts a countdown.
                self.say(&format!("resync: {reason}"));
                self.load(*snapshot);
                // And the board is re-read, for the reason the attach reads it: a resync is the daemon
                // saying this head fell behind, and the board is not in a snapshot, so the copy in hand
                // may name a row whose question was taken back while the events were dropped.
                if !self.queued.contains(&Action::ListTodos) {
                    self.queued.push(Action::ListTodos);
                }
                Disposition::Control
            }
            ServerFrame::Accepted { note, seq, .. } => {
                // The daemon answering a STOP is the one acceptance that is not routine: it is the only
                // evidence the request was READ, where the frame being written says only that the bytes
                // reached the kernel. Recorded rather than said - a notice would be the same fact twice.
                if note == letibot_sessionlog::NOTE_STOPPING
                    && let Some(s) = &mut self.stopping
                {
                    s.acked = true;
                    self.redraw = true;
                } else if note != letibot_sessionlog::protocol::NOTE_PROMPT_QUEUED {
                    // Telling the person who just pressed enter that their prompt was accepted is not
                    // information, and the old head left exactly that sitting on the input line for the
                    // rest of the session. Anything OTHER than the routine acceptance still gets said.
                    self.say(&note);
                }
                // The daemon saying where it is: the only route by which a head learns it is BEHIND
                // rather than at the end, since both look the same from inside. A `seq` below this head's
                // own is a redelivery, not a gap; only a greater one is a distance, and a lower bound.
                self.behind = seq.saturating_sub(self.seq);
                Disposition::Control
            }
            ServerFrame::Rejected {
                client_request_id,
                reason,
                expected_seq,
                actual_seq,
            } => {
                // **A refused answer puts its card back** (R90.29): it was closed when the
                // answer left.
                self.answer_refused(&client_request_id, &reason);
                // Both numbers, so the operator can see what they were looking at.
                // A peek answered with one of these also ends its waiting.
                self.sub_out_pending = None;
                // And a refusal while the head is between seats is the trip's: not in the store, a restore
                // that failed, a switch to a session that went away. Any of them ends the wait - the head
                // stays where it is and the line below says why. Matched on the state, not the reason.
                if self.fetching.take().is_some() {
                    self.attaching = false;
                }
                // And this is the daemon naming a seq this head has not reached - the same fact `Accepted`
                // carries, stated the other way round. A refusal "for a stale `expected_seq`" is what
                // being behind produces, so the head had better know the distance.
                self.behind = actual_seq.saturating_sub(self.seq);
                self.say(&format!(
                    "rejected: {reason} (you saw {expected_seq}, the session is at {actual_seq})"
                ));
                Disposition::Control
            }
            ServerFrame::Bye { reason } => {
                // Into the transcript, AND kept for after the terminal is restored: a `Bye` is the last
                // thing this head draws and the frame it lands in is about to go with the alternate
                // screen, so on the path that matters most - a skew - a head must not vanish silently.
                self.say(&format!("daemon: {reason}"));
                self.bye = Some(reason.clone());
                self.quit = true;
                Disposition::Control
            }
            _ => unreachable!("on_replies was handed a variant it does not handle"),
        }
    }

    /// **A frame carrying a session event**, handed on to [`App::event`]. One family of [`App::apply`]'s arms, moved here
    /// verbatim; `apply` hands it only these frames.
    pub(crate) fn on_event_frame(&mut self, frame: ServerFrame) -> Disposition {
        match frame {
            ServerFrame::Event(env) => {
                // `seq` is dense, so a jump is proof of a miss: the daemon assigns it and nobody else does,
                // and a head that assigned unconditionally could not tell a delivered row from a dropped
                // one. Guarded on the session so a `Switch` cannot read as a gap; 0 is a head with no mark.
                if self.seq > 0 && env.session_id == self.session_id && env.seq > self.seq + 1 {
                    let lost = env.seq - self.seq - 1;
                    self.gaps += 1;
                    // **Said in the conversation, once per gap, and the range is
                    // named.** Two gaps have to be two lines; deduping on the code
                    // would collapse a session's whole history of them into one.
                    self.note(Note::Warned(Warned {
                        code: "log_gap".into(),
                        detail: format!(
                            "{lost} event(s) never reached this head: seq {}..{} are \
                             missing, and the conversation you are reading has a hole in \
                             it. The daemon has them and this head does not — asking for a \
                             resync rebuilds from the snapshot, and `/status` counts how \
                             often this has happened.",
                            self.seq + 1,
                            env.seq - 1
                        ),
                        ts: 0,
                    }));
                    // **Repaired, and the repair is the daemon's to make.** A resync
                    // is the one thing that can put the two back in step, and the head
                    // cannot take a snapshot of a transcript it does not hold.
                    if !self.queued.iter().any(|a| matches!(a, Action::Resync)) {
                        self.queued.push(Action::Resync);
                    }
                }
                self.seq = env.seq;
                self.last_event_at = self.now_ms;
                let ts = env.ts;
                let d = self.event(env.event, ts);
                match d {
                    Disposition::Rendered => self.rendered += 1,
                    Disposition::Filtered => self.filtered += 1,
                    Disposition::Control => {}
                }
                d
            }
            _ => unreachable!("on_event_frame was handed a variant it does not handle"),
        }
    }
}
