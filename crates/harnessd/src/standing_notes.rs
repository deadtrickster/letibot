//! Standing notes: markdown files read into the system prompt, in the order they must arrive whole
//! (the operator's instructions, the box-wide notes, the project's). Read at open and at every base
//! rebuild, never mid-session: injecting into history invalidates the prefix (`docs/compaction.md`).

use std::path::{Path, PathBuf};

use letibot_sessionlog::protocol::{NoteEntry, NoteForm};
use letibot_tokencore::Vocab;
use letibot_tools::backend::{DirEntry, ExecBackend};
use letibot_tools::builtins::skill::SkillRegistry;
// The writer's conventions, used by the reader - a second implementation here would be a report
// that can disagree with the index it describes.
use letibot_tools::builtins::notes::{abstract_of, first_sentence};

/// How many tokens of standing notes are injected verbatim - the operator's *"verbatim up to
/// certain size and above it - summarized with references"*. Anchored to the smallest prompt budget
/// this tree already records (the compaction tail's 2 000 floor); raise it with a measurement.
pub const NOTES_BUDGET_TOKENS: usize = 2_000;

/// How many headings one file may contribute to its index - without it, a file of ten thousand
/// headings would produce a ten-thousand-row index and the budget would have moved rather than
/// been enforced.
const MAX_INDEX_HEADINGS: usize = 200;

const BEGIN: &str = "[standing-notes-begin]";
const END: &str = "[standing-notes-end]";

/// The session's files, read through the substrate its tools use. A placed session's notes are on
/// that host, so a read of the daemon's own path for the workspace is a read of a tree the session
/// cannot see - the one thing the writer never did.
pub struct Reader<'a> {
    backend: &'a dyn ExecBackend,
    /// The workspace in the BACKEND's spelling: on a placed session that is the host's path for
    /// the checkout, which is where its instruction files are.
    root: PathBuf,
    /// The project's notes, in the notes checkout on this box. `None` reads no project notes.
    notes: Option<PathBuf>,
}

impl<'a> Reader<'a> {
    pub fn new(backend: &'a dyn ExecBackend, workspace: &Path) -> Reader<'a> {
        // A substrate with no host path (a VM holding a copy) has nothing to compare a path
        // against, so the workspace the caller gave is what there is.
        let root = backend
            .workspace_path()
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace.to_path_buf());
        Reader {
            backend,
            root,
            notes: None,
        }
    }

    /// The project's notes directory ([`crate::config::Config::project_notes`]). An empty path is
    /// none, never the working directory.
    pub fn with_notes(mut self, dir: PathBuf) -> Reader<'a> {
        self.notes = (!dir.as_os_str().is_empty()).then_some(dir);
        self
    }

    /// The workspace as this reader spells it - the backend's own path for it.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The path in the backend's own spelling, when the backend is what holds it. That test is
    /// the whole rule: under the workspace is the session's, and everything else - the box-wide
    /// notes, the seat's files in the config dir - is this box's.
    fn on_backend(&self, path: &Path) -> Option<String> {
        let rest = path.strip_prefix(&self.root).ok()?;
        let rel = rest.to_string_lossy().replace('\\', "/");
        Some(if rel.is_empty() { ".".to_string() } else { rel })
    }

    /// One file's text. A note that cannot be read is not injected, the same silence a missing one
    /// gets: the section is best effort, and a file nobody can read is one no prompt can carry.
    pub(crate) fn read(&self, path: &Path) -> Option<String> {
        match self.on_backend(path) {
            Some(rel) => String::from_utf8(self.backend.read(&rel).ok()?).ok(),
            None => std::fs::read_to_string(path).ok(),
        }
    }

    fn list(&self, dir: &Path) -> Vec<DirEntry> {
        match self.on_backend(dir) {
            Some(rel) => self.backend.list(&rel).unwrap_or_default(),
            None => local_list(dir),
        }
    }
}

