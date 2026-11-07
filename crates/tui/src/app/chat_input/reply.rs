//! What the next line answers: the `seq` that `R` set, and the quote the composer shows above the
//! field. The send turns it into the `>N` prefix `rooms_say` already threads on, so a reply by
//! selection and a `>N text` typed by hand are one path.

use crate::app::*;

impl App {
    /// The message the next line answers.
    pub(crate) fn rooms_reply_target(&self) -> Option<u64> {
        self.compose.reply
    }

    pub(crate) fn rooms_set_reply(&mut self, seq: Option<u64>) {
        self.compose.reply = seq;
    }

    /// The strip above the field: `replying to #14 gatekeeper: merged ...`, the message's first
    /// line. A target whose message is not loaded still names its seq, because a reply is a seq.
    pub(crate) fn rooms_reply_quote(&self) -> Option<String> {
        let seq = self.rooms_reply_target()?;
        let Some(m) = self.rooms_message(seq) else {
            return Some(format!("replying to #{seq}"));
        };
        let who = self.rooms_display_name(&m.author);
        let first = m.text.lines().next().unwrap_or("").trim();
        Some(if first.is_empty() {
            format!("replying to #{seq} {who}")
        } else {
            format!("replying to #{seq} {who}: {first}")
        })
    }
}
