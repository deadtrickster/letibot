//! **The standing-notes pane's state**: which row the cursor is on, and the disk facts the rows
//! are drawn from.
//!
//! The rows themselves come from the daemon (`ListNotes`), because the two fields that make
//! the pane worth having — the abstract the index shows and the FORM the budget gave each
//! file — are decided where the section is assembled, with the session's own token counter.
//! This head holds no counter and must not grow one: two functions each deciding what a note
//! *is* is two answers, and the pane would then be able to disagree with the prompt about the
//! very thing it is showing.
//!
//! What is left here is the disk's half. A file's size, its mtime and whether it is still
//! there are not the index's facts: they change while the pane is open, and the one case that
//! must never be drawn as a row that looks fine — **the index names a note the disk no longer
//! has** — is a comparison between the two that only a fresh `stat` can make.
//!
//! **And the row's Enter, which is not a read of this head's own.** A note opens in the editor
//! pane, on its file, through the door every other file opens through (`App::open_in_editor`) —
//! see [`App::open_note`] for why the file rather than the text.

use super::editor::FileRef;
use super::*;

/// **A note's disk facts, read at draw time** — and `None` from [`OnDisk::read`] is the file
/// being gone, which is the one answer the row must never dress up.
///
/// One `stat` per row per draw, the rule the todos pane's `refresh_repo_todos` states for the
/// same reason: the file is edited — or deleted — while somebody is looking at the pane, and a
/// fact cached at open is a fact about a moment that has passed.
pub(crate) struct OnDisk {
    pub(crate) bytes: u64,
    /// Milliseconds since the epoch, from the file's own mtime. `None` for a filesystem that
    /// would not say, which is drawn as *at an unknown time* rather than as 1970.
    pub(crate) mtime_ms: Option<u64>,
}

impl OnDisk {
    /// `None` — the file is not there — for every failure, and no attempt to tell *deleted*
    /// from *unreadable*: both are *the index names something this disk does not have*, which
    /// is the one sentence the row owes.
    pub(crate) fn read(path: &str) -> Option<OnDisk> {
        let meta = std::fs::metadata(path).ok()?;
        Some(OnDisk {
            bytes: meta.len(),
            mtime_ms: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64),
        })
    }
}

/// **The corpus as the disk has it** — what the pane compares between two draws to know
/// whether the rows it is showing are still the rows the budget would give.
///
/// The same trick `repo_todos_at` keeps for `TODO.md`, one pane over and for a sharper
/// reason: **the form on a row is the daemon's answer and the size beside it is the head's**,
/// taken at two different moments — so a note edited while the pane is open would draw a new
/// size next to the form it had before the edit, and the form is the field the whole pane
/// exists for. The head cannot decide the form (it holds no token counter; see
/// `app::standing`), so what it does is notice that the corpus moved and ask again.
///
/// The two halves are the two ways a note can change: a file's own `(mtime, bytes)`, and a
/// **directory's** mtime and count of `*.md` entries, so a note that has just been WRITTEN is
/// a change and not a silence.
#[derive(Clone, PartialEq, Eq, Default)]
pub(crate) struct Corpus {
    /// Each note the rows name: its path and `(mtime_ms, bytes)`, or `None` when it is gone.
    files: Vec<(String, Option<(u64, u64)>)>,
    /// Each directory worth watching, in the order it was first named.
    dirs: Vec<(String, Option<(u64, usize)>)>,
}

impl Corpus {
    /// Fold one note in, from the `stat` the row's own draw has already taken — so the watch
    /// costs a `read_dir` per directory and no second pass over the files.
    pub(crate) fn note(&mut self, path: &str, disk: Option<&OnDisk>) {
        self.files.push((
            path.to_string(),
            disk.map(|d| (d.mtime_ms.unwrap_or(0), d.bytes)),
        ));
        if let Some(dir) = std::path::Path::new(path).parent() {
            self.watch_dir(dir);
        }
    }

    /// **Watch a directory whether or not any row sits in it** — the workspace's own notes
    /// directory, so that a note which does not exist YET is still a change when it is
    /// written, and an empty pane does not sit there claiming there are none.
    pub(crate) fn watch_dir(&mut self, dir: &std::path::Path) {
        let key = dir.display().to_string();
        if self.dirs.iter().any(|(d, _)| *d == key) {
            return;
        }
        self.dirs.push((key, dir_signature(dir)));
    }
}

/// A directory's own mtime and how many `*.md` files it holds: the two facts that move when a
/// note is written, renamed or deleted. `None` for a directory that is not there — an absent
/// notes directory is a real state, and one a first note is a change in.
fn dir_signature(dir: &std::path::Path) -> Option<(u64, usize)> {
    let md = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .count();
    let mtime = std::fs::metadata(dir)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Some((mtime, md))
}

impl App {
    /// **The pane row the note at the cursor was DRAWN on**, read out of
    /// [`App::standing_stop_rows`] — the record the pane wrote while drawing, never arithmetic
    /// over the list it drew from. The clamp is the jobs pane's: the list can change under the
    /// cursor, and an arrow pressed against a shorter list must land on a row rather than on
    /// an index that no longer exists.
    pub(crate) fn standing_row_of(&self) -> usize {
        let at = self
            .standing_sel
            .min(self.standing_stop_rows.len().saturating_sub(1));
        self.standing_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// **Enter: open the note the cursor is on, in the editor pane** — rano, the same pane a
    /// click on an edit row and `ctrl-]` open, through the same door
    /// ([`App::open_in_editor`]).
    ///
    /// **It opens the FILE, and that is the point of the key.** A note the budget gave
    /// `indexed` is one the prompt does NOT carry whole — its headings and line ranges are all
    /// the model was given — so a pane drawing what the prompt has would be showing the index
    /// of a note and calling it the note. The file is where all of it is, for both forms, and
    /// rano is where a reader reads and edits a file in this head.
    ///
    /// **A file that is gone is SAID, and nothing is opened.** The row already draws `gone —
    /// the file is not there` for it; this turns rano's own refusal to open it into a sentence
    /// and leaves the list exactly where it was, on the row that said it, so the reader is not
    /// left looking at a conversation wondering what happened to their note.
    ///
    /// **Already open on another file.** The editor pane is made once and kept, so this is
    /// rano switching to the note — or to the buffer that already shows it — with whatever
    /// else was open left as it was. The one way a reader can be here with the pane already up
    /// is a pane put away with `ctrl-e`: while the editor is on the screen it has the keyboard
    /// and this list does not (see [`App::key_standing_pane`]), so the list under it is not
    /// what is being looked at.
    ///
    /// **And the list stays open under the pane**, which is what makes leaving rano (`^X`,
    /// `ctrl-q`) land back on the rows with the cursor where it was — the same round trip the
    /// diff popup and the editor already have.
    pub(crate) fn open_note(&mut self) {
        let Some(entry) = self.standing.get(self.standing_sel) else {
            return;
        };
        self.open_in_editor(&FileRef::file(&entry.path));
        self.redraw = true;
    }
}
