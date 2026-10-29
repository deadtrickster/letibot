//! **What a fence that cites code looks like** — the one rule both halves of a citation read.
//!
//! A model is told: *"an empty code fence labelled with the path and the line range is filled
//! with those lines, numbered as the file numbers them"*. Two spellings of that fence are in
//! use, and both are citations — the empty block:
//!
//! ````text
//! ```flow/connectors/postgres/pgdump_schema.go:242-248
//! ```
//! ````
//!
//! and the one-liner, its opening run and its closing run on one line:
//!
//! ````text
//! ```flow/connectors/postgres/pgdump_schema.go:242-248```
//! ````
//!
//! **The daemon reads the lines and the head fills the fence, and neither can see the other** —
//! this module is read by `harnessd::citations` and by `tui::ui::transcript::cite`.
//! So the two spellings, the label, and where a block ends live here, once, rather than in two
//! scanners that drift apart. The drift was measured: a one-liner used to be read as an *opening*
//! fence whose label was the rest of the line with the closing run glued onto it, so a citation
//! written that way was never recognised by the daemon and arrived on the operator's screen as
//! an empty box with a path in it.

/// The runs that fence a block. A line opens a fence when, trimmed, it starts with one.
const RUNS: [&str; 2] = ["```", "~~~"];

/// Whether `line` (already trimmed) begins a fence, whatever run it uses.
fn begins(line: &str) -> bool {
    RUNS.iter().any(|run| line.starts_with(run))
}

/// **One fence met in a reply**: what opened it, its label, whether it was closed with nothing
/// inside, and where the block ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fence<'a> {
    /// The run that opened it — and the run that closes it.
    pub run: &'a str,
    /// The text between the runs, trimmed: the label. Empty for a bare fence.
    pub label: &'a str,
    /// Nothing but blank lines stood between the opening run and the closing one.
    pub empty: bool,
    /// The closing run was found. On the same line, for a one-liner.
    pub closed: bool,
    /// The block's last line: the closer's, the one-liner's own line, or — for a fence that was
    /// never closed — the last line before the next fence began.
    pub end: usize,
}

impl Fence<'_> {
    /// **A citation's shape**: closed, with nothing inside it. Whether the label names a range
    /// is [`range`]'s question, and whether the daemon sent lines for it is neither's.
    pub fn cites(&self) -> bool {
        self.closed && self.empty
    }
}

/// **The fence that opens at `lines[i]`, if one does.**
///
/// A line opens a fence when, trimmed, it starts with a run. It is a **one-liner** when that
/// same run closes on the line it opened, with something between them — the label is what is
/// between them, and the block is that one line. Otherwise the block runs to the next line that
/// is exactly the run.
///
/// **A fence that is never closed stops at the next line that begins a fence**, or at the end of
/// the text, and says `closed: false`. That is the rule that keeps a reply's remainder from being
/// swallowed: an unclosed fence cannot take a later block's closer for its own, and the block
/// that later opener begins is still there to be read — which matters most in the case that
/// found it, an unclosed fence directly above a well-formed citation.
pub fn fence_at<'a>(lines: &[&'a str], i: usize) -> Option<Fence<'a>> {
    let line: &'a str = lines.get(i)?.trim();
    let run = RUNS.into_iter().find(|run| line.starts_with(run))?;
    let rest = &line[run.len()..];
    if let Some(inner) = rest.strip_suffix(run) {
        let label = inner.trim();
        if !label.is_empty() {
            return Some(Fence {
                run,
                label,
                empty: true,
                closed: true,
                end: i,
            });
        }
    }
    let label = rest.trim();
    let mut empty = true;
    for (j, body) in lines.iter().enumerate().skip(i + 1) {
        let body = body.trim();
        if body == run {
            return Some(Fence {
                run,
                label,
                empty,
                closed: true,
                end: j,
            });
        }
        if begins(body) {
            return Some(Fence {
                run,
                label,
                empty,
                closed: false,
                end: j - 1,
            });
        }
        if !body.is_empty() {
            empty = false;
        }
    }
    Some(Fence {
        run,
        label,
        empty,
        closed: false,
        end: lines.len() - 1,
    })
}

/// **A fence label's range**, split: the label's first word and the lines it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range<'a> {
    /// The word the label starts with — the label a citation is keyed by, since a label may say
    /// more than the range (`src/main.rs:7 the entry point`).
    pub label: &'a str,
    /// The path, relative to the session's workspace or absolute.
    pub path: &'a str,
    /// The first and last line, 1-based and inclusive; `N` alone names the line `N` once.
    pub start: u32,
    pub end: u32,
}

