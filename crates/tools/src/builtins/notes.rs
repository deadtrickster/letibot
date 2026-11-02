//! `notes` - read and write the standing notes the harness injects.
//! Writes go to the workspace's `.letibot/notes/` only, through a [`NotesScope`] the daemon
//! supplies and through the session's [`ExecBackend`], so a note lands where `write` lands; the
//! index's derivation and sort live here so both sides agree.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use letibot_transcript::text::clip_line_counted;
use serde_json::Value;

use crate::backend::{DirEntry, ExecBackend};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};
use crate::similarity;

/// Where the standing notes live, as the harness that injects them sees it: the
/// daemon owns the workspace and the config dir, and this crate holds only the trait
/// so that `letibot-tools` depends on nothing above it.
pub trait NotesScope: Send + Sync {
    /// The workspace root: `AGENTS.md` and `.letibot/notes/` are read from
    /// here, and `.letibot/notes/` is the one directory the tool writes.
    fn workspace(&self) -> PathBuf;
    /// The box-wide notes directory, beside `providers.toml` - read here,
    /// never written.
    fn global_dir(&self) -> PathBuf;
}

pub struct NotesTool {
    scope: Arc<dyn NotesScope>,
}

impl NotesTool {
    pub fn new(scope: Arc<dyn NotesScope>) -> Self {
        NotesTool { scope }
    }
}

/// One note file, as the listing that found it saw it: the path to open it by, and what `list`
/// reports about it - so neither verb asks the substrate for the same file twice.
struct Note {
    path: PathBuf,
    bytes: u64,
    modified: Option<u64>,
}

/// Where the tool's files are: the session's backend for anything under its workspace, so a note
/// lands where `write` lands, and this box's own filesystem for the box-wide directory, which the
/// harness reads here and the tool never writes.
struct NoteFs<'a> {
    backend: &'a dyn ExecBackend,
    /// The backend's workspace, which is where the session's files are. Not the scope's: that is
    /// the path the DAEMON sees, and on a placed session it names another machine's tree.
    root: PathBuf,
}

impl<'a> NoteFs<'a> {
    fn new(backend: &'a dyn ExecBackend, scope: &dyn NotesScope) -> Self {
        // A substrate with no host path (a VM holding a copy) has nothing to compare a path
        // against, so the scope's own workspace is what there is.
        let root = backend
            .workspace_path()
            .map(PathBuf::from)
            .unwrap_or_else(|| scope.workspace());
        NoteFs { backend, root }
    }

    /// The path in the backend's own spelling, when the backend is what holds it. That test is
    /// the whole rule: under the workspace is the session's, and everything else - the box-wide
    /// notes - is this box's.
    fn on_backend(&self, path: &Path) -> Option<String> {
        let rest = path.strip_prefix(&self.root).ok()?;
        let rel = rest.to_string_lossy().replace('\\', "/");
        Some(if rel.is_empty() { ".".to_string() } else { rel })
    }

    fn read(&self, path: &Path) -> Option<String> {
        match self.on_backend(path) {
            Some(rel) => String::from_utf8(self.backend.read(&rel).ok()?).ok(),
            None => std::fs::read_to_string(path).ok(),
        }
    }

    fn stat(&self, path: &Path) -> Option<DirEntry> {
        match self.on_backend(path) {
            Some(rel) => self.backend.stat(&rel),
            None => {
                let meta = std::fs::metadata(path).ok()?;
                Some(DirEntry {
                    path: path.display().to_string(),
                    name: path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    is_dir: meta.is_dir(),
                    bytes: meta.len(),
                    modified: modified_secs(&meta),
                })
            }
        }
    }

    fn list(&self, dir: &Path) -> Vec<DirEntry> {
        match self.on_backend(dir) {
            Some(rel) => self.backend.list(&rel).unwrap_or_default(),
            None => local_list(dir),
        }
    }

    /// `ExecBackend::write` is atomic by its contract and makes the parent directories; a path on
    /// this box keeps [`write_atomic`], which is those same rules with nobody to ask.
    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        match self.on_backend(path) {
            Some(rel) => self.backend.write(&rel, bytes).map_err(|e| e.to_string()),
            None => write_atomic(path, bytes).map_err(|e| e.to_string()),
        }
    }
}

