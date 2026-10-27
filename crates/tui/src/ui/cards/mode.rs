//! **The mode card's confirmation line.**

use crate::app::*;

impl App {
    /// The line the screen shows while the `allow-all` card is up.
    ///
    /// **One warning for both ways the card arrives**, because the thing being agreed to is
    /// the same either way: the operator typed `/mode allow-all`, or another head asked for it
    /// (`leticode_mode`, B8). What differs is that the second one is somebody else's wish, so
    /// the line opens by saying who asked — a card raised by a program that read as the
    /// person's own keystroke would be the worst kind of quiet.
    pub(crate) fn mode_confirm_line(&self) -> Option<String> {
        self.mode_confirm.as_ref().map(|_| {
            let what = "allow-all: privilege escalation, deletes outside the project and \
                        first contact with a new host all stop asking. On this box that is \
                        this box. It lasts for this session only, and a daemon restart drops \
                        it.  [y] or [enter] confirm   [esc] or any other key cancels";
            match &self.mode_asked {
                Some(asked) => format!("`{}` asked this session for {what}", asked.by),
                None => what.to_string(),
            }
        })
    }
}
