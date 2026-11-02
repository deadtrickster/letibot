//! The header: the session, the model, the context, the clock.

use crate::app::*;
use crate::ui::*;
use letibot_sessionlog::registry::SessionBrief;
use rano::agent::header::{Context, GitMark, Header};

impl App {
    /// The session header: which session, what it is talking to, and how big it has got.
    ///
    /// ```text
    ///   ▌ the cache question  ~/Projects/letibot   2/4 · glm-5.3-flash · 41.2k ctx · 92% cached · 45 tok/s · 12.3s · 1.2k out
    /// ```
    ///
    /// `cached` is cached over THIS prompt (`f_sim`, never `f_keep`, which needs the previous
    /// turn's entry); the last three fields are the last turn's rate, wall time and output, and
    /// this is their only home - a second copy of a number is where a reader stops to check they
    /// agree. It degrades by deletion: the tail is built in priority order and the path is
    /// shortened from its left first, because a path is recognisable from its end.
    // A test's door, and `cfg(test)` says so: its only readers are two assertions in
    // `app/tests/session.rs`. The drawing goes through `header_row`, which is where the click
    // target is read off what was drawn.
    #[cfg(test)]
    pub(crate) fn header_line(&self, w: usize) -> String {
        self.header_row(w).0
    }

    /// The header row, and where its workspace path sits on it: a rectangle read off what was
    /// drawn, never from the words' length, because the fit shortens the path from its left. `None`
    /// when no path is there: no target is safer than a pane for a tree nobody pointed at.
    pub(crate) fn header_row(&self, w: usize) -> (String, Option<(usize, usize)>) {
        let h = self.header_view();
        let line = h.line(w);
        // The spans before the path: the name, then `subagent of …` when there is one. The path's
        // span is recognised by its own leading two columns rather than by its index alone, since a
        // frame that dropped the path has the git field's lead or the padding there instead.
        let after = 1 + usize::from(h.subagent_of.is_some());
        let mut at = 0usize;
        let mut path = None;
        for (i, s) in line.spans.iter().enumerate() {
            if i == after
                && let Some(rest) = s.content.strip_prefix("  ")
                && !rest.is_empty()
                && !rest.starts_with('(')
                && !rest.trim().is_empty()
            {
                path = Some((at + 2, s.width() - 2));
                break;
            }
            at += s.width();
        }
        (crate::ui::render::row(&line, self.cfg.palette()), path)
    }

    /// **The header's facts, in rano's words** — `rano::agent::header` lays them out (most
    /// valuable first, dropped from the end until the name fits; the name, the parent and the
    /// workspace in one quiet register; the repository's segments in their own colours).
    pub(crate) fn header_view(&self) -> Header {
        let name = self.session_label(&self.session_id);
        // Shown for one session too: otherwise the one case with no session identity anywhere else
        // is also the case with no position indicator, and "1/1" is a fact. Both halves count ROOTS
        // and the position is the current session's root - this is which conversation, not a tree.
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
        let position = (at, roots.len());
        // What this session is talking to: whichever the head was told more recently. `wiring.model`
        // arrives once at attach and the `model` settings row only as the ANSWER to a request, so a
        // switch is heard only from a `TurnStarted`; the two are compared by the `seq` each arrived at.
        let from_settings = self
            .settings
            .iter()
            .find(|r| r.key == "model")
            .map(|r| header_model(&r.value))
            .filter(|m| !m.is_empty());
        let live_is_newer = self.model_from_live_at > self.model_from_settings_at;
        let model = match (from_settings, live_is_newer) {
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

        // Live prefill numbers win over the last turn's: while a turn runs, "how big is this
        // prompt" is a question about the prompt being sent. The third element says whether the
        // cache fraction is a measurement, which a kept usage is only if the turn measured it.
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
        let spent_micros = self.spent_seen.then_some(self.spent_micros);
        // A percentage nobody measured is refused, the rule the rate beside it is held to: a
        // row that carries the size but not the fraction shows the size and says nothing about
        // the cache.
        let context = usage.map(|(tokens, cached, cache_measured)| Context {
            tokens,
            cached,
            cache_measured,
        });
        // The last turn's speed and duration, measured when it ended. A rate nobody measured is
        // refused - a turn that decoded nothing has no `predicted_ms` - and this field is dropped
        // first on a narrow screen, because the context numbers are what this header exists for.
        let (mut tok_per_s, mut duration_ms, mut out_tokens) = (None, None, None);
        if let (Some(u), Some(tm)) = (self.usage, self.last_timings) {
            if tm.predicted_ms > 0.0 {
                tok_per_s = Some(u.predicted_tokens as f64 * 1000.0 / tm.predicted_ms);
            }
            // While a turn runs the header shows NO duration: `last_timings` is measured when a
            // ROUND ends, and a round ends at every tool call, so its wall-ms restarts inside one
            // turn. The composer row is the one clock then; `tok/s` and the out-count stay as they are.
            duration_ms = (!self.turn_busy() && tm.wall_ms > 0).then_some(tm.wall_ms);
            // And how much the answer was — the last of the turn's numbers, and
            // the reason an ordinary ending leaves the body with no footer line
            // at all.
            out_tokens = (u.predicted_tokens > 0).then_some(u.predicted_tokens);
        }
        use crate::gitfield::GitRole as R;
        Header {
            // **One label, and only for a subagent: `subagent of <parent>`**, named from the
            // daemon's own list (`parent_session_id`) and only then: a session whose brief this
            // head does not hold draws no label rather than a guess.
            subagent_of: self
                .parent_session()
                .map(|parent| self.session_label(&parent)),
            name,
            workspace: if self.wiring.workspace.is_empty() {
                String::new()
            } else {
                tilde(&self.header_workspace())
            },
            // The workspace's repository, in gitstatus's own segments, drawn from the pieces the
            // READER rendered. An unreadable repository is no pieces, and draws nothing rather than
            // a blank that reads like a clean tree.
            git: self
                .git
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|(text, role)| {
                    (
                        text.clone(),
                        match role {
                            R::BranchClean => GitMark::BranchClean,
                            R::BranchDirty => GitMark::BranchDirty,
                            R::Behind => GitMark::Behind,
                            R::Ahead => GitMark::Ahead,
                            R::Stash => GitMark::Stash,
                            R::Action => GitMark::Action,
                            R::Conflict => GitMark::Conflict,
                            R::Staged => GitMark::Staged,
                            R::Unstaged => GitMark::Unstaged,
                            R::Untracked => GitMark::Untracked,
                        },
                    )
                })
                .collect(),
            position,
            model,
            spent_micros,
            context,
            tok_per_s,
            duration_ms,
            out_tokens,
        }
    }
}

/// The `model` settings row, as the header shows it: `local (glm-5.3-flash)` or
/// `deepseek/deepseek-flash`. The header has one slot and wants the name - `glm-5.3-flash`, or the
/// provider pair, which is worth its width because it is also how you tell you are being billed.
pub(crate) use rano::agent::header::header_model;

impl App {
    /// What the terminal's window title says: the session's name and the folder, so a tab is told
    /// apart by the conversation in it rather than reading `leticode` like every other tab. The
    /// name is the header's; before a session is attached there is nothing to name but the program.
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
