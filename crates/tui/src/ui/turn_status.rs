//! **The turn's status row**: what the model is doing — green while it goes, yellow when slow.

use crate::app::*;
use crate::ui::render::{row, trim_to, visible_width};
use rano::agent::turn_status::{Prefill, SLOW_AFTER_MS, TurnStatus};
use rano::render::{Line, Role};

/// **The fewest columns a transient is cut to** before the status row drops it instead: below
/// this a cut sentence is an ellipsis with a word in front of it, which says nothing.
pub(crate) const TRANSIENT_MIN: usize = 12;

impl App {
    /// The turn's status, inlaid in the composer's bottom border and pinned
    /// right: the spinner, what phase the turn is in, and — once anything has
    /// arrived — how much. Empty when nothing is running.
    ///
    /// §5.6's prefill progress is the thing nothing surveyed reports, and it is
    /// not a small difference: both projects read for `letibot-ui` talk to a
    /// metered API and have **no prefill number at all**, so their in-flight line
    /// can only say `Responding… 15s`. `PromptProgress { total, cache, processed,
    /// time_ms }` supports a three-segment bar that answers *how far along* and
    /// *how much of this did the prefix cache save me* at once, which is the
    /// number §10 says the whole prompt pipeline exists to move.
    ///
    /// The arithmetic is `progress::Prefill`'s and the module header is worth
    /// reading before touching it: `processed` **includes** `cache`, so the
    /// fraction is `processed / total` and the work done is `processed - cache`.
    /// Read the other way a 90 %-cached prompt shows as 10 % done and then jumps
    /// to 100 %, which is the classic progress-bar lie; and the throughput has to
    /// divide by the computed tokens or it reports a cache hit as a speed in the
    /// hundreds of thousands.
    ///
    /// # The spinner runs on this head's clock
    ///
    /// It used to be keyed off `t.last_ms`, the last event's timestamp, and the
    /// defect was visible: a spinner that only moves when a token or a prefill
    /// batch arrives is not a spinner, it is a snapshot of one — a tool running
    /// thirty silent seconds froze it on one glyph. The phase is `now_ms` now,
    /// which the driver advances every tick whether or not anything arrived.
    /// The duration is measured against the same clock: head and daemon share
    /// the machine, which is the assumption the stuck line below already makes
    /// when it diffs `now_ms` against an event timestamp.
    #[cfg(test)]
    pub(crate) fn turn_status(&self, w: usize) -> String {
        row(&self.turn_line(w), self.cfg.palette())
    }

    /// [`App::turn_status`] as a [`Line`], so [`App::status_row`] can add to it before painting.
    fn turn_line(&self, w: usize) -> Line {
        let t = match self.turn.as_ref() {
            Some(t) if self.turn_busy() => t,
            _ => return Line::default(),
        };
        let status = TurnStatus {
            busy: true,
            // `started_ms == 0` means the turn came out of a snapshot, which has no timestamps:
            // the row says it started before this head attached rather than a number nobody took.
            elapsed_ms: (t.started_ms != 0).then(|| self.now_ms.saturating_sub(t.started_ms)),
            prefill: t.progress.as_ref().map(|pp| Prefill {
                total: pp.total,
                cache: pp.cache,
                processed: pp.processed,
                time_ms: pp.time_ms,
            }),
            now_ms: self.now_ms,
            slow: self.turn_slow(),
        };
        let mut line = status.line(w);
        if let Some(held) = self.held_label() {
            line.spans
                .push(rano::render::Span::role(format!(" · {held}"), Role::Faint));
            line = rano::render::text::truncate_owned(line, w);
        }
        line
    }

