//! The line the `allow-all` card shows: one warning, for both ways it arrives.

use crate::app::*;

impl App {
    /// The line the screen shows while the `allow-all` card is up; one warning for both
    /// doors, because what is being agreed to is the same. A card raised by another head
    /// opens by saying who asked - a program's wish must not read as the operator's own key.
    pub(crate) fn mode_confirm_line(&self) -> Option<String> {
        self.mode_confirm.as_ref().map(|_| {
            let what = "allow-all: privilege escalation, deletes outside the project and \
                        first contact with a new host all stop asking. On this box that is \
                        this box. It lasts for this session only, and a daemon restart drops \
                        it.  [y] or [enter] confirm   [n] refuses   [esc] puts it away";
            match &self.mode_asked {
                Some(asked) => format!("`{}` asked this session for {what}", asked.by),
                None => what.to_string(),
            }
        })
    }
}