/// A file's mtime in whole seconds - the unit [`DirEntry::modified`] carries, so a listing's age
/// and a stat's agree.
fn modified_secs(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// One directory on this box, in the shape a backend's `list` answers in and the order it
/// answers in, so a reader cannot tell which of the two it was given.
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
                modified: modified_secs(&meta),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// One source of notes, as `list` presents it and `read` resolves against it.
struct Source {
    /// The directory (or, for `AGENTS.md`, the file's parent) this source
    /// lives in.
    dir: PathBuf,
    /// `Some(file)` for the single-file source that is `AGENTS.md`.
    file: Option<PathBuf>,
    /// What `list` calls the source, and who writes it.
    label: &'static str,
}

impl Source {
    /// The `*.md` files this source contributes, newest first, in the reader's own order
    /// ([`sort_newest_first`]). A single-file source contributes its file only when it is
    /// there, because the reader skips an absent `AGENTS.md`.
    fn notes(&self, fs: &NoteFs<'_>) -> Vec<Note> {
        if let Some(f) = &self.file {
            return match fs.stat(f) {
                Some(e) if !e.is_dir => vec![Note {
                    path: f.clone(),
                    bytes: e.bytes,
                    modified: e.modified,
                }],
                _ => Vec::new(),
            };
        }
        let mut notes: Vec<Note> = fs
            .list(&self.dir)
            .into_iter()
            .filter(|e| !e.is_dir && Path::new(&e.name).extension().is_some_and(|x| x == "md"))
            .map(|e| Note {
                path: self.dir.join(&e.name),
                bytes: e.bytes,
                modified: e.modified,
            })
            .collect();
        // Newest first, ties by name: the reader's order, and the one `list` claims to report.
        // The time is the listing's, so a placed session pays no round trip per note for it.
        notes.sort_by(|a, b| {
            b.modified
                .cmp(&a.modified)
                .then_with(|| a.path.cmp(&b.path))
        });
        notes
    }
}

/// The three sources in the reader's order: `AGENTS.md`, then the box-wide notes, then
/// the project's. The order decides what arrives whole and what arrives as an index,
/// and it is what makes `list`'s "in prompt order" true.
fn sources(fs: &NoteFs<'_>, global: &Path) -> Vec<Source> {
    let ws = fs.root.clone();
    vec![
        Source {
            dir: ws.clone(),
            file: Some(ws.join("AGENTS.md")),
            label: "workspace instructions (the operator's)",
        },
        Source {
            dir: global.to_path_buf(),
            file: None,
            label: "box-wide (the operator's)",
        },
        Source {
            dir: ws.join(NOTES_DIR),
            file: None,
            label: "project notes (written with this tool, and by the operator)",
        },
    ]
}

/// Every note the harness injects, as `(path, text)`: the corpus a score is computed
/// over, in the same sources and order the injected section is assembled from, so no
/// score is ever about a note the reader would not inject.
fn corpus(fs: &NoteFs<'_>, global: &Path) -> Vec<(PathBuf, String)> {
    sources(fs, global)
        .iter()
        .flat_map(|s| s.notes(fs))
        .filter_map(|n| fs.read(&n.path).map(|text| (n.path, text)))
        .collect()
}

/// Newest first, ties by name: the order the reader assembles each notes directory in
/// and the order `list` reports. It lives beside the writer because the reader imports
/// it, and a file whose mtime cannot be read sorts last rather than out of the listing.
pub fn sort_newest_first(paths: &mut [PathBuf]) {
    let mtime = |p: &PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    paths.sort_by(|a, b| mtime(b).cmp(&mtime(a)).then_with(|| a.cmp(b)));
}

/// Where the project notes live under a workspace - the one directory this tool
/// writes, named once so the reader, the writer and the refusals that keep the
/// other doors out cannot come to disagree about it.
pub const NOTES_DIR: &str = ".letibot/notes";

/// The other doors into the notes directory: `write` and `edit` take a path, and a
/// path can spell `.letibot/notes/foo.md`, which lands a note nobody scored. Not a
/// [`crate::adjudicate::NEVER_WRITE`] row, because that would refuse this tool's own writes.
pub(crate) fn refuse_a_path_into_the_notes_dir(
    backend: &dyn crate::backend::ExecBackend,
    path: &str,
) -> Option<Invocation> {
    let relative = if path.starts_with('/') {
        // An absolute spelling of the same file. `workspace_path` is the cwd a
        // relative path starts from - not `root_path`, which for an unconfined
        // seat is `/` and would make the tail match mean nothing.
        let workspace = backend.workspace_path()?;
        let rest = path.strip_prefix(workspace.trim_end_matches('/'))?;
        rest.strip_prefix('/')?.to_string()
    } else {
        path.to_string()
    };
    let mut lead: Vec<&str> = Vec::new();
    for seg in relative.split('/') {
        match seg {
            "" | "." => {}
            // `..` is resolved rather than counted: `docs/../.letibot/notes/x.md`
            // is the same file, and a path that climbs out of the directory it
            // starts from cannot be this one.
            ".." => {
                lead.pop()?;
            }
            other => lead.push(other),
        }
    }
    // Three components or more: the directory itself gets the backend's own "is a
    // directory" refusal, and this rule is about a file inside the tool's directory.
    if lead.len() < 3 || !lead.starts_with(&NOTES_DIR.split('/').collect::<Vec<_>>()) {
        return None;
    }
    Some(Invocation::failed(
        format!("`{path}` is inside `{NOTES_DIR}/`, which the `notes` tool owns"),
        format!(
            "Nothing was written. A note put there by `write` or `edit` goes around the 
             similarity gate those two verbs do not carry — the gate that refuses a note 
             which would land as a near-copy of one that already exists — and the reader 
             injects what lands exactly as it injects the notes that were scored. So this 
             is not a file to write: it is a note. Call `notes` with a bare `name` (not a 
             path) and `action` = \"add\" to create one, \"append\" to grow one that 
             exists, or \"replace\" to rewrite one — it writes into `{NOTES_DIR}/` itself, 
             and it refuses a near-copy rather than letting the corpus fill with them."
        ),
    ))
}

/// A note NAME as this tool accepts it: a file stem, no path in it, `.md` stripped
/// because `add foo` and `add foo.md` are the same note. A separator or a dot prefix is
/// a path wearing a name, and the dot also keeps notes off the tool's own temporaries.
fn note_stem(raw: &str) -> Result<String, String> {
    let mut s = raw.trim();
    if s.len() >= 3 && s[s.len() - 3..].eq_ignore_ascii_case(".md") {
        s = s[..s.len() - 3].trim();
    }
    if s.is_empty() {
        return Err("a note needs a name".into());
    }
    if s.contains('/') || s.contains('\\') || s == "." || s == ".." {
        return Err(format!(
            "`{raw}` names a path, and this tool's writes go to the project notes directory \
             only — never `AGENTS.md`, never the box-wide notes. Give a bare name.",
        ));
    }
    if s.starts_with('.') {
        return Err(format!(
            "`{raw}` starts with a dot: a hidden note is a note a listing reads as absent. \
             Give a bare name.",
        ));
    }
    if s.chars().any(|c| c.is_control()) {
        return Err(format!("`{raw}` has a control character in it."));
    }
    if s.chars().count() > 100 {
        // R93.F42: this was a plain literal, so `{raw}` reached the model as written.
        return Err(format!(
            "that name is {} characters long; a note name is at most 100. Give a shorter one.",
            s.chars().count()
        ));
    }
    Ok(s.to_string())
}

/// How long ago `modified` was last written, in the word `ps` uses for ages  -
/// the date beside the caveat, in this tree's own idiom.
fn age_of(modified: Option<u64>) -> String {
    let Some(secs) = modified else {
        return "at an unknown time".into();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let d = std::time::Duration::from_secs(now.saturating_sub(secs));
    if d.as_secs() < 60 {
        "under a minute ago".into()
    } else {
        format!("{} ago", crate::exec::procs::age_word(d))
    }
}

/// The short caveat every read of a note carries: a note records what was true when it
/// was written, and without this line a stale one reads as a current fact.
fn caveat(modified: Option<u64>) -> String {
    format!(
        "[historical note — last written {}; it records what was true then and may be \
         outdated. Verify against the tree before relying on it.]",
        age_of(modified)
    )
}

/// The one-line frame for what a write means: when the harness will read the
/// note back. A note that never lands in a prompt is a file, not a note.
const WHEN_READ: &str = "The harness reads notes into the prompt at session open, at every \
                         base rebuild (a compaction counts), and every turn on a provider \
                         session.";

/// The marker line an author's abstract rides on, at the top of a note:
///
/// ```text
/// <!-- abstract: one line saying what this note is about -->
///
/// the note's own text follows
/// ```
///
/// An HTML comment rather than a heading or front matter: the file is injected
/// verbatim when it is under the budget, so a marker that rendered as markdown would
/// put a line the author did not write into the text the model reads.
pub const ABSTRACT_MARKER: &str = "<!-- abstract:";

/// How long an abstract may be in the index, in characters: one line of a block that
/// has to fit a budget. Over it the abstract is cut at a character boundary and says so
/// with an ellipsis.
pub const ABSTRACT_CHARS: usize = 240;

/// How many matching lines one `search` prints before it stops: a common word can match
/// a thousand, and an answer that floods the turn costs the reader a turn. Past the cap
/// the reply names the count it left out, so the caller can narrow.
const MAX_SEARCH_LINES: usize = 200;

/// How alike two notes have to be before the second one is refused: a floor, because a
/// refusal has to be arguable, and the refusal names the note that was too close. It
/// errs toward refusing - one refused write costs less than a corpus of near-copies.
pub const DUPLICATE_FLOOR: f64 = 0.30;

/// A note's abstract: the line the index shows for it, and where that line came
/// from.
pub struct Abstract {
    /// The line itself - one line, capped at [`ABSTRACT_CHARS`].
    pub text: String,
    /// `true` for the author's own marker line, `false` for the derived first sentence: a
    /// reader that cannot tell them apart takes a guess for a statement.
    pub written: bool,
}

/// The abstract the index shows for a note: the author's marker line when the
/// file carries one, the first proper sentence of its prose otherwise. `None`
/// only for a file with no prose at all - one that is nothing but headings.
pub fn abstract_of(text: &str) -> Option<Abstract> {
    if let Some(written) = written_abstract(text) {
        return Some(Abstract {
            text: cap(&written),
            written: true,
        });
    }
    first_sentence(text).map(|text| Abstract {
        text,
        written: false,
    })
}

/// The first proper sentence of a note's prose, capped at [`ABSTRACT_CHARS`], or `None`
/// when the text holds no prose. A period ends a sentence only when whitespace or the
/// end of the text follows it, so `2.7` and `notes.md` do not cut a lead short.
pub fn first_sentence(text: &str) -> Option<String> {
    let prose = prose_lines(text);
    if prose.is_empty() {
        return None;
    }
    let joined = prose.join(" ");
    let end = joined
        .char_indices()
        .find(|(i, c)| *c == '.' && ends_a_sentence(&joined, *i))
        .map(|(i, _)| i + 1)
        .unwrap_or(joined.len());
    Some(cap(joined[..end].trim_end()))
}

/// Whether the period at byte `at` ends a sentence: followed by whitespace or the end of
/// the text, and not inside a number or a filename. A filename joined across a line
/// break cannot be told from a sentence end, and the join is the caller's.
fn ends_a_sentence(text: &str, at: usize) -> bool {
    match text[at + 1..].chars().next() {
        None => true,
        Some(c) => c.is_whitespace(),
    }
}

/// The author's abstract, as the marker line carries it - `None` when the file
/// has no marker, or has one with nothing after it.
fn written_abstract(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix(ABSTRACT_MARKER))
        .map(|rest| rest.trim().trim_end_matches("-->").trim().to_string())
        .filter(|a| !a.is_empty())
}

/// The note's own text with its marker line taken out - what an `append` grows,
/// and what keeps a rewrite from leaving two abstracts in one file.
fn without_abstract(text: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut dropped = false;
    for line in text.lines() {
        if !dropped && line.trim().starts_with(ABSTRACT_MARKER) {
            dropped = true;
            continue;
        }
        kept.push(line);
    }
    kept.join("\n").trim().to_string()
}

/// The file's bytes: the abstract line when there is one to carry, then the
/// body - the marker never doubled, and never added when there is none.
fn with_abstract(abstract_line: Option<&str>, body: &str) -> String {
    match abstract_line {
        Some(a) => format!("{ABSTRACT_MARKER} {a} -->\n\n{body}"),
        None => body.to_string(),
    }
}

/// One line, from whatever the caller passed: an abstract is the index's line,
/// so its newlines and runs of spaces are flattened rather than refused - the
/// note's own text is where a paragraph belongs.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A note's prose, line by line: a heading, a blank line and the abstract marker
/// carry none, and a list marker is stripped so a bullet's words are prose rather
/// than `- `.
fn prose_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with(ABSTRACT_MARKER))
        .map(strip_list_marker)
        .filter(|l| !l.is_empty())
        .collect()
}

