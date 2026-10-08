//! **Keys a pane owns**: the outputs (a subagent's, a job's, a slash command's), the config
//! pane, the overlays, the session picker, and the subagents, todos, queue and jobs panes.

use super::*;
use std::ops::ControlFlow;

impl App {
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
                Key::Esc | Key::CtrlC => {
                    self.sub_out = None;
                    self.redraw = true;
                    return ControlFlow::Break(None);
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
                Key::Esc | Key::CtrlC => {
                    self.job_out = None;
                    self.redraw = true;
                    return ControlFlow::Break(None);
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
                Key::Esc | Key::CtrlC => {
                    self.slash_out = None;
                    self.pane_scroll = 0;
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
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
        if self.config_pane {
            match k {
                Key::Up => {
                    let n = self.config_rows().len().max(1);
                    self.config_sel = if self.config_sel == 0 {
                        n - 1
                    } else {
                        self.config_sel - 1
                    };
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                Key::Down => {
                    let n = self.config_rows().len().max(1);
                    self.config_sel = (self.config_sel + 1) % n;
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
            if matches!(k, Key::Esc | Key::CtrlC) {
                self.queue_open = None;
                self.pane_scroll = 0;
                self.redraw = true;
                return ControlFlow::Break(None);
            }
            if matches!(k, Key::Up | Key::Down | Key::PageUp | Key::PageDown) {
                let page = self.pane_room.max(1);
                match k {
                    Key::Up => self.pane_scroll = self.pane_scroll.saturating_sub(1),
                    Key::Down => self.pane_scroll += 1,
                    Key::PageUp => self.pane_scroll = self.pane_scroll.saturating_sub(page),
                    _ => self.pane_scroll += page,
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

    pub(crate) fn key_overlays(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if (self.help
            || self.picker
            || self.pick.is_some()
            || self.stats
            || self.todos_pane
            || self.subagents_pane
            || self.jobs_pane
            || self.queue_pane
            || self.config_pane)
            && matches!(k, Key::Esc | Key::CtrlC)
        {
            self.help = false;
            self.picker = false;
            self.pick = None;
            self.quit_card = false;
            self.stats = false;
            self.todos_pane = false;
            self.subagents_pane = false;
            self.jobs_pane = false;
            self.queue_pane = false;
            self.config_pane = false;
            self.sub_out_pending = None;
            self.redraw = true;
            return ControlFlow::Break(None);
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
        if self.picker && !self.sessions.is_empty() {
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
                Key::Click { y, .. } => {
                    // The same arithmetic the screen did: the optional session
                    // header takes a row, then the picker's title and a blank,
                    // then the sessions. Only a row the last render actually
                    // drew is trusted — `picker_rows_drawn` knows where
                    // `truncate(room)` cut the list off, so a click into the
                    // blank space under a truncated list moves nothing.
                    let header_rows =
                        usize::from(self.screen_rows >= 6 && !self.session_id.is_empty());
                    let first = header_rows + 2;
                    let row = usize::from(*y).saturating_sub(first);
                    if row < self.picker_rows_drawn.saturating_sub(2) {
                        self.picker_sel = row.min(n - 1);
                        self.redraw = true;
                    }
                    return ControlFlow::Break(None);
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
        if self.subagents_pane {
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
                                self.subagents_pane = false;
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

    /// **An open todos pane owns Up and Down, and Enter unfolds the item.**
    ///
    /// The items carry the detail a TODO.md puts under them — the commit a
    /// vendoring pins, the `Deps:` that says what blocks it — and the pane
    /// showed the first line only, so an item trailed off mid-sentence. Arrows
    /// move, Enter acts: the same two the jobs and subagent panes use.
    /// **ONE ENUMERATION, AND EVERY KEY READS IT** — leticl's `todos-stops`, whose docstring is
    /// the operator's two reports: *"arrows dont go here"* and *"mouse doesnt click"*. Both were
    /// the same defect, a cursor whose position came from one list and whose row came from
    /// another. The stops are the add control, the operator's own items, and the repo's items —
    /// and NOT the model's rows, which no key acts on.
    ///
    /// **A key this block does not name FALLS THROUGH**, which is what keeps the pane from
    /// eating the composer: `Tab` is the completion key while a `/command` is half-typed (the
    /// guard below is the same empty-composer one the card uses), and every ordinary character
    /// is the operator's to type. The first cut of this block ended in an unconditional
    /// `return None` and the pane swallowed the whole keyboard — a `▸` that looked right with
    /// nothing behind it, which is a worse defect than the one it replaced.
    pub(crate) fn key_todos_pane(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if self.todos_pane {
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
                // **Enter acts on what the cursor is ON**, which is the whole point of one
                // enumeration: the add control opens the card, one of your rows is marked done,
                // and a repo item unfolds. Tab stays the repo's unfold, because the hand is
                // already there for it and the composer is empty here.
                // **A click moves the cursor, and Enter still does the act** — the same two
                // acts, kept two, that the pickers here already keep (*"select and confirm stay
                // two acts"*). Straight off the recorded rows, so the row the pointer is on is
                // the row the pane drew there.
                //
                // **A click into the blank space moves nothing**, and so does one on a row that
                // is not a stop at all — one of the model's rows, or a repo heading. There is no
                // key that would act on it, which is the same reason it is not a stop.
                Key::Click { y, .. } => {
                    if let Some(sel) = self.todo_stop_at_row(*y) {
                        self.todos_sel = sel;
                        self.sync_repo_from_stop(&stops);
                        self.redraw = true;
                    }
                    return ControlFlow::Break(None);
                }
                Key::Enter | Key::Tab if self.editor.text().is_empty() => match &stops[at] {
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
                        return ControlFlow::Break(Some(Action::SetOperatorTodos(mine)));
                    }
                    TodoStop::Repo(i) => {
                        self.repo_sel = *i;
                        self.repo_open = !self.repo_open;
                        self.scroll_into_view(self.todos_row_of());
                        self.redraw = true;
                        return ControlFlow::Break(None);
                    }
                },
                _ => {}
            }
        }
        ControlFlow::Continue(())
    }

    /// **An open queue pane owns Up and Down, and Enter opens the row it is on.** The rows
    /// are ONE enumeration (`merge`, in the daemon's own order), so the drawn cursor, the
    /// arrows and Enter cannot disagree.
    pub(crate) fn key_queue_pane(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if self.queue_pane && self.queue_open.is_none() {
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
                        self.pane_scroll = 0;
                        self.redraw = true;
                        return ControlFlow::Break(None);
                    }
                    // **A click moves the cursor, and Enter still opens the entry** — the same
                    // two acts the pickers here keep (*select and confirm stay two acts*),
                    // straight off the rows the pane recorded while drawing.
                    Key::Click { y, .. } => {
                        if let Some(sel) = self.queue_stop_at_row(*y) {
                            self.queue_sel = sel;
                            self.redraw = true;
                        }
                        return ControlFlow::Break(None);
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
        if self.jobs_pane {
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
                            self.redraw = true;
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