    /// **The status row whole: the turn's words, then a progress, then the transients** — and what
    /// a click on the row opens, `(title, text)`, when the row left something out: a cut flash
    /// notice (`Target::FlashNotice`, R89), or a progress whose one-line form is not all of it.
    ///
    /// The operator, 2026-10-14: *"it showed me "resync..." in violet and "reading job output"
    /// very nice! but they were on their bottom lines and conversation was jumping back and forth.
    /// can we show such things [...] where Responding... line lives - it is preallocated and
    /// fixed"*. Each of those used to be a row of its own above the composer (the flash notice,
    /// counted by the fit ladder as `notice`) or at the foot of the conversation (the boot step),
    /// so it took a line from the transcript when it appeared and handed it back when it went —
    /// and with the jobs pane re-reading once a second against a daemon that acknowledges each
    /// read, that was a jump every second. And then, the same day: *"yes, move the compacting
    /// progress blocks too"* — the fill, fold and announced-rows blocks, which did the same thing
    /// on the operations that last minutes ([`crate::ui::progress_row`]).
    ///
    /// This row is the one that never changes height: the fit ladder reserves it whether or not a
    /// turn runs (`rano::agent::fit`, *"the row is RESERVED, not conditional"*). So the transients
    /// and the progress are drawn IN it and the transcript's budget is the same with or without
    /// them. The rules:
    ///
    /// * **A turn is running**: everything follows the turn's clock after ` · `
    ///   (`⠸ Responding · 2m30s · Compacting half 1 of 2 · ▐██░░▌ 40% · …`), and the turn's own
    ///   words are never cut for any of it.
    /// * **Idle**: the progress leads behind a spinner of its own, then the transients.
    /// * **The progress gives way piece by piece** — the clock, then the counts, then the bar —
    ///   to keep the transients after it whole, and never gives up its label: a flash notice is
    ///   over in seconds and the progress gets its numbers back when it goes.
    ///
    /// The transients' order is how much a reader loses by losing it: the link being down, then
    /// the daemon's boot step, then the notice. One that does not fit whole is cut to the room
    /// left if at least [`TRANSIENT_MIN`] columns remain, and dropped otherwise; nothing after it
    /// is drawn.
    pub(crate) fn status_row(&self, w: usize) -> (String, Option<(String, String)>) {
        let turn = self.turn_line(w);
        let progress = self.progress_shown();
        let Some((forms, detail, more)) = progress else {
            let (line, cut, _) = self.with_transients(turn, w);
            let click = cut.then(|| {
                (
                    "notice".to_string(),
                    self.notice.clone().unwrap_or_default(),
                )
            });
            return (row(&line, self.cfg.palette()), click);
        };
        let last = forms.len() - 1;
        for (i, form) in forms.iter().enumerate() {
            let Some((line, label_cut)) =
                crate::ui::progress_row::with_progress(&turn, form, w, self.now_ms)
            else {
                // Not even the label fits after the turn's words: the row is the turn's.
                let (line, cut, _) = self.with_transients(turn, w);
                let click = if cut {
                    Some((
                        "notice".to_string(),
                        self.notice.clone().unwrap_or_default(),
                    ))
                } else {
                    Some(("progress".to_string(), detail))
                };
                return (row(&line, self.cfg.palette()), click);
            };
            // A fuller form that does not fit whole is passed over for the next one.
            if i < last && label_cut {
                continue;
            }
            let (line, cut, whole) = self.with_transients(line, w);
            if whole || i == last {
                let click = if cut {
                    Some((
                        "notice".to_string(),
                        self.notice.clone().unwrap_or_default(),
                    ))
                } else if i > 0 || more || label_cut {
                    Some(("progress".to_string(), detail))
                } else {
                    None
                };
                return (row(&line, self.cfg.palette()), click);
            }
        }
        unreachable!("the last form always returns")
    }

    /// `line` with the transients after it, by [`App::status_row`]'s rules: the line, whether the
    /// flash notice was cut, and whether every transient was drawn whole.
    fn with_transients(&self, mut line: Line, w: usize) -> (Line, bool, bool) {
        use rano::render::Span;
        let mut notice_cut = false;
        let mut whole = true;
        let hint = format!(" · {}", crate::ui::transcript::line_cards::CLICK);
        for (text, role, is_notice) in self.transients() {
            // The notice carries its own `· ` mark; the others are joined by one.
            let sep = match (line.width(), is_notice) {
                (0, _) => "",
                (_, true) => " ",
                (_, false) => " · ",
            };
            let room = w.saturating_sub(line.width() + visible_width(sep));
            let text = text.replace('\n', " ");
            let shown = if visible_width(&text) <= room {
                text
            } else if is_notice && room >= visible_width(&hint) + TRANSIENT_MIN {
                notice_cut = true;
                whole = false;
                format!("{}{hint}", trim_to(&text, room - visible_width(&hint)))
            } else if !is_notice && room >= TRANSIENT_MIN {
                whole = false;
                trim_to(&text, room)
            } else {
                whole = false;
                break;
            };
            if !sep.is_empty() {
                line.spans.push(Span::raw(sep.to_string()));
            }
            line.spans.push(Span::role(shown, role));
            if notice_cut {
                break;
            }
        }
        (line, notice_cut, whole)
    }

