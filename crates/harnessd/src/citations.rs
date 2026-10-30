//! **A reply that cites code gets the code** — the model writes an empty fence labelled
//! `path:start-end` and the daemon fills it from the file when the reply lands.
//!
//! The operator, 2026-10-15: *"model wants to cite me a file with line range e.g.
//! flow/connectors/postgres/pgdump_schema.go:242-248 and it is manually extracting the code and
//! line numbers lost"*. Copying lines by hand loses exactly what a citation is for — where they
//! are — and costs the model the tokens of copying them. So the model names the range and the
//! harness reads it:
//!
//! ````text
//! ```flow/connectors/postgres/pgdump_schema.go:242-248
//! ```
//! ````
//!
//! — or the same fence as a one-liner, its opening and closing runs on one line, which is the
//! spelling a model uses most and the one that was read as an *opening* fence with the closer
//! glued onto its label, and so was never filled. Both spellings, and the rule for a block that
//! is never closed, are [`letibot_transcript::citation`]'s: the head fills these fences, and the
//! two halves must not disagree about which fences there are.
//!
//! # A snapshot, kept beside the reply
//!
//! The lines are read once, when the reply lands, through the session's own backend (so a session
//! in a VM reads the VM's tree), and stored ([`letibot_tokencore::store::Store::put_citations`]),
//! keyed by the reply's text so a fork that carries the reply under a new row id finds them. The
//! reply's text is never rewritten: the next request renders the conversation the model wrote,
//! and the prompt cache is keyed on exactly that. A head shows the snapshot in the fence, with the
//! file's own line numbers, and marks it when the file on its disk no longer says the same.

use letibot_sessionlog::event::Citation;
use letibot_transcript::citation;

/// **The most lines one citation reads.** A citation is a passage, and a fence naming a whole
/// file is read up to here and says so, rather than pouring the file into the reply.
pub const MAX_CITED_LINES: u32 = 400;

/// One empty fence a reply cites by: its label and the range it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cited {
    pub label: String,
    pub path: String,
    pub start: u32,
    pub end: u32,
}

/// **The range a fence label names**, or `None` for an ordinary fence (`rust`, `sql`, empty).
///
/// `path:N` or `path:N-M`, the label's first word; the path must look like one — it has a `/` or
/// a `.` in it — so a label like `rust:2` is not read as a file. `M < N` is not a range. The rule
/// itself is [`letibot_transcript::citation::range`]'s, so the head that fills the fence reads a
/// label exactly as this does.
pub fn parse_label(label: &str) -> Option<Cited> {
    let r = citation::range(label)?;
    Some(Cited {
        label: r.label.to_string(),
        path: r.path.to_string(),
        start: r.start,
        end: r.end,
    })
}

/// **Every empty fence in `text` whose label names a range**, in order — whether it was written
/// as a block or on one line. A fence with anything in it is the model's own code, quoted, and is
/// left alone; an unclosed one is not a citation.
pub fn cited_fences(text: &str) -> Vec<Cited> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(fence) = citation::fence_at(&lines, i) else {
            i += 1;
            continue;
        };
        // Past the whole block, so a fence inside quoted code is not read as one of these — and
        // so an unclosed fence cannot lend its own body to the scanner.
        i = fence.end + 1;
        if fence.cites()
            && let Some(c) = parse_label(fence.label)
        {
            out.push(c);
        }
    }
    out
}