/// A list marker off the front of a line: `- `, `* `, `+ `, `1. `, `2) ` or a bare `1.`, so a
/// bulleted note's abstract is its own words and a numbered list's `1.` is never a first
/// sentence. The space is required: `2.7 s` is a number, not item two.
fn strip_list_marker(line: &str) -> String {
    if let Some(rest) = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("+ "))
    {
        return rest.trim_start().to_string();
    }
    let digits: String = line.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        let after = &line[digits.len()..];
        if let Some(rest) = after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))
        {
            return rest.trim_start().to_string();
        }
        if after == "." || after == ")" {
            return String::new();
        }
    }
    line.to_string()
}

/// Cut to [`ABSTRACT_CHARS`] at a character boundary, with an explicit ellipsis  -
/// an abstract that simply stops reads as a fact about the note.
fn cap(s: &str) -> String {
    if s.chars().count() <= ABSTRACT_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(ABSTRACT_CHARS - 1).collect();
    out.push('…');
    out
}

impl Tool for NotesTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "notes",
            "The standing notes the harness reads into the system prompt and re-reads after \
             every compaction. `action`: `list` (every note, with path, size and when last \
             written), `read` (one note's text — give `name` for a project note or `path` as \
             `list` reported it), `search` (a `query`, answered with the paths, line numbers \
             and matching lines of every note that holds it — local and case-insensitive, \
             and the door to use when the index in the prompt is not enough to tell you \
             which note you want), `add` (create one), `append` (add to one that exists), \
             `replace` (rewrite one that exists, reporting what it replaced). Writes go to \
             the workspace's `.letibot/notes/` only, by bare `name` — `AGENTS.md` and the \
             box-wide notes are the operator's. A write that would land as a near-copy of a \
             note that already exists is refused and names it: grow that note instead, or say \
             what distinguishes this one. Write down what is worth keeping: something \
             interesting, remarkable or surprising you would not want to rediscover — not a \
             summary of what happened.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["list", "read", "search", "add", "append", "replace"]},
                    "name": {"type": "string", "description": "A note's name — the file stem, no path. For `add`/`append`/`replace` it is where the note lives; for `read` it resolves in the project notes first."},
                    "path": {"type": "string", "description": "For `read`: a note's path exactly as `list` reported it, when what you have is a path rather than a name."},
                    "query": {"type": "string", "description": "For `search`: what to look for, matched case-insensitively against every line of every note."},
                    "abstract": {"type": "string", "description": "For `add`/`append`/`replace`: one line saying what the note is about, shown first in the index the harness builds for a note that is too long to inject whole. Written into the note above its text; newlines are flattened to spaces. Without it the harness derives the note's first sentence."},
                    "text": {"type": "string", "description": "For `add`/`append`/`replace`: the note's text — for `replace`, the whole new text."}
                },
                "required": ["action"]
            }),
            Access::Write,
        )
    }

    /// `list`, `read` and `search` only ever read, and a read that reaches the
    /// gate is a question the operator is asked about a fact. Narrowing only, per
    /// the trait: the schema's `Write` stands for every verb that writes.
    fn access_for(&self, args: &Value) -> Option<Access> {
        match args.get("action").and_then(|v| v.as_str()) {
            Some("list") | Some("read") | Some("search") => Some(Access::Read),
            _ => None,
        }
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(action) = args.get("action").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "notes needs an action",
                "call `notes` with `action` = \"list\", \"read\", \"search\", \"add\", \"append\" or \
                 \"replace\".",
            );
        };
        let fs = NoteFs::new(ctx.backend, self.scope.as_ref());
        match action {
            "list" => self.list(&fs),
            "read" => self.read(
                &fs,
                args.get("path").and_then(|v| v.as_str()),
                args.get("name").and_then(|v| v.as_str()),
            ),
            "search" => self.search(&fs, args.get("query").and_then(|v| v.as_str())),
            "add" | "append" | "replace" => self.write(ctx, &fs, action, args),
            other => Invocation::failed(
                format!("unknown notes action `{other}`"),
                "call `notes` with `action` = \"list\", \"read\", \"search\", \"add\", \"append\" or \
                 \"replace\".",
            ),
        }
    }
}

