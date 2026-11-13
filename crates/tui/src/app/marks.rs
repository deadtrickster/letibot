//! What the conversation marks beside the text: the lines a reply cited, and the lines a finished
//! call changed, as rano's marks.
//!
//! Rano holds marks on a buffer, and a head opens a file long after the reply that marked it - so
//! the remembering is the head's, and a file that is not open is marked when it opens.

use super::*;
use letibot_sessionlog::event::ToolEdit;
use letibot_transcript::TranscriptItem;
use rano::marks::{Mark, Outcome};
use rano::review::Change;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The source a reply's marks are filed under: its row's id, so a newer reply on the same file
/// takes the older one's place rather than piling highlights on top of each other.
fn reply_source(item_id: &str) -> String {
    format!("reply:{item_id}")
}

/// The same for a call: the ROW's id and not the call's, because a call id is positional within a
/// round and rano replaces a source's whole set - two rounds naming `call_0` would lose each
/// other's hunks.
fn call_source(item_id: &str) -> String {
    format!("call:{item_id}")
}

/// A file's marks are filed under its canonical name, as rano compares its buffers.
pub(crate) fn key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The change one edit excerpt made: both sides of it, at each side's own first line.
pub(crate) fn change_of(e: &ToolEdit) -> Change {
    Change {
        before: e.before.clone(),
        after: e.after.clone(),
        before_start: e.before_start,
        after_start: e.after_start,
    }
}

/// The marks the conversation set, by file and then by the source that set them. Rano replaces a
/// source's whole set at a time, so the groups are kept apart here for the same reason.
#[derive(Default)]
pub(crate) struct FileMarks {
    by_file: BTreeMap<PathBuf, Vec<(String, Vec<Mark>)>>,
}

impl FileMarks {
    /// Record `source`'s marks on `path`, and answer the sources it put out: `reply:` marks are a
    /// remark about what the file said at the time, so only the newest reply's are drawn.
    pub(crate) fn set(&mut self, path: &Path, source: &str, marks: Vec<Mark>) -> Vec<String> {
        let key = key(path);
        let groups = self.by_file.entry(key.clone()).or_default();
        let mut gone: Vec<String> = Vec::new();
        if source.starts_with("reply:") {
            gone = groups
                .iter()
                .map(|(s, _)| s.clone())
                .filter(|s| s.starts_with("reply:") && s != source)
                .collect();
            groups.retain(|(s, _)| !gone.contains(s));
        }
        groups.retain(|(s, _)| s != source);
        if !marks.is_empty() {
            groups.push((source.to_string(), marks));
        }
        if groups.is_empty() {
            self.by_file.remove(&key);
        }
        gone
    }

    /// Every group `path` holds, in the order they were set.
    pub(crate) fn groups(&self, path: &Path) -> &[(String, Vec<Mark>)] {
        self.by_file
            .get(&key(path))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// A decided mark is gone for good: the operator kept it, dropped it or put the text back, and
    /// opening the file again must not offer it a second time.
    pub(crate) fn forget(&mut self, id: &str) {
        for groups in self.by_file.values_mut() {
            for (_, marks) in groups.iter_mut() {
                marks.retain(|m| m.id != id);
            }
            groups.retain(|(_, marks)| !marks.is_empty());
        }
        self.by_file.retain(|_, groups| !groups.is_empty());
    }

    /// Everything goes: the marks of a conversation this head is no longer reading.
    pub(crate) fn clear(&mut self) {
        self.by_file.clear();
    }
}

impl App {
    /// What one row marks: the lines its reply cited, and the lines its call changed. Filed by row
    /// rather than by event, because a row is what both a live frame and a snapshot carry.
    pub(crate) fn marks_of_row(&mut self, idx: usize) {
        let Some(it) = self.items.get(idx) else {
            return;
        };
        let item_id = it.item_id.clone();
        let citations = it.citations.clone();
        // The row's own excerpt, or what this head watched the call write when the announcement
        // carried none - the same pair `change_of_row` opens.
        let edits = match &it.item {
            Some(TranscriptItem::ToolResult { edits, .. }) => self
                .call_edits
                .get(&item_id)
                .cloned()
                .unwrap_or_else(|| edits.clone()),
            _ => Vec::new(),
        };
        if !citations.is_empty() {
            let mut by_file: BTreeMap<PathBuf, Vec<Mark>> = BTreeMap::new();
            for c in &citations {
                // A range the daemon could not read has no anchor to land on, and the row already
                // carries the sentence saying why it is empty.
                if c.lines.is_empty() {
                    continue;
                }
                let id = format!("cite:{item_id}:{}", c.label);
                let mark = Mark::cite(&id, c.start as usize, c.end as usize, c.lines.clone());
                by_file
                    .entry(self.resolve_path(&c.path))
                    .or_default()
                    .push(mark);
            }
            let source = reply_source(&item_id);
            for (path, marks) in by_file {
                self.give_marks(&path, &source, marks);
            }
        }
        if !edits.is_empty() {
            let mut by_file: BTreeMap<PathBuf, Vec<Mark>> = BTreeMap::new();
            for (i, e) in edits.iter().enumerate() {
                let id = format!("edit:{item_id}:{i}");
                let mark = Mark::edit(&id, change_of(e));
                by_file
                    .entry(self.resolve_path(&e.path))
                    .or_default()
                    .push(mark);
            }
            let source = call_source(&item_id);
            for (path, marks) in by_file {
                self.give_marks(&path, &source, marks);
            }
        }
    }

    /// Every row's marks, derived from the rows themselves: what a snapshot replays is the only
    /// record there is, so a re-attached head's gutter and hunks come back from the transcript.
    pub(crate) fn marks_from_rows(&mut self) {
        self.file_marks.clear();
        for idx in 0..self.items.len() {
            self.marks_of_row(idx);
        }
    }

    /// Keep `source`'s marks on one file, and put them in front of the operator when that file is
    /// the one the pane is showing.
    fn give_marks(&mut self, path: &Path, source: &str, marks: Vec<Mark>) {
        let gone = self.file_marks.set(path, source, marks.clone());
        let Some(p) = self.edit_pane.as_mut() else {
            return;
        };
        let key = key(path);
        // Only a file rano has open has anything to replace: the rest wait for `mark_the_shown_file`.
        if p.shown().as_deref() != Some(key.as_path()) {
            return;
        }
        for old in gone {
            p.ed.clear_marks(Some(&old));
        }
        p.ed.set_marks(&key, source, marks);
        p.dirty = true;
    }

    /// Hand the pane every mark the conversation set on the file it has just opened. Once per file
    /// it switches to, because pushing again would put a decided hunk back on the screen.
    pub(crate) fn mark_the_shown_file(&mut self) {
        let Some(p) = self.edit_pane.as_mut() else {
            return;
        };
        // A file still loading has no buffer to carry marks yet, and its next tick asks again.
        if p.ed.loading() {
            return;
        }
        let shown = p.shown().map(|n| key(&n));
        if shown == p.marked {
            return;
        }
        p.marked = shown.clone();
        let Some(path) = shown else {
            return;
        };
        let groups = self.file_marks.groups(&path).to_vec();
        if groups.is_empty() {
            return;
        }
        for (source, marks) in groups {
            p.ed.set_marks(&path, &source, marks);
        }
        p.dirty = true;
    }

    /// A decided hunk stops being offered: rano dropped it from the buffer, and the copy kept here
    /// goes with it. What the operator decided is L8's to send on the next prompt.
    pub(crate) fn forget_decided_marks(&mut self, decided: &[(String, Outcome)]) {
        for (id, _) in decided {
            self.file_marks.forget(id);
        }
    }
}
