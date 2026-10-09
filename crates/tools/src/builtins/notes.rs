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
    /// The `*.md` files this source contributes, sorted by name — the same
    /// order the reader's glob produces, so the tool's listing and the
    /// injected block agree about what follows what. A single-file source
    /// (`AGENTS.md`) contributes its file only when the file is there: the
    /// reader skips an absent `AGENTS.md`, and a listing that named it would
    /// be a note that does not exist.
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
        paths.sort();
        paths
    }
}

/// The three sources, in the reader's order: box-wide, the workspace's
/// `AGENTS.md`, then the project's notes.
fn sources(scope: &dyn NotesScope) -> Vec<Source> {
    let ws = scope.workspace();
    vec![
        Source {
            dir: scope.global_dir(),
            file: None,
            label: "box-wide (the operator's)",
        },
        Source {
            dir: ws.clone(),
            file: Some(ws.join("AGENTS.md")),
            label: "workspace instructions (the operator's)",
        },
        Source {
            dir: ws.join(".letibot").join("notes"),
            file: None,
            label: "project notes (written with this tool, and by the operator)",
        },
    ]
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

impl Tool for NotesTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "notes",
            "The standing notes the harness reads into the system prompt and re-reads after \
             every compaction. `action`: `list` (every note, with path, size and when last \
             written), `read` (one note's text — give `name` for a project note or `path` as \
             `list` reported it), `add` (create one), `append` (add to one that exists), \
             `replace` (rewrite one that exists, reporting what it replaced). Writes go to \
             the workspace's `.letibot/notes/` only, by bare `name` — `AGENTS.md` and the \
             box-wide notes are the operator's. Write down what is worth keeping: something \
             interesting, remarkable or surprising you would not want to rediscover — not a \
             summary of what happened.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["list", "read", "add", "append", "replace"]},
                    "name": {"type": "string", "description": "A note's name — the file stem, no path. For `add`/`append`/`replace` it is where the note lives; for `read` it resolves in the project notes first."},
                    "path": {"type": "string", "description": "For `read`: a note's path exactly as `list` reported it, when what you have is a path rather than a name."},
                    "text": {"type": "string", "description": "For `add`/`append`/`replace`: the note's text — for `replace`, the whole new text."}
                },
                "required": ["action"]
            }),
            Access::Write,
        )
    }

    /// `list` and `read` only ever read, and a read that reaches the gate is
    /// a question the operator is asked about a fact. Narrowing only, per the
    /// trait: the schema's `Write` stands for every verb that writes.
    fn access_for(&self, args: &Value) -> Option<Access> {
        match args.get("action").and_then(|v| v.as_str()) {
            Some("list") | Some("read") => Some(Access::Read),
            _ => None,
        }
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(action) = args.get("action").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "notes needs an action",
                "call `notes` with `action` = \"list\", \"read\", \"add\", \"append\" or \
                 \"replace\".",
            );
        };
        match action {
            "list" => self.list(),
            "read" => self.read(
                args.get("path").and_then(|v| v.as_str()),
                args.get("name").and_then(|v| v.as_str()),
            ),
            "add" | "append" | "replace" => self.write(ctx, action, args),
            other => Invocation::failed(
                format!("unknown notes action `{other}`"),
                "call `notes` with `action` = \"list\", \"read\", \"add\", \"append\" or \
                 \"replace\".",
            ),
        }
    }
}

impl NotesTool {
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
                format!("{}\n", text.trim_end()),
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
                let mut grown = old.trim_end().to_string();
                grown.push_str("\n\n");
                grown.push_str(text.trim());
                grown.push('\n');
                let said = format!(
                    "appended {} to `{}`: {} → {}",
                    super::human(text.trim().len() as u64),
                    target.display(),
                    super::human(old.len() as u64),
                    super::human(grown.len() as u64),
                );
                (old.clone(), grown, said)
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
                (old.clone(), format!("{}\n", text.trim_end()), said)
            }
            _ => unreachable!("the match on action is total above"),
        };
        if before == after {
            return Invocation::ok(format!(
                "nothing to do: `{stem}` already holds exactly that text.\n\n{WHEN_READ}\n"
            ));
        }
        if let Err(e) = write_atomic(&target, after.as_bytes()) {
            return Invocation::failed(
                format!("could not write `{}`: {e}", target.display()),
                "the note is unchanged — the write is a temporary file renamed over the \
                 target, so a failure leaves the original untouched.",
            );
        }
        let mut inv = Invocation::ok(format!(
            "{said}: {}.\n\n{WHEN_READ}\n",
            super::human(after.len() as u64)
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

    /// **The listing carries every source, in the reader's order, with the
    /// age and the one-line caveat.**
    #[test]
    fn list_reports_every_source_in_the_readers_order() {
        let mut h = writable_harness();
        let (ws, global) = h.notes_dirs();
        std::fs::create_dir_all(&global).expect("global dir");
        std::fs::write(global.join("g.md"), "global note\n").expect("fixture");
        std::fs::write(ws.join("AGENTS.md"), "# Rules\n").expect("fixture");
        std::fs::create_dir_all(ws.join(".letibot/notes")).expect("project dir");
        std::fs::write(ws.join(".letibot/notes/p.md"), "project note\n").expect("fixture");
        let r = h.call("notes", r#"{"action":"list"}"#);
        assert!(r.is_grounded(), "{}", r.render());
        let said = r.render();
        let pos = |needle: &str| said.find(needle).expect(needle);
        assert!(
            pos("g.md") < pos("AGENTS.md") && pos("AGENTS.md") < pos("p.md"),
            "box-wide, then workspace instructions, then project: {said}"
        );
        assert!(
            said.contains("last written") && said.contains("may be outdated"),
            "the age and the caveat ride the listing: {said}"
        );
    }

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
