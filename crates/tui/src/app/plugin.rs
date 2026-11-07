//! Plugin panes as this head holds them: the view each answered, which screen is shown, and the
//! stop under the cursor. A pane is not a `Surface`, because there can be any number of them; keys
//! stay the host's, and a plugin chooses only its panels' keys and its stops' letters.

use super::*;
use crate::ui::panes::plugin as view;
use crate::ui::render::row_strings;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::pane as pv;

/// One plugin pane's held state: the view its last look answered, or why this head could not
/// read it. The reason is kept because an empty dashboard and a broken plugin look identical
/// on a screen and mean opposite things.
#[derive(Debug, Clone, Default)]
pub(crate) struct PluginState {
    pub(crate) held: Option<pv::View>,
    pub(crate) why: Option<String>,
    /// `X8`'s cursor mode: a pane that owns the composer has its own keys live only after Tab put
    /// the reader into the view, because otherwise every letter is the line being typed.
    pub(crate) cursor: bool,
}

impl PluginState {
    /// The view, parsed and checked - the daemon validates a plugin's answer before it
    /// publishes one, and the head checks it again for the view it is about to draw, because
    /// a head is the half that knows which keys it can honour.
    fn of(value: serde_json::Value) -> PluginState {
        match serde_json::from_value::<pv::View>(value) {
            Ok(v) => match v.validate() {
                Ok(()) => PluginState {
                    held: Some(v),
                    why: None,
                    cursor: false,
                },
                Err(why) => PluginState {
                    why: Some(why),
                    ..Default::default()
                },
            },
            Err(e) => PluginState {
                held: None,
                why: Some(format!("this head cannot read that pane's view: {e}")),
                cursor: false,
            },
        }
    }
}

impl App {
    /// A view arrived from the daemon. Stored under its pane's name, and the screen
    /// redrawn; nothing is opened and nothing is closed - a pane the operator has not opened is
    /// simply held, so opening it draws what is already known instead of an empty frame.
    pub(crate) fn on_plugin_event(&mut self, e: SessionEvent) -> Disposition {
        match e {
            SessionEvent::PluginPane { pane, view } => {
                self.plugin_panes.insert(pane, PluginState::of(view));
                Disposition::Control
            }
            SessionEvent::PluginPaneGone { pane } => {
                self.plugin_gone(&pane);
                Disposition::Control
            }
            _ => Disposition::Control,
        }
    }

    /// The pane is gone: its view is dropped, and the pane is closed if it is the one on
    /// screen - a surface left holding a name nobody answers is a screen whose keys do nothing.
    pub(crate) fn plugin_gone(&mut self, pane: &str) {
        self.plugin_panes.remove(pane);
        if self.plugin_open.as_deref() == Some(pane) {
            self.close_surface(Surface::PluginPane);
        }
    }

    /// The view of the pane on the screen, if it parsed and validated.
    pub(crate) fn plugin_view(&self) -> Option<&pv::View> {
        let name = self.plugin_open.as_deref()?;
        self.plugin_panes.get(name)?.held.as_ref()
    }

    /// Why the pane on the screen has no view, when it has none.
    pub(crate) fn plugin_why(&self) -> Option<&str> {
        let name = self.plugin_open.as_deref()?;
        self.plugin_panes.get(name)?.why.as_deref()
    }

    /// Whether the pane on the screen is in cursor mode (`X8`).
    pub(crate) fn plugin_cursor(&self) -> bool {
        let Some(name) = self.plugin_open.as_deref() else {
            return false;
        };
        self.plugin_panes.get(name).is_some_and(|s| s.cursor)
    }

    /// Enter or leave cursor mode. Leaving it hands the letters back to the composer, which is what
    /// Esc promises on a pane that owns the input.
    pub(crate) fn plugin_cursor_set(&mut self, on: bool) {
        if let Some(name) = self.plugin_open.clone()
            && let Some(st) = self.plugin_panes.get_mut(&name)
        {
            st.cursor = on;
        }
    }

    /// Whether the view's own keys are live: always on a pane with no input, and in cursor mode on
    /// one that owns the composer (`X8`).
    fn plugin_keys_live(&self) -> bool {
        self.plugin_view()
            .map(|v| v.input.is_none())
            .unwrap_or(false)
            || self.plugin_cursor()
    }

