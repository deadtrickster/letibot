//! **The mouse, routed by what was drawn** (R69): a click and a wheel notch go to the topmost
//! target the last frame recorded under the pointer (`app::hits`), in that target's own rows.

use super::*;
use std::ops::ControlFlow;

impl App {
    /// **A card that owns every key is up** — the `allow-all` confirmation, the new-todo card,
    /// a password, a `!term close` confirmation, a prompt, a key ask, or the diff popup. Their
    /// handlers take the mouse as one of their keys, so the pointer's position means nothing
    /// while one of them is in front.
    fn modal_up(&self) -> bool {
        self.mode_confirm.is_some()
            || self.todo_draft.is_some()
            || self.secret.is_some()
            || self.term_ask.is_some()
            || (self.prompt.is_some() && !self.prompt_away)
            || self.key_ask.is_some()
            || self.diff_popup.is_some()
    }

    /// **Is the target still what is on the screen** — a key earlier in the same read can have
    /// closed the pane or answered the card the last frame drew, and a click measured against
    /// that frame must not be answered by whatever is in its place now.
    fn still_drawn(&self, t: Target) -> bool {
        match t {
            Target::Surface(s) => self.focused_surface() == Some(s),
            Target::QueueTab(_) => self.focused_surface() == Some(Surface::Queue),
            Target::TodoWait => self.focused_surface() == Some(Surface::Todos),
            Target::Conversation | Target::Notice | Target::NoteCard => {
                self.drawn() == Drawn::Conversation
            }
            Target::Decision => !self.open.is_empty(),
            Target::QuitCard => self.quit_card,
            Target::ReseatCard => self.reseat_card_up(),
            Target::SettingCard => self.pick.is_some(),
            Target::CountLabel(_) | Target::GitField => true,
            Target::FlashNotice => self.status_click.is_some(),
        }
    }

    /// **A click, answered from the last frame alone.**
    ///
    /// The topmost target under the pointer; the stop drawn on that row; that surface's act.
    /// **A click that lands on no target falls through** — it used to be taken by whichever of
    /// the todos, queue, picker or setting-card handlers was open, hit or miss, which is why the
    /// count labels stopped answering while one of those was up. A click on a target with no
    /// stop on that row (a pane's title, the blank under a short list) is the target's and moves
    /// nothing: there is nothing under it to fall through to.
    ///
    /// **What a click does is select**, everywhere a row is a choice — the picker's own rule,
    /// *"select and confirm stay two acts"*: Enter still does the switching, the answering, the
    /// opening. The exceptions are the targets that are buttons: a count label opens its pane,
    /// a queue tab switches the view, an edit row opens its change.
    pub(crate) fn key_click(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        let Key::Click { x, y } = *k else {
            return ControlFlow::Continue(());
        };
        let Some(hit) = self.hit_at(x, y).cloned() else {
            return ControlFlow::Continue(());
        };
        // **Anywhere on an entry is the entry** — its second line too (a subagent's facts), the
        // same block that lights up under the pointer. See `App::stop_block`.
        let row = usize::from(y);
        let stop = hit
            .stop_at(row)
            .or_else(|| self.stop_block(&hit, row).map(|(s, _)| s));
        let target = hit.target;
        if !self.still_drawn(target) {
            return ControlFlow::Break(None);
        }
        // **A run marker's clause opens its run, that kind only** (`app::runclick`).
        if target == Target::Conversation
            && let Some(m) = self.marker_at(x, y).cloned()
        {
            self.open_marker(m);
            return ControlFlow::Break(None);
        }
        self.click_on(target, stop)
    }

