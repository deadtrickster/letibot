//! **The daemon's events, applied**: a snapshot loaded, a frame applied, a transcript row
//! recorded, and the bookkeeping of forks and echoed prompts.

use super::*;
use crate::markdown::IncrementalMarkdown;
use crate::render::BlockCache;
use letibot_sessionlog::event::{DeltaTarget, SessionEvent, Usage};
use letibot_sessionlog::protocol::ServerFrame;
use letibot_sessionlog::view::{
    CallState, OpenDecision, SettledDecision, Snapshot, SnapshotItem, TurnState, Warned,
};
use letibot_transcript::{TranscriptItem, UserPart};
use letibot_ui::text::{without_control, without_control_lines};

impl App {
    pub fn open_decisions(&self) -> &[OpenDecision] {
        &self.open
    }

    /// Apply one frame. Never sends anything; see the module note on acking.
    pub fn apply(&mut self, frame: ServerFrame) -> Disposition {
        match frame {
            // Once per *attachment*, which is now more than once per connection: a
            // `Switch` is answered with a second `Hello`, and this arm is the whole
            // of the head's switch path. That is the reason the daemon answers a
            // switch with a `Hello` rather than a frame of its own — the late-join
            // path is the best-tested path in this head, and a second one that
            // "also seats you somewhere" is a second one to keep in step.
            ServerFrame::Hello {
                protocol_version,
                session_id,
                head_id,
                dropped,
                snapshot,
                scrubbed,
                wiring,
                sessions,
                ..
            } => {
                let moved = !self.session_id.is_empty() && self.session_id != session_id;
                // The daemon has answered, so whatever the head drew while it was
                // asking is about to be replaced by the truth. See `attaching`.
                self.attaching = false;
                // **The version the daemon is TOLD to be, checked at the handshake.**
                //
                // This is the one moment a skew is cheap to say: nothing has been read
                // yet, the direction is known, and the sentence can name both numbers
                // while the operator is still looking at the head rather than at a frame
                // that failed to parse. `protocol_skew` is `None` when they match — the
                // normal case says nothing at all, so a line here is a fact and not
                // furniture.
                //
                // **It does not exit.** A skew is usually survivable — that is R3's whole
                // argument — so the head says which way round it is and carries on. What
                // differs is what the operator should expect, and that is why the two
                // directions get different sentences: a newer daemon means frames this
                // head will report and skip, an older one means the next command the two
                // do not share ends the session.
                self.daemon_protocol = Some(protocol_version);
                // **And which daemon that is.** A head outlives the daemon that gave it its
                // facts — the operator's box has had three `letibot-tui` processes up for days
                // while the daemon was replaced underneath them — so the party at the other end
                // of this socket has to be checked rather than assumed. The pid is re-read per
                // connection by the caller that opened it, so by the time this arm runs it is
                // *this* daemon's, and the protocol rides this frame.
                //
                // **The check is on the SEATING, and the seating is where it belongs**: a
                // `Hello` is an attach, a re-attach and the return from a switch, and a switch
                // is answered on the same socket by the same process — so this fires once per
                // connection that lands somewhere new, and not once per keystroke.
                let seat = DaemonSeat {
                    pid: self.daemon_pid,
                    protocol: protocol_version,
                };
                let was = self.daemon_seat;
                self.daemon_seat = Some(seat);
                // **Held, not filed.** `load` replaces `self.notes` wholesale a few lines down,
                // so a note written here is thrown away — the same reason the skew's sentence
                // and the reattach's are held rather than said. See `App::link_up`.
                //
                // **Both numbers on both sides, and `None` is said as `not told`.** A pid the
                // kernel declined to name is not a pid of zero, and the same rule R30 keeps on
                // the farewell applies here: a head that printed a number it did not have would
                // send the operator to `ps` for a process that is not there.
                let replaced_said = was.filter(|w| *w != seat).map(|w| {
                    format!(
                        "this is not the daemon this head was attached to. That one was {} and \
                         spoke protocol {}; this one is {} and speaks {}. Everything the old one \
                         told me that was its own — its session list, its job table, its children \
                         — has been asked for again, because a daemon's registry is in memory \
                         and a replacement holds none of it. What you are reading below is what \
                         THIS daemon holds.",
                        pid_word(w.pid),
                        w.protocol,
                        pid_word(seat.pid),
                        seat.protocol,
                    )
                });
                // **The `Hello` is what says the link is back.** It is the frame that
                // seats this connection, so it is the only honest answer to "are we
                // attached" — a socket that accepts and then says nothing is not.
                // `link_up` is a no-op when nothing was down, which is every ordinary
                // attach and the second `Hello` a `Switch` produces.
                let reattached = self.link_up();
                let mut skew_said = None;
                if let Some(said) = letibot_sessionlog::protocol_skew(
                    protocol_version,
                    letibot_sessionlog::protocol::PROTOCOL_VERSION,
                ) {
                    // **Held until after the snapshot is folded in.** `load` replaces
                    // `self.notes` wholesale — a snapshot's warnings are the head's whole
                    // warning history — so a note filed before it is not "anchored at the
                    // frame that revealed this", it is thrown away. Found by the test
                    // that asserts the sentence exists, which is the only reason this is
                    // not a silence nobody would have noticed.
                    skew_said = Some(said);
                }
                // **And every session-scoped read this head owes itself, in one place.**
                //
                // This is the attach, the re-attach (a reconnect is answered with a
                // `Hello` too) and the return from a switch — the daemon answers a
                // `Switch` with a second `Hello`, which is why there is one call here
                // and not three. See [`App::refetch_session_facts`] for what a switch
                // drops and why each of the three is asked for rather than waited on.
                //
                // **And what this session's pane is running** — the read behind the head's own
                // line about a program it is not drawing. Asked here, with the others, for
                // the same shape of reason: the daemon answers a read and never volunteers one,
                // and a head that has just been SEATED somewhere knows nothing about the pane
                // there — its own pane went with the session it left (see [`App::load`]), and
                // the pane in the new one is the session's, not this head's.
                //
                // **`Unasked` rather than `None`**, because those are different facts and one
                // of them decides whether `!term close` asks or refuses — see [`PaneFact`].
                self.term_fact = PaneFact::Unasked;
                self.close_pending = false;
                self.term_ask = None;
                self.queued.push(Action::TermStatus);
                self.refetch_session_facts();
                self.head_id = head_id.clone();
                self.seated = Some(head_id);
                self.wiring = wiring;
                // **Sub-sessions are KEPT, and the belief that used to filter them here is the
                // defect this line closes.** The operator, 2026-10-03: *"yes subagents are not even
                // scratch session they are session, just sub sessions"*, and *"why readonly?
                // subagent session is more like you driving others via tmux"*. A child is a
                // session the daemon holds, a head can post to it and it answers — so a head that
                // drops it is throwing away a session it can DRIVE.
                //
                // What the filter was protecting against was noise, and that is
                // [`App::session_rows`]'s business now: nested, collapsed by default, with the
                // parent's id already on the row.
                self.sessions = sessions;
                self.dropped += dropped;
                self.scrubbed += scrubbed.total();
                // `session_id` is assigned by `load` and **not before it**: `load`
                // decides whether this is the same session by comparing the two,
                // and assigning first made that comparison always true — so a
                // switch kept the previous session's token count and model on the
                // header, over the new session's empty transcript. Seen under tmux:
                // a brand-new session claiming `4470 ctx · 34% cached`.
                match snapshot {
                    Some(s) => self.load(*s),
                    // A resume served from the scrollback: no snapshot, and the
                    // state that is already here is this session's.
                    None => self.session_id = session_id,
                }
                // **Now it can be said.** A note rather than a `say`: this is a fact about
                // the connection that outlives the next keystroke, and it belongs in the
                // conversation with everything else that happened — anchored at the end of
                // whatever the snapshot carried, which is where the reader is. `note`
                // dedupes on `(code, detail, ts)`, so the second `Hello` a `Switch`
                // produces does not say it twice.
                if let Some(said) = skew_said {
                    self.note(Note::Warned(Warned {
                        code: "protocol_skew".into(),
                        detail: said,
                        ts: 0,
                    }));
                }
                if let Some(said) = replaced_said {
                    self.note(Note::Warned(Warned {
                        code: "daemon_replaced".into(),
                        detail: said,
                        ts: 0,
                    }));
                }
                if let Some(said) = reattached {
                    self.note(Note::Warned(Warned {
                        code: "reattached".into(),
                        detail: said,
                        ts: 0,
                    }));
                }
                // A daemon that restarted has no turn state in the snapshot —
                // `TurnFinished` is ephemeral, and a view rebuilt from the
                // transcript has no turn — so the context the last turn left
                // behind comes from the session's own row: the daemon writes it
                // on every round finish, and the brief carries it here. A live
                // session's snapshot already set the usage, and that wins.
                if self.usage.is_none()
                    && let Some(b) = self
                        .sessions
                        .iter()
                        .find(|s| s.session_id == self.session_id)
                    && let Some(tokens) = b.context_tokens
                {
                    // The row knows the prompt size; it knows the cache fraction
                    // only if a turn finished after the column existed. A
                    // backfilled row has the size and not the fraction, and the
                    // header shows the percentage only when it was measured.
                    self.usage_cache_measured = b.context_cached.is_some();
                    self.usage = Some(Usage {
                        prompt_tokens: tokens,
                        cached_tokens: b.context_cached.unwrap_or(0),
                        predicted_tokens: 0,
                        // A backfilled row carries the prompt size, never the
                        // cost: nothing recorded it per turn before now, and a
                        // zero here would report a metered session as free.
                        cost_micros_usd: None,
                    });
                }
                if moved {
                    // The picker is closed by arriving, not by the key that opened
                    // it: the switch is the answer to the question the picker
                    // asked, and leaving it up over the session you just joined is
                    // a screen the operator has to dismiss for no reason.
                    self.picker = false;
                    self.say(&format!(
                        "switched to {}",
                        self.session_label(&self.session_id)
                    ));
                }
                // **AND THE STARTER TODOS** — leticl's `%seed-operator-todos`, on the one moment
                // leticl runs it: the HELLO, where a head learns its project (its own docstring:
                // *"this is the one function where a head learns its list, so it is the one place
                // a list can be STARTED"*). A switch lands here too, so each project is checked
                // in its own right. The seed itself waits for the board — see `todo_seed_pending`
                // — because this head keeps no second list and must not send a half it has not
                // read.
                if self.todo_seed_due() {
                    self.queued.push(Action::ListTodos);
                    self.todo_seed_pending = true;
                }
                Disposition::Control
            }
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
            ServerFrame::TermAttached { command } => {
                // **The daemon naming the pane this head attached to**, which is the half of the
                // attach the head cannot know: it sent `!term` with no command, and the head that
                // typed the original line may be another one or may have switched away. The
                // command is what the daemon was handed at `TermOpen` — with the verb stripped,
                // because that is how it received it — so the verb goes back on here, where the
                // line is a thing a person reads.
                //
                // **The bytes follow this frame**, so the pane is up and empty when this lands.
                // See `ClientFrame::TermOpen` for why the daemon sends the name first: a head
                // that drew the screen and learned what it was afterwards would flash a
                // rectangle it could not name.
                if let Some(p) = self.term.as_mut() {
                    p.line = format!("!term {command}");
                }
                // **And the fact the head draws when it is not drawing the pane.** An attach is
                // the daemon answering *what is running* in the same breath as handing over the
                // screen, so a head that detaches a moment later already knows what to say.
                self.term_fact = PaneFact::Running(command.clone());
                self.say(&format!(
                    "attached to `!term {command}` — the pane this session already has. \
                     ctrl-\\ leaves it running, `!term close` ends it."
                ));
                self.redraw = true;
                Disposition::Control
            }
            // **What this session's pane is running, or nothing** — the answer to a read, and
            // the fact a head draws **instead of a row** when it is not drawing the pane (see
            // [`App::pane_behind`] and [`PaneFact`]).
            ServerFrame::TermStatus { command } => {
                self.term_fact = match &command {
                    Some(command) => PaneFact::Running(command.clone()),
                    None => PaneFact::None,
                };
                self.redraw = true;
                // **A `!term close` that was waiting for this answer runs now**, through the same
                // decision it would have taken had the head known — see [`App::begin_close`]. The
                // line is held rather than guessed, which is the whole reason `Unasked` is a
                // state and not an `Option`.
                if self.close_pending {
                    self.close_pending = false;
                    if let Some(action) = self.begin_close() {
                        self.queued.push(action);
                    }
                }
                Disposition::Control
            }
            ServerFrame::TermOutput { bytes } => {
                // **A pane this head opened, fed its bytes.** Not an event and not counted as
                // one: `TermOutput` carries no seq, so it is `Control` for the same reason a
                // `Jobs` reply is — the ack's `rendered`/`filtered` are this head's disclosure
                // about *the batch*, and this frame is not in any batch.
                //
                // **A pane this head did NOT open is dropped, quietly.** The frames are fanned
                // out to every head of the session like events (one pane per session, see
                // `TerminalDriver`), and a second head attached to the same session has no
                // rectangle to draw them in. Dropping them is the honest reading of *a pane
                // this head did not open*, and the alternative — opening a pane from a frame
                // nobody asked for — would be a screen program appearing on a head that never
                // ran `!term`.
                //
                // **And no `redraw` flag**, which is the difference between a pane and a
                // transcript. `redraw` makes the driver call `Terminal::invalidate`, which
                // forgets the glass so the next frame is written whole — right for Ctrl-L, a
                // resize and a fold, and *wrong here*: a screen program repaints ten times a
                // second and the terminal's own diff writes exactly the rows that changed. A
                // flag per frame would pin the terminal rewriting all 24 rows ten times a
                // second, which is the flicker `term.rs`'s whole diff encoder exists to
                // remove. The frame is composed and drawn every tick either way — this flag
                // is about the *glass*, not about whether to draw.
                if let Some(p) = self.term.as_mut() {
                    p.screen.feed(&bytes);
                }
                Disposition::Control
            }
            ServerFrame::TermEnded { reason } => {
                // **The one place a pane closes, and the reason always comes from the daemon**
                // — *"the program exited with 3"*, *"you closed the terminal"*, *"a pane is
                // already open in this session"*. So a head never guesses why its rectangle
                // came back, and a refusal to start is the same frame as an ending.
                //
                // **And it becomes a row, not a notice.** This was `self.say(…)` — a sentence
                // on the chrome for `NOTICE_MS` — which is exactly why a program that dies at
                // once was *invisible*: the rectangle came back, one line appeared, and a few
                // seconds later the session said nothing about what had happened or what the
                // program had printed. The screen the program left is read here, while the
                // pane still holds it, and goes into the note with the daemon's sentence. See
                // [`Note::Pane`].
                //
                // **A pane this head had DETACHED from is the same arm, and that is the
                // point.** The pane is kept while the operator is away (see
                // [`TermPane::detached`]), the bytes keep arriving into its screen, and this
                // is where the row they would have seen had they been looking is filed — so
                // detaching does not hide a death. The only difference is that there is no
                // rectangle to give back, which `pane_open()` already accounts for.
                self.term_fact = PaneFact::None;
                if let Some(p) = self.term.take() {
                    self.file_note(Note::Pane {
                        line: p.line.clone(),
                        said: p.last_rows(),
                        closed: p.closing,
                        reason,
                    });
                    self.redraw = true;
                }
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
        }
    }

    /// Replace all state from a snapshot. This is the late-join path and the
    /// resync path; they are the same path, which is why resync is not special.
    pub(crate) fn load(&mut self, s: Snapshot) {
        // **The same conversation, or a different one**, read once and at the top: the
        // session id is assigned a few lines down, and three things here ask the
        // question — what is carried over, and (R19) which of this head's own notes keep
        // the seam they were filed at.
        let same_session = self.session_id == s.session_id;
        // Everything session-scoped goes, not just the transcript. A snapshot is a
        // *replacement*, and this is also the switch path: carrying the previous
        // session's model name or a tool target keyed by a call id that only
        // existed over there is how a switched head shows the right conversation
        // with the wrong facts attached to it.
        if !same_session {
            self.call_targets.clear();
            self.call_ms.clear();
            self.call_edits.clear();
            self.usage = None;
            self.usage_cache_measured = true;
            self.last_timings = None;
            // The total belongs to the conversation, not to the head: switching
            // sessions must not carry one session's bill onto another's header.
            self.spent_micros = 0;
            self.spent_seen = false;
            self.model.clear();
            self.turn = None;
            self.heads = 0;
            // **The pane goes with the session it was opened in.**
            //
            // A screen belongs to the conversation it was drawn over: carried across a switch it
            // would be another session's program drawn in this one's rectangle, which is the
            // same lie a carried-over model name is. **The daemon's pane is not closed here**,
            // and it cannot be — a `TermClose` sent now would arrive *after* the `Switch`, on
            // the new session's hub, and kill the wrong thing. So the program is left running
            // for the session it belongs to, and it ends when the daemon stops or when a head in
            // that session leaves it. **And a head that switches back finds it again**: a bare
            // `!term` attaches to the pane this session has, and the daemon — which held the
            // screen all along — replays it. That is the half this rectangle's own TODO used to
            // say was missing (*"a head that switches back does not find its pane again, it
            // finds the transcript"*).
            self.term = None;
            // **And what this head believed about the pane is the session's, not this head's.**
            // A `Hello` re-asks (see that arm), and until the answer lands the fact is
            // `Unasked` — which is the state that makes `!term close` hold its line rather than
            // guess. Carrying the previous session's answer across a switch would be a
            // confirmation naming another session's program.
            self.term_fact = PaneFact::Unasked;
            self.term_ask = None;
            self.close_pending = false;
            // The subagent tree is the PARENT's fact. Carried across a switch it
            // put "1 subagent running" on the composer of the very subagent being
            // looked at (measured 2026-09-16), and Enter in the pane there would
            // have switched to itself.
            //
            // **So the rows are REPLACED, by this session's own children out of the
            // snapshot — not cleared, and not carried.** The snapshot's rows are the
            // parent's fact and nobody else's: the view they are cut from is per-session,
            // so a head that switches into `s-sub-1` is handed `s-sub-1`'s children and
            // not `s`'s, and the rule above is kept by construction rather than by a
            // clear. See `Snapshot::subagents` for the measurement that put them there.
            //
            // **This is the fix for a count that flapped.** The clear this replaces was
            // what turned a live child into a finished one on the way back: the rows were
            // gone, and the only thing left to rebuild them from was the daemon's session
            // list — whose `running` is *a turn is generating in that session at this
            // instant*, and `false` for a child parked on its own background job. So the
            // operator's `N subagents running` segment went away on a switch and came back
            // when some later list reply happened to catch the child generating, over a
            // subagent that ran throughout (*"so the counter is gone"* … *"yep and now it
            // is back. wtf"*). A switch sends `since_seq = 0`, so nothing is replayed and
            // the snapshot is the only route by which the head can be told what it watched
            // — see the `Subagent` arm in `letibot_sessionlog::view`.
            //
            // **What the clear was protecting is still protected.** A row can no longer
            // arrive from a session the head is not in: this is the snapshot's list, keyed
            // by the session the snapshot is of. And the fold below still refuses to emit a
            // row for a child whose brief belongs to somebody else.
            self.subagents = s
                .subagents
                .iter()
                .map(|v| SubagentState {
                    session_id: v.session_id.clone(),
                    state: v.state.clone(),
                    // **The event's own word for the instant**, read exactly as the live arm
                    // reads it: `running` is the one state it names in which a turn is
                    // generating. See [`SubagentState::generating`].
                    generating: v.state == "running",
                    prompt: v.prompt.clone(),
                    role: v.role.clone(),
                    task: v.task.clone(),
                    model: v.model.clone(),
                    answer: v.answer.clone(),
                    // The daemon's own stamp for when the event was published. The list's
                    // `created_ms` overrides it in the fold, the way it always did.
                    spawned_ms: v.ts,
                })
                .collect();
            self.subagents_sel = 0;
            // Jobs are the session's, the same way. The rows survived a switch
            // and kept drawing the old session's ids with the old session's byte
            // counts — and now that Enter on a row asks THIS session for that id,
            // a carried row is a question about a job that was never here.
            self.jobs.clear();
            self.jobs_sel = 0;
            // The queue is the old session's. Whatever was queued there stays
            // queued *there* — the hub drains it into that session's transcript —
            // but this head is no longer looking at that session, and an echo of
            // words belonging to a conversation that is no longer on the screen is
            // the same lie a carried-over model name is.
            self.pending_prompts.clear();
            self.unconfirmed.clear();
            // **And the viewport's place, which is a place in the rows that just went**
            // (R36). Carrying it across a switch would hold the reader on a row of another
            // conversation's transcript — the same lie a carried-over model name is.
            self.anchor = None;
        }
        self.session_id = s.session_id;
        self.seq = s.seq;
        self.dropped = self.dropped.max(s.dropped);
        // **And the subagent rows are rebuilt from the snapshot's own children, then
        // overlaid with the daemon's list.**
        //
        // Here, and not in the `Subagent` event arm, because this IS the late-join path
        // and the resync path at once (see the docstring above) — and a seed that ran
        // somewhere else would be a second way for the pane to be filled, which is how
        // the two come to disagree. Called for a same-session resync as well as a switch:
        // the fold is a rebuild from the current facts and is the same answer either way,
        // and a resync is exactly when a head's own list may be the stale one.
        //
        // The rows themselves were seeded by the `!same_session` block above, from
        // `Snapshot::subagents` — the parent's own view of its children, which is what a
        // switch can carry and a session list cannot. This call then folds the list over
        // them: the list's `running` is the one measurement of NOW either half has.
        //
        // `self.sessions` is already the fresh list by now — the `Hello` arm assigns it
        // before calling this — so the fold is reading the daemon's word and not the
        // previous session's.
        self.fold_subagents();
        // A resync of the *same* session keeps the queue — the hub's command
        // queue survives a resync, and a prompt queued behind a running turn is
        // still behind that turn — but anything the snapshot's transcript already
        // holds has landed, and its echo stands down the way `record_item` would
        // have stood it down had the row arrived live.
        for it in &s.items {
            if let Some(TranscriptItem::User { parts, .. }) = &it.item {
                for text in parts.iter().filter_map(|p| match p {
                    UserPart::Text { text } => Some(text.as_str()),
                    _ => None,
                }) {
                    self.retire_pending(text);
                }
            }
        }
        // **And what the snapshot could not resolve stops claiming `queued`** — R16's
        // third mark, and the claim the head can actually support.
        //
        // `pending_prompts` says *the daemon owes me a row for this*. A snapshot
        // **replaces** the transcript, so after one, an echo the snapshot does not carry
        // is either a row still coming or a row that a fork replaced — and from the head
        // those two are the same picture. Leaving it at `queued` asserts the first when
        // it might be the second; dropping it silently loses the operator's words. So it
        // is `unconfirmed`, and it retires normally when a row does land.
        //
        // **Where this is marked, and why here rather than at the fork.** A snapshot is
        // the only route by which the transcript is replaced — `reconnect`, `/resync`,
        // `Switch`, an import — so marking at the snapshot catches every one of them
        // instead of the two that happened to be thought of. `App::resolve_fork` handles
        // the fork the head *asked for*, where it knows the row will never arrive.
        //
        // An echo queued AFTER this point is untouched: it is added to
        // `pending_prompts` by a later `submit`, so it is not in the set being marked.
        for q in &self.pending_prompts {
            if !self.unconfirmed.iter().any(|u| u == q) {
                self.unconfirmed.push(q.clone());
            }
        }
        // The snapshot's in-flight calls are **not** seeded into `call_targets`.
        // They reach the screen as `TurnPane::calls`, which carries each call's own
        // target on the row that is about to draw it; putting them in an id-keyed
        // table as well is how a live `call_0` came to relabel a settled one.
        if let Some(TurnState::Finished { usage, timings, .. }) = s.turn.as_ref().map(|t| &t.state)
        {
            // A turn that finished measured its own cache, so the percentage is
            // real even though the row's copy may not have been.
            self.usage = Some(*usage);
            self.usage_cache_measured = true;
            self.last_timings = Some(*timings);
        }
        self.items = s.items;
        // A snapshot replaces the rows, so everything derived from them — the `!`
        // candidates and the model's suggestions — is stale and goes with them.
        self.the_rows_moved();
        // **And the fill's bar goes with the stream that carried it.**
        //
        // A `Filling` tick rides the event stream and its ONLY exit is a tick whose `done` has
        // reached `total` — so a stream that stops carrying ticks leaves the bar standing for
        // the rest of the session. That is not hypothetical: a republish of 2000 rows overruns
        // a head's 1024-event queue, the hub **demotes** the head rather than blocking
        // (`Inner::append_and_fan`), and from that moment every tick is skipped — the final one
        // included, because a demoted head is not written to at all. The head is handed a
        // snapshot instead, and this line is the head taking it: **the view it was watching is
        // gone, so the progress it was reporting belongs to a stream that no longer exists.**
        // Measured on the operator's own head, 2026-10-04: it stood at `897 of 2000 rows —
        // restoring the stored conversation` and did not move.
        //
        // **A fill that is genuinely still running is not lost by this.** Its next tick re-arms
        // the line, and ticks come one per 64 rows — so clearing here can cost one tick of a
        // bar that is still going, and buys the end of one that never will. The bar must end by
        // FACT rather than by a clock (that is why `republish_after` publishes its completion
        // unconditionally), and the fact here is that the head was just told its queue was
        // thrown away.
        self.filling = None;
        // **A snapshot records the bulk announcement; a live append never does.**
        //
        // The rows a snapshot carries without bodies are a *carry* — a fork, a reseat, a
        // resume, an import, or an attach to a daemon mid-carry. The rows a live
        // `TranscriptAppended` adds are the R2 window of an ordinary message, and putting
        // them here is exactly the defect this replaces: a trigger built on "some row
        // lacks a body" fires on every healthy turn.
        self.bulk = {
            let ids: std::collections::HashSet<String> = self
                .items
                .iter()
                .filter(|i| i.item.is_none())
                .map(|i| i.item_id.clone())
                .collect();
            (!ids.is_empty()).then(|| Bulk {
                ids,
                at_ms: self.now_ms,
            })
        };
        // **A snapshot replaced every row, so the bindings are pruned to what is
        // still there and still body-less.** Pruned rather than cleared: a resync
        // mid-prompt is exactly when the reply is racing the prompt, and dropping
        // the binding for one frame would put the echo back at the tail and take it
        // away again on the next `TranscriptContent`. An id that is gone, or whose
        // row now has its body, has nothing left for a binding to stand for — the
        // row renders from its content, and the echo at the tail is the echo's own
        // business again.
        {
            let bodyless: std::collections::HashSet<&str> = self
                .items
                .iter()
                .filter(|it| it.item.is_none())
                .map(|it| it.item_id.as_str())
                .collect();
            self.bound_prompts
                .retain(|id, _| bodyless.contains(id.as_str()));
        }
        // The snapshot's turn carries its calls **with their edit excerpts**, and
        // the rows it appended in order — the same two facts the live hand-off
        // used when it moved a card's excerpt into `call_edits` as the row landed.
        // Only the live path filled that map, so a restarted head drew every
        // landed edit panel-less even though the wire had just handed it the
        // excerpt (operator, 2026-09-17: past edits lose their diff panels on
        // restart). Seed it the same way the live arm does: positionally, the
        // Nth tool_result row is the Nth call. Rows from turns before this one
        // are not on the wire — the view keeps one turn's calls — and render as
        // they always did.
        if let Some(t) = &s.turn {
            let kinds: std::collections::HashMap<&str, &str> = self
                .items
                .iter()
                .map(|it| (it.item_id.as_str(), it.kind.as_str()))
                .collect();
            let mut call_idx = 0usize;
            for item_id in &t.appended {
                if kinds.get(item_id.as_str()).copied() == Some("tool_result") {
                    if let Some(CallState::Finished { edit: Some(e), .. }) =
                        t.calls.get(call_idx).map(|c| &c.state)
                    {
                        self.call_edits.insert(item_id.clone(), e.clone());
                    }
                    call_idx += 1;
                }
            }
        }
        self.invalidate_history();
        self.open = s.open_decisions;
        // A snapshot can replace the open set wholesale; keep the highlight in range.
        self.sel = 0;
        // A permission settles on the call it gated, so it rides the call's card
        // rather than the note list; a question — or a log recorded before the field
        // existed — has no call to ride and stays a note. The live arm makes the same
        // split, and the two have to agree.
        let (call_bound, notes_bound): (Vec<SettledDecision>, Vec<SettledDecision>) = s
            .settled_decisions
            .into_iter()
            .partition(|d| d.call_id.is_some());
        // **A snapshot's notes are HISTORY, and this head's own are not** (R19).
        //
        // Until this, everything the snapshot carried was planted at anchor 0 —
        // *everything in a snapshot is history and none of it is anchored* — which is
        // right about where it goes and wrong about what it is: a fresh head showed
        // nothing, so replaying hours of announcements as though they had just happened
        // put them above a conversation they did not precede. The operator restarted a
        // head and was met by twelve red lines: *"i dont want to see that on restart."*
        //
        // So the two kinds are sorted rather than merged. This head's own notes keep
        // their seam — it filed them while watching, at rows of this very conversation —
        // and what the snapshot adds is [`Placed::Before`]: listed by `/notes`, counted
        // by `/status`, and not drawn, because a head that has just attached has shown
        // nothing and the log is where these facts live.
        //
        // **A seam the new transcript no longer has is not a seam.** A compaction or a
        // reseat forks the conversation, so a note filed at row 200 of a 250-row
        // transcript is no longer between any two rows of this one; it joins the history
        // rather than being drawn at a place that has stopped existing.
        let mut mine: Vec<(Placed, Note)> = if same_session {
            std::mem::take(&mut self.notes)
                .into_iter()
                .map(|(place, note)| match place {
                    Placed::Seam(at) if at <= self.items.len() => (Placed::Seam(at), note),
                    _ => (Placed::Before, note),
                })
                .collect()
        } else {
            // A different conversation: these are that session's notes about rows this
            // head no longer holds, and the same rule that clears `call_targets` clears
            // them.
            Vec::new()
        };
        let mut before: Vec<(Placed, Note)> = Vec::new();
        for w in s.warnings {
            // Same rule as the live arm: `turn_failed` is the log's record of what
            // the turn's own terminal state already says on the screen. Filtering
            // it here as well is what stops a *snapshot* from putting it back —
            // which is exactly what happened the first time, and is the reason the
            // live path and the snapshot path have to agree about every filter.
            if w.code == "turn_failed" {
                continue;
            }
            let n = Note::Warned(w);
            if holds(&mine, &n) {
                continue;
            }
            before.push((Placed::Before, n));
        }
        for d in notes_bound {
            let n = Note::Decided(d);
            if holds(&mine, &n) {
                continue;
            }
            before.push((Placed::Before, n));
        }
        // Oldest first: the facts from before this window are older than anything this
        // head filed, and `/notes` numbers them in the order a reader reads.
        before.append(&mut mine);
        self.notes = before;
        self.note_upto = 0;
        // The settled rows this snapshot carries are history, and a decision that
        // gated one of them has to ride that row rather than vanish with the live
        // card. The snapshot's decisions are keyed by call id, which is
        // round-positional, so the match is best-effort: walking the rows newest
        // first, each decision goes to the most recent tool_result row that carries
        // its id, and a decision is spent on the first row it matches. Rows older
        // than the snapshot's decision window render without the approval — the same
        // `Replayed` rule the duration and the edit pair follow.
        {
            let mut by_call: std::collections::HashMap<String, SettledDecision> =
                std::collections::HashMap::new();
            for d in &call_bound {
                if let Some(cid) = &d.call_id {
                    by_call.insert(cid.clone(), d.clone());
                }
            }
            for it in self.items.iter().rev() {
                if let Some(TranscriptItem::ToolResult { call_id, .. }) = &it.item {
                    if let Some(d) = by_call.remove(call_id) {
                        self.call_decisions.insert(it.item_id.clone(), d);
                    }
                }
            }
        }
        self.heads = s.heads.len();
        self.turn = s.turn.map(|t| {
            self.model = t.model.clone();
            let mut pane = TurnPane {
                turn_id: t.turn_id,
                model: t.model,
                // No timestamps in a snapshot, so every one of these renders
                // without a duration rather than with a fabricated one.
                calls: t
                    .calls
                    .into_iter()
                    .map(|c| CallRow {
                        call_id: c.call_id,
                        name: c.name,
                        target: c.target,
                        state: c.state,
                        started_ms: 0,
                        // The same rule one field along: a snapshot carries no
                        // timestamps, so there is no anchor to measure against
                        // either.
                        started_at: 0,
                        ended_ms: 0,
                        note: None,
                        decision: None,
                    })
                    .collect(),
                progress: t.progress,
                state: Some(t.state),
                // Which rows this turn produced. Without it a head that joined late
                // cannot tell that the transcript already holds the answer, and
                // renders it twice — measured on a second head attached to a
                // finished turn, where the whole reply appeared above itself.
                appended: t.appended,
                // **`turn_rows` is deliberately NOT here, because it is not on the wire.** A head
                // that attaches mid-turn gets the current round's `appended` from the snapshot and
                // starts with no history of the turn's earlier rounds, so for the window between
                // attaching and the next `TurnStarted` it can draw the duplicate this field exists
                // to prevent. Said rather than hidden: closing it needs a daemon field, and the
                // window is one attach rather than every round.
                ..TurnPane::default()
            };
            // The snapshot carries the accumulated text **once**. Everything after
            // this is an increment. That is §13.3's wire half, arriving.
            pane.text.push(&t.text);
            pane.reasoning.push(&t.reasoning);
            // A head joining mid-call gets the markup too, so the raw chord shows
            // the same thing on a reattach as it does on the head that watched it.
            // `writing_call` stays false: a snapshot cannot say whether the block
            // is still open, and inventing a spinner that never stops is worse
            // than not showing one.
            pane.raw_call = t.raw_calls;
            // A permission settles on the call it gated, so the snapshot attaches
            // it to the call's card the way the live arm does. A decision whose call
            // is not in this turn — a log recorded before the field existed, or a
            // call the snapshot did not carry — has nowhere to ride and is dropped
            // here rather than rendered twice.
            for d in call_bound {
                let cid = d.call_id.clone().unwrap_or_default();
                if let Some(c) = pane.calls.iter_mut().find(|c| c.call_id == cid) {
                    c.decision = Some(d);
                }
            }
            pane
        });
        self.scroll = 0;
        self.redraw = true;
    }

    pub(crate) fn event(&mut self, e: SessionEvent, ts: u64) -> Disposition {
        if let Some(t) = self.turn.as_mut() {
            t.last_ms = ts.max(t.last_ms);
        }
        match e {
            // The name of the session this head is *in*. Folded into the row this
            // head already holds rather than triggering a `ListSessions` round trip:
            // the event carries the whole of the change, and asking the daemon to
            // resend a list to learn something it just told us is how a head ends up
            // one frame behind its own screen.
            SessionEvent::SessionRenamed { title } => {
                let id = self.session_id.clone();
                if let Some(row) = self.sessions.iter_mut().find(|s| s.session_id == id) {
                    row.title = title.clone();
                }
                self.redraw = true;
                // Said out loud, because the header changes under the operator and an
                // unexplained change of the one label that identifies where you are
                // is worse than no label.
                self.say(&format!("this session is now called {title:?}"));
                Disposition::Control
            }
            // The model revised its plan. The whole list, not a delta — keep the
            // latest and let the pane show it. Said only when the pane is open:
            // a line in the scrollback for every todo write would bury the work
            // the todos exist to organize, and the pane is where this state
            // lives.
            SessionEvent::TodosUpdated { todos } => {
                self.todos = todos;
                // **AND A WAITING SEED RUNS HERE** — the board has just been read whole, which is
                // the only moment this head may add rows to its own half without risking a wipe:
                // `SetOperatorTodos` REPLACES the operator half, so seeding against a stale list
                // would take rows off the board rather than add to it.
                if self.todo_seed_pending {
                    self.todo_seed_pending = false;
                    self.seed_todos();
                }
                self.redraw = true;
                if self.todos_pane {
                    Disposition::Rendered
                } else {
                    Disposition::Filtered
                }
            }
            // A subagent spawn/finish. Fold into the tree, replacing the row with the
            // same session id, so `running` becomes `done` rather than a second line.
            SessionEvent::Subagent {
                subagent_id,
                state,
                prompt,
                role,
                task,
                model,
                answer,
            } => {
                // **The event's own word for the instant**: it publishes `running` when the
                // child's harness comes up and `opening` before that, so `running` is the one
                // state it names in which a turn is generating. See
                // [`SubagentState::generating`].
                let generating = state == "running";
                if let Some(row) = self
                    .subagents
                    .iter_mut()
                    .find(|s| s.session_id == subagent_id)
                {
                    row.state = state;
                    row.generating = generating;
                    row.prompt = prompt;
                    row.role = role;
                    row.task = task;
                    row.model = model;
                    row.answer = answer;
                } else {
                    self.subagents.push(SubagentState {
                        session_id: subagent_id,
                        state,
                        generating,
                        prompt,
                        role,
                        task,
                        model,
                        answer,
                        // When this head heard of it, which is the best a spawn event can say.
                        // `fold_subagents` replaces it with the daemon's own `created_ms` the
                        // moment a list carrying the child arrives.
                        spawned_ms: ts,
                    });
                }
                self.redraw = true;
                Disposition::Filtered
            }
            // A background job settled — the daemon publishes this between turns,
            // which is exactly when nothing else could say it. Fold into the row
            // the backgrounded finish pushed; a settlement whose start is beyond
            // this head's window still gets its row, with the command unknown,
            // because a job that ran is a fact even when its beginning scrolled
            // off.
            // **The daemon has admitted a call the operator is running themselves** (R24 part
            // two, decision 4). Nothing is filed as a note: the admission is the daemon's
            // record, and the call's visible half is the result row, which arrives with its
            // `origin` set. A head that narrated the admission would be reading a keystroke the
            // operator just made back to them.
            //
            // **Counted, not ignored.** A head that runs operator calls wants to know one was
            // admitted; a head that does not — this one, until the chord lands — must not
            // swallow it silently, or *"the event never came"* and *"this head drops it"* look
            // the same, and the second is the one a reader would never find.
            //
            // **`Filtered` and NOT `Control`, and the difference is the whole of R53 §1.2.** The
            // paragraph above asks for "counted, not ignored" and the code answered `Control`,
            // whose own definition is *"Not an event: a Hello, a Resync, a command reply"* — so the
            // arm contradicted the variant's documentation and its own comment at the same time.
            // `Filtered` is the counted one and says so. MEASURED consequence before the fix: the
            // `filtered` figure on `Ack` (and on `/status`) differed by one per occurrence between
            // the two heads, and letibot's stated protection against *"the event never came"*
            // reading exactly like *"this head drops it"* was not in force on the one event whose
            // comment argues for it. **No test caught it because nothing asserts the count for
            // this event, and the comment reads as the specification** — a reader checking the
            // file found an argument for the correct behaviour sitting on top of the incorrect
            // one, which is the same failure mode as a docstring describing colours over a
            // function that returns a plain string.
            SessionEvent::OperatorCallAllowed { .. } => Disposition::Filtered,
            SessionEvent::JobSettled {
                job,
                state,
                produced,
                elapsed_ms,
            } => {
                // **Folded, never invented.** The daemon owns the table; a
                // settlement for a job this head has not been told about is not a
                // row to make up, it is a row that arrives with the next
                // `ListJobs`. Inventing one is how the pane used to show a job
                // with no command and no idea how it got there.
                if let Some(row) = self.jobs.iter_mut().find(|j| j.id == job) {
                    row.state = state;
                    row.running = false;
                    row.produced = produced;
                    row.elapsed_ms = elapsed_ms;
                    // `never_ran` is deliberately **not** taken from this event: the
                    // settlement carries no such fact, and it cannot be stale here — a
                    // job that never ran never started, so it was never listed as a
                    // running one, and the `never_ran` the row already holds came from
                    // the daemon's own listing (`JobEntry`). A job that ran is never
                    // settled as one that did not.
                }
                // **And the count drops by one**, so the row above the composer re-asks (R51 item
                // 5). The fold just above is the pane's copy: a job settled in a turn this head
                // never watched has no row to fold into, which is exactly the case a count taken
                // from the pane's rows would get wrong.
                self.queued.push(Action::ListJobs);
                self.redraw = true;
                Disposition::Filtered
            }
            // **A job's output, the answer to the jobs pane's Enter.** Not folded
            // into any view: the pane that asked draws the window, and only that
            // pane has anywhere to put it. It is ephemeral besides
            // (`scrub::is_interactive`), so no late head replays one.
            SessionEvent::JobOutput {
                job,
                from,
                to,
                produced,
                dropped,
                state,
                never_ran,
                lines,
                next,
            } => {
                // Taken only when a window is open for *this* job: a head here may
                // have closed the pane with Esc before the reply landed, and a
                // window for a job nobody is looking at is nothing to keep.
                if let Some(v) = self.job_out.as_mut()
                    && v.job == job
                {
                    v.state = state;
                    v.never_ran = never_ran;
                    v.from = from;
                    v.to = to;
                    v.produced = produced;
                    v.dropped = dropped;
                    v.lines = lines;
                    v.next = next;
                    v.loading = false;
                    v.error = None;
                    // A window lands at its **tail**: a fresh page, or a re-read of
                    // a running job, should show what it just wrote. `back` is left
                    // alone, so ← still walks the pages the reader came through.
                    v.scroll = 0;
                    self.redraw = true;
                    return Disposition::Rendered;
                }
                Disposition::Filtered
            }
            // **A fill the daemon NAMED (R6).** `what` is the operation in the daemon's
            // own words and `done`/`total` are its own count in the `unit` it named — so
            // the head draws the fact instead of inferring a cause from a symptom.
            // Counting the rows still lacking a body would draw *"N rows announced and
            // never filled in"* three seconds into a healthy generation (see
            // `BODY_PATIENCE`), and — worse — would name the wrong operation: an ordinary
            // reply is not a carry.
            //
            // Ephemeral (`scrub::is_interactive`), so a late head never replays a tick.
            // On the last one the line goes: the daemon's durable finish note is what
            // says the operation ended, and a bar left at `total of total` would sit on
            // the screen for ever.
            SessionEvent::Filling {
                what,
                unit,
                done,
                total,
            } => {
                self.filling = (done < total).then_some((what, unit, done, total));
                self.redraw = true;
                Disposition::Rendered
            }
            SessionEvent::CompactionProgress {
                half,
                halves,
                prompt_tokens,
                processed,
                written,
                unit,
            } => {
                // **Stored in its own field, which is the whole point of the event.** This
                // must not touch `self.turn.progress`: that slot is the SESSION's turn, and
                // a scratch summary's numbers landing there is exactly the mislabel the
                // old suppression traded away the progress line to avoid. Nothing else
                // writes `self.compacting`, so the two cannot be confused.
                self.compacting = Some(CompactionLine {
                    half,
                    halves,
                    prompt_tokens,
                    processed,
                    written,
                    unit,
                });
                self.redraw = true;
                Disposition::Rendered
            }
            SessionEvent::TurnStarted {
                turn_id,
                model,
                ledger_head: _,
                // **The WHOLE turn's start, when the emitter knows it.** This event fires once
                // per ROUND, so `started_ms: ts` below restarted the header's clock at every
                // round — `2.1s` a minute into a turn. `began_ms` is the prompt's own stamp and
                // is used when present; `ts` stays as the fallback for a turn nobody measured
                // (a snapshot, or a test that emits the event by hand).
                began_ms,
            } => {
                // Asked *before* the pane is replaced. The rows that stop being
                // drawn live are the previous turn's, and once its pane is gone
                // there is nothing left to ask which they were.
                let stale_from = self.turn_first_row();
                let previous = self.turn.as_ref();
                // **A boundary the head has ALREADY HAD, taken again.** The frame is an event like
                // any other and may be delivered twice — and rebuilding the pane here is not
                // idempotent: the round's own stream (its text, its reasoning, its proposed calls)
                // is state the event does not describe and cannot restore, so a second copy of one
                // boundary would throw the round's work away and re-open a run that is already
                // open.
                //
                // `turn_id` is the key that can say so, and it is a sound one: `run_turn_steered`
                // mints it as `{transcript_id}#{turn_seq}` with `turn_seq += 1` per round, and a
                // restored session starts that counter at the number of restored items precisely
                // so that *"within one transcript the ids never repeat"* (`crates/turn/src/
                // resume.rs`, "`turn_seq` is a watermark, not a turn count"). So an equal
                // non-empty `turn_id` is one round, and this event has already been folded in.
                if !turn_id.is_empty() && previous.is_some_and(|t| t.turn_id == turn_id) {
                    return Disposition::Filtered;
                }
                self.model = model.clone();
                // Stamped, so the header can prefer this over a settings row it read
                // earlier at attach. See `model_from_turn_at`.
                self.model_from_turn_at = self.seq;
                // **Which boundary is this?** — and only one of the answers is *a new run*.
                //
                // [`TurnPane::turn_rows`] hangs the run on `began_ms`, because it is stamped once
                // per prompt and carried by every round of it: the one thing on the wire that
                // distinguishes *the next round of this prompt* from *a new prompt*. What the head
                // then does with it is the defect this closes, and the mistake is asking the wrong
                // question — *does this number match* rather than *may I start a new run here*.
                //
                // A boundary the head cannot MATCH is not a boundary it can tell APART. `began_ms:
                // None` means *nobody measured this one*, and two very different events carry it: a
                // turn that began before this head attached, and **a turn the daemon ran without
                // stamping its clock at all.** The second is real and reachable —
                // `harnessd::sessions::run_prompt` calls `begin_turn_clock` and
                // `Sessions::wake` does not, so the turn a job's settlement or a monitor's firing
                // opens arrives with no start in it (`harnessd/src/sessions.rs`: `run_prompt` vs
                // `wake`).
                //
                // Read as a new run, such a boundary emptied `turn_rows` and moved `started_ms` to
                // the event's own `ts`: the run the work is still in stopped reading as this turn's,
                // so `live_here` and `walk_carried_live` both answered no, the pane drew its OWN
                // marker beside the walk's, and the `Responding` clock restarted. The operator's
                // report, in their words: *"it looks like turn end or some other border is
                // misinterpreted and Responding timer resets and I get new line with `[N thinking
                // lines]` which then gets merged to the previous `[N tools, M thinking]`"* — and the
                // merge is the next row landing, which puts a row of the turn back into
                // `turn_rows`.
                //
                // **So an unmeasured boundary over a turn that is still RUNNING is the same run.**
                // The head is drawing that turn right now; nothing the reader can see has separated
                // the work before the event from the work after it; and the row filter, not this
                // event, is what actually ends a run on the screen ([`row_drawn`]) — a new prompt
                // is preceded by the operator's own message, which is a drawn row and ends the run
                // by itself. A head that says *new run* here is inventing a border out of an event
                // that carries no evidence for one, which is exactly the second `Responding` the
                // operator watched appear.
                let same_turn = match began_ms {
                    Some(b) => previous.is_some_and(|t| t.started_ms == b),
                    None => previous.is_some_and(|t| matches!(t.state, Some(TurnState::Running))),
                };
                // **The clock is never restarted by an event that carries no clock.** Same rule, on
                // the one field the operator watches move: a boundary with no `began_ms` leaves the
                // base where it was, so the row keeps counting from the prompt that is actually
                // running rather than from the frame that arrived. `ts` stays the base for a turn
                // nobody measured at all — the first `TurnStarted` of a head that joined late.
                let started = match (began_ms, same_turn) {
                    (Some(b), _) => b,
                    (None, true) => previous.map(|t| t.started_ms).unwrap_or(ts),
                    (None, false) => ts,
                };
                let turn_rows = if same_turn {
                    previous.map(|t| t.turn_rows.clone()).unwrap_or_default()
                } else {
                    Vec::new()
                };
                self.turn = Some(TurnPane {
                    turn_id,
                    model,
                    state: Some(TurnState::Running),
                    started_ms: started,
                    last_ms: started,
                    turn_rows,
                    ..TurnPane::default()
                });
                // A new turn takes the pane away from the previous one, so the
                // previous one's rows now own everything they proposed. Same
                // reason as the terminal states above: the history is cached —
                // from the first row that pane owned, which is the only part of
                // it that can render differently now.
                if let Some(k) = stale_from {
                    self.invalidate_history_from(k);
                }
                Disposition::Rendered
            }
            SessionEvent::PromptProgress { progress, .. } => {
                if let Some(t) = self.turn.as_mut() {
                    t.progress = Some(progress);
                }
                // Shown on the status line, not in the transcript — which is what a
                // progress frame is for. It counts as rendered because it does
                // change the screen.
                Disposition::Rendered
            }
            SessionEvent::TokensGenerated { turn_id, tokens } => {
                let Some(t) = self.turn.as_mut() else {
                    return Disposition::Filtered;
                };
                if t.turn_id != turn_id {
                    return Disposition::Filtered;
                }
                // **The counter it used to move is no longer drawn** — see `turn_status`: the
                // operator does not want the number to read. The event still counts as
                // `Rendered`, because it is a frame in which the turn is plainly alive: the
                // spinner beside the clock moves on it, and that is the row's whole business
                // now. It also counts as liveness for the stuck line, the same way a delta does.
                let _ = tokens;
                Disposition::Rendered
            }
            SessionEvent::Delta {
                target,
                text,
                turn_id,
            } => {
                let Some(t) = self.turn.as_mut() else {
                    return Disposition::Filtered;
                };
                if t.turn_id != turn_id {
                    if std::env::var("LETIBOT_MARKER_DEBUG").is_ok() {
                        eprintln!(
                            "MARKER delta REFUSED: pane={:?} delta={:?} target={:?} bytes={}",
                            t.turn_id,
                            turn_id,
                            target,
                            text.len()
                        );
                    }
                    return Disposition::Filtered;
                }
                match target {
                    DeltaTarget::Text => {
                        // **§3.1: model prose is content this head did not author**, and
                        // this is where it enters — every streamed chunk that becomes the
                        // answer goes through here and ends up as a row `paint_full`
                        // writes verbatim. A control byte the model emits (or copies out
                        // of a file it read) is an instruction to the operator's
                        // terminal: `ESC ] 0 ; … BEL` sets the window title, `ESC [ 2 J`
                        // clears the screen.
                        //
                        // Sanitised here rather than at the renderer because
                        // `IncrementalMarkdown` is a frozen-prefix lexer (§13.3): a
                        // control byte that reaches it is frozen into the stable half
                        // and cannot be removed later without re-lexing. The trade is
                        // `without_control`'s own — one space per control byte, so the
                        // character count `arrived_chars` keeps is unchanged.
                        let text = without_control_lines(&text);
                        t.text.push(&text);
                        Disposition::Rendered
                    }
                    DeltaTarget::Reasoning => {
                        // **Accumulated at every rung, drawn only where the rung shows it.**
                        //
                        // The guard used to be on the accumulation, which is the same mistake
                        // R37's marker made one layer up: `conversation` *hides* the working and
                        // does not *discard* it — the rung is a view — so the text has to be
                        // here to be counted, to be drawn when the reader opens the run, and to
                        // be there the moment they change rung. Found by the marker's own test:
                        // at `conversation` nothing accumulated, so the streamed thinking the
                        // operator was watching could not be counted.
                        if t.think_started_ms == 0 {
                            t.think_started_ms = ts;
                        }
                        t.think_last_ms = ts;
                        // The same rule as the answer above, for the same reason: reasoning is
                        // the model's own text and it reaches the glass through the markdown
                        // renderer. **And it counts**, for the reason the field gives: on a
                        // `messages` backend this is the channel that streams from the first
                        // second, and it was the one the row's number could not see.
                        let text = without_control_lines(&text);
                        t.reasoning.push(&text);
                        // **The `thinking` switch, not a rung comparison** — it is the same
                        // question (`>= Normal` meant *the reasoning rows are drawn*, and at the
                        // bottom rung they are not) asked of the set, so a set that hides the
                        // reasoning cannot count it as rendered.
                        if self.visibility.shows(Show::Thinking) {
                            Disposition::Rendered
                        } else {
                            Disposition::Filtered
                        }
                    }
                    // Never `t.text`. This is the markup, and the whole point of
                    // the channel is that the default view does not show it — see
                    // `letibot_sessionlog::event::DeltaTarget`. It *is* reachable
                    // through `ctrl-x`, so it is sanitised where it is drawn
                    // (`raw_call_lines`) rather than here.
                    DeltaTarget::ToolCall => {
                        // **The other channel that arrives and that the row could not see**, and
                        // the one an agentic round is *made of*: a call's arguments come as deltas,
                        // and on the backend that produced the measurement above the row sat at a
                        // frozen number while thousands of tokens of exactly this went by.
                        t.raw_call.push_str(&text);
                        t.writing_call = true;
                        Disposition::Rendered
                    }
                }
            }
            SessionEvent::ToolCallProposed {
                call_id,
                name,
                target,
                ..
            } => {
                // Not kept beyond the turn, and not put in `call_targets`. It used
                // to be, on the argument that a settled `Assistant { tool_calls }`
                // row needs the word and the proposal is where the head saw it —
                // but the row carries the arguments the word is derived from, and
                // the id it would be filed under is reused by the next round. The
                // proposal's target lives on the `CallRow` below, which is the row
                // that draws it, and dies with the turn that made it.
                if let Some(t) = self.turn.as_mut() {
                    // The proposal is the settled form of whatever was being
                    // written, so the pending affordance stands down here.
                    t.writing_call = false;
                    t.calls.push(CallRow {
                        call_id,
                        name,
                        target,
                        state: CallState::Proposed,
                        started_ms: ts,
                        started_at: self.now_ms,
                        ended_ms: 0,
                        note: None,
                        decision: None,
                    });
                }
                Disposition::Rendered
            }
            SessionEvent::ToolStarted { call_id, name, .. } => {
                if let Some(t) = self.turn.as_mut() {
                    match open_call(&mut t.calls, &call_id) {
                        Some(c) => {
                            c.state = CallState::Running;
                            // The clock starts when the tool starts, not when the
                            // model asked for it: a call that waited on a decision
                            // did not spend that time running.
                            c.started_ms = ts;
                            // **And the head's anchor moves with it** (R13): the two
                            // clocks are the same clock, so a row anchored at the
                            // *proposal* would count the decision wait as running
                            // time — the exact sentence the line above exists to
                            // avoid, arrived at from the other side.
                            c.started_at = self.now_ms;
                            // **And the note from before it started is over.**
                            //
                            // `ToolProgress` notes are facts about right now, and
                            // the ones a call collects while it is `Proposed` are
                            // about the DECISION — "asking the guard". Only
                            // `ToolFinished` cleared the field, so that sentence
                            // rode the card through the whole run.
                            //
                            // Measured 2026-09-20 in the rano session: `cargo test
                            // stream_tests` ran for its full 300 s deadline with
                            // "asking the guard" underneath it the entire time. The
                            // operator read the screen exactly as it was written and
                            // reported the session hung requesting the oracle — the
                            // guard had answered in milliseconds and the diagnosis
                            // cost an hour. A stale note is worse than no note: it
                            // is a measurement of a moment that has passed, with
                            // nothing on it to say so.
                            //
                            // Cleared rather than replaced: the tool is running and
                            // the phase already says so. A note appears again when
                            // the tool sends one of its own.
                            c.note = None;
                        }
                        None => t.calls.push(CallRow {
                            call_id,
                            name,
                            // `ToolStarted` carries no target and none is invented.
                            target: String::new(),
                            state: CallState::Running,
                            started_ms: ts,
                            // **The clock starts with the same event the log's
                            // does** (R13): a call that waited on a decision did
                            // not spend that time running, and the head's clock is
                            // read here rather than at render so that a replay —
                            // which has no clock yet — falls back to the log's.
                            started_at: self.now_ms,
                            ended_ms: 0,
                            note: None,
                            decision: None,
                        }),
                    }
                }
                Disposition::Rendered
            }
            // Still not accumulated — partial tool output has no durable form, and
            // this event is interactive-only and scrubbed for a late head. But the
            // *latest* note is a fact about right now and a running card has a
            // place for it, which a one-line renderer did not: the old comment
            // ("nowhere to put it, by design") was true of the renderer, not of
            // the event.
            SessionEvent::ToolProgress { call_id, note, .. } => {
                match self
                    .turn
                    .as_mut()
                    .and_then(|t| open_call(&mut t.calls, &call_id))
                {
                    Some(c) => {
                        c.note = Some(note);
                        Disposition::Rendered
                    }
                    None => Disposition::Filtered,
                }
            }
            SessionEvent::ToolFinished {
                call_id,
                outcome,
                payload_digest,
                inline_bytes,
                full_bytes,
                spill,
                edit,
                ..
            } => {
                // A backgrounded call leaves a job behind, and the row the jobs
                // pane shows starts here: the handle is the outcome's own field,
                // not a parse of the result text. The command joins at draw time,
                // through the call's §4.1 target.
                // A backgrounded finish used to push a row here, built from the
                // call this head happened to be holding. The daemon publishes its
                // table every round now, so the row arrives with the next
                // `ListJobs` — with a command that is right whichever turn it is
                // read in.
                // **A backgrounded call is the job STARTING, and the only event that says so**
                // (R51 item 5). There is no `JobStarted` on the wire: `JobSettled` is the whole
                // job vocabulary, so the count cannot be folded out of events — it has to be
                // ASKED for. The proof is the outcome rather than the tool's name: only a call
                // that was actually backgrounded carries a handle.
                //
                // Without this the row a reader sees is a **souvenir**: a count drawn from a list
                // only `/jobs` ever fetched sits at zero for the life of the job.
                //
                // Asked BEFORE the outcome is moved into the call's own state, because this is the
                // last read of it in this arm.
                if matches!(
                    outcome,
                    letibot_transcript::ToolOutcome::Backgrounded { .. }
                ) {
                    self.queued.push(Action::ListJobs);
                }
                if let Some(t) = self.turn.as_mut()
                    && let Some(c) = open_call(&mut t.calls, &call_id)
                {
                    c.ended_ms = ts;
                    c.note = None;
                    c.state = CallState::Finished {
                        outcome,
                        payload_digest,
                        inline_bytes,
                        full_bytes,
                        spill,
                        edit,
                    };
                }
                Disposition::Rendered
            }
            SessionEvent::DecisionRequested {
                req_id,
                kind,
                call_id,
                access,
                summary,
                target,
                write_targets,
                detail,
                options,
                choices,
                because,
                advice,
                deadline,
                on_timeout,
                subagent,
                ..
            } => {
                self.open.retain(|d| d.req_id != req_id);
                // A fresh question starts at the top of its ladder rather than
                // wherever the last one was left: the highlight must never be
                // somewhere the operator did not put it when Enter is one key away.
                self.sel = 0;
                self.open.push(OpenDecision {
                    req_id,
                    write_targets,
                    kind,
                    call_id,
                    access,
                    summary,
                    target,
                    detail,
                    options,
                    choices,
                    because,
                    advice,
                    deadline,
                    on_timeout,
                    subagent,
                    // Not `ts`. A head renders how long a decision has been waiting
                    // from the view's own stamp, and this arm is the live one — the
                    // snapshot path at `apply` carries the real `asked_ts`.
                    asked_ts: 0,
                });
                Disposition::Rendered
            }
            SessionEvent::DecisionAnswered {
                req_id,
                outcome,
                by,
                basis,
                late,
            } => {
                // Three things are read off the open decision **before** it is
                // removed, because the answer event carries only the `req_id`: the
                // summary, the call to put the outcome on, and the oracle's advice.
                // The last is the one the answer event can never carry — its `basis`
                // is the DECIDER's, and under `/supervise` the decider is usually the
                // operator.
                let (summary, call_id, advice) = self
                    .open
                    .iter()
                    .find(|d| d.req_id == req_id)
                    .map(|d| (d.summary.clone(), d.call_id.clone(), d.advice.clone()))
                    .unwrap_or_default();
                self.open.retain(|d| d.req_id != req_id);
                let d = SettledDecision {
                    req_id,
                    call_id: call_id.clone(),
                    summary,
                    outcome,
                    by,
                    basis,
                    advice,
                    late,
                };
                // A permission settles on the call it gated: the approval is a fact
                // about the call, so it rides the call's card in the dim register
                // rather than as a standalone note. A question — or a log recorded
                // before the field existed — has no call to ride, and stays a note.
                if let Some(call_id) = &call_id
                    && let Some(t) = self.turn.as_mut()
                    && let Some(c) = t.calls.iter_mut().find(|c| &c.call_id == call_id)
                {
                    c.decision = Some(d);
                } else {
                    self.note(Note::Decided(d));
                }
                Disposition::Rendered
            }
            SessionEvent::TurnFinished {
                finish_reason,
                usage,
                timings,
                ..
            } => {
                // Kept on the head, not only on the pane: the session header says
                // how much context this conversation is carrying, and that question
                // is asked between turns, when the pane may have been superseded by
                // the transcript. The timings are kept with it — the header now
                // carries the turn's rate and duration too, which is why the footer
                // no longer does. The turn measured its own cache, so the
                // percentage is real.
                self.usage = Some(usage);
                // Summed as the turns land. A turn with no cost — the local
                // server, or a metered model nothing prices — adds nothing and
                // does not light the meter: free and unpriced are both "no
                // number", and `$0.0000` on every local header would be noise.
                if let Some(c) = usage.cost_micros_usd {
                    self.spent_micros += c;
                    self.spent_seen = true;
                }
                self.usage_cache_measured = true;
                self.last_timings = Some(timings);
                // **And the count is re-read at the turn's end** (R51 item 5's third moment). A job
                // that was already running when this head attached is announced by no event at all:
                // the `ToolFinished` that started it happened before the attach, and a `JobSettled`
                // may be an hour away. The turn boundary is the moment the head knows it has been
                // through a round without hearing about it, so it is where the count gets its chance
                // to be right.
                self.queued.push(Action::ListJobs);
                if let Some(t) = self.turn.as_mut() {
                    t.progress = None;
                    t.state = Some(TurnState::Finished {
                        finish_reason,
                        usage,
                        timings,
                    });
                }
                // A terminal state can hand a call back to the transcript.
                // While the pane is drawing a turn, that turn's assistant rows do
                // not draw their own unsettled calls; once it stands down they
                // must, or a call the turn was interrupted in the middle of leaves
                // the screen with nothing said about it. The rendered history is
                // cached, so it has to be told — from the first row this pane
                // owns, which is the only part of it that can render differently
                // now.
                self.invalidate_turn_rows();
                Disposition::Rendered
            }
            // §4.5's terminal event, which did not exist. The head used to be told
            // a `Warning` and nothing else, so `TurnState` stayed `Running` and the
            // spinner span at a dead turn until somebody closed the window; the
            // "nothing received for 17.0s" line below is the *disclosure* that
            // covered for it, and it stays as the backstop for a genuine stall.
            SessionEvent::TurnFailed {
                turn_id,
                error,
                partial_kept,
            } => {
                if let Some(t) = self.turn.as_mut()
                    && (t.turn_id == turn_id || turn_id.is_empty())
                {
                    t.progress = None;
                    t.state = Some(TurnState::Failed {
                        error,
                        partial_kept,
                    });
                }
                // A terminal state can hand a call back to the transcript.
                // While the pane is drawing a turn, that turn's assistant rows do
                // not draw their own unsettled calls; once it stands down they
                // must, or a call the turn was interrupted in the middle of leaves
                // the screen with nothing said about it. The rendered history is
                // cached, so it has to be told — from the first row this pane
                // owns, which is the only part of it that can render differently
                // now.
                self.invalidate_turn_rows();
                Disposition::Rendered
            }
            SessionEvent::TurnInterrupted {
                reason,
                partial_kept,
                ..
            } => {
                if let Some(t) = self.turn.as_mut() {
                    t.progress = None;
                    t.state = Some(TurnState::Interrupted {
                        reason,
                        partial_kept,
                    });
                }
                // A terminal state can hand a call back to the transcript.
                // While the pane is drawing a turn, that turn's assistant rows do
                // not draw their own unsettled calls; once it stands down they
                // must, or a call the turn was interrupted in the middle of leaves
                // the screen with nothing said about it. The rendered history is
                // cached, so it has to be told — from the first row this pane
                // owns, which is the only part of it that can render differently
                // now.
                self.invalidate_turn_rows();
                Disposition::Rendered
            }
            SessionEvent::TranscriptAppended {
                item_id,
                kind,
                ledger_head,
            } => {
                // A tool-result row hands one live card over to the transcript.
                // Positional, not by id: the engine invokes a round's calls in
                // order and appends their rows in the same order, and the ids
                // repeat every round so there is nothing to match on. The duration,
                // the edit pair and the decision are carried across here because
                // they are facts the live card had that the row does not.
                let mut carried: Option<u64> = None;
                let mut carried_edit: Option<letibot_sessionlog::event::ToolEdit> = None;
                let mut carried_decision: Option<letibot_sessionlog::view::SettledDecision> = None;
                if let Some(t) = self.turn.as_mut() {
                    t.appended.push(item_id.clone());
                    t.turn_rows.push(item_id.clone());
                    if kind == "tool_result" {
                        let c = t.calls.get(t.settled_calls);
                        carried = c
                            .filter(|c| c.started_ms > 0 && c.ended_ms > c.started_ms)
                            .map(|c| c.ended_ms - c.started_ms);
                        // The pair rides across with the duration: same card, same
                        // moment, same positional match.
                        carried_edit = c.and_then(|c| match &c.state {
                            CallState::Finished { edit: Some(e), .. } => Some(e.clone()),
                            _ => None,
                        });
                        // The approval rides across too: the decision is a fact
                        // about this call, and the row that outlives the card is
                        // where it has to keep being shown.
                        carried_decision = c.and_then(|c| c.decision.clone());
                        t.settled_calls += 1;
                    }
                }
                if let Some(ms) = carried {
                    self.call_ms.insert(item_id.clone(), ms);
                }
                if let Some(e) = carried_edit {
                    self.call_edits.insert(item_id.clone(), e);
                }
                if let Some(d) = carried_decision {
                    self.call_decisions.insert(item_id.clone(), d);
                }
                // **A user row is drawn from the moment it is announced.** The body
                // follows on its own channel and, behind a running turn, the reply
                // streams in the meantime — so without this the prompt is invisible
                // while the answer to it is already on the screen, and the echo
                // underneath goes on saying `queued` about words that have landed.
                // See `App::bound_prompts` for why this is a guess and what keeps it
                // honest.
                if kind == "user" {
                    self.bind_echo(&item_id);
                }
                self.items.push(SnapshotItem {
                    item_id,
                    kind,
                    ledger_head,
                    // When it happened, from the log's own clock. A head reading a
                    // recorded session must show the same times as the one that
                    // watched it, so this is never `now`.
                    ts,
                    item: None,
                });
                // A row landed, so the transcript moved and everything derived from it
                // — the `!` candidates and the model's suggestions — is stale. The next
                // Tab for the same prefix is a fresh ask.
                self.the_rows_moved();
                Disposition::Rendered
            }
            // The body for a row already announced. Before this existed, a head
            // that was attached when the row landed had no route to the content at
            // all and rendered `[kind id — content not loaded]` for the rest of the
            // session — including for the operator's own prompt.
            SessionEvent::TranscriptContent { item_id, item } => {
                self.record_item(&item_id, *item);
                Disposition::Rendered
            }
            SessionEvent::HeadAttached { .. } => {
                self.heads += 1;
                // **The `system` switch**: *"who attached, and who issued which command"* —
                // the same three events `>= Verbosity::Loud` gated, and now the switch the
                // three profiles name, so `loud` is the set that turns them on.
                if self.visibility.shows(Show::System) {
                    Disposition::Rendered
                } else {
                    Disposition::Filtered
                }
            }
            SessionEvent::HeadDetached { .. } => {
                self.heads = self.heads.saturating_sub(1);
                if self.visibility.shows(Show::System) {
                    Disposition::Rendered
                } else {
                    Disposition::Filtered
                }
            }
            // Into the transcript, where it happened.
            //
            // It used to be pinned to the bottom of the body: the last three
            // warnings sat above the status line forever, so a warning about turn
            // three was still shoving turn nine up the screen, and the operator had
            // no way to say "seen". A warning is an event with a place in the
            // conversation, and putting it there is what makes it scroll away like
            // one — and still be there when you scroll back.
            SessionEvent::ScreenRequested { req_id } => {
                // Queued, not answered here: the answer is the rows this head
                // DRAWS, and they do not exist until the frame is built. The
                // driver takes these after `screen()` and sends exactly what it
                // put on the terminal — anything rendered here instead would be
                // a second rendering, which is the reconstruction this whole
                // frame exists to avoid.
                self.screen_requests.push(req_id);
                self.redraw = true;
                // **And `Filtered` here too, for the same reason R53 gives one paragraph over.**
                // This is a `SessionEvent` — session content, read, and deliberately not drawn as
                // a row of its own, because the answer IS the rows this head draws. `Control`'s
                // definition is *"Not an event"*, and its own docstring above says the same thing
                // about this arm that `OperatorCallAllowed`'s says about its: a reader would never
                // find the difference. The two are changed together because they are one defect
                // spelled at two arms.
                Disposition::Filtered
            }
            SessionEvent::SecretRequested {
                req_id,
                prompt,
                command,
                deadline,
            } => {
                if command.is_empty() {
                    self.key_secrets.push(req_id.clone());
                }
                self.secret = Some(SecretAsk {
                    req_id,
                    prompt,
                    command,
                    deadline,
                });
                self.secret_buf.clear();
                self.redraw = true;
                Disposition::Rendered
            }
            SessionEvent::SecretSettled { req_id, given, by } => {
                // A key card is a secret request with no command (`Harness::obtain_key`),
                // remembered by id because the card itself may already be gone.
                let was_key = match self.key_secrets.iter().position(|r| *r == req_id) {
                    Some(i) => {
                        self.key_secrets.remove(i);
                        true
                    }
                    None => false,
                };
                if self.secret.as_ref().is_some_and(|s| s.req_id == req_id) {
                    self.secret = None;
                    self.secret_buf.clear();
                }
                if was_key {
                    // Given: the daemon says where it was saved (`provider_key_saved`), so
                    // one line rather than two. Not given: the refusal is the operator's.
                    if !given {
                        self.note(Note::Warned(Warned {
                            code: "provider_key_refused".into(),
                            detail: format!("no key given ({by})"),
                            ts,
                        }));
                    }
                    return Disposition::Rendered;
                }
                self.note(Note::Warned(Warned {
                    code: "sudo".into(),
                    detail: if given {
                        format!("password given by {by}")
                    } else {
                        format!("no password given ({by})")
                    },
                    ts,
                }));
                Disposition::Rendered
            }
            // **A command of the operator's own is waiting for an answer.** The card is up
            // for as long as the run is blocked, and the keyboard belongs to it while it
            // is — see the `Key` arm and [`App::prompt_lines`].
            //
            // **The text is NOT masked**, and that is the difference from the arm above
            // rather than an oversight: what this card carries is a line for a program's
            // stdin, drawn in the open. A password has its own path and its own card, and
            // the two must not be one.
            SessionEvent::PromptRequested {
                req_id,
                job,
                command,
                question,
            } => {
                // **A question from a RUNNING PROGRAM outranks the question about killing one.**
                // The two cards are mutually exclusive by construction everywhere else (the
                // confirmation is raised from the composer, and the prompt card owns the
                // composer while it is up) — but a prompt can arrive *while* the confirmation
                // is standing, and then one keystroke would have two meanings: `y` is this
                // card's yes and that card's text. The operator's rule is that neither may be
                // answerable by the other's keystroke, so the confirmation yields, with a
                // sentence — the program has asked something and must not be killable by the
                // answer to it.
                if let Some(ask) = self.term_ask.take() {
                    self.say(&format!(
                        "{} is still running — your command's question came first, so nothing \
                         was ended. `!term close` asks again.",
                        ask.line
                    ));
                }
                self.prompt = Some(PromptAsk {
                    req_id,
                    job,
                    command,
                    question,
                });
                self.prompt_buf.clear();
                self.redraw = true;
                Disposition::Rendered
            }
            // The card comes down, whether it was answered or the command ended — and the
            // sentence says which, because those are different things to have happened to a
            // person who was about to type.
            SessionEvent::PromptSettled { req_id, sent, by } => {
                if self.prompt.as_ref().is_some_and(|p| p.req_id == req_id) {
                    self.prompt = None;
                    self.prompt_buf.clear();
                }
                self.note(Note::Warned(Warned {
                    code: "prompt".into(),
                    detail: if sent {
                        format!("answer sent by {by}")
                    } else {
                        format!("nothing sent ({by})")
                    },
                    ts,
                }));
                Disposition::Rendered
            }
            SessionEvent::Warning { code, detail, .. } => {
                // `turn_failed` is the log's grep-able record of the same fact
                // `TurnFailed` puts under the turn, and the daemon publishes both
                // on purpose — one is state, the other is history. On a *screen*
                // they are the same sentence twice, three lines apart, so this head
                // renders the terminal state and counts the warning as filtered.
                // Counted, not dropped: the status line's `filtered` is what makes
                // "I chose not to show this" different from "nothing happened".
                if code == "turn_failed" {
                    return Disposition::Filtered;
                }
                // **R16, the two halves of a fork.** `auto_compact` is published
                // *before* the fork and says the conversation is about to be
                // replaced; `compacted`/`reseated` are published after it and say it
                // has been. An echo in the air across those two lines was waiting for
                // a row the fork summarised away, so it is resolved here rather than
                // left saying `queued` for the rest of the session.
                if code == "auto_compact" {
                    self.mark_fork();
                }
                if code == "compacted" || code == "reseated" {
                    self.resolve_fork();
                    // **And the fold is over, so the line that walks stops walking.** The
                    // progress event is ephemeral by construction — it has no "done" — so
                    // what ends it is this: the durable warning that says the fork landed.
                    // `auto_compact_failed` clears it too, below: a fold that failed is not
                    // a fold that is still running, and a line that outlives its operation is
                    // the stale measurement this file's `ToolStarted` arm already refuses.
                    self.compacting = None;
                }
                if code == "auto_compact_failed" {
                    self.compacting = None;
                }
                // A slash LISTING opens the pane; a slash sentence stays a note.
                // The daemon sends both under one code — `detail` is the command
                // it echoes back, then the reply — so the head splits them by the
                // only thing that distinguishes them, which is length.
                if code == "slash" || code == "slash_refused" {
                    let (echo, body) = detail.split_once('\n').unwrap_or((&detail, ""));
                    let lines: Vec<String> = body.lines().map(|l| without_control(l)).collect();
                    if lines.len() > 3 {
                        self.slash_out = Some((echo.to_string(), lines));
                        self.pane_scroll = 0;
                        self.redraw = true;
                        return Disposition::Rendered;
                    }
                }
                // A refused job-output read is answered **in the pane that asked**,
                // which is still open — otherwise it would sit at `reading…` for
                // ever, waiting for a window that is not coming. The conversation
                // gets the note as well.
                if code == "job_output_refused"
                    && let Some(v) = self.job_out.as_mut()
                {
                    v.loading = false;
                    v.error = Some(detail.clone());
                    self.redraw = true;
                }
                // **A note about the weather goes on the edge, not in the record.**
                //
                // The operator, on `model_slow_first_byte`: *"it is important diagnostics -
                // we have a yellow triangle for that. both heads should not emit it inside
                // conversation."* So the diagnostic is kept and its PLACEMENT is moved: the
                // count moves a counter, the triangle comes up, and `/status` is where the
                // number lives. `warning::ALARM_ONLY` is the rule and the docstring there
                // says why a compaction stays a row and this does not.
                //
                // **Counted, and `Filtered` rather than dropped.** `Filtered` is what makes
                // "I chose not to show this" different from "nothing happened" — the same
                // distinction the `turn_failed` arm above is refused for. And if this head
                // has no register for a code the tree says is edge-bound, it says so rather
                // than swallowing it: a note that reaches neither the record nor a counter
                // is a note nobody has.
                if letibot_sessionlog::warning::to_the_alarm(&code) {
                    if !self.count_edge_note(&code) {
                        self.note(Note::Warned(Warned {
                            code: "alarm_only_unregistered".into(),
                            detail: format!(
                                "`{code}` is classified as edge-bound and this head has no \
                                 counter for it, so the diagnostic above is the only copy. \
                                 See `warning::ALARM_ONLY`."
                            ),
                            ts,
                        }));
                    }
                    self.redraw = true;
                    return Disposition::Filtered;
                }
                self.note(Note::Warned(Warned { code, detail, ts }));
                Disposition::Rendered
            }
            SessionEvent::CommandIssued {
                head_id,
                identity,
                command,
                note,
                ..
            } => {
                // Two humans in one session: seeing who did what is the point — and
                // seeing *yourself* do what you just did is not. Our own routine
                // acceptances are already covered by `Accepted`.
                if head_id != self.head_id {
                    self.say(&format!("{identity} · {command}: {note}"));
                }
                if self.visibility.shows(Show::System) {
                    Disposition::Rendered
                } else {
                    Disposition::Filtered
                }
            }
            SessionEvent::MergeEntryAdded { entry } => {
                // **Folded, never invented** — the jobs pane's rule: the daemon owns the queue,
                // so an entry this head has not been told about is not one to make up. An entry
                // already here is REPLACED, because the same id arriving twice is the same
                // entry (the enqueue is idempotent by id) and the newer word is the true one.
                match self.merge.iter_mut().find(|e| e.id == entry.id) {
                    Some(row) => *row = entry,
                    None => self.merge.push(entry),
                }
                self.redraw = true;
                // **Counted as filtered and not as control**, for the reason
                // `OperatorCallAllowed`'s arm records: this is a session event, read, and
                // deliberately not drawn as a row of the conversation — the pane is where it
                // goes. `Control`'s definition is *not an event*, and a reader would never find
                // the difference.
                Disposition::Filtered
            }
            SessionEvent::MergeEntryMoved {
                id,
                state,
                evidence,
            } => {
                // **The move, folded onto the row the queue already holds.** An id this head
                // does not have is a move for an entry whose `MergeEntryAdded` it missed —
                // possible, since the events and the snapshot are two arrivals — so the row is
                // NOT invented here: the next `ListMergeQueue` carries it. The state is applied
                // either way, because a row that is here must not go on claiming the state it
                // had.
                if let Some(row) = self.merge.iter_mut().find(|e| e.id == id) {
                    row.state = state;
                    row.evidence = evidence;
                }
                self.redraw = true;
                Disposition::Filtered
            }
            // §6's plan is a document; the transcript is not where it goes.
            SessionEvent::Explain { .. } => Disposition::Filtered,
            // **Always rendered, at every verbosity.**
            //
            // `docs/boundary-and-adjudication.md` §4b: a denial the operator cannot
            // see manufactures the workaround, so there is no verbosity at which
            // hiding this is correct — it is a decision taken on their behalf and
            // only they can lift it. It goes into the transcript, in the place it
            // happened, beside the tool call it refused.
            //
            // This reuses `Note::Warned` rather than growing a note kind of its own:
            // making a denial *look* different from a warning is presentation, this
            // head is somebody else's this session, and a placeholder that rendered
            // nothing would be the invisible denial again with a different cause.
            // The `code` carries the distinction a reader needs, and
            // `repeat_count`/`breaker_open` are on the event for a head that later
            // wants to collapse repeats.
            SessionEvent::DenialRaised {
                request_id,
                tool,
                summary,
                by,
                basis,
                outcome,
                repeat_count,
                breaker_open,
                grant,
                ..
            } => {
                let repeat = if breaker_open {
                    format!(
                        " · breaker OPEN after {repeat_count} consecutive refusals; only you \
                         can lift it"
                    )
                } else if repeat_count > 1 {
                    format!(" · attempt {repeat_count} at the same task direction")
                } else {
                    String::new()
                };
                // **A refusal the harness made is not a question for the operator.**
                //
                // `boundary:` signs the deterministic ones — the normaliser could not
                // read the command, the host boundary refused it. Nobody was asked,
                // no grant lifts it, and the whole explanation is already on the
                // screen as the tool's own result one row above. Rendering it again
                // in red, in full, is the same wall twice: measured at 20 lines for
                // one shell one-liner, and the operator's answer to it was *"it just
                // throws up on my chat"*.
                //
                // A refusal somebody DECIDED still gets the loud register and the
                // request id, because granting it is a thing the operator can do.
                if by.starts_with("boundary:") {
                    // **And only when it has something the row does not.**
                    //
                    // The refused call is already a row in the transcript, one line
                    // below this, carrying the same first sentence — so on a single
                    // refusal this note is the same words twice, which is the noise
                    // it was just cut down from. What the row cannot say is that
                    // this is the SECOND attempt at the same direction, or that the
                    // breaker has closed the direction for the session: that is a
                    // fact about the shape of the session and not about one call,
                    // and it is the one an operator wants to be told.
                    if !repeat.is_empty() {
                        self.note(Note::NotRun(Warned {
                            code: format!("denied:{request_id}"),
                            // The gist, not the transcript of it: layer A's
                            // explanation runs to a paragraph per unresolved
                            // construct and every word of it is for the model.
                            detail: format!(
                                "{tool} {outcome} — {}{repeat}",
                                first_sentence(&basis)
                            ),
                            ts,
                        }));
                    }
                } else {
                    self.note(Note::Warned(Warned {
                        code: format!("denied:{request_id}"),
                        detail: format!(
                            "REFUSED {tool} — {summary}. {outcome} by {by}: {basis}{repeat}. {grant}"
                        ),
                        ts,
                    }));
                }
                Disposition::Rendered
            }
        }
    }

    /// **A page or a wheel over a TAIL-ORIGIN overlay**, which both of them are.
    ///
    /// `sub_out` and `job_out` window their content from the end — a subagent's answer
    /// and a running job's newest bytes are what those panes are opened for — so their
    /// `scroll` counts rows hidden **below** the bottom and moving toward the beginning
    /// ADDS to it. That is the opposite of `pane_scroll`, which counts rows hidden above
    /// the top because a `help`/`todos`/`slash` pane is read from its head.
    ///
    /// One function, because the two must agree about the sign — and because the second
    /// one was **forgotten**: the page keys and the wheel reached this overlay's
    /// transcript instead of the overlay, which is the defect the subagent view had
    /// already been fixed for one arm above (`"a wheel in the subagent output view
    /// scrolled the conversation underneath it"`). One arm is a place to forget.
    ///
    /// Returns whether an overlay took the key, so the caller can fall through to the
    /// transcript when none did.
    pub(crate) fn scroll_tail_overlay(&mut self, up: bool, by: usize) -> bool {
        // **A closure over the value, not a binding to the struct.** The two overlays
        // are different types (`SubOut`, `JobOut`) that happen to share a field name, so
        // an `if let … else if let …` binding one `&mut` for both arms does not compile —
        // which is the compiler saying the obvious thing: there is no shared type here,
        // only a shared rule.
        let moved = |scroll: usize| {
            if up {
                scroll.saturating_add(by)
            } else {
                scroll.saturating_sub(by)
            }
        };
        if let Some(v) = self.sub_out.as_mut() {
            v.scroll = moved(v.scroll);
        } else if let Some(v) = self.job_out.as_mut() {
            v.scroll = moved(v.scroll);
        } else {
            return false;
        }
        self.redraw = true;
        true
    }

    /// **The conversation is about to be replaced** (R16): remember what is in the
    /// air, so the fork can resolve those echoes rather than orphan them.
    ///
    /// Clone rather than a flag, because the list has to survive the echoes being
    /// retired normally in between — a prompt whose row lands before the fork needs
    /// no help from this, and one still waiting does.
    pub(crate) fn mark_fork(&mut self) {
        self.fork_pending = self.pending_prompts.clone();
    }

    /// **The fork happened** (R16): retire the echoes that were waiting on a
    /// transcript that no longer exists.
    ///
    /// The echo's prompt is in the ledger — a fork summarises everything said
    /// before it, which is why the summary exists — so what is retired is the
    /// *mark*, not the words: the conversation above already holds them, as prose
    /// in the summary, and the row that would have carried them was replaced.
    ///
    /// **Only the marked ones.** An echo queued after the fork began belongs to the
    /// new transcript and its row is still coming; retiring it would take a sentence
    /// off the screen that has not landed, which is the defect `pending_prompts`
    /// exists for.
    pub(crate) fn resolve_fork(&mut self) {
        for text in std::mem::take(&mut self.fork_pending) {
            self.retire_pending(&text);
        }
        // **A fork answers the question a snapshot could only raise.** The marks here were
        // `unconfirmed` because the head could not tell *still coming* from *replaced*; a
        // fork the head itself asked for is the second, and the echo is gone with it. Same
        // intersection, so a mark whose echo survived a piece-of-the-row retirement (the
        // engine split the run across a notice) stays until its own row lands.
        let queue: std::collections::HashSet<String> =
            self.pending_prompts.iter().cloned().collect();
        self.unconfirmed.retain(|u| queue.contains(u));
    }

    /// Stand down the echo of a queued prompt whose row has landed.
    ///
    /// **The unit is a LINE, not a message, and that is the whole of the fix.** The
    /// old rule asked whether the landing row *was* the echo, or began with it, and
    /// both questions are about whole strings while the thing being compared is a
    /// **run of prompts**: the engine merges the operator's consecutive prompts into
    /// ONE user item joined by newlines (`SteeringMessage::to_item`), and a head only
    /// sometimes does the same joining itself ([`App::submit`]'s coalescing is
    /// conditional on a turn it thinks is running). So the shapes that meet are
    /// "six prompts in the queue, one six-line row" and "two prompts joined in the
    /// queue, one five-line row" — and against a whole-string rule every one of them
    /// compares NO, which leaves the echo on the screen for the rest of the session.
    ///
    /// Measured on this head, 2026-09-23: six `queued ·` echoes of the R27
    /// instruction's six paragraphs, every one of them answered, none retired —
    /// because the row that answered them was their **join** (2752 characters, six
    /// lines) while the queue held two hundred-to-five-hundred characters per entry.
    ///
    /// So: a row's lines are consumed, once each, by the pending pieces that equal
    /// them as **whole lines**, from the front of the queue backwards. A piece that
    /// consumes nothing keeps its place; an entry left with nothing retires. The
    /// word *whole* is the correctness: a prompt `second thing` is NOT retired by a
    /// row reading `first thing\nsecond thing-guess`, which is the failure that
    /// matters — a head that swallows a prompt the daemon has not answered has put a
    /// sentence the operator typed where nobody will ever see it.
    ///
    /// **What is deliberately not done: matching a substring, or matching pieces out
    /// of order.** A piece is claimed only by a whole line at or after the last line
    /// claimed, so a queue whose pieces appear reversed in a row keeps them (an echo
    /// left standing costs a stale line; an echo wrongly retired costs the
    /// sentence), and a piece that is a *fragment* of a line claims nothing at all.
    ///
    /// A prompt coincidentally EQUAL to one whole line of a longer prompt the
    /// operator typed separately would still retire. That residue is accepted and
    /// older than this fix — it is the same risk [`App::retire_pending`]'s old
    /// front-piece branch carried, and the alternative (never retiring a piece) is
    /// the defect measured above.
    ///
    /// **This is the only thing that retires an echo, and it takes content.** The
    /// announcement of a user row says nothing about whose words it carries — a
    /// harness notice and another head's prompt are the same item — so a head that
    /// retired on [`SessionEvent::TranscriptAppended`] would lose the echo of a
    /// prompt still sitting in the hub's queue, and the operator would watch their
    /// own sentence vanish. See [`App::bound_prompts`] for the half that *is* drawn
    /// from an announcement.
    pub(crate) fn retire_pending(&mut self, row: &str) {
        // **An echo that is not in the queue is not in the unconfirmed set either.**
        // An intersection, not a removal of the row's text.
        //
        // Measured on this surface, 2026-09-23, in leticl's words and true here for the
        // same structural reason: *"an unconfirmed echo's text is never a queued text"*,
        // so `unconfirmed.retain(|u| u != row)` matches nothing — the row's text is the
        // engine's JOIN, not the echo — and an echo marked unconfirmed by an earlier
        // snapshot **whose row later landed inside a merged item** would be retired from
        // the queue and stay in this set for ever. The set is not the echo's text; it is a
        // mark ON an echo, so the only honest update is to keep the marks whose echo is
        // still there.
        let lines: Vec<&str> = row.split('\n').collect();
        // **A line is spent once.** Two prompts that say the same thing stay queued
        // separately until each of their rows lands — the property the equality rule
        // gave, which a rule retiring every matching entry would lose.
        let mut claimed = vec![false; lines.len()];
        let mut cursor = 0usize;
        let mut i = 0usize;
        while i < self.pending_prompts.len() {
            match strip_landed(&self.pending_prompts[i], &lines, &mut claimed, &mut cursor) {
                // Nothing of this entry is in the row. It keeps its place.
                None => i += 1,
                // Every piece of it has landed. The echo stands down.
                Some(rest) if rest.is_empty() => {
                    self.pending_prompts.remove(i);
                }
                // Some of it has. The echo shrinks to what is still owed.
                Some(rest) => {
                    self.pending_prompts[i] = rest;
                    i += 1;
                }
            }
        }
        // **The intersection, taken AFTER the loop.** Building it before is the same
        // defect inverted, and it is how the first version of this fix leaked: the set
        // held the queue as it was when the row arrived, so an echo that had just stood
        // down was still in it — a mark outliving the thing it was a mark on, which is
        // precisely the leak the intersection exists to close. Found by the assertion
        // below this call, on the first run.
        let queue: std::collections::HashSet<String> =
            self.pending_prompts.iter().cloned().collect();
        self.unconfirmed.retain(|u| queue.contains(u));
    }

    /// **Bind the oldest unbound echo to a row that has just been announced.**
    ///
    /// Called for a body-less `user` row, which is the shape of this head's own
    /// prompt *and* of a steering notice, a §5.7 salvage notice and another head's
    /// prompt. The announcement cannot tell them apart, so this is a guess: what it
    /// buys is that the row is drawn from the words the head already holds, at the
    /// position the transcript gave it — above the reply it caused — instead of
    /// being invisible until its body catches up while the reply streams above it.
    ///
    /// Oldest first, and never an echo already bound: several prompts in the air at
    /// once is the normal case behind a running turn, and their rows are announced
    /// in the order they were sent. The echo **stays in `pending_prompts`** — this
    /// binds a drawing, it does not retire anything (see [`App::retire_pending`]).
    ///
    /// The order is: a `BTreeMap`-free [`HashMap`] lookup, one scan of the pending
    /// list, and one clone of the text being bound. `pending_prompts` is one to a few
    /// entries — behind a running turn the engine merges consecutive operator
    /// messages, so it is ordinarily *one* — and this runs once per announced row,
    /// so it is nothing next to the render it is feeding.
    pub(crate) fn bind_echo(&mut self, item_id: &str) {
        if self.pending_prompts.is_empty() {
            return;
        }
        let taken: std::collections::HashSet<&str> =
            self.bound_prompts.values().map(String::as_str).collect();
        let Some(text) = self
            .pending_prompts
            .iter()
            .find(|p| !taken.contains(p.as_str()))
            .cloned()
        else {
            return;
        };
        self.bound_prompts.insert(item_id.to_string(), text);
    }

    /// The echo texts a **body-less row on screen is already drawing**, so the tail
    /// must not draw them a second time.
    ///
    /// Owned rather than borrowed, because the caller holds it across the frame's
    /// disjoint borrow of `self`. It is one entry per prompt in the air — ordinarily
    /// one — and it is built once per frame.
    ///
    /// Derived from `items` on every frame rather than counted, for the reason
    /// [`App::bulk`] gives: `items` is replaced wholesale by a snapshot, so anything
    /// remembered about the rows it replaced describes rows that no longer exist. A
    /// binding whose row has been trimmed out of the view, or replaced by a snapshot,
    /// draws nothing — and this then draws the echo at the tail again, which is the
    /// honest answer: the words are still this head's to show.
    ///
    /// **As LINES, not as whole texts** (R51 item 15). The tail does not ask *is this
    /// entry already on screen* — that question is answered NO for any entry that grew
    /// after it was bound, and the whole entry is then drawn twice. What it needs is the
    /// pieces being drawn, so it can take exactly those out. See [`unclaimed_prompts`],
    /// which is the walk that spends them.
    pub(crate) fn echoes_on_screen(&self) -> Vec<(String, Vec<String>)> {
        self.items
            .iter()
            .filter(|it| it.item.is_none())
            .filter_map(|it| self.bound_prompts.get(&it.item_id))
            .map(|text| (text.clone(), text.split('\n').map(str::to_string).collect()))
            .collect()
    }

    /// **Move the running command to the background** — `ctrl-o` and `/promote`.
    ///
    /// The fact to guard is a command running, and the check used to ask whether the
    /// TURN was running instead. They come apart: a terminal turn state can leave a call
    /// unsettled — the comment on the `TurnFinished` arm says so in as many words, and
    /// the engine emits `TurnFinished` on the interrupt paths while a tool is still
    /// executing. The operator, looking at a `◐ Running "cargo test …"` card while the
    /// head said otherwise: *"nothing is running to move to the background"* /
    /// *"how come"*.
    ///
    /// So it asks the calls. The daemon honours a promote inside `bash`'s own wait loop,
    /// which exists only while a command is executing, so a running call is not a proxy
    /// for the thing being promoted — it IS it.
    pub(crate) fn promote(&mut self) -> Option<Action> {
        if self.running_call().is_some() {
            self.say("moving the running command to the background");
            return Some(Action::Promote);
        }
        // Two different silences, and a head that said the same thing for both sent the
        // operator looking for a command that had not been started yet. **Busy, not generating**: a
        // turn waiting on a call is still working, and *the model is still working* is the true
        // sentence for it.
        if self.turn_busy() {
            self.say("the model is still working — there is no command running to move yet");
        } else {
            self.say("nothing is running to move to the background");
        }
        None
    }

    /// **The command running right now**, whatever the turn's own state says.
    ///
    /// Ctrl+O's precondition, and deliberately not [`App::turn_busy`] either: this asks for a
    /// command the daemon is *executing*, which is a fact about ONE call and not about the turn.
    pub(crate) fn running_call(&self) -> Option<&CallRow> {
        self.turn
            .as_ref()?
            .calls
            .iter()
            .find(|c| matches!(c.state, CallState::Running))
    }

    /// Attach content to a transcript row, from whatever route the daemon offers.
    pub fn record_item(&mut self, item_id: &str, item: TranscriptItem) {
        let prose = matches!(item, TranscriptItem::Assistant { .. });
        // **A reasoning row takes over the reasoning it carries** — the mark advances to the end
        // of what has arrived, so the live count is only ever the part no row holds. See
        // [`TurnPane::reasoned_upto`]: without this the count includes landed reasoning twice, and
        // the round boundary then makes it fall.
        if matches!(item, TranscriptItem::Reasoning { .. })
            && let Some(t) = self.turn.as_mut()
        {
            t.reasoned_upto = t.reasoning.raw().len();
        }
        // **Content ends the binding, either way.** Confirmed: the row renders from
        // its real body and the echo retires by text below. Contradicted: the row was
        // never this head's prompt — a steering notice, a §5.7 salvage notice,
        // another head's prompt — and the echo is still in `pending_prompts`, so it
        // goes back to the tail where it belongs. Either way the guess has served its
        // purpose, and neither branch may retire on the announcement instead — see
        // `App::retire_pending`.
        self.bound_prompts.remove(item_id);
        // **A body landing takes its id off the bulk announcement**, so the count follows
        // the evidence and not a clock — and an empty set means the carry is complete and
        // the trigger clears itself.
        if let Some(b) = self.bulk.as_mut() {
            b.ids.remove(item_id);
            if b.ids.is_empty() {
                self.bulk = None;
            }
        }
        // A user row with body is the transcript taking a queued prompt over. The
        // steering path appends the operator's words verbatim
        // (`SteeringMessage::to_item`: "a plain `User` item with exactly its own
        // text"), so the text is the match — and one row retires one entry, so two
        // prompts that say the same thing stay queued separately until each of
        // their rows lands.
        // **Every text part, like [`App::load`].** This read only the FIRST part, and
        // the snapshot path read every one — so the two paths could retire different
        // things from the same row, which is the drift leticl measured on its own head
        // (*"the live arm read only the FIRST text part where the snapshot path reads
        // every part"*). One call site each; the common case is one part holding the
        // engine's join, and a two-part item is two things said.
        if let TranscriptItem::User { parts, .. } = &item {
            for text in parts.iter().filter_map(|p| match p {
                UserPart::Text { text } => Some(text.as_str()),
                _ => None,
            }) {
                self.retire_pending(text);
            }
        }
        let Some(idx) = self.items.iter().position(|r| r.item_id == item_id) else {
            // **A body with no row to land on — counted, never silent** (R17).
            //
            // This used to be a bare `return`, and it is the third way a row the
            // ledger has can be missing from the screen: the announcement was
            // replaced by a snapshot that no longer carries this id, so the words
            // arrive with nowhere to go and are thrown away. Nothing said so, and
            // nothing could — the row is not in `items`, so there is not even a
            // placeholder to notice.
            //
            // It is counted rather than made to work because there is nothing to
            // recover: an out-of-order body for a row nobody has is exactly the
            // case a snapshot exists to resolve. What can be wrong here is the
            // *frequency*, and a number is how that becomes visible.
            self.orphan_bodies += 1;
            self.note(Note::Warned(Warned {
                code: "orphan_body".into(),
                detail: format!(
                    "a row's content arrived for `{item_id}`, which this head is not \
                     holding — a snapshot replaced the rows and this one was not in it, \
                     so its words have nowhere to land and are recorded only here. \
                     `/status` counts how often this has happened; a body that arrives \
                     for a row that is gone is not a rendering choice."
                ),
                ts: 0,
            }));
            self.redraw = true;
            return;
        };
        self.items[idx].item = Some(item);
        // **A body landing is the transcript moving too.** The row was announced with no
        // content, so it carried no tool calls a moment ago: a `!` candidate list built
        // then is missing every command this row ran, and a model asked then was asked
        // about a row that had not arrived. See [`App::the_rows_moved`].
        self.the_rows_moved();
        // The row's rendered form changed, so the history cache from that row
        // on is stale. From that row on, and not from row zero: this is the
        // hottest of the invalidations — one per transcript row, so one per
        // row per session — and re-rendering the rows above a row whose body
        // just arrived is the whole session, again, for every row in it. From
        // the head of its ROUND, because a round is what renders as a unit; see
        // `round_head`.
        let k = self.round_head(idx);
        self.invalidate_history_from(k);
        // The pane's accumulated prose, handed over the same way its calls are.
        //
        // `TurnPane::text` is every `Delta { target: Text }` of the whole turn, and
        // a turn's prose is committed to the transcript one ROUND at a time. So
        // once a round's assistant row has its body, the sentence the model wrote
        // before its first tool call is on the screen twice — in history where it
        // belongs and again in the pane below the cards. Measured at 60x34 on the
        // operator's session: "I'll take a look at what's in the tree first."
        // appearing above the round's cards and again under them.
        //
        // Clearing rather than counting bytes, because that is what "the
        // transcript has taken this over" means, and because `IncrementalMarkdown`
        // is a frozen-prefix lexer — slicing it would mean re-lexing what it has
        // already frozen, which is the §13.3 rule this head is built around.
        //
        // Safe against a race only because the engine appends a round's assistant
        // row before generating the next round (`harnessd::harness`), so no delta
        // of round N+1 can arrive before round N's row.
        if prose
            && let Some(t) = self.turn.as_mut()
            && t.appended.iter().any(|a| a == item_id)
        {
            t.text = IncrementalMarkdown::new();
            t.text_cache = BlockCache::new();
        }
    }
}