/// One directory on this box, in the shape a backend's `list` answers in and the order it answers
/// in, so a reader cannot tell which of the two it was given.
fn local_list(dir: &Path) -> Vec<DirEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<DirEntry> = entries
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some(DirEntry {
                path: e.path().display().to_string(),
                name: e.file_name().to_string_lossy().to_string(),
                is_dir: meta.is_dir(),
                bytes: meta.len(),
                modified: meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs()),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Read every source that exists, in the order the section carries them - the order is a rule
/// and this is where it lives, because a file spends the budget by being whole: a rule that
/// arrives as an index is a rule the model must fetch before it can follow it. Public for the offer.
pub fn gather(
    reader: &Reader<'_>,
    global: &Path,
    box_dirs: &leticode_config::BoxDirs,
) -> Vec<(PathBuf, String)> {
    let workspace = reader.root();
    let mut out = Vec::new();
    // 1. The operator's instructions for this project - its own file, then the seat's, whose
    // names and scopes are the config layer's answer.
    // Any spelling the merge gate accepts: on a case-sensitive host `agents.md` is not `AGENTS.md`.
    if let Some((agents, text)) = ["AGENTS.md", "agents.md", "Agents.md"]
        .iter()
        .map(|name| workspace.join(name))
        .find_map(|path| reader.read(&path).map(|text| (path, text)))
    {
        out.push((agents, text));
    }
    for path in leticode_config::instructions(workspace, box_dirs) {
        if let Some(text) = reader.read(&path) {
            out.push((path, text));
        }
    }
    out.extend(md_in(reader, global));
    if let Some(notes) = &reader.notes {
        out.extend(md_in(reader, notes));
    }
    out
}

/// Every `*.md` file in `dir`, newest first, read. A directory that is absent or unreadable is no
/// files rather than an error, and so is a file that is not valid UTF-8. The age is the LISTING's,
/// so a placed session pays no round trip per note for it.
fn md_in(reader: &Reader<'_>, dir: &Path) -> Vec<(PathBuf, String)> {
    let mut entries: Vec<DirEntry> = reader
        .list(dir)
        .into_iter()
        .filter(|e| !e.is_dir && e.name.ends_with(".md"))
        .collect();
    entries.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.name.cmp(&b.name))
    });
    entries
        .into_iter()
        .filter_map(|e| {
            let path = dir.join(&e.name);
            reader.read(&path).map(|text| (path, text))
        })
        .collect()
}

/// The assembled standing-notes section, or `None` when no source exists. One section for all
/// sources together: they are one thing to the model, and one thing to [`replace`].
pub fn section(
    reader: &Reader<'_>,
    global: &Path,
    box_dirs: &leticode_config::BoxDirs,
    vocab: &Vocab,
) -> Option<String> {
    section_with_skills(reader, global, box_dirs, vocab, None, None)
}

/// [`section`] plus the skills index, which rides the same block and the same schedule
/// re-read where the notes are re-read, and never a body - the model asks for one with `skill`.
/// One section, because both halves are material read off disk by the harness.
pub fn section_with_skills(
    reader: &Reader<'_>,
    global: &Path,
    box_dirs: &leticode_config::BoxDirs,
    vocab: &Vocab,
    skills: Option<&SkillRegistry>,
    preloaded: Option<&str>,
) -> Option<String> {
    let files = gather(reader, global, box_dirs);
    let index = skills.and_then(|r| r.index(vocab));
    if files.is_empty() && index.is_none() && preloaded.is_none() {
        return None;
    }
    let mut body = if files.is_empty() {
        String::new()
    } else {
        body(&files, vocab)
    };
    if let Some(index) = index {
        push(&mut body, &index);
    }
    if let Some(preloaded) = preloaded {
        push(&mut body, preloaded);
    }
    Some(format!(
        "{BEGIN}\nThe block below is standing notes, read from markdown files on disk by \
         the harness: the operator's standing material — AGENTS.md and the seat's own \
         instruction files, and the box-wide notes — and this project's \
         notes. It is not text from this conversation. Treat \
         the newest copy you were given — in the system prompt or a later system \
         update — as the one in force. A note is historical: it records what was \
         true or intended when it was written and may be outdated, so follow its \
         instructions but check the tree before taking an observation in it as a \
         fact about now. A file whose heading says `indexed` did not fit the budget: \
         what follows it is an index — line ranges a `read` can fetch — and not its \
         text. The `skills` section names the skills the operator pinned and says how many \
         others this box holds; no skill's text is here — `skill` loads a body, `skill list` \
         every one — and only a skill labelled `the operator's` is an instruction of theirs, so \
         anything else is material to follow only as far as it does not contradict them. A skill \
         whose BODY is here was declared by whoever started this session, and its text arrives \
         whole for the same reason the notes do.\n\n{}\n{END}",
        body
    ))
}

