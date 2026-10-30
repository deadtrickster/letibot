//! **A reply's citations, put into the fences that name them** — the head's half of
//! `letibot_harnessd::citations`.
//!
//! The model wrote an empty fence labelled `path:start-end`; the daemon read those lines when the
//! reply landed and sent them as [`Citation`]s. Here they go into the fence before the markdown
//! is lexed, so rano draws the block as a citation — numbered as the file numbers it — and the
//! label grows a note when there is something to say: the daemon could not read the lines, or
//! the file on this machine no longer says what it said.
//!
//! **Which fences those are is [`letibot_transcript::citation`]'s rule, not a second copy of it
//! here.** The daemon scans the same text with the same function, so a citation it recognised is
//! a citation this fills — including the one-liner (` ```a/b.go:2-3``` `, opener and closer on one
//! line), which this head used to read as a block whose label had the closer glued onto it and so
//! left empty.

use std::borrow::Cow;
use std::path::Path;

use letibot_sessionlog::Citation;
use letibot_transcript::citation;

/// What a citation's label says when the file on disk no longer has the lines it cited.
pub(crate) const DRIFTED: &str = "changed since the reply";

/// **`text` with every cited fence filled**, or `text` itself when it cites nothing. Each empty
/// fence whose label is a citation takes the next citation with that label; one the daemon sent
/// nothing for is left as written, and so is every block with something in it.
pub(crate) fn fill<'t>(text: &'t str, cits: &[Citation], workspace: &str) -> Cow<'t, str> {
    if cits.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut used = vec![false; cits.len()];
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let Some(fence) = citation::fence_at(&lines, i) else {
            out.push(lines[i].to_string());
            i += 1;
            continue;
        };
        // The label the daemon keyed the citation by is the label's first word, so a label that
        // says more than the range (`a/b.go:2-3 the loop`) still finds its lines.
        let hit = fence
            .cites()
            .then(|| citation::range(fence.label))
            .flatten()
            .and_then(|r| (0..cits.len()).find(|&k| !used[k] && cits[k].label == r.label));
        match hit {
            Some(k) => {
                used[k] = true;
                out.push(block(fence.run, &cits[k], workspace));
            }
            // Not one of ours: the block goes back exactly as written — the model's own code is
            // its own, and a fence inside it is text, not a fence.
            None => out.extend(lines[i..=fence.end].iter().map(|l| l.to_string())),
        }
        i = fence.end + 1;
    }
    Cow::Owned(out.join("\n"))
}

/// One citation as a fence: its label with whatever needs saying, its lines, the close.
fn block(fence: &str, c: &Citation, workspace: &str) -> String {
    let mut notes: Vec<String> = Vec::new();
    if let Some(e) = &c.error {
        notes.push(e.clone());
    }
    if drifted(c, workspace) {
        notes.push(DRIFTED.into());
    }
    let mut s = format!("{fence}{}", c.label);
    for n in notes {
        s.push_str(" · ");
        s.push_str(&n.replace('\n', " "));
    }
    s.push('\n');
    for l in &c.lines {
        // A line that is itself a fence would close this one early.
        if l.trim_start().starts_with(fence) {
            s.push(' ');
        }
        s.push_str(l);
        s.push('\n');
    }
    s.push_str(fence);
    s
}