impl NotesTool {
    /// The gate's question about the bytes that are about to become durable: `after` is the
    /// whole file as it would land, because half a note has no score, and `target` is left
    /// out of its own corpus so an `append` is never refused for resembling what it grows.
    fn duplicate(&self, fs: &NoteFs<'_>, target: &Path, after: &str) -> Option<(PathBuf, f64)> {
        let corpus = similarity::Corpus::new(&corpus(fs, &self.scope.global_dir()));
        if corpus.is_empty() {
            return None;
        }
        let candidate = similarity::index_text(target, after);
        corpus
            .nearest(&candidate, Some(target))
            .filter(|(_, score)| *score >= DUPLICATE_FLOOR)
            .map(|(path, score)| (path.to_path_buf(), score))
    }

    /// Every note the harness reads, in the order the injected block carries
    /// them - the model's map for `read`, and the honest report of a source
    /// that is empty or missing rather than silence about it.
    fn list(&self, fs: &NoteFs<'_>) -> Invocation {
        let mut out = String::from("the standing notes the harness reads, in prompt order:\n");
        let mut total = 0usize;
        for src in sources(fs, &self.scope.global_dir()) {
            let notes = src.notes(fs);
            if notes.is_empty() {
                out.push_str(&format!(
                    "\n{} — none ({}).\n",
                    src.label,
                    if src.file.is_some() {
                        "no AGENTS.md in this workspace".to_string()
                    } else {
                        format!("nothing readable in {}", src.dir.display())
                    }
                ));
                continue;
            }
            total += notes.len();
            out.push_str(&format!("\n{}:\n", src.label));
            for n in notes {
                out.push_str(&format!(
                    "- {} — {}, last written {}\n",
                    n.path.display(),
                    super::human(n.bytes),
                    age_of(n.modified)
                ));
            }
        }
        out.push_str(&format!(
            "\n{total} note(s). A note records what was true when it was written and may be \
             outdated — check the tree before relying on one as a fact about now.\n"
        ));
        Invocation::ok(out)
    }

