//! **The surface stack**: which screen holds the conversation's rectangle, and therefore which one
//! the keys, the wheel and the hint bar are about (R69).
//!
//! # What this replaces
//!
//! Each pane was its own `bool` — `help`, `stats`, `picker`, `todos_pane`, `config_pane`,
//! `queue_pane`, `standing_pane`, `subagents_pane`, `jobs_pane` — and every toggle flipped only
//! its own. Three hand-written orders then decided what that meant: the draw order in
//! `main_area` (todos > queue > standing > subagents > jobs), the key order in `App::key`
//! (subagents > todos > queue > jobs > standing) and the hint bar's. They disagreed, and the
//! review of 2026-10-11 measured what that costs: `ctrl-t` then `ctrl-g` drew the todos pane
//! while ↑↓ and Enter went to the hidden subagents list, and **Enter switched session into a
//! row nobody saw**. One shared `pane_scroll` made `scroll_into_view` jump the visible pane
//! for a cursor moving in a hidden one. The operator's summary of the class: *"mouse clicks -
//! sometimes they work, sometimes they dont, per pane key maps etc etc."*
//!
//! # The rule
//!
//! **One stack, and its top is the surface.** Opening pushes (or brings an open one to the
//! top), Esc pops, and [`App::drawn`] is the single answer that `main_area` draws, `App::key`
//! routes and `hint_bar` describes. A surface that is not on top is not drawn and receives
//! nothing — its cursor and its scroll wait for it, kept per entry, so coming back to it lands
//! where the reader left it.
//!
//! The overlays a pane opens over itself — a todo row read whole, a queue entry, a subagent's
//! output, a job's output — are surfaces on the same stack with the pane as their
//! [`Surface::parent`]: Esc goes back to the list because the list is the next entry down, not
//! because a handler was placed above another one in a long function.

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
    /// **A plugin's pane** — the dashboard a `pane_<name>` hook answered, open by `/pane NAME`
    /// or by a row of `/pane`'s list. Which pane it shows is `App::plugin_open`: the surface
    /// says where on the screen, the data says what, the split `Surface::Listing` makes with
    /// `slash_out`.
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

    /// **Does this surface take Enter even when it has nothing to do with it.**
    ///
    /// The operator, 2026-10-03, on a stray character and Enter in the jobs pane: *"the pane
    /// own keyboard in a way, so enter is a pane thing."* Every surface owns Enter except the
    /// three where a TYPED LINE is the answer — the session picker (a number, an id prefix),
    /// the todos list (the stops' typed path) — and `submit` routes the words to them. It used
    /// to be a hand-kept list in `App::key`, and `queue_pane` was missing from it, so Enter
    /// with typed text SENT the prompt over an empty queue and opened an entry over a
    /// non-empty one (R69).
    pub(crate) fn owns_enter(self) -> bool {
        !matches!(self, Surface::Picker | Surface::Todos)
    }

    /// Every surface, for the tests and the generated help that walk them.
    pub(crate) const ALL: [Surface; 17] = [
        Surface::Help,
        Surface::Stats,
        Surface::Picker,
        Surface::Config,
        Surface::Todos,
        Surface::TodoRow,
        Surface::Queue,
        Surface::QueueEntry,
        Surface::Standing,
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

/// **What the conversation's rectangle holds this frame** — the one value `main_area` draws,
/// `App::key` routes and the hint bar describes.
///
/// The three layers above the stack are not on it because they are not opened and closed the
/// way a pane is: a `!term` pane takes the raw bytes (`Link::tick`), the diff popup is closed
/// by its own keys, and rano has its own focus. What matters is that they are answered HERE,
/// in the same order the frame draws them, so a pane under rano is a pane that is not drawn —
/// and therefore one whose Enter is not swallowed after `ctrl-]` comes back to the composer.
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

/// **One entry on the stack**: the surface, and the scroll it had when something covered it.
///
/// The live scroll of the TOP is `App::pane_scroll`, which every pane draws through
/// (`pane_window`) and every key moves; an entry's `scroll` is only written when it stops
/// being the top and read back when it becomes the top again. That is what ends the shared
/// scroll jumping the visible pane: a hidden pane's offset is in its own entry, not in the
/// field the visible one draws from.
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

    /// **Put `s` on top**: pushed, or brought forward when it is already open. It opens at its
    /// head (`pane_scroll` 0), which is what every opener did by hand before; the surface it
    /// covers keeps its own scroll in its entry.
    ///
    /// This is the stack's half of opening. The surface's own half — asking the daemon for its
    /// rows, seeding a cursor — is [`App::open_surface`], which every chord, verb and click
    /// calls; this is what it and the overlays' openers share.
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
            // **A plugin's pane closes the pane's own state with it**: which of them was open,
            // which of its screens, and the row the detail was read from. The views themselves
            // stay — they are the daemon's answers about the session, not about this screen —
            // so reopening a pane draws what is already known while the new look travels.
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

    /// **The one door a surface is opened through** — chords, slash verbs, a click on a count
    /// label and the Esc that climbs out of a subagent all call this, so two spellings of
    /// "open the jobs pane" cannot come to do two different things (the drift R67 item 8 found
    /// between `/sessions` and `ctrl-s`).
    ///
    /// Returns what the daemon has to be asked for the surface to have rows: the jobs table,
    /// the todo list, the standing notes, the queue, the settings, the session list.
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

    /// **A chord's or a verb's toggle**: the surface in front closes (and so does a pane whose own
    /// overlay is in front — `ctrl-t` over a todo row read whole closes the pane, as it always
    /// did); one that is open behind another, or not open at all, comes to the front. Bringing a covered pane forward rather
    /// than closing it is the stack's answer to `ctrl-t`, `ctrl-g`, `ctrl-t`: the third press
    /// shows the todos pane, it does not close a pane the operator cannot see.
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
