//! **The hint bar**: the keys that do something right now.

use crate::app::*;
use crate::ui::render::{trim_to, visible_width};
use letibot_ui::painter::Sgr;
use rano::style::Role;

impl App {
    /// The bottom bar: what the keys do, right now.
    ///
    /// The composer owns the first half and changes it after the first Esc or
    /// Ctrl+C — that is how anyone finds out a double-tap exists. The head owns
    /// the second half, which is its own keys.
    pub(crate) fn hint_bar(&self, w: usize) -> String {
        let p = self.cfg.palette();
        // The editor's own half is about the composer's double-taps. While the
        // quit card is up there is no double-tap left to learn — the card IS
        // the second press — and the editor's "ctrl+c again to exit" would name
        // a key that now closes the card instead. So the card's line stands
        // alone.
        let s = if self.quit_card {
            String::new()
        } else {
            self.editor.hint(self.now_ms, p)
        };

        let tail = if self.quit_card {
            // The footer said "ctrl+c again to exit" here, which stopped being
            // true the moment the second press started opening a card instead:
            // a third press now CLOSES it. A hint that names the wrong key is
            // worse than none.
            "1/2 or ↑↓ then enter · esc stays"
        } else if self.detached() {
            // **The one thing the keys cannot do while the link is down**, said where the
            // keys are described: enter is held rather than sent. Everything else on this
            // bar still works, which is the point of keeping the head up — reading the
            // session, folding, scrolling, /status.
            "no daemon connection · enter holds your line · this head keeps trying"
        } else if self.help || self.stats {
            "esc closes this"
        } else if self.picker {
            "type a number to switch · /new [title] · esc closes"
        } else if self.pick.is_some() {
            "a row number switches · ↑↓ then enter · or type a name · esc closes"
        } else if self.todos_pane {
            "↑↓ moves · enter or tab unfolds · pgup/pgdn and the wheel scroll · esc closes"
        } else if self.config_pane {
            "arrows move · enter changes a row marked ✎ · esc closes"
        } else if self.subagents_pane {
            "↑↓ moves · enter (or o) opens a subagent, or unfolds finished · p reads its output · esc closes"
        } else if self.job_out.is_some() {
            "↑↓ scroll · → next page · ← back · esc back to jobs"
        } else if self.queue_open.is_some() {
            "↑↓ scroll · esc back to the queue"
        } else if self.queue_pane {
            "↑↓ moves · enter opens the entry: its ask, the gate's words, the reviewer's verdict · esc closes"
        } else if self.jobs_pane {
            "↑↓ moves · enter reads a job, or unfolds finished · esc closes"
        } else if !self.open.is_empty() {
            "a row number answers · ↑↓ then enter · or type an option · /help"
        } else {
            // **What a chord says must be what the chord does** (R40), and this bar was the
            // last place that was not true. It read `ctrl-t long output` — long, output, the
            // conversation-wide reading, and exactly the reading R10 spent a ruling
            // removing — while `/t`, which DOES the conversation-wide unfold, was on the bar
            // nowhere. So the key advertised at the bottom of the screen no longer did what
            // the bar said and the verb that did was not advertised at all. That is R29's
            // rule failing on the bar instead of on a note: a remedy the reader has to go
            // looking for was not offered.
            //
            // **The pair is adjacent on purpose.** `ctrl-t` and `/t` are the two things a
            // reader confuses, so the bar states both and states the difference in its own
            // nouns: one result against all of them. The seams follow the same rule from the
            // other end — the newest long result's own row names `ctrl-t` (a chord may only
            // be named where it acts) and every other elided row names `/t`.
            //
            // **`tab completes /commands` gave up its space, and it is the one that
            // should.** This bar is over capacity by construction at 80 columns — nine
            // entries, about five of which fit — so *which* are visible is a decision and
            // not an accident. It was R22 that measured that (same bar, 136 columns then,
            // which is why `ctrl-n` sits second rather than last), and the measurement
            // applies to every entry added since. What gives way is the entry that **answers
            // before it is ever named**: press Tab on a half-typed `/models` and it
            // completes, unasked, which is the one thing on this bar a reader cannot fail to
            // find out. Its fact is still in `/help`, on the row a reader is looking at when
            // they go there. `/help` itself stays, because it is the index and R29's remedy
            // rule needs the index on the screen.
            //
            // Measured after the swap, 2026-09-23: 146 columns, so at 80 the bar reads
            // through `ctrl-r thinki`; at 100, through `ctrl-t newest r`; at 120 the pair is
            // whole; at the operator's own 210, all of it is.
            "ctrl-s sessions · ctrl-n notes · ctrl-t todos · ctrl-g subagents · ctrl-r thinking · ctrl-v newest result · /t all tool rows · ctrl-q jobs · ctrl-p hold · /help"
        };
        // The separator belongs between two halves, not in front of one: with
        // the editor's half suppressed the bar used to open with a bare `·`.
        let joined = if s.is_empty() {
            p.painted(Role::Faint, tail)
        } else {
            format!("{s}{}", p.painted(Role::Faint, &format!(" · {tail}")))
        };
        // **Centred — the operator's ask of 2026-10-04: *"please center the keymap bottom line"*.**
        // Escape-aware on the VISIBLE width (`visible_width` skips the sequences), because the
        // bar is already painted — the editor's half in its own register and the tail in faint —
        // and a centring that measured bytes would sit half a screen off. The pad is prepended as
        // plain spaces and the paint is left alone.
        //
        // **An over-long bar centres to itself** (pad 0) and keeps the head, which is the half
        // naming the first keys — the same degradation the left-aligned bar had. And the bar's
        // own rule survives one note down: the *content* still moves when a turn starts or a pane
        // opens, because the editor's half and the tail change; the centring moves the whole line
        // with them, which is the look asked for rather than the fixed left edge the old comment
        // argued for.
        let pad = w.saturating_sub(visible_width(&joined)) / 2;
        trim_to(&format!("{}{joined}", " ".repeat(pad)), w)
    }
}
