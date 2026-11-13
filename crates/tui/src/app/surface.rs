//! The surface stack: which screen holds the conversation's rectangle, and so which one the keys,
//! the wheel and the hint bar are about. Opening pushes or raises, Esc pops, and `App::drawn` is
//! the one answer `main_area` draws, `App::key` routes and `hint_bar` describes.

use super::*;

/// One thing that can hold the conversation's rectangle. The data a surface shows lives
/// where it always did (`sub_out`, `job_out`, `slash_out`, `todos_open`, `queue_open` for the
/// overlays that carry some); the stack says which one is in front.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Surface {
    /// `/help`.
    Help,
    /// `/status`.
    Stats,
    /// The session picker - `ctrl-s`, `/sessions`.
    Picker,
    /// `/config`.
    Config,
    /// The todos pane - `ctrl-t`, `/todos`.
    Todos,
    /// One todo row read whole, opened by the todos pane's Enter.
    TodoRow,
    /// The merge queue - `/queue`.
    Queue,
    /// One queue entry, opened by the queue pane's Enter.
    QueueEntry,
    /// The standing notes - `/standing`.
    Standing,
    /// The skills this box has - `/skills`. One row each: the name, where it was found and
    /// its one line; Enter hands the body to the session.
    Skills,
    /// The subagent tree - `ctrl-g`, `/subagents`.
    Subagents,
    /// One subagent's output - the tree's `p`, `/peek ID`.
    SubOut,
    /// The background jobs - `ctrl-q`, `/jobs`.
    Jobs,
    /// One job's output, opened by the jobs pane's Enter.
    JobOut,
    /// The monitors the model armed, live and ended - `/monitors`, or a click on the edge's
    /// `N monitors watching`.
    Monitors,
    /// A plugin's pane - the dashboard a `pane_<name>` hook answered, open by `/pane NAME` or by a
    /// row of `/pane`'s list. The surface says where on the screen; which pane it shows is
    /// `App::plugin_open`, and the data is `slash_out`.
    PluginPane,
    /// One of that pane's stops read whole - its Enter. An overlay like every other, so Esc
    /// gives the pane back rather than reopening a screen the reader was done with.
    PluginDetail,
    /// A slash command's listing (`/notes`, a diagnostic, a long warning).
    Listing,
    /// The chat rooms - `/rooms`: channels on the left, the selected channel on the right.
    Rooms,
}

impl Surface {
    /// The pane an overlay was opened from. Closing the pane takes the overlay with it -
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
        !matches!(self, Surface::Picker | Surface::Todos | Surface::Rooms)
    }

    /// Every surface, for the tests and the generated help that walk them.
    pub(crate) const ALL: [Surface; 19] = [
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
        Surface::Rooms,
    ];
}

