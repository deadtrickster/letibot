//! **What the last frame drew, as click targets** (R69): one record per frame, and one function
//! that answers *what is under (x, y)*.
//!
//! # What this replaces
//!
//! Each clickable surface did its own hit-testing: the picker read `picker_hits`, the setting
//! card `mode_hits`, the todos and queue panes combined their recorded stop rows with the LIVE
//! `pane_scroll` and a recorded top, the count labels read `box_top_hits`, the conversation read
//! `file_rows`. Some recomputed layout arithmetic, and the review of 2026-10-11 found three
//! defects of that shape (the picker's two-line rows, the mode card under the link line, a click
//! in the same read as a wheel step combining the new scroll with the old rows). And because each
//! handler decided for itself whether a click was its own, the todos, queue, picker and setting
//! card handlers took EVERY click, hit or miss — so the count labels stopped working while one
//! of them was open. The subagents, jobs, standing and config panes, the decision card and the
//! quit card had row maps and took no clicks at all.
//!
//! # The rule
//!
//! **The frame records `(Rect, Target, rows → stops)` with the scroll already applied**, and a
//! click is answered from that record alone: the topmost rectangle under the pointer, the stop on
//! that row, that surface. A click that lands on no rectangle falls through. Nothing here reads
//! the live scroll, so a click in the same read as a wheel step is measured against the frame the
//! operator actually saw — whole, never half of one frame and half of the next — and a target
//! the keys before it took off the screen (an Esc that closed the pane) is dropped rather than
//! answered by whatever is drawn there now.

use super::*;

/// A rectangle of terminal cells, 0-based, the way a mouse report counts them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Rect {
    pub(crate) x: usize,
    pub(crate) y: usize,
    pub(crate) w: usize,
    pub(crate) h: usize,
}

impl Rect {
    pub(crate) fn contains(&self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// **What a rectangle on the last frame was.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    /// A surface from the stack, holding the conversation's rectangle.
    Surface(Surface),
    /// One label of the queue pane's view strip.
    QueueTab(QueueView),
    /// The conversation itself; its stops are the edit and write rows (`file_rows`).
    Conversation,
    /// **One row of a job's completion notice**, over the conversation; its one stop is the
    /// notice's index in `items`. A click opens the notice whole, as a click on an edit row
    /// opens its change.
    Notice,
    /// **One warning drawn as a card's line** (`line_cards`); its one stop is the card's
    /// index in `note_cards`. A click opens the detail whole, in the same popup.
    NoteCard,
    /// **The status row, when it left something out**: a flash notice cut to its line (R89), or a
    /// progress whose one-line form is not all of it. A click opens the whole of it.
    FlashNotice,
    /// **A todo row's `waits on …` link** (`todos_wait_links`), over the todos pane; its one stop
    /// is the link's index in that record. A click opens the job or the child the row waits on.
    TodoWait,
    /// **A panel's title row in a plugin's pane** — C3's *a click on a panel opens its view*. Its
    /// one stop is the panel's index in the view, and it is not a stop of the pane: a panel's
    /// title is a door, and Enter is for the rows behind it.
    PluginPanel,
    /// The decision card; its stops are the ladder's rows.
    Decision,
    /// The quit card; its stops are its two choices.
    QuitCard,
    /// The reseat card, open (`app::reseat`); its stops are its two choices.
    ReseatCard,
    /// The setting card (`/mode`, `/verbosity`, `/diff`, `/models`); its stops are its values.
    SettingCard,
    /// A count label on the composer's top edge.
    CountLabel(CountLabel),
    /// **A picture's thumbnail rows in the conversation** (`app::pictures`) — each row of the box,
    /// over the block the picture was drawn in. A click opens the picture larger, in the popup the
    /// change and the notice open in.
    Picture,
    /// **The header's git field** — its `(…)` on the header row; a click opens the repo card.
    GitField,
    /// **The merge queue's standings on the composer's bottom edge** — `3 in review · 1 being
    /// merged · 0 parked`, the words `Standings::words()` drew, with the `⚠`/`‼` alarms in front
    /// when a target could not be judged or is red. A click opens the queue pane.
    ///
    /// It is the one piece of the edge that is not about this head (see `bottom_markers`), and it
    /// is drawn on every screen — so it is the only door to the queue that a reader finds without
    /// knowing `/queue`. The rectangle is the span's own columns on the row the frame drew, read
    /// off that row (`screen::drawn_span`), because the edge truncates from the right and the
    /// standings are the last thing on it.
    QueueStandings,
}

/// **One target on the last frame**: where it was, what it was, and which of its stops each
/// screen row belonged to — `(y, stop)`, in terminal rows, scroll applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hit {
    pub(crate) rect: Rect,
    pub(crate) target: Target,
    pub(crate) stops: Vec<(usize, usize)>,
}

