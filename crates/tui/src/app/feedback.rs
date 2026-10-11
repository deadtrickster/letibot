//! **The operator's marks** (TODO R81, `docs/metrics.md` §2.5): `/fb`, `/fb+`, `/fb-`, and the
//! two keys the operator chose — alt-g marks the current turn good, alt-b bad (decision 4).
//!
//! A mark is one [`Action::Feedback`], sent as its own frame (`PROTOCOL_VERSION` 45) and answered
//! on the daemon's connection thread — never queued behind a busy worker. The daemon places it on
//! the turn running now; when the view is scrolled back, on the row the view is anchored to, which
//! is this head's nearest thing to "a selected past row" (there is no row cursor to put an `f`
//! on). The confirmation comes back as a routine `feedback` warning, drawn like any other.
//!
//! **alt-b is readline's word-left**, and this head has always decoded it so. It is a mark only
//! on an EMPTY composer with nothing in front of the conversation — the `ctrl-e` rule, where a
//! motion key has nothing to move over — and word-left everywhere else; `ctrl-left` is word-left
//! always. alt-g was nobody's, and marks whenever no card owns the keys.
//!
//! **A second press within five seconds opens the note**: the composer is filled with `/fb- ` or
//! `/fb+ ` so the word that explains the mark is one sentence away, not a command to remember.

use super::*;

/// How long a second alt-g / alt-b reads as "and I want to say why".
const AGAIN_MS: u64 = 5_000;

impl App {
    /// **`/fb`, `/fb+`, `/fb-`** — `None` when `cmd` is not one of them (the caller goes on to
    /// the other verbs), `Some(action)` when it is.
    pub(crate) fn feedback_command(&mut self, cmd: &str) -> Option<Option<Action>> {
        let rest = cmd.trim().strip_prefix("fb")?;
        let (polarity, note) = match rest.chars().next() {
            None => ("note", ""),
            Some('+') => ("good", &rest[1..]),
            Some('-') => ("bad", &rest[1..]),
            Some(c) if c.is_whitespace() => ("note", rest),
            // `/fbx` is some other word.
            Some(_) => return None,
        };
        let note = note.trim();
        if polarity == "note" && note.is_empty() {
            self.say(
                "/fb NOTE marks this moment with a note · /fb+ and /fb- mark it good or bad · \
                 #words in the note are tags · alt-g / alt-b mark it without typing",
            );
            return Some(None);
        }
        Some(self.mark(polarity, note))
    }

    /// **alt-g / alt-b**, settled: a mark, or (twice within five seconds) the composer opened
    /// on the note for it.
    pub(crate) fn feedback_key(&mut self, good: bool) -> Option<Action> {
        let now = letibot_sessionlog::event::now_ms();
        if let Some((was_good, at)) = self.fb_last
            && was_good == good
            && now.saturating_sub(at) <= AGAIN_MS
        {
            self.fb_last = None;
            // Only into an empty composer: what the operator was typing is theirs.
            if self.editor.is_empty() {
                self.editor.insert(if good { "/fb+ " } else { "/fb- " });
                self.redraw = true;
            }
            return None;
        }
        self.fb_last = Some((good, now));
        self.mark(if good { "good" } else { "bad" }, "")
    }

    /// Whether alt-g may mark right now: nothing in front of the conversation that owns the
    /// keys. alt-b additionally needs an empty composer — see the module header.
    pub(crate) fn feedback_key_free(&self, needs_empty: bool) -> bool {
        (!needs_empty || self.editor.text().is_empty())
            && self.mode_confirm.is_none()
            && self.todo_draft.is_none()
            && self.secret.is_none()
            && self.term_ask.is_none()
            && self.prompt.is_none()
            && self.key_ask.is_none()
            && self.diff_popup.is_none()
            && self.payload_sel.is_none()
            && !self.pane_open()
            && self.surfaces.is_empty()
    }

    fn mark(&mut self, polarity: &str, note: &str) -> Option<Action> {
        if self.session_id.is_empty() {
            self.say("not attached to a session yet — there is nothing to mark");
            return None;
        }
        Some(Action::Feedback {
            at_ms: letibot_sessionlog::event::now_ms(),
            polarity: polarity.to_string(),
            note: note.to_string(),
            // Scrolled back: the row the view is held on. Following: now.
            item_id: self.anchor.as_ref().map(|h| h.item_id.clone()),
        })
    }
}
