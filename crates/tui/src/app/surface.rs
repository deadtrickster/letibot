//! The surface stack: which screen holds the conversation's rectangle, and therefore which one the
//! keys, the wheel and the hint bar are about.
//!
//! One stack, and its top is the surface. Opening pushes (or brings an open one to the top), Esc
//! pops, and [`App::drawn`] is the single answer that `main_area` draws, `App::key` routes and
//! `hint_bar` describes - so a surface that is not on top is not drawn and receives nothing, which
//! is what three hand-written orders used to disagree about. An overlay a pane opens is on the same
//! stack with the pane as its [`Surface::parent`], so Esc goes back one entry, not one handler.

use super::*;

/// **One thing that can hold the conversation's rectangle.** The data a surface shows lives
/// where it always did (`sub_out`, `job_out`, `slash_out`, `todos_open`, `queue_open` for the
/// overlays that carry some); the stack says which one is in front.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Surface {
    /// `/help`.
    Help,
    /// `/status`.
    Stats,
    /// The session picker — `ctrl-s`, `/sessions`.
    Picker,
    /// `/config`.
    Config,
    /// The todos pane — `ctrl-t`, `/todos`.
    Todos,
    /// One todo row read whole, opened by the todos pane's Enter.
    TodoRow,
    /// The merge queue — `/queue`.
    Queue,
    /// One queue entry, opened by the queue pane's Enter.
    QueueEntry,
    /// The standing notes — `/standing`.
    Standing,
    /// **The skills this box has** — `/skills`. One row each: the name, where it was found and
    /// its one line; Enter hands the body to the session.
    Skills,
    /// The subagent tree — `ctrl-g`, `/subagents`.
    Subagents,
    /// One subagent's output — the tree's `p`, `/peek ID`.
    SubOut,
    /// The background jobs — `ctrl-q`, `/jobs`.
    Jobs,
    /// One job's output, opened by the jobs pane's Enter.
    JobOut,
    /// The monitors the model armed, live and ended — `/monitors`, or a click on the edge's
    /// `N monitors watching`.
    Monitors,
    /// A plugin's pane - the dashboard a `pane_<name>` hook answered, open by `/pane NAME` or by a
    /// row of `/pane`'s list. The surface says where on the screen; which pane it shows is
    /// `App::plugin_open`, and the data is `slash_out`.
    PluginPane,
    /// **One of that pane's stops read whole** — its Enter. An overlay like every other, so Esc
    /// gives the pane back rather than reopening a screen the reader was done with.
    PluginDetail,
    /// A slash command's listing (`/notes`, a diagnostic, a long warning).
    Listing,
}

impl Surface {
    /// **The pane an overlay was opened from.** Closing the pane takes the overlay with it —
    /// a row left standing over the list it belonged to, after the list was closed by some
    /// other door, is a screen whose Esc goes nowhere the reader expects.
    pub(crate) fn parent(self) -> Option<Surface> {
        match self {
            Surface::TodoRow => Some(Surface::Todos),
            Surface::QueueEntry => Some(Surface::Queue),
            Surface::SubOut => Some(Surface::Subagents),
            Surface::JobOut => Some(Surface::Jobs),
            Surface::PluginDetail => Some(Surface::PluginPane),
            _ => None,
        }
    }

    /// Does this surface take Enter even when the Enter has nothing to do with it. Every surface
    /// owns Enter except the two where a TYPED LINE is the answer - the session picker and the
    /// todos list - and `submit` routes the words to those, so a stray line cannot be sent instead.
    pub(crate) fn owns_enter(self) -> bool {
        !matches!(self, Surface::Picker | Surface::Todos)
    }

    /// Every surface, for the tests and the generated help that walk them.
    pub(crate) const ALL: [Surface; 18] = [
        Surface::Help,
        Surface::Stats,
        Surface::Picker,
        Surface::Config,
        Surface::Todos,
        Surface::TodoRow,
        Surface::Queue,
        Surface::QueueEntry,
        Surface::Standing,
        Surface::Skills,
        Surface::Subagents,
        Surface::SubOut,
        Surface::Jobs,
        Surface::JobOut,
        Surface::Monitors,
        Surface::PluginPane,
        Surface::PluginDetail,
        Surface::Listing,
    ];
}

