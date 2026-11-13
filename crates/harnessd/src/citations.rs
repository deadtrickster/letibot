//! A reply that cites code gets the code: the model writes an empty fence
//! labelled `path:start-end` and the daemon fills it from the file when the reply
//! lands. Copying lines by hand loses what a citation is for and costs the model
//! the tokens of copying them.
//!
//! ````text
//! ```flow/connectors/postgres/pgdump_schema.go:242-248
//! ```
//! ````
//!
//! The same fence may be a one-liner, its opening and closing runs on one line
//! which is the spelling a model uses most. Both spellings, and the rule for a
//! block that is never closed, are `letibot_transcript::citation`'s: the head
//! fills these fences too, and the two halves must agree about which fences there
//! are.
//!
//! The fence also MARKS lines: `note path:3-4` holds a sentence about them and
//! `suggest path:3-4` a replacement for them, and both ride the same event with
//! their body, so the editor draws a mark instead of a quotation.
//!
//! The lines are read once, when the reply lands, through the session's own
//! backend (so a session in a VM reads the VM's tree), and stored
//! ([`letibot_tokencore::store::Store::put_citations`]) keyed by the reply's text
//! so a fork carrying the reply under a new row id still finds them. The reply is
//! never rewritten, since the prompt cache is keyed on exactly it. A range an
//! unsaved editor buffer covered is read from THAT text - the model was shown the
//! buffer, so the box's file is the wrong anchor for the mark it made.

use letibot_sessionlog::event::{Citation, CitationKind};
use letibot_sessionlog::protocol::EditorContext;
use letibot_transcript::citation::{self, MarkKind};

/// The most lines one citation reads. A citation is a passage, and a fence naming
/// a whole file is read up to here and says so, rather than pouring the file in.
pub const MAX_CITED_LINES: u32 = 400;

/// One line-addressed fence a reply makes: its label and the range it names, what kind of mark
/// it is, and the text the block held (`note`'s sentence, `suggest`'s replacement).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cited {
    pub label: String,
    pub path: String,
    pub start: u32,
    pub end: u32,
    pub kind: CitationKind,
    pub body: String,
}

/// The range a fence label names, or `None` for an ordinary fence (`rust`, `sql` empty).
/// `path:N` or `path:N-M`, the label's first word, and the path must look like one (it has
/// a `/` or a `.`) so `rust:2` is not read as a file.
pub fn parse_label(label: &str) -> Option<Cited> {
    let r = citation::range(label)?;
    Some(Cited {
        label: r.label.to_string(),
        path: r.path.to_string(),
        start: r.start,
        end: r.end,
        kind: CitationKind::Cite,
        body: String::new(),
    })
}

/// Every fence in `text` whose label names a range, in order, whether it was written as a block
/// or on one line: an empty one is a citation, and one labelled `note`/`suggest` is a mark whose
/// body is the block's own text. Any other fence with something in it is the model's own code,
/// quoted, and is left alone; an unclosed one is neither.
pub fn cited_fences(text: &str) -> Vec<Cited> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(fence) = citation::fence_at(&lines, i) else {
            i += 1;
            continue;
        };
        // Past the whole block, so a fence inside quoted code is not read as one of
        // these, and an unclosed fence cannot lend its own body to the scanner.
        i = fence.end + 1;
        if let Some((kind, r)) = citation::mark(fence.label) {
            // A one-liner has no room for a body, and an unclosed block's body cannot be told
            // from the next one's: only a closed block is a mark.
            if !fence.closed {
                continue;
            }
            out.push(Cited {
                label: r.label.to_string(),
                path: r.path.to_string(),
                start: r.start,
                end: r.end,
                kind: match kind {
                    MarkKind::Note => CitationKind::Note,
                    MarkKind::Suggest => CitationKind::Suggest,
                },
                body: fence.body(&lines).join("\n"),
            });
        } else if fence.cites()
            && let Some(c) = parse_label(fence.label)
        {
            out.push(c);
        }
    }
    out
}

/// A file's lines as an editor's buffer had them, from an `unsaved` `ContextRef` a prompt
/// carried: the anchor a mark must land on, since the file on disk does not say what the model
/// was shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Buffer {
    /// The file in the spelling the model read in the context block - relative to the session's
    /// workspace when it is inside it, which is also how a fence may cite it.
    pub file: String,
    /// The number the buffer's first line has in the file.
    pub first: u32,
    /// The captured lines, without their line ends.
    pub text: String,
}

