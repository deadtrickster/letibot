//! The lines this session was shown of each file, and what it may therefore change.
//! An `edit` may only replace lines a tool displayed, never their neighbours. One fingerprint per
//! shown line, not an mtime (a `touch` moves it) and not a whole-file digest.

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
    /// Lines shown only in part (a clipped prefix, a `column_offset` window): the line's
    /// fingerprint and the byte ranges of it displayed, sorted and merged.
    parts: BTreeMap<usize, (u64, Vec<(usize, usize)>)>,
}

/// Where a match of `len` bytes at byte `at` of `text` lies: its first line and the byte column
/// it starts at there, its last line and the byte column it ends at (exclusive) there.
pub fn hit_span(text: &str, at: usize, len: usize) -> (usize, usize, usize, usize) {
    // Both ends are floored to a char boundary once, here: every slice below is taken from them,
    // and an offset a caller computed against another string can land inside a character.
    let at = crate::edit::floor_boundary(text, at);
    let start_of = |b: usize| text[..b].rfind('\n').map_or(0, |i| i + 1);
    let first = text[..at].matches('\n').count() + 1;
    let first_col = at - start_of(at);
    let end = crate::edit::floor_boundary(text, (at + len).min(text.len()));
    // The start of the match's last char: one byte back can land inside a multibyte char.
    let last_byte = crate::edit::floor_boundary(text, end.saturating_sub(1)).max(at);
    let last = text[..last_byte].matches('\n').count() + 1;
    let last_start = start_of(last_byte);
    let line_len = text[last_start..]
        .find('\n')
        .unwrap_or(text.len() - last_start);
    (first, first_col, last, (end - last_start).min(line_len))
}

impl Seen {
    /// May an edit replace lines `first..=last`, given the file's current lines?
    pub fn check(&self, lines: &[&str], first: usize, last: usize) -> Cover {
        let end = lines.get(last.wrapping_sub(1)).map_or(0, |l| l.len());
        self.check_span(lines, first, 0, last, end)
    }

    /// May an edit replace the bytes from column `first_col` of line `first` to column `last_end`
    /// of line `last`? A line shown in part covers a change that lies inside a shown range.
    pub fn check_span(
        &self,
        lines: &[&str],
        first: usize,
        first_col: usize,
        last: usize,
        last_end: usize,
    ) -> Cover {
        for n in first..=last {
            let Some(is) = lines.get(n.wrapping_sub(1)) else {
                return Cover::Never;
            };
            match self.table.get(n - 1) {
                Some(Some(was)) if fp_line(is) == *was => continue,
                Some(Some(_)) => return Cover::Stale,
                _ => {}
            }
            let Some((was, ranges)) = self.parts.get(&n) else {
                // Never stored: the model was never shown what it is replacing.
                return Cover::Never;
            };
            if fp_line(is) != *was {
                return Cover::Stale;
            }
            let a = if n == first { first_col } else { 0 };
            let b = if n == last { last_end } else { is.len() };
            if !ranges.iter().any(|&(x, y)| x <= a && b <= y) {
                return Cover::Never;
            }
        }
        Cover::Covered
    }

