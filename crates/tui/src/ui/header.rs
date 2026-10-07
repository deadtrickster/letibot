//! **The header**: the session, the model, the context, the clock.

use crate::app::*;
use crate::render::{dur_human, trim_to, visible_width};
use crate::ui::*;
use letibot_sessionlog::registry::SessionBrief;
use letibot_ui::progress;
use letibot_ui::style::Role;
use letibot_ui::text::without_control_lines;

impl App {
    /// The session header: which session, what it is talking to, and how big it
    /// has got.
    ///
    /// ```text
    ///   ▌ the cache question  ~/Projects/letibot   2/4 · glm-5.3-flash · 41.2k ctx · 92% cached · 45 tok/s · 12.3s · 1.2k out
    /// ```
    ///
    /// The model name is here and not on the composer's border, where it used to
    /// sit with the dialect and the endpoint beside it: the border is the row the
    /// eye crosses on every return to the field, and a socket address is not part
    /// of a sentence. The last three fields are the last turn's decode rate, wall
    /// time and output, and this is their only home: they used to close the turn
    /// footer, on a line that also repeated the context and cache numbers
    /// already above — one fact, two places, and the reader stops to check
    /// whether they agree. The footer keeps only the ending that is news; an
    /// ordinary one leaves no footer line at all.
    ///
    /// **What is deliberately not on it.** opencode's right-hand side reads
    /// `39,413  20% ($0.29)` — tokens, context *used as a percentage*, and money.
    /// The percentage needs the context window and the price needs a tariff, and
    /// this harness has neither: nothing on the wire carries `n_ctx`, and the model
    /// is on the other side of a Unix socket on this box and costs nothing per
    /// token. Rendering `20%` against a denominator nobody sent would be the same
    /// move as rendering `0.0s` for a call that was never timed.
    ///
    /// What is here instead is the number this harness exists to move and neither
    /// surveyed head can show at all: **how much of the prompt was cached**. It is
    /// `f_sim` — cached over *this* prompt — and it is labelled `cached`, never
    /// `f_keep`, which needs the previous turn's entry as its denominator.
    /// # It degrades by deletion, one field at a time
    ///
    /// The first version handed the two halves to `split_row`, which drops the
    /// **whole** right half when they do not both fit — correct for the in-flight
    /// line, where the left half is what is happening, and wrong here, where the
    /// right half is the part you cannot get any other way. Measured under tmux at
    /// 110 columns: an 82-column path plus a 27-column tail is 111, and the entire
    /// tail vanished with nothing to say it had. So the tail is built in priority
    /// order and the path is shortened from its left before anything is dropped —
    /// a path is recognisable from its end, and a token count is not recoverable
    /// from anywhere else on the screen.
    pub(crate) fn header_line(&self, w: usize) -> String {
        let p = self.cfg.palette();
        let name = self.session_label(&self.session_id);

        // Most valuable first: which of several sessions this is, then how big the
        // prompt has got, then how much of it the cache saved.
        let mut right: Vec<String> = Vec::new();
        // Shown for one session too. It used to be gated on `len() > 1`, and the
        // effect was that the *only* case with no session identity anywhere on the
        // screen — one untitled session, whose name is therefore an opaque id — was
        // also the case with no position indicator. Two absences do not add up to a
        // fact, and "1/1" is a fact: this daemon holds one session and you are in it.
        // **Conversations, not rows.** The header answers *which of several sessions this is*, and
        // a denominator that grew because somebody expanded a tree would be answering a question
        // about the PICKER. So both halves count roots — and the position is the current session's
        // ROOT, so a head driving a sub-session still says which conversation it belongs to instead
        // of reading `0/4`.
        let roots: Vec<&SessionBrief> = self
            .sessions
            .iter()
            .filter(|s| s.parent_session_id.is_none())
            .collect();
        let root = self.session_root();
        let at = roots
            .iter()
            .position(|s| s.session_id == root)
            .map(|i| i + 1)
            .unwrap_or(0);
        right.push(format!("{at}/{}", roots.len().max(1)));
        // What this session is talking to: the daemon's own word from `Hello`, or
        // — before that has arrived — the model the running turn named. It lived
        // on the composer's top border, the one row the eye crosses on every
        // return to the field; the header is where this session's facts live now.
        // The dialect and the endpoint do not ride along: the dialect's name is
        // the model's name whenever the two differ at all, and the endpoint is a
        // socket path, which is the daemon's business and not the sentence's.
        // **Whichever the head was told more recently**, and that is the whole fix
        // for the stale label.
        //
        // `wiring.model` is the daemon's word from `Hello`, sent once at attach. The
        // `model` settings row is republished *to the registry* whenever the provider
        // changes — but `ServerFrame::Settings` is only ever sent as the **answer to a
        // request**, so a head that attached earlier is never told. Found by reading
        // the protocol (2026-09-20): one send site, and it is in the `ClientFrame::
        // Settings` arm. So a mid-conversation switch updated the registry, the turns
        // and the config pane, and left every attached head drawing the model it read
        // at attach — the operator's *"still qwen"*.
        //
        // A turn, by contrast, **does** arrive unprompted and names the model that is
        // answering it (`TurnStarted`). So the two are compared by the sequence number
        // each arrived at, and the later one wins. `seq` is the only clock the head
        // has, and it is the right one: both are facts about the same stream.
        //
        // The general fix is to push the rows to attached heads instead of only
        // answering — `TODO.md` R20.1. This makes the head honest with the information
        // it is actually given.
        let from_settings = self
            .settings
            .iter()
            .find(|r| r.key == "model")
            .map(|r| header_model(&r.value))
            .filter(|m| !m.is_empty());
        let turn_is_newer = self.model_from_turn_at > self.model_from_settings_at;
        let model = match (from_settings, turn_is_newer) {
            (Some(row), false) => row,
            (Some(row), true) if self.model.is_empty() => row,
            (Some(_), true) => self.model.clone(),
            (None, _) => {
                if !self.model.is_empty() {
                    self.model.clone()
                } else if !self.wiring.model.is_empty() {
                    self.wiring.model.clone()
                } else {
                    self.model.clone()
                }
            }
        };
        if !model.is_empty() {
            right.push(model);
        }
        // Live prefill numbers win over the last turn's: while a turn is running,
        // "how big is this prompt" is a question about the prompt being sent. The
        // third element says whether the cache fraction is a measurement: a live
        // prefill always is, and a kept usage is one only if the turn that made it
        // measured its own cache rather than the row supplying a size without one.
        let usage: Option<(u64, u64, bool)> =
            match self.turn.as_ref().and_then(|t| t.progress.as_ref()) {
                Some(pp) if pp.total > 0 => Some((pp.total, pp.cache, true)),
                _ => self
                    .usage
                    .filter(|u| u.prompt_tokens > 0)
                    .map(|u| (u.prompt_tokens, u.cached_tokens, self.usage_cache_measured)),
            };
        // **The meter.** Beside the token count, because that is where the
        // question "what is this costing me" is already being asked.
        if self.spent_seen {
            right.push(format!("${:.4}", self.spent_micros as f64 / 1_000_000.0));
        }
        if let Some((total, cached, cache_measured)) = usage {
            right.push(format!("{} ctx", progress::thousands(total)));
            // A percentage nobody measured is refused, the rule the rate beside it
            // is held to: a row that carries the size but not the fraction shows
            // the size and says nothing about the cache.
            if cache_measured {
                right.push(format!(
                    "{:.0}% cached",
                    cached as f64 * 100.0 / total as f64
                ));
            }
        }
        // The last turn's speed and duration, measured when it ended. A rate nobody
        // measured is refused, the rule the footer's rate was held to when it lived
        // there: a turn that decoded nothing has no `predicted_ms`, and `0 tok/s`
        // would be a number nobody took. Dropped first on a narrow screen — the
        // context numbers are the ones this header exists for.
        if let (Some(u), Some(tm)) = (self.usage, self.last_timings) {
            if tm.predicted_ms > 0.0 {
                right.push(format!(
                    "{:.0} tok/s",
                    u.predicted_tokens as f64 * 1000.0 / tm.predicted_ms
                ));
            }
            // **WHILE A TURN RUNS THE DURATION IS THE TURN'S, and only the duration.**
            //
            // `last_timings` is measured when a round ENDS, and a round ends at every tool call and
            // every job — so this number restarted three times in a turn the operator watched, while
            // the row above the composer counted the turn. MEASURED on the other head's live screen,
            // the same pair side by side: `Responding · 151s` on the composer edge and `· 4.5s ·`
            // here, 151204 ms against 4474 ms. Two clocks for one turn, and the smaller one is the
            // one next to the word *Responding*, which reads as a clock.
            //
            // **The ruling is the operator's**: while a turn runs the duration shown is the TURN's
            // elapsed; when idle it falls back to the last turn's wall-ms, which is what this field
            // is for and what an idle header has always shown.
            //
            // **`tok/s` and the out-count are NOT moved with it.** They stay on the last round's
            // basis, because that is what `last_timings` is FOR — the operator's own earlier
            // complaint about them was *"the rate comes and goes"*, so a rate from the round that
            // just ended is the useful number and a duration from it is the false one. One field of
            // the trio, not the trio.
            //
            // **This DEPARTS from the reference deliberately.** The comment above records this line
            // as parity with the reference's `app.rs`, and after this change neither head matches
            // it. Recorded here rather than left to look like a defect: an operator reading a bare
            // duration beside a running state reads it as a clock, and they did.
            let running_since = self
                .turn
                .as_ref()
                .filter(|_| self.turn_busy())
                .map(|t| t.started_ms)
                .filter(|started| *started > 0);
            match running_since {
                Some(started) => right.push(dur_human(self.now_ms.saturating_sub(started))),
                None if tm.wall_ms > 0 => right.push(dur_human(tm.wall_ms)),
                None => {}
            }
            // And how much the answer was — the last of the turn's numbers, and
            // the reason an ordinary ending leaves the body with no footer line
            // at all.
            if u.predicted_tokens > 0 {
                right.push(format!("{} out", progress::thousands(u.predicted_tokens)));
            }
        }
        // Drop from the end until it leaves room for the name.
        let name_cols = visible_width(&name) + 2;
        while right.len() > 1 && name_cols + right.join(" · ").chars().count() + 2 > w {
            right.pop();
        }
        let tail = right.join(" · ");
        let tail_cols = if tail.is_empty() {
            0
        } else {
            tail.chars().count() + 2
        };

        // **The header is FACTS, and neither half of it is a sentence** (R51 item 10).
        //
        // The name used to open with `▌` in the user-accent register, and that glyph is how both
        // heads say *a person said this* — so on the row the reader crosses on every return to the
        // field, the session's own name read as somebody's message. The operator: *"the project
        // directory and session name are pinned in the first row with the same blue bar we use for
        // my messages. very confusing. just make both gray and remove the bar."*
        //
        // **Deleted, not recoloured**: a grey bar is still a bar and still makes the claim. And
        // the name loses `Strong` as well as the bar — one quiet register for the header, because
        // the two things on it are the same kind of fact (which session, and where) and a reader
        // sounding out which half is emphasised learns nothing from either.
        //
        // Note the departure this settles: leticl recorded the register as *its* choice against
        // letibot (`4110e7b`), and the operator is now asking for it here too.
        let mut left = String::new();
        left.push_str(&p.paint(Role::Faint, &without_control_lines(&name)));
        let mut left_cols = visible_width(&name);
        // **One label, and only for a subagent: `subagent of <parent>`.** This head can be
        // switched into a child session, and then the row that names the session named only
        // the child — so a screen showing somebody else's conversation looked like a screen
        // showing one's own, which is the whole of the operator's ask: *"when I \"Enter\"
        // Subagent it is like completely switching session with just one piece of info - a
        // Label that it is a subagent"*. It sits here, on the row that already names the
        // session, in the same faint register as the workspace and the branch — one fact of
        // the same kind, added to the same field, and **nothing else anywhere on the
        // screen**; the session's own tree of children is where it was, under `ctrl-g`.
        //
        // The parent is named from the daemon's own list (`parent_session_id`), and only
        // then: a session whose brief this head does not hold draws no label rather than a
        // guess about which session spawned it.
        if let Some(parent) = self.parent_session() {
            let of = format!("  subagent of {}", self.session_label(&parent));
            left.push_str(&p.paint(Role::Faint, &without_control_lines(&of)));
            left_cols += visible_width(&of);
        }
        // The workspace fills whatever is left, shortened from its *left*: the end
        // of a path is the part that identifies it.
        if !self.wiring.workspace.is_empty() {
            let path = tilde(&self.wiring.workspace);
            let room = w.saturating_sub(left_cols + tail_cols + 2);
            if room >= 8 {
                let shown = ellipsise_left(&path, room);
                left.push_str(&p.paint(Role::Faint, &format!("  {shown}")));
                left_cols += 2 + visible_width(&shown);
            }
            // **The workspace's repository, in gitstatus's own segments and colours** — the
            // operator's port of leticl's field, beside the path it is a fact about. Drawn
            // from the pieces the READER rendered (`refresh_git` applies the format; a paint
            // never formats), fitted here where the width is: the branch is the floor and the
            // marks fall off the right, so a narrow screen loses `?4` and not the branch. The
            // segments carry their own styles — a green branch, a yellow `!`, a red `~` — and
            // the parens are the row's own faint, so the field still reads as one thing.
            // An unreadable repository draws NOTHING rather than a blank that reads like a
            // clean tree (`gitfield`'s own rule).
            if let Some(pieces) = self.git.as_deref() {
                let room = w.saturating_sub(left_cols + tail_cols + 4);
                let fit = crate::gitfield::git_fit(pieces, room);
                if !fit.is_empty() {
                    left.push_str(&p.paint(Role::Faint, " ("));
                    let mut cols = 3usize;
                    for (text, role) in fit {
                        cols += visible_width(text);
                        left.push_str(&git_paint(&self.cfg, *role, text));
                    }
                    left.push_str(&p.paint(Role::Faint, ")"));
                    left_cols += cols;
                }
            }
        }
        let pad = w.saturating_sub(left_cols + tail.chars().count());
        trim_to(
            &format!("{left}{}{}", " ".repeat(pad), p.paint(Role::Faint, &tail)),
            w,
        )
    }
}

