//! The frame: everything the head draws, composed into the terminal's rows - the header,
//! the conversation, the cards and panes, the composer and the hint bar - and where the cursor goes.

use crate::app::*;
use crate::ui::panes::queue::Standings;
use crate::ui::render::{row, row_strings, trim_to, visible_width, wrap};
use crate::ui::*;
use rano::agent::composer::{BoxBottom, BoxTop};
use rano::agent::screens;
use rano::style::Role;

impl App {
    /// One frame: `h` lines of at most `w` columns, and while the view is held the same frame every time. The
    /// hold is a wrapper because the frame is composed once and returned byte for byte, which is what lets a
    /// mouse selection survive a streaming turn. See [`App::toggle_hold`].
    pub fn screen(&mut self, term_w: usize, h: usize) -> Vec<String> {
        if self.hold
            && self.hold_size == (term_w, h)
            && let Some(frame) = &self.hold_frame
        {
            // The held frame, with the pointer's marks on it - a selection made on a held view
            // (and a drag holds the view, `app::pointer`) changes only the rows it marks.
            let mut out = frame.clone();
            self.relight();
            self.paint_pointer(&mut out);
            return out;
        }
        let mut out = self.compose_screen(term_w, h);
        // Every key hint on this frame is a button, read back off the frame itself - see
        // `app::keyhints`. Before the links pass, which only adds escapes.
        self.scan_hints(&out);
        // Every link on this frame is a link (OSC 8) where the terminal speaks it, and a
        // target - see `App::draw_links` and `app::links`.
        self.draw_links(&mut out);
        if self.hold {
            // The marker takes the hint bar's row. It is one row that always exists, so the
            // freeze costs no height and reflows nothing - and it is the row that already talks
            // about keys, which is the row a held view's one sentence belongs on.
            let gutter = Self::gutter(term_w);
            let w = term_w.saturating_sub(2 * gutter).max(1);
            let marker = row(
                &rano::agent::text::one(trim_to(HOLD_MARKER, w), Role::Attention),
                self.cfg.palette(),
            );
            let row = if gutter > 0 {
                format!("{}{marker}", " ".repeat(gutter))
            } else {
                marker
            };
            if let Some(last) = out.last_mut() {
                *last = row;
            }
            self.hold_size = (term_w, h);
            self.hold_frame = Some(out.clone());
        }
        // The pointer's marks, last - the hover and the selection - and after the held frame
        // is kept, so what is kept is the frame itself and the marks are painted on every serve.
        // See `app::pointer`.
        self.relight();
        self.paint_pointer(&mut out);
        out
    }

    /// The links on a finished frame, drawn and recorded (OSC 8), in three passes that are each rano's own
    /// decision about what a link is: the URLs, then `path:line`, then the words rano recognises. The frame is
    /// read back for the pairs it carries, since a link is a target only where it was drawn.
    fn draw_links(&mut self, out: &mut [String]) {
        if !self.features.links {
            self.link_targets.clear();
            return;
        }
        let root = self.cfg.links.clone();
        let refs = self.gh_refs.clone();
        for l in out.iter_mut() {
            // The URLs first, so a `path:line` inside one is not read as a file: the passes under this one leave
            // text inside a link already drawn alone, which is what makes `https://example.com/x:8080` a URL.
            if l.contains("://") {
                *l = rano::term::links::link_urls(l);
            }
            if let Some(root) = root.as_deref()
                && l.contains(':')
                && let Some(linked) = crate::app::link_path_lines(l, root)
            {
                *l = linked;
            }
            if let Some(refs) = &refs {
                let mut url_of = |w: &str| refs.url_of(w);
                if let Some(linked) = rano::term::links::link_words(l, &mut url_of) {
                    *l = linked;
                }
            }
        }
        for (y, row) in out.iter().enumerate().take(self.body_rows) {
            if !row.contains("\u{1b}]8;") {
                continue;
            }
            for (from, to, url) in crate::app::osc8_spans(row) {
                let k = self.link_targets.len();
                self.link_targets.push(url);
                self.hits.push(Hit {
                    rect: Rect {
                        x: from,
                        y,
                        w: to - from,
                        h: 1,
                    },
                    target: Target::Link,
                    stops: vec![(y, k)],
                });
            }
        }
    }

