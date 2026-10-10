//! `notes` — read and write the standing notes the harness injects.
//!
//! Standing notes landed (`fb96763`) as a one-way loop: the operator wrote
//! markdown files, the harness read them into the prompt at session open and
//! every base rebuild, the model read them. The operator's ruling, 2026-10-09,
//! opened the other half: *"i think both. and not only 'survive past a
//! compaction' but also 'find interesting, remarkable or surprising, something
//! you dont want to rediscover.' The read tool should carry a short note - 'it
//! is a historical note, might be outdated'"*.
//!
//! # The write scope — and why it is as narrow as it is
//!
//! The reader (`crates/harnessd/src/standing_notes`) reads three sources: the
//! config dir's `notes/`, the workspace's `AGENTS.md`, and the workspace's
//! `.letibot/notes/`. This tool writes **the third one only**. A tool that
//! could rewrite the other two could destroy the operator's own instructions,
//! and — worse — could write a note that reads as one: the injected block is
//! the place the model looks for what the operator wants, and authorship that
//! cannot be told apart is how a note the model wrote comes to read as an
//! instruction the operator gave. The directory line is the tell, and it is
//! kept honest from both sides: the verbs take a bare NAME (never a path), so
//! there is no spelling of a write that leaves `.letibot/notes/`, and the
//! injected block's own opening says which sources are whose.
//!
//! # The read caveat
//!
//! Every `read` result carries a short line above the body: the note is
//! historical, and when it was last written. A note is a record of what was
//! true when someone wrote it, and the failure the line prevents is a stale
//! note read as a current fact — the exact thing that makes a notes feature
//! dangerous rather than useful. "May be outdated" is much stronger with the
//! age beside it, so the mtime rides along wherever the caveat does.
//!
//! # The abstract — the one line the index shows for a note
//!
//! The injected block is verbatim up to a budget and an index above it, and the
//! first line of each note's index entry is its **abstract**: the first *proper*
//! sentence of the note's prose, or the author's own line when the file carries
//! one (`<!-- abstract: … -->` at the top, above the text).
//!
//! Both halves live here rather than in the reader, for one reason: this tool is
//! what tells an author what the index will say, and a report computed by a
//! second implementation is a report that can disagree with the index it
//! describes. The reader (`letibot_harnessd::standing_notes`) imports
//! [`abstract_of`], [`first_sentence`] and [`sort_newest_first`] for exactly that
//! reason — the derivation, the cap and the order are stated once, here, and used
//! by both.
//!
//! `add`, `append` and `replace` take an optional `abstract`. Supplied, it is
//! written into the note as the marker line; supplied to `append`, it replaces
//! the one that was there. Absent, nothing is written and the reader derives the
//! abstract — the derivation is deterministic, so a note carrying a copy of it
//! would be a note that can drift from the text under it, and the duplication is
//! what an author would see first.
//!
//! # The similarity gate on a write, and why it is here rather than at creation
//!
//! A note that is a near-copy of one that exists makes the index worse for every
//! reader of it, and the corpus only grows. So `add`, `append` and `replace` all
//! ask [`crate::similarity`] the same question before the bytes become durable —
//! [`DUPLICATE_FLOOR`] is the answer that refuses — and the refusal names the
//! note that was too close, because *"that name is taken"* is only half an
//! answer.
//!
//! **Both triggers, and the operator said so twice:** *"since we ride similarity
//! score - worth having it as a gate for new notes"*, and then *"add this
//! new-note similarity check. add it for 'just before edit made durable' too. we
//! dont want to endup with n almost identical notes by edits anyway"*. A defect
//! that arrives by a second door is the same defect, so the check is not at
//! creation: it is at `write_atomic`, where every one of the three verbs arrives,
//! and the note being edited is excluded from its own corpus so an `append` is
//! never refused for resembling the note it grows.
//!
//! **What it scores** is the same three fields the index shows — title, abstract,
//! headings — and never a note's body, so an author can move a note out of the
//! gate's way by saying what distinguishes it, which is the act the refusal is
//! asking for.
//!
//! **And the door it does not close.** `write` and `edit` can put a file into
//! `.letibot/notes/` without passing through here, and nothing in this tool can
//! see that. Closing it needs the same [`NotesScope`] seam threaded into those
//! two tools, which are unit structs today (`crates/tools/src/builtins/edit.rs`),
//! so it is named here rather than pretended about. The gate is on the documented
//! door: the one the schema describes, the one `list` reports, and the one a
//! session reaches for when it means *write a note*.
//!
//! # The search door
//! `search` answers a query with paths, line numbers and the matching lines —
//! local, no model, no network, and deliberately useful *without* the index being
//! any good: the index names a note and its headings, and the note whose abstract
//! is a poor summary of what it holds is still findable by its own words. That is
//! the case the door exists for.
//!
//! # The seam
//!
//! `letibot-tools` cannot know where the workspace or the config dir are, so
//! the tool holds a [`NotesScope`] the daemon supplies — the same shape as
//! `HarnessFacts` and `TranscriptSource`. Reads go through that scope with
//! `std::fs` rather than through the session backend, deliberately: the reader
//! the notes must round-trip with is host-side, the box-wide notes dir is
//! outside every session's backend view (the file `read` tool cannot reach it
//! at all — reaching it is half of what this tool is for), and a session
//! placed in a firecode VM must still land its notes host-side or the harness
//! would never inject them. Writes honour the one backend fact that matters —
//! [`crate::backend::ExecBackend::is_writable`] — so a session whose view was
//! opened read-only refuses here as it would at the file tools, naming both
//! gates, rather than writing around its own boundary.
//!
//! Writes are atomic by the same rules `HostBackend::write` keeps — a
//! temporary file in the target directory, synced, then renamed over — because
//! a half-written note is a note the next session injects.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};
use crate::similarity;

/// Where the standing notes live, as the harness that injects them sees it.
/// Supplied by the daemon, which owns the workspace and the config dir; this
/// crate holds only the trait so that `letibot-tools` keeps depending on
/// nothing above it.
pub trait NotesScope: Send + Sync {
    /// The workspace root: `AGENTS.md` and `.letibot/notes/` are read from
    /// here, and `.letibot/notes/` is the one directory the tool writes.
    fn workspace(&self) -> PathBuf;
    /// The box-wide notes directory, beside `providers.toml` — read here,
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
    /// The `*.md` files this source contributes, **newest first** — the same
    /// order the reader assembles in ([`sort_newest_first`]), so the tool's
    /// listing and the injected block agree about what follows what. A
    /// single-file source (`AGENTS.md`) contributes its file only when the file
    /// is there: the reader skips an absent `AGENTS.md`, and a listing that
    /// named it would be a note that does not exist.
    fn notes(&self) -> Vec<PathBuf> {
        if let Some(f) = &self.file {
            return if f.is_file() {
                vec![f.clone()]
            } else {
                Vec::new()
            };
        }
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "md"))
            .collect();
        sort_newest_first(&mut paths);
        paths
    }
}

/// **The three sources, in the reader's order — a rule, not a preference.**
///
/// `AGENTS.md`, then the box-wide notes, then the project's: the order the
/// section is assembled in, and therefore the order that decides what arrives
/// whole and what arrives as an index (see `letibot_harnessd::standing_notes`,
/// where that rule is written down in full). `list` says it reports the notes
/// "in prompt order", and this is what makes that sentence true.
fn sources(scope: &dyn NotesScope) -> Vec<Source> {
    let ws = scope.workspace();
    vec![
        Source {
            dir: ws.clone(),
            file: Some(ws.join("AGENTS.md")),
            label: "workspace instructions (the operator's)",
        },
        Source {
            dir: scope.global_dir(),
            file: None,
            label: "box-wide (the operator's)",
        },
        Source {
            dir: ws.join(".letibot").join("notes"),
            file: None,
            label: "project notes (written with this tool, and by the operator)",
        },
    ]
}