    /// The stop under the cursor, clamped to a list that can move under it - a view is
    /// replaced wholesale on every look, and an arrow pressed against a shorter list must land
    /// on a stop rather than on an index that no longer exists.
    pub(crate) fn plugin_selected(&self) -> Option<view::Target<'_>> {
        let v = self.plugin_view()?;
        view::target_of(v, self.plugin_screen, self.plugin_sel)
    }

    /// The legend's words for the row the pointer is over: a legend documents every label, and
    /// the same text answers on hover. Read from `pointer_at`, not `hovered` - the pointer over
    /// a legend row acts on nothing, which is exactly the row a reader wants explained.
    pub(crate) fn plugin_hover_legend(&self) -> Option<&str> {
        let (_, y) = self.pointer_at?;
        let key = self
            .plugin_legend_at
            .iter()
            .find(|(row, _)| *row == usize::from(y))?
            .1
            .clone();
        self.plugin_view()?.legend.get(&key).map(String::as_str)
    }

    /// The stops of the screen on the screen - the list the arrows walk, Enter acts on, and a
    /// click is measured against.
    pub(crate) fn plugin_stops(&self) -> Vec<view::Stop> {
        match self.plugin_view() {
            Some(v) => view::stops(v, self.plugin_screen),
            None => Vec::new(),
        }
    }

    /// The pane row the stop at the cursor was DRAWN on, read out of
    /// [`App::plugin_stop_rows`] - the record the pane wrote while drawing, and never
    /// arithmetic over the view. The sibling of [`App::jobs_row_of`], clamp and all.
    pub(crate) fn plugin_row_of(&self) -> usize {
        let at = self
            .plugin_sel
            .min(self.plugin_stop_rows.len().saturating_sub(1));
        self.plugin_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// Open a plugin's pane, by the name the daemon publishes it under - `/pane NAME`, or a row
    /// of `/pane`'s list. It asks for a fresh look as it opens, because a held view may be an
    /// hour old; the held view is drawn first, so the pane is never blank while that travels.
    pub(crate) fn open_plugin(&mut self, name: &str) -> Option<Action> {
        if !self.plugin_panes.contains_key(name) {
            self.say(&format!(
                "no plugin pane called `{name}` — `/pane` lists the ones this session has"
            ));
            return None;
        }
        // A different pane in the same rectangle is a pane that closed: the surface is one, and
        // the daemon holds a refresh clock per pane NAME. Without this the dashboard the operator
        // navigated away from would keep its look running for the rest of the session.
        if let Some(previous) = self.plugin_open.replace(name.to_string())
            && previous != name
        {
            self.queued.push(Action::PaneClose { pane: previous });
        }
        self.plugin_screen = view::Screen::Overview;
        self.plugin_sel = 0;
        self.push_surface(Surface::PluginPane);
        // The rectangle is a guess here and a measurement on the next frame - see
        // [`Action::PaneOpen`]. What this head last sent is the best it has.
        let (w, h) = self
            .plugin_sent
            .as_ref()
            .map(|(_, w, h)| (*w, *h))
            .unwrap_or((0, 0));
        self.plugin_sent = Some((name.to_string(), w, h));
        Some(Action::PaneOpen {
            pane: name.to_string(),
            w,
            h,
        })
    }

    /// The pane leaves the surface stack, so the daemon is told to stop refreshing it - the
    /// opposite of the `PaneOpen` `open_plugin` sends. The name is read BEFORE the close, because
    /// `forget_surface` clears `plugin_open` with the pane.
    pub(crate) fn close_plugin(&mut self) -> Option<Action> {
        // The surface closes whatever the name says: a pane left on the stack with no name is
        // a screen whose keys go nowhere, and the name is only needed for the frame.
        let pane = self.plugin_open.clone();
        self.close_surface(Surface::PluginPane);
        pane.map(|pane| Action::PaneClose { pane })
    }

    /// Esc inside a plugin pane: leave the view, then the pane. `true` when it took the key. A
    /// detail is its own surface, so the stack's Esc has already taken that step; what is left is
    /// the view a panel's key (or `f`, or `l`) opened, and the overview under it.
    pub(crate) fn plugin_esc(&mut self) -> bool {
        if self.focused_surface() != Some(Surface::PluginPane) {
            return false;
        }
        // Cursor mode is a mode INSIDE the view, so Esc leaves it before it leaves the view.
        if self.plugin_cursor() {
            self.plugin_cursor_set(false);
            return true;
        }
        if self.plugin_screen == view::Screen::Overview {
            return false;
        }
        self.plugin_screen = view::Screen::Overview;
        self.plugin_sel = 0;
        self.pane_scroll = 0;
        true
    }

    /// One key of the pane's own - a panel's key, `f`, `l`, or a letter of the selected stop's
    /// actions. `Continue` when it is none of those, so the letter reaches the composer: a pane
    /// that swallowed every one would be a pane you cannot type under.
    pub(crate) fn plugin_key(&mut self, k: &Key) -> std::ops::ControlFlow<Option<Action>> {
        use std::ops::ControlFlow;
        // Tab on a pane that owns an input enters its cursor mode (`X8`); without an input there
        // is no mode to enter, and the view's keys are live all the time.
        if matches!(k, Key::Tab)
            && self.plugin_view().is_some_and(|v| v.input.is_some())
            && !self.plugin_cursor()
        {
            self.plugin_cursor_set(true);
            return ControlFlow::Break(None);
        }
        // The view's own keys: a letter of either case, `ctrl-k`, `home` or `end`. Live only in
        // cursor mode on a pane with an input, and gated by `validate_for` otherwise.
        if let Some(word) = view_key_word(k)
            && self.plugin_keys_live()
            && let Some(kind) = self.plugin_view().and_then(|v| {
                v.keys
                    .iter()
                    .find(|vk| vk.word() == word)
                    .and_then(|vk| vk.kind())
            })
        {
            let pane = self.plugin_open.clone();
            let item = self
                .plugin_selected()
                .map(|t| t.id().to_string())
                .unwrap_or_default();
            return match (kind, pane) {
                // A line to the session's model goes through the composer's own path, as a stop's
                // `say` does, and is never a second door into the model.
                (pv::ActionKind::Say(text), _) => ControlFlow::Break(self.submit(text)),
                (pv::ActionKind::Look, Some(pane)) => {
                    ControlFlow::Break(Some(Action::PaneAction {
                        pane,
                        item,
                        action: "look".to_string(),
                    }))
                }
                (pv::ActionKind::Event(word) | pv::ActionKind::Verb(word), Some(pane)) => {
                    ControlFlow::Break(Some(Action::PaneAction {
                        pane,
                        item,
                        action: word,
                    }))
                }
                (_, None) => ControlFlow::Continue(()),
            };
        }
        let Key::Char(c) = *k else {
            return ControlFlow::Continue(());
        };
        // `f` and `l` are the host's, and they are the two views that are not a panel's.
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
            return ControlFlow::Break(None);
        }
        if let Some(i) = panel {
            self.plugin_panel_key(i);
            return ControlFlow::Break(None);
        }
        // The selected stop's own action letter. Read out as owned values, so the borrow
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
            // A line to the session's model, through the composer's own path: queued and
            // announced as a typed one is, attributed to the operator who pressed the key. Not a
            // `PaneAction` - a pane does not get a second door into the model.
            Some(pv::ActionKind::Say(text)) => ControlFlow::Break(self.submit(text)),
            // A call back into the plugin, answered with a new view on the log.
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
            // The host's own verb, on the entry this row names: the same door an `event` word
            // takes, because the host that performs it is the one that drew the row. This head
            // has no such state to act on and no business guessing at one.
            Some(pv::ActionKind::Verb(word)) => ControlFlow::Break(Some(Action::PaneAction {
                pane,
                item,
                action: word,
            })),
            // A view `validate` refused cannot be drawn, so this arm is the belt beside its
            // braces: a key with no meaning is the composer's.
            None => ControlFlow::Continue(()),
        }
    }

    /// Enter: the stop under the cursor - the act the row carries, or else the row read whole.
    /// The act comes first: `View::validate_for` has already refused a row reaching an undeclared
    /// verb, so by here it is one the layer was allowed to ask for. No act falls to the detail.
    pub(crate) fn plugin_enter(&mut self) -> Option<Action> {
        // Nothing under the cursor - an empty list, or a view that did not parse - is a pane
        // that has nothing to open, and Enter is still the pane's.
        let act = self.plugin_selected()?.enter().cloned();
        let item = self
            .plugin_selected()
            .map(|t| t.id().to_string())
            .unwrap_or_default();
        match act {
            Some(pv::Act::Switch { session }) => {
                // The pane leaves with the switch. A plugin pane is a screen, not a column the
                // host lays out beside the conversation: leaving it open would show the session
                // list of the session you just left, over the conversation you just arrived in.
                if let Some(close) = self.close_plugin() {
                    self.queued.push(close);
                }
                self.switch_to(session)
            }
            Some(pv::Act::Event { event }) => {
                let pane = self.plugin_open.clone()?;
                Some(Action::PaneAction {
                    pane,
                    item,
                    action: event,
                })
            }
            // A verb the layer declared, sent to the host that drew the row: `validate_for` gates
            // it before this head sees the view. The queue's own verbs are performed by the
            // queue's surface against this head's snapshot, and never arrive here.
            Some(pv::Act::Verb { verb, id }) => {
                let pane = self.plugin_open.clone()?;
                Some(Action::PaneAction {
                    pane,
                    item: id,
                    action: verb,
                })
            }
            // A row with no act, or one this surface cannot perform: the detail.
            Some(pv::Act::Fold { .. }) | None => {
                self.plugin_detail = Some((self.plugin_screen, self.plugin_sel));
                self.push_surface(Surface::PluginDetail);
                None
            }
        }
    }

    /// The detail's rows - the stop the overlay was opened on, read out of the screen it
    /// was opened FROM (a value, kept at open: the overlay is not the pane's screen and never
    /// becomes it). `None` when the view moved under the overlay and its row is gone.
    pub(crate) fn plugin_detail_lines(&self, w: usize) -> Option<Vec<rano::render::Line>> {
        let (screen, index) = self.plugin_detail?;
        let v = self.plugin_view()?;
        Some(view::detail(v, screen, index, w).lines)
    }

    /// A click on panel `i`'s title, or that panel's key - one door for the two, so a
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
        None
    }

    /// The pane's rows, recording where each stop and each panel title landed - the record
    /// the arrows scroll by and a click is tested against, written by the frame that drew them.
    pub(crate) fn plugin_lines(&mut self, w: usize) -> Vec<String> {
        // A stream names its pictures by blob id: the head fetches the ones it has not got, exactly
        // as it does for the chat's own messages, so the picture draws where the layer put it.
        let wanted: Vec<crate::app::rooms::Attach> = self
            .plugin_view()
            .map(|v| {
                v.panels
                    .iter()
                    .flat_map(|p| p.summary.iter().chain(p.view.iter()))
                    .filter_map(|b| match b {
                        pv::Block::Stream(s) => Some(crate::app::chat_plugin::stream_pictures(s)),
                        _ => None,
                    })
                    .flatten()
                    .collect()
            })
            .unwrap_or_default();
        if !wanted.is_empty() {
            let refs: Vec<&crate::app::rooms::Attach> = wanted.iter().collect();
            self.room_pictures_wanted(&refs);
        }
        let Some(v) = self.plugin_view() else {
            // A pane with no readable view says why: an empty dashboard and a plugin this
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
        // The blobs of the pane's stream, by the id a body named: a layer names ids, and the host
        // is the half that holds the bytes.
        let mut blobs: std::collections::BTreeMap<String, crate::ui::chat::Blob> =
            std::collections::BTreeMap::new();
        for b in v
            .panels
            .iter()
            .flat_map(|p| p.summary.iter().chain(p.view.iter()))
        {
            if let pv::Block::Stream(s) = b {
                for a in crate::app::chat_plugin::stream_pictures(s) {
                    blobs.insert(
                        a.id.clone(),
                        crate::ui::chat::Blob {
                            name: String::new(),
                            picture: true,
                            size: None,
                        },
                    );
                }
            }
        }
        let picture = |blob: &str| self.room_picture(blob);
        let selected = self.plugin_selected().map(|t| t.id().to_string());
        let draw = view::PaneDraw::new(&self.cfg, &picture, &blobs)
            .selected(selected.as_deref().filter(|id| !id.is_empty()));
        let content = view::content_with(v, self.plugin_screen, self.plugin_sel, w, &draw);
        self.plugin_stop_rows = content.lines.stop_rows.clone();
        self.plugin_panel_rows = content.panels;
        self.plugin_legend_rows = content.legend;
        row_strings(&content.lines.lines, self.cfg.palette())
    }

    /// The plugin panes this head holds, by title - `/pane`'s list, and the panes list the
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

    /// The selected stop's own action letters, for the bottom bar: read off the stop under the
    /// cursor, the same value the key dispatch reads, so the bar cannot offer a letter that does
    /// nothing there. Empty when the stop has none, and the bar says the host's keys alone.
    pub(crate) fn plugin_actions_hint(&self) -> Option<String> {
        let target = self.plugin_selected()?;
        let letters: Vec<String> = target
            .actions()
            .iter()
            .filter(|a| a.letter().is_some())
            .map(|a| format!("{} {}", a.key, rano::agent::text::clean_line(&a.label)))
            .collect();
        (!letters.is_empty()).then(|| letters.join(" · "))
    }

    /// `/pane`'s listing: every plugin pane this session has, by title, with the spelling that
    /// opens it. The names are the daemon's, so a fixed list would be a second copy of them.
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

    /// The status row's badge for the pane on the screen - the pane's own counter, drawn
    /// where a turn's progress is, and only while the pane is on the screen: a counter about a
    /// screen nobody is looking at is furniture.
    pub(crate) fn plugin_badge(&self) -> Option<(String, rano::style::Role)> {
        self.plugin_view().and_then(view::badge)
    }

    // ----------------------------------------------------------------------------------------
    // The operator's own door
    // ----------------------------------------------------------------------------------------

    /// `/plugin` - every layer this session has that a person PLACED, and whether its bytes are
    /// pinned or adjudicated call by call. A built-in is not listed: a list of things that need
    /// no decision hides the ones that do.
    pub(crate) fn plugin_trust_listing(&self) -> Vec<String> {
        let slot = self.plugin_host();
        let Some(host) = slot.as_ref() else {
            return vec!["this head has no plugin stack for this folder".to_string()];
        };
        let placed = host.placed();
        if placed.is_empty() {
            return vec![
                "no plugin layers here — a built-in is part of this binary and runs freely, so \
                 there is nothing to pin in this folder"
                    .to_string(),
            ];
        }
        let mut out = vec![
            "/plugin trust NAME pins what is on disk now; a placed layer with no pin is \
             adjudicated call by call"
                .to_string(),
            String::new(),
        ];
        for layer in placed {
            let state = if layer.trusted {
                match &layer.pin {
                    Some(pin) => format!("pinned — module sha256 {}", pin.module),
                    None => "free".to_string(),
                }
            } else {
                match &layer.pin {
                    Some(pin) => format!(
                        "NOT pinned any more — pinned module sha256 {}, on disk now {}",
                        pin.module, layer.sha256
                    ),
                    None => "not pinned — every call is adjudicated".to_string(),
                }
            };
            out.push(format!(
                "  {} ({} · {})\n    {}",
                layer.manifest.name, layer.id, layer.scope, state
            ));
            out.push(format!("    /plugin trust {}", layer.manifest.name));
        }
        out
    }

    /// `/plugin trust NAME` - pin the bytes on disk now, and say which hash was pinned; any later
    /// change to those bytes un-pins the layer until it is trusted anew. This is the operator's
    /// door and not a model's: a layer that could pin itself would run freely because it said so.
    pub(crate) fn trust_plugin(&mut self, arg: &str) -> String {
        let (verb, name) = match arg.trim().split_once(char::is_whitespace) {
            Some((verb, rest)) => (verb, rest.trim()),
            None => (arg.trim(), ""),
        };
        // Build the stack first - the host is lazy, and the borrow it takes has to end before the
        // mutable one below.
        {
            let _ = self.plugin_host();
        }
        let Some(host) = self.sessions_host.get_mut().as_mut() else {
            return "this head has no plugin stack for this folder".to_string();
        };
        match verb {
            "trust" => match host.pin(name) {
                Ok((pin, wrote)) => {
                    let who = if wrote.is_empty() {
                        "nothing on this box recorded a tool writing these bytes".to_string()
                    } else {
                        format!("written by a tool: {wrote}")
                    };
                    format!(
                        "pinned `{}`: module sha256 {}, manifest sha256 {} — {who}. Any change to \
                         either un-pins it",
                        pin.name, pin.module, pin.manifest
                    )
                }
                Err(why) => why,
            },
            "untrust" => match host.unpin(name) {
                Ok(pin) => format!(
                    "`{}` is not pinned any more — module sha256 {} is adjudicated call by call \
                     from now on",
                    pin.name, pin.module
                ),
                Err(why) => why,
            },
            other => format!(
                "`/plugin {other}` is not a verb — `/plugin` lists the layers, `/plugin trust NAME` \
                 pins one, `/plugin untrust NAME` takes the pin back"
            ),
        }
    }
}

/// The word a view key is matched by: a letter of either case, lowercased so a shift and a plain
/// letter cannot be two keys the head's `Key::Char` cannot tell apart, or one of `NAMED_KEYS`.
fn view_key_word(k: &Key) -> Option<String> {
    Some(match k {
        Key::Char(c) => c.to_ascii_lowercase().to_string(),
        Key::KillToEnd => "ctrl-k".to_string(),
        Key::End => "end".to_string(),
        Key::Home => "home".to_string(),
        _ => return None,
    })
}