    pub(crate) fn compose_screen(&mut self, term_w: usize, h: usize) -> Vec<String> {
        // The gutter, applied to the whole frame rather than to the transcript: insetting only the body would
        // leave the header and the composer's box a different distance from the edge, which is what a reader
        // notices.
        let gutter = Self::gutter(term_w);
        let w = term_w - 2 * gutter;
        self.cfg.width = w;
        self.cfg.links = self
            .features
            .links
            .then(|| self.wiring.workspace.clone())
            .filter(|w| !w.is_empty());
        self.cfg.light = self.features.background && self.light_background == Some(true);
        // Pictures in the one form this head can draw: the kitty protocol's Unicode placeholders, rows of
        // ordinary text a terminal fills from bytes it holds, which is what makes a picture survive a scroll and
        // a redraw for one upload. Hence the gate is the PROTOCOL and not `features.images`.
        self.cfg.images = matches!(
            self.proto,
            rano::term::Proto::Kitty | rano::term::Proto::KittyPassthrough
        );
        if self.cfg.images {
            self.queue_image_uploads();
        }
        // The terminal's width beside the frame's: `cfg.width` is the inner one, the gutter already taken off,
        // so anything re-rendering from a stored size has to start from this one.
        self.term_cols = term_w;
        let h = h.max(1);
        // Click mapping has to redo this frame's arithmetic without a repaint, so the height it was composed for
        // is the fact it needs. A height the history was not rendered for is stale rows and not just a stale map:
        // a tool row's payload window is sized from the terminal's height.
        let resized = self.screen_rows != h;
        self.screen_rows = h;
        if resized {
            self.invalidate_history();
        }
        // The notice's clock, read rather than decremented: `say` sets a deadline and a key moves it to now, so
        // the sentence's lifetime is wall time and this is the frame either takes effect on. See [`NOTICE_MS`].
        if let Some(until) = self.notice_until
            && self.now_ms >= until
        {
            self.notice = None;
            self.notice_until = None;
        }
        // A fleeting note's clock, read the same way and here for the same reason: the rebuild is what un-draws
        // the row, on the frame the deadline passes and not on every frame after it. See [`App::fade_fleeting_notes`].
        if self.fade_fleeting_notes() {
            self.invalidate_history();
        }

        // The card is two pieces and only one gives way: `dec` is the content, which a viewport shrinks and
        // scrolls, and `dec_pinned` is the answer, which the fit loop may not touch. A card that dropped its
        // choices is a question with no way to answer it. See [`App::decision_card`].
        let shown_prompt = if self.prompt_away {
            None
        } else {
            self.prompt.as_ref()
        };
        // A password card Esc put away is not drawn, exactly as a prompt card is not: the ask
        // stays open at the daemon, and the slot falls to whatever waits behind it.
        let shown_secret = if self.secret_away {
            None
        } else {
            self.secret.as_ref()
        };
        // Which rows of the card are which value's, set only when the card in the slot IS
        // the setting card - see `mode_hits` below.
        let mut pick_spans: Option<Vec<std::ops::Range<usize>>> = None;
        // Which card the slot holds, as a click target - set by the arm that drew it,
        // so the record names the card on the screen and not one that is merely open.
        let mut card_target: Option<Target> = None;
        let (dec, dec_pinned): (Vec<String>, Vec<String>) =
            match (shown_secret, shown_prompt, self.open.first()) {
                (Some(ask), _, _) => (self.secret_lines(ask, w), Vec::new()),
                // The confirmation that ends a pane rides the same slot and comes second, ahead of the prompt
                // card: a `!term close` can only be typed while no prompt card is up, so this arm is about which
                // card is drawn.
                (None, _, _) if self.term_ask.is_some() => (
                    self.term_ask_lines(self.term_ask.as_ref().expect("just checked"), w),
                    Vec::new(),
                ),
                // The prompt card rides the same slot and comes second: a password ask is `sudo` blocking the run
                // it is inside, and when both are up the secret is the one that must not be typed past. See
                // [`App::prompt_lines`].
                (None, Some(ask), _) => (self.prompt_lines(ask, w), Vec::new()),
                // A key the picker asked for rides here too, ahead of a decision and the pickers: it owns the
                // keyboard while it is up, and the slot draws the card the keys go to. A decision drawn here while
                // every key typed the key was a screen that lied about the keys.
                (None, None, _) if self.key_ask.is_some() => (self.key_ask_lines(w), Vec::new()),
                // The new-todo card rides in the ask card's slot too, ahead of a decision and
                // the quit card, for the same reason: Enter and Esc are its own while it is up.
                (None, None, _) if self.todo_draft.is_some() => {
                    (self.todo_card_lines(w), Vec::new())
                }
                (None, None, Some(d)) => {
                    card_target = Some(Target::Decision);
                    self.decision_card(d, w)
                }
                // The mode card rides the ask card's slot: a compact card at the bottom with the transcript still
                // above it. The two are never up at once, since a decision owns the ladder keys, so the card waits
                // out an ask and comes back.
                (None, None, None) if self.quit_card => {
                    card_target = Some(Target::QuitCard);
                    (self.quit_card_lines(w), Vec::new())
                }
                (None, None, None) if self.pick.is_some() => {
                    let (rows, spans) = self.setting_picker_layout(w);
                    pick_spans = Some(spans);
                    card_target = Some(Target::SettingCard);
                    (rows, Vec::new())
                }
                // The reseat card comes last: it is not a question with a deadline, and
                // anything else that wants the slot is more urgent. While one does, the card is
                // drawn as its pinned strip, so a stale prompt is never off the screen.
                (None, None, None) if self.reseat_card_up() => {
                    card_target = Some(Target::ReseatCard);
                    self.reseat_card_lines(w)
                }
                (None, None, None) => (Vec::new(), Vec::new()),
            };
        let dec_full = dec.len();
        // The window the key handler asks about, set every frame: the content's length is a function of the
        // width and the room of the terminal, and the scroll is reset in one place here so a second card cannot
        // inherit the first one's offset.
        let card_here = format!(
            "{card_target:?}:{}",
            self.open
                .first()
                .map(|d| d.req_id.as_str())
                .unwrap_or_default()
        );
        if card_here != self.dec_scroll_for {
            self.dec_scroll_for = card_here;
            self.dec_scroll = 0;
        }
        // The link line, read with the other chrome rather than in the body: it is a state of the connection and
        // not a row of the conversation, and it belongs where the eye crosses on the way to the composer.
        let link = self.link_line(w);
        // A read-only attach says so in the same slot, on the link line's budget, since it is the same kind of
        // fact: a state of the connection, never given up by the fit ladder while true. The chord it spells is
        // the one act that changes it.
        let lock = self.lock_line(w);
        // A stood-down auto-compaction rides the same slot for the same reason: a state of the session, never
        // given up by the fit ladder while it is true. See [`App::compact_off_line`].
        let compact_off = self.compact_off_line(w);
        // A paused plan-check rides the same slot, for the same reason - see
        // [`App::nag_pause_line`]: a state of the session, never given up by the fit ladder while
        // it is true, so a silent board is never read as a broken one.
        let nag_pause = self.nag_pause_line(w);
        // A board waiting on the operator rides the same slot - see [`App::waiting_line`]: a
        // state, never given up by the fit ladder while a person is being waited on.
        let waiting = self.waiting_line(w);
        // A stale prompt the operator said `not now` to rides the same slot - the reseat
        // card pinned (`app::reseat`): a state, never given up by the fit ladder while it is
        // true, so it keeps staring while the operator keeps working.
        let reseat_strip = self.reseat_strip(w);
        // A stop the person ordered is the loudest thing on the screen while it lasts, above the link line
        // because the two share the slot and only one can be true: `link_down` refuses to run while a stop is in
        // flight.
        let stopping = self.stopping_line(w);
        // A pane this head is not drawing, and the fact that it is still running: a statement about now, drawn
        // while true and gone when it stops. Nothing is drawn while the pane is on the screen, since the pane
        // itself is the fact. See [`App::pane_behind`].
        let pane = self
            .pane_behind()
            .map(|line| row(&screens::pane_behind(&line, w), self.cfg.palette()));
        // The turn's status is a row of its own immediately above the composer: an edge truncates, so a
        // legend in the border would lose the words that matter to a trim.
        let (status, click) = self.status_row(w);
        let row_clicks = click.is_some();
        self.status_click = click;
        let mut notice_at: Option<usize> = None;
        // Live slash-command matches, one dim row above the composer: a typing aid, not a message. It is a SLOT
        // and not an appearance, because a row that comes and goes takes its line from the transcript and hands
        // it back, and the reader's page moves.
        let completions = self.completions_legend(w);

        let Fit {
            rows,
            hint,
            show_notice: _,
            show_stuck: _,
            show_pane,
            show_status,
            boxed,
            content_rows,
        } = fit_ladder(&FitInput {
            h,
            composer: self.editor.height(self.composer_cols(), h),
            card: dec.len(),
            pinned: dec_pinned.len(),
            // Never a row of its own - the notice is drawn in the status row, which is
            // reserved whether or not anything is in it (see `let status`).
            notice: false,
            // No stuck sentence any more: a slow turn is the Responding row in yellow.
            stuck: false,
            pane: pane.is_some(),
            // The completions live in the box's edge now; they never take a row.
            completion_slot: false,
            link: link.len()
                + lock.len()
                + compact_off.len()
                + nag_pause.len()
                + waiting.len()
                + reseat_strip.len(),
            stopping: stopping.len(),
            alarmed: self.alarmed(),
        });

        let (input_rows, caret_row, caret_col) = self.composer_rows(w, rows, boxed);
        let mut chrome: Vec<String> = Vec::new();
        // A head with no daemon says so first, ahead of the `allow-all` confirmation and the decision card: it is
        // the reason every other line is not moving, and a card read without it is a question nothing waits on.
        chrome.extend(stopping);
        chrome.extend(link);
        chrome.extend(lock);
        chrome.extend(compact_off);
        chrome.extend(nag_pause);
        chrome.extend(waiting);
        chrome.extend(reseat_strip);
        // The `allow-all` confirmation sits at the front of the chrome, because while it is up every key belongs
        // to it. Wrapped rather than trimmed: this one is read, not glanced at.
        if let Some(line) = self.mode_confirm_line() {
            chrome.extend(row_strings(&screens::asking(&line, w), self.cfg.palette()));
        }
        // The content as a window, then the answer in full underneath: the seam says how many lines are out of
        // view and names the keys that move, and it is drawn only when there is something out of view.
        // `card_window` clamps the scroll to what this frame has.
        let (card_rows, _) = self.card_window(w, &dec, content_rows);
        // Where the card starts in the chrome: BELOW the stopping, link, lock, compact-off and
        // confirmation lines pushed above. The click is measured from the top of the
        // chrome plus those rows.
        let card_chrome_at = chrome.len();
        let (card_rows_len, pinned_len) = (card_rows.len(), dec_pinned.len());
        chrome.extend(card_rows);
        chrome.extend(dec_pinned);
        if show_pane && let Some(l) = pane {
            chrome.push(l);
        }
        // The turn's own row, last before the box: directly above the composer when nothing else
        // is up, and below the typing aids when they are - a completion list that is not adjacent
        // to the line being typed is the one row here that must not move.
        if show_status {
            notice_at = Some(chrome.len());
            chrome.push(status);
        }
        let mut box_top_at: Option<(usize, BoxTopLabels)> = None;
        if boxed {
            // The top edge carries the facts that exist only while they are true, pinned right: subagents this
            // session spawned and jobs it started. A legend here would be a row of attention paid
            // for ever for facts read once.
            let (edge, labels) = self.box_top(w);
            box_top_at = Some((chrome.len(), labels));
            chrome.push(edge);
        }
        let caret_at = chrome.len() + caret_row;
        chrome.extend(input_rows);
        // The bottom edge's standings, and where on the edge they were drawn - kept the way
        // `box_top_at` keeps the count labels', because the fit and the backstop below can still
        // move the row, so the row is resolved once the frame's final shape is known.
        let mut standings_at: Option<(usize, (usize, usize))> = None;
        if boxed {
            // The bottom edge, pinned right: the alarm as a triangle, since the counters behind it are
            // `/status`'s. The turn's own status is a row above the box now, because a legend on an edge truncates.
            let (edge, at) = self.box_bottom(w, completions.as_ref());
            standings_at = at.map(|cols| (chrome.len(), cols));
            chrome.push(edge);
        } else if self.alarmed() {
            chrome.push(self.status_line(w));
        }
        if hint {
            chrome.push(self.hint_bar(w));
        }
        // The setting card's click facts are recorded below, once the frame's final shape is
        // known (`mode_hits`).
        let pre_chrome = chrome.len();
        // Backstop. The ladder above cannot always win - `h` can be 2 - and a head
        // that returns more lines than the terminal has scrolls its own composer
        // off the bottom.
        if chrome.len() >= h {
            chrome.drain(..chrome.len() - h.max(1));
        }
        // Clicks on the setting card are trusted only when the whole card survived, since a click into a list
        // nobody saw whole would pick a value nobody saw. The arrows work either way.
        let pick_whole = content_rows == dec_full && pre_chrome == chrome.len();

        // The session header, pinned above everything: one row, and both surveyed heads spend their first on
        // it. It costs a row of transcript and answers which session this is and how big it has grown, which is
        // the question a reader has after a switch.
        let (header, header_path) = if h >= 6 && !self.session_id.is_empty() {
            let (row, path) = self.header_row(w);
            (Some(row), path)
        } else {
            (None, None)
        };
        let room = h
            .saturating_sub(chrome.len() + usize::from(header.is_some()))
            .max(1);
        let mut out = self.main_area(w, room, header.is_some());
        // The frame's click record starts here, with what `main_area` just drew - its stop
        // rows and its scroll are this frame's. See `app::hits`.
        self.record_main_hits(gutter, term_w, usize::from(header.is_some()), room);
        while out.len() < room {
            out.push(String::new());
        }
        out.truncate(room);
        let mut body_rows = out.len();
        if let Some(l) = header {
            out.insert(0, l);
            body_rows += 1;
        }
        // The body's height, kept on the head - the first row of the chrome is where a link
        // stops being a target: a URL the operator is typing is text, and a click in the middle
        // of it belongs to the caret. See [`App::draw_links`].
        self.body_rows = body_rows;
        // Rows the backstop took off the front of the chrome, read before the extend
        // below consumes it - the edge's index has to come down by exactly this many.
        let drained = pre_chrome - chrome.len();
        // The setting card's hit rows, `(y, k)` for every row value `k` was drawn on, its `why` lines included,
        // placed where THIS frame put the card. Empty unless the card is the setting card and was drawn whole.
        self.mode_hits = match pick_spans {
            Some(spans) if pick_whole => {
                let top = body_rows + card_chrome_at;
                spans
                    .iter()
                    .enumerate()
                    .flat_map(|(k, span)| span.clone().map(move |r| (top + r, k)))
                    .filter(|(y, _)| *y < h)
                    .collect()
            }
            _ => Vec::new(),
        };
        // The card in the slot, as a click target, and only on a frame whose chrome the backstop did not drain:
        // rows measured from a front that was cut are rows nobody can see. The decision's ladder is its answer
        // half, which the fit never trims; the quit card's choices need the whole card.
        // Cleared first: a frame that drew no card must not leave the last one armed as the click's provenance.
        self.drawn_card = None;
        if drained == 0
            && let Some(target) = card_target
        {
            // Which card this frame drew and, for a decision, which request's rows - the hits'
            // provenance, checked again when the click arrives.
            self.drawn_card = Some((
                target,
                self.open
                    .first()
                    .map_or(String::new(), |d| d.req_id.clone()),
            ));
            let top = body_rows + card_chrome_at;
            let stops: Vec<(usize, usize)> = match target {
                Target::Decision => match self.open.first().cloned() {
                    Some(d) => self
                        .choice_rows(&d, w)
                        .into_iter()
                        .enumerate()
                        .flat_map(|(k, span)| span.map(move |r| (top + card_rows_len + r, k)))
                        .collect(),
                    None => Vec::new(),
                },
                Target::QuitCard if content_rows == dec_full => {
                    let card = self.quit_view();
                    let [one, two] = card.choice_rows(w);
                    // The card's OWN height: the consequence sentence appended under it belongs
                    // to the choice above and is not a target for it.
                    let own = card.lines(w).len();
                    (one..two)
                        .map(|r| (top + r, 0))
                        .chain((two..own).map(|r| (top + r, 1)))
                        // One stop for the consequence sentence appended under the choices, so
                        // the block above cannot reach down and arm it.
                        .chain(std::iter::once((top + own, NO_CHOICE)))
                        .collect()
                }
                Target::SettingCard => self.mode_hits.clone(),
                // The two rows open the answer half, which the fit never trims - each as many
                // lines as it wrapped to.
                Target::ReseatCard => {
                    let [one, two] = self.reseat_choice_lines(w).map(|r| r.len());
                    let at = top + card_rows_len;
                    (at..at + one)
                        .map(|r| (r, 0))
                        .chain((at + one..at + one + two).map(|r| (r, 1)))
                        .collect()
                }
                _ => Vec::new(),
            };
            let rect = Rect {
                x: 0,
                y: top,
                w: term_w,
                h: card_rows_len + pinned_len,
            };
            self.record_card_hit(target, rect, stops, h);
        }
        out.extend(chrome);
        out.truncate(h);
        // The count labels' hit boxes, recorded where the frame's final shape is known: the backstop may have
        // drained rows off the front of the chrome and the truncate may have cut its tail, and the edge is a
        // target only while it is still in the frame. The columns are the row's own plus the gutter.
        self.box_top_hits = box_top_at.and_then(|(at, labels)| {
            let at = at.checked_sub(drained)?;
            let row = body_rows + at;
            (row < out.len()).then_some(BoxTopHits {
                row,
                subagents: labels.subagents.map(|(col, w)| (col + gutter, w)),
                jobs: labels.jobs.map(|(col, w)| (col + gutter, w)),
                monitors: labels.monitors.map(|(col, w)| (col + gutter, w)),
            })
        });
        // The cut flash notice, as a target - where this frame put it, if the backstop and the
        // truncate left it on the frame.
        if row_clicks
            && let Some(row) = notice_at
                .and_then(|at| at.checked_sub(drained))
                .map(|at| body_rows + at)
                .filter(|row| *row < out.len())
        {
            self.hits.push(Hit {
                rect: Rect {
                    x: 0,
                    y: row,
                    w: term_w,
                    h: 1,
                },
                target: Target::FlashNotice,
                stops: vec![(row, 0)],
            });
        }
        // The corner's repository, as a target: the span the header drew for the workspace path, on row 0,
        // since that is where the header is inserted. The gutter is added here because `out`'s rows have none
        // yet and a click's `x` is in terminal cells.
        if let Some((col, cols)) = header_path {
            self.hits.push(Hit {
                rect: Rect {
                    x: col + gutter,
                    y: 0,
                    w: cols,
                    h: 1,
                },
                target: Target::HeaderRepo,
                stops: Vec::new(),
            });
        }
        // The header's git field, as a target, found on the drawn row by its own text so wherever the fit put
        // it is where it is. The columns are the row's own plus the gutter, since this record is made before the
        // pad is inserted; missing here, the field was measurably two columns left of what it drew.
        if let Some(pieces) = &self.git {
            let text: String = pieces.iter().map(|(t, _)| t.as_str()).collect();
            let want: Vec<char> = format!("({text}").chars().collect();
            'rows: for (y, row) in out.iter().enumerate().take(2) {
                let cells = crate::app::keyhints::cells(row);
                for x in 0..cells.len().saturating_sub(want.len() - 1) {
                    if cells[x..x + want.len()] == want[..] {
                        let end = cells[x..]
                            .iter()
                            .position(|c| *c == ')')
                            .map_or(want.len(), |e| e + 1);
                        self.hits.push(Hit {
                            rect: Rect {
                                x: x + gutter,
                                y,
                                w: end,
                                h: 1,
                            },
                            target: Target::GitField,
                            stops: Vec::new(),
                        });
                        break 'rows;
                    }
                }
            }
        }
        // And the labels as targets, from the same record.
        if let Some(b) = self.box_top_hits {
            for (label, at) in [
                (CountLabel::Subagents, b.subagents),
                (CountLabel::Jobs, b.jobs),
                (CountLabel::Monitors, b.monitors),
            ] {
                if let Some((x, lw)) = at {
                    self.hits.push(Hit {
                        rect: Rect {
                            x,
                            y: b.row,
                            w: lw,
                            h: 1,
                        },
                        target: Target::CountLabel(label),
                        stops: Vec::new(),
                    });
                }
            }
        }
        // The standings on the bottom edge, as a target: its rectangle is the span's own columns on the row
        // rano drew it on, resolved after the backstop and the truncate, since a target for words the frame cut
        // is a click nobody can see land.
        if let Some((at, (col, cols))) = standings_at
            && let Some(row) = at
                .checked_sub(drained)
                .map(|at| body_rows + at)
                .filter(|row| *row < out.len())
        {
            self.hits.push(Hit {
                rect: Rect {
                    x: col + gutter,
                    y: row,
                    w: cols,
                    h: 1,
                },
                target: Target::QueueStandings,
                stops: Vec::new(),
            });
        }
        // The caret is the affordance. It goes where the composer says, and the
        // terminal draws it as a steady block because `term::enter` asked for one.
        self.cursor = Some((
            (body_rows + caret_at).min(out.len().saturating_sub(1)),
            caret_col.min(w.saturating_sub(1)) + gutter,
        ));
        // While the editor pane has the keyboard, the caret is rano's - where it put it, or
        // hidden when rano hides it (over its diff view, a list, a help page).
        if self.editor_focused() {
            self.cursor = self.edit_pane.as_ref().and_then(|p| p.caret);
        }
        // And while the diff popup is open there is no caret: the keys are the popup's, and it is a visible
        // cursor the terminal moves through every row a scroll rewrites.
        if self.diff_popup.is_some() && self.term.is_none() {
            self.cursor = None;
        }
        // The gutter and the trim, in one pass and in place: `collect()` built a second `Vec` of `h` elements
        // and `trim_to` returns an owned `String` even when the line already fits, which is two allocations per
        // line per frame.
        let pad = " ".repeat(gutter);
        for l in &mut out {
            if visible_width(l) > w {
                *l = trim_to(l, w);
            }
            // The pad goes on in place - one reallocation that may extend the existing buffer,
            // rather than a second `String` that leaves the first to be freed.
            if gutter > 0 && !l.is_empty() {
                l.insert_str(0, &pad);
            }
        }
        out
    }

    /// The gutter this terminal can afford. It is the first thing given up on a
    /// very narrow screen, before any content is: four columns out of forty is a
    /// tenth of the line, and out of twenty it is a fifth.
    pub(crate) fn gutter(w: usize) -> usize {
        if w >= 40 { Self::GUTTER } else { 0 }
    }

    /// Where the terminal's own caret belongs, from the last [`App::screen`].
    pub fn cursor(&self) -> Option<(usize, usize)> {
        self.cursor
    }
}