/// Every note the harness injects, as `(path, text)` — the corpus a score is
/// computed over.
///
/// The same three sources and the same order the injected section is assembled
/// from, so the notes a score is over are exactly the notes a model can be told
/// about: a note the reader would not inject is not in the corpus, and a
/// similarity score is never about something unreachable.
///
/// Read here rather than through the session backend, for the reason
/// [`NotesScope`] gives: the box-wide notes are outside every session's backend
/// view, and the corpus the gate scores against must be the whole one.
fn corpus(scope: &dyn NotesScope) -> Vec<(PathBuf, String)> {
    sources(scope)
        .iter()
        .flat_map(|s| s.notes())
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|text| (p, text)))
        .collect()
}

/// **Newest first, ties by name** — the order the reader assembles each notes
/// directory in, and the order `list` reports, so the two cannot disagree about
/// what follows what. It lives beside the writer because the reader imports it;
/// a second sort one crate over is a sort that drifts.
///
/// A file whose mtime cannot be read sorts last, by name: an unreadable clock is
/// not a reason to drop a note out of the listing, and it is not a reason to put
/// it first either.
///
/// A sort and not `readdir`'s order, on the same principle the reader states: a
/// listing that varied with directory order is a listing whose consumer misses
/// its prefix cache for no reason.
pub fn sort_newest_first(paths: &mut [PathBuf]) {
    let mtime = |p: &PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    paths.sort_by(|a, b| mtime(b).cmp(&mtime(a)).then_with(|| a.cmp(b)));
}

/// A note NAME as this tool accepts it: a file stem, no path in it. `.md` is
/// stripped if the caller spelled the whole filename, because `add foo` and
/// `add foo.md` are the same note and the tool adds the extension itself.
///
/// `Err` is the refusal: a name with a separator or a dot-prefix is not a
/// name but a path wearing one, and writes go to `.letibot/notes/` only — the
/// dot-prefix rule also keeps a note from colliding with this tool's own
/// `.name.letibot-PID.tmp` temporaries.
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
        return Err("`{raw}` is too long for a filename (over 100 characters).".into());
    }
    Ok(s.to_string())
}

/// How long ago `meta` was last written, in the word `ps` uses for ages —
/// the date beside the caveat, in this tree's own idiom.
fn age_of(meta: &std::fs::Metadata) -> String {
    let Ok(mtime) = meta.modified() else {
        return "at an unknown time".into();
    };
    let d = std::time::SystemTime::now()
        .duration_since(mtime)
        .unwrap_or_default();
    if d.as_secs() < 60 {
        "under a minute ago".into()
    } else {
        format!("{} ago", crate::exec::procs::age_word(d))
    }
}

/// One line of a note, cut at a length a `search` answer can carry — the cap
/// `grep`'s `clip` keeps, and for the same reason: a minified note is one line
/// of forty kilobytes, and printing it whole turns a match into a page.
fn clip(line: &str) -> String {
    const CAP: usize = 400;
    if line.chars().count() <= CAP {
        return line.trim_end().to_string();
    }
    let head: String = line.chars().take(CAP).collect();
    format!(
        "{head}… (+{} more characters on this line)",
        line.chars().count() - CAP
    )
}

/// The short line every read of a note carries. Why it exists: a note is a
/// record of what was true when someone wrote it, and without this line a
/// stale note reads as a current fact — the one failure a notes feature must
/// prevent rather than merely survive.
fn caveat(meta: &std::fs::Metadata) -> String {
    format!(
        "[historical note — last written {}; it records what was true then and may be \
         outdated. Verify against the tree before relying on it.]",
        age_of(meta)
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
/// An HTML comment rather than a heading or front matter, for the reason that
/// decides it: the same file is injected VERBATIM when it is under the budget,
/// so a marker that rendered as markdown would put a line the author did not
/// write into the text the model reads. This one renders as nothing.
pub const ABSTRACT_MARKER: &str = "<!-- abstract:";

/// How long an abstract may be in the index, in characters.
///
/// A cap is not decoration: an abstract is one line of a block that has to fit a
/// budget, and the note that most needs one is the longest one. 240 characters is
/// a sentence and a half of this tree's prose — enough to say what a note is
/// about, short enough that two hundred of them are still an index. Over it the
/// abstract is cut at a character boundary and says so with an ellipsis.
pub const ABSTRACT_CHARS: usize = 240;

/// How many matching lines one `search` prints before it stops and says how
/// many it did not.
///
/// A cap and not a budget: the corpus is the operator's own notes and a common
/// word can match a thousand lines, and an answer that floods the turn is an
/// answer the model has to spend a turn reading around. Past the cap the reply
/// names the count it left out, so the caller can narrow rather than guess.
const MAX_SEARCH_LINES: usize = 200;

/// **How alike two notes have to be before the second one is refused.**
///
/// A floor and not a scale, because the gate is a refusal and a refusal has to be
/// arguable: at or over this and the write does not happen, and the refusal names
/// the note that was too close so the author can act on it. Raise it with a
/// measurement, not a feeling.
///
/// MEASURED 2026-10-12 against this box's own corpus — the operator's 23 standing
/// notes, read once by a scratch test deleted before the commit. The numbers, and
/// the arithmetic that produced them, are in `crate::similarity`'s module doc:
/// the closest honest pair in that corpus scores 0.183, the weakest near-copy
/// 0.363, a verbatim copy under a new name 0.957. 0.30 sits in the gap, and it
/// errs toward refusing — a refusal costs one round, a corpus of near-duplicates
/// costs every reader of the index.
pub const DUPLICATE_FLOOR: f64 = 0.30;

/// A note's abstract: the line the index shows for it, and where that line came
/// from.
pub struct Abstract {
    /// The line itself — one line, capped at [`ABSTRACT_CHARS`].
    pub text: String,
    /// `true` for the author's own marker line, `false` for the note's first
    /// proper sentence. The index says which: an abstract the harness derived is
    /// the harness's reading of the note, and a reader that cannot tell it from
    /// the author's is a reader taking a guess for a statement.
    pub written: bool,
}

/// The abstract the index shows for a note: the author's marker line when the
/// file carries one, the first proper sentence of its prose otherwise. `None`
/// only for a file with no prose at all — one that is nothing but headings.
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

/// The first **proper** sentence of a note's prose, capped at [`ABSTRACT_CHARS`]
/// — `None` when the text holds no prose at all (only headings, blank lines or
/// the abstract marker).
///
/// Proper, because the first version of this rule counted every period, and it
/// was measured cutting a lead inside `exit 101 in 2.` (the period of `2.7 s`)
/// and inside a path (`merge_review.attempts, .`). A period ends a sentence only
/// when whitespace or the end of the text follows it, which is the same
/// statement as *never a period inside a number or a filename*: `2.7`,
/// `merge_review.attempts` and `notes.md` all continue with a character.
///
/// A text with no sentence end in it is its own first sentence, and the cap is
/// what makes that a line rather than the whole note.
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

/// Whether the period at byte `at` ends a sentence.
///
/// The whole rule is this one test, in the three spellings it has to hold in: a
/// period followed by whitespace or by the end of the text ends a sentence, and
/// a period inside a number (`2.7`) or inside a filename (`merge_review.attempts`,
/// `notes.md`) is followed by neither, so it does not. The one case it does not
/// catch is a filename that a LINE BREAK put a space after — `crates/x.rs` at the
/// end of a line, joined to the next with a space — which cannot be told from a
/// sentence end without guessing at tokens; the join is the caller's, and the
/// limitation is stated rather than defended against.
fn ends_a_sentence(text: &str, at: usize) -> bool {
    match text[at + 1..].chars().next() {
        None => true,
        Some(c) => c.is_whitespace(),
    }
}

/// The author's abstract, as the marker line carries it — `None` when the file
/// has no marker, or has one with nothing after it.
fn written_abstract(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix(ABSTRACT_MARKER))
        .map(|rest| rest.trim().trim_end_matches("-->").trim().to_string())
        .filter(|a| !a.is_empty())
}

/// The note's own text with its marker line taken out — what an `append` grows,
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
/// body — the marker never doubled, and never added when there is none.
fn with_abstract(abstract_line: Option<&str>, body: &str) -> String {
    match abstract_line {
        Some(a) => format!("{ABSTRACT_MARKER} {a} -->\n\n{body}"),
        None => body.to_string(),
    }
}

/// One line, from whatever the caller passed: an abstract is the index's line,
/// so its newlines and runs of spaces are flattened rather than refused — the
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

/// A list marker off the front of a line — `- `, `* `, `+ `, `1. `, `2) `, or a
/// bare `1.` — so the abstract of a bulleted note is the bullet's own words, and
/// a numbered list's `1.` is never mistaken for a first sentence.
///
/// The space after the marker is required: `2.7 s` is a number, not item two.
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

/// Cut to [`ABSTRACT_CHARS`] at a character boundary, with an explicit ellipsis —
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
        match action {
            "list" => self.list(),
            "read" => self.read(
                args.get("path").and_then(|v| v.as_str()),
                args.get("name").and_then(|v| v.as_str()),
            ),
            "search" => self.search(args.get("query").and_then(|v| v.as_str())),
            "add" | "append" | "replace" => self.write(ctx, action, args),
            other => Invocation::failed(
                format!("unknown notes action `{other}`"),
                "call `notes` with `action` = \"list\", \"read\", \"search\", \"add\", \"append\" or \
                 \"replace\".",
            ),
        }
    }
}