/// **The range a fence label names**, or `None` for an ordinary fence (`rust`, `sql`, empty).
///
/// `path:N` or `path:N-M`, the label's first word; the path must look like one — it has a `/` or
/// a `.` in it — so a label like `rust:2` is not read as a file. `M < N` is not a range.
pub fn range(label: &str) -> Option<Range<'_>> {
    let word = label.split_whitespace().next()?;
    let (path, lines) = word.rsplit_once(':')?;
    if path.is_empty() || !(path.contains('/') || path.contains('.')) {
        return None;
    }
    let (start, end) = match lines.split_once('-') {
        Some((a, b)) => (a.parse::<u32>().ok()?, b.parse::<u32>().ok()?),
        None => {
            let a = lines.parse::<u32>().ok()?;
            (a, a)
        }
    };
    (start >= 1 && end >= start).then_some(Range {
        label: word,
        path,
        start,
        end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The input from the real reply that found the defect: prose, a one-liner alone on its
    /// line, prose.
    const REPLY: &str = "The poll loop:\n\n\
        ```/Users/dead/Projects/firecode/firecode/scripts/vsock-exec.py:60-97```\n\n\
        It waits on the socket.\n";

    fn fences(text: &str) -> Vec<Fence<'_>> {
        let lines: Vec<&str> = text.lines().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let Some(f) = fence_at(&lines, i) else {
                i += 1;
                continue;
            };
            out.push(f);
            i = f.end + 1;
        }
        out
    }

    #[test]
    fn a_one_line_fence_opens_and_closes_on_its_own_line() {
        let got = fences(REPLY);
        assert_eq!(got.len(), 1);
        let f = got[0];
        assert_eq!(f.run, "```");
        assert_eq!(
            f.label,
            "/Users/dead/Projects/firecode/firecode/scripts/vsock-exec.py:60-97"
        );
        assert!(f.closed && f.empty && f.cites());
        assert_eq!(f.end, 2, "the block is the one line it is written on");
        let r = range(f.label).unwrap();
        assert_eq!(
            (r.path, r.start, r.end),
            (
                "/Users/dead/Projects/firecode/firecode/scripts/vsock-exec.py",
                60,
                97
            )
        );
        assert_eq!(r.label, f.label, "no extra words, so the label is the word");
    }

    #[test]
    fn an_empty_block_and_a_quoted_block_are_told_apart() {
        let text = "```a/b.go:2-3\n```\n```a/b.go:2-3\nmine\n```\n~~~c.py:5\n\n~~~\n";
        let got = fences(text);
        let shapes: Vec<(&str, bool, bool, usize)> = got
            .iter()
            .map(|f| (f.label, f.closed, f.empty, f.end))
            .collect();
        assert_eq!(
            shapes,
            vec![
                ("a/b.go:2-3", true, true, 1),
                ("a/b.go:2-3", true, false, 4),
                ("c.py:5", true, true, 7),
            ]
        );
        assert!(!got[1].cites(), "the model's own code is not a citation");
    }

    /// **The rule for a fence that is never closed**: it ends on the line before the next fence
    /// begins, so the block below it is read on its own terms — closer included.
    #[test]
    fn an_unclosed_fence_stops_at_the_next_fence_and_keeps_no_closer() {
        let text = "```a/b.go:1-2\nnever closed\n\n```c/d.go:3-4\n```\n";
        let got = fences(text);
        assert_eq!(got.len(), 2);
        assert_eq!(
            (got[0].label, got[0].closed, got[0].end),
            ("a/b.go:1-2", false, 2),
            "the blank line before the next fence is its last"
        );
        assert!(!got[0].cites());
        assert_eq!((got[1].label, got[1].closed), ("c/d.go:3-4", true));
        assert_eq!(got[1].end, 4, "the well-formed block kept its own closer");
        assert_eq!(range(got[1].label).unwrap().path, "c/d.go");
    }

    #[test]
    fn a_fence_unclosed_to_the_end_of_the_text_is_not_a_citation() {
        let got = fences("```a/b.go:1-2\n\n");
        assert_eq!(got.len(), 1);
        assert!(!got[0].closed && !got[0].cites());
        assert_eq!(got[0].end, 1, "the last line of the text");
    }

    #[test]
    fn a_labelled_empty_fence_without_a_range_is_no_citation() {
        for text in ["```bash```\n", "```\n```\n", "``````\n", "```rust\n```\n"] {
            let got = fences(text);
            assert!(got.iter().all(|f| range(f.label).is_none()), "{text:?}");
        }
    }

    #[test]
    fn a_label_names_a_range_and_an_ordinary_fence_names_none() {
        let r = range("flow/connectors/postgres/pgdump_schema.go:242-248").unwrap();
        assert_eq!(
            (r.path, r.start, r.end),
            ("flow/connectors/postgres/pgdump_schema.go", 242, 248)
        );
        let one = range("src/main.rs:7 the entry point").unwrap();
        assert_eq!((one.start, one.end, one.label), (7, 7, "src/main.rs:7"));
        for not in ["rust", "", "rust:2", "a.go:9-3", "a.go:0", "a.go:x-y", ":4"] {
            assert_eq!(range(not), None, "{not:?}");
        }
    }
}
