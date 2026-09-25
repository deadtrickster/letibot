//! Finding the text an edit names, and saying what is actually there when it is
//! not found.
//!
//! This module is clause 1 for the write tools, and it is where the design
//! decision that matters was made:
//!
//! > **A relaxed match is a diagnosis, never an application.**
//!
//! # Why, given that both prior harnesses do the opposite
//!
//! opencode's shipping `edit` runs a nine-rung ladder — `SimpleReplacer`,
//! `LineTrimmedReplacer`, `BlockAnchorReplacer`, `WhitespaceNormalizedReplacer`,
//! `IndentationFlexibleReplacer`, `EscapeNormalizedReplacer`,
//! `TrimmedBoundaryReplacer`, `ContextAwareReplacer`, `MultiOccurrenceReplacer` —
//! and **writes the relaxed match to disk**, with a Levenshtein similarity floor
//! of 0.65 deciding whether two blocks are "the same". Two facts about that
//! design are worth more than the design itself:
//!
//! 1. it needed a guard on top of itself. `isDisproportionateMatch` refuses when
//!    the matched span is `>= max(oldLines+3, oldLines*2)` lines, with the
//!    message *"Refusing replacement because the matched span is much larger than
//!    oldString."* A ladder that has to be told when it has gone too far has an
//!    upper bound nobody can state;
//! 2. their own rewrite dropped it. `packages/core/src/tool/edit.ts:84` defers
//!    the whole thing: *"Port V1 fuzzy correction strategies only after
//!    exact-edit behavior is established: line-trimmed matching, block-anchor
//!    fallback, indentation correction, and similarity-threshold review."*
//!
//! grok-build never had it: `search_replace` is `match_indices` on the exact
//! bytes, and its one relaxation (Unicode confusables) is **off by default** and
//! round-trip validated before it is allowed to apply.
//!
//! The cost of relaxing-and-applying is paid in a place nobody looks: a
//! whitespace-normalised match applied to Python, YAML, a Makefile or a Go
//! raw-string literal writes bytes the model did not ask for, and it writes them
//! into the file the operator asked to be careful with. The cost of
//! relaxing-and-reporting is **one extra call** — and only when the report is not
//! good enough to fix the call, which is what this module spends its effort on:
//! a miss comes back with the exact bytes that are actually there, so the retry
//! is a copy rather than a guess.
//!
//! # The one relaxation that is applied
//!
//! Line endings. Both prior harnesses do this and both are right:
//! [`FileText`] holds the file's dominant ending, matching happens on an
//! LF-normalised copy, and the file's own ending is restored on write. A file's
//! line endings are a property of the file, not of what the model meant, and
//! there is no edit anybody wants that turns on which of them the model typed.
//!
//! The one place this differs from both: a file with **mixed** endings is left
//! mixed outside the edited span. grok-build's
//! `new_text.replace("\r\n","\n").replace('\n',"\r\n")` rewrites every line in
//! the file when one CRLF is present, which turns a one-line edit into a
//! whole-file diff.

/// A file's text, with its line endings remembered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileText {
    /// The content with every `\r\n` turned into `\n`. All matching happens here.
    pub lf: String,
    /// Whether the file used `\r\n`, and should get it back.
    pub crlf: bool,
    /// True when the file had both kinds. Restoring endings then cannot be done
    /// without rewriting lines nobody touched, so it is not attempted and the tool
    /// says so.
    pub mixed: bool,
    /// The file was not valid UTF-8 and had to be read lossily. A write would
    /// replace the undecodable bytes with U+FFFD permanently, so a write refuses.
    pub lossy: bool,
}

impl FileText {
    pub fn of(bytes: &[u8]) -> Self {
        let (text, lossy) = crate::builtins::text_of(bytes);
        let crlf_count = text.matches("\r\n").count();
        let lf_count = text.matches('\n').count();
        FileText {
            lf: text.replace("\r\n", "\n"),
            crlf: crlf_count > 0,
            mixed: crlf_count > 0 && crlf_count != lf_count,
            lossy,
        }
    }

    /// The bytes to write back: LF text with the file's own ending restored.
    pub fn restore(&self, lf: &str) -> String {
        if self.crlf && !self.mixed {
            lf.replace('\n', "\r\n")
        } else {
            lf.to_string()
        }
    }

