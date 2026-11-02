//! The lines this session was shown of each file, and what it may therefore change.
//!
//! An `edit` may only replace lines a tool displayed: a `read` window, a search hit with its context, or
//! the region of this session's own earlier edit, and never their neighbours. One fingerprint per shown
//! line, not an mtime and not a whole-file digest: an mtime moves on a `touch`, and the question is "did
//! these lines change".

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::sync::Mutex;

use crate::edit::display_lines;

/// One line's fingerprint. `DefaultHasher::new()` is keyed the same way for the
/// life of the process, which is as long as this ledger lives.
fn fp_line(line: &str) -> u64 {
    let mut h = DefaultHasher::new();
    h.write(line.as_bytes());
    h.finish()
}

/// What the session was shown of one path, line by line: `table[n - 1]` is line `n`, `Some(fp)` when a
/// tool displayed that line with exactly those bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Seen {
    /// Fingerprint of the whole file at the last whole-file fact, which `write`'s staleness check reads:
    /// an overwrite discards everything, so its question is about the whole file.
    pub digest: String,
    pub bytes: usize,
    table: Vec<Option<u64>>,
}

impl Seen {
    /// May an edit replace lines `first..=last`, given the file's current lines?
    pub fn check(&self, lines: &[&str], first: usize, last: usize) -> Cover {
        for n in first..=last {
            match (self.table.get(n - 1), lines.get(n - 1)) {
                (Some(Some(was)), Some(is)) if fp_line(is) == *was => continue,
                (Some(Some(_)), Some(_)) => return Cover::Stale,
                // No stored line, or a line the current file does not have:
                // either way the model was never shown what it is replacing.
                _ => return Cover::Never,
            }
        }
        Cover::Covered
    }

    /// Move the table across this session's own edit. Lines above the change
    /// keep their numbers, lines below shift by the change's delta, and the
    /// changed span itself is dropped: what is there now is new text.
    pub fn reseat(&mut self, before: &str, after: &str) {
        let span = crate::edit::changed_span(before, after);
        let after_lines = display_lines(after).len();
        let mut out = vec![None; after_lines];
        for old_line in 1..=span.last_before {
            if old_line < span.first {
                out[old_line - 1] = self.table.get(old_line - 1).copied().flatten();
            }
        }
        let delta = span.last_after as isize - span.last_before as isize;
        for old_line in (span.last_before + 1)..=span.before_lines {
            let at = (old_line as isize + delta - 1) as usize;
            if let Some(slot) = out.get_mut(at) {
                *slot = self.table.get(old_line - 1).copied().flatten();
            }
        }
        self.table = out;
    }

    pub fn mark(&mut self, lines: &[&str], first: usize, last: usize) {
        let total = lines.len();
        if first < 1 || last < first || first > total {
            return;
        }
        let last = last.min(total);
        if self.table.len() < total {
            self.table.resize(total, None);
        }
        for n in first..=last {
            self.table[n - 1] = Some(fp_line(lines[n - 1]));
        }
    }

    /// The shown runs, as a model reads them: "lines 1-50, 120-140".
    pub fn describe(&self) -> String {
        let mut runs: Vec<String> = Vec::new();
        let mut run: Option<(usize, usize)> = None;
        for (i, slot) in self.table.iter().enumerate() {
            match (run, slot.is_some()) {
                (None, true) => run = Some((i + 1, i + 1)),
                (Some((a, b)), true) => run = Some((a, b + 1)),
                (Some((a, b)), false) => {
                    runs.push(if a == b {
                        a.to_string()
                    } else {
                        format!("{a}-{b}")
                    });
                    run = None;
                }
                (None, false) => {}
            }
        }
        if let Some((a, b)) = run {
            runs.push(if a == b {
                a.to_string()
            } else {
                format!("{a}-{b}")
            });
        }
        if runs.is_empty() {
            "none of it".to_string()
        } else {
            format!("lines {}", runs.join(", "))
        }
    }
}

/// The answer to "may this edit replace these lines".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cover {
    Covered,
    /// No tool displayed those lines.
    Never,
    /// They were displayed, and the bytes there have moved since.
    Stale,
}