/// The one sentence a held view says, in the words the two heads agreed on, since a person who learns it on
/// one reaches for it on the other. It takes the hint bar's row and names the key that undoes the hold, which
/// follows the rule for a disclosure: it carries the act that ends it.
pub const HOLD_MARKER: &str = "⏸ the view is held — ctrl-p follows again";

/// One count label's columns in the drawn edge, in the row's own columns. The facts are rano's wording, joined
/// and inlaid at the edge's right end, so this walks the spans rano built and finds the fact's boundaries around
/// the `needle` it is known by, which makes the label the whole fact and its tail with it.
fn box_top_label(line: &rano::render::Line, needle: &str) -> Option<(usize, usize)> {
    let mut col = 0usize;
    for span in &line.spans {
        let text = span.content.as_str();
        if let Some(at) = text.find(needle) {
            let start = text[..at].rfind(" · ").map_or(0, |sep| sep + 3);
            let end = text[start..]
                .find(" · ")
                .map_or(text.len(), |next| start + next);
            return Some((
                col + visible_width(&text[..start]),
                visible_width(&text[start..end]),
            ));
        }
        col += visible_width(text);
    }
    None
}

/// Where a drawn row carries `words`, in the row's own columns, or `None` when this frame did not draw them
/// whole. The edge is truncated from the right on a narrow terminal and the standings are the last thing on it,
/// so the cut eats them first, and a target for words the frame cut is a click nobody can see land.
fn drawn_span(line: &str, words: &str) -> Option<(usize, usize)> {
    let cells = crate::app::keyhints::cells(line);
    let want: Vec<char> = words.chars().collect();
    let at = (0..=cells.len().checked_sub(want.len())?)
        .find(|i| cells[*i..*i + want.len()] == want[..])?;
    let before: String = cells[..at].iter().collect();
    let span: String = cells[at..at + want.len()].iter().collect();
    Some((visible_width(&before), visible_width(&span)))
}

