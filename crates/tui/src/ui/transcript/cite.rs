//! **A reply's citations, put into the fences that name them** — the head's half of
//! `letibot_harnessd::citations`.
//!
//! The model wrote an empty fence labelled `path:start-end`; the daemon read those lines when the
//! reply landed and sent them as [`Citation`]s. Here they go into the fence before the markdown
//! is lexed, so rano draws the block as a citation — numbered as the file numbers it — and the
//! label grows a note when there is something to say: the daemon could not read the lines, or
//! the file on this machine no longer says what it said.

use std::borrow::Cow;
use std::path::Path;

use letibot_sessionlog::Citation;

/// What a citation's label says when the file on disk no longer has the lines it cited.
pub(crate) const DRIFTED: &str = "changed since the reply";

/// **`text` with every cited fence filled**, or `text` itself when it cites nothing. Each empty
/// fence whose label is a citation takes the next citation with that label; one the daemon sent
/// nothing for is left as written.
pub(crate) fn fill<'t>(text: &'t str, cits: &[Citation], workspace: &str) -> Cow<'t, str> {
    if cits.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut used = vec![false; cits.len()];
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_start();
        let fence = ["```", "~~~"].into_iter().find(|f| t.starts_with(f));
        let filled = fence.and_then(|fence| {
            let label = t[fence.len()..].trim();
            rano::markdown::citation(label)?;
            // The body up to the closing fence must be empty: the model's own code is its own.
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim().is_empty() {
                j += 1;
            }
            if j >= lines.len() || lines[j].trim() != fence {
                return None;
            }
            let k = (0..cits.len()).find(|&k| !used[k] && cits[k].label == label)?;
            used[k] = true;
            Some((j, block(fence, &cits[k], workspace)))
        });
        match filled {
            Some((close, block)) => {
                out.push(block);
                i = close + 1;
            }
            None => {
                out.push(line.to_string());
                i += 1;
            }
        }
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
