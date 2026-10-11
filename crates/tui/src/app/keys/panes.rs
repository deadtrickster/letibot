//! **Keys a pane owns**: the outputs (a subagent's, a job's, a slash command's), the config
//! pane, the overlays, the session picker, and the subagents, todos, queue and jobs panes.

use super::*;
use std::ops::ControlFlow;

impl App {
    /// **The keys of the surface in front, and of no other** (R69).
    ///
    /// The surface is [`App::focused_surface`] — the stack's top, unless a `!term` pane, the diff
    /// popup or rano is drawn over it — which is the same value `main_area` draws. A pane that is
    /// open behind another is not asked at all. That is the whole fix for the review's worst
    /// head finding: `ctrl-t` then `ctrl-g` drew the todos pane while ↑↓ and Enter went to the
    /// hidden subagents list, and **Enter switched session into a row nobody saw**, because the
    /// key order (subagents > todos > queue > jobs > standing) was a second list from the draw
    /// order (todos > queue > standing > subagents > jobs).
    ///
    /// # Esc and ctrl-c pop
    ///
    /// Whatever the surface did not take for itself. An overlay's Esc goes back to its list
    /// because the list is the next entry down — not because its handler was placed above the
    /// block that closed every pane at once, which is what used to hold it there.
    ///
    /// # A surface owns Enter, even when it has nothing to act on
    ///
    /// The operator, 2026-10-03, having gone to the jobs pane and pressed Enter with a stray
    /// character in the composer: *"the pane own keyboard in a way, so enter is a pane thing."*
    /// A surface that names no meaning for Enter (help, `/status`, an empty jobs list) takes it
    /// anyway. **What is deliberately NOT here** is in [`Surface::owns_enter`]: where a typed
    /// line IS the answer — a row number, an id prefix — `submit` routes the words and the
    /// surface keeps the typed path it advertises. The merge queue was missing from the list
    /// this replaced, so Enter with typed text sent the prompt over an empty queue and opened an
    /// entry over a non-empty one.
    pub(crate) fn key_surface(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        let Some(s) = self.focused_surface() else {
            return ControlFlow::Continue(());
        };
        let taken = match s {
            Surface::SubOut => self.key_sub_out(k),
            Surface::JobOut => self.key_job_out(k),
            Surface::Listing => self.key_slash_out(k),
            Surface::Config => self.key_config_pane(k),
            Surface::QueueEntry => self.key_queue_review(k),
            Surface::TodoRow => self.key_todo_open(k),
            Surface::Picker => self.key_picker(k),
            Surface::Subagents => self.key_subagents_pane(k),
            Surface::Todos => self.key_todos_pane(k),
            Surface::Queue => self.key_queue_pane(k),
            Surface::Jobs => self.key_jobs_pane(k),
            Surface::Standing => self.key_standing_pane(k),
            Surface::Help | Surface::Stats => ControlFlow::Continue(()),
        };
        if taken.is_break() {
            return taken;
        }
        match k {
            Key::Esc | Key::CtrlC => {
                self.pop_surface();
                ControlFlow::Break(None)
            }
            Key::Enter if s.owns_enter() => ControlFlow::Break(None),
            _ => ControlFlow::Continue(()),
        }
    }

    /// **The subagent output view owns the keys while it is open.** Arrows
    /// scroll it like a terminal — up toward the beginning, down back to the
    /// tail — Enter reads the same subagent again, because a running one has
    /// new output, and Esc goes back to the tree. This sits ahead of the
    /// generic Esc below on purpose: Esc here means "back to the tree", not
    /// "close everything".
    pub(crate) fn key_sub_out(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if self.sub_out.is_some() {
            match k {
                // **Through the one function that knows the sign**, so the arrows, the
                // page keys and the wheel cannot disagree about which way is back.
                Key::Up => {
                    self.scroll_tail_overlay(true, 1);
                    return ControlFlow::Break(None);
                }
                Key::Down => {
                    self.scroll_tail_overlay(false, 1);
                    return ControlFlow::Break(None);
                }
                Key::Enter => {
                    let id = self.sub_out.as_ref().unwrap().session_id.clone();
                    self.sub_out_pending = Some(id.clone());
                    return ControlFlow::Break(Some(Action::Peek(id)));
                }
                _ => {}
            }
        }
        ControlFlow::Continue(())
    }