    pub fn line_of(&self, byte: usize) -> usize {
        self.lf[..byte.min(self.lf.len())].matches('\n').count() + 1
    }
}

/// Every exact occurrence of `needle`, as byte offsets into `hay`.
///
/// Non-overlapping and left to right, which is what a replacement needs: two
/// overlapping occurrences of the same string cannot both be replaced, and
/// counting them as two would make the ambiguity report a lie.
pub fn occurrences(hay: &str, needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = hay[from..].find(needle) {
        let at = from + rel;
        out.push(at);
        from = at + needle.len();
    }
    out
}

/// What a relaxation relaxed. The name the model is told, so that a retry can be
/// aimed rather than guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relax {
    /// Trailing whitespace on one or more lines differs.
    TrailingWhitespace,
    /// Every line differs by the same leading indent.
    Indentation,
    /// Whitespace *within* lines differs — a run of spaces against a tab, or two
    /// spaces against one.
    InnerWhitespace,
    /// Only the case differs.
    Case,
    /// The first and last lines were found framing a region, and the middle
    /// differs. The weakest probe, and the one whose report is worth the most:
    /// the region between two anchors is almost always the thing the model meant.
    Anchors,
}

impl Relax {
    /// The short name, for the `reason` line and for a test to assert on.
    pub fn as_str(&self) -> &'static str {
        match self {
            Relax::TrailingWhitespace => "trailing whitespace",
            Relax::Indentation => "indentation",
            Relax::InnerWhitespace => "whitespace inside the lines",
            Relax::Case => "letter case",
            Relax::Anchors => "only the first and last lines",
        }
    }

    /// The clause that finishes *"N place(s) match under a relaxed comparison —
    /// …"*. Written to be readable in that sentence rather than to be a label,
    /// because the sentence is what the model reads.
    pub fn headline(&self) -> &'static str {
        match self {
            Relax::TrailingWhitespace => "identical except for trailing whitespace",
            Relax::Indentation => "identical except for how far the block is indented",
            Relax::InnerWhitespace => {
                "identical except for whitespace inside the lines — a tab where you have \
                 spaces, or a different number of them"
            }
            Relax::Case => "identical except for letter case",
            Relax::Anchors => {
                "only the first and the last line match, and what is between them is not \
                 what you expected"
            }
        }
    }

    /// The clause that finishes the `reason` line: *"…; at N place(s), …"*.
    pub fn differs(&self) -> &'static str {
        match self {
            Relax::TrailingWhitespace => "the trailing whitespace differs",
            Relax::Indentation => "the indentation differs",
            Relax::InnerWhitespace => "the whitespace inside the lines differs",
            Relax::Case => "the letter case differs",
            Relax::Anchors => "only the first and last lines matched",
        }
    }

    /// What to do about it. Written as an instruction rather than a diagnosis,
    /// because the model's next act is a retry and a diagnosis alone leaves the
    /// retry to be invented.
    pub fn advice(&self) -> &'static str {
        match self {
            Relax::TrailingWhitespace => {
                "copy the text below, trailing spaces and all, rather than retyping it"
            }
            Relax::Indentation => "copy the text below with its leading whitespace",
            Relax::InnerWhitespace => {
                "a run of spaces and a tab are different bytes, so copy the text below"
            }
            Relax::Case => "copy the text below",
            Relax::Anchors => "read what is between them before editing it",
        }
    }
}

/// A region of the file a relaxation matched, in bytes and in lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub relax: Relax,
    /// Byte range in [`FileText::lf`].
    pub start: usize,
    pub end: usize,
    pub first_line: usize,
    pub last_line: usize,
}

impl Candidate {
    /// The exact bytes the model should have sent.
    pub fn text<'a>(&self, file: &'a FileText) -> &'a str {
        &file.lf[self.start..self.end]
    }
}