impl App {
    /// The composer box's top edge, carrying what this session has running, and the labels' columns read off the
    /// line rano returned: the line is the thing on the screen, rano pins the labels right and truncates them, and
    /// both move a target that arithmetic over the counts would misplace.
    pub(crate) fn box_top(&self, w: usize) -> (String, BoxTopLabels) {
        // The number is the rows the pane draws, and not a second rule about them: the one
        // lifecycle predicate ([`SubagentState::is_finished`]) the pane's own active group is
        // built from - *"an agent is alive from spawn until it has finished"*.
        let top = BoxTop {
            subagents_running: self.subagents.iter().filter(|s| !s.is_finished()).count(),
            jobs_running: self.jobs.iter().filter(|j| j.running).count(),
            jobs_to_a_file: self
                .jobs
                .iter()
                .filter(|j| j.running && j.redirect.is_some())
                .count(),
            monitors_live: self.monitors.iter().filter(|m| m.live).count(),
        };
        let line = top.line(w);
        let labels = BoxTopLabels {
            subagents: box_top_label(&line, "subagent"),
            jobs: box_top_label(&line, "job"),
            monitors: box_top_label(&line, "monitor"),
        };
        (crate::ui::rows::row(&line, self.cfg.palette()), labels)
    }

    /// The composer box's bottom edge, carrying the alarm, where the reader is, the rung and the queue's
    /// standings, and where the standings were drawn, since that span is a click target. Read off the line rano
    /// returned: the edge is cut from the right and the standings are the last thing on it.
    pub(crate) fn box_bottom(
        &self,
        w: usize,
        completions: Option<&rano::render::Line>,
    ) -> (String, Option<(usize, usize)>) {
        // The alarm, then the viewport's state only when it is holding, then the rung only when it hides things:
        // each drawn only when it is news, since a marker always on is furniture. The standings follow the rule
        // from the other side, drawn while the queue holds something that stands.
        let bottom = BoxBottom {
            alarmed: self.alarmed(),
            holding: self.scroll_state().is_some(),
            rung: self.rung_state(),
        };
        let standings = self.queue_standings();
        let mut right = bottom_markers(
            &bottom,
            &standings,
            self.asks_folded_count(),
            self.plugin_badge(),
        );
        // The step is what the edge can spare. rano cuts the block's tail to `w - 5` cells, so a
        // step that does not fit would take `stuck` and `done` with it, and the click target, which
        // is read off words drawn whole, with them. The six counts keep their columns instead.
        let mut words = standings.words();
        if standings.gate.is_some()
            && right
                .as_ref()
                .is_some_and(|r| r.width() > w.saturating_sub(5))
        {
            let bare = Standings {
                gate: None,
                ..standings
            };
            words = bare.words();
            right = bottom_markers(
                &bottom,
                &bare,
                self.asks_folded_count(),
                self.plugin_badge(),
            );
        }
        let p = self.cfg.palette();
        let edge = match completions.filter(|l| l.width() > 0) {
            // One builder for both paths: rano's `BoxBottom::line` composes the same right side, and using the
            // local builder keeps the edge from saying one thing with a completion open and another without.
            None => crate::ui::rows::row(
                &rano::agent::composer::box_edge(
                    w,
                    '╰',
                    '╯',
                    &rano::render::Line::default(),
                    &right.unwrap_or_default(),
                ),
                p,
            ),
            Some(legend) => {
                // The completions inlaid at the left: the right side's markers keep their room, and the
                // completions are cut to what is left, so typing `/` never hides an alarm or a standing queue.
                let keep = right.as_ref().map_or(0, |r| r.width() + 4);
                let room = w.saturating_sub(6 + keep).max(8);
                let mut left = legend.clone();
                if let Some(first) = left.spans.first_mut() {
                    first.content = first.content.trim_start().to_string();
                }
                let left = rano::render::text::truncate(&left, room);
                crate::ui::rows::row(
                    &rano::agent::composer::box_edge(
                        w,
                        '╰',
                        '╯',
                        &left,
                        &right.unwrap_or_default(),
                    ),
                    p,
                )
            }
        };
        // Where the standings landed, off the finished row - the target's rectangle is the
        // span's own columns, and the only thing that knows them is the line the terminal is
        // about to draw.
        let at = words.and_then(|words| drawn_span(&edge, &words));
        (edge, at)
    }

