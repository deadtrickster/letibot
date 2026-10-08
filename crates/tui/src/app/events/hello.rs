//! **A `Hello`**: the daemon seating this head in a session — on the first attach, a switch, a
//! reconnect — with the snapshot it brings.

use super::*;
use letibot_sessionlog::event::Usage;
use letibot_sessionlog::protocol::ServerFrame;
use letibot_sessionlog::view::Warned;

impl App {
    /// **A `Hello`**: the head seated in a session, and the snapshot loaded. One family of [`App::apply`]'s arms, moved here
    /// verbatim; `apply` hands it only these frames.
    pub(crate) fn on_hello(&mut self, frame: ServerFrame) -> Disposition {
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
            _ => unreachable!("on_hello was handed a variant it does not handle"),
        }
    }
}
