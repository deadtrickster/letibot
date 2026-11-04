//! The turn's status row: what the model is doing - green while it goes, yellow when slow.

use crate::app::*;
use crate::ui::render::{row, trim_to, visible_width};
use letibot_sessionlog::activity::Waiting;
use rano::agent::turn_status::{Prefill, SLOW_AFTER_MS, TurnStatus};
use rano::render::{Line, Role};

/// The fewest columns a transient is cut to before the status row drops it instead: below
/// this a cut sentence is an ellipsis with a word in front of it, which says nothing.
pub(crate) const TRANSIENT_MIN: usize = 12;

impl App {
    /// The turn's status, inlaid in the composer's bottom border: spinner, phase, and how much
    /// once anything arrived. `Prefill`'s `processed` INCLUDES `cache`, so the fraction is
    /// `processed / total` and the work `processed - cache`. Spinner and clock run on `now_ms`.
    #[cfg(test)]
    pub(crate) fn turn_status(&self, w: usize) -> String {
        row(&self.turn_line(w), self.cfg.palette())
    }

    /// [`App::turn_status`] as a [`Line`], so [`App::status_row`] can add to it before painting.
    fn turn_line(&self, w: usize) -> Line {
        let t = match self.turn.as_ref() {
            Some(t) if self.turn_busy() => t,
            // No turn running, but the session is not idle either: it waits on a job, a
            // settlement, a monitor or a person. The row is reserved whether or not a turn runs,
            // so this takes no line from the conversation.
            _ => return self.activity_line(w),
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
        // What this turn is holding, and a switch waiting for it to end - the two facts
        // about NOW that are neither the phase nor the progress. Joined into ONE faint span so
        // a narrow row cuts the last word rather than losing a whole segment's separator.
        let mut extra: Vec<String> = Vec::new();
        if let Some(held) = self.held_label() {
            extra.push(held);
        }
        if let Some(pending) = self.model_pending.clone() {
            extra.push(pending);
        }
        if !extra.is_empty() {
            line.spans.push(rano::render::Span::role(
                format!(" · {}", extra.join(" · ")),
                Role::Faint,
            ));
            line = rano::render::text::truncate_owned(line, w);
        }
        line
    }

    /// What this session is waiting on, one muted line - `waiting on: job j8032 · monitor ci`.
    ///
    /// The daemon's own list (`PROTOCOL_VERSION` 52), drawn where a turn's state is drawn. It exists for the case no transcript row can say:
    /// a session whose turn has ended but which will act again on its own, because a job the plan depends on is still running.
    ///
    /// Only when no turn is running: a running turn's own row owns the slot, and listing what is running beside `Responding · 2m30s · …`
    /// says the same thing twice. Empty for a quiet session and for a daemon older than 52, which keeps it invisible to a head talking to one.
    ///
    /// The daemon's [`letibot_sessionlog::activity::Activity::line`] read, on the operator's screen:
    /// ```text
    /// waiting on: monitor letibot-gate-after-wired · child s-1793010881933039000-sub-1793225939092557000 · child s-1793010881933039000-sub-1793225939285082000 · child s-1793010881933039000-sub-1793225939482158000
    /// ```
    ///
    /// A session id is not a name a person can use, and three of them push the monitor off a narrow screen.
    /// [`App::waiting_words`] names a child the way the subagents pane does and counts a kind that repeats.
    fn activity_line(&self, w: usize) -> Line {
        let mut text = self.waiting_words();
        // A switch waiting for a turn reads here too, for the one idle case: a head that
        // attached mid-turn folds `model_switch_pending` out of the replayed log. Same state,
        // same words, whichever line has the room.
        if let Some(pending) = self.model_pending.clone() {
            if text.is_empty() {
                text = pending;
            } else {
                text = format!("{text} · {pending}");
            }
        }
        if text.is_empty() {
            return Line::default();
        }
        let mut line = Line::default();
        line.spans
            .push(rano::render::Span::role(trim_to(&text, w), Role::Faint));
        line
    }

    /// The daemon's list in a person's words: the whole of [`App::activity_line`]'s text and the
    /// one place a [`Waiting`] is spelled. A child is named by its slug, not its id, and a kind
    /// that repeats is counted - the order stays the daemon's (`Waiting::rank`).
    fn waiting_words(&self) -> String {
        let list = self.activity.waiting_on();
        if list.is_empty() {
            return String::new();
        }
        // Grouped by KIND in first-seen order, which is the daemon's order - a run of one kind is
        // what the count is about, and a `Waiting` the daemon sent out of order still groups.
        let mut groups: Vec<(&'static str, Vec<&Waiting>)> = Vec::new();
        for w in list {
            match groups.iter_mut().find(|(k, _)| *k == w.kind()) {
                Some((_, group)) => group.push(w),
                None => groups.push((w.kind(), vec![w])),
            }
        }
        let parts: Vec<String> = groups
            .iter()
            .map(|(_, group)| match group.as_slice() {
                [one] => self.one_waiting(one),
                many => format!("{} {}", many.len(), plural(many[0])),
            })
            .collect();
        format!("waiting on: {}", parts.join(" · "))
    }

    /// One entry of the list, named: a child by the name the subagents pane gives it, everything
    /// else by the daemon's own label (`job j8032`, `monitor ci`, `ask adj-s1-0001`, `turn`).
    fn one_waiting(&self, w: &Waiting) -> String {
        match w {
            Waiting::Child(id) => format!("child {}", self.child_label(id)),
            _ => w.label(),
        }
    }

    /// The status row whole - the turn's words, then a progress, then the transients - and what a
    /// click opens when the row left something out. The fit ladder reserves this row whether or
    /// not a turn runs, so the transients are drawn IN it and the transcript loses no line.
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

    /// The transient one-liners, in the order the row gives them up last-first: the store being
    /// newer than this daemon first (a cut row drops from the END, and "restart this daemon" most
    /// needs its room), then the link, the boot step, the switch, the notice.
    pub(crate) fn transients(&self) -> Vec<(String, Role, bool)> {
        let mut out = Vec::new();
        if let Some(said) = &self.store_newer {
            out.push((format!("⚠ {said}"), Role::Failure, false));
        }
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
        // A switch in flight: the rows on screen are the session being LEFT, so without this the
        // head says nothing about the only thing happening. Above the notice, being a state
        // rather than an acknowledgement; it goes when that session's `Hello` lands.
        if let Some(to) = self.switching_to() {
            out.push((format!("switching to {to}…"), Role::Pending, false));
        }
        if let Some(n) = &self.notice {
            out.push((format!("· {n}"), Role::Keyword, true));
        }
        out
    }

    /// The link is down and has been for less than [`LINK_IMPATIENT_MS`]: the reason, for the
    /// status row. `None` while attached, and `None` once the head is impatient - from then on
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

    /// What the daemon is holding back from the model, in a few words (`held: 2 jobs · 1
    /// subagent`), or `None`. Results are held only so a running turn is not interrupted, so it
    /// sits after the turn's own words.
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

    /// The turn is slow: the model should be emitting and nothing has arrived for
    /// [`SLOW_AFTER_MS`]. Gated on `turn_generating` and NOT `turn_busy`, because a silent
    /// `cargo test` is a call, not a slow model, and a yellow row through it trains readers off.
    pub(crate) fn turn_slow(&self) -> bool {
        self.turn.is_some()
            && self.turn_generating()
            && self.last_event_at != 0
            && self.now_ms.saturating_sub(self.last_event_at) > SLOW_AFTER_MS
    }
}

/// A kind's plural, for the count: `3 subagents`, `2 monitors`, `2 jobs`. The singular stays the
/// daemon's `child <name>`, while a count is a number of things out there. A `match` over the
/// enum, so a new kind cannot arrive without a word.
fn plural(w: &Waiting) -> &'static str {
    match w {
        Waiting::Turn => "turns",
        Waiting::Calls => "calls",
        Waiting::Delivery => "deliveries",
        Waiting::Job(_) => "jobs",
        Waiting::Monitor(_) => "monitors",
        Waiting::Child(_) => "subagents",
        Waiting::Ask(_) => "asks",
    }
}