    /// What fills the screen above the cards: the terminal pane, an open output or pane,
    /// the help, the picker - or, when none of them is open, the conversation.
    pub(crate) fn main_area(&mut self, w: usize, room: usize, has_header: bool) -> Vec<String> {
        // The rectangle in the terminal's cells, whatever is drawn in it, and the click map emptied: only the
        // conversation's branch below fills it, so a click is never measured against rows a pane is covering.
        let (x, y) = (Self::gutter(self.term_cols), usize::from(has_header));
        self.edit_area = crate::ui::editor::editor_inner(x, y, w, room).into();
        self.file_rows.clear();
        self.notice_rows.clear();
        // And a finished child's lines with them - a frame with no conversation on it draws no
        // settlement, and a click measured against the last frame's rows would switch to a child
        // from under whatever is covering it now.
        self.task_rows.clear();
        self.task_switches.clear();
        self.card_rows.clear();
        // And the turn's call cards go with them - a frame with no conversation on it draws
        // no live pane, and a click measured against the last frame's rows would open a call
        // under whatever is covering it now.
        self.call_rows.clear();
        // And a picture's rows go with them - a frame with no conversation on it draws no
        // thumbnail, and a click measured against the last frame's rows would open a picture under
        // whatever is covering it now.
        self.picture_rows.clear();
        self.picture_refs.clear();
        // And the open payload window's lines go with them - a frame with no conversation on
        // it draws no window, and a click measured against the last frame's lines would scroll a
        // window under whatever is covering it now.
        self.payload_rows.clear();
        // What is drawn here is `drawn()`'s answer - the same value `App::key` routes
        // on and the hint bar describes, so the three cannot disagree about which of several
        // open panes is in front.
        match self.drawn() {
            Drawn::Term => {
                // The pane takes the conversation's rectangle and gives it back. First in the chain, because
                // while a pane is drawn it owns the keyboard, so a card or picker drawn under it would be a screen
                // the person could see and not answer; a detached pane is drawn nowhere and owns nothing.
                let palette = self.cfg.palette();
                let (rows, moved) = {
                    let p = self.term.as_mut().expect("just checked");
                    // Where it is drawn, for the click that lands on it - the pane's top-left
                    // in the terminal's cells, which `TermPane::route` subtracts from a mouse
                    // report so the program is told about its own column and row.
                    p.origin = (x, y);
                    let rows = p.rows(w, room, palette);
                    // Only for a pane the daemon has numbered: until `TermAttached` names it
                    // there is no pane to tell, and `sent` is left as it was so the first frame
                    // after the number arrives sends the rectangle.
                    let moved = p.pane.filter(|_| p.sent != (w, room)).map(|pane| {
                        p.sent = (w, room);
                        (pane, w, room)
                    });
                    (rows, moved)
                };
                // The program is told the rectangle it is drawn in, and this is the only place that is known:
                // `room` is `h` minus the chrome and the header. A change detector rather than a frame per tick,
                // since a resize frame per redraw would be a frame per keystroke.
                if let Some((pane, cols, rows_n)) = moved {
                    self.queued.push(Action::TermResize {
                        pane,
                        cols,
                        rows: rows_n,
                    });
                }
                rows
            }
            Drawn::DiffPopup => {
                // A change opened by a click, read in its whole file - over the conversation,
                // under a `!term` pane (which owns the keys first). See `app::diff_popup`.
                self.diff_popup_rows(w, room)
            }
            Drawn::Editor => {
                // The editor pane takes the rectangle next, under a `!term` pane and over
                // everything else the conversation's rectangle can hold: it was opened on purpose,
                // with a click or a chord, and it is what the keys go to while it has them.
                self.editor_rows(x, y, w, room)
            }
            Drawn::Surface(Surface::Listing) => {
                let (echo, lines) = self.slash_out.clone().unwrap_or_default();
                let p = self.cfg.palette();
                // Not sanitised, and the regression is why: the rows in `slash_out` are this head's own composed
                // lines, already painted, so the sanitising guard stripped the head's own colour and left an escape's
                // body on the screen as text.
                let mut rows = vec![
                    row(&rano::agent::text::one(echo, Role::Strong), p),
                    String::new(),
                ];
                rows.extend(lines.iter().flat_map(|l| wrap(l, w)));
                rows.push(String::new());
                rows.push(row(
                    &rano::agent::pane::faint("    esc closes · up/down scrolls"),
                    p,
                ));
                self.pane_window(rows, room)
            }
            Drawn::Surface(Surface::Help) => {
                let help = help_lines(&self.cfg, w);
                self.pane_window(help, room)
            }
            Drawn::Surface(Surface::Stats) => {
                let rows = self.status_lines(w);
                self.pane_window(rows, room)
            }
            Drawn::Surface(Surface::Picker) => {
                // A window that keeps the cursor's session on the screen, and a record of which row is which
                // session's: a list cut at `room` with no scrolling would move arrows past the cut onto a
                // cursor nobody could see. The click is read off the rows this frame drew.
                let (rows, spans) = self.picker_layout(w);
                let sel = self.picker_sel.min(spans.len().saturating_sub(1));
                let skip = spans.get(sel).map_or(0, |s| s.end.saturating_sub(room));
                let rows: Vec<String> = rows.into_iter().skip(skip).take(room).collect();
                let top = usize::from(has_header);
                let drawn = skip..skip + rows.len();
                self.picker_hits = spans
                    .iter()
                    .enumerate()
                    .flat_map(|(k, span)| {
                        span.clone()
                            .filter(|r| drawn.contains(r))
                            .map(move |r| (top + r - skip, k))
                    })
                    .collect();
                rows
            }
            Drawn::Surface(Surface::TodoRow) => {
                // One row, whole, through `pane_window` so it pages like the panes: a row is written by a model
                // and can be longer than the screen, so an overlay that could not be scrolled would be the same
                // defect with a bigger box. Esc gives the pane back.
                self.refresh_repo_todos();
                let rows = self.todos_open_lines(w);
                self.pane_window(rows, room)
            }
            Drawn::Surface(Surface::Todos) => {
                // One `stat` before the draw: the file is edited while this pane is
                // open, which is the case the open-time read could not see.
                self.refresh_repo_todos();
                let rows = self.todos_lines(w);
                self.pane_window(rows, room)
            }
            Drawn::Surface(Surface::Config) => {
                let rows = self.config_lines(w);
                self.pane_window(rows, room)
            }
            Drawn::Surface(Surface::SubOut) => {
                let mut rows = self.sub_out_lines(room);
                rows.truncate(room);
                rows
            }
            Drawn::Surface(Surface::JobOut) => {
                let mut rows = self.job_out_lines(room);
                rows.truncate(room);
                rows
            }
            Drawn::Surface(Surface::QueueEntry) => {
                // The overlay, through `pane_window` so it scrolls like the panes: an entry's evidence is up to
                // four kilobytes of captured output, and a detail view that could not scroll would show the first
                // screenful of a failure and nothing else.
                let rows = self.queue_out_lines(w);
                self.pane_window(rows, room)
            }
            Drawn::Surface(Surface::Queue) => {
                let rows = self.queue_lines(w);
                self.pane_window(rows, room)
            }
            Drawn::Surface(Surface::Standing) => {
                let rows = self.standing_lines(w);
                self.pane_window(rows, room)
            }
            Drawn::Surface(Surface::Rooms) => self.rooms_lines(w, room),
            Drawn::Surface(Surface::Skills) => {
                let rows = self.skills_lines(w);
                self.pane_window(rows, room)
            }
            Drawn::Surface(Surface::Subagents) => {
                let rows = self.subagents_lines(w);
                self.pane_window(rows, room)
            }
            Drawn::Surface(Surface::Jobs) => {
                let rows = self.jobs_lines(w);
                self.pane_window(rows, room)
            }
            Drawn::Surface(Surface::Monitors) => {
                let rows = self.monitors_lines(w);
                self.pane_window(rows, room)
            }
            // A plugin's pane, drawn through `pane_window` like every other: its own
            // document is as long as a project's plan is, so it scrolls and pages with the
            // keys the bar names.
            Drawn::Surface(Surface::PluginPane) => {
                let rows = self.plugin_lines(w);
                // The daemon is told the rectangle the pane is drawn in: `room` is `h` minus the chrome and the
                // header, so this is the only place it is known, and a pane laid out for a guess wraps its labels
                // for a terminal nobody is looking at.
                if let Some(pane) = self.plugin_open.clone() {
                    let told = (pane.clone(), w as u32, room as u32);
                    if self.plugin_sent.as_ref() != Some(&told) {
                        self.plugin_sent = Some(told.clone());
                        self.queued.push(Action::PaneOpen {
                            pane: told.0,
                            w: told.1,
                            h: told.2,
                        });
                    }
                }
                self.pane_window(rows, room)
            }
            // One stop read whole, over the pane - also through `pane_window`: a finding's
            // evidence and a row's detail are written by a plugin and can be a screenful.
            Drawn::Surface(Surface::PluginDetail) => {
                let lines = self.plugin_detail_lines(w).unwrap_or_else(|| {
                    vec![rano::agent::pane::faint(
                        "  that row is gone — esc goes back to the pane",
                    )]
                });
                let rows = row_strings(&lines, self.cfg.palette());
                self.pane_window(rows, room)
            }
            Drawn::Conversation => {
                let rows = self.body_window(room);
                // The window's rows, as the terminal's: the header is above them.
                for (row, _) in &mut self.file_rows {
                    *row += y;
                }
                for (row, _) in &mut self.notice_rows {
                    *row += y;
                }
                for (row, _) in &mut self.card_rows {
                    *row += y;
                }
                // A finished child's line too, which was the one record this list left out: a click one row above
                // it hit nothing and fell through to the card, which answered the same, until the header's own row
                // became a target.
                for (row, _) in &mut self.task_rows {
                    *row += y;
                }
                for (row, _) in &mut self.picture_rows {
                    *row += y;
                }
                // And the turn's own call cards, which live below the history and are rows of
                // this same window - the shift is the same one, and a record that forgot it is
                // the whole defect this list's note describes.
                for (row, _) in &mut self.call_rows {
                    *row += y;
                }
                rows
            }
        }
    }
}