/// The unsaved references a prompt carried, in the order it attached them. Pure: the text came
/// with the frame, so nothing is read here.
pub fn buffers(context: &EditorContext, workspace: &str) -> Vec<Buffer> {
    context
        .refs
        .iter()
        .filter(|r| r.unsaved && !r.text.is_empty())
        .map(|r| Buffer {
            file: crate::editor_context::workspace_relative(workspace, &r.file),
            first: r
                .range
                .map(|x| x.start.line)
                .or(r.cursor.map(|c| c.line))
                .unwrap_or(1),
            text: r.text.clone(),
        })
        .collect()
}

/// The lines a buffer holds for this range, or `None` when no unsaved capture of the file covers
/// it - then the file on disk is the only anchor there is.
fn buffered(buffers: &[Buffer], c: &Cited) -> Option<Vec<String>> {
    if c.end - c.start + 1 > MAX_CITED_LINES {
        return None;
    }
    // The last capture wins: a file attached twice is the buffer as it was at the later ask.
    let b = buffers.iter().rev().find(|b| b.file == c.path)?;
    let held: Vec<&str> = b.text.lines().collect();
    let past = b.first + held.len() as u32;
    if b.first > c.start || c.end >= past {
        return None;
    }
    let from = (c.start - b.first) as usize;
    Some(
        held[from..(c.end - b.first) as usize + 1]
            .iter()
            .map(|l| l.to_string())
            .collect(),
    )
}