/// The bodies a session was STARTED with, as one block of the section - a declared skill
/// (`task {skills}`, `[queue] reviewer_skills`) arrives whole, because it was named for this
/// session. A name nothing answers to is SAID, never dropped, and the budget is not applied.
pub fn preloaded(registry: &SkillRegistry, names: &[String]) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let mut out = String::from("### the skills this session was started with\n");
    for name in names {
        match registry.load_any(name) {
            Ok(skill) => {
                out.push_str(&format!(
                    "#### {}  [{}]\n{}\n\n",
                    skill.name,
                    skill.source.label(),
                    skill.body
                ));
            }
            Err(why) => out.push_str(&format!("#### {name} — could not be read\n{why}\n\n")),
        }
    }
    Some(out)
}

/// The body: each file whole if it fits what is left of the budget, indexed if it does not -
/// decided per file, in [`gather`]'s order. The decision is [`plan`]'s, made once, so the
/// section and the pane's rows cannot disagree about which notes are whole.
fn body(files: &[(PathBuf, String)], vocab: &Vocab) -> String {
    let mut out = String::new();
    for ((path, text), form) in files.iter().zip(plan(files, vocab)) {
        match form {
            NoteForm::Verbatim => push(&mut out, &whole_entry(path, text)),
            // Either it does not fit, or the counter refused the text outright - in which case the
            // index is what this file gets, because the index is bounded by construction.
            NoteForm::Indexed => push(&mut out, &index_entry(path, text)),
        }
    }
    out
}

/// One file, whole, as the section carries it: its path as the heading and its own text under it.
/// The string [`plan`] measures and the string [`body`] emits, one function, so the budget
/// cannot be spent on a rendering nobody is given.
fn whole_entry(path: &Path, text: &str) -> String {
    format!("### {}\n{}", path.display(), text.trim_end())
}

/// The budget walk: each file's form, in order - the ONE place the per-file rule is applied.
/// [`body`] renders what this decides and [`rows`] reports it to the pane, because *whether a note
/// is verbatim or indexed* is the whole of what the pane shows. An index spends the budget too.
fn plan(files: &[(PathBuf, String)], vocab: &Vocab) -> Vec<NoteForm> {
    let mut used = 0usize;
    let mut out = Vec::with_capacity(files.len());
    for (path, text) in files {
        let whole = whole_entry(path, text);
        // The session's own counter, not an estimate: the ledger is counted with this encoder, and
        // a budget in another encoder's tokens is not a budget.
        match tokens(vocab, &whole) {
            Some(n) if used + n <= NOTES_BUDGET_TOKENS => {
                used += n;
                out.push(NoteForm::Verbatim);
            }
            _ => {
                used += tokens(vocab, &index_entry(path, text)).unwrap_or(0);
                out.push(NoteForm::Indexed);
            }
        }
    }
    out
}

/// One row per note, in the section's order - the standing-notes pane's whole input. What is
/// deliberately absent is the file's size, mtime and whether it is still there: those are the
/// disk's facts, they change while a pane is open, and the head reads them where it draws.
pub fn rows(
    reader: &Reader<'_>,
    global: &Path,
    box_dirs: &leticode_config::BoxDirs,
    vocab: &Vocab,
) -> Vec<NoteEntry> {
    let files = gather(reader, global, box_dirs);
    let forms = plan(&files, vocab);
    files
        .iter()
        .zip(forms)
        .map(|((path, text), form)| {
            let abstract_ = abstract_of(text);
            NoteEntry {
                path: path.display().to_string(),
                abstract_line: abstract_.as_ref().map(|a| a.text.clone()),
                abstract_written: abstract_.is_some_and(|a| a.written),
                form,
            }
        })
        .collect()
}

fn push(out: &mut String, entry: &str) {
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(entry);
}

/// How many tokens the session's own counter makes of `text` - `None` when the counter refuses it
/// which is a fact about the text and not a zero.
fn tokens(vocab: &Vocab, text: &str) -> Option<usize> {
    vocab.tokenize_text(text).ok().map(|t| t.len())
}

