//! **The header**: the session, the model, the context, the clock.

use crate::app::*;
use crate::ui::*;
use letibot_sessionlog::registry::SessionBrief;
use rano::agent::header::{Context, GitMark, Header};

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
        crate::ui::render::row(&self.header_view().line(w), self.cfg.palette())
    }

    /// **The header's facts, in rano's words** — `rano::agent::header` lays them out (most
    /// valuable first, dropped from the end until the name fits; the name, the parent and the
    /// workspace in one quiet register; the repository's segments in their own colours).
    pub(crate) fn header_view(&self) -> Header {
        let name = self.session_label(&self.session_id);
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
        let position = (at, roots.len());
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
        let spent_micros = self.spent_seen.then_some(self.spent_micros);
        // A percentage nobody measured is refused, the rule the rate beside it is held to: a
        // row that carries the size but not the fraction shows the size and says nothing about
        // the cache.
        let context = usage.map(|(tokens, cached, cache_measured)| Context {
            tokens,
            cached,
            cache_measured,
        });
        // The last turn's speed and duration, measured when it ended. A rate nobody
        // measured is refused, the rule the footer's rate was held to when it lived
        // there: a turn that decoded nothing has no `predicted_ms`, and `0 tok/s`
        // would be a number nobody took. Dropped first on a narrow screen — the
        // context numbers are the ones this header exists for.
        let (mut tok_per_s, mut duration_ms, mut out_tokens) = (None, None, None);
        if let (Some(u), Some(tm)) = (self.usage, self.last_timings) {
            if tm.predicted_ms > 0.0 {
                tok_per_s = Some(u.predicted_tokens as f64 * 1000.0 / tm.predicted_ms);
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
            duration_ms = match running_since {
                Some(started) => Some(self.now_ms.saturating_sub(started)),
                None if tm.wall_ms > 0 => Some(tm.wall_ms),
                None => None,
            };
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
                tilde(&self.wiring.workspace)
            },
            // **The workspace's repository, in gitstatus's own segments** — drawn from the pieces
            // the READER rendered (`refresh_git` applies the format; a paint never formats). An
            // unreadable repository is no pieces, and draws nothing rather than a blank that
            // reads like a clean tree.
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

/// The `model` settings row, as the header shows it.
///
/// That row reads `local (glm-5.3-flash)` or `deepseek/deepseek-flash` — the
/// first form so the config pane still names the model the local server runs,
/// which is what it said before this row replaced the read-only one. The header
/// has one slot and wants the name: `glm-5.3-flash`, or the provider pair, which
/// is worth its width because it is also how you can tell you are being billed.
pub(crate) use rano::agent::header::header_model;

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
