//! **The mode card's confirmation line.**

use crate::app::*;

impl App {
    pub(crate) fn mode_confirm_line(&self) -> Option<String> {
        self.mode_confirm.as_ref().map(|_| {
            "allow-all: privilege escalation, deletes outside the project and \
             first contact with a new host all stop asking. On this box that is \
             this box. It lasts for this session only, and a daemon restart drops \
             it.  [y] or [enter] confirm   [esc] or any other key cancels"
                .into()
        })
    }
}
