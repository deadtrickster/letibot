//! The other BOARDS a session's `todo` row may wait on (`docs/seat-todo.md`): an external seat's,
//! kept in `seat_todo`, and another session's of this project, kept in `todo`. One reader, seated
//! on `todo_write` and used by the wait sweep in `harness::nags`, so the write and the release
//! cannot disagree about what a name means.

use std::path::Path;

use letibot_tokencore::store::Store;
use letibot_tools::builtins::todo::{Board, Boards, board_of, folder_words};

use crate::config::StoreAt;

/// The daemon's door onto another board: the store the session's own rows live in, and the folder
/// the daemon serves - which is what makes two boards the same project.
pub struct SeatBoards {
    at: Option<StoreAt>,
    workspace: String,
}

impl SeatBoards {
    pub fn new(at: Option<StoreAt>, workspace: impl Into<String>) -> SeatBoards {
        SeatBoards {
            at,
            workspace: workspace.into(),
        }
    }

    /// The store, opened per call: [`Store::open`] hands back the process's own handle for a file
    /// it already holds, so this is a lock and an upgrade rather than a second connection.
    fn store(&self) -> Result<Store, String> {
        let at = self.at.as_ref().ok_or_else(|| {
            "this session has no store, so no other board can be read".to_string()
        })?;
        at.open_existing()
            .map_err(|e| format!("the store would not open: {e}"))
    }

    /// The words this project is known by, for the one rule that keeps a wait inside a project.
    fn words(&self) -> Vec<String> {
        folder_words(Path::new(&self.workspace))
    }
}

impl Boards for SeatBoards {
    fn board_of(&self, seat: &str) -> Result<Board, String> {
        board_of(&self.store()?, &self.words(), seat)
    }
}