/// A card in the chrome that owns every key, in the order `App::key` asks them. One list, so the
/// keyboard, the bottom bar and [`App::focused_surface`] cannot disagree about which card is up: a
/// surface under a card is not focused, whatever it is, because the next keystroke is the card's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Card {
    /// `allow-all` needs a person's confirmation.
    ModeConfirm,
    /// A password, or the daemon's key ask (the same card, worded for a key).
    Secret,
    /// The confirmation that ends a `!term` pane.
    TermAsk,
    /// A question a running program is blocked on.
    Prompt,
    /// The daemon asking for a provider key.
    KeyAsk,
    /// A new todo being written into the plan.
    TodoDraft,
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
    /// What the conversation's rectangle holds, in the order the frame draws it. See
    /// [`Drawn`].
    pub(crate) fn drawn(&self) -> Drawn {
        if self.pane_open() {
            Drawn::Term
        } else if self.diff_popup.is_some() {
            Drawn::DiffPopup
        } else if self.editor_fit(self.editor_term_w.max(self.term_cols)) == EditorFit::Zoom {
            // Only where the two cannot share, or where `alt-m` asked: in a column the pane is
            // drawn beside the conversation instead (`compose_framed`). The wider of the two
            // widths is the terminal's, since a split frame measures its main column.
            Drawn::Editor
        } else if let Some(s) = self.top_surface() {
            Drawn::Surface(s)
        } else {
            Drawn::Conversation
        }
    }

    /// The surface on top of the stack - drawn unless a layer above the stack covers it.
    pub(crate) fn top_surface(&self) -> Option<Surface> {
        self.surfaces.last().map(|o| o.surface)
    }

    /// The surface that is drawn and has the keys, or nothing - the stack's top, unless a `!term` pane, the diff
    /// popup or rano is in front, or a card in the chrome holds every key. The pane under a card stays on screen,
    /// but the next keystroke is the card's, so it must not answer wheel, hover or click as focused.
    pub(crate) fn focused_surface(&self) -> Option<Surface> {
        if self.card_up().is_some() {
            return None;
        }
        match self.drawn() {
            Drawn::Surface(s) => Some(s),
            _ => None,
        }
    }

    /// The card holding the keyboard, if any. `prompt_away` is the prompt card put aside by Esc:
    /// still drawn in the slot, and no longer owning a key.
    pub(crate) fn card_up(&self) -> Option<Card> {
        if self.mode_confirm.is_some() {
            Some(Card::ModeConfirm)
        } else if self.secret.is_some() && !self.secret_away {
            Some(Card::Secret)
        } else if self.term_ask.is_some() {
            Some(Card::TermAsk)
        } else if self.prompt.is_some() && !self.prompt_away {
            Some(Card::Prompt)
        } else if self.key_ask.is_some() {
            Some(Card::KeyAsk)
        } else if self.todo_draft.is_some() {
            Some(Card::TodoDraft)
        } else {
            None
        }
    }

    /// Whether the setting card is the one in the slot, drawn and therefore owning its keys:
    /// the screen's own match (`compose_screen`), as one function so the keys and the drawing
    /// cannot disagree. A card in front hides it, and its keys fall to whatever is in front.
    pub(crate) fn pick_card_up(&self) -> bool {
        self.pick.is_some()
            && (self.secret.is_none() || self.secret_away)
            && self.term_ask.is_none()
            && (self.prompt.is_none() || self.prompt_away)
            && self.key_ask.is_none()
            && self.todo_draft.is_none()
            && self.open.is_empty()
            && !self.quit_card
    }

    /// Whether a card that owns the clicks holds the slot: the chrome cards, a decision, the
    /// quit or setting card. The reseat card is not one - it yields to everything, and the diff
    /// popup is drawn over a decision, so its own hints stay buttons.
    pub(crate) fn card_in_front(&self) -> bool {
        self.card_up().is_some()
            || (self.drawn() != Drawn::DiffPopup
                && (!self.open.is_empty() || self.quit_card || self.pick.is_some()))
    }

    /// Whether `s` is anywhere on the stack - open, drawn or not.
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
        // The session list's own failure, said where a person will see it. Opening the list is
        // the moment their broken layer matters (`app::sessions_pane`).
        if s == Surface::Picker
            && let Some(said) = self.take_sessions_failure()
        {
            self.say(&said);
        }
    }

    /// Take `s` off the stack, and every overlay opened from it. The data an overlay
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
    }

    /// Pop the top - Esc's act on a surface. Returns what was closed.
    pub(crate) fn pop_surface(&mut self) -> Option<Surface> {
        let top = self.top_surface()?;
        self.close_surface(top);
        Some(top)
    }

    /// Every surface at once - for a test that sets one pane up after another.
    #[cfg(test)]
    pub(crate) fn close_all_surfaces(&mut self) {
        while self.pop_surface().is_some() {}
    }

    /// Ask for a subagent's output, remembering which surface asked.
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
            Surface::Listing => {
                self.slash_out = None;
                self.pane_listing.clear();
                self.pane_listing_rows.clear();
            }
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

    /// Put the picker cursor on the session this head is in, from the list being drawn (the
    /// layer's or the head's own). Not found: the first session row, never a group line, and the
    /// next list tries again.
    pub(crate) fn seat_picker(&mut self) {
        let rows = self.picker_rows();
        let here = rows.iter().position(|r| {
            r.session
                .is_some_and(|i| self.sessions[i].session_id == self.session_id)
        });
        self.picker_seat_pending = here.is_none();
        self.picker_sel = here
            .or_else(|| rows.iter().position(|r| r.session.is_some()))
            .unwrap_or(0);
    }

    /// The one door a surface is opened through, so two spellings of "open the jobs pane" cannot
    /// come to do two different things. Returns what the daemon has to be asked for the surface to
    /// have rows: the jobs table, the todo list, the standing notes, the queue, the settings.
    pub(crate) fn open_surface(&mut self, s: Surface) -> Option<Action> {
        self.push_surface(s);
        match s {
            Surface::Picker => {
                // The two pickers are never open together: the setting card goes, so the
                // screen holds one list and the arrows mean one thing. The cursor starts on
                // the session this head is in, so Enter on an untouched list is a no-op.
                self.pick = None;
                self.seat_picker();
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
                // Read at open, and re-read on every draw the file has moved under - see
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
            // The first poll goes out now and the view polls on while it is on the stack.
            Surface::Rooms => self.open_rooms(),
            Surface::Subagents => {
                self.fold_subagents();
                None
            }
            Surface::Jobs => Some(Action::ListJobs),
            // The monitors come on the job table's answer.
            Surface::Monitors => Some(Action::ListJobs),
            // A plugin's pane is opened by NAME, and the name is not this function's
            // argument: [`App::open_plugin`] asks the daemon for a fresh look and pushes this
            // surface itself. Nothing here could name which pane, so nothing here asks.
            Surface::PluginPane => None,
            Surface::Stats => {
                // Opening the screen IS the acknowledgement - and only on the
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
