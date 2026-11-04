//! A `Hello`: the daemon seating this head in a session - on the first attach, a switch, a
//! reconnect - with the snapshot it brings.

use super::*;
use letibot_sessionlog::event::Usage;
use letibot_sessionlog::protocol::ServerFrame;
use letibot_sessionlog::view::Warned;

impl App {
    /// A `Hello`: the head seated in a session, and the snapshot loaded. One family of [`App::apply`]'s arms, moved here
    /// verbatim; `apply` hands it only these frames.
    pub(crate) fn on_hello(&mut self, frame: ServerFrame) -> Disposition {
        match frame {
            // Once per ATTACHMENT, not per connection: a `Switch` is answered with a second `Hello`,
            // and this arm is the whole of the head's switch path - which is why the daemon answers a
            // switch with a `Hello` rather than a frame of its own, the late-join path being proven.
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
                // The reseat card is the daemon's to raise, on every attach (`seat_in` says
                // `prefix_stale` right after this `Hello` while it is still true), so a seating forgets
                // it: the daemon's word, not this head's memory, decides what is stale.
                self.reseat = None;
                // A board this head read is not a board THIS attachment read (see [`App::asks_board`]):
                // an attach, a re-attach and a return from a switch all land here, and the board may
                // have moved while the head was away - so it is dropped and asked for again below.
                self.asks_board = None;
                // The daemon has answered, so whatever the head drew while asking is about to be
                // replaced by the truth (see `attaching`) - unless this is the stop on the way: a head
                // that asked for a stored session is seated elsewhere first, and that is not the answer.
                match self.fetching.as_deref() {
                    Some(asked) if asked != session_id => {}
                    _ => {
                        self.fetching = None;
                        self.attaching = false;
                    }
                }
                // The version the daemon is TOLD to be, checked at the handshake - the one moment a
                // skew is cheap to say, with nothing read yet and both numbers nameable. It does not
                // exit: a skew is usually survivable, so the head says which way round it is and goes on.
                self.daemon_protocol = Some(protocol_version);
                // And which daemon that is: a head outlives the daemon that gave it its facts, so the
                // party at the other end is checked rather than assumed. The pid is re-read per
                // connection, and the check is on the SEATING - once per attach, not per keystroke.
                let seat = DaemonSeat {
                    pid: self.daemon_pid,
                    protocol: protocol_version,
                };
                let was = self.daemon_seat;
                self.daemon_seat = Some(seat);
                // Held, not filed: `load` replaces `self.notes` wholesale a few lines down, so a note
                // written here is thrown away (see `App::link_up`). Both numbers on both sides, and
                // `None` is said as `not told` - a pid the kernel declined is not a pid of zero.
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
                // The `Hello` is what says the link is back: it is the frame that seats this
                // connection, so it is the only honest answer to "are we attached". `link_up` is a
                // no-op when nothing was down - every ordinary attach, and a `Switch`'s second `Hello`.
                let reattached = self.link_up();
                let mut skew_said = None;
                if let Some(said) = letibot_sessionlog::protocol_skew(
                    protocol_version,
                    letibot_sessionlog::protocol::PROTOCOL_VERSION,
                ) {
                    // Held until after the snapshot is folded in: `load` replaces `self.notes`
                    // wholesale, so a note filed before it is thrown away rather than anchored at the
                    // frame that revealed it.
                    skew_said = Some(said);
                }
                // Every session-scoped read this head owes itself, in one place - an attach, a
                // re-attach and the return from a switch are all this frame, so there is one call and
                // not three. `term_fact` starts `Unasked` rather than `None`: they are different facts.
                self.term_fact = PaneFact::Unasked;
                self.close_pending = false;
                self.term_ask = None;
                // The seating's own asks are held while the attach is read-only, and that is the feature
                // working at all: each is a frame an older daemon may not know, sent without the operator
                // typing anything, and one refusal would kill the attach. `App::attach_anyway` re-sends.
                if !self.skew_locked() {
                    self.queued.push(Action::TermStatus);
                    self.refetch_session_facts();
                }
                self.head_id = head_id.clone();
                self.seated = Some(head_id);
                self.wiring = wiring;
                // Sub-sessions are KEPT: a child is a session the daemon holds, a head can post to it
                // and it answers, so dropping it throws away a session the head can DRIVE. The noise
                // the filter was for is [`App::session_rows`]'s business - nested, folded by default.
                self.sessions = sessions;
                // A removal is about a row of the daemon's list, so it goes when the row does - down
                // the chain to where this head has just been seated (`App::prune_removed`), or a switch
                // onto a removed row stands in a subtree the head's own list filters out.
                self.prune_removed(&session_id);
                self.dropped += dropped;
                self.scrubbed += scrubbed.total();
                // `session_id` is assigned by `load` and NOT before it: `load` decides whether this is
                // the same session by comparing the two, and assigning first made that comparison always
                // true - so a switch kept the previous session's token count over an empty transcript.
                match snapshot {
                    Some(s) => self.load(*s),
                    // A resume served from the scrollback: no snapshot, and the
                    // state that is already here is this session's.
                    None => {
                        self.session_id = session_id;
                        self.restoring = self.rows_the_store_holds();
                    }
                }
                // A reconnect's replay scrubs the held counts, so a count this head
                // read before the gap may be one whose zero it never saw; the daemon says the
                // count again after every attach.
                self.held = None;
                // Now it can be said. A note rather than a `say`: this outlives the next keystroke and
                // belongs in the conversation, anchored at the end of whatever the snapshot carried.
                // `note` dedupes on `(code, detail, ts)`, so a `Switch`'s second `Hello` says it once.
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
                // A daemon that restarted has no turn state in the snapshot (`TurnFinished` is
                // ephemeral, and a view rebuilt from the transcript has no turn), so the context comes
                // from the session's own row. A live session's snapshot already set it, and that wins.
                if self.usage.is_none()
                    && let Some(b) = self
                        .sessions
                        .iter()
                        .find(|s| s.session_id == self.session_id)
                    && let Some(tokens) = b.context_tokens
                {
                    // The row knows the prompt size, and the cache fraction only if a turn finished
                    // after that column existed: a backfilled row has the size and not the fraction,
                    // and the header shows the percentage only when it was measured.
                    self.usage_cache_measured = b.context_cached.is_some();
                    self.usage = Some(Usage {
                        prompt_tokens: tokens,
                        cached_tokens: b.context_cached.unwrap_or(0),
                        predicted_tokens: 0,
                        // A backfilled row carries the prompt size, never the cost: a zero would report a metered session
                        // as free. Nor a slot: a row that predates the column cannot say which slot served those turns.
                        cost_micros_usd: None,
                        ..Default::default()
                    });
                }
                // And what the store says this session holds, while the head holds nothing - the
                // disagreeing sources as a state the screen can draw instead of the empty-transcript
                // banner. `load` asks it for the snapshot arm; this asks it for the arm without one.
                self.restoring = self.rows_the_store_holds();
                if moved {
                    // The picker is closed by arriving, not by the key that opened it: the switch is
                    // the answer to the question the picker asked, and leaving it up over the session
                    // just joined is a screen the operator has to dismiss for no reason.
                    self.close_surface(Surface::Picker);
                    self.say(&format!(
                        "switched to {}",
                        self.session_label(&self.session_id)
                    ));
                }
                // And the starter todos, on the one moment a head learns its project: the HELLO. A
                // switch lands here too, so each project is checked in its own right, and the seed
                // itself waits for the board (`todo_seed_pending`) - this head keeps no second list.
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
