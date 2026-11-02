//! The standing-notes pane's state: the cursor's row, and the disk facts the rows are drawn from.
//! The form the budget gave each file is the daemon's, decided with the session's own token
//! counter, and this head must not grow a second counter or the pane would contradict the prompt.

use super::editor::FileRef;
use super::*;

/// A note's disk facts, read at draw time - and `None` from `OnDisk::read` is the file being gone,
/// which is the one answer the row must never dress up. One `stat` per row per draw, because the
/// file is edited or deleted while somebody is looking at the pane.
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

/// The corpus as the disk has it: what the pane compares between two draws to know whether its rows
/// are still the rows the budget would give, since the form is the daemon's answer and the size is
/// the head's. Directories are watched too, so a note just written is a change, not a silence.
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
    /// The pane row the note at the cursor was drawn on, read out of `standing_stop_rows` - the record
    /// the pane wrote while drawing, never arithmetic over the list it drew from. The clamp is the
    /// jobs pane's: the list can change under the cursor.
    pub(crate) fn standing_row_of(&self) -> usize {
        let at = self
            .standing_sel
            .min(self.standing_stop_rows.len().saturating_sub(1));
        self.standing_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// Enter: open the note the cursor is on in the editor pane. It opens the FILE and not the text,
    /// because an indexed note is one the prompt does not carry whole, so drawing the prompt's words
    /// would show the index and call it the note. A file that is gone is said, and nothing opens.
    pub(crate) fn open_note(&mut self) {
        let Some(entry) = self.standing.get(self.standing_sel) else {
            return;
        };
        self.open_in_editor(&FileRef::file(&entry.path));
        self.redraw = true;
    }
}