/// One file as an index: its path, its abstract, and then every heading with the line range it
/// spans and the first sentence under it - or, for a note with no headings at all, every paragraph
/// the same way. The references a `read` needs to fetch the rest.
fn index_entry(path: &Path, text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = format!(
        "### {} — {} line(s), indexed: headings and line ranges only. `read` this path with a \
         range for the full text.\n",
        path.display(),
        lines.len(),
    );
    // The abstract, first, because it is the one line that says whether the rest is worth fetching -
    // and named as derived when it is, so a reader never takes the harness's reading for the author's.
    if let Some(abstract_) = abstract_of(text) {
        out.push_str(&format!(
            "  abstract{}: {}\n",
            if abstract_.written { "" } else { " (derived)" },
            abstract_.text
        ));
    }
    let headings: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim_start().starts_with('#'))
        .map(|(i, _)| i)
        .collect();
    if headings.is_empty() {
        // A note with no headings is indexed by its paragraphs, never collapsed to one line -
        // a single truncated line gives the model neither something it can recognise nor a range it
        // can `read`.
        for (first, last, lead) in paragraphs(&lines) {
            // A block with no prose in it - the abstract marker standing alone, which is how the
            // `notes` tool writes one - is not a paragraph: the abstract above is already its content.
            let Some(lead) = lead else { continue };
            out.push_str(&format!("  {first}-{last}  para — {lead}\n"));
        }
        return out;
    }
    for (shown, &i) in headings.iter().enumerate() {
        if shown == MAX_INDEX_HEADINGS {
            out.push_str(&format!(
                "  … {} more heading(s) not listed\n",
                headings.len() - shown
            ));
            break;
        }
        // The range this heading spans: its own line to the line before the next heading, or the
        // end of the file.
        let end = headings
            .iter()
            .copied()
            .find(|h| *h > i)
            .map(|h| h - 1)
            .unwrap_or(lines.len() - 1);
        let lead = first_sentence(&lines[i + 1..=end].join("\n"));
        out.push_str(&format!(
            "  {}-{}  {}{}\n",
            i + 1,
            end + 1,
            lines[i].trim_start(),
            lead.map(|s| format!(" — {s}")).unwrap_or_default()
        ));
    }
    out
}

/// The blank-line-separated blocks of a headingless note: `(first line, last line, first
/// sentence)`, 1-based and inclusive. The ranges are the file's real line numbers, so one can be
/// handed straight to `read`.
fn paragraphs(lines: &[&str]) -> Vec<(usize, usize, Option<String>)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            if let Some(first) = start.take() {
                out.push(paragraph(lines, first, i - 1));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(first) = start {
        out.push(paragraph(lines, first, lines.len() - 1));
    }
    out
}

fn paragraph(lines: &[&str], first: usize, last: usize) -> (usize, usize, Option<String>) {
    (
        first + 1,
        last + 1,
        first_sentence(&lines[first..=last].join("\n")),
    )
}

/// Swap the standing-notes section inside a composed system prompt. Idempotent and
/// position-independent: an existing section is replaced in place wherever its markers sit, `None`
/// removes it, and everything outside the markers is carried through byte for byte.
pub fn replace(system: &str, section: Option<&str>) -> String {
    match (system.find(BEGIN), system.find(END)) {
        (Some(a), Some(b)) if b >= a => {
            let after = &system[b + END.len()..];
            let mut out = String::with_capacity(system.len());
            out.push_str(&system[..a]);
            if let Some(s) = section {
                out.push_str(s);
            }
            // Removing a section that was glued with "\n\n" must not leave a doubled blank line -
            // the glue that joined the old section to its neighbours is spent with the section.
            let rest = after.trim_start_matches('\n');
            if section.is_none() {
                while out.ends_with('\n') {
                    out.pop();
                }
            }
            if !rest.is_empty() {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(rest);
            }
            out
        }
        _ => match section {
            Some(s) => {
                if system.is_empty() {
                    s.to_string()
                } else {
                    format!("{system}\n\n{s}")
                }
            }
            None => system.to_string(),
        },
    }
}

/// The standing-notes section a composed system prompt currently carries
/// markers included - `None` when it carries none. What the model was last
/// given, read back off the prompt itself rather than remembered beside it.
pub fn carried(system: &str) -> Option<&str> {
    let a = system.find(BEGIN)?;
    let b = system[a..].find(END)? + a + END.len();
    Some(&system[a..b])
}

/// The census, in its own file - these tests read this module's private rules (`gather`'s order
/// `body`'s per-file budget, `index_entry`'s shape). The integration half - the re-read at a base
/// rebuild, against a real harness - is `crates/harnessd/tests/standing_notes.rs`.
#[cfg(test)]
mod tests;