    fn click_on(&mut self, target: Target, stop: Option<usize>) -> ControlFlow<Option<Action>> {
        match (target, stop) {
            // **The git field opens the repo card** — the same card `/repo` opens.
            (Target::GitField, _) => {
                let _ = self.command("repo");
                return ControlFlow::Break(None);
            }
            (Target::CountLabel(l), _) => {
                // **The label's chord, through the one door** — the operator's ask: *"make so that
                // when i click on running subagents or jobs count labels i get to respective
                // panes"*.
                return ControlFlow::Break(self.toggle_surface(match l {
                    CountLabel::Subagents => Surface::Subagents,
                    CountLabel::Jobs => Surface::Jobs,
                    CountLabel::Monitors => Surface::Monitors,
                }));
            }
            (Target::QueueTab(view), _) => self.queue_view = view,
            // **An edit or write row opens its change in the diff popup** — the whole file with
            // the change inline; `ctrl-]` from there is the editor. A click elsewhere in the
            // conversation is nobody's and falls through.
            (Target::Conversation, Some(k)) => {
                if let Some((_, f)) = self.file_rows.get(k).cloned() {
                    // A change opens to be reviewed; a cited line has none, and opens the file
                    // there.
                    if f.change.is_some() {
                        self.open_diff_popup(&f);
                    } else {
                        self.open_in_editor(&f);
                    }
                }
                return ControlFlow::Break(None);
            }
            // **A job's completion notice opens whole** — the operator: *"click behaves similar
            // to diff click - popup with full card"*.
            (Target::Notice, Some(k)) => {
                self.open_notice_popup(k);
                return ControlFlow::Break(None);
            }
            // **And a red target's warning opens whole** — the operator: *"a one-line similar
            // to job - with a popup"*.
            (Target::NoteCard, Some(i)) => {
                self.open_card_popup(i);
                return ControlFlow::Break(None);
            }
            // **A todo row's `waits on j97 · pg-timing` opens the job** — the operator: *"\"waits
            // on j97\" - make j97 a link to the real job, also render job slug"*. A button, like
            // the notice: the click is the act, and `w` on the row is the same act.
            // **The flash notice, cut to its line, opens whole** (R89) — and so does a progress
            // the status row could not say all of.
            (Target::FlashNotice, _) => {
                if let Some((title, text)) = self.status_click.clone() {
                    self.open_text_popup(text, (title, rano::style::Role::Pending));
                }
                return ControlFlow::Break(None);
            }
            (Target::TodoWait, Some(k)) => {
                return ControlFlow::Break(self.open_wait_link(k));
            }
            (Target::Conversation | Target::Notice | Target::NoteCard, None) => {
                return ControlFlow::Continue(());
            }
            (_, None) => return ControlFlow::Break(None),
            (Target::Surface(s), Some(k)) => match s {
                Surface::Picker => self.picker_sel = k,
                Surface::Todos => {
                    self.todos_sel = k;
                    let stops = self.todos_stops();
                    self.sync_repo_from_stop(&stops);
                }
                Surface::Queue => self.queue_sel = k,
                Surface::Subagents => self.subagents_sel = k,
                Surface::Jobs => self.jobs_sel = k,
                Surface::Monitors => self.monitors_sel = k,
                Surface::Standing => self.standing_sel = k,
                Surface::Config => self.config_sel = k,
                _ => return ControlFlow::Break(None),
            },
            (Target::Decision, Some(k)) => self.sel = k,
            (Target::QuitCard, Some(k)) => self.quit_sel = k,
            (Target::ReseatCard, Some(k)) => {
                if let Some(c) = self.reseat.as_mut() {
                    c.sel = k;
                }
            }
            (Target::SettingCard, Some(k)) => {
                self.mode_sel = k;
                // The reader has taken the cursor, so a settings answer landing from here on must
                // not move it — see [`App::pick_unseeded`].
                self.pick_unseeded = false;
            }
        }
        self.redraw = true;
        ControlFlow::Break(None)
    }

    /// **A wheel notch, sent to the surface under the pointer.**
    ///
    /// `Continue` hands the notch on with no position — to the cards that own every key, and
    /// for a pointer over nothing the frame recorded (the composer, the hint bar), where the
    /// notch does what it always did. Over a target, the notch is that target's and only its:
    /// the pane or output under the pointer scrolls, the conversation scrolls (or the ctrl-v
    /// window in it pages), the decision card scrolls its content while it has somewhere to go,
    /// and a card or label with nothing to scroll takes the notch and moves nothing — a notch
    /// over a card must not scroll the transcript behind the reader's back.
    pub(crate) fn wheel_at(&mut self, up: bool, x: u16, y: u16) -> ControlFlow<Option<Action>> {
        if self.modal_up() {
            return ControlFlow::Continue(());
        }
        let Some(target) = self.hit_at(x, y).map(|h| h.target) else {
            return ControlFlow::Continue(());
        };
        if !self.still_drawn(target) {
            return ControlFlow::Break(None);
        }
        const NOTCH: usize = 3;
        match target {
            Target::Surface(_) | Target::QueueTab(_) | Target::TodoWait => {
                self.scroll_pane(up, NOTCH)
            }
            // A window, not an open run — a run is the conversation's own rows and the
            // conversation scrolls under it (see [`App::run_is_open`]).
            Target::Conversation | Target::Notice | Target::NoteCard
                if self.payload_window_open() =>
            {
                self.page_payload(up, NOTCH)
            }
            Target::Conversation | Target::Notice | Target::NoteCard => {
                let k = if up { Key::WheelUp } else { Key::WheelDown };
                self.scroll_conversation(&k, up, NOTCH);
            }
            Target::Decision | Target::QuitCard | Target::ReseatCard => {
                self.scroll_card(up, NOTCH);
            }
            Target::SettingCard
            | Target::CountLabel(_)
            | Target::GitField
            | Target::FlashNotice => {}
        }
        ControlFlow::Break(None)
    }
}
