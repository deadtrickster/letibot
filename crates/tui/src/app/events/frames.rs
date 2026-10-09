//! **The daemon's other frames**: the lists it answers with (sessions, jobs, the merge queue,
//! shell suggestions, todos, settings), its replies (accepted, rejected, a resync, a row fetched,
//! a peek, a diagnostic, a secret, bye), and the frame that carries an event.

use super::*;
use letibot_sessionlog::protocol::ServerFrame;
use letibot_sessionlog::view::Warned;

impl App {
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
                self.session_id = current;
                // **And the rows the list is the source of are re-derived from it, here.**
                //
                // A `Sessions` frame REPLACES `self.sessions`, and `self.sessions` is what
                // [`App::fold_subagents`] folds the subagent rows and the composer's count
                // from — so a list that lands without this line moves the picker and leaves
                // the pane and the count standing on the list that was just thrown away.
                // That is the defect this arm had, and it is what made a re-ask useless:
                // the answer to *what are my children now* arrived and was not applied.
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
                    // **Not** `self.picker = true`. A `Sessions` frame is the answer
                    // to three different questions — a list, a rename, and a switch
                    // to the session you are already in — and only the first of them
                    // wants a picker. Opening it here put the session list over the
                    // screen after `/rename`, which the operator then had to dismiss
                    // to see the header they had just changed. The key and the
                    // command that ask for a list already open it themselves.
                    None => {}
                }
                self.redraw = true;
                Disposition::Control
            }
            // The bootstrap read for the todos pane. The session named is the one
            // the daemon answered for; a head that has since switched keeps what
            // it has until the pane is opened again, which re-asks.
            // **The daemon's job table, whole.** Which jobs are listed, what the
            // command reads as and what the state word is are all its answers —
            // the head used to fold them out of the event stream and join the
            // command against whichever turn it happened to be showing, which is
            // how a job that outlived its turn lost its name.
            ServerFrame::Jobs { session_id, jobs } => {
                if session_id == self.session_id {
                    self.jobs = jobs;
                    self.jobs_sel = self.jobs_sel.min(self.job_stops().len().saturating_sub(1));
                    self.redraw = true;
                }
                Disposition::Control
            }
            // **The merge queue, whole, and the reviewer's verdicts beside it.** The queue is
            // daemon-level, so there is no `session_id` to check against this head's — the
            // `session_id` on an entry is its origin and not a filter, which is why the same
            // frame reaches a head attached to any session.
            ServerFrame::MergeQueue { entries, reviews } => {
                self.merge = entries;
                self.merge_reviews = reviews;
                self.queue_sel = self.queue_sel.min(self.merge.len().saturating_sub(1));
                self.redraw = true;
                Disposition::Control
            }
            // **The model's proposed `!` completions, answered.** The answer to a
            // `SuggestShell` this head sent, correlated by the id it minted: a
            // suggestion cached under the wrong prefix is a wrong suggestion, and the
            // id is what tells one answer from another. An id this head is not
            // holding — the transcript advanced and the ask was cleared — is stale
            // and is dropped, because a suggestion about a conversation that moved is
            // a suggestion about the wrong conversation.
            //
            // **Nothing here fills the composer.** The lines are cached and drawn as
            // candidates, with their provenance; only a Tab fills the composer with
            // one, and Enter is still the operator's.
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
                    // **A waiting seed runs here too** — this is the REPLY to the `ListTodos` the
                    // attach queues when a seed is due, and it reads the board as whole as the
                    // event does. One seed, two arrivals, because the wire has two: the reply and
                    // the announcement.
                    if self.todo_seed_pending {
                        self.todo_seed_pending = false;
                        self.seed_todos();
                    }
                    self.redraw = true;
                }
                Disposition::Control
            }
            // The answer to the tree's read key (`p`, or `/peek ID`): the named subagent's
            // scrollback, scrubbed as a replay. Read, never folded — these events are not
            // this session's history, and folding them would lie about whose
            // turn is whose. The pane shows the tool results; the whole view is
            // spilled to a file so no cap on the pane is a cap on the record.
            ServerFrame::Settings { rows } => {
                self.settings = rows;
                // Stamped, so the header can tell whether a turn has named a model
                // since. See `model_from_settings_at`.
                self.model_from_settings_at = self.seq;
                // **A PICKER THAT OPENED BEFORE ITS OWN LIST ARRIVED SEEDS NOW** — see
                // [`App::pick_unseeded`]. `/mode` and `/models` send the ask and draw the card in
                // the same breath, so on a head whose rows have not landed the cursor goes to row
                // 0 and the current row is marked further down with the cursor somewhere else.
                //
                // **Only while the cursor is still untouched.** A frame landing mid-arrow must not
                // snap the reader back to where they started, which is the reason this was not
                // re-seeded at all before; the flag is what tells the two cases apart.
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
            // **A fetched row window the head did not ask for.**
            //
            // `FetchRow` exists and is answered (see `ClientFrame::FetchRow` and the
            // server's arm), and **this head does not send it yet**. Its paging works over
            // the rows the snapshot gave it: `ViewBounds` is 2000 rows and 8 MB, and the
            // head's own paging draws windows of a payload it already holds. What is
            // missing is the case where the row is *not* held — trimmed by those bounds —
            // and that needs the head to ask, track the answer, and render a partial body.
            // Filed in `TODO.md` R19.2 rather than half-built here.
            //
            // The arm is explicit rather than a wildcard so that the day the head starts
            // asking, a frame that arrives unhandled is visible here rather than swallowed
            // by a `_ =>`.
            // **R11's locator, answered.** The bytes go to the pane that asked, on the same
            // channel `Peeked` and `JobOutput` use — a pane, not the conversation, because a
            // head asked for them to READ and a brief scrolled past in the chat is a brief
            // nobody finds again.
            //
            // **`None` reads as *not recorded*, never as empty.** The store holds `NULL` on
            // every row written before R11 kept the exchange, and an oracle that never answered
            // has no reply either — the two are one sentence to a reader and neither is *"here
            // it is, and it is nothing"*.
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
                self.pane_scroll = 0;
                self.redraw = true;
                Disposition::Control
            }
            ServerFrame::RowFetched { .. } => Disposition::Control,
            ServerFrame::Peeked {
                session_id,
                dropped,
                events,
                snapshot,
            } => {
                self.sub_out_pending = None;
                // **The session's rows, when the daemon sent them** — and the whole point of the
                // field is that this is the ORDINARY path now. A child is a session (the operator,
                // 2026-10-03: *"yes subagents are not even scratch session they are session, just
                // sub sessions"*), so its rows are drawn by the one renderer that draws rows, and
                // the hand-rolled plain-string path below is what an older daemon falls back to.
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
                self.redraw = true;
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
                // **Through `say`, so this line has a clock like every other.** It used to
                // be a direct write to the slot, which armed no countdown: the sentence
                // then stood until the next notice replaced it, for the rest of the
                // session. The reason is worth reading and is not worth keeping — the
                // `resyncs` counter holds it, the alarm triangle repeats it, and
                // `/status` spells it out — so it is news like the rest, and `say` is the
                // one writer that starts a clock.
                self.say(&format!("resync: {reason}"));
                self.load(*snapshot);
                Disposition::Control
            }
            ServerFrame::Accepted { note, seq, .. } => {
                // **The daemon answering a STOP is the one acceptance that is not
                // routine.** R30: it is the only evidence that the request was *read* —
                // the frame being written says the bytes went to the kernel, and this says
                // a process on the other end understood them. Recorded rather than said:
                // the head is already drawing `stopping the daemon: the daemon answered`
                // from the state, and a notice on top of it would be the same fact twice.
                if note == letibot_sessionlog::NOTE_STOPPING
                    && let Some(s) = &mut self.stopping
                {
                    s.acked = true;
                    self.redraw = true;
                } else if note != letibot_sessionlog::protocol::NOTE_PROMPT_QUEUED {
                    // Telling the person who just pressed enter that their prompt was
                    // accepted is not information — and the old head left exactly that
                    // sitting on the input line for the rest of the session. Anything
                    // *other* than the routine acceptance still gets said.
                    self.say(&note);
                }
                // **The daemon saying where it is** (R17). This is the only route by
                // which a head learns it is *behind* rather than *at the end*: both
                // look the same from inside — an empty queue and a screen that has
                // drawn everything it was given — and on 2026-09-22 the difference
                // was 36 rows that were in the ledger and not on the screen.
                //
                // A `seq` below this head's own is a redelivery, not a gap; only a
                // greater one is a distance, and it is a lower bound, because the
                // daemon has moved on since it answered.
                self.behind = seq.saturating_sub(self.seq);
                Disposition::Control
            }
            ServerFrame::Rejected {
                reason,
                expected_seq,
                actual_seq,
                ..
            } => {
                // Both numbers, so the operator can see what they were looking at.
                // A peek answered with one of these also ends its waiting.
                self.sub_out_pending = None;
                // And a refusal while the head is between seats is the trip's: not in the
                // store, a restore that failed, a switch to a session that went away. Any of
                // them ends the wait — the head stays where it is, and the line below says
                // why. Matched on the state rather than the reason, because a failed restore
                // carries whatever the store said.
                if self.fetching.take().is_some() {
                    self.attaching = false;
                }
                // **And this is the daemon naming a seq this head has not reached**
                // (R17) — the same fact `Accepted` carries, stated the other way round.
                // A refusal "for a stale `expected_seq`" is exactly what being behind
                // produces, so the head had better know the distance rather than
                // apologising for the operator's screen.
                self.behind = actual_seq.saturating_sub(self.seq);
                self.say(&format!(
                    "rejected: {reason} (you saw {expected_seq}, the session is at {actual_seq})"
                ));
                Disposition::Control
            }
            ServerFrame::Bye { reason } => {
                // Into the transcript, AND kept for after the terminal is restored.
                // A `Bye` is the last thing this head will draw, and the frame it is
                // drawn into is about to be torn down with the alternate screen — so
                // on the path that matters most, a version skew, the operator saw a
                // head vanish and nothing else. See `App::farewell`.
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
                // **R17: `seq` is dense, so a jump is proof of a miss.**
                //
                // The daemon assigns it and nobody else does; a head that assigns
                // unconditionally cannot tell a delivered row from a dropped one.
                // Guarded on the session so a `Switch` — whose events may arrive on
                // the same socket before the new `Hello` has been folded — cannot
                // read the other session's numbering as a gap in this one's.
                //
                // Nothing is done about a `self.seq` of 0: that is a head with no
                // mark yet, and the first thing it hears is not a gap.
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