    /// Bytes `from..to` of line `n` were displayed. A line shown whole by now is moved to the table.
    pub fn mark_part(&mut self, lines: &[&str], n: usize, from: usize, to: usize) {
        let Some(line) = lines.get(n.wrapping_sub(1)) else {
            return;
        };
        let fp = fp_line(line);
        if matches!(self.table.get(n - 1), Some(Some(was)) if *was == fp) {
            return;
        }
        let slot = self.parts.entry(n).or_insert((fp, Vec::new()));
        if slot.0 != fp {
            *slot = (fp, Vec::new());
        }
        slot.1.push((
            crate::edit::floor_boundary(line, from),
            crate::edit::floor_boundary(line, to),
        ));
        slot.1.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (x, y) in slot.1.drain(..) {
            match merged.last_mut() {
                Some(last) if x <= last.1 => last.1 = last.1.max(y),
                _ => merged.push((x, y)),
            }
        }
        if merged
            .first()
            .is_some_and(|&(x, y)| x == 0 && y >= line.len())
        {
            self.parts.remove(&n);
            self.mark(lines, n, n);
        } else {
            slot.1 = merged;
        }
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
        self.parts = std::mem::take(&mut self.parts)
            .into_iter()
            .filter_map(|(n, p)| {
                if n < span.first {
                    Some((n, p))
                } else if n > span.last_before {
                    Some(((n as isize + delta) as usize, p))
                } else {
                    None
                }
            })
            .filter(|(n, _)| *n >= 1 && *n <= after_lines)
            .collect();
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
            self.parts.remove(&n);
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
        let parts: Vec<String> = self.parts.keys().map(|n| n.to_string()).collect();
        match (runs.is_empty(), parts.is_empty()) {
            (true, true) => "none of it".to_string(),
            (false, true) => format!("lines {}", runs.join(", ")),
            (true, false) => format!("part of line(s) {}", parts.join(", ")),
            (false, false) => format!(
                "lines {} and part of line(s) {}",
                runs.join(", "),
                parts.join(", ")
            ),
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
        s.parts.clear();
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

    /// Part of line `n` was displayed: bytes `from..to` of it. The digest is recorded as in
    /// [`FileLedger::mark_read`], since the bytes were read.
    pub fn mark_part(&self, path: &str, content: &[u8], n: usize, from: usize, to: usize) {
        let text = String::from_utf8_lossy(content);
        let lines: Vec<&str> = display_lines(&text);
        let mut g = self.entry();
        let s = g.entry(normalise(path)).or_default();
        s.digest = crate::spill::content_hash(content);
        s.bytes = content.len();
        s.mark_part(&lines, n, from, to);
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
                s.parts.remove(&n);
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

    /// [`Seen::check_span`] for `path`.
    pub fn check_span(
        &self,
        path: &str,
        lines: &[&str],
        (first, first_col, last, last_end): (usize, usize, usize, usize),
    ) -> Cover {
        match self.entry().get(&normalise(path)) {
            Some(s) => s.check_span(lines, first, first_col, last, last_end),
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
                t.parts.retain(|n, _| *n <= total);
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
    fn a_part_shown_covers_a_change_inside_it_and_moves_with_an_edit_above() {
        let mut s = Seen::default();
        let before = "head\n0123456789abcdef\n";
        let lines: Vec<&str> = before.lines().collect();
        s.mark_part(&lines, 2, 4, 10);
        assert_eq!(s.check_span(&lines, 2, 5, 2, 9), Cover::Covered);
        assert_eq!(s.check_span(&lines, 2, 3, 2, 9), Cover::Never);
        assert_eq!(s.check(&lines, 2, 2), Cover::Never);
        assert_eq!(hit_span(before, 9, 4), (2, 4, 2, 8));
        assert_eq!(hit_span(before, 0, 6), (1, 0, 2, 1));
        let cyr = "ab\nпривет\n";
        assert_eq!(hit_span(cyr, 3, "привет".len()), (2, 0, 2, 12));
        // An offset that is not a char boundary is floored rather than sliced: `привет` starts
        // at 0, so a start of 1 is the same start as 0.
        assert_eq!(hit_span("привет", 1, 2), (1, 0, 1, 2));
        assert_eq!(hit_span("👍🏽x", 1, 1), (1, 0, 1, 0));

        // A line added above moves the part down with its line.
        let after = "new\nhead\n0123456789abcdef\n";
        s.reseat(before, after);
        let lines: Vec<&str> = after.lines().collect();
        assert_eq!(s.check_span(&lines, 3, 5, 3, 9), Cover::Covered);

        // The rest shown too: the line is whole now.
        s.mark_part(&lines, 3, 0, 4);
        s.mark_part(&lines, 3, 10, 16);
        assert_eq!(s.check(&lines, 3, 3), Cover::Covered);
        assert!(s.parts.is_empty());
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