/// What the conversation's rectangle holds this frame - the one value `main_area` draws, `App::key`
/// routes and the hint bar describes. The three layers above the stack are answered HERE, in the
/// order the frame draws them, so a pane under rano is not drawn and its Enter is not swallowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Drawn {
    /// A `!term` program, drawn and holding the keyboard.
    Term,
    /// A change opened by a click on an edit row.
    DiffPopup,
    /// The editor pane (rano), visible.
    Editor,
    /// The top of the surface stack.
    Surface(Surface),
    /// Nothing over the conversation.
    Conversation,
}

/// One entry on the stack: the surface, and the scroll it had when something covered it. The live
/// scroll of the top is `App::pane_scroll`; an entry's own is written when it stops being the top
/// and read back when it returns, so a hidden pane's offset cannot move the visible one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Open {
    pub(crate) surface: Surface,
    pub(crate) scroll: usize,
}

impl App {
    /// **What the conversation's rectangle holds**, in the order the frame draws it. See
    /// [`Drawn`].
    pub(crate) fn drawn(&self) -> Drawn {
        if self.pane_open() {
            Drawn::Term
        } else if self.diff_popup.is_some() {
            Drawn::DiffPopup
        } else if self.edit_pane.as_ref().is_some_and(|p| !p.hidden) {
            Drawn::Editor
        } else if let Some(s) = self.top_surface() {
            Drawn::Surface(s)
        } else {
            Drawn::Conversation
        }
    }

    /// The surface on top of the stack — drawn unless a layer above the stack covers it.
    pub(crate) fn top_surface(&self) -> Option<Surface> {
        self.surfaces.last().map(|o| o.surface)
    }

    /// **The surface that is drawn and has the keys**, or nothing — the stack's top, unless a
    /// `!term` pane, the diff popup or rano is in front of it.
    pub(crate) fn focused_surface(&self) -> Option<Surface> {
        match self.drawn() {
            Drawn::Surface(s) => Some(s),
            _ => None,
        }
    }

    /// Whether `s` is anywhere on the stack — open, drawn or not.
    pub(crate) fn is_open(&self, s: Surface) -> bool {
        self.surfaces.iter().any(|o| o.surface == s)
    }

    /// Put `s` on top: pushed, or brought forward when it is already open, at its head. The surface
    /// it covers keeps its scroll in its entry. This is the stack's half of opening; the surface's
    /// own half - asking the daemon for rows, seeding a cursor - is [`App::open_surface`].
    pub(crate) fn push_surface(&mut self, s: Surface) {
        if let Some(top) = self.surfaces.last_mut() {
            top.scroll = self.pane_scroll;
        }
        // Brought forward, it comes without the overlays it had: they were opened over the
        // pane as it was then, and one left deeper in the stack would come back on an Esc
        // the reader meant for something else.
        self.surfaces
            .retain(|o| o.surface != s && o.surface.parent() != Some(s));
        self.surfaces.push(Open {
            surface: s,
            scroll: 0,
        });
        self.pane_scroll = 0;
        self.redraw = true;
        // **The session list's own failure, said where a person will see it.** Opening the list is
        // the moment their broken layer matters (`app::sessions_pane`).
        if s == Surface::Picker
            && let Some(said) = self.take_sessions_failure()
        {
            self.say(&said);
        }
    }

    /// **Take `s` off the stack, and every overlay opened from it.** The data an overlay
    /// carries goes with it, so an `Option` that is `Some` is always a surface that is on the
    /// stack. When the top changes, the new top's scroll comes back from its entry.
    pub(crate) fn close_surface(&mut self, s: Surface) {
        if !self.is_open(s) {
            return;
        }
        let was_top = self.top_surface();
        let gone: Vec<Surface> = self
            .surfaces
            .iter()
            .map(|o| o.surface)
            .filter(|t| *t == s || t.parent() == Some(s))
            .collect();
        self.surfaces.retain(|o| !gone.contains(&o.surface));
        for g in gone {
            self.forget_surface(g);
        }
        if self.top_surface() != was_top {
            self.pane_scroll = self.surfaces.last().map_or(0, |o| o.scroll);
        }
        self.redraw = true;
    }

    /// **Pop the top** — Esc's act on a surface. Returns what was closed.
    pub(crate) fn pop_surface(&mut self) -> Option<Surface> {
        let top = self.top_surface()?;
        self.close_surface(top);
        Some(top)
    }

    /// **Every surface at once** — for a test that sets one pane up after another.
    #[cfg(test)]
    pub(crate) fn close_all_surfaces(&mut self) {
        while self.pop_surface().is_some() {}
    }