    /// **The transient one-line statuses, in the order the status row gives them up last-first**:
    /// `(text, colour, is the flash notice)`. See [`App::status_row`].
    ///
    /// * The link while it is down and the head has not yet grown impatient — the ordinary
    ///   reconnect across a daemon restart, over in a second or two. Past [`LINK_IMPATIENT_MS`]
    ///   the outage is a state, not a blip, and it gets its resident lines above the composer
    ///   with the advice ([`App::link_line`]).
    /// * The daemon's boot step (`daemon_starting`, R92.2), which used to be a row at the foot of
    ///   the conversation.
    /// * The flash notice ([`App::say`]) — `resync: …`, `reading output of job …`, every
    ///   acknowledgement and refusal — prefixed `· ` as it always was, in its own violet.
    pub(crate) fn transients(&self) -> Vec<(String, Role, bool)> {
        let mut out = Vec::new();
        if let Some(why) = self.link_blip() {
            out.push((
                format!("⚠ daemon connection is down — reconnecting. {why}"),
                Role::Pending,
                false,
            ));
        }
        if let Some(step) = &self.booting {
            out.push((format!("daemon {step}"), Role::Pending, false));
        }
        if let Some(n) = &self.notice {
            out.push((format!("· {n}"), Role::Keyword, true));
        }
        out
    }

    /// **The link is down and has been for less than [`LINK_IMPATIENT_MS`]**: the reason, for the
    /// status row. `None` while attached, and `None` once the head is impatient — from then on
    /// [`App::link_line`] says it, with how long and what to do.
    pub(crate) fn link_blip(&self) -> Option<&str> {
        match &self.link {
            Link::Reconnecting { since_ms, why, .. }
                if self.now_ms.saturating_sub(*since_ms) < LINK_IMPATIENT_MS =>
            {
                Some(why.as_str())
            }
            _ => None,
        }
    }

    /// **What the daemon is holding back from the model, in a few words** — `held: 2 jobs · 1
    /// subagent`, `held: 1 job (failed)` — or `None` when nothing is held.
    ///
    /// The operator, 2026-10-11: *"what would be useful is a counter of how much and what kind is
    /// held - somewhere close to Responding ...."*. Faint, after the turn's own words on its
    /// status row, because it is a fact about the turn that is running: results are held only so
    /// a running turn is not interrupted, and they are delivered when it ends.
    pub(crate) fn held_label(&self) -> Option<String> {
        let (jobs, subagents, failed) = self.held?;
        let n = |k: u64, one: &str, many: &str| match k {
            0 => None,
            1 => Some(format!("1 {one}")),
            k => Some(format!("{k} {many}")),
        };
        let kinds: Vec<String> = [
            n(jobs, "job", "jobs"),
            n(subagents, "subagent", "subagents"),
        ]
        .into_iter()
        .flatten()
        .collect();
        if kinds.is_empty() {
            return None;
        }
        let failed = match failed {
            0 => String::new(),
            f if f == jobs + subagents => " (failed)".to_string(),
            f => format!(" ({f} failed)"),
        };
        Some(format!("held: {}{failed}", kinds.join(" · ")))
    }

    /// **The turn is slow**: the model should be emitting and nothing has arrived for
    /// [`SLOW_AFTER_MS`]. It turns the Responding row yellow and says nothing else — the
    /// operator, 2026-10-08: *"I dont want notification that it is slow yet we continue"*. The
    /// sentence that used to sit above the composer (*"nothing received for 17s. The turn is
    /// still marked running"*) was there because a FAILED turn used to look like this; it ends
    /// with `TurnFailed` now, so silence here is only ever slowness.
    ///
    /// **`turn_generating` and deliberately NOT `turn_busy`**: a `cargo test` that runs silently
    /// for two minutes is a call, not a slow model, and gated on busy the row would go yellow
    /// through every long command — the false alarm that trains a reader to ignore it.
    pub(crate) fn turn_slow(&self) -> bool {
        self.turn.is_some()
            && self.turn_generating()
            && self.last_event_at != 0
            && self.now_ms.saturating_sub(self.last_event_at) > SLOW_AFTER_MS
    }
}