/// The bottom edge's right-hand markers as a line: the alarm, `holding`, the rung, and the queue's standings.
/// The standings are last and are the one piece here that is not about this head: they are drawn from the
/// daemon's own states and never from a count this head keeps.
fn bottom_markers(
    b: &BoxBottom,
    standings: &Standings,
    folded: Option<usize>,
    badge: Option<(String, Role)>,
) -> Option<rano::render::Line> {
    use rano::render::{Line, Span};
    use rano::style::Role;
    let mut right = Line::default();
    let sep = |l: &mut Line| {
        if l.width() > 0 {
            l.push(Span::raw(" · "));
        }
    };
    if b.alarmed {
        right.push(Span::role("⚠", Role::Attention));
    }
    // What waits on you, folded away - counted where the eye already goes, in the register
    // the line it replaces was in, and with the key that brings it back.
    if let Some(n) = folded {
        sep(&mut right);
        right.push(Span::role(format!("{n} waiting · alt-h"), Role::Pending));
    }
    if b.holding {
        sep(&mut right);
        right.push(Span::role("holding", Role::Pending));
    }
    if let Some(r) = &b.rung {
        sep(&mut right);
        right.push(Span::role(
            rano::agent::text::clean_line(r),
            Role::Attention,
        ));
    }
    // A plugin pane's own counter, in the register the pane chose - the operator's ask for
    // an agent's turn count, one screen over (`docs/plugins-slice1.md` C3's `badge`): drawn
    // where the waits-on-you counter is drawn, and only while that pane is on the screen.
    if let Some((text, role)) = badge {
        sep(&mut right);
        right.push(Span::role(text, role));
    }
    if let Some(words) = standings.words() {
        sep(&mut right);
        right.push(Span::role(words, standings.role()));
    }
    (right.width() > 0).then_some(right)
}
