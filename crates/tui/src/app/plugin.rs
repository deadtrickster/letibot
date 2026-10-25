//! **Plugin panes, as this head holds them**: the document each one answered, which of its
//! screens is on the screen, the stop under the cursor, and the three acts a key has — open a
//! panel's view, read a stop whole, or send one of its actions back to the plugin.
//!
//! # Where the state lives, and why it is keyed by name
//!
//! A plugin pane is not a `Surface` with its own fields, because there can be any number of
//! them: `App::plugin_panes` is a map from the pane's name (`<layer>/<hook>`, the wire's own
//! spelling) to the view its last look answered, and `App::plugin_open` says which of them the
//! [`Surface::PluginPane`] on the stack is showing. That is the same split `Surface::Listing`
//! makes with `slash_out`: the surface says *where on the screen*, the data says *what*.
//!
//! # The keys are the host's, templated
//!
//! `q`, `j`, `k`, `f` and `l` mean here what they mean in every other pane, `↑↓` move the
//! cursor through the stops, Enter reads one whole and Esc leaves what it entered. The only
//! keys a plugin chooses are its panels' keys and its stops' action letters, both single
//! lowercase letters, both refused by `pane::View::validate` when they collide with the host's.
//! So a plugin cannot own a key the operator expects to leave with — which is C3's whole
//! sentence about the keys being the host's. **`j` and `k` are reserved and unbound**: C3 puts
//! them in the host's set so a plugin cannot take them, and this head does not give them a
//! meaning here (the arrows are what moves), so they fall through to the composer with every
//! other letter the pane did not claim. A reserved letter that swallowed typing would be a
//! pane you cannot write a sentence under.

use super::*;
use crate::ui::panes::plugin as view;
use crate::ui::render::row_strings;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::pane as pv;

/// **One plugin pane's held state**: the view its last look answered, or why this head could
/// not read it.
///
/// A view that does not parse is not an empty pane — an empty dashboard and a broken plugin
/// look identical on a screen and mean opposite things, which is why the reason is kept and
/// drawn rather than dropped.
#[derive(Debug, Clone, Default)]
pub(crate) struct PluginState {
    pub(crate) held: Option<pv::View>,
    pub(crate) why: Option<String>,
}

impl PluginState {
    /// **The view, parsed and checked** — the daemon validates a plugin's answer before it
    /// publishes one, and the head checks it again for the view it is about to draw, because
    /// a head is the half that knows which keys it can honour.
    fn of(value: serde_json::Value) -> PluginState {
        match serde_json::from_value::<pv::View>(value) {
            Ok(v) => match v.validate() {
                Ok(()) => PluginState {
                    held: Some(v),
                    why: None,
                },
                Err(why) => PluginState {
                    why: Some(why),
                    ..Default::default()
                },
            },
            Err(e) => PluginState {
                held: None,
                why: Some(format!("this head cannot read that pane's view: {e}")),
            },
        }
    }
}

impl App {
    /// **A view arrived from the daemon.** Stored under its pane's name, and the screen
    /// redrawn; nothing is opened and nothing is closed — a pane the operator has not opened is
    /// simply held, so opening it draws what is already known instead of an empty frame.
    pub(crate) fn on_plugin_event(&mut self, e: SessionEvent) -> Disposition {
        match e {
            SessionEvent::PluginPane { pane, view } => {
                self.plugin_panes.insert(pane, PluginState::of(view));
                self.redraw = true;
                Disposition::Control
            }
            SessionEvent::PluginPaneGone { pane } => {
                self.plugin_gone(&pane);
                self.redraw = true;
                Disposition::Control
            }
            _ => Disposition::Control,
        }
    }

    /// **The pane is gone**: its view is dropped, and the pane is closed if it is the one on
    /// screen — a surface left holding a name nobody answers is a screen whose keys do nothing.
    pub(crate) fn plugin_gone(&mut self, pane: &str) {
        self.plugin_panes.remove(pane);
        if self.plugin_open.as_deref() == Some(pane) {
            self.close_surface(Surface::PluginPane);
        }
    }

    /// **The view of the pane on the screen**, if it parsed and validated.
    pub(crate) fn plugin_view(&self) -> Option<&pv::View> {
        let name = self.plugin_open.as_deref()?;
        self.plugin_panes.get(name)?.held.as_ref()
    }

    /// **Why the pane on the screen has no view**, when it has none.
    pub(crate) fn plugin_why(&self) -> Option<&str> {
        let name = self.plugin_open.as_deref()?;
        self.plugin_panes.get(name)?.why.as_deref()
    }