    /// One note's text with the caveat line above it. A bare name looks in the project notes
    /// first (where `add` writes, so the two agree), then the box-wide notes, then
    /// `AGENTS.md`; a path matches exactly or by its trailing components.
    fn read(&self, fs: &NoteFs<'_>, path: Option<&str>, name: Option<&str>) -> Invocation {
        let all: Vec<Note> = sources(fs, &self.scope.global_dir())
            .iter()
            .flat_map(|s| s.notes(fs))
            .collect();
        let found: Vec<&Note> = if let Some(name) = name {
            let stem = name.trim().trim_end_matches(".md");
            all.iter()
                .filter(|n| {
                    n.path.file_stem().is_some_and(|s| {
                        s == stem || s.to_string_lossy().eq_ignore_ascii_case(stem)
                    })
                })
                .collect()
        } else if let Some(path) = path {
            let want = path.trim().trim_start_matches("./");
            all.iter()
                .filter(|n| {
                    let have = n.path.display().to_string();
                    if want.starts_with('/') {
                        have == want
                    } else {
                        // A relative path matches by its trailing components, so a bare `foo.md` and
                        // `.letibot/notes/foo.md` find one note.
                        have == want || have.ends_with(&format!("/{want}"))
                    }
                })
                .collect()
        } else {
            return Invocation::failed(
                "notes read needs a name or a path",
                "call `notes` with `action` = \"read\" and either `name` (a project note's \
                 name) or `path` (as `list` reported it).",
            );
        };
        match found.as_slice() {
            [one] => match fs.read(&one.path) {
                Some(text) => Invocation::ok(format!(
                    "### {} — {}\n\n{}\n\n{}\n",
                    one.path.display(),
                    match one.modified {
                        Some(_) => format!(
                            "{}, last written {}",
                            super::human(one.bytes),
                            age_of(one.modified)
                        ),
                        // The age is the caveat's evidence; without it the line still says what
                        // a note is, and the header says why the date is missing rather than
                        // pretending to one.
                        None => format!(
                            "{} (when last written is unreadable)",
                            super::human(one.bytes)
                        ),
                    },
                    caveat(one.modified),
                    text.trim_end()
                )),
                None => Invocation::failed(
                    format!("`{}` could not be read", one.path.display()),
                    "it was listed a moment ago, so it moved or its permissions changed. \
                     `notes` action=\"list\" re-reads the directory.",
                ),
            },
            [] => {
                let mut tell = String::from(
                    "no note matches. The notes that exist, in prompt \
                     order:\n",
                );
                for n in &all {
                    tell.push_str(&format!("- {}\n", n.path.display()));
                }
                if all.is_empty() {
                    tell.push_str("(none — no source has a note right now)\n");
                }
                Invocation::failed("no such note", tell)
            }
            many => {
                let tell: String = many
                    .iter()
                    .map(|n| format!("- {}\n", n.path.display()))
                    .collect();
                Invocation::failed(
                    "that name matches more than one note",
                    format!("give the full path instead. Matches:\n{tell}"),
                )
            }
        }
    }