impl Hit {
    /// The stop drawn on screen row `y`, if any.
    pub(crate) fn stop_at(&self, y: usize) -> Option<usize> {
        self.stops.iter().find(|(r, _)| *r == y).map(|(_, s)| *s)
    }
}

impl App {
    /// **The topmost target under the pointer on the last frame**, or nothing. Later records are
    /// on top: the queue's strip over the pane it sits in, a card over nothing.
    pub(crate) fn hit_at(&self, x: u16, y: u16) -> Option<&Hit> {
        let (x, y) = (usize::from(x), usize::from(y));
        self.hits.iter().rev().find(|h| h.rect.contains(x, y))
    }

    /// **A pane's recorded stop rows, as screen rows** — pane row `r` is on the screen at
    /// `top + r - pane_scroll` while it is inside the window. Read once, at draw time, with the
    /// scroll the draw itself clamped.
    fn pane_stops(&self, rows: &[usize], top: usize, room: usize) -> Vec<(usize, usize)> {
        rows.iter()
            .enumerate()
            .filter_map(|(k, r)| {
                r.checked_sub(self.pane_scroll)
                    .filter(|d| *d < room)
                    .map(|d| (top + d, k))
            })
            .collect()
    }

    /// **The conversation's rectangle, recorded** — called by `compose_screen` straight after
    /// `main_area` drew it, so the stop rows, the scroll and `drawn()` are the ones this frame
    /// used. Starts the frame's record.
    ///
    /// The rectangle is the whole width of the terminal, gutters included: a click in the
    /// margin beside a row is a click on that row, as it always was. `gutter` is where the
    /// pane's own columns start, for the targets that are narrower than a row (the queue's
    /// strip).
    pub(crate) fn record_main_hits(
        &mut self,
        gutter: usize,
        term_w: usize,
        top: usize,
        room: usize,
    ) {
        self.hits.clear();
        self.plugin_legend_at.clear();
        let x = gutter;
        let rect = Rect {
            x: 0,
            y: top,
            w: term_w,
            h: room,
        };
        match self.drawn() {
            Drawn::Surface(s) => {
                let stops = match s {
                    Surface::Picker => self.picker_hits.clone(),
                    Surface::Todos => self.pane_stops(&self.todos_stop_rows, top, room),
                    Surface::Queue => self.pane_stops(&self.queue_stop_rows, top, room),
                    Surface::Subagents => self.pane_stops(&self.subagents_stop_rows, top, room),
                    Surface::Jobs => self.pane_stops(&self.jobs_stop_rows, top, room),
                    Surface::Monitors => self.pane_stops(&self.monitors_stop_rows, top, room),
                    Surface::PluginPane => self.pane_stops(&self.plugin_stop_rows, top, room),
                    Surface::Standing => self.pane_stops(&self.standing_stop_rows, top, room),
                    Surface::Config => self.pane_stops(&self.config_stop_rows(), top, room),
                    _ => Vec::new(),
                };
                self.hits.push(Hit {
                    rect,
                    target: Target::Surface(s),
                    stops,
                });
                // **Each `waits on …` link, over the row it sits on** — exactly the columns its
                // words were drawn in, and only while the window shows its row.
                if s == Surface::Todos {
                    for (k, l) in self.todos_wait_links.clone().into_iter().enumerate() {
                        if let Some(d) = l.row.checked_sub(self.pane_scroll)
                            && d < room
                        {
                            self.hits.push(Hit {
                                rect: Rect {
                                    x: x + l.from,
                                    y: top + d,
                                    w: l.to.saturating_sub(l.from),
                                    h: 1,
                                },
                                target: Target::TodoWait,
                                stops: vec![(top + d, k)],
                            });
                        }
                    }
                }
                // **A plugin pane's panels, over the title rows they were drawn on** — the
                // frame's own record (`plugin_panel_rows`), scroll applied, so a click lands on
                // the panel whose title the operator saw. A title scrolled out of the window is
                // not a target.
                if s == Surface::PluginPane {
                    for (row, panel) in self.plugin_panel_rows.clone() {
                        if let Some(d) = row.checked_sub(self.pane_scroll)
                            && d < room
                        {
                            self.hits.push(Hit {
                                rect: Rect {
                                    x,
                                    y: top + d,
                                    w: term_w.saturating_sub(x),
                                    h: 1,
                                },
                                target: Target::PluginPanel,
                                stops: vec![(top + d, panel)],
                            });
                        }
                    }
                    // **The rows a legend documents, in the terminal's rows** — the record a
                    // hover reads to answer *what does this label mean* (C3's *the same text
                    // answers on hover*). Not a hit: a legend row is documentation, and a click
                    // on it acts on nothing, so it wants no hand pointer and no target. The
                    // translation lives here because this is the one place that knows the pane's
                    // top and its scroll.
                    self.plugin_legend_at = self
                        .plugin_legend_rows
                        .iter()
                        .filter_map(|(row, key)| {
                            row.checked_sub(self.pane_scroll)
                                .filter(|d| *d < room)
                                .map(|d| (top + d, key.clone()))
                        })
                        .collect();
                }
                // **The strip's labels, over the pane they sit in** — only while the window shows
                // the strip's row; a strip scrolled away is not a target.
                if s == Surface::Queue
                    && let Some(tabs) = self.queue_tabs.clone()
                    && let Some(d) = tabs.row.checked_sub(self.pane_scroll)
                    && d < room
                {
                    for (view, (from, to)) in [
                        (QueueView::Review, tabs.review),
                        (QueueView::Merge, tabs.merge),
                    ] {
                        self.hits.push(Hit {
                            rect: Rect {
                                x: x + from,
                                y: top + d,
                                w: to.saturating_sub(from),
                                h: 1,
                            },
                            target: Target::QueueTab(view),
                            stops: Vec::new(),
                        });
                    }
                }
            }
            Drawn::Conversation => {
                let stops = self
                    .file_rows
                    .iter()
                    .enumerate()
                    .map(|(k, (row, _))| (*row, k))
                    .collect();
                self.hits.push(Hit {
                    rect,
                    target: Target::Conversation,
                    stops,
                });
                // **The notice rows, over the conversation they sit in** — one rectangle a row,
                // the whole width, like the conversation's own: later records are on top.
                for (y, k) in self.notice_rows.clone() {
                    self.hits.push(Hit {
                        rect: Rect {
                            x: 0,
                            y,
                            w: term_w,
                            h: 1,
                        },
                        target: Target::Notice,
                        stops: vec![(y, k)],
                    });
                }
                for (y, i) in self.card_rows.clone() {
                    self.hits.push(Hit {
                        rect: Rect {
                            x: 0,
                            y,
                            w: term_w,
                            h: 1,
                        },
                        target: Target::NoteCard,
                        stops: vec![(y, i)],
                    });
                }
                // **A picture's rows, over the block it was drawn in** — one rectangle a row, the
                // whole width, like a notice's: the thumbnail is a box of placeholder cells whose
                // columns a person cannot see, so the row IS the target.
                for (y, k) in self.picture_rows.clone() {
                    self.hits.push(Hit {
                        rect: Rect {
                            x: 0,
                            y,
                            w: term_w,
                            h: 1,
                        },
                        target: Target::Picture,
                        stops: vec![(y, k)],
                    });
                }
            }
            // rano takes its own mouse reports before `App::key` (`App::route`), a `!term` pane
            // is handed none, and the diff popup owns every key while it is up.
            Drawn::Term | Drawn::DiffPopup | Drawn::Editor => {}
        }
    }

    /// **A card in the chrome, recorded** where the frame put it: `top` is its first screen row,
    /// `stops` its rows relative to that. Rows past the frame are dropped.
    pub(crate) fn record_card_hit(
        &mut self,
        target: Target,
        rect: Rect,
        stops: impl IntoIterator<Item = (usize, usize)>,
        h: usize,
    ) {
        let stops: Vec<(usize, usize)> = stops.into_iter().filter(|(y, _)| *y < h).collect();
        let rect = Rect {
            h: rect.h.min(h.saturating_sub(rect.y)),
            ..rect
        };
        if rect.h > 0 {
            self.hits.push(Hit {
                rect,
                target,
                stops,
            });
        }
    }
}