/// Per-session: the lines shown, per path. Interior mutability because [`crate::runtime::Tool::invoke`]
/// takes `&self`, since tools are values in a registry shared across calls.
#[derive(Debug, Default)]
pub struct FileLedger {
    seen: Mutex<BTreeMap<String, Seen>>,
}

impl FileLedger {
    pub fn new() -> Self {
        Self::default()
    }

    fn entry(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Seen>> {
        // A poisoned lock means a panic mid-mark; the ledger's facts are then
        // merely incomplete, which is the safe direction (less is shown).
        self.seen.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The whole file was shown (a `read` that displayed every line) or was
    /// authored by this session (`write`). Marks every line and records the
    /// whole-file digest.
    pub fn mark_whole(&self, path: &str, content: &[u8]) {
        let text = String::from_utf8_lossy(content);
        let lines: Vec<&str> = display_lines(&text);
        let mut g = self.entry();
        let s = g.entry(normalise(path)).or_default();
        s.digest = crate::spill::content_hash(content);
        s.bytes = content.len();
        s.table = Vec::new();
        s.mark(&lines, 1, lines.len());
    }

    /// A partial read: mark the lines it displayed, in full. A line `read`
    /// clipped is not marked, because the model saw a prefix of it. The whole-file
    /// digest is still recorded: the bytes were read, which is `write`'s fact.
    pub fn mark_read(&self, path: &str, content: &[u8], shown: &[usize]) {
        let text = String::from_utf8_lossy(content);
        let lines: Vec<&str> = display_lines(&text);
        let mut g = self.entry();
        let s = g.entry(normalise(path)).or_default();
        s.digest = crate::spill::content_hash(content);
        s.bytes = content.len();
        for &n in shown {
            s.mark(&lines, n, n);
        }
    }

    /// Search hits, with any context the same call displayed. No digest: a
    /// search says nothing about the file as a whole.
    pub fn mark_hits(&self, path: &str, lines: &[(usize, &str)]) {
        if lines.is_empty() {
            return;
        }
        let mut g = self.entry();
        let s = g.entry(normalise(path)).or_default();
        for &(n, text) in lines {
            let total = s.table.len().max(n);
            s.table.resize(total, None);
            if n >= 1 {
                s.table[n - 1] = Some(fp_line(text));
            }
        }
    }

    /// This session's own edit landed: reseat the table across the change and mark the region the result
    /// displayed. `mark_from`/`mark_to` are line numbers in `after`; `on_disk` is what was written.
    pub fn mark_edit(
        &self,
        path: &str,
        before: &str,
        after: &str,
        on_disk: &[u8],
        mark_from: usize,
        mark_to: usize,
    ) {
        let lines: Vec<&str> = display_lines(after);
        let mut g = self.entry();
        let s = g.entry(normalise(path)).or_default();
        s.digest = crate::spill::content_hash(on_disk);
        s.bytes = on_disk.len();
        s.reseat(before, after);
        s.mark(&lines, mark_from, mark_to);
    }

    /// May an edit replace lines `first..=last` of `path`? `lines` are the
    /// file's current lines, so a change since the showing is caught here.
    pub fn check(&self, path: &str, lines: &[&str], first: usize, last: usize) -> Cover {
        match self.entry().get(&normalise(path)) {
            Some(s) => s.check(lines, first, last),
            None => Cover::Never,
        }
    }

    /// The whole-file fact `write` gates on: `(digest, bytes)` at the last
    /// whole-file read or this session's own write. A path only ever touched by
    /// a search has no whole-file fact: its entry exists, its digest does not.
    pub fn whole_of(&self, path: &str) -> Option<(String, usize)> {
        let g = self.entry();
        g.get(&normalise(path))
            .filter(|s| !s.digest.is_empty())
            .map(|s| (s.digest.clone(), s.bytes))
    }

    /// A copy of one path's shown table, for a batch's steps: the batch checks
    /// each step against the text as the steps before it left it, and mutates
    /// nothing until the whole batch has written.
    pub fn checkout(&self, path: &str) -> Option<Seen> {
        self.entry().get(&normalise(path)).cloned()
    }

    /// What has been shown of `path`, for a refusal that says it. `total` is
    /// the file's current line count, so the answer never names lines the file
    /// no longer has.
    pub fn describe(&self, path: &str, total: usize) -> String {
        match self.entry().get(&normalise(path)) {
            Some(s) => {
                let mut t = s.clone();
                t.table.truncate(total);
                t.describe()
            }
            None => "none of it".to_string(),
        }
    }

    /// Paths this session has been shown, in a stable order.
    pub fn paths(&self) -> Vec<String> {
        match self.seen.lock() {
            Ok(g) => g.keys().cloned().collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// The key two spellings of one path share, because a ledger keyed on the model's spelling would let a
/// re-spelling walk past the guard. Lexical only: the backend's own `stat` resolves a symlink.
pub fn normalise(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_spellings_of_one_path_are_one_entry() {
        let l = FileLedger::new();
        l.mark_whole("./src//lib.rs", b"hello");
        assert_eq!(
            l.check("src/lib.rs", &["hello"], 1, 1),
            Cover::Covered,
            "a re-spelling of a shown path is the same file"
        );
        assert_eq!(l.paths(), vec!["src/lib.rs".to_string()]);
    }

    #[test]
    fn a_change_elsewhere_leaves_the_shown_lines_current() {
        let l = FileLedger::new();
        let before: Vec<String> = (1..=50)
            .map(|i| format!("line {i}"))
            .chain((51..=900).map(|i| format!("line {i}")))
            .collect();
        l.mark_whole("a.rs", before.join("\n").as_bytes());
        // A formatter rewrites line 900 only.
        let mut after = before.clone();
        after[899] = "line 900, reformatted".to_string();
        let lines: Vec<&str> = after.iter().map(|s| s.as_str()).collect();
        assert_eq!(l.check("a.rs", &lines, 12, 12), Cover::Covered);
        assert_eq!(l.check("a.rs", &lines, 900, 900), Cover::Stale);
    }

    #[test]
    fn a_line_never_shown_is_never_covered() {
        let l = FileLedger::new();
        let body: String = (1..=400).map(|i| format!("line {i}\n")).collect();
        l.mark_read("a.rs", body.as_bytes(), &(1..=50).collect::<Vec<_>>());
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(l.check("a.rs", &lines, 50, 50), Cover::Covered);
        assert_eq!(l.check("a.rs", &lines, 300, 300), Cover::Never);
    }

    #[test]
    fn the_sessions_own_edit_shifts_the_shown_lines_below_it() {
        let l = FileLedger::new();
        let before: String = (1..=40).map(|i| format!("line {i}\n")).collect();
        l.mark_read("a.rs", before.as_bytes(), &(1..=40).collect::<Vec<_>>());
        // Five lines inserted at line 10; the changed region is shown.
        let mut after: Vec<String> = (1..=9).map(|i| format!("line {i}")).collect();
        after.extend((0..5).map(|i| format!("new {i}")));
        after.extend((10..=40).map(|i| format!("line {i}")));
        let after_text = after.join("\n");
        l.mark_edit("a.rs", &before, &after_text, after_text.as_bytes(), 7, 17);
        let lines: Vec<&str> = after_text.lines().collect();
        // What was line 30 is now line 35, and it is still shown.
        assert_eq!(l.check("a.rs", &lines, 35, 35), Cover::Covered);
        // The inserted lines are inside the marked region.
        assert_eq!(l.check("a.rs", &lines, 10, 14), Cover::Covered);
    }

    #[test]
    fn a_search_hit_marks_its_line_without_a_whole_file_fact() {
        let l = FileLedger::new();
        l.mark_hits("a.rs", &[(30, "the hit line")]);
        let mut lines: Vec<&str> = vec!["filler"; 30];
        lines[29] = "the hit line";
        assert_eq!(l.check("a.rs", &lines, 30, 30), Cover::Covered);
        assert_eq!(l.check("a.rs", &lines, 2, 2), Cover::Never);
        assert!(
            l.whole_of("a.rs").is_none(),
            "a search is not a whole-file read"
        );
    }

    #[test]
    fn describe_names_the_runs() {
        let mut s = Seen::default();
        s.mark(&["a", "b", "c", "d", "e"], 1, 2);
        s.mark(&["a", "b", "c", "d", "e"], 5, 5);
        assert_eq!(s.describe(), "lines 1-2, 5");
    }
}