/// **Whether the file on this machine no longer has the lines cited** — `false` when it cannot
/// be read here at all (a remote daemon, a VM's copy): no comparison is not a difference.
pub(crate) fn drifted(c: &Citation, workspace: &str) -> bool {
    if c.lines.is_empty() || workspace.is_empty() && !Path::new(&c.path).is_absolute() {
        return false;
    }
    let path = if Path::new(&c.path).is_absolute() {
        Path::new(&c.path).to_path_buf()
    } else {
        Path::new(workspace).join(&c.path)
    };
    let Ok(now) = std::fs::read_to_string(path) else {
        return false;
    };
    let now: Vec<&str> = now.lines().collect();
    let from = (c.start as usize).saturating_sub(1);
    now.get(from..from + c.lines.len())
        .is_none_or(|have| have.iter().zip(&c.lines).any(|(a, b)| a != b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cite(label: &str, path: &str, start: u32, lines: &[&str]) -> Citation {
        Citation {
            label: label.into(),
            path: path.into(),
            start,
            end: start + lines.len() as u32 - 1,
            lines: lines.iter().map(|l| l.to_string()).collect(),
            error: None,
        }
    }

    #[test]
    fn an_empty_cited_fence_gets_its_lines_and_quoted_code_is_left_alone() {
        let text = "See:\n\n```a/b.go:2-3\n```\n\nMine:\n\n```a/b.go:2-3\nmy own\n```\n";
        let got = fill(
            text,
            &[cite("a/b.go:2-3", "a/b.go", 2, &["x := 1", "y := 2"])],
            "",
        );
        assert_eq!(
            got,
            "See:\n\n```a/b.go:2-3\nx := 1\ny := 2\n```\n\nMine:\n\n```a/b.go:2-3\nmy own\n```\n"
        );
    }

    /// **The reply that found the defect**: prose, the one-liner alone on its line, prose — the
    /// daemon cites it now, so the head must fill it.
    ///
    /// Asserted by its parts rather than byte for byte, because the label is an ABSOLUTE path and
    /// this box may well have that file: when it does and the lines differ, the label grows
    /// [`DRIFTED`], which is `a_citation_the_file_no_longer_matches_is_marked`'s subject.
    #[test]
    fn a_citation_written_on_one_line_is_filled() {
        const LABEL: &str = "/Users/dead/Projects/firecode/firecode/scripts/vsock-exec.py:60-97";
        const PATH: &str = "/Users/dead/Projects/firecode/firecode/scripts/vsock-exec.py";
        let text = "The poll loop:\n\n\
            ```/Users/dead/Projects/firecode/firecode/scripts/vsock-exec.py:60-97```\n\n\
            It waits on the socket.\n";
        let got = fill(
            text,
            &[cite(LABEL, PATH, 60, &["sock = connect()", "poll(sock)"])],
            "",
        );
        assert!(
            got.starts_with(&format!("The poll loop:\n\n```{LABEL}")),
            "the one-liner became a block labelled as the model wrote it: {got}"
        );
        assert!(
            got.contains("\nsock = connect()\npoll(sock)\n```\n"),
            "the lines it named are in the fence: {got}"
        );
        assert!(
            got.ends_with("It waits on the socket.\n"),
            "and the prose after it is untouched: {got}"
        );
    }

    /// **An unclosed fence does not take the closer of the block below it**, and the well-formed
    /// block is still filled — the head reads fences by the same rule the daemon does.
    #[test]
    fn an_unclosed_fence_does_not_swallow_the_citation_below_it() {
        let text = "```a/b.go:2-3\nleft open\n\n```a/b.go:2-3```\n";
        assert_eq!(
            fill(
                text,
                &[cite("a/b.go:2-3", "a/b.go", 2, &["x := 1", "y := 2"])],
                ""
            ),
            "```a/b.go:2-3\nleft open\n\n```a/b.go:2-3\nx := 1\ny := 2\n```\n"
        );
    }

    /// A label that says more than the range still finds its citation: the daemon keys one by the
    /// label's first word, and this head matches that word rather than the whole label.
    #[test]
    fn a_label_with_words_after_the_range_still_finds_its_lines() {
        assert_eq!(
            fill(
                "```a/b.go:2-3 the loop\n```",
                &[cite("a/b.go:2-3", "a/b.go", 2, &["x := 1", "y := 2"])],
                ""
            ),
            "```a/b.go:2-3\nx := 1\ny := 2\n```"
        );
    }

    #[test]
    fn a_citation_that_could_not_be_read_says_why_in_its_label() {
        let mut c = cite("gone.rs:4", "gone.rs", 4, &["x"]);
        c.lines.clear();
        c.error = Some("gone.rs could not be read: no such file".into());
        assert_eq!(
            fill("```gone.rs:4\n```", &[c], ""),
            "```gone.rs:4 · gone.rs could not be read: no such file\n```"
        );
    }

    /// **The file on disk is compared, and a citation it no longer matches is marked.**
    #[test]
    fn a_citation_the_file_no_longer_matches_is_marked() {
        let dir = std::env::temp_dir().join(format!("letibot-drift-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.go"), "a\nb\nc\n").unwrap();
        let ws = dir.to_str().unwrap();
        let same = cite("f.go:2-3", "f.go", 2, &["b", "c"]);
        assert!(!drifted(&same, ws));
        std::fs::write(dir.join("f.go"), "a\nB\nc\n").unwrap();
        assert!(drifted(&same, ws));
        assert!(
            fill("```f.go:2-3\n```", std::slice::from_ref(&same), ws)
                .starts_with("```f.go:2-3 · changed since the reply\n")
        );
        assert!(
            !drifted(&same, ""),
            "nothing to compare with is not a change"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