    /// **The job-output view owns the keys while it is open.** Up and down walk
    /// the loaded window; right asks for the next page and left walks back the
    /// way right came; Enter refreshes a running job, or takes the next page when
    /// there is one; Esc goes back to the jobs list, not out of everything — the
    /// same shape, key for key, as the subagent-output view above.
    pub(crate) fn key_job_out(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if self.job_out.is_some() {
            match k {
                Key::Up => {
                    self.scroll_tail_overlay(true, 1);
                    return ControlFlow::Break(None);
                }
                Key::Down => {
                    self.scroll_tail_overlay(false, 1);
                    return ControlFlow::Break(None);
                }
                Key::Enter => {
                    return ControlFlow::Break(self.job_out_page(true));
                }
                // **`Right` keeps the guard that `Enter` just lost**, and the difference
                // is what the key is *for*. Enter here is the pane's — it is the key the
                // pane advertises and the operator's words are not what they meant by it.
                // Right is a cursor key first: a half-typed line keeps its motion, which
                // is the same reason the composer's own arrows are not up for grabs.
                Key::Right if self.editor.text().is_empty() => {
                    return ControlFlow::Break(self.job_out_page(true));
                }
                Key::Left if self.editor.text().is_empty() => {
                    return ControlFlow::Break(self.job_out_page(false));
                }
                _ => {}
            }
        }
        ControlFlow::Continue(())
    }