/// The `model` settings row, as the header shows it.
///
/// That row reads `local (glm-5.3-flash)` or `deepseek/deepseek-flash` — the
/// first form so the config pane still names the model the local server runs,
/// which is what it said before this row replaced the read-only one. The header
/// has one slot and wants the name: `glm-5.3-flash`, or the provider pair, which
/// is worth its width because it is also how you can tell you are being billed.
pub(crate) fn header_model(value: &str) -> String {
    match value
        .strip_prefix("local (")
        .and_then(|v| v.strip_suffix(')'))
    {
        Some(alias) => alias.to_string(),
        None => value.to_string(),
    }
}

impl App {
    /// **What the terminal's window title says**: the session's name and the folder, so a
    /// tab is told apart by the conversation in it rather than reading `leticode` like
    /// every other tab. The name is the header's (`session_label`: its title, or a short id
    /// before it has one); before a session is attached there is nothing to name but the
    /// program. The terminal strips control characters before writing it.
    pub fn window_title(&self) -> String {
        if self.session_id.is_empty() {
            return "letibot".to_string();
        }
        let label = self.session_label(&self.session_id);
        let titled = self
            .sessions
            .iter()
            .any(|s| s.session_id == self.session_id && !s.title.is_empty());
        let folder = std::path::Path::new(&self.wiring.workspace)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        // A name leads; a short id does not — before the first message (which titles the
        // session) the folder is the more useful word to find a tab by.
        match (titled, folder.is_empty()) {
            (_, true) => label,
            (true, false) => format!("{label} · {folder}"),
            (false, false) => format!("{folder} · {label}"),
        }
    }
}