/// **Read one citation**, through `read` (the session backend's own read, relative to the
/// session's root). A file that cannot be read, a range past its end and a range longer than
/// [`MAX_CITED_LINES`] all say so in `error` — with whatever lines there are.
pub fn snapshot(c: &Cited, read: impl Fn(&str) -> Result<Vec<u8>, String>) -> Citation {
    let mut out = Citation {
        label: c.label.clone(),
        path: c.path.clone(),
        start: c.start,
        end: c.end,
        lines: Vec::new(),
        error: None,
    };
    let bytes = match read(&c.path) {
        Ok(b) => b,
        Err(why) => {
            out.error = Some(format!("{} could not be read: {why}", c.path));
            return out;
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    let all: Vec<&str> = text.lines().collect();
    let n = all.len() as u32;
    if c.start > n {
        out.error = Some(format!(
            "{} has {n} line{}; the citation starts at {}",
            c.path,
            if n == 1 { "" } else { "s" },
            c.start
        ));
        return out;
    }
    let last = c.end.min(n).min(c.start + MAX_CITED_LINES - 1);
    out.lines = all[(c.start - 1) as usize..last as usize]
        .iter()
        .map(|l| l.to_string())
        .collect();
    if last < c.end {
        out.error = Some(if c.end > n {
            format!("{} has {n} lines; the citation runs to {}", c.path, c.end)
        } else {
            format!(
                "{} lines shown of {}: a citation is a passage, and {MAX_CITED_LINES} is as \
                 many as one reads",
                last - c.start + 1,
                c.end - c.start + 1
            )
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_names_a_range_and_an_ordinary_fence_names_none() {
        let c = parse_label("flow/connectors/postgres/pgdump_schema.go:242-248").unwrap();
        assert_eq!(
            (c.path.as_str(), c.start, c.end),
            ("flow/connectors/postgres/pgdump_schema.go", 242, 248)
        );
        let one = parse_label("src/main.rs:7 the entry point").unwrap();
        assert_eq!(
            (one.start, one.end, one.label.as_str()),
            (7, 7, "src/main.rs:7")
        );
        for not in ["rust", "", "rust:2", "a.go:9-3", "a.go:0", "a.go:x-y", ":4"] {
            assert_eq!(parse_label(not), None, "{not:?}");
        }
    }

    #[test]
    fn only_an_empty_closed_fence_with_a_range_is_a_citation() {
        let text = "See:\n\n```a/b.go:2-3\n```\n\nAnd quoted:\n\n```a/b.go:2-3\nmine\n```\n\
                    ```rust\n```\n~~~c.py:5\n\n~~~\n```d.rs:1\n";
        let got: Vec<String> = cited_fences(text).into_iter().map(|c| c.label).collect();
        assert_eq!(got, vec!["a/b.go:2-3", "c.py:5"]);
    }

    /// **The reply that found the defect**: prose, the one-liner alone on its line, prose. It was
    /// read as an opening fence with the closing run glued onto its label, so nothing was cited
    /// and the operator's screen showed an empty box with a path in it.
    const ONE_LINER: &str = "The poll loop:\n\n\
        ```/Users/dead/Projects/firecode/firecode/scripts/vsock-exec.py:60-97```\n\n\
        It waits on the socket.\n";

    #[test]
    fn a_citation_written_on_one_line_is_cited() {
        assert_eq!(
            cited_fences(ONE_LINER),
            vec![Cited {
                label: "/Users/dead/Projects/firecode/firecode/scripts/vsock-exec.py:60-97"
                    .to_string(),
                path: "/Users/dead/Projects/firecode/firecode/scripts/vsock-exec.py".to_string(),
                start: 60,
                end: 97,
            }]
        );
        // A labelled one-liner with no range, and a bare run, are not citations either.
        assert!(cited_fences("```bash```\n").is_empty());
        assert!(cited_fences("```\n```\n").is_empty());
    }

    #[test]
    fn a_snapshot_of_a_one_line_citation_reads_its_lines() {
        let file = (1..=120).map(|i| format!("line {i}\n")).collect::<String>();
        let cited = cited_fences(ONE_LINER);
        let s = snapshot(&cited[0], |_| Ok(file.clone().into_bytes()));
        assert_eq!(s.lines.len(), 38, "60 through 97");
        assert_eq!(s.lines.first().map(String::as_str), Some("line 60"));
        assert_eq!(s.lines.last().map(String::as_str), Some("line 97"));
        assert_eq!(s.error, None);
    }

    /// **A fence with no closing line must not swallow the reply** — and must not take a later
    /// block's closer for its own: it ends on the line before the next fence begins.
    #[test]
    fn an_unclosed_fence_does_not_eat_the_block_below_it() {
        let text = "```a/b.go:1-2\nnever closed\n\n```c/d.go:3-4\n```\n";
        let got: Vec<String> = cited_fences(text).into_iter().map(|c| c.label).collect();
        assert_eq!(got, vec!["c/d.go:3-4"], "the closed one, and only it");
        assert!(cited_fences("```a/b.go:1-2\n\n").is_empty());
    }

    #[test]
    fn a_snapshot_reads_the_range_and_says_what_it_could_not() {
        let file = (1..=10).map(|i| format!("line {i}\n")).collect::<String>();
        let read = |p: &str| {
            if p == "f.go" {
                Ok(file.clone().into_bytes())
            } else {
                Err("no such file".to_string())
            }
        };
        let s = snapshot(&parse_label("f.go:3-5").unwrap(), read);
        assert_eq!(s.lines, vec!["line 3", "line 4", "line 5"]);
        assert_eq!(s.error, None);
        let past = snapshot(&parse_label("f.go:9-14").unwrap(), read);
        assert_eq!(past.lines, vec!["line 9", "line 10"]);
        assert_eq!(
            past.error.as_deref(),
            Some("f.go has 10 lines; the citation runs to 14")
        );
        let beyond = snapshot(&parse_label("f.go:40").unwrap(), read);
        assert!(beyond.lines.is_empty());
        assert_eq!(
            beyond.error.as_deref(),
            Some("f.go has 10 lines; the citation starts at 40")
        );
        let gone = snapshot(&parse_label("g.go:1").unwrap(), read);
        assert_eq!(
            gone.error.as_deref(),
            Some("g.go could not be read: no such file")
        );
    }

    #[test]
    fn a_whole_file_is_read_up_to_the_cap_and_says_so() {
        let file = (1..=1000).map(|i| format!("{i}\n")).collect::<String>();
        let s = snapshot(&parse_label("big.txt:1-1000").unwrap(), |_| {
            Ok(file.clone().into_bytes())
        });
        assert_eq!(s.lines.len(), MAX_CITED_LINES as usize);
        assert!(s.error.unwrap().starts_with("400 lines shown of 1000"));
    }
}