/// Read one citation through `read` (the session backend's own read, relative to the
/// session's root). A file that cannot be read, a range past its end and a range longer
/// than [`MAX_CITED_LINES`] all say so in `error`, with whatever lines there are. A range an
/// unsaved buffer covers is read from the buffer instead: that is what the model was shown.
pub fn snapshot(
    c: &Cited,
    buffers: &[Buffer],
    read: impl Fn(&str) -> Result<Vec<u8>, String>,
) -> Citation {
    let mut out = Citation {
        label: c.label.clone(),
        path: c.path.clone(),
        start: c.start,
        end: c.end,
        lines: Vec::new(),
        error: None,
        kind: c.kind,
        body: c.body.clone(),
    };
    if let Some(lines) = buffered(buffers, c) {
        out.lines = lines;
        return out;
    }
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

    /// A reply that puts the one-liner alone on its line, between prose: the
    /// spelling a model uses most, and the one that must not be read as an opening
    /// fence with the closing run glued onto its label.
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
                kind: CitationKind::Cite,
                body: String::new(),
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
        let s = snapshot(&cited[0], &[], |_| Ok(file.clone().into_bytes()));
        assert_eq!(s.lines.len(), 38, "60 through 97");
        assert_eq!(s.lines.first().map(String::as_str), Some("line 60"));
        assert_eq!(s.lines.last().map(String::as_str), Some("line 97"));
        assert_eq!(s.error, None);
    }

    /// A fence with no closing line must not swallow the reply, and must not take a
    /// later block's closer for its own: it ends on the line before the next fence
    /// begins.
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
        let s = snapshot(&parse_label("f.go:3-5").unwrap(), &[], read);
        assert_eq!(s.lines, vec!["line 3", "line 4", "line 5"]);
        assert_eq!(s.error, None);
        let past = snapshot(&parse_label("f.go:9-14").unwrap(), &[], read);
        assert_eq!(past.lines, vec!["line 9", "line 10"]);
        assert_eq!(
            past.error.as_deref(),
            Some("f.go has 10 lines; the citation runs to 14")
        );
        let beyond = snapshot(&parse_label("f.go:40").unwrap(), &[], read);
        assert!(beyond.lines.is_empty());
        assert_eq!(
            beyond.error.as_deref(),
            Some("f.go has 10 lines; the citation starts at 40")
        );
        let gone = snapshot(&parse_label("g.go:1").unwrap(), &[], read);
        assert_eq!(
            gone.error.as_deref(),
            Some("g.go could not be read: no such file")
        );
    }

    #[test]
    fn a_whole_file_is_read_up_to_the_cap_and_says_so() {
        let file = (1..=1000).map(|i| format!("{i}\n")).collect::<String>();
        let s = snapshot(&parse_label("big.txt:1-1000").unwrap(), &[], |_| {
            Ok(file.clone().into_bytes())
        });
        assert_eq!(s.lines.len(), MAX_CITED_LINES as usize);
        assert!(s.error.unwrap().starts_with("400 lines shown of 1000"));
    }

    /// A `note` or a `suggest` fence is the same range with a word in front of it and the body the
    /// editor draws: the kind is the word, the body is the block, and the label a head matches by
    /// is the range alone.
    #[test]
    fn a_marking_fence_is_read_with_its_kind_and_its_body() {
        let text = "Two findings.\n\n```note docs/article.md:41-42\nwarn: the date is wrong\nthe cable was 1866\n```\n\n\
                    ```suggest docs/article.md:41-41\nlaid in 1866 by the Great Eastern\n```\n";
        let got = cited_fences(text);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].kind, CitationKind::Note);
        assert_eq!(got[0].label, "docs/article.md:41-42");
        assert_eq!(
            (got[0].path.as_str(), got[0].start, got[0].end),
            ("docs/article.md", 41, 42)
        );
        assert_eq!(got[0].body, "warn: the date is wrong\nthe cable was 1866");
        assert_eq!(got[1].kind, CitationKind::Suggest);
        assert_eq!(got[1].body, "laid in 1866 by the Great Eastern");
        assert_eq!(
            (got[1].path.as_str(), got[1].start, got[1].end),
            ("docs/article.md", 41, 41)
        );

        // An empty fence is still a citation, and the body of a one-liner is nothing.
        assert_eq!(
            cited_fences("```a/b.go:2-3\n```\n")[0].kind,
            CitationKind::Cite
        );
        let one = cited_fences("```note a/b.go:2-3```\n");
        assert_eq!(one[0].kind, CitationKind::Note);
        assert!(one[0].body.is_empty());
        // A word that is not one of the two, and an unclosed mark, are neither.
        assert!(cited_fences("```noted a/b.go:2-3\nbody\n```\n").is_empty());
        assert!(cited_fences("```suggest a/b.go:2-3\nnever closed\n").is_empty());
    }

    /// A mark on an unsaved article lands on the BUFFER's lines: the file on disk is not what the
    /// model read, so its anchor is the editor's text cut to the range - and only when the capture
    /// covers the whole range, else the disk is all there is.
    #[test]
    fn an_unsaved_buffers_text_is_the_anchor_a_mark_lands_on() {
        let disk = (1..=10).map(|i| format!("disk {i}\n")).collect::<String>();
        let read = |_: &str| Ok(disk.clone().into_bytes());
        let buffer = Buffer {
            file: "docs/article.md".into(),
            first: 40,
            text: "saved 40\n41 the cable\n42 was laid\nunwritten 43\n".into(),
        };
        let cited = cited_fences(
            "```suggest docs/article.md:41-42\n41 the cable, in 1866\n42 was laid there\n```\n",
        );
        let s = snapshot(&cited[0], std::slice::from_ref(&buffer), read);
        assert_eq!(s.lines, vec!["41 the cable", "42 was laid"]);
        assert_eq!(
            s.error, None,
            "the buffer is what was shown, so nothing is missing"
        );
        assert_eq!(s.kind, CitationKind::Suggest);
        assert!(s.body.starts_with("41 the cable, in 1866"));

        // A range the capture does not hold, and a different file: the disk answers, as it always
        // did. (The buffer starts at 40, so lines 1-2 are outside it.)
        let outside = cited_fences("```docs/article.md:1-2\n```\n");
        let s = snapshot(&outside[0], std::slice::from_ref(&buffer), read);
        assert_eq!(s.lines, vec!["disk 1", "disk 2"]);
        let other = cited_fences("```notes/other.md:3\n```\n");
        let s = snapshot(&other[0], &[buffer], read);
        assert_eq!(s.lines, vec!["disk 3"]);
    }

    /// The buffers a prompt carried are its unsaved references and nothing else, spelled as the
    /// model read them in the context block - which is the spelling a fence can name.
    #[test]
    fn the_buffers_are_the_unsaved_refs_in_the_blocks_own_spelling() {
        use letibot_sessionlog::protocol::{ContextPos, ContextRange, ContextRef};
        let mut saved = ContextRef {
            file: "/ws/notes/sources.md".into(),
            text: "a line\n".into(),
            ..Default::default()
        };
        saved.unsaved = false;
        let unsaved = ContextRef {
            file: "/ws/docs/article.md".into(),
            range: Some(ContextRange {
                start: ContextPos {
                    line: 41,
                    column: 1,
                },
                end: ContextPos {
                    line: 48,
                    column: 1,
                },
            }),
            text: "the cable\n".into(),
            unsaved: true,
            ..Default::default()
        };
        let empty = ContextRef {
            file: "/ws/empty.md".into(),
            unsaved: true,
            ..Default::default()
        };
        let c = EditorContext {
            origin: "rano".into(),
            refs: vec![saved, unsaved, empty],
            ..Default::default()
        };
        let got = buffers(&c, "/ws");
        assert_eq!(
            got.len(),
            1,
            "only an unsaved ref with text is an anchor: {got:?}"
        );
        assert_eq!(got[0].file, "docs/article.md");
        assert_eq!(got[0].first, 41);
    }
}