    /// **The stop under the cursor**, clamped to a list that can move under it — a view is
    /// replaced wholesale on every look, and an arrow pressed against a shorter list must land
    /// on a stop rather than on an index that no longer exists.
    pub(crate) fn plugin_selected(&self) -> Option<view::Target<'_>> {
        let v = self.plugin_view()?;
        view::target_of(v, self.plugin_screen, self.plugin_sel)
    }

    /// The stops of the screen on the screen — the list the arrows walk, Enter acts on, and a
    /// click is measured against.
    pub(crate) fn plugin_stops(&self) -> Vec<view::Stop> {
        match self.plugin_view() {
            Some(v) => view::stops(v, self.plugin_screen),
            None => Vec::new(),
        }
    }

    /// **The pane row the stop at the cursor was DRAWN on**, read out of
    /// [`App::plugin_stop_rows`] — the record the pane wrote while drawing, and never
    /// arithmetic over the view. The sibling of [`App::jobs_row_of`], clamp and all.
    pub(crate) fn plugin_row_of(&self) -> usize {
        let at = self
            .plugin_sel
            .min(self.plugin_stop_rows.len().saturating_sub(1));
        self.plugin_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// **Open a plugin's pane**, by the name the daemon publishes it under — `/pane NAME`, or a
    /// row of `/pane`'s list.
    ///
    /// It asks the daemon for a fresh look (`Action::PaneOpen`) as it opens, for the reason the
    /// jobs pane asks for its table: what this head holds may be a view from an hour ago, and a
    /// dashboard an hour old is a dashboard of another moment. The held view is drawn first, so
    /// the pane is never blank while the answer travels.
    pub(crate) fn open_plugin(&mut self, name: &str) -> Option<Action> {
        if !self.plugin_panes.contains_key(name) {
            self.say(&format!(
                "no plugin pane called `{name}` — `/pane` lists the ones this session has"
            ));
            self.redraw = true;
            return None;
        }
        self.plugin_open = Some(name.to_string());
        self.plugin_screen = view::Screen::Overview;
        self.plugin_sel = 0;
        self.push_surface(Surface::PluginPane);
        Some(Action::PaneOpen {
            pane: name.to_string(),
        })
    }

    /// **Esc, inside a plugin pane**: leave the view, then the pane. `true` when it took the
    /// key.
    ///
    /// C3's ladder and nobody else's — a detail is its own surface, so the stack's Esc has
    /// already taken that step by the time this runs; what is left is the view a panel's key
    /// (or `f`, or `l`) opened, and the overview under it.
    pub(crate) fn plugin_esc(&mut self) -> bool {
        if self.focused_surface() != Some(Surface::PluginPane) {
            return false;
        }
        if self.plugin_screen == view::Screen::Overview {
            return false;
        }
        self.plugin_screen = view::Screen::Overview;
        self.plugin_sel = 0;
        self.pane_scroll = 0;
        self.redraw = true;
        true
    }

    /// **One key of the pane's own** — a panel's key, `f`, `l`, or a letter of the selected
    /// stop's actions. `Continue` when the letter is none of those, so it reaches the composer:
    /// the letters are the operator's to type first, and a pane that swallowed every one would
    /// be a pane you cannot type under (`Guard::EmptyComposer` in the binding table).
    pub(crate) fn plugin_key(&mut self, k: &Key) -> std::ops::ControlFlow<Option<Action>> {
        use std::ops::ControlFlow;
        let Key::Char(c) = *k else {
            return ControlFlow::Continue(());
        };
        // **`f` and `l` are the host's**, and they are the two views that are not a panel's.
        let panel = self.plugin_view().and_then(|v| v.panel_of_key(c));
        let target = match c {
            'f' => Some(view::Screen::Findings),
            'l' => Some(view::Screen::Legend),
            _ => None,
        };
        if let Some(want) = target {
            // A key whose view is already on the screen closes it again: the same toggle rule
            // every fold in this head keeps.
            self.plugin_screen = if self.plugin_screen == want {
                view::Screen::Overview
            } else {
                want
            };
            self.plugin_sel = 0;
            self.pane_scroll = 0;
            self.redraw = true;
            return ControlFlow::Break(None);
        }
        if let Some(i) = panel {
            self.plugin_panel_key(i);
            return ControlFlow::Break(None);
        }
        // **The selected stop's own action letter.** Read out as owned values, so the borrow
        // on the view ends before anything below writes to this head.
        let chosen = self.plugin_selected().map(|t| {
            let action = t.actions().iter().find(|a| a.letter() == Some(c)).cloned();
            (t.id().to_string(), action)
        });
        let Some((item, Some(action))) = chosen else {
            return ControlFlow::Continue(());
        };
        let Some(pane) = self.plugin_open.clone() else {
            return ControlFlow::Continue(());
        };
        match action.kind() {
            // **A line to the session's model, through the composer's own path** — queued and
            // announced exactly as a typed one is, and attributed to the operator, who is the
            // one who pressed the key. Not a `PaneAction`: a pane does not get a second door
            // into the model.
            Some(pv::ActionKind::Say(text)) => ControlFlow::Break(self.submit(text)),
            // **A call back into the plugin**, answered with a new view on the log.
            Some(pv::ActionKind::Look) => ControlFlow::Break(Some(Action::PaneAction {
                pane,
                item,
                action: "look".to_string(),
            })),
            Some(pv::ActionKind::Event(word)) => ControlFlow::Break(Some(Action::PaneAction {
                pane,
                item,
                action: word,
            })),
            // A view `validate` refused cannot be drawn, so this arm is the belt beside its
            // braces: a key with no meaning is the composer's.
            None => ControlFlow::Continue(()),
        }
    }

    /// **Enter: the stop under the cursor, whole** — its detail, and for a finding the numbers
    /// behind it, the fix and the way to check it (C3). Opened as a surface of its own, so Esc
    /// has somewhere to come back to.
    pub(crate) fn plugin_enter(&mut self) -> Option<Action> {
        // Nothing under the cursor — an empty list, or a view that did not parse — is a pane
        // that has nothing to open, and Enter is still the pane's.
        self.plugin_selected()?;
        self.plugin_detail = Some((self.plugin_screen, self.plugin_sel));
        self.push_surface(Surface::PluginDetail);
        None
    }

    /// **The detail's rows** — the stop the overlay was opened on, read out of the screen it
    /// was opened FROM (a value, kept at open: the overlay is not the pane's screen and never
    /// becomes it). `None` when the view moved under the overlay and its row is gone.
    pub(crate) fn plugin_detail_lines(&self, w: usize) -> Option<Vec<rano::render::Line>> {
        let (screen, index) = self.plugin_detail?;
        let v = self.plugin_view()?;
        Some(view::detail(v, screen, index, w).lines)
    }

    /// **A click on panel `i`'s title, or that panel's key** — one door for the two, so a
    /// click and a key cannot come to disagree about what opening a panel means. It toggles,
    /// as the key does: the view you are in closes back to the overview.
    pub(crate) fn plugin_panel_key(&mut self, i: usize) -> Option<Action> {
        self.plugin_screen = if self.plugin_screen == view::Screen::Panel(i) {
            view::Screen::Overview
        } else {
            view::Screen::Panel(i)
        };
        self.plugin_sel = 0;
        self.pane_scroll = 0;
        self.redraw = true;
        None
    }

    /// **The pane's rows, recording where each stop and each panel title landed** — the record
    /// the arrows scroll by and a click is tested against, written by the frame that drew them.
    pub(crate) fn plugin_lines(&mut self, w: usize) -> Vec<String> {
        let Some(v) = self.plugin_view() else {
            // **A pane with no readable view says why**: an empty dashboard and a plugin this
            // head cannot read look identical on a screen and mean opposite things.
            self.plugin_stop_rows.clear();
            self.plugin_panel_rows.clear();
            let why = self
                .plugin_why()
                .map(|w| w.to_string())
                .unwrap_or_else(|| "this pane has answered nothing yet".to_string());
            let lines = vec![
                rano::agent::pane::title(
                    self.plugin_open
                        .clone()
                        .unwrap_or_else(|| "a plugin's pane".to_string()),
                ),
                rano::agent::pane::faint(format!("  {why}")),
            ];
            return row_strings(&lines, self.cfg.palette());
        };
        let content = view::content(v, self.plugin_screen, self.plugin_sel, w);
        self.plugin_stop_rows = content.lines.stop_rows.clone();
        self.plugin_panel_rows = content.panels;
        row_strings(&content.lines.lines, self.cfg.palette())
    }

    /// **The plugin panes this head holds, by title** — `/pane`'s list, and the panes list the
    /// operator picks from. `(name, title, badge)`.
    pub(crate) fn plugin_list(&self) -> Vec<(String, String, String)> {
        self.plugin_panes
            .iter()
            .map(|(name, st)| {
                let (title, badge) = match &st.held {
                    Some(v) => (
                        if v.title.is_empty() {
                            name.clone()
                        } else {
                            v.title.clone()
                        },
                        v.badge.as_ref().map(|b| b.text.clone()).unwrap_or_default(),
                    ),
                    None => (name.clone(), String::new()),
                };
                (name.clone(), title, badge)
            })
            .collect()
    }

    /// **`/pane`'s listing**: every plugin pane this session has, by title, with the spelling
    /// that opens it. The panes list the operator picks from, on this head, is this one and the
    /// help screen's `/pane NAME` row: the names are the daemon's and a fixed list would be a
    /// second copy of them.
    pub(crate) fn plugin_listing(&self) -> Vec<String> {
        if self.plugin_panes.is_empty() {
            return vec![
                "no plugin panes — none of this session's layers exports a `pane_` hook yet"
                    .to_string(),
            ];
        }
        let mut out = vec!["/pane NAME opens one of these:".to_string(), String::new()];
        for (name, title, badge) in self.plugin_list() {
            out.push(if badge.is_empty() {
                format!("  {title}")
            } else {
                format!("  {title} — {badge}")
            });
            out.push(format!("    /pane {name}"));
        }
        out
    }

    /// **The status row's badge for the pane on the screen** — the pane's own counter, drawn
    /// where a turn's progress is, and only while the pane is on the screen: a counter about a
    /// screen nobody is looking at is furniture.
    pub(crate) fn plugin_badge(&self) -> Option<(String, rano::style::Role)> {
        self.plugin_view().and_then(view::badge)
    }
}