    /// **Ask for a subagent's output**, remembering which surface asked (R90.17).
    pub(crate) fn ask_peek(&mut self, id: &str) {
        self.sub_out_pending = Some(id.to_string());
        self.sub_out_from = self.focused_surface();
    }

    /// The data a closed surface leaves behind, dropped with it.
    fn forget_surface(&mut self, s: Surface) {
        if self.sub_out_from == Some(s) {
            self.sub_out_pending = None;
        }
        match s {
            Surface::TodoRow => self.todos_open = None,
            Surface::QueueEntry => self.queue_open = None,
            Surface::SubOut => self.sub_out = None,
            Surface::JobOut => self.job_out = None,
            Surface::Listing => self.slash_out = None,
            Surface::Subagents => self.sub_out_pending = None,
            Surface::Standing => self.standing_corpus = None,
            // The rows and the cursor stay: a skill written since is a row at the next open,
            // and the reader lands on the one they left.
            Surface::Skills => {}
            // A plugin's pane closes its own state with it: which pane was open, which of its
            // screens, and the row a detail was read from. The views stay - they are the daemon's
            // answers about the session, not about this screen - so reopening draws what is known.
            Surface::PluginPane => {
                self.plugin_open = None;
                self.plugin_screen = Default::default();
                self.plugin_sel = 0;
                self.plugin_detail = None;
            }
            Surface::PluginDetail => self.plugin_detail = None,
            _ => {}
        }
    }

    /// The one door a surface is opened through, so two spellings of "open the jobs pane" cannot
    /// come to do two different things. Returns what the daemon has to be asked for the surface to
    /// have rows: the jobs table, the todo list, the standing notes, the queue, the settings.
    pub(crate) fn open_surface(&mut self, s: Surface) -> Option<Action> {
        self.push_surface(s);
        match s {
            Surface::Picker => {
                // **The two pickers are never open together**: the setting card goes, so the
                // screen holds one list and the arrows mean one thing. The cursor starts on
                // the session this head is in, so Enter on an untouched list is a no-op.
                self.pick = None;
                self.picker_sel = self
                    .session_rows()
                    .iter()
                    .position(|r| {
                        r.idx
                            .is_some_and(|i| self.sessions[i].session_id == self.session_id)
                    })
                    .unwrap_or(0);
                Some(Action::ListSessions)
            }
            Surface::Config => {
                self.config_sel = 0;
                self.pick = None;
                // Opening asks the daemon for its settings; a pane drawn from the last answer
                // would show the mode the session had when this head attached.
                (!self.session_id.is_empty()).then_some(Action::Settings)
            }
            Surface::Todos => {
                // Read at open, and re-read on every draw the file has moved under — see
                // `refresh_repo_todos`.
                self.refresh_repo_todos();
                Some(Action::ListTodos)
            }
            Surface::Queue => Some(Action::ListMergeQueue),
            Surface::Standing => {
                // The watch starts again from whatever the next answer says.
                self.standing_corpus = None;
                Some(Action::ListNotes)
            }
            // The skills, read on the open: a skill written a minute ago is a row.
            Surface::Skills => Some(Action::ListSkills),
            Surface::Subagents => {
                self.fold_subagents();
                None
            }
            Surface::Jobs => Some(Action::ListJobs),
            // The monitors come on the job table's answer.
            Surface::Monitors => Some(Action::ListJobs),
            // **A plugin's pane is opened by NAME**, and the name is not this function's
            // argument: [`App::open_plugin`] asks the daemon for a fresh look and pushes this
            // surface itself. Nothing here could name which pane, so nothing here asks.
            Surface::PluginPane => None,
            Surface::Stats => {
                // **Opening the screen IS the acknowledgement** (R51 item 17) — and only on the
                // way in; see the `/status` verb.
                self.acknowledge_counters();
                None
            }
            Surface::Help
            | Surface::TodoRow
            | Surface::QueueEntry
            | Surface::SubOut
            | Surface::JobOut
            | Surface::Listing
            | Surface::PluginDetail => None,
        }
    }

    /// A chord's or a verb's toggle: the surface in front closes (and so does a pane whose own
    /// overlay is in front), while one open behind another, or not open at all, comes to the front -
    /// so `ctrl-t`, `ctrl-g`, `ctrl-t` shows the todos pane instead of closing one nobody can see.
    pub(crate) fn toggle_surface(&mut self, s: Surface) -> Option<Action> {
        let top = self.top_surface();
        if top == Some(s) || top.and_then(Surface::parent) == Some(s) {
            self.close_surface(s);
            None
        } else {
            self.open_surface(s)
        }
    }
}