    /// Every note's own lines, by the words in them: a local substring match over the whole
    /// corpus, so a note stays findable when its index entry is a poor summary. It answers
    /// in `grep`'s register, and no match is an abstention that lists the notes there are.
    fn search(&self, fs: &NoteFs<'_>, query: Option<&str>) -> Invocation {
        let Some(query) = query.map(str::trim).filter(|q| !q.is_empty()) else {
            return Invocation::failed(
                "notes search needs a query",
                "call `notes` with `action` = \"search\" and `query` set to the words to look \
                 for. It is matched case-insensitively against every line of every note the \
                 harness reads.",
            );
        };
        let needle = query.to_lowercase();
        let all: Vec<Note> = sources(fs, &self.scope.global_dir())
            .iter()
            .flat_map(|s| s.notes(fs))
            .collect();
        let mut out = String::new();
        let mut found = 0usize;
        let mut printed = 0usize;
        let mut notes_hit = 0usize;
        for note in &all {
            // A note that cannot be read is not a note that does not match: the
            // corpus this answers over is the one that could be read, and the
            // count of what was searched is what the answer is worth.
            let Some(text) = fs.read(&note.path) else {
                continue;
            };
            let hits: Vec<(usize, &str)> = text
                .lines()
                .enumerate()
                .filter(|(_, l)| l.to_lowercase().contains(&needle))
                .map(|(i, l)| (i + 1, l))
                .collect();
            if hits.is_empty() {
                continue;
            }
            notes_hit += 1;
            out.push_str(&format!("\n### {}\n", note.path.display()));
            for (n, line) in hits {
                found += 1;
                if printed < MAX_SEARCH_LINES {
                    printed += 1;
                    out.push_str(&format!("  {n}: {}\n", clip_line_counted(line, 400)));
                }
            }
        }
        if found == 0 {
            let mut tell = format!(
                "no line of any note matches `{query}` — the search is case-insensitive and \
                 line-by-line, over the whole of every note the harness reads, and {} note(s) \
                 were read. The notes that exist:\n",
                all.len()
            );
            if all.is_empty() {
                tell.push_str("(none — no source has a note right now)\n");
            }
            for n in &all {
                tell.push_str(&format!(
                    "- {} — {}, last written {}\n",
                    n.path.display(),
                    super::human(n.bytes),
                    age_of(n.modified)
                ));
            }
            tell.push_str(
                "\nA note records what was true when it was written and may be outdated — \
                 check the tree before relying on one as a fact about now.\n",
            );
            return Invocation::abstained(format!("`{query}` is in no note"), tell);
        }
        let mut answer = format!(
            "{found} line(s) in {notes_hit} note(s) match `{query}` (case-insensitive, every \
             note the harness reads):\n{out}"
        );
        if found > printed {
            answer.push_str(&format!(
                "\n… {} more matching line(s) not shown; narrow the query to see them.\n",
                found - printed
            ));
        }
        answer.push_str(
            "\nA note records what was true when it was written and may be outdated — check \
             the tree before relying on one as a fact about now. `read` a path with a range for \
             the lines around a match.\n",
        );
        Invocation::ok(answer)
    }

