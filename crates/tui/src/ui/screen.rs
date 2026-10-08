//! **The frame**: everything the head draws, composed into the terminal's rows — the header,
//! the conversation, the cards and panes, the composer and the hint bar — and where the cursor goes.

use crate::app::*;
use crate::ui::render::{sgr, trim_to, visible_width, wrap};
use crate::ui::*;
use letibot_ui::painter::Sgr;
use rano::agent::composer::{BoxBottom, BoxTop};
use rano::style::Role;

impl App {
    /// One frame: `h` lines of at most `w` columns — **and while the view is held, the same frame
    /// every time.**
    ///
    /// # The hold, and why it is a wrapper rather than a flag inside the renderer
    ///
    /// While the view is held the head writes NOTHING, so a mouse selection survives a streaming
    /// turn: the frame is composed once — with the marker on it, which is the one write the freeze
    /// owes — and every later call returns that same frame byte for byte, which `Terminal::draw`
    /// diffs into no bytes at all. See [`App::toggle_hold`] for the contract and the three pieces
    /// that keep it.
    ///
    /// It is a wrapper because the composition must run EXACTLY once while held and must keep
    /// running normally otherwise; a flag threaded through the body would have to be honoured at
    /// every one of the hundreds of writes the body makes, and one arm that forgot would be a lost
    /// selection.
    pub fn screen(&mut self, term_w: usize, h: usize) -> Vec<String> {
        if self.hold
            && self.hold_size == (term_w, h)
            && let Some(frame) = &self.hold_frame
        {
            return frame.clone();
        }
        let mut out = self.compose_screen(term_w, h);
        // **Every `http(s)://` on the frame is a link** (OSC 8) where the terminal speaks it —
        // the reply, the tool output, a note: one pass over the finished frame rather than one
        // per renderer, so no renderer can be the one that forgot.
        if self.features.links {
            for l in out.iter_mut() {
                if l.contains("://") {
                    *l = rano::term::links::link_urls(l);
                }
            }
        }
        if self.hold {
            // **The marker takes the hint bar's row.** It is one row that always exists, so the
            // freeze costs no height and reflows nothing — and it is the row that already talks
            // about keys, which is the row a held view's one sentence belongs on.
            let gutter = Self::gutter(term_w);
            let w = term_w.saturating_sub(2 * gutter).max(1);
            let marker = self
                .cfg
                .palette()
                .painted(Role::Attention, &trim_to(HOLD_MARKER, w));
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
        out
    }

    pub(crate) fn compose_screen(&mut self, term_w: usize, h: usize) -> Vec<String> {
        // The gutter, applied to the *whole* frame rather than to the transcript.
        // The operator's report was "no margins for the main output — things are
        // hard left with literally zero space"; inseting only the body would have
        // fixed that sentence and left the header and the composer's box a
        // different distance from the edge, which is the thing a reader notices.
        let gutter = Self::gutter(term_w);
        let w = term_w - 2 * gutter;
        self.cfg.width = w;
        self.cfg.links = self
            .features
            .links
            .then(|| self.wiring.workspace.clone())
            .filter(|w| !w.is_empty());
        self.cfg.light = self.features.background && self.light_background == Some(true);
        self.cfg.images = self.features.images;
        if self.features.images {
            self.queue_image_uploads();
        }
        // The TERMINAL's width, kept beside the frame's. `cfg.width` is the inner
        // one — the gutter already taken off — so anything that re-renders from a
        // stored size has to start from this one or the frame narrows by two
        // columns every time it is asked for.
        self.term_cols = term_w;
        let h = h.max(1);
        // Click mapping has to redo this frame's arithmetic without a repaint;
        // the height the frame was composed for is the fact it needed.
        self.screen_rows = h;
        // **The notice's clock, read rather than decremented.** A comparison against the
        // head's own clock is the whole mechanism: `say` sets a deadline and a key moves it
        // to now, and this is the frame either of those takes effect on. Nothing here
        // counts frames, so the sentence's lifetime is the same on a head woken ten times a
        // second and on one that is quiet — see [`NOTICE_MS`].
        if let Some(until) = self.notice_until {
            if self.now_ms >= until {
                self.notice = None;
                self.notice_until = None;
            }
        }

        // **R20: the card is two pieces, and only one of them gives way.** `dec` is the
        // content — the question and its evidence, which a viewport shrinks and scrolls —
        // and `dec_pinned` is the answer: the ladder, the deadline, the hint. The fit loop
        // below may not touch the second, because a card that has dropped its choices is a
        // question with no way to answer it. See [`App::decision_card`].
        let (dec, dec_pinned): (Vec<String>, Vec<String>) =
            match (&self.secret, &self.prompt, self.open.first()) {
                (Some(ask), _, _) => (self.secret_lines(ask, w), Vec::new()),
                // **The confirmation that ends a pane rides in the same slot and comes second**,
                // ahead of the prompt card. The two are mutually exclusive — a `!term close` can
                // only be typed while the prompt card is not up, and a prompt that arrives while
                // this is up takes the screen back (see the `PromptRequested` arm) — so this arm
                // is about *which* card is drawn, and the operator is never shown one while the
                // keys are owned by the other.
                (None, _, _) if self.term_ask.is_some() => (
                    self.term_ask_lines(self.term_ask.as_ref().expect("just checked"), w),
                    Vec::new(),
                ),
                // **The prompt card rides in the same slot and comes second.** A password ask is
                // `sudo` blocking the run it is inside, so the two are rarely up together — and
                // when they are, the secret is the one that must not be typed past: a person who
                // answers the password releases the command that was going to ask them the other
                // question. See [`App::prompt_lines`] for what the second card shows.
                (None, Some(ask), _) => (self.prompt_lines(ask, w), Vec::new()),
                (None, None, Some(d)) => self.decision_card(d, w),
                // **A key the picker asked for rides in the ask card's slot too**, ahead of the
                // pickers and the todo card: it is the newest question, it owns the keyboard
                // while it is up, and a list under it is a list nobody is going to use.
                (None, None, None) if self.key_ask.is_some() => (self.key_ask_lines(w), Vec::new()),
                // **The new-todo card rides in the ask card's slot too**, and ahead of the quit card:
                // it is the newest question and the one the keyboard belongs to while it is up.
                (None, None, None) if self.todo_draft.is_some() => {
                    (self.todo_card_lines(w), Vec::new())
                }
                // The mode card rides in the ask card's slot: a compact card at
                // the bottom of the screen with the transcript still visible above
                // it, which is where everything else that wants a choice sits.
                // The two are never up at once — a decision owns the ladder keys,
                // and a second cursor under it would be a cursor nothing moves —
                // so the card waits out an ask and comes back when it is answered.
                // Ahead of the mode picker: a head on its way out is answering the
                // last question it will be asked, and a list under it is a list
                // nobody is going to use.
                (None, None, None) if self.quit_card => (self.quit_card_lines(w), Vec::new()),
                (None, None, None) if self.pick.is_some() => {
                    (self.setting_picker_lines(w), Vec::new())
                }
                (None, None, None) => (Vec::new(), Vec::new()),
            };
        let dec_full = dec.len();
        // **The window the key handler asks about.** Set every frame, because the length of
        // the content is a function of the width and the room is a function of the terminal
        // — the same arrangement `pane_len`/`pane_room` make for the panes. The scroll
        // itself is reset in one place, here, so a second card cannot inherit the first
        // one's offset however it arrived.
        let card_here = self
            .open
            .first()
            .map(|d| d.req_id.clone())
            .unwrap_or_default();
        if card_here != self.dec_scroll_for {
            self.dec_scroll_for = card_here;
            self.dec_scroll = 0;
        }
        // **The link line.** Read here with the other chrome rather than in the body: it
        // is a state of the connection, not a row of the conversation, and it belongs
        // where the eye crosses on the way to the composer — the same place the stuck
        // line and a stuck decision card sit.
        let link = self.link_line(w);
        // **A stop the operator ordered is the loudest thing on the screen while it
        // lasts** (R30), and it goes above the link line because the two are the same
        // slot and only one of them can be true — `link_down` refuses to run while a stop
        // is in flight, so the link line is empty here and this is the sentence the
        // operator reads while the daemon goes.
        let stopping = self.stopping_line(w);
        let stuck = self.stuck_line(w);
        // **A pane this head is not drawing, and the fact that it is still running.** See
        // [`App::pane_behind`] for the two sources of that fact and for why it is a line and
        // not a row: a detach is not an event, so what a reader gets is a statement about NOW
        // — drawn while it is true, gone the moment it stops being true, and naming the two
        // verbs that do something about it.
        //
        // **Nothing is drawn while the pane is on the screen**: the pane itself is the fact,
        // and a sentence about it would be the same fact twice.
        let pane = self.pane_behind().map(|line| {
            self.cfg.palette().painted(
                Role::Pending,
                &trim_to(
                    &format!("a pane is running: {line} — `!term` attaches, `!term close` ends it"),
                    w,
                ),
            )
        });
        // **The turn's status is a ROW of its own, immediately above the composer** (R51 item 1).
        //
        // It used to be a legend inlaid in the composer's bottom border, sharing that edge with
        // the alarm, the hold marker and the rung — and an edge truncates. leticl lifted it out
        // (`8ebcfdb`) for the reason a border is the wrong container for a sentence: the words that
        // matter are the ones a trim takes, and this line is allowed to grow (`Responding · 4.2s ·
        // 12.4k tok`, or a whole prefill bar with its cache split).
        //
        // **What it costs, stated because the old comment claimed the opposite.** As a border
        // legend it cost no row at all; as a row it costs one, so it enters the fit ladder below
        // and is counted in the frame's height. It is given up *after* the stuck disclosure and
        // before the box, which is the order of what a reader loses least by losing.
        //
        // **The past tense is not here, and that is deliberate rather than an omission.** The
        // header already carries a finished turn's report — its duration, its rate and its output
        // count, measured when it ended (`header_line`, from `last_timings` + `usage`) — and the
        // transcript carries the reply. A second copy of those three numbers on a row above the
        // composer would be the same facts twice, which is the defect this document keeps naming;
        // so the row is present-tense and lives while the work does, and the tense comes from
        // `turn_busy` rather than from the state name.
        let status = self.turn_status(w);
        let notice = self
            .notice
            .clone()
            .map(|n| colour(&self.cfg, sgr::MAGENTA, &trim_to(&format!("· {n}"), w)));
        // Live slash-command matches, one dim row above the composer. It is a
        // typing aid, not a message.
        //
        // **And it is a SLOT, not an appearance.** The operator watched the transcript jump
        // a line up and then a line back on every appearance and dismissal of this row: the
        // conversation's height is `h` minus the chrome (`room`, below), so a row that comes
        // and goes takes its line from the transcript and hands it back, and the reader's
        // page moves under them while they type. So the row is counted and drawn while the
        // composer holds a line that COULD be completed — [`App::completion_slot`], the same
        // predicate the row's own content is gated on — and it is drawn empty when the
        // prefix matches nothing: what appears and disappears is the text in the slot, never
        // the slot. A pane that re-flows when a hint arrives makes the transcript move under
        // the reader, which is worse than the hint is useful.
        //
        // **The slot is the line's shape and not the candidate list**, which is what keeps
        // it still: the candidates come and go with every character typed, and a row whose
        // height followed them would be the defect. The shape changes once, when the operator
        // starts or abandons a `!` or `/` line — and an ordinary frame, with neither in the
        // composer, is exactly the frame it was before: no row, no cost.
        //
        // This is the same trade the turn's own row makes one row below (see `let status`),
        // and it is the reason that row is reserved too.
        let completion_slot = self.completion_slot();
        let completions = self.completions_line(w);

        let Fit {
            rows,
            hint,
            show_notice,
            show_stuck,
            show_pane,
            show_status,
            boxed,
            content_rows,
        } = fit_ladder(&FitInput {
            h,
            composer: self.editor.height(self.composer_cols(), h),
            card: dec.len(),
            pinned: dec_pinned.len(),
            notice: notice.is_some(),
            stuck: stuck.is_some(),
            pane: pane.is_some(),
            completion_slot,
            link: link.len(),
            stopping: stopping.len(),
            alarmed: self.alarmed(),
        });

        let (input_rows, caret_row, caret_col) = self.composer_rows(w, rows, boxed);
        let mut chrome: Vec<String> = Vec::new();
        // **A head with no daemon says so first**, ahead of the `allow-all`
        // confirmation and ahead of the decision card: it is the reason every other
        // line on the screen is not moving, and a person who reads the card without
        // it reads a question nothing is waiting on.
        chrome.extend(stopping);
        chrome.extend(link);
        // **The `allow-all` confirmation sits at the front of the chrome**, above
        // the decision card and the composer, because while it is up every key
        // belongs to it (see `key`) and a question that owns the keyboard has to be
        // the thing on screen. Wrapped rather than trimmed: this one is read, not
        // glanced at.
        if let Some(line) = self.mode_confirm_line() {
            let p = self.cfg.palette();
            for l in wrap(&line, w) {
                chrome.push(p.painted(Role::Attention, &l));
            }
        }
        // **The content, as a window** (R20), then the answer in full underneath it.
        //
        // The seam is what makes the window honest: it says how many lines are out of
        // view and names the keys that move, and it is only drawn when there is something
        // out of view. `card_window` clamps the scroll to what this frame actually has,
        // because the length of the content is a function of the width and nothing else
        // knows it.
        let (card_rows, _) = self.card_window(w, &dec, content_rows);
        chrome.extend(card_rows);
        chrome.extend(dec_pinned);
        if show_stuck && let Some(l) = stuck {
            chrome.push(l);
        }
        if show_pane && let Some(l) = pane {
            chrome.push(l);
        }
        if show_notice && let Some(l) = notice {
            chrome.push(l);
        }
        if completion_slot {
            // **Drawn even when it is empty.** The row is furniture while a `!` or `/` line
            // is in the composer, and the price of a transcript that does not move is a blank
            // row when the prefix matches nothing. It is NOT a rung of the ladder above:
            // a row the fit loop may delete is a row that appears and disappears again, which
            // is the jump this whole arrangement exists to stop.
            chrome.push(completions.unwrap_or_default());
        }
        // The turn's own row, last before the box: directly above the composer when nothing else
        // is up, and below the typing aids when they are — a completion list that is not adjacent
        // to the line being typed is the one row here that must not move.
        if show_status {
            chrome.push(status);
        }
        if boxed {
            // The top edge carries the facts that exist ONLY while they are true, pinned right:
            // subagents this session spawned and background jobs it started. The legend that used
            // to live here — model, dialect, endpoint, verbosity — was a row of attention paid for
            // ever for facts read once; these are facts that stop being drawn when they stop being
            // true, which is what makes them worth a resident edge.
            //
            // **The jobs count is R51 item 5**, and the placement is this head's answer to *"the
            // placement is yours"*: the other head put it on the status row, and here the status
            // row has just been given to the turn (item 1) while this edge already carries the
            // session's other running things. One edge for *what this session has in flight* is
            // one place to look, and it is the same kind of fact as the subagent count beside it.
            chrome.push(self.box_top(w));
        }
        let caret_at = chrome.len() + caret_row;
        chrome.extend(input_rows);
        if boxed {
            // The bottom edge, pinned right: the alarm as a triangle — the counters behind it are
            // /status's, and were never worth a resident sentence of bright yellow. **The turn's
            // own status is no longer here** (R51 item 1): it is a row above the box, because a
            // legend on an edge truncates and a status is allowed to grow a sentence.
            chrome.push(self.box_bottom(w));
        } else if self.alarmed() {
            chrome.push(self.status_line(w));
        }
        if hint {
            chrome.push(self.hint_bar(w));
        }
        // The mode card's click facts, redone without a repaint: the card is
        // the front of chrome, so its first choice sits one row below the
        // card's first line. Clicks are trusted only when the whole card
        // survived — neither the fit loop (`dec_rows == dec_full`) nor this
        // backstop cut it — because a click into a list nobody saw whole would
        // pick a mode nobody saw. The arrows still work either way.
        let pre_chrome = chrome.len();
        // Backstop. The ladder above cannot always win — `h` can be 2 — and a head
        // that returns more lines than the terminal has scrolls its own composer
        // off the bottom.
        if chrome.len() >= h {
            chrome.drain(..chrome.len() - h.max(1));
        }
        let card_at = h.saturating_sub(chrome.len());
        self.mode_first_row = card_at + 1;
        self.mode_rows_drawn = if self.pick.is_some()
            && self.open.is_empty()
            && self.secret.is_none()
            && content_rows == dec_full
            && pre_chrome == chrome.len()
        {
            self.mode_choices().len()
        } else {
            0
        };

        // The session header, pinned above everything. One row, and it is the row
        // both surveyed heads spend first: opencode puts the title left and
        // `39,413  20% ($0.29)` right, grok-build puts the cwd left and `9.5K /
        // 500K` right. What is here is the same shape with this harness's own
        // numbers — see `header_line` for which of theirs are deliberately absent.
        //
        // It costs a row of transcript and it is worth it because the question it
        // answers ("which session am I in, and how big has it got") is otherwise
        // answered by scrolling.
        let header = (h >= 6 && !self.session_id.is_empty()).then(|| self.header_line(w));
        let room = h
            .saturating_sub(chrome.len() + usize::from(header.is_some()))
            .max(1);
        let mut out = self.main_area(w, room, header.is_some());
        while out.len() < room {
            out.push(String::new());
        }
        out.truncate(room);
        let mut body_rows = out.len();
        if let Some(l) = header {
            out.insert(0, l);
            body_rows += 1;
        }
        out.extend(chrome);
        out.truncate(h);
        // The caret is the affordance. It goes where the composer says, and the
        // terminal draws it as a steady block because `term::enter` asked for one.
        self.cursor = Some((
            (body_rows + caret_at).min(out.len().saturating_sub(1)),
            caret_col.min(w.saturating_sub(1)) + gutter,
        ));
        // **The gutter and the trim, in ONE pass and in place.**
        //
        // This was `out.into_iter().map(…).collect()`, and it cost two allocations per line per
        // frame plus a whole second `Vec`:
        //
        //   * `collect()` built a NEW `Vec<String>` of `h` elements — the old one was dropped
        //     straight after, so the frame existed twice for as long as it took to copy;
        //   * `trim_to` is `width::truncate`, which returns an owned `String` **even when the line
        //     already fits** (`s.to_string()` on the early path) — so a line that needed no
        //     trimming was still re-allocated and re-copied;
        //   * and the padded case allocated a second `String` for `format!("{pad}{l}")` on top of
        //     that one.
        //
        // `App::screen` runs once per pass of the head's loop — tens of times a second — over a
        // window of `h` lines, so this is the single hottest allocation site in the head: at 40
        // lines and 38 passes/second it was on the order of three thousand `String`s a second,
        // every one of them handed to `Terminal::draw`, compared character by character against
        // the previous frame, and thrown away.
        //
        // **The width test is what makes it cheap, and it is not a micro-optimisation**: most
        // lines of a settled frame are already the right width, and asking `visible_width` first
        // turns *allocate-and-copy* into *measure* for all of them. `truncate`'s own early return
        // is the same measurement, so nothing is measured twice.
        let pad = " ".repeat(gutter);
        for l in &mut out {
            if visible_width(l) > w {
                *l = trim_to(l, w);
            }
            // The pad goes on in place — one reallocation that may extend the existing buffer,
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

/// **The one sentence a held view says** (R56), in the words the two heads agreed on, because an
/// operator who learns it on one head reaches for it on the other. It takes the hint bar's row —
/// the row that already talks about keys — and it names the key that undoes the hold, which is
/// R29's rule for a disclosure: it carries the act that ends it.
pub const HOLD_MARKER: &str = "⏸ the view is held — ctrl-p follows again";

impl App {
    /// **The composer box's top edge**, carrying what this session has running: its live
    /// subagents and its background jobs.
    pub(crate) fn box_top(&self, w: usize) -> String {
        // **The number is the rows the pane draws, and not a second rule about them**: the one
        // lifecycle predicate ([`SubagentState::is_finished`]) the pane's own active group is
        // built from — *"an agent is alive from spawn until it has finished"*.
        let top = BoxTop {
            subagents_running: self.subagents.iter().filter(|s| !s.is_finished()).count(),
            jobs_running: self.jobs.iter().filter(|j| j.running).count(),
            jobs_to_a_file: self
                .jobs
                .iter()
                .filter(|j| j.running && j.redirect.is_some())
                .count(),
        };
        crate::ui::rows::edge_row(&top.line(w), self.cfg.palette())
    }

    /// **The composer box's bottom edge**, carrying the alarm, where the reader is in the
    /// conversation, and the visibility rung.
    pub(crate) fn box_bottom(&self, w: usize) -> String {
        // The alarm, then the viewport's state **only when it is holding** (R36 — following is
        // the ordinary state and owes the reader nothing), then the rung **when it is the one
        // that hides things** (R37): each drawn only when it is news, because a marker that is
        // always on is furniture.
        let bottom = BoxBottom {
            alarmed: self.alarmed(),
            holding: self.scroll_state().is_some(),
            rung: self.rung_state(),
        };
        crate::ui::rows::edge_row(&bottom.line(w), self.cfg.palette())
    }

    /// **What fills the screen above the cards**: the terminal pane, an open output or pane,
    /// the help, the picker — or, when none of them is open, the conversation.
    pub(crate) fn main_area(&mut self, w: usize, room: usize, has_header: bool) -> Vec<String> {
        if self.pane_open() {
            // **The pane takes the conversation's rectangle and gives it back.**
            //
            // First in the chain, and that is a decision rather than an ordering: while a pane
            // is DRAWN it owns the keyboard (see `App::pane_keys`), so a card, a picker or a
            // pane drawn *under* it would be a screen the operator could see and not answer.
            // **A detached pane is not drawn and owns nothing** — the conversation has the
            // rectangle back, the composer has its keys, and the one line above the composer
            // says the program is still running (see `let pane`).
            // The chrome below still draws — the composer keeps its rows and the header keeps
            // its line, which is the whole requirement — and a card that arrives while a pane
            // is up waits until the operator leaves with `ctrl-\`.
            //
            // **`pane_rows` returns exactly `room`**, so nothing above the pane moves by a line
            // and nothing below it loses a row. See `TermPane`.
            let palette = self.cfg.palette();
            let (rows, moved) = {
                let p = self.term.as_mut().expect("just checked");
                let rows = p.rows(w, room, palette);
                let moved = (p.sent != (w, room)).then(|| {
                    p.sent = (w, room);
                    (w, room)
                });
                (rows, moved)
            };
            // **The program is told the rectangle it is drawn in**, and this is the only place
            // that is known: `room` is `h` minus the chrome and the header, and neither is the
            // daemon's to compute. A change detector rather than a frame per tick, because a
            // resize frame per redraw would be a frame per keystroke.
            if let Some((cols, rows_n)) = moved {
                self.queued.push(Action::TermResize { cols, rows: rows_n });
            }
            rows
        } else if let Some((echo, lines)) = self.slash_out.clone() {
            let p = self.cfg.palette();
            // **Not sanitised, and the regression is why.** The rows in `slash_out` are
            // **this head's own composed lines**: `/notes` draws them through
            // `note_lines_unfolded`, which paints each one with `sgr::RED`, and `/gate`'
            // and `/job`'s are laid out here by the same kind of code. Running the §3.1
            // guard over them stripped the head's own colour — the operator's screen
            // showed ` [31m! gate — a refusal [0m`, an escape's body left as text, and the
            // wrapping broke because those five columns are not what the terminal
            // measures.
            //
            // §3.1's subject is content this head did **not** write. Whatever a listing
            // is showing, the strings in this `Vec` went through a renderer that
            // sanitised its own foreign inputs already — a note's detail, a job's
            // command, a gate's summary — so there is nothing left here to guard.
            let mut rows = vec![p.painted(Role::Strong, &echo), String::new()];
            rows.extend(lines.iter().flat_map(|l| wrap(l, w)));
            rows.push(String::new());
            rows.push(p.painted(Role::Faint, "    esc closes · up/down scrolls"));
            self.pane_window(rows, room)
        } else if self.help {
            let help = help_lines(&self.cfg, w);
            self.pane_window(help, room)
        } else if self.stats {
            let rows = self.status_lines(w);
            self.pane_window(rows, room)
        } else if self.picker {
            let mut rows = self.picker_lines(w);
            rows.truncate(room);
            self.picker_rows_drawn = rows.len();
            rows
        } else if self.todos_pane {
            // **Where the pane's own first row goes on the screen**, which is what a click's `y`
            // has to be measured against: the session header sits above it when the frame is tall
            // enough for one, and it is not a row of this pane.
            self.todos_pane_top = usize::from(has_header);
            // One `stat` before the draw: the file is edited while this pane is
            // open, which is the case the open-time read could not see.
            self.refresh_repo_todos();
            let rows = self.todos_lines(w);
            self.pane_window(rows, room)
        } else if self.config_pane {
            let rows = self.config_lines(w);
            self.pane_window(rows, room)
        } else if self.sub_out.is_some() {
            let mut rows = self.sub_out_lines(room);
            rows.truncate(room);
            rows
        } else if self.job_out.is_some() {
            let mut rows = self.job_out_lines(room);
            rows.truncate(room);
            rows
        } else if self.queue_open.is_some() {
            // **The overlay, through `pane_window`** so it scrolls like the panes: an entry's
            // evidence is the gate's own captured output, which is up to four kilobytes of
            // text, and a detail view that could not be scrolled would be a view that showed
            // the first screenful of a failure and nothing else.
            let rows = self.queue_out_lines(w);
            self.pane_window(rows, room)
        } else if self.queue_pane {
            // **Where the pane's own first row goes on the screen**, which is what a click's `y`
            // is measured against — the same record the todos pane keeps, for the same reason.
            self.queue_pane_top = usize::from(has_header);
            let rows = self.queue_lines(w);
            self.pane_window(rows, room)
        } else if self.subagents_pane {
            let rows = self.subagents_lines(w);
            self.pane_window(rows, room)
        } else if self.jobs_pane {
            let rows = self.jobs_lines(w);
            self.pane_window(rows, room)
        } else {
            self.body_window(room)
        }
    }
}