    /// **The slash listing owns the keyboard while it is up**, the way the
    /// subagent-output pane above does: it is a screen covering the
    /// conversation, so the keys that scroll and dismiss it must not also
    /// reach the composer behind it.
    pub(crate) fn key_slash_out(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if self.slash_out.is_some() {
            match k {
                // **Up moves toward the beginning, which means DECREASING this
                // offset.** `pane_scroll` counts rows hidden **above the top** —
                // `pane_window` is literally `skip(self.pane_scroll)` — so adding to it
                // walks further *down* the document. These two arms had it inverted, so
                // `↑` scrolled a `/notes` listing toward its end while the footer under
                // it said `up/down scrolls`. It is leticl's own finding in the same
                // place: *"called with the top-origin sign, ↑ walked toward the END
                // while the hint bar said otherwise."*
                Key::Up => {
                    self.pane_scroll = self.pane_scroll.saturating_sub(1);
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                // Down is the bounded one: `pane_window` clamps it against the rows
                // it actually has, which is the only place that knows how many there
                // are (the slash listing is built on every draw).
                Key::Down => {
                    self.pane_scroll = self.pane_scroll.saturating_add(1);
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                _ => {}
            }
        }
        ControlFlow::Continue(())
    }

    /// Help and the picker are screens, and the two keys that mean "go back"
    /// close them before the composer ever sees them.
    /// The config pane owns Up/Down/Enter while it is open: arrows move,
    /// Enter changes the row under the cursor when it is one that can change
    /// now, and says why when it is not.
    pub(crate) fn key_config_pane(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        {
            match k {
                Key::Up => {
                    let n = self.config_rows().len().max(1);
                    self.config_sel = if self.config_sel == 0 {
                        n - 1
                    } else {
                        self.config_sel - 1
                    };
                    self.config_follow();
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                Key::Down => {
                    let n = self.config_rows().len().max(1);
                    self.config_sel = (self.config_sel + 1) % n;
                    self.config_follow();
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                Key::Enter => {
                    return ControlFlow::Break(self.config_change());
                }
                _ => {}
            }
        }
        ControlFlow::Continue(())
    }

    /// **An entry's detail overlay owns Esc, the arrows and nothing else.** Esc goes back to
    /// the LIST, which is still behind it — the jobs pane's rule, one pane along.
    ///
    /// **It sits ABOVE the block that closes every pane on Esc, and that is load-bearing.**
    /// Below it, the first Esc closed the PANE and left the overlay standing — and because
    /// the overlay is drawn before the pane, the screen did not change: one press did
    /// nothing, two presses left the queue. Every other overlay in this file (`sub_out`,
    /// `job_out`) is above that block for the same reason, and
    /// `esc_leaves_the_entry_overlay_with_the_queue_still_behind_it` is the test that holds
    /// this one there.
    pub(crate) fn key_queue_review(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if self.queue_open.is_some() {
            // **The arrows only.** The page keys and the wheel are `key_chords`'s, which runs
            // first and settles them for every pane through `pane_scroll` — the rule
            // `key_todo_open` writes down. A PageUp/PageDown arm stood here and was dead code that
            // looked live: the chords took the keys first and, with the queue missing from their
            // list, scrolled the transcript instead.
            if matches!(k, Key::Up | Key::Down) {
                if matches!(k, Key::Up) {
                    self.pane_scroll = self.pane_scroll.saturating_sub(1);
                } else {
                    self.pane_scroll += 1;
                }
                // Clamped against the last draw's own numbers: the key handler has no width and
                // no height, and a scroll clamped against a guess walks past the end.
                let max = self.pane_len.saturating_sub(self.pane_room);
                self.pane_scroll = self.pane_scroll.min(max);
                self.redraw = true;
                return ControlFlow::Break(None);
            }
        }
        ControlFlow::Continue(())
    }

    /// **An open session picker owns Up and Down, and Enter on an empty line.**
    ///
    /// After the decision ladder, which keeps precedence while a prompt is up. The
    /// number path is untouched — digits still land in the composer and Enter
    /// still answers them — but the list is on the screen, so the arrows move the
    /// cursor on it rather than the caret in a composer the picker is covering.
    /// The empty-composer rule is the decision ladder's own: a half-typed id's
    /// Enter still means the id.
    pub(crate) fn key_picker(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if !self.sessions.is_empty() {
            let rows = self.session_rows();
            let n = rows.len();
            match k {
                Key::Up => {
                    self.picker_sel = if self.picker_sel == 0 {
                        n - 1
                    } else {
                        self.picker_sel - 1
                    };
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                Key::Down => {
                    self.picker_sel = (self.picker_sel + 1) % n;
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                // **A conversation with sub-sessions opens and closes**, the two gestures a tree
                // uses everywhere — and the reason a collapsed default can afford to be collapsed.
                // Guarded on an empty composer, like every other pane's arrows.
                Key::Right if self.editor.text().is_empty() => {
                    let id = self.sessions[rows[self.picker_sel.min(n - 1)].idx]
                        .session_id
                        .clone();
                    if !self.expanded.iter().any(|e| *e == id) {
                        self.expanded.push(id);
                        self.redraw = true;
                    }
                    return ControlFlow::Break(None);
                }
                Key::Left if self.editor.text().is_empty() => {
                    let row = rows[self.picker_sel.min(n - 1)];
                    // **A child closes its PARENT**, so `←` means *back up the tree* rather than
                    // nothing at all on the row you just arrived at.
                    let want = match row.depth {
                        0 => Some(self.sessions[row.idx].session_id.clone()),
                        _ => self.sessions[row.idx].parent_session_id.clone(),
                    };
                    if let Some(want) = want
                        && let Some(pos) = self.expanded.iter().position(|e| *e == want)
                    {
                        self.expanded.remove(pos);
                        self.redraw = true;
                    }
                    return ControlFlow::Break(None);
                }
                Key::Enter if self.editor.text().is_empty() => {
                    let id = self.sessions[rows[self.picker_sel.min(n - 1)].idx]
                        .session_id
                        .clone();
                    return ControlFlow::Break(self.switch_to(id));
                }
                _ => {}
            }
        }
        ControlFlow::Continue(())
    }

    /// **An open subagent pane owns Up and Down, and ENTER IS THE SWITCH INTO THAT
    /// SUBAGENT'S SESSION.** One keystroke, because that is what entering a row means
    /// everywhere else in this head and going into a subagent *is* going to that
    /// session — the operator, having driven into one and then been unable to get out
    /// again: *"when I \"Enter\" Subagent it is like completely switching session"*,
    /// and *"so after o I couldnt just Esc from the subagent — had to switch back here
    /// via session. Which narrows the subagent prompt - make \"o\" to \"Enter\""*.
    ///
    /// `o` stays as an alias for the same act: it is the key this pane has always used
    /// to move the head, and a hand that learned it must not have to learn something
    /// new. What moved is the READ — `p` now, and `p` is `/peek ID`'s own key. Reading
    /// a child's output without leaving the session is a real thing to want (R20's whole
    /// argument for the `Peek` frame), and it must not be the thing Enter does when the
    /// operator means to go there. It is neither Enter nor Esc, which is what the two
    /// gestures had to be kept apart from.
    ///
    /// **And the group row is the third thing Enter means here.** The finished children live
    /// under a fold (see [`App::subagent_stops`]); Enter on that row unfolds them, which is
    /// the same *Enter acts on what the cursor is on* rule the todos pane keeps. The arrows
    /// also scroll the cursor into view now — the fix for the operator's other report,
    /// *"subagents panel doesnt scroll"*, which was a child appended below the fold and no
    /// key that would bring it up.
    pub(crate) fn key_subagents_pane(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        {
            let stops = self.subagent_stops();
            if !stops.is_empty() {
                let n = stops.len();
                let at = self.subagents_sel.min(n - 1);
                match k {
                    Key::Up => {
                        self.subagents_sel = if at == 0 { n - 1 } else { at - 1 };
                        self.scroll_into_view(self.subagents_row_of());
                        self.redraw = true;
                        return ControlFlow::Break(None);
                    }
                    Key::Down => {
                        self.subagents_sel = (at + 1) % n;
                        self.scroll_into_view(self.subagents_row_of());
                        self.redraw = true;
                        return ControlFlow::Break(None);
                    }
                    // **Enter takes the row, and `o` is the same act as the alias this pane
                    // has always used.** One arm for one behaviour: Enter is unconditional (a
                    // pane owns Enter), and `o` keeps the composer's claim on a letter that is
                    // being typed — half a word on the line falls through to the composer, which
                    // is why the guard is here rather than in a second copy of these six lines.
                    Key::Enter | Key::Char('o')
                        if matches!(k, Key::Enter) || self.editor.text().is_empty() =>
                    {
                        match stops[at] {
                            // **The group row is a fold, not a session**: nobody to switch into,
                            // so Enter is the unfold.
                            SubStop::Finished => {
                                self.subagents_finished_open = !self.subagents_finished_open;
                                self.redraw = true;
                                return ControlFlow::Break(None);
                            }
                            SubStop::Agent(i) => {
                                // **Moving, not reading.** The row is a session and Enter goes to
                                // it; one that is not open yet is refused here, by name, rather
                                // than bounced off the daemon.
                                let row = &self.subagents[i];
                                if row.state == "opening" {
                                    // Nothing to attach to yet, and the daemon would refuse the
                                    // switch by name anyway; saying it here keeps the operator in
                                    // the pane they were using rather than bouncing them through
                                    // a rejection.
                                    self.say(
                                        "that subagent is still opening — nothing to attach to yet",
                                    );
                                    self.redraw = true;
                                    return ControlFlow::Break(None);
                                }
                                let id = row.session_id.clone();
                                self.close_surface(Surface::Subagents);
                                return ControlFlow::Break(self.switch_to(id));
                            }
                        }
                    }
                    // **Reading, not moving: the output pane opens on the `Peeked` reply and
                    // this head never leaves the session it is in.** Nothing to read under the
                    // fold header, so `p` on it falls through.
                    Key::Char('p') if self.editor.text().is_empty() => match stops[at] {
                        SubStop::Finished => {}
                        SubStop::Agent(i) => {
                            let row = &self.subagents[i];
                            if row.state == "opening" {
                                self.say("that subagent is still opening — nothing to read yet");
                                self.redraw = true;
                                return ControlFlow::Break(None);
                            }
                            let id = row.session_id.clone();
                            self.sub_out_pending = Some(id.clone());
                            return ControlFlow::Break(Some(Action::Peek(id)));
                        }
                    },
                    _ => {}
                }
            }
        }
        ControlFlow::Continue(())
    }

    /// **An open todos pane owns Up and Down, Enter reads the row whole, and Tab acts on it.**
    ///
    /// The items carry the detail a TODO.md puts under them — the commit a
    /// vendoring pins, the `Deps:` that says what blocks it — and the pane
    /// showed the first line only, so an item trailed off mid-sentence. Arrows
    /// move and Enter opens the row under the cursor: the same two the jobs and
    /// subagent panes use.
    /// **ONE ENUMERATION, AND EVERY KEY READS IT** — leticl's `todos-stops`, whose docstring is
    /// the operator's two reports: *"arrows dont go here"* and *"mouse doesnt click"*. Both were
    /// the same defect, a cursor whose position came from one list and whose row came from
    /// another. The stops are the add control, the session's rows (the model's and the operator's
    /// alike) and the repo's items.
    ///
    /// # Which key wins, and why
    ///
    /// **Enter READS the row and Tab ACTS on it, and that is a change to what Enter meant here.**
    /// Every other pane in this head spends Enter on *open the thing under the cursor* — the queue
    /// entry, a job's output, a standing note — and this pane was the one place it meant *toggle a
    /// row done* or *unfold an item*. The operator's report is why the read wins: *"in todo list i
    /// cant expand your todo items - only truncated titles available"*, against rows the pane
    /// draws to its own width with nothing behind them ([`App::todos_open`]).
    ///
    /// **The acts are not lost, they are on Tab** — which already carried every one of them
    /// (`enter or tab …` was the old bar's own sentence), so nothing becomes unreachable and no
    /// second key has to be learned. The exception is the ADD CONTROL: **the card wins there**,
    /// because that row has no text of its own to read — it is a control, and its act is what it
    /// is for.
    ///
    /// **A key this block does not name FALLS THROUGH**, which is what keeps the pane from
    /// eating the composer: `Tab` is the completion key while a `/command` is half-typed (the
    /// guard below is the same empty-composer one the card uses), and every ordinary character
    /// is the operator's to type. The first cut of this block ended in an unconditional
    /// `return None` and the pane swallowed the whole keyboard — a `▸` that looked right with
    /// nothing behind it, which is a worse defect than the one it replaced.
    ///
    /// **And the read carries the same empty-composer guard the acts do**, because with a line
    /// typed Enter is the composer's in this pane — the contract `keys/mod.rs` records for the
    /// todos stops (*a typed line IS the answer there*) — and a read that took that Enter would
    /// silently swallow what the operator was writing.
    pub(crate) fn key_todos_pane(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        {
            let stops = self.todos_stops();
            let n = stops.len();
            let at = self.todos_sel.min(n.saturating_sub(1));
            match k {
                Key::Up => {
                    self.todos_sel = if at == 0 { n - 1 } else { at - 1 };
                    self.todos_sel = self.todos_sel.min(n - 1);
                    self.sync_repo_from_stop(&stops);
                    self.scroll_into_view(self.todos_row_of());
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                Key::Down => {
                    self.todos_sel = (at + 1) % n;
                    self.sync_repo_from_stop(&stops);
                    self.scroll_into_view(self.todos_row_of());
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                // **Enter READS the row under the cursor** — the whole of it, in an overlay, which
                // is the one place a row longer than the pane can be read at all. The add control
                // is the exception, and it is not a special case so much as the row's own nature:
                // there is no text there to open, and its act is opening the card.
                Key::Enter if self.editor.text().is_empty() => match &stops[at] {
                    TodoStop::Add => {
                        self.open_todo_card();
                        return ControlFlow::Break(None);
                    }
                    row => {
                        self.todos_open = Some(row.clone());
                        self.push_surface(Surface::TodoRow);
                        return ControlFlow::Break(None);
                    }
                },
                // **Tab is the pane's own acts**, which is what it was beside Enter before the read
                // took Enter: the card on the control, the toggle on one of the operator's rows,
                // the unfold on a repo item. On the model's rows there is no act to perform — the
                // R44 boundary, said rather than left as a key that does nothing.
                Key::Tab if self.editor.text().is_empty() => match &stops[at] {
                    TodoStop::Add => {
                        self.open_todo_card();
                        return ControlFlow::Break(None);
                    }
                    // **Marking done is the model's act too and it is the operator's own row**:
                    // the daemon's `set_operator_states` is its own door and `/todo done N` is the
                    // typed one. Here it is the same act under the cursor. A completed row can be
                    // reopened, because a cursor that can only go one way is a cursor you cannot
                    // correct.
                    TodoStop::Mine(content) => {
                        let content = content.clone();
                        let mut mine = self.operator_todos();
                        if let Some(t) = mine.iter_mut().find(|t| t.content == content) {
                            t.status =
                                if t.status == letibot_sessionlog::event::TodoStatus::Completed {
                                    letibot_sessionlog::event::TodoStatus::Pending
                                } else {
                                    letibot_sessionlog::event::TodoStatus::Completed
                                };
                        }
                        self.say("toggled");
                        self.echo_operator_todos(mine.clone());
                        return ControlFlow::Break(Some(Action::SetOperatorTodos {
                            items: mine,
                            moved: Vec::new(),
                        }));
                    }
                    TodoStop::Repo(i) => {
                        self.repo_sel = *i;
                        self.repo_open = !self.repo_open;
                        self.scroll_into_view(self.todos_row_of());
                        self.redraw = true;
                        return ControlFlow::Break(None);
                    }
                    // **Said, not silently nothing** — the rule this pane's own bar keeps about a
                    // key that names the wrong act: the model moves its own row's status and the
                    // operator may not, and a Tab that did nothing here would read as a pane that
                    // had stopped answering.
                    TodoStop::Theirs(_) => {
                        self.say(
                            "that row is the model's — nothing here moves it; enter reads it whole",
                        );
                        self.redraw = true;
                        return ControlFlow::Break(None);
                    }
                },
                _ => {}
            }
        }
        ControlFlow::Continue(())
    }

    /// **The row the todos pane's Enter opened owns the keyboard while it is up** — Esc goes back
    /// to the LIST, which is still behind it, the arrows walk the window, and nothing else reaches
    /// the composer. The queue entry's overlay, one pane along, and it sits in the same place that
    /// one does: above the block that closes every pane on Esc, and below the chords.
    ///
    /// # Why the keys are split between here and the chords
    ///
    /// **The page keys and the wheel are `key_chords`'s**, not this handler's: they are settled for
    /// every open pane there (`self.todos_pane` is true whenever this overlay is — it is only ever
    /// opened from that pane, and both doors that close the pane clear the overlay), and they move
    /// `pane_scroll`, which is the field this overlay is drawn through. An arm here for the same
    /// keys would be dead code that looks live, and a second spelling of one rule is how the two
    /// come to disagree. The ARROWS are here because no other block owns them for a pane: the
    /// pane's own handler is below this one, and it would move the cursor of a list nobody can see.
    ///
    /// # Why a key it does not name does not fall through
    ///
    /// A reading surface with no field on it has no meaning for a key it does not name, and the
    /// alternative is a draft growing in the composer under the row being read. **The chords above
    /// still work** — `ctrl-t` closes the pane, the visibility chords still switch — because this
    /// handler is below them; what it takes is what nothing else wanted.
    pub(crate) fn key_todo_open(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if self.todos_open.is_some() {
            // Esc and ctrl-c are the stack's: `App::key_surface` pops this row and the list is
            // the next entry down.
            if matches!(k, Key::Esc | Key::CtrlC) {
                return ControlFlow::Continue(());
            }
            if matches!(k, Key::Up | Key::Down) {
                self.pane_scroll = if matches!(k, Key::Up) {
                    self.pane_scroll.saturating_sub(1)
                } else {
                    self.pane_scroll + 1
                };
                // Clamped against the last draw's own numbers: the key handler has no width and
                // no height, and a scroll clamped against a guess walks past the end.
                let max = self.pane_len.saturating_sub(self.pane_room);
                self.pane_scroll = self.pane_scroll.min(max);
                self.redraw = true;
                return ControlFlow::Break(None);
            }
            return ControlFlow::Break(None);
        }
        ControlFlow::Continue(())
    }

    /// **An open standing-notes pane owns Up and Down, and Enter opens the note it is on in the
    /// editor pane.**
    ///
    /// The rows are ONE list ([`App::standing`], in the order the section carries them), so the
    /// drawn cursor, the arrows and Enter cannot disagree — and the arrows scroll the cursor
    /// into view, so a note below the fold is reachable.
    ///
    /// **Enter goes through the editor's own door, not a reader of this head's.**
    /// [`App::open_note`] builds a [`FileRef`](super::super::editor::FileRef) for the row and
    /// hands it to `App::open_in_editor`, which is what a click on an edit row and `ctrl-]`
    /// already do — so a note opens in the same rano, with the same keys, the same `alt-s` send
    /// and the same way out, and the whole file is what the reader gets (`indexed` notes are
    /// carried by the prompt as headings and line ranges alone). A file that is gone is said by
    /// that door's own sentence and nothing is opened; see [`App::open_note`].
    ///
    /// **And not while the editor pane is drawn over it.** Opening a note takes this list's
    /// rectangle — rano is what the reader is looking at — so the arrows must not go on moving
    /// a cursor nobody can see: they are the editor's while it has the keyboard, and the
    /// composer's once `ctrl-]` crosses. Leaving rano puts the list back with its cursor where
    /// it was, which is the round trip the key is for.
    pub(crate) fn key_standing_pane(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        {
            if !self.standing.is_empty() {
                let n = self.standing.len();
                let at = self.standing_sel.min(n - 1);
                match k {
                    Key::Up => {
                        self.standing_sel = if at == 0 { n - 1 } else { at - 1 };
                        self.scroll_into_view(self.standing_row_of());
                        self.redraw = true;
                        return ControlFlow::Break(None);
                    }
                    Key::Down => {
                        self.standing_sel = (at + 1) % n;
                        self.scroll_into_view(self.standing_row_of());
                        self.redraw = true;
                        return ControlFlow::Break(None);
                    }
                    Key::Enter => {
                        self.open_note();
                        return ControlFlow::Break(None);
                    }
                    _ => {}
                }
            }
        }
        ControlFlow::Continue(())
    }

    /// **An open queue pane owns Up and Down, and Enter opens the row it is on.** The rows
    /// are ONE enumeration (`merge`, in the daemon's own order), so the drawn cursor, the
    /// arrows and Enter cannot disagree.
    ///
    /// **And `tab` owns the VIEW.** The two halves of one queue are two views of the same list
    /// (`ui::panes::queue::QueueView`): the list, its order and its cursor are one, and only the
    /// row under each entry changes. So the switch moves nothing a reader has to find again —
    /// which is why it is a key on the pane rather than a second pane.
    pub(crate) fn key_queue_pane(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        {
            // **`tab` moves between the two views**, and the strip the pane draws is what says
            // so (`ui::panes::queue::tab_row`). It is deliberately not behind the entries: the
            // strip is drawn for an empty queue too — the pane still says which half you are
            // looking at — and a key that stopped working when the queue emptied would be a key
            // that failed exactly where the pane says `none`.
            if matches!(k, Key::Tab) {
                self.queue_view = self.queue_view.other();
                self.redraw = true;
                return ControlFlow::Break(None);
            }
            if !self.merge.is_empty() {
                let n = self.merge.len();
                let at = self.queue_sel.min(n - 1);
                match k {
                    Key::Up => {
                        self.queue_sel = if at == 0 { n - 1 } else { at - 1 };
                        self.scroll_into_view(self.queue_row_of());
                        self.redraw = true;
                        return ControlFlow::Break(None);
                    }
                    Key::Down => {
                        self.queue_sel = (at + 1) % n;
                        self.scroll_into_view(self.queue_row_of());
                        self.redraw = true;
                        return ControlFlow::Break(None);
                    }
                    Key::Enter => {
                        // **What Enter opens is the ENTRY, and everything the queue knows
                        // about it**: the ask it was produced under, the state and its reason
                        // — which is the gate's own words when the gate refused it, and the
                        // reviewer's verdict when the reviewer did — and the verdict's
                        // evidence. Nothing here is a second read: the row carries the
                        // evidence and the snapshot carries the verdict, so the overlay is
                        // the two rows the pane already holds, in full.
                        self.queue_open = Some(self.merge[at].id.clone());
                        self.push_surface(Surface::QueueEntry);
                        return ControlFlow::Break(None);
                    }
                    // **`r` re-attempts a parked entry's review** — the operator's ask, in
                    // their words: *"so merge queue has 4 failed items, we need a way to
                    // restart them"*. It is the pane's own act on the row under the cursor,
                    // and it sends the SAME verb `/queue restart ID` sends (`Action::Slash`),
                    // so the key and the typed spelling cannot drift.
                    //
                    // **An entry that is not parked is said, not sent.** `waiting` and `taken`
                    // are the queue working and `landed` is done; a restart there would be a
                    // frame the daemon refuses, and a head that sent it anyway would be a head
                    // that made the operator wait for a round trip to be told no.
                    //
                    // **Only on an empty composer**, the guard the subagents pane's `o`/`p` carry
                    // and every plain letter a pane binds must: the letters are the operator's to
                    // type first. Without it, typing "deploy" with `/queue` open sent `queue rm`
                    // on its `d` and `queue approve` on its `a` against whatever row the cursor
                    // was on (R67 item 1) — every letter was eaten and two of them acted.
                    Key::Char('r') if self.editor.text().is_empty() => {
                        let e = &self.merge[at];
                        // **The same predicate the review view's verb row draws**
                        // (`ui::panes::queue::verb_moves`): a row that says what `r` would do and
                        // a key that does something else is the drift one predicate prevents.
                        if !crate::ui::panes::queue::verb_moves(
                            crate::ui::panes::queue::Verb::Restart,
                            e.state,
                        ) {
                            self.say(&format!(
                                "`{}` is `{}` — only a parked entry (failed, conflict, stale, \
                                 vetoed) is restarted by hand; a waiting one is what the queue's \
                                 own retry is for.",
                                e.branch,
                                crate::ui::panes::queue::merge_state_word(e.state)
                            ));
                            self.redraw = true;
                            return ControlFlow::Break(None);
                        }
                        let line = format!("queue restart {}", e.id);
                        return ControlFlow::Break(Some(Action::Slash { line }));
                    }
                    // **`a`, `v` and `d` are the person's three verbs** — the operator's ask, in
                    // their words: *"i want to be able to approve / veto / delete"*. Each sends
                    // the SAME verb its typed spelling sends (`Action::Slash`), so the key and
                    // `/queue approve|veto|rm ID` cannot drift.
                    //
                    // **A row the daemon would refuse is said, not sent** — `r`'s own rule, one
                    // state over: `taken` is the queue mid-merge and `landed` is merged, and both
                    // are refusals the head already knows, so making the operator wait for a
                    // round trip to be told no would be a round trip wasted. Everything else —
                    // waiting, the three machine-parked states and a person's own `vetoed` — is
                    // a row a person may answer, and what the DAEMON says about it (a live
                    // review attempt, a verdict that already accepts) is the daemon's to say.
                    //
                    // Guarded on an empty composer, `r`'s own guard above and for the same reason.
                    Key::Char('a') | Key::Char('v') | Key::Char('d')
                        if self.editor.text().is_empty() =>
                    {
                        use crate::ui::panes::queue::{Verb, verb_moves};
                        let e = &self.merge[at];
                        // **The verb's own letter and word**, so the key that was pressed and
                        // the verb it sends come from one entry ([`Verb`]) rather than from a
                        // `match` here and a `format!` below that can drift apart.
                        let verb = match k {
                            Key::Char('a') => Verb::Approve,
                            Key::Char('v') => Verb::Veto,
                            _ => Verb::Rm,
                        };
                        if !verb_moves(verb, e.state) {
                            self.say(&format!(
                                "`{}` is `{}` — a person's verb moves an entry that is waiting or \
                                 parked, never one the queue has taken (it is mid-merge) or landed \
                                 (it is merged). Nothing was sent.",
                                e.branch,
                                crate::ui::panes::queue::merge_state_word(e.state)
                            ));
                            self.redraw = true;
                            return ControlFlow::Break(None);
                        }
                        let line = format!("queue {} {}", verb.word(), e.id);
                        return ControlFlow::Break(Some(Action::Slash { line }));
                    }
                    _ => {}
                }
            }
        }
        ControlFlow::Continue(())
    }

    /// **An open jobs pane owns Up and Down, and Enter reads the row it is on — or folds the
    /// group.** The same shape the subagents pane keeps, and for the same reason the
    /// operator gave: *"jobs panel - same as subagents - show list of running, group
    /// finished"*. The rows are ONE enumeration ([`App::job_stops`]), so the drawn cursor,
    /// the arrows and Enter cannot disagree — and the arrows scroll the cursor into view, so
    /// a job below the fold is reachable.
    pub(crate) fn key_jobs_pane(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        {
            let stops = self.job_stops();
            if !stops.is_empty() {
                let n = stops.len();
                let at = self.jobs_sel.min(n - 1);
                match k {
                    Key::Up => {
                        self.jobs_sel = if at == 0 { n - 1 } else { at - 1 };
                        self.scroll_into_view(self.jobs_row_of());
                        self.redraw = true;
                        return ControlFlow::Break(None);
                    }
                    Key::Down => {
                        self.jobs_sel = (at + 1) % n;
                        self.scroll_into_view(self.jobs_row_of());
                        self.redraw = true;
                        return ControlFlow::Break(None);
                    }
                    Key::Enter => match stops[at] {
                        // **The group row is a fold, not a job**: nobody to read.
                        JobStop::Finished => {
                            self.jobs_finished_open = !self.jobs_finished_open;
                            self.redraw = true;
                            return ControlFlow::Break(None);
                        }
                        JobStop::Job(i) => {
                            if self.session_id.is_empty() {
                                self.say("not attached to a session yet");
                                self.redraw = true;
                                return ControlFlow::Break(None);
                            }
                            let row = &self.jobs[i];
                            let job = row.id.clone();
                            // **Where its output went, when it did not come here** (R41). The
                            // window for a redirected job is empty by construction, so the
                            // pane needs the file's name to say anything true at all — see
                            // [`JobOut::redirect`].
                            let redirect = row.redirect.clone();
                            // **The output opens in a pane, not in the conversation.**
                            // This used to return `Action::Slash { "job {job}" }`, whose
                            // reply is a `Warning` on the session log — so the pane closed
                            // and the operator read a build log scrolling past in the chat.
                            // The operator, 2026-09-20: *"when i press enter on jobs pane im
                            // not shown the job output im brought back to the main
                            // conversation with /job <id> posted - this is not what i
                            // want"*. Now the read is a `ReadJobOutput`: it comes back as a
                            // `JobOutput` event with the offsets attached, and the overlay
                            // draws it. The jobs list stays behind it, so Esc returns here.
                            self.job_out = Some(JobOut {
                                job: job.clone(),
                                state: String::new(),
                                never_ran: false,
                                redirect,
                                from: 0,
                                to: 0,
                                produced: 0,
                                dropped: 0,
                                lines: Vec::new(),
                                next: None,
                                back: Vec::new(),
                                scroll: 0,
                                loading: true,
                                error: None,
                            });
                            self.push_surface(Surface::JobOut);
                            return ControlFlow::Break(Some(Action::ReadJobOutput {
                                job,
                                offset: 0,
                            }));
                        }
                    },
                    _ => {}
                }
            }
        }
        ControlFlow::Continue(())
    }
}
