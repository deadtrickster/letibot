//! The operator's marks: `/fb`, `/fb+`, `/fb-`, and alt-g / alt-b for the turn running now.
//! alt-b is readline's word-left everywhere except an empty composer with nothing in front of the
//! conversation, where it marks; a second press within five seconds opens the note.

use super::*;

/// How long a second alt-g / alt-b reads as "and I want to say why".
const AGAIN_MS: u64 = 5_000;

impl App {
    /// `/fb`, `/fb+`, `/fb-` - `None` when `cmd` is not one of them (the caller goes on to
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

    /// alt-g / alt-b, settled: a mark, or (twice within five seconds) the composer opened
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
    /// keys. alt-b additionally needs an empty composer - see the module header.
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