    /// `add`, `append`, `replace` - the three write verbs, each refusing the
    /// case it must not do silently: `add` will not overwrite, `append` and
    /// `replace` will not create, and `replace` says what it threw away.
    fn write(
        &self,
        ctx: &mut InvokeCtx<'_>,
        fs: &NoteFs<'_>,
        action: &str,
        args: &Value,
    ) -> Invocation {
        let Some(raw_name) = args.get("name").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                format!("notes {action} needs a name"),
                format!(
                    "call `notes` with `action` = \"{action}\", `name` (a bare name — the \
                     note's stem) and `text`."
                ),
            );
        };
        let Some(text) = args.get("text").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                format!("notes {action} needs `text`"),
                "an absent `text` cannot mean an empty note — that has to be said. Nothing \
                 was written.",
            );
        };
        let stem = match note_stem(raw_name) {
            Ok(s) => s,
            Err(why) => {
                return Invocation::failed(
                    why.clone(),
                    // The payload repeats the reason and adds the one fact
                    // every write refusal owes: that nothing happened.
                    format!("{why} Nothing was written."),
                );
            }
        };
        // The second gate, named: the adjudicator may have admitted this call,
        // and a backend opened read-only is what refuses. Writing around it
        // would be a tool with its own boundary policy.
        if !ctx.backend.is_writable() {
            return Invocation::failed(
                "this session's backend was opened read-only",
                format!(
                    "that is a second, independent gate below the adjudication one, and it \
                     applies to standing notes as it does to `write` and `edit` ({}). \
                     Nothing on disk changed.",
                    ctx.backend.describe()
                ),
            );
        }
        let dir = fs.root.join(NOTES_DIR);
        let target = dir.join(format!("{stem}.md"));
        let existing = fs.read(&target);
        // The abstract is the author's to set and the one part of a note the text cannot be
        // asked for: absent, the harness derives it ([`abstract_of`]); supplied, it goes in as
        // the marker line above the text.
        let asked = args.get("abstract").and_then(|v| v.as_str()).map(one_line);
        let (before, after, said) = match (action, &existing) {
            ("add", Some(_)) => {
                let meta = fs.stat(&target);
                return Invocation::failed(
                    format!("a note named `{stem}` already exists"),
                    format!(
                        "it is {} — {}, last written {}. Nothing was written. Append to it \
                         with `action` = \"append\", or rewrite it deliberately with \
                         `action` = \"replace\", which reports what it replaced.",
                        target.display(),
                        meta.as_ref()
                            .map(|m| super::human(m.bytes))
                            .unwrap_or_else(|| "?".into()),
                        age_of(meta.as_ref().and_then(|m| m.modified)),
                    ),
                );
            }
            ("add", None) => (
                String::new(),
                format!("{}\n", with_abstract(asked.as_deref(), text.trim_end())),
                format!("created `{}`", target.display()),
            ),
            (_, None) => {
                // `append` and `replace` both refuse to create: a typo'd name
                // that silently grows a new note is a note nobody meant, and
                // the refusal is where the near-miss belongs.
                let near = sources(fs, &self.scope.global_dir())[2]
                    .notes(fs)
                    .iter()
                    .map(|n| {
                        n.path
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default()
                    })
                    .collect::<Vec<_>>();
                let near_names = super::near_names(
                    &stem,
                    &near
                        .iter()
                        .map(|n| crate::backend::DirEntry {
                            path: format!("{n}.md"),
                            name: format!("{n}.md"),
                            is_dir: false,
                            bytes: 0,
                            modified: None,
                        })
                        .collect::<Vec<_>>(),
                    5,
                );
                return Invocation::failed(
                    format!("no note named `{stem}` to {action}"),
                    format!(
                        "{} a note is how a typo becomes a note nobody meant. The project \
                         notes:{} — create it with `action` = \"add\" if that was the intent.",
                        if action == "append" {
                            "appending to a missing"
                        } else {
                            "replacing a missing"
                        },
                        if near.is_empty() {
                            " (none exist yet)".to_string()
                        } else if near_names.is_empty() {
                            format!(" {}", near.join(", "))
                        } else {
                            format!(" {} — closest: {}", near.join(", "), near_names.join(", "))
                        }
                    ),
                );
            }
            ("append", Some(old)) => {
                // The marker is lifted out, the note grown, and the abstract - the new one or the
                // one that was there - put back on top, so an `append` that asked for none keeps
                // the line the index leads with.
                let was = written_abstract(old);
                let kept = asked.as_deref().or(was.as_deref());
                let old_body = without_abstract(old);
                let body = if old_body.is_empty() {
                    text.trim().to_string()
                } else {
                    format!("{old_body}\n\n{}", text.trim())
                };
                let grown = with_abstract(kept, &body);
                let said = format!(
                    "appended {} to `{}`: {} → {}",
                    super::human(text.trim().len() as u64),
                    target.display(),
                    super::human(old.len() as u64),
                    super::human(grown.len() as u64),
                );
                (old.clone(), format!("{grown}\n"), said)
            }
            ("replace", Some(old)) => {
                let said = format!(
                    "replaced `{}` — was {}, {} line(s), first line {:?}. Nothing of the old \
                     text survives; `transcript` holds this call if it must be recovered.",
                    target.display(),
                    super::human(old.len() as u64),
                    old.lines().count(),
                    old.lines().next().unwrap_or_default(),
                );
                (
                    old.clone(),
                    format!("{}\n", with_abstract(asked.as_deref(), text.trim_end())),
                    said,
                )
            }
            _ => unreachable!("the match on action is total above"),
        };
        if before == after {
            return Invocation::ok(format!(
                "nothing to do: `{stem}` already holds exactly that text.\n\n{WHEN_READ}\n"
            ));
        }
        // The similarity gate on both write triggers, after the bytes are computed and before
        // they are durable: a near-copy is a defect wherever it arrives from, the `add` that
        // creates it and the `append`/`replace` that grows one into it.
        if let Some((other, score)) = self.duplicate(fs, &target, &after) {
            return Invocation::failed(
                format!(
                    "`{stem}` would be a near-copy of `{}` (score {score:.2} of 1.00)",
                    other.display()
                ),
                format!(
                    "Nothing was written. The score is the same lexical measure the read hint \
                     uses — IDF-weighted cosine over each note's abstract, title and \
                     headings — and {:.2} is at or over the {DUPLICATE_FLOOR:.2} floor. The \
                     corpus is curated on purpose: every note in it is one somebody decided was \
                     worth keeping, so a near-copy makes the index worse for every reader of \
                     it. What to do instead: grow the note that exists (`action` = \"append\", \
                     `name` = {:?}), or rewrite it deliberately (`action` = \"replace\"), or — if \
                     this really is a different thing — say what distinguishes it in the \
                     abstract and the headings, because those are the fields the score reads. \
                     The nearest note is the one named above; `notes` action=\"read\" opens it.",
                    score,
                    other
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default()
                ),
            );
        }
        if let Err(e) = fs.write(&target, after.as_bytes()) {
            return Invocation::failed(
                format!("could not write `{}`: {e}", target.display()),
                "the note is unchanged — the write is a temporary file renamed over the \
                 target, so a failure leaves the original untouched.",
            );
        }
        let mut inv = Invocation::ok(format!(
            "{said}: {}.\n{}\n\n{WHEN_READ}\n",
            super::human(after.len() as u64),
            // What the index will lead with, said while the author can still change it: their
            // line, the first sentence, or the fact that there is no prose. It uses the reader's
            // own function, so it cannot describe an index the reader would not build.
            match abstract_of(&after) {
                Some(a) if a.written => {
                    format!("The index will show your abstract: {}", a.text)
                }
                Some(a) => format!(
                    "The index will show the note's first sentence: {} — pass `abstract` to \
                     choose that line yourself.",
                    a.text
                ),
                None => "The note has no prose for the index to summarise; pass `abstract` to \
                         say what it is about."
                    .to_string(),
            }
        ));
        // The digests and the span before the strings move into the pair - a
        // diff card whose digest disagrees with its own before-side is worse
        // than no card.
        let (before_digest, after_digest) = (
            crate::spill::content_hash(before.as_bytes()),
            crate::spill::content_hash(after.as_bytes()),
        );
        let changed = crate::edit::changed_span(&before, &after);
        inv.edit = Some(crate::edit::FileEdit {
            path: target
                .strip_prefix(&fs.root)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| target.display().to_string()),
            before,
            after,
            created: action == "add",
            before_digest,
            after_digest,
            replacements: 1,
            changed,
        });
        inv
    }
}

/// Atomic write, by the rules `HostBackend::write` keeps: a temporary in the target's
/// own directory (0600, so nothing reads a half-written note), synced, then renamed over
/// the target. A failure removes the temporary and leaves the original alone.
fn write_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let Some(dir) = target.parent() else {
        return Err(std::io::Error::other("the target has no parent directory"));
    };
    std::fs::create_dir_all(dir)?;
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "note".into());
    static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = dir.join(format!(
        ".{name}.letibot-{}-{}.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let write_it = || -> std::io::Result<()> {
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(target)
                .map(|m| m.permissions().mode())
                .unwrap_or(0o644);
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
        }
        std::fs::rename(&tmp, target)?;
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    };
    match write_it() {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests;