/// The relaxation ladder, run **for diagnosis**.
///
/// Ordered strongest first, and it stops at the first rung that finds anything:
/// a report naming five ways the string nearly matched is a report nobody acts
/// on. The rungs are the same relaxations opencode's replacers apply; the
/// difference is entirely in what is done with the answer.
pub fn probe(file: &FileText, needle: &str) -> Vec<Candidate> {
    let hay = &file.lf;
    let want: Vec<&str> = needle.split('\n').collect();

    for (relax, key) in [
        (
            Relax::TrailingWhitespace,
            (|s: &str| s.trim_end().to_string()) as fn(&str) -> String,
        ),
        (Relax::Indentation, |s: &str| s.trim_start().to_string()),
        (Relax::InnerWhitespace, |s: &str| {
            s.split_whitespace().collect::<Vec<_>>().join(" ")
        }),
        (Relax::Case, |s: &str| s.to_lowercase()),
    ] {
        let found = windows_matching(file, &want, &key);
        if !found.is_empty() {
            // The indentation rung must not report a hit that the trailing-space
            // rung would have: `trim_start` also equalises a line that is only
            // trailing whitespace. Ordering handles that, since the stronger rung
            // runs first.
            return found
                .into_iter()
                .map(|(start, end, fl, ll)| Candidate {
                    relax,
                    start,
                    end,
                    first_line: fl,
                    last_line: ll,
                })
                .collect();
        }
    }

    // The anchor rung, last and separate: it does not compare the middle at all,
    // so it can find a region when every line-wise rung has failed. Needs at
    // least two lines to be an anchor rather than a search.
    if want.len() >= 2 {
        let first = want[0].trim();
        let last = want[want.len() - 1].trim();
        if !first.is_empty() && !last.is_empty() {
            let lines: Vec<&str> = hay.split('\n').collect();
            let offsets = line_starts(hay);
            let mut out = Vec::new();
            let mut anchors: Vec<usize> = Vec::new();
            for (i, l) in lines.iter().enumerate() {
                if l.trim() == first {
                    anchors.push(i);
                }
            }
            for s in anchors {
                for (j, l) in lines.iter().enumerate().skip(s + 1) {
                    if l.trim() == last {
                        let (start, end) = span_of_lines(hay, &offsets, s, j);
                        out.push(Candidate {
                            relax: Relax::Anchors,
                            start,
                            end,
                            first_line: s + 1,
                            last_line: j + 1,
                        });
                        break;
                    }
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
    }
    Vec::new()
}

/// Line windows of the file whose key-mapped lines equal the key-mapped needle.
fn windows_matching(
    file: &FileText,
    want: &[&str],
    key: &dyn Fn(&str) -> String,
) -> Vec<(usize, usize, usize, usize)> {
    let lines: Vec<&str> = file.lf.split('\n').collect();
    if want.len() > lines.len() {
        return Vec::new();
    }
    let wanted: Vec<String> = want.iter().map(|l| key(l)).collect();
    // A needle that maps to nothing at all (all whitespace) would match every
    // blank run in the file. That is not a diagnosis, it is noise.
    if wanted.iter().all(|w| w.is_empty()) {
        return Vec::new();
    }
    let mapped: Vec<String> = lines.iter().map(|l| key(l)).collect();
    let offsets = line_starts(&file.lf);
    let mut out = Vec::new();
    for i in 0..=(lines.len() - want.len()) {
        if mapped[i..i + want.len()] == wanted[..] {
            let j = i + want.len() - 1;
            let (start, end) = span_of_lines(&file.lf, &offsets, i, j);
            out.push((start, end, i + 1, j + 1));
        }
        // Bounded: a file where every line is `}` should not produce a thousand
        // candidates, and after a handful the report says "and N more" anyway.
        if out.len() >= 32 {
            break;
        }
    }
    out
}

/// The byte offset each line starts at. `line_starts(s).len()` is exactly
/// `s.split('\n').count()`, so a line index from one is valid in the other.
pub fn line_starts(hay: &str) -> Vec<usize> {
    let mut v = vec![0usize];
    for (i, b) in hay.bytes().enumerate() {
        if b == b'\n' {
            v.push(i + 1);
        }
    }
    v
}

/// Byte span of lines `[a, b]`, zero-based and inclusive, excluding the newline
/// that ends line `b`.
fn span_of_lines(hay: &str, starts: &[usize], a: usize, b: usize) -> (usize, usize) {
    let start = *starts.get(a).unwrap_or(&hay.len());
    let end = match starts.get(b + 1) {
        // `- 1` drops the `\n` that ended line `b`, so the span is the text of the
        // lines and not the text plus a separator.
        Some(&s) => s.saturating_sub(1),
        None => hay.len(),
    };
    (start, end.max(start))
}

// ---------------------------------------------------------------------------
// The hand-off to the head.
// ---------------------------------------------------------------------------

/// **Both sides of one file change**, which is what a head needs and what nothing
/// in this harness carried before.
///
/// `crates/ui`'s `diff` — Myers with a `max_d` cap plus word-level intra-line
/// highlight — was built, tested and unwired *"because nothing carries both sides
/// of an edit"*. This is the thing that carries them.
///
/// # What the head must do
///
/// ```text
/// let before: Vec<&str> = edit.before.lines().collect();
/// let after:  Vec<&str> = edit.after.lines().collect();
/// let rows = letibot_ui::diff::render(&before, &after, &DiffConfig { .. });
/// ```
///
/// That is the whole contract. Three notes on it:
///
/// - `before` and `after` are **LF-normalised** and carry no `\r`, so the diff
///   never shows a phantom change on a CRLF file. [`FileText::restore`] put the
///   real endings back on the bytes that reached the disk;
/// - a created file has `before == ""` and `created == true`. `"".lines()` is
///   empty, which is what `crates/ui`'s empty-to-empty case documents as the
///   right input — *"without context it is an empty file write and must produce
///   no hunk lines at all"*;
/// - this is on [`crate::result::ToolResult`] and **not** in a [`crate::events::ToolEvent`],
///   deliberately. The events module's rule is that *"no event carries the
///   payload"*, because an event fans out to every attached head; a whole file on
///   both sides is exactly the payload that rule is about. A head reads this off
///   the result the runtime returns, in process. Sending it over the socket is
///   `letibot-sessionlog`'s decision to make, and [`FileEdit::before_digest`] /
///   [`FileEdit::after_digest`] are there so it can be sent by reference if that
///   is the answer.
///
/// # Why the model does not get a rendered diff
///
/// The model asked for the change; re-rendering it back is telling it what it
/// just said, in the most expensive form available — permanent context, every
/// turn, for the rest of the session. What it does not know is *where the change
/// landed and what it displaced*, and that is what the tool's payload says
/// instead. §9's whole argument is about what is worth keeping in the prompt, and
/// a diff of an edit the model authored is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEdit {
    /// Relative to the session root, as the head should label it.
    pub path: String,
    /// The file before, LF-normalised. `""` for a created file.
    pub before: String,
    /// The file after, LF-normalised.
    pub after: String,
    pub created: bool,
    pub before_digest: String,
    pub after_digest: String,
    /// How many places changed. `1` for an `edit`; `n` for `replace_all`;
    /// whatever [`changed_span`] found for a whole-file `write`.
    pub replacements: usize,
    /// The 1-based line span that differs, from the outside in.
    pub changed: ChangedSpan,
}

/// The region of a file that differs, found by trimming the common prefix and
/// suffix — **not** by an edit script.
///
/// This is an exact fact and it needs no differ: the lines before `first` are
/// identical in both, and so are the lines after `last_before` / `last_after`.
/// It is deliberately weaker than a minimal diff, and that is the point — this
/// crate does not own a differ, `crates/ui` does, and computing a second, worse
/// one here to put a number in a sentence is how two answers to one question
/// start disagreeing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangedSpan {
    /// 1-based first differing line. Equal to `before_lines + 1` when the change
    /// is a pure append.
    pub first: usize,
    /// 1-based last differing line in the old text; `first - 1` for a pure
    /// insertion.
    pub last_before: usize,
    /// 1-based last differing line in the new text; `first - 1` for a pure
    /// deletion.
    pub last_after: usize,
    pub before_lines: usize,
    pub after_lines: usize,
}

impl ChangedSpan {
    pub fn describe(&self) -> String {
        let removed = (self.last_before + 1).saturating_sub(self.first);
        let added = (self.last_after + 1).saturating_sub(self.first);
        format!(
            "lines {}–{} differ ({} line(s) replaced by {}); the file went from {} to {} lines",
            self.first,
            self.last_before.max(self.last_after),
            removed,
            added,
            self.before_lines,
            self.after_lines
        )
    }
}

/// The changed span of two texts, from the outside in.
///
/// Counted in `str::lines()` lines, deliberately: that is what `read` numbers a
/// file by, and a `write` that reported eleven lines for the file `read` had just
/// called ten would teach the model that one of the two is lying.
pub fn changed_span(before: &str, after: &str) -> ChangedSpan {
    let b: Vec<&str> = before.lines().collect();
    let a: Vec<&str> = after.lines().collect();
    let mut head = 0usize;
    while head < b.len() && head < a.len() && b[head] == a[head] {
        head += 1;
    }
    let mut tail = 0usize;
    while tail < b.len() - head
        && tail < a.len() - head
        && b[b.len() - 1 - tail] == a[a.len() - 1 - tail]
    {
        tail += 1;
    }
    ChangedSpan {
        first: head + 1,
        last_before: b.len() - tail,
        last_after: a.len() - tail,
        before_lines: b.len(),
        after_lines: a.len(),
    }
}

/// A file's lines as they are numbered everywhere the model sees them.
///
/// One function, so `read`, `edit` and `write` cannot drift apart on whether a
/// trailing newline makes a last, empty line.
pub fn display_lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

/// The before/after of a file-editing call, bounded to what differs, as it
/// rides the tool event to a head.
///
/// One definition, in `letibot-transcript` — the one crate this crate and the
/// log can both see — re-exported here under the name the runtime has always
/// used. It used to be defined in this file and lifted field by field into the
/// log's own shape; two copies of a nine-field struct with a lift between them
/// is how copies drift, and the transcript row now carries the excerpt too, so
/// a third copy was about to exist.
pub use letibot_transcript::ToolEditExcerpt;

impl FileEdit {
    /// The bounded before/after a head draws a two-panel diff from: the
    /// changed span plus `context` lines either side, capped at `cap`
    /// lines per side.
    ///
    /// The cap is not stinginess for its own sake — the event fans out to
    /// every attached head, and a whole-file `write` of a large file would
    /// otherwise put tens of thousands of lines on the wire for a display
    /// that shows sixty rows. [`ChangedSpan`] is an exact fact, so the
    /// excerpt contains the whole change unless the cap cut it, and
    /// `truncated` says when it did.
    pub fn excerpt(&self, context: usize, cap: usize) -> ToolEditExcerpt {
        let before: Vec<&str> = self.before.lines().collect();
        let after: Vec<&str> = self.after.lines().collect();
        // The span is 1-based inclusive; widen by `context` and clamp to the
        // file, then work 0-based. A pure insertion has `last_before ==
        // first - 1`, so the old range comes out empty by the same
        // arithmetic, and a created file has no old lines at all.
        let b_lo = self.changed.first.saturating_sub(context + 1);
        let a_lo = b_lo;
        let b_hi = (self.changed.last_before + context).min(before.len());
        let a_hi = (self.changed.last_after + context).min(after.len());
        let take_before = b_hi.saturating_sub(b_lo);
        let take_after = a_hi.saturating_sub(a_lo);
        let truncated = take_before > cap || take_after > cap;
        let (n_before, n_after) = (take_before.min(cap), take_after.min(cap));
        ToolEditExcerpt {
            path: self.path.clone(),
            created: self.created,
            before_start: b_lo + 1,
            after_start: a_lo + 1,
            before_lines: before.len(),
            after_lines: after.len(),
            truncated,
            before: before[b_lo..b_lo + n_before].join("\n"),
            after: after[a_lo..a_lo + n_after].join("\n"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_excerpt_bounds_the_change_and_numbers_the_gutters() {
        let before: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        let after = before.replacen("line 25", "line 25 changed", 1);
        let fe = FileEdit {
            path: "f".into(),
            before: before.clone(),
            after: after.clone(),
            created: false,
            before_digest: String::new(),
            after_digest: String::new(),
            replacements: 1,
            changed: changed_span(&before, &after),
        };
        let ex = fe.excerpt(3, 400);
        assert_eq!(ex.before_start, 22, "25 minus three lines of context");
        assert_eq!(ex.after_start, 22);
        assert_eq!(ex.before.lines().count(), 7, "22..=28");
        assert_eq!(ex.before.lines().next().unwrap(), "line 22");
        assert_eq!(ex.after.lines().nth(3), Some("line 25 changed"));
        assert_eq!(ex.before_lines, 50, "whole-file counts ride along");
        assert!(!ex.truncated);
    }

    #[test]
    fn the_excerpt_cap_cuts_and_says_so() {
        let before: String = (1..=900).map(|i| format!("{i}\n")).collect();
        let after: String = (1..=900).map(|i| format!("{i}x\n")).collect();
        let fe = FileEdit {
            path: "f".into(),
            before,
            after,
            created: false,
            before_digest: String::new(),
            after_digest: String::new(),
            replacements: 900,
            changed: ChangedSpan {
                first: 1,
                last_before: 900,
                last_after: 900,
                before_lines: 900,
                after_lines: 900,
            },
        };
        let ex = fe.excerpt(3, 100);
        assert!(ex.truncated);
        assert_eq!(ex.before.lines().count(), 100);
        assert_eq!(ex.before_start, 1);
    }

    #[test]
    fn a_created_file_has_an_empty_before_and_a_numbered_after() {
        let fe = FileEdit {
            path: "new.rs".into(),
            before: String::new(),
            after: "x\ny\n".into(),
            created: true,
            before_digest: String::new(),
            after_digest: String::new(),
            replacements: 1,
            changed: changed_span("", "x\ny\n"),
        };
        let ex = fe.excerpt(3, 400);
        assert!(ex.created && ex.before.is_empty());
        assert_eq!(ex.after, "x\ny", "LF-joined, no trailing newline");
        assert_eq!(ex.after_start, 1);
        assert!(!ex.truncated);
    }

    #[test]
    fn occurrences_do_not_overlap_because_a_replacement_cannot() {
        assert_eq!(occurrences("aaaa", "aa"), vec![0, 2]);
        assert_eq!(occurrences("abc", ""), Vec::<usize>::new());
    }

    #[test]
    fn a_crlf_file_matches_on_lf_and_gets_its_endings_back() {
        let f = FileText::of(b"one\r\ntwo\r\n");
        assert_eq!(f.lf, "one\ntwo\n");
        assert!(f.crlf && !f.mixed);
        assert_eq!(f.restore("one\nTWO\n"), "one\r\nTWO\r\n");
    }

    #[test]
    fn a_mixed_ending_file_is_left_mixed_rather_than_rewritten_whole() {
        let f = FileText::of(b"one\r\ntwo\nthree\r\n");
        assert!(f.mixed);
        assert_eq!(f.restore("x\ny\n"), "x\ny\n");
    }

    #[test]
    fn indentation_is_diagnosed_and_the_exact_text_comes_back() {
        let f = FileText::of(b"fn a() {\n        let x = 1;\n}\n");
        let c = probe(&f, "    let x = 1;");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].relax, Relax::Indentation);
        assert_eq!(c[0].text(&f), "        let x = 1;");
        assert_eq!(c[0].first_line, 2);
    }

    #[test]
    fn trailing_whitespace_is_diagnosed_before_indentation() {
        let f = FileText::of(b"a\nlet x = 1;   \nb\n");
        let c = probe(&f, "let x = 1;");
        assert_eq!(c[0].relax, Relax::TrailingWhitespace);
        assert_eq!(c[0].text(&f), "let x = 1;   ");
    }

    #[test]
    fn a_tab_against_spaces_is_diagnosed_as_inner_whitespace() {
        let f = FileText::of("if a\tthen b\n".as_bytes());
        let c = probe(&f, "if a then b");
        assert_eq!(c[0].relax, Relax::InnerWhitespace);
    }

    #[test]
    fn anchors_find_the_region_when_the_middle_is_wrong() {
        let f = FileText::of(b"fn a() {\n    let x = 2;\n}\n");
        let c = probe(&f, "fn a() {\n    let x = 1;\n}");
        assert_eq!(c[0].relax, Relax::Anchors);
        assert_eq!(c[0].text(&f), "fn a() {\n    let x = 2;\n}");
    }

    #[test]
    fn an_all_whitespace_needle_diagnoses_nothing_rather_than_everything() {
        let f = FileText::of(b"a\n\n\nb\n\n\nc\n");
        assert!(probe(&f, "  \n  ").is_empty());
    }

    #[test]
    fn the_changed_span_is_the_region_and_not_an_edit_script() {
        let s = changed_span("a\nb\nc\nd\n", "a\nB\nc\nd\n");
        assert_eq!(s.first, 2);
        assert_eq!(s.last_before, 2);
        assert_eq!(s.last_after, 2);

        // A pure append: nothing in the old text differs.
        let s = changed_span("a\n", "a\nb\n");
        assert_eq!(s.first, 2);
        assert_eq!(s.last_before, 1, "nothing old differs");
        assert_eq!(s.last_after, 2);

        // Created from nothing, and counted the way `read` counts.
        let s = changed_span("", "x\ny\n");
        assert_eq!(s.before_lines, 0);
        assert_eq!(s.after_lines, 2, "a trailing newline is not a third line");
    }
}