impl NotesTool {
    /// **The gate's question, asked of the bytes that are about to be durable:**
    /// is this note a near-copy of one that exists?
    ///
    /// `target` is the note being written and is excluded from its own corpus —
    /// an `append` must not be refused for being similar to the note it grows.
    /// `after` is the whole file as it would land, not the fragment that changed:
    /// the score is a property of the note, and half a note has no score.
    ///
    /// `None` when nothing is close, and `None` too when there is no corpus — the
    /// first note in a workspace cannot be a duplicate of anything, and a gate
    /// that refused the first write would be a gate nobody kept.
    fn duplicate(&self, target: &Path, after: &str) -> Option<(PathBuf, f64)> {
        let corpus = similarity::Corpus::new(&corpus(self.scope.as_ref()));
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
    /// them — the model's map for `read`, and the honest report of a source
    /// that is empty or missing rather than silence about it.
    fn list(&self) -> Invocation {
        let mut out = String::from("the standing notes the harness reads, in prompt order:\n");
        let mut total = 0usize;
        for src in sources(self.scope.as_ref()) {
            let notes = src.notes();
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
            for p in notes {
                let (size, age) = std::fs::metadata(&p)
                    .map(|m| (super::human(m.len()), age_of(&m)))
                    .unwrap_or_else(|_| ("?".into(), "at an unknown time".into()));
                out.push_str(&format!(
                    "- {} — {}, last written {}\n",
                    p.display(),
                    size,
                    age
                ));
            }
        }
        out.push_str(&format!(
            "\n{total} note(s). A note records what was true when it was written and may be \
             outdated — check the tree before relying on one as a fact about now.\n"
        ));
        Invocation::ok(out)
    }

    /// One note's text, with the caveat line above it. Resolution: a bare
    /// name looks in the project notes first (that is the directory `add`
    /// writes, so reading and writing agree), then the box-wide notes, then
    /// `AGENTS.md`; a path matches the sources exactly or by its trailing
    /// components, so the paths `list` reports and the paths a digest in the
    /// prompt shows both work.
    fn read(&self, path: Option<&str>, name: Option<&str>) -> Invocation {
        let all: Vec<PathBuf> = sources(self.scope.as_ref())
            .iter()
            .flat_map(|s| s.notes())
            .collect();
        let found: Vec<PathBuf> = if let Some(name) = name {
            let stem = name.trim().trim_end_matches(".md");
            all.iter()
                .filter(|p| {
                    p.file_stem().is_some_and(|s| {
                        s == stem || s.to_string_lossy().eq_ignore_ascii_case(stem)
                    })
                })
                .cloned()
                .collect()
        } else if let Some(path) = path {
            let want = path.trim().trim_start_matches("./");
            all.iter()
                .filter(|p| {
                    let have = p.display().to_string();
                    if want.starts_with('/') {
                        have == want
                    } else {
                        // A relative path matches by its trailing components, so
                        // `.letibot/notes/foo.md` and a bare `foo.md` both find
                        // the same note; two sources with one name between them
                        // is the ambiguous case below, not a silent pick.
                        have == want || have.ends_with(&format!("/{want}"))
                    }
                })
                .cloned()
                .collect()
        } else {
            return Invocation::failed(
                "notes read needs a name or a path",
                "call `notes` with `action` = \"read\" and either `name` (a project note's \
                 name) or `path` (as `list` reported it).",
            );
        };
        match found.as_slice() {
            [one] => match std::fs::read_to_string(one) {
                Ok(text) => match std::fs::metadata(one) {
                    Ok(meta) => Invocation::ok(format!(
                        "### {} — {}, last written {}\n\n{}\n\n{}\n",
                        one.display(),
                        super::human(meta.len()),
                        age_of(&meta),
                        caveat(&meta),
                        text.trim_end()
                    )),
                    Err(e) => Invocation::ok(format!(
                        "### {} (when last written is unreadable: {e})\n\n{}\n\n{}\n",
                        one.display(),
                        // The age is the caveat's evidence; without it the line
                        // still says what a note is, and the header says why the
                        // date is missing rather than pretending to one.
                        "[historical note — it records what was true when it was written and \
                         may be outdated. Verify against the tree before relying on it.]",
                        text.trim_end()
                    )),
                },
                Err(e) => Invocation::failed(
                    format!("`{}` could not be read: {e}", one.display()),
                    "it was listed a moment ago, so it moved or its permissions changed. \
                     `notes` action=\"list\" re-reads the directory.",
                ),
            },
            [] => {
                let mut tell = String::from(
                    "no note matches. The notes that exist, in prompt \
                     order:\n",
                );
                for p in &all {
                    tell.push_str(&format!("- {}\n", p.display()));
                }
                if all.is_empty() {
                    tell.push_str("(none — no source has a note right now)\n");
                }
                Invocation::failed("no such note", tell)
            }
            many => {
                let tell: String = many
                    .iter()
                    .map(|p| format!("- {}\n", p.display()))
                    .collect();
                Invocation::failed(
                    "that name matches more than one note",
                    format!("give the full path instead. Matches:\n{tell}"),
                )
            }
        }
    }

    /// **The search door: every note's own lines, by the words in them.**
    ///
    /// The case it exists for is the index being poor. The index names a note,
    /// its headings and one line of it; a note whose abstract is a weak summary
    /// of what it holds is still findable by its own words, and this is a local
    /// substring match over the whole corpus — no model, no network, and no
    /// dependence on the digest having been any good. That last part is the
    /// point: the door has to work when the thing it is a door to does not.
    ///
    /// It answers in `grep`'s register, because that is the register a reader
    /// already knows how to act on: the path, then the matching lines with their
    /// numbers, so a line can be cited — or read around with `read`'s `ranges` —
    /// without a second search. Case-insensitive, because this is a recall act
    /// and not a syntax one.
    ///
    /// A query that matches nothing is an **abstention**, the same outcome
    /// `grep` gives an empty search: the corpus was searched completely, so "no
    /// note holds this" is an answer — and the body lists the notes there are,
    /// which is the one thing a caller can act on next.
    fn search(&self, query: Option<&str>) -> Invocation {
        let Some(query) = query.map(str::trim).filter(|q| !q.is_empty()) else {
            return Invocation::failed(
                "notes search needs a query",
                "call `notes` with `action` = \"search\" and `query` set to the words to look \
                 for. It is matched case-insensitively against every line of every note the \
                 harness reads.",
            );
        };
        let needle = query.to_lowercase();
        let all: Vec<PathBuf> = sources(self.scope.as_ref())
            .iter()
            .flat_map(|s| s.notes())
            .collect();
        let mut out = String::new();
        let mut found = 0usize;
        let mut printed = 0usize;
        let mut notes_hit = 0usize;
        for path in &all {
            // A note that cannot be read is not a note that does not match: the
            // corpus this answers over is the one that could be read, and the
            // count of what was searched is what the answer is worth.
            let Ok(text) = std::fs::read_to_string(path) else {
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
            out.push_str(&format!("\n### {}\n", path.display()));
            for (n, line) in hits {
                found += 1;
                if printed < MAX_SEARCH_LINES {
                    printed += 1;
                    out.push_str(&format!("  {n}: {}\n", clip(line)));
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
            for p in &all {
                let (size, age) = std::fs::metadata(p)
                    .map(|m| (super::human(m.len()), age_of(&m)))
                    .unwrap_or_else(|_| ("?".into(), "at an unknown time".into()));
                tell.push_str(&format!(
                    "- {} — {}, last written {}\n",
                    p.display(),
                    size,
                    age
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

    /// `add`, `append`, `replace` — the three write verbs, each refusing the
    /// case it must not do silently: `add` will not overwrite, `append` and
    /// `replace` will not create, and `replace` says what it threw away.
    fn write(&self, ctx: &mut InvokeCtx<'_>, action: &str, args: &Value) -> Invocation {
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
        let dir = self.scope.workspace().join(".letibot").join("notes");
        let target = dir.join(format!("{stem}.md"));
        let existing = std::fs::read_to_string(&target).ok();
        // The abstract is the author's to set, and it is the one part of a note
        // the text cannot be asked for: absent, nothing is written and the
        // harness derives the note's first sentence ([`abstract_of`]). Supplied,
        // it goes in as the marker line above the text.
        let asked = args.get("abstract").and_then(|v| v.as_str()).map(one_line);
        let (before, after, said) = match (action, &existing) {
            ("add", Some(_)) => {
                let meta = std::fs::metadata(&target).ok();
                return Invocation::failed(
                    format!("a note named `{stem}` already exists"),
                    format!(
                        "it is {} — {}, last written {}. Nothing was written. Append to it \
                         with `action` = \"append\", or rewrite it deliberately with \
                         `action` = \"replace\", which reports what it replaced.",
                        target.display(),
                        meta.as_ref()
                            .map(|m| super::human(m.len()))
                            .unwrap_or_else(|| "?".into()),
                        meta.as_ref()
                            .map(|m| age_of(m))
                            .unwrap_or_else(|| "at an unknown time".into()),
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
                let near = sources(self.scope.as_ref())[2]
                    .notes()
                    .iter()
                    .map(|p| {
                        p.file_stem()
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
                // The marker is not text to grow: it is lifted out, the note is
                // grown, and the abstract — the new one, or the one that was
                // there — goes back on top. An `append` that asked for no new
                // abstract keeps the old one rather than silently dropping the
                // line the index leads with.
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
        // **The similarity gate, on BOTH write triggers and just before the write
        // becomes durable.** A note that is a near-copy of one that exists is a
        // defect wherever it arrives from — the `add` that creates it and the
        // `append`/`replace` that grows one into it — so the check sits here,
        // after the bytes are computed and before `write_atomic`, rather than at
        // creation only. See [`DUPLICATE_FLOOR`].
        if let Some((other, score)) = self.duplicate(&target, &after) {
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
        if let Err(e) = write_atomic(&target, after.as_bytes()) {
            return Invocation::failed(
                format!("could not write `{}`: {e}", target.display()),
                "the note is unchanged — the write is a temporary file renamed over the \
                 target, so a failure leaves the original untouched.",
            );
        }
        let mut inv = Invocation::ok(format!(
            "{said}: {}.\n{}\n\n{WHEN_READ}\n",
            super::human(after.len() as u64),
            // **What the index will lead with**, said at the moment the author
            // can still change it: their own line, the note's first sentence, or
            // the fact that there is no prose to take one from. It is computed
            // by the reader's own function, so it cannot describe an index the
            // reader would not build.
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
        // The digests and the span before the strings move into the pair — a
        // diff card whose digest disagrees with its own before-side is worse
        // than no card.
        let (before_digest, after_digest) = (
            crate::spill::content_hash(before.as_bytes()),
            crate::spill::content_hash(after.as_bytes()),
        );
        let changed = crate::edit::changed_span(&before, &after);
        inv.edit = Some(crate::edit::FileEdit {
            path: target
                .strip_prefix(self.scope.workspace())
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

/// Atomic write, by the same rules `HostBackend::write` keeps: a temporary in
/// the target's own directory (created 0600, so nothing reads a half-written
/// note), synced, then renamed over the target — which keeps the previous
/// mode, or 0644 for a note that did not exist. A failure removes the
/// temporary and leaves the original untouched.
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
mod tests {
    use super::*;
    use crate::testing::writable_harness;

    /// **`read` carries the historical line and the age, above the body — the
    /// operator's ask verbatim.**
    #[test]
    fn read_carries_the_historical_line_with_the_age() {
        let mut h = writable_harness();
        let (ws, _global) = h.notes_dirs();
        std::fs::create_dir_all(ws.join(".letibot/notes")).expect("project dir");
        std::fs::write(
            ws.join(".letibot/notes/oddity.md"),
            "the rano pin is absent on this box; build from outside the repo\n",
        )
        .expect("fixture");
        let r = h.call("notes", r#"{"action":"read","name":"oddity"}"#);
        assert!(r.is_grounded(), "{}", r.render());
        let said = r.render();
        assert!(
            said.contains("[historical note — last written"),
            "the caveat line is there, dated: {said}"
        );
        assert!(
            said.contains("may be outdated"),
            "the operator's own words: {said}"
        );
        assert!(
            said.contains("build from outside the repo"),
            "the body follows the caveat: {said}"
        );
        let caveat_at = said.find("[historical note").expect("caveat");
        let body_at = said.find("build from outside").expect("body");
        assert!(caveat_at < body_at, "the caveat comes first: {said}");
    }

    /// **A note the tool writes lands where the reader reads it — and by
    /// name, by path, and with `.md` spelled or not, it is the same note.**
    #[test]
    fn add_writes_into_the_project_notes_dir_and_reads_back() {
        let mut h = writable_harness();
        let r = h.call(
            "notes",
            r#"{"action":"add","name":"finding","text":"qwen scores better with the tail clamped"}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        let (ws, global) = h.notes_dirs();
        let wrote = std::fs::read_to_string(ws.join(".letibot/notes/finding.md"))
            .expect("the note is on disk");
        assert_eq!(wrote, "qwen scores better with the tail clamped\n");
        assert!(
            !global.join("finding.md").exists(),
            "the box-wide dir is never written"
        );
        assert!(
            !ws.join("finding.md").exists(),
            "the workspace root is never written"
        );
        // Read back three ways: bare name, name with the extension spelled,
        // and the full path.
        for args in [
            r#"{"action":"read","name":"finding"}"#,
            r#"{"action":"read","name":"finding.md"}"#,
            &format!(
                r#"{{"action":"read","path":"{}"}}"#,
                ws.join(".letibot/notes/finding.md").display()
            ),
        ] {
            let r = h.call("notes", args);
            assert!(r.is_grounded(), "{args}: {}", r.render());
            assert!(
                r.render().contains("tail clamped"),
                "{args}: {}",
                r.render()
            );
        }
        // And the diff card is there for the head.
        assert!(r.edit.is_some(), "a write carries its before/after pair");
    }

    /// **`add` refuses to overwrite and says what is there; `append` and
    /// `replace` refuse to create and name the near misses.**
    #[test]
    fn the_write_verbs_refuse_the_cases_they_must_not_do_silently() {
        let mut h = writable_harness();
        h.call(
            "notes",
            r#"{"action":"add","name":"gate","text":"the gate refuses by class"}"#,
        );
        let r = h.call(
            "notes",
            r#"{"action":"add","name":"gate","text":"overwriting"}"#,
        );
        assert!(!r.is_grounded(), "an add over a note is a refusal");
        let said = r.render();
        assert!(
            said.contains("already exists") && said.contains("last written"),
            "the refusal reports the note it refused to touch: {said}"
        );
        let (ws, _) = h.notes_dirs();
        assert_eq!(
            std::fs::read_to_string(ws.join(".letibot/notes/gate.md")).unwrap(),
            "the gate refuses by class\n",
            "the refusal left the original alone"
        );

        // append to a missing note names the near miss rather than creating.
        let r = h.call("notes", r#"{"action":"append","name":"gat","text":"typo"}"#);
        assert!(!r.is_grounded(), "an append to a missing note is a refusal");
        let said = r.render();
        assert!(
            said.contains("no note named `gat`"),
            "the refusal names the missing note: {said}"
        );
        assert!(
            said.contains("closest: gate.md"),
            "and the near miss beside it: {said}"
        );

        // replace reports what it replaced.
        let r = h.call(
            "notes",
            r#"{"action":"replace","name":"gate","text":"replaced wholesale"}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        let said = r.render();
        assert!(
            said.contains("first line \"the gate refuses by class\""),
            "the replacement says what it threw away: {said}"
        );
        assert_eq!(
            std::fs::read_to_string(ws.join(".letibot/notes/gate.md")).unwrap(),
            "replaced wholesale\n"
        );

        // append grows what exists.
        let r = h.call(
            "notes",
            r#"{"action":"append","name":"gate","text":"- and by name"}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        assert_eq!(
            std::fs::read_to_string(ws.join(".letibot/notes/gate.md")).unwrap(),
            "replaced wholesale\n\n- and by name\n",
            "append joins with a blank line and keeps one trailing newline"
        );
    }

    /// A note about the store's write lock, written the way a session writes
    /// one: an abstract and three headings, which is the whole of what the
    /// score reads.
    const STORE_LOCK: &str = "<!-- abstract: a store connection waits five seconds for the \
                              write lock and a busy handler holds the wait -->\n\n\
                              ## The lock\n\nThe connection waits, and the wait is what the busy \
                              handler is for.\n\n\
                              ## What the operator saw\n\nA failed write with no wait at all.\n\n\
                              ## The handler\n\nIt holds the wait rather than failing the call.\n";

    /// **The near-copy: the same note, a new name, its abstract reworded.**
    ///
    /// The measurement in `crate::similarity` puts this shape at 0.363 and the
    /// closest honest pair in this box's corpus at 0.183, so it is the fixture
    /// the floor has to be on the right side of.
    const STORE_LOCK_AGAIN: &str = "<!-- abstract: a connection to the store holds for the \
                                    write lock, and the busy handler is what carries the \
                                    wait -->\n\n\
                                    ## The lock\n\nThe connection waits, and the wait is what the \
                                    busy handler is for.\n\n\
                                    ## What the operator saw\n\nA failed write with no wait at \
                                    all.\n\n\
                                    ## The handler\n\nIt holds the wait rather than failing the \
                                    call.\n";

    /// **A near-duplicate note is refused by the write gate, and a genuinely
    /// new one is not.**
    ///
    /// The operator's ask, verbatim: *"since we ride similarity score - worth
    /// having it as a gate for new notes"*. Both halves are asserted against the
    /// REAL tool over a fixture workspace, because a gate that only refuses — or
    /// only admits — is not a gate, and the half that is easier to forget is the
    /// one that must not fire.
    #[test]
    fn a_near_duplicate_note_is_refused_and_a_new_one_is_not() {
        let mut h = writable_harness();
        let (ws, _) = h.notes_dirs();

        // The first note in an empty workspace cannot be a duplicate of anything,
        // and the gate says so by admitting it. The second is admitted too, and
        // for a reason that is worth stating: **with one note in the corpus every
        // term is in every note, so `idf` is zero and nothing scores at all.**
        // A gate that cannot discriminate admits — the unsafe direction, and the
        // same limitation the read hint has in the other direction. Two notes is
        // where the arithmetic starts to mean something.
        let r = h.call(
            "notes",
            r###"{"action":"add","name":"tui-scrollbar-width","abstract":"the pane scrollbar is one column wide and the border colour is not the theme's","text":"## The width\n\nOne column.\n\n## The colour\n\nIt comes from the theme.\n"}"###,
        );
        assert!(r.is_grounded(), "the first note lands: {}", r.render());
        let r = h.call(
            "notes",
            &format!(r#"{{"action":"add","name":"store-write-lock","text":{STORE_LOCK:?}}}"#),
        );
        assert!(r.is_grounded(), "the second lands: {}", r.render());

        // The near-copy, under a name that says nothing about it being one.
        let r = h.call(
            "notes",
            &format!(
                r#"{{"action":"add","name":"store-connection-wait","text":{STORE_LOCK_AGAIN:?}}}"#
            ),
        );
        assert!(!r.is_grounded(), "a near-copy is refused");
        let said = r.render();
        assert!(
            said.contains("near-copy") && said.contains("store-write-lock.md"),
            "the refusal names the note that was too close: {said}"
        );
        assert!(
            said.contains("Nothing was written"),
            "and that nothing happened: {said}"
        );
        assert!(
            !ws.join(".letibot/notes/store-connection-wait.md").exists(),
            "the refused note is not on disk"
        );

        // **And a genuinely new note is admitted**, in the same corpus, through
        // the same gate — the half that a floor set too low would break.
        let r = h.call(
            "notes",
            r###"{"action":"add","name":"compaction-tail-clamp","abstract":"compaction folds the first half of a conversation into a summary and the tail is clamped against the budget","text":"## What is folded\n\nThe first half.\n\n## The tail\n\nClamped.\n"}"###,
        );
        assert!(
            r.is_grounded(),
            "a note about something else is not a near-copy: {}",
            r.render()
        );
        assert!(ws.join(".letibot/notes/compaction-tail-clamp.md").exists());
    }

    /// **An edit that would turn a note into a near-copy of another is caught
    /// just before it is durable — and the note's own text is not what it is
    /// measured against.**
    ///
    /// The operator's second half, verbatim: *"add it for 'just before edit made
    /// durable' too. we dont want to endup with n almost identical notes by edits
    /// anyway"*. `replace` is the edit that rewrites a note wholesale, and it is
    /// the one that can make a near-copy out of two notes that were distinct.
    /// `append` is the same door with a smaller step, and the note being edited is
    /// excluded from its own corpus — otherwise growing a note would refuse itself.
    #[test]
    fn an_edit_that_would_make_a_note_a_near_copy_is_caught() {
        let mut h = writable_harness();
        let (ws, _) = h.notes_dirs();
        h.call(
            "notes",
            &format!(r#"{{"action":"add","name":"store-write-lock","text":{STORE_LOCK:?}}}"#),
        );
        h.call(
            "notes",
            r###"{"action":"add","name":"tui-scrollbar-width","abstract":"the pane scrollbar is one column wide and the border colour is not the theme's","text":"## The width\n\nOne column.\n\n## The colour\n\nIt comes from the theme.\n"}"###,
        );
        let before = std::fs::read_to_string(ws.join(".letibot/notes/tui-scrollbar-width.md"))
            .expect("the second note is on disk");

        // `replace`, turning the TUI note into the store note's near-copy.
        let r = h.call(
            "notes",
            &format!(
                r#"{{"action":"replace","name":"tui-scrollbar-width","text":{STORE_LOCK_AGAIN:?}}}"#
            ),
        );
        assert!(!r.is_grounded(), "the edit is refused: {}", r.render());
        assert!(
            r.render().contains("store-write-lock.md"),
            "and it names what it would have duplicated: {}",
            r.render()
        );
        assert_eq!(
            std::fs::read_to_string(ws.join(".letibot/notes/tui-scrollbar-width.md")).unwrap(),
            before,
            "the refusal is before the write, so the note is byte-identical"
        );

        // **An edit that grows a note away from the other one is admitted**, and
        // an `append` of a note's own words to itself is not a self-duplicate.
        let r = h.call(
            "notes",
            r###"{"action":"append","name":"store-write-lock","text":"## And the timeout is configurable\n\nThirty seconds, not five, since 2026-10-11.\n"}"###,
        );
        assert!(
            r.is_grounded(),
            "a note is not a duplicate of itself: {}",
            r.render()
        );
        let grown = std::fs::read_to_string(ws.join(".letibot/notes/store-write-lock.md")).unwrap();
        assert!(grown.contains("configurable"), "the append landed: {grown}");
    }

    /// **The floor's two sides, stated as numbers, so a change to the
    /// arithmetic cannot move the gate quietly.**
    ///
    /// The measured corpus is not in the tree — these are fixtures built to the
    /// shapes that were measured, and the numbers they hold are the ones
    /// `crate::similarity`'s module doc records.
    #[test]
    fn the_gate_floor_sits_between_the_measured_shapes() {
        let corpus = similarity::Corpus::new(&[
            (
                PathBuf::from("/notes/store-write-lock.md"),
                STORE_LOCK.to_string(),
            ),
            (
                PathBuf::from("/notes/tui-scrollbar-width.md"),
                "<!-- abstract: the pane scrollbar is one column wide and the border colour is \
                 not the theme's -->\n\n## The width\n\nOne column.\n\n## The colour\n\nIt \
                 comes from the theme.\n"
                    .to_string(),
            ),
        ]);
        let at = |p: &str, text: &str| similarity::index_text(&PathBuf::from(p), text);

        // The near-copy: over the floor.
        let copy = at("/notes/store-connection-wait.md", STORE_LOCK_AGAIN);
        let (path, score) = corpus.nearest(&copy, None).expect("a corpus of two");
        assert!(
            score >= DUPLICATE_FLOOR,
            "the near-copy scores {score:.3}, under the {DUPLICATE_FLOOR:.2} floor"
        );
        assert!(path.ends_with("store-write-lock.md"), "{path:?}");

        // Two notes about different subjects: under it, with room.
        let (path, score) = corpus
            .nearest(
                &at(
                    "/notes/compaction-tail.md",
                    "<!-- abstract: compaction folds the first half of a conversation into a \
                     summary and the tail is clamped against the budget -->\n\n## What is \
                     folded\n\nThe first half.\n",
                ),
                None,
            )
            .expect("a corpus of two");
        assert!(
            score < DUPLICATE_FLOOR,
            "a genuinely new note scores {score:.3} against {path:?}, over the \
             {DUPLICATE_FLOOR:.2} floor"
        );

        // And the note being edited is not in its own corpus: the same text
        // scored against a corpus that still holds it would be a self-refusal.
        let itself = PathBuf::from("/notes/store-write-lock.md");
        let (path, score) = corpus
            .nearest(&at("/notes/store-write-lock.md", STORE_LOCK), Some(&itself))
            .expect("the other note is still there");
        assert!(path.ends_with("tui-scrollbar-width.md"), "{path:?}");
        assert!(score < DUPLICATE_FLOOR, "{score:.3}");
    }

    /// **There is no spelling of a write that leaves the project notes
    /// directory.**
    #[test]
    fn a_name_that_escapes_the_notes_dir_is_refused() {
        let mut h = writable_harness();
        // The target of a path-spelling name is the operator's own AGENTS.md,
        // so write one and compare bytes after: the honest check is
        // byte-identical, not absent.
        let (ws, _) = h.notes_dirs();
        std::fs::write(ws.join("AGENTS.md"), "the operator's own instructions\n")
            .expect("fixture AGENTS.md");
        let agents_before = std::fs::read(ws.join("AGENTS.md")).expect("the fixture's AGENTS.md");
        for (name, because) in [
            ("../AGENTS", "names a path"),
            ("sub/evil", "names a path"),
            ("/etc/passwd", "names a path"),
            ("..", "names a path"),
            (".hidden", "starts with a dot"),
        ] {
            let args = format!(r#"{{"action":"add","name":"{name}","text":"no"}}"#);
            let r = h.call("notes", args.as_str());
            assert!(!r.is_grounded(), "`{name}` must be refused");
            assert!(r.render().contains(because), "`{name}`: {}", r.render());
            assert!(
                r.render().contains("Nothing was written") || r.render().contains("writes go to"),
                "`{name}`: {}",
                r.render()
            );
        }
        let (ws, _) = h.notes_dirs();
        assert_eq!(
            std::fs::read(ws.join("AGENTS.md")).expect("the fixture's AGENTS.md"),
            agents_before,
            "AGENTS.md is byte-identical — no spelling of a write reached it"
        );
        let wrote: Vec<_> = std::fs::read_dir(ws.join(".letibot/notes"))
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert!(wrote.is_empty(), "nothing was written: {wrote:?}");
    }

    /// **A read reaches the box-wide dir — outside every session's backend
    /// view — and an unknown name refuses with the notes that exist.**
    #[test]
    fn read_reaches_the_box_wide_dir_and_a_miss_lists_what_exists() {
        let mut h = writable_harness();
        let (ws, global) = h.notes_dirs();
        std::fs::create_dir_all(&global).expect("global dir");
        std::fs::write(global.join("box-rules.md"), "box-wide rule\n").expect("fixture");
        let by_path = format!(
            r#"{{"action":"read","path":"{}"}}"#,
            global.join("box-rules.md").display()
        );
        let r = h.call("notes", by_path.as_str());
        assert!(
            r.is_grounded(),
            "the config dir is outside the backend root and the tool reads it anyway: {}",
            r.render()
        );
        assert!(r.render().contains("box-wide rule"));

        let r = h.call("notes", r#"{"action":"read","name":"nope"}"#);
        assert!(!r.is_grounded());
        let said = r.render();
        assert!(
            said.contains("no such note") && said.contains("box-rules.md"),
            "the miss lists what there is: {said}"
        );

        // The miss stays honest after the note it would have named is gone:
        // the listing is re-read, so it shows what exists NOW — which, with the
        // box-wide note deleted and no AGENTS.md in the fixture, is nothing,
        // and it says so rather than showing an empty listing.
        let _ = std::fs::remove_file(global.join("box-rules.md"));
        let r = h.call("notes", r#"{"action":"read","name":"nope"}"#);
        let said = r.render();
        assert!(
            said.contains("(none — no source has a note right now)"),
            "the empty case is said, not shown as silence: {said}"
        );
    }

    /// **The tool is `Access::Write` in the schema and narrows to a read for
    /// `list`/`read` — and a write-denied downgrade drops it from the seat.**
    #[test]
    fn the_access_class_is_write_and_the_read_verbs_narrow() {
        let scope: Arc<dyn NotesScope> = Arc::new(FixtureScope {
            workspace: std::env::temp_dir(),
            global: std::env::temp_dir(),
        });
        let tool = NotesTool::new(scope.clone());
        assert_eq!(tool.schema().access, Access::Write);
        assert_eq!(
            tool.access_for(&serde_json::json!({"action": "read", "name": "x"})),
            Some(Access::Read),
            "the read verbs narrow below the gate"
        );
        assert_eq!(
            tool.access_for(&serde_json::json!({"action": "list"})),
            Some(Access::Read)
        );
        assert_eq!(
            tool.access_for(&serde_json::json!({"action": "add", "name": "x", "text": "y"})),
            None,
            "the write verbs keep the schema's class"
        );
        // Seated beside `write`/`edit` in the roles that have them, and in no
        // other seat: a note is a write to the operator's tree, so it rides
        // the write decision rather than arriving with the read-only seats.
        for role in [
            crate::runtime::roles::m2_coder(),
            crate::runtime::roles::leticode(),
        ] {
            assert!(
                role.tools.contains(&"notes".to_string()),
                "`{}` seats `notes`",
                role.name
            );
        }
        for role in [
            crate::runtime::roles::m1_orchestrator(),
            crate::runtime::roles::planner(),
            crate::runtime::roles::m3_researcher(),
            crate::runtime::roles::m2_runner(),
            crate::runtime::roles::gatekeeper(),
        ] {
            assert!(
                !role.tools.contains(&"notes".to_string()),
                "`{}` must not seat `notes` — a seat without `write`/`edit` does not \
                 gain a write path by the notes door",
                role.name
            );
        }
        // And the downgrade drops it with the other write tools.
        let denied = std::collections::BTreeSet::from([Access::Write]);
        let mut reg = crate::runtime::Registry::new();
        reg.register(Box::new(NotesTool::new(scope)))
            .expect("register");
        let after = reg.without_access(&denied);
        assert!(
            !after.names().contains(&"notes".to_string()),
            "a no-write seat is not told it can write notes"
        );
    }

    /// **A read-only backend refuses the write verbs, naming the gate.**
    #[test]
    fn a_read_only_backend_refuses_the_write_verbs() {
        let mut h = crate::testing::read_only_harness_with_gate(Some(crate::testing::allow_all()));
        let r = h.call("notes", r#"{"action":"add","name":"x","text":"y"}"#);
        assert!(!r.is_grounded());
        assert!(
            r.render().contains("read-only"),
            "the refusal names the second gate: {}",
            r.render()
        );
        assert!(
            r.render().contains("Nothing on disk changed"),
            "and says the disk is untouched: {}",
            r.render()
        );
    }

    /// **The listing carries every source, in the prompt's own order, with the
    /// age and the one-line caveat — and within a directory, newest first.**
    ///
    /// The order is the reader's rule (`letibot_harnessd::standing_notes`):
    /// `AGENTS.md`, then the box-wide notes, then the project's, each directory
    /// newest first. `list` claims to report them "in prompt order", and a
    /// listing in another order is a listing that makes that sentence false.
    #[test]
    fn list_reports_every_source_in_the_readers_order() {
        let mut h = writable_harness();
        let (ws, global) = h.notes_dirs();
        std::fs::create_dir_all(&global).expect("global dir");
        std::fs::write(global.join("g.md"), "global note\n").expect("fixture");
        std::fs::write(ws.join("AGENTS.md"), "# Rules\n").expect("fixture");
        std::fs::create_dir_all(ws.join(".letibot/notes")).expect("project dir");
        std::fs::write(ws.join(".letibot/notes/older.md"), "project older\n").expect("fixture");
        std::fs::write(ws.join(".letibot/notes/newer.md"), "project newer\n").expect("fixture");
        set_mtime(&ws.join(".letibot/notes/older.md"), 1_000);
        set_mtime(&ws.join(".letibot/notes/newer.md"), 2_000);
        let r = h.call("notes", r#"{"action":"list"}"#);
        assert!(r.is_grounded(), "{}", r.render());
        let said = r.render();
        let pos = |needle: &str| said.find(needle).expect(needle);
        assert!(
            pos("AGENTS.md") < pos("g.md")
                && pos("g.md") < pos("newer.md")
                && pos("newer.md") < pos("older.md"),
            "AGENTS.md, then box-wide, then the project's newest first: {said}"
        );
        assert!(
            said.contains("last written") && said.contains("may be outdated"),
            "the age and the caveat ride the listing: {said}"
        );
    }

    /// **`search` answers with paths, line numbers and the matching lines —
    /// case-insensitively, and over the box-wide notes too.**
    ///
    /// The box-wide dir is the half of the corpus no `read` of the session's
    /// own tree can reach, and it is the half an index built from the wrong
    /// assumption would leave out.
    #[test]
    fn search_answers_with_paths_line_numbers_and_lines() {
        let mut h = writable_harness();
        let (ws, global) = h.notes_dirs();
        std::fs::create_dir_all(&global).expect("global dir");
        std::fs::write(
            global.join("box.md"),
            "the box-wide rule\nand a second line about the budget\n",
        )
        .expect("fixture");
        std::fs::create_dir_all(ws.join(".letibot/notes")).expect("project dir");
        std::fs::write(
            ws.join(".letibot/notes/ledger.md"),
            "the ledger counts tokens\n\nnothing else here\n",
        )
        .expect("fixture");

        let r = h.call("notes", r#"{"action":"search","query":"tokens"}"#);
        assert!(r.is_grounded(), "{}", r.render());
        let said = r.render();
        assert!(
            said.contains("ledger.md") && said.contains("1: the ledger counts tokens"),
            "the path, the line number and the line: {said}"
        );

        // Case-insensitive, and the other source is answered under its own path.
        let r = h.call("notes", r#"{"action":"search","query":"BUDGET"}"#);
        assert!(r.is_grounded(), "{}", r.render());
        let said = r.render();
        assert!(
            said.contains("box.md") && said.contains("2: and a second line about the budget"),
            "the box-wide note is in the corpus: {said}"
        );
        assert!(
            said.contains("may be outdated"),
            "a hit carries the same caveat a read does: {said}"
        );
    }

    /// **A search that matches nothing abstains, and lists the notes there
    /// are** — clause 1 in the tool that most needs it, since a miss here is
    /// the caller's only evidence about a corpus it cannot see.
    #[test]
    fn a_search_that_matches_nothing_abstains_with_the_corpus() {
        let mut h = writable_harness();
        let (_ws, global) = h.notes_dirs();
        std::fs::create_dir_all(&global).expect("global dir");
        std::fs::write(global.join("box.md"), "the box-wide rule\n").expect("fixture");
        let r = h.call("notes", r#"{"action":"search","query":"quokka_sentinel"}"#);
        assert!(!r.is_grounded());
        let said = r.render();
        assert_eq!(
            crate::result::Envelope::classify(&said),
            Some("NO_RESULT"),
            "a complete search that found nothing abstains rather than failing: {said}"
        );
        assert!(
            said.contains("no line of any note matches")
                && said.contains("box.md")
                && said.contains("may be outdated"),
            "the miss says what it searched and what there is: {said}"
        );
        // An empty query is not a search of the corpus; it is a caller that has
        // not said what to look for, and that is a failure rather than an
        // abstention about content.
        let r = h.call("notes", r#"{"action":"search","query":"   "}"#);
        assert!(!r.is_grounded());
        assert!(r.render().contains("needs a query"), "{}", r.render());
    }

    /// **The author's abstract is written into the note and leads the index;
    /// without one the harness derives the note's first sentence.**
    #[test]
    fn an_abstract_is_the_authors_line_or_the_notes_first_sentence() {
        let mut h = writable_harness();
        let (ws, _global) = h.notes_dirs();
        let r = h.call(
            "notes",
            r#"{"action":"add","name":"pins","abstract":"the rano pin is absent on this box","text":"Build from outside the repo.\nThe second line is detail."}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        let wrote = std::fs::read_to_string(ws.join(".letibot/notes/pins.md")).expect("on disk");
        assert_eq!(
            wrote,
            concat!(
                "<!-- abstract: the rano pin is absent on this box -->\n\n",
                "Build from outside the repo.\n",
                "The second line is detail.\n"
            ),
            "the marker line, then the text: {wrote}"
        );
        assert!(
            r.render()
                .contains("your abstract: the rano pin is absent on this box"),
            "the write says what the index will lead with: {}",
            r.render()
        );

        // No abstract asked for: nothing is written into the note, and the
        // report derives the same line the reader will derive.
        let r = h.call(
            "notes",
            r#"{"action":"add","name":"derived","text":"First proper sentence here. Second one."}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        assert_eq!(
            std::fs::read_to_string(ws.join(".letibot/notes/derived.md")).unwrap(),
            "First proper sentence here. Second one.\n",
            "nothing is added to the author's text"
        );
        assert!(
            r.render()
                .contains("first sentence: First proper sentence here."),
            "the derivation is the reader's own: {}",
            r.render()
        );

        // An append keeps the abstract that was there, and one that asks for a
        // new one replaces it rather than leaving two.
        let r = h.call(
            "notes",
            r#"{"action":"append","name":"pins","text":"- and a bullet"}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        let grew = std::fs::read_to_string(ws.join(".letibot/notes/pins.md")).unwrap();
        assert_eq!(
            grew.matches("<!-- abstract:").count(),
            1,
            "one marker, never two: {grew}"
        );
        assert!(
            grew.contains("the rano pin is absent on this box") && grew.contains("- and a bullet"),
            "the kept abstract and the new text: {grew}"
        );
        let r = h.call(
            "notes",
            r#"{"action":"append","name":"pins","abstract":"now about pins and bullets","text":"more"}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        let regrew = std::fs::read_to_string(ws.join(".letibot/notes/pins.md")).unwrap();
        assert_eq!(regrew.matches("<!-- abstract:").count(), 1, "{regrew}");
        assert!(
            regrew.starts_with("<!-- abstract: now about pins and bullets -->")
                && !regrew.contains("the rano pin is absent"),
            "the new abstract replaced the old one: {regrew}"
        );
    }

    /// **The derivation, spelled out**: a period ends a sentence only when
    /// whitespace or the end of the text follows it, so `2.7 s`, a path and a
    /// numbered list are not sentence ends.
    #[test]
    fn the_derived_abstract_stops_at_a_proper_sentence_end() {
        for (text, want) in [
            (
                "It exits 101 in 2.7 s. Then it stops.",
                "It exits 101 in 2.7 s.",
            ),
            (
                "See merge_review.attempts, .5 of the rows. Then read it.",
                "See merge_review.attempts, .5 of the rows.",
            ),
            (
                "Read crates/x.md for the shape. Then edit it.",
                "Read crates/x.md for the shape.",
            ),
            (
                "1. first thing\n2. second thing",
                "first thing second thing",
            ),
            (
                "no period anywhere in this line",
                "no period anywhere in this line",
            ),
        ] {
            assert_eq!(
                first_sentence(text).as_deref(),
                Some(want),
                "the first proper sentence of {text:?}"
            );
        }
        // A sentence longer than the cap is cut at a character boundary and says
        // so — never silently, and never mid-character.
        let long = format!("{} tail", "word ".repeat(80));
        let cut = first_sentence(&long).expect("a sentence");
        assert_eq!(cut.chars().count(), ABSTRACT_CHARS);
        assert!(cut.ends_with('…'), "{cut}");
    }

    /// **`search` is a read verb: it narrows below the gate like `list` and
    /// `read`, and a seat with no write keeps it.**
    #[test]
    fn search_narrows_to_a_read_like_the_other_read_verbs() {
        let scope: Arc<dyn NotesScope> = Arc::new(FixtureScope {
            workspace: std::env::temp_dir(),
            global: std::env::temp_dir(),
        });
        let tool = NotesTool::new(scope);
        assert_eq!(
            tool.access_for(&serde_json::json!({"action": "search", "query": "x"})),
            Some(Access::Read)
        );
        assert_eq!(
            tool.access_for(&serde_json::json!({"action": "replace", "name": "x", "text": "y"})),
            None,
            "the write verbs keep the schema's class"
        );
    }

    /// A file's mtime, **stated rather than waited for**: "newest first" is a
    /// rule these tests assert, and a sleep would make the assertion depend on
    /// the clock's resolution instead.
    fn set_mtime(path: &Path, secs: i64) {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("no NUL in a path");
        let times = [
            libc::timespec {
                tv_sec: secs,
                tv_nsec: 0,
            },
            libc::timespec {
                tv_sec: secs,
                tv_nsec: 0,
            },
        ];
        let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) };
        assert_eq!(rc, 0, "utimensat on {}", path.display());
    }

    struct FixtureScope {
        workspace: PathBuf,
        global: PathBuf,
    }
    impl NotesScope for FixtureScope {
        fn workspace(&self) -> PathBuf {
            self.workspace.clone()
        }
        fn global_dir(&self) -> PathBuf {
            self.global.clone()
        }
    }
}
